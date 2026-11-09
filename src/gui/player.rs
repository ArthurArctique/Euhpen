//! Lecteur d'aperçu natif. Le son sert d'horloge : la vidéo suit la position audio.
//!
//! Le son de l'aperçu est un fichier PCM projeté en mémoire ; le rendu se fait échantillon par
//! échantillon dans le rappel audio, ce qui rend les sauts de montage exacts. Les images viennent
//! d'un ffmpeg qui décode le proxy vers un tube, relancé seulement en cas de saut.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arc_swap::{ArcSwap, ArcSwapOption};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use memmap2::Mmap;

use crate::intervals::Interval;
use crate::probe::{find_tool, hidden_command};

pub const SOURCE_RATE: f64 = 48_000.0;
/// Durée des fondus aux sauts : assez pour supprimer le clic, trop court pour s'entendre.
const FADE_SECONDS: f64 = 0.005;
const NO_SEEK: u64 = u64::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Lit le résultat : les coupes sont sautées.
    Montage,
    /// Lit tout le média.
    Original,
    /// Ne lit que ce qui est coupé.
    Cuts,
}

impl Mode {
    fn from_u8(value: u8) -> Mode {
        match value {
            1 => Mode::Original,
            2 => Mode::Cuts,
            _ => Mode::Montage,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Mode::Montage => 0,
            Mode::Original => 1,
            Mode::Cuts => 2,
        }
    }

    pub fn next(self) -> Mode {
        match self {
            Mode::Montage => Mode::Original,
            Mode::Original => Mode::Cuts,
            Mode::Cuts => Mode::Montage,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Mode::Montage => "montage",
            Mode::Original => "original",
            Mode::Cuts => "cuts",
        }
    }

    pub fn from_key(key: &str) -> Mode {
        match key {
            "original" => Mode::Original,
            "cuts" => Mode::Cuts,
            _ => Mode::Montage,
        }
    }
}

struct Pcm {
    map: Mmap,
}

impl Pcm {
    fn open(path: &Path) -> Result<Pcm, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{} : {e}", path.display()))?;
        // Sûreté : le fichier du cache n'est pas réécrit pendant sa lecture (écrit sous un nom
        // temporaire puis renommé, jamais modifié ensuite).
        let map = unsafe { Mmap::map(&file) }.map_err(|e| format!("{} : {e}", path.display()))?;
        Ok(Pcm { map })
    }

    #[inline]
    fn sample(&self, index: usize) -> f32 {
        match self.map.get(index * 2..index * 2 + 2) {
            Some(b) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            None => 0.0,
        }
    }

    #[inline]
    fn at(&self, position: f64) -> f32 {
        if position < 0.0 {
            return 0.0;
        }
        let index = position.floor();
        let fraction = (position - index) as f32;
        let index = index as usize;
        let a = self.sample(index);
        a + (self.sample(index + 1) - a) * fraction
    }
}

struct Shared {
    pcm: ArcSwapOption<Pcm>,
    /// Bits du f64 : durée en secondes.
    duration: AtomicU64,
    /// Coupes triées et disjointes.
    cuts: ArcSwap<Vec<Interval>>,
    mode: AtomicU8,
    playing: AtomicBool,
    ended: AtomicBool,
    /// Bits du f64 : position en échantillons à 48 kHz.
    position: AtomicU64,
    seek: AtomicU64,
    volume: AtomicU64,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            pcm: ArcSwapOption::empty(),
            duration: AtomicU64::new(0f64.to_bits()),
            cuts: ArcSwap::from_pointee(Vec::new()),
            mode: AtomicU8::new(0),
            playing: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            position: AtomicU64::new(0f64.to_bits()),
            seek: AtomicU64::new(NO_SEEK),
            volume: AtomicU64::new(1f64.to_bits()),
        }
    }

    fn duration(&self) -> f64 {
        f64::from_bits(self.duration.load(Ordering::Relaxed))
    }
}

