//! Calage des coupes sur les images et assemblage de la timeline (un ou plusieurs fichiers).

use crate::decision::CutPlan;
use crate::fps::{format_rate, is_xmeml_exact, rate_from_xmeml, seconds_to_frame_ceil, seconds_to_frame_floor, xmeml_rate, Rate};
use crate::intervals::Interval;
use crate::probe::MediaInfo;

pub const DEFAULT_AUDIO_ONLY_FPS: Rate = Rate::whole(25);

/// Un morceau gardé. Toutes les valeurs sont des images à la fréquence de la séquence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Première image gardée dans la source.
    pub source_in: i64,
    /// Image qui suit la dernière gardée (exclue).
    pub source_out: i64,
    pub timeline_start: i64,
}

impl Segment {
    pub fn duration(&self) -> i64 {
        self.source_out - self.source_in
    }

    pub fn timeline_end(&self) -> i64 {
        self.timeline_start + self.duration()
    }
}

#[derive(Clone, Debug)]
pub struct TimelineItem {
    pub media: MediaInfo,
    pub segments: Vec<Segment>,
    /// Longueur du fichier, en images de la séquence.
    pub source_frames: i64,
    pub plan: Option<CutPlan>,
}

impl TimelineItem {
    pub fn kept_frames(&self) -> i64 {
        self.segments.iter().map(Segment::duration).sum()
    }

    /// Nombre de passages supprimés (début et fin de fichier compris).
    pub fn cut_count(&self) -> i64 {
        let (Some(first), Some(last)) = (self.segments.first(), self.segments.last()) else {
            return i64::from(self.source_frames != 0);
        };
        let gaps = self.segments.windows(2).filter(|p| p[1].source_in > p[0].source_out).count() as i64;
        gaps + i64::from(first.source_in > 0) + i64::from(last.source_out < self.source_frames)
    }
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub name: String,
    pub fps: Rate,
    pub width: i64,
    pub height: i64,
    pub sample_rate: i64,
    pub items: Vec<TimelineItem>,
    pub warnings: Vec<String>,
}

impl Timeline {
    pub fn duration_frames(&self) -> i64 {
        self.items.iter().flat_map(|i| i.segments.iter()).map(Segment::timeline_end).max().unwrap_or(0)
    }

    pub fn has_video(&self) -> bool {
        self.items.iter().any(|i| i.media.has_video)
    }
}

/// Cale les segments gardés sur les images, toujours vers l'extérieur.
///
/// Le début est arrondi à l'image inférieure et la fin à l'image supérieure : un segment
/// ne peut que s'allonger (jamais tomber sous la durée minimale, jamais rogner un mot).
/// Deux segments qui se touchent après calage sont fusionnés.
pub fn snap_to_frames(kept: &[Interval], rate: Rate, total_frames: i64) -> Vec<(i64, i64)> {
    let mut result: Vec<(i64, i64)> = Vec::with_capacity(kept.len());
    for &(start, end) in kept {
        let a = seconds_to_frame_floor(start, rate).max(0);
        let mut b = seconds_to_frame_ceil(end, rate).min(total_frames);
        if b <= a {
            if a >= total_frames {
                continue;
            }
            b = a + 1;
        }
        match result.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => result.push((a, b)),
        }
    }
    result
}

/// Réglages de séquence pris sur le premier fichier vidéo (fps, taille, fréquence audio).
fn sequence_format(medias: &[&MediaInfo], audio_only_fps: Rate) -> (Rate, i64, i64, i64, Vec<String>) {
    let mut warnings = Vec::new();
    let videos: Vec<&&MediaInfo> = medias.iter().filter(|m| m.has_video && m.fps.is_some()).collect();
    let (mut fps, width, height) = match videos.first() {
        Some(first) => {
            let fps = first.fps.unwrap();
            for other in &videos[1..] {
                if other.fps != Some(fps) {
                    warnings.push(format!(
                        "{} est à {} i/s, la séquence à {} i/s (réglages du premier fichier) : le logiciel de montage convertira ce clip.",
                        other.name(),
                        format_rate(other.fps),
                        format_rate(Some(fps))
                    ));
                }
            }
            let or = |value: i64, default: i64| if value != 0 { value } else { default };
            (fps, or(first.width, 1920), or(first.height, 1080))
        }
        None => (audio_only_fps, 1920, 1080),
    };
    if !is_xmeml_exact(fps) {
        let (timebase, ntsc) = xmeml_rate(fps);
        let rounded = rate_from_xmeml(timebase, ntsc);
        warnings.push(format!(
            "{} i/s n'existe pas dans le format XML : séquence à {} i/s.",
            format_rate(Some(fps)),
            format_rate(Some(rounded))
        ));
        fps = rounded;
    }
    let sample_rate = medias.first().map_or(48000, |m| m.sample_rate());
    (fps, width, height, sample_rate, warnings)
}

/// Met les fichiers bout à bout, chacun réduit à ses segments gardés.
pub fn build_timeline(entries: Vec<(MediaInfo, Vec<Interval>, Option<CutPlan>)>, name: &str, audio_only_fps: Rate) -> Timeline {
    let medias: Vec<&MediaInfo> = entries.iter().map(|(m, _, _)| m).collect();
    let (fps, width, height, sample_rate, mut warnings) = sequence_format(&medias, audio_only_fps);
    for media in &medias {
        if media.vfr {
            warnings.push(format!(
                "{} a une fréquence d'images variable (r={}, moyenne={}) : les coupes risquent d'être décalées dans le logiciel de montage. Convertissez-le d'abord : euhpen cfr \"{}\"",
                media.name(),
                format_rate(media.r_frame_rate),
                format_rate(media.avg_frame_rate),
                media.path
            ));
        }
    }

    let mut items = Vec::with_capacity(entries.len());
    let mut cursor = 0;
    for (media, kept, plan) in entries {
        let source_frames = media.frame_count(fps);
        let mut segments = Vec::new();
        for (a, b) in snap_to_frames(&kept, fps, source_frames) {
            segments.push(Segment { source_in: a, source_out: b, timeline_start: cursor });
            cursor += b - a;
        }
        items.push(TimelineItem { media, segments, source_frames, plan });
    }
    Timeline { name: name.to_string(), fps, width, height, sample_rate, items, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cale_vers_l_exterieur_et_fusionne() {
        let rate = Rate::whole(25);
        assert_eq!(snap_to_frames(&[(0.01, 0.99)], rate, 100), vec![(0, 25)]);
        assert_eq!(snap_to_frames(&[(0.0, 1.0), (1.01, 2.0)], rate, 100), vec![(0, 50)]);
        assert_eq!(snap_to_frames(&[(3.9, 4.5)], rate, 100), vec![(97, 100)]);
        assert_eq!(snap_to_frames(&[(4.0, 4.5)], rate, 100), vec![]);
    }

    #[test]
    fn un_segment_vide_garde_une_image() {
        assert_eq!(snap_to_frames(&[(1.0, 1.0)], Rate::whole(25), 100), vec![(25, 26)]);
    }
}
