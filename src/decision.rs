//! DÉCISION : fonction pure (analyse + transcription + réglages → segments gardés).
//!
//! Aucun accès disque ni appel à ffmpeg ici : l'interface rappelle [`decide`] à chaque
//! mouvement de curseur sans relancer l'analyse ni la transcription.
//!
//! Une hésitation coupée est d'abord comptée comme du silence : « mot … euh … mot » devient
//! un seul long silence, coupé avec l'air habituel autour des vrais mots. Elle est aussi
//! coupée directement ([début − marge, fin + marge]), avec des bords recalés sur le point
//! le plus calme à ±50 ms près.

use crate::analysis::Analysis;
use crate::audio::FLOOR_DB;
use crate::intervals::{clip, complement, subtract, total, union, Interval};
use crate::settings::{Settings, Threshold};
use crate::transcript::{Kind, Transcript};
use serde::{Deserialize, Serialize};

/// Seuil automatique : on sépare les fenêtres en deux groupes (bruit / parole) par la
/// méthode d'Otsu, puis on place le seuil à 35 % de l'écart entre leurs médianes, plus
/// près du bruit que de la parole pour ne pas rogner les fins de mots douces.
const AUTO_RATIO: f64 = 0.35;
/// En dessous, aucune séparation nette entre bruit et parole (musique, brouhaha…).
pub const LOW_CONTRAST_DB: f64 = 10.0;
const FALLBACK_THRESHOLD_DB: f64 = -40.0;
/// Les fenêtres de silence numérique (zéros) ne renseignent pas sur le bruit de fond.
const DIGITAL_SILENCE_DB: f32 = -95.0;
const EPS: f64 = 1e-9;
/// Les bords d'une coupe d'hésitation sont recalés sur le point le plus calme à ±50 ms.
const EDGE_SEARCH: f64 = 0.05;

