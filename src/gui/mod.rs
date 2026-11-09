//! Interface native (eframe/egui). Aucune page web : tout est dessiné par l'application.

mod dialogs;
mod panels;
mod player;
mod preview;
pub(crate) mod selftest;
mod state;
pub(crate) mod theme;
mod timeline;
mod words;

use eframe::egui;
use egui::{Key, Modifiers};

use selftest::SelfTest;
use state::{accepts_media, App, Dialog};

struct Euhpen {
    app: App,
    selftest: Option<SelfTest>,
}

impl Euhpen {
    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || !matches!(self.app.dialog, Dialog::None) {
            return;
        }
        let app = &mut self.app;
        let shift = ctx.input(|i| i.modifiers.shift);
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key) || i.consume_key(Modifiers::SHIFT, key));
        if pressed(Key::Space) {
            app.player.toggle();
        }
        let step = if shift { 5.0 } else { 1.0 };
        if pressed(Key::ArrowLeft) {
            app.nudge(-step);
        }
        if pressed(Key::ArrowRight) {
            app.nudge(step);
        }
        if pressed(Key::ArrowUp) {
            app.select_relative(-1);
        }
        if pressed(Key::ArrowDown) {
            app.select_relative(1);
        }
        if pressed(Key::N) {
            app.jump_cut(true);
        }
        if pressed(Key::P) {
            app.jump_cut(false);
        }
        if pressed(Key::X) {
            app.toggle_under_playhead();
        }
        if pressed(Key::M) {
            let mode = app.player.mode().next();
            app.set_mode(mode);
        }
        if pressed(Key::Plus) || pressed(Key::Equals) {
            app.zoom_keyboard(1.0 / 1.5);
        }
        if pressed(Key::Minus) {
            app.zoom_keyboard(1.5);
        }
        if pressed(Key::Num0) {
            app.zoom_all();
        }
        if pressed(Key::E) {
            app.request_export();
        }
        if pressed(Key::Questionmark) {
            app.dialog = Dialog::Help;
        }
    }

    fn dropped_files(&mut self, ctx: &egui::Context) {
        let (paths, hovering) = ctx.input(|i| {
            let paths: Vec<String> = i
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path())
                .filter(|path| accepts_media(path))
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            (paths, !i.raw.hovered_files.is_empty())
        });
        self.app.drop_hover = hovering;
        if !paths.is_empty() {
            self.app.add_paths(&paths, true);
        }
    }
}

impl eframe::App for Euhpen {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.update_state();
        self.dropped_files(&ctx);
        self.keyboard(&ctx);
        if let Some(selftest) = self.selftest.as_mut() {
            selftest.tick(&mut self.app, &ctx);
        }

        let app = &mut self.app;
        egui::Panel::top("haut")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 7)))
            .show(ui, |ui| app.top_bar(ui));
        egui::Panel::left("rushes")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(10, 6)))
            .default_size(300.0)
            .size_range(230.0..=460.0)
            .show(ui, |ui| app.rushes_panel(ui));
        egui::Panel::right("reglages")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(12, 6)))
            .default_size(340.0)
            .size_range(290.0..=460.0)
            .show(ui, |ui| app.settings_panel(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(10))
            .show(ui, |ui| {
                let height = ui.available_height();
                let width = ui.available_width();
                let transcript_h = (height * 0.17).clamp(84.0, 220.0);
                let timeline_h = (height * 0.30).clamp(190.0, 330.0);
                let controls_h = 26.0 + 22.0 + 3.0 * ui.spacing().item_spacing.y;
                let preview_h = (height - transcript_h - timeline_h - controls_h - 14.0).max(120.0);

                ui.allocate_ui(egui::vec2(width, preview_h), |ui| app.preview_ui(ui, &ctx));
                ui.add_space(2.0);
                app.transport_ui(ui);
                ui.allocate_ui(egui::vec2(width, timeline_h), |ui| app.timeline_ui(ui));
                ui.horizontal(|ui| app.legend_ui(ui));
                ui.add_space(4.0);
                ui.allocate_ui(egui::vec2(width, ui.available_height()), |ui| app.words_ui(ui));
            });

        app.drop_overlay(&ctx);
        app.dialogs(&ctx);
        app.toasts_ui(&ctx);

        if app.player.is_playing() {
            ctx.request_repaint();
        } else if let Some(wait) = app.next_wakeup() {
            ctx.request_repaint_after(wait);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.app.shutdown();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(theme::BG).to_array()
    }
}

/// Ouvre la fenêtre de l'application et rend la main à sa fermeture.
pub fn run(fresh: bool) -> Result<(), String> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("Euhpen")
        .with_inner_size([1440.0, 880.0])
        .with_min_inner_size([1040.0, 640.0])
        .with_drag_and_drop(true);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "euhpen",
        options,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx);
            let selftest = SelfTest::from_env();
            let app = App::new(&creation.egui_ctx, fresh || selftest.is_some());
            Ok(Box::new(Euhpen { app, selftest }))
        }),
    )
    .map_err(|e| format!("la fenêtre n'a pas pu s'ouvrir : {e}"))
}
