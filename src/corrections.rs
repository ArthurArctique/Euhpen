//! Corrections manuelles : inversion du sort d'un passage, fichier `overrides.json`, et journal
//! local réutilisable à l'entraînement (rien ne sort de la machine).

use crate::cache::{analysis_dir, read_json, write_json};
use crate::config::config_dir;
use crate::decision::{CutPlan, Overrides, Reason};
use crate::intervals::{subtract, union, Interval};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn overrides_path(fingerprint: &str) -> PathBuf {
    analysis_dir(fingerprint).join("overrides.json")
}

pub fn load_overrides(fingerprint: &str) -> Overrides {
    read_json(&overrides_path(fingerprint)).unwrap_or_default()
}

pub fn save_overrides(fingerprint: &str, overrides: &Overrides) -> Result<(), String> {
    write_json(&overrides_path(fingerprint), overrides)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub action: &'static str,
    pub start: f64,
    pub end: f64,
    pub reason: String,
}

fn contains(interval: &Interval, t: f64) -> bool {
    interval.0 <= t && t < interval.1
}

/// Inverse le sort du passage à l'instant `t` : coupé ↔ gardé.
///
/// Avec `span` (un mot de la transcription), seul ce morceau est coupé, pas tout le passage
/// gardé qui le contient.
pub fn toggle(plan: &CutPlan, ov: &mut Overrides, t: f64, span: Option<Interval>) -> Option<Change> {
    if let Some(piece) = plan.removed.iter().find(|p| p.start <= t && t < p.end) {
        if piece.reason == Reason::Manual {
            ov.force_cut.retain(|i| !contains(i, t));
            return Some(Change { action: "undo", start: t, end: t, reason: String::new() });
        }
        let interval = (piece.start, piece.end);
        ov.force_cut = subtract(&ov.force_cut, &[interval]);
        let mut keep = ov.force_keep.clone();
        keep.push(interval);
        ov.force_keep = union(&keep);
        return Some(Change { action: "keep", start: piece.start, end: piece.end, reason: piece.reason.as_str().to_string() });
    }
    if ov.force_keep.iter().any(|i| contains(i, t)) {
        ov.force_keep.retain(|i| !contains(i, t));
        return Some(Change { action: "undo", start: t, end: t, reason: String::new() });
    }
    let mut kept = *plan.kept.iter().find(|i| contains(i, t))?;
    if let Some(span) = span {
        kept = (kept.0.max(span.0), kept.1.min(span.1));
    }
    if kept.1 <= kept.0 {
        return None;
    }
    ov.force_keep = subtract(&ov.force_keep, &[kept]);
    let mut cut = ov.force_cut.clone();
    cut.push(kept);
    ov.force_cut = union(&cut);
    Some(Change {
        action: "cut",
        start: kept.0,
        end: kept.1,
        reason: if span.is_some() { "word" } else { "passage" }.to_string(),
    })
}

// --- fichiers réservés à l'évaluation ---------------------------------------------------

/// « Test audio euhh.mp3 » : jamais d'entraînement ni de réglage dessus.
const HOLDOUT_SHA256: &str = "bc6d8d0d71abbabdbc4bdca1374a86fe2763dbbdf5d32a43afd24bab33a275bf";
const HOLDOUT_NAME: &str = "test audio euhh";

fn content_hash(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut digest = Sha256::new();
    let mut block = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut block).ok()?;
        if read == 0 {
            break;
        }
        digest.update(&block[..read]);
    }
    Some(digest.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn is_holdout(source: &str) -> bool {
    static KNOWN: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let path = Path::new(source);
    if path.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase() == HOLDOUT_NAME) {
        return true;
    }
    let known = KNOWN.get_or_init(Default::default);
    if let Some(&answer) = known.lock().unwrap().get(source) {
        return answer;
    }
    let answer = content_hash(path).is_some_and(|hash| hash == HOLDOUT_SHA256);
    known.lock().unwrap().insert(source.to_string(), answer);
    answer
}

/// Garde la correction pour un entraînement futur. Se fait en arrière-plan (l'empreinte d'un
/// long rush prend du temps) et un échec ne gêne jamais l'interface.
pub fn journal_correction(fingerprint: &str, source: &str, change: Change) {
    let (fingerprint, source) = (fingerprint.to_string(), source.to_string());
    std::thread::spawn(move || {
        if is_holdout(&source) {
            return;
        }
        let target = config_dir().join("finetune").join("corrections.jsonl");
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let line = serde_json::json!({
            "fingerprint": fingerprint,
            "source": source,
            "action": change.action,
            "start": change.start,
            "end": change.end,
            "reason": change.reason,
        });
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(target) {
            let _ = writeln!(file, "{line}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{CutPiece, Levels};

    fn plan() -> CutPlan {
        CutPlan {
            duration: 10.0,
            levels: Levels { threshold_db: -40.0, noise_floor_db: None, speech_db: None },
            kept: vec![(0.0, 4.0), (6.0, 10.0)],
            removed: vec![CutPiece { start: 4.0, end: 6.0, reason: Reason::Silence }],
            events: vec![],
        }
    }

    #[test]
    fn garde_un_passage_coupe() {
        let mut ov = Overrides::default();
        let change = toggle(&plan(), &mut ov, 5.0, None).unwrap();
        assert_eq!(change.action, "keep");
        assert_eq!(change.reason, "silence");
        assert_eq!(ov.force_keep, vec![(4.0, 6.0)]);
    }

    #[test]
    fn coupe_un_passage_ou_un_mot() {
        let mut ov = Overrides::default();
        assert_eq!(toggle(&plan(), &mut ov, 1.0, None).unwrap().reason, "passage");
        assert_eq!(ov.force_cut, vec![(0.0, 4.0)]);
        let mut ov = Overrides::default();
        assert_eq!(toggle(&plan(), &mut ov, 1.0, Some((0.5, 1.5))).unwrap().reason, "word");
        assert_eq!(ov.force_cut, vec![(0.5, 1.5)]);
    }

    #[test]
    fn annule_une_correction() {
        let mut ov = Overrides { force_keep: vec![(1.0, 2.0)], force_cut: vec![] };
        assert_eq!(toggle(&plan(), &mut ov, 1.5, None).unwrap().action, "undo");
        assert!(ov.force_keep.is_empty());
        let mut manual = plan();
        manual.removed[0].reason = Reason::Manual;
        let mut ov = Overrides { force_keep: vec![], force_cut: vec![(4.0, 6.0)] };
        assert_eq!(toggle(&manual, &mut ov, 5.0, None).unwrap().action, "undo");
        assert!(ov.force_cut.is_empty());
    }
}
