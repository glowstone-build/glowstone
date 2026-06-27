# RESEARCH — Stage PYRO devices: CO2 cannon/jet + cold-spark machine

Design doc for adding two stage **PYRO** devices to the carthage/glowstone wgpu
lighting-glowstone app:

1. a **CO2 cannon / cryo jet** — a fast, dramatic white plume that catches
   coloured stage light, and
2. a **cold-spark machine** ("cold pyro" / Sparkular-type) — an upward
   fireworks-like golden spark fountain.

The two are deliberately one *feature* with two *kinds* (like `LedScreen` is one
emissive-surface feature) because they share a scene data-model slot, a DMX
footprint + decode path, a particle/fog renderer pass, and the
Add-menu/inspector/outliner/`.archie` plumbing. Where they differ is the
**rendering model** (ballistic billboard particle fountain vs. a cheap rising
white fog column) and the physical/DMX parameter ranges.

Grounding:
- Industry mechanics + DMX footprints from manufacturer manuals (MagicFX
  ECO2JET/PSYCO2JET, Showven Sparkular line, Club Cannon, CryoFX, Showven CO2
  JET X-C1, Moka, etc.) — verified against per-product channel maps.
- Rendering math distilled from Unreal Engine 4 Cascade + Niagara source
  (exact file/line citations preserved below) and standard blackbody/physics.
- Codebase integration grounded in the live files cited per section
  (`src/scene/screen.rs`, `src/scene/mod.rs`, `src/dmx/decode.rs`,
  `src/dmx/patch.rs`, `src/renderer/mod.rs`, `src/ui/project.rs`,
  `src/ui/windows/add_menu.rs`, `src/ui/panels.rs`, `src/scene/library.rs`).

---

## 1. How each device works IRL

### 1.1 CO2 cannon / cryo jet

A CO2 cannon is **not** a fog-fluid machine — it is a *liquid → gas/solid
phase-change* effect:

- A **siphon (dip-tube) cylinder** is mandatory: the tube reaches the bottom
  liquid CO2 (a full cylinder is ~2/3 liquid, 1/3 gas). The cylinder's own
  saturated vapour pressure forces **liquid** CO2 up the dip tube.
- A fast **electromagnetic/solenoid valve** (often piston-actuated, low-temp
  PTFE/HNBR/VITON seats) gates the liquid on trigger/DMX and releases it
  through an orifice (~3.5–12 mm).
- On exiting to atmosphere the liquid undergoes rapid **adiabatic
  (Joule–Thomson) expansion** and flash-boils: ~half flashes to gas, the
  latent heat drives the temperature to the CO2 **sublimation point ≈ −78.5 °C
  (−109 °F)**, freezing the rest into micron-scale **dry-ice particles** that —
  plus condensed atmospheric humidity — form a dense **opaque white plume**.
- The plume is **intrinsically white**; it has *no colour of its own* and is
  tinted entirely by stage lighting or an integrated RGB LED ring at the
  nozzle. It behaves as a fast, dense, short-lived **fog column / mid-air fog
  screen** that rises and dissipates in 1–3 s (it does NOT fill the room like a
  fogger).
- Variants: single jet, dual/multi-nozzle, and motorised **moving/rotating
  nozzle** ("PSYCO2JET") that sweeps 0–180° to paint a CO2 fan.

**Physical parameter ranges** (treat all as ranges — they vary by orifice,
pressure, hose length, ambient temp/humidity):

| Parameter | Value (typical) | Unit | Notes |
|---|---|---|---|
| Plume height / throw | 6–20 (commonly 7–12) | m | ≈ 20–40 ft; up to ~60 ft for long-throw units. Cold+humid = taller/denser. |
| Plume colour | white | — | Phase-change cloud; tinted only by stage light / nozzle RGB. |
| Supply pressure (HP cylinder) | 57–62 (≈ 830–900) | bar (psi) | Cylinder saturated vapour pressure at room temp; falls as it cools. |
| Supply pressure (LP bulk dewar) | ≈ 24–38 (≈ 350–550) | bar (psi) | Lower pressure, shorter throw, huge capacity (200 L ≈ 8× a 50 lb cylinder). |
| Exit/sublimation temp | ≈ −78.5 (−109) | °C (°F) | Frostbite/burn hazard. |
| Consumption | ~0.45–1.0 (≈ 1–2 s/lb) | kg CO2 / s | Manufacturers quote ~1–2 s spray per pound. |
| Blasts per 50 lb (~22 kg) tank | ~20–45 | blasts | ~30–45 two-second bursts; or 60–90 s continuous. |
| Typical blast duration | 0.5–5 (commonly 1–3) | s | Burst-style, near-instant; dissipates in seconds. |
| Duty cycle / cool-down | ≤30 s continuous; cool 3–10 min after 3–5 blasts | — | Valve body freezes on long runs. |
| Moving/fan sweep | 0–180 | ° | PSYCO2JET-style rotating nozzle. |
| Electrical power | 30–200 | W | Runs controller/solenoid/LEDs only — plume energy is the CO2. |
| Orifice diameter | 3.5–12 | mm | Stock fitting ~3.5 mm restricts flow. |

Indoor CO2 asphyxiation/displacement hazard (OSHA PEL 5000 ppm 8-hr TWA, STEL
30 000 ppm) — out of scope for the sim, noted for completeness.

### 1.2 Cold-spark machine ("cold pyro" / Sparkular-type)

Electromechanical, **not** pyrotechnic — no propellant, flame, or detonation:

1. **Storage/feed**: granular **titanium/zirconium (Ti-Zr) alloy** powder sits
   in a hopper (~200 g on a standard Sparkular). A motor-driven auger/screw
   meters granules into the hot zone — **feed rate sets spark density**.
2. **Heating**: a vertical electric heating tube raises granules to a user-set
   chamber temperature (menu **480–620 °C**, Sparkular II **400–620 °C**;
   factory default **580 °C**). ~500 W mains.
3. **Acceleration/ejection**: a blower fan propels the now-incandescent
   particles up through a nozzle → the fountain. Particle exit velocity +
   gravity define the parabolic envelope and falling tails. (A "Fall" variant
   sprays downward.)
4. **The spark itself**: each hot Ti granule's surface **oxidises
   exothermically** on contact with air — titanium *burns* in air, briefly
   self-heating and fragmenting (micro-explosions) into bright forking sparks.
   The light is genuine metal combustion at the particle surface.
5. **Why "cold"**: each particle is micron-to-sub-mm with tiny thermal mass; it
   radiates/convects heat away within fractions of a second over ~1 m, so brief
   contact transfers little energy. **Skeptical caveat**: the titanium itself
   burns ~**610 °C (~1130 °F)**; the marketed "62 °F/17 °C" figure is
   misleading. Fabrics/paper can ignite; the hopper holds flammable metal
   powder (Class D fire risk).
