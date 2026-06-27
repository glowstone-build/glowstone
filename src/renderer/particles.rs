//! Stage-pyro runtime simulation, renderer-owned (never serialized / undo-cloned).
//!
//! - **Cold sparks** are emissive points → a CPU billboard particle fountain
//!   (blackbody colour-over-life, additive), built into [`ParticleInstance`]s.
//! - **CO2** is dense participating media → a real PARTICLE SMOKE SIMULATION
//!   ([`Co2Sim`]): a high-pressure jet launch + age-rising Stokes drag (the stem
//!   shoots, the head stalls), a small decaying buoyant updraft (dry-ice is dense,
//!   not hot), Bridson **curl-noise** turbulence (divergence-free churning, scaled
//!   by a height amplitude ramp so the stalk stays coherent + only the head
//!   churns), and an emergent **vortex-ring** kick on the leading edge → the
//!   billowing mushroom/cauliflower head. The puffs are voxelized (Gaussian splat)
//!   into a per-emitter density grid that the existing volumetric raymarch samples
//!   + lights (every beam scatters through it, with thickness self-shadow + beam-
//!   blocking) — the proven volumetric look, fed by a real simulation. Physics
//!   distilled from the smoke-sim research (`docs/RESEARCH-pyro.md` lineage).
//!
//! Advanced once per real frame (`Renderer::advance_pyro`, the app tick) — never
//! in `record_scene` (the double-advance trap).

use std::collections::HashMap;
use std::f32::consts::TAU;

use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Quat, Vec3};

use crate::scene::pyro::{PyroDevice, PyroKind};
use crate::scene::{EntityId, Scene};

use super::mesh::ParticleInstance;

/// Beyond `LOD_NEAR_M` a spark emitter spawns proportionally fewer particles; past
/// `LOD_CULL_M` it spawns none and freezes (it's a speck on screen anyway).
const LOD_NEAR_M: f32 = 35.0;
const LOD_CULL_M: f32 = 120.0;

/// CO2 density grid resolution (voxels per axis, per emitter), by device `quality`
/// preset. The grid AABB follows the live particle cloud, so a higher resolution =
/// finer voxels = crisper cauliflower edges — at more CPU splat + upload cost, which
/// is why it's a user-chosen quality knob, not a fixed value.
pub fn quality_res(quality: u8) -> u32 {
    match quality.min(3) {
        0 => 40,  // Low
        1 => 52,  // Medium
        2 => 64,  // High (default)
        _ => 84,  // Ultra
    }
}
/// Initial / fallback grid resolution before any device's quality is known.
pub const CO2_RES_INIT: u32 = 64;
/// Max simultaneous splatted plumes (texture Z-slab layers).
pub const CO2_LAYERS: u32 = 8;

// =====================================================================================
// CO2 particle smoke simulation
// =====================================================================================

/// Tunable physical parameters for the CO2 sim (defaults match a real stage CO2
/// cannon: thin fast stem → mushroom cap, dark thick core, fast rise, ~1–3 s
/// dissipation). Most are derived per-device from the [`PyroDevice`].
#[derive(Clone, Copy)]
pub struct Co2Tune {
    pub exit_speed: f32,   // m/s — high-pressure valve exit velocity (a CANNON)
    pub cone_inner: f32,   // rad — tight launch cone
    pub cone_outer: f32,
    pub nozzle_radius: f32, // m — birth disk
    pub drag_k0: f32,      // 1/s — fresh stem (low drag → shoots up)
    pub drag_k1: f32,      // 1/s — aged head (high drag → stalls → billows)
    pub buoy0: f32,        // m/s² — entrained-air updraft (small; dry-ice is dense)
    pub buoy_decay: f32,   // 1/s — rise then stall
    pub g_settle: f32,     // m/s² — slight downward settle (cold/dense), NOT 9.81
    pub turb_str: f32,     // m/s² — curl turbulence accel
    pub turb_freq: f32,    // 1/m — eddy scale (big rolling billows)
    pub turb_scroll: f32,  // time-advance of the curl field (evolving eddies)
    pub ring_out: f32,     // m/s² — cap radial-outward flare (mushroom)
    pub ring_down: f32,    // m/s² — cap roll-back-down (toroidal recirculation)
    pub front_frac: f32,   // cap zone is h > front_frac * front_h (EMA)
    pub life_min: f32,
    pub life_max: f32,
    pub grow_rate: f32,    // radius = base * (1 + grow * t)
    pub base_radius: f32,  // m — birth puff radius (big & few)
    pub opacity_peak: f32, // peak splat density of a hold puff
    pub burst_count: u32,  // puffs on the trigger rising edge (at output = 1)
    pub sustain_rate: f32, // puffs/s while held
    pub max_particles: usize,
    pub sigma_scale: f32,  // grid density 0..1 → raymarch extinction σₜ
}

