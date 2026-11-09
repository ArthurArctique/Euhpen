//! Métadonnées des fichiers via ffprobe : fps exact, durée, pistes audio, timecode, VFR.

use crate::fps::{nearest_standard, normalize_rate, parse_rate, seconds_to_frame_floor, Rate, STANDARD_RATES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Commande qui n'ouvre pas de fenêtre de console sous Windows.
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let extra = homebrew_dirs().iter().map(PathBuf::from);
    std::env::split_paths(&path).chain(extra).map(|dir| dir.join(&file)).find(|full| full.is_file())
}

/// Le Finder lance les applications sans le PATH du terminal : Homebrew n'y figure pas.
fn homebrew_dirs() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin", "/usr/local/bin"]
    } else {
        &[]
    }
}

/// Copie posée par l'installateur, dans `tools/` à la racine de l'application : à côté de
/// l'exécutable, ou trois niveaux plus haut depuis l'intérieur d'un paquet `.app` sous macOS.
fn bundled(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let exe = std::env::current_exe().ok()?;
    let levels = if cfg!(target_os = "macos") { 4 } else { 1 };
    exe.ancestors().skip(1).take(levels).map(|folder| folder.join("tools").join(&file)).find(|full| full.is_file())
}

/// ffmpeg/ffprobe est-il déjà accessible par le PATH ?
pub fn tool_on_path(name: &str) -> bool {
    on_path(name).is_some()
}

/// Chemin de ffmpeg/ffprobe : variable EUHPEN_FFMPEG / EUHPEN_FFPROBE, puis la copie de
/// l'installateur, sinon le PATH.
pub fn find_tool(name: &str) -> Result<String, String> {
    let variable = format!("EUHPEN_{}", name.to_uppercase());
    if let Some(value) = std::env::var_os(&variable).filter(|v| !v.is_empty()) {
        return Ok(value.to_string_lossy().into_owned());
    }
    if let Some(found) = bundled(name) {
        return Ok(found.to_string_lossy().into_owned());
    }
    if let Some(found) = on_path(name) {
        return Ok(found.to_string_lossy().into_owned());
    }
    Err(format!(
        "{name} est introuvable. Installez ffmpeg :\n  macOS   : brew install ffmpeg\n  Windows : winget install Gyan.FFmpeg\n  Linux   : sudo apt install ffmpeg\nou indiquez son chemin dans la variable {variable}."
    ))
}

