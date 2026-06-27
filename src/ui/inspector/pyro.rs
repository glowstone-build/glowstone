//! `impl Inspect for PyroDevice` — the editable property grid for a selected stage
//! pyro device (CO2 cannon or cold-spark machine). Ported from the legacy
//! `pyro_inspector` (RESEARCH-pyro §4) onto the declarative [`Props`] builder.
//!
//! The kind-aware **Effect** rows differ for ColdSpark vs Co2Jet (different fields,
//! ranges, suffixes + tooltips); **Movement** is the nozzle aim; **Quality** is the
//! smoke-grid resolution + viewport-speed tradeoff + a particle cap; **DMX** is the
//! inline patch (universe/address/mode), NOT the fixture PatchTable. The pyro
//! sliders never CLAMP to their range (`NumField::no_clamp` → suggest-only): the
//! user can type any raw value.
//!
//! The Transform (Position/Rotation) + the heading/Visible chrome stay in the
//! wrapper [`super::pyro_inspector`] (it decomposes/recomposes the device's `Mat4`),
//! mirroring `geometry_inspector`.

use super::props::{Inspect, Props};
use super::theme::icon;
use crate::scene::pyro::{PyroKind, PyroMode, PyroPatch};
use crate::scene::PyroDevice;

impl Inspect for PyroDevice {
    fn inspect(&mut self, p: &mut Props) {
        let spark = self.kind == PyroKind::ColdSpark;

        // --- Effect (kind-aware physical + look tunables) ---
        // Sliders show a suggested range but NEVER clamp — the user can type any value.
        p.group("Effect", icon::PYRO, true, |p| {
            if spark {
                p.f32("Height", &mut self.height_m).range(0.5..=8.0).suffix(" m").slider().no_clamp();
                p.f32("Amount", &mut self.density).range(0.0..=1.0).slider().no_clamp();
                p.f32("Spread", &mut self.cone_deg).range(0.0..=45.0).suffix("°").slider().no_clamp();
                p.f32("Brightness", &mut self.brightness).range(1.0..=20.0).slider().no_clamp();
                p.f32("Hot colour", &mut self.color_t0_k).range(1500.0..=3500.0).suffix(" K").slider().no_clamp();
                p.f32("Tip colour", &mut self.color_t1_k).range(900.0..=1600.0).suffix(" K").slider().no_clamp();
            } else {
                p.f32("Throw", &mut self.throw_m)
                    .range(3.0..=20.0)
                    .suffix(" m")
                    .slider()
                    .no_clamp()
                    .tip("How high the plume billows up (buoyant rise).");
                p.f32("Speed", &mut self.speed)
                    .range(2.0..=22.0)
                    .suffix(" m/s")
                    .slider()
                    .no_clamp()
                    .tip("Jet exit velocity — the launch blast, separate from Throw.");
                p.f32("Output", &mut self.density).range(0.0..=1.0).slider().no_clamp();
                p.f32("Spread", &mut self.cone_deg).range(0.0..=20.0).suffix("°").slider().no_clamp();
                p.f32("Opacity", &mut self.opacity).range(0.05..=1.0).slider().no_clamp();
                p.f32("Density", &mut self.thickness)
                    .range(0.1..=4.0)
                    .suffix("×")
                    .slider()
                    .no_clamp()
                    .tip(
                        "Visual density of the smoke. Higher = denser, with a \
                         darker self-shadowed core (the dark region spreads) and \
                         a lighter rim. Type any value.",
                    );
                p.f32("Dissipation", &mut self.dissipation)
                    .range(0.3..=10.0)
                    .suffix(" s")
                    .slider()
                    .no_clamp()
                    .tip(
                        "Hang time in seconds — how long the smoke lingers after \
                         the valve shuts (per-puff jittered so edges dissolve \
                         raggedly). Output itself is instant. Type any value.",
                    );
                p.color("Tint", &mut self.tint, None);
            }
        });

        // --- Movement (nozzle aim; meaningful on the moving / spin variants) ---
        p.group("Movement", icon::INSPECTOR, false, |p| {
            p.f32("Pan", &mut self.pan).range(-90.0..=90.0).suffix("°").slider().no_clamp();
            p.f32("Tilt", &mut self.tilt).range(-60.0..=60.0).suffix("°").slider().no_clamp();
            p.f32("Spin", &mut self.spin_rpm).range(-100.0..=100.0).suffix(" rpm").slider().no_clamp();
        });

        // --- Quality (smoke grid resolution + cap). The Detail preset is the
        // crisp-edges lever; renders are sharper than the half-res live preview. ---
        p.group("Quality", icon::PERF, true, |p| {
            if !spark {
                let mut hq = self.viewport_hq;
                if p.combo("Viewport", &mut hq, &[(false, "Fast"), (true, "Full")]) {
                    self.viewport_hq = hq;
                }
                // The combo can't carry a tooltip; a note explains the tradeoff.
                p.note(
                    "Viewport: Fast skips per-beam smoke shadowing for smooth editing; \
                     Full is render-accurate live (heavier). Exports are always Full.",
                );
            }
            let mut q = self.quality.min(3) as usize;
            if p.combo(
                "Detail",
                &mut q,
                &[(0usize, "Low"), (1, "Medium"), (2, "High"), (3, "Ultra")],
            ) {
                self.quality = q as u8;
            }
            p.note(
                "Detail: CO2 smoke grid resolution (Low 40³ … Ultra 84³ voxels) — \
                 higher = crisper edges at more CPU cost. The live viewport renders \
                 volumetrics at half-resolution, so an exported render is sharpest.",
            );
            // No upper limit (the user can enter any value); soft lower floor only.
            p.custom("Max particles", true, |ui| {
                ui.add(egui::DragValue::new(&mut self.max_particles).speed(50.0).range(1..=u32::MAX));
            });
        });

        // --- DMX patch (inline; NOT through the fixture patch table) ---
        p.group("DMX", icon::PATCH, true, |p| {
            let mut patched = self.patch.is_some();
            p.custom("Patched", true, |ui| {
                if ui.checkbox(&mut patched, "").changed() {
                    self.patch = patched.then(PyroPatch::default);
                }
            });
            p.custom("Mode", true, |ui| {
                egui::ComboBox::from_id_salt("pyro-mode")
                    .selected_text(self.mode.label())
                    .show_ui(ui, |ui| {
                        for m in PyroMode::ALL {
                            ui.selectable_value(&mut self.mode, m, m.label());
                        }
                    });
            });
            if let Some(patch) = &mut self.patch {
                p.custom("Universe", true, |ui| {
                    let mut v = patch.universe as i32;
                    if ui.add(egui::DragValue::new(&mut v).speed(1.0).range(1..=63999)).changed() {
                        patch.universe = v.clamp(1, 63999) as u16;
                    }
                });
                p.custom("Address", true, |ui| {
                    let mut a = patch.address as i32;
                    if ui.add(egui::DragValue::new(&mut a).speed(1.0).range(1..=512)).changed() {
                        patch.address = a.clamp(1, 512) as u16;
                    }
                });
            }
            let n = self.mode.footprint(self.kind);
            p.note(format!("Footprint  {n} ch"));
        });
    }
}
