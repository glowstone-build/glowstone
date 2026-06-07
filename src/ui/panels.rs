//! The individual dock panels. Each is a plain function taking the egui `Ui`
//! plus whatever scene state it reads or edits.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{DragValue, Grid, RichText, Sense, Slider};
use glam::{Vec2, Vec3};

use super::{DuplicateDialog, GdtfTextures};
use crate::gdtf::GdtfFixture;
use crate::optics::{self, OpticalControls};
use crate::renderer::camera::OrbitCamera;
use crate::scene::environment::Environment;
use crate::scene::{Fixture, Library, RenderSettings, Scene, Selection};

/// Left tab: the scene outliner — every fixture and environment, selectable —
/// plus the global view/look controls.
pub fn scene_outliner(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &mut Selection,
    settings: &mut RenderSettings,
) {
    ui.heading("Scene");
    ui.separator();

    ui.label(RichText::new("FIXTURES").small().strong());
    if scene.fixtures.is_empty() {
        ui.label(RichText::new("none — add from the Library").weak().small());
    }
    // Click selects; ⌘/Ctrl/Shift-click toggles into a multi-selection.
    for (i, fixture) in scene.fixtures.iter().enumerate() {
        let label = format!("{}  ·  {}", fixture.name, fixture.profile);
        let resp = ui.selectable_label(selection.contains_fixture(i), label);
        if resp.clicked() {
            if ui.input(|x| x.modifiers.command || x.modifiers.shift) {
                selection.toggle_fixture(i);
            } else {
                *selection = Selection::fixture(i);
            }
        }
    }

    ui.add_space(8.0);
    ui.label(RichText::new("ENVIRONMENTS").small().strong());
    if scene.environments.is_empty() {
        ui.label(RichText::new("none — add from the Library").weak().small());
    }
    for (i, env) in scene.environments.iter().enumerate() {
        if ui
            .selectable_label(selection.environment == Some(i), env.name.as_str())
            .clicked()
        {
            *selection = Selection::environment(i);
        }
    }

    // Imported MVR static geometry (stage / truss / set) — read-only list.
    if !scene.geometry.is_empty() {
        ui.add_space(8.0);
        egui::CollapsingHeader::new(
            RichText::new(format!("GEOMETRY ({})", scene.geometry.len()))
                .small()
                .strong(),
        )
        .default_open(false)
        .show(ui, |ui| {
            for g in &scene.geometry {
                ui.label(RichText::new(&g.name).weak().small());
            }
        });
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(RichText::new("VIEW").small().strong());
    Grid::new("view-grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Exposure");
            ui.add(DragValue::new(&mut settings.exposure).speed(0.01).range(0.05..=8.0));
            ui.end_row();

            ui.label("Bloom");
            ui.add(DragValue::new(&mut settings.bloom).speed(0.01).range(0.0..=3.0));
            ui.end_row();

            ui.label("Beam");
            ui.add(DragValue::new(&mut settings.beam_intensity).speed(2.0).range(0.0..=4000.0));
            ui.end_row();

            ui.label("Steps");
            ui.add(DragValue::new(&mut settings.steps).speed(1.0).range(8..=192));
            ui.end_row();

            ui.label("Beam gizmo");
            ui.checkbox(&mut settings.show_beam_wireframes, "wireframe");
            ui.end_row();

            ui.label("Origin grid");
            ui.checkbox(&mut settings.show_grid, "show");
            ui.end_row();
        });
    ui.label(
        RichText::new("Beam look also follows the Fog Box density / anisotropy / tint.")
            .weak()
            .small(),
    );
}