impl Co2Tune {
    /// Per-device tune. User inputs are taken RAW (only sanity floors, never an
    /// upper limit — the inspector lets the user enter any value): `speed` is the
    /// jet exit velocity, `throw` how high it billows, `dissipation` the hang time
    /// in seconds, `cone`/`opacity` the spread/density.
    fn for_device(dev: &PyroDevice) -> Self {
        let throw = dev.throw_m.max(0.0);
        let outer = dev.cone_deg.max(0.0).to_radians();
        // Dissipation is now SECONDS — the nominal puff lifetime — with a wide
        // per-puff jitter (0.45×…1.5×) so puffs die at different times and the
        // cauliflower edges dissolve raggedly, not in a uniform pop.
        let diss = dev.dissipation.max(0.05);
        Self {
            // Exit velocity is the user's Speed (decoupled from throw); throw now only
            // feeds the buoyant rise (how high it billows up).
            exit_speed: dev.speed.max(0.0),
            cone_inner: outer * 0.35,
            cone_outer: outer,
            nozzle_radius: 0.04,
            drag_k0: 0.9,
            drag_k1: 2.0,
            buoy0: (throw * 0.22).max(0.0),
            buoy_decay: 2.0,
            g_settle: 0.5,
            turb_str: 4.2,
            turb_freq: 0.6,
            turb_scroll: 0.4,
            ring_out: 3.6,
            ring_down: 1.6,
            front_frac: 0.62,
            life_min: diss * 0.45,
            life_max: diss * 1.5,
            grow_rate: 2.7,
            // Smaller birth puffs → a THIN stem (a small nozzle, not a huge circle
            // cannon); they still grow into a wide billowing head via grow_rate.
            base_radius: 0.15,
            // Translucent, feathery jet (the reference is see-through, NOT a solid
            // opaque wall): lower peak opacity + a much lower σ-scale so the plume
            // reads as wispy white smoke you can see through, thickening only where
            // puffs pile up in the head. Small launch burst so the START is thin and
            // builds, not an instant thick blob.
            opacity_peak: (dev.opacity * 0.7).max(0.0),
            // A thinner stem needs more puffs to stay coherent + dense.
            burst_count: 55,
            sustain_rate: 1150.0,
            // Not user-facing; a generous safety bound keeps a runaway from OOMing.
            max_particles: dev.max_particles.clamp(50, 6000) as usize,
            // Visual density is user-driven (the Density slider) → scales extinction:
            // denser smoke = darker self-shadowed core + lighter rim.
            sigma_scale: 9.0 * dev.thickness.max(0.0),
        }
    }
}

/// GPU descriptor of one emitter's splatted density grid — the volumetric shader
/// maps a world sample into `[0,1]³` via `(p - box_min)/box_size` and samples the
/// density texture's Z-slab for this layer. Mirrors `Co2Volume` in volumetric.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Co2Volume {
    /// xyz = grid AABB min (world m), w = σ-scale (density → extinction).
    pub box_min: [f32; 4],
    /// xyz = grid AABB size (world m), w = texture Z-slab layer index (f32).
    pub box_size: [f32; 4],
}

