//! Ce qui est propre à Windows : téléchargements, raccourcis, registre, droits sur les fichiers.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::install::{extract_zip, io_message, sandbox, Archive, Options, STAMP};
use super::job::{Job, Outcome};
use crate::probe::hidden_command;

pub const FFMPEG: &[Archive] = &[Archive {
    url: "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip",
    sha256: "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba",
    members: &["ffmpeg.exe", "ffprobe.exe"],
}];
pub const UV: Archive = Archive {
    url: "https://github.com/astral-sh/uv/releases/download/0.13.7/uv-x86_64-pc-windows-msvc.zip",
    sha256: "0cdc5b38fe5a8168ed6a25814f5ba43c21d313dd3187314fccb37b7cec46e021",
    members: &["uv.exe"],
};

/// Environnement Python sans CUDA, mesuré sur une installation réelle.
pub const BASE_MODEL_SIZE: u64 = 700_000_000;
pub const FFMPEG_DOWNLOAD: &str = "115 Mo";
pub const REQUIREMENTS: &str = "python\\requirements.txt";

pub const GUI: &str = "euhpen-gui.exe";
/// Le programme d'installation est posé directement à la racine du dossier.
pub const SETUP_DEPTH: usize = 1;
/// Retirés avant de poser la nouvelle version : d'anciens fichiers ne doivent pas lui survivre.
pub const REPLACED: &[&str] = &["python"];
/// Seuls ces éléments sont supprimés par la désinstallation : jamais le dossier entier à l'aveugle.
pub const INSTALLED: &[&str] = &["euhpen-gui.exe", "euhpen.exe", "LICENSE", STAMP, "python", ".venv", "tools"];

pub const UNREGISTER_LABEL: &str = "Raccourcis et inscription dans Windows";
pub const UNINSTALL_HINT: &str = "Pour désinstaller : Paramètres de Windows > Applications > Euhpen.";

const SETUP_EXE: &str = "euhpen-setup.exe";

/// Torch contient des chemins très longs : au-delà, on dépasse la limite de 260 signes de Windows.
const MAX_ROOT_LENGTH: usize = 80;

const SHORTCUTS: &str = r#"$ErrorActionPreference = 'Stop'
$shell = New-Object -ComObject WScript.Shell
foreach ($place in ($env:EUHPEN_LNK_PLACES -split ';')) {
  if (-not $place) { continue }
  $folder = if ($place.StartsWith('<')) { [Environment]::GetFolderPath($place.Trim('<', '>')) } else { $place }
  $path = Join-Path $folder 'Euhpen.lnk'
  if ($env:EUHPEN_LNK_TARGET) {
    New-Item -ItemType Directory -Force $folder | Out-Null
    $link = $shell.CreateShortcut($path)
    $link.TargetPath = $env:EUHPEN_LNK_TARGET
    $link.WorkingDirectory = Split-Path -Parent $env:EUHPEN_LNK_TARGET
    $link.Description = 'Euhpen'
    $link.Save()
  } elseif (Test-Path -LiteralPath $path) {
    Remove-Item -LiteralPath $path -Force
  }
}
"#;

fn registry_path() -> &'static str {
    if sandbox().is_some() {
        r"Software\Euhpen-Setup-Test"
    } else {
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Euhpen"
    }
}

pub fn default_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join("Euhpen")
}

fn powershell(script: &str) -> Command {
    let mut command = hidden_command("powershell");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    command
}

/// L'emplacement inscrit dans le registre par une installation précédente.
pub fn recorded_root() -> Option<PathBuf> {
    // PowerShell plutôt que reg.exe, qui écrit dans la page de code de la console : un dossier
    // comme C:\Users\Clément serait abîmé.
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8; (Get-ItemProperty -LiteralPath 'HKCU:\\{}' -ErrorAction SilentlyContinue).InstallLocation",
        registry_path()
    );
    let output = powershell(&script).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let location = text.trim().trim_start_matches('\u{feff}');
    (!location.is_empty()).then(|| PathBuf::from(location))
}

/// Refuse les dossiers où Windows empêcherait d'installer.
pub fn check_location(root: &Path) -> Result<(), String> {
    let length = root.as_os_str().to_string_lossy().chars().count();
    if length > MAX_ROOT_LENGTH {
        return Err(format!("Ce chemin est trop long ({length} signes, {MAX_ROOT_LENGTH} au plus) : Windows ne pourrait pas y installer PyTorch."));
    }
    let lowered = root.to_string_lossy().to_lowercase().replace('/', "\\");
    if ["\\windows", "\\program files"].iter().any(|protected| lowered.get(2..).is_some_and(|rest| rest.starts_with(protected))) {
        return Err("Ce dossier demande des droits d'administrateur. Choisissez-en un dans votre dossier personnel.".into());
    }
    Ok(())
}

extern "system" {
    fn GetDiskFreeSpaceExW(path: *const u16, available: *mut u64, total: *mut u64, free: *mut u64) -> i32;
}

/// Espace libre sur le disque du dossier (qui n'existe pas forcément encore).
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let existing = path.ancestors().find(|candidate| candidate.is_dir())?;
    let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut available = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    // SAFETY: `wide` se termine par un zéro et les trois pointeurs désignent des u64 valides.
    let done = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    (done != 0).then_some(available)
}

pub fn friendly_io_error(what: &Path, error: &io::Error) -> Option<String> {
    match error.raw_os_error() {
        Some(5 | 32) => Some(format!("Un fichier d'Euhpen est en cours d'utilisation ({}). Fermez Euhpen, puis réessayez.", what.display())),
        Some(112) => Some("Le disque est plein. Libérez de la place, puis réessayez.".to_string()),
        _ => None,
    }
}

