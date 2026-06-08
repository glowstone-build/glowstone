// ssao.wgsl — cheap depth-only screen-space ambient occlusion. Run as a
// fullscreen pass that MULTIPLY-blends an occlusion factor onto the HDR target in
// the Unlit viewport mode, so the otherwise-flat albedo geometry gains contact /
// crevice shading and shapes read. Depth-only (no normal buffer, no matrices):
// a pixel is occluded when nearby pixels sit in FRONT of it within a small world
// window — which darkens concave corners, contacts and creases.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
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

struct Ao {
    // x = near, y = far, z = world-radius in pixels at 1 m, w = intensity.
    params: vec4<f32>,
};

@group(0) @binding(0) var depth_tex: texture_depth_2d;
@group(0) @binding(1) var<uniform> ao: Ao;

// wgpu depth is [0,1] (reverse-Z-free perspective); recover positive view-space Z.
fn linear_z(d: f32, near: f32, far: f32) -> f32 {
    return near * far / (far - d * (far - near));
}

fn ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * p.x + 0.00583715 * p.y));
}

const PI2: f32 = 6.28318530718;
const N: i32 = 16;

@fragment
fn fs_ssao(in: VsOut) -> @location(0) vec4<f32> {
    let dimsf = vec2<f32>(textureDimensions(depth_tex));
    let maxp = vec2<i32>(dimsf) - vec2<i32>(1, 1);
    let pix = clamp(vec2<i32>(in.uv * dimsf), vec2<i32>(0, 0), maxp);
    let dc = textureLoad(depth_tex, pix, 0);
    if (dc >= 0.9999) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0); // background sky: no AO
    }
    let near = ao.params.x;
    let far = ao.params.y;
    let zc = linear_z(dc, near, far);
    // World radius -> screen px, shrinking with distance so it stays ~constant size.
    let radius = clamp(ao.params.z / zc, 3.0, 96.0);
    // Depth window (metres) that counts as a cavity rather than a separate object.
    let range = max(zc * 0.06, 0.25);

    let a = ign(in.pos.xy) * PI2;
    let ca = cos(a);
    let sa = sin(a);
    var occ = 0.0;
    for (var i = 0; i < N; i = i + 1) {
        let t = (f32(i) + 0.5) / f32(N);
        let ang = t * PI2 * 3.0;             // a few turns -> spiral disc, not a ring
        let r = radius * sqrt(t);            // even area coverage
        let d0 = vec2<f32>(cos(ang), sin(ang));
        let dir = vec2<f32>(ca * d0.x - sa * d0.y, sa * d0.x + ca * d0.y);
        let p2 = clamp(pix + vec2<i32>(dir * r), vec2<i32>(0, 0), maxp);
        let z2 = linear_z(textureLoad(depth_tex, p2, 0), near, far);
        let diff = zc - z2;                  // neighbour in front => occluder
        // count it when in front, fade out big gaps (silhouettes of far-front objects)
        let w = step(0.02, diff) * (1.0 - smoothstep(range, range * 3.0, diff));
        occ += w * clamp(diff / range, 0.0, 1.0);
    }
    let ao_factor = 1.0 - clamp(occ / f32(N) * ao.params.w, 0.0, 1.0);
    return vec4<f32>(vec3<f32>(ao_factor), 1.0);
}
