// shadow.wgsl — depth-only render of scene occluders from a hero beam's point of
// view, into one layer of the shared shadow atlas. The light view-projection for
// the current layer is supplied via a dynamic-offset uniform. No fragment shader:
// only depth is written, then sampled by mesh.wgsl (surfaces) and volumetric.wgsl
// (the beam shaft) so set pieces cast shadows and occlude beams mid-air.

@group(0) @binding(0) var<uniform> light_vp: mat4x4<f32>;

struct VsIn {
    @location(0) position: vec3<f32>,
    // Instance model matrix (same layout as MeshInstance; only the model is used).
    @location(5) model_0: vec4<f32>,
    @location(6) model_1: vec4<f32>,
    @location(7) model_2: vec4<f32>,
    @location(8) model_3: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    return light_vp * model * vec4<f32>(in.position, 1.0);
}
