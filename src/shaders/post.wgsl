// post.wgsl — HDR post chain: bloom (bright-pass + separable blur) and the
// final exposure → ACES tonemap → sRGB resolve into the LDR target egui samples.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
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
    // uv: 0 at top-left.
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 1.0 - (xy.y * 0.5 + 0.5));
    return out;
}

// --- bloom: single source texture + sampler ---
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
// Scene depth (full-res) for the depth-aware volumetric composite (fs_composite only).
@group(0) @binding(2) var comp_depth: texture_depth_2d;

// Bright pass: keep only the over-1.0 HDR energy (so only beams/lenses bloom).
@fragment
fn fs_bright(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src_tex, src_samp, in.uv).rgb;
    let luma = max(max(c.r, c.g), c.b);
    let knee = 0.6;
    let soft = max(luma - knee, 0.0) / max(luma, 1e-4);
    return vec4<f32>(c * soft, 1.0);
}

fn blur(uv: vec2<f32>, dir: vec2<f32>) -> vec3<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(src_tex, 0));
    let off = dir * texel;
    var w = array<f32, 5>(0.227027, 0.194595, 0.121622, 0.054054, 0.016216);
    var sum = textureSample(src_tex, src_samp, uv).rgb * w[0];
    for (var i = 1; i < 5; i = i + 1) {
        let o = off * f32(i);
        sum += textureSample(src_tex, src_samp, uv + o).rgb * w[i];
        sum += textureSample(src_tex, src_samp, uv - o).rgb * w[i];
    }
    return sum;
}

@fragment
fn fs_blur_h(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur(in.uv, vec2<f32>(1.0, 0.0)), 1.0);
}

@fragment
fn fs_blur_v(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur(in.uv, vec2<f32>(0.0, 1.0)), 1.0);
}

// Upsample the half-res volumetric target (scatter.rgb, transmittance.a). The
// pipeline blends it as  out = scatter + scene·transmittance  (One, SrcAlpha).
// DEPTH-AWARE upsample of the (temporally-accumulated) half-res volumetric. A 5-tap
// cross, but each side tap is weighted by how close its scene depth is to the centre
// pixel's: on the smooth beam interior all taps share a depth → full blend (softens the
// half-res→full-res stair-step); across a geometry silhouette the far-side taps get ~0
// weight → the beam edge stays CRISP against set/truss instead of haloing past it
// (joint-bilateral upsample, c0de517e/BO3). Depth is non-linear, so equal surfaces read
// near-identical while a near-vs-far jump reads large — exactly the discriminator wanted.
@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    let t = 1.0 / vec2<f32>(textureDimensions(src_tex, 0));
    let dd = vec2<f32>(textureDimensions(comp_depth));
    let hi = vec2<i32>(dd) - vec2<i32>(1);
    let dc = textureLoad(comp_depth, clamp(vec2<i32>(in.uv * dd), vec2<i32>(0), hi), 0);
    let c = textureSample(src_tex, src_samp, in.uv);
    // DESPECKLE+upsample of the half-res volumetric. The per-pixel blue-noise jitter, on
    // thin/oblique beams the ray grazes, makes each pixel a HIGH-CONTRAST outlier (it hits
    // the shaft vs misses it → bright-dot/dark-gap) — so a range/bilateral guard FAILS (it
    // can't tell that grain from a real edge; both jump by ~the beam brightness). Instead
    // detect grain GEOMETRICALLY: build a depth-weighted neighbourhood MEAN (depth weight →
    // never average across a geometry silhouette, so no halo past occluders) and pull each
    // pixel toward it PROPORTIONAL to how far it sits from that mean. Grain makes every
    // pixel an alternating outlier → it collapses to the local mean. A smooth broad beam or
    // gradual haze cluster has center ≈ mean → left UNCHANGED (the "nearly perfect" look is
    // preserved); only speckle and hard edges soften — the thin-beam case asked to approximate.
    // 5×5 depth-weighted neighbourhood mean (a single 3×3 only cuts grain ~3× — the
    // half-res FBM speckle needs a wider support to collapse).
    var m = c;
    var wsum = 1.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if (dx == 0 && dy == 0) { continue; }
            let uv = in.uv + vec2<f32>(f32(dx) * t.x, f32(dy) * t.y);
            let dt = textureLoad(comp_depth, clamp(vec2<i32>(uv * dd), vec2<i32>(0), hi), 0);
            let w = exp(-abs(dt - dc) * 300.0); // ~1 same surface, →0 across a depth edge
            m += textureSample(src_tex, src_samp, uv) * w;
            wsum += w;
        }
    }
    m = m / wsum;
    // Outlier strength: how far this pixel's luminance sits from the neighbourhood mean,
    // relative to it. ~0 in smooth regions (untouched); →1 on grain spikes/gaps (collapsed).
    let lc = max(c.r, max(c.g, c.b));
    let lm = max(m.r, max(m.g, m.b));
    let noisiness = clamp((abs(lc - lm) / (lm + 0.02)) * 2.6 - 0.05, 0.0, 1.0);
    return mix(c, m, noisiness);
}

