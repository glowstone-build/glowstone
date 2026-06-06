//! The individual dock panels. Each is a plain function taking the egui `Ui`
//! plus whatever scene state it reads or edits.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{DragValue, Grid, RichText, Sense};

use super::GdtfTextures;
use crate::gdtf::GdtfFixture;
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
    for (i, fixture) in scene.fixtures.iter().enumerate() {
        let label = format!("{}  ·  {}", fixture.name, fixture.profile);
        ui.selectable_value(selection, Selection::Fixture(i), label);
    }

    ui.add_space(8.0);
    ui.label(RichText::new("ENVIRONMENTS").small().strong());
    if scene.environments.is_empty() {
        ui.label(RichText::new("none — add from the Library").weak().small());
    }
    for (i, env) in scene.environments.iter().enumerate() {
        ui.selectable_value(selection, Selection::Environment(i), env.name.as_str());
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
            ui.add(DragValue::new(&mut settings.beam_intensity).speed(0.1).range(0.0..=64.0));
            ui.end_row();

            ui.label("Steps");
            ui.add(DragValue::new(&mut settings.steps).speed(1.0).range(8..=192));
            ui.end_row();

            ui.label("Beam gizmo");
            ui.checkbox(&mut settings.show_beam_wireframes, "wireframe");
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
                *selection = Selection::Fixture(fidx);
            }
            Err(e) => log::error!("GDTF import failed: {e}"),
        }
    }

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
                    *selection = Selection::Fixture(fidx);
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
                *selection = Selection::Fixture(idx);
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
                *selection = Selection::Environment(idx);
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
    selection: Selection,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
) {
    ui.heading("Inspector");
    ui.separator();

    match selection {
        Selection::None => {
            ui.label("Nothing selected.");
        }
        Selection::Fixture(id) => match scene.fixtures.get_mut(id) {
            Some(fixture) if fixture.is_gdtf() => gdtf_inspector(ui, fixture, gdtf_textures),
            Some(fixture) => fixture_inspector(ui, fixture),
            None => {
                ui.label("Selection is no longer valid.");
            }
        },
        Selection::Environment(id) => match scene.environments.get_mut(id) {
            Some(env) => environment_inspector(ui, env),
            None => {
                ui.label("Selection is no longer valid.");
            }
        },
    }
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

    egui::CollapsingHeader::new(format!("Wheels ({})", gdtf.wheels.len()))
        .default_open(true)
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
/// Drag to orbit, shift+drag to pan, scroll to zoom.
pub fn viewport(
    ui: &mut egui::Ui,
    camera: &mut OrbitCamera,
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

    ui.painter().text(
        rect.left_bottom() + egui::vec2(8.0, -6.0),
        egui::Align2::LEFT_BOTTOM,
        "drag: orbit   ·   shift+drag: pan   ·   scroll: zoom",
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

fn unit_to_dmx(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Map a symmetric angle range (±span/2) onto a single 8-bit DMX channel.
fn angle_to_dmx(angle_deg: f32, span_deg: f32) -> u8 {
    let normalized = (angle_deg / span_deg + 0.5).clamp(0.0, 1.0);
    (normalized * 255.0).round() as u8
}
