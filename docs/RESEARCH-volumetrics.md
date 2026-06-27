# Hyper-Realistic Volumetric Beams & Haze — Rendering R&D

> Implementation guide for the volumetric layer of this `wgpu 29 + winit` stage-lighting
> glowstone. Audience: the engineer who will write the WGSL/compute passes. This document is
> the design rationale and the phased plan; it intentionally names texture formats, buffer
> layouts, workgroup sizes, and the exact integration math you must copy verbatim.
>
> Repo context this targets (already present): a hand-written forward renderer
> (`src/renderer/`) with a grid, instanced lit meshes, an offscreen viewport target
> (`Rgba8Unorm` color + `Depth32Float`) shown in egui; a `Fixture` struct with
> `position / pan / tilt / color / intensity / beam_angle` (`src/scene/fixture.rs`); and an
> `Environment` fog box with `density (sigma_t) / color (albedo tint) / anisotropy (g)`
> (`src/scene/environment.rs`). **There are currently no compute pipelines** — the
> volumetric work is greenfield.

---

## 1. Executive summary & recommended architecture

The look we are after — razor-sharp moving-head shafts and CO2 jets cutting through drifting
haze, scaling to hundreds-to-thousands of fixtures at 60 fps — is best served by a **two-tier
hybrid volumetric system** rather than any single technique. Tier A is a **camera-frustum
"froxel" grid** (Wronski 2014 / Hillaire 2015): a low-resolution clip-space 3D texture
(start `160×90×64`, `rgba16float`) filled by two compute passes — *light injection* (per
froxel, fed by **clustered/Forward+ light culling** so each froxel only loops over fixtures
whose cone reaches it) and *front-to-back scattering integration* using the
energy-conserving analytic slice integral `(S − S·Tr)/σ_t` — then composited into the
existing forward pass with one trilinear lookup at each pixel's depth (`out = surface·Tr +
inScatter`). The froxel grid carries the **breadth**: global haze fill plus ambient
in-scatter from *all* fixtures, at a cost decoupled from screen resolution and from fixture
count. Tier B is a small set of **high-resolution per-"hero"-beam shadow-mapped raymarches**
(Bevy/Wicked/Killzone style) for the few fixtures the eye is actually tracking, which adds
the **sharpness** the low-res grid physically cannot: crisp cone edges, gobo cutouts,
volumetric self-shadowing. Both tiers share one world-space procedural density field (FBM +
curl-noise advection + domain warp, bounded by the fog box) and one Henyey–Greenstein phase
function (`g ≈ 0.7–0.85` for forward-scattered "searchlight" punch). Temporal reprojection +
Halton/blue-noise sub-froxel jitter (≈5% EMA history blend) is mandatory **from day one** —
anisotropic beams on a frustum-aligned grid strobe badly without it. Everything renders into
a new **HDR (`Rgba16Float`) offscreen target** with exposure + bloom + tonemap before the
egui blit, because a bright beam clipped to flat white loses exactly the forward-scattered
glow that sells "intense light in haze".

```
                          ┌─────────────────────────────────────────────────────┐
   fixtures (storage buf) │  COMPUTE                                             │
   env fog box ──────────►│  [0] cluster build (resize-only)  →  clusterAABB[]   │
   density params         │  [1] light cull (per frame)       →  lightGrid[],idx │
                          │  [2] froxel material inject       →  vol0(scat,σt)   │
                          │  [3] froxel light inject          →  vol1(S, σt)     │  Tier A
                          │  [4] froxel integrate front→back  →  volInt(scat,Tr) │  (breadth)
                          │  [5] per-hero-beam raymarch       →  beamRT (½ res)  │  Tier B (sharp)
                          └─────────────────────────────────────────────────────┘
   forward opaque pass ──►  HDR Rgba16Float viewport target
                          └─► composite: out = surf·Tr_froxel + scat_froxel + beam·Tr_froxel
                          └─► exposure → bloom → ACES tonemap → sRGB + dither → egui blit
```

---

## 2. Background: the volume rendering math

A view ray through participating media accumulates *out-scattering + absorption* (which
darken what is behind) and *in-scattering* (light from sources redirected toward the eye).
The two coefficients are absorption `σ_a` and scattering `σ_s`; their sum is the **extinction**
coefficient and the **single-scattering albedo** is the fraction that re-radiates:

```
σ_t = σ_a + σ_s          (extinction, units m^-1)
ρ   = σ_s / σ_t          (single-scattering albedo)
```

Theatrical haze is nearly non-absorbing, so `ρ ≈ 0.9–1.0`; keep the media `σ_s` near-neutral
and let the *fixtures* color the beams (matching how real coloured stage lights + grey haze
read). This is exactly the unified parameterization Frostbite advocates: every contributor
(height fog, the fog box, CO2 jets, particle puffs) sums into the same `{σ_s, σ_a, σ_t, Le, g}`
fields.

**Beer–Lambert transmittance** — the fraction of light surviving a path `A→B`:

```
Tr(A→B) = exp( − ∫_A^B σ_t(x) dx )
homogeneous slice of length d:   Tr = exp( −σ_t · d )
```

**The single-scattering volume rendering equation** along a ray of length `s` from the eye:

```
L(x,ω) = Tr(x, x_s)·L_surface  +  ∫_0^s Tr(x, x_t)·σ_s(x_t)·L_scat(x_t, ω) dt

L_scat(x_t, ω) = Σ_lights  p(θ_l) · Vis_l · L_l(x_t)     (+ ambient/sky term)
```

i.e. surface radiance attenuated by transmittance, plus, at every point along the ray, the
in-scattered radiance (sum over lights of *phase × visibility × incident radiance*) attenuated
by the transmittance back to the eye. `Vis_l` folds together the opaque shadow map and the
*volumetric* (media self-)shadow.

**Henyey–Greenstein phase function** — how strongly light scatters toward angle `θ` between
the incident light direction and the view direction, controlled by one anisotropy parameter
`g ∈ (−1,1)` (`g=0` isotropic, `g→1` forward, `g<0` back). Haze (Mie scattering) is strongly
forward-peaked, which is *why* a beam blazes when you look into it and dims from the side:

```
                 1        (1 − g²)
