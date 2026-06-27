//! The Fixture Profile editor window: full GDTF data (read-only) alongside the
//! per-instance overrides for the selected fixture, organised into detail tabs.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Grid, RichText, ScrollArea, Sense, Slider};

use super::Preferences;
use crate::scene::{Scene, Selection};
use crate::ui::theme;
use crate::ui::GdtfTextures;

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
    /// A leading Phosphor glyph per tab, so the strip reads as named sections.
    fn icon(self) -> &'static str {
        match self {
            Self::Identity => theme::icon::INFO,
            Self::Physical => theme::icon::FIXTURE,
            Self::Optics => theme::icon::INSPECTOR,
            Self::Modes => theme::icon::DMX,
            Self::Channels => theme::icon::PATCH,
            Self::Wheels => theme::icon::COLOR,
            Self::Defaults => theme::icon::SETTINGS,
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
        .or_insert_with(|| crate::ui::inspector::load_gdtf_textures(ctx, &gdtf));

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
            let ink = theme::ink(!ui.visuals().dark_mode);
            ui.horizontal(|ui| {
                // Left: emitter / geometry tree.
                ui.vertical(|ui| {
                    ui.set_min_width(170.0);
                    theme::section(ui, "EMITTERS");
                    let emitters = gdtf.emitters(scene.fixtures[ed.fixture].mode_index);
                    if emitters.is_empty() {
                        ui.label(RichText::new("(single source)").italics().color(ink.muted));
                    }
                    ScrollArea::vertical().id_salt("prof-emitters").max_height(body_h).show(ui, |ui| {
                        for (i, em) in emitters.iter().enumerate() {
                            let on = ed.emitter == i;
                            // Index reads quiet + monospace; the name carries the
                            // weight when selected so the active emitter is clear.
                            let tag = RichText::new(format!("{i:>2}  ·  {}", em.name))
                                .color(if on { ink.primary } else { ink.secondary });
                            if ui.selectable_label(on, tag).clicked() {
                                ed.emitter = i;
                            }
                        }
                    });
                });
                ui.separator();
                // Right: tabbed detail, in a fixed-height scroll so the window
                // keeps a tall, stable footprint.
                ui.vertical(|ui| {
                    // Tab strip: tighten spacing so the seven tabs read as one
                    // contiguous segmented control, each with a leading glyph.
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.horizontal_wrapped(|ui| {
                            for t in ProfileTab::ALL {
                                let on = ed.tab == t;
                                let txt = RichText::new(format!("{}  {}", t.icon(), t.label()))
                                    .color(if on { ink.primary } else { ink.secondary });
                                ui.selectable_value(&mut ed.tab, t, txt);
                            }
                        });
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

/// Badge marking a value as modeled by glowstone, not present in the GDTF file.
fn modeled(ui: &mut egui::Ui) {
    let ink = theme::ink(!ui.visuals().dark_mode);
    ui.label(RichText::new("modeled").small().italics().color(ink.muted))
        .on_hover_text("Synthesized by glowstone — not carried in the GDTF file");
}

/// A read-only key/value grid row: the key reads as a quiet label (secondary
/// ink), the value as primary monospace, so the tables scan cleanly.
fn ro(ui: &mut egui::Ui, k: &str, v: impl Into<String>) {
    let ink = theme::ink(!ui.visuals().dark_mode);
    ui.label(RichText::new(k).color(ink.secondary));
    ui.label(RichText::new(v.into()).monospace().color(ink.primary));
    ui.end_row();
}

fn tab_identity(ui: &mut egui::Ui, fixture: &mut crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    let ink = theme::ink(!ui.visuals().dark_mode);
    theme::section(ui, "IDENTITY");
    ui.add_space(2.0);
    Grid::new("prof-id").num_columns(2).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
        ro(ui, "Manufacturer", &gdtf.manufacturer);
        ro(ui, "Model", &gdtf.name);
        ro(ui, "Long name", &gdtf.long_name);
        ro(ui, "Short name", &gdtf.short_name);
        if !gdtf.spec.is_empty() {
            ro(ui, "GDTF file", &gdtf.spec);
        }
        ui.label(RichText::new("Instance name").color(ink.secondary));
        ui.text_edit_singleline(&mut fixture.name);
        ui.end_row();
        if let Some(m) = fixture.mvr.as_deref_mut() {
            ui.label(RichText::new("Fixture ID").color(ink.secondary));
            ui.text_edit_singleline(&mut m.fixture_id);
            ui.end_row();
        }
    });
    if !gdtf.description.is_empty() {
        ui.add_space(8.0);
        theme::section(ui, "DESCRIPTION");
        ui.add_space(2.0);
        ui.label(RichText::new(&gdtf.description).small().color(ink.tertiary));
    }
}

fn tab_physical(ui: &mut egui::Ui, gdtf: &crate::gdtf::GdtfFixture, prefs: &Preferences) {
    let b = &gdtf.beam;
    theme::section(ui, "LAMP & BEAM");
    ui.add_space(2.0);
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
    let ink = theme::ink(!ui.visuals().dark_mode);
    let beam = gdtf
        .emitters(fixture.mode_index)
        .get(emitter)
        .map(|e| e.beam.clone())
        .unwrap_or_else(|| gdtf.beam.clone());
    theme::section(ui, "OPTICAL CHAIN");
    ui.add_space(2.0);
    Grid::new("prof-optics").num_columns(2).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
        ro(ui, "Beam type", &beam.beam_type);
        ro(ui, "Beam angle", format!("{:.1}°", beam.beam_angle));
        ro(ui, "Field angle", format!("{:.1}°", beam.field_angle));
        ro(ui, "Beam radius", format!("{:.3} m", beam.beam_radius));
        ro(ui, "Luminous flux", format!("{:.0} lm", beam.luminous_flux));
        ro(ui, "Colour temp", format!("{:.0} K", beam.color_temp));
    });
    ui.add_space(10.0);
    theme::section(ui, "MODELED — NOT IN GDTF");
    ui.add_space(2.0);
    Grid::new("prof-optics-mod").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
        ui.label(RichText::new("Move speed").color(ink.secondary));
        ui.add(Slider::new(&mut fixture.move_speed, 0.0..=1.0));
        modeled(ui);
        ui.end_row();
        let (pmax, tmax) = fixture.max_slew();
        ui.label(RichText::new(format!("{}  pan / tilt", theme::icon::ARROW_RIGHT)).color(ink.tertiary));
        ui.label(RichText::new(format!("{pmax:.0} / {tmax:.0} °/s")).monospace().color(ink.primary));
        ui.label("");
        ui.end_row();
    });
}

