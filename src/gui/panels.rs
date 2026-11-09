//! Barre du haut, liste des rushes et panneau des réglages.

use eframe::egui;
use egui::{vec2, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder};

use crate::cli::duration_text;
use crate::decision::{CutPlan, Reason};
use crate::fps::format_rate;
use crate::probe::MediaInfo;
use crate::settings::Threshold;
use crate::transcript::{Kind, LANGUAGES};

use super::state::{App, CfrStatus, Dialog, ProxyStatus, Status, TrStatus};
use super::theme::{self, alpha, card, hint, primary};
use super::timeline::number;

/// Interrupteur coulissant.
fn toggle_switch(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(32.0, 18.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let progress = ui.ctx().animate_bool_responsive(response.id, *on);
    let fill = theme::LINE_2.lerp_to_gamma(theme::ACCENT, progress);
    let radius = rect.height() / 2.0;
    ui.painter().rect_filled(rect, radius, fill);
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), progress);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, theme::TEXT);
    response
}

fn section_title(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong().size(14.0));
}

struct Slide<'a> {
    label: &'a str,
    tip: &'a str,
    min: f64,
    max: f64,
    step: f64,
    decimals: usize,
    unit: &'a str,
}

fn slide(ui: &mut Ui, value: &mut f64, spec: Slide<'_>) {
    ui.horizontal(|ui| {
        ui.label(spec.label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{} {}", number(*value, spec.decimals), spec.unit)).color(theme::MUTED));
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(egui::Slider::new(value, spec.min..=spec.max).step_by(spec.step).show_value(false));
    ui.add(egui::Label::new(hint(spec.tip)).wrap());
    ui.add_space(3.0);
}

fn small_progress(ui: &mut Ui, text: &str, fraction: f64) {
    ui.label(hint(text));
    ui.add(egui::ProgressBar::new(fraction as f32).desired_height(3.0).fill(theme::ACCENT).corner_radius(2));
}

fn describe_media(media: &MediaInfo) -> String {
    let mut parts = Vec::new();
    if media.has_video {
        parts.push(format!("{}×{}", media.width, media.height));
        if media.fps.is_some() {
            parts.push(format!("{} i/s", format_rate(media.fps)));
        }
    } else {
        parts.push("audio".to_string());
    }
    let channels = media.audio_channels();
    if channels > 0 {
        parts.push(if channels == 1 { "mono".to_string() } else { format!("{channels} canaux") });
    } else {
        parts.push("sans son".to_string());
    }
    if media.start_timecode.is_some() {
        parts.push("TC".to_string());
    }
    parts.join(" · ")
}

impl App {
    pub fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Euhpen").strong().size(16.0).color(theme::ACCENT));
            ui.add_space(14.0);
            ui.label(hint("Preset"));
            let modified = self.preset_modified();
            let title = if modified { format!("{} (modifié)", self.prefs.preset) } else { self.prefs.preset.clone() };
            let mut chosen: Option<String> = None;
            let mut save = false;
            let mut hovered_description = None;
            egui::ComboBox::from_id_salt("preset").selected_text(title).width(210.0).show_ui(ui, |ui| {
                let current = self.prefs.preset.clone();
                for (builtin, heading) in [(true, "Euhpen"), (false, "Mes presets")] {
                    if self.presets.iter().any(|p| p.builtin == builtin) {
                        ui.label(hint(heading));
                        for preset in self.presets.iter().filter(|p| p.builtin == builtin) {
                            let row = ui.selectable_label(preset.name == current, &preset.name);
                            if row.hovered() {
                                hovered_description = Some(preset.description.clone());
                            }
                            if row.clicked() {
                                chosen = Some(preset.name.clone());
                            }
                        }
                    }
                }
                ui.separator();
                if ui.selectable_label(false, "Enregistrer…").clicked() {
                    save = true;
                }
                if let Some(text) = hovered_description.take() {
                    ui.add(egui::Label::new(hint(text)).wrap());
                }
            });
            if let Some(name) = chosen {
                self.apply_preset(&name);
            }
            if save {
                let name = if self.presets.iter().any(|p| p.name == self.prefs.preset && !p.builtin) { self.prefs.preset.clone() } else { String::new() };
                self.dialog = Dialog::SavePreset(name);
            }
            let user_preset = self.presets.iter().any(|p| p.name == self.prefs.preset && !p.builtin);
            if user_preset && ui.small_button("Supprimer").on_hover_text("Supprimer ce preset").clicked() {
                self.delete_preset();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Quitter").clicked() {
                    self.dialog = Dialog::ConfirmQuit;
                }
                if ui.button("Aide").on_hover_text("Raccourcis clavier (?)").clicked() {
                    self.dialog = Dialog::Help;
                }
                if ui.button("Système").clicked() {
                    self.open_system();
                }
            });
        });
    }

    pub fn rushes_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Rushes").strong().size(14.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+ Dossier").clicked() {
                    self.add_folder_dialog();
                }
                if ui.button("+ Fichiers").clicked() {
                    self.add_files_dialog();
                }
            });
        });
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.path_input)
                .hint_text("Coller un chemin puis Entrée")
                .desired_width(f32::INFINITY),
        );
        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let path = std::mem::take(&mut self.path_input);
            self.add_paths(&[path], true);
        }
        ui.add_space(4.0);

        let footer = 62.0;
        let list_height = (ui.available_height() - footer).max(60.0);
        let mut select = None;
        let mut remove = None;
        let mut moved = None;
        let mut retry = None;
        let mut convert = None;
        let mut retry_proxy = None;
        let width = ui.available_width();
        ScrollArea::vertical().id_salt("rushes").max_height(list_height).auto_shrink([false, false]).show(ui, |ui| {
            if self.items.is_empty() {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| ui.label(hint("Glissez vos fichiers ou dossiers dans la fenêtre.")));
            }
            let count = self.items.len();
            for (index, item) in self.items.iter().enumerate() {
                let selected = self.selected == Some(item.id);
                let builder = UiBuilder::new().id_salt(("rush", item.id)).sense(Sense::click());
                let row = ui.scope_builder(builder, |ui| {
                    let (fill, stroke) = if selected {
                        (alpha(theme::ACCENT, 0.10), Stroke::new(1.0, alpha(theme::ACCENT, 0.7)))
                    } else {
                        (theme::PANEL_2, Stroke::new(1.0, theme::LINE))
                    };
                    egui::Frame::new().fill(fill).stroke(stroke).corner_radius(6).inner_margin(7).show(ui, |ui| {
                        ui.set_width(width - 30.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            let name_width = ui.available_width() - 66.0;
                            ui.allocate_ui_with_layout(vec2(name_width.max(30.0), 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(&item.name).strong()).truncate().selectable(false)).on_hover_text(&item.path);
                            });
                            if ui.add_enabled(index > 0, egui::Button::new("↑").small()).clicked() {
                                moved = Some((item.id, -1));
                            }
                            if ui.add_enabled(index + 1 < count, egui::Button::new("↓").small()).clicked() {
                                moved = Some((item.id, 1));
                            }
                            if ui.small_button("×").on_hover_text("Retirer").clicked() {
                                remove = Some(item.id);
                            }
                        });
                        match &item.status {
                            Status::Queued => {
                                ui.label(hint("En attente"));
                            }
                            Status::Analyzing(f) => small_progress(ui, &format!("Analyse {} %", (f * 100.0).round() as i64), *f),
                            Status::Error(message) => {
                                ui.add(egui::Label::new(RichText::new(message).small().color(theme::DANGER)).wrap());
                                if ui.small_button("Réessayer").clicked() {
                                    retry = Some(item.id);
                                }
                            }
                            Status::Ready => {}
                        }
                        let Some(analysis) = &item.analysis else { return };
                        let media = &analysis.media;
                        ui.label(hint(format!("{} · {}", duration_text(media.duration), describe_media(media))));
                        if let Some(stats) = &item.stats {
                            let removed = if stats.duration > 0.0 { (1.0 - stats.kept / stats.duration) * 100.0 } else { 0.0 };
                            let mut text = format!("−{} % → {} · {} coupes", removed.round() as i64, duration_text(stats.kept), stats.cuts);
                            if stats.hesitations > 0 {
                                text += &format!(" · {} euh", stats.hesitations);
                            }
                            ui.label(RichText::new(text).small().color(theme::ACCENT));
                            if stats.low_contrast {
                                ui.label(RichText::new("contraste faible").small().color(theme::WARN));
                            }
                        }
                        match &item.proxy {
                            ProxyStatus::Running(f) => small_progress(ui, &format!("Aperçu {} %", (f * 100.0).round() as i64), *f),
                            ProxyStatus::Error(message) => {
                                ui.add(egui::Label::new(RichText::new(format!("Aperçu : {message}")).small().color(theme::DANGER)).wrap());
                                if ui.small_button("Réessayer").clicked() {
                                    retry_proxy = Some(item.id);
                                }
                            }
                            _ => {}
                        }
                        if let TrStatus::Running(f, label) = &item.tr {
                            let text = if label.is_empty() { format!("Transcription {} %", (f * 100.0).round() as i64) } else { label.clone() };
                            small_progress(ui, &text, *f);
                        } else if item.tr == TrStatus::Queued {
                            ui.label(hint("Transcription en attente"));
                        }
                        if media.vfr {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("fréquence variable").small().color(theme::WARN));
                                match &item.cfr {
                                    CfrStatus::None => {
                                        if ui.small_button("Convertir").on_hover_text("Crée une copie à fréquence constante").clicked() {
                                            convert = Some(item.id);
                                        }
                                    }
                                    CfrStatus::Running => {
                                        ui.spinner();
                                    }
                                    CfrStatus::Done(message) => {
                                        ui.label(hint(message.clone()));
                                    }
                                    CfrStatus::Error(message) => {
                                        ui.label(RichText::new(message).small().color(theme::DANGER));
                                    }
                                }
                            });
                        }
                    });
                });
                if row.response.clicked() {
                    select = Some(item.id);
                }
                ui.add_space(4.0);
            }
        });
        if let Some(id) = select {
            self.select(Some(id));
        }
        if let Some(id) = remove {
            self.remove(id);
        }
        if let Some((id, delta)) = moved {
            self.move_item(id, delta);
        }
        if let Some(id) = retry {
            self.retry_analysis(id);
        }
        if let Some(id) = retry_proxy {
            self.request_proxy(id);
        }
        if let Some(id) = convert {
            self.convert_cfr(id);
        }

        ui.add_space(4.0);
        ui.separator();
        let (mut before, mut after) = (0.0, 0.0);
        for stats in self.items.iter().filter_map(|i| i.stats.as_ref()) {
            before += stats.duration;
            after += stats.kept;
        }
        ui.horizontal(|ui| {
            ui.label(hint(format!("{} fichier{}", self.items.len(), if self.items.len() > 1 { "s" } else { "" })));
            if before > 0.0 {
                ui.label(hint(format!("{} → {}", duration_text(before), duration_text(after))));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.items.is_empty() && ui.small_button("Tout retirer").clicked() {
                    self.dialog = Dialog::ConfirmClear;
                }
            });
        });
    }

    fn result_card(&mut self, ui: &mut Ui, plan: &CutPlan) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let kept = plan.kept_duration();
            let removed = if plan.duration > 0.0 { (1.0 - kept / plan.duration) * 100.0 } else { 0.0 };
            let cuts = self.current().and_then(|i| i.stats).map(|s| s.cuts).unwrap_or(0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(hint("Avant"));
                    ui.label(RichText::new(duration_text(plan.duration)).strong());
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(hint("Après"));
                    ui.label(RichText::new(duration_text(kept)).strong().color(theme::ACCENT));
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(hint("Retiré"));
                    ui.label(RichText::new(format!("−{} %", removed.round() as i64)).strong());
                });
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(hint("Coupes"));
                    ui.label(RichText::new(cuts.to_string()).strong());
                });
            });
            ui.add_space(4.0);
            for reason in [Reason::Silence, Reason::Short, Reason::Hesitation, Reason::Cough, Reason::Laugh, Reason::Breath, Reason::Event, Reason::Manual] {
                let seconds = plan.removed_by(reason);
                if seconds > 0.0 {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(rect, 2, theme::reason_color(reason));
                        let mut text = format!("{} : {}", theme::reason_label(reason), duration_text(seconds));
                        if reason == Reason::Hesitation {
                            text += &format!(" ({})", plan.event_count(Kind::Hesitation));
                        }
                        ui.label(hint(text));
                    });
                }
            }
            let lv = plan.levels;
            let level = |value: Option<f64>| value.map(|db| number(db, 1)).unwrap_or_else(|| "?".to_string());
            ui.label(hint(format!(
                "seuil {} dB · bruit {} dB · parole {} dB",
                number(lv.threshold_db, 1),
                level(lv.noise_floor_db),
                level(lv.speech_db)
            )));
        });
    }

    fn warnings_block(&mut self, ui: &mut Ui, plan: &CutPlan) {
        let vfr = self.current_analysis().is_some_and(|a| a.media.vfr);
        let notes = [
            (vfr, "Fréquence d'images variable : convertissez ce fichier avant d'exporter."),
            (plan.levels.low_contrast(), "Peu d'écart entre le bruit de fond et la parole : vérifiez le résultat."),
            (plan.kept.is_empty(), "Rien n'est gardé : tout est sous le seuil."),
        ];
        for (active, text) in notes {
            if active {
                ui.add(egui::Label::new(RichText::new(text).small().color(theme::WARN)).wrap());
            }
        }
        let corrections = self.current().map(|i| i.overrides.count()).unwrap_or(0);
        if corrections > 0 {
            ui.horizontal(|ui| {
                let plural = if corrections > 1 { "s" } else { "" };
                ui.label(hint(format!("{corrections} correction{plural} à la main")));
                if ui.small_button("Tout annuler").clicked() {
                    self.clear_overrides();
                }
            });
        }
    }

    fn silence_section(&mut self, ui: &mut Ui, plan: Option<&CutPlan>) {
        section_title(ui, "Silences");
        ui.horizontal(|ui| {
            ui.label("Seuil de volume");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let manual = matches!(self.settings.threshold, Threshold::Db(_));
                if ui.selectable_label(manual, "Manuel").clicked() && !manual {
                    let db = plan.map(|p| (p.levels.threshold_db * 2.0).round() / 2.0).unwrap_or(-40.0);
                    self.settings.threshold = Threshold::Db(db.clamp(-80.0, -5.0));
                }
                if ui.selectable_label(!manual, "Auto").clicked() {
                    self.settings.threshold = Threshold::Auto;
                }
            });
        });
        if let Threshold::Db(mut db) = self.settings.threshold {
            ui.spacing_mut().slider_width = ui.available_width() - 70.0;
            if ui.add(egui::Slider::new(&mut db, -80.0..=-5.0).step_by(0.5).suffix(" dB").fixed_decimals(1)).changed() {
                self.settings.threshold = Threshold::Db(db);
            }
        }
        let tip = match (self.settings.threshold, plan) {
            (Threshold::Auto, Some(p)) => format!("Calculé pour chaque fichier d'après son bruit de fond. Ici : {} dBFS.", number(p.levels.threshold_db, 1)),
            (Threshold::Auto, None) => "Calculé pour chaque fichier d'après son bruit de fond.".to_string(),
            _ => "Seuil fixe, le même pour tous les fichiers. Glissez aussi la ligne pointillée sur l'onde.".to_string(),
        };
        ui.add(egui::Label::new(hint(tip)).wrap());
        ui.add_space(3.0);

        let s = &mut self.settings;
        slide(ui, &mut s.min_silence, Slide { label: "Silence minimal à couper", tip: "Les pauses plus courtes sont gardées pour le rythme.", min: 0.1, max: 3.0, step: 0.05, decimals: 2, unit: "s" });
        slide(ui, &mut s.pad_before, Slide { label: "Air avant la parole", tip: "Gardé avant chaque reprise, pour ne pas manger l'attaque des mots.", min: 0.0, max: 0.5, step: 0.01, decimals: 2, unit: "s" });
        slide(ui, &mut s.pad_after, Slide { label: "Air après la parole", tip: "Gardé après chaque fin de phrase.", min: 0.0, max: 0.8, step: 0.01, decimals: 2, unit: "s" });
        egui::CollapsingHeader::new(RichText::new("Réglages avancés").color(theme::MUTED)).id_salt("avances").show(ui, |ui| {
            slide(ui, &mut s.min_keep, Slide { label: "Passage gardé minimal", tip: "Un passage gardé plus court est supprimé.", min: 0.0, max: 1.0, step: 0.01, decimals: 2, unit: "s" });
            slide(ui, &mut s.min_sound, Slide { label: "Bruit isolé ignoré", tip: "Un son plus court (clic, claquement) compte comme du silence.", min: 0.0, max: 0.3, step: 0.01, decimals: 2, unit: "s" });
            slide(ui, &mut s.hysteresis_db, Slide { label: "Hystérésis", tip: "La parole ne s'arrête que sous seuil − hystérésis : pas d'hésitation autour du seuil.", min: 0.0, max: 10.0, step: 0.5, decimals: 1, unit: "dB" });
        });
    }

    fn hesitation_section(&mut self, ui: &mut Ui, plan: Option<&CutPlan>) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Hésitations").strong().size(14.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                toggle_switch(ui, &mut self.settings.hesitations);
            });
        });
        ui.add(egui::Label::new(hint("Détecte les « euh », « hum » et les syllabes étirées grâce à une transcription mot à mot, faite sur votre ordinateur. La détection a été entraînée sur du français.")).wrap());
        if let Some(Err(message)) = &self.hesitations {
            ui.add(egui::Label::new(RichText::new(message).small().color(theme::WARN)).wrap());
        }

        if self.settings.hesitations {
            ui.horizontal(|ui| {
                ui.label("Langue parlée");
                let current = LANGUAGES.iter().find(|(code, _)| *code == self.prefs.language).map(|(_, name)| *name).unwrap_or("?");
                let before = self.prefs.language.clone();
                egui::ComboBox::from_id_salt("langue").selected_text(current).width(130.0).show_ui(ui, |ui| {
                    for (code, name) in LANGUAGES.iter() {
                        ui.selectable_value(&mut self.prefs.language, code.to_string(), *name);
                    }
                });
                if before != self.prefs.language {
                    self.refresh_all_transcripts();
                    self.plan_dirty = true;
                    self.stats_soon();
                }
            });
            let s = &mut self.settings;
            slide(ui, &mut s.event_margin, Slide { label: "Marge autour des hésitations", tip: "Ajoutée de chaque côté de l'hésitation.", min: 0.0, max: 0.2, step: 0.01, decimals: 2, unit: "s" });
            self.transcription_status(ui, plan);
        }
        ui.add(egui::Label::new(hint("Transcription : Whisper (OpenAI, licence MIT). Détection entraînée sur le corpus Rhapsodie (CC BY-NC-SA 3.0 FR) : usage non commercial.")).wrap());
    }

    fn transcription_status(&mut self, ui: &mut Ui, plan: Option<&CutPlan>) {
        let Some(item) = self.current() else { return };
        let id = item.id;
        let status = item.tr.clone();
        let transcript = item.transcript.clone();
        match status {
            TrStatus::Running(f, label) => {
                let text = if label.is_empty() { "Transcription…".to_string() } else { label };
                small_progress(ui, &format!("{text} {} %", (f * 100.0).round() as i64), f);
                if ui.small_button("Annuler").clicked() {
                    self.cancel_transcription(id);
                }
            }
            TrStatus::Queued => {
                ui.horizontal(|ui| {
                    ui.label(hint("En attente…"));
                    if ui.small_button("Annuler").clicked() {
                        self.cancel_transcription(id);
                    }
                });
            }
            TrStatus::Ready => {
                let cut = plan.map(|p| p.event_count(Kind::Hesitation)).unwrap_or(0);
                let found = plan.map(|p| p.events.iter().filter(|e| e.kind == Kind::Hesitation).count()).unwrap_or(0);
                let device = transcript.as_ref().map(|t| t.device.clone()).unwrap_or_default();
                let model = transcript.as_ref().map(|t| t.model.clone()).unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    ui.label(hint(format!("Transcrit avec {model} ({device}) · {found} hésitations, {cut} coupées")));
                    if ui.small_button("Refaire").clicked() {
                        self.retranscribe(id, true);
                    }
                });
            }
            state @ (TrStatus::Cancelled | TrStatus::Off) => {
                ui.horizontal(|ui| {
                    ui.label(hint(if state == TrStatus::Cancelled { "Annulée." } else { "Pas de transcription." }));
                    if ui.small_button("Réessayer").clicked() {
                        self.retranscribe(id, false);
                    }
                });
            }
            TrStatus::Error(message) => {
                ui.add(egui::Label::new(RichText::new(message).small().color(theme::DANGER)).wrap());
                if ui.small_button("Réessayer").clicked() {
                    self.retranscribe(id, false);
                }
            }
            TrStatus::Unavailable(message) => {
                ui.add(egui::Label::new(RichText::new(message).small().color(theme::WARN)).wrap());
            }
        }
    }

    fn export_section(&mut self, ui: &mut Ui) {
        section_title(ui, "Export");
        ui.horizontal(|ui| {
            ui.label("Logiciel");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (key, label) in [("both", "Les deux"), ("resolve", "Resolve"), ("premiere", "Premiere")] {
                    if ui.selectable_label(self.prefs.target == key, label).clicked() {
                        self.prefs.target = key.to_string();
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Séquence");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (key, label) in [("separate", "Une par fichier"), ("combined", "Une seule")] {
                    if ui.selectable_label(self.prefs.export_mode == key, label).clicked() {
                        self.prefs.export_mode = key.to_string();
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Ordre");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (key, label) in [("list", "Liste"), ("date", "Date"), ("name", "Nom")] {
                    if ui.selectable_label(self.prefs.order == key, label).clicked() {
                        self.prefs.order = key.to_string();
                    }
                }
            });
        });
        if self.prefs.export_mode != "separate" {
            ui.add(egui::TextEdit::singleline(&mut self.export_name).hint_text("Nom de la séquence").desired_width(f32::INFINITY));
        }
        ui.horizontal(|ui| {
            ui.label("Dossier");
            let text = self.prefs.output_dir.clone().unwrap_or_else(|| "à côté du premier fichier".to_string());
            ui.add(egui::Label::new(hint(text.clone())).truncate()).on_hover_text(text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.prefs.output_dir.is_some() && ui.small_button("×").clicked() {
                    self.prefs.output_dir = None;
                }
                if ui.small_button("Choisir…").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().set_title("Dossier d'export").pick_folder() {
                        self.prefs.output_dir = Some(folder.to_string_lossy().into_owned());
                    }
                }
            });
        });
        egui::CollapsingHeader::new(RichText::new("Options de compatibilité").color(theme::MUTED)).id_salt("compat").show(ui, |ui| {
            ui.checkbox(&mut self.prefs.zero_timecode, "Timecode à zéro");
            ui.label(hint("Si le logiciel décale les clips par rapport au son."));
            ui.checkbox(&mut self.prefs.raw_paths, "Chemins bruts");
            ui.label(hint("Si le logiciel ne retrouve pas les médias."));
            ui.horizontal(|ui| {
                ui.label("Aperçu");
                let before = self.prefs.proxy_height;
                ui.selectable_value(&mut self.prefs.proxy_height, 540, "540p");
                ui.selectable_value(&mut self.prefs.proxy_height, 720, "720p");
                if before != self.prefs.proxy_height {
                    self.request_all_proxies();
                }
            });
        });
        ui.add_space(4.0);
        let ready = self.ready_count();
        let button = ui.add_enabled(ready > 0, primary("Exporter").min_size(vec2(ui.available_width(), 32.0)));
        if button.clicked() {
            self.request_export();
        }
        let text = match ready {
            0 => "Ajoutez un fichier pour exporter.".to_string(),
            1 => "1 fichier prêt.".to_string(),
            n => format!("{n} fichiers prêts."),
        };
        ui.vertical_centered(|ui| ui.label(hint(text)));
    }

    pub fn settings_panel(&mut self, ui: &mut Ui) {
        let plan = self.plan.clone();
        ScrollArea::vertical().id_salt("reglages").auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            ui.set_width(ui.available_width() - 10.0);
            if let Some(plan) = plan.as_deref() {
                self.result_card(ui, plan);
                self.warnings_block(ui, plan);
            }
            self.silence_section(ui, plan.as_deref());
            ui.separator();
            self.hesitation_section(ui, plan.as_deref());
            ui.separator();
            self.export_section(ui);
            ui.add_space(8.0);
        });
    }
}