p_HG(θ) =  ───────── · ───────────────────────       cosθ = dot(viewDir, lightDir)
              4π       (1 + g² − 2g·cosθ)^(3/2)
```

> **Sign-convention warning (a frequent, real bug).** PBR-Book writes the denominator with
> `+2g·cosθ` because it measures the angle between two directions *both pointing away* from the
> scatter point; the real-time/graphics convention used in Godot, Bevy, diharaw etc. writes
> `−2g·cosθ` with `cosθ = dot(viewDir, lightDir)`. They are the *same function* under the
> opposite angle definition. **Pick one and verify empirically that a beam aimed AT the
> camera gets brighter, not dimmer.** Constant: `1/(4π) = 0.07957747154594767`.

**Schlick approximation** (drops the `pow(·,1.5)` — worth it at thousands of fixtures × many
steps): `k = 1.55·g − 0.55·g³`, then

```
p_Schlick(θ) = (1/4π) · (1 − k²) / (1 + k·cosθ)²
```

For very tight beam fixtures, **Cornette–Shanks** softens HG's overly-sharp forward spike:
`p_CS = (3/8π)·((1−g²)(1+cos²θ)) / ((2+g²)(1+g²−2g·cosθ)^(3/2))`. A **dual-lobe**
`α·p_HG(g₁) + (1−α)·p_HG(g₂)` with `g₁≈0.8, g₂≈−0.2` gives the forward halo plus a faint
back-glow real hazers show — make it an optional "realistic" mode.

---

## 3. The recommended froxel pipeline (Tier A)

### 3.1 The froxel grid (data structure)

Allocate a clip-space-aligned 3D texture: XY map to screen NDC tiles, Z steps through view
depth. Each voxel is a frustum-shaped "froxel" (wide far from camera, narrow near it).
Lighting is evaluated **once per froxel**, not per pixel, so cost is decoupled from the egui
viewport resolution and `160×90×64 = 921,600` texels ≈ one 720p surface. The apply step uses
**quadrilinear filtering** (trilinear in the volume + the bilinear screen tap), so individual
froxels are never visible and there are none of the depth-discontinuity / bilateral-upsample
edge artifacts that plague 2D half-res volumetrics.

| Resource | Format | Default size | Contents |
|---|---|---|---|
| `vol_scatter_ext` (V-buffer 0) | `rgba16float` | `160×90×64` | `scattering.rgb`, `extinction(σ_t).a` |
| `vol_emit_phase`  (V-buffer 1, optional) | `rgba16float` | `160×90×64` | `emissive.rgb`, `phase g.a` |
| `vol_light` | `rgba16float` | `160×90×64` | per-froxel in-scattered radiance `S.rgb`, `σ_t.a` |
| `vol_integrated` | `rgba16float` | `160×90×64` | `accumScatter.rgb`, `accumTransmittance.a` |
| `vol_history` | `rgba16float` | `160×90×64` | previous frame `vol_light`/integrated for EMA |

> **Store EXTINCTION (linear), never pre-baked transmittance (non-linear)** in the volumes
> you temporally accumulate — averaging `exp()` values is biased and causes beam-brightness
> flicker. Hillaire is explicit about this.

WGSL/wgpu-29 notes: these are `texture_storage_3d<rgba16float, ...>`. **Read-write storage
textures on the same view are not universally available** in wgpu 29 / WebGPU — design each
pass as *read one volume, write another* (ping-pong), or use the
`TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` path. The integrate pass writes in-place per
column so a `write` storage texture + a `texture_3d` sampled read of the *injected* volume is
the safe layout. Confirm the adapter exposes `rgba16float` with `STORAGE_BINDING` at startup;
fall back to a flat storage *buffer* + manual indexing if not.

### 3.2 Exponential depth-slice distribution

Slices are **not** linear in depth — pack them near the camera (where beams are sharpest and
aliasing shows) and spread them far (where fog is low-frequency). Wronski's slides describe
this only verbally as "exponential, concentrated near camera"; the closed forms below are the
canonical ones the follow-on implementations (Godot, Filament, diharaw, clustered-shading)
actually use. **Use the same mapping for the froxel grid and the cluster grid** so injection
can read the cluster light list directly.

```
geometric / logarithmic (Filament, DOOM, clustered-shading):
    viewZ(slice) = near · (far/near)^(slice / N)
    slice(viewZ) = N · log(viewZ/near) / log(far/near)        ← one madd + floor:
                   slice = floor( log(viewZ) · scale + bias )
                   scale = N / log(far/near)
                   bias  = −N · log(near) / log(far/near)

power / "Detail Spread" (Frostbite, Godot):
    viewZ(slice) = near + (far−near) · (slice/N)^k ,  k ≈ 2–4 pushes detail near
