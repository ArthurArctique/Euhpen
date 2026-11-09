//! Fenêtres modales et messages temporaires.

use std::path::Path;
use std::time::Instant;

use eframe::egui;
use egui::{vec2, RichText, Ui};

use crate::cache::cache_dir;
use crate::cli::duration_text;
use crate::config::config_dir;
use crate::files::reveal;
use crate::transcript::{local_model_path, HEAD_ATTRIBUTION, HEAD_LICENSE, HEAD_LICENSE_URL, MODEL_ATTRIBUTION, MODEL_LABEL};
use crate::worker::worker_log_path;

use super::state::{App, Dialog, ExportResult, ToastKind};
use super::theme::{self, hint, primary};

const KEYS: [(&str, &str); 9] = [
    ("Espace", "lecture / pause"),
    ("← →", "reculer / avancer d'1 s (Maj : 5 s)"),
    ("N  P", "coupe suivante / précédente"),
    ("X", "garder ou couper le passage sous la tête de lecture"),
    ("M", "mode de lecture : après / avant / coupes seules"),
    ("+  −  0", "zoom avant / arrière / tout afficher"),
    ("↑ ↓", "fichier précédent / suivant"),
    ("E", "exporter"),
    ("?", "cette aide"),
];

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["o", "Ko", "Mo", "Go"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if unit == 0 || value >= 100.0 { 0 } else { 1 };
    format!("{value:.decimals$} {}", UNITS[unit]).replace('.', ",")
}

/// Boutons alignés à droite, sans prendre toute la hauteur du dialogue.
fn button_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), add);
}

fn path_row(ui: &mut Ui, label: &str, path: &Path) {
    ui.horizontal(|ui| {
        ui.label(hint(label));
        button_row(ui, |ui| {
            if ui.small_button("Ouvrir").clicked() {
                reveal(path);
            }
            ui.add(egui::Label::new(RichText::new(path.display().to_string()).small()).truncate());
        });
    });
}

