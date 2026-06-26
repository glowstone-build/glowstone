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
    pub const PERF: &str = p::GAUGE;
    // Entities
    pub const FIXTURE: &str = p::LIGHTBULB_FILAMENT;
    pub const ENVIRONMENT: &str = p::SPHERE;
    pub const WORLD: &str = p::SUN_HORIZON;
    pub const IMAGE: &str = p::IMAGE;
    pub const GEOMETRY: &str = p::CUBE;
    pub const SCREEN: &str = p::MONITOR;
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
    // Play + directional arrows — use the Phosphor glyphs (the bundled text fonts
    // lack the raw Unicode ▶/←/→/↑/↓, which render as tofu squares).
    pub const PLAY: &str = p::PLAY;
    pub const ARROW_RIGHT: &str = p::ARROW_RIGHT;
    pub const ARROW_LEFT: &str = p::ARROW_LEFT;
    pub const ARROW_UP: &str = p::ARROW_UP;
    pub const ARROW_DOWN: &str = p::ARROW_DOWN;
    // Outliner-tree disclosure triangles (expanded ▾ / collapsed ▸), Blender-style.
    pub const TREE_OPEN: &str = p::CARET_DOWN;
    pub const TREE_CLOSED: &str = p::CARET_RIGHT;
    pub const COLOR: &str = p::PALETTE;
    pub const INFO: &str = p::INFO;
    pub const RESET: &str = p::ARROW_CLOCKWISE;
    pub const UNDO: &str = p::ARROW_COUNTER_CLOCKWISE;
    pub const REDO: &str = p::ARROW_CLOCKWISE;
    pub const DUPLICATE: &str = p::COPY;
    pub const DESELECT: &str = p::X_CIRCLE;
    pub const EYE: &str = p::EYE;
    pub const EYE_OFF: &str = p::EYE_SLASH;
    pub const CAMERA: &str = p::VIDEO_CAMERA;
    pub const LAYOUT: &str = p::SQUARES_FOUR;
    pub const KEYBOARD: &str = p::KEYBOARD;
    // Viewport region toggles (Blender's N-panel sidebar + T-panel tool rail).
    pub const N_PANEL: &str = p::SIDEBAR_SIMPLE;
    pub const T_PANEL: &str = p::TOOLBOX;

    // Render (still-image / animation render + the Render dock tab).
    pub const RENDER: &str = p::IMAGE;
    pub const RENDER_GO: &str = p::PLAY;
    pub const RENDER_STOP: &str = p::STOP_CIRCLE;
    pub const ANIMATION: &str = p::FILM_STRIP;
    pub const SAVE_IMAGE: &str = p::FLOPPY_DISK;
    pub const FULLSCREEN: &str = p::CORNERS_OUT;
    pub const FULLSCREEN_EXIT: &str = p::CORNERS_IN;
    pub const TIMER: &str = p::TIMER;

    // Viewport tool rail (§2.4 ActiveTool). X=red/Y=green/Z=blue gizmo handle
    // colours live on `Axis::color` (mod.rs); these are just the rail glyphs.
    pub const TOOL_SELECT: &str = p::CURSOR;
    pub const TOOL_MOVE: &str = p::ARROWS_OUT_CARDINAL;
    pub const TOOL_ROTATE: &str = p::ARROWS_CLOCKWISE;
    pub const TOOL_SCALE: &str = p::RESIZE;
    pub const TOOL_AIM: &str = p::TARGET;
    pub const TOOL_MEASURE: &str = p::RULER;
    // Transform-tool options (§2.4): grid/increment snap toggle + 3D-cursor pivot.
    pub const SNAP: &str = p::MAGNET;
    pub const CURSOR_3D: &str = p::CROSSHAIR_SIMPLE;
    // Status glyphs (toasts / reports — `ui::notify`). Aliases keep the notify
    // module reading semantic names (WARN/ERROR) rather than raw Phosphor consts.
    pub const WARNING: &str = p::WARNING;
    pub const WARN: &str = p::WARNING;
    pub const ERROR: &str = p::WARNING_CIRCLE;
    /// The report-log window (the toast/notification history).
    pub const LOG: &str = p::LIST_BULLETS;
    // Online fixture library (GDTF Share)
    pub const ONLINE: &str = p::GLOBE_SIMPLE;
    pub const DOWNLOAD: &str = p::DOWNLOAD_SIMPLE;
    pub const CLOUD: &str = p::CLOUD_ARROW_DOWN;
    pub const CACHED: &str = p::CLOUD_CHECK;
    pub const CHECK: &str = p::CHECK;
    pub const STAR: &str = p::STAR;
    pub const SIGN_IN: &str = p::SIGN_IN;
    pub const SIGN_OUT: &str = p::SIGN_OUT;
    pub const USER: &str = p::USER_CIRCLE;
}