/// Left tab: the content library — categorized fixtures and environments you
/// can add to the scene.
pub fn library_browser(
    ui: &mut egui::Ui,
    library: &mut Library,
    scene: &mut Scene,
    selection: &mut Selection,
    camera: &mut OrbitCamera,
) {
    ui.heading("Library");
    ui.separator();

    // Import a GDTF fixture file from disk.
    if ui
        .button("📁  Import GDTF…")
        .on_hover_text("Load a .gdtf fixture (real model, wheels, channels)")
        .clicked()
        && let Some(path) = rfd::FileDialog::new()
            .add_filter("GDTF fixture", &["gdtf"])
            .pick_file()
    {
        match library.import_gdtf(&path) {
            Ok(idx) => {
                let arc = library.gdtf[idx].clone();
                let fidx = scene.add_gdtf(arc, glam::Vec3::new(0.0, 4.0, 0.0));
                *selection = Selection::fixture(fidx);
            }
            Err(e) => log::error!("GDTF import failed: {e}"),
        }
    }

    // Import / export a full MVR scene (fixtures + stage/truss geometry).
    ui.horizontal(|ui| {
        if ui
            .button("📦  Import MVR…")
            .on_hover_text("Load an .mvr scene (fixtures + stage geometry) from a console or CAD tool")
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("MVR scene", &["mvr"])
                .pick_file()
        {
            match crate::mvr::MvrImport::load_path(&path) {
                Ok(import) => {
                    scene.import_mvr(import);
                    if let Some((center, radius)) = scene.scene_frame() {
                        camera.frame(center, radius * 1.15);
                    }
                    *selection = Selection::default();
                }
                Err(e) => log::error!("MVR import failed: {e}"),
            }
        }

        // Export is only meaningful once there's something to write.
        let can_export = !scene.fixtures.is_empty() || !scene.geometry.is_empty();
        if ui
            .add_enabled(can_export, egui::Button::new("💾  Export MVR…"))
            .on_hover_text("Write the current scene to an .mvr (fixtures, patch, placement, bundled GDTF + geometry)")
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("MVR scene", &["mvr"])
                .set_file_name("scene.mvr")
                .save_file()
        {
            match crate::mvr::export_path(scene, &path) {
                Ok(()) => log::info!("exported MVR: {}", path.display()),
                Err(e) => log::error!("MVR export failed: {e}"),
            }
        }
    });

    // Imported GDTF fixtures (click + to add another instance).
    if !library.gdtf.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("IMPORTED").small().strong());
        for i in 0..library.gdtf.len() {
            let (manuf, name) = {
                let g = &library.gdtf[i];
                (g.manufacturer.clone(), g.name.clone())
            };
            ui.horizontal(|ui| {
                if ui.button("+").on_hover_text("Add to scene").clicked() {
                    let arc = library.gdtf[i].clone();
                    let fidx = scene.add_gdtf(arc, glam::Vec3::new(0.0, 4.0, 0.0));
                    *selection = Selection::fixture(fidx);
                }
                ui.label(format!("{manuf} · {name}"));
            });
        }
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(RichText::new("FIXTURES").small().strong());
    let mut last_category = "";
    for profile in &library.fixtures {
        if profile.category != last_category {
            ui.label(RichText::new(profile.category).weak().small());
            last_category = profile.category;
        }
        ui.horizontal(|ui| {
            if ui.button("+").on_hover_text("Add to scene").clicked() {
                let idx = scene.add_fixture(profile);
                *selection = Selection::fixture(idx);
            }
            ui.label(profile.name);
        });
    }

    ui.add_space(10.0);
    ui.label(RichText::new("ENVIRONMENTS").small().strong());
    last_category = "";
    for profile in &library.environments {
        if profile.category != last_category {
            ui.label(RichText::new(profile.category).weak().small());
            last_category = profile.category;
        }
        ui.horizontal(|ui| {
            if ui.button("+").on_hover_text("Add to scene").clicked() {
                let idx = scene.add_environment(profile);
                *selection = Selection::environment(idx);
            }
            ui.label(profile.name);
        });
    }
}

/// Right tab: editable parameters for the current selection. Edits flow
/// straight into the scene, so the viewport updates on the next frame.
pub fn inspector(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &Selection,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
) {
    ui.heading("Inspector");
    ui.separator();

    if let Some(env_id) = selection.environment {
        match scene.environments.get_mut(env_id) {
            Some(env) => environment_inspector(ui, env),
            None => {
                ui.label("Selection is no longer valid.");
            }
        }
        return;
    }

    // Keep only still-valid fixture indices.
    let ids: Vec<usize> = selection
        .fixtures
        .iter()
        .copied()
        .filter(|&i| i < scene.fixtures.len())
        .collect();
    match ids.as_slice() {
        [] => {
            ui.label("Nothing selected.");
        }
        [id] => {
            let fixture = &mut scene.fixtures[*id];
            if fixture.is_gdtf() {
                gdtf_inspector(ui, fixture, gdtf_textures);
            } else {
                fixture_inspector(ui, fixture);
            }
        }
        many => bulk_inspector(ui, scene, many),
    }
}

