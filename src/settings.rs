//! Réglages de la détection (sérialisables en JSON pour les presets).

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Seuil de volume : relatif au bruit de fond, ou une valeur fixe en dBFS.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Threshold {
    #[default]
    Auto,
    Db(f64),
}

impl Serialize for Threshold {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Threshold::Auto => s.serialize_str("auto"),
            Threshold::Db(v) => s.serialize_f64(*v),
        }
    }
}

impl<'de> Deserialize<'de> for Threshold {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Threshold;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("« auto » ou un nombre en dBFS")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Threshold, E> {
                if v == "auto" {
                    return Ok(Threshold::Auto);
                }
                v.parse::<f64>()
                    .map(Threshold::Db)
                    .map_err(|_| E::custom("le seuil doit être « auto » ou un nombre en dBFS"))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Threshold, E> {
                Ok(Threshold::Db(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Threshold, E> {
                Ok(Threshold::Db(v as f64))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Threshold, E> {
                Ok(Threshold::Db(v as f64))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Seuil de volume : "auto" (relatif au bruit de fond) ou une valeur en dBFS.
    pub threshold: Threshold,
    /// Une fois la parole détectée, elle ne s'arrête que sous (seuil − hystérésis).
    pub hysteresis_db: f64,
    /// Les silences plus courts sont gardés (rythme naturel).
    pub min_silence: f64,
    /// Un son isolé plus court (clic, claquement de langue) compte comme du silence.
    pub min_sound: f64,
    /// Air gardé avant la reprise de la parole.
    pub pad_before: f64,
    /// Air gardé après la fin de la parole.
    pub pad_after: f64,
    /// Un segment gardé plus court est supprimé (fusionné avec les coupes voisines).
    pub min_keep: f64,
    /// Couper les hésitations (« euh », « hum », syllabes étirées…).
    pub hesitations: bool,
    /// Couper aussi les toux, raclements de gorge et éternuements.
    pub coughs: bool,
    /// Couper les rires (gardés par défaut : ils font souvent partie du propos).
    pub laughs: bool,
    /// Couper les respirations et bruits de bouche notés par le modèle.
    pub breaths: bool,
    /// Marge ajoutée de chaque côté d'une hésitation coupée.
    pub event_margin: f64,
    /// Une hésitation dont la probabilité est plus faible n'est pas coupée.
    pub min_probability: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            threshold: Threshold::Auto,
            hysteresis_db: 3.0,
            min_silence: 0.40,
            min_sound: 0.05,
            pad_before: 0.10,
            pad_after: 0.15,
            min_keep: 0.25,
            hesitations: false,
            coughs: true,
            laughs: false,
            breaths: false,
            event_margin: 0.05,
            min_probability: 0.50,
        }
    }
}

const KNOWN_KEYS: [&str; 13] = [
    "threshold",
    "hysteresis_db",
    "min_silence",
    "min_sound",
    "pad_before",
    "pad_after",
    "min_keep",
    "hesitations",
    "coughs",
    "laughs",
    "breaths",
    "event_margin",
    "min_probability",
];

impl Settings {
    pub fn validate(self) -> Result<Settings, String> {
        if let Threshold::Db(v) = self.threshold {
            if !(-120.0..=0.0).contains(&v) {
                return Err("le seuil doit être compris entre -120 et 0 dBFS".to_string());
            }
        }
        let durations = [
            ("hysteresis_db", self.hysteresis_db),
            ("min_silence", self.min_silence),
            ("min_sound", self.min_sound),
            ("pad_before", self.pad_before),
            ("pad_after", self.pad_after),
            ("min_keep", self.min_keep),
            ("event_margin", self.event_margin),
        ];
        for (name, value) in durations {
            if value < 0.0 {
                return Err(format!("{name} ne peut pas être négatif"));
            }
        }
        if !(0.0..=1.0).contains(&self.min_probability) {
            return Err("min_probability doit être compris entre 0 et 1".to_string());
        }
        Ok(self)
    }

    /// Lecture stricte (fichier passé à `--settings`) : une clé inconnue est une erreur.
    pub fn from_json_strict(text: &str) -> Result<Settings, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let map = value.as_object().ok_or("les réglages doivent être un objet JSON")?;
        let mut unknown: Vec<&str> =
            map.keys().map(String::as_str).filter(|k| !KNOWN_KEYS.contains(k)).collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(format!("réglages inconnus : {}", unknown.join(", ")));
        }
        let settings: Settings = serde_json::from_value(value).map_err(|e| e.to_string())?;
        settings.validate()
    }

    /// Lecture souple (préférences, presets) : les clés inconnues sont ignorées.
    pub fn from_value_lenient(value: &serde_json::Value) -> Option<Settings> {
        serde_json::from_value::<Settings>(value.clone()).ok()?.validate().ok()
    }

    pub fn wants_transcript(&self) -> bool {
        self.hesitations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialise_comme_avant() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(json["threshold"], "auto");
        assert_eq!(json["min_silence"], 0.4);
        let manual = serde_json::from_str::<Settings>(r#"{"threshold": -40}"#).unwrap();
        assert_eq!(manual.threshold, Threshold::Db(-40.0));
        assert_eq!(manual.min_keep, 0.25);
    }

    #[test]
    fn refuse_le_mauvais() {
        assert!(Settings::from_json_strict(r#"{"inconnu": 1}"#).is_err());
        assert!(Settings::from_json_strict(r#"{"min_silence": -1}"#).is_err());
        assert!(Settings::from_json_strict(r#"{"threshold": 5}"#).is_err());
        assert!(Settings::from_json_strict(r#"{"threshold": "auto"}"#).is_ok());
    }
}
