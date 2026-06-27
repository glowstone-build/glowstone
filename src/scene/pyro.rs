//! Stage **pyro devices** — a CO2 cannon/jet and a cold-spark machine.
//!
//! Modelled exactly like [`LedScreen`](super::screen::LedScreen): a placed,
//! serializable *descriptor* (transform + physical/visual tunables + an inline
//! DMX patch) that the renderer reads to drive a **billboard particle / fog
//! pass**. The live particle simulation is runtime-only and owned by the
//! renderer (keyed by [`EntityId`](super::EntityId)) — never serialized, exactly
//! like `LedScreen::frame` / the renderer's `screen_runtime`.
//!
//! The two kinds are one feature with two rendering models (see
//! `docs/RESEARCH-pyro.md`):
//! - **CO2 jet** — a fast, dense, short-lived WHITE fog column that catches
//!   coloured stage light (alpha-blended, *lit* not emissive).
//! - **Cold spark** — an upward golden spark fountain: ballistic, additive,
//!   velocity-stretched sprites whose colour cools gold→orange→red (blackbody).
//!
//! DMX footprints (verified against MagicFX / Showven Sparkular / Club Cannon /
//! CryoFX manuals) are decoded inline in `dmx::decode::apply_pyro`, NOT through
//! the fixture `PatchTable` — same as the LED pixel-map path.

use glam::{Mat4, Quat, Vec3};

/// Which pyro device this is — selects the rendering model + parameter ranges.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PyroKind {
    /// CO2 cannon / cryo jet — a rising white fog column.
    Co2Jet,
    /// Cold-spark machine ("cold pyro" / Sparkular-type) — a golden fountain.
    ColdSpark,
}

impl PyroKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Co2Jet => "CO2 Jet",
            Self::ColdSpark => "Cold Spark",
        }
    }
}

/// DMX footprint mode. Both modes share the same channel order, so the minimal
/// mode is a strict prefix of the rich one (mirrors GDTF modes / the two-mode
/// LED-screen idea). Channel counts: CO2 → 1 / 7, Spark → 3 / 5.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PyroMode {
    /// CO2 "Blast" (1ch) / Spark "Spark" (3ch).
    Minimal,
    /// CO2 "Safe Jet" (7ch, incl. arm + duration + pan/tilt) / Spark "Spark+" (5ch).
    Rich,
}

impl PyroMode {
    pub const ALL: [PyroMode; 2] = [Self::Minimal, Self::Rich];

    pub fn label(self) -> &'static str {
        match self {
            Self::Minimal => "Minimal",
            Self::Rich => "Rich",
        }
    }

    /// Channel footprint width for `kind` in this mode.
    pub fn footprint(self, kind: PyroKind) -> u16 {
        match (kind, self) {
            (PyroKind::Co2Jet, Self::Minimal) => 1,
            (PyroKind::Co2Jet, Self::Rich) => 7,
            (PyroKind::ColdSpark, Self::Minimal) => 3,
            (PyroKind::ColdSpark, Self::Rich) => 5,
        }
    }
}

/// An inline DMX patch for a pyro device (universe + 1-based start address).
/// Patched directly on the device like [`PixelMap`](super::screen::PixelMap),
/// NOT through the fixture `PatchTable` — so it never churns fixture
/// fingerprints and persists with the show.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PyroPatch {
    pub universe: u16,
    /// 1-based start channel.
    pub address: u16,
}

impl Default for PyroPatch {
    fn default() -> Self {
        Self { universe: 1, address: 1 }
    }
}

