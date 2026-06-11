// lens.wgsl — emitter lens faces: one disc per LED cell / lens, placed at the
// beam exit and oriented along the beam.
//
// Physically-motivated model (étendue / luminance conservation — see
// .context/led-lens-rendering-research.md):
//  • A collimating lens trades angle for area: when the view ray lies INSIDE
//    the beam cone the whole aperture appears at near-die luminance (a
//    blinding uniform disc); outside it collapses ~50–500× to a scatter floor
//    (Fresnel/TIR leakage in the lens body). The angular gate is the SAME
//    super-Gaussian the beam uses → the face and the shaft agree by
//    reciprocity, and arrays "sparkle" as heads sweep across the camera.
//  • The LED die is visible through the lens as a hotspot whose image
//    magnifies to fill the aperture on-axis and slides opposite the view
//    direction off-axis (refraction parallax).
//  • Limb darkening + a faint collimator ring give the close-up structure.
//  • The aperture edge stays CRISP (AA by fwidth only) — the soft glow must
//    come from HDR bloom, not from a soft alpha edge (that reads as cotton).
//  • A dark cell is dark GLASS: near-black with a Fresnel rim, not grey.

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) emissive: f32,
    // LensInstance
    @location(5) m0: vec4<f32>,
    @location(6) m1: vec4<f32>,
    @location(7) m2: vec4<f32>,
    @location(8) m3: vec4<f32>,
    @location(9) color: vec4<f32>,  // rgb = cell color, w = level (0 = off)
    @location(10) params: vec4<f32>, // x = tan_half, y = n_order, z = candela, w = lens radius
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) axis: vec3<f32>,   // beam direction (disc normal)
    @location(3) right: vec3<f32>,
    @location(4) up: vec3<f32>,
    @location(5) color: vec4<f32>,
    @location(6) params: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    let wp = model * vec4<f32>(in.position, 1.0);
    var out: VsOut;
    out.clip = camera.view_proj * wp;
    out.local = in.position.xy;
    out.world_pos = wp.xyz;
    out.axis = normalize(in.m2.xyz);
    out.right = normalize(in.m0.xyz);
    out.up = normalize(in.m1.xyz);
    out.color = in.color;
    out.params = in.params;
    return out;
}

const LN2: f32 = 0.6931472;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = length(in.local);
    if (r > 1.0) {
        discard;
    }
    // Crisp aperture: anti-alias only over the last screen pixel.
    let edge = clamp((1.0 - r) / max(fwidth(r), 1e-4), 0.0, 1.0);

    let view = normalize(camera.eye.xyz - in.world_pos);
    let cosv = dot(in.axis, view); // >0 = camera on the emitting side

    // --- dark glass base (always present; all a dead cell shows) ---
    let fres = pow(1.0 - abs(cosv), 3.0);
    var rgb = vec3<f32>(0.015 + 0.06 * fres);

    let level = in.color.w;
    if (level > 1e-4 && cosv > 0.0) {
        let tan_half = max(in.params.x, 1e-3);
        let n_order = max(in.params.y, 1.2);
        let candela = clamp(in.params.z, 0.25, 16.0);

        // Reciprocity gate: the face is at full luminance exactly where the
        // beam would reach the eye — the beam's own super-Gaussian profile
        // evaluated at the view angle.
        let sinv = sqrt(max(1.0 - cosv * cosv, 0.0));
        let x = (sinv / max(cosv, 1e-3)) / tan_half;
        let gate = exp(-LN2 * pow(x, 2.0 * n_order));

        // Die image: parallax-shifted hotspot, magnifying to fill the
        // aperture on-axis. View direction projected into the lens plane
        // drives the shift (die sits behind the lens → image moves opposite).
        let v_lens = vec2<f32>(dot(view, in.right), dot(view, in.up));
        let die_c = -v_lens * 0.55 * (1.0 - 0.6 * gate);
        let die_r = mix(0.28, 1.15, gate);
        let die_d = length(in.local - die_c) / die_r;
        let die = exp(-LN2 * pow(die_d, 4.0));

        // Close-up structure: limb darkening toward the rim and a faint dark
        // ring where the collimator's central lenslet meets the TIR annulus.
        let limb = 1.0 - 0.22 * r * r;
        let ring = 1.0 - 0.08 * smoothstep(0.32, 0.44, r) * (1.0 - smoothstep(0.5, 0.68, r));

        // Scatter floor: TIR/Fresnel leakage lights the whole lens body a few
        // percent of full even far off-axis.
        let floor_g = 0.028 * (0.4 + 0.6 * cosv);

        // Aperture radiance profile: in-beam blast, else the brighter of the
        // die image (at ~30% die luminance off-axis) and the body glow.
        let profile = max(gate * limb, max(die * mix(0.3, 1.0, gate) * limb, floor_g)) * ring;

        // HDR anchor: a key-lit stage surface sits near 1; an in-beam lens
        // face is orders of magnitude above it and must clip to white through
        // the tonemap (that clip + bloom IS the "light source" look). Tighter
        // zoom concentrates the same flux → brighter face (∝ candela).
        let l0 = 60.0 * candela;
        rgb += in.color.rgb * (level * l0 * profile);
    } else if (level > 1e-4) {
        // Back side of a lit cell: the lens body still glows faintly.
        rgb += in.color.rgb * (level * 0.4);
    }

    return vec4<f32>(rgb * edge, 1.0);
}
