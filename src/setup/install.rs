//! Le travail de l'installateur : ce qu'il pose, où, et comment le retirer.
//!
//! Ce fichier est commun aux systèmes. `platform` (`windows.rs` ou `macos.rs`) connaît ce qui
//! change de l'un à l'autre : les téléchargements, les chemins, les raccourcis, l'inscription.
//!
//! `EUHPEN_SETUP_SANDBOX=<dossier>` isole tout dans ce dossier (application, raccourcis, inscription),
//! pour essayer l'installateur sans toucher au système. `EUHPEN_SETUP_UV=<programme>` utilise
//! ce uv-là au lieu de télécharger le nôtre.

use std::fs::{self, File};
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha1::{Digest as _, Sha1};
use zip::ZipArchive;

use super::job::{End, Job, Outcome, Status, Stop, Watch};
use super::platform;
use crate::probe::hidden_command;

/// L'application à poser : archive préparée par le script de `packaging/` du système.
const PAYLOAD: &[u8] = include_bytes!(env!("EUHPEN_PAYLOAD"));
const REQUIREMENTS_NAME: &str = "python/requirements.txt";

const UV_VERSION: &str = "0.13.7";
const TORCH_CUDA_INDEX: &str = "https://download.pytorch.org/whl/cu128";

const WHISPER_FOLDER: &str = "models--openai--whisper-large-v3-turbo";

// Tailles indicatives, mesurées sur une installation réelle.
pub const WHISPER_SIZE: u64 = 1_620_000_000;
const CUDA_SIZE: u64 = 4_400_000_000;
const APP_SIZE: u64 = 400_000_000;
const SPARE_SIZE: u64 = 1_000_000_000;

pub const STAMP: &str = ".euhpen-install";

/// Un outil à télécharger : une archive, et les fichiers à en extraire dans `tools/`.
pub struct Archive {
    pub url: &'static str,
    pub sha256: &'static str,
    pub members: &'static [&'static str],
}

pub struct Existing {
    pub root: PathBuf,
    pub hesitations: bool,
}

#[derive(Clone)]
pub struct Options {
    pub root: PathBuf,
    pub ffmpeg: bool,
    pub hesitations: bool,
    pub whisper: bool,
    pub desktop: bool,
    pub nvidia: bool,
}

#[derive(Clone)]
pub struct Removal {
    pub root: PathBuf,
    pub data: bool,
    pub whisper: bool,
}

#[derive(Clone, Default)]
pub struct Report {
    pub hesitations: bool,
    pub cuda: Option<String>,
    pub mps: bool,
    pub torch: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    App,
    Ffmpeg,
    Uv,
    Python,
    Torch,
    Packages,
    Check,
    Whisper,
    Finish,
}

impl Step {
    fn label(self) -> &'static str {
        match self {
            Step::App => "Fichiers d'Euhpen",
            Step::Ffmpeg => "ffmpeg, pour lire les médias",
            Step::Uv => "Gestionnaire de paquets Python",
            Step::Python => "Python 3.12",
            Step::Torch => "PyTorch pour carte NVIDIA (CUDA)",
            Step::Packages => "Bibliothèques du modèle",
            Step::Check => "Vérification de PyTorch",
            Step::Whisper => "Modèle Whisper",
            Step::Finish => "Raccourcis et finalisation",
        }
    }
}

fn plan(options: &Options) -> Vec<Step> {
    let mut steps = vec![Step::App];
    if options.ffmpeg {
        steps.push(Step::Ffmpeg);
    }
    if options.hesitations {
        steps.extend([Step::Uv, Step::Python]);
        if options.nvidia {
            steps.push(Step::Torch);
        }
        steps.extend([Step::Packages, Step::Check]);
        if options.whisper {
            steps.push(Step::Whisper);
        }
    }
    steps.push(Step::Finish);
    steps
}

