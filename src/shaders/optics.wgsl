// optics.wgsl — shared beam-optics helpers, prepended to BOTH volumetric.wgsl
// (the beam) and mesh.wgsl (the floor pool) so they stay in lock-step. Defines
// NO entry points, NO bindings, and no names that clash with either shader
// (`PI`, `hg`, `vs_fullscreen` live in the including shaders). The gobo atlas is
// passed in as a texture+sampler argument so each shader can bind it at its own
// @group/@binding.

// Project a world-space offset `rel` (sample - lens) onto the lens plane, giving
// a gobo cookie UV in [0,1] whose inscribed circle (uv radius 0.5) is the cone
// edge at this depth. `radius` is the true cone radius (lens_r + depth·tan_half)
// so the round radial falloff is INSCRIBED in the cookie square — otherwise the
// square clips the circle and the beam looks like a rounded square.
fn opt_project(rel: vec3<f32>, depth: f32, right: vec3<f32>, up: vec3<f32>, radius: f32) -> vec2<f32> {
    let inv = 0.5 / max(radius, 1e-4);
    return vec2<f32>(dot(rel, right), dot(rel, up)) * inv + vec2<f32>(0.5, 0.5);
}

// Rotate a UV about its centre (gobo / prism image rotation).
fn opt_rot(uv: vec2<f32>, a: f32) -> vec2<f32> {
    let s = sin(a);
    let c = cos(a);
    let p = uv - vec2<f32>(0.5, 0.5);
    return vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y) + vec2<f32>(0.5, 0.5);
}

// Super-Gaussian radial falloff: exactly 0.5 at axis_dist==beam_r; `n` large =
// hard flat-top (spot), n~1.5 = soft (frost/wash). A faint spill tail keeps a
// believable glow just outside the cone.
fn opt_radial(axis_dist: f32, beam_r: f32, n: f32) -> f32 {
    let x = axis_dist / max(beam_r, 1e-4);
    let core = exp(-0.6931472 * pow(x, 2.0 * n));
    let spill = 0.02 * exp(-x * x * 0.5);
    return max(core, spill);
}

// Per-channel radial with **lateral (transverse) chromatic aberration**: the red
// and blue images shift in OPPOSITE directions across the lens plane (red one
// way, blue the other, green centred), so one edge of the beam fringes warm
// (amber/red) and the opposite edge fringes cool (blue) — the classic two-sided
// CA you actually see, rather than a single-colour rim. `(pu, pv)` are the
// sample's lens-plane coordinates (distance-from-axis along right/up). The core
// is white (all three overlap), so energy is conserved; only the edge separates.
fn opt_radial_ca(pu: f32, pv: f32, beam_r: f32, n: f32, k: f32) -> vec3<f32> {
    let off = k * beam_r;
    return vec3<f32>(
        opt_radial(length(vec2<f32>(pu + off, pv)), beam_r, n), // red  shifts −u (amber edge)
        opt_radial(length(vec2<f32>(pu, pv)), beam_r, n),       // green centred
        opt_radial(length(vec2<f32>(pu - off, pv)), beam_r, n), // blue shifts +u (cool edge)
    );
}

// Mip-LOD for the gobo from focus error (sharp at focus_dist) + frost blur.
fn opt_lod(depth: f32, focus_dist: f32, frost01: f32) -> f32 {
    let defocus = abs(depth - focus_dist) / max(focus_dist, 0.5);
    let sigma = 0.5 * defocus + 1.2 * frost01;
    return clamp(log2(1.0 + sigma * 64.0), 0.0, 8.0);
}

// Sample one atlas layer at a rotated UV. The UV is clamped (the sampler is
// ClampToEdge), so outside the image we read the border texel — white for the
// "open" slot (→ the beam stays a clean round disc shaped only by the radial
// falloff, no square cutoff) and black for a gobo's holder (→ round gobo).
fn opt_layer(t: texture_2d_array<f32>, s: sampler, uv: vec2<f32>, layer: i32, rot: f32, lod: f32) -> vec4<f32> {
    if (layer < 0) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    let r = clamp(opt_rot(uv, rot), vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
    return textureSampleLevel(t, s, r, clamp(layer, 0, 63), lod);
}

// Gobo with slot cross-fade: `layer_f` is an absolute fractional atlas layer
// (<0 = none/open). During a wheel move it blends consecutive slots.
fn opt_gobo(t: texture_2d_array<f32>, s: sampler, uv: vec2<f32>, layer_f: f32, rot: f32, lod: f32) -> vec4<f32> {
    if (layer_f < 0.0) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    let a = floor(layer_f);
    let frac = layer_f - a;
    var c = opt_layer(t, s, uv, i32(a), rot, lod);
    if (frac > 0.001) {
        c = mix(c, opt_layer(t, s, uv, i32(a) + 1, rot, lod), frac);
    }
    return c;
}

// Animation glass: a scrolling tiled mask (the classic fire/water shimmer).
fn opt_anim(t: texture_2d_array<f32>, s: sampler, uv: vec2<f32>, layer: i32, scroll: f32, lod: f32) -> f32 {
    if (layer < 0) {
        return 1.0;
    }
    let auv = fract(uv * 1.5 + vec2<f32>(scroll, scroll * 0.35));
    return textureSampleLevel(t, s, auv, clamp(layer, 0, 63), lod).a;
}

// The combined gobo stack (wheel 1 × wheel 2) sampled at one lens-frame uv.
fn opt_cookie_at(
    t: texture_2d_array<f32>, s: sampler, guv: vec2<f32>,
    g1: f32, r1: f32, g2: f32, r2: f32, lod: f32,
) -> vec4<f32> {
    return opt_gobo(t, s, guv, g1, r1, lod) * opt_gobo(t, s, guv, g2, r2, lod);
}

// Per-channel transmitted colour of the whole cookie (gobo 1 × gobo 2 ×
// animation) WITH chromatic aberration: red/green/blue are sampled at lens-frame
// uv offset ±(ca·0.5) along the dispersion axis (uv.x = lens "right"), so the
// projected gobo's pattern edges fringe red/blue exactly like the beam edge —
// not just the open cone. Degrades to a plain cookie when ca = 0, and costs
// nothing extra for an open beam (the gobo samples early-out to white).
fn opt_cookie_ca(
    t: texture_2d_array<f32>, s: sampler, guv: vec2<f32>,
    g1: f32, r1: f32, g2: f32, r2: f32,
    anim_layer: i32, anim_scroll: f32, lod: f32, ca: f32,
) -> vec3<f32> {
    let off = vec2<f32>(ca * 0.5, 0.0);
    let cr = opt_cookie_at(t, s, guv + off, g1, r1, g2, r2, lod);
    let cg = opt_cookie_at(t, s, guv, g1, r1, g2, r2, lod);
    let cb = opt_cookie_at(t, s, guv - off, g1, r1, g2, r2, lod);
    let anim = opt_anim(t, s, guv, anim_layer, anim_scroll, lod);
    return vec3<f32>(cr.r * cr.a, cg.g * cg.a, cb.b * cb.a) * anim;
}
