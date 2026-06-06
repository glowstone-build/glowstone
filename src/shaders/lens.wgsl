// lens.wgsl — the fixture's front lens: a glassy, dusty, self-illuminated disc
// placed at the beam exit and oriented to face along the beam. Up close it reads
// like a real lit lens (bright core, fresnel rim glow, dust speckle), in HDR so
// it blooms. Instanced with the same `MeshInstance` rows as the meshes.

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) emissive: f32,
    @location(5) m0: vec4<f32>,
    @location(6) m1: vec4<f32>,
    @location(7) m2: vec4<f32>,
    @location(8) m3: vec4<f32>,
    @location(9) color: vec3<f32>,
    @location(10) intensity: f32,
    @location(11) selected: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) wnormal: vec3<f32>,
    @location(3) color: vec3<f32>,
    @location(4) intensity: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    let wp = model * vec4<f32>(in.position, 1.0);
    var out: VsOut;
    out.clip = camera.view_proj * wp;
    out.local = in.position.xy;
    out.world_pos = wp.xyz;
    out.wnormal = (model * vec4<f32>(in.normal, 0.0)).xyz;
    out.color = in.color;
    out.intensity = in.intensity;
    return out;
}

fn hash21(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = length(in.local);
    if (r > 1.0) {
        discard;
    }
    let view = normalize(camera.eye.xyz - in.world_pos);
    let nv = abs(dot(normalize(in.wnormal), view));

    // Glassy emission: bright lit core fading out, a fresnel rim glow, and a
    // sprinkling of dust specks + fine grain frozen onto the glass.
    let core = mix(1.0, 0.22, smoothstep(0.0, 1.0, r));
    let fres = pow(1.0 - nv, 3.0);
    let cell = floor(in.local * 36.0);
    let speck = step(0.93, hash21(cell)) * (0.5 + hash21(cell + 3.7));
    let grain = 0.65 + 0.35 * hash21(in.local * 90.0);
    let glass = (core + fres * 1.4 + speck * 2.0) * grain;

    let edge = smoothstep(1.0, 0.9, r); // soft round rim
    let hdr = 9.0;
    let rgb = in.color * max(in.intensity, 0.0) * glass * edge * hdr;
    return vec4<f32>(rgb, 1.0);
}
