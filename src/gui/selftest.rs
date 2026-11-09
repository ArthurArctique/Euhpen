//! Autotest de l'interface, sans intervention : `EUHPEN_SELFTEST=<dossier>` et
//! `EUHPEN_SELFTEST_FILES=<fichier>;<fichier>`. L'application charge les fichiers, joue, prend des
//! captures, exporte puis se ferme. Résultat dans `<dossier>/selftest.log`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;

use super::player::Mode;
use super::state::{App, Dialog, ProxyStatus, Status, TrStatus};

enum Step {
    WaitReady,
    Settle(&'static str),
    Play,
    SeekMiddle,
    Toggle,
    OpenDialog(u8),
    Export,
    Finish,
}

pub struct SelfTest {
    folder: PathBuf,
    files: Vec<String>,
    wants_transcript: bool,
    step: Step,
    since: Instant,
    started: bool,
    shot_asked: bool,
    log: String,
    last_sample: Instant,
    samples: Vec<(f64, f64)>,
    plan_before: Option<usize>,
    pub frames: u64,
}

impl SelfTest {
    pub fn from_env() -> Option<SelfTest> {
        let folder = PathBuf::from(std::env::var_os("EUHPEN_SELFTEST")?);
        let _ = std::fs::create_dir_all(&folder);
        let files = std::env::var("EUHPEN_SELFTEST_FILES").unwrap_or_default().split(';').filter(|s| !s.is_empty()).map(String::from).collect();
        Some(SelfTest {
            folder,
            files,
            wants_transcript: std::env::var_os("EUHPEN_SELFTEST_HES").is_some(),
            step: Step::WaitReady,
            since: Instant::now(),
            started: false,
            shot_asked: false,
            log: String::new(),
            last_sample: Instant::now(),
            samples: Vec::new(),
            plan_before: None,
            frames: 0,
        })
    }

    fn note(&mut self, text: impl AsRef<str>) {
        let _ = writeln!(self.log, "[{:7.2}] {}", self.since.elapsed().as_secs_f64(), text.as_ref());
        let _ = std::fs::write(self.folder.join("selftest.log"), &self.log);
    }

    fn go(&mut self, step: Step) {
        self.step = step;
        self.since = Instant::now();
        self.shot_asked = false;
    }

    fn shot(&mut self, ctx: &egui::Context, name: &str) {
        let path = self.folder.join(format!("{name}.png"));
        ctx.request_screenshot(move |image| {
            let _ = save_png(&path, image.size[0], image.size[1], image.as_raw());
        });
    }

    fn elapsed(&self) -> f64 {
        self.since.elapsed().as_secs_f64()
    }

