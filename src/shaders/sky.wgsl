// sky.wgsl — draws the world HDRI as the scene background. A fullscreen triangle
// reconstructs the per-pixel world ray from the inverse view-projection and
// samples the equirectangular environment map. Drawn first in the forward pass
// (depth-compare Always, no depth write) so opaque geometry overdraws it.

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    render_mode: vec4<f32>,
    world: vec4<f32>,       // x = brightness, y = rotation, z = ambient, w = has-HDRI
    inv_view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var world_tex: texture_2d<f32>;
@group(1) @binding(1) var world_samp: sampler;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;

fn rotate_y(d: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec3<f32>(c * d.x + s * d.z, d.y, -s * d.x + c * d.z);
}
fn equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let u = atan2(d.x, -d.z) / TAU + 0.5;
    let v = acos(clamp(d.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Oversized fullscreen triangle.
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var out: VsOut;
    out.ndc = p[vi];
    out.clip = vec4<f32>(p[vi], 1.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Reconstruct the world-space ray through this pixel.
    let near = camera.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let far = camera.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize(far.xyz / far.w - near.xyz / near.w);
    let d = rotate_y(dir, camera.world.y);
    let col = textureSampleLevel(world_tex, world_samp, equirect_uv(d), 0.0).rgb;
    return vec4<f32>(col * camera.world.x, 1.0);
}
