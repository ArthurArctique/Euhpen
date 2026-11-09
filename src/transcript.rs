//! Lecture des transcriptions écrites par le processus Python du modèle.
//!
//! Whisper donne les mots et leurs horodatages ; la tête entraînée sur du français y ajoute les
//! hésitations sous forme de jetons entre crochets (`[euh]`, `[étirement]`). Le résultat est mis
//! en cache, indexé par l'empreinte du fichier, le modèle (avec sa révision) et la langue.

use crate::cache::{analysis_dir, home_dir, read_json};
use crate::config::config_dir;
use regex::Regex;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const TRANSCRIPT_VERSION: i64 = 1;
pub const WHISPER_MODEL: &str = "whisper-turbo";
pub const HEAD_SUFFIX: &str = "+head";
pub const WHISPER_HEAD_MODEL: &str = "whisper-turbo+head";
pub const MODEL_REPO: &str = "openai/whisper-large-v3-turbo";
pub const MODEL_REVISION: &str = "41f01f3fe87f28c78e2fbf8b568835947dd65ed9";
/// Fichiers sans lesquels transformers ne peut pas charger le modèle.
pub const MODEL_FILES: [&str; 6] = [
    "config.json",
    "generation_config.json",
    "preprocessor_config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "model.safetensors",
];
pub const MODEL_LABEL: &str = "Whisper + détection des hésitations";
pub const MODEL_ATTRIBUTION: &str = "Whisper (OpenAI, licence MIT)";
pub const HEAD_ATTRIBUTION: &str = "Détection entraînée sur le corpus Rhapsodie (CC BY-NC-SA 3.0 FR)";
pub const HEAD_LICENSE: &str = "Les poids de la détection sont fournis avec Euhpen sous licence CC BY-NC-SA 3.0 FR : à créditer, sans usage commercial. Whisper n'est pas inclus : il est téléchargé depuis Hugging Face au premier usage.";
pub const HEAD_LICENSE_URL: &str = "https://creativecommons.org/licenses/by-nc-sa/3.0/fr/";

/// Langues proposées (la langue est toujours passée explicitement au modèle).
pub const LANGUAGES: [(&str, &str); 24] = [
    ("fr", "Français"),
    ("en", "English"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("nl", "Nederlands"),
    ("pl", "Polski"),
    ("ru", "Русский"),
    ("tr", "Türkçe"),
    ("ja", "日本語"),
    ("zh", "中文"),
    ("ko", "한국어"),
    ("ar", "العربية"),
    ("sv", "Svenska"),
    ("da", "Dansk"),
    ("no", "Norsk"),
    ("fi", "Suomi"),
    ("cs", "Čeština"),
    ("ro", "Română"),
    ("uk", "Українська"),
    ("el", "Ελληνικά"),
    ("hu", "Magyar"),
    ("ca", "Català"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Word,
    Hesitation,
    Cough,
    Laugh,
    Breath,
    Event,
}

/// Fin d'un mot coupé en deux par la tête.
const WORD_TAIL: &str = "…";

const HESITATION_TOKENS: [&str; 29] = [
    "uh", "um", "uhm", "umm", "uhh", "hm", "hmm", "hmmm", "mm", "mmm", "mhm", "er", "erm", "ah", "eh", "ehm", "euh",
    "heu", "euhm", "hum", "äh", "ähm", "öh", "öhm", "em", "eeh", "filler", "hesitation", "étirement",
];
const LAUGH_TOKENS: [&str; 10] =
    ["laugh", "laughs", "laughter", "laughing", "chuckle", "chuckles", "giggle", "giggles", "rire", "rires"];
const COUGH_TOKENS: [&str; 11] = [
    "cough",
    "coughs",
    "coughing",
    "throat clearing",
    "clears throat",
    "throat clear",
    "clearing throat",
    "sneeze",
    "sneezes",
    "sneezing",
    "toux",
];
const BREATH_TOKENS: [&str; 17] = [
    "breath",
    "breathing",
    "breathes",
    "inhale",
    "inhales",
    "inhaling",
    "exhale",
    "exhales",
    "sigh",
    "sighs",
    "sniff",
    "sniffs",
    "sniffing",
    "lip smack",
    "smack",
    "click",
    "tsk",
];
/// Hésitations écrites comme des mots ordinaires (sans crochets).
const PLAIN_FILLERS: [&str; 13] =
    ["euh", "heu", "euhm", "hum", "hmm", "hm", "mmh", "uh", "um", "uhm", "erm", "äh", "ähm"];

fn bracket_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\W*\[([^\]]+)\]\W*$").unwrap())
}

