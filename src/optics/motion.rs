//! Time-based wheel-motion simulation.
//!
//! Continuous rotations are **integrated** every real frame (phase += speed·dt),
//! not derived from absolute time, so changing a speed never snaps the wheel.
//! One `WheelMotion` lives on each [`Fixture`](crate::scene::Fixture) and is
//! advanced once per frame by [`crate::scene::Scene::advance`] — never inside
//! the renderer's `record_scene` (which also runs for headless capture and would
//! double-advance the animation).
//!
//! GDTF max speeds for the Khamsin: gobo image ±876°/s, gobo wheel ±702°/s,
//! animation ±702°/s, prism ±600°/s, color ±702°/s.

use super::OpticalControls;

/// Accumulated motion phases for one fixture (radians, or 0..1 for scrolls).
#[derive(Clone, Copy, Debug, Default)]
pub struct WheelMotion {
    /// Rotating-gobo image angle, radians (GoboWheel 1 / 2).
    pub gobo1_angle: f32,
    pub gobo2_angle: f32,
    /// Animation-glass scroll, wrapping 0..1.
    pub anim_scroll: f32,
    /// Prism rotation, radians (Prism 1 / 2).
    pub prism1_angle: f32,
    pub prism2_angle: f32,
    /// Color-wheel continuous "rainbow" position, wrapping 0..1.
    pub color_phase: f32,
}

/// Bipolar speed control: 0.5 = stopped, 0 = full reverse, 1 = full forward.
fn bipolar(x: f32) -> f32 {
    (x - 0.5) * 2.0
}

impl WheelMotion {
    /// Advance all phases by `dt` seconds given the current control values.
    pub fn advance(&mut self, c: &OpticalControls, dt: f32) {
        let dt = dt.clamp(0.0, 0.1); // ignore long stalls / first-frame spikes
        let dps = |deg: f32| deg.to_radians();
        self.gobo1_angle += bipolar(c.gobo1_rot) * dps(876.0) * dt;
        self.gobo2_angle += bipolar(c.gobo2_rot) * dps(876.0) * dt;
        self.prism1_angle += bipolar(c.prism1_rot) * dps(600.0) * dt;
        self.prism2_angle += bipolar(c.prism2_rot) * dps(600.0) * dt;
        // Scrolls/positions wrap in 0..1. Animation glass at ±702°/s ≈ ±1.95 rev/s.
        self.anim_scroll = (self.anim_scroll + bipolar(c.anim_spin) * (702.0 / 360.0) * dt).rem_euclid(1.0);
        self.color_phase = (self.color_phase + bipolar(c.color_spin) * (702.0 / 360.0) * dt).rem_euclid(1.0);
    }
}