/// One CO2 emitter: the SoA particle pool + its CPU density grid (uploaded to a
/// texture Z-slab each frame). `front_h` is the smoothed leading-edge height that
/// gates the mushroom-cap force.
pub struct Co2Sim {
    pos: Vec<Vec3>,
    vel: Vec<Vec3>,
    age: Vec<f32>,
    life: Vec<f32>,
    base_r: Vec<f32>,
    front_h: f32,
    was_firing: bool,
    spawn_accum: f32,
    rng: u64,
    /// `res³` density, rebuilt each frame, uploaded to the texture.
    pub grid: Vec<f32>,
    pub box_min: Vec3,
    pub box_size: Vec3,
    /// Grid resolution (voxels/axis) this emitter is currently splatting at.
    pub res: u32,
}

impl Co2Sim {
    fn new(id: EntityId, res: u32) -> Self {
        Self {
            pos: Vec::new(),
            vel: Vec::new(),
            age: Vec::new(),
            life: Vec::new(),
            base_r: Vec::new(),
            front_h: 0.0,
            was_firing: false,
            spawn_accum: 0.0,
            rng: 0x9E3779B97F4A7C15 ^ id.wrapping_mul(0xD1B54A32D192ED03).wrapping_add(1),
            grid: vec![0.0; (res * res * res) as usize],
            box_min: Vec3::ZERO,
            box_size: Vec3::splat(1.0),
            res,
        }
    }
    fn u32(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D)) >> 32) as u32
    }
    fn f01(&mut self) -> f32 {
        self.u32() as f32 / u32::MAX as f32
    }
    fn rng2(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f01()
    }
    fn swap_remove(&mut self, i: usize) {
        self.pos.swap_remove(i);
        self.vel.swap_remove(i);
        self.age.swap_remove(i);
        self.life.swap_remove(i);
        self.base_r.swap_remove(i);
    }

    /// Advance the emitter by `dt`. `output` is the instant-up / slow-decay firing
    /// envelope `0..1`; `t_now` is the scene clock (for the evolving curl field).
    fn advance(&mut self, nozzle: Vec3, dir: Vec3, output: f32, t_now: f32, dt: f32, tn: &Co2Tune) {
        if dt <= 0.0 {
            return;
        }
        let basis = basis_of(dir);
        // --- spawn: rising-edge burst + sustained while held ---
        let firing = output > 0.01;
        let edge = firing && !self.was_firing;
        self.was_firing = firing;
        let mut want = 0usize;
        if edge {
            want += (tn.burst_count as f32 * output).round() as usize;
        }
        if firing {
            self.spawn_accum += tn.sustain_rate * output * dt;
            let n = self.spawn_accum.floor();
            self.spawn_accum -= n;
            want += n as usize;
        } else {
            self.spawn_accum = 0.0;
        }
        let room = tn.max_particles.saturating_sub(self.pos.len());
        for _ in 0..want.min(room) {
            let theta = tn.cone_inner + (tn.cone_outer - tn.cone_inner) * self.f01().sqrt();
            let phi = self.f01() * TAU;
            let local = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
            let speed = tn.exit_speed * self.rng2(0.85, 1.05);
            let v = basis * (local * speed);
            let jr = tn.nozzle_radius * self.f01().sqrt();
            let ja = self.f01() * TAU;
            let off = basis * Vec3::new(jr * ja.cos(), 0.0, jr * ja.sin());
            let life = self.rng2(tn.life_min, tn.life_max);
            let base_r = tn.base_radius * self.rng2(0.8, 1.2);
            self.pos.push(nozzle + off);
            self.vel.push(v);
            self.age.push(0.0);
            self.life.push(life);
            self.base_r.push(base_r);
        }
        // --- front-edge EMA (cap gate) ---
        let mut frame_front = 0.0f32;
        for p in &self.pos {
            frame_front = frame_front.max((*p - nozzle).dot(dir).max(0.0));
        }
        let ema = 1.0 - (-4.0 * dt).exp();
        self.front_h += (frame_front - self.front_h) * ema;
        if !firing {
            self.front_h *= (-1.2 * dt).exp();
        }
        // --- integrate (symplectic Euler) ---
        for i in (0..self.pos.len()).rev() {
            self.age[i] += dt;
            if self.age[i] >= self.life[i] {
                self.swap_remove(i);
                continue;
            }
            let t = (self.age[i] / self.life[i]).clamp(1e-5, 1.0);
            let rel = self.pos[i] - nozzle;
            let h = rel.dot(dir);
            let hfrac = (h / self.front_h.max(0.5)).clamp(0.0, 1.5);
            // amplitude ramp: coherent stalk at base, churn in body, calm at the top.
            let a_ramp = smoothstep(0.05, 0.35, hfrac) * (1.0 - smoothstep(0.8, 1.0, hfrac));
            let buoy = dir * tn.buoy0 * (-tn.buoy_decay * self.age[i]).exp();
            let grav = Vec3::new(0.0, -tn.g_settle, 0.0);
            let cp = self.pos[i] * tn.turb_freq + Vec3::new(0.0, 0.0, t_now * tn.turb_scroll);
            let turb = curl_noise(cp) * (tn.turb_str * a_ramp);
            // mushroom cap: leading-edge particles get a radial-out + slight-down kick
            // = the toroidal vortex-ring roll-up = the cauliflower head.
            let mut ring = Vec3::ZERO;
            if h > tn.front_frac * self.front_h && self.front_h > 0.6 {
                let axis_pt = nozzle + dir * h;
                let r_vec = self.pos[i] - axis_pt;
                if r_vec.length_squared() > 1e-6 {
                    ring = r_vec.normalize() * tn.ring_out - dir * tn.ring_down;
                }
            }
            let accel = grav + buoy + turb + ring;
            let mut v = self.vel[i] + accel * dt;
            // linear Stokes drag (a RATE, 1/s), rising with age so the head stalls.
            let k = tn.drag_k0 + (tn.drag_k1 - tn.drag_k0) * t;
            v *= (1.0 - k * dt).clamp(0.0, 1.0);
            self.vel[i] = v;
            self.pos[i] += v * dt;
        }
    }

    /// Voxelize the live puffs into `self.grid` (a Gaussian splat per particle) and
    /// set the grid AABB. Returns the GPU descriptor (the renderer fills the layer).
    fn splat(&mut self, tn: &Co2Tune) -> Option<Co2Volume> {
        if self.pos.is_empty() {
            return None;
        }
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        let mut max_r = 0.0f32;
        for i in 0..self.pos.len() {
            let t = (self.age[i] / self.life[i]).clamp(0.0, 1.0);
            let r = self.base_r[i] * (1.0 + tn.grow_rate * t);
            max_r = max_r.max(r);
            lo = lo.min(self.pos[i] - r);
            hi = hi.max(self.pos[i] + r);
        }
        let pad = max_r * 1.5 + 0.2;
        lo -= Vec3::splat(pad);
        hi += Vec3::splat(pad);
        self.box_min = lo;
        self.box_size = (hi - lo).max(Vec3::splat(0.5));
        let n = self.res as usize;
        for d in self.grid.iter_mut() {
            *d = 0.0;
        }
        let cell = self.box_size / self.res as f32;
        let inv_cell = Vec3::ONE / cell;
        for i in 0..self.pos.len() {
            let t = (self.age[i] / self.life[i]).clamp(0.0, 1.0);
            let r = self.base_r[i] * (1.0 + tn.grow_rate * t);
            // Near-instant appear at the nozzle (0.03 ramp = visible smoke the moment
            // a puff is born → instant output), then a long dissipating tail.
            let env =
                tn.opacity_peak * smoothstep(0.0, 0.03, t) * (1.0 - smoothstep(0.55, 1.0, t));
            let thin = (self.base_r[i] * self.base_r[i]) / (r * r); // mass-conserving
            let amp = env * thin;
            if amp < 1e-4 {
                continue;
            }
            let c = (self.pos[i] - self.box_min) * inv_cell;
            let rv = r * inv_cell;
            // Tighter kernel (σ = r/2.9) → sharper puff edges = crisper cauliflower
            // (and cheaper: more of the footprint falls under the g<0.004 cutoff).
            let inv2sig = 1.0 / (2.0 * (r * 0.345).powi(2)).max(1e-4);
            let (x0, x1) = span(c.x, rv.x, n);
            let (y0, y1) = span(c.y, rv.y, n);
            let (z0, z1) = span(c.z, rv.z, n);
            for z in z0..z1 {
                for y in y0..y1 {
                    for x in x0..x1 {
                        let wc = self.box_min
                            + Vec3::new(
                                (x as f32 + 0.5) * cell.x,
                                (y as f32 + 0.5) * cell.y,
                                (z as f32 + 0.5) * cell.z,
                            );
                        let d2 = (wc - self.pos[i]).length_squared();
                        let g = (-d2 * inv2sig).exp();
                        if g > 0.004 {
                            self.grid[(z * n + y) * n + x] += amp * g;
                        }
                    }
                }
            }
        }
        // soft-clamp accumulated density (the dark core comes from the raymarch
        // self-shadow, NOT from unbounded summed density).
        for d in self.grid.iter_mut() {
            *d = (*d).tanh() * 1.2;
        }
        Some(Co2Volume {
            box_min: [self.box_min.x, self.box_min.y, self.box_min.z, tn.sigma_scale],
            box_size: [self.box_size.x, self.box_size.y, self.box_size.z, 0.0],
        })
    }
}

