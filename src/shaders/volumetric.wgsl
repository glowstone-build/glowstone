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
    counts: vec4<f32>,          // y = step count
};

struct Fixture {
    pos_range: vec4<f32>, // xyz = lens position, w = range (m)
    dir_cos: vec4<f32>,   // xyz = beam dir (unit), w = tan(half-angle)
    color: vec4<f32>,     // rgb = color * intensity, w = lens radius (m)
};

@group(0) @binding(0) var<uniform> u: Volumetric;
@group(0) @binding(1) var<storage, read> fixtures: array<Fixture>;
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var noise_tex: texture_3d<f32>;
@group(0) @binding(4) var noise_samp: sampler;

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

fn hg(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = 1.0 + g2 - 2.0 * g * cos_theta;
    return (1.0 - g2) / (4.0 * PI * pow(max(denom, 1e-4), 1.5));
}

// Interleaved gradient noise — per-pixel ray-start jitter (kills banding).
fn ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * p.x + 0.00583715 * p.y));
}

// Haze density factor (0 = clear air) from the tiling 3D noise texture, two
// scrolling scales for structure across the whole size range, high-contrast
// remap so the beam reveals clear pockets and dense wisps.
fn density_at(p: vec3<f32>, t: f32) -> f32 {
    let wind1 = vec3<f32>(0.10, 0.020, 0.06) * t;
    let wind2 = vec3<f32>(-0.06, 0.015, 0.05) * t;
    let wind3 = vec3<f32>(0.04, -0.03, 0.08) * t;
    // Three scrolling scales: large billows, medium wisps, fine turbulence.
    let n1 = textureSampleLevel(noise_tex, noise_samp, p * 0.08 + wind1, 0.0).r;
    let n2 = textureSampleLevel(noise_tex, noise_samp, p * 0.21 + wind2, 0.0).r;
    let n3 = textureSampleLevel(noise_tex, noise_samp, p * 0.46 + wind3, 0.0).r;
    let n = n1 * 0.5 + n2 * 0.32 + n3 * 0.18;
    // Thin base haze + strong high-contrast variation so the beam shows clear
    // air gaps and dense smoke wisps drifting through it.
    return 0.16 + smoothstep(0.26, 0.64, n) * 2.1;
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

    let steps = max(i32(u.counts.y), 1);
    let count = i32(arrayLength(&fixtures));
    let g = u.fog_max_g.w;
    let base = u.fog_min_density.w;
    let albedo = u.albedo_beam.rgb;
    let beam = u.albedo_beam.w;
    let time = u.eye_time.w;
    let ambient = albedo * 0.05; // dim ambient light so haze reads everywhere

    let seg = t_far - t_near;
    let dt = seg / f32(steps);
    let jitter = ign(in.pos.xy + time * 60.0);
    var t = t_near + dt * jitter;

    var transmittance = 1.0;
    var scatter = vec3<f32>(0.0);

    for (var i = 0; i < steps; i = i + 1) {
        let p = ro + rd * t;
        let dens = base * density_at(p, time);
        let sigma_t = max(dens, 1e-5);
        let sigma_s = sigma_t * albedo;

        var lin = ambient;
        for (var f = 0; f < count; f = f + 1) {
            let fx = fixtures[f];
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
            let axis_pt = lpos + bdir * depth;
            let axis_dist = length(p - axis_pt);
            // Disc beam: radius grows from the lens radius with the beam angle.
            let beam_r = lens_r + depth * tan_half;
            let radial = smoothstep(beam_r, beam_r * 0.45, axis_dist);
            if (radial <= 0.0) {
                continue;
            }
            let atten = 1.0 / (1.0 + depth * depth * 0.04);
            let phase = max(hg(dot(bdir, -rd), g), 0.05);
            lin += fx.color.rgb * (radial * atten * phase * beam);
        }

        let step_tr = exp(-sigma_t * dt);
        let integ = (lin * sigma_s) * ((1.0 - step_tr) / sigma_t);
        scatter += transmittance * integ;
        transmittance *= step_tr;
        if (transmittance < 0.012) {
            break;
        }
        t += dt;
    }

    return vec4<f32>(scatter, transmittance);
}
