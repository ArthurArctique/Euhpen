//! Export XML Final Cut Pro 7 (« xmeml ») pour Premiere Pro et DaVinci Resolve.
//!
//! Premiere : Fichier > Importer. Resolve : Fichier > Importer > Timeline.
//!
//! Deux variantes, car les deux logiciels ne lisent pas l'audio de la même façon :
//!
//! * `premiere` : pistes « éclatées ». Une source stéréo donne deux pistes (gauche/droite), une
//!   source mono ou multicanal une piste par canal ; les liens audio/vidéo sont portés par le
//!   clip vidéo (avec trackindex/clipindex) ;
//! * `resolve` : une piste par flux audio ; chaque clip audio est relié au clip vidéo.
//!
//! Les valeurs `in`/`out` sont des images comptées depuis le début du fichier source,
//! `start`/`end` des images de la timeline. Chaque `<file>` n'est décrit en entier qu'une fois ;
//! les clips suivants n'y font référence que par son id.

use crate::fps::{format_timecode, is_xmeml_exact, parse_timecode, xmeml_rate, Rate};
use crate::probe::MediaInfo;
use crate::timeline::{Segment, Timeline, TimelineItem};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::fs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Premiere,
    Resolve,
}

pub const FLAVORS: [Flavor; 2] = [Flavor::Premiere, Flavor::Resolve];

impl Flavor {
    pub fn as_str(self) -> &'static str {
        match self {
            Flavor::Premiere => "premiere",
            Flavor::Resolve => "resolve",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Flavor::Premiere => "Premiere Pro (Fichier > Importer)",
            Flavor::Resolve => "DaVinci Resolve (Fichier > Importer > Timeline)",
        }
    }
}

fn percent_encode(text: &str, keep: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || "_.-~".contains(c) || (byte < 128 && keep.contains(c)) {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_drive_path(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

/// Chemin absolu → URL `file://` encodée en pourcentage (UTF-8).
///
/// `/Users/moi/Prise été.mov` → `file://localhost/Users/moi/Prise%20%C3%A9t%C3%A9.mov`
/// `C:\Rushes\a b.mov`        → `file://localhost/C:/Rushes/a%20b.mov`
/// `\\nas\rushes\a.mov`       → `file://nas/rushes/a.mov`
pub fn path_to_url(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    if p.starts_with("//") {
        return format!("file:{}", percent_encode(&p, "/:"));
    }
    if is_drive_path(&p) {
        p.insert(0, '/');
    }
    format!("file://localhost{}", percent_encode(&p, "/:"))
}

/// Variante de secours : chemin absolu brut.
pub fn raw_path(path: &str) -> String {
    let b = path.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        path.replace('\\', "/")
    } else {
        path.to_string()
    }
}

// --- petit modèle d'arbre XML -----------------------------------------------------------

struct El {
    tag: &'static str,
    attrs: Vec<(&'static str, String)>,
    text: String,
    children: Vec<El>,
}

impl El {
    fn new(tag: &'static str, attrs: Vec<(&'static str, String)>) -> El {
        El { tag, attrs, text: String::new(), children: Vec::new() }
    }

    fn sub(&mut self, tag: &'static str) -> &mut El {
        self.sub_with(tag, Vec::new())
    }

    fn sub_with(&mut self, tag: &'static str, attrs: Vec<(&'static str, String)>) -> &mut El {
        self.children.push(El::new(tag, attrs));
        self.children.last_mut().unwrap()
    }

    fn text(&mut self, tag: &'static str, value: impl Display) {
        self.sub(tag).text = value.to_string();
    }

    fn write(&self, out: &mut String, level: usize) {
        out.push('<');
        out.push_str(self.tag);
        for (key, value) in &self.attrs {
            out.push_str(&format!(" {key}=\"{}\"", escape_attribute(value)));
        }
        if !self.children.is_empty() {
            out.push('>');
            for child in &self.children {
                out.push('\n');
                out.push_str(&"  ".repeat(level + 1));
                child.write(out, level + 1);
            }
            out.push('\n');
            out.push_str(&"  ".repeat(level));
            out.push_str(&format!("</{}>", self.tag));
        } else if !self.text.is_empty() {
            out.push('>');
            out.push_str(&escape_text(&self.text));
            out.push_str(&format!("</{}>", self.tag));
        } else {
            out.push_str(" />");
        }
    }
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attribute(text: &str) -> String {
    escape_text(text).replace('"', "&quot;").replace('\r', "&#13;").replace('\n', "&#10;").replace('\t', "&#09;")
}

fn add_rate(parent: &mut El, rate: Rate) {
    let (timebase, ntsc) = xmeml_rate(rate);
    let el = parent.sub("rate");
    el.text("timebase", timebase);
    el.text("ntsc", if ntsc { "TRUE" } else { "FALSE" });
}

// --- plan des pistes --------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LaneKind {
    Stereo,
    Mono,
    Stream,
}

/// Une piste audio de la séquence alimentée par un canal (ou un flux) de la source.
#[derive(Clone, Copy, Debug)]
struct Lane {
    kind: LaneKind,
    /// 0 = gauche, 1 = droite (pistes stéréo éclatées de Premiere).
    exploded_index: i64,
    /// Valeur de `<sourcetrack><trackindex>`.
    source_track: i64,
}

fn audio_lanes(media: &MediaInfo, flavor: Flavor) -> Vec<Lane> {
    if flavor == Flavor::Resolve {
        return (0..media.audio_streams.len() as i64)
            .map(|k| Lane { kind: LaneKind::Stream, exploded_index: 0, source_track: k + 1 })
            .collect();
    }
    let mut lanes = Vec::new();
    let mut offset = 0;
    for stream in &media.audio_streams {
        if stream.channels == 2 {
            lanes.push(Lane { kind: LaneKind::Stereo, exploded_index: 0, source_track: offset + 1 });
            lanes.push(Lane { kind: LaneKind::Stereo, exploded_index: 1, source_track: offset + 2 });
        } else {
            for c in 0..stream.channels {
                lanes.push(Lane { kind: LaneKind::Mono, exploded_index: 0, source_track: offset + c + 1 });
            }
        }
        offset += stream.channels;
    }
    lanes
}

struct AudioClip {
    /// À partir de 0.
    track: usize,
    lane: Lane,
    clip_id: String,
    /// À partir de 1, position dans sa piste.
    clip_index: i64,
}

struct Placed<'a> {
    item: &'a TimelineItem,
    segment: Segment,
    video_id: Option<String>,
    video_index: i64,
    audio: Vec<AudioClip>,
}

