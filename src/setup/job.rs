//! Avancement d'une installation : état partagé avec la fenêtre, journal, processus, téléchargements.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui;
use sha2::{Digest, Sha256};

use super::install::Report;

const KEPT_LINES: usize = 400;

/// Pourquoi une installation s'arrête avant la fin.
pub enum Stop {
    Cancelled,
    Failed(String),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Stop::Failed(message)
    }
}

pub type Outcome<T = ()> = Result<T, Stop>;

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Pending,
    Running,
    Done,
    Failed,
}

pub struct StepLine {
    pub label: String,
    pub status: Status,
}

pub enum End {
    Success(Report),
    Failure(String),
    Cancelled,
}

#[derive(Default)]
pub struct Shared {
    pub steps: Vec<StepLine>,
    pub detail: String,
    pub fraction: Option<f32>,
    pub log: VecDeque<String>,
    pub end: Option<End>,
    file: Option<File>,
}

/// Dossier dont la taille mesure un téléchargement que le processus ne chiffre pas.
pub struct Watch {
    pub folder: PathBuf,
    pub expected: u64,
}

#[derive(Clone)]
pub struct Job {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
}

pub fn log_path() -> PathBuf {
    std::env::temp_dir().join("Euhpen-Setup.log")
}

impl Job {
    pub fn new(ctx: egui::Context, labels: Vec<String>) -> Self {
        let steps = labels.into_iter().map(|label| StepLine { label, status: Status::Pending }).collect();
        let shared = Shared { steps, file: File::create(log_path()).ok(), ..Default::default() };
        Job { shared: Arc::new(Mutex::new(shared)), cancel: Arc::new(AtomicBool::new(false)), ctx }
    }