// --- CO2 sim helpers ---------------------------------------------------------
fn span(c: f32, r: f32, n: usize) -> (usize, usize) {
    let lo = (c - r).floor().max(0.0) as usize;
    let hi = ((c + r).ceil() as i64).clamp(0, n as i64) as usize;
    (lo.min(n), hi.min(n))
}
fn basis_of(dir: Vec3) -> Mat3 {
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let right = up.cross(dir).normalize_or(Vec3::X);
    let fwd = dir.cross(right);
    Mat3::from_cols(right, dir, fwd)
}
// Curl noise (Bridson 2007): v = curl(ψ), ψ = 3 uncorrelated fBm fields → the
// field is divergence-free, so particles churn/billow with no sinks/gutters.
const OFF1: Vec3 = Vec3::new(31.41, 0.0, 0.0);
const OFF2: Vec3 = Vec3::new(0.0, 0.0, 47.13);
fn potential(p: Vec3) -> Vec3 {
    Vec3::new(fbm3(p), fbm3(p + OFF1), fbm3(p + OFF2))
}
fn curl_noise(p: Vec3) -> Vec3 {
    let eps = 1e-3;
    let dpsi_dx = (potential(p + Vec3::X * eps) - potential(p - Vec3::X * eps)) / (2.0 * eps);
    let dpsi_dy = (potential(p + Vec3::Y * eps) - potential(p - Vec3::Y * eps)) / (2.0 * eps);
    let dpsi_dz = (potential(p + Vec3::Z * eps) - potential(p - Vec3::Z * eps)) / (2.0 * eps);
    Vec3::new(
        dpsi_dy.z - dpsi_dz.y,
        dpsi_dz.x - dpsi_dx.z,
        dpsi_dx.y - dpsi_dy.x,
    )
}
fn fbm3(p: Vec3) -> f32 {
    let (mut amp, mut f, mut sum, mut norm) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
    for _ in 0..3 {
        sum += amp * vnoise3(p * f);
        norm += amp;
        f *= 2.0;
        amp *= 0.5;
    }
    sum / norm
}
fn vnoise3(p: Vec3) -> f32 {
    let i = p.floor();
    let f = p - i;
    let u = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let h = |o: Vec3| -> f32 {
        let q = i + o;
        let mut n = (q.x as i64).wrapping_mul(374761393)
            ^ (q.y as i64).wrapping_mul(668265263)
            ^ (q.z as i64).wrapping_mul(2147483647);
        n = (n ^ (n >> 13)).wrapping_mul(1274126177);
        ((n & 0xffff) as f32 / 65535.0) * 2.0 - 1.0
    };
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let c000 = h(Vec3::new(0., 0., 0.));
    let c100 = h(Vec3::new(1., 0., 0.));
    let c010 = h(Vec3::new(0., 1., 0.));
    let c110 = h(Vec3::new(1., 1., 0.));
    let c001 = h(Vec3::new(0., 0., 1.));
    let c101 = h(Vec3::new(1., 0., 1.));
    let c011 = h(Vec3::new(0., 1., 1.));
    let c111 = h(Vec3::new(1., 1., 1.));
    lerp(
        lerp(lerp(c000, c100, u.x), lerp(c010, c110, u.x), u.y),
        lerp(lerp(c001, c101, u.x), lerp(c011, c111, u.x), u.y),
        u.z,
    )
}