// --- semantic status colours (consistent across every panel) ---
// Kept as `const` aliases onto the live [`Palette`] defaults so legacy call
// sites (`theme::OK`, `theme::CONFLICT`) keep compiling unchanged while new
// code reads the enum-indexed table via [`Palette::get`]. Identical bytes — no
// visual change. Prefer `Palette::get(ui).ok` etc. in new code.
/// "Good / ready / receiving" green (a data-flow + sign-in accent, not a
/// per-fixture "live" badge — those were removed).
pub const OK: Color32 = Color32::from_rgb(120, 210, 120);
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

/// The semantic token table — one enum-indexed set of *role* colours (à la
/// Unreal's `EStyleColor`) built once per frame in [`apply`] and stashed in
/// egui's per-context data, so any panel can read `Palette::get(ui).surface_hi`
/// instead of hand-coding an RGB literal. One hue, lightness steps only; the
/// accent + status hues are the only chromatic roles. Swapping this table (e.g.
/// a future light/HC variant) restyles the whole app from one place.
#[derive(Clone, Copy)]
pub struct Palette {
    // Backgrounds (canvas = darkest panel field; surface(_hi) = raised chips;
    // input = the recessed text-field well; window = popovers above the canvas).
    pub canvas: Color32,
    pub surface: Color32,
    pub surface_hi: Color32,
    pub input: Color32,
    pub window: Color32,
    pub faint: Color32,
    // Hairlines.
    pub border: Color32,
    pub border_hi: Color32,
    // Single accent (selection / primary / live) — the user's preference.
    pub accent: Color32,
    // Four-tier text ramp (primary…muted), mirrors [`Ink`].
    pub ink: Color32,
    pub ink_secondary: Color32,
    pub ink_tertiary: Color32,
    pub ink_muted: Color32,
    // Semantic status hues.
    pub ok: Color32,
    pub warn: Color32,
    pub conflict: Color32,
    pub idle: Color32,
}

impl Palette {
    /// Build the role table for the given prefs (the single source of truth that
    /// [`apply`] both installs into egui Visuals *and* stashes for direct reads).
    pub fn build(prefs: &Preferences) -> Self {
        let dark = !prefs.theme_light;
        let ink = ink(prefs.theme_light);
        let (canvas, surface, surface_hi, input, window, faint, border, border_hi) = if dark {
            (
                Color32::from_gray(20),
                Color32::from_gray(28),
                Color32::from_gray(36),
                Color32::from_gray(14),
                Color32::from_gray(33),
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
        Self {
            canvas,
            surface,
            surface_hi,
            input,
            window,
            faint,
            border,
            border_hi,
            accent: accent(prefs),
            ink: ink.primary,
            ink_secondary: ink.secondary,
            ink_tertiary: ink.tertiary,
            ink_muted: ink.muted,
            ok: OK,
            warn: WARN,
            conflict: CONFLICT,
            idle: IDLE,
        }
    }

    /// Read the palette installed for this frame. Falls back to the dark default
    /// if [`apply`] hasn't run yet (e.g. a unit test painting in isolation).
    pub fn get(ui: &egui::Ui) -> Palette {
        Self::get_ctx(ui.ctx())
    }

    /// Read the palette from the [`egui::Context`] directly — for overlays that
    /// paint at the context level (toasts) and have no `&Ui` in hand yet.
    pub fn get_ctx(ctx: &egui::Context) -> Palette {
        ctx.data(|d| d.get_temp::<Palette>(egui::Id::NULL))
            .unwrap_or_else(|| Palette::build(&Preferences::default()))
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

/// A floating viewport overlay pill: a dark, rounded, padded chip with light
/// (or accent-tinted) text — the shared visual language for the selection
/// label, the modal-transform hint, and the bottom help line. `anchor` + `align`
/// position the pill exactly like `Painter::text` does for plain text, so call
/// sites can drop-in replace a raw `painter.text(...)`.
pub fn overlay_label(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    align: egui::Align2,
    text: &str,
    accent: Option<egui::Color32>,
) {
    let fg = accent.unwrap_or(egui::Color32::from_gray(238));
    let font = egui::FontId::proportional(12.5);
    let galley = painter.layout_no_wrap(text.to_owned(), font, fg);
    let pad = egui::vec2(9.0, 5.0);
    let size = galley.size() + pad * 2.0;
    // Place the padded box so `anchor`/`align` line up with the text box, the
    // same contract egui's Align2 uses against a rect of `size`.
    let bg = align.anchor_size(anchor, size);
    painter.rect_filled(bg, 5.0, egui::Color32::from_black_alpha(150));
    painter.galley(bg.min + pad, galley, fg);
}

/// Accent colour as a Color32 from the preference triple.
pub fn accent(prefs: &Preferences) -> Color32 {
    Color32::from_rgb(
        (prefs.accent[0] * 255.0) as u8,
        (prefs.accent[1] * 255.0) as u8,
        (prefs.accent[2] * 255.0) as u8,
    )
}

/// Procedurally derive a widget state's [`WidgetVisuals`] from one base
/// surface + the role tokens, instead of hand-setting every permutation
/// (Blender's `widget_active_color` HSL-multiply approach). `bg`/`stroke` pick
/// the fill + hairline; `expand` is the egui hover bloom; `fg` the label ink.
/// One call shape per state keeps the accent + surfaces coherent: bump the base
/// and every state tracks it.
fn widget_state(bg: Color32, stroke: Color32, fg: Color32, expand: f32) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, stroke),
        fg_stroke: Stroke::new(1.0, fg),
        corner_radius: CornerRadius::same(4),
        expansion: expand,
    }
}

/// Apply the full theme (visuals + spacing + type scale + zoom) for this frame.
/// Cheap; egui dedups identical styles. Also stashes the live [`Palette`] in
/// `ctx` data so panels can read role tokens directly.
#[allow(deprecated)] // egui 0.34 style/set_style rename — migrated project-wide later
pub fn apply(ctx: &egui::Context, prefs: &Preferences) {
    let dark = !prefs.theme_light;
    let p = Palette::build(prefs);
    let a = p.accent;

    // Publish the role table for direct reads this frame (Palette::get).
    ctx.data_mut(|d| d.insert_temp(egui::Id::NULL, p));

    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };

    v.panel_fill = p.canvas;
    v.window_fill = p.window;
    v.faint_bg_color = p.faint;
    v.extreme_bg_color = p.input;
    v.window_stroke = Stroke::new(1.0, p.border);
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

    // States derived from the role tokens, not hand-tuned per permutation:
    // rest → surface; hover → surface_hi (brighter hairline + 1px bloom);
    // press/active → accent wash; the accent fill is one gamma step of `a`.
    let w = &mut v.widgets;
    w.noninteractive = widget_state(p.canvas, p.border, p.ink, 0.0);
    w.inactive = widget_state(p.surface, p.border, p.ink_secondary, 0.0);
    w.hovered = widget_state(p.surface_hi, p.border_hi, p.ink, 1.0);
    w.active = widget_state(a.gamma_multiply(0.45), a, p.ink, 1.0);
    w.open = widget_state(p.surface_hi, p.border_hi, p.ink, 0.0);

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

    // Dense workbench spacing on a 4px grid. `ui_scale` drives the egui zoom
    // (fonts + every pixel literal) so this base spacing stays in DESIGN units
    // and the scale is applied once, uniformly — no double-scaling.
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(6.0, 5.0);
    s.button_padding = egui::vec2(8.0, 3.0);
    s.indent = 16.0;
    s.window_margin = Margin::same(8);
    s.menu_margin = Margin::same(6);
    s.interact_size.y = 22.0;
    s.scroll.bar_width = 9.0;
    s.scroll.floating = true;

    // Plain Labels are NOT selectable text in a CAD/console UI: egui defaults
    // `selectable_labels`/`multi_widget_text_select` to true, which made hovering
    // or dragging across the UI rubber-band-select every label (the splash bug).
    // TextEdit / DragValue keep their own selection — this only affects Labels.
    style.interaction.selectable_labels = false;
    style.interaction.multi_widget_text_select = false;

    ctx.set_style(style);

    // Single continuous application scale. Round to whole device pixels so
    // borders + the type baseline stay crisp 1px hairlines at any density (a
    // fractional zoom otherwise smears 1px strokes across two rows).
    let ppp = ctx.native_pixels_per_point().unwrap_or(1.0).max(1.0);
    let raw = prefs.ui_scale.clamp(0.6, 2.5);
    let snapped = (raw * ppp).round() / ppp;
    ctx.set_zoom_factor(snapped);
}

/// An icon glyph as a `RichText`, sized to sit with body text.
pub fn ico(glyph: &str) -> RichText {
    RichText::new(glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The role table's status hues must equal the legacy `const` tokens, so
    /// migrating a call site from `theme::OK` to `Palette::get(ui).ok` is a pure
    /// rename with zero visual change (the no-regression guarantee).
    #[test]
    fn palette_status_matches_legacy_consts() {
        let p = Palette::build(&Preferences::default());
        assert_eq!(p.ok, OK);
        assert_eq!(p.warn, WARN);
        assert_eq!(p.conflict, CONFLICT);
        assert_eq!(p.idle, IDLE);
    }

    /// Dark (default) and light prefs select different canvas/surface fields, and
    /// the surface ramp stays monotone (canvas darkest, surface_hi lightest in
    /// dark mode) — the "one hue, lightness steps only" contract.
    #[test]
    fn palette_dark_light_and_ramp() {
        let dark = Palette::build(&Preferences { theme_light: false, ..Default::default() });
        let light = Palette::build(&Preferences { theme_light: true, ..Default::default() });
        assert_ne!(dark.canvas, light.canvas);
        // Dark ramp ascends in lightness from canvas to surface_hi.
        let lum = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        assert!(lum(dark.canvas) < lum(dark.surface));
        assert!(lum(dark.surface) < lum(dark.surface_hi));
    }

    /// The accent role tracks the preference triple exactly.
    #[test]
    fn palette_accent_follows_pref() {
        let prefs = Preferences { accent: [1.0, 0.0, 0.0], ..Default::default() };
        let p = Palette::build(&prefs);
        assert_eq!(p.accent, Color32::from_rgb(255, 0, 0));
    }
}