/// Bulk editor shown when several fixtures are selected: edits a shared property
/// on **all** of them at once (set-semantics, seeded from the first selected).
fn bulk_inspector(ui: &mut egui::Ui, scene: &mut Scene, ids: &[usize]) {
    let primary = ids[0];
    ui.label(
        RichText::new(format!("{} fixtures selected — bulk edit", ids.len()))
            .strong(),
    );
    ui.label(RichText::new("Edits apply to all selected.").weak().small());
    ui.separator();

    Grid::new("bulk-grid")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .striped(true)
        .show(ui, |ui| {
            let mut pan = scene.fixtures[primary].pan;
            ui.label("Pan");
            if ui.add(DragValue::new(&mut pan).speed(0.5).range(-270.0..=270.0).suffix("°")).changed() {
                for &i in ids {
                    scene.fixtures[i].pan = pan;
                }
            }
            ui.end_row();

            let mut tilt = scene.fixtures[primary].tilt;
            ui.label("Tilt");
            if ui.add(DragValue::new(&mut tilt).speed(0.5).range(-180.0..=180.0).suffix("°")).changed() {
                for &i in ids {
                    scene.fixtures[i].tilt = tilt;
                }
            }
            ui.end_row();

            let mut intensity = scene.fixtures[primary].intensity;
            ui.label("Intensity");
            if ui.add(Slider::new(&mut intensity, 0.0..=1.0)).changed() {
                for &i in ids {
                    scene.fixtures[i].intensity = intensity;
                }
            }
            ui.end_row();

            let mut color = scene.fixtures[primary].color;
            ui.label("Color");
            if ui.color_edit_button_rgb(&mut color).changed() {
                for &i in ids {
                    scene.fixtures[i].color = color;
                }
            }
            ui.end_row();
        });

    ui.add_space(6.0);
    ui.label(RichText::new("Nudge position (all)").small().strong());
    ui.horizontal(|ui| {
        let mut delta = glam::Vec3::ZERO;
        // Drag from zero applies a delta; the field snaps back each frame.
        for (axis, label) in [(0usize, "x"), (1, "y"), (2, "z")] {
            let mut v = 0.0f32;
            if ui.add(DragValue::new(&mut v).speed(0.05).prefix(format!("{label} "))).changed() {
                delta[axis] += v;
            }
        }
        if delta != glam::Vec3::ZERO {
            for &i in ids {
                scene.fixtures[i].position += delta;
            }
        }
    });

    // Shared optical controls (applied to every selected fixture).
    egui::CollapsingHeader::new("Optics (all selected)")
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("bulk-optics")
                .num_columns(2)
                .spacing([10.0, 5.0])
                .striped(true)
                .show(ui, |ui| {
                    bulk_opt(ui, scene, ids, "Dimmer", |o| o.dimmer, |o, v| o.dimmer = v);
                    bulk_opt(ui, scene, ids, "Zoom", |o| o.zoom, |o, v| o.zoom = v);
                    bulk_opt(ui, scene, ids, "Focus", |o| o.focus, |o, v| o.focus = v);
                    bulk_opt(ui, scene, ids, "Iris", |o| o.iris, |o, v| o.iris = v);
                    bulk_opt(ui, scene, ids, "Frost", |o| o.frost, |o, v| o.frost = v);
                    bulk_opt(ui, scene, ids, "CTO", |o| o.cto, |o, v| o.cto = v);
                    bulk_opt(ui, scene, ids, "Cyan", |o| o.cmy[0], |o, v| o.cmy[0] = v);
                    bulk_opt(ui, scene, ids, "Magenta", |o| o.cmy[1], |o, v| o.cmy[1] = v);
                    bulk_opt(ui, scene, ids, "Yellow", |o| o.cmy[2], |o, v| o.cmy[2] = v);
                    bulk_opt(ui, scene, ids, "Color wheel", |o| o.color, |o, v| o.color = v);
                    bulk_opt(ui, scene, ids, "Gobo 1", |o| o.gobo1, |o, v| o.gobo1 = v);
                    bulk_opt(ui, scene, ids, "Gobo 1 spin", |o| o.gobo1_rot, |o, v| o.gobo1_rot = v);
                    bulk_opt(ui, scene, ids, "Prism 1", |o| o.prism1, |o, v| o.prism1 = v);
                    bulk_opt(ui, scene, ids, "Prism 1 spin", |o| o.prism1_rot, |o, v| o.prism1_rot = v);
                    bulk_opt(ui, scene, ids, "Shutter", |o| o.shutter, |o, v| o.shutter = v);
                    bulk_opt(ui, scene, ids, "Chromatic ab.", |o| o.ca, |o, v| o.ca = v);
                });
        });
}

/// One bulk optics slider (0..1) seeded from the primary, written to all `ids`.
fn bulk_opt(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    ids: &[usize],
    label: &str,
    get: impl Fn(&OpticalControls) -> f32,
    set: impl Fn(&mut OpticalControls, f32),
) {
    let mut v = get(&scene.fixtures[ids[0]].optics);
    ui.label(label);
    if ui.add(Slider::new(&mut v, 0.0..=1.0)).changed() {
        for &i in ids {
            set(&mut scene.fixtures[i].optics, v);
        }
    }
    ui.end_row();
}