// =====================================================================================
// PyroSystem
// =====================================================================================

/// All pyro runtime state: per-spark-device particle sims + per-CO2-device smoke
/// sims, plus the per-frame spark instances + CO2 volume descriptors the renderer
/// reads (the grids stay in the sims for the renderer to upload).
pub struct PyroSystem {
    sims: HashMap<EntityId, PyroSim>,
    co2_sims: HashMap<EntityId, Co2Sim>,
    /// Additive spark sprites built this frame (drawn by the spark pipeline).
    pub spark: Vec<ParticleInstance>,
    /// CO2 volume descriptors (one per active emitter, this frame). The renderer
    /// fills `box_size.w` with the texture layer and uploads the matching grid.
    pub co2_vol: Vec<Co2Volume>,
    /// Active CO2 emitter ids, parallel to `co2_vol` (the grid lives in `co2_sims`).
    pub co2_ids: Vec<EntityId>,
}

impl Default for PyroSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl PyroSystem {
    pub fn new() -> Self {
        Self {
            sims: HashMap::new(),
            co2_sims: HashMap::new(),
            spark: Vec::new(),
            co2_vol: Vec::new(),
            co2_ids: Vec::new(),
        }
    }

    /// The splatted density grid for an active CO2 emitter (the renderer uploads it).
    pub fn co2_grid(&self, id: EntityId) -> Option<&[f32]> {
        self.co2_sims.get(&id).map(|s| s.grid.as_slice())
    }

