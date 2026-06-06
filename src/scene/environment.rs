//! An environment volume — currently a box of participating media (haze/fog).
//!
//! Today it only contributes a wireframe gizmo showing the volume bounds. Its
//! parameters (`density`, `color`, `anisotropy`) are the inputs the upcoming
//! volumetric pass will read, so they live here now as a clean seam. See
//! `docs/RESEARCH-volumetrics.md`.

use glam::Vec3;

use super::library::{EnvironmentKind, EnvironmentProfile};

#[derive(Clone, Debug)]
pub struct Environment {
    pub name: String,
    pub kind: EnvironmentKind,

    /// Box center in world space, metres.
    pub center: Vec3,
    /// Box size (width, height, depth), metres.
    pub size: Vec3,

    /// Uniform extinction density (sigma_t scale) for now.
    pub density: f32,
    /// Scattering tint / albedo, linear RGB.
    pub color: [f32; 3],
    /// Henyey-Greenstein anisotropy `g` in `-1..=1` (forward scattering > 0).
    pub anisotropy: f32,
}

impl Environment {
    pub fn from_profile(
        profile: &EnvironmentProfile,
        name: impl Into<String>,
        center: Vec3,
    ) -> Self {
        Self {
            name: name.into(),
            kind: profile.kind,
            center,
            size: Vec3::from_array(profile.default_size),
            density: profile.default_density,
            color: [0.7, 0.72, 0.78],
            anisotropy: 0.25,
        }
    }

    pub fn min(&self) -> Vec3 {
        self.center - self.size * 0.5
    }

    pub fn max(&self) -> Vec3 {
        self.center + self.size * 0.5
    }
}