mod rate_text {
    use crate::fps::{parse_rate, Rate};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(rate: &Option<Rate>, s: S) -> Result<S::Ok, S::Error> {
        match rate {
            Some(r) => s.serialize_str(&r.text()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Rate>, D::Error> {
        Ok(Option::<String>::deserialize(d)?.and_then(|t| parse_rate(&t)))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioStream {
    /// Index absolu du flux dans le fichier (pour ffmpeg -map 0:N).
    pub index: i64,
    pub channels: i64,
    pub sample_rate: i64,
    pub bit_depth: i64,
    pub codec: String,
    #[serde(default)]
    pub start_time: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: String,
    /// Durée de la vidéo si le fichier en a une, sinon de l'audio.
    pub duration: f64,
    pub has_video: bool,
    /// Fréquence exacte retenue (normalisée).
    #[serde(default, with = "rate_text")]
    pub fps: Option<Rate>,
    #[serde(default, with = "rate_text")]
    pub r_frame_rate: Option<Rate>,
    #[serde(default, with = "rate_text")]
    pub avg_frame_rate: Option<Rate>,
    #[serde(default)]
    pub vfr: bool,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
    #[serde(default)]
    pub nb_frames: Option<i64>,
    #[serde(default)]
    pub start_timecode: Option<String>,
    #[serde(default)]
    pub audio_streams: Vec<AudioStream>,
    /// Début de l'audio − début de la vidéo, en secondes.
    #[serde(default)]
    pub audio_offset: f64,
    /// WAV pro : début en échantillons depuis minuit.
    #[serde(default)]
    pub bwf_time_reference: Option<i64>,
    #[serde(default)]
    pub creation_time: Option<String>,
    #[serde(default)]
    pub container: String,
}

impl MediaInfo {
    pub fn name(&self) -> String {
        Path::new(&self.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn audio_channels(&self) -> i64 {
        self.audio_streams.iter().map(|s| s.channels).sum()
    }

    pub fn sample_rate(&self) -> i64 {
        self.audio_streams.first().map_or(48000, |s| s.sample_rate)
    }

    /// Nombre d'images entières du fichier à la fréquence `rate`.
    ///
    /// Le nombre d'images déclaré n'est fiable qu'en fréquence fixe : un fichier VFR compte
    /// les images réellement enregistrées, pas la durée à la fréquence nominale.
    pub fn frame_count(&self, rate: Rate) -> i64 {
        let from_duration = seconds_to_frame_floor(self.duration, rate);
        if let Some(n) = self.nb_frames {
            if self.has_video && n != 0 && self.fps == Some(rate) && !self.vfr && (n - from_duration).abs() <= 2 {
                return n;
            }
        }
        from_duration
    }
}

fn run_ffprobe(path: &str) -> Result<Value, String> {
    let output = hidden_command(find_tool("ffprobe")?)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", path])
        .output()
        .map_err(|e| format!("ffprobe n'a pas pu démarrer : {e}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe ne peut pas lire {path} : {}", message.trim()));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("réponse illisible de ffprobe : {e}"))
}

fn to_f64(value: Option<&Value>) -> Option<f64> {
    let parsed = match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    (!parsed.is_nan()).then_some(parsed)
}

fn to_i64(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Valeur non nulle : `a or b or 0.0` en Python.
fn nonzero(value: Option<f64>) -> Option<f64> {
    value.filter(|v| *v != 0.0)
}

fn tag(obj: &Value, key: &str) -> Option<String> {
    let tags = obj.get("tags")?.as_object()?;
    tags.iter().find(|(k, _)| k.to_lowercase() == key).and_then(|(_, v)| v.as_str().map(str::to_string))
}

/// Durée MKV stockée en tag « DURATION=00:01:02.040000000 ».
fn tag_duration(stream: &Value) -> Option<f64> {
    let text = tag(stream, "duration")?;
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: i64 = parts[0].trim().parse().ok()?;
    let m: i64 = parts[1].trim().parse().ok()?;
    let s: f64 = parts[2].trim().parse().ok()?;
    Some(h as f64 * 3600.0 + m as f64 * 60.0 + s)
}

/// Fréquence variable si r_frame_rate et avg_frame_rate diffèrent (au-delà des arrondis).
pub fn is_vfr(r_rate: Option<Rate>, avg_rate: Option<Rate>) -> bool {
    match (r_rate, avg_rate) {
        (Some(r), Some(a)) => (r.ratio(a) - 1.0).abs() > 1e-4,
        _ => false,
    }
}

/// Fréquence retenue. En VFR, la fréquence nominale du conteneur si elle est standard (c'est
/// en général celle qu'affiche le logiciel de montage), sinon la standard la plus proche de
/// la moyenne.
fn video_rate(r_rate: Option<Rate>, avg_rate: Option<Rate>, vfr: bool) -> Option<Rate> {
    if !vfr {
        return r_rate.or(avg_rate).map(normalize_rate);
    }
    if let Some(r) = r_rate {
        let normalized = normalize_rate(r);
        if STANDARD_RATES.contains(&normalized) {
            return Some(normalized);
        }
    }
    avg_rate.map(|a| nearest_standard(a.as_f64()))
}

pub fn media_info_from_ffprobe(path: &str, data: &Value) -> MediaInfo {
    let no_streams = Vec::new();
    let streams = data.get("streams").and_then(Value::as_array).unwrap_or(&no_streams);
    let empty = Value::Object(Default::default());
    let fmt = data.get("format").filter(|v| v.is_object()).unwrap_or(&empty);
    let format_duration = to_f64(fmt.get("duration"));
    let is_type = |s: &Value, kind: &str| s.get("codec_type").and_then(Value::as_str) == Some(kind);

    // Une pochette de MP3/M4A n'est pas une vraie vidéo.
    let attached = |s: &Value| {
        s.get("disposition").and_then(|d| d.get("attached_pic")).and_then(Value::as_i64).unwrap_or(0) != 0
    };
    let video = streams.iter().find(|s| is_type(s, "video") && !attached(s));

    let mut audio_streams = Vec::new();
    for s in streams.iter().filter(|s| is_type(s, "audio")) {
        let channels = to_i64(s.get("channels")).unwrap_or(0);
        if channels <= 0 {
            continue;
        }
        let depth = to_i64(s.get("bits_per_raw_sample"))
            .filter(|d| *d != 0)
            .or_else(|| to_i64(s.get("bits_per_sample")).filter(|d| *d != 0))
            .unwrap_or(16);
        audio_streams.push(AudioStream {
            index: s.get("index").and_then(Value::as_i64).unwrap_or(0),
            channels,
            sample_rate: to_i64(s.get("sample_rate")).filter(|r| *r != 0).unwrap_or(48000),
            bit_depth: if matches!(depth, 16 | 24 | 32) { depth } else { 16 },
            codec: s.get("codec_name").and_then(Value::as_str).unwrap_or("").to_string(),
            start_time: to_f64(s.get("start_time")).unwrap_or(0.0),
        });
    }

    let mut info = MediaInfo {
        path: path.to_string(),
        duration: 0.0,
        has_video: video.is_some(),
        fps: None,
        r_frame_rate: None,
        avg_frame_rate: None,
        vfr: false,
        width: 0,
        height: 0,
        nb_frames: None,
        start_timecode: None,
        audio_streams,
        audio_offset: 0.0,
        bwf_time_reference: None,
        creation_time: tag(fmt, "creation_time"),
        container: fmt.get("format_name").and_then(Value::as_str).unwrap_or("").to_string(),
    };

    if let Some(v) = video {
        let rate = |key: &str| v.get(key).and_then(Value::as_str).and_then(parse_rate);
        info.r_frame_rate = rate("r_frame_rate");
        info.avg_frame_rate = rate("avg_frame_rate");
        info.vfr = is_vfr(info.r_frame_rate, info.avg_frame_rate);
        info.fps = video_rate(info.r_frame_rate, info.avg_frame_rate, info.vfr);
        info.width = to_i64(v.get("width")).unwrap_or(0);
        info.height = to_i64(v.get("height")).unwrap_or(0);
        info.nb_frames = to_i64(v.get("nb_frames"));
        info.duration = nonzero(to_f64(v.get("duration")))
            .or(nonzero(tag_duration(v)))
            .or(nonzero(format_duration))
            .unwrap_or(0.0);
        if let Some(first) = info.audio_streams.first() {
            let video_start = to_f64(v.get("start_time")).unwrap_or(0.0);
            info.audio_offset = first.start_time - video_start;
        }
    } else {
        let first_audio = streams.iter().find(|s| is_type(s, "audio"));
        info.duration = first_audio
            .and_then(|a| nonzero(to_f64(a.get("duration"))))
            .or_else(|| first_audio.and_then(|a| nonzero(tag_duration(a))))
            .or(nonzero(format_duration))
            .unwrap_or(0.0);
    }

    // Timecode de départ : flux vidéo, puis piste tmcd, puis conteneur.
    let candidates = video.into_iter().chain(streams.iter()).chain(std::iter::once(fmt));
    info.start_timecode = candidates.filter_map(|obj| tag(obj, "timecode")).find(|tc| !tc.is_empty());

    info.bwf_time_reference = tag(fmt, "time_reference").and_then(|t| t.trim().parse::<i64>().ok()).filter(|t| *t != 0);
    info
}

/// Chemin absolu sans préfixe `\\?\`.
pub fn absolute(path: &str) -> String {
    std::path::absolute(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string())
}

/// Lit les métadonnées d'un fichier (chemin rendu absolu).
pub fn probe(path: &str) -> Result<MediaInfo, String> {
    let path = absolute(path);
    if !Path::new(&path).is_file() {
        return Err(format!("fichier introuvable : {path}"));
    }
    let info = media_info_from_ffprobe(&path, &run_ffprobe(&path)?);
    if info.audio_streams.is_empty() {
        return Err(format!("{} ne contient aucune piste audio.", info.name()));
    }
    if info.duration <= 0.0 {
        return Err(format!("durée inconnue pour {}.", info.name()));
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn video_29_97_avec_timecode_et_decalage() {
        let data = json!({
            "format": {"duration": "12.5", "format_name": "mov,mp4", "tags": {"creation_time": "2024-05-01T10:00:00Z"}},
            "streams": [
                {"index": 0, "codec_type": "video", "r_frame_rate": "30000/1001", "avg_frame_rate": "30000/1001",
                 "width": 1920, "height": 1080, "nb_frames": "374", "duration": "12.479", "start_time": "0.000000",
                 "tags": {"timecode": "01:00:00;00"}},
                {"index": 1, "codec_type": "audio", "channels": 2, "sample_rate": "48000", "codec_name": "aac",
                 "start_time": "0.021000", "duration": "12.5"}
            ]
        });
        let info = media_info_from_ffprobe("/a.mov", &data);
        assert_eq!(info.fps, Some(Rate::new(30000, 1001)));
        assert!(!info.vfr);
        assert_eq!(info.duration, 12.479);
        assert_eq!(info.nb_frames, Some(374));
        assert_eq!(info.start_timecode.as_deref(), Some("01:00:00;00"));
        assert!((info.audio_offset - 0.021).abs() < 1e-9);
        assert_eq!(info.audio_channels(), 2);
        assert_eq!(info.audio_streams[0].bit_depth, 16);
    }

    #[test]
    fn la_pochette_n_est_pas_une_video() {
        let data = json!({
            "format": {"duration": "3.0"},
            "streams": [
                {"index": 0, "codec_type": "video", "disposition": {"attached_pic": 1}, "r_frame_rate": "90000/1"},
                {"index": 1, "codec_type": "audio", "channels": 1, "sample_rate": "44100", "duration": "2.5"}
            ]
        });
        let info = media_info_from_ffprobe("/a.mp3", &data);
        assert!(!info.has_video);
        assert_eq!(info.duration, 2.5);
    }

    #[test]
    fn frequence_variable_vers_la_nominale_standard() {
        let data = json!({
            "format": {},
            "streams": [{"index": 0, "codec_type": "video", "r_frame_rate": "30/1", "avg_frame_rate": "26/1"},
                        {"index": 1, "codec_type": "audio", "channels": 1, "sample_rate": "48000"}]
        });
        let info = media_info_from_ffprobe("/a.mp4", &data);
        assert!(info.vfr);
        assert_eq!(info.fps, Some(Rate::whole(30)));
    }

    #[test]
    fn duree_mkv_en_tag() {
        let stream = json!({"tags": {"DURATION": "00:01:02.040000000"}});
        assert_eq!(tag_duration(&stream), Some(62.04));
    }

    #[test]
    fn le_format_du_cache_est_conserve() {
        let info = media_info_from_ffprobe("/a.mp4", &json!({"format": {"duration": "1"}, "streams": []}));
        let value = serde_json::to_value(&info).unwrap();
        assert!(value["fps"].is_null());
        let text = r#"{"path":"/x","duration":2.0,"has_video":true,"fps":"30000/1001","r_frame_rate":"30000/1001",
            "avg_frame_rate":"30000/1001","vfr":false,"width":1,"height":1,"nb_frames":null,"start_timecode":null,
            "audio_streams":[{"index":1,"channels":2,"sample_rate":48000,"bit_depth":16,"codec":"aac","start_time":0.0}],
            "audio_offset":0.0,"bwf_time_reference":null,"creation_time":null,"container":"mov"}"#;
        let back: MediaInfo = serde_json::from_str(text).unwrap();
        assert_eq!(back.fps, Some(Rate::new(30000, 1001)));
        assert_eq!(serde_json::to_value(&back).unwrap()["fps"], "30000/1001");
    }
}