impl App {
    pub fn dialogs(&mut self, ctx: &egui::Context) {
        let mut dialog = std::mem::replace(&mut self.dialog, Dialog::None);
        if matches!(dialog, Dialog::None) {
            return;
        }
        let frame = egui::Frame::new().fill(theme::PANEL).stroke(egui::Stroke::new(1.0, theme::LINE_2)).corner_radius(10).inner_margin(18);
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("dialogue")).frame(frame).show(ctx, |ui| match &mut dialog {
            Dialog::None => {}
            Dialog::Export(result) => close = self.export_dialog(ui, result),
            Dialog::ExportConfirm(count) => close = self.export_confirm(ui, *count),
            Dialog::System => close = self.system_dialog(ui),
            Dialog::Help => close = help_dialog(ui),
            Dialog::SavePreset(name) => close = self.preset_dialog(ui, name),
            Dialog::ConfirmQuit => close = self.confirm(ui, "Quitter Euhpen ?", "Vos fichiers, réglages et corrections sont conservés pour la prochaine fois.", "Quitter", Confirmed::Quit, ctx),
            Dialog::ConfirmClear => close = self.confirm(ui, "Tout retirer ?", "La liste des rushes sera vidée. Les fichiers eux-mêmes ne sont pas touchés.", "Tout retirer", Confirmed::Clear, ctx),
            Dialog::ConfirmClearCache => close = self.confirm(ui, "Vider le cache ?", "Les analyses, aperçus et transcriptions seront refaits au besoin. Vos corrections manuelles sont aussi dans ce cache et seront perdues.", "Vider", Confirmed::ClearCache, ctx),
        });
        if modal.should_close() {
            close = true;
        }
        // Un bouton a pu ouvrir un autre dialogue pendant l'affichage.
        if !close && matches!(self.dialog, Dialog::None) {
            self.dialog = dialog;
        }
    }

    fn confirm(&mut self, ui: &mut Ui, title: &str, text: &str, action: &str, what: Confirmed, ctx: &egui::Context) -> bool {
        ui.set_width(360.0);
        ui.heading(title);
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(text).color(theme::MUTED)).wrap());
        ui.add_space(12.0);
        let mut close = false;
        button_row(ui, |ui| {
            if ui.add(primary(action)).clicked() {
                close = true;
                match what {
                    Confirmed::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Confirmed::Clear => self.clear_all(),
                    Confirmed::ClearCache => self.clear_cache(),
                }
            }
            if ui.button("Annuler").clicked() {
                close = true;
            }
        });
        close
    }

    fn export_confirm(&mut self, ui: &mut Ui, count: usize) -> bool {
        ui.set_width(380.0);
        ui.heading("Transcriptions pas prêtes");
        ui.add_space(4.0);
        let text = if count == 1 {
            "Un fichier n'a pas encore de transcription : ses hésitations ne seront pas coupées.".to_string()
        } else {
            format!("{count} fichiers n'ont pas encore de transcription : leurs hésitations ne seront pas coupées.")
        };
        ui.add(egui::Label::new(RichText::new(text).color(theme::MUTED)).wrap());
        ui.add_space(12.0);
        let mut close = false;
        button_row(ui, |ui| {
            if ui.add(primary("Exporter quand même")).clicked() {
                close = true;
                self.run_export();
            }
            if ui.button("Attendre").clicked() {
                close = true;
            }
        });
        close
    }

    fn export_dialog(&mut self, ui: &mut Ui, result: &ExportResult) -> bool {
        ui.set_width(560.0);
        ui.heading("Export terminé");
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            for file in &result.files {
                theme::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(file.label).strong());
                    ui.add(egui::Label::new(RichText::new(&file.path).small().color(theme::MUTED)).wrap());
                    ui.horizontal(|ui| {
                        ui.label(hint(&file.sequence));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Copier").clicked() {
                                ui.ctx().copy_text(file.path.clone());
                            }
                            if ui.small_button("Afficher").clicked() {
                                reveal(Path::new(&file.path));
                            }
                        });
                    });
                });
                ui.add_space(3.0);
            }
            ui.add_space(4.0);
            for sequence in &result.sequences {
                ui.label(hint(format!(
                    "{} : {} i/s, {}, {} (source {}), {} clips",
                    sequence.name,
                    sequence.fps,
                    sequence.size,
                    duration_text(sequence.duration),
                    duration_text(sequence.source),
                    sequence.clips
                )));
            }
            for warning in &result.warnings {
                ui.add(egui::Label::new(RichText::new(warning).small().color(theme::WARN)).wrap());
            }
            ui.add_space(4.0);
            ui.label(hint("Premiere Pro : Fichier > Importer, choisissez le fichier .premiere.xml."));
            ui.label(hint("DaVinci Resolve : Fichier > Importer > Timeline, fichier .resolve.xml."));
        });
        ui.add_space(10.0);
        let mut close = false;
        button_row(ui, |ui| {
            close = ui.add(primary("Fermer")).clicked();
        });
        close
    }

    fn preset_dialog(&mut self, ui: &mut Ui, name: &mut String) -> bool {
        ui.set_width(340.0);
        ui.heading("Enregistrer le preset");
        ui.add_space(6.0);
        let field = ui.add(egui::TextEdit::singleline(name).char_limit(60).hint_text("Nom du preset").desired_width(f32::INFINITY));
        field.request_focus();
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(10.0);
        let mut close = false;
        button_row(ui, |ui| {
            if ui.add_enabled(!name.trim().is_empty(), primary("Enregistrer")).clicked() || (enter && !name.trim().is_empty()) {
                close = self.save_preset(name);
            }
            if ui.button("Annuler").clicked() {
                close = true;
            }
        });
        close
    }

    fn system_dialog(&mut self, ui: &mut Ui) -> bool {
        ui.set_width(600.0);
        ui.heading("Système");
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(470.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.label(format!("Euhpen {}", crate::VERSION));
            ui.add_space(4.0);
            for (tool, status) in self.tools_status() {
                match status {
                    Ok(path) => ui.label(hint(format!("{tool} : {path}"))),
                    Err(message) => ui.label(RichText::new(format!("{tool} : {message}")).small().color(theme::DANGER)),
                };
            }
            match &self.hesitations {
                None => {
                    ui.label(hint("Hésitations : vérification…"));
                }
                Some(Ok(())) => {
                    ui.label(hint("Hésitations : installées"));
                }
                Some(Err(message)) => {
                    ui.add(egui::Label::new(RichText::new(message).small().color(theme::WARN)).wrap());
                }
            }
            ui.separator();
            ui.label(RichText::new("Calcul").strong());
            let exact = self.hardware.is_some();
            match self.hardware.as_ref().or(self.quick_hardware.as_ref()) {
                Some(hw) => {
                    let memory = hw.memory_gb.map(|m| format!(" · {} Go", m.to_string().replace('.', ","))).unwrap_or_default();
                    let suffix = if exact { "" } else { " (estimation, confirmée à la première transcription)" };
                    ui.label(format!("{} · {}{memory}{suffix}", hw.device, hw.name));
                    if let Some(version) = &hw.torch_version {
                        ui.label(hint(format!("PyTorch {version} · {}", hw.compute_type)));
                    }
                    for warning in &hw.warnings {
                        ui.add(egui::Label::new(RichText::new(warning).small().color(theme::WARN)).wrap());
                    }
                    if let Some(error) = &hw.error {
                        ui.add(egui::Label::new(RichText::new(error).small().color(theme::DANGER)).wrap());
                    }
                }
                None => {
                    ui.label(hint("Détection…"));
                }
            }
            if let Some((device, memory)) = &self.last_run {
                let device = device.clone().unwrap_or_else(|| "inconnu".to_string());
                let memory = memory.map(|m| format!(", {} Mo de mémoire graphique au maximum", m.round() as i64)).unwrap_or_default();
                ui.label(RichText::new(format!("Dernière transcription : {device}{memory}")).color(theme::ACCENT));
            }
            ui.label(hint(match self.player.device_name.as_deref() {
                Some(name) => format!("Sortie son : {name}"),
                None => "Sortie son : aucune (aperçu muet)".to_string(),
            }));
            ui.label(hint(if local_model_path().is_some() { format!("{MODEL_LABEL} : téléchargé") } else { format!("{MODEL_LABEL} : téléchargé à la première utilisation") }));
            ui.separator();
            ui.label(RichText::new("Dossiers").strong());
            ui.horizontal(|ui| {
                let size = self.cache_size.map(format_size).unwrap_or_else(|| "…".to_string());
                ui.label(hint(format!("Taille du cache : {size}")));
                if ui.small_button("Vider").clicked() {
                    self.dialog = Dialog::ConfirmClearCache;
                }
            });
            path_row(ui, "Cache", &cache_dir());
            path_row(ui, "Réglages", &config_dir());
            path_row(ui, "Journal", &worker_log_path());
            ui.separator();
            ui.label(RichText::new("Crédits").strong());
            ui.add(egui::Label::new(hint(MODEL_ATTRIBUTION)).wrap());
            ui.add(egui::Label::new(hint(HEAD_ATTRIBUTION)).wrap());
            ui.hyperlink_to(RichText::new(HEAD_LICENSE).small(), HEAD_LICENSE_URL);
        });
        ui.add_space(10.0);
        let mut close = false;
        button_row(ui, |ui| {
            close = ui.add(primary("Fermer")).clicked();
        });
        close
    }

    pub fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::unique("messages"))
            .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-14.0, -14.0))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                for toast in &self.toasts {
                    let color = match toast.kind {
                        ToastKind::Info => theme::ACCENT,
                        ToastKind::Warning => theme::WARN,
                        ToastKind::Error => theme::DANGER,
                    };
                    egui::Frame::new()
                        .fill(theme::PANEL_2)
                        .stroke(egui::Stroke::new(1.0, color))
                        .corner_radius(8)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(toast.text.as_str()).wrap());
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

