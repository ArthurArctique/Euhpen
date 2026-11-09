//! L'aperçu : image, commandes de lecture.

use eframe::egui;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Response, Sense, Shape, Stroke, TextureOptions, Ui};

use crate::cli::duration_text;
use crate::intervals::{total, union, Interval};

use super::player::Mode;
use super::state::{App, ProxyStatus, Status};
use super::theme::{self, alpha};

#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    Previous,
    Next,
}

fn icon_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 26.0), Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect(rect, 6, visuals.bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
    let c = rect.center();
    let color = theme::TEXT;
    let painter = ui.painter();
    let triangle = |points: [egui::Pos2; 3]| Shape::convex_polygon(points.to_vec(), color, Stroke::NONE);
    match icon {
        Icon::Play => {
            painter.add(triangle([c + vec2(-4.0, -6.0), c + vec2(6.0, 0.0), c + vec2(-4.0, 6.0)]));
        }
        Icon::Pause => {
            painter.rect_filled(Rect::from_center_size(c + vec2(-3.5, 0.0), vec2(3.0, 12.0)), 1, color);
            painter.rect_filled(Rect::from_center_size(c + vec2(3.5, 0.0), vec2(3.0, 12.0)), 1, color);
        }
        Icon::Previous => {
            painter.rect_filled(Rect::from_center_size(c + vec2(-6.0, 0.0), vec2(2.0, 12.0)), 0, color);
            painter.add(triangle([c + vec2(6.0, -6.0), c + vec2(6.0, 6.0), c + vec2(-4.0, 0.0)]));
        }
        Icon::Next => {
            painter.rect_filled(Rect::from_center_size(c + vec2(6.0, 0.0), vec2(2.0, 12.0)), 0, color);
            painter.add(triangle([c + vec2(-6.0, -6.0), c + vec2(4.0, 0.0), c + vec2(-6.0, 6.0)]));
        }
    }
    response.on_hover_text(tip)
}

impl App {
    /// Saute à la coupe suivante ou précédente, un peu avant pour l'entendre arriver.
    pub fn jump_cut(&mut self, forward: bool) {
        let Some(plan) = self.plan.clone() else { return };
        let merged: Vec<Interval> = union(&plan.removed.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>());
        let t = self.player.position();
        let target = if forward {
            merged.iter().find(|c| c.0 > t + 0.05)
        } else {
            merged.iter().rev().find(|c| c.0 < t - 0.3)
        };
        if let Some(cut) = target {
            self.player.seek(cut.0 - 0.6);
        }
    }

    pub fn nudge(&mut self, seconds: f64) {
        self.player.seek(self.player.position() + seconds);
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.player.set_mode(mode);
        self.prefs.play_mode = mode.key().to_string();
    }

    pub fn toggle_under_playhead(&mut self) {
        let t = self.player.position();
        self.toggle_at(t, None);
    }

    fn receive_frame(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.player.take_frame() else { return };
        let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
        match self.texture.as_mut() {
            Some(texture) if texture.size() == [frame.width, frame.height] => texture.set(image, TextureOptions::LINEAR),
            _ => self.texture = Some(ctx.load_texture("apercu", image, TextureOptions::LINEAR)),
        }
    }

    fn overlay_text(&self) -> Option<String> {
        let Some(item) = self.current() else {
            return Some("Aucun fichier\nGlissez des rushes ici, ou utilisez « + Fichiers ».".to_string());
        };
        match &item.status {
            Status::Queued => return Some("En attente d'analyse…".to_string()),
            Status::Analyzing(f) => return Some(format!("Analyse du fichier… {} %", (f * 100.0).round() as i64)),
            Status::Error(message) => return Some(format!("Analyse impossible\n{message}")),
            Status::Ready => {}
        }
        match &item.proxy {
            ProxyStatus::Ready(_) => None,
            ProxyStatus::Running(f) => Some(format!("Préparation de l'aperçu… {} %", (f * 100.0).round() as i64)),
            ProxyStatus::Error(message) => Some(format!("Aperçu indisponible\n{message}")),
            _ => Some("Préparation de l'aperçu…".to_string()),
        }
    }

