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
    // Narrow beam or stopped iris → small k_ap → wide DoF. (Tuned stronger than
    // the textbook value so a normal spot still visibly softens away from its
    // focus plane — a knife-sharp-everywhere beam reads as fake.)
    let k_ap = 1.1 * (0.04 + lens_r) * clamp(iris, 0.02, 1.0) * max(tan_half, 1e-3);
    // Diopter defocus with a small hyperfocal deadband (at focus → mip0 so the
    // gobo sharpening sees full-res; just outside, blur ramps up immediately).
    let diopter = abs(1.0 / max(focus_dist, 0.25) - 1.0 / max(depth, 0.25));
    let defocus = max(diopter - 0.003, 0.0);
    // Blur as a fraction of the cone radius (×~half the atlas width) + frost.
    let sigma = k_ap * defocus * 230.0 + 1.2 * frost01;
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
// Highest valid gobo-atlas layer index — MUST track `atlas::LAYERS - 1`.
const ATLAS_MAX_LAYER: i32 = 127;

fn opt_layer(t: texture_2d_array<f32>, s: sampler, uv: vec2<f32>, layer: i32, rot: f32, lod: f32) -> vec4<f32> {
    if (layer < 0) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    let r = clamp(opt_rot(uv, rot), vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
    return textureSampleLevel(t, s, r, clamp(layer, 0, ATLAS_MAX_LAYER), lod);
}

// Animation glass: a scrolling tiled mask (the classic fire/water shimmer).
// The effect / animation wheel is a WHOLE rotating disc, not a single centred
// gobo: the beam passes through a narrow window at radius R on it, so as the disc
// turns the pattern sweeps across the gate as a continuous radial motion. We place
// the gate window off-centre on the disc and rotate the disc by `scroll` turns.
fn opt_anim(t: texture_2d_array<f32>, s: sampler, uv: vec2<f32>, layer: i32, scroll: f32, lod: f32) -> f32 {
    if (layer < 0) {
        return 1.0;
    }
    // The gate window sits OFF-CENTRE on the disc (at radius ~0.42 of the image)
    // and the whole disc spins by `scroll` turns, so the pattern sweeps across the
    // gate as a continuous radial motion. The sampled coords stay inside [0,1]
    // (no fract / tiling) so a non-tiling glass image shows no wrap seam.
    let a = scroll * 6.2831853;            // one full disc revolution per phase wrap
    let ca = cos(a);
    let sa = sin(a);
    let g = uv - vec2<f32>(0.5, 0.5);      // gate-local coords (~[-0.5, 0.5])
    let disk = vec2<f32>(0.42, 0.0) + g * 0.55; // window region on the disc
    let rp = vec2<f32>(ca * disk.x - sa * disk.y, sa * disk.x + ca * disk.y);
    let auv = clamp(rp + vec2<f32>(0.5, 0.5), vec2<f32>(0.0), vec2<f32>(1.0));
    return textureSampleLevel(t, s, auv, clamp(layer, 0, ATLAS_MAX_LAYER), lod).a;
}

// PHYSICAL WHEEL: a disc of N slots whose continuous slewed `position` (slot
// units) passes across the beam gate. The fragment's tangential beam coordinate
// `t = guv.x - 0.5` maps to a wheel position, so a move SPLITS the beam between
// adjacent slots (and, with a `gap`, shows the dark metal holder between gobos —
// colour wheels pass gap≈0 so slots abut). `base < 0` = no wheel (clear).
// See .context/research-wheel-optics.md.
fn opt_wheel(
    tex: texture_2d_array<f32>, samp: sampler, guv: vec2<f32>,
    base: f32, position: f32, n: f32, gap: f32, rot: f32, lod: f32,
) -> vec4<f32> {
    if (base < 0.0 || n < 1.0) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    const GATE_FRAC: f32 = 0.7;        // gate width in slot units (gate < 1 slot)
    let tcoord = guv.x - 0.5;          // tangential position across the beam
    let u = position + tcoord * GATE_FRAC;
    let cell = floor(u + 0.5);
    // wrapped slot index in [0, n)
    let slot = cell - floor(cell / n) * n;
    let frac = (u + 0.5) - cell;       // 0..1 within the pitch
    let d = abs(frac - 0.5);           // 0 centre … 0.5 boundary
    if (gap > 0.001 && d > (0.5 - gap * 0.5)) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0); // opaque holder gap (gobo) / seam
    }
    return opt_layer(tex, samp, guv, i32(base + slot), rot, lod);
}

