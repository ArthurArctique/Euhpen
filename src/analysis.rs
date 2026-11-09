//! ANALYSE : l'étape coûteuse, faite une seule fois par fichier et mise en cache.
//!
//! Le résultat (métadonnées + enveloppe de volume) est un JSON indexé par l'empreinte du fichier
//! et la version de l'analyse. La transcription est stockée à côté, dans un fichier indexé en
//! plus par modèle et langue, pour qu'un changement de langue ne refasse pas l'enveloppe.

use crate::audio::{compute_envelope, HOP_SECONDS};
use crate::cache::{analysis_dir, file_fingerprint, read_json, write_json};
use crate::probe::{absolute, probe, MediaInfo};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// À incrémenter dès que le contenu de l'analyse audio change.
pub const AUDIO_ANALYSIS_VERSION: i64 = 2;

#[derive(Clone, Debug)]
pub struct Envelope {
    /// Pas entre deux valeurs, en secondes.
    pub hop: f64,
    /// RMS en dBFS, valeur i = audio [i*hop, (i+1)*hop).
    pub db: Vec<f32>,
    /// Niveau de parole de chaque canal.
    pub channel_db: Option<Vec<f64>>,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub fingerprint: String,
    pub media: MediaInfo,
    pub envelope: Envelope,
}

#[derive(Serialize, Deserialize)]
struct EnvelopeFile {
    hop: f64,
    db: Vec<f64>,
    #[serde(default)]
    channel_db: Option<Vec<f64>>,
}

#[derive(Serialize, Deserialize)]
struct AnalysisFile {
    version: i64,
    #[serde(default)]
    euhpen: String,
    media: MediaInfo,
    envelope: EnvelopeFile,
}

fn cache_path(fingerprint: &str) -> PathBuf {
    analysis_dir(fingerprint).join(format!("audio-v{AUDIO_ANALYSIS_VERSION}.json"))
}

/// Au dixième de dB dans le cache : une analyse fraîche et une analyse relue du cache donnent
/// ainsi exactement les mêmes coupes.
fn round_db(value: f32) -> f32 {
    (value * 10.0).round_ties_even() / 10.0
}

/// Analyse un fichier. Renvoie (analyse, venait_du_cache).
pub fn analyze(
    path: &str,
    use_cache: bool,
    progress: &mut dyn FnMut(f64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(Analysis, bool), String> {
    let path = absolute(path);
    let fingerprint = file_fingerprint(Path::new(&path))?;
    let cache_file = cache_path(&fingerprint);

    if use_cache {
        if let Some(data) = read_json::<AnalysisFile>(&cache_file) {
            if data.version == AUDIO_ANALYSIS_VERSION {
                let mut media = data.media;
                media.path = path; // le fichier a pu être déplacé ou renommé
                let envelope = Envelope {
                    hop: data.envelope.hop,
                    db: data.envelope.db.iter().map(|v| *v as f32).collect(),
                    channel_db: data.envelope.channel_db,
                };
                return Ok((Analysis { fingerprint, media, envelope }, true));
            }
        }
    }

    let media = probe(&path)?;
    let result = compute_envelope(&media, progress, cancelled)?;
    let db: Vec<f32> = result.db.iter().map(|v| round_db(*v)).collect();
    let file = AnalysisFile {
        version: AUDIO_ANALYSIS_VERSION,
        euhpen: crate::VERSION.to_string(),
        media: media.clone(),
        envelope: EnvelopeFile {
            hop: HOP_SECONDS,
            db: db.iter().map(|v| ((*v as f64) * 10.0).round() / 10.0).collect(),
            channel_db: Some(result.channel_db.clone()),
        },
    };
    write_json(&cache_file, &file)?;
    let envelope = Envelope { hop: HOP_SECONDS, db, channel_db: Some(result.channel_db) };
    Ok((Analysis { fingerprint, media, envelope }, false))
}