    pub fn state(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn cancelling(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Outcome {
        if self.cancelling() {
            Err(Stop::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        let mut state = self.state();
        if let Some(file) = state.file.as_mut() {
            let _ = writeln!(file, "{line}");
        }
        state.log.push_back(line);
        if state.log.len() > KEPT_LINES {
            state.log.pop_front();
        }
        drop(state);
        self.ctx.request_repaint();
    }

    pub fn set_step(&self, index: usize, status: Status) {
        let mut state = self.state();
        if status == Status::Running {
            state.detail.clear();
            state.fraction = None;
        }
        if let Some(step) = state.steps.get_mut(index) {
            step.status = status;
        }
        drop(state);
        self.ctx.request_repaint();
    }

    pub fn progress(&self, fraction: Option<f32>, detail: impl Into<String>) {
        let mut state = self.state();
        state.fraction = fraction;
        state.detail = detail.into();
        drop(state);
        self.ctx.request_repaint();
    }

    pub fn finish(&self, end: End) {
        self.state().end = Some(end);
        self.ctx.request_repaint();
    }

    fn drain(&self, stream: impl Read + Send + 'static, kept: Arc<Mutex<Vec<String>>>) -> JoinHandle<()> {
        let job = self.clone();
        thread::spawn(move || {
            for chunk in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                // Une barre de progression réécrit sa ligne avec \r : seule la dernière version compte.
                let last = chunk.split(|byte| *byte == b'\r').rfind(|part| !part.is_empty()).unwrap_or(&[]);
                let line = String::from_utf8_lossy(last).trim_end().to_string();
                if !line.is_empty() {
                    job.log(line.clone());
                    if let Ok(mut lines) = kept.lock() {
                        lines.push(line);
                    }
                }
            }
        })
    }

    /// Lance un programme et attend sa fin. Renvoie sa sortie standard.
    pub fn run(&self, mut command: Command, watch: Option<Watch>) -> Outcome<String> {
        self.check()?;
        let program = PathBuf::from(command.get_program());
        let name = program.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let arguments: Vec<_> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        // Un script PowerShell passé en argument encombrerait le journal.
        let shown: String = arguments.join(" ").chars().take(300).collect();
        self.log(format!("> {} {shown}", program.display()));
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| format!("{name} n'a pas pu démarrer : {e}"))?;

        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let mut readers = Vec::new();
        if let Some(stream) = child.stdout.take() {
            readers.push(self.drain(stream, out.clone()));
        }
        if let Some(stream) = child.stderr.take() {
            readers.push(self.drain(stream, err.clone()));
        }

        let mut measured = Instant::now();
        let status = loop {
            if self.cancelling() {
                let _ = child.kill();
                let _ = child.wait();
                for reader in readers {
                    let _ = reader.join();
                }
                return Err(Stop::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => return Err(Stop::Failed(format!("{name} : {e}"))),
            }
            if let Some(watch) = watch.as_ref().filter(|_| measured.elapsed() >= Duration::from_secs(1)) {
                measured = Instant::now();
                let size = dir_size(&watch.folder);
                // Jamais 100 % sur une estimation : la taille réelle peut dépasser l'attendu.
                let fraction = (size as f64 / watch.expected.max(1) as f64).min(0.97) as f32;
                self.progress(Some(fraction), format!("{} sur environ {}", human_size(size), human_size(watch.expected)));
            }
            thread::sleep(Duration::from_millis(100));
        };
        for reader in readers {
            let _ = reader.join();
        }
        let stdout = out.lock().map(|lines| lines.join("\n")).unwrap_or_default();
        if status.success() {
            return Ok(stdout);
        }
        // La fenêtre montre l'essentiel ; le journal garde tout.
        let last: String = err.lock().ok().and_then(|lines| lines.last().cloned()).unwrap_or_default().chars().take(220).collect();
        Err(Stop::Failed(format!("{name} s'est arrêté (code {}) : {last}", status.code().unwrap_or(-1))))
    }

    /// Télécharge un fichier et contrôle son empreinte.
    pub fn download(&self, url: &str, sha256: &str, target: &Path) -> Outcome {
        self.check()?;
        self.log(format!("Téléchargement de {url}"));
        let network = |e: &dyn std::fmt::Display| Stop::Failed(format!("Téléchargement impossible ({url}) : {e}. Vérifiez la connexion à Internet."));
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .into();
        let mut response = agent.get(url).call().map_err(|e| network(&e))?;
        let total = response.body().content_length();
        let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();

        let part = target.with_extension("part");
        let mut file = File::create(&part).map_err(|e| format!("{} : {e}", part.display()))?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        let mut received = 0u64;
        let mut shown = Instant::now();
        loop {
            if self.cancelling() {
                drop(file);
                let _ = fs::remove_file(&part);
                return Err(Stop::Cancelled);
            }
            let count = reader.read(&mut buffer).map_err(|e| network(&e))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
            file.write_all(&buffer[..count]).map_err(|e| format!("{} : {e}", part.display()))?;
            received += count as u64;
            if shown.elapsed() >= Duration::from_millis(200) {
                shown = Instant::now();
                match total {
                    Some(total) if total > 0 => {
                        self.progress(Some(received as f32 / total as f32), format!("{} sur {}", human_size(received), human_size(total)))
                    }
                    _ => self.progress(None, human_size(received)),
                }
            }
        }
        drop(file);
        let found = format!("{:x}", digest.finalize());
        if !found.eq_ignore_ascii_case(sha256) {
            let _ = fs::remove_file(&part);
            return Err(Stop::Failed(format!(
                "Le fichier téléchargé n'est pas celui attendu ({url}). Réessayez : s'il s'agit d'un incident passager, cela suffira."
            )));
        }
        fs::rename(&part, target).map_err(|e| format!("{} : {e}", target.display()))?;
        Ok(())
    }
}

pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut folders = vec![path.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else { continue };
            if metadata.is_dir() {
                folders.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

pub fn human_size(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} Go", bytes / GB).replace('.', ",")
    } else {
        format!("{:.0} Mo", bytes / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_written_the_french_way() {
        assert_eq!(human_size(1_600_000_000), "1,6 Go");
        assert_eq!(human_size(115_000_000), "115 Mo");
    }

    #[test]
    fn folder_size_counts_nested_files() {
        let folder = std::env::temp_dir().join(format!("euhpen-size-{}", std::process::id()));
        fs::create_dir_all(folder.join("a/b")).unwrap();
        fs::write(folder.join("one"), [0u8; 10]).unwrap();
        fs::write(folder.join("a/b/two"), [0u8; 5]).unwrap();
        assert_eq!(dir_size(&folder), 15);
        fs::remove_dir_all(&folder).unwrap();
    }
}
