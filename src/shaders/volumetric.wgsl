// volumetric.wgsl — single-scattering raymarch of fixture beams through haze
// (Phase 0/1 of docs/RESEARCH-volumetrics.md), rendered at reduced resolution.
//
// A fullscreen pass: reconstruct each pixel's world-space view ray, clip it to
// the fog-box AABB and the opaque depth, then march front-to-back summing
// in-scattered light. Each fixture is a *disc* light (starts at the lens
// radius, widens with the beam angle) so the beam is a truncated cone, not a
// point cone. Haze density comes from a precomputed tiling 3D noise texture
// (cheap, high quality) scrolled over time; a small ambient term makes the
// smoke visible everywhere, with the beams lighting it brightly. Output is
// (in-scatter.rgb, transmittance); a later composite blends it over the scene.

struct Volumetric {
    inv_view_proj: mat4x4<f32>,
    eye_time: vec4<f32>,        // xyz = eye, w = time (s)
    fog_min_density: vec4<f32>, // xyz = box min, w = base density (sigma_t)
    fog_max_g: vec4<f32>,       // xyz = box max, w = anisotropy g
    albedo_beam: vec4<f32>,     // rgb = scattering tint, w = beam intensity
    counts: vec4<f32>,          // y = max step count, z = constant-dt target (world m)
    chroma: vec4<f32>,          // x = Helmholtz–Kohlrausch chroma read-up strength; yzw reserved
    tile: vec4<f32>,            // x = tiles_x, y = tiles_y, z = tile size (full-res px), w = CO2 quality (<0 no CO2)
    co2_amb: vec4<f32>,         // rgb = ambient room colour the white CO2 reflects, w = strength
};

struct Fixture {
    pos_range: vec4<f32>, // xyz = lens position, w = range (m)
    dir_cos: vec4<f32>,   // xyz = beam dir (unit), w = tan(half zoom angle)
    color: vec4<f32>,     // rgb = tint*intensity*candela*shutter, w = lens radius (m)
    cookie_r: vec4<f32>,  // xyz = lens-plane right, w = wheel buffer offset
    cookie_u: vec4<f32>,  // xyz = lens-plane up,    w = wheel count (dynamic chain)
    extra: vec4<f32>,     // x = anim layer (<0 none), y = anim scroll; z/w = shutter (close,kind) — or, on a PLAIN cell, z = -1 sentinel + w = HDR whiteness
    shape: vec4<f32>,     // x = super-Gaussian order, y = focus dist, z = iris frac, w = frost
    misc: vec4<f32>,      // x = CA strength, y = laser flag, z = atlas count, w = shadow layer
    cmyf: vec4<f32>,      // CMY flag insertions c,m,y, unused
};

@group(0) @binding(0) var<uniform> u: Volumetric;
@group(0) @binding(1) var<storage, read> fixtures: array<Fixture>;
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var noise_tex: texture_3d<f32>;
@group(0) @binding(4) var noise_samp: sampler;
@group(0) @binding(5) var gobo_tex: texture_2d_array<f32>;
@group(0) @binding(6) var gobo_samp: sampler;
// Hero-beam shadow maps (a beam with misc.w >= 0 has a layer here) — so the beam
// shaft is occluded mid-air where geometry blocks the light.
@group(0) @binding(7) var shadow_atlas: texture_depth_2d_array;
@group(0) @binding(8) var shadow_samp: sampler_comparison;
@group(0) @binding(9) var<storage, read> shadow_mats: array<mat4x4<f32>>;
// Per-fixture wheel chain (dynamic count); each fixture indexes a [offset,count)
// slice via cookie_r.w / cookie_u.w.
@group(0) @binding(10) var<storage, read> wheels: array<WheelGpu>;
// Tiled light culling: per-screen-tile CSR light lists, SHARED with the mesh pass.
// One ray = one screen tile, so the slice is fetched once and reused for every march
// sample (the whole saving amortizes over the march).
@group(0) @binding(11) var<storage, read> tile_offsets: array<u32>;
@group(0) @binding(12) var<storage, read> tile_lights: array<u32>;
// Stage CO2: a real particle SMOKE SIM (particles.rs) splats per-emitter density
// into co2_density (one CO2_GRID-tall Z-slab per emitter, stacked). Each Co2Volume
// maps a world sample into its grid; the density is added to the haze so the beams
// scatter through the SIMULATED plume (mushroom shape, churn + thickness all from
// the sim — the shader no longer fakes any of it).
struct Co2Volume {
    box_min: vec4<f32>,  // xyz = grid AABB min (world m), w = σ-scale (density → extinction)
    box_size: vec4<f32>, // xyz = grid AABB size (world m), w = texture Z-slab layer index
};
@group(0) @binding(13) var<storage, read> co2_vols: array<Co2Volume>;
@group(0) @binding(14) var co2_density: texture_3d<f32>; // R8, GRID³ per layer stacked in Z
@group(0) @binding(15) var co2_density_samp: sampler;
const CO2_LAYERS_F: f32 = 8.0;

