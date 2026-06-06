// mesh.wgsl — instanced, lit triangle meshes (the diffuse floor + fixtures),
// illuminated by a soft key light AND by every fixture's spotlight (so the
// floor catches the beam pool). Vertices flagged `emissive` (a fixture lens)
// glow in the instance color instead of being shaded.

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// Fixtures as disc spotlights (mirrors FixtureGpu / the volumetric `Fixture`).
// The array length comes from the sized buffer binding.
struct Light {
    pos_range: vec4<f32>, // xyz = lens, w = range
    dir_cos: vec4<f32>,   // xyz = beam dir, w = tan(half-angle)
    color: vec4<f32>,     // rgb = color * intensity, w = lens radius
};

@group(1) @binding(0)
var<storage, read> lights: array<Light>;

struct VsIn {
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

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) intensity: f32,
    @location(3) selected: f32,
    @location(4) emissive: f32,
    @location(5) world_pos: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    let world_position = model * vec4<f32>(in.position, 1.0);

    var out: VsOut;
    out.clip_position = camera.view_proj * world_position;
    out.world_normal = (model * vec4<f32>(in.normal, 0.0)).xyz;
    out.color = in.color;
    out.intensity = in.intensity;
    out.selected = in.selected;
    out.emissive = in.emissive;
    out.world_pos = world_position.xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let normal = normalize(in.world_normal);

    // Soft key light + ambient fill (kept dim so fixtures dominate the look).
    let key_dir = normalize(vec3<f32>(0.4, 1.0, 0.6));
    let key = 0.09 + max(dot(normal, key_dir), 0.0) * 0.28;

    // Illumination from every fixture spotlight reaching this surface point.
    var fixture_light = vec3<f32>(0.0);
    let n_lights = arrayLength(&lights);
    for (var i = 0u; i < n_lights; i = i + 1u) {
        let lt = lights[i];
        let lpos = lt.pos_range.xyz;
        let bdir = lt.dir_cos.xyz;
        let tan_half = lt.dir_cos.w;
        let lens_r = lt.color.w;

        let rel = in.world_pos - lpos;
        let depth = dot(rel, bdir);
        if (depth < 0.0 || depth > lt.pos_range.w) {
            continue;
        }
        // Same disc-beam footprint as the volumetric pass, so the lit pool on
        // the floor lines up with the beam.
        let axis_pt = lpos + bdir * depth;
        let axis_dist = length(in.world_pos - axis_pt);
        let beam_r = lens_r + depth * tan_half;
        let radial = smoothstep(beam_r, beam_r * 0.45, axis_dist);
        if (radial <= 0.0) {
            continue;
        }
        let l = normalize(lpos - in.world_pos);
        let ndl = max(dot(normal, l), 0.0);
        let atten = 1.0 / (1.0 + depth * depth * 0.04);
        fixture_light += lt.color.rgb * (radial * ndl * atten * 3.0);
    }

    let albedo = in.color;
    var lit = albedo * key * max(in.intensity, 0.08);
    lit += albedo * fixture_light;

    // Self-illuminated surface (a fixture lens): a bright bloomy disc (HDR,
    // well above 1 so it blooms strongly like a real lamp face).
    let emit = in.color * max(in.intensity, 0.2) * 14.0;
    var rgb = mix(lit, emit, clamp(in.emissive, 0.0, 1.0));

    // Selection highlight: additive amber rim on shaded surfaces.
    let highlight = in.selected * 0.4 * (1.0 - in.emissive);
    rgb = rgb + vec3<f32>(1.0, 0.75, 0.2) * highlight * 0.3;

    return vec4<f32>(rgb, 1.0);
}
