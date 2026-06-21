//! App theme + icon set — the design tokens every panel reads from.
//!
//! Direction: a **lighting console / CAD tool** for a designer at a dark venue —
//! dense, technical, precise. Depth is borders-only over a near-black canvas with
//! whisper-quiet surface tints (no drop shadows on panels); a single accent
//! (the user's preference, default cyan-blue) carries selection / live / primary;
//! semantic green/amber/red carry status. Numbers are tabular monospace. One hue,
//! shifted only in lightness across surfaces.
//!
//! `icon` re-exports the Phosphor glyphs we use under semantic names, so call
//! sites read `icon::LIBRARY` and the icon set can be swapped in one place.
//!
//! This is the design-token vocabulary for the whole app; not every token is
//! referenced at all times, so unused-API lint is silenced module-wide.
#![allow(dead_code)]

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Stroke, TextStyle};

use super::windows::Preferences;

/// Semantic Phosphor icons (vector, theme-coloured — never emoji).
pub mod icon {
    use egui_phosphor::regular as p;
    // Panels / tabs
    pub const SCENE: &str = p::STACK;
    pub const LIBRARY: &str = p::PACKAGE;
    pub const INSPECTOR: &str = p::SLIDERS_HORIZONTAL;
    pub const VIEWPORT: &str = p::CUBE;
    pub const DMX: &str = p::SQUARES_FOUR;
    pub const PATCH: &str = p::GRID_FOUR;
    pub const CONNECT: &str = p::PLUGS_CONNECTED;
    // Entities
    pub const FIXTURE: &str = p::LIGHTBULB_FILAMENT;
    pub const ENVIRONMENT: &str = p::SPHERE;
    pub const GEOMETRY: &str = p::CUBE;
    pub const CATEGORY: &str = p::TAG;
    // Actions
    pub const ADD: &str = p::PLUS;
    pub const IMPORT_GDTF: &str = p::FOLDER_OPEN;
    pub const IMPORT_MVR: &str = p::PACKAGE;
    pub const EXPORT: &str = p::FLOPPY_DISK;
    pub const SEARCH: &str = p::MAGNIFYING_GLASS;
    pub const SORT: &str = p::FUNNEL_SIMPLE;
    pub const SETTINGS: &str = p::GEAR_SIX;
    pub const PROFILE: &str = p::FADERS;
    pub const TRASH: &str = p::TRASH;
    pub const FRAME: &str = p::CROSSHAIR;
    pub const CLOSE: &str = p::X;
    pub const PREV: &str = p::CARET_LEFT;
    pub const NEXT: &str = p::CARET_RIGHT;
    pub const COLOR: &str = p::PALETTE;
    pub const INFO: &str = p::INFO;
    pub const RESET: &str = p::ARROW_CLOCKWISE;
    pub const DUPLICATE: &str = p::COPY;
    pub const DESELECT: &str = p::X_CIRCLE;
    pub const CAMERA: &str = p::VIDEO_CAMERA;
    pub const LAYOUT: &str = p::SQUARES_FOUR;
    pub const KEYBOARD: &str = p::KEYBOARD;
    // Status glyphs
    pub const LIVE: &str = p::CIRCLE; // filled-via-color
    pub const WARNING: &str = p::WARNING;
}

// --- semantic status colours (consistent across every panel) ---
pub const LIVE: Color32 = Color32::from_rgb(120, 210, 120);
pub const IDLE: Color32 = Color32::from_rgb(120, 120, 128);
pub const CONFLICT: Color32 = Color32::from_rgb(232, 92, 92);
pub const WARN: Color32 = Color32::from_rgb(232, 184, 96);

/// Four-tier text colour ramp for the current theme (primary…muted).
pub struct Ink {
    pub primary: Color32,
    pub secondary: Color32,
    pub tertiary: Color32,
    pub muted: Color32,
}

pub fn ink(light: bool) -> Ink {
    if light {
        Ink {
            primary: Color32::from_gray(28),
            secondary: Color32::from_gray(70),
            tertiary: Color32::from_gray(110),
            muted: Color32::from_gray(150),
        }
    } else {
        Ink {
            primary: Color32::from_gray(226),
            secondary: Color32::from_gray(176),
            tertiary: Color32::from_gray(132),
            muted: Color32::from_gray(96),
        }
    }
}

/// Install the Phosphor icon font (once, at startup) as a fallback on both the
/// proportional and monospace families, so icon glyphs render inline with text.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