/// Ce qu'il faut faire à une position donnée de la lecture.
#[derive(Debug, PartialEq)]
enum Step {
    /// Continuer jusqu'à cet instant, puis réévaluer.
    Continue { until: f64 },
    Jump { to: f64 },
    End { at: f64 },
}

fn resolve(mode: Mode, cuts: &[Interval], t: f64, duration: f64) -> Step {
    match mode {
        Mode::Original => {
            if t >= duration {
                Step::End { at: duration }
            } else {
                Step::Continue { until: duration }
            }
        }
        Mode::Montage => {
            let index = cuts.partition_point(|c| c.1 <= t);
            match cuts.get(index) {
                Some(&(start, end)) if t >= start => {
                    if end >= duration - 0.02 {
                        // La dernière coupe va jusqu'au bout : on s'arrête juste avant.
                        Step::End { at: (start - 0.01).max(0.0) }
                    } else {
                        Step::Jump { to: end }
                    }
                }
                Some(&(start, _)) => Step::Continue { until: start },
                None if t >= duration => Step::End { at: duration },
                None => Step::Continue { until: duration },
            }
        }
        Mode::Cuts => {
            let index = cuts.partition_point(|c| c.1 <= t);
            match cuts.get(index) {
                Some(&(start, _)) if t < start => Step::Jump { to: start },
                Some(&(_, end)) => Step::Continue { until: end },
                None => Step::End { at: duration },
            }
        }
    }
}

/// État propre au fil audio, conservé entre deux rappels.
struct RenderState {
    fade_position: f64,
    fade_left: usize,
    gain: f32,
    was_playing: bool,
}

impl RenderState {
    fn new() -> RenderState {
        RenderState { fade_position: 0.0, fade_left: 0, gain: 0.0, was_playing: false }
    }
}

/// Produit `out.len() / channels` trames en avançant la position de lecture.
fn render(shared: &Shared, state: &mut RenderState, out: &mut [f32], channels: usize, step: f64, device_rate: f64) {
    let pcm = shared.pcm.load();
    let cuts = shared.cuts.load();
    let mode = Mode::from_u8(shared.mode.load(Ordering::Relaxed));
    let duration = shared.duration();
    let volume = f64::from_bits(shared.volume.load(Ordering::Relaxed)) as f32;
    let fade_len = ((FADE_SECONDS * device_rate) as usize).max(1);
    let ramp = 1.0 / fade_len as f32;

    let mut position = f64::from_bits(shared.position.load(Ordering::Acquire));
    let mut playing = shared.playing.load(Ordering::Acquire);
    let requested = shared.seek.swap(NO_SEEK, Ordering::AcqRel);
    if requested != NO_SEEK {
        if state.was_playing {
            state.fade_position = position;
            state.fade_left = fade_len;
        }
        position = f64::from_bits(requested);
        state.gain = 0.0;
    }
    if state.was_playing && !playing {
        state.fade_position = position;
        state.fade_left = fade_len;
        state.gain = 0.0;
    }

    let mut boundary = f64::NEG_INFINITY;
    for frame in out.chunks_mut(channels.max(1)) {
        if playing && position >= boundary {
            match resolve(mode, &cuts, position / SOURCE_RATE, duration) {
                Step::Continue { until } => boundary = until * SOURCE_RATE,
                Step::Jump { to } => {
                    state.fade_position = position;
                    state.fade_left = fade_len;
                    state.gain = 0.0;
                    position = to * SOURCE_RATE;
                    boundary = f64::NEG_INFINITY;
                }
                Step::End { at } => {
                    state.fade_position = position;
                    state.fade_left = fade_len;
                    state.gain = 0.0;
                    position = at * SOURCE_RATE;
                    playing = false;
                    shared.playing.store(false, Ordering::Release);
                    shared.ended.store(true, Ordering::Release);
                }
            }
        }
        let target = if playing { 1.0 } else { 0.0 };
        state.gain = if state.gain < target { (state.gain + ramp).min(target) } else { target };

        let mut value = 0.0;
        if let Some(pcm) = pcm.as_deref() {
            if state.gain > 0.0 {
                value = pcm.at(position) * state.gain;
            }
            if state.fade_left > 0 {
                value += pcm.at(state.fade_position) * (state.fade_left as f32 * ramp);
            }
        }
        if state.fade_left > 0 {
            state.fade_position += step;
            state.fade_left -= 1;
        }
        let value = (value * volume).clamp(-1.0, 1.0);
        frame.iter_mut().for_each(|s| *s = value);
        if playing {
            position += step;
        }
    }
    state.was_playing = playing;
    shared.position.store(position.to_bits(), Ordering::Release);
}

