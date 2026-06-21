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
    // Green is always centred; compute it once. With no chromatic aberration the
    // red/blue images coincide with green (off = 0), so collapse to a single
    // broadcast — byte-for-byte identical, but 2 of 3 super-Gaussian pow() gone.
    let green = opt_radial(length(vec2<f32>(pu, pv)), beam_r, n);
    if (abs(k) <= 0.001) {
        return vec3<f32>(green, green, green);
    }
    let off = k * beam_r;
    return vec3<f32>(
        opt_radial(length(vec2<f32>(pu + off, pv)), beam_r, n), // red  shifts −u (amber edge)
        green,                                                  // green centred
        opt_radial(length(vec2<f32>(pu - off, pv)), beam_r, n), // blue shifts +u (cool edge)
    );
}

// Mip-LOD for the gobo from APERTURE-DEPENDENT depth of field + frost.
// Sharp (mip0) at focus_dist; the in-focus band WIDENS for narrow / iris-stopped
// beams (high f-number) and TIGHTENS for wide-open beams — like a real ERS /
// profile spot (the "donut" trick: stopping the iris deepens DoF). Defocus is
// measured in reciprocal distance (diopters) so the band is asymmetric and
// unbounded past the hyperfocal point — a far-focused beam stays sharp on the
// floor. Returns EXACTLY 0 in the hyperfocal band so gobo sharpening sees an
// un-blurred mip0. See .context/research-focus-dof.md.
fn opt_lod(
    depth: f32, focus_dist: f32, frost01: f32,
    tan_half: f32, iris: f32, lens_r: f32,
) -> f32 {
    // Aperture / DoF gain ∝ (lens radius + floor) · iris · beam half-angle.
    // Narrow beam or stopped iris → small k_ap → wide DoF.
    let k_ap = 0.45 * (0.04 + lens_r) * clamp(iris, 0.02, 1.0) * max(tan_half, 1e-3);
    // Diopter defocus with a hyperfocal deadband (at/after focus → mip0).
    let diopter = abs(1.0 / max(focus_dist, 0.25) - 1.0 / max(depth, 0.25));
    let defocus = max(diopter - 0.006, 0.0);
    // Blur as a fraction of the cone radius (×~half the atlas width) + frost.
    let sigma = k_ap * defocus * 200.0 + 1.2 * frost01;
    return clamp(log2(1.0 + sigma * 64.0), 0.0, 8.0);
}

// Contour-preserving edge steepening for a transmittance/mask value, fixed-point
// at 0, 0.5, 1 so the 0.5 iso-contour (the gobo edge) can't move (no fattening/
// thinning). `k` ~ sharpen amount; the multiplier peaks at a=0.5 and is 1 at the
// solid/empty extremes. Cheaper than CAS (no extra taps) and ideal for masks;
// used on the floor pool only — it maximally steepens mid-tones, which would
// alias the aerial shaft into stripes.
fn opt_sharpen_iso(a: f32, k: f32) -> f32 {
    return clamp(0.5 + (a - 0.5) * (1.0 + k * (1.0 - abs(2.0 * a - 1.0))), 0.0, 1.0);
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
    anim_layer: i32, anim_scroll: f32, lod: f32, ca: f32, sharpen: f32,
) -> vec3<f32> {
    // Green/centre cookie + animation are always needed; sample once. With no
    // chromatic aberration the red/blue offset samples coincide with green, so
    // skip them — byte-for-byte identical, up to 2 fewer cookie evaluations.
    let cg = opt_cookie_at(t, s, guv, g1, r1, g2, r2, lod);
    let anim = opt_anim(t, s, guv, anim_layer, anim_scroll, lod);
    var out: vec3<f32>;
    if (abs(ca) <= 0.001) {
        out = vec3<f32>(cg.r * cg.a, cg.g * cg.a, cg.b * cg.a) * anim;
    } else {
        let off = vec2<f32>(ca * 0.5, 0.0);
        let cr = opt_cookie_at(t, s, guv + off, g1, r1, g2, r2, lod);
        let cb = opt_cookie_at(t, s, guv - off, g1, r1, g2, r2, lod);
        out = vec3<f32>(cr.r * cr.a, cg.g * cg.a, cb.b * cb.a) * anim;
    }
    // Edge sharpening, faded out with defocus (exp2(-lod) = fraction of mip0
    // detail still present) so blurred mips are never re-sharpened. Zero-tap
    // contour steepening; callers pass sharpen=0 to disable (aerial shaft).
    if (sharpen > 0.001) {
        let gain = sharpen * max(exp2(-lod) - 0.03, 0.0);
        if (gain > 0.0) {
            out = vec3<f32>(
                opt_sharpen_iso(out.r, gain),
                opt_sharpen_iso(out.g, gain),
                opt_sharpen_iso(out.b, gain),
            );
        }
    }
    return out;
}

// Hard-shadow visibility (1 = lit, 0 = occluded) of a world-space point from a
// hero beam whose light-space view-projection is `vp`, sampling that beam's layer
// of the shared depth atlas with 2x2 hardware PCF. Returns 1 outside the map or
// behind the light plane (unshadowed). Both the floor pool (mesh.wgsl) and the
// beam shaft (volumetric.wgsl) call this so geometry both casts floor shadows and
// occludes the beam mid-air. The texture/sampler are passed in so the one helper
// serves either shader's own bindings.
fn opt_shadow(
    world_pos: vec3<f32>, vp: mat4x4<f32>,
    t: texture_depth_2d_array, s: sampler_comparison, layer: i32,
) -> f32 {
    let lc = vp * vec4<f32>(world_pos, 1.0);
    if (lc.w <= 0.0) {
        return 1.0;
    }
    let ndc = lc.xyz / lc.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0) {
        return 1.0;
    }
    return textureSampleCompareLevel(t, s, uv, layer, ndc.z - 0.001);
}