/// A placed pyro device. Only stable, serializable descriptor fields live here;
/// the live particle simulation is runtime-only in the renderer (keyed by `id`).
/// New serialized fields go on the END so the positional `.archie` stream stays
/// aligned (the format version is bumped when this struct changes — see
/// `ui/project.rs` FORMAT).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PyroDevice {
    pub name: String,
    pub kind: PyroKind,
    /// Library component this was built from (display / info only).
    pub profile_name: String,
    /// World placement (Y-up, metres). The **nozzle** sits at the transform
    /// origin and the effect fires along local **+Y** (modified by pan/tilt).
    /// Mirrors [`LedScreen::transform`](super::screen::LedScreen::transform).
    pub transform: Mat4,

    /// DMX footprint mode (selects the channel layout decoded in `apply_pyro`).
    pub mode: PyroMode,
    /// Inline DMX patch (`None` = unpatched → free-runs at `density` for glowstone).
    pub patch: Option<PyroPatch>,

    // --- physical / visual tunables (defaults from the profile; see RESEARCH-pyro §4) ---
    /// Cold-spark fountain apex height (m). Drives launch speed `v=√(2·g·h)`.
    pub height_m: f32,
    /// CO2 column throw height (m). Drives the puff launch speed.
    pub throw_m: f32,
    /// Master output / spark density `0..1` — the free-run amount when unpatched,
    /// and the spawn-rate scale (the dominant perf lever).
    pub density: f32,
    /// Spread / cone half-angle at the nozzle (degrees).
    pub cone_deg: f32,
    /// Cold-spark hot-base blackbody temperature (K) — gold↔white.
    pub color_t0_k: f32,
    /// Cold-spark cooling-tip blackbody temperature (K) — dim red↔orange.
    pub color_t1_k: f32,
    /// Cold-spark HDR emission brightness (drives bloom).
    pub brightness: f32,
    /// CO2 core opacity `0..1`.
    pub opacity: f32,
    /// CO2 wash tint (linear RGB) — the plume is white; a faint nozzle-LED tint
    /// can warm/cool it. The stage lighting does the real colouring.
    pub tint: [f32; 3],
    /// Rotating-nozzle spin (RPM, Spin variant; 0 = static).
    pub spin_rpm: f32,
    /// Moving-variant aim: pan about +Y (deg) and tilt about local X (deg).
    pub pan: f32,
    pub tilt: f32,

    // --- quality / perf ---
    /// Hard cap on live particles for this emitter (spawn is throttled so the
    /// live count never exceeds it → perf never tanks).
    pub max_particles: u32,
    /// Quality preset 0=Low 1=Med 2=High 3=Ultra (scales substeps / soft fade /
    /// curl / CO2 volumetric).
    pub quality: u8,

    /// Hidden in the viewport (outliner eye toggle): not drawn, not pickable.
    pub hidden: bool,

    /// CO2 smoke hang time in **seconds** — the nominal puff lifetime, so the smoke
    /// lingers ~this long after the valve shuts (jittered per-puff so the cauliflower
    /// edges dissolve raggedly). Output stays INSTANT regardless. No hard limit — the
    /// inspector slider has a suggested range but accepts any raw value. FORMAT 10.
    #[serde(default = "default_dissipation")]
    pub dissipation: f32,

    /// CO2 jet **exit velocity** (m/s) — the launch speed, decoupled from `throw_m`
    /// (which now only sets how high it billows). No hard limit. FORMAT 11.
    #[serde(default = "default_speed")]
    pub speed: f32,

    /// Live-VIEWPORT CO2 quality: `false` = Fast preview (the expensive per-beam
    /// smoke shadowing is skipped — smooth editing); `true` = Full quality live
    /// (matches a render, heavier). **Exports/renders are ALWAYS full quality** —
    /// this only trades preview speed vs fidelity. FORMAT 12.
    #[serde(default)]
    pub viewport_hq: bool,

    /// CO2 visual **density** (multiplies the smoke extinction). Higher = denser, with
    /// a darker self-shadowed core whose dark region spreads + a lighter rim. Distinct
    /// from `opacity` (per-puff) and `density` (output rate). No hard limit. FORMAT 13.
    #[serde(default = "default_thickness")]
    pub thickness: f32,

    // --- runtime control (serde-skip — written by DMX decode each frame) ---
    /// True when a patched device is receiving live DMX (so the sim uses the
    /// decoded `armed`/`fire` instead of free-running at `density`).
    #[serde(skip)]
    pub driven: bool,
    /// Safety/arm state (decoded from the Arm/Safety channel).
    #[serde(skip)]
    pub armed: bool,
    /// Live commanded output `0..1` this frame (CO2 blast level or spark amount).
    #[serde(skip)]
    pub fire: f32,
    /// Session-stable identity (serde-skip → reassigned by `Scene::ensure_ids`).
    #[serde(skip)]
    pub id: super::EntityId,
}