// --- sortie son ------------------------------------------------------------------------

fn fill_with<T: cpal::SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
    failed: Arc<AtomicBool>,
    convert: fn(f32) -> T,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let device_rate = config.sample_rate as f64;
    let step = SOURCE_RATE / device_rate;
    let mut state = RenderState::new();
    let mut buffer: Vec<f32> = Vec::new();
    device
        .build_output_stream::<T, _, _>(
            config,
            move |out: &mut [T], _| {
                buffer.resize(out.len(), 0.0);
                render(&shared, &mut state, &mut buffer, channels, step, device_rate);
                for (target, value) in out.iter_mut().zip(&buffer) {
                    *target = convert(*value);
                }
            },
            move |_| failed.store(true, Ordering::Release),
            None,
        )
        .map_err(|e| e.to_string())
}

fn open_stream(shared: &Arc<Shared>, failed: &Arc<AtomicBool>) -> Result<(cpal::Stream, String), String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("aucune sortie son")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let config = cpal::StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };
    let shared = shared.clone();
    let failed = failed.clone();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => fill_with::<f32>(&device, config, shared, failed, |v| v)?,
        cpal::SampleFormat::I16 => fill_with::<i16>(&device, config, shared, failed, |v| (v * 32767.0) as i16)?,
        cpal::SampleFormat::U16 => {
            fill_with::<u16>(&device, config, shared, failed, |v| ((v + 1.0) * 32767.5) as u16)?
        }
        other => return Err(format!("format de sortie non géré : {other:?}")),
    };
    stream.play().map_err(|e| e.to_string())?;
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_default();
    Ok((stream, name))
}

/// Sans sortie son, un fil fait avancer l'horloge en temps réel.
struct SilentClock {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SilentClock {
    fn start(shared: Arc<Shared>) -> SilentClock {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            let mut state = RenderState::new();
            let mut buffer = vec![0f32; 4800];
            let mut last = Instant::now();
            while !flag.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(5));
                let frames = ((last.elapsed().as_secs_f64() * SOURCE_RATE) as usize).min(buffer.len());
                if frames > 0 {
                    last = Instant::now();
                    render(&shared, &mut state, &mut buffer[..frames], 1, 1.0, SOURCE_RATE);
                }
            }
        });
        SilentClock { stop, thread: Some(thread) }
    }
}

impl Drop for SilentClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// --- vidéo ------------------------------------------------------------------------------

#[derive(Clone)]
pub struct VideoSource {
    pub path: PathBuf,
    pub width: usize,
    pub height: usize,
    pub fps: f64,
}

pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

struct VideoShared {
    want: AtomicI64,
    generation: AtomicU64,
    source: Mutex<Option<VideoSource>>,
    latest: Mutex<Option<VideoFrame>>,
    stop: AtomicBool,
}

