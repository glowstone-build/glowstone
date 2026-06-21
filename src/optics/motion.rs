//! Time-based wheel-motion simulation.
//!
//! Everything is **integrated** every real frame (value += rate·dt), not derived
//! from absolute time, so changing a speed/target never snaps. One `WheelMotion`
//! lives on each [`Fixture`](crate::scene::Fixture) and is advanced once per
//! frame by [`crate::scene::Scene::advance`] — never inside the renderer's
//! `record_scene` (which also runs for headless capture and would double-advance).
//!
//! Per component (aligned with the GDTF mode's chain) we track:
//! - `positions[i]` — the **physical wheel angle** in slot units: a stepper that
//!   slews toward the selected slot (so a move sweeps through the split + gap) or
//!   free-runs for continuous scroll. This is what the shader samples spatially.
//! - `phases[i]` — the gobo **image** rotation (radians) / prism rotation /
//!   animation scroll (wrapping 0..1).
//! - `shake_phases[i]` — gobo/colour shake oscillation.
//! And `cmy` — the three CMY flag insertions, slewed toward their targets at
//! ColorMixMSpeed so the flags visibly slide.

use crate::gdtf::{OpticalComponent, WheelKind};

use super::OpticalControls;

/// Per-component motion state for one fixture.
#[derive(Clone, Debug, Default)]
pub struct WheelMotion {
    /// Gobo image / prism / animation rotation+scroll (radians or wrapping 0..1).
    pub phases: Vec<f32>,
    /// Shake oscillation phase (wrapping 0..1).
    pub shake_phases: Vec<f32>,
    /// Physical wheel position in slot units (slewed; wraps at the slot count).
    pub positions: Vec<f32>,
    /// Slewed CMY flag insertions (cyan/magenta/yellow), each `0..1`.
    pub cmy: [f32; 3],
}

/// Bipolar speed control: 0.5 = stopped, 0 = full reverse, 1 = full forward.
fn bipolar(x: f32) -> f32 {
    (x - 0.5) * 2.0
}

/// ColorMixMSpeed (0 = fast … 1 = slow) → CMY flag full-travel time (s).
/// Robe colour-mixing-time range 0.2–25.5 s; default 0.5 s. (research-cmy.md)
fn cmy_travel_time(m: f32) -> f32 {
    const T_FAST: f32 = 0.25;
    const T_SLOW: f32 = 25.5;
    T_FAST * (T_SLOW / T_FAST).powf(m.clamp(0.0, 1.0))
}

impl WheelMotion {
    /// The gobo-image / prism / anim phase for component `i`.
    pub fn phase(&self, i: usize) -> f32 {
        self.phases.get(i).copied().unwrap_or(0.0)
    }

    /// The physical wheel position (slot units) for component `i`.
    pub fn position(&self, i: usize) -> f32 {
        self.positions.get(i).copied().unwrap_or(0.0)
    }

    /// The shake oscillation offset (radians) for component `i` at amplitude
    /// scaled by its `shake` control — a sine sway of the indexed element.
    pub fn shake_offset(&self, i: usize, shake: f32) -> f32 {
        if shake <= 0.01 {
            return 0.0;
        }
        const MAX_AMP: f32 = 0.35; // ~20° sway at full
        (self.shake_phases.get(i).copied().unwrap_or(0.0) * std::f32::consts::TAU).sin()
            * MAX_AMP
            * shake.clamp(0.0, 1.0)
    }

    /// Settle wheel positions + CMY to their commanded targets instantly (no
    /// slew). Used on load and by headless capture paths that render without the
    /// per-frame [`advance`](Self::advance) integrator. Continuous scroll/spin
    /// can't "settle", so spinning wheels keep their phase.
    pub fn settle(&mut self, c: &OpticalControls, components: &[OpticalComponent]) {
        let n = components.len();
        self.positions.resize(n, 0.0);
        if self.phases.len() != n {
            self.phases.resize(n, 0.0);
        }
        if self.shake_phases.len() != n {
            self.shake_phases.resize(n, 0.0);
        }
        for (i, comp) in components.iter().enumerate() {
            let ctl = c.wheel(i);
            let slots = comp.slots.max(1) as f32;
            let spinning = matches!(comp.kind, WheelKind::Color) && (ctl.spin - 0.5).abs() > 0.02;
            if matches!(comp.kind, WheelKind::Gobo | WheelKind::Color) && !spinning {
                self.positions[i] = slot_target(ctl.value, slots);
            }
        }
        self.cmy = [c.cmy[0].clamp(0.0, 1.0), c.cmy[1].clamp(0.0, 1.0), c.cmy[2].clamp(0.0, 1.0)];
    }

