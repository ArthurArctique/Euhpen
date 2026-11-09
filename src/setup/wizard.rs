//! Les pages de l'installateur.
//!
//! Autotest sans intervention : `EUHPEN_SETUP_AUTOTEST=<dossier>` parcourt les pages, prend des
//! captures et écrit `autotest.log`. Choix : `EUHPEN_SETUP_HES`, `EUHPEN_SETUP_WHISPER`,
//! `EUHPEN_SETUP_DATA` (`0` ou `1`), `EUHPEN_SETUP_CANCEL_AFTER` (secondes) et
//! `EUHPEN_SETUP_NO_GPU=1` pour se comporter comme sur une machine sans carte NVIDIA.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Align, Color32, Layout, RichText, Stroke, Ui};

use super::install::{self, Existing, Options, Removal, Report};
use super::job::{human_size, log_path, End, Job, Status};
use super::platform;
use crate::gui::selftest::save_png;
use crate::gui::theme;
use crate::probe::tool_on_path;
use crate::worker::{quick_hardware, Hardware};

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Welcome,
    Components,
    Working,
    Finished,
    Confirm,
    Removed,
}

enum Act {
    Wait(f64),
    Shot(&'static str),
    Next,
    Start,
    WaitEnd,
    Write,
    Close,
}

struct Auto {
    folder: PathBuf,
    script: VecDeque<Act>,
    since: Instant,
    cancel_after: Option<f64>,
}

struct Wizard {
    uninstalling: bool,
    page: Page,
    root: String,
    root_problem: Option<String>,
    existing: Option<Existing>,
    hardware: Hardware,
    system_ffmpeg: bool,
    hesitations: bool,
    whisper: bool,
    desktop: bool,
    remove_data: bool,
    remove_whisper: bool,
    free: Option<u64>,
    job: Option<Job>,
    report: Report,
    launch_error: Option<String>,
    cleanup: Arc<Mutex<Option<PathBuf>>>,
    auto: Option<Auto>,
}

fn switch(variable: &str, default: bool) -> bool {
    std::env::var(variable).map_or(default, |value| value == "1")
}

fn title(text: &str) -> RichText {
    RichText::new(text).size(21.0).strong()
}

/// Bulle d'état d'une étape, dessinée pour ne dépendre d'aucune police.
fn status_icon(ui: &mut Ui, status: Status) {
    const SIZE: f32 = 16.0;
    if status == Status::Running {
        ui.add(egui::Spinner::new().size(SIZE).color(theme::ACCENT));
        return;
    }
    let (rect, _) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::hover());
    let painter = ui.painter();
    let c = rect.center();
    let ink = Stroke::new(1.8, theme::BG);
    match status {
        Status::Done => {
            painter.circle_filled(c, 7.5, theme::ACCENT);
            painter.line_segment([c + egui::vec2(-3.5, 0.2), c + egui::vec2(-1.0, 2.8)], ink);
            painter.line_segment([c + egui::vec2(-1.0, 2.8), c + egui::vec2(3.8, -2.6)], ink);
        }
        Status::Failed => {
            painter.circle_filled(c, 7.5, theme::DANGER);
            painter.line_segment([c + egui::vec2(-3.0, -3.0), c + egui::vec2(3.0, 3.0)], ink);
            painter.line_segment([c + egui::vec2(-3.0, 3.0), c + egui::vec2(3.0, -3.0)], ink);
        }
        _ => {
            painter.circle_stroke(c, 7.0, Stroke::new(1.2, theme::LINE_2));
        }
    }
}

fn message_card(ui: &mut Ui, color: Color32, heading: &str, body: &str) {
    theme::card().stroke(Stroke::new(1.0, theme::alpha(color, 0.6))).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(heading).strong().color(color));
        ui.add(egui::Label::new(body).wrap());
    });
}

fn component(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        body(ui);
    });
    ui.add_space(2.0);
}

