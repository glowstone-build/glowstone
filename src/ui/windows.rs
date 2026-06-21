//! Floating windows + app preferences: the Preferences dialog, the Fixture
//! Profile editor, and the About / Keyboard-Shortcuts boxes. Kept out of
//! `panels.rs` (the dock panels) since these are modeless `egui::Window`s.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Grid, RichText, ScrollArea, Sense, Slider};

use super::theme;
use super::GdtfTextures;
use crate::dmx::DmxConfig;
use crate::scene::{RenderSettings, Scene, Selection, ViewportMode};

/// What a fixture label shows in the viewport overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelMode {
    Name,
    FixtureId,
    Address,
}

impl LabelMode {
    pub const ALL: [LabelMode; 3] = [Self::Name, Self::FixtureId, Self::Address];
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::FixtureId => "Fixture ID",
            Self::Address => "DMX address",
        }
    }
}

/// App-wide user preferences (theme, units, viewport overlays, DMX defaults).
/// Applied live each frame; not yet persisted to disk.
#[derive(Clone, Debug)]
pub struct Preferences {
    /// Display lengths in feet (else metres). Drives DragValue suffixes + status.
    pub units_feet: bool,
    pub theme_light: bool,
    pub accent: [f32; 3],
    pub ui_scale: f32,
    /// Viewport overlays.
    pub show_labels: bool,
    pub labels_selected_only: bool,
    pub label_mode: LabelMode,
    pub show_fps: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            units_feet: false,
            theme_light: false,
            accent: [0.36, 0.66, 1.0],
            ui_scale: 1.0,
            show_labels: true,
            labels_selected_only: false,
            label_mode: LabelMode::Name,
            show_fps: true,
        }
    }
}

impl Preferences {
    /// Metres → display value + unit suffix per the current units setting.
    pub fn len(&self, metres: f32) -> (f32, &'static str) {
        if self.units_feet {
            (metres * 3.280_84, " ft")
        } else {
            (metres, " m")
        }
    }

    /// Apply the full app theme (visuals + spacing + type scale + DPI) for this
    /// frame. Delegates to [`super::theme::apply`] — the single source of tokens.
    pub fn apply_theme(&self, ctx: &egui::Context) {
        super::theme::apply(ctx, self);
    }
}

/// The `s` quick-select palette — a small, keyboard-driven menu for batch
/// fixture selection (All / same profile / same maker / invert / none). Each
/// option has a one-key shortcut; Esc dismisses.
pub fn quick_select_window(
    ctx: &egui::Context,
    scene: &Scene,
    selection: &mut Selection,
    open: &mut bool,
) {
    if !*open {
        return;
    }
    let n = scene.fixtures.len();
    if n == 0 {
        *open = false;
        return;
    }

    // The primary selection drives the "same type / same maker" options.
    let primary = selection.primary_fixture().filter(|&i| i < n);
    let prof = primary.map(|i| scene.fixtures[i].profile.clone());
    let maker = primary
        .and_then(|i| scene.fixtures[i].gdtf.as_ref())
        .map(|g| g.manufacturer.clone())
        .filter(|m| !m.is_empty());
    let type_n = prof.as_ref().map(|p| scene.fixtures.iter().filter(|f| &f.profile == p).count());
    let maker_n = maker.as_ref().map(|m| {
        scene.fixtures.iter().filter(|f| f.gdtf.as_ref().map(|g| &g.manufacturer) == Some(m)).count()
    });
    let inv_n = n - selection.fixtures.iter().filter(|&&i| i < n).count();

    // Resolve a chosen action id into a new selection.
    let mut action: Option<u8> = None;
    ctx.input(|i| {
        use egui::Key;
        if i.key_pressed(Key::A) {
            action = Some(0);
        }
        if i.key_pressed(Key::T) && prof.is_some() {
            action = Some(1);
        }
        if i.key_pressed(Key::M) && maker.is_some() {
            action = Some(2);
        }
        if i.key_pressed(Key::I) {
            action = Some(3);
        }
        if i.key_pressed(Key::N) {
            action = Some(4);
        }
        if i.key_pressed(Key::Escape) {
            action = Some(255);
        }
    });

    egui::Window::new("quick-select")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
        .show(ctx, |ui| {
            ui.set_min_width(260.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}  Select", theme::icon::FIXTURE)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Esc").small().weak());
                });
            });
            ui.separator();
            let rows: [(char, &str, Option<usize>, u8, bool); 5] = [
                ('A', "All fixtures", Some(n), 0, true),
                ('T', "All of this type", type_n, 1, prof.is_some()),
                ('M', "All by this maker", maker_n, 2, maker.is_some()),
                ('I', "Invert selection", Some(inv_n), 3, true),
                ('N', "Select none", None, 4, true),
            ];
            for (key, label, count, id, enabled) in rows {
                let resp = quick_row(ui, key, label, count, enabled);
                if resp.clicked() {
                    action = Some(id);
                }
            }
        });

    if let Some(a) = action {
        let new: Option<Vec<usize>> = match a {
            0 => Some((0..n).collect()),
            1 => prof.map(|p| (0..n).filter(|&i| scene.fixtures[i].profile == p).collect()),
            2 => maker.map(|m| {
                (0..n)
                    .filter(|&i| scene.fixtures[i].gdtf.as_ref().map(|g| &g.manufacturer) == Some(&m))
                    .collect()
            }),
            3 => {
                let cur: std::collections::HashSet<usize> = selection.fixtures.iter().copied().collect();
                Some((0..n).filter(|i| !cur.contains(i)).collect())
            }
            4 => Some(Vec::new()),
            _ => None,
        };
        if let Some(f) = new {
            selection.fixtures = f;
            selection.environment = None;
        }
        *open = false;
    }
}

