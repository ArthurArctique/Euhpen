//! Client du processus Python qui exécute le modèle (Whisper + détection des hésitations).
//!
//! Le modèle reste chargé entre deux fichiers. On parle au processus en lignes JSON : une
//! demande par ligne sur son entrée standard, des messages d'avancement sur sa sortie standard.
//! L'arrêter (annulation) libère aussitôt la mémoire graphique, et un plantage de PyTorch ne
//! fait pas tomber l'interface.
//!
//! Rust décode l'audio (mono 16 kHz, flottants 32 bits) dans un fichier temporaire ; le
//! processus le lit, transcrit, et écrit `words-*.json` dans le dossier de cache du fichier.

use crate::analysis::Analysis;
use crate::audio::decode_mono_to_file;
use crate::cache::{analysis_dir, cache_dir};
use crate::probe::hidden_command;
use crate::transcript::{cached_transcript, project_root, Transcript};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

/// Appareil de calcul rapporté par le processus (ou estimé sans charger PyTorch).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hardware {
    /// "cuda" | "mps" | "cpu"
    pub device: String,
    pub name: String,
    pub memory_gb: Option<f64>,
    pub compute_type: String,
    pub torch_version: Option<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

pub fn worker_log_path() -> PathBuf {
    cache_dir().join("worker.log")
}

/// L'interpréteur Python de l'environnement du modèle.
pub fn python_exe() -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os("EUHPEN_PYTHON").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let root = project_root().ok_or("dossier d'Euhpen introuvable (variable EUHPEN_ROOT).")?;
    let venv = root.join(".venv");
    let python = if cfg!(windows) { venv.join("Scripts").join("python.exe") } else { venv.join("bin").join("python") };
    if python.is_file() {
        Ok(python)
    } else {
        Err(format!(
            "L'environnement Python du modèle est introuvable ({}). Relancez l'installateur d'Euhpen en cochant la détection des hésitations. Avec le code source, relancez le lanceur (si vous aviez refusé cette installation, supprimez d'abord le fichier .euhpen-install).",
            python.display()
        ))
    }
}

/// La détection des hésitations est-elle installée ?
pub fn hesitations_available() -> Result<(), String> {
    let python = python_exe()?;
    let code = "import importlib.util as u\nprint(','.join(m for m in ('torch','transformers','accelerate') if u.find_spec(m) is None))";
    let output = hidden_command(python).args(["-c", code]).output().map_err(|e| format!("Python n'a pas pu démarrer : {e}"))?;
    if !output.status.success() {
        return Err(format!("Python a échoué : {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let missing = String::from_utf8_lossy(&output.stdout).trim().replace(',', ", ");
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "La détection des hésitations n'est pas installée (manque : {missing}). Relancez l'installateur d'Euhpen (ou le lanceur, avec le code source) et acceptez son installation."
        ))
    }
}

/// Estimation sans charger PyTorch, avant que le processus ne donne la réponse exacte.
pub fn quick_hardware() -> Hardware {
    let cpu = || Hardware { device: "cpu".into(), name: "processeur".into(), compute_type: "float32".into(), ..Default::default() };
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Hardware { device: "mps".into(), name: "Apple arm64 (Metal)".into(), compute_type: "float16".into(), ..Default::default() };
    }
    let Ok(output) = hidden_command("nvidia-smi").args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]).output() else {
        return cpu();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some((name, memory)) = text.lines().next().and_then(|l| l.rsplit_once(',')) else {
        return cpu();
    };
    match memory.trim().parse::<f64>() {
        Ok(mb) => Hardware {
            device: "cuda".into(),
            name: name.trim().to_string(),
            memory_gb: Some((mb / 1024.0 * 10.0).round() / 10.0),
            compute_type: "float16".into(),
            ..Default::default()
        },
        Err(_) => cpu(),
    }
}

enum Event {
    Ready(Hardware),
    Message(Value),
    Exited,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Event>,
}

impl Worker {
    fn spawn() -> Result<Worker, String> {
        let root = project_root().ok_or("dossier d'Euhpen introuvable (variable EUHPEN_ROOT).")?;
        let python = python_exe()?;
        fs::create_dir_all(cache_dir()).map_err(|e| e.to_string())?;
        let log_path = worker_log_path();
        let mut log = fs::OpenOptions::new().create(true).append(true).open(&log_path).map_err(|e| format!("{} : {e}", log_path.display()))?;
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(log, "\n--- {now} démarrage du processus de transcription");
        let mut child = hidden_command(python)
            .args(["-m", "euhpen.worker"])
            .current_dir(&root)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONPATH", root.join("python"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|e| format!("le processus de transcription n'a pas pu démarrer : {e}"))?;
        let stdin = child.stdin.take().ok_or("entrée du processus indisponible")?;
        let stdout = child.stdout.take().ok_or("sortie du processus indisponible")?;
        let (sender, events) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let event = if message["type"] == "ready" {
                    Event::Ready(serde_json::from_value(message["hardware"].clone()).unwrap_or_default())
                } else {
                    Event::Message(message)
                };
                if sender.send(event).is_err() {
                    return;
                }
            }
            let _ = sender.send(Event::Exited);
        });
        Ok(Worker { child, stdin, events })
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Résultat d'une transcription et de ce qu'elle a réellement utilisé comme matériel.
pub struct JobOutcome {
    pub transcript: Transcript,
    pub cached: bool,
    pub gpu_memory_mb: Option<f64>,
    pub encoder_device: Option<String>,
}

pub struct JobRequest<'a> {
    pub analysis: &'a Analysis,
    pub language: &'a str,
    pub model: &'a str,
    pub force: bool,
}