/// Un exécutable en cours d'exécution ne s'ouvre pas en écriture : on le sait avant de toucher à quoi que ce soit.
pub fn ensure_not_running(root: &Path) -> Outcome {
    for name in ["euhpen-gui.exe", "euhpen.exe"] {
        let path = root.join(name);
        if path.is_file() {
            OpenOptions::new().write(true).open(&path).map_err(|e| io_message(&path, &e))?;
        }
    }
    Ok(())
}

pub fn unpack(archive: &Path, members: &[&str], tools: &Path) -> Result<(), String> {
    let wanted: Vec<(&str, PathBuf)> = members.iter().map(|member| (*member, tools.join(member))).collect();
    extract_zip(archive, &wanted)
}

pub fn setup_copy(root: &Path) -> PathBuf {
    root.join(SETUP_EXE)
}

pub fn place_setup(_root: &Path, own: &Path, copy: &Path) -> Result<(), String> {
    fs::copy(own, copy).map_err(|e| io_message(copy, &e))?;
    Ok(())
}

pub fn remove_setup(root: &Path) {
    let _ = fs::remove_file(setup_copy(root));
}

fn shortcut_places(desktop: bool) -> Vec<String> {
    let places = match sandbox() {
        Some(folder) => {
            let shortcuts = folder.join("shortcuts");
            [shortcuts.join("programs").display().to_string(), shortcuts.join("desktop").display().to_string()]
        }
        None => ["<Programs>".to_string(), "<Desktop>".to_string()],
    };
    let [programs, on_desktop] = places;
    if desktop {
        vec![programs, on_desktop]
    } else {
        vec![programs]
    }
}

fn shortcuts(job: &Job, target: Option<&Path>, desktop: bool) -> Outcome {
    let mut command = powershell(SHORTCUTS);
    command.env("EUHPEN_LNK_PLACES", shortcut_places(desktop).join(";"));
    match target {
        Some(path) => command.env("EUHPEN_LNK_TARGET", path),
        None => command.env_remove("EUHPEN_LNK_TARGET"),
    };
    job.run(command, None)?;
    Ok(())
}

pub fn remove_shortcuts(job: &Job) -> Outcome {
    shortcuts(job, None, true)
}

pub fn create_shortcuts(job: &Job, root: &Path, desktop: bool) -> Outcome {
    shortcuts(job, Some(&root.join(GUI)), desktop)
}

pub fn register(job: &Job, options: &Options) -> Outcome {
    let key = format!(r"HKCU\{}", registry_path());
    let root = options.root.display().to_string();
    let setup = setup_copy(&options.root).display().to_string();
    let gui = options.root.join(GUI).display().to_string();
    let values = [
        ("DisplayName", "Euhpen".to_string()),
        ("DisplayVersion", crate::VERSION.to_string()),
        ("Publisher", "Euhpen".to_string()),
        ("InstallLocation", root),
        ("DisplayIcon", gui),
        ("UninstallString", format!("\"{setup}\" --uninstall")),
    ];
    for (name, value) in values {
        let mut command = hidden_command("reg");
        command.args(["add", &key, "/v", name, "/t", "REG_SZ", "/d", &value, "/f"]);
        job.run(command, None)?;
    }
    for name in ["NoModify", "NoRepair"] {
        let mut command = hidden_command("reg");
        command.args(["add", &key, "/v", name, "/t", "REG_DWORD", "/d", "1", "/f"]);
        job.run(command, None)?;
    }
    Ok(())
}

pub fn unregister(job: &Job) -> Outcome {
    let key = format!(r"HKCU\{}", registry_path());
    let query = hidden_command("reg").args(["query", &key]).output();
    if query.is_ok_and(|output| output.status.success()) {
        let mut command = hidden_command("reg");
        command.args(["delete", &key, "/f"]);
        job.run(command, None)?;
    }
    Ok(())
}

/// Un programme ne peut pas s'effacer lui-même : `cmd` s'en charge une fois la fenêtre fermée.
pub fn delete_after_exit(root: &Path) {
    use std::os::windows::process::CommandExt;
    let setup = setup_copy(root);
    let script = format!("/c \"ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"\"", setup.display(), root.display());
    let _ = hidden_command("cmd").current_dir(std::env::temp_dir()).raw_arg(script).spawn();
}

pub fn launch(root: &Path) -> Result<(), String> {
    Command::new(root.join(GUI)).current_dir(root).spawn().map(|_| ()).map_err(|e| format!("Euhpen n'a pas pu s'ouvrir : {e}"))
}

pub fn open_log(path: &Path) {
    let _ = hidden_command("notepad").arg(path).spawn();
}

/// Sous Windows, la désinstallation passe par l'option `--uninstall`.
pub fn launched_as_uninstaller() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::super::install::check_root;
    use std::path::{Path, PathBuf};

    #[test]
    fn unsafe_destinations_are_refused() {
        assert!(check_root(Path::new(r"C:\")).is_err());
        assert!(check_root(Path::new("relatif")).is_err());
        assert!(check_root(Path::new(r"C:\Windows\Euhpen")).is_err());
        assert!(check_root(Path::new(r"C:\Program Files\Euhpen")).is_err());
        assert!(check_root(&PathBuf::from(format!(r"C:\{}", "a".repeat(100)))).is_err());
    }
}