/// One row of the quick-select palette: label on the left, count + key badge on
/// the right, full-width clickable.
fn quick_row(ui: &mut egui::Ui, key: char, label: &str, count: Option<usize>, enabled: bool) -> egui::Response {
    let ink = theme::ink(!ui.visuals().dark_mode);
    let h = 26.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter_at(rect);
    if enabled && resp.hovered() {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let fg = if enabled { ink.primary } else { ink.muted };
    painter.text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        fg,
    );
    let mut x = rect.right() - 8.0;
    // key badge
    let badge = egui::Rect::from_center_size(egui::pos2(x - 8.0, rect.center().y), egui::vec2(18.0, 16.0));
    painter.rect_filled(badge, 3.0, ui.visuals().extreme_bg_color);
    painter.text(badge.center(), egui::Align2::CENTER_CENTER, key, egui::FontId::monospace(11.0), ink.secondary);
    x -= 26.0;
    if let Some(c) = count {
        painter.text(
            egui::pos2(x, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{c}"),
            egui::FontId::monospace(11.0),
            ink.tertiary,
        );
    }
    resp
}

/// The Fixture Profile editor's currently-open detail tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileTab {
    Identity,
    Physical,
    Optics,
    Modes,
    Channels,
    Wheels,
    Defaults,
}

impl ProfileTab {
    const ALL: [ProfileTab; 7] = [
        Self::Identity,
        Self::Physical,
        Self::Optics,
        Self::Modes,
        Self::Channels,
        Self::Wheels,
        Self::Defaults,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Identity => "Identity",
            Self::Physical => "Physical",
            Self::Optics => "Optics",
            Self::Modes => "DMX Modes",
            Self::Channels => "Channels",
            Self::Wheels => "Wheels",
            Self::Defaults => "Defaults",
        }
    }
}

/// Open-state for the Fixture Profile editor window.
pub struct ProfileEditor {
    pub fixture: usize,
    pub tab: ProfileTab,
    /// Selected emitter index in the geometry tree (for per-emitter optics).
    pub emitter: usize,
}

impl ProfileEditor {
    pub fn new(fixture: usize) -> Self {
        Self { fixture, tab: ProfileTab::Identity, emitter: 0 }
    }
}