fn fixture_inspector(ui: &mut egui::Ui, fixture: &mut Fixture) {
    Grid::new("fixture-grid")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label("Name");
            ui.label(fixture.name.as_str());
            ui.end_row();

            ui.label("Profile");
            ui.label(format!("{} · {}", fixture.category, fixture.profile));
            ui.end_row();

            ui.label("Pan");
            ui.add(
                DragValue::new(&mut fixture.pan)
                    .speed(0.5)
                    .range(-270.0..=270.0)
                    .suffix("°"),
            );
            ui.end_row();

            ui.label("Tilt");
            ui.add(
                DragValue::new(&mut fixture.tilt)
                    .speed(0.5)
                    .range(-180.0..=180.0)
                    .suffix("°"),
            );
            ui.end_row();

            ui.label("Intensity");
            ui.add(
                DragValue::new(&mut fixture.intensity)
                    .speed(0.005)
                    .range(0.0..=1.0),
            );
            ui.end_row();

            ui.label("Beam");
            ui.add(
                DragValue::new(&mut fixture.beam_angle)
                    .speed(0.2)
                    .range(2.0..=90.0)
                    .suffix("°"),
            );
            ui.end_row();

            ui.label("Color");
            ui.color_edit_button_rgb(&mut fixture.color);
            ui.end_row();

            ui.label("Position");
            ui.horizontal(|ui| {
                ui.add(DragValue::new(&mut fixture.position.x).speed(0.05).prefix("x "));
                ui.add(DragValue::new(&mut fixture.position.y).speed(0.05).prefix("y "));
                ui.add(DragValue::new(&mut fixture.position.z).speed(0.05).prefix("z "));
            });
            ui.end_row();
        });
}

fn environment_inspector(ui: &mut egui::Ui, env: &mut Environment) {
    Grid::new("environment-grid")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label("Name");
            ui.label(env.name.as_str());
            ui.end_row();

            ui.label("Type");
            ui.label(format!("{:?}", env.kind));
            ui.end_row();

            ui.label("Center");
            ui.horizontal(|ui| {
                ui.add(DragValue::new(&mut env.center.x).speed(0.1).prefix("x "));
                ui.add(DragValue::new(&mut env.center.y).speed(0.1).prefix("y "));
                ui.add(DragValue::new(&mut env.center.z).speed(0.1).prefix("z "));
            });
            ui.end_row();

            ui.label("Size");
            ui.horizontal(|ui| {
                ui.add(DragValue::new(&mut env.size.x).speed(0.1).range(0.1..=500.0).prefix("w "));
                ui.add(DragValue::new(&mut env.size.y).speed(0.1).range(0.1..=500.0).prefix("h "));
                ui.add(DragValue::new(&mut env.size.z).speed(0.1).range(0.1..=500.0).prefix("d "));
            });
            ui.end_row();

            ui.label("Density");
            ui.add(DragValue::new(&mut env.density).speed(0.005).range(0.0..=4.0));
            ui.end_row();

            ui.label("Anisotropy");
            ui.add(
                DragValue::new(&mut env.anisotropy)
                    .speed(0.005)
                    .range(-0.95..=0.95),
            )
            .on_hover_text("Henyey-Greenstein g (forward scattering > 0)");
            ui.end_row();

            ui.label("Tint");
            ui.color_edit_button_rgb(&mut env.color);
            ui.end_row();
        });
}

