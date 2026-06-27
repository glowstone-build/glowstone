//! Optical-chain inspector section: the per-fixture optics control bank (sliders +
//! wheels) plus the EditCondition inertness predicate it uses. Declared through the
//! [`Props`](super::props::Props) property builder — the dynamic set of controls a
//! GDTF actually exposes, each greyed when unsupported/inert.

use super::props::{self, Props};
use super::{approx, InspectorState};
use crate::gdtf::{GdtfFixture, WheelKind};
use crate::optics::{self, OpticField, OpticalControls, ShutterKind};
use crate::scene::Fixture;

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

/// One optics control row, declared through [`Props`]: read/write via [`OpticField`]
/// (so single + bulk enumerate the SAME set), greyed when unsupported or inert, with an
/// optional derived readout (zoom shown in degrees). The revert arrow targets the
/// neutral default; an unsupported/inert row can't differ actionably (`enabled=false`).
fn optic_row(
    p: &mut Props,
    o: &mut OpticalControls,
    def: &OpticalControls,
    f: OpticField,
    gdtf: &GdtfFixture,
    inert: bool,
    text: Option<String>,
) {
    let enabled = f.supported(gdtf) && !inert;
    let mut v = f.get(o);
    let mut nf = p
        .f32(f.label(), &mut v)
        .range(f.range())
        .slider()
        .decimals(2)
        .enabled(enabled)
        .default(f.get(def));
    if let Some(t) = text {
        nf = nf.text_value(t);
    }
    if inert {
        nf = nf.tip("Inactive at the current settings (no light to shape)");
    } else if f == OpticField::Green {
        nf = nf.tip("Plus/minus-green (CC axis): −1 magenta … +1 green");
    }
    if nf.show() {
        f.set(o, v);
    }
}

pub(super) fn optics_section(ui: &mut egui::Ui, fixture: &mut Fixture, gdtf: &GdtfFixture, state: &mut InspectorState) {
    const BEAM_COMMON: [OpticField; 3] = [OpticField::Zoom, OpticField::Focus, OpticField::Iris];
    const BEAM_ADV: [OpticField; 3] = [OpticField::Ca, OpticField::Shutter, OpticField::Strobe];
    const COLOR_COMMON: [OpticField; 4] =
        [OpticField::Cto, OpticField::Cyan, OpticField::Magenta, OpticField::Yellow];
    const COLOR_ADV: [OpticField; 1] = [OpticField::Green];

    let beam_angle = fixture.beam_angle;
    let zoom_deg = optics::map_attr(gdtf, "Zoom", fixture.optics.zoom, (beam_angle, beam_angle));
    // The dynamic wheel chain of the active mode (any number of color/gobo/prism/
    // animation/frost components).
    let components: Vec<crate::gdtf::OpticalComponent> = gdtf
        .modes
        .get(fixture.mode_index)
        .map(|m| m.components.clone())
        .unwrap_or_default();
    fixture.optics.ensure_wheels(components.len());
    let def = OpticalControls::default();
    // P2 #66: pre-evaluate the EditCondition for every optic field BEFORE the mutable
    // borrow of `fixture.optics`. `optic_inert` is the single (unit-tested) predicate.
    let inert_of: std::collections::HashMap<&'static str, bool> = OpticField::BEAM
        .iter()
        .chain(&OpticField::COLOR)
        .map(|&f| (f.label(), optic_inert(fixture, f)))
        .collect();
    let inert = |f: OpticField| inert_of.get(f.label()).copied().unwrap_or(false);
    let show_shutter_blades = OpticField::Shutter.supported(gdtf) || fixture.shutter != ShutterKind::None;
    let shutter_opts: Vec<(ShutterKind, &str)> = ShutterKind::ALL.iter().map(|&k| (k, k.label())).collect();
    let beam_common_labels: Vec<&str> = BEAM_COMMON.iter().map(|f| f.label()).collect();
    let color_common_labels: Vec<&str> = COLOR_COMMON.iter().map(|f| f.label()).collect();
    let wheel_names: Vec<&str> =
        components.iter().map(|c| c.wheel.as_deref().unwrap_or(c.attribute.as_str())).collect();

    props::with(ui, state, |p| {
        p.group("Optics", "", true, |p| {
            // Shutter blade style — OUR editable model (GDTF lacks blade geometry).
            // Only offered to fixtures that actually have a shutter (or already set one).
            if show_shutter_blades {
                p.combo("Shutter blades", &mut fixture.shutter, &shutter_opts);
            }
            // Data-driven rows gated by the fixture's GDTF attributes; simple/advanced split.
            if p.any_visible(&beam_common_labels) {
                p.subhead("BEAM SHAPING");
            }
            for f in BEAM_COMMON {
                let text = (f == OpticField::Zoom).then(|| format!("{zoom_deg:.0}°"));
                optic_row(p, &mut fixture.optics, &def, f, gdtf, inert(f), text);
            }
            p.advanced("beam", |p| {
                for f in BEAM_ADV {
                    optic_row(p, &mut fixture.optics, &def, f, gdtf, inert(f), None);
                }
            });

            if p.any_visible(&color_common_labels) {
                p.subhead("COLOR MIXING");
            }
            for f in COLOR_COMMON {
                optic_row(p, &mut fixture.optics, &def, f, gdtf, inert(f), None);
            }
            p.advanced("color", |p| {
                for f in COLOR_ADV {
                    optic_row(p, &mut fixture.optics, &def, f, gdtf, inert(f), None);
                }
            });

            // One block per wheel component, generated from the GDTF chain.
            if !components.is_empty() && p.any_visible(&wheel_names) {
                p.subhead("WHEELS");
            }
            for (i, comp) in components.iter().enumerate() {
                let value_label = match comp.kind {
                    WheelKind::Gobo | WheelKind::Color => "select",
                    WheelKind::Prism | WheelKind::Animation | WheelKind::Frost => "insert",
                };
                let name = comp
                    .wheel
                    .as_deref()
                    .map(|n| format!("{} · {n}", comp.attribute))
                    .unwrap_or_else(|| comp.attribute.clone());
                let Some(w) = fixture.optics.wheels.get_mut(i) else { continue };
                p.f32(&name, &mut w.value).range(0.0..=1.0).slider().decimals(2).tip(value_label);
                if comp.has_index || comp.kind == WheelKind::Prism {
                    p.f32("index", &mut w.index).range(0.0..=1.0).slider().decimals(2);
                }
                if comp.has_spin || matches!(comp.kind, WheelKind::Color | WheelKind::Animation | WheelKind::Prism) {
                    p.f32("spin", &mut w.spin)
                        .range(0.0..=1.0)
                        .slider()
                        .decimals(2)
                        .tip("0.5 = stopped · below CCW · above CW");
                }
                if matches!(comp.kind, WheelKind::Gobo | WheelKind::Color) {
                    p.f32("shake", &mut w.shake)
                        .range(0.0..=1.0)
                        .slider()
                        .decimals(2)
                        .tip("Oscillate the indexed element");
                }
            }
        });
    });
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
