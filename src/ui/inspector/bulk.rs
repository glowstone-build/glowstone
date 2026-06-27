//! Bulk (multi-fixture) inspector editor. Pure code move out of [`super`] (the
//! inspector `mod.rs`): the editor shown when several fixtures are selected.

use super::theme;
use super::{bulk_f32_row, category, common_f32, common_rgb, InspectorState};
use crate::dmx::PatchTable;
use crate::gdtf::WheelKind;
use crate::optics::OpticField;
use crate::scene::Scene;
use egui::{DragValue, Grid, RichText, Slider};

/// Bulk editor shown when several fixtures are selected: edits a shared property
/// on **all** of them at once (set-semantics, seeded from the first selected).
/// Categories are collapsible and the Optics / Wheels rows are **dynamic** — they
/// show the union of controls the selected fixtures actually expose, not a fixed
/// hardcoded list.
pub(super) fn bulk_inspector(ui: &mut egui::Ui, scene: &mut Scene, patch: &mut PatchTable, ids: &[usize], state: &mut InspectorState) {
    let primary = ids[0];
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  {} fixtures", theme::icon::FIXTURE, ids.len())).strong());
    });
    ui.label(RichText::new("Bulk edit — changes apply to all selected.").weak().small());
    ui.separator();

    // --- DMX MODE (only when every selected fixture shares one profile, so a
    // single mode list applies to all). Drives the patch footprint; decode syncs
    // each fixture's active mode from the patch next frame.
    let p0 = scene.fixtures[primary].profile.clone();
    let same_profile = ids.iter().all(|&i| scene.fixtures[i].profile == p0);
    let ref_modes: Vec<String> = if same_profile {
        scene.fixtures[primary]
            .gdtf
            .as_ref()
            .map(|g| g.modes.iter().map(|m| m.name.clone()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if ref_modes.len() > 1 {
        let cur = patch.get(primary).map(|p| p.mode_index).unwrap_or(0);
        let cur_name = ref_modes.get(cur).cloned().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("DMX mode");
            let mut pick = None;
            egui::ComboBox::from_id_salt("bulk-mode")
                .selected_text(RichText::new(cur_name).small())
                .show_ui(ui, |ui| {
                    for (mi, name) in ref_modes.iter().enumerate() {
                        if ui.selectable_label(mi == cur, name).clicked() {
                            pick = Some(mi);
                        }
                    }
                });
            if let Some(mi) = pick {
                for &i in ids {
                    let f = &scene.fixtures[i];
                    if f.gdtf.as_ref().is_some_and(|g| mi < g.modes.len()) {
                        patch.set_mode(f, i, mi);
                    }
                }
            }
        });
        ui.separator();
    }

    // --- TRANSFORM ---
    category(
        ui,
        state,
        "Transform",
        format!("{}  Transform", theme::icon::INSPECTOR),
        true,
        &["Pan", "Tilt", "Nudge position", "Nudge rotation"],
        |ui, fs| {
            Grid::new("bulk-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                let pan = common_f32(ids.iter().map(|&i| scene.fixtures[i].pan));
                bulk_f32_row(
                    ui,
                    fs,
                    "Pan",
                    pan,
                    scene.fixtures[primary].pan,
                    |ui, v| ui.add(DragValue::new(v).speed(0.5).range(-270.0..=270.0).suffix("°")),
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].pan = v),
                );
                let tilt = common_f32(ids.iter().map(|&i| scene.fixtures[i].tilt));
                bulk_f32_row(
                    ui,
                    fs,
                    "Tilt",
                    tilt,
                    scene.fixtures[primary].tilt,
                    |ui, v| ui.add(DragValue::new(v).speed(0.5).range(-180.0..=180.0).suffix("°")),
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].tilt = v),
                );
            });
            if fs.row_visible("Nudge position") {
                ui.add_space(4.0);
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
            }
            if fs.row_visible("Nudge rotation") {
                ui.add_space(4.0);
                ui.label(RichText::new("Nudge rotation (all · individual origins)").small().strong());
                ui.horizontal(|ui| {
                    // Drag from zero applies a delta rotation about each fixture's OWN
                    // origin (orientation only), so the rig keeps its arrangement and
                    // each head tilts in place — never collapsed onto one pivot.
                    let mut d = glam::Vec3::ZERO; // euler degrees (x, y, z)
                    for (axis, label) in [(0usize, "x"), (1, "y"), (2, "z")] {
                        let mut v = 0.0f32;
                        if ui
                            .add(DragValue::new(&mut v).speed(0.5).suffix("°").prefix(format!("{label} ")))
                            .changed()
                        {
                            d[axis] += v;
                        }
                    }
                    if d != glam::Vec3::ZERO {
                        let delta = glam::Quat::from_euler(
                            glam::EulerRot::YXZ,
                            d.y.to_radians(),
                            d.x.to_radians(),
                            d.z.to_radians(),
                        );
                        for &i in ids {
                            scene.fixtures[i].orientation =
                                (delta * scene.fixtures[i].orientation).normalize();
                        }
                    }
                });
            }
        },
    );

    // --- FIXTURE ---
    category(
        ui,
        state,
        "Fixture",
        format!("{}  Fixture", theme::icon::COLOR),
        true,
        &["Dimmer", "Beam", "Color"],
        |ui, fs| {
            Grid::new("bulk-fixture").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                let dimmer = common_f32(ids.iter().map(|&i| scene.fixtures[i].optics.dimmer));
                bulk_f32_row(
                    ui,
                    fs,
                    "Dimmer",
                    dimmer,
                    scene.fixtures[primary].optics.dimmer,
                    |ui, v| ui.add(Slider::new(v, 0.0..=1.0)),
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].optics.dimmer = v),
                );
                let beam = common_f32(ids.iter().map(|&i| scene.fixtures[i].beam));
                bulk_f32_row(
                    ui,
                    fs,
                    "Beam",
                    beam,
                    scene.fixtures[primary].beam,
                    |ui, v| {
                        ui.add(Slider::new(v, 0.0..=4.0).text("vol"))
                            .on_hover_text("Volumetric beam intensity (0 = off)")
                    },
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].beam = v),
                );
                // Colour: same mixed/unify pattern, but the widget is a colour well.
                if fs.row_visible("Color") {
                    ui.label("Color");
                    match common_rgb(ids.iter().map(|&i| scene.fixtures[i].color)) {
                        Some(mut color) => {
                            if ui.color_edit_button_rgb(&mut color).changed() {
                                for &i in ids {
                                    scene.fixtures[i].color = color;
                                }
                            }
                        }
                        None => {
                            let seed = scene.fixtures[primary].color;
                            if ui
                                .add(egui::Button::new(RichText::new("— Multiple —").small().weak()))
                                .on_hover_text("Colours differ — click to set all to the active colour")
                                .clicked()
                            {
                                for &i in ids {
                                    scene.fixtures[i].color = seed;
                                }
                            }
                        }
                    }
                    ui.end_row();
                }
            });
        },
    );

    // --- OPTICS (dynamic): only fields some selected fixture actually exposes ---
    let supports = |f: OpticField| {
        ids.iter().any(|&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
    };
    let beam: Vec<OpticField> = OpticField::BEAM.into_iter().filter(|&f| supports(f)).collect();
    let color: Vec<OpticField> = OpticField::COLOR.into_iter().filter(|&f| supports(f)).collect();
    if !beam.is_empty() || !color.is_empty() {
        let optic_labels: Vec<&str> = beam.iter().chain(&color).map(|f| f.label()).collect();
        category(
            ui,
            state,
            "Optics",
            format!("{}  Optics", theme::icon::INSPECTOR),
            true,
            &optic_labels,
            |ui, fs| {
                if !beam.is_empty() && fs.category_visible(&beam.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                    ui.label(RichText::new("BEAM SHAPING").small().strong());
                    Grid::new("bulk-beam").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                        for f in &beam {
                            bulk_opt_field(ui, fs, scene, ids, *f);
                        }
                    });
                }
                if !color.is_empty() && fs.category_visible(&color.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                    ui.add_space(4.0);
                    ui.label(RichText::new("COLOR MIXING").small().strong());
                    Grid::new("bulk-color").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                        for f in &color {
                            bulk_opt_field(ui, fs, scene, ids, *f);
                        }
                    });
                }
            },
        );
    }

    // --- WHEELS (dynamic): the union of components across all selected fixtures ---
    let mut wheels: Vec<(WheelKind, u32, String)> = Vec::new();
    for &i in ids {
        let f = &scene.fixtures[i];
        if let Some(comps) = f.gdtf.as_ref().and_then(|g| g.modes.get(f.mode_index)).map(|m| &m.components) {
            for c in comps {
                if !wheels.iter().any(|(k, n, _)| *k == c.kind && *n == c.number) {
                    wheels.push((c.kind, c.number, c.attribute.clone()));
                }
            }
        }
    }
    if !wheels.is_empty() {
        let wheel_labels: Vec<&str> = wheels.iter().map(|(_, _, l)| l.as_str()).collect();
        category(
            ui,
            state,
            "Wheels",
            format!("{}  Wheels", theme::icon::COLOR),
            true,
            &wheel_labels,
            |ui, fs| {
                Grid::new("bulk-wheels").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for (kind, number, label) in &wheels {
                        bulk_wheel(ui, fs, scene, ids, *kind, *number, label);
                    }
                });
            },
        );
    }
}

