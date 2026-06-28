//! `impl Inspect for Fixture` — the editable property grid shared by the built-in
//! and GDTF fixture inspectors (the Transform + Fixture categories). The surrounding
//! chrome (name/header, provenance chip, GDTF thumbnail + specs, the dynamic optics
//! section and the wheel/DMX-mode galleries) stays in the dispatch functions; those
//! are not "properties", they're bespoke read-outs.

use super::props::{Inspect, Props};
use super::FixtureDefaults;
use crate::scene::Fixture;
use crate::ui::theme::icon;

impl Inspect for Fixture {
    fn inspect(&mut self, p: &mut Props) {
        let def = FixtureDefaults::for_fixture(self);
        let is_gdtf = self.gdtf.is_some();
        // GDTF heads tilt ±135°; the legacy/built-in body allows the full ±180.
        let tilt_range: std::ops::RangeInclusive<f32> =
            if is_gdtf { -135.0..=135.0 } else { -180.0..=180.0 };

        // TRANSFORM = rig placement: hang Position + Rotation + the pan/tilt motor speed.
        p.group("Transform", icon::INSPECTOR, true, |p| {
            // Position has no recoverable default (no revert arrow).
            p.vec3("Position", &mut self.position).speed(0.05);
            // Rotation = the rig HANG orientation; reverts to identity.
            p.rotation("Rotation", &mut self.orientation);
            p.f32("Move speed", &mut self.move_speed)
                .range(0.0..=1.0)
                .decimals(2)
                .slider()
                .default(0.0)
                .tip("Pan/tilt motor speed: 0 = fastest (snap), 1 = slowest");
        });

        // FIXTURE = the head's own properties: aim (Pan/Tilt), level, colour, beam.
        let (pan_now, tilt_now) = (self.pan_actual, self.tilt_actual);
        p.group("Fixture", icon::COLOR, true, |p| {
            p.custom("Sequence", true, |ui| {
                ui.add(egui::DragValue::new(&mut self.sequence).range(1..=u32::MAX).speed(0.2));
            });
            p.f32("Pan", &mut self.pan)
                .speed(0.5)
                .range(-270.0..=270.0)
                .suffix("°")
                .default(def.pan)
                .tip(format!("commanded · now {pan_now:.0}°"));
            p.f32("Tilt", &mut self.tilt)
                .speed(0.5)
                .range(tilt_range.clone())
                .suffix("°")
                .default(def.tilt)
                .tip(format!("commanded · now {tilt_now:.0}°"));
            p.f32("Dimmer", &mut self.optics.dimmer).speed(0.005).range(0.0..=1.0).default(def.dimmer);
            // Colour only shows a revert arrow when its template is known (GDTF white
            // master); a built-in's library tint isn't stored (def.color = None).
            p.color("Color", &mut self.color, def.color);
            // Advanced: the volumetric / cone tuning a designer touches rarely.
            p.advanced("fixture", |p| {
                p.f32("Beam", &mut self.beam)
                    .speed(0.01)
                    .range(0.0..=4.0)
                    .default(def.beam)
                    .tip("Volumetric beam intensity (0 = off, 1 = normal)");
                // GDTF beam angle is template-only (driven by Zoom); only the built-in
                // path exposes an editable Beam angle.
                if !is_gdtf {
                    p.f32("Beam angle", &mut self.beam_angle)
                        .speed(0.2)
                        .range(2.0..=90.0)
                        .suffix("°")
                        .default_opt(def.beam_angle);
                }
            });
        });
    }
}
