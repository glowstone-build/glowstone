// mask.wgsl — selection silhouette.
//
// Two halves:
//  1. The MASK pass writes constant 1.0 into a full-res R8 target for every
//     fragment of a SELECTED object's opaque surface, depth-TESTED (LessEqual,
//     no write) against the scene depth so an occluded part of the object does
//     not mark the mask — the outline then hugs only the visible silhouette.
//     Two vertex entries reuse the existing instance buffers verbatim:
//       - `vs_mesh`: MeshInstance (fixtures, scene geometry); `selected` scalar.
//       - `vs_wall`: WallInstance (LED screens); `selected` is look.w.
//     A fragment that is NOT selected is discarded, so the same draw calls as the
//     forward pass can be re-issued without splitting out the selected subset.
//  2. The COMPOSITE pass (fullscreen triangle) edge-detects that mask and adds a
//     bright amber ring into the HDR target wherever the center texel is unset but
//     a neighbour is set — i.e. just OUTSIDE each selected silhouette.

// ----------------------------------------------------------------------------
// Mask pass
// ----------------------------------------------------------------------------

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    render_mode: vec4<f32>,
    world: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// MeshInstance layout (matches mesh.wgsl): model rows + color/intensity/selected.
struct MeshVsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) emissive: f32,
    @location(5) model_0: vec4<f32>,
    @location(6) model_1: vec4<f32>,
    @location(7) model_2: vec4<f32>,
    @location(8) model_3: vec4<f32>,
    @location(9) color: vec3<f32>,
    @location(10) intensity: f32,
    @location(11) selected: f32,
};

// WallInstance layout (matches wall.wgsl): the curvature bend lives in extra.z and
// the selected flag in look.w. We reproduce the arc bend so the wall silhouette
// matches the lit pass exactly.
struct WallVsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) emissive: f32,
    @location(5) m0: vec4<f32>,
    @location(6) m1: vec4<f32>,
    @location(7) m2: vec4<f32>,
    @location(8) m3: vec4<f32>,
    @location(9) grid: vec4<f32>,
    @location(10) color: vec4<f32>,
    @location(11) look: vec4<f32>,
    @location(12) extra: vec4<f32>,
};

struct MaskVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) selected: f32,
};

@vertex
fn vs_mesh(in: MeshVsIn) -> MaskVsOut {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    var out: MaskVsOut;
    out.clip_position = camera.view_proj * (model * vec4<f32>(in.position, 1.0));
    out.selected = in.selected;
    return out;
}

@vertex
fn vs_wall(in: WallVsIn) -> MaskVsOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    var lp = in.position;
    let theta = in.extra.z;
    if (abs(theta) > 1e-4) {
        let r = 1.0 / theta;
        let phi = in.position.x * theta;
        lp = vec3<f32>(r * sin(phi), in.position.y, r * (cos(phi) - 1.0));
    }
    var out: MaskVsOut;
    out.clip_position = camera.view_proj * (model * vec4<f32>(lp, 1.0));
    out.selected = in.look.w;
    return out;
}

@fragment
fn fs_mask(in: MaskVsOut) -> @location(0) vec4<f32> {
    // Only selected surfaces mark the mask; everything else is discarded so the
    // (depth-tested) draw leaves those texels at the cleared 0.
    if (in.selected < 0.5) {
        discard;
    }
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}

// ----------------------------------------------------------------------------
// Composite pass (edge-detect → amber outline added into the HDR target)
// ----------------------------------------------------------------------------

struct FsVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vid: u32) -> FsVsOut {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let xy = corners[vid];
    var out: FsVsOut;
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 1.0 - (xy.y * 0.5 + 0.5));
    return out;
}

@group(0) @binding(0) var mask_tex: texture_2d<f32>;
@group(0) @binding(1) var mask_samp: sampler;

// Amber selection accent, pushed >1 so it reads as a glow through bloom.
const ACCENT: vec3<f32> = vec3<f32>(1.0, 0.62, 0.12);

// Add the outline into the HDR scene (blend One/One on the pipeline). The ring is
// where the CENTER texel is outside a silhouette (mask 0) but some neighbour within
// ~2 px is inside it (mask > 0) — i.e. a ~2 px band hugging each selected object.
@fragment
fn fs_outline(in: FsVsOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(mask_tex, 0));
    let t = 1.0 / dims;
    let center = textureSampleLevel(mask_tex, mask_samp, in.uv, 0.0).r;
    if (center > 0.5) {
        // Inside the silhouette → no outline (keep the surface fill subtle/separate).
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // Max of the 8-neighbour ring at radius ~2 px: any selected texel nearby → edge.
    var nbr = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 2) {
        for (var dx = -2; dx <= 2; dx = dx + 2) {
            if (dx == 0 && dy == 0) { continue; }
            let uv = in.uv + vec2<f32>(f32(dx) * t.x, f32(dy) * t.y);
            nbr = max(nbr, textureSampleLevel(mask_tex, mask_samp, uv, 0.0).r);
        }
    }
    let edge = step(0.5, nbr);
    return vec4<f32>(ACCENT * edge * 1.5, 1.0);
}