fn tab_modes(ui: &mut egui::Ui, fixture: &mut crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    let ink = theme::ink(!ui.visuals().dark_mode);
    theme::section(ui, "DMX MODE — PER INSTANCE");
    ui.add_space(2.0);
    let mut changed = None;
    for (i, m) in gdtf.modes.iter().enumerate() {
        let on = fixture.mode_index == i;
        let label = RichText::new(format!(
            "{}  ·  {} ch  ·  {} emitters",
            m.name,
            m.footprint,
            m.emitters.len()
        ))
        .color(if on { ink.primary } else { ink.secondary });
        if ui.selectable_label(on, label).clicked() {
            changed = Some(i);
        }
    }
    if let Some(i) = changed {
        fixture.mode_index = i;
        fixture.sync_mode();
    }
}

fn tab_channels(ui: &mut egui::Ui, fixture: &crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    let ink = theme::ink(!ui.visuals().dark_mode);
    let Some(mode) = gdtf.modes.get(fixture.mode_index) else { return };
    theme::section(ui, "DMX CHANNELS");
    ui.label(RichText::new(format!("{}  ·  {} ch", mode.name, mode.footprint)).small().color(ink.tertiary));
    ui.add_space(2.0);
    Grid::new("prof-chan").num_columns(4).spacing([12.0, 3.0]).striped(true).show(ui, |ui| {
        let head = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).small().strong().color(ink.secondary));
        };
        head(ui, "Addr");
        head(ui, "Geometry");
        head(ui, "Attribute");
        head(ui, "Function");
        ui.end_row();
        for rc in &mode.resolved {
            let ch = &mode.channels[rc.channel];
            let addr = rc.offsets.first().map(|o| o.to_string()).unwrap_or_else(|| "—".into());
            ui.label(RichText::new(addr).monospace().color(ink.primary));
            let geo = rc.instance.clone().unwrap_or_else(|| ch.geometry.clone());
            ui.label(RichText::new(geo).small().color(ink.tertiary));
            ui.label(RichText::new(&ch.attribute).small().color(ink.primary));
            ui.label(RichText::new(&ch.function).small().color(ink.muted));
            ui.end_row();
        }
    });
}

fn tab_wheels(ui: &mut egui::Ui, gdtf: &crate::gdtf::GdtfFixture, tex: &GdtfTextures) {
    let ink = theme::ink(!ui.visuals().dark_mode);
    theme::section(ui, "WHEELS");
    if gdtf.wheels.is_empty() {
        ui.add_space(2.0);
        ui.label(RichText::new("No wheels.").italics().color(ink.muted));
        return;
    }
    ui.add_space(2.0);
    let empty = ui.visuals().extreme_bg_color;
    for (wi, wheel) in gdtf.wheels.iter().enumerate() {
        ui.label(
            RichText::new(format!("{}  ·  {} slots", wheel.name, wheel.slots.len()))
                .small()
                .strong()
                .color(ink.secondary),
        );
        ui.add_space(2.0);
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
                        .unwrap_or(empty);
                    ui.painter().rect_filled(rect, 4.0, col);
                    resp.on_hover_text(&slot.name);
                }
            }
        });
        ui.add_space(8.0);
    }
}

fn tab_defaults(ui: &mut egui::Ui, fixture: &crate::scene::Fixture, gdtf: &crate::gdtf::GdtfFixture) {
    let ink = theme::ink(!ui.visuals().dark_mode);
    let Some(mode) = gdtf.modes.get(fixture.mode_index) else { return };
    theme::section(ui, "CHANNEL DEFAULTS");
    ui.label(RichText::new("InitialFunction values from the GDTF.").small().color(ink.tertiary));
    ui.add_space(2.0);
    Grid::new("prof-def").num_columns(2).spacing([14.0, 3.0]).striped(true).show(ui, |ui| {
        for ch in &mode.channels {
            ui.label(RichText::new(&ch.attribute).small().color(ink.secondary));
            ui.label(RichText::new(format!("{:.3}", ch.default)).monospace().color(ink.primary));
            ui.end_row();
        }
    });
}
