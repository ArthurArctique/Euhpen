//! Fichier d'aperçu léger (proxy) pour la lecture dans l'interface.
//!
//! Vidéo : H.264 + AAC en 540p (ou 720p), une image clé toutes les demi-secondes pour que
//! les sauts par-dessus les coupes soient instantanés. Audio seul : AAC dans un .m4a.
//! L'audio est un mélange mono des canaux utiles, recalé sur le temps du média (0 = début
//! de la vidéo) : l'aperçu et les coupes partagent exactement la même horloge.
//!
//! Le lecteur natif lit aussi une version PCM de cet audio, dérivée du proxy.
//! Tout est mis en cache à côté de l'analyse ; l'original n'est jamais modifié.

use crate::analysis::Analysis;
use crate::audio::{collect_stderr, mono_filter};
use crate::cache::analysis_dir;
use crate::fps::Rate;
use crate::probe::{find_tool, hidden_command};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub const PROXY_VERSION: u32 = 1;
pub const PROXY_HEIGHTS: [u32; 2] = [540, 720];
pub const AUDIO_RATE: u32 = 48_000;
const PREVIEW_AUDIO_VERSION: u32 = 1;

pub fn proxy_path(analysis: &Analysis, height: u32) -> PathBuf {
    let folder = analysis_dir(&analysis.fingerprint);
    if analysis.media.has_video {
        folder.join(format!("proxy-{height}p-v{PROXY_VERSION}.mp4"))
    } else {
        folder.join(format!("proxy-audio-v{PROXY_VERSION}.m4a"))
    }
}

fn non_empty(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

pub fn existing_proxy(analysis: &Analysis, height: u32) -> Option<PathBuf> {
    let path = proxy_path(analysis, height);
    non_empty(&path).then_some(path)
}

/// Au plus 30 images/s : un aperçu en 50 ou 60 i/s ne sert à rien et pèse deux fois plus.
pub fn proxy_rate(fps: Option<Rate>) -> Rate {
    match fps {
        None => Rate::whole(25),
        Some(fps) if fps.as_f64() > 31.0 => fps.halved(),
        Some(fps) => fps,
    }
}

fn proxy_args(analysis: &Analysis, output: &str, height: u32) -> Vec<String> {
    let media = &analysis.media;
    let mut args: Vec<String> =
        ["-nostdin", "-hide_banner", "-v", "error", "-y", "-i", &media.path].map(String::from).to_vec();
    let mut graph_parts = Vec::new();
    let mut maps: Vec<String> = Vec::new();

    if !media.audio_streams.is_empty() {
        let (graph, label) = mono_filter(media, analysis.envelope.channel_db.as_deref(), AUDIO_RATE);
        // Recalage sur le temps du média : l'audio qui commence après la vidéo est précédé
        // de silence, celui qui commence avant est raccourci.
        let mut align = "asetpts=PTS-STARTPTS".to_string();
        let offset = if media.has_video { media.audio_offset } else { 0.0 };
        if offset > 0.0005 {
            align += &format!(",adelay=delays={}:all=1", (offset * 1000.0).round_ties_even() as i64);
        } else if offset < -0.0005 {
            align += &format!(",atrim=start={:.6},asetpts=PTS-STARTPTS", -offset);
        }
        graph_parts.push(format!("{graph};{label}{align}[aout]"));
        maps.extend(["-map".to_string(), "[aout]".to_string()]);
    }

    let mut codec: Vec<String> = Vec::new();
    if media.has_video {
        let rate = proxy_rate(media.fps);
        let gop = ((rate.as_f64() / 2.0).round_ties_even() as i64).max(1).to_string();
        graph_parts.push(format!(
            "[0:v:0]setpts=PTS-STARTPTS,fps={}/{},scale=-2:'min({height},ih)':flags=bilinear,format=yuv420p[vout]",
            rate.num, rate.den
        ));
        let mut video_map = vec!["-map".to_string(), "[vout]".to_string()];
        video_map.append(&mut maps);
        maps = video_map;
        codec.extend(
            [
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "27", "-g", &gop, "-keyint_min", &gop, "-sc_threshold",
                "0", "-bf", "0", "-profile:v", "high", "-movflags", "+faststart",
            ]
            .map(String::from),
        );
    } else {
        codec.extend(["-movflags", "+faststart"].map(String::from));
    }
    if !media.audio_streams.is_empty() {
        codec.extend(["-c:a", "aac", "-b:a", "128k", "-ac", "1", "-ar", "48000"].map(String::from));
    }
    // La durée du proxy est celle du média (une piste audio plus longue est coupée).
    args.extend(["-filter_complex".to_string(), graph_parts.join(";")]);
    args.append(&mut maps);
    args.append(&mut codec);
    args.extend(["-t".to_string(), format!("{:.6}", media.duration)]);
    args.extend(["-sn", "-dn", "-map_metadata", "-1", "-progress", "pipe:1", "-nostats", output].map(String::from));
    args
}

/// Fichier temporaire unique dans le même dossier (renommé une fois complet).
fn temporary_beside(output: &Path) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let extension = output.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    output.with_file_name(format!("tmp-{}-{nanos}.{extension}", std::process::id()))
}