/// Inspector for an imported GDTF fixture: identity + thumbnail, editable
/// instance params, wheels (with slot images), and the DMX modes/channels.
fn gdtf_inspector(
    ui: &mut egui::Ui,
    fixture: &mut Fixture,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
) {
    let gdtf = fixture.gdtf.clone().expect("gdtf");
    let key = Arc::as_ptr(&gdtf) as usize;
    let tex = gdtf_textures
        .entry(key)
        .or_insert_with(|| load_gdtf_textures(ui.ctx(), &gdtf));

    ui.heading(gdtf.name.as_str());
    ui.label(
        RichText::new(format!("{} · {}", gdtf.manufacturer, gdtf.long_name))
            .weak()
            .small(),
    );

    if let Some(thumb) = &tex.thumbnail {
        let s = thumb.size_vec2();
        let w = 200.0_f32.min(ui.available_width());
        let h = w * s.y / s.x.max(1.0);
        ui.add_space(4.0);
        ui.image((thumb.id(), egui::vec2(w, h)));
    }

    // Physical source / beam spec from the GDTF Beam geometry.
    let b = &gdtf.beam;
    ui.label(
        RichText::new(format!(
            "{} engine · {:.0} K · CRI {:.0} · {:.0} lm · {:.0} W",
            b.lamp_type, b.color_temp, b.cri, b.luminous_flux, b.power
        ))
        .weak()
        .small(),
    );
    ui.label(
        RichText::new(format!(
            "{} · beam {:.0}° / field {:.0}° · throw {:.2}",
            b.beam_type, b.beam_angle, b.field_angle, b.throw_ratio
        ))
        .weak()
        .small(),
    );

    // MVR patch identity (FixtureID, DMX address, mode) when imported from a scene.
    if let Some(m) = fixture.mvr.as_deref() {
        let id = if m.fixture_id.is_empty() { "—" } else { m.fixture_id.as_str() };
        let addr = m
            .addresses
            .first()
            .map(|a| format!("{}.{:03}", a.universe(), a.channel()))
            .unwrap_or_else(|| "—".into());
        let mode = if m.gdtf_mode.is_empty() { "—" } else { m.gdtf_mode.as_str() };
        ui.label(
            RichText::new(format!("MVR · ID {id} · addr {addr} · {mode}"))
                .weak()
                .small(),
        )
        .on_hover_text("Fixture ID · DMX universe.channel · mode, from the imported MVR patch");
    }

    ui.separator();
    Grid::new("gdtf-params")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label("Pan");
            ui.add(DragValue::new(&mut fixture.pan).speed(0.5).range(-270.0..=270.0).suffix("°"));
            ui.end_row();
            ui.label("Tilt");
            ui.add(DragValue::new(&mut fixture.tilt).speed(0.5).range(-135.0..=135.0).suffix("°"));
            ui.end_row();
            ui.label("Intensity");
            ui.add(DragValue::new(&mut fixture.intensity).speed(0.005).range(0.0..=1.0));
            ui.end_row();
            ui.label("Color");
            ui.color_edit_button_rgb(&mut fixture.color);
            ui.end_row();
            ui.label("Position");
            ui.horizontal(|ui| {
                ui.add(DragValue::new(&mut fixture.position.x).speed(0.05).prefix("x "));
                ui.add(DragValue::new(&mut fixture.position.y).speed(0.05).prefix("y "));
                ui.add(DragValue::new(&mut fixture.position.z).speed(0.05).prefix("z "));
            });
            ui.end_row();
        });

    optics_section(ui, fixture, &gdtf);

    egui::CollapsingHeader::new(format!("Wheels ({})", gdtf.wheels.len()))
        .default_open(false)
        .show(ui, |ui| {
            for (wi, wheel) in gdtf.wheels.iter().enumerate() {
                ui.label(RichText::new(&wheel.name).strong().small());
                ui.horizontal_wrapped(|ui| {
                    for (si, slot) in wheel.slots.iter().enumerate() {
                        let handle = tex
                            .wheels
                            .get(wi)
                            .and_then(|w| w.get(si))
                            .and_then(|h| h.as_ref());
                        let size = egui::vec2(42.0, 42.0);
                        if let Some(h) = handle {
                            ui.image((h.id(), size)).on_hover_text(&slot.name);
                        } else {
                            let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
                            let col = slot
                                .color
                                .map(|c| {
                                    egui::Color32::from_rgb(
                                        (c[0] * 255.0) as u8,
                                        (c[1] * 255.0) as u8,
                                        (c[2] * 255.0) as u8,
                                    )
                                })
                                .unwrap_or(egui::Color32::from_gray(40));
                            ui.painter().rect_filled(rect, 4.0, col);
                            resp.on_hover_text(&slot.name);
                        }
                    }
                });
                ui.add_space(4.0);
            }
        });

    egui::CollapsingHeader::new(format!("DMX modes ({})", gdtf.modes.len()))
        .show(ui, |ui| {
            for mode in &gdtf.modes {
                egui::CollapsingHeader::new(format!("{} — {} ch", mode.name, mode.footprint))
                    .id_salt(&mode.name)
                    .show(ui, |ui| {
                        Grid::new(format!("dmx-{}", mode.name))
                            .num_columns(3)
                            .striped(true)
                            .spacing([10.0, 2.0])
                            .show(ui, |ui| {
                                ui.strong("Addr");
                                ui.strong("Attribute");
                                ui.strong("Function");
                                ui.end_row();
                                for ch in &mode.channels {
                                    let addr = ch
                                        .offsets
                                        .first()
                                        .map(|o| o.to_string())
                                        .unwrap_or_else(|| "—".into());
                                    ui.monospace(addr);
                                    ui.label(&ch.attribute);
                                    ui.label(&ch.function);
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
}

/// The optical-chain control bank for a GDTF fixture: sliders for every stage
/// the fixture actually exposes (disabled if the GDTF lacks that attribute).
/// Drives `fixture.optics`, which the renderer resolves into the beam each frame.
fn optics_section(ui: &mut egui::Ui, fixture: &mut Fixture, gdtf: &GdtfFixture) {
    let beam_angle = fixture.beam_angle;
    let o = &mut fixture.optics;
    let has = |a: &str| gdtf.has_attribute(a);

    egui::CollapsingHeader::new("Optics")
        .default_open(true)
        .show(ui, |ui| {
            ui.label(RichText::new("BEAM SHAPING").small().strong());
            Grid::new("optics-beam").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                ui.label("Dimmer");
                ui.add(Slider::new(&mut o.dimmer, 0.0..=1.0));
                ui.end_row();

                let zoom_deg = optics::map_attr(gdtf, "Zoom", o.zoom, (beam_angle, beam_angle));
                ui.label("Zoom");
                ui.add_enabled(has("Zoom"), Slider::new(&mut o.zoom, 0.0..=1.0).text(format!("{zoom_deg:.0}°")));
                ui.end_row();

                ui.label("Focus");
                ui.add_enabled(has("Focus1"), Slider::new(&mut o.focus, 0.0..=1.0));
                ui.end_row();

                ui.label("Iris");
                ui.add_enabled(has("Iris"), Slider::new(&mut o.iris, 0.0..=1.0));
                ui.end_row();

                ui.label("Frost");
                ui.add_enabled(has("Frost1") || has("Frost2"), Slider::new(&mut o.frost, 0.0..=1.0));
                ui.end_row();

                ui.label("Chromatic ab.");
                ui.add(Slider::new(&mut o.ca, 0.0..=1.0));
                ui.end_row();
            });

            ui.add_space(4.0);
            ui.label(RichText::new("COLOR").small().strong());
            Grid::new("optics-color").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                ui.label("CTO (warm)");
                ui.add_enabled(has("CTO"), Slider::new(&mut o.cto, 0.0..=1.0));
                ui.end_row();
                let cmy = has("ColorSub_C") || has("ColorSub_M") || has("ColorSub_Y");
                ui.label("Cyan");
                ui.add_enabled(cmy, Slider::new(&mut o.cmy[0], 0.0..=1.0));
                ui.end_row();
                ui.label("Magenta");
                ui.add_enabled(cmy, Slider::new(&mut o.cmy[1], 0.0..=1.0));
                ui.end_row();
                ui.label("Yellow");
                ui.add_enabled(cmy, Slider::new(&mut o.cmy[2], 0.0..=1.0));
                ui.end_row();
                ui.label("Color wheel");
                ui.add_enabled(has("Color1"), Slider::new(&mut o.color, 0.0..=1.0));
                ui.end_row();
                ui.label("Color spin");
                ui.add_enabled(has("Color1"), Slider::new(&mut o.color_spin, 0.0..=1.0).text("0.5=stop"));
                ui.end_row();
            });

            ui.add_space(4.0);
            ui.label(RichText::new("CONTENT").small().strong());
            Grid::new("optics-content").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                ui.label("Gobo 1");
                ui.add_enabled(has("Gobo1"), Slider::new(&mut o.gobo1, 0.0..=1.0));
                ui.end_row();
                ui.label("Gobo 1 index");
                ui.add_enabled(has("Gobo1"), Slider::new(&mut o.gobo1_index, 0.0..=1.0));
                ui.end_row();
                ui.label("Gobo 1 spin");
                ui.add_enabled(has("Gobo1"), Slider::new(&mut o.gobo1_rot, 0.0..=1.0).text("0.5=stop"));
                ui.end_row();
                ui.label("Gobo 2");
                ui.add_enabled(has("Gobo2"), Slider::new(&mut o.gobo2, 0.0..=1.0));
                ui.end_row();
                ui.label("Gobo 2 spin");
                ui.add_enabled(has("Gobo2"), Slider::new(&mut o.gobo2_rot, 0.0..=1.0).text("0.5=stop"));
                ui.end_row();
                ui.label("Animation");
                ui.add_enabled(has("AnimationWheel1"), Slider::new(&mut o.anim, 0.0..=1.0));
                ui.end_row();
                ui.label("Anim. spin");
                ui.add_enabled(has("AnimationWheel1"), Slider::new(&mut o.anim_spin, 0.0..=1.0).text("0.5=stop"));
                ui.end_row();
                ui.label("Prism 1");
                ui.add_enabled(has("Prism1"), Slider::new(&mut o.prism1, 0.0..=1.0));
                ui.end_row();
                ui.label("Prism 1 spin");
                ui.add_enabled(has("Prism1"), Slider::new(&mut o.prism1_rot, 0.0..=1.0).text("0.5=stop"));
                ui.end_row();
                ui.label("Prism 2");
                ui.add_enabled(has("Prism2"), Slider::new(&mut o.prism2, 0.0..=1.0));
                ui.end_row();
                ui.label("Shutter");
                ui.add_enabled(has("Shutter1"), Slider::new(&mut o.shutter, 0.0..=1.0));
                ui.end_row();
                ui.label("Strobe");
                ui.add_enabled(has("Shutter1"), Slider::new(&mut o.strobe, 0.0..=1.0));
                ui.end_row();
            });
        });
}

fn load_gdtf_textures(ctx: &egui::Context, gdtf: &GdtfFixture) -> GdtfTextures {
    let thumbnail = gdtf
        .thumbnail
        .as_ref()
        .and_then(|b| decode_texture(ctx, "gdtf-thumb", b));
    let wheels = gdtf
        .wheels
        .iter()
        .map(|w| {
            w.slots
                .iter()
                .map(|s| {
                    s.media
                        .as_ref()
                        .and_then(|b| decode_texture(ctx, &s.name, b))
                })
                .collect()
        })
        .collect();
    GdtfTextures { thumbnail, wheels }
}

fn decode_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    // Downscale large wheel/thumbnail images for the panel.
    let img = img.thumbnail(256, 256);
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

/// Central tab: the 3D scene, rendered offscreen and shown as a texture.
/// Drag to orbit, shift+drag to pan, scroll to zoom, click to select, `d` to
/// duplicate the selected fixture.
#[allow(clippy::too_many_arguments)]
pub fn viewport(
    ui: &mut egui::Ui,
    camera: &mut OrbitCamera,
    scene: &Scene,
    selection: &mut Selection,
    viewport_focused: &mut bool,
    duplicate: &mut Option<DuplicateDialog>,
    texture: egui::TextureId,
    requested_px: &mut (u32, u32),
    fps: f32,
) {
    let available = ui.available_size();
    let ppp = ui.pixels_per_point();

    *requested_px = (
        (available.x * ppp).round().max(1.0) as u32,
        (available.y * ppp).round().max(1.0) as u32,
    );

    let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
    ui.painter().image(
        texture,
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    // Focus follows the most recent pointer press: inside the viewport focuses
    // it, anywhere else releases it (so the `d` shortcut only fires in here).
    if ui.input(|i| i.pointer.any_pressed())
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        *viewport_focused = rect.contains(p);
    }

    if response.dragged() {
        let delta = response.drag_delta();
        if ui.input(|i| i.modifiers.shift) {
            camera.pan(delta.x, delta.y);
        } else {
            camera.orbit(delta.x, delta.y);
        }
    }
    if response.contains_pointer() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            camera.zoom(scroll * 0.01);
        }
    }

    // Click to select: cast a ray through the cursor and pick the nearest object.
    // ⌘/Ctrl-click toggles a fixture into a multi-selection (shift is pan here).
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let aspect = rect.width() / rect.height().max(1.0);
        let (ro, rd) = camera.ray(ndc, aspect);
        let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        match pick(scene, ro, rd) {
            Some(Hit::Fixture(i)) if multi => selection.toggle_fixture(i),
            Some(Hit::Fixture(i)) => *selection = Selection::fixture(i),
            Some(Hit::Environment(i)) => *selection = Selection::environment(i),
            None if !multi => *selection = Selection::default(),
            None => {}
        }
    }

    // `d` opens the Duplicate dialog for the selected fixture.
    if *viewport_focused
        && duplicate.is_none()
        && ui.input(|i| i.key_pressed(egui::Key::D))
        && let Some(idx) = selection.primary_fixture()
    {
        *duplicate = Some(DuplicateDialog {
            fixture: idx,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            y_angle: 36.0,
            count: 9,
        });
    }

    // Focus border.
    if *viewport_focused {
        ui.painter().rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(90, 170, 255)),
            egui::StrokeKind::Inside,
        );
    }

    ui.painter().text(
        rect.left_bottom() + egui::vec2(8.0, -6.0),
        egui::Align2::LEFT_BOTTOM,
        "drag: orbit · shift+drag: pan · scroll: zoom · click: select · d: duplicate",
        egui::FontId::proportional(11.0),
        egui::Color32::from_white_alpha(110),
    );

    // FPS HUD (top-left), color-coded.
    let color = if fps >= 55.0 {
        egui::Color32::from_rgb(120, 230, 120)
    } else if fps >= 30.0 {
        egui::Color32::from_rgb(235, 215, 110)
    } else {
        egui::Color32::from_rgb(235, 120, 110)
    };
    ui.painter().text(
        rect.left_top() + egui::vec2(8.0, 6.0),
        egui::Align2::LEFT_TOP,
        format!("{fps:.0} fps"),
        egui::FontId::monospace(13.0),
        color,
    );
}