impl Wizard {
    fn new(uninstalling: bool, cleanup: Arc<Mutex<Option<PathBuf>>>) -> Self {
        let existing = install::find_existing();
        let root = existing.as_ref().map_or_else(install::default_root, |found| found.root.clone()).display().to_string();
        let auto = std::env::var_os("EUHPEN_SETUP_AUTOTEST").filter(|v| !v.is_empty()).map(|folder| {
            use Act::*;
            let script: Vec<Act> = if uninstalling {
                vec![Wait(0.8), Shot("1-confirmation"), Wait(0.3), Start, WaitEnd, Wait(0.8), Shot("2-fin"), Write, Wait(0.3), Close]
            } else {
                // Une capture part à l'image suivante : on attend avant de changer de page.
                vec![
                    Wait(0.8),
                    Shot("1-dossier"),
                    Wait(0.3),
                    Next,
                    Wait(0.6),
                    Shot("2-composants"),
                    Wait(0.3),
                    Start,
                    Wait(2.0),
                    Shot("3-progression"),
                    WaitEnd,
                    Wait(0.8),
                    Shot("4-fin"),
                    Write,
                    Wait(0.3),
                    Close,
                ]
            };
            let folder = PathBuf::from(folder);
            let _ = std::fs::create_dir_all(&folder);
            let cancel_after = std::env::var("EUHPEN_SETUP_CANCEL_AFTER").ok().and_then(|v| v.parse().ok());
            Auto { folder, script: script.into(), since: Instant::now(), cancel_after }
        });
        let hesitations = switch("EUHPEN_SETUP_HES", existing.as_ref().is_none_or(|found| found.hesitations));
        let mut wizard = Wizard {
            uninstalling,
            page: if uninstalling { Page::Confirm } else { Page::Welcome },
            root,
            root_problem: None,
            existing,
            hardware: if switch("EUHPEN_SETUP_NO_GPU", false) { Hardware::default() } else { quick_hardware() },
            system_ffmpeg: tool_on_path("ffmpeg") && tool_on_path("ffprobe"),
            hesitations,
            whisper: switch("EUHPEN_SETUP_WHISPER", true),
            desktop: true,
            remove_data: switch("EUHPEN_SETUP_DATA", false),
            remove_whisper: switch("EUHPEN_SETUP_DATA", false),
            free: None,
            job: None,
            report: Report::default(),
            launch_error: None,
            cleanup,
            auto,
        };
        wizard.check_root();
        wizard
    }

    fn root_path(&self) -> PathBuf {
        PathBuf::from(self.root.trim())
    }

    fn check_root(&mut self) {
        self.root_problem = install::check_root(&self.root_path()).err();
    }

    fn has_nvidia(&self) -> bool {
        self.hardware.device == "cuda"
    }

    fn options(&self) -> Options {
        let root = self.root_path();
        let bundled = install::has_bundled_ffmpeg(&root);
        Options {
            ffmpeg: !self.system_ffmpeg && !bundled,
            root,
            hesitations: self.hesitations,
            whisper: self.whisper,
            desktop: self.desktop,
            nvidia: self.has_nvidia(),
        }
    }

    fn show_components(&mut self) {
        self.free = platform::free_space(&self.root_path());
        self.page = Page::Components;
    }

    fn start(&mut self, ctx: &egui::Context) {
        let job = if self.uninstalling {
            let Some(found) = self.existing.as_ref() else { return };
            let removal = Removal { root: found.root.clone(), data: self.remove_data, whisper: self.remove_whisper };
            let job = Job::new(ctx.clone(), install::removal_labels(&removal));
            let worker = job.clone();
            let cleanup = self.cleanup.clone();
            thread::spawn(move || {
                if install::uninstall(&worker, &removal) {
                    if let Ok(mut slot) = cleanup.lock() {
                        *slot = Some(removal.root.clone());
                    }
                }
            });
            job
        } else {
            let options = self.options();
            let job = Job::new(ctx.clone(), install::step_labels(&options));
            let worker = job.clone();
            thread::spawn(move || install::install(&worker, &options));
            job
        };
        self.job = Some(job);
        self.page = Page::Working;
    }

    fn poll(&mut self) {
        let Some(job) = self.job.as_ref() else { return };
        let mut state = job.state();
        if matches!(state.end, Some(End::Success(_))) {
            if let Some(End::Success(report)) = state.end.take() {
                self.report = report;
                self.page = if self.uninstalling { Page::Removed } else { Page::Finished };
            }
        }
    }