struct Decoder {
    child: Child,
    stdout: std::process::ChildStdout,
    next: i64,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_decoder(source: &VideoSource, want: i64) -> Option<Decoder> {
    let ffmpeg = find_tool("ffmpeg").ok()?;
    // Un quart d'image avant : la seule image gardée après le saut est bien `want`.
    let at = ((want as f64 - 0.25) / source.fps).max(0.0);
    let mut child = hidden_command(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-v", "error", "-ss", &format!("{at:.6}"), "-i"])
        .arg(&source.path)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some(Decoder { child, stdout, next: want })
}

fn video_loop(shared: Arc<VideoShared>, ctx: egui::Context) {
    let mut generation = u64::MAX;
    let mut source: Option<VideoSource> = None;
    let mut decoder: Option<Decoder> = None;
    let mut shown: i64 = -1;
    let mut end_at: Option<i64> = None;
    let mut buffer: Vec<u8> = Vec::new();

    while !shared.stop.load(Ordering::Acquire) {
        let current = shared.generation.load(Ordering::Acquire);
        if current != generation {
            generation = current;
            decoder = None;
            shown = -1;
            end_at = None;
            source = shared.source.lock().ok().and_then(|s| s.clone());
        }
        let Some(video) = source.as_ref() else {
            thread::park_timeout(Duration::from_millis(50));
            continue;
        };
        let want = shared.want.load(Ordering::Acquire);
        if want < 0 || want == shown || end_at.is_some_and(|end| want >= end) {
            thread::park_timeout(Duration::from_millis(20));
            continue;
        }

        let window = (video.fps * 2.0).ceil() as i64;
        let restart = match decoder.as_ref() {
            None => true,
            Some(d) => want < d.next || want > d.next + window,
        };
        if restart {
            end_at = None;
            decoder = spawn_decoder(video, want);
            if decoder.is_none() {
                thread::sleep(Duration::from_millis(250));
                continue;
            }
        }

        let size = video.width * video.height * 4;
        buffer.resize(size, 0);
        let Some(d) = decoder.as_mut() else { continue };
        if d.stdout.read_exact(&mut buffer).is_err() {
            end_at = Some(d.next);
            decoder = None;
            continue;
        }
        d.next += 1;
        // Le décodeur a pu prendre du retard pendant cette lecture : on garde l'image seulement
        // si c'est la bonne (ou la plus proche qu'on pourra obtenir).
        let index = d.next - 1;
        let target = shared.want.load(Ordering::Acquire);
        if index >= target && shared.generation.load(Ordering::Acquire) == generation {
            let rgba = std::mem::take(&mut buffer);
            if let Ok(mut latest) = shared.latest.lock() {
                *latest = Some(VideoFrame { width: video.width, height: video.height, rgba });
            }
            shown = index;
            ctx.request_repaint();
        }
    }
}

// --- lecteur ----------------------------------------------------------------------------

pub struct Media {
    pub audio: Option<PathBuf>,
    pub video: Option<VideoSource>,
    pub duration: f64,
}

pub struct Player {
    shared: Arc<Shared>,
    failed: Arc<AtomicBool>,
    stream: Option<cpal::Stream>,
    silent: Option<SilentClock>,
    pub device_name: Option<String>,
    last_attempt: Instant,
    video: Arc<VideoShared>,
    video_thread: Option<JoinHandle<()>>,
    fps: f64,
    sent_frame: i64,
}

impl Player {
    pub fn new(ctx: &egui::Context) -> Player {
        let shared = Arc::new(Shared::new());
        let video = Arc::new(VideoShared {
            want: AtomicI64::new(-1),
            generation: AtomicU64::new(0),
            source: Mutex::new(None),
            latest: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let worker = video.clone();
        let repaint = ctx.clone();
        let video_thread = thread::Builder::new()
            .name("video".to_string())
            .spawn(move || video_loop(worker, repaint))
            .ok();
        let mut player = Player {
            shared,
            failed: Arc::new(AtomicBool::new(false)),
            stream: None,
            silent: None,
            device_name: None,
            last_attempt: Instant::now(),
            video,
            video_thread,
            fps: 25.0,
            sent_frame: -1,
        };
        player.open_output();
        player
    }

    fn open_output(&mut self) {
        self.last_attempt = Instant::now();
        self.failed.store(false, Ordering::Release);
        self.stream = None;
        match open_stream(&self.shared, &self.failed) {
            Ok((stream, name)) => {
                self.silent = None;
                self.stream = Some(stream);
                self.device_name = Some(name);
            }
            Err(_) => {
                self.device_name = None;
                if self.silent.is_none() {
                    self.silent = Some(SilentClock::start(self.shared.clone()));
                }
            }
        }
    }

    /// À appeler à chaque image : rouvre la sortie son si elle a disparu (casque débranché…).
    pub fn poll(&mut self) {
        let lost = self.failed.load(Ordering::Acquire);
        let retry = self.stream.is_none() || lost;
        if retry && self.last_attempt.elapsed() > Duration::from_secs(2) {
            self.open_output();
        }
    }

    pub fn load(&mut self, media: Option<Media>) -> Result<(), String> {
        self.pause();
        let mut problem = None;
        let mut video = None;
        match media {
            Some(media) => {
                let pcm = match media.audio.as_deref().map(Pcm::open) {
                    Some(Ok(pcm)) => Some(Arc::new(pcm)),
                    Some(Err(message)) => {
                        problem = Some(message);
                        None
                    }
                    None => None,
                };
                self.shared.pcm.store(pcm);
                self.shared.duration.store(media.duration.to_bits(), Ordering::Release);
                video = media.video;
            }
            None => {
                self.shared.pcm.store(None);
                self.shared.duration.store(0f64.to_bits(), Ordering::Release);
            }
        }
        self.fps = video.as_ref().map(|v| v.fps).unwrap_or(25.0);
        self.shared.ended.store(false, Ordering::Release);
        self.seek(0.0);
        if let Ok(mut slot) = self.video.source.lock() {
            *slot = video;
        }
        if let Ok(mut latest) = self.video.latest.lock() {
            *latest = None;
        }
        self.sent_frame = -1;
        self.video.want.store(-1, Ordering::Release);
        self.video.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(thread) = &self.video_thread {
            thread.thread().unpark();
        }
        problem.map_or(Ok(()), Err)
    }

    pub fn set_cuts(&self, cuts: Vec<Interval>) {
        self.shared.cuts.store(Arc::new(cuts));
    }

    pub fn set_mode(&self, mode: Mode) {
        self.shared.mode.store(mode.as_u8(), Ordering::Release);
    }

    pub fn mode(&self) -> Mode {
        Mode::from_u8(self.shared.mode.load(Ordering::Acquire))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    pub fn duration(&self) -> f64 {
        self.shared.duration()
    }

    pub fn play(&self) {
        if self.shared.ended.swap(false, Ordering::AcqRel) {
            self.seek(0.0);
        }
        self.shared.playing.store(true, Ordering::Release);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Release);
    }

    pub fn toggle(&self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn seek(&self, seconds: f64) {
        let target = seconds.clamp(0.0, self.duration().max(0.0)) * SOURCE_RATE;
        self.shared.ended.store(false, Ordering::Release);
        self.shared.seek.store(target.to_bits(), Ordering::Release);
    }

    /// Position en secondes, y compris un saut demandé mais pas encore pris en compte.
    pub fn position(&self) -> f64 {
        let pending = self.shared.seek.load(Ordering::Acquire);
        let samples = if pending != NO_SEEK { pending } else { self.shared.position.load(Ordering::Acquire) };
        f64::from_bits(samples) / SOURCE_RATE
    }

    /// Demande au décodeur l'image correspondant à la position courante.
    pub fn sync_video(&mut self) {
        let frame = (self.position() * self.fps + 1e-6).floor() as i64;
        if frame != self.sent_frame {
            self.sent_frame = frame;
            self.video.want.store(frame, Ordering::Release);
            if let Some(thread) = &self.video_thread {
                thread.thread().unpark();
            }
        }
    }

    pub fn take_frame(&self) -> Option<VideoFrame> {
        self.video.latest.lock().ok()?.take()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.video.stop.store(true, Ordering::Release);
        if let Some(thread) = self.video_thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn montage_skips_cuts_and_stops_on_a_final_cut() {
        let cuts = [(1.0, 2.0), (5.0, 10.0)];
        assert_eq!(resolve(Mode::Montage, &cuts, 0.5, 10.0), Step::Continue { until: 1.0 });
        assert_eq!(resolve(Mode::Montage, &cuts, 1.0, 10.0), Step::Jump { to: 2.0 });
        assert_eq!(resolve(Mode::Montage, &cuts, 2.0, 10.0), Step::Continue { until: 5.0 });
        assert_eq!(resolve(Mode::Montage, &cuts, 5.0, 10.0), Step::End { at: 4.99 });
    }

    #[test]
    fn cuts_mode_plays_only_what_is_removed() {
        let cuts = [(1.0, 2.0), (5.0, 6.0)];
        assert_eq!(resolve(Mode::Cuts, &cuts, 0.0, 10.0), Step::Jump { to: 1.0 });
        assert_eq!(resolve(Mode::Cuts, &cuts, 1.5, 10.0), Step::Continue { until: 2.0 });
        assert_eq!(resolve(Mode::Cuts, &cuts, 2.0, 10.0), Step::Jump { to: 5.0 });
        assert_eq!(resolve(Mode::Cuts, &cuts, 6.0, 10.0), Step::End { at: 10.0 });
    }

    #[test]
    fn original_mode_plays_to_the_end() {
        assert_eq!(resolve(Mode::Original, &[(1.0, 2.0)], 1.5, 10.0), Step::Continue { until: 10.0 });
        assert_eq!(resolve(Mode::Original, &[], 10.0, 10.0), Step::End { at: 10.0 });
    }

    #[test]
    fn render_advances_the_clock_only_while_playing_and_jumps_exactly() {
        let shared = Shared::new();
        shared.duration.store(10f64.to_bits(), Ordering::Release);
        shared.cuts.store(Arc::new(vec![(0.01, 0.02)]));
        let mut state = RenderState::new();
        let mut buffer = vec![0f32; 480];
        render(&shared, &mut state, &mut buffer, 1, 1.0, SOURCE_RATE);
        assert_eq!(f64::from_bits(shared.position.load(Ordering::Acquire)), 0.0);

        shared.playing.store(true, Ordering::Release);
        render(&shared, &mut state, &mut buffer, 1, 1.0, SOURCE_RATE);
        // 480 trames : on atteint 0,01 s pile, le saut est pris à la trame suivante.
        assert_eq!(f64::from_bits(shared.position.load(Ordering::Acquire)), 480.0);
        render(&shared, &mut state, &mut buffer, 1, 1.0, SOURCE_RATE);
        assert!((f64::from_bits(shared.position.load(Ordering::Acquire)) - 960.0 - 480.0).abs() < 1e-6);
    }

    #[test]
    fn a_seek_requested_during_playback_wins_over_the_running_position() {
        let shared = Shared::new();
        shared.duration.store(10f64.to_bits(), Ordering::Release);
        shared.playing.store(true, Ordering::Release);
        let mut state = RenderState::new();
        let mut buffer = vec![0f32; 480];
        render(&shared, &mut state, &mut buffer, 1, 1.0, SOURCE_RATE);
        shared.seek.store((5.0 * SOURCE_RATE).to_bits(), Ordering::Release);
        render(&shared, &mut state, &mut buffer, 1, 1.0, SOURCE_RATE);
        assert_eq!(f64::from_bits(shared.position.load(Ordering::Acquire)), 5.0 * SOURCE_RATE + 480.0);
    }
}