/// The Preferences window. Returns nothing; mutates `prefs`/`settings`/`config`.
pub fn preferences_window(
    ctx: &egui::Context,
    open: &mut bool,
    prefs: &mut Preferences,
    settings: &mut RenderSettings,
    config: &mut DmxConfig,
) {
    let mut keep = *open;
    egui::Window::new("Preferences")
        .open(&mut keep)
        .resizable(true)
        .default_width(440.0)
        .show(ctx, |ui| {
            egui::CollapsingHeader::new("General")
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Units");
                        ui.selectable_value(&mut prefs.units_feet, false, "Metres");
                        ui.selectable_value(&mut prefs.units_feet, true, "Feet");
                    });
                });
            egui::CollapsingHeader::new("Theme & Display")
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Theme");
                        ui.selectable_value(&mut prefs.theme_light, false, "Dark");
                        ui.selectable_value(&mut prefs.theme_light, true, "Light");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Accent");
                        ui.color_edit_button_rgb(&mut prefs.accent);
                    });
                    ui.horizontal(|ui| {
                        ui.label("UI scale");
                        ui.add(Slider::new(&mut prefs.ui_scale, 0.7..=2.0));
                    });
                });
            egui::CollapsingHeader::new("Viewport")
                .default_open(true)
                .show(ui, |ui| {
                    ui.checkbox(&mut prefs.show_labels, "Fixture labels");
                    ui.checkbox(&mut prefs.labels_selected_only, "  selected only");
                    ui.horizontal(|ui| {
                        ui.label("Label");
                        for m in LabelMode::ALL {
                            ui.selectable_value(&mut prefs.label_mode, m, m.label());
                        }
                    });
                    ui.checkbox(&mut prefs.show_fps, "FPS overlay");
                });
            egui::CollapsingHeader::new("Rendering")
                .default_open(false)
                .show(ui, |ui| {
                    Grid::new("prefs-render").num_columns(2).show(ui, |ui| {
                        ui.label("Exposure");
                        ui.add(Slider::new(&mut settings.exposure, 0.05..=8.0));
                        ui.end_row();
                        ui.label("Bloom");
                        ui.add(Slider::new(&mut settings.bloom, 0.0..=3.0));
                        ui.end_row();
                        ui.label("Beam intensity");
                        ui.add(Slider::new(&mut settings.beam_intensity, 0.0..=4000.0));
                        ui.end_row();
                        ui.label("Volumetric steps");
                        ui.add(Slider::new(&mut settings.steps, 8..=192));
                        ui.end_row();
                        ui.label("Gobo sharpen");
                        ui.add(Slider::new(&mut settings.gobo_sharpness, 0.0..=2.0));
                        ui.end_row();
                        ui.label("Display mode");
                        ui.horizontal(|ui| {
                            for m in ViewportMode::ALL {
                                ui.selectable_value(&mut settings.mode, m, m.label());
                            }
                        });
                        ui.end_row();
                    });
                });
            egui::CollapsingHeader::new("DMX / Network")
                .default_open(false)
                .show(ui, |ui| {
                    ui.checkbox(&mut config.artnet, "Art-Net");
                    ui.checkbox(&mut config.sacn, "sACN");
                    ui.label(
                        RichText::new("Start receiving in the Connectivity panel.")
                            .weak()
                            .small(),
                    );
                });
            ui.separator();
            if ui.button("Reset to defaults").clicked() {
                *prefs = Preferences::default();
            }
        });
    *open = keep;
}

/// The About box.
pub fn about_window(ctx: &egui::Context, open: &mut bool) {
    let mut keep = *open;
    egui::Window::new("About previz")
        .open(&mut keep)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.heading("previz");
            ui.label("Stage-lighting previsualization");
            ui.add_space(6.0);
            ui.label(format!("version {}", env!("CARGO_PKG_VERSION")));
            ui.label("wgpu 29 · winit · egui · egui_dock");
            ui.label("GDTF + MVR import/export");
            ui.add_space(6.0);
            ui.label(
                RichText::new("Physically-motivated optical-chain beam engine,\nmulti-emitter fixtures, live Art-Net / sACN.")
                    .weak()
                    .small(),
            );
        });
    *open = keep;
}