const PI: f32 = 3.14159265359;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vid: u32) -> VsOut {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let xy = corners[vid];
    var out: VsOut;
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.ndc = xy;
    return out;
}

// Exact Henyey–Greenstein phase (forward peak at cosθ=+1 with the −2g·cosθ
// convention, cosθ = dot(bdir, −rd)). We tried the Schlick approximation
// k=1.55g−0.55g³, but k crosses 1 at |g|≳0.93 (the anisotropy slider reaches
// ±0.95), where (1−k²) flips negative and the forward lobe inverts to backscatter
// — a visible blow-up at exactly the sharp-beam setting users crank toward. Keep
// the exact form, but compute denom^1.5 as denom*sqrt(denom) instead of generic pow.
fn hg(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = max(1.0 + g2 - 2.0 * g * cos_theta, 1e-4);
    return (1.0 - g2) / (4.0 * PI * denom * sqrt(denom));
}

// Interleaved gradient noise — per-pixel ray-start jitter (kills banding).
fn ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * p.x + 0.00583715 * p.y));
}

// Haze density factor (0 = clear air) from the tiling 3D noise texture, two
// scrolling scales for structure across the whole size range, high-contrast
// remap so the beam reveals clear pockets and dense wisps.
fn density_at(p: vec3<f32>, t: f32) -> f32 {
    if (u.chroma.z >= 0.999) {
        return 1.0;
    }
    let wind1 = vec3<f32>(0.10, 0.020, 0.06) * t;
    let wind2 = vec3<f32>(-0.06, 0.015, 0.05) * t;
    let wind3 = vec3<f32>(0.04, -0.03, 0.08) * t;
    // FINE scales (2–5 m) are what actually READ as smoke structure — this is the original
    // high-contrast haze field that showed strong wisps. (Biasing to coarse scales to hide
    // the old "beads" also hid all the structure; the right answer is to keep the fine field
    // and let UNIFORMITY collapse its contrast instead.)
    let n1 = textureSampleLevel(noise_tex, noise_samp, p * 0.08 + wind1, 0.0).r;
    let n2 = textureSampleLevel(noise_tex, noise_samp, p * 0.21 + wind2, 0.0).r;
    let n3 = textureSampleLevel(noise_tex, noise_samp, p * 0.46 + wind3, 0.0).r;
    let n = n1 * 0.5 + n2 * 0.32 + n3 * 0.18;
    // Cluster mask (0 clear … 1 dense pocket).
    let cluster = smoothstep(0.26, 0.64, n);
    // Cluster CONTRAST (chroma.w, 0..1) is an OPTIONAL push: at 0 it reproduces the liked
    // baseline look exactly (0.16 + cluster·2.1); higher widens the dense-vs-clear ratio so
    // pockets read brighter against the haze (near-clear gaps + much denser, sparser pockets).
    let contrast = clamp(u.chroma.w, 0.0, 1.0);
    let lo = mix(0.16, 0.015, contrast); // gaps thin toward near-clear
    let hi = mix(2.26, 22.0, contrast);  // pockets get DENSE → genuinely bright wisps
    let pk = pow(cluster, mix(1.0, 2.2, contrast)); // higher contrast → sparser, discrete pockets
    let structured = mix(lo, hi, pk);
    // Uniformity (chroma.z): 1 = perfectly smooth even haze; 0 = full clustered smoke.
    // Crossfade flat↔structured. The temporal history cap (mod.rs) also drops with
    // uniformity so the clusters stay crisp + drifting instead of being averaged away.
    let clump = 1.0 - clamp(u.chroma.z, 0.0, 1.0);
    return mix(1.0, structured, clump);
}