6. **Colour/cooling**: incandescence is hottest (**gold/white**) at the dense
   base near the nozzle and red-shifts to **orange/red** at the cooling falling
   tips — straightforward blackbody cooling.

**Physical parameter ranges**:

| Parameter | Value (typical) | Unit | Notes |
|---|---|---|---|
| Fountain height | 1.5–5 (Sparkular II up to 6); ~16 ft cited | m | 10 discrete "gears"; large "jet" units claim 10–15 m. |
| Spread / cone | ~vertical, tiltable to 45° | ° | Tilt sensor disables firing beyond 45°. Spin/fan/Fall variants exist. |
| Chamber temperature | 480–620 (default 580) | °C | Sparkular II 400–620. Surface combustion ~610 °C. |
| Particle size | ~2–25 (indicative, from Ti/Zr research) | µm | Proprietary; burn time ∝ size². |
| Consumable | Ti-Zr alloy granules | — | RFID-coded; 200 g ≈ 10–15 min. |
| Granule consumption | ~15–20 | g/min per nozzle | |
| Pre-heat time | ~5 min (handheld Blaster ~100 s) | — | Needed before firing. |
| Max continuous burst | ~30 | s | Avoid overheating; "clear material" purge after. |
| Power | ~500 (clones 600–750) | W | 110/220 V. |
| Spin RPM (Spin variant) | 60–100 | RPM | CW/CCW. |
| Smoke / odor | ~80% less than a gerb | — | Re-fireable from refillable hopper; often indoor-permitted. |

---

## 2. Recommended generic DMX footprints

Each device ships **two modes** that share the same channel order, so the first
*N* channels are a strict subset (minimal mode = first N of the richer mode).
This mirrors the codebase's existing two-mode LED screen idea and the GDTF
`modes` concept (`patch.rs::footprint_for`, `decode.rs`).

Key encoded conventions, grounded in the verified product maps:
- **Arm/Safety is a real, important, separate concept** on every professional
  unit. Convention: a **mid-band window (~100–155 of 255)** arms, not 255 —
  this guards against both stuck-low and stuck-high faults. We default the Arm
  channel to **0 (disarmed)** so a freshly-patched fixture can never fire.
- **Trigger/Blast** fires on a **high window (200–255)** so a single dropped
  bit can't fire.
- Defaults are chosen so a freshly-patched, un-driven fixture is **inert**
  (everything off / disarmed), matching how MVR/GDTF imports keep an un-driven
  rig dark in this codebase.

### 2.1 CO2 cannon/jet — DMX footprint

**Minimal mode — "Blast" (1 channel)** — matches the overwhelming majority of
real fixed jets (Club Cannon DMX/Micro/Pro Jet, CryoFX Cryo Jet, MOKA MK-C09;
passive jets fired via an external 1-ch relay collapse onto this):

| Ch | Name | Range | Function | Default |
|---|---|---|---|---|
| 1 | Blast / Trigger | 0–199 closed; **200–255 blast** | trigger | 0 |

**Richer mode — "Safe Jet" (4 ch fixed + up to 3 moving)**:

| Ch | Name | Range | Function | Default |
|---|---|---|---|---|
| 1 | Arm / Safety | 0–99 disarmed; **100–199 ARMED**; 200–255 test (armed, valve inhibited) | arm | 0 |
| 2 | Blast / Trigger | 0–199 closed; **200–255 blast** (only when armed) | trigger | 0 |
| 3 | Intensity / Output | 0–255 proportional plume size/output (cosmetic in sim) | dimmer | 0 |
| 4 | Duration | 0–255 → blast length 0…max-safe (hardware-capped ~3–8 s); 0 = momentary while Blast held | duration | 0 |
| 5 | Pan (moving variant only) | 0–127 = −90…0°, 128 centre, 129–255 = 0…+90° | pan | 128 |
| 6 | Tilt (moving variant only) | 0–255 mapped to mechanical tilt, centre 128 | tilt | 128 |
| 7 | Move Speed (moving variant only) | 0–255 slow → fast slew | speed | 0 |

Notes: only MagicFX **PSYCO2JET** is a true CO2 mover, and it is **pan-only**
(180° rotating nozzle) with a separate Speed channel; Tilt is a forward-looking
option. "MagicFX Swirl" is a confetti blower, not CO2; Showven "Sparkular" is
cold-spark, not CO2.

#### Per-product comparison (CO2)

| Brand / model | Ch count | Arm/Safety | Trigger/Blast | Move | Source |
|---|---|---|---|---|---|
| MagicFX ECO2JET (MFX1801) Std | 2 | Enable ch (100–154 enable; 200–255 prime) | Start (Enable@100-154: 200–255 fire) | — | ECO2JET manual §1.6 |
| MagicFX ECO2JET Extended | 3 | Enable | Start | + Heater ch | ECO2JET manual §1.6.2 |
| MagicFX PSYCO2JET RAW | 4 | Safety (100–155 enable; 156–255 test) | Output (200–255 on) | Angle ±90° + Speed | PSYCO2JET manual §1.6 |
| MagicFX PSYCO2JET PRESET | 5 | Safety | Go (200–249 cont / 250–255 step) | Preset + Speed + Direction | PSYCO2JET manual §1.7 |
| Showven CO2 JET X-C1 | 1 (+ sep. safety addr) | CH-S window (default 127–178) | CH1 (100–255 relay on) | — | X-C1 manual V1.0 |
| Club Cannon Pro Jet | 1 | — | 255 = fire (RDM) | — | Pro Jet manual v1.0 |
| Club Cannon DMX/Micro Jet MKII | 1 | — | high = open (addr 000 = momentary power) | — | DMX Jet MKII manual |
| CryoFX Cryo Jet DMX 512 | 1 | — | high = open (max blast 3 s) | — | Cryo Jet DMX manual |
| CryoFX CO2 LED Jet | 6 | — | CO2 on/off | + RGB/preset/dimmer | CO2 LED Jet (6-ch) manual |
| MOKA MK-C09 | 1 | — | high = fire | — | MOKA product page |
| OneLight MK-C17 (3-output) | 5 | — | per-pipe + all + chase triggers | — | MK-C17 manual |
| Galaxis G-Flame (FLAME, shared-safety design ref) | 2 | Safety (shared, 0–15 ON) | 229–255 fire | — | OFL / Finale3D |

### 2.2 Cold-spark machine — DMX footprint

**Minimal mode — "Spark" (3 channels)** — smallest map capturing what every
real unit exposes (safety + spark + height); a 2-ch real fixture collapses onto
it by folding Arm into the status macros:

