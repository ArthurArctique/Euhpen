//! Préférences de l'utilisateur (dossier de configuration, distinct du cache).

use crate::cache::{home_dir, read_json, write_json};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Dossier de configuration : EUHPEN_CONFIG_DIR, sinon l'emplacement standard du système.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = env_path("EUHPEN_CONFIG_DIR") {
        return dir;
    }
    if cfg!(target_os = "macos") {
        return home_dir().join("Library").join("Application Support").join("euhpen");
    }
    if cfg!(windows) {
        return env_path("APPDATA").unwrap_or_else(|| home_dir().join("AppData").join("Roaming")).join("euhpen");
    }
    env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home_dir().join(".config")).join("euhpen")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub language: String,
    pub proxy_height: u32,
    /// premiere | resolve | both
    pub target: String,
    /// combined : une séquence ; separate : une par fichier
    pub export_mode: String,
    /// None : à côté du premier fichier
    pub output_dir: Option<String>,
    /// name | date | list
    pub order: String,
    pub zero_timecode: bool,
    pub raw_paths: bool,
    pub preset: String,
    /// montage | original | cuts
    pub play_mode: String,
    /// Réglages en cours (None : ceux du preset)
    pub settings: Option<serde_json::Value>,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            language: "fr".to_string(),
            proxy_height: 540,
            target: "both".to_string(),
            export_mode: "combined".to_string(),
            output_dir: None,
            order: "name".to_string(),
            zero_timecode: false,
            raw_paths: false,
            preset: "Standard".to_string(),
            play_mode: "montage".to_string(),
            settings: None,
        }
    }
}

pub fn load_preferences() -> Preferences {
    read_json(&config_dir().join("preferences.json")).unwrap_or_default()
}

pub fn save_preferences(prefs: &Preferences) -> Result<(), String> {
    write_json(&config_dir().join("preferences.json"), prefs)
}

#[derive(Serialize, Deserialize, Default)]
struct SessionFile {
    #[serde(default)]
    files: Vec<String>,
}

pub fn load_session() -> Vec<String> {
    read_json::<SessionFile>(&config_dir().join("session.json")).unwrap_or_default().files
}

pub fn save_session(paths: &[String]) -> Result<(), String> {
    write_json(&config_dir().join("session.json"), &SessionFile { files: paths.to_vec() })
}