    /// Advance every device by `dt` and rebuild the spark instances + CO2 grids.
    /// `time` is the scene clock (for the evolving curl field); `co2_res` is the
    /// density-grid resolution to splat at (the renderer derives it from device
    /// quality + sizes the texture to match). Call once per real frame from the app
    /// update tick.
    pub fn advance(&mut self, scene: &Scene, cam_pos: Vec3, time: f32, dt: f32, co2_res: u32) {
        let mut live_spark: Vec<EntityId> = Vec::new();
        let mut live_co2: Vec<EntityId> = Vec::new();
        for d in &scene.pyro {
            match d.kind {
                PyroKind::ColdSpark => live_spark.push(d.id),
                PyroKind::Co2Jet => live_co2.push(d.id),
            }
        }
        self.sims.retain(|id, _| live_spark.contains(id));
        self.co2_sims.retain(|id, _| live_co2.contains(id));

        let dt = dt.clamp(0.0, 1.0 / 20.0);
        self.spark.clear();
        self.co2_vol.clear();
        self.co2_ids.clear();

        for dev in &scene.pyro {
            match dev.kind {
                PyroKind::ColdSpark => {
                    let dist = (cam_pos - dev.world_nozzle()).length();
                    let lod = lod_factor(dist);
                    let sim = self.sims.entry(dev.id).or_insert_with(|| PyroSim::new(dev.id));
                    sim.advance(dev, dt, lod);
                    if !dev.hidden {
                        sim.build_spark(dev, &mut self.spark);
                    }
                }
                PyroKind::Co2Jet => {
                    // OUTPUT is INSTANT: the spawn rate follows emit_amount DIRECTLY
                    // (valve open → dense smoke at the nozzle THIS frame; valve shut →
                    // no new puffs). The lingering after shut-off is purely the already-
                    // emitted puffs dissipating over their lifetime (the Dissipation
                    // slider) — NOT an output envelope, which is what delayed the rise.
                    let output = dev.emit_amount();
                    let tn = Co2Tune::for_device(dev);
                    let sim = self
                        .co2_sims
                        .entry(dev.id)
                        .or_insert_with(|| Co2Sim::new(dev.id, co2_res));
                    if sim.res != co2_res {
                        sim.res = co2_res;
                        sim.grid = vec![0.0; (co2_res * co2_res * co2_res) as usize];
                    }
                    sim.advance(dev.world_nozzle(), dev.world_dir(), output, time, dt, &tn);
                    if !dev.hidden
                        && self.co2_ids.len() < CO2_LAYERS as usize
                        && let Some(v) = sim.splat(&tn)
                    {
                        self.co2_vol.push(v);
                        self.co2_ids.push(dev.id);
                    }
                }
            }
        }
    }
}

