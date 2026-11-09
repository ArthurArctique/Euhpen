//! Extraction de l'enveloppe de volume (RMS en dBFS par fenêtres de 10 ms) avec ffmpeg.
//!
//! L'audio est décodé par ffmpeg en flottants 32 bits à 16 kHz et lu en flux (rien n'est écrit
//! sur le disque). Si le fichier a plusieurs canaux ou plusieurs pistes, on calcule le RMS de
//! chaque canal et on garde le plus fort : une caméra 4 canaux dont seul le canal 1 porte le
//! micro n'est ainsi pas atténuée par les canaux vides.
//!
//! Le niveau de parole de chaque canal est aussi mesuré : la transcription et l'aperçu
//! n'écoutent que les canaux utiles (à 6 dB près du plus fort), pas les canaux vides.

use crate::probe::{find_tool, hidden_command, MediaInfo};
use std::io::Read;
use std::path::Path;
use std::process::{ChildStderr, Stdio};
use std::thread::JoinHandle;

pub const SAMPLE_RATE: u32 = 16_000;
pub const HOP_SECONDS: f64 = 0.01;
pub const WINDOW: usize = 160;
pub const FLOOR_DB: f64 = -100.0;
/// Un canal dont la parole est plus faible que le plus fort de cet écart est ignoré pour la
/// transcription et l'aperçu (canal vide, micro de la caméra…).
pub const USEFUL_CHANNEL_DB: f64 = 6.0;

/// Lit en arrière-plan la sortie d'erreur d'un processus, pour ne pas le bloquer.
pub fn collect_stderr(stderr: Option<ChildStderr>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut stream) = stderr {
            let _ = stream.read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).trim().to_string()
    })
}

/// Graphe ffmpeg qui réunit toutes les pistes audio en un flux de N canaux.
/// Renvoie (graphe, étiquette de sortie).
fn merge_graph(media: &MediaInfo, sample_rate: u32) -> (String, String) {
    let streams = &media.audio_streams;
    // Les pistes de caméra ou d'OBS n'ont souvent pas de disposition de canaux connue, et
    // amerge refuse alors de les assembler : on impose « N canaux » sans ordre.
    let parts: Vec<String> = streams
        .iter()
        .enumerate()
        .map(|(k, s)| {
            format!("[0:{}]aresample={sample_rate},aformat=sample_fmts=flt:channel_layouts={}c[a{k}]", s.index, s.channels)
        })
        .collect();
    if streams.len() == 1 {
        return (parts[0].clone(), "[a0]".to_string());
    }
    let inputs: String = (0..streams.len()).map(|k| format!("[a{k}]")).collect();
    (format!("{};{inputs}amerge=inputs={}[mix]", parts.join(";"), streams.len()), "[mix]".to_string())
}

/// Commande ffmpeg qui écrit sur stdout tous les canaux de toutes les pistes audio.
fn ffmpeg_audio_args(media: &MediaInfo) -> Vec<String> {
    let mut args: Vec<String> =
        ["-nostdin", "-hide_banner", "-v", "error", "-i", &media.path].map(String::from).to_vec();
    if media.audio_streams.len() == 1 {
        args.extend(["-map".to_string(), format!("0:{}", media.audio_streams[0].index)]);
        args.extend(["-ar".to_string(), SAMPLE_RATE.to_string()]);
    } else {
        let (graph, label) = merge_graph(media, SAMPLE_RATE);
        args.extend(["-filter_complex".to_string(), graph, "-map".to_string(), label]);
    }
    args.extend(["-vn", "-sn", "-dn", "-f", "f32le", "-acodec", "pcm_f32le", "-"].map(String::from));
    args
}

/// Canaux à écouter : ceux dont la parole est à 6 dB près du canal le plus fort.
pub fn useful_channels(channel_db: Option<&[f64]>, channels: usize) -> Vec<usize> {
    match channel_db {
        Some(levels) if !levels.is_empty() && levels.len() == channels => {
            let best = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (0..channels).filter(|&k| levels[k] >= best - USEFUL_CHANNEL_DB).collect()
        }
        _ => (0..channels).collect(),
    }
}