fn bracket_content(text: &str) -> Option<&str> {
    bracket_regex().captures(text.trim()).map(|c| c.get(1).unwrap().as_str().trim())
}

/// Nature d'un mot : mot réel, hésitation, toux, rire, souffle ou autre événement.
pub fn classify(text: &str) -> Kind {
    if let Some(content) = bracket_content(text) {
        let token = content.to_lowercase().replace('_', " ");
        let token = token.as_str();
        if HESITATION_TOKENS.contains(&token) {
            return Kind::Hesitation;
        }
        if LAUGH_TOKENS.contains(&token) || token.starts_with("laugh") {
            return Kind::Laugh;
        }
        if COUGH_TOKENS.contains(&token) || token.starts_with("cough") {
            return Kind::Cough;
        }
        if BREATH_TOKENS.contains(&token) || token.starts_with("breath") {
            return Kind::Breath;
        }
        return Kind::Event;
    }
    let bare: String = text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
    if PLAIN_FILLERS.contains(&bare.as_str()) {
        return Kind::Hesitation;
    }
    Kind::Word
}

/// Le jeton entre crochets tel que le modèle l'a écrit (`[UH]`), sinon rien.
pub fn bracket_token(text: &str) -> Option<String> {
    bracket_content(text).map(|c| format!("[{c}]"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    /// Secondes, dans le temps de la piste audio.
    pub start: f64,
    pub end: f64,
    pub probability: Option<f64>,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Transcript {
    pub model: String,
    pub revision: String,
    pub language: String,
    pub words: Vec<Word>,
    pub device: String,
    pub processing_time: f64,
    pub created: String,
}

#[derive(Deserialize)]
struct TranscriptFile {
    version: i64,
    model: String,
    #[serde(default)]
    revision: String,
    language: String,
    #[serde(default)]
    device: String,
    #[serde(default)]
    processing_time: f64,
    #[serde(default)]
    created: String,
    words: Vec<(String, f64, f64, Option<f64>)>,
}

impl Transcript {
    /// Tous les jetons entre crochets rencontrés, du plus fréquent au moins fréquent.
    pub fn bracket_tokens(&self) -> Vec<(String, usize)> {
        let mut order: Vec<(String, usize)> = Vec::new();
        let mut positions: HashMap<String, usize> = HashMap::new();
        for token in self.words.iter().filter_map(|w| bracket_token(&w.text)) {
            match positions.get(&token) {
                Some(&at) => order[at].1 += 1,
                None => {
                    positions.insert(token.clone(), order.len());
                    order.push((token, 1));
                }
            }
        }
        order.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        order
    }

    pub fn count(&self, kind: Kind) -> usize {
        self.words.iter().filter(|w| w.kind == kind).count()
    }

    /// Milieux des mots qu'un segment gardé trop court ne doit pas emporter.
    ///
    /// Seulement avec la tête : elle coupe à l'intérieur des mots, et ce qui reste entre deux
    /// coupes est souvent un mot entier mais bref (« de », « un »).
    pub fn protected_word_centers(&self) -> Vec<f64> {
        if !self.model.ends_with(HEAD_SUFFIX) {
            return Vec::new();
        }
        self.words
            .iter()
            .filter(|w| w.kind == Kind::Word && w.text != WORD_TAIL)
            .map(|w| (w.start + w.end) / 2.0)
            .collect()
    }
}

fn parse_file(path: &Path) -> Option<Transcript> {
    let data: TranscriptFile = read_json(path)?;
    if data.version != TRANSCRIPT_VERSION {
        return None;
    }
    let words = data
        .words
        .into_iter()
        .map(|(text, start, end, probability)| {
            let kind = classify(&text);
            Word { text, start, end, probability, kind }
        })
        .collect();
    Some(Transcript {
        model: data.model,
        revision: data.revision,
        language: data.language,
        words,
        device: data.device,
        processing_time: data.processing_time,
        created: data.created,
    })
}

/// Dossier de l'application (celui qui contient `python/euhpen/worker.py`).
pub fn project_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("EUHPEN_ROOT").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(root));
    }
    let is_root = |dir: &Path| dir.join("python").join("euhpen").join("worker.py").is_file();
    let exe = std::env::current_exe().ok()?;
    let found = exe.ancestors().find(|dir| is_root(dir)).map(Path::to_path_buf);
    found.or_else(|| std::env::current_dir().ok().filter(|dir| is_root(dir)))
}

const HEAD_FILES: [&str; 2] = ["head.pt", "head.json"];

