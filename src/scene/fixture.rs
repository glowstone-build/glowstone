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
use crate::optics::{OpticalControls, ShutterKind, WheelMotion};

/// One controllable fixture.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
    #[serde(skip)]
    pub gdtf: Option<Arc<GdtfFixture>>,

    /// World position of the fixture head, in metres. Y is up.
    pub position: Vec3,
    /// Base hang orientation in world space — the rigging rotation an MVR import
    /// places the fixture with (upside-down on truss, angled, etc.). Identity for
    /// app-created fixtures. Pan/tilt are applied *on top* of this.
    pub orientation: Quat,
    /// Commanded pan angle in degrees (rotation about world Y) — the *target*
    /// the head slews toward. The inspector and live DMX write this.
    pub pan: f32,
    /// Commanded tilt angle in degrees (rotation about the fixture's local X).
    /// 0 aims straight down `-Y`; 45 aims down-and-forward.
    pub tilt: f32,
    /// Physically-slewed actual pan/tilt the renderer uses, lagging the
    /// commanded `pan`/`tilt` at the head's motor speed (see [`advance_movement`]).
    ///
    /// [`advance_movement`]: Self::advance_movement
    pub pan_actual: f32,
    pub tilt_actual: f32,
    /// Current angular velocities (deg/s) for the accel-limited slew.
    pub pan_vel: f32,
    pub tilt_vel: f32,
    /// Motor-speed control `0..1` from GDTF `PositionMSpeed` (0 = fastest /
    /// "tracking", 1 = slowest). Scales the max slew rate.
    pub move_speed: f32,

    /// Emitted color, linear RGB in `0.0..=1.0`. For a multi-emitter GDTF
    /// fixture this is the manual master tint multiplied over every cell.
    pub color: [f32; 3],
    /// Master intensity / dimmer in `0.0..=1.0`.
    pub intensity: f32,
    /// Full beam cone angle in degrees (drives the beam indicator now, the
    /// volumetric beam later).
    pub beam_angle: f32,

    /// A laser engine — rendered as a thin haze-only streak (no inverse-square
    /// cone falloff, razor edge), not a lamp cone.
    pub is_laser: bool,
    /// Hidden in the viewport (the Scene outliner's eye toggle): the renderer
    /// skips its body, beam, and lens. Still patchable/selectable in the lists.
    pub hidden: bool,
    /// Per-fixture volumetric beam intensity multiplier (0 = no beam/pool, 1 =
    /// normal). Scales the projected beam shaft + floor pool only — the lens face
    /// still shows the fixture is lit. Lets a designer tame or kill hazy beams on
    /// specific fixtures without touching their colour or dimmer.
    pub beam: f32,
    /// Mechanical shutter blade style (our editable model; defaulted on import) —
    /// simulated on the lens face + the projected beam.
    pub shutter: ShutterKind,

    /// Active GDTF DMX mode — selects the geometry root (emitter layout), the
    /// component chain and the channel map. Synced from the patch table.
    pub mode_index: usize,
    /// Per-emitter cell color (linear RGB, level premultiplied, white folded),
    /// aligned with the mode's emitters. White at rest; live DMX rewrites it
    /// every frame from the per-cell color layers.
    pub cells: Vec<[f32; 3]>,

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

    /// Instantiate a fixture from a library profile at a world position.
    pub fn from_profile(profile: &FixtureProfile, name: impl Into<String>, position: Vec3) -> Self {
        Self {
            name: name.into(),
            profile: profile.name.to_string(),
            category: profile.category.to_string(),
            geometry: profile.geometry,
            gdtf: None,
            is_laser: profile.laser,
            hidden: false,
            beam: 1.0,
            position,
            orientation: Quat::IDENTITY,
            pan: 0.0,
            tilt: 0.0,
            pan_actual: 0.0,
            tilt_actual: 0.0,
            pan_vel: 0.0,
            tilt_vel: 0.0,
            move_speed: 0.0,
            color: profile.default_color,
            intensity: 1.0,
            beam_angle: profile.default_beam_angle,
            mode_index: 0,
            cells: Vec::new(),
            optics: OpticalControls::default(),
            motion: WheelMotion::default(),
            mvr: None,
            shutter: ShutterKind::None,
        }
    }

    /// Instantiate a fixture from an imported GDTF definition.
    pub fn from_gdtf(gdtf: Arc<GdtfFixture>, name: impl Into<String>, position: Vec3) -> Self {
        let beam_angle = gdtf.beam_angle.max(1.0);
        let is_laser = gdtf.beam.lamp_type.to_lowercase().contains("laser");
        let shutter = default_shutter(&gdtf);
        let mut f = Self {
            name: name.into(),
            profile: gdtf.name.clone(),
            category: gdtf.manufacturer.clone(),
            geometry: FixtureGeometry::Cylinder,
            gdtf: Some(gdtf),
            is_laser,
            hidden: false,
            beam: 1.0,
            position,
            orientation: Quat::IDENTITY,
            pan: 0.0,
            tilt: 0.0,
            pan_actual: 0.0,
            tilt_actual: 0.0,
            pan_vel: 0.0,
            tilt_vel: 0.0,
            move_speed: 0.0,
            color: [1.0, 1.0, 1.0],
            intensity: 1.0,
            beam_angle,
            mode_index: 0,
            cells: Vec::new(),
            optics: OpticalControls::default(),
            motion: WheelMotion::default(),
            mvr: None,
            shutter,
        };
        f.sync_mode();
        f
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
        // The patched DMX mode rides in the MVR metadata, by name.
        let mode_index = imported
            .gdtf
            .as_ref()
            .and_then(|g| g.modes.iter().position(|m| m.name == imported.meta.gdtf_mode))
            .unwrap_or(0);
        let is_laser = imported
            .gdtf
            .as_ref()
            .map(|g| g.beam.lamp_type.to_lowercase().contains("laser"))
            .unwrap_or(false);
        let shutter = imported.gdtf.as_ref().map(|g| default_shutter(g)).unwrap_or(ShutterKind::None);
        let mut f = Self {
            name: imported.name,
            profile,
            category,
            geometry: FixtureGeometry::Cylinder,
            gdtf: imported.gdtf,
            is_laser,
            hidden: false,
            beam: 1.0,
            position,
            orientation,
            pan: 0.0,
            tilt: 0.0,
            pan_actual: 0.0,
            tilt_actual: 0.0,
            pan_vel: 0.0,
            tilt_vel: 0.0,
            move_speed: 0.0,
            color: imported.color.unwrap_or([1.0, 1.0, 1.0]),
            // Master is always full; the level lives in the dimmer (which DMX
            // drives). See the `optics.dimmer = 0.0` below.
            intensity: 1.0,
            beam_angle,
            mode_index,
            cells: Vec::new(),
            optics: OpticalControls::default(),
            motion: WheelMotion::default(),
            mvr: Some(Box::new(imported.meta)),
            shutter,
        };
        // Imported rigs start blacked out — with no DMX feeding levels, a real
        // rig emits nothing, and importing 100+ fixtures at full would white out
        // the view. The dimmer (not the master) carries the level, so live DMX or
        // the Inspector's Dimmer / bulk-select brings them up.
        f.optics.dimmer = 0.0;
        f.sync_mode();
        f
    }

    pub fn is_gdtf(&self) -> bool {
        self.gdtf.is_some()
    }

    /// The active GDTF mode's emitters (empty for plain fixtures).
    pub fn emitters(&self) -> &[crate::gdtf::EmitterDef] {
        self.gdtf
            .as_ref()
            .map(|g| g.emitters(self.mode_index))
            .unwrap_or(&[])
    }

    /// Re-align the per-mode state (wheel controls, motion phases, cells) with
    /// the active GDTF mode. Call after construction or a mode change. Cells
    /// reset to white (manual rest state); live DMX rewrites them per frame.
    pub fn sync_mode(&mut self) {
        let Some(gdtf) = &self.gdtf else {
            return;
        };
        if self.mode_index >= gdtf.modes.len() {
            self.mode_index = 0;
        }
        let mode = &gdtf.modes[self.mode_index];
        self.optics.ensure_wheels(mode.components.len());
        if self.motion.phases.len() != mode.components.len() {
            self.motion.phases.resize(mode.components.len(), 0.0);
        }
        if self.motion.shake_phases.len() != mode.components.len() {
            self.motion.shake_phases.resize(mode.components.len(), 0.0);
        }
        if self.cells.len() != mode.emitters.len() {
            self.cells = vec![[1.0, 1.0, 1.0]; mode.emitters.len()];
        }
    }

    /// Mutable control for a wheel component by kind+number (UI/preset paths).
    pub fn wheel_control_mut(
        &mut self,
        kind: crate::gdtf::WheelKind,
        number: u32,
    ) -> Option<&mut crate::optics::WheelControl> {
        let gdtf = self.gdtf.as_ref()?;
        let mode = gdtf.modes.get(self.mode_index)?;
        let i = mode.component_index(kind, number)?;
        self.optics.ensure_wheels(mode.components.len());
        self.optics.wheels.get_mut(i)
    }

    /// Mutable accumulated motion phase for a component (preset/dev paths).
    pub fn wheel_phase_mut(
        &mut self,
        kind: crate::gdtf::WheelKind,
        number: u32,
    ) -> Option<&mut f32> {
        let gdtf = self.gdtf.as_ref()?;
        let mode = gdtf.modes.get(self.mode_index)?;
        let i = mode.component_index(kind, number)?;
        if self.motion.phases.len() != mode.components.len() {
            self.motion.phases.resize(mode.components.len(), 0.0);
        }
        self.motion.phases.get_mut(i)
    }

    /// Model matrix that places and aims the fixture body. Local convention:
    /// the body's lens points down `-Y` at rest; the base [`orientation`] (the
    /// MVR hang rotation, identity for app fixtures) applies first, then pan
    /// about world Y, then tilt about the local X axis. Rigid (no scale).
    ///
    /// Uses the physically-slewed [`pan_actual`]/[`tilt_actual`], NOT the
    /// commanded targets, so the rendered head lags realistically.
    ///
    /// [`orientation`]: Self::orientation
    /// [`pan_actual`]: Self::pan_actual
    /// [`tilt_actual`]: Self::tilt_actual
    pub fn model_matrix(&self) -> Mat4 {
        Mat4::from_translation(self.position)
            * Mat4::from_quat(self.orientation)
            * Mat4::from_rotation_y(self.pan_actual.to_radians())
            * Mat4::from_rotation_x(self.tilt_actual.to_radians())
    }

    /// Snap the slewed pan/tilt to the commanded target with zero velocity.
    /// Used on construction and by headless capture paths that set a pose and
    /// render without running the per-frame [`advance_movement`] integrator.
    ///
    /// [`advance_movement`]: Self::advance_movement
    pub fn snap_movement(&mut self) {
        self.pan_actual = self.pan;
        self.tilt_actual = self.tilt;
        self.pan_vel = 0.0;
        self.tilt_vel = 0.0;
        // Also settle the physical wheels (gobo/colour position + CMY flags) to
        // their targets so headless captures show the selected slot, not "open".
        if let Some(gdtf) = &self.gdtf
            && let Some(mode) = gdtf.modes.get(self.mode_index)
        {
            self.motion.settle(&self.optics, &mode.components);
        }
    }

    /// Max pan / tilt slew rate (deg/s) at this fixture's current motor speed.
    /// GDTF carries no real deg/s (`PositionMSpeed` is a normalised 0..1 motor
    /// control), so the fast ceiling is a per-class default (spec-sheet survey,
    /// `.context/research-movement.md`) and the slow end ≈ full travel in ~25.5 s
    /// (Robe/Martin DMX charts). Linearly interpolated by `move_speed`
    /// (0 = fast/"tracking", 1 = slowest).
    pub fn max_slew(&self) -> (f32, f32) {
        let (vmax_pan, vmax_tilt) = self.movement_class();
        const VMIN_PAN: f32 = 21.0; // ~540° in 25.5 s
        const VMIN_TILT: f32 = 11.0; // ~280° in 25.5 s
        let s = self.move_speed.clamp(0.0, 1.0);
        (
            vmax_pan + (VMIN_PAN - vmax_pan) * s,
            vmax_tilt + (VMIN_TILT - vmax_tilt) * s,
        )
    }

    /// Full-motor-speed pan/tilt ceiling (deg/s) by fixture class — beams are
    /// fastest, big LED washes (heavy front lens / large head) slowest.
    fn movement_class(&self) -> (f32, f32) {
        let hay = format!("{} {}", self.category.to_lowercase(), self.profile.to_lowercase());
        let has = |k: &str| hay.contains(k);
        if has("beam") {
            (260.0, 200.0)
        } else if has("wash") || self.emitters().len() > 4 {
            (160.0, 130.0) // heavier head / front lens
        } else {
            (220.0, 180.0) // default: mid spot / profile
        }
    }

    /// Advance the accel-limited pan/tilt slew toward the commanded target by
    /// `dt` seconds (trapezoidal velocity profile: ramp up to cruise, ramp down
    /// to stop without overshoot). Called once per real frame by
    /// [`Scene::advance`](super::Scene::advance).
    pub fn advance_movement(&mut self, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        let dt = dt.min(0.1);
        let (max_pan, max_tilt) = self.max_slew();
        // Reach cruise in ~0.18 s (the spec-sheet ramp time); also bounds decel.
        let accel = |max_v: f32| (max_v / 0.18).max(1.0);
        self.pan_actual = slew_axis(self.pan_actual, &mut self.pan_vel, self.pan, max_pan, accel(max_pan), dt);
        self.tilt_actual = slew_axis(self.tilt_actual, &mut self.tilt_vel, self.tilt, max_tilt, accel(max_tilt), dt);
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

/// One axis of an accel-limited follower (trapezoidal velocity profile): ramp
/// `vel` toward the speed that still lets it brake to `target`, integrate, and
/// settle exactly when slow + close. No wrap (a yoke goes to the absolute
/// angle, it doesn't take the short way round). Returns the new position.
fn slew_axis(cur: f32, vel: &mut f32, target: f32, max_v: f32, max_a: f32, dt: f32) -> f32 {
    let err = target - cur;
    if err.abs() < 1e-3 && vel.abs() < 1e-2 {
        *vel = 0.0;
        return target;
    }
    // Fastest speed from which we can still decelerate to a stop at the target.
    let v_brake = (2.0 * max_a * err.abs()).sqrt();
    let desired = err.signum() * max_v.min(v_brake);
    // Approach the desired velocity, slew-rate-limited by the acceleration.
    let dv = (desired - *vel).clamp(-max_a * dt, max_a * dt);
    *vel += dv;
    let next = cur + *vel * dt;
    // Snap on the frame we would cross the target (prevents tiny oscillation).
    if (target - next).signum() != err.signum() {
        *vel = 0.0;
        return target;
    }
    next
}

/// Default mechanical-shutter style for an imported GDTF: a fixture with any
/// Shutter channel gets a `Blade` (most strobe shutters are blades); the user can
/// switch it to Sawtooth or None in the inspector. GDTF doesn't describe blade
/// geometry, so this is our model filling the gap.
fn default_shutter(gdtf: &GdtfFixture) -> ShutterKind {
    let has_shutter = gdtf.modes.iter().any(|m| {
        m.channels.iter().any(|c| {
            c.attribute.starts_with("Shutter")
                || c.functions.iter().any(|f| f.attribute.starts_with("Shutter"))
        })
    });
    if has_shutter { ShutterKind::Blade } else { ShutterKind::None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::library::Library;

    /// The trapezoidal slew reaches the target without overshoot, and a slower
    /// motor speed takes longer.
    #[test]
    fn slew_reaches_target_no_overshoot() {
        let run = |move_speed: f32| -> (f32, usize) {
            let lib = Library::standard();
            let mut f = Fixture::from_profile(&lib.fixtures[0], "t", Vec3::ZERO);
            f.move_speed = move_speed;
            f.pan = 270.0; // command a big move from 0
            let mut max_seen: f32 = 0.0;
            let mut frames = 0;
            for _ in 0..6000 {
                f.advance_movement(1.0 / 120.0);
                max_seen = max_seen.max(f.pan_actual);
                frames += 1;
                if (f.pan_actual - 270.0).abs() < 0.05 {
                    break;
                }
            }
            (max_seen, frames)
        };
        let (max_fast, n_fast) = run(0.0);
        let (_max_slow, n_slow) = run(1.0);
        assert!(max_fast <= 270.0 + 0.5, "no overshoot, peaked at {max_fast}");
        assert!((max_fast - 270.0).abs() < 1.0, "reaches target, got {max_fast}");
        assert!(n_slow > n_fast * 3, "slow motor is much slower: {n_slow} vs {n_fast}");
    }

    /// `snap_movement` settles instantly to the commanded pose.
    #[test]
    fn snap_settles_immediately() {
        let lib = Library::standard();
        let mut f = Fixture::from_profile(&lib.fixtures[0], "t", Vec3::ZERO);
        f.pan = 120.0;
        f.tilt = -40.0;
        f.snap_movement();
        assert_eq!(f.pan_actual, 120.0);
        assert_eq!(f.tilt_actual, -40.0);
        assert_eq!(f.pan_vel, 0.0);
    }
}