```

Per-froxel slice **thickness** `dz = viewZ(s+1) − viewZ(s)` is what feeds Beer–Lambert and
the scattering integral. **Anchor `near` to the fog box, not the camera near plane** (cf.
Filament's `zLightNear ≈ 5 m`): the media is bounded, so spend slice resolution inside the
haze where beams live, not on empty foreground. Stages are small (a few tens of metres), so
you can afford dense slicing; bias `k` strongly toward the camera since the glowstone operator
orbits close to fixtures.

### 3.3 Pass [2]/[3] — material + light injection (one thread per froxel)

For each froxel: reconstruct its world position from `(uv*2−1)` through the inverse
view-projection at `viewZ(slice + jitter)`; sample the **procedural density** there (§5),
zero outside the fog box AABB; compute in-scattered radiance from the lights reaching it.

```wgsl
// per froxel, in the light-injection pass:
let cluster = clusterIndexOf(froxel.xy, slice);          // §6: same exp mapping as froxel z
let grid    = lightGrid[cluster];                        // {offset, count}
var S = vec3(0.0);
for (var i = 0u; i < grid.count; i++) {
    let f  = fixtures[ lightIndexList[grid.offset + i] ];
    let Lp = normalize(f.pos - froxelPos);
    let cosT = dot(normalize(camPos - froxelPos), Lp);   // view·light
    let cone = spotMask(f, froxelPos);                   // §4.4 cone + gobo
    let vis  = opaqueShadow(f, froxelPos) * volShadow(f, froxelPos);
    let atten = f.intensity * cone / max(dot(...), r0*r0);
    S += phaseHG(g, cosT) * vis * atten * f.color;
}
S = sigma_s * (S + ambientSH);                           // + emissive Le
textureStore(vol_light, froxelCoord, vec4(S, sigma_t));
```

This is **the only per-light cost in the whole pipeline**, and clustered culling keeps it
proportional to *local* fixture overlap, not total rig size. Wronski's "affects atmosphere"
flag is worth keeping so house/utility fixtures don't all pay volumetric cost.

### 3.4 Pass [4] — front-to-back integration (one thread per XY column, march +Z)

Walk the slices from the camera outward. **Use the energy-conserving analytic per-slice
integral, not the naive point-sample** `S·dz` (Hillaire flags the latter as "Wrong" — it
loses energy as slices get thick, visibly under-lighting far froxels). This is the diharaw
shader verified verbatim (`ray_march_cs.glsl`, `local_size 8×8×1`, `rgba16f`):

```
∫_0^D e^(−σ_t·x)·S dx  =  (S − S·e^(−σ_t·D)) / σ_t  =  S·(1 − exp(−σ_t·D)) / σ_t

per slice of thickness dz:
    Tr_slice  = exp(−σ_t · dz);
    Sint      = (S - S*Tr_slice) / max(σ_t, 1e-5);   // guard σ_t→0 (limit = S·dz)
    accumScat += accumTr * Sint;
    accumTr   *= Tr_slice;
    store(vol_integrated, froxel) = vec4(accumScat, accumTr);
```

### 3.5 Temporal reprojection + jitter (mandatory)

Each frame, jitter the slice sample depth by a Halton offset (one offset per view-ray column;
jitter the density *and* scatter samples in sync); reproject last frame's volume by the
froxel's previous-frame world position; EMA-blend ≈5% current into history. 3D reprojection
is far more robust than 2D TAA because the full view-ray data behind a moved object is still
valid — the *only* invalid history is froxels that fell outside the previous frustum (skip
history there) and the thin shell directly behind a moved dynamic object.

```
jitteredViewZ = viewZ( (slice + halton(frame)) / N )
prevUVW = worldToPrevFroxelGrid(froxelWorldPos)            // previous view-projection
valid   = all(prevUVW >= 0 && prevUVW <= 1)
result  = valid ? mix(history(prevUVW), current, 0.05) : current
```

Watch ghosting on **fast pan/tilt fixtures and intensity strobes** (common in live shows):
add a neighborhood clamp / variable blend factor if the 5% EMA smears chases. This is the
hardest temporal case and worth validating against a real console chase sequence.

### 3.6 Compositing with the forward/opaque pass

Because the integrate pass stored *accumulated in-scatter + total transmittance per froxel
along the whole view ray*, applying the volume is **one quadrilinear lookup at the fragment's
depth + one MAD** — no per-light work, decoupled from geometry, and it composes correctly
over arbitrarily many transparent layers (haze sheets, scrims, gels) since each fragment
samples the volume at its own depth:

```wgsl
let uvw = vec3(screenUV, sliceOfViewZ(viewZ) / N);   // inverse of the depth mapping
let fog = textureSampleLevel(vol_integrated, samp, uvw, 0.0);
out.rgb = surface.rgb * fog.a /*transmittance*/ + fog.rgb /*in-scatter*/;
```

Do it either inside the existing lit-mesh fragment shader (forward) or as a fullscreen pass
over the depth target. The egui viewport blit is unchanged.

---

## 4. Sharp beams (Tier B): crisp high-res fixture shafts

The froxel grid is intentionally low-res and *cannot* render the hard, high-frequency edges
of a beam or a gobo cutout — 64 depth slices blur them. The solution is to **layer a
higher-frequency per-beam pass over the froxel fog** for the few "hero" fixtures.

### 4.1 Selecting hero beams (LOD ladder)

Per fixture, at runtime, pick a tier so the cost is bounded:

```
useRaymarch = (cone_half_angle < θ_thresh)        // narrow Beam/MovingHead, not wide Wash
              && (screen_area(beam) > A_thresh)    // actually visible
              && (distance_rank < N_max)           // cap simultaneous hero beams