struct Writer<'a> {
    tl: &'a Timeline,
    premiere: bool,
    flavor: Flavor,
    zero_timecode: bool,
    raw_paths: bool,
    file_ids: HashMap<String, String>,
    declared: HashSet<String>,
    warnings: Vec<String>,
}

fn link(parent: &mut El, reference: &str, mediatype: Option<&str>, indices: Option<(i64, i64)>) {
    let el = parent.sub("link");
    el.text("linkclipref", reference);
    if let Some(kind) = mediatype {
        el.text("mediatype", kind);
    }
    if let Some((track, clip)) = indices {
        el.text("trackindex", track);
        el.text("clipindex", clip);
    }
}

fn add_format(parent: &mut El, rate: Rate, width: i64, height: i64) {
    add_rate(parent, rate);
    parent.text("width", width);
    parent.text("height", height);
    parent.text("anamorphic", "FALSE");
    parent.text("pixelaspectratio", "square");
    parent.text("fielddominance", "none");
}

impl<'a> Writer<'a> {
    fn place(&mut self) -> (Vec<Placed<'a>>, Vec<Lane>) {
        let mut track_kinds: Vec<Lane> = Vec::new();
        let mut lanes_by_item = Vec::with_capacity(self.tl.items.len());
        for item in &self.tl.items {
            let lanes = audio_lanes(&item.media, self.flavor);
            let mut mismatched = Vec::new();
            for (k, lane) in lanes.iter().enumerate() {
                match track_kinds.get(k) {
                    None => track_kinds.push(*lane),
                    Some(known) if (known.kind, known.exploded_index) != (lane.kind, lane.exploded_index) => {
                        mismatched.push(format!("A{}", k + 1));
                    }
                    _ => {}
                }
            }
            if !mismatched.is_empty() {
                self.warnings.push(format!(
                    "{} n'a pas la même disposition audio que le premier fichier (piste{} {}) : vérifiez l'audio après import.",
                    item.media.name(),
                    if mismatched.len() > 1 { "s" } else { "" },
                    mismatched.join(", ")
                ));
            }
            lanes_by_item.push(lanes);
        }

        let mut placed = Vec::new();
        let mut counter = 0;
        let mut video_count = 0;
        let mut audio_counts = vec![0_i64; track_kinds.len()];
        for (item, lanes) in self.tl.items.iter().zip(&lanes_by_item) {
            for &segment in &item.segments {
                let mut video_id = None;
                if item.media.has_video {
                    counter += 1;
                    video_count += 1;
                    video_id = Some(format!("clipitem-{counter}"));
                }
                let mut audio = Vec::with_capacity(lanes.len());
                for (k, lane) in lanes.iter().enumerate() {
                    counter += 1;
                    audio_counts[k] += 1;
                    audio.push(AudioClip { track: k, lane: *lane, clip_id: format!("clipitem-{counter}"), clip_index: audio_counts[k] });
                }
                placed.push(Placed { item, segment, video_id, video_index: video_count, audio });
            }
        }
        (placed, track_kinds)
    }

