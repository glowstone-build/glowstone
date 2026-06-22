// mesh.wgsl — instanced, lit triangle meshes (the diffuse floor + fixtures),
// illuminated by a soft key light AND by every fixture's spotlight (so the
// floor catches the beam pool). Vertices flagged `emissive` (a fixture lens)
// glow in the instance color instead of being shaded.

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    render_mode: vec4<f32>, // x: 0 = beauty (lit), 1 = unlit/flat, 2 = wireframe
    world: vec4<f32>,       // x = brightness, y = rotation, z = ambient, w = has-HDRI
    inv_view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

// World/HDRI environment map (equirectangular, with mips for blurred IBL ambient).
@group(2) @binding(0) var world_tex: texture_2d<f32>;
@group(2) @binding(1) var world_samp: sampler;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;

// Rotate a direction about +Y (world environment yaw).
fn world_rotate_y(d: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec3<f32>(c * d.x + s * d.z, d.y, -s * d.x + c * d.z);
}

// World direction → equirectangular UV. v=0 at +Y (zenith), v=1 at -Y.
fn world_equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let u = atan2(d.x, -d.z) / TAU + 0.5;
    let v = acos(clamp(d.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

// Image-based ambient irradiance in direction `n` (sampled from a blurred high
// mip of the env map), pre-scaled by brightness × ambient.
fn world_ambient(n: vec3<f32>) -> vec3<f32> {
    let d = world_rotate_y(normalize(n), camera.world.y);
    let uv = world_equirect_uv(d);
    let dims = textureDimensions(world_tex, 0);
    let max_lod = log2(f32(max(dims.x, dims.y)));
    // Sample a heavily-blurred mip → approximate diffuse irradiance.
    let irr = textureSampleLevel(world_tex, world_samp, uv, max_lod - 2.0).rgb;
    return irr * camera.world.x * camera.world.z;
}

// Fixtures as disc spotlights (mirrors FixtureGpu / the volumetric `Fixture`).
// The array length comes from the sized buffer binding.
struct Light {
    pos_range: vec4<f32>, // xyz = lens, w = range
    dir_cos: vec4<f32>,   // xyz = beam dir, w = tan(half zoom angle)
    color: vec4<f32>,     // rgb = tint*intensity*candela*shutter, w = lens radius
    cookie_r: vec4<f32>,  // xyz = lens-plane right, w = wheel buffer offset
    cookie_u: vec4<f32>,  // xyz = lens-plane up,    w = wheel count (dynamic chain)
    extra: vec4<f32>,     // x = anim layer (<0 none), y = anim scroll, z/w = unused
    shape: vec4<f32>,     // x = super-Gaussian order, y = focus dist, z = iris frac, w = frost
    misc: vec4<f32>,      // x = CA strength, y = laser flag, z = atlas count, w = shadow layer
    cmyf: vec4<f32>,      // CMY flag insertions c,m,y, unused
};

@group(1) @binding(0)
var<storage, read> lights: array<Light>;
@group(1) @binding(1) var gobo_tex: texture_2d_array<f32>;
@group(1) @binding(2) var gobo_samp: sampler;
// Hero-beam shadow maps (a beam with misc.w >= 0 has a layer here).
@group(1) @binding(3) var shadow_atlas: texture_depth_2d_array;
@group(1) @binding(4) var shadow_samp: sampler_comparison;
@group(1) @binding(5) var<storage, read> shadow_mats: array<mat4x4<f32>>;
// Per-fixture wheel chain (dynamic count); shared with the volumetric pass.
@group(1) @binding(6) var<storage, read> wheels: array<WheelGpu>;

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
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    // Two-sided shading: imported .3ds (and some GLB) geometry has unreliable
    // winding and is drawn with culling off, so back faces would otherwise read
    // as flat-dark. Flip the normal to face the camera for the lighting math.
    var normal = normalize(in.world_normal);
    if !front {
        normal = -normal;
    }

    // Unlit / wireframe viewport modes: skip all fixture/beam lighting, show the
    // raw geometry. (The volumetric pass is skipped on the CPU side for these.)
    let mode = camera.render_mode.x;
    if (mode > 1.5) {
        // Wireframe (line polygon mode): bright flat so edges read clearly.
        return vec4<f32>(in.color * 1.4 + vec3<f32>(0.12, 0.12, 0.14), 1.0);
    }
    if (mode > 0.5) {
        // Unlit: flat albedo (lifted for readability); emissive surfaces still glow.
        let flat = in.color * 1.1 + vec3<f32>(0.05, 0.05, 0.06);
        let emit = in.color * max(in.intensity, 0.2) * 24.0;
        return vec4<f32>(mix(flat, emit, clamp(in.emissive, 0.0, 1.0)), 1.0);
    }

    // Ambient fill: an HDRI world lights the geometry (image-based ambient), else
    // a faint flat key so set geometry stays readable in the dark void where no
    // beam reaches (previz wants to see the rig, not a pure blackout).
    var ambient: vec3<f32>;
    if (camera.world.w > 0.5) {
        ambient = world_ambient(normal);
    } else {
        let key_dir = normalize(vec3<f32>(0.4, 1.0, 0.6));
        ambient = vec3<f32>(0.03 + max(dot(normal, key_dir), 0.0) * 0.05);
    }

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
        // Same lossless radial pre-cull as the volumetric pass (keeps the floor
        // pool in lock-step with the beam shaft): skip the optics chain for samples
        // past where the radial falls under the 0.002 gate for any valid n_order
        // (≥ 1.2), with +|ca| margin for the chromatic side-samples.
        // Widen the cull as the edge softens (low n_order tail) so it stays lossless.
        let cull = beam_r * (2.5 + abs(lt.misc.x) + (2.0 - clamp(lt.shape.x, 1.0, 2.0)));
        if (pu * pu + pv * pv > cull * cull) {
            continue;
        }
        let rad3 = opt_radial_ca(pu, pv, beam_r, lt.shape.x, lt.misc.x);
        let rad_max = max(rad3.x, max(rad3.y, rad3.z));
        if (rad_max <= 0.002) {
            continue;
        }
        // Plain-beam fast-path (kept in lock-step with volumetric.wgsl): a plain
        // wash / pixel-bar cell (extra.z = -1 sentinel — never a real shutter_close
        // ≥ 0) has no gobo/anim/CMY/blade, so the projected-cookie chain returns
        // identity. Skip it for the floor pool too.
        let plain = lt.extra.z < -0.5;
        var trans = vec3<f32>(1.0, 1.0, 1.0);
        if (!plain) {
            let guv = opt_project(rel, depth, lt.cookie_r.xyz, lt.cookie_u.xyz, cone_r);
            // Aperture-dependent DoF LOD (tan_half, iris, lens_r). The floor pool is
            // a stable surface projection, so sharpen the in-focus cookie fully.
            let lod = opt_lod(depth, lt.shape.y, lt.shape.w, lt.dir_cos.w, lt.shape.z, lt.color.w);
            trans = opt_cookie(
                gobo_tex, gobo_samp, guv,
                lt.cookie_r.w, lt.cookie_u.w,
                lt.extra.x, lt.extra.y, lt.cmyf.xyz, lod, camera.render_mode.y,
            ) * opt_shutter((guv - vec2<f32>(0.5)) * 2.0, lt.extra.z, lt.extra.w, lt.cmyf.w);
            if (max(trans.r, max(trans.g, trans.b)) <= 0.001) {
                continue;
            }
        }
        let l = normalize(lpos - in.world_pos);
        // Half-Lambert wrap (not hard N·L): a surface edge-on to the beam (a wall or
        // riser under a downward wash) still catches the projected gobo/pool instead
        // of going black, so the light reads on set geometry, not just the up-facing
        // floor. The cookie/cone gates above already decide WHERE the light lands;
        // this only softens how brightness falls off with surface facing.
        let ndl = clamp(dot(normal, l) * 0.5 + 0.5, 0.0, 1.0);
        let atten = 1.0 / (1.0 + depth * depth * 0.015);
        // Hero beams cast shadows: occlude this surface point if geometry blocks it.
        var vis = 1.0;
        let sidx = i32(lt.misc.w);
        if (sidx >= 0) {
            vis = opt_shadow(in.world_pos, shadow_mats[sidx], shadow_atlas, shadow_samp, sidx);
        }
        // Per-cell HDR whiten + boost (accuracy) — same as the shaft, so the floor
        // pool agrees: bright pixel cells read brighter/whiter, coloured stay saturated.
        let white01 = select(0.0, lt.extra.w, plain);
        let lum = max(lt.color.r, max(lt.color.g, lt.color.b));
        let whitened = mix(lt.color.rgb, vec3<f32>(lum), white01 * white01 * 0.6);
        let boost = 1.0 + white01 * white01 * 0.6;
        fixture_light += whitened * trans * (rad3 * (ndl * atten * 9.0 * vis * boost));
    }

    let albedo = in.color;
    var lit = albedo * ambient * max(in.intensity, 0.08);
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