| Ch | Name | Range | Function | Default |
|---|---|---|---|---|
| 1 | Safety / Unlock (Arm) | 0–49 SAFE; **50–200 ARMED (pre-heat on)**; 201–255 SAFE | safety_unlock | 0 |
| 2 | Spark / Intensity | **0–9 OFF**; 10–255 spark on, scales density/amount (master dimmer; fires only when armed) | spark_intensity | 0 |
| 3 | Height | 0–255 → 0…100% of max effect height (optionally quantised to 10 gears) | height | 128 |

**Richer mode — "Spark+" (5 channels)** — adds the Showven status-channel macros
verbatim (so cues line up with real consoles) and an oscillation channel for
Spin / moving-head units:

| Ch | Name | Range | Function | Default |
|---|---|---|---|---|
| 1 | Safety / Unlock | 0–49 SAFE; **50–200 ARMED**; 201–255 SAFE | safety_unlock | 0 |
| 2 | Spark / Intensity | 0–9 OFF; 10–255 spark on | spark_intensity | 0 |
| 3 | Height | 0–255 (or 10 gears) | height | 128 |
| 4 | Function / Macro | 0–19 normal; **20–40 EMERGENCY STOP**; **60–80 Clear Material**; 81–255 reserved | function_macro | 0 |
| 5 | Oscillation / Angle | 0–15 static; **16–135 CW (speed scales)**; **136–255 CCW** | rotation | 0 |

There is **no dedicated live DMX duration channel** on mainstream cold-spark
units (duration is the cue hold time or an on-unit timer), so we deliberately do
**not** add one for the spark device — it would be fictional.

#### Per-product comparison (cold spark)

| Brand / model | Ch count | Safety/Status | Spark / Height | Move | Source |
|---|---|---|---|---|---|
| Showven Sparkular (V3.2) | 2 | Status (Pre-heat OFF 0–10 / ON 240–255; ESTOP 20–40; Clear 60–80) | Ch1 = 10 height gears (24-wide) | — | Sparkular manual |
| Showven Sparkular Mini | 2 | Status | Ch1 on/off (16–255) | — | Sparkular Mini manual |
| Showven Sparkular Fall | 2 | Status | Ch1 2-level (low/high), downward | — | Sparkular Fall manual |
| Showven Sparkular II 2CH-N | 2 | Status | Ch1 = 10 gears | — | Sparkular II manual |
| Showven Sparkular II 3CH-P | 3 | **Dedicated Safety ch (50–200 arm), shareable** | Ch1 = 10 gears; Ch2 trigger | — | Sparkular II manual |
| Showven Sparkular Spin 4CH | 4 | Status | Nozzle1 + Nozzle2 height | Spin (CW 16–135 / CCW 136–255, 60–100 RPM) | Sparkular Spin manual |
| Showven Sparkular Spin 2CH | 2 | Status | Ch1 folds height + spin | folded | Sparkular Spin manual |
| Sparktacular SparkOne SH-03B 2CH | 2 | Status | Ch1 = 4 height steps | — | SparkOne manual |
| Sparktacular SparkOne 4CH | 4 | Preheat + Clear + ESTOP as separate ch | Ch1 = 4 steps | — | SparkOne manual |
| specialfx.it ML-CS01 | 2 | Ch2 status (preheat/clear) | Ch1 amount (10–255, valid only when preheat) | — | Finale3D |
| Moka MK-E11 | 2 | Status | Ch1 spark/height (Showven-compatible) | — | Finale3D |
| rushstage/prostagelight 650–750W clone | 2 | **Status on Ch1 (INVERTED order)** | Height on Ch2 (10 gears) | — | clone manuals |
| Moka MK-E16 moving-head | 5 | Status | Ch2 = 5 height steps | Pan 540° / Tilt 120° / Speed | MK-E16 product page |

Importers must **not** assume Showven channel order (the 650–750W clone family
swaps status↔height).

---

## 3. The realistic rendering model

Both devices are **billboard particle / fog systems**. The math below is
distilled from the verified UE4 Cascade + Niagara source and is ready to
implement in WGSL/Rust. Everything is in the app's coordinate convention
(**Y-up, metres**) — UE source uses Z-up cm, so when porting a UE constant
divide cm→m by 100 and swap the up-axis.

### 3.0 The shared simulation spine (port from UE)

**Integrator** (UE GPU `ParticleSimulationShader.usf:549-637`,
velocity-Verlet half-step on position; the Niagara stateless analytic form is in
`NiagaraStatelessModule_SolveVelocitiesAndForces`):

```
// per particle, per substep dt:
force  = gravity + accel_over_life + drag_force        // m/s^2
accel  = force                                          // (mass folded into force already)
pos   += dt * (vel + 0.5 * accel * dt)                  // half-step position
vel   += accel * dt
age   += dt
```

- **Drag** is *linear/Stokes* (UE `ParticleModules.cpp:2641`; GPU
  `ParticleSimulationShader.usf:239`): `drag_force = -k * vel` where **k is a
  per-second rate (1/s), NOT a 0..1 damping**. Approx step: `vel *= (1 - k*dt)`
  or `vel += -k*vel*dt`. Niagara's analytic closed form (drag>0):
  ```
  terminal = accel/k + wind
  lambda   = (1 - exp(-age*k)) / k
  pos      = pos0 + (vel0 - terminal)*lambda + terminal*age
  ```
  Use the Euler substep form for sparks (cheaper, allows collision); the
  analytic form is an option if collisions are skipped.
- **Gravity** = world-down, mass-independent. UE default `(0,0,-980)` cm/s² →
  here `(0,-9.81,0)` m/s².
- **RelativeTime** `t = age / lifetime` ∈ [0,1] drives every over-life curve;
  particle dies at `t >= 1`. Guard `age = max(age, 1e-5)` so velocity-aligned
  sprites have a direction at birth (Niagara note).
- **Spawn** is a **leftover accumulator** (UE
  `ParticleEmitterInstances.cpp:2051`), framerate-independent:
  ```
  leftover += dt * rate;  n = floor(leftover);  leftover -= n;  // sustained
  ```
  plus explicit **bursts** (`Count` or random `[CountLow..Count]`) for an
  instantaneous CO2 hit.

**Per-particle state** (CPU sim → instance buffer; small, Pod):
`position: Vec3, velocity: Vec3, age: f32, lifetime: f32, seed: u32` (+ derived
`base_size`, `base_color`). 32 bytes is enough if size/colour are recomputed
each frame from `seed` + `t`.

### 3.1 Cold sparks — ballistic billboard fountain

**Launch velocity — velocity cone** (UE `ParticleModuleVelocityCone`,
`ParticleModules_Velocity.cpp:407-483`; Niagara cone in the stateless solver).
Pick polar angle θ in `[inner, outer]`, azimuth φ uniform in `[0, 2π)`, speed
`s` from a range; build the direction in the cone's basis around `Direction`
(default = up):