/// Bottom tab: a stub DMX patch table. Live sACN / Art-Net input isn't wired
/// up yet; this previews the channels each fixture would occupy.
pub fn dmx_monitor(ui: &mut egui::Ui, scene: &Scene) {
    ui.horizontal(|ui| {
        ui.heading("DMX Monitor");
        ui.label(RichText::new("stub").small().weak());
    });
    ui.label(
        RichText::new(
            "Live DMX input (sACN / Art-Net) is not wired up yet — \
             this previews the patch each fixture would occupy.",
        )
        .small()
        .weak(),
    );
    ui.separator();

    Grid::new("dmx-grid")
        .num_columns(8)
        .striped(true)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            for header in ["Fixture", "Addr", "Pan", "Tilt", "Dim", "R", "G", "B"] {
                ui.strong(header);
            }
            ui.end_row();

            let mut address = 1u32;
            for fixture in &scene.fixtures {
                ui.label(fixture.name.as_str());
                ui.monospace(format!("{address:>3}"));
                ui.monospace(format!("{:>3}", angle_to_dmx(fixture.pan, 540.0)));
                ui.monospace(format!("{:>3}", angle_to_dmx(fixture.tilt, 360.0)));
                ui.monospace(format!("{:>3}", unit_to_dmx(fixture.intensity)));
                ui.monospace(format!("{:>3}", unit_to_dmx(fixture.color[0])));
                ui.monospace(format!("{:>3}", unit_to_dmx(fixture.color[1])));
                ui.monospace(format!("{:>3}", unit_to_dmx(fixture.color[2])));
                ui.end_row();

                address += Fixture::DMX_FOOTPRINT;
            }
        });
}

