//! La transcription mot à mot, cliquable.

use std::ops::Range;
use std::sync::Arc;

use eframe::egui;
use egui::{pos2, vec2, Align2, FontId, Rect, ScrollArea, Sense, Stroke, Ui};

use crate::intervals::{find, union, Interval};
use crate::transcript::{Kind, Transcript};

use super::state::{App, TrStatus};
use super::theme::{self, alpha};

const ROW_H: f32 = 21.0;
const GAP: f32 = 5.0;

/// Texte affiché pour un jeton entre crochets.
pub fn token_label(text: &str, language: &str) -> String {
    let bare = text.replace(['[', ']'], "").to_lowercase();
    if language == "fr" {
        if bare == "uh" {
            return "euh".to_string();
        }
        if bare == "um" {
            return "hum".to_string();
        }
    }
    bare.replace('_', " ")
}

struct Token {
    text: String,
    width: f32,
}

#[derive(Default)]
pub struct WordsView {
    /// Transcription dont les jetons sont mesurés.
    measured: Option<Arc<Transcript>>,
    tokens: Vec<Token>,
    lines: Vec<Range<usize>>,
    line_width: f32,
    cut: Vec<bool>,
    plan_changed: bool,
    last_line: Option<usize>,
    info: String,
}

impl WordsView {
    pub fn invalidate(&mut self) {
        self.measured = None;
        self.line_width = 0.0;
        self.last_line = None;
    }

    pub fn mark_plan_changed(&mut self) {
        self.plan_changed = true;
    }

    fn measure(&mut self, ui: &Ui, transcript: &Arc<Transcript>) {
        if self.measured.as_ref().is_some_and(|m| Arc::ptr_eq(m, transcript)) {
            return;
        }
        let font = FontId::proportional(13.0);
        self.tokens = ui.fonts_mut(|fonts| {
            transcript
                .words
                .iter()
                .map(|word| {
                    let text = if word.kind == Kind::Word { word.text.trim().to_string() } else { token_label(&word.text, &transcript.language) };
                    let width = fonts.layout_no_wrap(text.clone(), font.clone(), theme::TEXT).size().x;
                    Token { text, width }
                })
                .collect()
        });
        let tokens = transcript.bracket_tokens();
        self.info = format!("{} mots", transcript.words.len());
        if !tokens.is_empty() {
            let list: Vec<String> = tokens.iter().map(|(t, n)| format!("{t} ×{n}")).collect();
            self.info += &format!(" · jetons : {}", list.join(", "));
        }
        self.measured = Some(transcript.clone());
        self.line_width = 0.0;
        self.plan_changed = true;
    }

    fn wrap(&mut self, width: f32) {
        if (self.line_width - width).abs() < 0.5 {
            return;
        }
        self.line_width = width;
        self.lines.clear();
        let (mut begin, mut used) = (0, 0.0);
        for (index, token) in self.tokens.iter().enumerate() {
            let needed = token.width + GAP;
            if used + needed > width && index > begin {
                self.lines.push(begin..index);
                begin = index;
                used = 0.0;
            }
            used += needed;
        }
        if begin < self.tokens.len() {
            self.lines.push(begin..self.tokens.len());
        }
        self.last_line = None;
    }

    fn line_of(&self, token: usize) -> Option<usize> {
        let at = self.lines.partition_point(|line| line.end <= token);
        (at < self.lines.len()).then_some(at)
    }

    fn mark_cuts(&mut self, transcript: &Transcript, merged: &[Interval], offset: f64) {
        if !self.plan_changed {
            return;
        }
        self.plan_changed = false;
        self.cut = transcript
            .words
            .iter()
            .map(|w| find(merged, (w.start + w.end) / 2.0 + offset).is_some())
            .collect();
    }
}

impl App {
    fn words_placeholder(&self) -> String {
        match self.current().map(|i| i.tr.clone()) {
            Some(TrStatus::Queued) => "Transcription en attente…".to_string(),
            Some(TrStatus::Running(..)) => "Transcription en cours…".to_string(),
            Some(TrStatus::Unavailable(message)) => message,
            Some(TrStatus::Error(message)) => format!("La transcription a échoué : {message}"),
            Some(TrStatus::Cancelled) => "Transcription annulée.".to_string(),
            _ => "Activez « Hésitations » à droite pour transcrire ce fichier mot à mot et repérer les « euh ».".to_string(),
        }
    }

