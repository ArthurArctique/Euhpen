//! Dossier de cache et empreinte rapide des fichiers.

use blake2::digest::consts::U16;
use blake2::{Blake2b, Digest};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const SAMPLE: u64 = 1 << 20; // 1 Mio

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Dossier de cache : EUHPEN_CACHE_DIR, sinon l'emplacement standard du système.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = env_path("EUHPEN_CACHE_DIR") {
        return dir;
    }
    if cfg!(target_os = "macos") {
        return home_dir().join("Library").join("Caches").join("euhpen");
    }
    if cfg!(windows) {
        let base = env_path("LOCALAPPDATA").unwrap_or_else(|| home_dir().join("AppData").join("Local"));
        return base.join("euhpen").join("Cache");
    }
    env_path("XDG_CACHE_HOME").unwrap_or_else(|| home_dir().join(".cache")).join("euhpen")
}

/// Dossier de cache d'un fichier (enveloppe, transcriptions, corrections, aperçu).
pub fn analysis_dir(fingerprint: &str) -> PathBuf {
    cache_dir().join("analysis").join(fingerprint)
}

/// Empreinte rapide : taille + 1 Mio au début, au milieu et à la fin.
///
/// Lire entièrement un rush de 50 Go prendrait plusieurs minutes ; ces échantillons
/// suffisent à distinguer deux fichiers vidéo, et ne dépendent ni du nom ni du dossier
/// (déplacer ou renommer un fichier conserve son analyse).
pub fn file_fingerprint(path: &Path) -> Result<String, String> {
    let describe = |e: std::io::Error| format!("{} : {e}", path.display());
    let size = fs::metadata(path).map_err(describe)?.len();
    let mut hasher = Blake2b::<U16>::new();
    hasher.update(size.to_string().as_bytes());
    let mut file = fs::File::open(path).map_err(describe)?;
    let mut offsets = vec![0, (size / 2).saturating_sub(SAMPLE / 2), size.saturating_sub(SAMPLE)];
    offsets.sort_unstable();
    offsets.dedup();
    let mut buffer = Vec::with_capacity(SAMPLE as usize);
    for offset in offsets {
        file.seek(SeekFrom::Start(offset)).map_err(describe)?;
        buffer.clear();
        (&mut file).take(SAMPLE).read_to_end(&mut buffer).map_err(describe)?;
        hasher.update(&buffer);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let data = fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Écriture atomique (fichier temporaire puis renommage).
pub fn write_json<T: Serialize>(path: &Path, data: &T) -> Result<(), String> {
    let describe = |e: std::io::Error| format!("{} : {e}", path.display());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(describe)?;
    }
    let body = serde_json::to_vec(data).map_err(|e| e.to_string())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, body).map_err(describe)?;
    fs::rename(&tmp, path).map_err(describe)
}

pub fn clear_cache() -> Result<PathBuf, String> {
    let root = cache_dir();
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|e| format!("{} : {e}", root.display()))?;
    }
    Ok(root)
}

pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_size(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}