    fn header(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Euhpen").size(16.0).strong().color(theme::ACCENT));
            ui.label(theme::hint(format!("version {}", crate::VERSION)));
            if self.uninstalling {
                return;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let current = match self.page {
                    Page::Welcome => 0,
                    Page::Components => 1,
                    Page::Working => 2,
                    _ => 3,
                };
                for (index, name) in ["Dossier", "Composants", "Installation", "Terminé"].iter().enumerate().rev() {
                    let color = if index == current { theme::TEXT } else { theme::MUTED };
                    ui.label(RichText::new(*name).small().color(color));
                    if index > 0 {
                        ui.label(RichText::new("›").small().color(theme::LINE_2));
                    }
                }
            });
        });
    }

    fn welcome(&mut self, ui: &mut Ui) {
        ui.label(title("Installer Euhpen"));
        ui.add_space(4.0);
        ui.label("Euhpen coupe les silences et les hésitations de vos rushes, puis exporte une timeline pour Premiere Pro et DaVinci Resolve.");
        ui.add_space(4.0);
        ui.label(theme::hint("Tout est installé dans un seul dossier, sans droits d'administrateur et sans modifier vos autres logiciels."));
        ui.add_space(14.0);
        if !install::has_payload() {
            message_card(ui, theme::DANGER, "Installateur incomplet", "Cet installateur a été compilé sans l'application. Construisez-le avec le script creer-installateur de packaging/.");
            return;
        }
        ui.label(RichText::new("Dossier d'installation").strong());
        ui.horizontal(|ui| {
            let width = ui.available_width() - 96.0;
            if ui.add(egui::TextEdit::singleline(&mut self.root).desired_width(width)).changed() {
                self.check_root();
            }
            if ui.button("Parcourir…").clicked() {
                if let Some(folder) = rfd::FileDialog::new().set_title("Dossier d'installation").pick_folder() {
                    let named = folder.file_name().is_some_and(|name| name.eq_ignore_ascii_case("Euhpen"));
                    self.root = if named { folder } else { folder.join("Euhpen") }.display().to_string();
                    self.check_root();
                }
            }
        });
        ui.add_space(6.0);
        if let Some(problem) = self.root_problem.clone() {
            ui.colored_label(theme::DANGER, problem);
        } else if self.existing.as_ref().is_some_and(|found| found.root == self.root_path()) {
            message_card(ui, theme::ACCENT, "Euhpen est déjà installé ici", "L'application sera mise à jour ou réparée. Vos réglages, vos presets et le modèle déjà téléchargé sont conservés.");
        }
    }

    fn components(&mut self, ui: &mut Ui) {
        ui.label(title("Ce qui va être installé"));
        ui.add_space(8.0);
        let options = self.options();
        component(ui, |ui| {
            ui.label(RichText::new("Euhpen").strong());
            ui.label(theme::hint("L'application, la ligne de commande et le calcul des coupes."));
        });
        component(ui, |ui| {
            ui.label(RichText::new("ffmpeg").strong());
            let text = if options.ffmpeg { &format!("Sera téléchargé ({}). Sert à lire le son et l'image de vos rushes.", platform::FFMPEG_DOWNLOAD) } else { "Déjà présent sur cet ordinateur." };
            ui.label(theme::hint(text));
        });
        component(ui, |ui| {
            ui.checkbox(&mut self.hesitations, RichText::new("Détection des « euh »").strong());
            ui.label(theme::hint("Repère les hésitations avec Whisper (OpenAI) et une tête de détection entraînée sur le corpus Rhapsodie. Sans elle, Euhpen coupe les silences normalement."));
            ui.label(RichText::new("Licence de la tête : CC BY-NC-SA 3.0 FR, pas d'usage commercial. En cochant cette case, vous acceptez cette condition.").small().color(theme::WARN));
            if !self.hesitations {
                return;
            }
            ui.add_space(4.0);
            if self.has_nvidia() {
                let memory = self.hardware.memory_gb.map_or(String::new(), |gb| format!(", {gb} Go").replace('.', ","));
                ui.label(RichText::new(format!("Carte détectée : {}{memory}. PyTorch sera installé pour CUDA.", self.hardware.name)).small().color(theme::ACCENT));
            } else if self.hardware.device == "mps" {
                ui.label(RichText::new("Puce Apple détectée : le modèle utilisera sa partie graphique (Metal).").small().color(theme::ACCENT));
            } else {
                ui.label(theme::hint("Aucune carte NVIDIA détectée : le modèle tournera sur le processeur, plus lentement."));
            }
            ui.checkbox(&mut self.whisper, format!("Télécharger Whisper maintenant ({})", human_size(install::WHISPER_SIZE)));
            if !self.whisper {
                ui.label(theme::hint("Il sera téléchargé à la première détection, ce qui demandera d'attendre et d'être connecté."));
            }
        });
        ui.checkbox(&mut self.desktop, "Créer un raccourci sur le Bureau");
        ui.add_space(6.0);
        let needed = install::required_space(&self.options());
        let room = match self.free {
            Some(free) => format!("Espace nécessaire : environ {}. Disponible : {}.", human_size(needed), human_size(free)),
            None => format!("Espace nécessaire : environ {}.", human_size(needed)),
        };
        ui.label(theme::hint(room));
        if self.not_enough_space() {
            ui.colored_label(theme::DANGER, "Il n'y a pas assez de place sur ce disque.");
        }
    }

    fn not_enough_space(&self) -> bool {
        self.free.is_some_and(|free| free < install::required_space(&self.options()))
    }

    fn working(&mut self, ui: &mut Ui) {
        let Some(job) = self.job.clone() else { return };
        let state = job.state();
        let doing = if self.uninstalling { "Désinstallation" } else { "Installation" };
        match &state.end {
            None if job.cancelling() => {
                ui.label(title("Annulation…"));
            }
            None => {
                ui.label(title(&format!("{doing} en cours")));
                if !self.uninstalling {
                    ui.label(theme::hint("Comptez de quelques minutes à une demi-heure selon votre connexion. Vous pouvez utiliser l'ordinateur en attendant."));
                }
            }
            Some(End::Failure(message)) => {
                ui.label(title(&format!("{doing} interrompue")));
                ui.add_space(6.0);
                message_card(ui, theme::DANGER, "Une étape a échoué", message);
                if !self.uninstalling {
                    ui.label(theme::hint("Ce qui est déjà installé est conservé : réessayer reprend là où ça s'est arrêté."));
                }
            }
            Some(End::Cancelled) => {
                ui.label(title(&format!("{doing} annulée")));
                ui.label(theme::hint("Rien n'est perdu : vous pouvez reprendre, ou relancer l'installateur plus tard."));
            }
            Some(End::Success(_)) => {}
        }
        ui.add_space(10.0);
        for step in &state.steps {
            ui.horizontal(|ui| {
                status_icon(ui, step.status);
                let text = RichText::new(&step.label);
                ui.label(if step.status == Status::Pending { text.color(theme::MUTED) } else { text });
                if step.status == Status::Running && !state.detail.is_empty() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(theme::hint(state.detail.as_str())));
                }
            });
            if step.status == Status::Running {
                if let Some(fraction) = state.fraction {
                    ui.add(egui::ProgressBar::new(fraction).desired_height(4.0).fill(theme::ACCENT).corner_radius(2));
                }
            }
        }
        ui.add_space(10.0);
        egui::CollapsingHeader::new(RichText::new("Détails").color(theme::MUTED)).id_salt("details").show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(150.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                for line in &state.log {
                    ui.label(RichText::new(line).small().monospace().color(theme::MUTED));
                }
            });
        });
    }

    fn finished(&mut self, ui: &mut Ui) {
        ui.label(title("Euhpen est installé"));
        ui.add_space(10.0);
        let report = self.report.clone();
        if !report.hesitations {
            message_card(ui, theme::MUTED, "Coupe des silences prête", "La détection des « euh » n'est pas installée. Relancez l'installateur pour l'ajouter.");
        } else if let Some(card) = &report.cuda {
            message_card(ui, theme::ACCENT, "La carte graphique sera utilisée", &format!("{card} répond à PyTorch {} : la transcription passera par CUDA.", report.torch.as_deref().unwrap_or("")));
        } else if report.mps {
            message_card(ui, theme::ACCENT, "La puce graphique sera utilisée", &format!("PyTorch {} répond sur la partie graphique de la puce Apple (Metal).", report.torch.as_deref().unwrap_or("")));
        } else if !self.has_nvidia() && !cfg!(target_os = "macos") {
            message_card(ui, theme::MUTED, "Calcul sur le processeur", "La détection des « euh » fonctionne sans carte NVIDIA, mais elle est plus lente.");
        }
        if report.hesitations && !self.whisper {
            ui.add_space(4.0);
            ui.label(theme::hint(format!("Whisper ({}) sera téléchargé à la première détection.", human_size(install::WHISPER_SIZE))));
        }
        for warning in &report.warnings {
            ui.add_space(4.0);
            message_card(ui, theme::WARN, "À savoir", warning);
        }
        ui.add_space(8.0);
        ui.label(theme::hint(format!("Installé dans {}.", self.root.trim())));
        ui.label(theme::hint(platform::UNINSTALL_HINT));
        if let Some(problem) = &self.launch_error {
            ui.colored_label(theme::DANGER, problem);
        }
    }

    fn confirm(&mut self, ui: &mut Ui) {
        ui.label(title("Désinstaller Euhpen"));
        ui.add_space(8.0);
        let Some(found) = self.existing.as_ref() else {
            ui.label("Aucune installation d'Euhpen n'a été trouvée sur cet ordinateur.");
            return;
        };
        ui.label(format!("L'application et ses composants seront retirés de {}.", found.root.display()));
        ui.label(theme::hint("Vos rushes et les timelines exportées ne sont jamais touchés."));
        ui.add_space(10.0);
        ui.checkbox(&mut self.remove_data, "Supprimer aussi mes réglages, mes presets et le cache");
        ui.checkbox(&mut self.remove_whisper, format!("Supprimer aussi le modèle Whisper téléchargé ({})", human_size(install::WHISPER_SIZE)));
    }

    fn removed(&mut self, ui: &mut Ui) {
        ui.label(title("Euhpen est désinstallé"));
        ui.add_space(8.0);
        ui.label("Tous les composants choisis ont été retirés.");
    }

    fn body(&mut self, ui: &mut Ui) {
        match self.page {
            Page::Welcome => self.welcome(ui),
            Page::Components => self.components(ui),
            Page::Working => self.working(ui),
            Page::Finished => self.finished(ui),
            Page::Confirm => self.confirm(ui),
            Page::Removed => self.removed(ui),
        }
    }

    fn actions(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let close = |ctx: &egui::Context| ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match self.page {
            Page::Welcome => {
                let ready = self.root_problem.is_none() && install::has_payload();
                if ui.add_enabled(ready, theme::primary("Suivant")).clicked() {
                    self.show_components();
                }
                if ui.button("Annuler").clicked() {
                    close(ctx);
                }
            }
            Page::Components => {
                if ui.add_enabled(!self.not_enough_space(), theme::primary("Installer")).clicked() {
                    self.start(ctx);
                }
                if ui.button("Retour").clicked() {
                    self.page = Page::Welcome;
                }
            }
            Page::Working => {
                let Some(job) = self.job.clone() else { return };
                let ended = job.state().end.is_some();
                if ended {
                    let verb = if job.state().steps.iter().any(|s| s.status == Status::Failed) { "Réessayer" } else { "Reprendre" };
                    if !self.uninstalling && ui.add(theme::primary(verb)).clicked() {
                        self.start(ctx);
                    }
                    if ui.button("Ouvrir le journal").clicked() {
                        platform::open_log(&log_path());
                    }
                    if ui.button("Fermer").clicked() {
                        close(ctx);
                    }
                } else if ui.add_enabled(!job.cancelling(), egui::Button::new("Annuler")).clicked() {
                    job.request_cancel();
                }
            }
            Page::Finished => {
                if ui.add(theme::primary("Ouvrir Euhpen")).clicked() {
                    match platform::launch(&self.root_path()) {
                        Ok(()) => close(ctx),
                        Err(problem) => self.launch_error = Some(problem),
                    }
                }
                if ui.button("Fermer").clicked() {
                    close(ctx);
                }
            }
            Page::Confirm => {
                if self.existing.is_some() && ui.add(theme::primary("Désinstaller")).clicked() {
                    self.start(ctx);
                }
                if ui.button("Annuler").clicked() {
                    close(ctx);
                }
            }
            Page::Removed => {
                if ui.add(theme::primary("Fermer")).clicked() {
                    close(ctx);
                }
            }
        });
    }

    fn write_report(&self, folder: &Path) {
        let mut text = String::new();
        let _ = writeln!(text, "racine={}", self.root.trim());
        if self.page == Page::Finished || self.page == Page::Removed {
            let _ = writeln!(text, "resultat=reussite");
        }
        if let Some(job) = &self.job {
            let state = job.state();
            for step in &state.steps {
                let status = match step.status {
                    Status::Pending => "attente",
                    Status::Running => "en cours",
                    Status::Done => "fait",
                    Status::Failed => "echec",
                };
                let _ = writeln!(text, "etape {status} : {}", step.label);
            }
            match &state.end {
                Some(End::Failure(message)) => {
                    let _ = writeln!(text, "resultat=echec\n{message}");
                }
                Some(End::Cancelled) => {
                    let _ = writeln!(text, "resultat=annule");
                }
                _ => {}
            }
        }
        let _ = writeln!(text, "cuda={}", self.report.cuda.as_deref().unwrap_or("non"));
        let _ = writeln!(text, "mps={}", self.report.mps);
        let _ = writeln!(text, "torch={}", self.report.torch.as_deref().unwrap_or("non"));
        for warning in &self.report.warnings {
            let _ = writeln!(text, "avertissement={warning}");
        }
        let _ = std::fs::write(folder.join("autotest.log"), text);
    }

    fn auto_tick(&mut self, ctx: &egui::Context) {
        let Some(mut auto) = self.auto.take() else { return };
        ctx.request_repaint_after(Duration::from_millis(30));
        let elapsed = auto.since.elapsed().as_secs_f64();
        let mut done = true;
        match auto.script.front() {
            Some(Act::Wait(seconds)) => done = elapsed >= *seconds,
            Some(Act::Shot(name)) => {
                let path = auto.folder.join(format!("{name}.png"));
                ctx.request_screenshot(move |image| {
                    let _ = save_png(&path, image.size[0], image.size[1], image.as_raw());
                });
            }
            Some(Act::Next) => self.show_components(),
            Some(Act::Start) => self.start(ctx),
            Some(Act::WaitEnd) => {
                let ended = self.page != Page::Working || self.job.as_ref().is_some_and(|job| job.state().end.is_some());
                if let (Some(limit), Some(job)) = (auto.cancel_after, self.job.as_ref()) {
                    if elapsed >= limit {
                        job.request_cancel();
                    }
                }
                done = ended;
            }
            Some(Act::Write) => self.write_report(&auto.folder),
            Some(Act::Close) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            None => {}
        }
        if done {
            auto.script.pop_front();
            auto.since = Instant::now();
        }
        self.auto = Some(auto);
    }
}