// CMY: three graduated dichroic FLAGS sliding across the aperture on distinct
// axes (≈120° apart), each producing a coloured gradient/edge that sweeps in as
// it inserts. Per-fragment optical density (densities add → 10^-D), mirroring
// the CPU density model. `p` = beam cross-section in the unit disc = (guv-0.5)*2;
// `cmy` = the three slewed insertions (0 = clear … 1 = full). Returns the
// transmittance triple to multiply into the beam. (research-cmy.md)
// A colour wheel packed as ONE atlas layer of vertical bands (see atlas.rs
// `color_strip`): same physical-wheel slot maths as `opt_wheel` (split + gap as
// the wheel moves), but the chosen slot is read as a horizontal band rather than
// a whole image layer. Returns the slot's transmittance triple. `base < 0` = none.
fn opt_color_strip(
    tex: texture_2d_array<f32>, samp: sampler, guv: vec2<f32>,
    base: f32, position: f32, n: f32, gap: f32,
) -> vec3<f32> {
    if (base < 0.0 || n < 1.0) {
        return vec3<f32>(1.0, 1.0, 1.0);
    }
    const GATE_FRAC: f32 = 0.7;
    let tcoord = guv.x - 0.5;
    let u = position + tcoord * GATE_FRAC;
    let cell = floor(u + 0.5);
    let slot = cell - floor(cell / n) * n;
    let frac = (u + 0.5) - cell;       // 0..1 within the pitch (0.5 = slot centre)
    let d = abs(frac - 0.5);           // 0 centre … 0.5 boundary
    // The two slots straddling the gate, and a SOFT cross-fade between them near
    // the boundary — a real colour-wheel spoke is a thin out-of-focus edge, not a
    // hard black line. Sharper than the CMY flags (the wheel sits nearer the gate),
    // but no longer a harsh seam.
    let dir = select(-1.0, 1.0, frac >= 0.5);
    let nb = cell + dir;
    let slotB = nb - floor(nb / n) * n;
    let colA = textureSampleLevel(tex, samp, vec2<f32>((slot + 0.5) / n, 0.5), i32(base), 0.0).rgb;
    let colB = textureSampleLevel(tex, samp, vec2<f32>((slotB + 0.5) / n, 0.5), i32(base), 0.0).rgb;
    const SEAM: f32 = 0.16;            // blend-zone half-width (in pitch units)
    let t = smoothstep(0.5 - SEAM, 0.5, d) * 0.5; // → 50/50 at the boundary
    var col = mix(colA, colB, t);
    if (gap > 0.001) {
        // Thin, soft, shallow darkening at the divider — never fully black.
        let spoke = smoothstep(0.5 - gap, 0.5, d);
        col = col * (1.0 - 0.45 * spoke);
    }
    return col;
}

