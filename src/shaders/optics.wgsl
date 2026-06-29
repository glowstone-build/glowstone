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
    let x2 = x * x;
    // Super-Gaussian exponent x^(2n). Instead of a per-sample pow() (and instead
    // of snapping to integer orders — which makes a zoom-driven shoulder sweep POP
    // as the shaft sharpens), BLEND the shape continuously across n ∈ [1,3] from a
    // mix of x², x⁴ and x⁶. Smooth (no snap), cheap (pow only for the rare n>3
    // spot), and exact at the integer orders (mix endpoints == old x⁴ / x⁶).
    let x4 = x2 * x2;
    let x6 = x4 * x2;
    var xn: f32;
    if (n > 3.0) {
        xn = pow(x, 2.0 * n);
    } else if (n >= 2.0) {
        xn = mix(x4, x6, n - 2.0);
    } else if (n >= 1.0) {
        xn = mix(x2, x4, n - 1.0);
    } else {
        xn = x2;
    }
    let core = exp(-0.6931472 * xn);
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
    // Frost is a strong diffuser — it blurs the gobo heavily.
    let sigma = k_ap * defocus * 230.0 + 3.0 * frost01;
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
// Where a beam fragment lands on a PHYSICAL ROTATING WHEEL DISC. The N slots sit
// at radius R around a hub below the gate; the disc spins by `position`·pitch so
// the gate looks onto a different part of the turning disc. Returns:
//   xy = the in-slot image UV (centred, upright when the slot is parked, and
//        rotated by the disc spin + the slot's own `rot` otherwise),
//   z  = wrapped slot index,
//   w  = holder mask (1 = inside a slot, 0 = metal between slots / outside the rim).
// Because the gate is OFF-CENTRE on the disc, a select move makes the projected
// slot ROTATE and translate (not a flat strip sliding by), and between two slots
// the curved edges of both neighbours show with the holder metal between them.
fn opt_disc(guv: vec2<f32>, position: f32, n: f32, gap: f32, rot: f32) -> vec4<f32> {
    let pitch = 6.2831853 / n;
    let rg = 0.5;                      // image radius: the slot fills the beam
    let rim = 0.62;                    // holder boundary — SET PAST the beam radius
                                       // (0.5) so a parked/open beam isn't radially
                                       // clipped to a black-edged circle; the metal
                                       // only shows in the angular gap between slots.
    // Hub distance SCALES with the slot count so adjacent slots keep a real metal
    // gap (centre spacing = 2·R·sin(pitch/2) = slot Ø·(1+gap_frac)) instead of
    // crowding edge-to-edge as N grows. gap_frac comes from the `gap` param
    // (gobo wheels ~0.5, colour wheels ~0.06 so colours nearly abut).
    let gap_frac = clamp(gap * 2.0, 0.04, 0.9);
    let R = clamp(rim * (1.0 + gap_frac) / sin(pitch * 0.5), 1.0, 4.0);
    let p = guv - vec2<f32>(0.5, 0.5); // beam-local (radius 0.5)
    let ang = position * pitch;        // disc rotation
    let ca = cos(ang);
    let sa = sin(ang);
    let v = p + vec2<f32>(0.0, R);     // relative to the hub at (0, -R)
    let w = vec2<f32>(ca * v.x - sa * v.y, sa * v.x + ca * v.y); // v rotated by +ang
    let theta = atan2(w.x, w.y);       // angle from the +y (gate) axis
    let slot = floor(theta / pitch + 0.5);
    let slot_w = slot - floor(slot / n) * n;
    let sth = slot * pitch;
    let w_s = vec2<f32>(R * sin(sth), R * cos(sth)); // slot centre on the disc
    let d = w - w_s;
    // Soft holder mask: 1 inside the slot, fading to 0 (metal) past the rim. Soft
    // so the gap reads as a real out-of-focus spoke, not a hard black line; the rim
    // sits outside the beam so a parked/open slot stays a clean disc.
    let inside = 1.0 - smoothstep(rim - 0.08, rim, length(d));
    // express the offset in the slot's image frame (upright when parked) + own spin
    let gth = -sth + rot;
    let gc = cos(gth);
    let gs = sin(gth);
    let gd = vec2<f32>(gc * d.x - gs * d.y, gs * d.x + gc * d.y);
    let uv = clamp(gd / (2.0 * rg) + vec2<f32>(0.5, 0.5), vec2<f32>(0.0), vec2<f32>(1.0));
    return vec4<f32>(uv.x, uv.y, slot_w, inside);
}

