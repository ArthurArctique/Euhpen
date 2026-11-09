//! La timeline : onde, coupes, mots, règle et vue d'ensemble.

use eframe::egui;
use egui::epaint::Mesh;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui};

use crate::decision::{CutPiece, Reason};
use crate::settings::Threshold;
use crate::transcript::Kind;

use super::state::App;
use super::theme::{self, alpha};
use super::words::token_label;

const DB_FLOOR: f64 = -66.0;
const DB_TOP: f64 = -6.0;
const BAND_H: f32 = 20.0;
const WORDS_H: f32 = 18.0;
const RULER_H: f32 = 18.0;
const OVERVIEW_H: f32 = 14.0;
const MIN_SPAN: f64 = 0.5;
const RULER_STEPS: [f64; 16] =
    [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0];

#[derive(Default)]
pub struct TimelineView {
    pub start: f64,
    pub span: f64,
    dragging_threshold: bool,
    panning: bool,
    last_position: f64,
}

impl TimelineView {
    pub fn reset(&mut self) {
        self.start = 0.0;
        self.span = 0.0;
        self.last_position = 0.0;
    }

    fn clamp(&mut self, duration: f64) {
        let shortest = MIN_SPAN.min(duration).max(0.001);
        if self.span <= 0.0 {
            self.span = duration.max(shortest);
        }
        self.span = self.span.clamp(shortest, duration.max(shortest));
        self.start = self.start.clamp(0.0, (duration - self.span).max(0.0));
    }

    /// Zoome autour d'un instant, qui reste au même endroit à l'écran.
    pub fn zoom_at(&mut self, t: f64, factor: f64, duration: f64) {
        let fraction = if self.span > 0.0 { ((t - self.start) / self.span).clamp(0.0, 1.0) } else { 0.5 };
        if self.span <= 0.0 {
            self.span = duration;
        }
        self.span *= factor;
        self.clamp(duration);
        self.start = t - fraction * self.span;
        self.clamp(duration);
    }

    pub fn show_all(&mut self, duration: f64) {
        self.start = 0.0;
        self.span = duration;
    }

    fn follow(&mut self, position: f64) {
        let end = self.start + self.span;
        if position < self.start || position > end {
            self.start = position - self.span * 0.3;
        } else if position > end - self.span * 0.04 {
            self.start = position - self.span * 0.08;
        }
    }
}

fn amplitude(db: f64) -> f32 {
    ((db - DB_FLOOR) / (DB_TOP - DB_FLOOR)).clamp(0.0, 1.0) as f32
}

