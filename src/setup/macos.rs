//! Ce qui est propre à macOS (Apple Silicon) : téléchargements, paquets `.app`, raccourci.
//!
//! ```text
//! ~/Applications/Euhpen/
//!   Euhpen.app                 l'interface
//!   Désinstaller Euhpen.app    une copie de l'installateur, qui s'ouvre en mode désinstallation
//!   euhpen                     la ligne de commande
//!   python/  .venv/  tools/    le modèle et ses outils, comme sous Windows
//! ```

use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::install::{extract_zip, io_message, sandbox, Archive, Options, STAMP};
use super::job::{Job, Outcome, Stop};
use crate::cache::home_dir;
use crate::config::config_dir;
use crate::probe::hidden_command;

pub const FFMPEG: &[Archive] = &[
    Archive {
        url: "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip",
        sha256: "c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924",
        members: &["ffmpeg"],
    },
    Archive {
        url: "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffprobe.zip",
        sha256: "fcbe839537485eaee7a7a8bc5cbc0f90d53617e80943e8a5b2e31cb851197ea6",
        members: &["ffprobe"],
    },
];
pub const UV: Archive = Archive {
    url: "https://github.com/astral-sh/uv/releases/download/0.13.7/uv-aarch64-apple-darwin.tar.gz",
    sha256: "81b811b57e4624df7ed3aac148e0359100d9962f3d9dce00aec533dc2e086731",
    members: &["uv"],
};

/// Python et PyTorch pour puce Apple.
pub const BASE_MODEL_SIZE: u64 = 1_100_000_000;
pub const FFMPEG_DOWNLOAD: &str = "57 Mo";
pub const REQUIREMENTS: &str = "python/requirements.txt";

const APP: &str = "Euhpen.app";
pub const GUI: &str = "Euhpen.app/Contents/MacOS/euhpen-gui";
/// `<dossier>/Désinstaller Euhpen.app/Contents/MacOS/<programme>`.
pub const SETUP_DEPTH: usize = 4;
pub const REPLACED: &[&str] = &["python", APP];
pub const INSTALLED: &[&str] = &[APP, "euhpen", "LICENSE", STAMP, "python", ".venv", "tools"];

pub const UNREGISTER_LABEL: &str = "Raccourci du Bureau";
pub const UNINSTALL_HINT: &str = "Pour désinstaller : ouvrez « Désinstaller Euhpen » dans le dossier d'installation.";

const UNINSTALLER: &str = "Désinstaller Euhpen.app";
/// Finder ne transmet pas d'arguments : c'est ce nom qui ouvre la désinstallation.
const UNINSTALLER_EXE: &str = "euhpen-uninstall";
const UNINSTALLER_PLIST: &str = include_str!("../../packaging/macos/Desinstaller.Info.plist");

/// Dossiers système où l'on n'installe jamais.
const PROTECTED: &[&str] = &["/System", "/Library", "/usr", "/bin", "/sbin"];

pub fn default_root() -> PathBuf {
    home_dir().join("Applications").join("Euhpen")
}

/// macOS n'a pas de registre : un petit fichier dans le dossier de configuration en tient lieu.
fn location_file() -> PathBuf {
    match sandbox() {
        Some(folder) => folder.join("emplacement"),
        None => config_dir().join("emplacement-installation"),
    }
}

pub fn recorded_root() -> Option<PathBuf> {
    let text = fs::read_to_string(location_file()).ok()?;
    let location = text.trim();
    (!location.is_empty()).then(|| PathBuf::from(location))
}

pub fn check_location(root: &Path) -> Result<(), String> {
    if PROTECTED.iter().any(|protected| root.starts_with(protected)) {
        return Err("Ce dossier est réservé au système. Choisissez-en un dans votre dossier personnel.".into());
    }
    Ok(())
}

/// Espace libre sur le disque du dossier (qui n'existe pas forcément encore).
pub fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|candidate| candidate.is_dir())?;
    let name = CString::new(existing.as_os_str().as_bytes()).ok()?;
    // SAFETY: `name` se termine par un zéro et `stats` est un tampon de la bonne taille, rempli par statfs.
    unsafe {
        let mut stats: libc::statfs = std::mem::zeroed();
        (libc::statfs(name.as_ptr(), &mut stats) == 0).then(|| stats.f_bavail * u64::from(stats.f_bsize))
    }
}