#[derive(Clone, Copy)]
enum Confirmed {
    Quit,
    Clear,
    ClearCache,
}

fn help_dialog(ui: &mut Ui) -> bool {
    ui.set_width(700.0);
    ui.heading("Aide");
    ui.add_space(6.0);
    let left = egui::Layout::top_down(egui::Align::LEFT);
    ui.columns(2, |columns| {
        columns[0].with_layout(left, |ui| {
            ui.label(RichText::new("Raccourcis").strong());
            egui::Grid::new("raccourcis").num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
                for (key, text) in KEYS {
                    ui.label(RichText::new(key).monospace().color(theme::ACCENT));
                    ui.add(egui::Label::new(hint(text)).wrap());
                    ui.end_row();
                }
            });
        });
        columns[1].with_layout(left, |ui| {
            ui.label(RichText::new("La forme d'onde").strong());
            for text in [
                "Vert : gardé. Gris : silence coupé. Orange : hésitation coupée. Rouge : coupé à la main.",
                "Cliquez sur la bande colorée du haut pour inverser un passage, sur l'onde pour déplacer la tête de lecture. Glissez pour vous déplacer, molette pour zoomer. En seuil manuel, la ligne pointillée se déplace à la souris.",
                "Dans la transcription, cliquez sur un mot pour y aller ; Alt+clic le garde ou le coupe.",
            ] {
                ui.add(egui::Label::new(hint(text)).wrap());
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Dans le logiciel de montage").strong());
            for text in [
                "Premiere Pro : Fichier > Importer, choisissez le fichier .premiere.xml.",
                "DaVinci Resolve : Fichier > Importer > Timeline, fichier .resolve.xml.",
                "Vos corrections manuelles sont gardées pour chaque fichier, même après fermeture.",
            ] {
                ui.add(egui::Label::new(hint(text)).wrap());
            }
        });
    });
    ui.add_space(10.0);
    let mut close = false;
    button_row(ui, |ui| {
        close = ui.add(primary("Fermer")).clicked();
    });
    close
}

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn sizes_are_readable() {
        assert_eq!(format_size(512), "512 o");
        assert_eq!(format_size(1536), "1,5 Ko");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5,0 Go");
    }
}