/// The keyboard-shortcuts cheat sheet.
pub fn shortcuts_window(ctx: &egui::Context, open: &mut bool) {
    let mut keep = *open;
    egui::Window::new("Keyboard Shortcuts")
        .open(&mut keep)
        .resizable(false)
        .show(ctx, |ui| {
            let rows = [
                ("Orbit / Pan / Zoom", "drag / shift+drag / scroll"),
                ("Select fixture", "click  (⌘/Ctrl = multi, Shift = range)"),
                ("Select all fixtures", "A"),
                ("Quick-select menu (empty selection)", "S"),
                ("Deselect all", "Esc"),
                ("Move / Rotate / Scale selection", "G / R / S   (then X·Y·Z to lock)"),
                ("  confirm / cancel transform", "click·Enter / Esc·right-click"),
                ("Nudge selected (floor / height)", "arrows / PageUp·Down  (Shift = 1 m)"),
                ("Duplicate / Array", "D"),
                ("Delete selected", "Delete / Backspace"),
                ("Frame selection / all", "F / Shift+F"),
                ("Top / Front / Right / Persp view", "numpad 7 / 1 / 3 / 5"),
                ("Toggle fixture labels", "L"),
                ("Preferences", "⌘/Ctrl+,"),
            ];
            Grid::new("shortcuts").num_columns(2).spacing([20.0, 6.0]).striped(true).show(ui, |ui| {
                for (action, key) in rows {
                    ui.label(action);
                    ui.label(RichText::new(key).monospace());
                    ui.end_row();
                }
            });
        });
    *open = keep;
}

/// The Fixture Profile editor window: full GDTF data (read-only) + the
/// per-instance overrides for the selected fixture. Closes itself when the
/// fixture is gone or the user closes it.
pub fn profile_editor_window(
    ctx: &egui::Context,
    scene: &mut Scene,
    selection: &mut Selection,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
    state: &mut Option<ProfileEditor>,
    prefs: &Preferences,
) {
    let Some(ed) = state.as_mut() else { return };
    if ed.fixture >= scene.fixtures.len() || !scene.fixtures[ed.fixture].is_gdtf() {
        *state = None;
        return;
    }
    let gdtf = scene.fixtures[ed.fixture].gdtf.clone().unwrap();
    let key = Arc::as_ptr(&gdtf) as usize;
    let tex = gdtf_textures
        .entry(key)
        .or_insert_with(|| super::panels::load_gdtf_textures(ctx, &gdtf));

    let mut keep = true;
    #[allow(deprecated)] // egui 0.34 screen_rect — content_rect migration later
    let center = ctx.screen_rect().center();
    let title = format!("{}  Fixture Profile — {}", theme::icon::PROFILE, gdtf.name);
    egui::Window::new(title)
        .open(&mut keep)
        .resizable(true)
        .collapsible(false)
        .default_size([860.0, 600.0])
        .min_size([580.0, 420.0])
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(center)
        .show(ctx, |ui| {
            // Both columns get a concrete body height (taller than before, and the
            // window stays resizable). A fixed height — rather than
            // available_height — avoids the auto-size/auto-shrink collapse loop
            // that previously shrank the window to its title bar.
            // A fixed-height body so the (resizable) window opens tall regardless
            // of which tab's content is shown, and the resize handle works.
            let body_h = 540.0;
            ui.horizontal(|ui| {
                // Left: emitter / geometry tree.
                ui.vertical(|ui| {
                    ui.set_min_width(170.0);
                    theme::section(ui, "EMITTERS");
                    let emitters = gdtf.emitters(scene.fixtures[ed.fixture].mode_index);
                    if emitters.is_empty() {
                        ui.label(RichText::new("(single source)").weak().small());
                    }
                    ScrollArea::vertical().id_salt("prof-emitters").max_height(body_h).show(ui, |ui| {
                        for (i, em) in emitters.iter().enumerate() {
                            let tag = format!("{}  ·  {}", i, em.name);
                            if ui.selectable_label(ed.emitter == i, tag).clicked() {
                                ed.emitter = i;
                            }
                        }
                    });
                });
                ui.separator();
                // Right: tabbed detail, in a fixed-height scroll so the window
                // keeps a tall, stable footprint.
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        for t in ProfileTab::ALL {
                            ui.selectable_value(&mut ed.tab, t, t.label());
                        }
                    });
                    ui.separator();
                    ScrollArea::vertical()
                        .id_salt("prof-detail")
                        .max_height(body_h)
                        .min_scrolled_height(body_h)
                        .show(ui, |ui| {
                            ui.set_min_width(560.0);
                            let fixture = &mut scene.fixtures[ed.fixture];
                            match ed.tab {
                                ProfileTab::Identity => tab_identity(ui, fixture, &gdtf),
                                ProfileTab::Physical => tab_physical(ui, &gdtf, prefs),
                                ProfileTab::Optics => tab_optics(ui, fixture, &gdtf, ed.emitter),
                                ProfileTab::Modes => tab_modes(ui, fixture, &gdtf),
                                ProfileTab::Channels => tab_channels(ui, fixture, &gdtf),
                                ProfileTab::Wheels => tab_wheels(ui, &gdtf, tex),
                                ProfileTab::Defaults => tab_defaults(ui, fixture, &gdtf),
                            }
                        });
                });
            });
        });
    if !keep {
        *state = None;
    }
    let _ = selection;
}