    pub fn words_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Transcription").strong());
            let measured = self.current().and_then(|i| i.transcript.clone()).is_some();
            if measured {
                ui.label(theme::hint(self.words.info.clone()));
            }
        });
        let Some(transcript) = self.current().and_then(|i| i.transcript.clone()) else {
            ui.add(egui::Label::new(theme::hint(self.words_placeholder())).wrap());
            return;
        };
        if transcript.words.is_empty() {
            ui.label(theme::hint("Aucune parole reconnue dans ce fichier (vérifiez la langue choisie)."));
            return;
        }
        let offset = self.current_analysis().map(|a| a.media.audio_offset).unwrap_or(0.0);
        let merged = self
            .plan
            .as_ref()
            .map(|plan| union(&plan.removed.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>()))
            .unwrap_or_default();
        let width = ui.available_width() - 14.0;
        self.words.measure(ui, &transcript);
        self.words.wrap(width.max(40.0));
        self.words.mark_cuts(&transcript, &merged, offset);

        let t = self.player.position() - offset;
        let at = transcript.words.partition_point(|w| w.end <= t);
        let now = (at < transcript.words.len() && transcript.words[at].start <= t + 0.05).then_some(at);

        let playing = self.player.is_playing();
        let spacing = ui.spacing().item_spacing.y;
        let row_total = ROW_H + spacing;
        let mut scroll = ScrollArea::vertical().id_salt("transcription").auto_shrink([false, false]);
        if let Some(line) = now.and_then(|n| self.words.line_of(n)) {
            if playing && self.words.last_line != Some(line) {
                let view = ui.available_height();
                scroll = scroll.vertical_scroll_offset((line as f32 * row_total - view / 2.0).max(0.0));
            }
            self.words.last_line = Some(line);
        }

        let mut action: Option<(f64, Interval, bool)> = None;
        let font = FontId::proportional(13.0);
        let view = &self.words;
        scroll.show_rows(ui, ROW_H, view.lines.len(), |ui, rows| {
            for row in rows {
                let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
                let mut x = rect.left();
                for index in view.lines[row].clone() {
                    let token = &view.tokens[index];
                    let word = &transcript.words[index];
                    let cut = view.cut.get(index).copied().unwrap_or(false);
                    let area = Rect::from_min_size(pos2(x - 2.0, rect.top() + 1.0), vec2(token.width + 4.0, ROW_H - 2.0));
                    let response = ui.interact(area, ui.scope_id().with(("mot", index)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                    let accent = theme::kind_color(word.kind);
                    let painter = ui.painter();
                    if now == Some(index) {
                        painter.rect_filled(area, 3, alpha(theme::ACCENT, 0.32));
                    } else if response.hovered() {
                        painter.rect_filled(area, 3, theme::LINE);
                    } else if let Some(color) = accent {
                        painter.rect_filled(area, 3, alpha(color, if cut { 0.22 } else { 0.12 }));
                    }
                    let color = match (accent, cut) {
                        (Some(color), _) => color,
                        (None, true) => alpha(theme::MUTED, 0.6),
                        (None, false) => theme::TEXT,
                    };
                    painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, &token.text, font.clone(), color);
                    if cut {
                        let y = rect.center().y + 1.0;
                        painter.hline(x..=x + token.width, y, Stroke::new(1.0, alpha(color, 0.8)));
                    }
                    if response.clicked() {
                        let alt = ui.input(|i| i.modifiers.alt);
                        action = Some((word.start + offset, (word.start + offset, word.end + offset), alt));
                    }
                    if word.kind != Kind::Word {
                        let probability = word.probability.map(|p| format!(" · confiance {} %", (p * 100.0).round() as i64)).unwrap_or_default();
                        response.on_hover_text(format!("{}{probability}\nAlt+clic : garder / couper", word.text));
                    }
                    x += token.width + GAP;
                }
            }
        });

        if let Some((start, span, alt)) = action {
            if alt {
                self.toggle_at((span.0 + span.1) / 2.0, Some(span));
            } else {
                self.player.seek(start - 0.15);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_read_naturally_in_french() {
        assert_eq!(token_label("[UH]", "fr"), "euh");
        assert_eq!(token_label("[um]", "fr"), "hum");
        assert_eq!(token_label("[uh]", "en"), "uh");
        assert_eq!(token_label("[throat_clearing]", "fr"), "throat clearing");
    }

    #[test]
    fn lines_wrap_on_the_available_width_and_keep_every_token() {
        let mut view = WordsView::default();
        view.tokens = (0..7).map(|i| Token { text: i.to_string(), width: 45.0 }).collect();
        view.wrap(100.0);
        assert_eq!(view.lines, vec![0..2, 2..4, 4..6, 6..7]);
        assert_eq!(view.line_of(5), Some(2));
        assert_eq!(view.line_of(6), Some(3));
    }

    #[test]
    fn a_token_wider_than_the_line_still_gets_its_own_line() {
        let mut view = WordsView::default();
        view.tokens = vec![Token { text: "a".into(), width: 500.0 }, Token { text: "b".into(), width: 10.0 }];
        view.wrap(100.0);
        assert_eq!(view.lines, vec![0..1, 1..2]);
    }
}