// =====================================================================================
// Cold-spark billboard particle sim (unchanged spine)
// =====================================================================================

struct PyroSim {
    pos: Vec<Vec3>,
    vel: Vec<Vec3>,
    age: Vec<f32>,
    life: Vec<f32>,
    seed: Vec<u32>,
    spawn_accum: f32,
    rng: u64,
}

impl PyroSim {
    fn new(id: EntityId) -> Self {
        Self {
            pos: Vec::new(),
            vel: Vec::new(),
            age: Vec::new(),
            life: Vec::new(),
            seed: Vec::new(),
            spawn_accum: 0.0,
            rng: 0x9E3779B97F4A7C15 ^ id.wrapping_mul(0xD1B54A32D192ED03).wrapping_add(1),
        }
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D)) >> 32) as u32
    }
    fn f01(&mut self) -> f32 {
        self.next_u32() as f32 / u32::MAX as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f01()
    }
    fn len(&self) -> usize {
        self.pos.len()
    }
    fn swap_remove(&mut self, i: usize) {
        self.pos.swap_remove(i);
        self.vel.swap_remove(i);
        self.age.swap_remove(i);
        self.life.swap_remove(i);
        self.seed.swap_remove(i);
    }

    fn advance(&mut self, dev: &PyroDevice, dt: f32, lod: f32) {
        if dt <= 0.0 {
            return;
        }
        let nozzle = dev.world_nozzle();
        let gravity = 9.81;
        let drag_k = 1.9;
        let noise_strength = 1.5;
        let floor_y = nozzle.y - 0.1;
        let damp = (1.0 - drag_k * dt).clamp(0.0, 1.0);
        for i in (0..self.len()).rev() {
            self.age[i] += dt;
            if self.age[i] >= self.life[i] {
                self.swap_remove(i);
                continue;
            }
            let accel = Vec3::new(0.0, -gravity, 0.0);
            let turb = spark_turbulence(self.seed[i], self.age[i]) * noise_strength;
            let mut v = self.vel[i] + (accel + turb) * dt;
            v *= damp;
            self.vel[i] = v;
            self.pos[i] += v * dt;
            if self.pos[i].y < floor_y && v.y < 0.0 {
                self.swap_remove(i);
            }
        }
        let emit = dev.emit_amount() * lod;
        let cap = dev.max_particles as usize;
        if emit > 0.0 && self.len() < cap {
            let base_rate = 1900.0;
            self.spawn_accum += base_rate * emit * dt;
            let want = self.spawn_accum.floor();
            self.spawn_accum -= want;
            let n = (want as usize).min(cap - self.len());
            let dir = dev.world_dir();
            let to_axis = Quat::from_rotation_arc(Vec3::Y, dir);
            let inner = dev.cone_deg.to_radians() * 0.2;
            let outer = dev.cone_deg.to_radians();
            let height = dev.working_height();
            let v0 = (2.0 * 9.81 * height).sqrt();
            let nozzle_r = 0.04;
            for _ in 0..n {
                let theta = self.range(inner, outer);
                let phi = self.f01() * TAU;
                let local = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
                let speed = v0 * self.range(0.82, 1.06);
                let vel = to_axis * (local * speed);
                let jr = nozzle_r * self.f01().sqrt();
                let ja = self.f01() * TAU;
                let off = to_axis * Vec3::new(jr * ja.cos(), 0.0, jr * ja.sin());
                let life = self.range(0.8, 1.7);
                self.pos.push(nozzle + off);
                self.vel.push(vel);
                self.age.push(0.0);
                self.life.push(life);
                let s = self.next_u32();
                self.seed.push(s);
            }
        } else if emit <= 0.0 {
            self.spawn_accum = 0.0;
        }
    }

    fn build_spark(&self, dev: &PyroDevice, out: &mut Vec<ParticleInstance>) {
        let t0 = dev.color_t0_k.max(1000.0);
        let t1 = dev.color_t1_k.max(900.0);
        let bright = dev.brightness.max(0.0);
        out.reserve(self.len());
        for i in 0..self.len() {
            let t = (self.age[i] / self.life[i]).clamp(0.0, 1.0);
            let s = self.seed[i];
            let hot = t0 + (hash01(s) - 0.3) * 1100.0;
            let cool = 1.6 + hash01(s ^ 0x9E37_79B9) * 2.2;
            let temp = lerp(t1, hot, (-cool * t).exp());
            let bb = blackbody(temp);
            let tw = twinkle(s, self.age[i]);
            let fresh = 1.0 - t;
            let lum = bright * (0.32 + 1.2 * fresh * fresh) * tw;
            let rgb = bb * lum;
            let alpha = fresh.powf(0.45);
            let speed = self.vel[i].length();
            let stretch = (0.17 * speed).clamp(1.0, 11.0);
            let radius = 0.016 * (0.55 + 0.9 * hash01(s ^ 0x0068_BC21));
            let p = self.pos[i];
            let v = self.vel[i];
            out.push(ParticleInstance {
                pos_radius: [p.x, p.y, p.z, radius],
                color: [rgb.x, rgb.y, rgb.z, alpha],
                vel_stretch: [v.x, v.y, v.z, stretch],
                aux: [0.0; 4],
            });
        }
    }
}