```wgsl
let theta = mix(inner_rad, outer_rad, rand());      // half-angle from axis
let phi   = rand() * 6.2831853;
let local = vec3(sin(theta)*cos(phi), cos(theta), sin(theta)*sin(phi)); // Y-up axis
let v0    = basis_of(direction) * local * speed;    // speed ~ exit velocity
```

Suggested defaults: `direction = (0,1,0)` straight up, `inner = 2°`,
`outer = 8–14°` (tight fountain), `speed` from height target via
`v = sqrt(2 * g * height)` (ballistic apex). For `height = 4 m`,
`speed ≈ sqrt(2*9.81*4) ≈ 8.86 m/s`.

**Forces**: gravity `(0,-9.81,0)`; **drag** `k ≈ 1.5–4 /s` (kills the long
ballistic tail so sparks decelerate and twinkle out rather than flying forever —
UE notes sparks want higher k than the default 1.0); optional **curl-noise
turbulence** for shimmer.

**Curl turbulence** (the cheap shimmer — UE Niagara LUT version,
`NiagaraStatelessNoiseLUT.cpp`; or live curl-of-simplex
`NiagaraDataInterfaceCurlNoise.cpp`). The app already has
`src/renderer/noise.rs` and `src/shaders/*` noise; the cheapest faithful port is
a small additive curl term added to velocity each substep:

```wgsl
// curl of a divergence-free field — guaranteed swirl, no sources/sinks
let c = curl_noise((pos * noise_freq) + bias);       // ~[-1,1]^3
vel += c * noise_strength * dt;                       // strength ~ 0.5..3 m/s^2
```

A zero-per-frame-cost alternative (Niagara stateless) is to precompute a small
LUT of integrated curl streamlines and index it by `t` and a hash of the start
position; the visible effect is a smooth wandering path whose total wander
scales with `lifetime * noise_strength`. For glowstone, the live curl term at a low
substep count is fine.

**Velocity-stretched additive sprites** (the streak look). Two coupled pieces:

1. **Size by speed** (UE `ParticleModuleSizeScaleBySpeed`,
   `ParticleModules_Size.cpp:404-435`):
   `finalSize = base_size * clamp(speed_scale * |vel|, 1.0, max_scale)` per axis.
   Set `speed_scale.y` large so fast sparks stretch into streaks; the floor of
   1.0 keeps slow sparks at base size.
2. **Velocity alignment** (UE `PSA_Velocity`): orient the quad's long axis along
   the screen-projected velocity. In the billboard VS, build the quad from the
   view-projected velocity direction:

```wgsl
// in the vertex shader, per corner (cx in [-1,1], cy in [-1,1]):
let vel_clip = normalize((view_proj * vec4(vel, 0.0)).xy);    // screen-space dir
let perp     = vec2(-vel_clip.y, vel_clip.x);
let half_len = base_h * clamp(speed_scale * length(vel), 1.0, max_stretch); // streak
let half_w   = base_w;
let offset   = vel_clip * (cy * half_len) + perp * (cx * half_w);
clip_pos.xy += offset * clip_pos.w / viewport;                 // expand in NDC
```

