//! Conversion d'un fichier à fréquence d'images variable (smartphone, OBS) en fréquence fixe.
//!
//! Le logiciel de montage suppose une fréquence fixe : avec un fichier VFR, l'image se
//! décale peu à peu par rapport au son et les coupes tombent à côté. On réencode donc la
//! vidéo en H.264 à fréquence fixe (images dupliquées ou retirées selon leurs horodatages,
//! le son ne bouge pas). L'original n'est pas modifié.

use crate::fps::{nearest_standard, parse_rate, Rate};
use crate::probe::{absolute, find_tool, hidden_command, MediaInfo};
use std::path::Path;
use std::process::Command;

/// Codecs audio qu'un .mov peut contenir tels quels.
const COPYABLE_AUDIO: [&str; 3] = ["aac", "alac", "mp3"];

/// Fréquence cible : celle retenue à l'analyse (la nominale si elle est standard).
///
/// On ne part pas de la moyenne réelle : un téléphone qui filme à 30 i/s et perd des
/// images en basse lumière tombe à 26 i/s de moyenne, et viser 25 jetterait des images
/// au lieu de dupliquer les manquantes.
pub fn target_rate(media: &MediaInfo) -> Result<Rate, String> {
    if let Some(fps) = media.fps {
        return Ok(fps);
    }
    media
        .r_frame_rate
        .or(media.avg_frame_rate)
        .map(|r| nearest_standard(r.as_f64()))
        .ok_or_else(|| format!("fréquence d'images inconnue pour {} ; précisez --fps.", media.name()))
}

pub fn default_output(path: &str) -> String {
    Path::new(path).with_extension("").to_string_lossy().into_owned() + "_cfr.mov"
}

fn cfr_args(media: &MediaInfo, output: &str, rate: Rate, crf: u32, overwrite: bool, stats: bool) -> Vec<String> {
    let audio_ok = media.audio_streams.iter().all(|s| COPYABLE_AUDIO.contains(&s.codec.as_str()) || s.codec.starts_with("pcm_"));
    let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-v", "error"].map(String::from).to_vec();
    if stats {
        args.push("-stats".to_string());
    }
    args.push(if overwrite { "-y" } else { "-n" }.to_string());
    args.extend(["-i".to_string(), media.path.clone()]);
    args.extend(["-map", "0:v:0", "-map", "0:a?", "-map_metadata", "0"].map(String::from));
    args.extend(["-c:v", "libx264", "-preset", "medium"].map(String::from));
    args.extend(["-crf".to_string(), crf.to_string(), "-pix_fmt".to_string(), "yuv420p".to_string()]);
    args.extend(["-fps_mode".to_string(), "cfr".to_string(), "-r".to_string(), format!("{}/{}", rate.num, rate.den)]);
    args.extend(["-c:a".to_string(), if audio_ok { "copy" } else { "pcm_s16le" }.to_string()]);
    args.push(output.to_string());
    args
}

pub struct CfrRequest<'a> {
    pub output: Option<&'a str>,
    pub fps: Option<&'a str>,
    pub crf: u32,
    pub overwrite: bool,
    /// Vrai en console : ffmpeg affiche son avancement. Faux dans l'interface.
    pub show_progress: bool,
}

pub fn convert_to_cfr(media: &MediaInfo, request: CfrRequest) -> Result<(String, Rate), String> {
    if !media.has_video {
        return Err(format!("{} n'a pas de vidéo : rien à convertir.", media.name()));
    }
    let rate = match request.fps {
        Some(text) => parse_rate(text).ok_or_else(|| format!("fréquence invalide : {text}"))?,
        None => target_rate(media)?,
    };
    let output = absolute(&request.output.map_or_else(|| default_output(&media.path), str::to_string));
    if absolute(&media.path) == output {
        return Err("le fichier de sortie doit être différent de l'original.".to_string());
    }
    let ffmpeg = find_tool("ffmpeg")?;
    let args = cfr_args(media, &output, rate, request.crf, request.overwrite, request.show_progress);
    let failure = format!("la conversion de {} a échoué", media.name());
    if request.show_progress {
        let status = Command::new(ffmpeg).args(args).status().map_err(|e| format!("{failure} : {e}"))?;
        if !status.success() {
            return Err(format!("{failure} (voir le message de ffmpeg ci-dessus)."));
        }
    } else {
        let result = hidden_command(ffmpeg).args(args).output().map_err(|e| format!("{failure} : {e}"))?;
        if !result.status.success() {
            return Err(format!("{failure} : {}", String::from_utf8_lossy(&result.stderr).trim()));
        }
    }
    Ok((output, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nom_de_sortie() {
        assert_eq!(default_output("/a/b.c.mp4"), "/a/b.c_cfr.mov");
    }
}