impl eframe::App for Wizard {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.auto_tick(&ctx);
        if self.page == Page::Working {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        egui::Panel::top("titre")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(18, 12)))
            .show(ui, |ui| self.header(ui));
        egui::Panel::bottom("actions")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(18, 12)))
            .show(ui, |ui| self.actions(ui, &ctx));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG).inner_margin(22)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.body(ui));
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Fermer la fenêtre pendant l'installation revient à l'annuler : les processus sont arrêtés.
        let Some(job) = self.job.as_ref() else { return };
        job.request_cancel();
        let since = Instant::now();
        while job.state().end.is_none() && since.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(theme::BG).to_array()
    }
}

/// Ouvre la fenêtre, puis finit de nettoyer après une désinstallation.
pub fn run(uninstalling: bool) -> Result<(), String> {
    let cleanup = Arc::new(Mutex::new(None));
    let to_clean = cleanup.clone();
    let name = if uninstalling { "Désinstaller Euhpen" } else { "Installer Euhpen" };
    let viewport = egui::ViewportBuilder::default().with_title(name).with_inner_size([720.0, 640.0]).with_min_inner_size([620.0, 520.0]);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "euhpen-setup",
        options,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx);
            Ok(Box::new(Wizard::new(uninstalling, cleanup)))
        }),
    )
    .map_err(|e| format!("la fenêtre n'a pas pu s'ouvrir : {e}"))?;
    if let Some(root) = to_clean.lock().ok().and_then(|mut slot| slot.take()) {
        platform::delete_after_exit(&root);
    }
    Ok(())
}