**Blackbody temperature → RGB colour-over-life** (the gold→orange→dim-red
cooling — UE `ParticleModuleColorOverLife` authoring a blackbody curve, but we
compute it analytically so it's tunable by chamber temperature). Map
RelativeTime → temperature, then temperature → linear RGB. Particle starts at
the chamber/combustion temperature `T0` (~1900 K visual for a bright gold spark;
the *physical* surface is ~880 K / 610 °C but the *visual* incandescence of a
burning-Ti spark reads much hotter/whiter, so expose `T0` as a tunable and let
defaults read gold-white) and cools toward `T1` (~1000 K dim red) by end of
life:

```wgsl
// 1) age -> temperature (exponential cooling reads better than linear)
let T = mix(T1, T0, exp(-cool_rate * t));   // t in [0,1]; cool_rate ~ 2.5

// 2) temperature -> linear RGB, a compact blackbody approximation
//    (Planckian locus fit; good 1000K..6500K). Returns un-normalised, then
//    we scale by an HDR brightness so RGB>1 drives bloom (UE keeps colour HDR
//    & UNCLAMPED on purpose — ParticleModuleColor).
fn blackbody(T: f32) -> vec3<f32> {
    let t = clamp(T, 1000.0, 6500.0) / 100.0;
    var r: f32; var g: f32; var b: f32;
    // red
    r = select(329.7 * pow(max(t - 55.0, 1e-3), -0.1332), 255.0, t <= 66.0);
    // green
    g = select(288.12 * pow(max(t - 60.0, 1e-3), -0.0755),
               99.47 * log(t) - 161.12, t <= 66.0);
    // blue
    b = select(255.0,
               select(0.0, 138.52 * log(t - 10.0) - 305.04, t > 19.0),
               t >= 66.0);
    return clamp(vec3(r, g, b) / 255.0, vec3(0.0), vec3(1.0));
}
```

Then `rgb_hdr = blackbody(T) * brightness * twinkle(seed, age)` and the additive
sprite uses `rgb_hdr` (no per-pixel lighting — sparks are emissive). HDR values
> 1 are intentional so the existing bloom pass (`post.wgsl`, `settings.bloom`)
makes them glow.

**Twinkle** (the crackle/flicker — a cheap per-particle hashed oscillation):
```wgsl
fn twinkle(seed: u32, age: f32) -> f32 {
    let ph = f32(seed & 0xffffu) * 0.0001;
    return 0.6 + 0.4 * (0.5 + 0.5 * sin((age * twinkle_hz + ph) * 6.2831853));
}
```

**Falling tails / shrink-fade**: alpha-over-life fades to 0 at the tail (UE
`AlphaOverLife`), and size-multiply-life can shrink. With velocity stretching,
the deceleration near apex + the streak gives the classic forking falling tail
for free.

**Soft-depth fade**: read scene depth at the sprite's pixel and fade alpha as
the sprite approaches geometry (prevents hard intersection seams). The codebase
already reconstructs scene depth for shadows / SSAO; reuse the depth target:
```wgsl
let scene_z = linearize(depth_sample);
let frag_z  = linearize(frag_depth);
alpha *= clamp((scene_z - frag_z) / soft_fade_m, 0.0, 1.0);
```

**Floor collision** (optional, GPU form — UE `ParticleModuleCollisionGPU`,
`ParticleSimulationShader.usf:280-282`): reflect off the stage floor plane:
```
perp = dot(v,n)*n;  tan = v - perp;
v_out = (1 - friction)*tan - resilience*perp;   // resilience ~0.3, friction ~0.4
```
Decrement a small collision budget (1–3), then kill. For glowstone this is optional
— cold sparks mostly burn out before landing; a simple kill-at-floor is enough.

### 3.2 CO2 — cheap rising white fog column

The CO2 plume is **not** a particle fountain in spirit — it is a fast, dense,
**short-lived white fog column** that **catches coloured stage light**. The app
already has exactly the right primitive for "a localised volume that catches
coloured light": the **LED-wall-as-area-light** trick in
`renderer/mod.rs:2419-2477`, which appends soft, wide `FixtureGpu` "wash"
emitters that the volumetric raymarch (`volumetric.wgsl`) scatters through. A
CO2 plume is the inverse — it should *receive* light, not emit it — so the model
is a **billboard fog column** plus an optional **localized density contribution
to the froxel/volumetric pass**.

Two complementary cheap renderings (ship the billboard one first; the
froxel/volumetric contribution is the LOD-friendly upgrade):

**(A) Billboard fog column (default, cheap)** — a vertical stack of large,
soft, **camera-facing** white billboards spawned as a burst on trigger and
advected upward:

- Spawn: on Blast, a burst of `N` puff billboards (`Count` 40–200) with upward
  `v0 ≈ throw_speed` (from plume height: `v ≈ sqrt(2*g*height)`, e.g.
  10 m → ~14 m/s) inside a small cone (cone half-angle 5–12°) and a small
  nozzle radius.
- Forces: gravity is *near zero* for the rising column (the cold dense cloud
  rises by ejection momentum then stalls), with a slight **buoyancy/updraft**
  that decays — model as `accel_over_life` ramping from `+up` to `~0`
  (UE `AccelerationOverLifetime`), plus moderate drag `k ≈ 0.8–1.5 /s` so it
  decelerates into a billowing stall and dissipates in 1–3 s.
- Curl turbulence at low frequency for billowing (same curl term as sparks but
  larger scale, lower strength).
- Size-over-life **grows** (the puff expands as it rises and entrains air):
  `size = base * (1 + grow_rate * t)`.
- Colour: **white**, but it is *lit*, not emissive. Two options:
  - cheap: shade each billboard by the same fixtures-as-spotlights data the
    volumetric pass already uploads, so a red wash makes the plume read red
    (sample the dominant nearby `FixtureGpu` cones, or read the froxel result
    texture at the billboard centre if froxel is on);
  - cheapest: tint by a single "ambient stage colour" + a soft self-shadow
    gradient (dark base, bright top) so it reads as a 3D mass.
  Alpha-over-life ramps 0 → peak → 0 (appear/dissipate), and density is high
  (opaque core) early, thinning at the end.
- Soft-depth fade + additive-over-alpha: fog is **alpha-blended** (not additive)
  because it *occludes*; use premultiplied alpha and a back-to-front sort, or a
  cheap order-independent approximation (weighted blended OIT) for the puff
  stack.

**(B) Localized volumetric/froxel contribution (LOD upgrade)** — inject the
plume as a transient **density blob** into the froxel grid (`froxel.wgsl`,
`FroxelState` in `renderer/mod.rs:90`) or as an extra extinction term in the
raymarch (`volumetric.wgsl`): a vertical capsule of extra `density` centred on
the nozzle, growing/rising/decaying with the same envelope as (A). This makes
the plume **catch coloured beams correctly for free** (the raymarch already
scatters every beam through the fog density) and gives true volumetric
self-occlusion, at the cost of a froxel/raymarch density edit. Gate it behind
the froxel toggle / a quality preset so it never tanks perf on the raymarch
default.

Math for both: same integrator as §3.0; the difference from sparks is
**no velocity-stretch**, **grow-not-shrink size**, **near-zero gravity +
decaying updraft**, **white-and-lit (not blackbody-emissive)**, and
**alpha-blend (occluding) not additive**.

---

## 4. Tunable parameters (physical + quality/perf)

Every tunable is modelled as the codebase's distribution idiom (UE
`FRawDistribution`): `enum Value { Constant(f32/Vec3), Uniform(min,max),
Curve(Vec<(t,val)>) }`, sampled with spawn RNG (initial) or `t=age/lifetime`
(over-life). Defaults below are the recommended starting values.

### 4.1 Cold spark — parameters

| Parameter | Default | Range | Trades off |
|---|---|---|---|
| **height_m** (from DMX Height) | 3.5 | 1.5–6 | Apex; drives `v0`. Higher → bigger sim volume. |
| **density** (from Spark/Intensity) | 0.5 | 0–1 | Spawn rate scale → particle count; the dominant perf lever. |
| cone_inner_deg | 2 | 0–20 | Beam tightness at base. |
| cone_outer_deg | 10 | 2–45 | Fountain spread. |
| spawn_rate (at density=1) | 1200 /s | 0–6000 | Maps to `max_particles`; perf. |
| lifetime_s | Uniform(0.6, 1.4) | 0.3–3 | Tail length; bigger → more live particles. |
| gravity (m/s²) | 9.81 | fixed | Apex shape. |
| drag_k (1/s) | 2.5 | 0–8 | Higher kills tails fast (snappier, fewer long-lived particles → cheaper). |
| noise_strength | 1.5 | 0–4 | Shimmer/scatter; visual only. |
| noise_freq | 1.0 /m | 0.2–4 | Wiggle scale. |
| T0_K (chamber/visual) | 1900 | 1200–2600 | Hot-base colour (gold↔white). |
| T1_K (tip) | 1050 | 900–1400 | Tip colour (dim red↔orange). |
| cool_rate | 2.5 | 0.5–6 | How fast gold→red along the tail. |
| brightness (HDR) | 6.0 | 1–20 | Glow/bloom strength. |
| twinkle_hz | 18 | 0–60 | Crackle speed. |
| base_size_m (w,h) | (0.012, 0.04) | 0.004–0.1 | Sprite footprint; bigger → more overdraw. |
| speed_scale (streak) | 0.06 | 0–0.2 | Streak length per m/s. |
| max_stretch | 6 | 1–12 | Streak clamp. |
| spin_rpm (Spin variant) | 0 | −100…+100 | Rotating nozzle. |
| oscillation (from DMX Ch5) | 0 | −1…+1 | Pan/spin amount + dir. |

### 4.2 CO2 — parameters

