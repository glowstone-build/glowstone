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
    @location(11) shutter: vec4<f32>, // x = close, y = kind, z = soft, w = unused
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
    @location(7) shutter: vec4<f32>,
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
    out.shutter = in.shutter;
    return out;
}

const LN2: f32 = 0.6931472;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = length(in.local);
    // Aperture mask: a disc for round lenses, a rounded rectangle for LED
    // strips/panels (shutter.w = 1). The billboard already carries the real W×H
    // scale, so `local` is normalised to the face. Crisp edge — the soft glow is
    // bloom, not a soft alpha (a soft edge reads as cotton).
    var edge: f32;
    if (in.shutter.w > 0.5) {
        // Rounded-rect signed distance in normalised [-1,1] face space.
        let cr = 0.12;
        let q = abs(in.local) - vec2<f32>(1.0 - cr, 1.0 - cr);
        let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - cr;
        if (d > 0.0) { discard; }
        edge = clamp(-d / max(fwidth(d), 1e-4), 0.0, 1.0);
    } else {
        if (r > 1.0) { discard; }
        edge = clamp((1.0 - r) / max(fwidth(r), 1e-4), 0.0, 1.0);
    }

    let view = normalize(camera.eye.xyz - in.world_pos);
    let cosv = dot(in.axis, view); // >0 = camera on the emitting side

    // --- dark glass base (always present; all a dead cell shows) ---
    let fres = pow(1.0 - abs(cosv), 3.0);
    var rgb = vec3<f32>(0.015 + 0.06 * fres);

    let level = in.color.w;
    if (level > 1e-4) {
        // Omnidirectional "this emitter is ON" glow. A real diffuse LED front
        // reads as lit across its whole front hemisphere (brightest head-on,
        // fading to grazing) and even hints from behind — NOT only when the eye
        // sits inside the fixture's narrow beam cone. Candela-independent, so a
        // wash/bar/LED-array shows its colour from any angle; the concentrated
        // in-beam blast below adds on top for spots. Pushed well into HDR (≫1) so
        // the source disc CLIPS to a hot core and BLOOMS into a visible halo —
        // not a dim disc lost against the background.
        var face = 3.0 + 34.0 * smoothstep(0.0, 0.3, cosv);

        if (cosv > 0.0) {
            let tan_half = max(in.params.x, 1e-3);
            let n_order = max(in.params.y, 1.2);
            let candela = clamp(in.params.z, 0.25, 16.0);

            // Reciprocity gate: the face hits FULL luminance exactly where the
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
            let profile = max(gate * limb, die * mix(0.3, 1.0, gate) * limb) * ring;

            // HDR anchor: a key-lit stage surface sits near 1; an in-beam lens
            // face is orders of magnitude above it and must clip to white through
            // the tonemap (that clip + bloom IS the "light source" look). Tighter
            // zoom concentrates the same flux → brighter face (∝ candela).
            let l0 = 60.0 * candela;
            face += l0 * profile;
        }
        rgb += in.color.rgb * (level * face);
    }

    // Mechanical shutter blade(s) across the lens face — the mechanism lives at
    // the gate, so it's visible on the glass (a thin parked sliver even when open,
    // more as it closes). Two blades from top + bottom; sawtooth edge if kind 2.
    let sk = in.shutter.y;
    if (sk > 0.5) {
        let bclose = in.shutter.x;
        let hw = clamp(in.shutter.z, 0.05, 1.3);
        var t = mix(1.0 + hw, -hw, bclose);     // open → past rim, shut → blackout
        if (sk > 1.5) {                              // sawtooth: a few BIG teeth
            let tri = abs(fract(in.local.y * 2.0) * 2.0 - 1.0);
            t = t + (tri - 0.5) * 0.30;
        }
        let under = smoothstep(t - hw, t + hw, abs(in.local.x)); // two blades, L+R, pinch to centre
        // Dim the lens face with the dimmer (uniform) + the soft blade artifact.
        rgb = rgb * mix(1.0 - bclose, 1.0 - under, 0.55);
    }

    return vec4<f32>(rgb * edge, 1.0);
}