fn opt_wheel(
    tex: texture_2d_array<f32>, samp: sampler, guv: vec2<f32>,
    base: f32, position: f32, n: f32, gap: f32, rot: f32, lod: f32,
) -> vec4<f32> {
    if (base < 0.0 || n < 1.0) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    if (n < 1.5) {
        return opt_layer(tex, samp, guv, i32(base), rot, lod); // single slot
    }
    let disc = opt_disc(guv, position, n, gap, rot);
    let g = textureSampleLevel(tex, samp, disc.xy, clamp(i32(base + disc.z), 0, ATLAS_MAX_LAYER), lod);
    // disc.w (soft holder) fades the slot out into the metal gap between gobos.
    return vec4<f32>(g.rgb, g.a * disc.w);
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
    if (n < 1.5) {
        return textureSampleLevel(tex, samp, vec2<f32>(0.5, 0.5), i32(base), 0.0).rgb;
    }
    // Same physical rotating disc as the gobo wheel, so the colour sectors CURVE as
    // the wheel turns. The chosen slot's dichroic band is read; near the spoke the
    // colour darkens softly toward the divider (a thin out-of-focus edge, not a hard
    // black line — and still sharper than the CMY flags, which sit further off-gate).
    let disc = opt_disc(guv, position, n, gap, 0.0);
    let su = (disc.z + 0.5) / n; // centre of the slot's band
    let col = textureSampleLevel(tex, samp, vec2<f32>(su, 0.5), i32(base), 0.0).rgb;
    let r = length(disc.xy - vec2<f32>(0.5, 0.5)) * 2.0; // 0 centre … ~1 at the spoke
    let edge = smoothstep(0.72, 1.0, r);
    return col * (1.0 - 0.4 * edge);
}

// One graduated dichroic CMY flag's coating density at axis-position `u` (−1..1)
// for insertion `c` (0 clear … 1 full). A real CMY flag is a GRADUATED dichroic —
// a smooth clear→saturated coating, not a hard slot. So this is a continuous ramp
// with NO `cover` step: at a partial insertion the beam sees a gradient across its
// width (denser toward the deep edge); at c=1 the dense end covers the whole
// aperture (uniform full). `G` sets the gradient span (smaller = longer, softer).
fn opt_cmy_flag(u: f32, c: f32) -> f32 {
    const G: f32 = 0.62;
    return clamp((1.0 + 2.0 * G) * c - G * u - G, 0.0, 1.0);
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
// Mechanical SHUTTER blades closing across the aperture. Two blades close from the
// top and bottom; `close` 0 = open … 1 = shut, `kind` 1 = straight blade / 2 =
// sawtooth edge, `soft` = edge softness (small = crisp blade, used for narrow beam
// fixtures whose gate images sharply; large = blurred away on wide washes). The lit
// centre stays full; the band under a blade darkens with a visible edge — so a
// partly-closed shutter shows the blade outline on the beam + floor pool.
// `p` = beam cross-section in the unit disc = (guv-0.5)*2.
fn opt_shutter(p: vec2<f32>, close: f32, kind: f32, soft: f32) -> f32 {
    if (kind < 0.5 || close < 0.004) {
        return 1.0;
    }
    let hw = clamp(soft, 0.05, 1.3);   // blur half-width (heavy → near-uniform dim)
    // TWO blades close symmetrically toward the centre (rotated 90° → along x),
    // like the real two-blade dimmer/shutter: the LIT band is |x| < t, with t
    // shrinking 1→0 as close 0→1, so the aperture pinches in from both edges.
    var t = mix(1.0 + hw, -hw, close);
    if (kind > 1.5) {                  // sawtooth: a few BIG teeth on the blade edge
        let tri = abs(fract(p.y * 2.0) * 2.0 - 1.0); // ~2 teeth across the beam
        t = t + (tri - 0.5) * 0.30;                   // big amplitude (few, big teeth)
    }
    let under = smoothstep(t - hw, t + hw, abs(p.x)); // 0 lit centre … 1 covered edge
    // Blend with a uniform dim so it reads as near-perfect smooth dimming with a
    // soft two-blade artifact (centre lit, both edges pinching in symmetrically).
    return mix(1.0 - close, 1.0 - under, 0.4);
}

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
