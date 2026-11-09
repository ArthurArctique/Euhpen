//! État de l'application et travaux en arrière-plan.
//!
//! Les fils de travail n'accèdent jamais à l'état : ils envoient des messages que le fil de
//! l'interface traite au début de chaque image.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::analysis::{analyze, Analysis};
use crate::cache::{cache_dir, clear_cache, dir_size};
use crate::cfr::{convert_to_cfr, default_output, CfrRequest};
use crate::config::{load_preferences, load_session, save_preferences, save_session, Preferences};
use crate::corrections;
use crate::decision::{decide, CutPlan, Overrides};
use crate::fcp7::{write_xmeml, Flavor, FLAVORS};
use crate::files::{date_key, is_media, media_in_dir, natural_cmp, output_paths, stem_of};
use crate::fps::{format_rate, frames_to_seconds, Rate};
use crate::intervals::{union, Interval};
use crate::presets::{self, Preset};
use crate::probe::{absolute, find_tool, probe};
use crate::proxy::{make_preview_audio, make_proxy, proxy_rate};
use crate::settings::Settings;
use crate::timeline::build_timeline;
use crate::transcript::{cached_transcript, Kind, Transcript, WHISPER_HEAD_MODEL};
use crate::worker::{hesitations_available, quick_hardware, Hardware, JobOutcome, JobRequest, TranscribeSession};

use super::player::{Media, Mode, Player, VideoSource};
use super::timeline::TimelineView;
use super::words::WordsView;

// --- file de travaux --------------------------------------------------------------------

struct QueueState<T> {
    jobs: Vec<(u64, T)>,
    prefer: Option<u64>,
    closed: bool,
}

/// File d'attente où le travail du fichier sélectionné passe devant les autres.
pub struct JobQueue<T> {
    state: Mutex<QueueState<T>>,
    ready: Condvar,
}

impl<T> JobQueue<T> {
    fn new() -> Arc<Self> {
        Arc::new(JobQueue { state: Mutex::new(QueueState { jobs: Vec::new(), prefer: None, closed: false }), ready: Condvar::new() })
    }

    fn push(&self, id: u64, job: T) {
        if let Ok(mut state) = self.state.lock() {
            state.jobs.retain(|(other, _)| *other != id);
            state.jobs.push((id, job));
        }
        self.ready.notify_all();
    }

    fn remove(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.jobs.retain(|(other, _)| *other != id);
        }
    }

    fn prefer(&self, id: Option<u64>) {
        if let Ok(mut state) = self.state.lock() {
            state.prefer = id;
        }
    }

    fn pop(&self) -> Option<(u64, T)> {
        let mut state = self.state.lock().ok()?;
        loop {
            if state.closed {
                return None;
            }
            if !state.jobs.is_empty() {
                let at = state.prefer.and_then(|p| state.jobs.iter().position(|(id, _)| *id == p)).unwrap_or(0);
                return Some(state.jobs.remove(at));
            }
            state = self.ready.wait(state).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
        self.ready.notify_all();
    }
}

// --- messages ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ProxyFiles {
    pub proxy: PathBuf,
    pub audio: Option<PathBuf>,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub duration: f64,
    pub kept: f64,
    pub cuts: usize,
    pub hesitations: usize,
    pub low_contrast: bool,
    pub empty: bool,
}

pub enum Msg {
    AnalysisStarted(u64),
    AnalysisProgress(u64, f64),
    AnalysisDone(u64, Result<Arc<Analysis>, String>),
    ProxyStarted(u64),
    ProxyProgress(u64, f64),
    ProxyDone(u64, u32, Result<ProxyFiles, String>),
    TranscriptionStarted(u64, u64),
    TranscriptionProgress(u64, u64, f64, String),
    TranscriptionDone(u64, u64, (String, String), Result<JobOutcome, String>),
    Hardware(Hardware),
    QuickHardware(Hardware),
    Stats(u64, Vec<(u64, Stats)>),
    CfrDone(u64, Result<(String, String), String>),
    Hesitations(Result<(), String>),
    CacheSize(u64),
}

#[derive(Clone)]
pub struct Bus {
    sender: Sender<Msg>,
    ctx: egui::Context,
}

impl Bus {
    fn send(&self, message: Msg) {
        let _ = self.sender.send(message);
        self.ctx.request_repaint();
    }
}

/// Évite d'inonder l'interface de messages de progression.
struct Throttle(f64);

impl Throttle {
    fn pass(&mut self, fraction: f64) -> bool {
        if fraction - self.0 >= 0.01 || fraction >= 1.0 || fraction < self.0 {
            self.0 = fraction;
            true
        } else {
            false
        }
    }
}