    pub fn tick(&mut self, app: &mut App, ctx: &egui::Context) {
        self.frames += 1;
        ctx.request_repaint_after(Duration::from_millis(30));
        if !self.started {
            self.started = true;
            let files = self.files.clone();
            app.add_paths(&files, false);
            if self.wants_transcript {
                app.settings.hesitations = true;
            }
            self.note(format!("fichiers : {}", self.files.len()));
        }
        match self.step {
            Step::WaitReady => {
                let Some(item) = app.current() else { return };
                if let Status::Error(message) = &item.status {
                    let message = message.clone();
                    self.note(format!("ERREUR analyse : {message}"));
                    self.go(Step::Finish);
                    return;
                }
                let proxy_ok = matches!(item.proxy, ProxyStatus::Ready(_));
                let tr_ok = !self.wants_transcript || item.tr == TrStatus::Ready;
                if let ProxyStatus::Error(message) = &item.proxy {
                    let message = message.clone();
                    self.note(format!("ERREUR aperçu : {message}"));
                    self.go(Step::Finish);
                    return;
                }
                if let TrStatus::Error(message) = &item.tr {
                    let message = message.clone();
                    self.note(format!("ERREUR transcription : {message}"));
                    self.go(Step::Finish);
                    return;
                }
                if proxy_ok && tr_ok && item.analysis.is_some() && app.plan.is_some() {
                    self.note("prêt");
                    if let Some(hw) = app.hardware.clone() {
                        self.note(format!("calcul : {} {} {:?}", hw.device, hw.name, hw.warnings));
                    }
                    if let Some(run) = app.last_run.clone() {
                        self.note(format!("dernière transcription : {run:?}"));
                    }
                    self.go(Step::Settle("01-pret"));
                } else if self.elapsed() > 900.0 {
                    self.note("ERREUR : délai dépassé");
                    self.go(Step::Finish);
                }
            }
            Step::Settle(name) => {
                if self.elapsed() > 1.2 && !self.shot_asked {
                    self.shot_asked = true;
                    self.shot(ctx, name);
                }
                if self.elapsed() > 1.8 {
                    app.set_mode(Mode::Montage);
                    app.player.seek(0.0);
                    app.player.play();
                    self.samples.clear();
                    self.note("lecture");
                    self.go(Step::Play);
                }
            }
            Step::Play => {
                if self.last_sample.elapsed() >= Duration::from_millis(100) {
                    self.last_sample = Instant::now();
                    self.samples.push((self.elapsed(), app.player.position()));
                }
                if self.elapsed() > 4.0 && !self.shot_asked {
                    self.shot_asked = true;
                    self.shot(ctx, "02-lecture");
                }
                if self.elapsed() > 4.5 {
                    app.player.pause();
                    self.report_playback(app);
                    let middle = app.player.duration() / 2.0;
                    app.player.seek(middle);
                    self.note(format!("saut à {middle:.2}"));
                    self.go(Step::SeekMiddle);
                }
            }
            Step::SeekMiddle => {
                if self.elapsed() > 1.2 && !self.shot_asked {
                    self.shot_asked = true;
                    app.timeline.zoom_at(app.player.position(), 0.05, app.player.duration());
                    self.shot(ctx, "03-saut");
                }
                if self.elapsed() > 1.8 {
                    let frames = app.texture.is_some();
                    self.note(format!("image affichée : {frames}, position {:.3}", app.player.position()));
                    self.plan_before = app.plan.as_ref().map(|p| p.removed.len());
                    let target = app.plan.as_ref().and_then(|p| p.removed.first()).map(|p| (p.start + p.end) / 2.0);
                    if let Some(t) = target {
                        app.toggle_at(t, None);
                    }
                    self.go(Step::Toggle);
                }
            }
            Step::Toggle => {
                if self.elapsed() > 0.3 {
                    let after = app.plan.as_ref().map(|p| p.removed.len());
                    self.note(format!("coupes avant/après inversion : {:?} / {:?}", self.plan_before, after));
                    app.clear_overrides();
                    self.go(Step::OpenDialog(0));
                }
            }
            Step::OpenDialog(which) => {
                if !self.shot_asked && self.elapsed() < 0.1 {
                    match which {
                        0 => app.dialog = Dialog::Help,
                        _ => app.open_system(),
                    }
                }
                if self.elapsed() > 0.8 && !self.shot_asked {
                    self.shot_asked = true;
                    self.shot(ctx, if which == 0 { "04-aide" } else { "05-systeme" });
                }
                if self.elapsed() > 1.3 {
                    app.dialog = Dialog::None;
                    if which == 0 {
                        self.go(Step::OpenDialog(1));
                    } else {
                        app.prefs.output_dir = Some(self.folder.to_string_lossy().into_owned());
                        app.run_export();
                        self.go(Step::Export);
                    }
                }
            }
            Step::Export => {
                if self.elapsed() > 0.8 && !self.shot_asked {
                    self.shot_asked = true;
                    self.shot(ctx, "06-export");
                }
                if self.elapsed() > 1.3 {
                    match &app.dialog {
                        Dialog::Export(result) => {
                            let summary: Vec<String> = result.files.iter().map(|f| f.path.clone()).collect();
                            let sequences = result.sequences.len();
                            self.note(format!("export : {sequences} séquence(s), fichiers {summary:?}"));
                        }
                        _ => self.note("ERREUR : pas de résultat d'export"),
                    }
                    app.dialog = Dialog::None;
                    self.go(Step::Finish);
                }
            }
            Step::Finish => {
                if self.elapsed() > 0.2 {
                    let frames = self.frames;
                    self.note(format!("fin, {frames} images"));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn report_playback(&mut self, app: &App) {
        let samples = std::mem::take(&mut self.samples);
        let cuts: Vec<(f64, f64)> = app.plan.as_ref().map(|p| p.removed.iter().map(|c| (c.start, c.end)).collect()).unwrap_or_default();
        let inside = samples.iter().filter(|(_, t)| cuts.iter().any(|c| *t > c.0 + 0.03 && *t < c.1 - 0.03)).count();
        let backwards = samples.windows(2).filter(|w| w[1].1 < w[0].1).count();
        let first = samples.first().copied().unwrap_or_default();
        let last = samples.last().copied().unwrap_or_default();
        self.note(format!(
            "lecture : {} mesures, position {:.3} → {:.3} en {:.3} s, retours arrière {backwards}, mesures dans une coupe {inside}",
            samples.len(),
            first.1,
            last.1,
            last.0 - first.0
        ));
    }
}

/// PNG sans compression : suffit pour des captures de contrôle, sans dépendance.
pub(crate) fn save_png(path: &Path, width: usize, height: usize, rgba: &[u8]) -> std::io::Result<()> {
    fn crc32(parts: &[&[u8]]) -> u32 {
        let mut crc = !0u32;
        for part in parts {
            for byte in *part {
                crc ^= *byte as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { 0xedb8_8320 ^ (crc >> 1) } else { crc >> 1 };
                }
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    }

    let mut raw = Vec::with_capacity((width * 3 + 1) * height);
    for row in rgba.chunks_exact(width * 4) {
        raw.push(0);
        for pixel in row.chunks_exact(4) {
            raw.extend_from_slice(&pixel[..3]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        zlib.push(u8::from(blocks.peek().is_none()));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut header = Vec::new();
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    std::fs::File::create(path)?.write_all(&out)
}