| Parameter | Default | Range | Trades off |
|---|---|---|---|
| **throw_m** (from Intensity/Output) | 9 | 6–20 | Column height; drives `v0`. |
| **output** (from Intensity) | 1.0 | 0–1 | Puff count + density. |
| **blast_duration_s** (from Duration ch) | 1.5 | 0.3–8 (HW-capped) | Burst length; longer → more puffs alive. |
| burst_count (at output=1) | 120 | 0–400 | Per-blast puff count; perf. |
| nozzle_cone_deg | 8 | 0–20 | Column taper. |
| nozzle_radius_m | 0.05 | 0–0.3 | Source width. |
| lifetime_s | Uniform(1.0, 2.5) | 0.5–4 | Dissipation time. |
| updraft0 (m/s²) | +3 | 0–8 | Initial buoyancy. |
| updraft_decay | 2.0 | 0.5–6 | How fast it stalls. |
| gravity (m/s²) | 0.5 | 0–9.81 | Slight settle (cold dense cloud). |
| drag_k (1/s) | 1.1 | 0.3–3 | Billow stall + dissipation. |
| grow_rate (size/t) | 2.5 | 0–6 | Plume widening. |
| base_size_m | 0.6 | 0.2–2 | Puff footprint; overdraw cost (fog billboards are big). |
| opacity_peak | 0.85 | 0.1–1 | Core density. |
| pan_deg / tilt_deg (moving) | 0 / 0 | ±90 / ±60 | Nozzle aim. |
| move_speed | 0 | 0–1 | Slew rate. |
| use_volumetric (LOD-B) | false | bool | Froxel/raymarch density inject (catches beams truly) vs cheap billboards. |

### 4.3 Quality / perf knobs (shared, both devices)

| Knob | Default | Range | Effect |
|---|---|---|---|
| **max_particles** (per emitter, hard cap) | sparks 4000 / CO2 600 | up to 20000 / 2000 | Spawn is throttled so live count never exceeds this — perf never tanks. |
| **global_particle_budget** (all pyro) | 40000 | 5000–200000 | Cap summed across all pyro fixtures; over budget → proportional spawn throttle. |
| sim_substeps | 1 | 1–4 | Stability for high speed/drag; cost ∝ substeps. |
| **lod_distance_m** | 35 | 10–120 | Beyond it, spawn rate + sprite count scale down (1/d²-ish), then freeze sim and bill as a single impostor. |
| lod_cull_m | 120 | 50–500 | Beyond it, the emitter draws nothing (frustum + distance cull). |
| quality_preset | High | Low/Med/High/Ultra | Scales max_particles, substeps, soft-depth, curl on/off, CO2 LOD-B on/off in one switch. |
| update_in_render | false | bool | Sim runs once per real frame in `Scene::advance` (NOT in the renderer) — mirrors the wheel-motion rule in `scene/mod.rs::advance` (capture + render share `record_scene`, so simulating in the renderer would double-advance). |

LOD scaling rule (per emitter, per frame):
```
d = distance(camera, emitter_nozzle)
lod = clamp(1 - (d - lod_distance) / (lod_cull - lod_distance), 0, 1)
effective_spawn = base_spawn * density * lod
if d > lod_cull { draw nothing }
```

---

## 5. File-by-file implementation plan (this codebase)

The pyro feature is modelled exactly on **`LedScreen`** — a new placed-object
kind on `Scene`, with its own library profiles, DMX footprint+decode, renderer
pass, Add-menu/inspector/outliner wiring, and a `.archie` format bump. Cited
files/symbols are live in the repo.

### 5.1 Data model — new `scene::pyro` module

**New file `src/scene/pyro.rs`** (sibling of `src/scene/screen.rs`). Define:

```rust
pub enum PyroKind { Co2Jet, ColdSpark }          // serde, ALL[], label(), code()
pub struct PyroDevice {                          // mirrors LedScreen
    pub name: String,
    pub kind: PyroKind,
    pub profile_name: String,                    // library component (display)
    pub transform: Mat4,                          // Y-up metres; nozzle at origin, aim +local-Y
    // physical tunables (per §4) with defaults from the profile:
    pub height_m: f32, pub density: f32, pub cone_deg: f32,
    pub color_t0_k: f32, pub color_t1_k: f32,    // spark blackbody endpoints
    pub throw_m: f32, pub opacity_peak: f32,     // CO2 column
    pub pan: f32, pub tilt: f32, pub move_speed: f32, // moving variant
    // live trigger state (serde-skip — runtime only, like LedScreen.frame):
    #[serde(skip)] pub armed: bool,
    #[serde(skip)] pub firing: f32,              // 0..1 trigger level this frame
    #[serde(skip)] pub fire_until: f32,          // scene-time the current blast ends
    pub hidden: bool,
    #[serde(skip)] pub id: super::EntityId,
    // quality/perf:
    pub max_particles: u32, pub quality: u8,
}
impl PyroDevice {
    pub fn from_profile(p: &PyroProfile, name, transform) -> Self { … }
    pub fn world_nozzle(&self) -> Vec3 { self.transform.transform_point3(Vec3::ZERO) }
    pub fn world_dir(&self) -> Vec3 { … pan/tilt applied … }
    pub fn world_bounds(&self) -> (Vec3, Vec3) { … capsule around the plume … }
    pub fn ray_hit(&self, ro, rd) -> Option<f32> { … small body box, like a fixture … }
}
```

The **particle simulation state** is runtime-only and lives in the renderer (or
an `app`-owned `PyroSim` keyed by `EntityId`), NOT serialized — exactly like
`ScreenFrame`/`screen_runtime`.

**Edit `src/scene/mod.rs`**:
- `pub mod pyro;` + `pub use pyro::PyroDevice;` (next to `pub mod screen;` /
  `pub use screen::LedScreen;`, lines 9 & 15).
- Add `pub pyro: Vec<PyroDevice>` to `struct Scene` (line 762, after `screens`).
- Init `pyro: Vec::new()` in `Scene::demo` (line 807) and clear it in
  `import_mvr` (line 1008).
- Extend `ensure_ids` (line 1141) to assign+reseed pyro ids (the serde-skip id
  trap — same loop as screens at line 1170).
- Add `add_pyro_at(&mut self, profile, ground) -> usize` (mirror
  `add_screen_at`, line 1091) and `pyro_index_of(id)` (mirror
  `screen_index_of`, line 1197).
- Selection: extend `Selection` (add `pub pyro: Vec<usize>`), `SelKind`,
  `SelItem`, `ObjectRef`, `apply_select`, `object_refs`, `object_world_bounds`
  (line 902), `object_anchor` (line 917), `translate_object` (line 979),
  `duplicate_object` (line 933). This is the bulk of the data-model work — pyro
  becomes a first-class transformable/duplicable kind like Screens. (Smaller
  alternative: reuse the Screens machinery and tag the kind on the struct;
  but a clean new `SelKind::Pyro` matches the existing pattern.)
- `scene_frame` (line 1052): include pyro nozzles in the framing bounds.

### 5.2 Library profiles + Add menu

**Edit `src/scene/library.rs`**:
- New `pub struct PyroProfile { name, category, kind: PyroKind, default_height_m,
  default_throw_m, default_color_t0_k, default_max_particles, moving: bool }`
  (mirror `ScreenProfile`, line 55).