pub fn step_labels(options: &Options) -> Vec<String> {
    plan(options).into_iter().map(|step| step.label().to_string()).collect()
}

pub fn has_payload() -> bool {
    !PAYLOAD.is_empty()
}

pub fn sandbox() -> Option<PathBuf> {
    std::env::var_os("EUHPEN_SETUP_SANDBOX").filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn default_root() -> PathBuf {
    if let Some(folder) = sandbox() {
        return folder.join("app");
    }
    platform::default_root()
}

fn read_stamp(root: &Path) -> Option<Existing> {
    let text = fs::read_to_string(root.join(STAMP)).ok()?;
    let hesitations = text.lines().any(|line| line.trim() == "hesitations=1");
    Some(Existing { root: root.to_path_buf(), hesitations })
}

/// Une installation déjà là : celle d'où l'on est lancé, ou celle inscrite sur le système.
pub fn find_existing() -> Option<Existing> {
    let here = std::env::current_exe().ok().and_then(|exe| exe.ancestors().nth(platform::SETUP_DEPTH).map(Path::to_path_buf));
    if let Some(found) = here.as_deref().and_then(read_stamp) {
        return Some(found);
    }
    match platform::recorded_root() {
        // Un essai isolé ignore ce qu'un autre essai a inscrit.
        Some(location) if sandbox().is_none_or(|folder| location.starts_with(folder)) => read_stamp(&location),
        _ => read_stamp(&default_root()),
    }
}

fn belongs_to_euhpen(root: &Path) -> bool {
    root.join(STAMP).is_file() || root.join(platform::GUI).is_file()
}

/// Refuse les dossiers où installer serait impossible ou dangereux.
pub fn check_root(root: &Path) -> Result<(), String> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err("Choisissez un dossier complet, autre que la racine d'un disque.".into());
    }
    platform::check_location(root)?;
    if root.is_file() {
        return Err("Ce chemin désigne un fichier.".into());
    }
    if root.is_dir() && !belongs_to_euhpen(root) {
        let empty = fs::read_dir(root).map(|mut entries| entries.next().is_none()).unwrap_or(false);
        if !empty {
            return Err("Ce dossier contient déjà autre chose. Choisissez un dossier vide ou un nouveau dossier.".into());
        }
    }
    Ok(())
}

/// Espace nécessaire dans le dossier d'installation.
pub fn required_space(options: &Options) -> u64 {
    let model = match (options.hesitations, options.nvidia) {
        (false, _) => 0,
        (true, true) => CUDA_SIZE,
        (true, false) => platform::BASE_MODEL_SIZE,
    };
    let whisper = if options.hesitations && options.whisper { WHISPER_SIZE } else { 0 };
    APP_SIZE + model + whisper + SPARE_SIZE
}

pub fn io_message(what: &Path, error: &io::Error) -> String {
    platform::friendly_io_error(what, error).unwrap_or_else(|| format!("{} : {error}", what.display()))
}

/// Nom d'un outil dans `tools/`, avec l'extension du système.
fn tool_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn tool_path(root: &Path, name: &str) -> PathBuf {
    root.join("tools").join(tool_name(name))
}

pub fn has_bundled_ffmpeg(root: &Path) -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|name| tool_path(root, name).is_file())
}