// CO2 density at `p` from the splatted smoke-sim grids: x = extinction σₜ summed
// over emitters, y = max raw grid density (0..1, drives the extra core darkening).
// Each emitter's grid is one Z-slab of co2_density; layer L occupies normalized w
// in [L/LAYERS, (L+1)/LAYERS). A half-texel inset stops bilinear bleed between
// stacked layers. The SHAPE (mushroom, churn, thickness) is entirely the sim's; the
// fine CAULIFLOWER detail is added on top in `co2_density_at` below.
fn co2_base_at(p: vec3<f32>) -> vec2<f32> {
    var sig = 0.0;
    var dmax = 0.0;
    let n = arrayLength(&co2_vols);
    // Half-texel inset along stacked Z (texture depth = res·layers) so linear
    // filtering can't bleed between adjacent emitters' Z-slabs. Uses the live
    // texture size, so it tracks the dynamic (quality-driven) resolution.
    let inset = 0.5 / f32(textureDimensions(co2_density).z);
    for (var i = 0u; i < n; i = i + 1u) {
        let v = co2_vols[i];
        if (v.box_size.x <= 0.0) { continue; }
        let uvw = (p - v.box_min.xyz) / v.box_size.xyz;
        if (any(uvw < vec3<f32>(0.0)) || any(uvw > vec3<f32>(1.0))) { continue; }
        let wz = (v.box_size.w + clamp(uvw.z, inset, 1.0 - inset)) / CO2_LAYERS_F;
        let d = textureSampleLevel(co2_density, co2_density_samp, vec3<f32>(uvw.x, uvw.y, wz), 0.0).r;
        sig = sig + d * v.box_min.w;   // density → extinction
        dmax = max(dmax, d);
    }
    return vec2<f32>(sig, dmax);
}

// CO2 density WITH fine cauliflower detail. The splatted grid is a SMOOTH blob (so
// no grid resolution alone yields crisp edges); here a multi-octave noise CARVES it
// into lumpy wisps with sharp boundaries — the core stays solid, the rim gets eaten
// into the ragged cauliflower silhouette the reference shows. Drifts upward + churns
// over time. Returns (extinction σₜ, carved density 0..1). x = sigma, y = core mask.
fn co2_density_at(p: vec3<f32>, t: f32) -> vec2<f32> {
    // Quick reject far from any plume (skip the warp noise). The grid's soft Gaussian
    // edge keeps near-edge points nonzero, so the warp still has density to displace.
    if (co2_base_at(p).x <= 1e-5) {
        return vec2<f32>(0.0, 0.0);
    }
    let scroll = vec3<f32>(0.0, -t * 0.10, 0.0);
    // DOMAIN WARP — the key to a lumpy cauliflower SILHOUETTE. The grid is a smooth
    // Gaussian blob, so its edge is a smooth funnel no matter the resolution or how
    // much you modulate the interior (multiplying density can't move the boundary
    // outward, only erode it inward — which deletes faint plumes). Instead, sample
    // the grid at a NOISE-DISPLACED position: the smooth boundary is pushed in AND out
    // into ragged florets, density is preserved (no erosion → faint plumes survive),
    // and it churns as the field drifts. ~0.4 m coarse warp + a finer one for florets.
    let q = p * 0.42 + scroll;
    let wx = textureSampleLevel(noise_tex, noise_samp, q, 0.0).r;
    let wy = textureSampleLevel(noise_tex, noise_samp, q + vec3<f32>(11.3, 2.1, 5.1), 0.0).r;
    let wz = textureSampleLevel(noise_tex, noise_samp, q + vec3<f32>(3.7, 7.7, 13.9), 0.0).r;
    let q2 = p * 1.05 + scroll * 1.6;
    let fx = textureSampleLevel(noise_tex, noise_samp, q2, 0.0).r;
    let fy = textureSampleLevel(noise_tex, noise_samp, q2 + vec3<f32>(9.2, 4.4, 1.3), 0.0).r;
    let fz = textureSampleLevel(noise_tex, noise_samp, q2 + vec3<f32>(2.6, 8.1, 6.5), 0.0).r;
    let warp = (vec3<f32>(wx, wy, wz) - 0.5) * 0.85   // ±0.42 m coarse florets
             + (vec3<f32>(fx, fy, fz) - 0.5) * 0.34;  // ±0.17 m fine bumps
    let base = co2_base_at(p + warp);
    if (base.x <= 1e-5) {
        return base;
    }
    // Mild interior modulation (NOT an erosion) for light/dark variation within.
    let mvar = mix(0.78, 1.42, smoothstep(0.34, 0.66, fx * 0.5 + wy * 0.5));
    return vec2<f32>(base.x * mvar, base.y * mvar);
}