fn ruler_label(t: f64, step: f64) -> String {
    let whole = t.round() as i64;
    if step < 1.0 {
        let minutes = (t / 60.0).floor() as i64;
        let seconds = t - minutes as f64 * 60.0;
        let decimals = if step < 1.0 { 1 } else { 0 };
        return format!("{minutes}:{seconds:04.decimals$}").replace('.', ",");
    }
    let (h, m, s) = (whole / 3600, whole / 60 % 60, whole % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Fragments de coupe : on ne dessine que ce qui est visible.
fn visible_pieces(removed: &[CutPiece], start: f64, end: f64) -> impl Iterator<Item = &CutPiece> {
    let from = removed.partition_point(|p| p.end <= start);
    removed[from..].iter().take_while(move |p| p.start < end)
}

impl App {
    pub fn zoom_keyboard(&mut self, factor: f64) {
        let Some(analysis) = self.current_analysis() else { return };
        let t = self.player.position();
        self.timeline.zoom_at(t, factor, analysis.media.duration);
    }

    pub fn zoom_all(&mut self) {
        if let Some(analysis) = self.current_analysis() {
            self.timeline.show_all(analysis.media.duration);
        }
    }

    pub fn timeline_ui(&mut self, ui: &mut Ui) {
        let width = ui.available_width();
        let Some(analysis) = self.current_analysis() else {
            let (rect, _) = ui.allocate_exact_size(vec2(width, ui.available_height().max(40.0)), Sense::hover());
            ui.painter().rect_filled(rect, 6, theme::BG);
            return;
        };
        let Some(plan) = self.plan.clone() else { return };
        let duration = analysis.media.duration.max(0.001);
        let position = self.player.position();
        let transcript = self.current().and_then(|i| i.transcript.clone());
        let force_keep = self.current().map(|i| i.overrides.force_keep.clone()).unwrap_or_default();
        let offset = analysis.media.audio_offset;

        self.timeline.clamp(duration);
        if !self.timeline.panning && (position - self.timeline.last_position).abs() > 1e-6 {
            self.timeline.follow(position);
            self.timeline.clamp(duration);
        }
        self.timeline.last_position = position;

        let words_h = if transcript.is_some() { WORDS_H } else { 0.0 };
        let total = ui.available_height().max(BAND_H + RULER_H + words_h + OVERVIEW_H + 40.0);
        let main_h = total - OVERVIEW_H - 4.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, main_h), Sense::click_and_drag());
        let band = Rect::from_min_size(rect.min, vec2(width, BAND_H));
        let wave = Rect::from_min_max(pos2(rect.left(), band.bottom()), pos2(rect.right(), rect.bottom() - RULER_H - words_h));
        let word_lane = Rect::from_min_max(pos2(rect.left(), wave.bottom()), pos2(rect.right(), wave.bottom() + words_h));
        let ruler = Rect::from_min_max(pos2(rect.left(), rect.bottom() - RULER_H), rect.max);

        let threshold_db = plan.levels.threshold_db;
        let half = wave.height() / 2.0;
        let middle = wave.center().y;
        let threshold_dy = amplitude(threshold_db) * half;
        let manual = matches!(self.settings.threshold, Threshold::Db(_));

        // Interactions (avant le dessin : la vue affichée est celle qu'on vient de calculer).
        let (start_before, span_before) = (self.timeline.start, self.timeline.span);
        let time_at = |x: f32| start_before + ((x - rect.left()) / width.max(1.0)) as f64 * span_before;
        let pointer = response.hover_pos().or(response.interact_pointer_pos());
        let near_threshold = pointer.is_some_and(|p| {
            manual && wave.contains(p) && ((p.y - (middle - threshold_dy)).abs() < 5.0 || (p.y - (middle + threshold_dy)).abs() < 5.0)
        });

        if response.drag_started() {
            if near_threshold {
                self.timeline.dragging_threshold = true;
            } else {
                self.timeline.panning = true;
            }
        }
        if response.dragged() {
            if self.timeline.dragging_threshold {
                if let Some(p) = response.interact_pointer_pos() {
                    let amp = ((p.y - middle).abs() / half.max(1.0)).min(1.0) as f64;
                    let db = ((DB_FLOOR + amp * (DB_TOP - DB_FLOOR)) * 2.0).round() / 2.0;
                    self.settings.threshold = Threshold::Db(db.clamp(-80.0, -5.0));
                }
            } else {
                self.timeline.start -= response.drag_delta().x as f64 / width.max(1.0) as f64 * self.timeline.span;
                self.timeline.clamp(duration);
            }
        }
        if response.drag_stopped() {
            self.timeline.dragging_threshold = false;
            self.timeline.panning = false;
        }
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let t = time_at(p.x).clamp(0.0, duration);
                if band.contains(p) {
                    self.toggle_at(t, None);
                } else {
                    self.player.seek(t);
                }
            }
        }
        if response.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta()));
            if let Some(p) = pointer {
                if scroll.y != 0.0 {
                    let factor = (-(scroll.y as f64) * 0.005).exp();
                    self.timeline.zoom_at(time_at(p.x), factor, duration);
                }
                if zoom != 1.0 {
                    self.timeline.zoom_at(time_at(p.x), 1.0 / zoom as f64, duration);
                }
            }
            if scroll.x != 0.0 {
                self.timeline.start -= scroll.x as f64 / width.max(1.0) as f64 * self.timeline.span;
                self.timeline.clamp(duration);
            }
        }

        // Dessin.
        let (start, span) = (self.timeline.start, self.timeline.span);
        let end = start + span;
        let x_of = |t: f64| rect.left() + ((t - start) / span) as f32 * width;
        let time_of = |x: f32| start + ((x - rect.left()) / width.max(1.0)) as f64 * span;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6, theme::BG);

        let mut mesh = Mesh::default();
        // Fond teinté sur les coupes.
        for piece in visible_pieces(&plan.removed, start, end) {
            let tint = match piece.reason {
                Reason::Silence | Reason::Short => alpha(Color32::WHITE, 0.045),
                reason => alpha(theme::reason_color(reason), 0.10),
            };
            let area = Rect::from_min_max(pos2(x_of(piece.start), wave.top()), pos2(x_of(piece.end), wave.bottom()));
            mesh.add_colored_rect(area.intersect(wave), tint);
        }

        // Onde : une barre par point, couleur selon le sort du passage.
        let hop = analysis.envelope.hop;
        let db = &analysis.envelope.db;
        let mut piece_at = plan.removed.partition_point(|p| p.end <= start);
        let columns = width.floor() as usize;
        for column in 0..columns {
            let x = rect.left() + column as f32;
            let (t0, t1) = (time_of(x), time_of(x + 1.0));
            if t0 >= duration {
                break;
            }
            let first = (t0 / hop).floor().max(0.0) as usize;
            let last = ((t1 / hop).ceil() as usize).max(first + 1).min(db.len());
            let Some(peak) = db.get(first..last).and_then(|s| s.iter().copied().reduce(f32::max)) else { continue };
            let height = (amplitude(peak as f64) * half).max(0.5);
            let centre = (t0 + t1) / 2.0;
            while plan.removed.get(piece_at).is_some_and(|p| p.end <= centre) {
                piece_at += 1;
            }
            let color = match plan.removed.get(piece_at).filter(|p| p.start <= centre) {
                Some(piece) => match piece.reason {
                    Reason::Silence | Reason::Short => alpha(theme::reason_color(piece.reason), 0.75),
                    reason => alpha(theme::reason_color(reason), 0.90),
                },
                None => theme::KEPT,
            };
            mesh.add_colored_rect(Rect::from_min_max(pos2(x, middle - height), pos2(x + 1.0, middle + height)), color);
        }

        // Bande des segments.
        mesh.add_colored_rect(band, alpha(theme::KEPT, 0.55));
        for piece in visible_pieces(&plan.removed, start, end) {
            let area = Rect::from_min_max(pos2(x_of(piece.start), band.top()), pos2(x_of(piece.end), band.bottom()));
            mesh.add_colored_rect(area.intersect(band), theme::reason_color(piece.reason));
        }
        painter.add(Shape::mesh(mesh));

        for &(a, b) in force_keep.iter().filter(|(a, b)| *b > start && *a < end) {
            let area = Rect::from_min_max(pos2(x_of(a).max(rect.left()), band.top() + 1.0), pos2(x_of(b).min(rect.right()), band.bottom() - 1.0));
            let stroke = Stroke::new(1.0, Color32::WHITE);
            let corners = [area.left_top(), area.right_top(), area.right_bottom(), area.left_bottom(), area.left_top()];
            painter.extend(Shape::dashed_line(&corners, stroke, 3.0, 2.0));
        }

        // Ligne de seuil.
        let line_color = if manual { theme::WARN } else { alpha(theme::WARN, 0.55) };
        for y in [middle - threshold_dy, middle + threshold_dy] {
            painter.extend(Shape::dashed_line(&[pos2(wave.left(), y), pos2(wave.right(), y)], Stroke::new(1.0, line_color), 5.0, 4.0));
        }
        let label = format!("seuil {} dB{}", number(threshold_db, 1), if manual { "" } else { " (auto)" });
        let font = FontId::proportional(11.0);
        let galley = painter.layout_no_wrap(label, font.clone(), theme::WARN);
        let label_at = pos2(wave.left() + 4.0, (middle - threshold_dy - 14.0).max(wave.top()));
        painter.rect_filled(Rect::from_min_size(label_at, galley.size() + vec2(8.0, 1.0)), 3, alpha(theme::BG, 0.8));
        painter.galley(label_at + vec2(4.0, 0.0), galley, theme::WARN);

        // Mots.
        if let Some(transcript) = transcript.as_ref() {
            painter.rect_filled(word_lane, 0, theme::PANEL);
            let seconds_per_point = span / width.max(1.0) as f64;
            let readable = seconds_per_point < 0.06;
            let from = transcript.words.partition_point(|w| w.end + offset <= start);
            for word in transcript.words[from..].iter().take_while(|w| w.start + offset < end) {
                let (a, b) = (x_of(word.start + offset), x_of(word.end + offset));
                let area = Rect::from_min_max(pos2(a, word_lane.top() + 2.0), pos2(b.max(a + 1.0), word_lane.bottom() - 2.0));
                let color = theme::kind_color(word.kind).unwrap_or(theme::LINE_2);
                painter.rect_filled(area, 2, if word.kind == Kind::Word { color } else { alpha(color, 0.75) });
                if readable {
                    let text = if word.kind == Kind::Word { word.text.clone() } else { token_label(&word.text, &transcript.language) };
                    let clip = painter.with_clip_rect(area.intersect(word_lane));
                    clip.text(area.left_center() + vec2(3.0, 0.0), Align2::LEFT_CENTER, text, font.clone(), theme::TEXT);
                }
            }
            if !readable {
                painter.text(word_lane.center(), Align2::CENTER_CENTER, "zoomez pour lire les mots", font.clone(), alpha(theme::MUTED, 0.8));
            }
        }

        // Règle.
        painter.rect_filled(ruler, 0, theme::PANEL);
        let step = RULER_STEPS.iter().copied().find(|s| (s / span) as f32 * width >= 74.0).unwrap_or(7200.0);
        let mut tick = (start / step).floor() * step;
        while tick <= end {
            if tick >= start {
                let x = x_of(tick);
                painter.vline(x, ruler.top()..=ruler.top() + 5.0, Stroke::new(1.0, theme::LINE_2));
                painter.text(pos2(x + 3.0, ruler.center().y + 1.0), Align2::LEFT_CENTER, ruler_label(tick, step), font.clone(), theme::MUTED);
            }
            tick += step;
        }

        // Survol et tête de lecture.
        if let Some(p) = response.hover_pos() {
            painter.vline(p.x, rect.y_range(), Stroke::new(1.0, alpha(Color32::WHITE, 0.25)));
        }
        if position >= start && position <= end {
            let x = x_of(position);
            painter.vline(x, rect.y_range(), Stroke::new(1.5, Color32::WHITE));
            let top = rect.top();
            painter.add(Shape::convex_polygon(
                vec![pos2(x - 5.0, top), pos2(x + 5.0, top), pos2(x, top + 7.0)],
                Color32::WHITE,
                Stroke::NONE,
            ));
        }

        let cursor = if self.timeline.dragging_threshold || near_threshold {
            CursorIcon::ResizeVertical
        } else if self.timeline.panning {
            CursorIcon::Grabbing
        } else if pointer.is_some_and(|p| band.contains(p)) {
            CursorIcon::PointingHand
        } else {
            CursorIcon::Default
        };
        let response = response.on_hover_cursor(cursor);
        if !self.timeline.panning && !self.timeline.dragging_threshold {
            if let Some(p) = response.hover_pos() {
                let t = time_of(p.x).clamp(0.0, duration);
                let text = self.hover_text(&analysis.envelope.db, hop, t, band.contains(p));
                response.on_hover_text_at_pointer(text);
            }
        }

        ui.add_space(4.0);
        self.overview_ui(ui, width, duration, position);
    }

    fn hover_text(&self, db: &[f32], hop: f64, t: f64, on_band: bool) -> String {
        let mut text = crate::cli::duration_text(t);
        if let Some(value) = db.get((t / hop) as usize) {
            text += &format!(" · {} dB", number(*value as f64, 0));
        }
        if let Some(plan) = &self.plan {
            match plan.removed.iter().find(|p| p.start <= t && t < p.end) {
                Some(piece) => {
                    text += &format!(
                        "\n{} · {} s",
                        theme::reason_label(piece.reason),
                        number(piece.end - piece.start, 2)
                    );
                    if on_band {
                        text += "\ncliquer pour garder";
                    }
                }
                None => {
                    text += "\ngardé";
                    if on_band {
                        text += "\ncliquer pour couper";
                    }
                }
            }
        }
        if let Some(item) = self.current() {
            if let (Some(transcript), Some(analysis)) = (&item.transcript, &item.analysis) {
                let inner = t - analysis.media.audio_offset;
                let at = transcript.words.partition_point(|w| w.end <= inner);
                if let Some(word) = transcript.words.get(at).filter(|w| w.start <= inner) {
                    let label = if word.kind == Kind::Word { word.text.clone() } else { token_label(&word.text, &transcript.language) };
                    text += &format!("\n« {label} »");
                    if let (Some(p), true) = (word.probability, word.kind != Kind::Word) {
                        text += &format!(" {} %", (p * 100.0).round() as i64);
                    }
                }
            }
        }
        text
    }

    fn overview_ui(&mut self, ui: &mut Ui, width: f32, duration: f64, position: f64) {
        let Some(plan) = self.plan.clone() else { return };
        let (rect, response) = ui.allocate_exact_size(vec2(width, OVERVIEW_H), Sense::click_and_drag());
        let x_of = |t: f64| rect.left() + (t / duration) as f32 * width;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3, alpha(theme::KEPT, 0.35));
        let mut mesh = Mesh::default();
        for piece in &plan.removed {
            let area = Rect::from_min_max(pos2(x_of(piece.start), rect.top()), pos2(x_of(piece.end), rect.bottom()));
            mesh.add_colored_rect(area, theme::BG);
        }
        painter.add(Shape::mesh(mesh));
        let view = Rect::from_min_max(
            Pos2::new(x_of(self.timeline.start), rect.top()),
            Pos2::new(x_of(self.timeline.start + self.timeline.span).max(x_of(self.timeline.start) + 4.0), rect.bottom()),
        );
        painter.rect_filled(view, 2, alpha(Color32::WHITE, 0.14));
        painter.rect_stroke(view, 2, Stroke::new(1.0, alpha(Color32::WHITE, 0.55)), egui::StrokeKind::Inside);
        painter.vline(x_of(position), rect.y_range(), Stroke::new(1.0, Color32::WHITE));
        if response.clicked() || response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                let t = ((p.x - rect.left()) / width.max(1.0)) as f64 * duration;
                self.timeline.start = t - self.timeline.span / 2.0;
                self.timeline.clamp(duration);
            }
        }
        response.on_hover_cursor(CursorIcon::Grab);
    }

    pub fn legend_ui(&self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.x = 10.0;
        let entries = [
            (theme::KEPT, "gardé"),
            (theme::SILENCE, "silence"),
            (theme::HESITATION, "hésitation"),
            (theme::COUGH, "toux"),
            (theme::LAUGH, "rire"),
            (theme::BREATH, "respiration"),
            (theme::MANUAL, "à la main"),
        ];
        for (color, label) in entries {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (rect, _) = ui.allocate_exact_size(vec2(9.0, 9.0), Sense::hover());
                ui.painter().rect_filled(rect, 2, color);
                ui.label(theme::hint(label));
            });
        }
    }
}