- Add `pub pyro: Vec<PyroProfile>` to `struct Library` (line 72) and populate in
  `Library::standard()` (line 82): e.g. "CO2 Jet", "CO2 Jet (LED ring)",
  "CO2 Moving Jet", "Cold Spark", "Cold Spark (moving head)", "Cold Spark Spin".

**Edit `src/ui/windows/add_menu.rs`**:
- Add `AddCategory::Pyro` to the enum (line 24), `ORDER` (line 32),
  `label`/`icon` (lines 35/43; add a `theme::icon::PYRO` glyph — Phosphor, no
  emoji per the font-coverage memo).
- Add `AddAction::Pyro(usize)` (line 54) + `category_entries` arm (line 141) +
  `action_item`/`clone_entry`/`action_of` arms.

**Edit `src/ui/mod.rs`** (AddAction handler, lines 2116-2139 and the
`lib_prefs::LibItem` map at 2101): add an `AddAction::Pyro(i)` arm that calls
`Selection::pyro(cx.scene.add_pyro_at(&profile, place))`. Also `src/ui/panels.rs`
line ~1053 (`LibKind::Screen` add path) gets a `LibKind::Pyro` sibling, and
`lib_prefs.rs` gets a `LibItem::Pyro`.

### 5.3 DMX footprint + decode

**Edit `src/dmx/patch.rs`**: pyro devices are **not** `Fixture`s, so the
existing `PatchTable` (index-parallel to `scene.fixtures`) won't cover them. Two
options:
- **(preferred)** add a parallel `pyro_patch: Vec<Option<Patch>>` to `DmxIo`
  (the same shape as `LedScreen::PixelMapDmx`, which is patched *inline* on the
  screen rather than through `PatchTable` — see `screen.rs::PixelMap` and
  `decode.rs::apply_screens`, line 529). Store `universe/start_address/mode`
  directly on `PyroDevice` (add a `pub patch: Option<PyroPatch>` field with
  `universe, address, mode`), exactly like `PixelMap`. This keeps pyro off the
  fixture patch table (no fingerprint/realign churn) and out of MVR round-trip.
- The two footprint **modes** are encoded as a `mode: u8` on the patch + a
  `footprint(kind, mode)` helper returning 1/4/7 (CO2) or 3/5 (spark).

**Edit `src/dmx/decode.rs`**: add `pub fn apply_pyro(pyro: &mut [PyroDevice],
snap: &UniverseSnapshot)` modelled on `apply_screens` (line 529). It reads the
device's `patch`, decodes the channels per §2 (reuse `snap.level(univ, ch)` /
`read_chan`), and writes into the runtime fields:
- CO2: `armed = (arm in 100..=199)`; `firing = blast >= 200`; if firing & armed
  and not already firing, set `fire_until = scene_time + duration_map(dur_ch)`;
  set `density/throw` from Intensity; `pan/tilt` from moving channels.
- Spark: `armed = (safety in 50..=200)`; `density = spark/255 if armed`;
  `height_m` from Height ch; macro 20–40 = emergency stop (clear `armed`),
  60–80 = clear (suppress spawn); `oscillation` from Ch5.