fn lod_factor(d: f32) -> f32 {
    if d <= LOD_NEAR_M {
        1.0
    } else if d >= LOD_CULL_M {
        0.0
    } else {
        1.0 - (d - LOD_NEAR_M) / (LOD_CULL_M - LOD_NEAR_M)
    }
}
fn spark_turbulence(seed: u32, age: f32) -> Vec3 {
    let a = seed as f32 * 0.0007;
    Vec3::new(
        (age * 7.0 + a).sin(),
        (age * 5.3 + a * 1.7).sin() * 0.3,
        (age * 6.1 + a * 2.3).cos(),
    )
}
fn hash01(x: u32) -> f32 {
    let mut h = x.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_216.0
}
fn twinkle(seed: u32, age: f32) -> f32 {
    let ph = (seed & 0xffff) as f32 * 0.0001;
    0.7 + 0.3 * (0.5 + 0.5 * ((age * 18.0 + ph) * TAU).sin())
}
fn blackbody(temp: f32) -> Vec3 {
    let t = temp.clamp(1000.0, 6500.0) / 100.0;
    let r = if t <= 66.0 { 255.0 } else { 329.7 * (t - 55.0).max(1e-3).powf(-0.1332) };
    let g = if t <= 66.0 {
        99.47 * t.ln() - 161.12
    } else {
        288.12 * (t - 60.0).max(1e-3).powf(-0.0755)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t > 19.0 {
        138.52 * (t - 10.0).ln() - 305.04
    } else {
        0.0
    };
    (Vec3::new(r, g, b).clamp(Vec3::ZERO, Vec3::splat(255.0))) / 255.0
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
