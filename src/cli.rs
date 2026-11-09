//! Interface en ligne de commande.
//!
//! ```text
//! euhpen ui                             # interface
//! euhpen cut rush1.mov rush2.mov        # coupe les silences, écrit les XML
//! euhpen cut --hesitations rush.mov     # coupe aussi les « euh » (Whisper + détection)
//! euhpen probe rush.mov                 # métadonnées (fps, timecode, pistes…)
//! euhpen analyze dossier/               # analyse et met en cache, sans exporter
//! euhpen cfr telephone.mp4              # convertit un fichier VFR en fréquence fixe
//! euhpen cache clear                    # vide le cache d'analyse
//! ```

use crate::analysis::{analyze, Analysis};
use crate::cache::{cache_dir, clear_cache};
use crate::cfr::{convert_to_cfr, CfrRequest};
use crate::decision::{decide, CutPlan};
use crate::fcp7::{write_xmeml, Flavor, FLAVORS};
use crate::files::{expand_inputs, output_paths, sort_media, stem_of};
use crate::fps::{format_rate, frames_to_seconds, parse_rate};
use crate::presets::{get_preset, list_presets};
use crate::probe::{probe, MediaInfo};
use crate::settings::{Settings, Threshold};
use crate::timeline::{build_timeline, Timeline};
use crate::transcript::{Transcript, HEAD_ATTRIBUTION, HEAD_LICENSE, LANGUAGES, MODEL_ATTRIBUTION, WHISPER_HEAD_MODEL};
use crate::worker::{hesitations_available, run_python_console, JobRequest, TranscribeSession};
use crate::VERSION;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};

// --- affichage --------------------------------------------------------------------------

fn warn(message: &str) {
    eprintln!("⚠  {message}");
}

fn num(value: f64, digits: usize) -> String {
    format!("{value:.digits$}").replace('.', ",")
}

/// `1:02:03`, ou `2:05,3` sous l'heure.
pub fn duration_text(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let hours = (seconds / 3600.0).floor();
    let rest = seconds - hours * 3600.0;
    let minutes = (rest / 60.0).floor();
    let secs = rest - minutes * 60.0;
    if hours >= 1.0 {
        return format!("{}:{:02}:{:02}", hours as i64, minutes as i64, secs as i64);
    }
    format!("{}:{:04.1}", minutes as i64, secs).replace('.', ",")
}

struct Progress {
    label: String,
    enabled: bool,
    last: i64,
    width: usize,
}

impl Progress {
    fn new(label: &str) -> Progress {
        Progress { label: label.to_string(), enabled: std::io::stderr().is_terminal(), last: -1, width: 0 }
    }

    fn step(&mut self, fraction: f64, label: &str) {
        if !self.enabled {
            return;
        }
        let pct = if 0.0 < fraction && fraction < 1.0 { format!(" {:3} %", (fraction * 100.0) as i64) } else { String::new() };
        let line = format!("\r  {} : {label}{pct}", self.label);
        let length = line.chars().count();
        eprint!("{line}{}", " ".repeat(self.width.saturating_sub(length)));
        let _ = std::io::stderr().flush();
        self.width = self.width.max(length);
        self.last = 0;
    }

    fn clear(&self) {
        if self.enabled && self.last >= 0 {
            eprint!("\r{}\r", " ".repeat(self.width.max(self.label.chars().count() + 12)));
            let _ = std::io::stderr().flush();
        }
    }

    fn percent(&mut self, fraction: f64) {
        let pct = (fraction * 100.0) as i64;
        if self.enabled && pct != self.last {
            self.last = pct;
            eprint!("\r  {} : {pct:3} %", self.label);
            let _ = std::io::stderr().flush();
        }
    }

    fn done(&self, cached: bool, verb: &str) {
        self.clear();
        eprintln!("  {} : {}", self.label, if cached { "cache" } else { verb });
    }
}

fn run_analyses(paths: &[String], use_cache: bool) -> Result<Vec<Analysis>, String> {
    eprintln!("Analyse de {} fichier{}…", paths.len(), if paths.len() > 1 { "s" } else { "" });
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let label = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut progress = Progress::new(&label);
        let (analysis, cached) = analyze(path, use_cache, &mut |f| progress.percent(f), &|| false)?;
        progress.done(cached, "analysé");
        results.push(analysis);
    }
    Ok(results)
}

