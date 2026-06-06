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
    dir_cos: vec4<f32>,   // xyz = beam dir, w = tan(half zoom angle)
    color: vec4<f32>,     // rgb = tint*intensity*candela*shutter, w = lens radius
    cookie_r: vec4<f32>,  // xyz = lens-plane right, w = gobo1 layer (frac; <0 none)
    cookie_u: vec4<f32>,  // xyz = lens-plane up,    w = gobo1 rotation (rad)
    extra: vec4<f32>,     // x = gobo2 layer, y = gobo2 rot, z = anim layer, w = anim scroll
    shape: vec4<f32>,     // x = super-Gaussian order, y = focus dist, z = iris frac, w = frost
    misc: vec4<f32>,      // x = CA strength, y/z/w = reserved
};

@group(1) @binding(0)
var<storage, read> lights: array<Light>;
@group(1) @binding(1) var gobo_tex: texture_2d_array<f32>;
@group(1) @binding(2) var gobo_samp: sampler;

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

    // Near-zero ambient: surfaces read dark like a blacked-out venue and are lit
    // almost entirely by the fixtures (the floor is dark except in beam pools).
    let key_dir = normalize(vec3<f32>(0.4, 1.0, 0.6));
    let key = 0.012 + max(dot(normal, key_dir), 0.0) * 0.03;

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
        // Same disc-beam footprint + cookie + falloff as the volumetric pass, so
        // the lit pool on the floor matches the beam shaft exactly.
        let pu = dot(rel, lt.cookie_r.xyz);
        let pv = dot(rel, lt.cookie_u.xyz);
        let cone_r = lens_r + depth * tan_half;
        let beam_r = cone_r * lt.shape.z;
        let rad3 = opt_radial_ca(pu, pv, beam_r, lt.shape.x, lt.misc.x);
        let rad_max = max(rad3.x, max(rad3.y, rad3.z));
        if (rad_max <= 0.002) {
            continue;
        }
        let guv = opt_project(rel, depth, lt.cookie_r.xyz, lt.cookie_u.xyz, cone_r);
        let lod = opt_lod(depth, lt.shape.y, lt.shape.w);
        let trans = opt_cookie_ca(
            gobo_tex, gobo_samp, guv,
            lt.cookie_r.w, lt.cookie_u.w, lt.extra.x, lt.extra.y,
            i32(lt.extra.z), lt.extra.w, lod, lt.misc.x,
        );
        if (max(trans.r, max(trans.g, trans.b)) <= 0.001) {
            continue;
        }
        let l = normalize(lpos - in.world_pos);
        let ndl = max(dot(normal, l), 0.0);
        let atten = 1.0 / (1.0 + depth * depth * 0.015);
        fixture_light += lt.color.rgb * trans * (rad3 * (ndl * atten * 9.0));
    }

    let albedo = in.color;
    var lit = albedo * key * max(in.intensity, 0.08);
    lit += albedo * fixture_light;

    // Self-illuminated surface (a fixture lens): a bright bloomy disc (HDR,
    // well above 1 so it blooms strongly like a real lamp face).
    let emit = in.color * max(in.intensity, 0.2) * 24.0;
    var rgb = mix(lit, emit, clamp(in.emissive, 0.0, 1.0));

    // Selection highlight: additive amber rim on shaded surfaces.
    let highlight = in.selected * 0.4 * (1.0 - in.emissive);
    rgb = rgb + vec3<f32>(1.0, 0.75, 0.2) * highlight * 0.3;

    return vec4<f32>(rgb, 1.0);
}
