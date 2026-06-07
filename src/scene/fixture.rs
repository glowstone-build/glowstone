//! A single lighting fixture instance in the scene.
//!
//! Plain data — no ECS. The fields are the GPU-friendly parameters the renderer
//! consumes each frame and the volumetric pass will consume later. A fixture is
//! created from a [`FixtureProfile`] in the library and then edited freely.

use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};

use super::library::{FixtureGeometry, FixtureProfile};
use crate::gdtf::GdtfFixture;
use crate::mvr::MvrFixtureMeta;
use crate::optics::{OpticalControls, WheelMotion};

/// One controllable fixture.
#[derive(Clone)]
pub struct Fixture {
    /// Instance name (e.g. "PAR Can 1").
    pub name: String,
    /// The library profile this came from (for display / grouping).
    pub profile: String,
    pub category: String,
    /// Body geometry the renderer draws for the placeholder (non-GDTF) path.
    pub geometry: FixtureGeometry,
    /// When set, the renderer draws this GDTF fixture's real 3D model and the
    /// beam comes from its Beam geometry.
    pub gdtf: Option<Arc<GdtfFixture>>,

    /// World position of the fixture head, in metres. Y is up.
    pub position: Vec3,
    /// Base hang orientation in world space — the rigging rotation an MVR import
    /// places the fixture with (upside-down on truss, angled, etc.). Identity for
    /// app-created fixtures. Pan/tilt are applied *on top* of this.
    pub orientation: Quat,
    /// Pan angle in degrees (rotation about world Y).
    pub pan: f32,
    /// Tilt angle in degrees (rotation about the fixture's local X). 0 aims
    /// straight down `-Y`; 45 aims down-and-forward.
    pub tilt: f32,

    /// Emitted color, linear RGB in `0.0..=1.0`.
    pub color: [f32; 3],
    /// Master intensity / dimmer in `0.0..=1.0`.
    pub intensity: f32,
    /// Full beam cone angle in degrees (drives the beam indicator now, the
    /// volumetric beam later).
    pub beam_angle: f32,

    /// The optical-chain control values (focus / frost / prism / color / gobo /
    /// animation / shutter …). Drives the GDTF optical model; neutral by default.
    pub optics: OpticalControls,
    /// Accumulated wheel-motion phases, advanced once per frame by
    /// [`Scene::advance`](super::Scene::advance).
    pub motion: WheelMotion,

    /// MVR round-trip metadata (UUID, FixtureID, DMX patch, class/position refs,
    /// custom commands) when this fixture came from — or is destined for — an
    /// MVR scene. `None` for purely app-created fixtures.
    pub mvr: Option<Box<MvrFixtureMeta>>,
}

impl Fixture {
    /// Length of the PAR-can body in metres (also the lens depth).
    pub const BODY_LENGTH: f32 = 0.32;
    /// Radius of the PAR-can body in metres.
    pub const BODY_RADIUS: f32 = 0.16;

    /// Number of DMX channels a fixture would occupy (pan, tilt, dimmer, R, G,
    /// B). Used by the DMX Monitor stub to lay out a faux patch.
    pub const DMX_FOOTPRINT: u32 = 6;

    /// Instantiate a fixture from a library profile at a world position.
    pub fn from_profile(profile: &FixtureProfile, name: impl Into<String>, position: Vec3) -> Self {
        Self {
            name: name.into(),
            profile: profile.name.to_string(),
            category: profile.category.to_string(),
            geometry: profile.geometry,
            gdtf: None,
            position,
            orientation: Quat::IDENTITY,
            pan: 0.0,
            tilt: 0.0,
            color: profile.default_color,
            intensity: 1.0,
            beam_angle: profile.default_beam_angle,
            optics: OpticalControls::default(),
            motion: WheelMotion::default(),
            mvr: None,
        }
    }

    /// Instantiate a fixture from an imported GDTF definition.
    pub fn from_gdtf(gdtf: Arc<GdtfFixture>, name: impl Into<String>, position: Vec3) -> Self {
        let beam_angle = gdtf.beam_angle.max(1.0);
        Self {
            name: name.into(),
            profile: gdtf.name.clone(),
            category: gdtf.manufacturer.clone(),
            geometry: FixtureGeometry::Cylinder,
            gdtf: Some(gdtf),
            position,
            orientation: Quat::IDENTITY,
            pan: 0.0,
            tilt: 0.0,
            color: [1.0, 0.95, 0.85],
            intensity: 1.0,
            beam_angle,
            optics: OpticalControls::default(),
            motion: WheelMotion::default(),
            mvr: None,
        }
    }

    /// Instantiate a fixture from an MVR import: a parsed GDTF plus a world-space
    /// base transform (decomposed into position + hang orientation) and the
    /// round-trip metadata. `color`/`gdtf` may be absent if the GDTF didn't
    /// resolve, in which case it renders as a placeholder.
    pub fn from_mvr(imported: crate::mvr::ImportedFixture) -> Self {
        let (position, orientation) = crate::mvr::fixture_base(imported.world);
        let beam_angle = imported.gdtf.as_ref().map(|g| g.beam_angle.max(1.0)).unwrap_or(15.0);
        let (profile, category) = match &imported.gdtf {
            Some(g) => (g.name.clone(), g.manufacturer.clone()),
            None => (imported.meta.gdtf_spec.clone(), "Unresolved".to_string()),
        };
        Self {
            name: imported.name,
            profile,
            category,
            geometry: FixtureGeometry::Cylinder,
            gdtf: imported.gdtf,
            position,
            orientation,
            pan: 0.0,
            tilt: 0.0,
            color: imported.color.unwrap_or([1.0, 0.95, 0.85]),
            // Imported rigs start blacked out — with no DMX feeding levels, a
            // real rig emits nothing. The user (or, later, live DMX) brings
            // fixtures up; importing 100+ fixtures at full would white out the
            // view. Edit intensity in the Inspector or bulk-select to bring up.
            intensity: 0.0,
            beam_angle,
            optics: OpticalControls::default(),
            motion: WheelMotion::default(),
            mvr: Some(Box::new(imported.meta)),
        }
    }

    pub fn is_gdtf(&self) -> bool {
        self.gdtf.is_some()
    }

    /// Model matrix that places and aims the fixture body. Local convention:
    /// the body's lens points down `-Y` at rest; the base [`orientation`] (the
    /// MVR hang rotation, identity for app fixtures) applies first, then pan
    /// about world Y, then tilt about the local X axis. Rigid (no scale).
    ///
    /// [`orientation`]: Self::orientation
    pub fn model_matrix(&self) -> Mat4 {
        Mat4::from_translation(self.position)
            * Mat4::from_quat(self.orientation)
            * Mat4::from_rotation_y(self.pan.to_radians())
            * Mat4::from_rotation_x(self.tilt.to_radians())
    }

    /// World-space position of the lens (where the beam exits).
    pub fn lens_position(&self) -> Vec3 {
        self.model_matrix()
            .transform_point3(Vec3::new(0.0, -Self::BODY_LENGTH, 0.0))
    }

    /// World-space unit direction the beam points.
    pub fn beam_direction(&self) -> Vec3 {
        self.model_matrix()
            .transform_vector3(Vec3::new(0.0, -1.0, 0.0))
            .normalize_or_zero()
    }
}