pub fn friendly_io_error(what: &Path, error: &io::Error) -> Option<String> {
    match error.raw_os_error() {
        Some(libc::EBUSY | libc::ETXTBSY) => Some(format!("Un fichier d'Euhpen est en cours d'utilisation ({}). Fermez Euhpen, puis réessayez.", what.display())),
        Some(libc::ENOSPC) => Some("Le disque est plein. Libérez de la place, puis réessayez.".to_string()),
        Some(libc::EACCES | libc::EPERM) => Some(format!("Pas le droit d'écrire ici ({}). Choisissez un dossier dans votre dossier personnel.", what.display())),
        _ => None,
    }
}

/// Remplacer l'application pendant qu'elle tourne lui retirerait ses modules Python.
pub fn ensure_not_running(root: &Path) -> Outcome {
    let pattern = regex::escape(&root.join(GUI).to_string_lossy());
    let running = hidden_command("/usr/bin/pgrep").args(["-f", &pattern]).output().is_ok_and(|output| output.status.success());
    if running {
        return Err(Stop::Failed("Euhpen est ouvert. Fermez-le, puis réessayez.".into()));
    }
    Ok(())
}

fn run_tar(arguments: &[&OsStr]) -> Result<Vec<u8>, String> {
    let output = Command::new("/usr/bin/tar").args(arguments).output().map_err(|e| format!("tar : {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!("archive illisible : {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

fn extract_tar(archive: &Path, members: &[&str], tools: &Path) -> Result<(), String> {
    let listing = String::from_utf8_lossy(&run_tar(&[OsStr::new("-tzf"), archive.as_os_str()])?).into_owned();
    for member in members {
        let entry = listing
            .lines()
            .find(|line| line.rsplit('/').next() == Some(*member))
            .ok_or_else(|| format!("{member} est absent de l'archive téléchargée."))?;
        let content = run_tar(&[OsStr::new("-xzOf"), archive.as_os_str(), OsStr::new(entry)])?;
        let target = tools.join(member);
        fs::write(&target, content).map_err(|e| io_message(&target, &e))?;
    }
    Ok(())
}

pub fn unpack(archive: &Path, members: &[&str], tools: &Path) -> Result<(), String> {
    // Réécrire en place un exécutable signé le ferait tuer par le noyau : on repart d'un fichier neuf.
    for member in members {
        let _ = fs::remove_file(tools.join(member));
    }
    if archive.extension().is_some_and(|extension| extension == "zip") {
        let wanted: Vec<(&str, PathBuf)> = members.iter().map(|member| (*member, tools.join(member))).collect();
        extract_zip(archive, &wanted)?;
    } else {
        extract_tar(archive, members, tools)?;
    }
    for member in members {
        let target = tools.join(member);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).map_err(|e| io_message(&target, &e))?;
    }
    Ok(())
}

pub fn setup_copy(root: &Path) -> PathBuf {
    root.join(UNINSTALLER).join("Contents").join("MacOS").join(UNINSTALLER_EXE)
}

/// Pose « Désinstaller Euhpen.app », qui contient une copie de ce programme.
pub fn place_setup(root: &Path, own: &Path, copy: &Path) -> Result<(), String> {
    let bundle = root.join(UNINSTALLER);
    if bundle.exists() {
        fs::remove_dir_all(&bundle).map_err(|e| io_message(&bundle, &e))?;
    }
    let folder = copy.parent().ok_or("emplacement du désinstallateur invalide")?;
    fs::create_dir_all(folder).map_err(|e| io_message(folder, &e))?;
    let plist = bundle.join("Contents").join("Info.plist");
    fs::write(&plist, UNINSTALLER_PLIST.replace("__VERSION__", crate::VERSION)).map_err(|e| io_message(&plist, &e))?;
    // Lu puis réécrit : `fs::copy` emporterait l'attribut de quarantaine de l'image disque.
    let program = fs::read(own).map_err(|e| io_message(own, &e))?;
    fs::write(copy, program).map_err(|e| io_message(copy, &e))?;
    fs::set_permissions(copy, fs::Permissions::from_mode(0o755)).map_err(|e| io_message(copy, &e))?;
    let signed = Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&bundle).output().map_err(|e| format!("codesign : {e}"))?;
    if signed.status.success() {
        Ok(())
    } else {
        Err(format!("Le désinstallateur n'a pas pu être signé : {}", String::from_utf8_lossy(&signed.stderr).trim()))
    }
}

pub fn remove_setup(root: &Path) {
    let _ = fs::remove_dir_all(root.join(UNINSTALLER));
}

fn desktop_link() -> PathBuf {
    match sandbox() {
        Some(folder) => folder.join("shortcuts").join(APP),
        None => home_dir().join("Desktop").join(APP),
    }
}

/// Ne retire que notre lien : jamais un vrai fichier que la personne aurait mis là.
pub fn remove_shortcuts(job: &Job) -> Outcome {
    let link = desktop_link();
    if fs::symlink_metadata(&link).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        fs::remove_file(&link).map_err(|e| io_message(&link, &e))?;
        job.log(format!("Supprimé : {}", link.display()));
    }
    Ok(())
}

pub fn create_shortcuts(_job: &Job, root: &Path, desktop: bool) -> Outcome {
    if !desktop {
        return Ok(());
    }
    let link = desktop_link();
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).map_err(|e| io_message(parent, &e))?;
    }
    // Une vraie copie déjà sur le Bureau est laissée telle quelle.
    if !link.exists() {
        symlink(root.join(APP), &link).map_err(|e| io_message(&link, &e))?;
    }
    Ok(())
}