/// Nombre avec virgule décimale et vrai signe moins.
pub fn number(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}").replace('.', ",").replace('-', "−")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_pointed_instant_in_place() {
        let mut view = TimelineView { start: 10.0, span: 20.0, ..Default::default() };
        view.zoom_at(15.0, 0.5, 100.0);
        assert!((view.span - 10.0).abs() < 1e-9);
        assert!((view.start - 12.5).abs() < 1e-9);
    }

    #[test]
    fn the_view_stays_inside_the_media() {
        let mut view = TimelineView { start: 95.0, span: 20.0, ..Default::default() };
        view.clamp(100.0);
        assert_eq!(view.start, 80.0);
        view.zoom_at(50.0, 1000.0, 100.0);
        assert_eq!((view.start, view.span), (0.0, 100.0));
        view.zoom_at(50.0, 0.0001, 100.0);
        assert_eq!(view.span, MIN_SPAN);
    }

    #[test]
    fn the_playhead_is_followed_when_it_leaves_the_view() {
        let mut view = TimelineView { start: 0.0, span: 10.0, ..Default::default() };
        view.follow(9.8);
        assert!((view.start - 9.0).abs() < 1e-9);
        view.follow(50.0);
        assert!((view.start - 47.0).abs() < 1e-9);
    }

    #[test]
    fn ruler_labels_follow_the_scale() {
        assert_eq!(ruler_label(65.0, 5.0), "1:05");
        assert_eq!(ruler_label(3725.0, 60.0), "1:02:05");
        assert_eq!(ruler_label(65.5, 0.5), "1:05,5");
    }
}