    /// Advance all motion by `dt` seconds given the current controls + chain.
    pub fn advance(&mut self, c: &OpticalControls, components: &[OpticalComponent], dt: f32) {
        let dt = dt.clamp(0.0, 0.1); // ignore long stalls / first-frame spikes
        let n = components.len();
        if self.phases.len() != n {
            self.phases.resize(n, 0.0);
        }
        if self.shake_phases.len() != n {
            self.shake_phases.resize(n, 0.0);
        }
        if self.positions.len() != n {
            self.positions.resize(n, 0.0);
        }

        // A real wheel indexes a slot fast (~0.1 s) — snappier than before so the
        // disc visibly spins through the change rather than crawling.
        const WHEEL_SLOT_RATE: f32 = 8.0; // slots/s for a select move
        const SCROLL_MAX: f32 = 10.0; // slots/s at full continuous scroll

        for (i, comp) in components.iter().enumerate() {
            let ctl = c.wheel(i);
            let slots = (comp.slots.max(1)) as f32;
            // Shake oscillation: 1..9 Hz, wrapping 0..1.
            if ctl.shake > 0.01 && matches!(comp.kind, WheelKind::Gobo | WheelKind::Color) {
                let hz = 1.0 + 8.0 * ctl.shake;
                self.shake_phases[i] = (self.shake_phases[i] + hz * dt).rem_euclid(1.0);
            }
            let spin = bipolar(ctl.spin);

            match comp.kind {
                WheelKind::Gobo => {
                    // Wheel position slews to the selected slot (move = split+gap
                    // sweep); the gobo IMAGE spins independently via `phases`.
                    // The select SNAPS to a whole slot (a console indexes the
                    // wheel to a slot) so a steady selection parks on one gobo —
                    // the split only shows while travelling between slots.
                    let target = slot_target(ctl.value, slots);
                    slew_toward(&mut self.positions[i], target, WHEEL_SLOT_RATE, dt);
                    if spin.abs() >= 1e-3 {
                        self.phases[i] += spin * 876.0_f32.to_radians() * dt;
                    }
                }
                WheelKind::Color => {
                    // Spin = continuous wheel SCROLL (rainbow): position free-runs
                    // and wraps. Otherwise slew to the selected slot (snapped).
                    if spin.abs() >= 1e-3 {
                        self.positions[i] =
                            (self.positions[i] + spin * SCROLL_MAX * dt).rem_euclid(slots);
                    } else {
                        let target = slot_target(ctl.value, slots);
                        slew_toward(&mut self.positions[i], target, WHEEL_SLOT_RATE, dt);
                    }
                }
                WheelKind::Prism => {
                    if spin.abs() >= 1e-3 {
                        self.phases[i] += spin * 600.0_f32.to_radians() * dt;
                    }
                }
                WheelKind::Animation => {
                    if spin.abs() >= 1e-3 {
                        self.phases[i] = (self.phases[i] + spin * (702.0 / 360.0) * dt).rem_euclid(1.0);
                    }
                }
                WheelKind::Frost => {}
            }
        }

        // CMY flags slide toward their targets at ColorMixMSpeed (each visibly
        // sweeps in over the travel time).
        let rate = 1.0 / cmy_travel_time(c.color_mix_speed);
        for k in 0..3 {
            slew_toward(&mut self.cmy[k], c.cmy[k].clamp(0.0, 1.0), rate, dt);
        }
    }
}

/// The target wheel position (slot units) for a select `value` (0..1): the
/// nearest whole slot, so a steady selection parks cleanly on one slot rather
/// than between two (which would permanently split the beam).
fn slot_target(value: f32, slots: f32) -> f32 {
    (value.clamp(0.0, 1.0) * (slots - 1.0)).round()
}

/// Constant-rate linear slew of `v` toward `target`, capped at `rate` per second.
fn slew_toward(v: &mut f32, target: f32, rate: f32, dt: f32) {
    let step = rate * dt;
    let d = target - *v;
    if d.abs() <= step {
        *v = target;
    } else {
        *v += step * d.signum();
    }
}