/// Default CO2 hang time, in seconds (FORMAT-10 `dissipation`).
pub(crate) fn default_dissipation() -> f32 {
    2.5
}
/// Default CO2 jet exit velocity, m/s (FORMAT-11 `speed`).
pub(crate) fn default_speed() -> f32 {
    11.0
}
/// Default CO2 visual density multiplier (FORMAT-13 `thickness`).
pub(crate) fn default_thickness() -> f32 {
    1.0
}

impl crate::dmx::patch::Patchable for PyroDevice {
    fn footprint(&self) -> u16 {
        self.mode.footprint(self.kind)
    }
    fn patch_slot(&self) -> Option<(u16, u16)> {
        self.patch.as_ref().map(|p| (p.universe, p.address))
    }
    fn set_patch(&mut self, universe: u16, address: u16) {
        self.patch = Some(PyroPatch { universe, address });
    }
    fn clear_patch(&mut self) {
        self.patch = None;
    }
}


impl PyroDevice {
    /// Build a device from a library profile at `transform` (nozzle at origin,
    /// aiming +Y).
    pub fn from_profile(
        profile: &super::library::PyroProfile,
        name: impl Into<String>,
        transform: Mat4,
    ) -> Self {
        let kind = profile.kind;
        let spark = kind == PyroKind::ColdSpark;
        Self {
            name: name.into(),
            kind,
            profile_name: profile.name.to_string(),
            transform,
            mode: PyroMode::Minimal,
            patch: None,
            height_m: profile.default_height_m,
            throw_m: profile.default_throw_m,
            // Free-run at a visible default so a freshly-placed device shows its
            // effect immediately (DMX overrides this once patched + live).
            density: if spark { 0.55 } else { 0.0 },
            cone_deg: if spark { 10.0 } else { 8.0 },
            color_t0_k: profile.default_color_t0_k,
            color_t1_k: 1100.0,
            brightness: 8.0,
            opacity: 0.85,
            tint: [1.0, 0.97, 0.93],
            spin_rpm: 0.0,
            pan: 0.0,
            tilt: 0.0,
            max_particles: profile.default_max_particles,
            quality: 2,
            hidden: false,
            dissipation: default_dissipation(),
            speed: (profile.default_throw_m * 1.3).max(6.0),
            viewport_hq: false,
            thickness: default_thickness(),
            driven: false,
            armed: true,
            fire: 0.0,
            id: 0,
        }
    }

    /// World-space nozzle position (the effect origin).
    pub fn world_nozzle(&self) -> Vec3 {
        self.transform.transform_point3(Vec3::ZERO)
    }

    /// World-space firing direction (local +Y, rotated by pan/tilt then the
    /// placement transform), normalised.
    pub fn world_dir(&self) -> Vec3 {
        let aim = Quat::from_rotation_y(self.pan.to_radians())
            * Quat::from_rotation_x(-self.tilt.to_radians())
            * Vec3::Y;
        self.transform.transform_vector3(aim).normalize_or(Vec3::Y)
    }

    /// The live emission amount the sim should use this frame: the DMX-commanded
    /// `fire` when driven & armed, else the manual `density` (free-run preview).
    pub fn emit_amount(&self) -> f32 {
        if self.driven {
            if self.armed { self.fire } else { 0.0 }
        } else {
            self.density
        }
        .clamp(0.0, 1.0)
    }

    /// Working height/throw (the apex used by the sim), kind-aware.
    pub fn working_height(&self) -> f32 {
        match self.kind {
            PyroKind::ColdSpark => self.height_m.clamp(0.3, 8.0),
            PyroKind::Co2Jet => self.throw_m.clamp(1.0, 25.0),
        }
    }

    /// World-space AABB of the device body + the effect envelope — for outliner
    /// framing and a coarse pick fallback. A vertical capsule from the nozzle up
    /// to the apex, widened by the spread.
    pub fn world_bounds(&self) -> (Vec3, Vec3) {
        let n = self.world_nozzle();
        let apex = n + self.world_dir() * self.working_height();
        let r = (self.working_height() * (self.cone_deg.to_radians()).tan()).max(0.4) + 0.3;
        let rv = Vec3::splat(r);
        let lo = n.min(apex) - rv;
        let hi = n.max(apex) + rv;
        (lo, hi)
    }