fn print_stats(tl: &Timeline) {
    let fps = tl.fps;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let (mut before_total, mut after_total, mut cuts_total, mut events_total) = (0.0, 0.0, 0, 0);
    let show_events = tl.items.iter().any(|i| i.plan.as_ref().is_some_and(|p| !p.events.is_empty()));
    let percent = |before: f64, after: f64| format!("{} %", num(if before != 0.0 { 100.0 * (1.0 - after / before) } else { 0.0 }, 1));
    for item in &tl.items {
        let before = frames_to_seconds(item.source_frames, fps);
        let after = frames_to_seconds(item.kept_frames(), fps);
        before_total += before;
        after_total += after;
        cuts_total += item.cut_count();
        let threshold = item.plan.as_ref().map_or(String::new(), |p| format!("{} dBFS", num(p.levels.threshold_db, 1)));
        let mut row = vec![item.media.name(), duration_text(before), duration_text(after), percent(before, after), item.cut_count().to_string(), threshold];
        if show_events {
            let cut_events = item.plan.as_ref().map_or(0, |p| p.events.iter().filter(|e| e.cut).count());
            events_total += cut_events;
            row.push(cut_events.to_string());
        }
        rows.push(row);
    }
    if tl.items.len() > 1 {
        let mut total = vec![
            "TOTAL".to_string(),
            duration_text(before_total),
            duration_text(after_total),
            percent(before_total, after_total),
            cuts_total.to_string(),
            String::new(),
        ];
        if show_events {
            total.push(events_total.to_string());
        }
        rows.push(total);
    }
    let mut header: Vec<String> = ["Fichier", "Avant", "Après", "Supprimé", "Coupes", "Seuil"].map(String::from).to_vec();
    if show_events {
        header.push("Euh coupés".to_string());
    }
    let length = |s: &String| s.chars().count();
    let mut widths: Vec<usize> =
        (0..header.len()).map(|i| rows.iter().chain(std::iter::once(&header)).map(|r| length(&r[i])).max().unwrap_or(0)).collect();
    widths[0] = widths[0].min(40);

    let line = |row: &[String]| {
        let name = if length(&row[0]) <= widths[0] {
            row[0].clone()
        } else {
            row[0].chars().take(widths[0] - 1).collect::<String>() + "…"
        };
        let mut cells = vec![format!("{name:<width$}", width = widths[0])];
        cells.extend(row[1..].iter().zip(&widths[1..]).map(|(c, w)| format!("{c:>w$}", w = *w)));
        cells.join("  ")
    };
    let rule = widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("  ");

    println!();
    println!("{}", line(&header));
    println!("{rule}");
    for (i, row) in rows.iter().enumerate() {
        if row[0] == "TOTAL" && i == rows.len() - 1 {
            println!("{rule}");
        }
        println!("{}", line(row));
    }
    println!();
    println!(
        "Séquence : {} i/s, {}×{}, {}",
        format_rate(Some(fps)),
        tl.width,
        tl.height,
        duration_text(frames_to_seconds(tl.duration_frames(), fps))
    );
}

fn round3(value: f64) -> f64 {
    format!("{value:.3}").parse().unwrap_or(value)
}

fn plan_to_json(tl: &Timeline, settings: &Settings) -> Value {
    let items: Vec<Value> = tl
        .items
        .iter()
        .map(|item| {
            let plan = item.plan.as_ref();
            json!({
                "path": item.media.path,
                "source_frames": item.source_frames,
                "threshold_db": plan.map(|p| p.levels.threshold_db),
                "noise_floor_db": plan.and_then(|p| p.levels.noise_floor_db),
                "speech_db": plan.and_then(|p| p.levels.speech_db),
                "segments": item.segments.iter().map(|s| json!({
                    "source_in": s.source_in, "source_out": s.source_out, "timeline_start": s.timeline_start,
                })).collect::<Vec<_>>(),
                "kept_seconds": plan.map_or(Vec::new(), |p| p.kept.iter().map(|&(a, b)| json!([round3(a), round3(b)])).collect()),
                "removed_seconds": plan.map_or(Vec::new(), |p| p.removed.iter().map(|r| json!({
                    "start": round3(r.start), "end": round3(r.end), "reason": r.reason.as_str(),
                })).collect()),
            })
        })
        .collect();
    json!({
        "euhpen": VERSION,
        "fps": format!("{}/{}", tl.fps.num, tl.fps.den),
        "settings": settings,
        "items": items,
    })
}