```

This maps cleanly onto fixture parameters: tight `beam_angle` + high `intensity` →
raymarch; wide wash / distant / tiny → froxel-only. Everything not selected for raymarch
still contributes to the froxel grid (and can additionally get the analytic shadowless
airlight term, §4.5). A cross-fade in in-scatter contribution avoids a visible pop when a
beam crosses the screen-area / distance threshold.

### 4.2 Per-beam shadow-mapped raymarch

For each hero beam: render a depth/shadow map from the fixture, clip the view ray to the fog
box AABB ∩ the cone, march `~8–24` steps. At each step: reconstruct world pos, test the
fixture's shadow map (lit if `distToLight ≤ shadowDepth`), apply the cone mask + gobo + `1/d²`
falloff, multiply by phase × local density, accumulate scattered light weighted by running
transmittance. **The energy-conserving step form lets you take big steps without darkening**,
which is the key to keeping many beams real-time:

```
S  = σ_s·density · phase(μ) · lightColor · vis · coneMask · gobo
Tr = exp(−σ_t·density·dt)
L += T_accum · (S − S·Tr)/max(σ_t·density, 1e-5);   T_accum *= Tr;
// jittered start kills banding:
t0 = dt · fract( blueNoise(pixel) + frame·0.61803398875 );
```

Render this into a **half-resolution target** (`beamRT`) and bilateral-upsample using the
depth buffer (the single biggest perf win). Composite over the froxel result front-to-back:
`T_total = Tr_froxel·Tr_beam`, `L = L_froxel + L_beam·Tr_froxel`. **To avoid double-counting
the haze fill, exclude hero fixtures from the froxel light-injection loop** (or weight them
down), since they are fully accounted for in Tier B.

### 4.3 Volumetric shadows (beams shadowing through haze, inter-beam occlusion)

Single-scattering self-shadowing makes a beam dim as it passes behind a denser smoke region
or another beam. Two routes:

- **Cheap (MVP, perfect for the bounded fog box):** in the froxel injection, march the *same
  froxel extinction* from light→froxel (Beer–Lambert) and multiply into `Vis_l`. This gives
  inter-beam occlusion and "beam dims through denser smoke" essentially for free inside the
  box. `Vis_l = opaqueShadow · exp(−∫_{light→froxel} σ_t dx)`.
- **Full (later):** Frostbite's separate low-res cascaded/clip-map extinction volume (3
  levels) accumulating extinction from the dominant light's viewpoint, so shadows from media
  *outside* the camera frustum work too. Defer until needed.

For hard gobo/blade shadows on hero beams, the per-fixture depth map (§4.2) already provides
them; a Variance Shadow Map gives soft penumbrae that read like cheap multi-scatter for wash
fixtures.

### 4.4 Gobo / zoom / cone profile

The beam's angular intensity is a smooth cone falloff between inner (full) and outer (zero)
half-angles; **zoom** changes those angles, **gobos** are a texture projected from the fixture
in its clip space. This extends the `Fixture` struct (`pan`/`tilt`/`beam_angle` already exist;
add `zoom`, `penumbra`, `gobo_id`, `gobo_angle`, `focus`):

```wgsl
// cone mask (LearnOpenGL smooth-edge spotlight):
let cosA = dot(normalize(samplePos - lightPos), spotDir);
let cone = clamp((cosA - cosOuter) / (cosInner - cosOuter), 0.0, 1.0);
// zoom maps DMX zoom → angles; penumbra softens the edge:
//   cosInner = cos(zoomHalfAngle*(1−penumbra));  cosOuter = cos(zoomHalfAngle)
// gobo: project the sample into the fixture's clip space and sample the cookie:
let p   = lightViewProj * vec4(samplePos, 1.0);
let guv = (rotate(goboAngle) * (p.xy / p.w)) * 0.5 + 0.5;
let gobo = textureSample(goboTex, goboSamp, guv).r;
contribution *= cone * gobo;
```

The cone mask multiplies *both* the analytic airlight and the raymarched in-scatter; the gobo
modulates only the raymarched/froxel-injected path (it needs the per-sample projective
lookup). Rotating gobos/prisms: a single projected cookie suffices for v1; prisms (multiple
beam copies) would need multiple projective samples per step — a cost multiplier to defer.

### 4.5 Analytic shadowless airlight (cheapest tier, Sun et al. 2005)

For fixtures too small/far for raymarch, add a **closed-form glow** with no marching at all:
the in-scatter integral for a point source in homogeneous media reduces to a precomputed 2D
special function `F(u,v)` stored as a texture; each pixel is a few ALU ops + one `F` lookup,
parameterized by source intensity, `σ_t (β)`, light-to-viewer distance, and the angle `γ`
between view ray and light direction. It assumes isotropic emission, so multiply by the cone
mask separately. This scales to thousands of fixtures essentially for free and is the right
fallback for distant/minor beams.

---

## 5. Procedural density: animated world-space haze

Sample density in **world space** (camera-independent) so the haze doesn't swim as the
operator orbits; bound everything to the fog box AABB.

**FBM** is the base field (IQ, confirmed verbatim — gain `G = exp2(−H)`, `H` the Hurst
roughness exponent, `H≈1` smooth/natural, `H≈0` rough/pink). 4–6 octaves for haze:

```wgsl
fn fbm(x: vec3<f32>, H: f32) -> f32 {
    let G = exp2(-H);  var f = 1.0;  var a = 1.0;  var t = 0.0;
    for (var i = 0; i < OCTAVES; i++) { t += a * noise(f * x); f *= 2.0; a *= G; }
    return t;
}
```

**Curl-noise advection** (Bridson 2007) gives the slow rolling/curling motion real fog has,
incompressibly (no sources/sinks), without a fluid sim: build a 3D vector potential `ψ` from
three decorrelated FBM fields and take its curl — `v = ∇×ψ`, and `div(∇×ψ) ≡ 0`. For glowstone,
advect just the *noise-sample offset* by `v·t` (whole-volume coherent, cheap) rather than
simulating particles. Ramp `ψ` to zero at the fog-box walls so flow stays tangent.

```
v_x = ∂ψ3/∂y − ∂ψ2/∂z;  v_y = ∂ψ1/∂z − ∂ψ3/∂x;  v_z = ∂ψ2/∂x − ∂ψ1/∂y
central diff: ∂ψ/∂x ≈ (ψ(p+εx) − ψ(p−εx)) / (2ε),  ε ≈ 1e-3..1e-2 m
```

**Domain warp** (IQ) cheaply turns isotropic noise into billowing, filament-like smoke:
`fbm(p + 4·fbm(p + 4·fbm(p)))` — replace `f(p)` with `f(p + h(p))`.

**Physical parameters** (give the artist a "Visibility (m)" slider, not an arbitrary density):
Koschmieder relates extinction to meteorological visibility `σ_t = 3.912 / V` (contrast
threshold 0.02). Light theatrical haze `V≈50 m → σ_t≈0.078 /m`; heavy `V≈10 m → σ_t≈0.39 /m`.
Defaults that "just look right": `σ_t = 3.912/V × density(p)`, `albedo ρ ≈ 0.95`, `g ≈ 0.7`.
These map onto `Environment.{density, color, anisotropy}` already in the repo.

**CO2 jets / hazer puffs** are additional `σ_s/σ_t` contributions: ship first as procedural
animated noise injected into specific froxels inside the box; add **atomic particle
voxelization** later (Frostbite/Godot: `atomicAdd(extVolU32, u32(ext*2048))` then `/2048`,
since floats aren't atomically addable — wgpu supports `atomic<u32>` on storage buffers) when
you need discrete directional bursts.

**Anti-banding:** jitter the ray start / froxel sample by **blue-noise** (best static
spatial quality) or **Interleaved Gradient Noise** (best under TAA — every 3×3 block is
low-discrepancy), animated per frame: blue-noise `fract(bn + frame·0.61803398875)`, IGN
`ign(x,y) = fract(52.9829189·fract(0.06711056x + 0.00583715y))` with temporal offset
`+5.588238·(frame%64)`.

---

## 6. Scaling to thousands of fixtures: clustered/Forward+ culling

The single most important scalability decision: **never loop all fixtures per froxel.** Build
a clustered light structure over the view frustum and have each froxel iterate only the
fixtures whose bounding volume reaches its cluster. The froxel grid *is* a cluster structure;
reuse it.

### 6.1 Cluster build (compute pass [0], rebuild on resize/FOV only)

One thread per cluster. Grid `16×9×24` (matches 16:9, 3456 clusters; DOOM used `16×8×24`).
For each cluster: convert its two screen-tile corners to NDC → view-space rays via
`inverseProjection`; compute near/far view-Z planes with the **same exponential formula as the
froxels** (§3.2); intersect the corner rays with those planes (`t = (z − dot(n,A))/dot(n,B−A)`,
`n=(0,0,1)`); store the component-wise min/max as the cluster AABB. Static while projection
and grid are fixed.

### 6.2 Light cull (compute pass [1], per frame)

One thread per cluster (e.g. `local_size 16×9×4 = 576`). Approximate each fixture as a
bounding sphere (point/PAR/wash: position+range; beam/spot: bounding radius of the cone — a
sphere is the simplest conservative proxy; tighten to a cone test later to cut false
positives from narrow moving heads). Stream fixtures through shared memory in batches; test
sphere-vs-AABB and append accepted indices:

```
center_view = (view · lightPosWorld).xyz                  // clusters live in view space
sqDist = Σ_axis ( c<min ? (min−c)² : c>max ? (c−max)² : 0 )
intersects iff sqDist ≤ range²
offset = atomicAdd(globalIndexCount, count);  lightIndexList[offset+i] = idx;
lightGrid[cluster] = vec2<u32>(offset, count);
```

### 6.3 Data layout in wgpu storage buffers

Two viable layouts:

- **List (DOOM/Filament/Ortiz — best documented, start here):** three storage buffers —
  `clusterAABB: array<vec4<f32>>` pairs (built on resize), `lightGrid: array<vec2<u32>>`
  `{offset,count}`, and a flat `lightIndexList: array<u32>` packed via an `atomic<u32>`
  counter. The atomic must live in `var<storage, read_write>` (workgroup atomics are
  separate) — this compaction is the part most likely to have race bugs; test it carefully.
- **Bitmask (Granite — bounded worst case, scalable):** per cluster store
  `array<u32, ceil(N/32)>` bits. At `3456 clusters × 2048 fixtures` that is ~884 KB (trivial),
  removes the atomic-append entirely, and scalarizes well via subgroup ops *if/when* wgpu
  exposes them. Prototype list first, benchmark bitmask if the cull pass divergence/atomics
  bottleneck.

Fixtures themselves: a `array<FixtureGpu>` storage buffer (`pos, range, dir, cone angles,
color, intensity, gobo id, flags`), 16-byte aligned. DOOM allowed 256 lights/cluster; a packed
truss can exceed that in hot clusters — size `lightIndexList` / bitmask width with headroom
and decide overflow behavior (clamp vs grow).

Then in injection (§3.3): derive the cluster from `(froxel.xy, slice)`, read `lightGrid`,
loop only `lightIndexList[offset..offset+count]`. **Cull with the cheap sphere; evaluate the
full cone + gobo + falloff at injection time** for realism. IES/gobo per fixture per froxel
for *thousands* may still be too expensive — LOD it: cheap cone falloff for far/minor
fixtures, full IES/gobo only for near/hero ones.

---

## 7. Phased implementation plan (this repo)

Each phase is independently shippable and visibly better than the last. wgpu-29 features
needed are flagged per phase. **Phase −1 (HDR) is a prerequisite for the look** and is
decoupled from all volumetric work, so do it first or in parallel.

### Phase −1 — HDR offscreen + bloom + tonemap *(no volumetrics; biggest "looks real" lever)*
Change `Viewport::COLOR_FORMAT` from `Rgba8Unorm` to `Rgba16Float`; render scene+beams in
linear light; add a post chain: exposure (`×exp2(EV)`) → threshold-free bloom (13-tap
downsample mip pyramid, Karis `1/(1+luma)` average on the first mip to kill fireflies from
bright beam tips, 3×3 tent upsample, mix ≈0.04) → ACES/Reinhard tonemap → sRGB + ordered/
blue-noise dither → blit into the egui `Rgba8Unorm` texture egui requires.
*wgpu features: `Rgba16Float` as render target (universally supported); a fullscreen-triangle
post pipeline. No compute.* Confirm `register_native_texture` still works (egui wants a
non-sRGB `Rgba8Unorm` view — keep the final tonemapped blit target in that format).

### Phase 0 — single-beam raymarch in the fog box *(quick win, validates the math)*
A fullscreen-triangle fragment pass (reuse the existing forward render-pass structure, no
compute). For *one* selected fixture: clip the view ray to the `Environment` box AABB,
`~32` fixed steps, FBM density (§5), HG phase (§2), Beer–Lambert + energy-conserving accumulation
(§3.4), IGN-jittered ray start. Sample the existing `Depth32Float` to stop at opaque geometry.
Composite `out = scene·Tr + inScatter`. This is the ground-truth reference everything else is
validated against and already looks like a real beam in haze.
*wgpu features: render pipeline + a sampler on the depth texture (add `TEXTURE_BINDING` usage
to the viewport depth target). No compute, no storage textures.*

### Phase 1 — multi-beam + per-beam shadows + gobo/cone (Tier B core)
Loop all fixtures in the Phase 0 pass (or one additive pass per beam). Add per-fixture cone
mask + zoom + projected gobo (§4.4); add per-hero-fixture shadow maps (render lazily, cache,
re-render only when pan/tilt/zoom/position changes — most are static between DMX frames). Add
the LOD selection (§4.1). Render at half-res + bilateral upsample.
*wgpu features: extra depth render targets (shadow atlas); storage buffer of fixtures
(`array<FixtureGpu>`); texture array for gobos. Still no compute required.*

### Phase 2 — froxel grid: inject + integrate (Tier A core) — **first compute work**
The three `rgba16float` 3D textures (§3.1); compute pass [2]/[3] inject (one thread per
froxel, `@workgroup_size(8,8,1)` like diharaw, or `4,4,4`), compute pass [4] integrate (one
thread per XY column). Port diharaw's two shaders as the skeleton; upgrade depth mapping to
the exponential form (§3.2) anchored to the fog box. Composite the integrated volume in the
forward fragment shader (§3.6). Validate against the Phase 0 reference. Initially inject from
a *small fixed list* of fixtures (no culling yet) for the global haze fill; keep the hero
beams on Tier B.
*wgpu features: **compute pipelines** (greenfield); `texture_storage_3d<rgba16float, write>`
+ sampled `texture_3d<f32>` reads (ping-pong); confirm adapter exposes `rgba16float` storage.
Workgroup sizes as above.*

### Phase 3 — temporal reprojection + jitter
Add `vol_history`; Halton sub-froxel jitter per frame; reproject by previous view-projection;
5% EMA blend on the *linear* injected volume; skip history outside the previous frustum. Tune
neighborhood clamp against a console chase to control moving-head ghosting (§3.5).
*wgpu features: a second 3D texture for history (ping-pong); previous-frame camera uniform.*

### Phase 4 — clustered/Forward+ culling feeding froxel injection
Compute pass [0] cluster build (resize-only), pass [1] per-frame light cull writing
`lightGrid` + `lightIndexList` via `atomic<u32>` (§6). Injection reads the cluster list. This
is what unlocks hundreds-to-thousands of fixtures.
*wgpu features: `atomic<u32>` in `var<storage, read_write>` storage buffers; a second compute
dispatch. (Subgroups for the bitmask variant are behind a wgpu feature flag and not assumed.)*

### Phase 5 — polish & scale
Volumetric self-shadow in injection (cheap froxel-march, §4.3); curl-noise advection + domain
warp (§5); CO2 jet particle voxelization via `atomicAdd` (§5); analytic airlight LUT for the
far/minor-fixture tier (§4.5); dual-lobe HG mode; cone cull test replacing sphere; epipolar /
1D min-max acceleration if a dominant wash dominates the frame.

---

## 8. Pitfalls

- **HG sign convention** — the #1 bug. Verify a beam aimed *at* the camera brightens. (§2)
- **Naive scattering accumulation** (`S·dz`) under-lights thick far froxels — use the
  analytic `(S − S·Tr)/σ_t` everywhere. (§3.4)
- **Storing transmittance (non-linear) instead of extinction in temporally-accumulated
  volumes** biases the EMA and flickers beams. Store `σ_t`. (§3.1)
- **No temporal/jitter from the start** — anisotropic beams on a frustum-aligned grid strobe
  on camera rotation. It is not optional polish. (§3.5)
- **Density in view space** makes haze swim as the camera orbits — sample in world space. (§5)
- **Hard fog-box edges** show a rectangular cutoff in beams — soft-edge the box in the
  density function (smoothstep over distance to walls), don't hard-clip.
- **LDR target clips bright beams to flat white**, losing the forward-scatter glow — Phase −1
  HDR + bloom is a prerequisite, not an afterthought.
- **wgpu 29 storage-texture caveats**: read-write storage textures and subgroups are not
  universally available; design for ping-pong + the list (atomic) cull layout first, and
  verify `rgba16float` `STORAGE_BINDING` on the target adapters (Metal/Vulkan/DX12). Falling
  back to a flat storage buffer + manual 3D indexing is always available.
- **Shadow-map budget** is the real cost driver for sharp gobo shafts — only hero fixtures
  need real depth maps; atlas + lazy-update-on-DMX-change for the rest; prototype how many fit
  in 16 ms before committing.
- **HDR dynamic range vs `rgba16float`** for thousands-of-lumens fixtures may band/clamp —
  consider pre-exposure before storing in the volume.
- **No in-repo ground truth** — validate the analytic airlight + froxel inject against the
  Phase 0 brute-force march to tune `σ_t / σ_s / g` defaults that look like real haze.
- **Multiple scattering is ignored** by single-scattering — for thick CO2 an ambient/SH term
  scaled by albedo, or dual-lobe HG, may be needed; decide if "good enough" for glowstone.

---

## 9. References

All URLs below were reported verified by the specialists and/or directly re-checked during
synthesis. Items explicitly flagged *unverified* are kept for completeness only — do not cite
them as primary.

### Froxel pipeline (the backbone)
- **Wronski, *Volumetric Fog* (SIGGRAPH 2014 Advances)** — the originating froxel method;
  `160×90×64/128`, exponential depth, two-pass inject+integrate, HG, temporal as future work.
  <https://bartwronski.com/wp-content/uploads/2014/08/bwronski_volumetric_fog_siggraph2014.pdf>
  · publications/GPU Pro 6 chapter: <https://bartwronski.com/publications/>
  · reference impl (C#/HLSL): <https://github.com/bartwronski/CSharpRenderer>
- **Hillaire, *Physically Based and Unified Volumetric Rendering in Frostbite* (SIGGRAPH 2015)**
  — the energy-conserving analytic slice integral `(S−S·e^(−σ_t·D))/σ_t`, two-RGBA16F V-buffer,
  Halton jitter + 5% EMA, particle voxelization, volumetric shadow clip-map.
  Official: <https://www.ea.com/frostbite/news/physically-based-unified-volumetric-rendering-in-frostbite>
  · course page: <https://advances.realtimerendering.com/s2015/>
  · deck (verbatim slides): <https://www.slideshare.net/slideshow/physically-based-and-unified-volumetric-rendering-in-frostbite/51840934>
- **Patapom, *Real-time Volumetric Rendering* course notes (Revision 2013)** — clean
  single-scattering RTE derivation, Beer–Lambert, HG/Schlick, concrete front-to-back loop.
  <https://patapom.com/topics/Revision2013/Revision%202013%20-%20Real-time%20Volumetric%20Rendering%20Course%20Notes.pdf>

### Phase function & math
- **PBR-Book, *Phase Functions*** — authoritative HG definition (note the `+2g·cosθ` convention).
  4th ed: <https://pbr-book.org/4ed/Volume_Scattering/Phase_Functions>
  · 3rd ed: <https://pbr-book.org/3ed-2018/Volume_Scattering/Phase_Functions>
- **Henyey–Greenstein phase function (Wikipedia)** — standard `−2g·cosθ` form, double-HG.
  <https://en.wikipedia.org/wiki/Henyey%E2%80%93Greenstein_phase_function>
- **Schlick phase approx** `k=1.55g−0.55g³` (Shadertoy / PBRT-cited): <https://www.shadertoy.com/view/4ltGWl>

### Beam / single-scattering / light shafts
- **Sun, Ramamoorthi, Narasimhan, Nayar, *A Practical Analytic Single Scattering Model*
  (SIGGRAPH 2005)** — closed-form airlight, precomputed `F` table.
  <http://www.cs.cmu.edu/~ILIM/publications/PDFs/SRNN-SIGGRAPH05.pdf>
  · re-derivation (readable equations): <https://www.scitepress.org/PublishedPapers/2011/33739/pdf/index.html>
- **Engelhardt & Dachsbacher, *Epipolar Sampling* (I3D 2010)** — march only at depth breaks
  along epipolar lines. <https://www.semanticscholar.org/paper/Epipolar-sampling-for-shadows-and-crepuscular-rays-Engelhardt-Dachsbacher/2b9640253af2bf397232f2436eddce2cf1d5f1eb>
- **Chen, Baran, Durand, Jarosz, *Real-Time Volumetric Shadows using 1D Min-Max Mipmaps*
  (2011)** — skip fully-lit/fully-shadowed ray segments. <https://groups.csail.mit.edu/graphics/mmvs/mmvs.pdf>
- **Mitchell, *Volumetric Light Scattering as a Post-Process* (GPU Gems 3, ch.13)** — cheap
  screen-space radial god-rays. <https://developer.nvidia.com/gpugems/gpugems3/part-ii-light-and-shadows/chapter-13-volumetric-light-scattering-post-process>
- **LearnOpenGL, *Light Casters*** — smooth-edge spotlight cone math.
  <https://learnopengl.com/Lighting/Light-casters>
- **Maxime Heckel, volumetric raymarching + post-processing** — WGSL-portable per-step shadow
  test, spot cone, blue-noise jitter, HG.
  <https://blog.maximeheckel.com/posts/shaping-light-volumetric-lighting-with-post-processing-and-raymarching/>
  · clouds: <https://blog.maximeheckel.com/posts/real-time-cloudscapes-with-volumetric-raymarching/>

### Clustered / Forward+ (scaling lights)
- **Olsson, Billeter, Assarsson, *Clustered Deferred and Forward Shading* (HPG 2012)** — the
  exponential view-Z subdivision + cluster keys. PDF is binary/large and did not text-extract
  during synthesis, but is reachable and canonical; equations corroborated by the secondary
  sources below. <https://www.cse.chalmers.se/~uffe/clustered_shading_preprint.pdf>
  · EG record: <https://diglib.eg.org/items/6342d4d6-5220-4376-a5c6-a153058f4a3c>
- **Angel Ortiz, *A Primer on … Clustered Shading*** — exact exp z-slice formula, AABB build,
  offset/count light-grid layout. <http://www.aortiz.me/2018/12/21/CG.html>
  · companion code: <https://github.com/Angelo1211/HybridRenderingEngine>
- **DaveH355/clustered-shading** — fully-commented OpenGL tutorial; `fragment zTile = log(z/zNear)·gridZ/log(zFar/zNear)`, inline-indices variant. <https://github.com/DaveH355/clustered-shading>
- **Courrèges, *DOOM (2016) Graphics Study*** — `16×8×24` clusters, log depth, indexed item lists. <https://www.adriancourreges.com/blog/2016/09/09/doom-2016-graphics-study/>
- **Filament `Froxelizer.cpp`** — production froxel/record-buffer layout, `linearizer = log2(zFar/zNear)/(N−1)`. <https://raw.githubusercontent.com/google/filament/main/filament/src/Froxelizer.cpp>
- **Arntzen, *Clustered shading evolution in Granite*** — list vs bitmask trade-off, subgroup scalarization. <https://themaister.net/blog/2020/01/10/clustered-shading-evolution-in-granite/>

### Procedural density / noise / HDR
- **Inigo Quilez** — *fBM* (`G=exp2(−H)`, verified): <https://iquilezles.org/articles/fbm/>
  · *Domain Warping*: <https://iquilezles.org/articles/warp/> · *Fog*: <https://iquilezles.org/articles/fog/>
- **Bridson, Hourihan, Nordenstam, *Curl-Noise for Procedural Fluid Flow* (SIGGRAPH 2007)** —
  divergence-free `v=∇×ψ`. <https://www.cs.ubc.ca/~rbridson/docs/bridson-siggraph2007-curlnoise.pdf>
  · readable derivation of the 3D curl components: <https://freder.github.io/UnityGraphicsProgrammingBook1/html-translated/vol2/Chapter%206%20_%20Curl%20Noise-Explanation%20of%20Noise%20Algorithms%20for%20Pseudo-Fluids.html>
- **Demofox (Alan Wolfe)** — *Ray Marching Fog With Blue Noise*: <https://blog.demofox.org/2020/05/10/ray-marching-fog-with-blue-noise/>
  · *Interleaved Gradient Noise*: <https://blog.demofox.org/2022/01/01/interleaved-gradient-noise-a-different-kind-of-low-discrepancy-sequence/>
- **Scratchapixel, *Ray Marching — Get it Right!*** — discrete front-to-back transmittance accumulation. <https://www.scratchapixel.com/lessons/3d-basic-rendering/volume-rendering-for-developers/ray-marching-get-it-right.html>
- **Christensen, *Physically Based Bloom* (LearnOpenGL)** — 13-tap down / 3×3 tent up, Karis average. <https://learnopengl.com/Guest-Articles/2022/Phys.-Based-Bloom>
- **Kim et al., *Extinction coefficient and visibility in fog* (Koschmieder, `σ=3.912/V`)** — physical haze defaults. <https://opg.optica.org/ao/abstract.cfm?uri=ao-44-18-3795>

### Open-source implementations to port from
- **diharaw/volumetric-fog** (GLSL compute) — smallest end-to-end froxel pipeline; verified
  `local_size 8×8×1`, `rgba16f`, `Sint = scattering·(1−T)/density`, `accumScatter += Sint·accumTr; accumTr *= T`. **Port `light_injection_cs.glsl` + `ray_march_cs.glsl` first.** <https://github.com/diharaw/volumetric-fog>
- **Bevy `bevy_pbr/volumetric_fog`** (Rust + WGSL — closest stack match) — valid WGSL HG
  (`FRAC_4_PI`), IGN jitter, per-step `exp(−step·density·(abs+scat))`, 3D density texture,
  dir/point/spot shadow sampling; `render.rs`/`mod.rs` show wgpu bind-group plumbing. <https://github.com/bevyengine/bevy/tree/main/crates/bevy_pbr/src/volumetric_fog>
- **Godot `volumetric_fog_process.glsl` (4.2)** — production froxel: `detail_spread` exp depth,
  Halton-16 + `to_prev_view` reprojection, multi-light injection, atomic float-as-int density. <https://github.com/godotengine/godot/blob/4.2/servers/rendering/renderer_rd/shaders/environment/volumetric_fog_process.glsl>
  · *Fog Volumes arrive in Godot 4* (64³ default, box fog volumes): <https://godotengine.org/article/fog-volumes-arrive-in-godot-4/>
- **WickedEngine `volumetricLight_SpotPS.hlsl`** — per-spotlight beam raymarch (16 samples,
  dither, `attenuation_spotlight`+shadow+cookie, `ComputeScattering` HG) — a stage moving-head
  almost exactly. <https://github.com/turanszkij/WickedEngine/blob/master/WickedEngine/shaders/volumetricLight_SpotPS.hlsl>
- **SlightlyMad/VolumetricLights** (Unity, Killzone-style) — per-light cone/sphere mesh
  raymarch, light cookies (=gobos), half/quarter-res + bilateral upscale. <https://github.com/SlightlyMad/VolumetricLights>
- **GameTechDev/LightScattering** (Intel) — epipolar + 1D min/max trees extended to spot/point
  (no closed-form spot in-scatter, uses a LUT) — closest published prior art for fixture beams. <https://github.com/GameTechDev/LightScattering>
  · maintained cross-API descendant: <https://github.com/DiligentGraphics/DiligentFX/tree/master/PostProcess/EpipolarLightScattering>
- **NogginBops/DD2470_Clustered_Volume_Renderer** — readable from-scratch clustered volumetric fog to cross-check GLSL. <https://github.com/NogginBops/DD2470_Clustered_Volume_Renderer>
- **threex.volumetricspotlight** — cheapest fake-cone LOD (`pow(dot(n,axis),anglePower)` edge falloff), for far/many fixtures. <https://github.com/jeromeetienne/threex.volumetricspotlight>
- **Ameobea/three-good-godrays** — compact screen-space godray raymarch + blue-noise dither + bilateral denoise. <https://github.com/Ameobea/three-good-godrays>
- **Azkellas/rust_wgpu_hot_reload** — WGSL hot-reload + `#import` + egui + raymarch demo; ideal iteration harness for the shaders above. <https://github.com/Azkellas/rust_wgpu_hot_reload>

*Flagged uncertain (do not cite as primary): cheneyshen.com Frostbite walkthrough — refused
connection during research; corroborated only via the official SlideShare deck.*