/// Bulk rows for one wheel component: value + spin sliders applied to the
/// matching component of every selected fixture.
fn bulk_wheel(
    ui: &mut egui::Ui,
    state: &InspectorState,
    scene: &mut Scene,
    ids: &[usize],
    kind: WheelKind,
    number: u32,
    label: &str,
) {
    // Seed from the first selected fixture that actually has this wheel (the
    // union may include wheels the primary doesn't have).
    let Some((seed_value, seed_spin)) = ids
        .iter()
        .find_map(|&i| scene.fixtures[i].wheel_control_mut(kind, number).map(|w| (w.value, w.spin)))
    else {
        return;
    };
    // Mixed-value detection (#7) over only the fixtures that HAVE this wheel.
    let value = common_f32(
        ids.iter().filter_map(|&i| scene.fixtures[i].wheel_control_mut(kind, number).map(|w| w.value)),
    );
    bulk_f32_row(ui, state, label, value, seed_value, |ui, v| ui.add(Slider::new(v, 0.0..=1.0)), |v| {
        for &i in ids {
            if let Some(w) = scene.fixtures[i].wheel_control_mut(kind, number) {
                w.value = v;
            }
        }
    });
    let spin = common_f32(
        ids.iter().filter_map(|&i| scene.fixtures[i].wheel_control_mut(kind, number).map(|w| w.spin)),
    );
    bulk_f32_row(
        ui,
        state,
        &format!("{label} spin"),
        spin,
        seed_spin,
        |ui, v| ui.add(Slider::new(v, 0.0..=1.0).text("0.5=stop")),
        |v| {
            for &i in ids {
                if let Some(w) = scene.fixtures[i].wheel_control_mut(kind, number) {
                    w.spin = v;
                }
            }
        },
    );
}

/// One bulk optics slider for an [`OpticField`], written to every selected
/// fixture (range-aware: e.g. green tint is bipolar). Seeds from the first
/// selected fixture that actually exposes the field (the union may include a
/// control the primary doesn't have), falling back to the primary.
fn bulk_opt_field(ui: &mut egui::Ui, state: &InspectorState, scene: &mut Scene, ids: &[usize], f: OpticField) {
    let seed = ids
        .iter()
        .copied()
        .find(|&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
        .unwrap_or(ids[0]);
    // Mixed-value detection (#7) over only the fixtures that EXPOSE this field.
    let common = common_f32(
        ids.iter()
            .filter(|&&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
            .map(|&i| f.get(&scene.fixtures[i].optics)),
    );
    bulk_f32_row(
        ui,
        state,
        f.label(),
        common,
        f.get(&scene.fixtures[seed].optics),
        |ui, v| ui.add(Slider::new(v, f.range())),
        |v| {
            for &i in ids {
                f.set(&mut scene.fixtures[i].optics, v);
            }
        },
    );
}