@fragment
fn fs_volumetric(in: VsOut) -> @location(0) vec4<f32> {
    let ndc = in.ndc;

    // Reconstruct the world-space view ray.
    let near_h = u.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let far_h = u.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let w_near = near_h.xyz / near_h.w;
    let w_far = far_h.xyz / far_h.w;
    let ro = u.eye_time.xyz;
    let rd = normalize(w_far - w_near);

    // Opaque depth: sample the full-res depth at this NDC. Because the pass is
    // half-res, take the NEAREST (min) depth over the ray's full-res footprint
    // — so the ray stops at the closest surface any of its pixels see and the
    // beam never bleeds past the floor at grazing angles.
    let dims = vec2<f32>(textureDimensions(depth_tex));
    let duv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let dpix = clamp(vec2<i32>(duv * dims), vec2<i32>(0, 0), vec2<i32>(dims) - vec2<i32>(2, 2));
    let d = min(
        min(textureLoad(depth_tex, dpix, 0), textureLoad(depth_tex, dpix + vec2<i32>(1, 0), 0)),
        min(textureLoad(depth_tex, dpix + vec2<i32>(0, 1), 0), textureLoad(depth_tex, dpix + vec2<i32>(1, 1), 0)),
    );
    var t_surface = 1e9;
    if (d < 0.999999) {
        let surf_h = u.inv_view_proj * vec4<f32>(ndc, d, 1.0);
        let surf = surf_h.xyz / surf_h.w;
        t_surface = length(surf - ro);
    }

    // Intersect the fog-box AABB (robust slab test; inf from /0 is fine).
    let bmin = u.fog_min_density.xyz;
    let bmax = u.fog_max_g.xyz;
    let inv = 1.0 / rd;
    let ta = (bmin - ro) * inv;
    let tb = (bmax - ro) * inv;
    let tsmall = min(ta, tb);
    let tbig = max(ta, tb);
    let t_near = max(max(tsmall.x, tsmall.y), max(tsmall.z, 0.0));
    let t_far = min(min(tbig.x, tbig.y), min(tbig.z, t_surface));

    if (t_far <= t_near) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0); // no media: scene passes through
    }

    let max_steps = max(i32(u.counts.y), 1);
    // Tiled light culling: this ray's screen tile, fetched ONCE and reused for every
    // march sample. in.pos.xy is the half-res frag coord; ×2 lifts it onto the full-res
    // tile grid the mesh pass uses, so the floor pool and the beam shaft march the
    // identical beam subset (lock-step). u.tile = (tiles_x, tiles_y, tile_px, _).
    let v_tpx = max(u32(u.tile.z), 1u);
    let v_txn = max(u32(u.tile.x), 1u);
    let v_tyn = max(u32(u.tile.y), 1u);
    let v_tx = min(u32(in.pos.x * 2.0) / v_tpx, v_txn - 1u);
    let v_ty = min(u32(in.pos.y * 2.0) / v_tpx, v_tyn - 1u);
    let v_tile = v_ty * v_txn + v_tx;
    let v_lo = tile_offsets[v_tile];
    let v_hi = tile_offsets[v_tile + 1u];
    let g = clamp(u.fog_max_g.w, -0.95, 0.95); // keep HG well-conditioned at extremes
    let base = u.fog_min_density.w;
    let albedo = u.albedo_beam.rgb;
    let beam = u.albedo_beam.w;
    let time = u.eye_time.w;
    // Very dim ambient haze; beams do the lighting. In hybrid mode the froxel
    // volume already supplies the ambient term, so the hero-only raymarch omits it
    // (else it double-counts where the two composites overlap).
    let ambient = select(albedo * 0.012, vec3<f32>(0.0), u.counts.x < -1.5);

    let seg = t_far - t_near;

    // Constant-dt step cap (1.4): scale the step count to the clipped segment so a
    // ray skimming a thin slice of the fog box doesn't pay the full budget. The
    // CPU supplies target_dt = box_diagonal / steps (world m) in counts.z; keeping
    // dt (not the count) roughly constant gives equal per-metre sampling on every
    // ray — no seam between short and long rays.
    var nsteps = max_steps;
    let target_dt = u.counts.z;
    if (target_dt > 1e-4) {
        nsteps = clamp(i32(round(seg / target_dt)), 8, max_steps);
    }
    let fn_steps = f32(nsteps);

    // Uniform spacing within the (constant-dt-capped) budget. We tried front-
    // loading samples exponentially toward the fog-box entry, but in this renderer
    // the sharp aerial detail (gobo cross-section, CA fringe) is spread along the
    // WHOLE beam in fixture space, not near the camera — so a camera-anchored bias
    // starves the far beam and aliases gobo structure into longitudinal stripes.
    // Equal spacing + per-pixel jitter is the robust choice; the count reduction
    // (and the constant-dt cap above) is where the speed comes from.
    let dt = seg / fn_steps;
    // Ray-start offset: ANIMATED interleaved-gradient-noise (Jiménez, COD:AW) — a
    // spatiotemporal blue-noise the cheap way. `chroma.y` carries the frame index (mod 64);
    // offsetting the IGN input by `frame·5.588238` gives a FRESH blue-noise pattern each
    // frame whose per-pixel sequence is well distributed over 64 frames. Why this beats the
    // old screen-coherent golden-ratio phase: that phase was the SAME for every pixel, so
    // the whole step-band pattern shifted RIGIDLY each frame → it read as coherent FLICKER
    // (and bands, since per-pixel variation was only 6%) even with a still camera. Blue
    // grain instead is integrated by the eye AND dissolved by the EMA when static (blue
    // along time → resolves in ~8 frames), so banding/flicker → faint stable grain. At
    // half-res each IGN sample already spans a 2×2 full-res block, so it does NOT dither
    // thin beams apart or mush clusters (the reason the old code capped IGN at 6% — that
    // concern was full-res per-pixel; here the march is already half-res). Research:
    // Wolfe 2021 STBN; UE BlueNoise.ush; Wronski temporal volumetric integration.
    let jitter = ign(in.pos.xy + vec2<f32>(u.chroma.y * 5.588238));

    // HYBRID mode (counts.x = -2 sentinel): the froxel volume carries the wide/dim
    // "masses", so the raymarch renders ONLY the sharp hero beams (those with a
    // dedicated shadow map, misc.w >= 0) — preserving their crisp gobo/CA/prism
    // detail at a few-beam cost. In raymarch-only mode counts.x is the shared
    // occluder layer (>= 0) or -1, so this stays false and every beam is marched.
    let hero_only = u.counts.x < -1.5;
    let clump_s = 1.0 - clamp(u.chroma.z, 0.0, 1.0);
    let co2_on = u.tile.w >= -0.5;
    let co2_hq = u.tile.w > 0.5;

    var transmittance = 1.0;
    var scatter = vec3<f32>(0.0);

    for (var i = 0; i < nsteps; i = i + 1) {
        let t = t_near + dt * (f32(i) + jitter);
        let p = ro + rd * t;
        // Haze density + any stage-CO2 plume density at this sample (the beams
        // scatter through both identically → the CO2 is lit like real volumetric smoke).
        var haze_d = 0.0;
        if (base > 1e-5) {
            haze_d = base * density_at(p, time);
        }
        var co2v = vec2<f32>(0.0, 0.0);
        if (co2_on) {
            co2v = co2_density_at(p, time);
        }
        let co2_d = co2v.x;     // extinction σₜ from the splatted sim grid
        let co2_core = co2v.y;  // raw grid density 0..1 (densest voxels → darker)
        let dens = haze_d + co2_d;
        let sigma_t = max(dens, 1e-5);
        let sigma_s = sigma_t * albedo;

        // CO2 SELF-SHADOW — the single most important cue for reading the cauliflower
        // as a 3D MASS (and the user's "denser = darker core, lighter edges"): a thick
        // plume's own lumps shadow each other from the rig/sky above. Crucially it uses
        // the DETAILED density (co2_density_at) so the LUMPS — not the smooth grid —
        // cast the shadows, giving dark cores + bright rims = crisp cauliflower even
        // under bright beams (a uniformly-lit plume reads as a smooth blob no matter how
        // much noise detail the density has). Floored so cores are deep grey, not black;
        // scales with density (faint plumes barely darken → stay visible).
        // tile.w = CO2 quality (1 = render, 0 = preview, <0 = no CO2).
        var co2_shadow = 1.0;
        if (co2_d > 1e-4) {
            let up = vec3<f32>(0.0, 1.0, 0.0);
            var sh = co2_density_at(p + up * 0.5, time).x * 3.0;
            if (co2_hq) {
                sh = co2_density_at(p + up * 0.35, time).x
                   + co2_density_at(p + up * 0.9, time).x
                   + co2_density_at(p + up * 1.8, time).x;
            }
            co2_shadow = (0.45 + 0.55 * exp(-sh * 0.4)) * (1.0 - 0.25 * co2_core);
        }
        let co2_frac = co2_d / sigma_t;
        // Self-ambient: thick white fog reads as a dim mass in the room's ambient.
        let co2_amb_in = u.co2_amb.rgb * u.co2_amb.w * co2_frac;
        var lin = ambient + co2_amb_in;
        for (var j = v_lo; j < v_hi; j = j + 1u) {
            let f = i32(tile_lights[j]);
            let fx = fixtures[f];
            if (hero_only && fx.misc.w < 0.0) {
                continue; // mass beam → handled by the froxel volume
            }
            let lpos = fx.pos_range.xyz;
            let range = fx.pos_range.w;
            let bdir = fx.dir_cos.xyz;
            let tan_half = fx.dir_cos.w;
            let lens_r = fx.color.w;

            let rel = p - lpos;
            let depth = dot(rel, bdir); // distance along the beam axis
            if (depth < 0.0 || depth > range) {
                continue;
            }
            // Lens-plane coordinates of the sample (along the cookie basis), used
            // for both the radial falloff and the lateral chromatic aberration.
            let pu = dot(rel, fx.cookie_r.xyz);
            let pv = dot(rel, fx.cookie_u.xyz);

            // Disc beam widening with zoom, cropped by iris; super-Gaussian edge
            // (hard spot → soft with frost) with two-sided chromatic fringe.
            let n_order = fx.shape.x;
            let iris = fx.shape.z;
            let frost = fx.shape.w;
            let cone_r = lens_r + depth * tan_half;     // un-iris cone (cookie scale)
            let beam_r = cone_r * iris;                  // iris crops the lit radius
            // Radial pre-cull: skip the super-Gaussian + cookie work for samples
            // far outside this beam. Past 2.5·beam_r BOTH the core and the spill
            // tail of opt_radial fall under the rad_max ≤ 0.002 gate below for
            // every valid n_order (clamped ≥ 1.2 in optics::resolve), and +|ca|
            // keeps the chromatic side-samples in range — so this is a lossless
            // early-out, not a clip.
            // Widen the cull as the edge softens (low n_order = frost/wash has a
            // long super-Gaussian tail) so it stays lossless.
            let cull = beam_r * (2.5 + abs(fx.misc.x) + (2.0 - clamp(fx.shape.x, 1.0, 2.0)));
            if (pu * pu + pv * pv > cull * cull) {
                continue;
            }
            let rad3 = opt_radial_ca(pu, pv, beam_r, n_order, fx.misc.x);
            let rad_max = max(rad3.x, max(rad3.y, rad3.z));
            if (rad_max <= 0.002) {
                continue;
            }

            // Plain-beam fast-path: multi-emitter wash / pixel-bar cells carry no
            // gobo / animation / CMY / shutter-blade, so the whole projected-cookie
            // chain (opt_project → opt_cookie → opt_shutter) returns identity. Such
            // a cell is flagged at build time with extra.z = -1 — a sentinel that
            // CANNOT collide with a real shutter_close (always ≥ 0). Skipping that
            // chain is the dominant per-step saving for dense pixel bars (the lit
            // cells are co-located, so every sample falls inside many cell cones).
            let plain = fx.extra.z < -0.5;
            var trans = vec3<f32>(1.0);
            if (!plain) {
                // Projected optical cookie: gobo wheel 1 × wheel 2 × animation glass,
                // blurred by focus error + frost (mip LOD), with per-channel chromatic
                // aberration so the gobo's pattern edges fringe too. The aerial shaft
                // has no surface footprint, so DON'T sharpen it (sharpen = 0).
                let guv = opt_project(rel, depth, fx.cookie_r.xyz, fx.cookie_u.xyz, cone_r);
                let lod = opt_lod(depth, fx.shape.y, frost, tan_half, iris, lens_r);
                trans = opt_cookie(
                    gobo_tex, gobo_samp, guv,
                    fx.cookie_r.w, fx.cookie_u.w,
                    fx.extra.x, fx.extra.y, fx.cmyf.xyz, lod, 0.0,
                ) * opt_shutter((guv - vec2<f32>(0.5)) * 2.0, fx.extra.z, fx.extra.w, fx.cmyf.w);
                if (max(trans.r, max(trans.g, trans.b)) <= 0.001) {
                    continue;
                }
            }

            // Laser (misc.y): a coherent collimated beam is visible ONLY via
            // Tyndall scatter off haze — no inverse-square cone falloff along the
            // streak, strong forward single-scatter. Lamps keep the cone atten.
            let laser = fx.misc.y > 0.5;
            let atten = select(1.0 / (1.0 + depth * depth * 0.015), 1.0, laser);
            // Lens hotspot: a real beam — especially a narrow one / complex optics — is
            // brightest right at the lens, blooming over the first ~1-2 m before settling
            // into the cone (see the reference). Scales with narrowness (a tight beam
            // concentrates flux into a sharper near-field hotspot). Lasers are already
            // collimated/uniform, so no hotspot for them.
            let narrow = clamp(1.0 - tan_half * 5.0, 0.0, 1.0);
            let hotspot = select(1.0 + narrow * 1.6 * exp(-depth * 0.9), 1.0, laser);
            let tyndall = select(1.0, 3.0, laser);
            let phase = max(hg(dot(bdir, -rd), g), 0.05);
            // Hero beams cast shadows into the haze: darken the shaft where geometry
            // occludes the light at this sample point (beam blocked mid-air). A hero
            // beam (sidx >= 0) uses its own crisp per-beam map; every OTHER beam falls
            // back to the ONE shared occluder map (counts.x), so it still can't shine
            // straight through a solid object — just with a coarser shared depth.
            var vis = 1.0;
            let sidx = i32(fx.misc.w);
            let shared_idx = i32(u.counts.x);
            if (sidx >= 0) {
                vis = opt_shadow(p, shadow_mats[sidx], shadow_atlas, shadow_samp, sidx);
            } else if (shared_idx >= 0) {
                vis = opt_shadow(p, shadow_mats[shared_idx], shadow_atlas, shadow_samp, shared_idx);
            }
            // Fog SELF-shadowing — THE thing that makes clustered smoke read: dense haze
            // BETWEEN the lens and this sample dims the beam here, so a dense pocket casts a
            // soft shadow into the fog behind it (god-ray / cloud-shaft structure). Density
            // modulation alone washes out in the airlight integral; structured OCCLUSION
            // does not. Crude one-tap proxy of the optical depth from lens→sample along
            // the light ray. Gated by clumpiness so SMOOTH fog (uniform → uniform → no
            // structure) is left exactly as-is.
            if (clump_s > 0.001 && !laser) {
                let s1 = density_at(mix(lpos, p, 0.52), time);
                let self_od = s1 * base * depth;
                vis = vis * exp(-self_od * 0.07 * clump_s);
            }
            // THICK CO2 BLOCKS THE BEAM: the optical depth of CO2 between the lens
            // and this sample dims the beam here, so a dense plume casts a real
            // shadow into the beams AND self-shadows its own far side → the high
            // contrast (bright lit face, dark core) the reference shows. (3-tap proxy
            // along the light ray through the splatted density grid.) RENDER-ONLY —
            // this is the per-beam-per-step cost the preview path skips (the split).
            if (!laser && co2_hq) {
                let c1 = co2_base_at(mix(lpos, p, 0.4)).x;
                let c2 = co2_base_at(mix(lpos, p, 0.7)).x;
                let c3 = co2_base_at(mix(lpos, p, 0.9)).x;
                let co2_od = (c1 + c2 + c3) * (depth / 3.0);
                vis = vis * exp(-co2_od * 0.16);
            }
            // Per-cell HDR whiten + boost (accuracy): for a plain pixel cell,
            // extra.w carries its peak raw DMX level. A bright/white cell pulls its
            // shaft core toward neutral luminance (so it clips WHITE through the
            // tonemap, matching the lens face) and lifts its radiance, so bright
            // cells punch distinct brighter/whiter shafts while dim coloured cells
            // stay saturated. Quadratic → only genuinely bright cells whiten.
            let white01 = select(0.0, fx.extra.w, plain);
            let lum = max(fx.color.r, max(fx.color.g, fx.color.b));
            let whitened = mix(fx.color.rgb, vec3<f32>(lum), white01 * white01 * 0.6);
            let boost = 1.0 + white01 * white01 * 0.6;
            // --- Helmholtz–Kohlrausch chroma read-up of saturated beams in haze ---
            // A saturated beam (blue/deep-red/magenta) reads brighter than its Rec709
            // luma on a dark stage; per-channel ACES + over-1.0 bloom otherwise crush
            // its tiny luma toward black while neutral/warm beams saturate to white. ONE
            // chroma-preserving SCALAR gain lifts saturated hues only; white/pastel (and
            // the already-whitened bright cells) self-gate to 1. `whitened` is HDR-scaled,
            // so saturation comes from the PEAK-NORMALISED hue (scale-invariant): a dim
            // blue and a blazing blue lift identically, each keeping its own intensity.
            // hk_strength == 0 → hk == 1.0 (today's look, bit-for-bit).
            let hk_strength = u.chroma.x;
            let hk_mx = max(whitened.r, max(whitened.g, whitened.b));
            let hk_hue = whitened * (1.0 / max(hk_mx, 1e-4));
            let hk_sat = clamp(1.0 - dot(hk_hue, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
            // sat² gates the lift to genuinely saturated hues (white/pastel ≈ 1) while
            // `strength` scales DIRECTLY — no asymptote, so it can actually push a deep
            // blue/red to read in haze — capped so it can't blow out to flat neon.
            let hk = clamp(1.0 + hk_strength * hk_sat * hk_sat, 1.0, 3.5);
            lin += (whitened * hk) * trans * (rad3 * (atten * hotspot * phase * beam * vis * tyndall * boost));
        }

        // Self-shadow the in-scattered light (ambient + beams) in CO2, scaled by the
        // CO2 fraction so haze keeps its own look. THIS is what carves the dark cores /
        // bright edges out of an otherwise uniformly-bright plume → the cauliflower reads.
        lin = lin * mix(1.0, co2_shadow, co2_frac);

        let step_tr = exp(-sigma_t * dt);
        let integ = (lin * sigma_s) * ((1.0 - step_tr) / sigma_t);
        scatter += transmittance * integ;
        transmittance *= step_tr;
        if (transmittance < 0.012) {
            break;
        }
    }

    // In HYBRID mode the froxel volume already attenuated the background by the full
    // medium transmittance; this hero-only pass composites over that result, so it
    // must NOT re-attenuate it (else the scene behind fog is darkened twice, ≈ T²).
    // The hero scatter is still correctly attenuated by `transmittance` internally
    // above — we only force the OUTPUT alpha (the background multiplier) to 1.0.
    let out_t = select(transmittance, 1.0, hero_only);
    return vec4<f32>(scatter, out_t);
}