/// What a viewport ray hit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Fixture(usize),
    Environment(usize),
}

/// Pick the object a world-space ray hits. Fixtures take priority (so you can
/// always click a head even when it sits inside the fog box); only if none is
/// hit do we test the environment volumes.
fn pick(scene: &Scene, ro: Vec3, rd: Vec3) -> Option<Hit> {
    let mut best: Option<(f32, usize)> = None;
    for (i, f) in scene.fixtures.iter().enumerate() {
        // Bounding sphere around the head; a bit generous so it's easy to click.
        if let Some(t) = ray_sphere(ro, rd, f.position, 0.5)
            && best.is_none_or(|(bt, _)| t < bt)
        {
            best = Some((t, i));
        }
    }
    if let Some((_, i)) = best {
        return Some(Hit::Fixture(i));
    }
    let mut env: Option<(f32, usize)> = None;
    for (i, e) in scene.environments.iter().enumerate() {
        if let Some(t) = ray_aabb(ro, rd, e.min(), e.max())
            && env.is_none_or(|(bt, _)| t < bt)
        {
            env = Some((t, i));
        }
    }
    env.map(|(_, i)| Hit::Environment(i))
}

/// Nearest positive ray–sphere intersection distance, if any.
fn ray_sphere(ro: Vec3, rd: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let oc = ro - center;
    let b = oc.dot(rd);
    let c = oc.dot(oc) - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let t = -b - s;
    if t > 0.0 {
        Some(t)
    } else {
        let t2 = -b + s;
        (t2 > 0.0).then_some(t2)
    }
}