Call `apply_pyro` once per frame from the same place `apply_screens` is called
(the app's DMX tick), and advance the **sim** once per frame in
`Scene::advance` (line 825) — NEVER in the renderer (the double-advance trap
documented there for wheel motion applies equally to particles).

### 5.4 Renderer — particle/fog pass + shaders

**New files**:
- `src/shaders/spark.wgsl` — additive, velocity-stretched, blackbody-coloured
  billboard sparks (the VS expands a unit quad per instance using §3.1 math;
  the FS does a soft round/streak falloff × HDR colour × soft-depth fade).
- `src/shaders/co2.wgsl` — alpha-blended (premultiplied) white fog billboards,
  lit by the same fixtures-as-spotlights buffer the volumetric pass uses
  (or sampling the froxel result texture), grow-over-life, soft-depth fade.
- Optionally a compute shader `src/shaders/pyro_sim.wgsl` for GPU simulation;
  for glowstone a **CPU sim** (in `app`/renderer, keyed by `EntityId`) writing an
  instance buffer is simpler and matches how the app already builds per-frame
  instance vectors.

**Edit `src/renderer/mesh.rs`**: add a `ParticleInstance` Pod struct + its
`vertex_buffer_layout()` (mirror `WallInstance`, line 133): `pos: [f32;3],
size: [f32;2], color: [f32;4] (HDR rgb + alpha), vel: [f32;3], kind/flags`.
Reuse the existing `unit_quad` (line 396) / a 2-tri quad and `GrowBuffer`
(line 189) for the instance buffer (mirror `wall_instances`, `renderer/mod.rs`
line 810).

**Edit `src/renderer/mod.rs`**:
- New `spark_pipeline` + `co2_pipeline` (mirror `wall_pipeline`, line 328 +
  `pipeline::wall_pipeline`, line 725; add `pipeline::spark_pipeline` /
  `co2_pipeline` in `src/renderer/pipeline.rs`). Spark pipeline = additive
  blend, depth-test-no-write; CO2 pipeline = premultiplied-alpha blend,
  depth-test-no-write, drawn back-to-front (or WBOIT).
- A `pyro_runtime: HashMap<EntityId, PyroSim>` (mirror `screen_runtime`, used at
  lines 2216/2457) holding the live particle arrays; advanced each frame from
  the CPU sim (`Scene::advance`), uploaded into `GrowBuffer`s.
- In `record_scene` (the per-frame prep around line 2212 where walls are built),
  add a pyro block: per device, run/advect the sim (or read the app-owned sim),
  build `ParticleInstance`s, upload, and remember draw ranges per device.
- In the forward/composite pass (around line 3290 where walls are drawn): draw
  CO2 fog **before** sparks; draw sparks additively **after** the HDR scene so
  bloom catches them. Both draw into the HDR target so `post.wgsl` blooms+tonemaps
  them.
- **CO2 catches coloured light**: reuse the fixtures-as-spotlights `FixtureGpu`
  buffer already uploaded for the volumetric pass (built at lines 2256-2477,
  including the LED-wall area-lights). Either (a) shade CO2 billboards in
  `co2.wgsl` by looping the nearest few cones, or (b) if `settings.froxel_volumetric`
  is on, sample the froxel `result_view` (line 96) at the billboard centre.
  For LOD-B (volumetric density inject), add the plume capsule as extra density
  in `volumetric.wgsl` / `froxel.wgsl` (the `VolumetricUniform`/`FroxelUniform`
  at lines 44/75) — gated by the froxel toggle/quality preset.
- Selection mask: add pyro to the `sel_mask` pass (mirror `sel_mask_wall_pipeline`,
  line 337) so selected pyro devices get the outline; and the
  `selection.screens`-style highlight gate (line 3354).
- Perf overlay counts (line 3615): add live-particle + draw counts.

### 5.5 Inspector + outliner

**Edit `src/ui/panels.rs`**:
- New `fn pyro_inspector(ui, &mut PyroDevice, …)` (mirror `led_screen_inspector`,
  line 2698 + 2844 row groups) exposing the §4 tunables grouped (Geometry /
  Look / Colour / Quality), the DMX patch (universe/address/mode like the
  PixelMap inspector at lines 3016+), and a **kind-aware** parameter set (spark
  shows T0/T1/cone/streak; CO2 shows throw/opacity/grow/volumetric).
- Dispatch it from `inspector_body` (line 1986, where `led_screen_inspector` is
  called for `primary_screen`).
- Outliner rows: the scene outliner already lists Screens with an eye toggle +
  rename + hide (see `ui/mod.rs` lines 1019-1075 for the screen eye/rename
  paths, and the `commit_delete` remap at lines 1107-1111). Add a parallel
  "Pyro" folder/section with the same eye/rename/hide/delete wiring keyed by
  `EntityId`.

**Edit `src/ui/mod.rs`**:
- `commit_delete` (line ~1107): handle deleting selected pyro devices, remapping
  selection/groups/cues exactly as screens are handled.
- Cursor-snap / framing helpers that iterate screens (lines 2428/2459) get a
  pyro sibling.

### 5.6 `.archie` format bump + cues

**Edit `src/ui/project.rs`**: bump `pub const FORMAT: u32` 8 → **9** (line 42)
with a changelog line: `v9: Scene gained pyro: Vec<PyroDevice> (CO2/spark
devices) + per-device PyroPatch`. Because `Scene` is serialized positionally by
bincode and the new `pyro` field is **last** in `Scene` (after `render`), and
`read` rejects any version != FORMAT up front (line 110), older files surface a
clean "unsupported version" error rather than mis-decoding (same discipline as
the v8 note). No serde-skip needed for the persisted fields; runtime fields
(`armed`, `firing`, `fire_until`, `id`, sim) are `#[serde(skip)]` like
`LedScreen.frame`/`id`. The `PyroPatch` (universe/address/mode) persists with the
device so a saved show keeps its patch (it is NOT in `PatchTable`).

**Cues** (`src/ui/cues.rs`): a cue can already fade fixture/screen state; a pyro
**Blast** is a momentary action, so add a cue action that drives a device's
`firing`/`fire_until` for a duration (the natural place to model CO2/spark
"Go" timing, since duration is a cue concern not a DMX channel for sparks).

### 5.7 Tests

Mirror the existing `decode.rs` tests (`pixelmap_screen_reads_rgb_grid_from_dmx`,
line 581; `synthetic_decode_*`): unit-test `apply_pyro` arm/blast/duration
decode for both modes; `project.rs` round-trip test adding a `PyroDevice`;
`scene/mod.rs` `ensure_ids`/duplicate/translate tests extended for the new kind.

---

## 6. Performance budget + LOD strategy

The non-negotiable rule (matching the codebase's volumetric perf philosophy in
`docs/RESEARCH-volumetric-scaling.md` and the `render_scale`/`shadow_max` levers
in `scene/mod.rs`): **performance must never tank**, so every cost is bounded by
a hard cap, not by content.

**Budget (per frame, target ≥ 60 fps at the default 50% `render_scale`)**:

| Item | Budget | How it's held |
|---|---|---|
| Total live pyro particles | ≤ 40 000 (global cap) | `global_particle_budget`; spawn throttled proportionally when over. |
| Per spark emitter | ≤ 4000 | hard `max_particles`; spawn = `min(rate*density*lod, cap-live)`. |
| Per CO2 emitter | ≤ 600 big puffs | as above (fog puffs are few + large). |
| Sim cost | ≤ ~0.5 ms CPU | CPU sim, 1 substep default; SoA arrays; once/frame in `Scene::advance`. |
| Overdraw (sparks) | bounded | small additive sprites; depth-test (no write); soft-depth fade. |
| Overdraw (CO2) | bounded | few large puffs; alpha-blend, no shadow maps, no per-pixel light loop in the cheap path. |
| Shadow maps | 0 | pyro never consumes a hero shadow layer (like LED walls, `mod.rs:2419`). |
| Volumetric inject (LOD-B) | opt-in | only when `froxel_volumetric` on or Ultra preset. |

**LOD strategy (by distance, per emitter, per frame)**:

1. **Distance fade of spawn rate**: `effective_spawn = base * density * lod`
   with `lod = clamp(1 - (d - lod_distance)/(lod_cull - lod_distance), 0, 1)` —
   far emitters spawn fewer particles (they're tiny on screen anyway).
2. **Sprite-count decimation**: beyond `lod_distance` (default 35 m) draw a
   fraction of particles (stochastic, hashed by `seed`) and scale survivors'
   size up to keep apparent density — caps overdraw for a back-of-house rig.
3. **Impostor freeze**: very far (near `lod_cull`) emitters freeze their sim and
   draw a single pre-baked impostor billboard (a fountain/plume sprite) — O(1).
4. **Frustum + distance cull** (`lod_cull`, default 120 m): off-screen / very
   distant emitters draw nothing and pause their sim (no spawn, no advect).
5. **Quality preset** scales `max_particles`, `sim_substeps`, soft-depth on/off,
   curl on/off, and CO2 LOD-B on/off in one switch — so a heavy show on a weak
   GPU drops to Low and stays interactive, the same way `render_scale` /
   `auto_resolution` / `shadow_max` already gate the existing renderer.
6. **Dynamic-resolution interaction**: the existing `auto_resolution` scaler
   (`scene/mod.rs:190`) already drops `render_scale` to hold `fps_target`; the
   pyro `quality_preset` is the coarse content lever that complements it, so the
   two together keep the frame budget.

This bounds pyro to a fixed worst-case cost regardless of how many CO2/spark
devices are placed or how long they fire — the same "lossless cap, then bounded
LOD" pattern that fixed the many-lights fog cliff.

---

## Open questions / forward-looking

- **Particle sim location**: CPU (in `app`/`Scene::advance`, simplest, matches
  existing per-frame instance building) vs. a GPU compute sim
  (`pyro_sim.wgsl`, scales further). Recommend CPU first; the instance-buffer
  contract is identical either way.
- **CO2 "catches light" fidelity**: cheap per-cone billboard shading vs. the
  froxel-density-inject LOD-B. Ship cheap first; LOD-B is the upgrade.
- **MVR round-trip**: pyro stays off `PatchTable` and out of MVR (like the LED
  pixel-map path), so an MVR import simply clears `scene.pyro`. If future MVR
  support is wanted, pyro would need its own GDTF-style fixture mapping.
- **Order-independent transparency** for stacked CO2 puffs: start with
  back-to-front sort per emitter; WBOIT only if puff counts grow.