/// Crée le proxy s'il n'est pas déjà en cache et renvoie son chemin.
pub fn make_proxy(
    analysis: &Analysis,
    height: u32,
    progress: &mut dyn FnMut(f64),
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    if let Some(done) = existing_proxy(analysis, height) {
        return Ok(done);
    }
    let output = proxy_path(analysis, height);
    let folder = output.parent().ok_or("dossier de cache invalide")?;
    fs::create_dir_all(folder).map_err(|e| format!("{} : {e}", folder.display()))?;
    let tmp = temporary_beside(&output);
    let result = run_proxy(analysis, height, &tmp, progress, cancelled);
    let result = result.and_then(|()| {
        fs::rename(&tmp, &output).map_err(|e| format!("{} : {e}", output.display()))
    });
    let _ = fs::remove_file(&tmp);
    result?;
    progress(1.0);
    Ok(output)
}

fn run_proxy(
    analysis: &Analysis,
    height: u32,
    tmp: &Path,
    progress: &mut dyn FnMut(f64),
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let duration = analysis.media.duration.max(0.001);
    let tmp_text = tmp.to_string_lossy().into_owned();
    let mut child = hidden_command(find_tool("ffmpeg")?)
        .args(proxy_args(analysis, &tmp_text, height))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg n'a pas pu démarrer : {e}"))?;
    let errors = collect_stderr(child.stderr.take());
    let stdout = child.stdout.take().ok_or("sortie de ffmpeg indisponible")?;
    let mut was_cancelled = false;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if cancelled() {
            was_cancelled = true;
            let _ = child.kill();
            break;
        }
        if let Some(value) = line.trim().strip_prefix("out_time_us=") {
            if let Ok(micros) = value.parse::<u64>() {
                progress((micros as f64 / 1e6 / duration).min(1.0));
            }
        }
    }
    let status = child.wait().map_err(|e| format!("ffmpeg : {e}"))?;
    let message = errors.join().unwrap_or_default();
    if was_cancelled || cancelled() {
        return Err("création de l'aperçu annulée".to_string());
    }
    if !status.success() {
        return Err(format!("ffmpeg n'a pas pu créer l'aperçu de {} : {message}", analysis.media.name()));
    }
    Ok(())
}

pub fn preview_audio_path(analysis: &Analysis) -> PathBuf {
    analysis_dir(&analysis.fingerprint).join(format!("preview-audio-v{PREVIEW_AUDIO_VERSION}.pcm"))
}

/// Son de l'aperçu pour le lecteur natif : PCM 16 bits mono à 48 kHz, dérivé du proxy.
pub fn make_preview_audio(analysis: &Analysis, proxy: &Path) -> Result<PathBuf, String> {
    let output = preview_audio_path(analysis);
    if non_empty(&output) {
        return Ok(output);
    }
    let tmp = temporary_beside(&output);
    let result = hidden_command(find_tool("ffmpeg")?)
        .args(["-nostdin", "-hide_banner", "-v", "error", "-y", "-i"])
        .arg(proxy)
        .args(["-vn", "-sn", "-dn", "-ac", "1", "-ar", "48000", "-f", "s16le", "-acodec", "pcm_s16le"])
        .arg(&tmp)
        .output()
        .map_err(|e| format!("ffmpeg n'a pas pu démarrer : {e}"))?;
    if !result.status.success() {
        let _ = fs::remove_file(&tmp);
        let message = String::from_utf8_lossy(&result.stderr);
        return Err(format!("ffmpeg n'a pas pu extraire le son de l'aperçu : {}", message.trim()));
    }
    fs::rename(&tmp, &output).map_err(|e| format!("{} : {e}", output.display()))?;
    Ok(output)
}