pub fn register(_job: &Job, options: &Options) -> Outcome {
    let file = location_file();
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| io_message(parent, &e))?;
    }
    fs::write(&file, options.root.display().to_string()).map_err(|e| io_message(&file, &e))?;
    Ok(())
}

pub fn unregister(_job: &Job) -> Outcome {
    let file = location_file();
    if file.is_file() {
        fs::remove_file(&file).map_err(|e| io_message(&file, &e))?;
    }
    Ok(())
}

/// Sous macOS, un programme en cours d'exécution peut effacer son propre fichier.
pub fn delete_after_exit(root: &Path) {
    remove_setup(root);
    let _ = fs::remove_dir(root);
}

pub fn launch(root: &Path) -> Result<(), String> {
    let status = Command::new("/usr/bin/open").arg(root.join(APP)).status().map_err(|e| format!("Euhpen n'a pas pu s'ouvrir : {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Euhpen n'a pas pu s'ouvrir.".to_string())
    }
}

pub fn open_log(path: &Path) {
    let _ = hidden_command("/usr/bin/open").arg("-t").arg(path).spawn();
}

pub fn launched_as_uninstaller() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.file_name().is_some_and(|name| name == UNINSTALLER_EXE))
}

#[cfg(test)]
mod tests {
    use super::super::install::check_root;
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("euhpen-macos-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn unsafe_destinations_are_refused() {
        assert!(check_root(Path::new("/")).is_err());
        assert!(check_root(Path::new("relatif")).is_err());
        assert!(check_root(Path::new("/System/Euhpen")).is_err());
        assert!(check_root(Path::new("/usr/local/Euhpen")).is_err());
        assert!(check_location(Path::new("/Users/moi/Applications/Euhpen")).is_ok());
    }

    #[test]
    fn the_tool_inside_a_tar_is_extracted_and_runnable() {
        let work = folder("tar");
        let inner = work.join("uv-aarch64-apple-darwin");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("uv"), "#!/bin/sh\necho contenu\n").unwrap();
        let archive = work.join("download.tar.gz");
        let done = Command::new("/usr/bin/tar").arg("-czf").arg(&archive).arg("-C").arg(&work).arg("uv-aarch64-apple-darwin").status().unwrap();
        assert!(done.success());
        let tools = work.join("tools");
        fs::create_dir_all(&tools).unwrap();
        fs::write(tools.join("uv"), "ancienne version").unwrap();

        unpack(&archive, &["uv"], &tools).unwrap();

        assert_eq!(fs::read_to_string(tools.join("uv")).unwrap(), "#!/bin/sh\necho contenu\n");
        assert_eq!(fs::metadata(tools.join("uv")).unwrap().permissions().mode() & 0o777, 0o755);
        fs::remove_dir_all(&work).unwrap();
    }

    #[test]
    fn a_missing_tool_is_reported() {
        let work = folder("absent");
        fs::write(work.join("autre"), "x").unwrap();
        let archive = work.join("download.tar.gz");
        assert!(Command::new("/usr/bin/tar").arg("-czf").arg(&archive).arg("-C").arg(&work).arg("autre").status().unwrap().success());
        assert!(unpack(&archive, &["uv"], &work).unwrap_err().contains("absent"));
        fs::remove_dir_all(&work).unwrap();
    }

    #[test]
    fn the_uninstaller_is_opened_by_its_own_name() {
        let copy = setup_copy(Path::new("/E"));
        assert_eq!(copy.file_name().unwrap(), UNINSTALLER_EXE);
        assert_eq!(copy.ancestors().nth(SETUP_DEPTH).unwrap(), Path::new("/E"));
        assert_eq!(Path::new("/E").join(GUI).ancestors().nth(SETUP_DEPTH).unwrap(), Path::new("/E"));
    }

    #[test]
    fn the_installed_list_never_reaches_outside_the_folder() {
        assert!(INSTALLED.iter().all(|name| !name.contains('/') && *name != "."));
    }
}