/// Badge marking a value as modeled by previz, not present in the GDTF file.
fn modeled(ui: &mut egui::Ui) {
    ui.label(RichText::new("modeled").weak().small().italics())
        .on_hover_text("Synthesized by previz — not carried in the GDTF file");
}

fn ro(ui: &mut egui::Ui, k: &str, v: impl Into<String>) {
    ui.label(k);
    ui.label(RichText::new(v.into()).monospace());
    ui.end_row();
}

fn tab_identity(ui: &mut egui::Ui, fixture: &mut crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    Grid::new("prof-id").num_columns(2).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
        ro(ui, "Manufacturer", &gdtf.manufacturer);
        ro(ui, "Model", &gdtf.name);
        ro(ui, "Long name", &gdtf.long_name);
        ro(ui, "Short name", &gdtf.short_name);
        if !gdtf.spec.is_empty() {
            ro(ui, "GDTF file", &gdtf.spec);
        }
        ui.label("Instance name");
        ui.text_edit_singleline(&mut fixture.name);
        ui.end_row();
        if let Some(m) = fixture.mvr.as_deref_mut() {
            ui.label("Fixture ID");
            ui.text_edit_singleline(&mut m.fixture_id);
            ui.end_row();
        }
    });
    if !gdtf.description.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(&gdtf.description).weak().small());
    }
}

fn tab_physical(ui: &mut egui::Ui, gdtf: &crate::gdtf::GdtfFixture, prefs: &Preferences) {
    let b = &gdtf.beam;
    Grid::new("prof-phys").num_columns(2).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
        ro(ui, "Lamp", &b.lamp_type);
        ro(ui, "Power", format!("{:.0} W", b.power));
        ro(ui, "Luminous flux", format!("{:.0} lm", b.luminous_flux));
        ro(ui, "Colour temp", format!("{:.0} K", b.color_temp));
        ro(ui, "CRI", format!("{:.0}", b.cri));
        let (r, u) = prefs.len(b.beam_radius);
        ro(ui, "Beam radius", format!("{r:.3}{u}"));
        ro(ui, "Models", format!("{}", gdtf.models.len()));
    });
}