/// Six chiffres significatifs sans zéros de fin, comme `%.6g` pour un gain dans ]0, 1].
fn six_digits(value: f64) -> String {
    let decimals = (5 - value.log10().floor() as i32).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// Graphe ffmpeg qui produit un flux mono : moyenne des canaux utiles.
pub fn mono_filter(media: &MediaInfo, channel_db: Option<&[f64]>, sample_rate: u32) -> (String, String) {
    let (graph, label) = merge_graph(media, sample_rate);
    let keep = useful_channels(channel_db, media.audio_channels() as usize);
    let gain = six_digits(1.0 / keep.len() as f64);
    let expr: Vec<String> = keep.iter().map(|k| format!("{gain}*c{k}")).collect();
    (format!("{graph};{label}pan=mono|c0={}[mono]", expr.join("+")), "[mono]".to_string())
}

/// Audio mono à 16 kHz en float32 (pour la transcription), écrit dans un fichier.
pub fn decode_mono_to_file(media: &MediaInfo, channel_db: Option<&[f64]>, output: &Path) -> Result<(), String> {
    let (graph, label) = mono_filter(media, channel_db, SAMPLE_RATE);
    let result = hidden_command(find_tool("ffmpeg")?)
        .args(["-nostdin", "-hide_banner", "-v", "error", "-y", "-i", &media.path])
        .args(["-filter_complex", &graph, "-map", &label])
        .args(["-vn", "-sn", "-dn", "-f", "f32le", "-acodec", "pcm_f32le"])
        .arg(output)
        .output()
        .map_err(|e| format!("ffmpeg n'a pas pu démarrer : {e}"))?;
    if !result.status.success() {
        let message = String::from_utf8_lossy(&result.stderr);
        return Err(format!("ffmpeg n'a pas pu décoder l'audio de {} : {}", media.name(), message.trim()));
    }
    Ok(())
}

pub struct EnvelopeResult {
    /// dBFS par fenêtre de 10 ms, maximum sur les canaux.
    pub db: Vec<f32>,
    /// Niveau de parole de chaque canal (90e centile), dBFS.
    pub channel_db: Vec<f64>,
}

fn to_db(power: f64) -> f64 {
    let db = 10.0 * power.log10();
    if db.is_nan() {
        return FLOOR_DB;
    }
    db.max(FLOOR_DB)
}

/// 90e centile avec interpolation linéaire, comme numpy.percentile.
fn percentile_90(values: &mut [f64]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    let virtual_index = (values.len() - 1) as f64 * 0.9;
    let previous = virtual_index.floor();
    let gamma = virtual_index - previous;
    let a = values[previous as usize];
    let b = values[(previous as usize + 1).min(values.len() - 1)];
    let difference = b - a;
    if gamma >= 0.5 {
        b - difference * (1.0 - gamma)
    } else {
        a + difference * gamma
    }
}

/// Arrondi au dixième à la manière de `round(x, 1)` en Python.
pub fn round_tenth(value: f64) -> f64 {
    format!("{value:.1}").parse().unwrap_or(value)
}

/// Accumule les fenêtres de 10 ms d'un flux entrelacé en flottants.
struct Windows {
    channels: usize,
    per_channel: Vec<Vec<f64>>,
}

impl Windows {
    fn new(channels: usize) -> Self {
        Windows { channels, per_channel: vec![Vec::new(); channels] }
    }

    fn push(&mut self, block: &[u8]) {
        let frame = 4 * self.channels;
        for k in 0..self.channels {
            let mut power = 0.0_f64;
            for n in 0..WINDOW {
                let at = n * frame + k * 4;
                let sample = f32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]]) as f64;
                power += sample * sample;
            }
            self.per_channel[k].push(to_db(power / WINDOW as f64));
        }
    }

    fn windows(&self) -> usize {
        self.per_channel.first().map_or(0, Vec::len)
    }
}