/// Corrections manuelles, stockées à part et appliquées après le calcul automatique.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    pub force_keep: Vec<Interval>,
    pub force_cut: Vec<Interval>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.force_keep.is_empty() && self.force_cut.is_empty()
    }

    pub fn count(&self) -> usize {
        self.force_keep.len() + self.force_cut.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reason {
    Silence,
    Hesitation,
    Cough,
    Laugh,
    Breath,
    Event,
    Short,
    Manual,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Silence => "silence",
            Reason::Hesitation => "hesitation",
            Reason::Cough => "cough",
            Reason::Laugh => "laugh",
            Reason::Breath => "breath",
            Reason::Event => "event",
            Reason::Short => "short",
            Reason::Manual => "manual",
        }
    }

    fn of_kind(kind: Kind) -> Reason {
        match kind {
            Kind::Hesitation => Reason::Hesitation,
            Kind::Cough => Reason::Cough,
            Kind::Laugh => Reason::Laugh,
            Kind::Breath => Reason::Breath,
            Kind::Event | Kind::Word => Reason::Event,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutPiece {
    pub start: f64,
    pub end: f64,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levels {
    pub threshold_db: f64,
    pub noise_floor_db: Option<f64>,
    pub speech_db: Option<f64>,
}

impl Levels {
    pub fn low_contrast(&self) -> bool {
        match (self.noise_floor_db, self.speech_db) {
            (Some(floor), Some(speech)) => speech - floor < LOW_CONTRAST_DB,
            _ => false,
        }
    }
}

/// Un jeton non verbal de la transcription (hésitation, toux…) et son sort.
#[derive(Clone, Debug, PartialEq)]
pub struct EventCut {
    /// Position du mot dans la transcription.
    pub index: usize,
    pub text: String,
    pub kind: Kind,
    /// Temps du média.
    pub start: f64,
    pub end: f64,
    pub probability: Option<f64>,
    pub cut: bool,
    /// Pourquoi il n'est pas coupé.
    pub note: &'static str,
    /// Intervalle effectivement coupé (marge et bords recalés).
    pub cut_span: Option<Interval>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CutPlan {
    pub duration: f64,
    pub levels: Levels,
    pub kept: Vec<Interval>,
    pub removed: Vec<CutPiece>,
    pub events: Vec<EventCut>,
}

impl CutPlan {
    pub fn kept_duration(&self) -> f64 {
        total(&self.kept)
    }

    pub fn removed_by(&self, reason: Reason) -> f64 {
        self.removed.iter().filter(|p| p.reason == reason).map(|p| p.end - p.start).sum()
    }

    /// Nombre de jetons coupés d'une nature donnée.
    pub fn event_count(&self, kind: Kind) -> usize {
        self.events.iter().filter(|e| e.cut && e.kind == kind).count()
    }
}

// --- enveloppe --------------------------------------------------------------------------

/// Enveloppe recalée sur le temps du média : valeur i = [i*hop, (i+1)*hop).
///
/// Applique le décalage entre le début de l'audio et celui de la vidéo ; les zones
/// sans audio (avant le début ou après la fin de la piste) comptent comme silence.
pub fn media_levels(analysis: &Analysis) -> Vec<f32> {
    let env = &analysis.envelope;
    let duration = analysis.media.duration;
    let n = ((duration / env.hop - EPS).ceil() as i64).max(1) as usize;
    let mut grid = vec![FLOOR_DB as f32; n];
    let shift = (analysis.media.audio_offset / env.hop).round_ties_even() as i64;
    let src = (-shift).max(0) as usize;
    let dst = shift.max(0) as usize;
    let count = (env.db.len() as i64 - src as i64).min(n as i64 - dst as i64);
    if count > 0 {
        let count = count as usize;
        grid[dst..dst + count].copy_from_slice(&env.db[src..src + count]);
    }
    grid
}

/// Seuil d'Otsu sur un histogramme de 200 cases couvrant [-100, 0] dB.
fn otsu_split(values: &[f32]) -> Option<f64> {
    const BINS: usize = 200;
    let mut hist = [0.0_f64; BINS];
    for &value in values {
        if !(-100.0..=0.0).contains(&value) {
            continue;
        }
        // Comme numpy une fois ses corrections faites : case k = [bord k, bord k+1[, la dernière
        // fermée. Les bords sont des multiples de 0,5 dB : ce calcul en f64 est exact.
        let bin = (((value as f64 + 100.0) * 2.0).floor() as usize).min(BINS - 1);
        hist[bin] += 1.0;
    }
    let sum: f64 = hist.iter().sum();
    if sum == 0.0 {
        return None;
    }
    let center = |i: usize| -100.0 + (i as f64 + 0.5) * 0.5;
    let mass: f64 = hist.iter().enumerate().map(|(i, w)| w * center(i)).sum();
    let (mut w0, mut m0) = (0.0, 0.0);
    let (mut best, mut best_k) = (f64::NEG_INFINITY, 0);
    for (k, &weight) in hist.iter().enumerate() {
        w0 += weight;
        m0 += weight * center(k);
        let w1 = sum - w0;
        let mean0 = if w0 > 0.0 { m0 / w0 } else { 0.0 };
        let mean1 = if w1 > 0.0 { (mass - m0) / w1 } else { 0.0 };
        let between = (w0 * w1) * ((mean0 - mean1) * (mean0 - mean1));
        if between > best {
            best = between;
            best_k = k;
        }
    }
    if best <= 0.0 {
        return None;
    }
    Some(-100.0 + (best_k as f64 + 1.0) * 0.5)
}

fn median(values: &mut [f32]) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

pub fn compute_levels(levels: &[f32], settings: &Settings) -> Levels {
    let valid: Vec<f32> = levels.iter().copied().filter(|v| *v > DIGITAL_SILENCE_DB).collect();
    let (mut floor, mut speech) = (None, None);
    let split = if valid.len() >= 20 { otsu_split(&valid) } else { None };
    if let Some(split) = split {
        let split = split as f32;
        let mut low: Vec<f32> = valid.iter().copied().filter(|v| *v < split).collect();
        let mut high: Vec<f32> = valid.iter().copied().filter(|v| *v >= split).collect();
        if !low.is_empty() && !high.is_empty() {
            floor = Some(median(&mut low) as f64);
            speech = Some(median(&mut high) as f64);
        }
    }
    let threshold_db = match (settings.threshold, floor, speech) {
        (Threshold::Db(value), _, _) => value,
        (Threshold::Auto, Some(f), Some(s)) => f + AUTO_RATIO * (s - f),
        _ => FALLBACK_THRESHOLD_DB,
    };
    Levels { threshold_db, noise_floor_db: floor, speech_db: speech }
}

// --- détection des silences -------------------------------------------------------------

/// Vrai là où il y a du son, avec hystérésis.
///
/// Le son commence quand le volume atteint le seuil et ne s'arrête que lorsqu'il passe
/// sous (seuil − hystérésis) : la détection n'oscille pas autour du seuil.
pub fn sound_mask(levels: &[f32], threshold_db: f64, hysteresis_db: f64) -> Vec<bool> {
    let low = (threshold_db - hysteresis_db) as f32;
    let high = threshold_db as f32;
    let mut on = false; // avant la première valeur décidée : silence
    levels
        .iter()
        .map(|&level| {
            if level >= high {
                on = true;
            } else if level < low {
                on = false;
            }
            on
        })
        .collect()
}

/// Débuts et fins (exclues) des suites de `value`.
fn runs(mask: &[bool], value: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &m) in mask.iter().enumerate() {
        match (m == value, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, mask.len()));
    }
    out
}

fn drop_short_sounds(mask: &mut [bool], min_windows: i64) {
    if min_windows <= 1 {
        return;
    }
    for (a, b) in runs(mask, true) {
        if ((b - a) as i64) < min_windows {
            mask[a..b].fill(false);
        }
    }
}

fn silence_intervals(mask: &[bool], hop: f64, duration: f64) -> Vec<Interval> {
    let raw: Vec<Interval> = runs(mask, false).iter().map(|&(a, b)| (a as f64 * hop, b as f64 * hop)).collect();
    clip(&raw, 0.0, duration)
}

/// Intervalles à supprimer : silences assez longs, moins l'air gardé autour de la parole.
///
/// En début et en fin de fichier, il n'y a pas de parole à protéger de ce côté-là.
fn silence_cuts(silences: &[Interval], settings: &Settings, duration: f64) -> Vec<Interval> {
    let mut cuts = Vec::new();
    for &(start, end) in silences {
        if end - start < settings.min_silence - EPS {
            continue;
        }
        let a = if start <= EPS { start } else { start + settings.pad_after };
        let b = if end >= duration - EPS { end } else { end - settings.pad_before };
        if b > a {
            cuts.push((a, b));
        }
    }
    cuts
}

// --- hésitations et bruits --------------------------------------------------------------

fn kind_enabled(kind: Kind, settings: &Settings) -> bool {
    settings.hesitations
        && match kind {
            Kind::Hesitation => true,
            Kind::Cough => settings.coughs,
            Kind::Laugh => settings.laughs,
            Kind::Breath => settings.breaths,
            Kind::Event | Kind::Word => false,
        }
}

/// Instant le plus calme près de `target` (±50 ms), compris dans [low, high].
///
/// Le volume d'un bord est le plus fort des deux fenêtres de 10 ms qui l'entourent ; à
/// égalité, on garde le bord le plus proche de la cible.
fn quietest_edge(levels: &[f32], hop: f64, target: f64, low: f64, high: f64) -> f64 {
    let fallback = target.max(low).min(high);
    let lo = low.max(target - EDGE_SEARCH);
    let hi = high.min(target + EDGE_SEARCH);
    if hi < lo {
        return fallback;
    }
    let first = (lo / hop - EPS).ceil() as i64;
    let last = (hi / hop + EPS).floor() as i64;
    let size = levels.len() as i64;
    let mut best: Option<(f64, f64)> = None;
    let mut best_t = fallback;
    for k in first..=last {
        let before = if 0 < k && k <= size { levels[k as usize - 1] as f64 } else { FLOOR_DB };
        let after = if 0 <= k && k < size { levels[k as usize] as f64 } else { FLOOR_DB };
        let t = k as f64 * hop;
        let key = (before.max(after), (t - target).abs());
        if best.is_none_or(|b| key < b) {
            best_t = t;
            best = Some(key);
        }
    }
    best_t
}

/// Sort de chaque jeton non verbal : coupé ou non, et sur quel intervalle.
fn event_cuts(
    transcript: Option<&Transcript>,
    settings: &Settings,
    levels: &[f32],
    hop: f64,
    offset: f64,
    duration: f64,
) -> Vec<EventCut> {
    let Some(transcript) = transcript else {
        return Vec::new();
    };
    let mut words: Vec<(usize, &crate::transcript::Word)> =
        transcript.words.iter().enumerate().filter(|(_, w)| w.end > w.start).collect();
    words.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));

    let mut events = Vec::new();
    for (index, w) in words {
        if w.kind == Kind::Word {
            continue;
        }
        let (mut cut, mut note) = (false, "");
        if !kind_enabled(w.kind, settings) {
            note = "désactivé";
        } else if w.probability.is_some_and(|p| p < settings.min_probability) {
            note = "probabilité trop faible";
        } else {
            cut = true;
        }
        events.push(EventCut {
            index,
            text: w.text.clone(),
            kind: w.kind,
            start: w.start + offset,
            end: w.end + offset,
            probability: w.probability,
            cut,
            note,
            cut_span: None,
        });
    }

    // Les mots voisins ne limitent pas les coupes : Whisper les colle les uns aux autres, et
    // leurs bornes ne disent pas où la parole s'arrête.
    for e in events.iter_mut().filter(|e| e.cut) {
        let mut a = (e.start - settings.event_margin).max(0.0);
        let mut b = (e.end + settings.event_margin).min(duration);
        if b - a > 2.0 * hop {
            // Les horodatages sont approximatifs : chaque bord peut bouger vers le point le plus calme.
            a = quietest_edge(levels, hop, a, 0.0, b - hop);
            b = quietest_edge(levels, hop, b, a + hop, duration);
        }
        if b > a {
            e.cut_span = Some((a, b));
        } else {
            e.cut = false;
            e.note = "hors du fichier";
        }
    }
    events
}