fn opt_cmy_flag(u: f32, c: f32) -> f32 {
    const EDGE_SOFT: f32 = 0.22;       // soft sawtooth/halftone edge width
    let edge = 2.0 * c - 1.0;          // inserts from -axis (c=0) to +axis (c=1)
    let depth = (edge - u) * 0.5;      // >0 ⇒ covered; magnitude ⇒ how deep
    let cover = smoothstep(-EDGE_SOFT, EDGE_SOFT, depth);
    let ramp = clamp(depth + 0.5, 0.0, 1.0); // graduated: denser deeper under the flag
    return cover * ramp;
}
fn opt_cmy(p: vec2<f32>, cmy: vec3<f32>) -> vec3<f32> {
    const D_PEAK: f32 = 1.8;
    const D_SHOULDER: f32 = 0.10;
    const GAMMA_INS: f32 = 1.6;
    let aC = vec2<f32>(1.0, 0.0);
    let aM = vec2<f32>(-0.5, 0.8660254);
    let aY = vec2<f32>(-0.5, -0.8660254);
    let rc = pow(opt_cmy_flag(dot(p, aC), cmy.x), GAMMA_INS);
    let rm = pow(opt_cmy_flag(dot(p, aM), cmy.y), GAMMA_INS);
    let ry = pow(opt_cmy_flag(dot(p, aY), cmy.z), GAMMA_INS);
    let dR = D_PEAK * rc + D_SHOULDER * rm + D_SHOULDER * ry; // cyan ↘ R
    let dG = D_SHOULDER * rc + D_PEAK * rm + D_SHOULDER * ry; // magenta ↘ G
    let dB = D_SHOULDER * rc + D_SHOULDER * rm + D_PEAK * ry; // yellow ↘ B
    return vec3<f32>(pow(10.0, -dR), pow(10.0, -dG), pow(10.0, -dB));
}

// One physical wheel in the per-fixture chain (a DYNAMIC count, not a fixed
// gobo1/gobo2/colour triple). `d` = base atlas layer / position(slot) / n_slots /
// gap; `m.x` = kind (0 = gobo image block, 1 = colour strip), `m.y` = gobo image
// rotation. Read from the global `wheels` storage buffer (declared per-shader).
struct WheelGpu {
    d: vec4<f32>,
    m: vec4<f32>,
};

// Fold a fixture's `count` wheels (starting at `offset` in `wheels`) into one
// transmittance triple — gobo wheels sample the atlas image block (with holder
// gap + image rotation), colour wheels sample the packed colour strip. Any
// number of wheels of any mix; the renderer only emits the wheels actually
// engaged this frame (parked ones are folded to a uniform CPU tint upstream).
fn opt_wheels(
    tex: texture_2d_array<f32>, samp: sampler, guv: vec2<f32>,
    offset: u32, count: u32, lod: f32,
) -> vec3<f32> {
    var cook = vec3<f32>(1.0, 1.0, 1.0);
    for (var i = 0u; i < count; i = i + 1u) {
        let w = wheels[offset + i];
        if (w.m.x < 0.5) {
            let g = opt_wheel(tex, samp, guv, w.d.x, w.d.y, w.d.z, w.d.w, w.m.y, lod);
            cook = cook * vec3<f32>(g.r * g.a, g.g * g.a, g.b * g.a);
        } else {
            cook = cook * opt_color_strip(tex, samp, guv, w.d.x, w.d.y, w.d.z, w.d.w);
        }
    }
    return cook;
}

// The full per-fragment optical cookie: the fixture's wheel chain × CMY ×
// animation, each evaluated spatially (physical wheels + sliding flags). Returns
// the per-channel transmittance multiplying the beam colour. `sharpen` > 0 (floor
// pool only) applies LOD-faded contour steepening to the gobo edge.
fn opt_cookie(
    tex: texture_2d_array<f32>, samp: sampler, guv: vec2<f32>,
    wheel_off: f32, wheel_count: f32,
    anim_layer: f32, anim_scroll: f32, cmy: vec3<f32>, lod: f32, sharpen: f32,
) -> vec3<f32> {
    let anim = opt_anim(tex, samp, guv, i32(anim_layer), anim_scroll, lod);
    var out = opt_wheels(tex, samp, guv, u32(max(wheel_off, 0.0)), u32(max(wheel_count, 0.0)), lod)
            * anim;
    if (cmy.x + cmy.y + cmy.z > 0.001) {
        out = out * opt_cmy((guv - vec2<f32>(0.5)) * 2.0, cmy);
    }
    // Contour edge sharpening, faded out with defocus (exp2(-lod)); floor only.
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