    fn file_id(&mut self, media: &MediaInfo) -> String {
        let next = format!("file-{}", self.file_ids.len() + 1);
        self.file_ids.entry(media.path.clone()).or_insert(next).clone()
    }

    fn native_rate(&self, media: &MediaInfo) -> Rate {
        match media.fps {
            Some(fps) if media.has_video && is_xmeml_exact(fps) => fps,
            _ => self.tl.fps,
        }
    }

    fn add_timecode(&mut self, parent: &mut El, media: &MediaInfo, rate: Rate) {
        let (mut frame, mut drop) = (0, false);
        if !self.zero_timecode {
            if let Some(tc) = media.start_timecode.as_deref().filter(|t| !t.is_empty()) {
                match parse_timecode(tc, rate) {
                    Ok(parsed) => (frame, drop) = parsed,
                    Err(_) => self.warnings.push(format!(
                        "timecode illisible pour {} ({tc}) : 00:00:00:00 utilisé.",
                        media.name()
                    )),
                }
            } else if let Some(reference) = media.bwf_time_reference.filter(|_| media.sample_rate() != 0) {
                let seconds = reference as f64 / media.sample_rate() as f64;
                frame = (seconds * rate.num as f64 / rate.den as f64).round_ties_even() as i64;
            }
        }
        let tc = parent.sub("timecode");
        add_rate(tc, rate);
        tc.text("string", format_timecode(frame, rate, drop));
        tc.text("frame", frame);
        tc.text("displayformat", if drop { "DF" } else { "NDF" });
    }

    fn add_file(&mut self, parent: &mut El, media: &MediaInfo) {
        let file_id = self.file_id(media);
        let el = parent.sub_with("file", vec![("id", file_id.clone())]);
        if !self.declared.insert(file_id) {
            return;
        }
        let rate = self.native_rate(media);
        el.text("name", media.name());
        el.text("pathurl", if self.raw_paths { raw_path(&media.path) } else { path_to_url(&media.path) });
        add_rate(el, rate);
        // Resolve veut la balise présente mais vide ; Premiere accepte la vraie durée.
        if self.premiere {
            el.text("duration", media.frame_count(rate));
        } else {
            el.text("duration", "");
        }
        self.add_timecode(el, media, rate);
        let m = el.sub("media");
        if media.has_video {
            let sc = m.sub("video").sub("samplecharacteristics");
            add_format(sc, rate, media.width, media.height);
        }
        for stream in &media.audio_streams {
            let audio = m.sub("audio");
            let sc = audio.sub("samplecharacteristics");
            sc.text("depth", stream.bit_depth);
            sc.text("samplerate", stream.sample_rate);
            audio.text("channelcount", stream.channels);
        }
    }

    fn clip_common(&mut self, el: &mut El, p: &Placed) {
        el.text("name", p.item.media.name());
        el.text("enabled", "TRUE");
        add_rate(el, self.tl.fps);
        el.text("start", p.segment.timeline_start);
        el.text("end", p.segment.timeline_end());
        el.text("in", p.segment.source_in);
        el.text("out", p.segment.source_out);
        self.add_file(el, &p.item.media);
    }

