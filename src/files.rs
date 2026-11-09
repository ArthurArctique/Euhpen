//! Fichiers d'entrée et de sortie : dossiers, ordre, chemins des XML, affichage dans l'explorateur.

use crate::fcp7::Flavor;
use crate::probe::{hidden_command, MediaInfo};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const MEDIA_EXTENSIONS: [&str; 21] = [
    "mov", "mp4", "m4v", "mxf", "mkv", "avi", "mts", "m2ts", "webm", "mpg", "mpeg", "wav", "bwf", "aif", "aiff", "mp3",
    "m4a", "flac", "aac", "ogg", "opus",
];

pub fn is_media(path: &Path) -> bool {
    path.extension().is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// Médias d'un dossier (non récursif), triés par nom.
pub fn media_in_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_media(p)).collect();
    names.sort();
    names
}

/// Fichiers, dossiers (médias qu'ils contiennent) et motifs (`*.mov`, utile sous Windows).
pub fn expand_inputs(items: &[String]) -> Result<Vec<String>, String> {
    let mut paths: Vec<String> = Vec::new();
    for item in items {
        let path = Path::new(item);
        if path.is_dir() {
            paths.extend(media_in_dir(path).iter().map(|p| p.to_string_lossy().into_owned()));
        } else if item.contains(['*', '?', '[']) && !path.exists() {
            let mut matches: Vec<String> = glob::glob(item)
                .map_err(|e| format!("motif invalide {item} : {e}"))?
                .flatten()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if matches.is_empty() {
                return Err(format!("aucun fichier ne correspond à {item}"));
            }
            matches.sort();
            paths.append(&mut matches);
        } else if !path.exists() {
            return Err(format!("fichier introuvable : {item}"));
        } else {
            paths.push(item.clone());
        }
    }
    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(crate::probe::absolute(p)));
    if paths.is_empty() {
        return Err("aucun fichier média trouvé.".to_string());
    }
    Ok(paths)
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
enum Part {
    Text(String),
    /// Nombre sans ses zéros de tête : comparé par longueur, puis chiffre à chiffre.
    Number(usize, String),
}

fn natural_parts(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut digits = false;
    let flush = |current: &mut String, digits: bool, parts: &mut Vec<Part>| {
        if digits {
            let trimmed = current.trim_start_matches('0').to_string();
            parts.push(Part::Number(trimmed.len(), trimmed));
        } else {
            parts.push(Part::Text(current.to_lowercase()));
        }
        current.clear();
    };
    for c in text.chars() {
        if c.is_ascii_digit() != digits {
            flush(&mut current, digits, &mut parts);
            digits = !digits;
        }
        current.push(c);
    }
    flush(&mut current, digits, &mut parts);
    parts
}

/// Tri naturel : « Prise 2 » avant « Prise 10 ».
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_parts(a).cmp(&natural_parts(b))
}

/// Date de tournage (ou de modification du fichier), en secondes.
pub fn date_key(media: &MediaInfo) -> f64 {
    if let Some(text) = &media.creation_time {
        if let Ok(date) = chrono::DateTime::parse_from_rfc3339(text) {
            return date.timestamp_millis() as f64 / 1000.0;
        }
        if let Ok(date) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f") {
            if let Some(local) = date.and_local_timezone(chrono::Local).earliest() {
                return local.timestamp_millis() as f64 / 1000.0;
            }
        }
    }
    fs::metadata(&media.path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64())
}

pub fn sort_media<T>(items: &mut [T], mode: &str, media: impl Fn(&T) -> &MediaInfo) {
    match mode {
        "name" => items.sort_by(|a, b| natural_cmp(&media(a).name(), &media(b).name())),
        "date" => items.sort_by(|a, b| date_key(media(a)).total_cmp(&date_key(media(b)))),
        _ => {}
    }
}

pub fn stem_of(name: &str) -> String {
    Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string())
}

/// Chemins des XML : `base` est un fichier .xml, une base de nom, un dossier, ou absent
/// (à côté du premier fichier).
pub fn output_paths(base: Option<&str>, first: &MediaInfo, targets: &[Flavor]) -> Result<Vec<(Flavor, String)>, String> {
    let describe = |path: &Path, e: std::io::Error| format!("{} : {e}", path.display());
    let default_name = format!("{}_euhpen", stem_of(&first.name()));
    let mut base: String = match base {
        None => Path::new(&first.path).with_file_name(&default_name).to_string_lossy().into_owned(),
        Some(text) if Path::new(text).is_dir() || text.ends_with('/') || text.ends_with(std::path::MAIN_SEPARATOR) => {
            let dir = Path::new(text);
            fs::create_dir_all(dir).map_err(|e| describe(dir, e))?;
            dir.join(&default_name).to_string_lossy().into_owned()
        }
        Some(text) => {
            if let Some(parent) = Path::new(text).parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent).map_err(|e| describe(parent, e))?;
            }
            text.to_string()
        }
    };
    if base.to_lowercase().ends_with(".xml") {
        if let [only] = targets {
            return Ok(vec![(*only, base)]);
        }
        base.truncate(base.len() - 4);
    }
    Ok(targets.iter().map(|t| (*t, format!("{base}.{}.xml", t.as_str()))).collect())
}

/// Montre un fichier dans l'explorateur du système.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = hidden_command("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = hidden_command("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let folder = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let _ = hidden_command("xdg-open").arg(folder).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tri_naturel() {
        let mut names = vec!["Prise 10.mov", "prise 2.mov", "Prise 1.mov", "A.mov"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["A.mov", "Prise 1.mov", "prise 2.mov", "Prise 10.mov"]);
        assert_eq!(natural_cmp("a01", "a1"), Ordering::Equal);
    }

    #[test]
    fn reconnait_les_medias() {
        assert!(is_media(Path::new("a.MOV")));
        assert!(is_media(Path::new("a.m2ts")));
        assert!(!is_media(Path::new("a.txt")));
    }
}
