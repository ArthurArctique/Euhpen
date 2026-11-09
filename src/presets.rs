//! Presets : jeux de réglages nommés. Ceux d'Euhpen sont fixes, ceux de l'utilisateur sont
//! enregistrés dans le dossier de configuration.

use crate::cache::{read_json, write_json};
use crate::config::config_dir;
use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Preset {
    pub name: String,
    pub builtin: bool,
    pub description: String,
    pub settings: Settings,
}

fn builtin() -> Vec<(&'static str, &'static str, Settings)> {
    let d = Settings::default();
    vec![
        ("Standard", "silences de plus de 0,4 s, air naturel autour de la parole", d.clone()),
        (
            "Sans hésitations",
            "comme Standard, et coupe les « euh » et les syllabes étirées",
            Settings { hesitations: true, ..d.clone() },
        ),
        (
            "Dynamique (YouTube)",
            "rythme serré : presque tous les blancs et les hésitations",
            Settings {
                min_silence: 0.25,
                pad_before: 0.06,
                pad_after: 0.08,
                min_keep: 0.20,
                hesitations: true,
                ..d.clone()
            },
        ),
        (
            "Interview posée",
            "ne coupe que les vrais blancs, garde de l'air",
            Settings { min_silence: 0.70, pad_before: 0.15, pad_after: 0.25, min_keep: 0.30, ..d.clone() },
        ),
        (
            "Podcast",
            "ne coupe que les longs silences",
            Settings { min_silence: 1.00, pad_before: 0.20, pad_after: 0.30, min_keep: 0.40, ..d },
        ),
    ]
}

fn is_builtin(name: &str) -> bool {
    builtin().iter().any(|(n, _, _)| *n == name)
}

fn path() -> PathBuf {
    config_dir().join("presets.json")
}

#[derive(Serialize, Deserialize, Default)]
struct PresetFile {
    #[serde(default)]
    presets: BTreeMap<String, serde_json::Value>,
}

fn user_presets() -> BTreeMap<String, serde_json::Value> {
    let mut map = read_json::<PresetFile>(&path()).unwrap_or_default().presets;
    map.retain(|name, value| value.is_object() && !is_builtin(name));
    map
}

pub fn list_presets() -> Vec<Preset> {
    let mut out: Vec<Preset> = builtin()
        .into_iter()
        .map(|(name, description, settings)| Preset {
            name: name.to_string(),
            builtin: true,
            description: description.to_string(),
            settings,
        })
        .collect();
    let mut user: Vec<(String, serde_json::Value)> = user_presets().into_iter().collect();
    user.sort_by_key(|(name, _)| name.to_lowercase());
    for (name, value) in user {
        if let Some(settings) = Settings::from_value_lenient(&value) {
            out.push(Preset { name, builtin: false, description: String::new(), settings });
        }
    }
    out
}

pub fn get_preset(name: &str) -> Option<Settings> {
    list_presets().into_iter().find(|p| p.name == name).map(|p| p.settings)
}

pub fn save_preset(name: &str, settings: &Settings) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("donnez un nom au preset.".to_string());
    }
    if name.chars().count() > 60 {
        return Err("nom de preset trop long (60 caractères au plus).".to_string());
    }
    if is_builtin(name) {
        return Err(format!("« {name} » est un preset d'Euhpen : choisissez un autre nom."));
    }
    let mut user = user_presets();
    let value = serde_json::to_value(settings.clone().validate()?).map_err(|e| e.to_string())?;
    user.insert(name.to_string(), value);
    write_json(&path(), &PresetFile { presets: user })
}

pub fn delete_preset(name: &str) -> Result<(), String> {
    if is_builtin(name) {
        return Err("les presets d'Euhpen ne peuvent pas être supprimés.".to_string());
    }
    let mut user = user_presets();
    if user.remove(name).is_none() {
        return Err(format!("preset inconnu : {name}"));
    }
    write_json(&path(), &PresetFile { presets: user })
}