pub fn extract_zip(archive_path: &Path, wanted: &[(&str, PathBuf)]) -> Result<(), String> {
    let file = File::open(archive_path).map_err(|e| format!("{} : {e}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("archive illisible : {e}"))?;
    for (suffix, target) in wanted {
        let index = (0..archive.len())
            .find(|index| archive.by_index(*index).is_ok_and(|entry| entry.is_file() && entry.name().is_ok_and(|name| name.rsplit('/').next() == Some(*suffix))))
            .ok_or_else(|| format!("{suffix} est absent de l'archive téléchargée."))?;
        let mut entry = archive.by_index(index).map_err(|e| format!("archive illisible : {e}"))?;
        let mut output = File::create(target).map_err(|e| io_message(target, &e))?;
        io::copy(&mut entry, &mut output).map_err(|e| io_message(target, &e))?;
    }
    Ok(())
}

/// Télécharge une archive, en contrôle l'empreinte, puis en extrait les outils.
fn fetch(job: &Job, archive: &Archive, tools: &Path) -> Outcome {
    let name = archive.url.rsplit('/').next().unwrap_or("archive");
    let file = tools.join(format!("download-{name}"));
    job.download(archive.url, archive.sha256, &file)?;
    job.progress(None, "décompression");
    platform::unpack(&file, archive.members, tools)?;
    let _ = fs::remove_file(&file);
    Ok(())
}

fn step_app(job: &Job, options: &Options) -> Outcome {
    let root = &options.root;
    fs::create_dir_all(root).map_err(|e| io_message(root, &e))?;
    platform::ensure_not_running(root)?;
    for name in platform::REPLACED {
        let path = root.join(name);
        if path.exists() {
            // D'anciens fichiers ne doivent pas survivre à une mise à jour.
            fs::remove_dir_all(&path).map_err(|e| io_message(&path, &e))?;
        }
    }
    let mut archive = ZipArchive::new(Cursor::new(PAYLOAD)).map_err(|e| format!("application embarquée illisible : {e}"))?;
    let count = archive.len();
    for index in 0..count {
        job.check()?;
        let mut entry = archive.by_index(index).map_err(|e| format!("application embarquée illisible : {e}"))?;
        let Some(relative) = entry.enclosed_name() else { continue };
        let target = root.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| io_message(&target, &e))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io_message(parent, &e))?;
        }
        // Réécrire en place un exécutable signé le ferait tuer par le noyau de macOS.
        #[cfg(unix)]
        let _ = fs::remove_file(&target);
        let mut output = File::create(&target).map_err(|e| io_message(&target, &e))?;
        io::copy(&mut entry, &mut output).map_err(|e| io_message(&target, &e))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777)).map_err(|e| io_message(&target, &e))?;
        }
        job.progress(Some((index + 1) as f32 / count as f32), format!("{} fichiers sur {count}", index + 1));
    }
    let own = std::env::current_exe().map_err(|e| format!("emplacement de l'installateur inconnu : {e}"))?;
    let copy = platform::setup_copy(root);
    if !same_file(&own, &copy) {
        platform::place_setup(root, &own, &copy)?;
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

fn step_ffmpeg(job: &Job, options: &Options) -> Outcome {
    let tools = options.root.join("tools");
    fs::create_dir_all(&tools).map_err(|e| io_message(&tools, &e))?;
    if has_bundled_ffmpeg(&options.root) {
        job.log("ffmpeg est déjà installé.");
        return Ok(());
    }
    for archive in platform::FFMPEG {
        fetch(job, archive, &tools)?;
    }
    // Mieux vaut l'apprendre ici qu'à l'ouverture du premier rush.
    for name in ["ffmpeg", "ffprobe"] {
        let mut command = hidden_command(tool_path(&options.root, name));
        command.arg("-version");
        job.run(command, None)?;
    }
    Ok(())
}

fn uv_command(options: &Options) -> Command {
    let tools = options.root.join("tools");
    let mut command = hidden_command(tools.join(tool_name("uv")));
    command
        .current_dir(&options.root)
        .env("UV_PYTHON_INSTALL_DIR", tools.join("python"))
        .env("UV_CACHE_DIR", tools.join("uv-cache"))
        .env("UV_NO_PROGRESS", "1");
    // L'environnement de l'utilisateur ne doit pas orienter l'installation ailleurs.
    for variable in ["VIRTUAL_ENV", "CONDA_PREFIX", "PYTHONHOME", "PYTHONPATH", "UV_PYTHON", "UV_PROJECT_ENVIRONMENT"] {
        command.env_remove(variable);
    }
    command
}

fn venv_python(root: &Path) -> PathBuf {
    let venv = root.join(".venv");
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn python_command(options: &Options) -> Command {
    let mut command = hidden_command(venv_python(&options.root));
    command
        .current_dir(&options.root)
        .env("PYTHONPATH", options.root.join("python"))
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env_remove("PYTHONHOME");
    command
}

fn step_uv(job: &Job, options: &Options) -> Outcome {
    let tools = options.root.join("tools");
    fs::create_dir_all(&tools).map_err(|e| io_message(&tools, &e))?;
    let uv = tools.join(tool_name("uv"));
    if let Some(local) = std::env::var_os("EUHPEN_SETUP_UV").filter(|v| !v.is_empty()) {
        let _ = fs::remove_file(&uv);
        fs::copy(&local, &uv).map_err(|e| io_message(&uv, &e))?;
        return Ok(());
    }
    if uv.is_file() {
        let version = hidden_command(&uv).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        if version.contains(UV_VERSION) {
            return Ok(());
        }
    }
    fetch(job, &platform::UV, &tools)
}

fn step_python(job: &Job, options: &Options) -> Outcome {
    let venv = options.root.join(".venv");
    let python = venv_python(&options.root);
    if python.is_file() {
        let healthy = hidden_command(&python).args(["-c", "pass"]).output().is_ok_and(|o| o.status.success());
        if healthy {
            return Ok(());
        }
        job.log("L'environnement Python existant ne démarre plus : il est recréé.");
        fs::remove_dir_all(&venv).map_err(|e| io_message(&venv, &e))?;
    }
    let mut command = uv_command(options);
    command.args(["venv", "--python", "3.12", "--python-preference", "only-managed", "--allow-existing", ".venv"]);
    job.run(command, None)?;
    Ok(())
}

fn pip_install(job: &Job, options: &Options, arguments: &[&str], expected: Option<u64>) -> Outcome {
    let mut command = uv_command(options);
    command.args(["pip", "install", "--python"]).arg(venv_python(&options.root)).args(arguments);
    let watch = expected.map(|expected| Watch { folder: options.root.join("tools").join("uv-cache"), expected });
    job.run(command, watch)?;
    Ok(())
}

const CHECK_TORCH: &str = "import importlib.util as u, json, torch
missing = [m for m in ('transformers', 'accelerate', 'huggingface_hub') if u.find_spec(m) is None]
if missing:
    raise SystemExit('Il manque : ' + ', '.join(missing))
ok = torch.cuda.is_available()
mps = torch.backends.mps.is_available()
print(json.dumps({'cuda': ok, 'mps': mps, 'name': torch.cuda.get_device_name(0) if ok else None, 'torch': torch.__version__}))
";

struct TorchState {
    cuda: Option<String>,
    mps: bool,
    version: Option<String>,
}

fn parse_torch_state(stdout: &str) -> Result<TorchState, String> {
    let line = stdout.lines().rfind(|line| line.trim_start().starts_with('{')).ok_or("PyTorch n'a pas répondu.")?;
    let value: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("réponse de PyTorch illisible : {e}"))?;
    let cuda = (value["cuda"] == true).then(|| value["name"].as_str().unwrap_or("carte NVIDIA").to_string());
    Ok(TorchState { cuda, mps: value["mps"] == true, version: value["torch"].as_str().map(str::to_string) })
}

fn torch_state(job: &Job, options: &Options) -> Outcome<TorchState> {
    let mut command = python_command(options);
    command.args(["-c", CHECK_TORCH]);
    let stdout = job.run(command, None)?;
    Ok(parse_torch_state(&stdout)?)
}

fn step_check(job: &Job, options: &Options, report: &mut Report) -> Outcome {
    let mut state = torch_state(job, options)?;
    if options.nvidia && state.cuda.is_none() {
        // Une version sans CUDA déjà présente satisfait pip : on la remplace pour de bon.
        job.log("PyTorch ne voit pas la carte NVIDIA : réinstallation de la version CUDA.");
        job.progress(None, "réinstallation de la version CUDA");
        pip_install(job, options, &["torch", "--index-url", TORCH_CUDA_INDEX, "--reinstall-package", "torch"], Some(CUDA_SIZE))?;
        state = torch_state(job, options)?;
        if state.cuda.is_none() {
            report.warnings.push(
                "PyTorch ne voit pas la carte NVIDIA : la transcription tournera sur le processeur, plus lentement. Mettez à jour le pilote NVIDIA, puis relancez l'installateur.".into(),
            );
        }
    }
    if cfg!(target_os = "macos") && !state.mps {
        report.warnings.push("PyTorch ne voit pas la puce graphique : la transcription tournera sur le processeur, plus lentement. Vérifiez que macOS est à jour.".into());
    }
    report.cuda = state.cuda;
    report.mps = state.mps;
    report.torch = state.version;
    Ok(())
}

/// Le cache Hugging Face, là où l'application cherchera le modèle.
fn huggingface_cache() -> PathBuf {
    if let Some(folder) = std::env::var_os("HF_HUB_CACHE").filter(|v| !v.is_empty()) {
        return PathBuf::from(folder);
    }
    if let Some(folder) = std::env::var_os("HF_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(folder).join("hub");
    }
    // Comme Python sous Windows, qui ne regarde pas HOME.
    let home = std::env::var_os("USERPROFILE").map_or_else(crate::cache::home_dir, PathBuf::from);
    home.join(".cache").join("huggingface").join("hub")
}

fn step_whisper(job: &Job, options: &Options) -> Outcome {
    let mut command = python_command(options);
    command.args(["-c", "from euhpen.transcribe import ensure_model; print(ensure_model())"]);
    job.run(command, Some(Watch { folder: huggingface_cache().join(WHISPER_FOLDER), expected: WHISPER_SIZE }))?;
    Ok(())
}

fn requirements_signature(root: &Path) -> String {
    let Ok(bytes) = fs::read(root.join(REQUIREMENTS_NAME)) else { return String::new() };
    format!("{:x}", Sha1::digest(bytes))
}

fn remove_path(job: &Job, path: &Path) -> Outcome {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else if path.exists() {
        fs::remove_file(path)
    } else {
        Ok(())
    };
    result.map_err(|e| Stop::Failed(io_message(path, &e)))?;
    job.log(format!("Supprimé : {}", path.display()));
    Ok(())
}

fn step_finish(job: &Job, options: &Options, report: &mut Report) -> Outcome {
    let root = &options.root;
    if !options.hesitations {
        // La détection a été décochée : on rend la place qu'elle occupait.
        for path in [root.join(".venv"), root.join("tools").join("python"), tool_path(root, "uv")] {
            remove_path(job, &path)?;
        }
    }
    // Retirer d'abord les raccourcis, pour qu'une case décochée supprime celui qui existe.
    platform::remove_shortcuts(job)?;
    platform::create_shortcuts(job, root, options.desktop)?;
    platform::register(job, options)?;
    let stamp = format!(
        "requirements={}\nhesitations={}\nversion={}\n",
        requirements_signature(root),
        u8::from(options.hesitations),
        crate::VERSION
    );
    let path = root.join(STAMP);
    fs::write(&path, stamp).map_err(|e| io_message(&path, &e))?;
    let cache = root.join("tools").join("uv-cache");
    if let Err(e) = fs::remove_dir_all(&cache) {
        if e.kind() != io::ErrorKind::NotFound {
            report.warnings.push(format!("Le cache temporaire n'a pas pu être supprimé : {}.", cache.display()));
        }
    }
    report.hesitations = options.hesitations;
    Ok(())
}

fn run_step(job: &Job, options: &Options, step: Step, report: &mut Report) -> Outcome {
    match step {
        Step::App => step_app(job, options),
        Step::Ffmpeg => step_ffmpeg(job, options),
        Step::Uv => step_uv(job, options),
        Step::Python => step_python(job, options),
        Step::Torch => pip_install(job, options, &["torch", "--index-url", TORCH_CUDA_INDEX], Some(CUDA_SIZE)),
        Step::Packages => pip_install(job, options, &["-r", platform::REQUIREMENTS], (!options.nvidia).then_some(platform::BASE_MODEL_SIZE)),
        Step::Check => step_check(job, options, report),
        Step::Whisper => step_whisper(job, options),
        Step::Finish => step_finish(job, options, report),
    }
}

/// Traduit un échec technique en ce que la personne peut faire.
fn explain(step: Step, stop: Stop) -> Stop {
    let Stop::Failed(detail) = stop else { return stop };
    let advice = match step {
        Step::Python | Step::Torch | Step::Packages => "L'installation de Python ou de ses bibliothèques a échoué. Vérifiez la connexion à Internet et l'espace disque, puis réessayez.",
        Step::Check => "PyTorch ne démarre pas correctement. Réessayez ; si le problème persiste, le journal donne le détail.",
        _ => return Stop::Failed(detail),
    };
    Stop::Failed(format!("{advice}\n\n{detail}"))
}

fn conclude(job: &Job, current: usize, result: Outcome, report: Report) {
    match result {
        Ok(()) => job.finish(End::Success(report)),
        Err(Stop::Cancelled) => {
            job.log("Installation annulée.");
            job.set_step(current, Status::Pending);
            job.finish(End::Cancelled);
        }
        Err(Stop::Failed(message)) => {
            job.log(format!("ÉCHEC : {message}"));
            job.set_step(current, Status::Failed);
            job.finish(End::Failure(message));
        }
    }
}

/// À lancer dans un fil dédié : la fenêtre suit l'avancement par `job`.
pub fn install(job: &Job, options: &Options) {
    let mut report = Report::default();
    let mut current = 0;
    let result = (|| {
        for (index, step) in plan(options).into_iter().enumerate() {
            current = index;
            job.check()?;
            job.log(format!("--- {}", step.label()));
            job.set_step(index, Status::Running);
            match run_step(job, options, step, &mut report) {
                // Euhpen marche sans ce téléchargement : il se refera à la première détection.
                Err(Stop::Failed(message)) if step == Step::Whisper => {
                    job.log(format!("ÉCHEC : {message}"));
                    job.set_step(index, Status::Failed);
                    report.warnings.push(
                        "Whisper n'a pas pu être téléchargé. Euhpen est prêt : le modèle sera téléchargé à la première détection, si l'ordinateur est connecté à Internet.".into(),
                    );
                }
                other => {
                    other.map_err(|stop| explain(step, stop))?;
                    job.set_step(index, Status::Done);
                }
            }
        }
        Ok(())
    })();
    conclude(job, current, result, report);
}

pub fn removal_labels(removal: &Removal) -> Vec<String> {
    let mut labels = vec![platform::UNREGISTER_LABEL.to_string(), "Fichiers d'Euhpen".to_string()];
    if removal.data {
        labels.push("Réglages et cache".to_string());
    }
    if removal.whisper {
        labels.push("Modèle Whisper".to_string());
    }
    labels
}

/// En essai isolé, on ne supprime jamais de données situées hors du dossier d'essai.
fn removable(path: &Path) -> bool {
    sandbox().is_none_or(|folder| path.starts_with(folder))
}

fn remove_outside_data(job: &Job, path: &Path) -> Outcome {
    if removable(path) {
        remove_path(job, path)
    } else {
        job.log(format!("Essai isolé : {} est conservé.", path.display()));
        Ok(())
    }
}

/// À lancer dans un fil dédié. Renvoie vrai quand l'installateur, en cours d'exécution, reste à supprimer.
pub fn uninstall(job: &Job, removal: &Removal) -> bool {
    let mut current = 0;
    let root = &removal.root;
    let mut steps: Vec<Box<dyn Fn() -> Outcome + '_>> = vec![
        Box::new(|| {
            platform::ensure_not_running(root)?;
            platform::remove_shortcuts(job)?;
            platform::unregister(job)
        }),
        Box::new(|| {
            for name in platform::INSTALLED {
                job.check()?;
                remove_path(job, &root.join(name))?;
            }
            Ok(())
        }),
    ];
    if removal.data {
        steps.push(Box::new(|| {
            remove_outside_data(job, &crate::cache::cache_dir())?;
            remove_outside_data(job, &crate::config::config_dir())
        }));
    }
    if removal.whisper {
        steps.push(Box::new(|| remove_outside_data(job, &huggingface_cache().join(WHISPER_FOLDER))));
    }
    let result = (|| {
        for (index, step) in steps.iter().enumerate() {
            current = index;
            job.check()?;
            job.set_step(index, Status::Running);
            step()?;
            job.set_step(index, Status::Done);
        }
        Ok(())
    })();
    let succeeded = result.is_ok();
    drop(steps);

    let own = std::env::current_exe().ok();
    let running_from_root = own.as_deref().is_some_and(|own| same_file(own, &platform::setup_copy(root)));
    if succeeded && !running_from_root {
        platform::remove_setup(root);
        let _ = fs::remove_dir(root);
    }
    conclude(job, current, result, Report::default());
    succeeded && running_from_root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("euhpen-setup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    fn options(nvidia: bool, hesitations: bool) -> Options {
        Options { root: PathBuf::from(r"C:\E"), ffmpeg: false, hesitations, whisper: true, desktop: true, nvidia }
    }

    #[test]
    fn a_new_or_empty_folder_is_accepted() {
        let root = folder("empty");
        assert!(check_root(&root).is_ok());
        fs::create_dir_all(&root).unwrap();
        assert!(check_root(&root).is_ok());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_folder_with_other_files_is_refused() {
        let root = folder("busy");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("thesis.docx"), b"x").unwrap();
        assert!(check_root(&root).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_existing_installation_can_be_updated() {
        let root = folder("again");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(STAMP), "requirements=a\nhesitations=1\n").unwrap();
        assert!(check_root(&root).is_ok());
        assert!(read_stamp(&root).unwrap().hesitations);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_plan_follows_the_options() {
        assert_eq!(plan(&options(false, false)), vec![Step::App, Step::Finish]);
        let nvidia = plan(&options(true, true));
        assert!(nvidia.contains(&Step::Torch) && nvidia.contains(&Step::Whisper));
        assert!(!plan(&options(false, true)).contains(&Step::Torch));
    }

    #[test]
    fn the_required_space_grows_with_the_options() {
        let without = required_space(&options(false, false));
        let cpu = required_space(&options(false, true));
        let cuda = required_space(&options(true, true));
        assert!(without < cpu && cpu < cuda);
    }

    #[test]
    fn torch_state_is_read_after_any_noise() {
        let state = parse_torch_state("avertissement\n{\"cuda\": true, \"mps\": false, \"name\": \"RTX\", \"torch\": \"2.8\"}").unwrap();
        assert_eq!(state.cuda.as_deref(), Some("RTX"));
        assert_eq!(state.version.as_deref(), Some("2.8"));
        assert!(!state.mps);
        let apple = parse_torch_state("{\"cuda\": false, \"mps\": true, \"name\": null, \"torch\": \"2.8\"}").unwrap();
        assert!(apple.cuda.is_none() && apple.mps);
        assert!(parse_torch_state("rien").is_err());
    }

    #[test]
    fn only_listed_items_are_removed() {
        assert!(!platform::INSTALLED.contains(&"."));
        assert!(platform::INSTALLED.iter().all(|name| !name.contains('\\') && !name.contains('/')));
    }
}