/// Enveloppe RMS en dBFS, une valeur toutes les 10 ms.
pub fn compute_envelope(
    media: &MediaInfo,
    progress: &mut dyn FnMut(f64),
    cancelled: &dyn Fn() -> bool,
) -> Result<EnvelopeResult, String> {
    let channels = media.audio_channels() as usize;
    let bytes_per_window = WINDOW * channels * 4;
    let mut child = hidden_command(find_tool("ffmpeg")?)
        .args(ffmpeg_audio_args(media))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg n'a pas pu démarrer : {e}"))?;
    let errors = collect_stderr(child.stderr.take());
    let mut stdout = child.stdout.take().ok_or("sortie de ffmpeg indisponible")?;

    let mut windows = Windows::new(channels);
    let mut pending: Vec<u8> = Vec::with_capacity(bytes_per_window * 1000);
    let mut chunk = vec![0_u8; bytes_per_window * 500]; // 5 s d'audio par lecture
    loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("analyse annulée".to_string());
        }
        let read = stdout.read(&mut chunk).map_err(|e| format!("lecture de ffmpeg : {e}"))?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..read]);
        let usable = pending.len() - pending.len() % bytes_per_window;
        if usable > 0 {
            for block in pending[..usable].chunks_exact(bytes_per_window) {
                windows.push(block);
            }
            pending.drain(..usable);
            if media.duration > 0.0 {
                progress((windows.windows() as f64 * HOP_SECONDS / media.duration).min(1.0));
            }
        }
    }
    drop(stdout);
    let status = child.wait().map_err(|e| format!("ffmpeg : {e}"))?;
    let message = errors.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("ffmpeg n'a pas pu décoder l'audio de {} : {message}", media.name()));
    }

    // Dernière fenêtre incomplète : complétée par du silence.
    let frame_bytes = channels * 4;
    let tail = pending.len() - pending.len() % frame_bytes;
    if tail > 0 {
        let mut padded = vec![0_u8; bytes_per_window];
        padded[..tail].copy_from_slice(&pending[..tail]);
        windows.push(&padded);
    }

    if windows.windows() == 0 {
        return Err(format!("aucun échantillon audio décodé dans {}.", media.name()));
    }
    let count = windows.windows();
    let mut db = Vec::with_capacity(count);
    for i in 0..count {
        let loudest = windows.per_channel.iter().map(|c| c[i]).fold(f64::NEG_INFINITY, f64::max);
        db.push(loudest as f32);
    }
    let channel_db = windows.per_channel.iter_mut().map(|c| round_tenth(percentile_90(c))).collect();
    Ok(EnvelopeResult { db, channel_db })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(index: i64, channels: i64) -> crate::probe::AudioStream {
        crate::probe::AudioStream { index, channels, sample_rate: 48000, bit_depth: 16, codec: "aac".into(), start_time: 0.0 }
    }

    fn media(streams: Vec<crate::probe::AudioStream>) -> MediaInfo {
        serde_json::from_value(serde_json::json!({
            "path": "/a.mov", "duration": 1.0, "has_video": false, "audio_streams": streams,
        }))
        .unwrap()
    }

    #[test]
    fn canaux_utiles() {
        assert_eq!(useful_channels(Some(&[-20.0, -60.0, -25.0]), 3), vec![0, 2]);
        assert_eq!(useful_channels(None, 2), vec![0, 1]);
        assert_eq!(useful_channels(Some(&[-20.0]), 2), vec![0, 1]);
    }

    #[test]
    fn gains_comme_printf_g() {
        assert_eq!(six_digits(1.0), "1");
        assert_eq!(six_digits(0.5), "0.5");
        assert_eq!(six_digits(1.0 / 3.0), "0.333333");
        assert_eq!(six_digits(1.0 / 11.0), "0.0909091");
    }

    #[test]
    fn graphe_mono() {
        let m = media(vec![stream(1, 2)]);
        let (graph, label) = mono_filter(&m, Some(&[-20.0, -60.0]), 16000);
        assert_eq!(graph, "[0:1]aresample=16000,aformat=sample_fmts=flt:channel_layouts=2c[a0];[a0]pan=mono|c0=1*c0[mono]");
        assert_eq!(label, "[mono]");
        let m = media(vec![stream(1, 1), stream(2, 1)]);
        let (graph, _) = mono_filter(&m, None, 48000);
        assert!(graph.ends_with(";[a0][a1]amerge=inputs=2[mix];[mix]pan=mono|c0=0.5*c0+0.5*c1[mono]"), "{graph}");
    }

    #[test]
    fn centile() {
        let mut v = vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert!((percentile_90(&mut v) - 9.0).abs() < 1e-9);
        let mut v = vec![0.0, 10.0];
        assert!((percentile_90(&mut v) - 9.0).abs() < 1e-9);
    }

    #[test]
    fn fenetres() {
        let mut w = Windows::new(2);
        let mut block = Vec::new();
        for _ in 0..WINDOW {
            block.extend_from_slice(&1.0_f32.to_le_bytes());
            block.extend_from_slice(&0.0_f32.to_le_bytes());
        }
        w.push(&block);
        assert_eq!(w.per_channel[0], vec![0.0]);
        assert_eq!(w.per_channel[1], vec![FLOOR_DB]);
    }
}