fn pretty_one_space<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    value.serialize(&mut serializer).map_err(|e| e.to_string())?;
    Ok(out)
}

// --- commandes --------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "euhpen",
    version,
    about = "Coupe les silences dans des rushes et exporte une timeline XML pour Premiere Pro et DaVinci Resolve. Les originaux ne sont jamais modifiés."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Coupe les silences et exporte les XML
    Cut(Box<CutArgs>),
    /// Ouvre l'interface
    Ui {
        /// Ne recharge pas la liste de fichiers précédente
        #[arg(long)]
        fresh: bool,
    },
    /// Analyse et met en cache, sans exporter
    Analyze {
        #[arg(required = true)]
        files: Vec<String>,
        #[arg(long)]
        no_cache: bool,
    },
    /// Affiche les métadonnées (fps, timecode, pistes audio)
    Probe {
        #[arg(required = true)]
        files: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Convertit un fichier à fréquence variable en fréquence fixe
    Cfr {
        file: String,
        /// Fichier de sortie (défaut : <nom>_cfr.mov)
        #[arg(short, long)]
        output: Option<String>,
        /// Fréquence cible (ex. 30000/1001, 25) ; défaut : la plus proche
        #[arg(long)]
        fps: Option<String>,
        /// Qualité H.264, plus bas = meilleur
        #[arg(long, default_value_t = 17)]
        crf: u32,
        /// Écrase la sortie si elle existe
        #[arg(long)]
        overwrite: bool,
    },
    /// Entraîne la tête de détection des hésitations (prototype, délégué à Python)
    Train {
        /// Dossier du corpus Rhapsodie (TextGrid et WAV, cherchés récursivement)
        corpus: String,
        /// Nombre maximal d'époques
        #[arg(long, default_value_t = 40)]
        epochs: u32,
        /// Dossier des poids (défaut : dossier de configuration d'Euhpen)
        #[arg(long)]
        output: Option<String>,
        /// N'ajoute pas les corrections faites dans l'interface
        #[arg(long)]
        no_corrections: bool,
    },
    /// Emplacement ou vidage du cache d'analyse
    Cache {
        #[arg(value_enum, default_value_t = CacheAction::Path)]
        action: CacheAction,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum CacheAction {
    Path,
    Clear,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Target {
    Premiere,
    Resolve,
    Both,
}

#[derive(Clone, Copy, ValueEnum)]
enum Order {
    Name,
    Date,
    Args,
}

fn parse_threshold(value: &str) -> Result<Threshold, String> {
    if value == "auto" {
        return Ok(Threshold::Auto);
    }
    value.parse::<f64>().map(Threshold::Db).map_err(|_| format!("seuil invalide : {value}"))
}

#[derive(Args)]
struct CutArgs {
    /// Fichiers vidéo/audio ou dossiers
    #[arg(required = true)]
    files: Vec<String>,
    /// Fichier .xml, base de nom ou dossier de sortie (défaut : à côté du premier fichier)
    #[arg(short, long)]
    output: Option<String>,
    /// Logiciel de montage visé
    #[arg(long, value_enum, default_value_t = Target::Both)]
    target: Target,
    /// Réglages en JSON (fichier)
    #[arg(long)]
    settings: Option<String>,
    /// Preset enregistré (« Standard », « Podcast »… ou un des vôtres)
    #[arg(long)]
    preset: Option<String>,

    /// Seuil en dBFS (ex. -40) ou « auto »
    #[arg(long, value_parser = parse_threshold, allow_hyphen_values = true, help_heading = "Détection des silences")]
    threshold: Option<Threshold>,
    /// Hystérésis en dB (défaut 3)
    #[arg(long = "hysteresis", help_heading = "Détection des silences")]
    hysteresis_db: Option<f64>,
    /// Durée minimale d'un silence à couper, s (défaut 0,4)
    #[arg(long, help_heading = "Détection des silences")]
    min_silence: Option<f64>,
    /// Son isolé plus court ignoré, s (défaut 0,05)
    #[arg(long, help_heading = "Détection des silences")]
    min_sound: Option<f64>,
    /// Air gardé avant la parole, s (défaut 0,1)
    #[arg(long, help_heading = "Détection des silences")]
    pad_before: Option<f64>,
    /// Air gardé après la parole, s (défaut 0,15)
    #[arg(long, help_heading = "Détection des silences")]
    pad_after: Option<f64>,
    /// Durée minimale d'un segment gardé, s (défaut 0,25)
    #[arg(long, help_heading = "Détection des silences")]
    min_keep: Option<f64>,

    /// Coupe aussi les « euh », « hum » et les syllabes étirées (transcription mot à mot)
    #[arg(long, overrides_with = "no_hesitations", help_heading = "Hésitations")]
    hesitations: bool,
    /// Ne coupe pas les hésitations
    #[arg(long, overrides_with = "hesitations", help_heading = "Hésitations")]
    no_hesitations: bool,
    /// Langue parlée
    #[arg(long, default_value = "fr", help_heading = "Hésitations")]
    language: String,
    /// Marge autour d'une hésitation, s (défaut 0,05)
    #[arg(long, help_heading = "Hésitations")]
    event_margin: Option<f64>,

    /// Ordre des fichiers : nom, date de tournage, ou ordre donné
    #[arg(long, value_enum, default_value_t = Order::Name, help_heading = "Timeline")]
    order: Order,
    /// Nom de la séquence
    #[arg(long, help_heading = "Timeline")]
    name: Option<String>,
    /// Fréquence de la séquence si tout est audio
    #[arg(long, default_value = "25", help_heading = "Timeline")]
    audio_fps: String,
    /// Déclare 00:00:00:00 comme timecode des sources (si le logiciel décale les clips)
    #[arg(long, help_heading = "Timeline")]
    zero_timecode: bool,
    /// Chemins bruts au lieu d'URL file:// (si le logiciel ne trouve pas les fichiers)
    #[arg(long, help_heading = "Timeline")]
    raw_paths: bool,

    /// Refait l'analyse même si elle est en cache
    #[arg(long)]
    no_cache: bool,
    /// Affiche les statistiques sans écrire de XML
    #[arg(long)]
    dry_run: bool,
    /// Écrit aussi le plan de coupe détaillé en JSON
    #[arg(long)]
    segments_json: Option<String>,
}

fn settings_from_args(args: &CutArgs) -> Result<Settings, String> {
    let mut s = if let Some(path) = &args.settings {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path} : {e}"))?;
        Settings::from_json_strict(&text)?
    } else if let Some(name) = &args.preset {
        get_preset(name).ok_or_else(|| {
            let names: Vec<String> = list_presets().iter().map(|p| format!("« {} »", p.name)).collect();
            format!("preset inconnu : {name}. Presets disponibles : {}", names.join(", "))
        })?
    } else {
        Settings::default()
    };
    if let Some(v) = args.threshold {
        s.threshold = v;
    }
    let assign = |target: &mut f64, value: Option<f64>| {
        if let Some(v) = value {
            *target = v;
        }
    };
    assign(&mut s.hysteresis_db, args.hysteresis_db);
    assign(&mut s.min_silence, args.min_silence);
    assign(&mut s.min_sound, args.min_sound);
    assign(&mut s.pad_before, args.pad_before);
    assign(&mut s.pad_after, args.pad_after);
    assign(&mut s.min_keep, args.min_keep);
    assign(&mut s.event_margin, args.event_margin);
    if args.hesitations {
        s.hesitations = true;
    }
    if args.no_hesitations {
        s.hesitations = false;
    }
    s.validate()
}

fn run_transcriptions(analyses: &[Analysis], args: &CutArgs) -> Result<HashMap<String, Transcript>, String> {
    if !LANGUAGES.iter().any(|(code, _)| *code == args.language) {
        let codes: Vec<&str> = LANGUAGES.iter().map(|(code, _)| *code).collect();
        return Err(format!("langue inconnue : {}. Langues : {}", args.language, codes.join(", ")));
    }
    hesitations_available()?;
    eprintln!("Hésitations : {MODEL_ATTRIBUTION}, langue {}.", args.language);
    eprintln!("  {HEAD_ATTRIBUTION}. {HEAD_LICENSE}");
    let mut session = TranscribeSession::default();
    let mut out = HashMap::new();
    for analysis in analyses {
        let mut progress = Progress::new(&analysis.media.name());
        let request = JobRequest { analysis, language: &args.language, model: WHISPER_HEAD_MODEL, force: args.no_cache };
        let outcome = session.run(request, &mut |f, label| progress.step(f, label), &|| false, &mut |hw| {
            if hw.device != "cuda" && hw.device != "mps" {
                progress_note(&format!("calcul sur {}", hw.name));
            }
        });
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(message) => {
                progress.clear();
                return Err(message);
            }
        };
        let verb = format!("transcrit en {} s", num(outcome.transcript.processing_time, 0));
        progress.done(outcome.cached, &verb);
        if let Some(mb) = outcome.gpu_memory_mb {
            eprintln!(
                "  calcul : {} (mémoire graphique max. {} Mio)",
                outcome.encoder_device.as_deref().unwrap_or(&outcome.transcript.device),
                mb.round() as i64
            );
        }
        out.insert(analysis.fingerprint.clone(), outcome.transcript);
    }
    Ok(out)
}

fn progress_note(message: &str) {
    eprintln!("  {message}");
}

fn cmd_cut(args: &CutArgs) -> Result<i32, String> {
    let settings = settings_from_args(args)?;
    let targets: Vec<Flavor> = match args.target {
        Target::Premiere => vec![Flavor::Premiere],
        Target::Resolve => vec![Flavor::Resolve],
        Target::Both => FLAVORS.to_vec(),
    };
    let audio_fps = parse_rate(&args.audio_fps).ok_or_else(|| format!("fréquence invalide : {}", args.audio_fps))?;

    let mut analyses = run_analyses(&expand_inputs(&args.files)?, !args.no_cache)?;
    let mode = match args.order {
        Order::Name => "name",
        Order::Date => "date",
        Order::Args => "args",
    };
    sort_media(&mut analyses, mode, |a| &a.media);
    let transcripts = if settings.wants_transcript() { run_transcriptions(&analyses, args)? } else { HashMap::new() };
    let mut entries: Vec<(MediaInfo, Vec<(f64, f64)>, Option<CutPlan>)> = Vec::new();
    for analysis in &analyses {
        let plan = decide(analysis, &settings, None, transcripts.get(&analysis.fingerprint));
        if plan.levels.low_contrast() {
            warn(&format!(
                "{} : peu d'écart entre bruit de fond et parole ({} / {} dBFS) ; vérifiez le résultat ou fixez le seuil avec --threshold.",
                analysis.media.name(),
                num(plan.levels.noise_floor_db.unwrap_or(0.0), 1),
                num(plan.levels.speech_db.unwrap_or(0.0), 1)
            ));
        }
        if plan.kept.is_empty() {
            warn(&format!("{} : aucun passage gardé (tout est sous le seuil).", analysis.media.name()));
        }
        entries.push((analysis.media.clone(), plan.kept.clone(), Some(plan)));
    }

    let first = analyses[0].media.clone();
    let stem = stem_of(&first.name());
    let name = args.name.clone().unwrap_or_else(|| if analyses.len() == 1 { stem.clone() } else { format!("{stem} (+{})", analyses.len() - 1) });
    let tl = build_timeline(entries, &name, audio_fps);
    for w in &tl.warnings {
        warn(w);
    }
    print_stats(&tl);

    if let Some(path) = &args.segments_json {
        let body = pretty_one_space(&plan_to_json(&tl, &settings))?;
        std::fs::write(path, body).map_err(|e| format!("{path} : {e}"))?;
        println!("Plan de coupe : {path}");
    }
    if args.dry_run {
        return Ok(0);
    }
    for (flavor, path) in output_paths(args.output.as_deref(), &first, &targets)? {
        for w in write_xmeml(&tl, &path, flavor, args.zero_timecode, args.raw_paths)? {
            warn(&w);
        }
        println!("{} : {path}", flavor.label());
    }
    Ok(0)
}

fn cmd_analyze(files: &[String], no_cache: bool) -> Result<i32, String> {
    let settings = Settings::default();
    let optional = |v: Option<f64>| v.map_or("—".to_string(), |x| format!("{} dBFS", num(x, 1)));
    for analysis in run_analyses(&expand_inputs(files)?, !no_cache)? {
        let lv = decide(&analysis, &settings, None, None).levels;
        println!(
            "{} : bruit de fond {}, parole {}, seuil auto {} dBFS",
            analysis.media.name(),
            optional(lv.noise_floor_db),
            optional(lv.speech_db),
            num(lv.threshold_db, 1)
        );
    }
    Ok(0)
}

fn cmd_probe(files: &[String], as_json: bool) -> Result<i32, String> {
    let infos = expand_inputs(files)?.iter().map(|p| probe(p)).collect::<Result<Vec<_>, _>>()?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&infos).map_err(|e| e.to_string())?);
        return Ok(0);
    }
    for info in infos {
        println!("{}", info.name());
        println!("  chemin     : {}", info.path);
        println!("  durée      : {} ({} s)", duration_text(info.duration), num(info.duration, 3));
        if info.has_video {
            let vfr = if info.vfr { "  ⚠ VARIABLE" } else { "" };
            println!("  vidéo      : {}×{}, {} i/s{vfr}", info.width, info.height, format_rate(info.fps));
            println!(
                "               r_frame_rate={}, avg_frame_rate={}, images={}",
                format_rate(info.r_frame_rate),
                format_rate(info.avg_frame_rate),
                info.nb_frames.filter(|n| *n != 0).map_or("?".to_string(), |n| n.to_string())
            );
        } else {
            println!("  vidéo      : aucune (audio seul)");
        }
        for s in &info.audio_streams {
            let unit = if s.channels > 1 { "canaux" } else { "canal" };
            println!("  audio #{}   : {}, {} {unit}, {} Hz, {} bits", s.index, s.codec, s.channels, s.sample_rate, s.bit_depth);
        }
        if info.audio_offset != 0.0 {
            println!("  décalage   : audio {} ms après la vidéo", num(info.audio_offset * 1000.0, 0));
        }
        println!("  timecode   : {}", info.start_timecode.as_deref().filter(|t| !t.is_empty()).unwrap_or("—"));
        if let Some(reference) = info.bwf_time_reference {
            println!("  BWF        : {reference} échantillons depuis minuit");
        }
    }
    Ok(0)
}

