// vol_temporal.wgsl — temporal accumulation (EMA) for the half-res volumetric.
//
// The raymarch (volumetric.wgsl) jitters its ray start per-pixel AND per-frame, so a
// single frame is noisy/banded. This pass reprojects the previous accumulated result
// (camera-only matrix reprojection — we have no per-fixture motion vectors) and blends
// it with the current raw frame: out = mix(current, history, opacity). Over a few
// frames the jittered samples average into a smooth, "super-sampled" beam — the exact
// trick used by Frostbite/EEVEE/TLOU2 to kill banding without visible dither. The CPU
// drops `opacity` (params.x) to a low floor (~0.35) when the camera or any beam moves,
// and to 0 when there is no valid history (first frame, viewport resize, or a froxel-only
// frame), so moving beams don't ghost (an EMA on a SHARP per-pixel buffer doesn't
// blockify like the froxel grid did).

struct TemporalU {
    cur_inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    eye: vec4<f32>,    // xyz = eye, w = far distance (sky reprojection)
    params: vec4<f32>, // x = history opacity (0 = ignore history); yzw reserved
};

@group(0) @binding(0) var<uniform> tu: TemporalU;
@group(0) @binding(1) var vol_cur: texture_2d<f32>;   // raw raymarch this frame
@group(0) @binding(2) var vol_hist: texture_2d<f32>;  // previous accumulated EMA
@group(0) @binding(3) var hist_samp: sampler;         // linear (bilinear reproject)
@group(0) @binding(4) var scene_depth: texture_depth_2d;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>, // 0 at top-left
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
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 1.0 - (xy.y * 0.5 + 0.5));
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let vdim = vec2<f32>(textureDimensions(vol_cur));
    let vpix = clamp(vec2<i32>(uv * vdim), vec2<i32>(0), vec2<i32>(vdim) - vec2<i32>(1));
    let cur = textureLoad(vol_cur, vpix, 0);

    let opacity = tu.params.x;
    if (opacity <= 0.0) {
        return cur; // history invalid (cut / first frame) → trust this frame
    }

    // Reconstruct this pixel's world position from the scene depth, so the
    // reprojection follows surfaces the fog sits in front of. With no surface
    // (sky), use a far point along the ray (translation barely shifts it, so the
    // reprojection becomes camera-rotation-only — correct for a panning view).
    let ddim = vec2<f32>(textureDimensions(scene_depth));
    let dpix = clamp(vec2<i32>(uv * ddim), vec2<i32>(0), vec2<i32>(ddim) - vec2<i32>(1));
    let d = textureLoad(scene_depth, dpix, 0);
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    var world: vec3<f32>;
    if (d >= 0.999999) {
        let fh = tu.cur_inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
        let dir = normalize(fh.xyz / fh.w - tu.eye.xyz);
        world = tu.eye.xyz + dir * tu.eye.w;
    } else {
        let wh = tu.cur_inv_view_proj * vec4<f32>(ndc, d, 1.0);
        world = wh.xyz / wh.w;
    }

    // Project into the previous frame; reject anything that wasn't on screen
    // last frame (disocclusion) and fall back to the current sample there.
    let pc = tu.prev_view_proj * vec4<f32>(world, 1.0);
    if (pc.w <= 0.0) {
        return cur;
    }
    let puv = vec2<f32>(pc.x / pc.w * 0.5 + 0.5, 0.5 - pc.y / pc.w * 0.5);
    if (any(puv < vec2<f32>(0.0)) || any(puv > vec2<f32>(1.0))) {
        return cur;
    }
    let hist = textureSampleLevel(vol_hist, hist_samp, puv, 0.0);
    return mix(cur, hist, opacity);
}