    pub fn preview_ui(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        self.receive_frame(ctx);
        let size = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        ui.painter().rect_filled(rect, 6, Color32::BLACK);
        if response.clicked() && self.current().is_some() {
            self.player.toggle();
        }

        let font = FontId::proportional(14.0);
        if let Some(text) = self.overlay_text() {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font, theme::MUTED);
            return;
        }
        let has_video = self.current_analysis().is_some_and(|a| a.media.has_video);
        if has_video {
            if let Some(texture) = &self.texture {
                let image = texture.size_vec2();
                let scale = (rect.width() / image.x).min(rect.height() / image.y);
                let area = Rect::from_center_size(rect.center(), image * scale);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(texture.id(), area, uv, Color32::WHITE);
            }
        } else if let Some(item) = self.current() {
            ui.painter().text(rect.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, "Fichier audio", font.clone(), theme::MUTED);
            ui.painter().text(rect.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, &item.name, font, theme::TEXT);
        }
    }

    fn montage_time(&self, t: f64) -> Option<(f64, f64)> {
        let plan = self.plan.as_ref()?;
        let before: Vec<Interval> = plan.kept.iter().filter(|k| k.0 < t).map(|k| (k.0, k.1.min(t))).collect();
        Some((total(&before), plan.kept_duration()))
    }

    pub fn transport_ui(&mut self, ui: &mut Ui) {
        let enabled = self.current().is_some_and(|i| i.analysis.is_some());
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                if icon_button(ui, Icon::Previous, "Coupe précédente (P)").clicked() {
                    self.jump_cut(false);
                }
                let playing = self.player.is_playing();
                let icon = if playing { Icon::Pause } else { Icon::Play };
                if icon_button(ui, icon, "Lecture / pause (Espace)").clicked() {
                    self.player.toggle();
                }
                if icon_button(ui, Icon::Next, "Coupe suivante (N)").clicked() {
                    self.jump_cut(true);
                }

                let t = self.player.position();
                let duration = self.player.duration();
                ui.add_space(6.0);
                let source = format!("{} / {}", duration_text(t), duration_text(duration));
                let montage = if self.player.mode() == Mode::Montage { self.montage_time(t) } else { None };
                match montage {
                    Some((done, length)) => {
                        ui.label(egui::RichText::new(format!("{} / {}", duration_text(done), duration_text(length))).monospace())
                            .on_hover_text(format!("Durée du montage. Dans le fichier : {source}"));
                        // Les boutons à droite occupent environ 330 points.
                        if ui.available_width() > 470.0 {
                            ui.label(theme::hint(format!("fichier {source}")));
                        }
                    }
                    None => {
                        ui.label(egui::RichText::new(source).monospace());
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("+").on_hover_text("Zoom avant (+)").clicked() {
                        self.zoom_keyboard(1.0 / 1.5);
                    }
                    if ui.button("Tout").on_hover_text("Tout afficher (0)").clicked() {
                        self.zoom_all();
                    }
                    if ui.button("−").on_hover_text("Zoom arrière (−)").clicked() {
                        self.zoom_keyboard(1.5);
                    }
                    ui.add_space(10.0);
                    let current = self.player.mode();
                    for (mode, label, tip) in [
                        (Mode::Cuts, "Coupes seules", "N'écouter que ce qui est retiré"),
                        (Mode::Original, "Avant", "Le fichier complet"),
                        (Mode::Montage, "Après", "Le résultat, sans les coupes"),
                    ] {
                        if ui.selectable_label(current == mode, label).on_hover_text(tip).clicked() {
                            self.set_mode(mode);
                        }
                    }
                });
            });
        });
    }

    /// Bande fine indiquant que la lecture suit le son.
    pub fn drop_overlay(&self, ctx: &egui::Context) {
        if !self.drop_hover {
            return;
        }
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("depot")));
        painter.rect_filled(screen, 0, alpha(theme::BG, 0.82));
        painter.rect_stroke(screen.shrink(10.0), 10, Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
        painter.text(screen.center(), Align2::CENTER_CENTER, "Déposez vos rushes", FontId::proportional(22.0), theme::TEXT);
    }
}