    /// Nearest positive ray hit against a small body box at the nozzle (so the
    /// device is clickable in the viewport, like a fixture).
    pub fn ray_hit(&self, ro: Vec3, rd: Vec3) -> Option<f32> {
        let c = self.world_nozzle();
        let h = Vec3::new(0.25, 0.18, 0.25);
        ray_aabb(ro, rd, c - h, c + h)
    }
}

/// Slab ray–AABB; returns the near intersection distance if the ray enters the
/// box ahead of the origin.
fn ray_aabb(ro: Vec3, rd: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let inv = Vec3::new(1.0 / rd.x, 1.0 / rd.y, 1.0 / rd.z);
    let t0 = (lo - ro) * inv;
    let t1 = (hi - ro) * inv;
    let tmin = t0.min(t1);
    let tmax = t0.max(t1);
    let near = tmin.x.max(tmin.y).max(tmin.z);
    let far = tmax.x.min(tmax.y).min(tmax.z);
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::library::Library;

    fn spark() -> PyroDevice {
        let lib = Library::standard();
        let p = lib.pyro.iter().find(|p| p.kind == PyroKind::ColdSpark).unwrap();
        PyroDevice::from_profile(p, "Spark", Mat4::IDENTITY)
    }

    /// Pyro now persists as a self-describing JSON trailer (see `ui::project`), so a
    /// plain derive round-trips AND an older save missing newer fields loads via
    /// `#[serde(default)]` — no positional/version-aware decode, no FORMAT-per-field.
    #[test]
    fn pyro_device_roundtrips_and_defaults_missing_fields() {
        let mut d = spark();
        d.name = "blast".into();
        d.throw_m = 2.5;
        d.thickness = 2.2;
        d.viewport_hq = true;
        let json = serde_json::to_string(&d).unwrap();
        let back: PyroDevice = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "blast");
        assert_eq!(back.throw_m, 2.5);
        assert_eq!(back.thickness, 2.2);
        assert!(back.viewport_hq);

        // Simulate an OLDER save: drop the fields added after pyro shipped. The
        // remaining show data still loads; the absent fields take their defaults.
        let mut obj: serde_json::Value = serde_json::from_str(&json).unwrap();
        let map = obj.as_object_mut().unwrap();
        for f in ["thickness", "viewport_hq", "speed", "dissipation"] {
            map.remove(f);
        }
        let old: PyroDevice = serde_json::from_value(obj).unwrap();
        assert_eq!(old.throw_m, 2.5); // pre-existing field preserved
        assert_eq!(old.thickness, default_thickness());
        assert_eq!(old.speed, default_speed());
        assert_eq!(old.dissipation, default_dissipation());
        assert!(!old.viewport_hq);
    }

    #[test]
    fn footprint_widths() {
        assert_eq!(PyroMode::Minimal.footprint(PyroKind::Co2Jet), 1);
        assert_eq!(PyroMode::Rich.footprint(PyroKind::Co2Jet), 7);
        assert_eq!(PyroMode::Minimal.footprint(PyroKind::ColdSpark), 3);
        assert_eq!(PyroMode::Rich.footprint(PyroKind::ColdSpark), 5);
    }

    #[test]
    fn unpatched_device_free_runs_at_density() {
        let mut s = spark();
        s.density = 0.7;
        s.driven = false;
        assert!((s.emit_amount() - 0.7).abs() < 1e-6);
        // Driven + disarmed → inert regardless of density.
        s.driven = true;
        s.armed = false;
        s.fire = 1.0;
        assert_eq!(s.emit_amount(), 0.0);
        // Driven + armed → uses the DMX fire level.
        s.armed = true;
        s.fire = 0.4;
        assert!((s.emit_amount() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn aim_default_is_up() {
        let s = spark();
        assert!((s.world_dir() - Vec3::Y).length() < 1e-5);
    }
}