/// A section header row: small, tracked, strong, in the muted ink — the quiet
/// label that groups a block of controls. Optionally icon-led.
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(text).size(10.5).strong().color(ink_for(ui).tertiary));
}

fn ink_for(ui: &egui::Ui) -> Ink {
    ink(!ui.visuals().dark_mode)
}

/// Accent colour as a Color32 from the preference triple.
pub fn accent(prefs: &Preferences) -> Color32 {
    Color32::from_rgb(
        (prefs.accent[0] * 255.0) as u8,
        (prefs.accent[1] * 255.0) as u8,
        (prefs.accent[2] * 255.0) as u8,
    )
}

/// Apply the full theme (visuals + spacing + type scale + zoom) for this frame.
/// Cheap; egui dedups identical styles.
pub fn apply(ctx: &egui::Context, prefs: &Preferences) {
    let a = accent(prefs);
    let dark = !prefs.theme_light;
    let ink = ink(prefs.theme_light);

    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };

    // Surfaces: one hue, lightness steps only. Borders carry structure.
    let (canvas, surface, surface_hi, input, window, faint, border, border_hi) = if dark {
        (
            Color32::from_gray(20),
            Color32::from_gray(28),
            Color32::from_gray(36),
            Color32::from_gray(14),
            Color32::from_gray(33), // window/popover fill — clearly above the canvas + viewport
            Color32::from_white_alpha(6),
            Color32::from_white_alpha(20),
            Color32::from_white_alpha(34),
        )
    } else {
        (
            Color32::from_gray(243),
            Color32::from_gray(252),
            Color32::from_gray(255),
            Color32::from_gray(248),
            Color32::from_gray(250),
            Color32::from_black_alpha(6),
            Color32::from_black_alpha(24),
            Color32::from_black_alpha(40),
        )
    };

    v.panel_fill = canvas;
    v.window_fill = window;
    v.faint_bg_color = faint;
    v.extreme_bg_color = input;
    v.window_stroke = Stroke::new(1.0, border);
    v.window_corner_radius = CornerRadius::same(7);
    v.menu_corner_radius = CornerRadius::same(6);
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 120 } else { 40 }),
    };
    v.window_shadow = v.popup_shadow;
    v.selection.bg_fill = a.gamma_multiply(0.30);
    v.selection.stroke = Stroke::new(1.0, a);
    v.hyperlink_color = a;

    let r = CornerRadius::same(4);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = canvas;
    w.noninteractive.weak_bg_fill = canvas;
    w.noninteractive.bg_stroke = Stroke::new(1.0, border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, ink.primary);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = surface;
    w.inactive.weak_bg_fill = surface;
    w.inactive.bg_stroke = Stroke::new(1.0, border);
    w.inactive.fg_stroke = Stroke::new(1.0, ink.secondary);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = surface_hi;
    w.hovered.weak_bg_fill = surface_hi;
    w.hovered.bg_stroke = Stroke::new(1.0, border_hi);
    w.hovered.fg_stroke = Stroke::new(1.0, ink.primary);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 1.0;

    w.active.bg_fill = a.gamma_multiply(0.45);
    w.active.weak_bg_fill = a.gamma_multiply(0.45);
    w.active.bg_stroke = Stroke::new(1.0, a);
    w.active.fg_stroke = Stroke::new(1.0, ink.primary);
    w.active.corner_radius = r;
    w.active.expansion = 1.0;

    w.open.bg_fill = surface_hi;
    w.open.weak_bg_fill = surface_hi;
    w.open.bg_stroke = Stroke::new(1.0, border_hi);
    w.open.fg_stroke = Stroke::new(1.0, ink.primary);
    w.open.corner_radius = r;

    let mut style = (*ctx.style()).clone();
    style.visuals = v;

    // Deliberate, distinct type scale (14-base would be loose for a console;
    // body 13 reads dense without strain). Weight/colour carry most hierarchy.
    style.text_styles = [
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
        (TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional)),
    ]
    .into();

    // Dense workbench spacing on a 4px grid.
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(6.0, 5.0);
    s.button_padding = egui::vec2(8.0, 3.0);
    s.indent = 16.0;
    s.window_margin = Margin::same(8);
    s.menu_margin = Margin::same(6);
    s.interact_size.y = 22.0;
    s.scroll.bar_width = 9.0;
    s.scroll.floating = true;

    ctx.set_style(style);
    ctx.set_zoom_factor(prefs.ui_scale.clamp(0.6, 2.5));
}

/// An icon glyph as a `RichText`, sized to sit with body text.
pub fn ico(glyph: &str) -> RichText {
    RichText::new(glyph)
}
