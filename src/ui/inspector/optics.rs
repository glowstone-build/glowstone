//! Optical-chain inspector section. Pure code move out of [`super`] (the
//! inspector `mod.rs`): the per-fixture optics control bank (sliders + wheels)
//! plus the EditCondition inertness predicate it uses.

use super::{advanced_section_filtered, approx, category, reset_arrow, slider_field_row, InspectorState};
use crate::gdtf::{GdtfFixture, WheelKind};
use crate::optics::{self, OpticField, OpticalControls};
use crate::scene::Fixture;
use egui::{Grid, RichText, Slider};

/// EditCondition-style inertness (P2 #66): whether an optic control, though the
/// fixture *exposes* it, is doing nothing at the fixture's current settings and so
/// should be greyed/disabled. The driver is the live fixture state, mirroring how
/// Blender greys a property whose effect another value has nullified:
///
/// * Every beam-shaping / colour control is inert when the lamp emits no light —
///   the dimmer is at 0 (nothing in the beam to zoom/iris/tint), OR the per-fixture
///   volumetric beam is killed (`beam == 0`) so there's no shaft to shape.
/// * The strobe rate is also inert when the shutter gate is fully closed
///   (`shutter == 0`) — a closed blackout can't strobe.
///
/// A pure predicate so it's unit-testable independent of egui.
fn optic_inert(fixture: &Fixture, f: OpticField) -> bool {
    // Only the DIMMER (the master level) makes the fixture dark. `fixture.beam`
    // merely scales the volumetric shaft/floor-pool — the lens still emits + shapes
    // surfaces with beam=0, so optics stay live (see Fixture.beam docs).
    let dark = approx(fixture.optics.dimmer, 0.0);
    if dark {
        return true;
    }
    // Strobe needs an open (or at least non-zero) shutter to pulse.
    matches!(f, OpticField::Strobe) && approx(fixture.optics.shutter, 0.0)
}

/// The optical-chain control bank for a GDTF fixture: sliders for every stage
/// the fixture actually exposes (disabled if the GDTF lacks that attribute).
/// Drives `fixture.optics`, which the renderer resolves into the beam each frame.
/// One data-driven optics slider row: label + range-aware slider (disabled when
/// the fixture doesn't expose it), with optional trailing text (e.g. zoom °).
fn optic_field_row(
    ui: &mut egui::Ui,
    state: &InspectorState,
    o: &mut OpticalControls,
    def: &OpticalControls,
    f: OpticField,
    enabled: bool,
    inert: bool,
    text: Option<String>,
) {
    // Per-property reset (#6): the arrow shows when this control left its neutral
    // default and is reachable (an unsupported/greyed row never differs anyway).
    let differs = enabled && !approx(f.get(o), f.get(def));
    // P2 #64: "show only modified" swallows rows still at their default. The
    // caller already filtered by label visibility, but the modified-only gate
    // depends on the live value, so it's applied here.
    if state.show_modified && !differs {
        return;
    }
    // P2 #66 (EditCondition): an inert control (supported but currently doing
    // nothing — e.g. beam shaping with the dimmer at 0) is greyed AND disabled,
    // so it reads as present-but-not-acting rather than absent.
    let enabled = enabled && !inert;
    let mut v = f.get(o);
    // Keep the readout narrow (was 5+ decimals) so it fits the value cell; when there's
    // a derived label (e.g. zoom degrees "58°") show ONLY that, not the raw 0-1 value.
    let mut slider = Slider::new(&mut v, f.range()).max_decimals(2);
    if let Some(t) = text {
        slider = slider.text(t).show_value(false);
    }
    let mut reset = false;
    let mut changed = false;
    slider_field_row(
        ui,
        state.panel_w,
        |ui| {
            reset = reset_arrow(ui, differs);
            // Grey the label of an inert control so the whole row reads disabled.
            let text = if inert { RichText::new(f.label()).weak() } else { RichText::new(f.label()) };
            let lbl = ui.add(egui::Label::new(text).truncate());
            if inert {
                lbl.on_hover_text("Inactive at the current settings (no light to shape)");
            } else if f == OpticField::Green {
                lbl.on_hover_text("Plus/minus-green (CC axis): −1 magenta … +1 green");
            }
        },
        |ui| {
            changed = ui.add_enabled(enabled, slider).changed();
        },
    );
    if changed {
        f.set(o, v);
    }
    if reset {
        f.set(o, f.get(def));
    }
}

