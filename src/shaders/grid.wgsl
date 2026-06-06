// grid.wgsl — flat, per-vertex-colored line geometry (ground grid + world axes).
//
// Drawn as a LineList. The whole grid + axes is uploaded once as a single
// vertex buffer; color is carried per-vertex so the same pipeline draws the
// dim grid lines and the bright RGB axes without a branch.

struct Camera {
    view_proj: mat4x4<f32>,
    // xyz = world-space eye position, w unused (kept for 16-byte alignment).
    eye: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_position = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
