//! Time-based wheel-motion simulation.
//!
//! Continuous rotations are **integrated** every real frame (phase += speed·dt),
//! not derived from absolute time, so changing a speed never snaps the wheel.
//! One `WheelMotion` lives on each [`Fixture`](crate::scene::Fixture) and is
//! advanced once per frame by [`crate::scene::Scene::advance`] — never inside
//! the renderer's `record_scene` (which also runs for headless capture and would
//! double-advance the animation).
//!
//! Phases align with the GDTF mode's dynamic component list (any number of
//! gobo/prism/color/animation wheels). Semantics per kind: gobo/prism phases
//! are image-rotation angles in radians; color/animation phases are wrapping
//! `0..1` scrolls. Max speeds are typical GDTF figures (Khamsin: gobo image
//! ±876°/s, animation/color ±702°/s, prism ±600°/s).

use crate::gdtf::{OpticalComponent, WheelKind};

use super::OpticalControls;

/// Accumulated motion phases for one fixture, aligned with the mode's
/// component list (radians for rotations, wrapping 0..1 for scrolls).
#[derive(Clone, Debug, Default)]
pub struct WheelMotion {
    pub phases: Vec<f32>,
}

/// Bipolar speed control: 0.5 = stopped, 0 = full reverse, 1 = full forward.
fn bipolar(x: f32) -> f32 {
    (x - 0.5) * 2.0
}

impl WheelMotion {
    /// The accumulated phase for component `i` (0 when not yet sized).
    pub fn phase(&self, i: usize) -> f32 {
        self.phases.get(i).copied().unwrap_or(0.0)
    }

    /// Advance all phases by `dt` seconds given the current control values and
    /// the fixture's component chain.
    pub fn advance(&mut self, c: &OpticalControls, components: &[OpticalComponent], dt: f32) {
        let dt = dt.clamp(0.0, 0.1); // ignore long stalls / first-frame spikes
        if self.phases.len() != components.len() {
            self.phases.resize(components.len(), 0.0);
        }
        for (i, comp) in components.iter().enumerate() {
            let spin = bipolar(c.wheel(i).spin);
            if spin.abs() < 1e-3 {
                continue;
            }
            let p = &mut self.phases[i];
            match comp.kind {
                WheelKind::Gobo => *p += spin * 876.0_f32.to_radians() * dt,
                WheelKind::Prism => *p += spin * 600.0_f32.to_radians() * dt,
                // Scrolls/positions wrap in 0..1; ±702°/s ≈ ±1.95 rev/s.
                WheelKind::Color | WheelKind::Animation => {
                    *p = (*p + spin * (702.0 / 360.0) * dt).rem_euclid(1.0)
                }
                WheelKind::Frost => {}
            }
        }
    }
}