// --- décision complète ------------------------------------------------------------------

pub fn decide(
    analysis: &Analysis,
    settings: &Settings,
    overrides: Option<&Overrides>,
    transcript: Option<&Transcript>,
) -> CutPlan {
    let duration = analysis.media.duration;
    let hop = analysis.envelope.hop;
    let levels = media_levels(analysis);
    let lv = compute_levels(&levels, settings);

    let events = event_cuts(transcript, settings, &levels, hop, analysis.media.audio_offset, duration);
    let event_spans: Vec<Interval> = events.iter().filter(|e| e.cut).filter_map(|e| e.cut_span).collect();

    let mut mask = sound_mask(&levels, lv.threshold_db, settings.hysteresis_db);
    // Les hésitations coupées comptent comme du silence.
    let size = mask.len();
    for &(a, b) in &event_spans {
        let first = ((a / hop - EPS).ceil() as i64).max(0) as usize;
        let last = ((b / hop + EPS).floor() as i64).max(0) as usize;
        let (first, last) = (first.min(size), last.min(size));
        if first < last {
            mask[first..last].fill(false);
        }
    }
    drop_short_sounds(&mut mask, (settings.min_sound / hop).round_ties_even() as i64);
    let silences = silence_cuts(&silence_intervals(&mask, hop, duration), settings, duration);
    let mut all_cuts = silences;
    all_cuts.extend_from_slice(&event_spans);
    let cuts = union(&all_cuts);

    // Segments gardés trop courts : absorbés par les coupes voisines.
    let mut short: Vec<Interval> =
        complement(&cuts, 0.0, duration).into_iter().filter(|k| k.1 - k.0 < settings.min_keep - EPS).collect();
    if let Some(transcript) = transcript {
        let offset = analysis.media.audio_offset;
        let centers: Vec<f64> = transcript.protected_word_centers().iter().map(|c| c + offset).collect();
        short.retain(|k| !centers.iter().any(|c| k.0 <= *c && *c < k.1));
    }

    let default_overrides = Overrides::default();
    let overrides = overrides.unwrap_or(&default_overrides);
    let force_keep = clip(&union(&overrides.force_keep), 0.0, duration);
    let force_cut = clip(&union(&overrides.force_cut), 0.0, duration);

    let mut with_short = cuts.clone();
    with_short.extend_from_slice(&short);
    let automatic = subtract(&union(&with_short), &force_keep);
    let mut all_removed = automatic;
    all_removed.extend_from_slice(&force_cut);
    let removed = union(&all_removed);
    let kept = complement(&removed, 0.0, duration);

    // Nature de chaque morceau coupé, pour l'affichage et les statistiques : les
    // hésitations d'abord, puis les silences, puis les segments trop courts.
    let mut pieces: Vec<CutPiece> =
        force_cut.iter().map(|&(start, end)| CutPiece { start, end, reason: Reason::Manual }).collect();
    let mut claimed: Vec<Interval> = force_cut.iter().chain(force_keep.iter()).copied().collect();
    let mut labelled: Vec<(Reason, Vec<Interval>)> = events
        .iter()
        .filter(|e| e.cut)
        .filter_map(|e| e.cut_span.map(|span| (Reason::of_kind(e.kind), vec![span])))
        .collect();
    labelled.push((Reason::Silence, cuts));
    labelled.push((Reason::Short, short));
    for (reason, intervals) in labelled {
        for (start, end) in subtract(&union(&intervals), &claimed) {
            if end - start > EPS {
                pieces.push(CutPiece { start, end, reason });
            }
        }
        claimed.extend_from_slice(&intervals);
        claimed = union(&claimed);
    }
    pieces.sort_by(|a, b| a.start.total_cmp(&b.start));

    CutPlan { duration, levels: lv, kept, removed: pieces, events }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Envelope;
    use crate::probe::MediaInfo;

    fn analysis(db: Vec<f32>) -> Analysis {
        let duration = db.len() as f64 * 0.01;
        let media: MediaInfo = serde_json::from_value(serde_json::json!({
            "path": "/a.wav", "duration": duration, "has_video": false,
            "audio_streams": [{"index": 0, "channels": 1, "sample_rate": 48000, "bit_depth": 16, "codec": "pcm"}],
        }))
        .unwrap();
        Analysis { fingerprint: "x".into(), media, envelope: Envelope { hop: 0.01, db, channel_db: None } }
    }

    /// 2 s de parole à -20 dB, 1 s de silence à -60 dB, 2 s de parole.
    fn speech_gap_speech() -> Vec<f32> {
        let mut db = vec![-20.0; 200];
        db.extend(vec![-60.0; 100]);
        db.extend(vec![-20.0; 200]);
        db
    }

    #[test]
    fn coupe_le_silence_en_gardant_de_l_air() {
        let plan = decide(&analysis(speech_gap_speech()), &Settings::default(), None, None);
        assert_eq!(plan.kept.len(), 2);
        assert!((plan.kept[0].1 - 2.15).abs() < 1e-6, "{:?}", plan.kept);
        assert!((plan.kept[1].0 - 2.90).abs() < 1e-6, "{:?}", plan.kept);
        assert_eq!(plan.removed.len(), 1);
        assert_eq!(plan.removed[0].reason, Reason::Silence);
        assert_eq!(plan.levels.noise_floor_db, Some(-60.0));
        assert_eq!(plan.levels.speech_db, Some(-20.0));
        assert!((plan.levels.threshold_db - -46.0).abs() < 1e-9);
    }

    #[test]
    fn un_silence_trop_court_est_garde() {
        let mut db = vec![-20.0; 200];
        db.extend(vec![-60.0; 20]);
        db.extend(vec![-20.0; 200]);
        let plan = decide(&analysis(db), &Settings::default(), None, None);
        assert_eq!(plan.kept.len(), 1);
    }

    #[test]
    fn hysteresis() {
        let levels = [-50.0, -39.0, -41.0, -44.0, -39.0];
        assert_eq!(sound_mask(&levels, -40.0, 3.0), vec![false, true, true, false, true]);
    }

    #[test]
    fn corrections_manuelles() {
        let a = analysis(speech_gap_speech());
        let keep = Overrides { force_keep: vec![(2.0, 3.0)], force_cut: vec![] };
        let plan = decide(&a, &Settings::default(), Some(&keep), None);
        assert_eq!(plan.kept, vec![(0.0, 5.0)]);
        let cut = Overrides { force_keep: vec![], force_cut: vec![(0.5, 1.0)] };
        let plan = decide(&a, &Settings::default(), Some(&cut), None);
        assert_eq!(plan.removed[0].reason, Reason::Manual);
        assert_eq!(plan.kept[0], (0.0, 0.5));
    }

    #[test]
    fn tout_silence_numerique() {
        let plan = decide(&analysis(vec![-100.0; 100]), &Settings::default(), None, None);
        assert!(plan.kept.is_empty());
        assert_eq!(plan.levels.threshold_db, -40.0);
        assert_eq!(plan.levels.noise_floor_db, None);
    }

    #[test]
    fn otsu_sans_separation() {
        assert_eq!(otsu_split(&[-30.0; 50]), None);
    }

    // --- hésitations (repris des tests de la version Python) ------------------------------

    use crate::transcript::{classify, Kind, Transcript, Word, WHISPER_HEAD_MODEL, WHISPER_MODEL};

    const SPEECH: f32 = -20.0;
    const SILENCE: f32 = -60.0;

    fn on() -> Settings {
        Settings { threshold: Threshold::Db(-40.0), hesitations: true, ..Settings::default() }
    }

    fn fixed() -> Settings {
        Settings { threshold: Threshold::Db(-40.0), ..Settings::default() }
    }

    fn make_analysis(segments: &[(f64, f32)], offset: f64, duration: Option<f64>) -> Analysis {
        let db: Vec<f32> = segments.iter().flat_map(|(d, level)| vec![*level; (d / 0.01).round() as usize]).collect();
        let mut made = analysis(db);
        made.media.audio_offset = offset;
        if let Some(duration) = duration {
            made.media.duration = duration;
        }
        made
    }

    /// (texte, début, fin, probabilité).
    fn words(items: &[(&str, f64, f64, f64)]) -> Transcript {
        with_model("small", items.iter().map(|&(t, s, e, p)| word(t, s, e, Some(p))).collect())
    }

    fn word(text: &str, start: f64, end: f64, probability: Option<f64>) -> Word {
        Word { text: text.to_string(), start, end, probability, kind: classify(text) }
    }

    fn with_model(model: &str, words: Vec<Word>) -> Transcript {
        Transcript { model: model.to_string(), revision: "abc".to_string(), language: "fr".to_string(), words, ..Transcript::default() }
    }

    #[track_caller]
    fn assert_kept(plan: &CutPlan, expected: &[Interval]) {
        assert_eq!(plan.kept.len(), expected.len(), "{:?}", plan.kept);
        for (got, want) in plan.kept.iter().zip(expected) {
            assert!((got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6, "{:?} au lieu de {:?}", plan.kept, expected);
        }
    }

    fn reasons(plan: &CutPlan) -> Vec<Reason> {
        plan.removed.iter().map(|p| p.reason).collect()
    }

    /// Mot A, 50 ms de calme, « euh » de 0,3 s, 50 ms de calme, mot B.
    fn short_hesitation() -> Analysis {
        make_analysis(&[(2.0, SPEECH), (0.05, SILENCE), (0.3, SPEECH), (0.05, SILENCE), (2.0, SPEECH)], 0.0, None)
    }

    fn alors_euh_voila(probability: f64) -> Transcript {
        words(&[("Alors", 0.0, 2.0, 0.99), ("[UH]", 2.05, 2.35, probability), ("voilà", 2.40, 4.40, 0.99)])
    }

    #[test]
    fn hesitation_entre_deux_mots_coupee_sans_les_toucher() {
        let plan = decide(&short_hesitation(), &on(), None, Some(&alors_euh_voila(0.99)));
        assert_kept(&plan, &[(0.0, 2.01), (2.39, 4.40)]);
        assert_eq!(reasons(&plan), [Reason::Hesitation]);
        assert_eq!(plan.event_count(Kind::Hesitation), 1);
        let span = plan.events[0].cut_span.unwrap();
        assert!(plan.events[0].cut && (span.0 - 2.01).abs() < 1e-6 && (span.1 - 2.39).abs() < 1e-6, "{span:?}");
    }

    #[test]
    fn rien_ne_change_sans_transcription_ou_si_desactive() {
        let a = short_hesitation();
        let base = decide(&a, &fixed(), None, None);
        let off = decide(&a, &fixed(), None, Some(&alors_euh_voila(0.99)));
        assert_kept(&base, &[(0.0, 4.40)]);
        assert_eq!(base.kept, off.kept);
        assert!(!off.events[0].cut);
        assert_eq!(off.events[0].note, "désactivé");
    }

    #[test]
    fn probabilite_faible_gardee() {
        let a = short_hesitation();
        let t = alors_euh_voila(0.3);
        let plan = decide(&a, &on(), None, Some(&t));
        assert_kept(&plan, &[(0.0, 4.40)]);
        assert_eq!(plan.events[0].note, "probabilité trop faible");
        let lower = decide(&a, &Settings { min_probability: 0.2, ..on() }, None, Some(&t));
        assert_eq!(lower.kept.len(), 2);
    }

    #[test]
    fn les_pauses_autour_d_une_hesitation_fusionnent() {
        // Pauses de 0,3 s, chacune trop courte pour être coupée seule.
        let a = make_analysis(&[(2.0, SPEECH), (0.3, SILENCE), (0.3, SPEECH), (0.3, SILENCE), (2.0, SPEECH)], 0.0, None);
        let t = words(&[("Alors", 0.0, 2.0, 0.99), ("[UH]", 2.3, 2.6, 0.99), ("voilà", 2.9, 4.9, 0.99)]);
        let plan = decide(&a, &on(), None, Some(&t));
        assert_kept(&plan, &[(0.0, 2.15), (2.8, 4.9)]);
        assert_eq!(reasons(&plan), [Reason::Silence, Reason::Hesitation, Reason::Silence]);
    }

    #[test]
    fn rires_gardes_et_toux_coupees_par_defaut() {
        let a = make_analysis(
            &[(2.0, SPEECH), (0.05, SILENCE), (0.5, SPEECH), (0.05, SILENCE), (2.0, SPEECH), (0.05, SILENCE), (0.5, SPEECH), (0.05, SILENCE), (2.0, SPEECH)],
            0.0,
            None,
        );
        let t = words(&[("un", 0.0, 2.0, 0.99), ("[laughter]", 2.05, 2.55, 0.99), ("deux", 2.6, 4.6, 0.99), ("[cough]", 4.65, 5.15, 0.99), ("trois", 5.2, 7.2, 0.99)]);
        let plan = decide(&a, &on(), None, Some(&t));
        assert_kept(&plan, &[(0.0, 4.61), (5.19, 7.2)]);
        assert_eq!((plan.event_count(Kind::Cough), plan.event_count(Kind::Laugh)), (1, 0));
        let both = decide(&a, &Settings { laughs: true, coughs: false, ..on() }, None, Some(&t));
        assert_eq!((both.event_count(Kind::Cough), both.event_count(Kind::Laugh)), (0, 1));
    }

    #[test]
    fn hesitations_consecutives_fusionnees() {
        let a = make_analysis(&[(2.0, SPEECH), (0.05, SILENCE), (0.6, SPEECH), (0.05, SILENCE), (2.0, SPEECH)], 0.0, None);
        let t = words(&[("Alors", 0.0, 2.0, 0.99), ("[UH]", 2.05, 2.30, 0.99), ("[UM]", 2.35, 2.65, 0.99), ("voilà", 2.70, 4.70, 0.99)]);
        let plan = decide(&a, &on(), None, Some(&t));
        assert_kept(&plan, &[(0.0, 2.01), (2.69, 4.70)]);
        assert_eq!(plan.event_count(Kind::Hesitation), 2);
    }

    #[test]
    fn hesitation_collee_garde_sa_marge() {
        // Whisper colle ses mots : leurs bornes ne limitent pas la coupe.
        let a = make_analysis(&[(5.0, SPEECH)], 0.0, None);
        let t = words(&[("Alors", 0.0, 2.0, 0.99), ("[UH]", 2.0, 2.3, 0.99), ("voilà", 2.3, 5.0, 0.99)]);
        assert_kept(&decide(&a, &on(), None, Some(&t)), &[(0.0, 1.95), (2.35, 5.0)]);
    }

    #[test]
    fn les_bords_se_calent_sur_le_creux() {
        // Le modèle place le « euh » un peu trop tôt ; il dure en fait de 2,13 à 2,43 s.
        let a = make_analysis(&[(2.1, SPEECH), (0.03, SILENCE), (0.3, SPEECH), (0.04, SILENCE), (2.0, SPEECH)], 0.0, None);
        let t = words(&[("Alors", 0.0, 2.04, 0.99), ("[UH]", 2.08, 2.40, 0.99), ("voilà", 2.47, 4.47, 0.99)]);
        let plan = decide(&a, &Settings { event_margin: 0.0, ..on() }, None, Some(&t));
        let span = plan.events[0].cut_span.unwrap();
        assert!((span.0 - 2.11).abs() < 1e-6 && (span.1 - 2.44).abs() < 1e-6, "{span:?}");
        assert_kept(&plan, &[(0.0, 2.11), (2.44, 4.47)]);
    }

    #[test]
    fn le_decalage_audio_deplace_les_evenements() {
        let a = make_analysis(&[(2.0, SPEECH), (0.05, SILENCE), (0.3, SPEECH), (0.05, SILENCE), (2.0, SPEECH)], 1.0, Some(5.4));
        let plan = decide(&a, &on(), None, Some(&alors_euh_voila(0.99)));
        assert!((plan.events[0].start - 3.05).abs() < 1e-6);
        assert_kept(&plan, &[(0.9, 3.01), (3.39, 5.4)]);
    }

    fn hesitation(start: f64, end: f64) -> Word {
        Word { text: "[hesitation]".to_string(), start, end, probability: None, kind: Kind::Hesitation }
    }

    fn head_settings() -> Settings {
        Settings { event_margin: 0.0, ..on() }
    }

    #[test]
    fn une_detection_entre_mots_colles_n_est_pas_retrecie() {
        let words = vec![word("alors", 0.5, 1.0, None), hesitation(1.0, 1.5), word("donc", 1.5, 2.0, None)];
        let t = with_model(WHISPER_HEAD_MODEL, words);
        let plan = decide(&make_analysis(&[(3.0, SPEECH)], 0.0, None), &head_settings(), None, Some(&t));
        let span = plan.events[0].cut_span.unwrap();
        assert!(span.0 <= 1.0 && span.1 >= 1.5, "{span:?}");
    }

    #[test]
    fn un_ilot_court_avec_un_mot_n_est_garde_qu_avec_la_tete() {
        let island = |model: &str| {
            let words = vec![hesitation(1.0, 1.5), word("de", 1.5, 1.6, Some(0.9)), hesitation(1.6, 2.1)];
            decide(&make_analysis(&[(3.0, SPEECH)], 0.0, None), &head_settings(), None, Some(&with_model(model, words)))
        };
        assert!(!reasons(&island(WHISPER_HEAD_MODEL)).contains(&Reason::Short));
        assert!(reasons(&island(WHISPER_MODEL)).contains(&Reason::Short));
    }
}
