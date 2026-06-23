// froxel.wgsl — frustum-aligned volumetric fog (Wronski/Hillaire). Two compute
// passes that decouple light cost from screen pixels:
//   inject:    one thread per froxel cell → loop the beams ONCE per cell, write
//              (in-scattered radiance × scattering coeff, extinction) into a 3D tex.
//   integrate: one thread per (x,y) column → march +Z accumulating the SAME slice
//              integral the fragment raymarch uses, writing (accumulated scatter,
//              transmittance) per cell.
// The composite (post.wgsl) then does ONE trilinear lookup per screen pixel.
//
// Z is distributed exponentially from `near` to `far` (distance along each pixel's
// ray) so resolution concentrates near the camera. `optics.wgsl` (opt_* + WheelGpu)
// is prepended by `load_with_optics`.

struct Froxel {
    inv_view_proj: mat4x4<f32>,
    eye_time: vec4<f32>,        // xyz = eye, w = time (s)
    fog_min_density: vec4<f32>, // xyz = box min, w = base density (sigma_t)
    fog_max_g: vec4<f32>,       // xyz = box max, w = anisotropy g
    albedo_beam: vec4<f32>,     // rgb = scattering tint, w = beam intensity
    dims: vec4<f32>,            // x,y,z = froxel grid dims, w = shared shadow layer (-1 none)
    planes: vec4<f32>,          // x = near, y = far (distance along ray, m); z = chroma read-up; w unused
};

