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
@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    // Light 5-tap cross over the half-res raymarch before upsample. With the
    // deterministic MIDPOINT march (volumetric.wgsl) the half-res buffer is already
    // smooth (no per-pixel jitter noise), so this is centre-heavy (12:1) — just
    // enough to take the edge off the half-res→full-res stair-step WITHOUT the wide
    // bleed of a full Gaussian, which haloed the lit shaft past occluder silhouettes
    // (the "outline on the far side of a solid object").
    let t = 1.0 / vec2<f32>(textureDimensions(src_tex, 0));
    var s = textureSample(src_tex, src_samp, in.uv) * 12.0;
    s += textureSample(src_tex, src_samp, in.uv + vec2<f32>(t.x, 0.0));
    s += textureSample(src_tex, src_samp, in.uv - vec2<f32>(t.x, 0.0));
    s += textureSample(src_tex, src_samp, in.uv + vec2<f32>(0.0, t.y));
    s += textureSample(src_tex, src_samp, in.uv - vec2<f32>(0.0, t.y));
    return s / 16.0;
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