// --- fichiers ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub enum Status {
    Queued,
    Analyzing(f64),
    Ready,
    Error(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum TrStatus {
    Off,
    Unavailable(String),
    Queued,
    Running(f64, String),
    Ready,
    Cancelled,
    Error(String),
}

#[derive(Clone)]
pub enum ProxyStatus {
    None,
    Queued,
    Running(f64),
    Ready(ProxyFiles),
    Error(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum CfrStatus {
    None,
    Running,
    Done(String),
    Error(String),
}

pub struct Item {
    pub id: u64,
    pub path: String,
    pub name: String,
    pub status: Status,
    pub analysis: Option<Arc<Analysis>>,
    pub cancel: Arc<AtomicBool>,
    pub overrides: Overrides,
    pub transcript: Option<Arc<Transcript>>,
    pub tr_key: Option<(String, String)>,
    pub tr: TrStatus,
    pub tr_token: u64,
    pub tr_cancel: Arc<AtomicBool>,
    pub proxy: ProxyStatus,
    pub stats: Option<Stats>,
    pub cfr: CfrStatus,
}

impl Item {
    fn new(id: u64, path: String) -> Item {
        let name = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        Item {
            id,
            path,
            name,
            status: Status::Queued,
            analysis: None,
            cancel: Arc::new(AtomicBool::new(false)),
            overrides: Overrides::default(),
            transcript: None,
            tr_key: None,
            tr: TrStatus::Off,
            tr_token: 0,
            tr_cancel: Arc::new(AtomicBool::new(false)),
            proxy: ProxyStatus::None,
            stats: None,
            cfr: CfrStatus::None,
        }
    }
}

struct AnalysisJob {
    path: String,
    cancel: Arc<AtomicBool>,
}

struct ProxyJob {
    analysis: Arc<Analysis>,
    height: u32,
    cancel: Arc<AtomicBool>,
}

struct TranscriptionJob {
    analysis: Arc<Analysis>,
    language: String,
    model: String,
    force: bool,
    cancel: Arc<AtomicBool>,
    token: u64,
}

/// Compte les fils de travail vivants, pour les attendre à la fermeture.
struct Alive(Arc<AtomicUsize>);

impl Alive {
    fn new(count: &Arc<AtomicUsize>) -> Alive {
        count.fetch_add(1, Ordering::AcqRel);
        Alive(count.clone())
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn analysis_worker(queue: Arc<JobQueue<AnalysisJob>>, bus: Bus, alive: Alive) {
    let _alive = alive;
    while let Some((id, job)) = queue.pop() {
        if job.cancel.load(Ordering::Acquire) {
            continue;
        }
        bus.send(Msg::AnalysisStarted(id));
        let mut throttle = Throttle(0.0);
        let cancel = job.cancel.clone();
        let result = analyze(
            &job.path,
            true,
            &mut |fraction| {
                if throttle.pass(fraction) {
                    bus.send(Msg::AnalysisProgress(id, fraction));
                }
            },
            &|| cancel.load(Ordering::Acquire),
        );
        bus.send(Msg::AnalysisDone(id, result.map(|(analysis, _)| Arc::new(analysis))));
    }
}

fn build_proxy(job: &ProxyJob, progress: &mut dyn FnMut(f64)) -> Result<ProxyFiles, String> {
    let cancel = job.cancel.clone();
    let proxy = make_proxy(&job.analysis, job.height, progress, &|| cancel.load(Ordering::Acquire))?;
    let audio = if job.analysis.media.audio_streams.is_empty() {
        None
    } else {
        Some(make_preview_audio(&job.analysis, &proxy)?)
    };
    let (mut width, mut height) = (0, 0);
    if job.analysis.media.has_video {
        let info = probe(&proxy.to_string_lossy())?;
        width = info.width.max(0) as usize;
        height = info.height.max(0) as usize;
    }
    Ok(ProxyFiles { proxy, audio, width, height })
}

fn proxy_worker(queue: Arc<JobQueue<ProxyJob>>, bus: Bus, alive: Alive) {
    let _alive = alive;
    while let Some((id, job)) = queue.pop() {
        if job.cancel.load(Ordering::Acquire) {
            continue;
        }
        bus.send(Msg::ProxyStarted(id));
        let mut throttle = Throttle(0.0);
        let result = build_proxy(&job, &mut |fraction| {
            if throttle.pass(fraction) {
                bus.send(Msg::ProxyProgress(id, fraction));
            }
        });
        bus.send(Msg::ProxyDone(id, job.height, result));
    }
}

fn transcription_worker(queue: Arc<JobQueue<TranscriptionJob>>, bus: Bus, alive: Alive) {
    let _alive = alive;
    let mut session = TranscribeSession::default();
    while let Some((id, job)) = queue.pop() {
        if job.cancel.load(Ordering::Acquire) {
            continue;
        }
        bus.send(Msg::TranscriptionStarted(id, job.token));
        let request = JobRequest { analysis: &job.analysis, language: &job.language, model: &job.model, force: job.force };
        let mut throttle = Throttle(0.0);
        let mut last_label = String::new();
        let cancel = job.cancel.clone();
        let hardware_bus = bus.clone();
        let result = session.run(
            request,
            &mut |fraction, label| {
                if throttle.pass(fraction) || label != last_label {
                    last_label = label.to_string();
                    bus.send(Msg::TranscriptionProgress(id, job.token, fraction, last_label.clone()));
                }
            },
            &|| cancel.load(Ordering::Acquire),
            &mut |hardware| hardware_bus.send(Msg::Hardware(hardware.clone())),
        );
        let key = (job.model.clone(), job.language.clone());
        bus.send(Msg::TranscriptionDone(id, job.token, key, result));
    }
}

// --- dialogues --------------------------------------------------------------------------

pub struct ExportedFile {
    pub path: String,
    pub label: &'static str,
    pub sequence: String,
}

pub struct ExportedSequence {
    pub name: String,
    pub fps: String,
    pub size: String,
    pub duration: f64,
    pub source: f64,
    pub clips: usize,
}

pub struct ExportResult {
    pub files: Vec<ExportedFile>,
    pub sequences: Vec<ExportedSequence>,
    pub warnings: Vec<String>,
}

pub enum Dialog {
    None,
    Export(ExportResult),
    ExportConfirm(usize),
    System,
    Help,
    SavePreset(String),
    ConfirmQuit,
    ConfirmClear,
    ConfirmClearCache,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Info,
    Warning,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub until: Instant,
}

// --- application ------------------------------------------------------------------------

pub struct App {
    pub items: Vec<Item>,
    pub selected: Option<u64>,
    next_id: u64,
    pub settings: Settings,
    applied: Settings,
    pub prefs: Preferences,
    saved_prefs: Preferences,
    prefs_due: Option<Instant>,
    session_dirty: bool,
    pub presets: Vec<Preset>,
    pub plan: Option<Arc<CutPlan>>,
    pub plan_dirty: bool,
    stats_due: Option<Instant>,
    stats_generation: u64,

    bus: Bus,
    receiver: Receiver<Msg>,
    alive: Arc<AtomicUsize>,
    analysis_queue: Arc<JobQueue<AnalysisJob>>,
    proxy_queue: Arc<JobQueue<ProxyJob>>,
    transcription_queue: Arc<JobQueue<TranscriptionJob>>,
    tr_counter: u64,

    pub hesitations: Option<Result<(), String>>,
    pub hardware: Option<Hardware>,
    pub quick_hardware: Option<Hardware>,
    pub last_run: Option<(Option<String>, Option<f64>)>,
    pub cache_size: Option<u64>,

    pub player: Player,
    loaded: Option<(u64, Option<PathBuf>)>,
    pub texture: Option<egui::TextureHandle>,
    pub timeline: TimelineView,
    pub words: WordsView,
    pub dialog: Dialog,
    pub toasts: Vec<Toast>,
    pub path_input: String,
    pub export_name: String,
    pub drop_hover: bool,
}

impl App {
    pub fn new(ctx: &egui::Context, fresh: bool) -> App {
        let (sender, receiver) = channel();
        let bus = Bus { sender, ctx: ctx.clone() };
        let alive = Arc::new(AtomicUsize::new(0));
        let analysis_queue = JobQueue::new();
        let proxy_queue = JobQueue::new();
        let transcription_queue = JobQueue::new();

        for index in 0..2 {
            let (queue, worker_bus, guard): (Arc<JobQueue<AnalysisJob>>, Bus, Alive) =
                (analysis_queue.clone(), bus.clone(), Alive::new(&alive));
            let _ = thread::Builder::new()
                .name(format!("analyse-{index}"))
                .spawn(move || analysis_worker(queue, worker_bus, guard));
        }
        {
            let (queue, worker_bus, guard) = (proxy_queue.clone(), bus.clone(), Alive::new(&alive));
            let _ = thread::Builder::new().name("proxy".to_string()).spawn(move || proxy_worker(queue, worker_bus, guard));
        }
        {
            let (queue, worker_bus, guard) = (transcription_queue.clone(), bus.clone(), Alive::new(&alive));
            let _ = thread::Builder::new()
                .name("transcription".to_string())
                .spawn(move || transcription_worker(queue, worker_bus, guard));
        }
        {
            let worker_bus = bus.clone();
            thread::spawn(move || worker_bus.send(Msg::Hesitations(hesitations_available())));
        }

        let prefs = load_preferences();
        let settings = prefs
            .settings
            .as_ref()
            .and_then(Settings::from_value_lenient)
            .or_else(|| presets::get_preset(&prefs.preset))
            .unwrap_or_default();
        let player = Player::new(ctx);
        player.set_mode(Mode::from_key(&prefs.play_mode));

        let mut app = App {
            items: Vec::new(),
            selected: None,
            next_id: 1,
            applied: settings.clone(),
            settings,
            saved_prefs: prefs.clone(),
            prefs,
            prefs_due: None,
            session_dirty: false,
            presets: presets::list_presets(),
            plan: None,
            plan_dirty: false,
            stats_due: None,
            stats_generation: 0,
            bus,
            receiver,
            alive,
            analysis_queue,
            proxy_queue,
            transcription_queue,
            tr_counter: 0,
            hesitations: None,
            hardware: None,
            quick_hardware: None,
            last_run: None,
            cache_size: None,
            player,
            loaded: None,
            texture: None,
            timeline: TimelineView::default(),
            words: WordsView::default(),
            dialog: Dialog::None,
            toasts: Vec::new(),
            path_input: String::new(),
            export_name: String::new(),
            drop_hover: false,
        };
        if !fresh {
            let restored = load_session();
            let existing: Vec<String> = restored.into_iter().filter(|p| Path::new(p).is_file()).collect();
            app.add_paths(&existing, false);
        }
        app
    }

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        let seconds = if kind == ToastKind::Error { 7 } else { 4 };
        self.toasts.push(Toast { text: text.into(), kind, until: Instant::now() + Duration::from_secs(seconds) });
    }

    pub fn wanted_key(&self) -> (String, String) {
        (WHISPER_HEAD_MODEL.to_string(), self.prefs.language.clone())
    }

    pub fn item(&self, id: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    fn item_mut(&mut self, id: u64) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    pub fn current(&self) -> Option<&Item> {
        self.selected.and_then(|id| self.item(id))
    }

    pub fn current_analysis(&self) -> Option<Arc<Analysis>> {
        self.current().and_then(|i| i.analysis.clone())
    }

    // fichiers

    pub fn add_paths(&mut self, paths: &[String], announce: bool) -> usize {
        let key = |path: &str| {
            let text = absolute(path);
            if cfg!(windows) { text.replace('/', "\\").to_lowercase() } else { text }
        };
        let mut known: std::collections::HashSet<String> = self.items.iter().map(|i| key(&i.path)).collect();
        let mut added = Vec::new();
        for raw in paths {
            let cleaned = raw.trim().trim_matches('"');
            if cleaned.is_empty() {
                continue;
            }
            let path = PathBuf::from(absolute(cleaned));
            let candidates: Vec<PathBuf> = if path.is_dir() {
                media_in_dir(&path)
            } else if path.is_file() {
                vec![path]
            } else {
                if announce {
                    self.toast(format!("Introuvable : {cleaned}"), ToastKind::Error);
                }
                continue;
            };
            for candidate in candidates {
                let text = candidate.to_string_lossy().into_owned();
                if known.insert(key(&text)) {
                    added.push(text);
                }
            }
        }
        let count = added.len();
        let first_new = self.next_id;
        for path in added {
            let id = self.next_id;
            self.next_id += 1;
            let item = Item::new(id, path.clone());
            self.analysis_queue.push(id, AnalysisJob { path, cancel: item.cancel.clone() });
            self.items.push(item);
        }
        if self.selected.is_none() && count > 0 {
            self.select(Some(first_new));
        }
        if count > 0 {
            self.session_dirty = true;
            self.prefs_due = Some(Instant::now() + Duration::from_millis(500));
        } else if announce && !paths.is_empty() {
            self.toast("Aucun nouveau fichier ajouté.", ToastKind::Info);
        }
        count
    }

    pub fn add_files_dialog(&mut self) {
        let extensions: Vec<&str> = crate::files::MEDIA_EXTENSIONS.iter().map(|e| e.trim_start_matches('.')).collect();
        if let Some(files) = rfd::FileDialog::new().add_filter("Médias", &extensions).set_title("Ajouter des rushes").pick_files() {
            let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            self.add_paths(&paths, true);
        }
    }

    pub fn add_folder_dialog(&mut self) {
        if let Some(folder) = rfd::FileDialog::new().set_title("Ajouter un dossier de rushes").pick_folder() {
            self.add_paths(&[folder.to_string_lossy().into_owned()], true);
        }
    }

    pub fn select(&mut self, id: Option<u64>) {
        if self.selected == id {
            return;
        }
        self.selected = id;
        self.proxy_queue.prefer(id);
        self.transcription_queue.prefer(id);
        self.plan = None;
        self.plan_dirty = true;
        self.timeline.reset();
        self.words.invalidate();
        self.player.pause();
    }

    pub fn select_relative(&mut self, delta: i64) {
        let Some(current) = self.selected else { return };
        let Some(at) = self.items.iter().position(|i| i.id == current) else { return };
        let target = (at as i64 + delta).clamp(0, self.items.len() as i64 - 1) as usize;
        let id = self.items[target].id;
        self.select(Some(id));
    }

    fn stop_item(&mut self, id: u64) {
        self.analysis_queue.remove(id);
        self.proxy_queue.remove(id);
        self.transcription_queue.remove(id);
        if let Some(item) = self.item(id) {
            item.cancel.store(true, Ordering::Release);
            item.tr_cancel.store(true, Ordering::Release);
        }
    }

    pub fn remove(&mut self, id: u64) {
        self.stop_item(id);
        let at = self.items.iter().position(|i| i.id == id);
        self.items.retain(|i| i.id != id);
        if self.selected == Some(id) {
            let next = at.and_then(|a| self.items.get(a.min(self.items.len().saturating_sub(1)))).map(|i| i.id);
            self.select(next);
        }
        self.session_dirty = true;
        self.prefs_due = Some(Instant::now() + Duration::from_millis(500));
    }

    pub fn clear_all(&mut self) {
        let ids: Vec<u64> = self.items.iter().map(|i| i.id).collect();
        for id in ids {
            self.stop_item(id);
        }
        self.items.clear();
        self.select(None);
        self.session_dirty = true;
        self.prefs_due = Some(Instant::now() + Duration::from_millis(500));
    }

    pub fn move_item(&mut self, id: u64, delta: i64) {
        let Some(at) = self.items.iter().position(|i| i.id == id) else { return };
        let target = at as i64 + delta;
        if target >= 0 && (target as usize) < self.items.len() {
            self.items.swap(at, target as usize);
            self.session_dirty = true;
            self.prefs_due = Some(Instant::now() + Duration::from_millis(500));
        }
    }

    pub fn retry_analysis(&mut self, id: u64) {
        let Some(item) = self.item_mut(id) else { return };
        item.status = Status::Queued;
        item.cancel = Arc::new(AtomicBool::new(false));
        let job = AnalysisJob { path: item.path.clone(), cancel: item.cancel.clone() };
        self.analysis_queue.push(id, job);
    }

    pub fn request_proxy(&mut self, id: u64) {
        let height = self.prefs.proxy_height;
        let Some(item) = self.item_mut(id) else { return };
        let Some(analysis) = item.analysis.clone() else { return };
        item.proxy = ProxyStatus::Queued;
        let cancel = item.cancel.clone();
        self.proxy_queue.push(id, ProxyJob { analysis, height, cancel });
    }

    pub fn request_all_proxies(&mut self) {
        let ids: Vec<u64> = self.items.iter().filter(|i| i.analysis.is_some()).map(|i| i.id).collect();
        for id in ids {
            self.request_proxy(id);
        }
    }

    pub fn convert_cfr(&mut self, id: u64) {
        let Some(item) = self.item_mut(id) else { return };
        let Some(analysis) = item.analysis.clone() else { return };
        item.cfr = CfrStatus::Running;
        let bus = self.bus.clone();
        thread::spawn(move || {
            let existing = default_output(&analysis.media.path);
            let result = if Path::new(&existing).exists() {
                let name = stem_file(&existing);
                Ok((format!("déjà converti : {name}"), existing))
            } else {
                let request = CfrRequest { output: None, fps: None, crf: 17, overwrite: false, show_progress: false };
                convert_to_cfr(&analysis.media, request).map(|(output, rate)| {
                    (format!("converti à {} i/s : {}", format_rate(Some(rate)), stem_file(&output)), output)
                })
            };
            bus.send(Msg::CfrDone(id, result));
        });
    }

    // transcription

    pub fn refresh_transcript(&mut self, id: u64) {
        let wanted = self.wanted_key();
        let hesitations_on = self.settings.hesitations;
        let available = self.hesitations.clone();
        let Some(item) = self.item_mut(id) else { return };
        let Some(analysis) = item.analysis.clone() else { return };
        if item.transcript.is_some() && item.tr_key.as_ref() == Some(&wanted) {
            item.tr = TrStatus::Ready;
            return;
        }
        let running = matches!(item.tr, TrStatus::Running(..));
        if running && item.tr_key.as_ref() == Some(&wanted) {
            return;
        }
        if running {
            item.tr_cancel.store(true, Ordering::Release);
            item.tr_token += 1;
        }
        if let Some(cached) = cached_transcript(&analysis.fingerprint, &wanted.0, &wanted.1) {
            item.transcript = Some(Arc::new(cached));
            item.tr_key = Some(wanted);
            item.tr = TrStatus::Ready;
            self.plan_dirty |= self.selected == Some(id);
            self.words.invalidate();
            self.stats_soon();
            return;
        }
        item.transcript = None;
        item.tr_key = Some(wanted);
        if !hesitations_on {
            item.tr = TrStatus::Off;
            self.transcription_queue.remove(id);
            return;
        }
        match available {
            Some(Err(message)) => {
                item.tr = TrStatus::Unavailable(message);
                self.transcription_queue.remove(id);
            }
            // La vérification n'est pas terminée : le message Hesitations relancera ceci.
            None => item.tr = TrStatus::Queued,
            Some(Ok(())) => {
                item.tr = TrStatus::Queued;
                self.queue_transcription(id, false);
            }
        }
    }

    fn queue_transcription(&mut self, id: u64, force: bool) {
        self.tr_counter += 1;
        let token = self.tr_counter;
        let (model, language) = self.wanted_key();
        let Some(item) = self.item_mut(id) else { return };
        let Some(analysis) = item.analysis.clone() else { return };
        item.tr_cancel.store(true, Ordering::Release);
        item.tr_cancel = Arc::new(AtomicBool::new(false));
        item.tr_token = token;
        let job = TranscriptionJob { analysis, language, model, force, cancel: item.tr_cancel.clone(), token };
        self.transcription_queue.push(id, job);
    }

    pub fn refresh_all_transcripts(&mut self) {
        let ids: Vec<u64> = self.items.iter().filter(|i| i.analysis.is_some()).map(|i| i.id).collect();
        for id in ids {
            self.refresh_transcript(id);
        }
    }

    pub fn retranscribe(&mut self, id: u64, force: bool) {
        let wanted = self.wanted_key();
        let Some(item) = self.item_mut(id) else { return };
        if force {
            item.transcript = None;
        }
        item.tr_key = Some(wanted);
        item.tr = TrStatus::Queued;
        self.words.invalidate();
        self.plan_dirty |= self.selected == Some(id);
        self.stats_soon();
        self.queue_transcription(id, force);
    }

    pub fn cancel_transcription(&mut self, id: u64) {
        self.transcription_queue.remove(id);
        let Some(item) = self.item_mut(id) else { return };
        item.tr_cancel.store(true, Ordering::Release);
        item.tr_token = item.tr_token.wrapping_add(1_000_000);
        item.tr = TrStatus::Cancelled;
    }

    // décision

    pub fn plan_of(&self, item: &Item) -> Option<CutPlan> {
        let analysis = item.analysis.as_deref()?;
        let transcript = if item.tr_key.as_ref() == Some(&self.wanted_key()) { item.transcript.as_deref() } else { None };
        Some(decide(analysis, &self.settings, Some(&item.overrides), transcript))
    }

    pub fn stats_soon(&mut self) {
        self.stats_due = Some(Instant::now() + Duration::from_millis(350));
    }

    fn start_stats(&mut self) {
        self.stats_generation += 1;
        let generation = self.stats_generation;
        let settings = self.settings.clone();
        let wanted = self.wanted_key();
        let rows: Vec<(u64, Arc<Analysis>, Overrides, Option<Arc<Transcript>>)> = self
            .items
            .iter()
            .filter_map(|i| {
                let transcript = if i.tr_key.as_ref() == Some(&wanted) { i.transcript.clone() } else { None };
                Some((i.id, i.analysis.clone()?, i.overrides.clone(), transcript))
            })
            .collect();
        let bus = self.bus.clone();
        thread::spawn(move || {
            let stats = rows
                .iter()
                .map(|(id, analysis, overrides, transcript)| {
                    (*id, stats_of(&decide(analysis, &settings, Some(overrides), transcript.as_deref())))
                })
                .collect();
            bus.send(Msg::Stats(generation, stats));
        });
    }

    pub fn toggle_at(&mut self, t: f64, span: Option<Interval>) {
        let Some(plan) = self.plan.clone() else { return };
        let Some(id) = self.selected else { return };
        let Some(item) = self.item_mut(id) else { return };
        let Some(analysis) = item.analysis.clone() else { return };
        let Some(change) = corrections::toggle(&plan, &mut item.overrides, t, span) else { return };
        let overrides = item.overrides.clone();
        let source = item.path.clone();
        corrections::journal_correction(&analysis.fingerprint, &source, change);
        self.write_overrides(&analysis.fingerprint, &overrides);
    }

    pub fn clear_overrides(&mut self) {
        let Some(id) = self.selected else { return };
        let Some(item) = self.item_mut(id) else { return };
        let Some(analysis) = item.analysis.clone() else { return };
        item.overrides = Overrides::default();
        self.write_overrides(&analysis.fingerprint, &Overrides::default());
    }

    fn write_overrides(&mut self, fingerprint: &str, overrides: &Overrides) {
        if let Err(message) = corrections::save_overrides(fingerprint, overrides) {
            self.toast(format!("Corrections non enregistrées : {message}"), ToastKind::Error);
        }
        self.plan_dirty = true;
        self.stats_soon();
    }

    // presets

    pub fn apply_preset(&mut self, name: &str) {
        if let Some(settings) = presets::get_preset(name) {
            self.settings = settings;
            self.prefs.preset = name.to_string();
        }
    }

    pub fn preset_modified(&self) -> bool {
        self.presets.iter().find(|p| p.name == self.prefs.preset).is_none_or(|p| p.settings != self.settings)
    }

    pub fn save_preset(&mut self, name: &str) -> bool {
        match presets::save_preset(name, &self.settings) {
            Ok(()) => {
                self.presets = presets::list_presets();
                self.prefs.preset = name.trim().to_string();
                self.toast(format!("Preset « {} » enregistré.", name.trim()), ToastKind::Info);
                true
            }
            Err(message) => {
                self.toast(message, ToastKind::Error);
                false
            }
        }
    }

    pub fn delete_preset(&mut self) {
        let name = self.prefs.preset.clone();
        match presets::delete_preset(&name) {
            Ok(()) => {
                self.presets = presets::list_presets();
                self.prefs.preset = "Standard".to_string();
                self.toast(format!("Preset « {name} » supprimé."), ToastKind::Info);
            }
            Err(message) => self.toast(message, ToastKind::Error),
        }
    }

    // système

    pub fn open_system(&mut self) {
        self.dialog = Dialog::System;
        self.refresh_cache_size();
        if self.quick_hardware.is_none() && self.hardware.is_none() {
            let bus = self.bus.clone();
            thread::spawn(move || bus.send(Msg::QuickHardware(quick_hardware())));
        }
    }

    fn refresh_cache_size(&mut self) {
        self.cache_size = None;
        let bus = self.bus.clone();
        thread::spawn(move || bus.send(Msg::CacheSize(dir_size(&cache_dir()))));
    }

    pub fn transcription_active(&self) -> bool {
        self.items.iter().any(|i| matches!(i.tr, TrStatus::Running(..)))
    }

    pub fn clear_cache(&mut self) {
        if self.transcription_active() {
            self.toast("Une transcription est en cours : attendez la fin ou annulez-la.", ToastKind::Error);
            return;
        }
        // Le fichier son de l'aperçu ne peut pas être effacé tant qu'il est projeté en mémoire.
        self.loaded = None;
        let _ = self.player.load(None);
        match clear_cache() {
            Ok(path) => self.toast(format!("Cache vidé : {}", path.display()), ToastKind::Info),
            Err(message) => self.toast(message, ToastKind::Error),
        }
        for item in &mut self.items {
            item.proxy = ProxyStatus::None;
            item.transcript = None;
            item.tr_key = None;
        }
        self.words.invalidate();
        self.request_all_proxies();
        self.refresh_all_transcripts();
        self.plan_dirty = true;
        self.stats_soon();
        self.refresh_cache_size();
    }

    // boucle

    fn handle(&mut self, message: Msg) {
        match message {
            Msg::AnalysisStarted(id) => {
                if let Some(item) = self.item_mut(id) {
                    item.status = Status::Analyzing(0.0);
                }
            }
            Msg::AnalysisProgress(id, fraction) => {
                if let Some(item) = self.item_mut(id) {
                    item.status = Status::Analyzing(fraction);
                }
            }
            Msg::AnalysisDone(id, result) => {
                let Some(item) = self.item_mut(id) else { return };
                match result {
                    Ok(analysis) => {
                        item.overrides = corrections::load_overrides(&analysis.fingerprint);
                        item.analysis = Some(analysis);
                        item.status = Status::Ready;
                        self.request_proxy(id);
                        self.refresh_transcript(id);
                        self.plan_dirty |= self.selected == Some(id);
                        self.stats_soon();
                    }
                    Err(message) => item.status = Status::Error(message),
                }
            }
            Msg::ProxyStarted(id) => {
                if let Some(item) = self.item_mut(id) {
                    item.proxy = ProxyStatus::Running(0.0);
                }
            }
            Msg::ProxyProgress(id, fraction) => {
                if let Some(item) = self.item_mut(id) {
                    item.proxy = ProxyStatus::Running(fraction);
                }
            }
            Msg::ProxyDone(id, height, result) => {
                if height != self.prefs.proxy_height && result.is_ok() {
                    self.request_proxy(id);
                    return;
                }
                let Some(item) = self.item_mut(id) else { return };
                item.proxy = match result {
                    Ok(files) => ProxyStatus::Ready(files),
                    Err(message) => ProxyStatus::Error(message),
                };
            }
            Msg::TranscriptionStarted(id, token) => {
                if let Some(item) = self.item_mut(id).filter(|i| i.tr_token == token) {
                    item.tr = TrStatus::Running(0.0, String::new());
                }
            }
            Msg::TranscriptionProgress(id, token, fraction, label) => {
                if let Some(item) = self.item_mut(id).filter(|i| i.tr_token == token) {
                    item.tr = TrStatus::Running(fraction, label);
                }
            }
            Msg::TranscriptionDone(id, token, key, result) => {
                let Some(item) = self.item_mut(id).filter(|i| i.tr_token == token) else { return };
                match result {
                    Ok(outcome) => {
                        item.transcript = Some(Arc::new(outcome.transcript));
                        item.tr_key = Some(key);
                        item.tr = TrStatus::Ready;
                        self.last_run = Some((outcome.encoder_device, outcome.gpu_memory_mb));
                        self.plan_dirty |= self.selected == Some(id);
                        self.words.invalidate();
                        self.stats_soon();
                    }
                    Err(message) => item.tr = TrStatus::Error(message),
                }
            }
            Msg::Hardware(hardware) => {
                let warnings = hardware.warnings.clone();
                self.hardware = Some(hardware);
                for warning in warnings {
                    self.toast(warning, ToastKind::Warning);
                }
            }
            Msg::QuickHardware(hardware) => self.quick_hardware = Some(hardware),
            Msg::Stats(generation, rows) => {
                if generation == self.stats_generation {
                    for (id, stats) in rows {
                        if let Some(item) = self.item_mut(id) {
                            item.stats = Some(stats);
                        }
                    }
                }
            }
            Msg::CfrDone(id, result) => match result {
                Ok((message, output)) => {
                    if let Some(item) = self.item_mut(id) {
                        item.cfr = CfrStatus::Done(message);
                    }
                    self.add_paths(&[output], false);
                }
                Err(message) => {
                    if let Some(item) = self.item_mut(id) {
                        item.cfr = CfrStatus::Error(message);
                    }
                }
            },
            Msg::Hesitations(result) => {
                self.hesitations = Some(result);
                self.refresh_all_transcripts();
            }
            Msg::CacheSize(size) => self.cache_size = Some(size),
        }
    }

    /// Traite les messages, recalcule ce qui doit l'être et enregistre les préférences.
    pub fn update_state(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            self.handle(message);
        }

        if self.settings != self.applied {
            let transcript_changed = self.settings.hesitations != self.applied.hesitations;
            self.applied = self.settings.clone();
            self.plan_dirty = true;
            self.stats_soon();
            if transcript_changed {
                self.refresh_all_transcripts();
            }
            self.prefs.settings = serde_json::to_value(&self.settings).ok();
        }
        if self.prefs != self.saved_prefs && self.prefs_due.is_none() {
            self.prefs_due = Some(Instant::now() + Duration::from_millis(500));
        }
        if self.prefs_due.is_some_and(|due| Instant::now() >= due) {
            self.flush_preferences();
        }

        if self.plan_dirty {
            self.plan_dirty = false;
            self.plan = self.current().and_then(|item| self.plan_of(item)).map(Arc::new);
            if let (Some(plan), Some(id)) = (self.plan.clone(), self.selected) {
                let stats = stats_of(&plan);
                if let Some(item) = self.item_mut(id) {
                    item.stats = Some(stats);
                }
            }
            let cuts = self
                .plan
                .as_ref()
                .map(|plan| union(&plan.removed.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>()))
                .unwrap_or_default();
            self.player.set_cuts(cuts);
            self.words.mark_plan_changed();
        }
        if self.stats_due.is_some_and(|due| Instant::now() >= due) {
            self.stats_due = None;
            self.start_stats();
        }

        self.sync_player();
        self.player.poll();
        self.player.sync_video();
    }

    /// Prochaine échéance à laquelle l'interface doit se réveiller d'elle-même.
    pub fn next_wakeup(&self) -> Option<Duration> {
        let now = Instant::now();
        [self.prefs_due, self.stats_due]
            .into_iter()
            .flatten()
            .chain(self.toasts.iter().map(|t| t.until))
            .min()
            .map(|due| due.saturating_duration_since(now))
    }

    fn sync_player(&mut self) {
        let wanted: Option<(u64, Option<ProxyFiles>, f64, bool, Option<Rate>)> = self.current().and_then(|item| {
            let analysis = item.analysis.as_ref()?;
            let files = match &item.proxy {
                ProxyStatus::Ready(files) => Some(files.clone()),
                _ => None,
            };
            Some((item.id, files, analysis.media.duration, analysis.media.has_video, analysis.media.fps))
        });
        let key = wanted.as_ref().map(|(id, files, ..)| (*id, files.as_ref().map(|f| f.proxy.clone())));
        if key == self.loaded {
            return;
        }
        let same_item = key.as_ref().map(|k| k.0) == self.loaded.as_ref().map(|k| k.0);
        let (position, was_playing) = (self.player.position(), self.player.is_playing());
        self.loaded = key;
        self.texture = None;
        let media = wanted.map(|(_, files, duration, has_video, fps)| match files {
            Some(files) => Media {
                audio: files.audio.clone(),
                video: (has_video && files.width > 0 && files.height > 0).then(|| VideoSource {
                    path: files.proxy.clone(),
                    width: files.width,
                    height: files.height,
                    fps: proxy_rate(fps).as_f64(),
                }),
                duration,
            },
            None => Media { audio: None, video: None, duration },
        });
        if let Err(message) = self.player.load(media) {
            self.toast(format!("Son de l'aperçu indisponible : {message}"), ToastKind::Error);
        }
        if same_item {
            self.player.seek(position);
            if was_playing {
                self.player.play();
            }
        }
    }

    pub fn flush_preferences(&mut self) {
        self.prefs_due = None;
        if self.prefs != self.saved_prefs {
            if let Err(message) = save_preferences(&self.prefs) {
                self.toast(format!("Préférences non enregistrées : {message}"), ToastKind::Error);
            }
            self.saved_prefs = self.prefs.clone();
        }
        if self.session_dirty {
            self.session_dirty = false;
            let paths: Vec<String> = self.items.iter().map(|i| i.path.clone()).collect();
            if let Err(message) = save_session(&paths) {
                self.toast(format!("Session non enregistrée : {message}"), ToastKind::Error);
            }
        }
    }

    // export

    pub fn export_blockers(&self) -> usize {
        if !self.settings.hesitations {
            return 0;
        }
        self.items.iter().filter(|i| i.status == Status::Ready && i.tr != TrStatus::Ready).count()
    }

    pub fn ready_count(&self) -> usize {
        self.items.iter().filter(|i| i.status == Status::Ready).count()
    }

    pub fn request_export(&mut self) {
        if self.ready_count() == 0 {
            self.toast("Aucun fichier analysé à exporter.", ToastKind::Error);
            return;
        }
        let waiting = self.export_blockers();
        if waiting > 0 {
            self.dialog = Dialog::ExportConfirm(waiting);
        } else {
            self.run_export();
        }
    }

    pub fn run_export(&mut self) {
        match self.export() {
            Ok(result) => self.dialog = Dialog::Export(result),
            Err(message) => {
                self.dialog = Dialog::None;
                self.toast(message, ToastKind::Error);
            }
        }
    }

    fn export(&self) -> Result<ExportResult, String> {
        let mut items: Vec<&Item> = self.items.iter().filter(|i| i.status == Status::Ready && i.analysis.is_some()).collect();
        if items.is_empty() {
            return Err("aucun fichier analysé à exporter.".to_string());
        }
        let flavors: Vec<Flavor> = match self.prefs.target.as_str() {
            "premiere" => vec![Flavor::Premiere],
            "resolve" => vec![Flavor::Resolve],
            _ => FLAVORS.to_vec(),
        };
        match self.prefs.order.as_str() {
            "name" => items.sort_by(|a, b| natural_cmp(&a.name, &b.name)),
            "date" => items.sort_by(|a, b| {
                let key = |i: &Item| i.analysis.as_ref().map(|an| date_key(&an.media)).unwrap_or(0.0);
                key(a).total_cmp(&key(b))
            }),
            _ => {}
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut not_transcribed: Vec<String> = Vec::new();
        let mut entries: Vec<(&Item, CutPlan)> = Vec::new();
        for item in items {
            let Some(plan) = self.plan_of(item) else { continue };
            if self.settings.hesitations && item.tr != TrStatus::Ready {
                not_transcribed.push(item.name.clone());
            }
            if plan.levels.low_contrast() {
                warnings.push(format!("{} : peu d'écart entre bruit de fond et parole, vérifiez le résultat.", item.name));
            }
            if plan.kept.is_empty() {
                warnings.push(format!("{} : aucun passage gardé (tout est sous le seuil).", item.name));
            }
            entries.push((item, plan));
        }
        if not_transcribed.len() == 1 {
            warnings.insert(0, format!("{} : transcription pas prête, les hésitations n'y sont pas coupées.", not_transcribed[0]));
        } else if !not_transcribed.is_empty() {
            warnings.insert(
                0,
                format!(
                    "Transcription pas prête pour {} fichiers ({}) : leurs hésitations ne sont pas coupées.",
                    not_transcribed.len(),
                    not_transcribed.join(", ")
                ),
            );
        }

        let separate = self.prefs.export_mode == "separate";
        let typed_name = self.export_name.trim().to_string();
        let mut groups: Vec<(String, Vec<(&Item, CutPlan)>)> = Vec::new();
        if separate {
            for entry in entries {
                groups.push((stem_of(&entry.0.name), vec![entry]));
            }
        } else {
            let first = stem_of(&entries[0].0.name);
            let name = if !typed_name.is_empty() {
                typed_name.clone()
            } else if entries.len() == 1 {
                first
            } else {
                format!("{first} (+{})", entries.len() - 1)
            };
            groups.push((name, entries));
        }

        let mut files = Vec::new();
        let mut sequences = Vec::new();
        for (name, group) in groups {
            let first_media = group[0].0.analysis.as_ref().ok_or("analyse manquante")?.media.clone();
            let rows = group
                .iter()
                .filter_map(|(item, plan)| {
                    let media = item.analysis.as_ref()?.media.clone();
                    Some((media, plan.kept.clone(), Some(plan.clone())))
                })
                .collect();
            let timeline = build_timeline(rows, &name, Rate::whole(25));
            warnings.extend(timeline.warnings.iter().cloned());
            let safe: String = name.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
            let base = match self.prefs.output_dir.as_deref().filter(|d| !d.is_empty()) {
                Some(dir) => Some(Path::new(dir).join(format!("{safe}_euhpen")).to_string_lossy().into_owned()),
                None if !separate && !typed_name.is_empty() => {
                    let parent = Path::new(&first_media.path).parent().unwrap_or(Path::new("."));
                    Some(parent.join(format!("{safe}_euhpen")).to_string_lossy().into_owned())
                }
                None => None,
            };
            for (flavor, path) in output_paths(base.as_deref(), &first_media, &flavors)? {
                warnings.extend(write_xmeml(&timeline, &path, flavor, self.prefs.zero_timecode, self.prefs.raw_paths)?);
                files.push(ExportedFile { path, label: flavor.label(), sequence: name.clone() });
            }
            sequences.push(ExportedSequence {
                name: name.clone(),
                fps: format_rate(Some(timeline.fps)),
                size: format!("{}×{}", timeline.width, timeline.height),
                duration: frames_to_seconds(timeline.duration_frames(), timeline.fps),
                source: timeline.items.iter().map(|i| frames_to_seconds(i.source_frames, timeline.fps)).sum(),
                clips: timeline.items.iter().map(|i| i.segments.len()).sum(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        warnings.retain(|w| seen.insert(w.clone()));
        Ok(ExportResult { files, sequences, warnings })
    }

    pub fn tools_status(&self) -> Vec<(&'static str, Result<String, String>)> {
        ["ffmpeg", "ffprobe"].into_iter().map(|tool| (tool, find_tool(tool))).collect()
    }

    pub fn shutdown(&mut self) {
        self.flush_preferences();
        self.analysis_queue.close();
        self.proxy_queue.close();
        self.transcription_queue.close();
        for item in &self.items {
            item.cancel.store(true, Ordering::Release);
            item.tr_cancel.store(true, Ordering::Release);
        }
        // Les fils tuent leur ffmpeg ou leur processus Python dès qu'ils voient l'annulation.
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.alive.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn stem_file(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

pub fn stats_of(plan: &CutPlan) -> Stats {
    Stats {
        duration: plan.duration,
        kept: plan.kept_duration(),
        cuts: union(&plan.removed.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>()).len(),
        hesitations: plan.event_count(Kind::Hesitation),
        low_contrast: plan.levels.low_contrast(),
        empty: plan.kept.is_empty(),
    }
}

pub fn accepts_media(path: &Path) -> bool {
    path.is_dir() || is_media(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_serves_the_preferred_job_first() {
        let queue: Arc<JobQueue<&str>> = JobQueue::new();
        queue.push(1, "a");
        queue.push(2, "b");
        queue.prefer(Some(2));
        assert_eq!(queue.pop(), Some((2, "b")));
        assert_eq!(queue.pop(), Some((1, "a")));
    }

    #[test]
    fn pushing_again_replaces_the_job_and_close_releases_waiters() {
        let queue: Arc<JobQueue<&str>> = JobQueue::new();
        queue.push(1, "a");
        queue.push(1, "b");
        assert_eq!(queue.pop(), Some((1, "b")));
        let waiter = {
            let queue = queue.clone();
            thread::spawn(move || queue.pop())
        };
        thread::sleep(Duration::from_millis(20));
        queue.close();
        assert_eq!(waiter.join().ok().flatten(), None);
    }
}