/// Nearest positive ray–AABB intersection distance (slab test), if any.
fn ray_aabb(ro: Vec3, rd: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = rd.recip(); // inf for parallel components is fine
    let t0 = (min - ro) * inv;
    let t1 = (max - ro) * inv;
    let tmin = t0.min(t1);
    let tmax = t0.max(t1);
    let near = tmin.x.max(tmin.y).max(tmin.z);
    let far = tmax.x.min(tmax.y).min(tmax.z);
    if far < near.max(0.0) {
        return None;
    }
    Some(if near > 0.0 { near } else { far })
}

fn unit_to_dmx(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod pick_tests {
    use super::*;

    #[test]
    fn ray_sphere_front_and_back() {
        let ro = Vec3::new(0.0, 0.0, -5.0);
        let rd = Vec3::new(0.0, 0.0, 1.0);
        let t = ray_sphere(ro, rd, Vec3::ZERO, 1.0).expect("hit");
        assert!((t - 4.0).abs() < 1e-3);
        // Sphere behind the ray origin: no hit.
        assert!(ray_sphere(Vec3::new(0.0, 0.0, 5.0), rd, Vec3::ZERO, 1.0).is_none());
        // Ray missing the sphere sideways.
        assert!(ray_sphere(ro, rd, Vec3::new(3.0, 0.0, 0.0), 1.0).is_none());
    }

    #[test]
    fn ray_aabb_hit() {
        let t = ray_aabb(
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::splat(-1.0),
            Vec3::splat(1.0),
        )
        .expect("hit");
        assert!((t - 4.0).abs() < 1e-3);
    }

    #[test]
    fn pick_prefers_fixture_over_fog_box() {
        // Demo scene: one fixture at (0,4,0) inside a large fog box.
        let scene = Scene::demo();
        let f = scene.fixtures[0].position;
        // Ray from in front of the fixture, aimed at it.
        let ro = f + Vec3::new(0.0, 0.0, 6.0);
        let rd = (f - ro).normalize();
        assert_eq!(pick(&scene, ro, rd), Some(Hit::Fixture(0)));
    }
}

/// Map a symmetric angle range (±span/2) onto a single 8-bit DMX channel.
fn angle_to_dmx(angle_deg: f32, span_deg: f32) -> u8 {
    let normalized = (angle_deg / span_deg + 0.5).clamp(0.0, 1.0);
    (normalized * 255.0).round() as u8
}