pub(super) fn optics_section(ui: &mut egui::Ui, fixture: &mut Fixture, gdtf: &GdtfFixture, state: &mut InspectorState) {
    let beam_angle = fixture.beam_angle;
    // The dynamic wheel chain of the active mode (any number of color/gobo/
    // prism/animation/frost components).
    let components: Vec<crate::gdtf::OpticalComponent> = gdtf
        .modes
        .get(fixture.mode_index)
        .map(|m| m.components.clone())
        .unwrap_or_default();
    fixture.optics.ensure_wheels(components.len());

    // The full label universe of this optics bank (used to decide whether the
    // category survives the filter): every exposed optic field, the wheel
    // component names, plus the shutter-blade picker.
    const BEAM_COMMON: [OpticField; 3] = [OpticField::Zoom, OpticField::Focus, OpticField::Iris];
    const BEAM_ADV: [OpticField; 3] = [OpticField::Ca, OpticField::Shutter, OpticField::Strobe];
    const COLOR_COMMON: [OpticField; 4] =
        [OpticField::Cto, OpticField::Cyan, OpticField::Magenta, OpticField::Yellow];
    const COLOR_ADV: [OpticField; 1] = [OpticField::Green];
    let mut all_labels: Vec<&str> = vec!["Shutter blades"];
    for f in BEAM_COMMON.iter().chain(&BEAM_ADV).chain(&COLOR_COMMON).chain(&COLOR_ADV) {
        all_labels.push(f.label());
    }
    for comp in &components {
        all_labels.push(comp.wheel.as_deref().unwrap_or(comp.attribute.as_str()));
    }

    category(
        ui,
        state,
        "Optics",
        "Optics",
        true,
        &all_labels,
        |ui, fs| {
            let zoom_deg = optics::map_attr(gdtf, "Zoom", fixture.optics.zoom, (beam_angle, beam_angle));
            // Shutter blade style — OUR editable model (GDTF lacks blade geometry).
            // Only shown for fixtures that actually have a shutter (or already set
            // one), so a plain PAR/wash isn't offered a blade it can't use.
            if (crate::optics::OpticField::Shutter.supported(gdtf)
                || fixture.shutter != crate::optics::ShutterKind::None)
                && fs.row_visible("Shutter blades")
            {
                ui.horizontal(|ui| {
                    ui.label("Shutter blades");
                    egui::ComboBox::from_id_salt("shutter-kind")
                        .selected_text(fixture.shutter.label())
                        .show_ui(ui, |ui| {
                            for k in crate::optics::ShutterKind::ALL {
                                ui.selectable_value(&mut fixture.shutter, k, k.label());
                            }
                        });
                });
            }
            let def = OpticalControls::default();
            // P2 #66: pre-evaluate the EditCondition for every optic field BEFORE
            // the mutable borrow of `fixture.optics`, so the live-edit loops can
            // read inertness without re-borrowing the fixture. `optic_inert` is the
            // single (unit-tested) source of the predicate.
            let inert_of: std::collections::HashMap<&'static str, bool> = OpticField::BEAM
                .iter()
                .chain(&OpticField::COLOR)
                .map(|&f| (f.label(), optic_inert(fixture, f)))
                .collect();
            let inert = |f: OpticField| inert_of.get(f.label()).copied().unwrap_or(false);
            let o = &mut fixture.optics;
            // Data-driven rows (gated by the fixture's GDTF attributes) so single
            // and bulk editing enumerate the SAME control set — see `OpticField`.
            // Simple/Advanced split (#8): the everyday controls show up front; the
            // power-user shaping (chromatic ab. / shutter / strobe / ±green tint)
            // tucks behind a per-section "Advanced" caret.

            if fs.category_visible(&BEAM_COMMON.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                ui.label(RichText::new("BEAM SHAPING").small().strong());
                Grid::new("optics-beam").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for f in BEAM_COMMON {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), (f == OpticField::Zoom).then(|| format!("{zoom_deg:.0}°")));
                        }
                    }
                });
            }
            advanced_section_filtered(ui, fs, "optics-beam", &BEAM_ADV.iter().map(|f| f.label()).collect::<Vec<_>>(), |ui| {
                Grid::new("optics-beam-adv").num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
                    for f in BEAM_ADV {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), None);
                        }
                    }
                });
            });

            if fs.category_visible(&COLOR_COMMON.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                ui.add_space(4.0);
                ui.label(RichText::new("COLOR MIXING").small().strong());
                Grid::new("optics-color").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for f in COLOR_COMMON {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), None);
                        }
                    }
                });
            }
            advanced_section_filtered(ui, fs, "optics-color", &COLOR_ADV.iter().map(|f| f.label()).collect::<Vec<_>>(), |ui| {
                Grid::new("optics-color-adv").num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
                    for f in COLOR_ADV {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), None);
                        }
                    }
                });
            });

            // One block per wheel component, generated from the GDTF chain.
            let wheel_names: Vec<&str> =
                components.iter().map(|c| c.wheel.as_deref().unwrap_or(c.attribute.as_str())).collect();
            if !components.is_empty() && fs.category_visible(&wheel_names) {
                ui.add_space(4.0);
                ui.label(RichText::new("WHEELS").small().strong());
                Grid::new("optics-wheels").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for (i, comp) in components.iter().enumerate() {
                        let Some(w) = o.wheels.get_mut(i) else { continue };
                        let match_name = comp.wheel.as_deref().unwrap_or(comp.attribute.as_str());
                        if !fs.row_visible(match_name) {
                            continue;
                        }
                        let value_label = match comp.kind {
                            WheelKind::Gobo | WheelKind::Color => "select",
                            WheelKind::Prism | WheelKind::Animation | WheelKind::Frost => "insert",
                        };
                        let name = comp
                            .wheel
                            .as_deref()
                            .map(|n| format!("{} · {n}", comp.attribute))
                            .unwrap_or_else(|| comp.attribute.clone());
                        slider_field_row(
                            ui,
                            fs.panel_w,
                            |ui| {
                                ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                            },
                            |ui| {
                                ui.add(Slider::new(&mut w.value, 0.0..=1.0).max_decimals(2))
                                    .on_hover_text(value_label);
                            },
                        );
                        // Prism always exposes rotation (index + spin) even when the
                        // profile didn't flag a dedicated Pos/PosRotate function.
                        if comp.has_index || comp.kind == WheelKind::Prism {
                            slider_field_row(ui, fs.panel_w, |ui| { ui.label("index"); }, |ui| {
                                ui.add(Slider::new(&mut w.index, 0.0..=1.0).max_decimals(2));
                            });
                        }
                        if comp.has_spin || matches!(comp.kind, WheelKind::Color | WheelKind::Animation | WheelKind::Prism) {
                            slider_field_row(ui, fs.panel_w, |ui| { ui.label("spin"); }, |ui| {
                                ui.add(Slider::new(&mut w.spin, 0.0..=1.0).max_decimals(2))
                                    .on_hover_text("0.5 = stopped · below CCW · above CW");
                            });
                        }
                        if matches!(comp.kind, WheelKind::Gobo | WheelKind::Color) {
                            slider_field_row(ui, fs.panel_w, |ui| { ui.label("shake"); }, |ui| {
                                ui.add(Slider::new(&mut w.shake, 0.0..=1.0).max_decimals(2))
                                    .on_hover_text("Oscillate the indexed element");
                            });
                        }
                    }
                });
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    // --- P2 #66: EditCondition gray-out predicate ---------------------------

    #[test]
    fn optic_inert_when_dark_or_shutter_closed() {
        let mut f = Scene::demo().fixtures[0].clone();
        // Lit, open shutter → every control is live.
        f.optics.dimmer = 1.0;
        f.beam = 1.0;
        f.optics.shutter = 1.0;
        assert!(!optic_inert(&f, OpticField::Zoom));
        assert!(!optic_inert(&f, OpticField::Cyan));
        assert!(!optic_inert(&f, OpticField::Strobe));

        // Dimmer at 0: nothing in the beam → all controls inert.
        f.optics.dimmer = 0.0;
        assert!(optic_inert(&f, OpticField::Zoom));
        assert!(optic_inert(&f, OpticField::Cyan));
        f.optics.dimmer = 1.0;

        // Per-fixture beam (volumetric shaft) killed → the light still emits + shapes
        // surfaces, so optics stay LIVE (beam only scales the haze, not the fixture).
        f.beam = 0.0;
        assert!(!optic_inert(&f, OpticField::Zoom));
        f.beam = 1.0;

        // Lit but the shutter is fully closed → strobe (only) is inert.
        f.optics.shutter = 0.0;
        assert!(optic_inert(&f, OpticField::Strobe));
        assert!(!optic_inert(&f, OpticField::Zoom));
    }
}
