//! Couleurs et réglages visuels de l'interface.

use std::sync::Arc;

use eframe::egui;
use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};

use crate::decision::Reason;
use crate::transcript::Kind;

pub const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x14);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x19, 0x1e);
pub const PANEL_2: Color32 = Color32::from_rgb(0x1c, 0x20, 0x26);
pub const LINE: Color32 = Color32::from_rgb(0x26, 0x2b, 0x33);
pub const LINE_2: Color32 = Color32::from_rgb(0x32, 0x38, 0x44);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xeb);
pub const MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa1);
pub const ACCENT: Color32 = Color32::from_rgb(0x3e, 0xcf, 0x8e);
pub const ACCENT_INK: Color32 = Color32::from_rgb(0x06, 0x2b, 0x1b);
pub const KEPT: Color32 = ACCENT;
pub const SILENCE: Color32 = Color32::from_rgb(0x59, 0x60, 0x6d);
pub const SHORT: Color32 = Color32::from_rgb(0x47, 0x4d, 0x58);
pub const HESITATION: Color32 = Color32::from_rgb(0xff, 0x9f, 0x43);
pub const COUGH: Color32 = Color32::from_rgb(0xec, 0x6f, 0x9d);
pub const LAUGH: Color32 = Color32::from_rgb(0xa7, 0x8b, 0xfa);
pub const BREATH: Color32 = Color32::from_rgb(0x4f, 0xc3, 0xf7);
pub const EVENT: Color32 = Color32::from_rgb(0xf6, 0xc3, 0x43);
pub const MANUAL: Color32 = Color32::from_rgb(0xef, 0x53, 0x50);
pub const DANGER: Color32 = MANUAL;
pub const WARN: Color32 = EVENT;

pub fn reason_color(reason: Reason) -> Color32 {
    match reason {
        Reason::Silence => SILENCE,
        Reason::Short => SHORT,
        Reason::Hesitation => HESITATION,
        Reason::Cough => COUGH,
        Reason::Laugh => LAUGH,
        Reason::Breath => BREATH,
        Reason::Event => EVENT,
        Reason::Manual => MANUAL,
    }
}

pub fn reason_label(reason: Reason) -> &'static str {
    match reason {
        Reason::Silence => "silence",
        Reason::Short => "passage trop court",
        Reason::Hesitation => "hésitation",
        Reason::Cough => "toux",
        Reason::Laugh => "rire",
        Reason::Breath => "respiration",
        Reason::Event => "bruit",
        Reason::Manual => "coupé à la main",
    }
}

pub fn kind_color(kind: Kind) -> Option<Color32> {
    match kind {
        Kind::Word => None,
        Kind::Hesitation => Some(HESITATION),
        Kind::Cough => Some(COUGH),
        Kind::Laugh => Some(LAUGH),
        Kind::Breath => Some(BREATH),
        Kind::Event => Some(EVENT),
    }
}

/// Une couleur rendue translucide (coefficient de 0 à 1).
pub fn alpha(color: Color32, amount: f32) -> Color32 {
    let a = (amount.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// La police système sert aux textes : elle couvre tous les signes utilisés (« − », « … », « × »).
fn system_font() -> Option<Vec<u8>> {
    let candidates: &[&str] = if cfg!(windows) {
        &[r"C:\Windows\Fonts\segoeui.ttf", r"C:\Windows\Fonts\arial.ttf"]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/HelveticaNeue.ttc",
            "/System/Library/Fonts/Helvetica.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
    };
    candidates.iter().find_map(|path| std::fs::read(path).ok())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Some(bytes) = system_font() {
        fonts.font_data.insert("systeme".to_string(), Arc::new(FontData::from_owned(bytes)));
        if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
            family.insert(0, "systeme".to_string());
        }
    }
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        use FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, Proportional)),
            (TextStyle::Body, FontId::new(13.0, Proportional)),
            (TextStyle::Button, FontId::new(13.0, Proportional)),
            (TextStyle::Heading, FontId::new(15.0, Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(9.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.slider_rail_height = 4.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = false;

        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.override_text_color = Some(TEXT);
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.extreme_bg_color = BG;
        v.faint_bg_color = PANEL_2;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = alpha(ACCENT, 0.32);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.window_stroke = Stroke::new(1.0, LINE_2);
        v.warn_fg_color = WARN;
        v.error_fg_color = DANGER;
        v.slider_trailing_fill = true;

        let radius = egui::CornerRadius::same(6);
        v.window_corner_radius = egui::CornerRadius::same(8);
        v.menu_corner_radius = egui::CornerRadius::same(8);
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = radius;
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_fill = PANEL;
        v.widgets.noninteractive.weak_bg_fill = PANEL;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.bg_fill = PANEL_2;
        v.widgets.inactive.weak_bg_fill = PANEL_2;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE_2);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.bg_fill = LINE;
        v.widgets.hovered.weak_bg_fill = LINE;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, MUTED);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.active.bg_fill = LINE_2;
        v.widgets.active.weak_bg_fill = LINE_2;
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.open.bg_fill = LINE;
        v.widgets.open.weak_bg_fill = LINE;
        v.widgets.open.bg_stroke = Stroke::new(1.0, LINE_2);
        v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    });
}

/// Une carte : fond légèrement plus clair, bord fin.
pub fn card() -> egui::Frame {
    egui::Frame::new().fill(PANEL_2).stroke(Stroke::new(1.0, LINE)).corner_radius(8).inner_margin(10)
}

/// Texte secondaire.
pub fn hint(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).small().color(MUTED)
}

/// Bouton principal, en couleur d'accent.
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).color(ACCENT_INK).strong()).fill(ACCENT)
}