/// Dossier de la tête : celle réentraînée par l'utilisateur, sinon les poids livrés.
pub fn head_dir() -> Option<PathBuf> {
    let own = config_dir().join("finetune").join(WHISPER_MODEL);
    if HEAD_FILES.iter().all(|name| own.join(name).is_file()) {
        return Some(own);
    }
    let shipped = project_root()?.join("python").join("euhpen").join("weights").join(WHISPER_MODEL);
    HEAD_FILES.iter().all(|name| shipped.join(name).is_file()).then_some(shipped)
}

/// Empreinte des poids installés et des jetons écrits : une tête réentraînée ne réutilise pas
/// d'anciennes détections.
pub fn head_revision() -> Option<String> {
    let folder = head_dir()?;
    let mut digest = Sha1::new();
    digest.update("[('hesitation', '[euh]'), ('lengthening', '[étirement]')]".as_bytes());
    for name in HEAD_FILES {
        digest.update(fs::read(folder.join(name)).ok()?);
    }
    let hex: String = digest.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Some(hex[..10].to_string())
}

fn safe_model(model: &str) -> String {
    model.chars().map(|c| if c.is_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' }).collect()
}

/// Transcription en cache pour ce fichier, ce modèle et cette langue (toute révision).
pub fn cached_transcript(fingerprint: &str, model: &str, language: &str) -> Option<Transcript> {
    let folder = analysis_dir(fingerprint);
    let entries = fs::read_dir(&folder).ok()?;
    let revision = if model.ends_with(HEAD_SUFFIX) { format!(r"\w+h{}", head_revision()?) } else { r"\w+".to_string() };
    let pattern = format!(
        r"^words-{}-{revision}-{}-v{TRANSCRIPT_VERSION}\.json$",
        regex::escape(&safe_model(model)),
        regex::escape(language)
    );
    let pattern = Regex::new(&pattern).ok()?;
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| pattern.is_match(&e.file_name().to_string_lossy()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    candidates.sort_by_key(|(time, _)| *time);
    candidates.iter().rev().find_map(|(_, path)| parse_file(path))
}

/// Dossier du modèle s'il est entièrement téléchargé (aucun accès réseau).
pub fn local_model_path() -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let hub = env("HF_HUB_CACHE").or_else(|| env("HF_HOME").map(|h| h.join("hub"))).unwrap_or_else(|| {
        env("XDG_CACHE_HOME").unwrap_or_else(|| home_dir().join(".cache")).join("huggingface").join("hub")
    });
    let folder = hub.join(format!("models--{}", MODEL_REPO.replace('/', "--"))).join("snapshots").join(MODEL_REVISION);
    MODEL_FILES.iter().all(|name| folder.join(name).is_file()).then_some(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classe_les_mots() {
        assert_eq!(classify("bonjour"), Kind::Word);
        assert_eq!(classify("[euh]"), Kind::Hesitation);
        assert_eq!(classify(" [étirement] "), Kind::Hesitation);
        assert_eq!(classify("euh,"), Kind::Hesitation);
        assert_eq!(classify("[UH]"), Kind::Hesitation);
        assert_eq!(classify("[Laughter]"), Kind::Laugh);
        assert_eq!(classify("[throat_clearing]"), Kind::Cough);
        assert_eq!(classify("[breathing]"), Kind::Breath);
        assert_eq!(classify("[musique]"), Kind::Event);
    }

    #[test]
    fn jetons() {
        assert_eq!(bracket_token(" [UH]. ").as_deref(), Some("[UH]"));
        assert_eq!(bracket_token("mot"), None);
        let w = |t: &str| Word { text: t.into(), start: 0.0, end: 1.0, probability: None, kind: classify(t) };
        let tr = Transcript { words: vec![w("[a]"), w("[b]"), w("[b]")], ..Default::default() };
        assert_eq!(tr.bracket_tokens(), vec![("[b]".to_string(), 2), ("[a]".to_string(), 1)]);
    }

    #[test]
    fn mots_proteges_seulement_avec_la_tete() {
        let word = Word { text: "de".into(), start: 1.0, end: 2.0, probability: None, kind: Kind::Word };
        let tail = Word { text: WORD_TAIL.into(), start: 3.0, end: 4.0, probability: None, kind: Kind::Word };
        let mut tr = Transcript { model: WHISPER_MODEL.into(), words: vec![word, tail], ..Default::default() };
        assert!(tr.protected_word_centers().is_empty());
        tr.model = WHISPER_HEAD_MODEL.into();
        assert_eq!(tr.protected_word_centers(), vec![1.5]);
    }

    #[test]
    fn nom_de_fichier_du_modele() {
        assert_eq!(safe_model(WHISPER_HEAD_MODEL), "whisper-turbo_head");
    }
}
