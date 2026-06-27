// particles.wgsl — additive, velocity-stretched billboard SPARKS for the cold-
// spark fountains (emissive embers that bloom). CO2 is NOT here — it is dense
// participating media injected into the volumetric raymarch (volumetric.wgsl).

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    render_mode: vec4<f32>,
    world: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) corner: vec2<f32>,
    @location(1) color: vec4<f32>,
};

const CORNERS = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>( 1.0,  1.0),
    vec2<f32>(-1.0,  1.0),
);

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) pos_radius: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) vel_stretch: vec4<f32>,
    @location(3) aux: vec4<f32>,
) -> VsOut {
    let c = CORNERS[vi];
    let p = pos_radius.xyz;
    let r = max(pos_radius.w, 0.0001);
    let vel = vel_stretch.xyz;
    let stretch = max(vel_stretch.w, 1.0);

    let to_cam = normalize(camera.eye.xyz - p);
    let speed = length(vel);
    let moving = speed > 1e-3 && stretch > 1.0;
    let long_dir = select(vec3<f32>(0.0, 1.0, 0.0), vel / max(speed, 1e-4), moving);

    var right = cross(long_dir, to_cam);
    let rl = length(right);
    right = select(normalize(cross(vec3<f32>(1.0, 0.0, 0.0), to_cam)), right / max(rl, 1e-4), rl > 1e-4);

    let world = p + right * (c.x * r) + long_dir * (c.y * r * stretch);

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.corner = c;
    out.color = color;
    return out;
}

@fragment
fn fs_spark(in: VsOut) -> @location(0) vec4<f32> {
    let d = length(in.corner);
    let core = exp(-d * d * 6.0);
    let halo = exp(-d * d * 1.6) * 0.35;
    let fall = core + halo;
    let a = clamp(in.color.a * fall, 0.0, 1.0);
    return vec4<f32>(in.color.rgb * a, a);
}