/// Supprime l'audio temporaire quelle que soit l'issue.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Un processus de transcription démarré à la demande et réutilisé entre les fichiers.
#[derive(Default)]
pub struct TranscribeSession {
    worker: Option<Worker>,
    jobs: u64,
    pub hardware: Option<Hardware>,
}

impl TranscribeSession {
    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.kill();
        }
    }

    fn submit(&mut self, message: &Value) -> Result<(), String> {
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn()?);
        }
        let worker = self.worker.as_mut().unwrap();
        let sent = writeln!(worker.stdin, "{message}").and_then(|()| worker.stdin.flush());
        if sent.is_err() {
            // Le processus était mort : on repart d'un neuf.
            self.stop();
            let worker = self.worker.insert(Worker::spawn()?);
            writeln!(worker.stdin, "{message}")
                .and_then(|()| worker.stdin.flush())
                .map_err(|e| format!("le processus de transcription ne répond pas : {e}"))?;
        }
        Ok(())
    }

    /// Transcrit un fichier analysé. `cancelled` tue le processus (mémoire graphique libérée).
    pub fn run(
        &mut self,
        request: JobRequest,
        progress: &mut dyn FnMut(f64, &str),
        cancelled: &dyn Fn() -> bool,
        on_hardware: &mut dyn FnMut(&Hardware),
    ) -> Result<JobOutcome, String> {
        let JobRequest { analysis, language, model, force } = request;
        if !force {
            if let Some(transcript) = cached_transcript(&analysis.fingerprint, model, language) {
                return Ok(JobOutcome { transcript, cached: true, gpu_memory_mb: None, encoder_device: None });
            }
        }
        self.jobs += 1;
        let job = format!("job-{}", self.jobs);
        let folder = analysis_dir(&analysis.fingerprint);
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let audio = TempFile(folder.join(format!("transcribe-{}-{job}.f32", std::process::id())));

        progress(0.0, "décodage de l'audio");
        decode_mono_to_file(&analysis.media, analysis.envelope.channel_db.as_deref(), &audio.0)?;
        if cancelled() {
            return Err("transcription annulée".to_string());
        }

        self.submit(&json!({
            "job": job,
            "audio": audio.0.to_string_lossy(),
            "cache_dir": folder.to_string_lossy(),
            "language": language,
            "model": model,
            "force": force,
        }))?;
        self.wait(&job, &analysis.fingerprint, language, model, progress, cancelled, on_hardware)
    }

    #[allow(clippy::too_many_arguments)]
    fn wait(
        &mut self,
        job: &str,
        fingerprint: &str,
        language: &str,
        model: &str,
        progress: &mut dyn FnMut(f64, &str),
        cancelled: &dyn Fn() -> bool,
        on_hardware: &mut dyn FnMut(&Hardware),
    ) -> Result<JobOutcome, String> {
        loop {
            if cancelled() {
                self.stop();
                return Err("transcription annulée".to_string());
            }
            let Some(worker) = self.worker.as_ref() else {
                return Err("le processus de transcription s'est arrêté.".to_string());
            };
            let event = match worker.events.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => Event::Exited,
            };
            match event {
                Event::Ready(hardware) => {
                    on_hardware(&hardware);
                    self.hardware = Some(hardware);
                }
                Event::Exited => {
                    self.stop();
                    return Err(format!(
                        "le processus de transcription s'est arrêté inopinément (voir {}).",
                        worker_log_path().display()
                    ));
                }
                Event::Message(message) if message["job"] != job => {}
                Event::Message(message) => match message["type"].as_str() {
                    Some("progress") => {
                        progress(message["fraction"].as_f64().unwrap_or(0.0), message["label"].as_str().unwrap_or(""));
                    }
                    Some("error") => {
                        return Err(message["message"].as_str().unwrap_or("erreur de transcription").to_string());
                    }
                    Some("done") => {
                        let transcript = cached_transcript(fingerprint, model, language)
                            .ok_or("la transcription terminée est introuvable dans le cache.")?;
                        return Ok(JobOutcome {
                            transcript,
                            cached: message["cached"].as_bool().unwrap_or(false),
                            gpu_memory_mb: message["gpu_memory_mb"].as_f64(),
                            encoder_device: message["encoder_device"].as_str().map(str::to_string),
                        });
                    }
                    _ => {}
                },
            }
        }
    }

    /// Fait tourner un calcul de contrôle du matériel : démarre le processus et attend sa réponse.
    pub fn probe_hardware(&mut self, timeout: Duration) -> Result<Hardware, String> {
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn()?);
        }
        let worker = self.worker.as_ref().unwrap();
        loop {
            match worker.events.recv_timeout(timeout) {
                Ok(Event::Ready(hardware)) => {
                    self.hardware = Some(hardware.clone());
                    return Ok(hardware);
                }
                Ok(Event::Exited) | Err(_) => return Err("le processus de transcription n'a pas démarré.".to_string()),
                Ok(Event::Message(_)) => {}
            }
        }
    }
}

impl Drop for TranscribeSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Lance une commande Python d'Euhpen en console (entraînement de la tête).
pub fn run_python_console(args: &[String]) -> Result<i32, String> {
    let root = project_root().ok_or("dossier d'Euhpen introuvable (variable EUHPEN_ROOT).")?;
    let status = std::process::Command::new(python_exe()?)
        .args(["-m", "euhpen"])
        .args(args)
        .current_dir(&root)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONPATH", root.join("python"))
        .status()
        .map_err(|e| format!("Python n'a pas pu démarrer : {e}"))?;
    Ok(status.code().unwrap_or(1))
}

pub fn shipped_python_dir(root: &Path) -> PathBuf {
    root.join("python").join("euhpen")
}