/// Point d'entrée de la ligne de commande. Renvoie le code de sortie.
pub fn main() -> i32 {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Cut(args) => cmd_cut(&args),
        Command::Ui { fresh } => crate::gui::run(fresh).map(|()| 0),
        Command::Analyze { files, no_cache } => cmd_analyze(&files, no_cache),
        Command::Probe { files, json } => cmd_probe(&files, json),
        Command::Cfr { file, output, fps, crf, overwrite } => (|| {
            let media = probe(&file)?;
            if !media.vfr {
                eprintln!("{} n'est pas détecté comme VFR ({} i/s) ; conversion quand même.", media.name(), format_rate(media.fps));
            }
            let request = CfrRequest { output: output.as_deref(), fps: fps.as_deref(), crf, overwrite, show_progress: true };
            let (path, rate) = convert_to_cfr(&media, request)?;
            println!("Converti à {} i/s : {path}", format_rate(Some(rate)));
            Ok(0)
        })(),
        Command::Train { corpus, epochs, output, no_corrections } => {
            let mut args = vec!["train".to_string(), corpus, "--epochs".to_string(), epochs.to_string()];
            if let Some(path) = output {
                args.extend(["--output".to_string(), path]);
            }
            if no_corrections {
                args.push("--no-corrections".to_string());
            }
            run_python_console(&args)
        }
        Command::Cache { action } => match action {
            CacheAction::Path => {
                println!("{}", cache_dir().display());
                Ok(0)
            }
            CacheAction::Clear => clear_cache().map(|root| {
                println!("Cache vidé : {}", root.display());
                0
            }),
        },
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("Erreur : {message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durees() {
        assert_eq!(duration_text(3723.4), "1:02:03");
        assert_eq!(duration_text(125.3), "2:05,3");
        assert_eq!(duration_text(0.0), "0:00,0");
        assert_eq!(duration_text(-1.0), "0:00,0");
    }

    #[test]
    fn arguments_de_decoupe() {
        let cli = Cli::try_parse_from(["euhpen", "cut", "a.mov", "--threshold", "-40", "--hesitations"]).unwrap();
        let Command::Cut(args) = cli.command else { panic!() };
        let settings = settings_from_args(&args).unwrap();
        assert_eq!(settings.threshold, Threshold::Db(-40.0));
        assert!(settings.hesitations);
    }
}