// Mirrors `Fixture` in volumetric.wgsl (same FixtureGpu packing).
struct Fixture {
    pos_range: vec4<f32>,
    dir_cos: vec4<f32>,
    color: vec4<f32>,
    cookie_r: vec4<f32>,
    cookie_u: vec4<f32>,
    extra: vec4<f32>,
    shape: vec4<f32>,
    misc: vec4<f32>,
    cmyf: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Froxel;
@group(0) @binding(1) var<storage, read> fixtures: array<Fixture>;
@group(0) @binding(2) var noise_tex: texture_3d<f32>;
@group(0) @binding(3) var noise_samp: sampler;
@group(0) @binding(4) var gobo_tex: texture_2d_array<f32>;
@group(0) @binding(5) var gobo_samp: sampler;
@group(0) @binding(6) var shadow_atlas: texture_depth_2d_array;
@group(0) @binding(7) var shadow_samp: sampler_comparison;
@group(0) @binding(8) var<storage, read> shadow_mats: array<mat4x4<f32>>;
@group(0) @binding(9) var<storage, read> wheels: array<WheelGpu>;
// inject WRITES this; integrate reads it (bound as a plain texture, binding 11).
@group(0) @binding(10) var froxel_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(11) var froxel_in: texture_3d<f32>;

const PI: f32 = 3.14159265359;

fn hg(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = 1.0 + g2 - 2.0 * g * cos_theta;
    return (1.0 - g2) / (4.0 * PI * pow(max(denom, 1e-4), 1.5));
}

fn density_at(p: vec3<f32>, t: f32) -> f32 {
    let wind1 = vec3<f32>(0.10, 0.020, 0.06) * t;
    let wind2 = vec3<f32>(-0.06, 0.015, 0.05) * t;
    let wind3 = vec3<f32>(0.04, -0.03, 0.08) * t;
    let n1 = textureSampleLevel(noise_tex, noise_samp, p * 0.08 + wind1, 0.0).r;
    let n2 = textureSampleLevel(noise_tex, noise_samp, p * 0.21 + wind2, 0.0).r;
    let n3 = textureSampleLevel(noise_tex, noise_samp, p * 0.46 + wind3, 0.0).r;
    let n = n1 * 0.5 + n2 * 0.32 + n3 * 0.18;
    return 0.16 + smoothstep(0.26, 0.64, n) * 2.1;
}

// Distance along a pixel ray for froxel slice `zf` ∈ [0,1] (exponential).
fn slice_dist(zf: f32) -> f32 {
    let near = u.planes.x;
    let far = u.planes.y;
    return near * pow(far / near, zf);
}

// World-space centre of froxel cell (ix,iy,iz).
fn cell_world(ix: u32, iy: u32, iz: u32) -> vec3<f32> {
    let uv = (vec2<f32>(f32(ix) + 0.5, f32(iy) + 0.5)) / u.dims.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let nh = u.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let fh = u.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let wn = nh.xyz / nh.w;
    let wf = fh.xyz / fh.w;
    let dir = normalize(wf - wn);
    let t = slice_dist((f32(iz) + 0.5) / u.dims.z);
    return u.eye_time.xyz + dir * t;
}

// In-scattered radiance × scattering coefficient at world point `p`, summed over
// every beam. Mirrors the volumetric.wgsl inner loop (radial super-Gaussian,
// optional cookie, distance attenuation, HG phase, shadow occlusion) but evaluated
// once per froxel cell instead of per pixel-step.
fn inscatter(p: vec3<f32>, sigma_t: f32) -> vec3<f32> {
    let g = u.fog_max_g.w;
    let beam = u.albedo_beam.w;
    let albedo = u.albedo_beam.rgb;
    let eye = u.eye_time.xyz;
    let view = normalize(eye - p); // toward camera, for the HG phase
    let shared_idx = i32(u.dims.w);

    var lin = albedo * 0.012; // dim ambient haze
    let count = i32(arrayLength(&fixtures));
    for (var f = 0; f < count; f = f + 1) {
        let fx = fixtures[f];
        // HYBRID: the few hero beams (misc.w >= 0, a dedicated shadow map) are
        // rendered SHARP by the fragment raymarch — skip them here so the froxel
        // only carries the wide/dim "masses" and there's no double-counting.
        if (fx.misc.w >= 0.0) {
            continue;
        }
        let lpos = fx.pos_range.xyz;
        let range = fx.pos_range.w;
        let bdir = fx.dir_cos.xyz;
        let tan_half = fx.dir_cos.w;
        let lens_r = fx.color.w;

        let rel = p - lpos;
        let depth = dot(rel, bdir);
        if (depth < 0.0 || depth > range) {
            continue;
        }
        let pu = dot(rel, fx.cookie_r.xyz);
        let pv = dot(rel, fx.cookie_u.xyz);
        let n_order = fx.shape.x;
        let iris = fx.shape.z;
        let frost = fx.shape.w;
        let cone_r = lens_r + depth * tan_half;
        let beam_r = cone_r * iris;
        let cull = beam_r * (2.5 + abs(fx.misc.x) + (2.0 - clamp(fx.shape.x, 1.0, 2.0)));
        if (pu * pu + pv * pv > cull * cull) {
            continue;
        }
        let rad3 = opt_radial_ca(pu, pv, beam_r, n_order, fx.misc.x);
        let rad_max = max(rad3.x, max(rad3.y, rad3.z));
        if (rad_max <= 0.002) {
            continue;
        }

        let plain = fx.extra.z < -0.5;
        var trans = vec3<f32>(1.0);
        if (!plain) {
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
        let laser = fx.misc.y > 0.5;
        let atten = select(1.0 / (1.0 + depth * depth * 0.015), 1.0, laser);
        let tyndall = select(1.0, 3.0, laser);
        let phase = max(hg(dot(bdir, view), g), 0.05);
        var vis = 1.0;
        let sidx = i32(fx.misc.w);
        if (sidx >= 0) {
            vis = opt_shadow(p, shadow_mats[sidx], shadow_atlas, shadow_samp, sidx);
        } else if (shared_idx >= 0) {
            vis = opt_shadow(p, shadow_mats[shared_idx], shadow_atlas, shadow_samp, shared_idx);
        }
        let white01 = select(0.0, fx.extra.w, plain);
        let lum = max(fx.color.r, max(fx.color.g, fx.color.b));
        let whitened = mix(fx.color.rgb, vec3<f32>(lum), white01 * white01 * 0.6);
        let boost = 1.0 + white01 * white01 * 0.6;
        // --- Helmholtz–Kohlrausch chroma read-up (identical to volumetric.wgsl) ---
        // Lifts saturated hues (blue/deep-red/magenta) so they read in haze; white/
        // pastel/whitened cells self-gate to 1. Saturation is taken from the PEAK-
        // NORMALISED hue so it's invariant to the HDR-scaled `whitened` magnitude.
        // hk_strength == 0 → hk == 1.0. Kept byte-for-byte in step with the raymarch
        // so the hybrid froxel(masses)+raymarch(heroes) seam stays seamless.
        let hk_strength = u.planes.z;
        let hk_mx = max(whitened.r, max(whitened.g, whitened.b));
        let hk_hue = whitened * (1.0 / max(hk_mx, 1e-4));
        let hk_sat = clamp(1.0 - dot(hk_hue, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
        // sat² gate + direct strength scaling (identical to volumetric.wgsl).
        let hk = clamp(1.0 + hk_strength * hk_sat * hk_sat, 1.0, 3.5);
        lin += (whitened * hk) * trans * (rad3 * (atten * phase * beam * vis * tyndall * boost));
    }
    return lin * (sigma_t * albedo); // sigma_s * lin
}

@compute @workgroup_size(8, 8, 1)
fn inject(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= u32(u.dims.x) || gid.y >= u32(u.dims.y) || gid.z >= u32(u.dims.z)) {
        return;
    }
    let p = cell_world(gid.x, gid.y, gid.z);
    let dens = u.fog_min_density.w * density_at(p, u.eye_time.w);
    let sigma_t = max(dens, 1e-5);
    // Outside the fog-box AABB there is no medium.
    let inside = all(p >= u.fog_min_density.xyz) && all(p <= u.fog_max_g.xyz);
    let st = select(0.0, sigma_t, inside);
    let scatter = select(vec3<f32>(0.0), inscatter(p, st), inside);
    textureStore(froxel_out, vec3<i32>(gid), vec4<f32>(scatter, st));
}

@compute @workgroup_size(8, 8, 1)
fn integrate(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= u32(u.dims.x) || gid.y >= u32(u.dims.y)) {
        return;
    }
    let fz = i32(u.dims.z);
    var transmittance = 1.0;
    var acc = vec3<f32>(0.0);
    for (var z = 0; z < fz; z = z + 1) {
        let s = textureLoad(froxel_in, vec3<i32>(i32(gid.x), i32(gid.y), z), 0);
        let t0 = slice_dist(f32(z) / u.dims.z);
        let t1 = slice_dist(f32(z + 1) / u.dims.z);
        let dz = max(t1 - t0, 1e-4);
        let sigma_t = max(s.a, 1e-5);
        let step_tr = exp(-sigma_t * dz);
        acc += transmittance * (s.rgb * ((1.0 - step_tr) / sigma_t));
        transmittance *= step_tr;
        textureStore(froxel_out, vec3<i32>(i32(gid.x), i32(gid.y), z), vec4<f32>(acc, transmittance));
    }
}