fn tab_optics(
    ui: &mut egui::Ui,
    fixture: &mut crate::scene::Fixture,
    gdtf: &crate::gdtf::GdtfFixture,
    emitter: usize,
) {
    let beam = gdtf
        .emitters(fixture.mode_index)
        .get(emitter)
        .map(|e| e.beam.clone())
        .unwrap_or_else(|| gdtf.beam.clone());
    Grid::new("prof-optics").num_columns(2).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
        ro(ui, "Beam type", &beam.beam_type);
        ro(ui, "Beam angle", format!("{:.1}°", beam.beam_angle));
        ro(ui, "Field angle", format!("{:.1}°", beam.field_angle));
        ro(ui, "Beam radius", format!("{:.3} m", beam.beam_radius));
        ro(ui, "Luminous flux", format!("{:.0} lm", beam.luminous_flux));
        ro(ui, "Colour temp", format!("{:.0} K", beam.color_temp));
    });
    ui.add_space(8.0);
    ui.label(RichText::new("MODELED (not in GDTF)").small().strong());
    Grid::new("prof-optics-mod").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
        ui.label("Move speed");
        ui.add(Slider::new(&mut fixture.move_speed, 0.0..=1.0));
        modeled(ui);
        ui.end_row();
        let (pmax, tmax) = fixture.max_slew();
        ui.label("→ pan / tilt");
        ui.label(RichText::new(format!("{pmax:.0} / {tmax:.0} °/s")).monospace());
        ui.label("");
        ui.end_row();
    });
}

fn tab_modes(ui: &mut egui::Ui, fixture: &mut crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    ui.label(RichText::new("Active mode (per-instance):").small());
    let mut changed = None;
    for (i, m) in gdtf.modes.iter().enumerate() {
        let label = format!("{}  ·  {} ch  ·  {} emitters", m.name, m.footprint, m.emitters.len());
        if ui.selectable_label(fixture.mode_index == i, label).clicked() {
            changed = Some(i);
        }
    }
    if let Some(i) = changed {
        fixture.mode_index = i;
        fixture.sync_mode();
    }
}

fn tab_channels(ui: &mut egui::Ui, fixture: &crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    let Some(mode) = gdtf.modes.get(fixture.mode_index) else { return };
    ui.label(RichText::new(format!("{} — {} ch", mode.name, mode.footprint)).small().strong());
    Grid::new("prof-chan").num_columns(4).spacing([12.0, 3.0]).striped(true).show(ui, |ui| {
        ui.strong("Addr");
        ui.strong("Geometry");
        ui.strong("Attribute");
        ui.strong("Function");
        ui.end_row();
        for rc in &mode.resolved {
            let ch = &mode.channels[rc.channel];
            let addr = rc.offsets.first().map(|o| o.to_string()).unwrap_or_else(|| "—".into());
            ui.monospace(addr);
            let geo = rc.instance.clone().unwrap_or_else(|| ch.geometry.clone());
            ui.label(RichText::new(geo).small());
            ui.label(RichText::new(&ch.attribute).small());
            ui.label(RichText::new(&ch.function).weak().small());
            ui.end_row();
        }
    });
}

fn tab_wheels(ui: &mut egui::Ui, gdtf: &crate::gdtf::GdtfFixture, tex: &GdtfTextures) {
    if gdtf.wheels.is_empty() {
        ui.label(RichText::new("No wheels.").weak());
        return;
    }
    for (wi, wheel) in gdtf.wheels.iter().enumerate() {
        ui.label(RichText::new(&wheel.name).strong().small());
        ui.horizontal_wrapped(|ui| {
            for (si, slot) in wheel.slots.iter().enumerate() {
                let handle = tex.wheels.get(wi).and_then(|w| w.get(si)).and_then(|h| h.as_ref());
                let size = egui::vec2(40.0, 40.0);
                if let Some(h) = handle {
                    ui.image((h.id(), size)).on_hover_text(&slot.name);
                } else {
                    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
                    let col = slot
                        .color
                        .map(|c| egui::Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8))
                        .unwrap_or(egui::Color32::from_gray(40));
                    ui.painter().rect_filled(rect, 4.0, col);
                    resp.on_hover_text(&slot.name);
                }
            }
        });
        ui.add_space(4.0);
    }
}

fn tab_defaults(ui: &mut egui::Ui, fixture: &crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    let Some(mode) = gdtf.modes.get(fixture.mode_index) else { return };
    ui.label(RichText::new("GDTF channel defaults (InitialFunction):").small());
    Grid::new("prof-def").num_columns(2).spacing([14.0, 3.0]).striped(true).show(ui, |ui| {
        for ch in &mode.channels {
            ui.label(RichText::new(&ch.attribute).small());
            ui.monospace(format!("{:.3}", ch.default));
            ui.end_row();
        }
    });
}