    fn build(&mut self) -> El {
        let tl = self.tl;
        let (placed, track_kinds) = self.place();

        let mut root = El::new("xmeml", vec![("version", "5".to_string())]);
        let mut seq_attrs = vec![("id", "sequence-1".to_string())];
        if self.premiere {
            seq_attrs.push(("explodedTracks", "true".to_string()));
        }
        let seq = root.sub_with("sequence", seq_attrs);
        seq.text("name", &tl.name);
        seq.text("duration", tl.duration_frames());
        add_rate(seq, tl.fps);
        let tc = seq.sub("timecode");
        add_rate(tc, tl.fps);
        tc.text("string", "00:00:00:00");
        tc.text("frame", 0);
        tc.text("displayformat", "NDF");

        let media = seq.sub("media");

        // Vidéo
        let video = media.sub("video");
        let sc = video.sub("format").sub("samplecharacteristics");
        add_format(sc, tl.fps, tl.width, tl.height);
        if placed.iter().any(|p| p.video_id.is_some()) {
            let track = video.sub("track");
            for p in &placed {
                let Some(video_id) = &p.video_id else {
                    continue;
                };
                let ci = track.sub_with("clipitem", vec![("id", video_id.clone())]);
                self.clip_common(ci, p);
                if self.premiere {
                    link(ci, video_id, Some("video"), Some((1, p.video_index)));
                    for a in &p.audio {
                        link(ci, &a.clip_id, Some("audio"), Some((a.track as i64 + 1, a.clip_index)));
                    }
                } else {
                    link(ci, video_id, None, None);
                    for a in &p.audio {
                        link(ci, &a.clip_id, None, None);
                    }
                }
            }
        }

        // Audio
        let audio = media.sub("audio");
        if self.premiere {
            audio.text("numOutputChannels", 2);
        }
        let sc = audio.sub("format").sub("samplecharacteristics");
        sc.text("depth", 16);
        sc.text("samplerate", tl.sample_rate);
        for (k, kind) in track_kinds.iter().enumerate() {
            let mut attrs = Vec::new();
            if self.premiere {
                let stereo = kind.kind == LaneKind::Stereo;
                attrs.push(("currentExplodedTrackIndex", kind.exploded_index.to_string()));
                attrs.push(("totalExplodedTrackCount", if stereo { "2" } else { "1" }.to_string()));
                attrs.push(("premiereTrackType", if stereo { "Stereo" } else { "Mono" }.to_string()));
            }
            let track = audio.sub_with("track", attrs);
            if self.premiere && kind.kind == LaneKind::Stereo && tl.has_video() {
                track.text("outputchannelindex", kind.exploded_index + 1);
            }
            for p in &placed {
                for a in p.audio.iter().filter(|a| a.track == k) {
                    let mut attrs = vec![("id", a.clip_id.clone())];
                    if self.premiere {
                        attrs.push(("premiereChannelType", if a.lane.kind == LaneKind::Stereo { "stereo" } else { "mono" }.to_string()));
                    }
                    let ci = track.sub_with("clipitem", attrs);
                    self.clip_common(ci, p);
                    let st = ci.sub("sourcetrack");
                    st.text("mediatype", "audio");
                    st.text("trackindex", a.lane.source_track);
                    if let (false, Some(video_id)) = (self.premiere, &p.video_id) {
                        link(ci, video_id, Some("video"), None);
                        for other in &p.audio {
                            link(ci, &other.clip_id, None, None);
                        }
                    }
                }
            }
        }

        // Un marqueur au début de chaque fichier (s'il y en a plusieurs).
        if tl.items.len() > 1 {
            for item in tl.items.iter().filter(|i| !i.segments.is_empty()) {
                let marker = seq.sub("marker");
                marker.text("comment", "");
                marker.text("name", item.media.name());
                marker.text("in", item.segments[0].timeline_start);
                marker.text("out", -1);
            }
        }
        root
    }
}

/// Renvoie (contenu XML en UTF-8, avertissements).
pub fn timeline_to_xmeml(tl: &Timeline, flavor: Flavor, zero_timecode: bool, raw_paths: bool) -> (String, Vec<String>) {
    let mut writer = Writer {
        tl,
        premiere: flavor == Flavor::Premiere,
        flavor,
        zero_timecode,
        raw_paths,
        file_ids: HashMap::new(),
        declared: HashSet::new(),
        warnings: Vec::new(),
    };
    let root = writer.build();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n");
    root.write(&mut xml, 0);
    xml.push('\n');
    (xml, writer.warnings)
}

pub fn write_xmeml(tl: &Timeline, path: &str, flavor: Flavor, zero_timecode: bool, raw_paths: bool) -> Result<Vec<String>, String> {
    let (data, warnings) = timeline_to_xmeml(tl, flavor, zero_timecode, raw_paths);
    let describe = |e: std::io::Error| format!("{path} : {e}");
    let tmp = format!("{path}.tmp");
    fs::write(&tmp, data).map_err(describe)?;
    fs::rename(&tmp, path).map_err(describe)?;
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(path_to_url("/Users/moi/Prise été.mov"), "file://localhost/Users/moi/Prise%20%C3%A9t%C3%A9.mov");
        assert_eq!(path_to_url(r"C:\Rushes\a b.mov"), "file://localhost/C:/Rushes/a%20b.mov");
        assert_eq!(path_to_url(r"\\nas\rushes\a.mov"), "file://nas/rushes/a.mov");
    }

    #[test]
    fn chemins_bruts() {
        assert_eq!(raw_path(r"C:\Rushes\a.mov"), "C:/Rushes/a.mov");
        assert_eq!(raw_path("/a/b.mov"), "/a/b.mov");
    }

    #[test]
    fn serialisation_comme_elementtree() {
        let mut root = El::new("a", vec![("v", "x\"y".to_string())]);
        root.text("vide", "");
        root.text("texte", "a<b");
        root.sub("parent").text("enfant", 1);
        let mut out = String::new();
        root.write(&mut out, 0);
        assert_eq!(
            out,
            "<a v=\"x&quot;y\">\n  <vide />\n  <texte>a&lt;b</texte>\n  <parent>\n    <enfant>1</enfant>\n  </parent>\n</a>"
        );
    }
}
