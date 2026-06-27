//! An environment volume — currently a box of participating media (haze/fog).
//!
//! Today it only contributes a wireframe gizmo showing the volume bounds. Its
//! parameters (`density`, `color`, `anisotropy`) are the inputs the upcoming
//! volumetric pass will read, so they live here now as a clean seam. See
//! `docs/RESEARCH-volumetrics.md`.

use glam::Vec3;

use super::library::{EnvironmentKind, EnvironmentProfile};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
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
    /// Haze uniformity in `0..=1`: 1 = smooth even fog; 0 = strong clusters of
    /// smoke/clouds (dense pockets scatter more → brighter wisps with clear gaps
    /// between). Drives the density-noise contrast in volumetric.wgsl. (Serialized
    /// with the show.)
    pub uniformity: f32,
    /// Cluster contrast in `0..=1`: how much DENSER (brighter) the smoke clusters are
    /// vs the surrounding haze, and how clear the gaps. Higher = pockets pop harder.
    /// Only matters as `uniformity` drops below 1. (Serialized with the show.)
    pub cluster_contrast: f32,

    /// Hidden in the viewport (the Scene outliner's eye toggle). serde-skip →
    /// session-only, not persisted (matches the other entities' eye,
    /// which reuse a persisted `hidden`; environments gain it session-only here).
    // Wired to the outliner eye column (the tree's visibility toggle).
    #[serde(skip)]
    pub hidden: bool,
    /// Session-stable identity (serde-skip → reassigned by `Scene::ensure_ids`).
    #[serde(skip)]
    pub id: super::EntityId,
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
            uniformity: 0.0,
            cluster_contrast: 0.345,
            hidden: false,
            id: 0, // assigned by Scene::add_environment / ensure_ids
        }
    }

    pub fn min(&self) -> Vec3 {
        self.center - self.size * 0.5
    }

    pub fn max(&self) -> Vec3 {
        self.center + self.size * 0.5
    }
}