// --- tonemap/resolve: HDR scene + bloom + a small uniform ---
struct Post {
    exposure: f32,
    bloom: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var hdr_tex: texture_2d<f32>;
@group(0) @binding(1) var bloom_tex: texture_2d<f32>;
@group(0) @binding(2) var post_samp: sampler;
@group(0) @binding(3) var<uniform> post: Post;

// Narkowicz ACES filmic approximation.
fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

// linear -> sRGB gamma (egui treats the user texture as gamma-encoded).
fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c < vec3<f32>(0.0031308));
}

@fragment
fn fs_tonemap(in: VsOut) -> @location(0) vec4<f32> {
    var col = textureSample(hdr_tex, post_samp, in.uv).rgb;
    let bloom = textureSample(bloom_tex, post_samp, in.uv).rgb;
    col += bloom * post.bloom;
    col *= post.exposure;
    col = aces(col);
    col = to_srgb(col);
    return vec4<f32>(col, 1.0);
}

// --- froxel volumetric composite (PREVIZ_FROXEL path) ---
// Upsamples the integrated froxel volume (accumulated scatter + transmittance,
// distributed exponentially along each pixel's ray) with ONE trilinear lookup at
// the opaque-surface depth, then blends scatter + scene·transmittance (the same
// One,SrcAlpha blend the half-res raymarch composite uses).
struct FroxelU {
    inv_view_proj: mat4x4<f32>,
    eye_time: vec4<f32>,
    fog_min_density: vec4<f32>,
    fog_max_g: vec4<f32>,
    albedo_beam: vec4<f32>,
    dims: vec4<f32>,
    planes: vec4<f32>, // x = near, y = far (distance along ray, m)
};
@group(0) @binding(0) var<uniform> fxu: FroxelU;
@group(0) @binding(1) var froxel_result: texture_3d<f32>;
@group(0) @binding(2) var froxel_samp: sampler;
@group(0) @binding(3) var fx_depth: texture_depth_2d;

@fragment
fn fs_froxel_composite(in: VsOut) -> @location(0) vec4<f32> {
    let near = fxu.planes.x;
    let far = fxu.planes.y;
    // Distance from eye to the opaque surface (or `far` if the sky/no surface).
    let dims = vec2<f32>(textureDimensions(fx_depth));
    let dpix = clamp(vec2<i32>(in.uv * dims), vec2<i32>(0), vec2<i32>(dims) - vec2<i32>(1));
    let d = textureLoad(fx_depth, dpix, 0);
    var t = far;
    if (d < 0.999999) {
        let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
        let wh = fxu.inv_view_proj * vec4<f32>(ndc, d, 1.0);
        t = length(wh.xyz / wh.w - fxu.eye_time.xyz);
    }
    // Inverse of the exponential slice distribution → normalised froxel Z.
    let zf = clamp(log(max(t, near) / near) / log(far / near), 0.0, 1.0);
    let r = textureSampleLevel(froxel_result, froxel_samp, vec3<f32>(in.uv, zf), 0.0);
    return r; // rgb = accumulated scatter, a = transmittance
}
