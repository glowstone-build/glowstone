// wall.wgsl — an LED video wall surface.
//
// The whole wall is ONE instance of a unit quad (never per-pixel geometry). The
// fragment shader synthesizes the content (solid colour or a procedural test
// pattern) and, crucially, a DISTANCE-AWARE LED PIXEL MASK: when the camera is
// far the wall reads as a continuous emissive surface, but as you zoom in the
// individual LED dots and the dark inter-pixel gaps resolve — exactly like a
// real panel. The reveal is driven by screen-space derivatives (fwidth) of the
// pixel coordinate, so it is correct at any zoom with no LOD popping.
//
// HDR + bloom do the "it's a light source" glow; this shader only writes the
// emissive colour. The wall's contribution to scene/volumetric lighting is a
// separate, cheap, blurred area-light injected on the CPU side (see record_scene).

struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// Per-wall content texture (image / NDI / CITP / pixel-map). Procedural walls
// (solid / test pattern) bind a 1×1 placeholder and ignore it.
@group(1) @binding(0) var content_tex: texture_2d<f32>;
@group(1) @binding(1) var content_samp: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) emissive: f32,
    // WallInstance
    @location(5) m0: vec4<f32>,
    @location(6) m1: vec4<f32>,
    @location(7) m2: vec4<f32>,
    @location(8) m3: vec4<f32>,
    @location(9) grid: vec4<f32>,   // x,y = res px; z,w = panels w,h
    @location(10) color: vec4<f32>, // rgb = solid/tint, w = nits HDR scale
    @location(11) look: vec4<f32>,  // x = kind, y = test idx, z = opacity, w = selected
    @location(12) extra: vec4<f32>, // x = gamma, y = seam frac, z = unused, w = time
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) grid: vec4<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) look: vec4<f32>,
    @location(4) @interpolate(flat) extra: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    // Bend the (tessellated) surface into a horizontal arc for curved walls. The
    // local quad is [-0.5,0.5]² in width-fraction units; total subtended angle =
    // curvature (extra.z, radians). Arc length stays the developed width, so the
    // chord narrows and the surface bows in local +Z (×width via surface_matrix).
    var lp = in.position;
    let theta = in.extra.z;
    if (abs(theta) > 1e-4) {
        let r = 1.0 / theta;            // local radius (width = 1 unit)
        let phi = in.position.x * theta;
        lp = vec3<f32>(r * sin(phi), in.position.y, r * (cos(phi) - 1.0));
    }
    let wp = model * vec4<f32>(lp, 1.0);
    var out: VsOut;
    out.clip = camera.view_proj * wp;
    out.uv = in.position.xy + vec2<f32>(0.5, 0.5); // local [-0.5,0.5] -> [0,1]
    out.grid = in.grid;
    out.color = in.color;
    out.look = in.look;
    out.extra = in.extra;
    return out;
}

fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(5.0, 3.0, 1.0);
    let p = abs(fract(vec3<f32>(h, h, h) + k / 6.0) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

// SMPTE-ish 7-bar colour bars.
fn color_bars(uv: vec2<f32>) -> vec3<f32> {
    let i = floor(uv.x * 7.0);
    let lvl = 0.75;
    if (i < 0.5) { return vec3<f32>(lvl); }
    if (i < 1.5) { return vec3<f32>(lvl, lvl, 0.0); }
    if (i < 2.5) { return vec3<f32>(0.0, lvl, lvl); }
    if (i < 3.5) { return vec3<f32>(0.0, lvl, 0.0); }
    if (i < 4.5) { return vec3<f32>(lvl, 0.0, lvl); }
    if (i < 5.5) { return vec3<f32>(lvl, 0.0, 0.0); }
    return vec3<f32>(0.0, 0.0, lvl);
}

// An alignment grid: light crosshatch on a dark field, with a magenta border.
fn grid_pattern(uv: vec2<f32>) -> vec3<f32> {
    let lines = 16.0;
    let cells = uv * lines;
    let d = min(fract(cells), 1.0 - fract(cells)); // distance to nearest gridline
    let dl = min(d.x, d.y);
    let aa = fwidth(dl) + 1e-4;
    let line = 1.0 - smoothstep(0.04, 0.04 + aa, dl);
    let field = vec3<f32>(0.015, 0.04, 0.11);
    let grid_col = vec3<f32>(0.70, 0.76, 0.88);
    var col = mix(field, grid_col, line);
    // coloured border so the panel extent is unambiguous
    let b = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
    col = mix(vec3<f32>(0.9, 0.1, 0.55), col, smoothstep(0.0, 0.012, b));
    return col;
}

fn content_color(look: vec4<f32>, color: vec4<f32>, uv: vec2<f32>) -> vec3<f32> {
    // Sample unconditionally (uniform control flow) — placeholder for procedural.
    // Flip V: texture row 0 is the top, but the wall's uv.y = 1 is the top edge.
    let tex = textureSample(content_tex, content_samp, vec2<f32>(uv.x, 1.0 - uv.y)).rgb;
    if (look.x > 1.5) {
        return tex; // image / NDI / CITP / pixel-map
    }
    if (look.x < 0.5) {
        return color.rgb; // solid
    }
    let tp = look.y;
    if (tp < 0.5) { return grid_pattern(uv); }
    if (tp < 1.5) { return color_bars(uv); }
    // gradient: horizontal hue sweep, vertical brightness
    return hsv2rgb(uv.x, 0.9, 1.0) * mix(0.12, 1.0, uv.y);
}

// One LED's appearance inside its cell: the emitted colour (content, or a
// channel-split for discrete RGB), the lit coverage 0..1, and the average lit
// fraction (duty cycle) used for the far-field LOD + transparency.
struct Pixel {
    color: vec3<f32>,
    lit: f32,
    avg: f32,
}

fn led_pixel(shape: f32, content: vec3<f32>, cell: vec2<f32>, opacity: f32, fw: f32) -> Pixel {
    var out: Pixel;
    out.color = content;
    if (shape < 0.5) {
        // SMD, round: a single combined dot. The lit die is a small fraction of
        // the pitch (sparser for see-through walls).
        let fill = mix(0.16, 0.30, opacity);
        let aa = fw * 0.6 + 1e-3;
        out.lit = 1.0 - smoothstep(fill - aa, fill + aa, length(cell));
        out.avg = clamp(3.14159 * fill * fill, 0.02, 1.0);
    } else if (shape < 1.5) {
        // SMD, square.
        let h = mix(0.18, 0.36, opacity);
        let aa = fw * 0.6 + 1e-3;
        let d = max(abs(cell.x), abs(cell.y));
        out.lit = 1.0 - smoothstep(h - aa, h + aa, d);
        out.avg = clamp(4.0 * h * h, 0.02, 1.0);
    } else {
        // Discrete RGB: three vertical sub-pixel stripes, each emitting one
        // channel (the real "DIP" / discrete-pixel look up close).
        let sx = (cell.x + 0.5) * 3.0;     // 0..3 across the three sub-pixels
        let sub = floor(clamp(sx, 0.0, 2.999));
        let inx = fract(sx) - 0.5;          // within the sub-pixel stripe
        let hx = 0.33;                       // half-width in stripe units
        let hy = mix(0.30, 0.42, opacity);   // half-height in cell units
        let aax = fw * 1.8 + 1e-3;           // x runs at 3× the cell frequency
        let aay = fw * 0.6 + 1e-3;
        let cx = 1.0 - smoothstep(hx - aax, hx + aax, abs(inx));
        let cy = 1.0 - smoothstep(hy - aay, hy + aay, abs(cell.y));
        out.lit = cx * cy;
        var chan = vec3<f32>(0.0, 0.0, 1.0);
        if (sub < 0.5) { chan = vec3<f32>(1.0, 0.0, 0.0); }
        else if (sub < 1.5) { chan = vec3<f32>(0.0, 1.0, 0.0); }
        out.color = content * chan;          // brightness is restored via lit/avg
        out.avg = clamp((hx * 2.0) * (hy * 2.0), 0.02, 1.0);
    }
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = max(in.grid.xy, vec2<f32>(1.0, 1.0));
    let panels = max(in.grid.zw, vec2<f32>(1.0, 1.0));
    let opacity = clamp(in.look.z, 0.0, 1.0);
    let nits = max(in.color.w, 0.0);
    let shape = in.extra.w;

    let pix = in.uv * res;                          // continuous LED-pixel coord
    let cell = fract(pix) - vec2<f32>(0.5, 0.5);
    let fw = max(fwidth(pix.x), fwidth(pix.y));      // screen footprint, in LEDs

    // PHYSICAL KEY: every physical LED emits ONE colour — the content sampled at
    // the LED's CENTRE (quantised to the panel's native pixel grid). A real pixel
    // has NO detail inside it; the RGB sub-pixels below show R/G/B of this single
    // colour. The smooth full-res content is used only when the LEDs are too small
    // to resolve (the eye integrates them) — the two blend via `detail`.
    let led_uv = (floor(pix) + vec2<f32>(0.5, 0.5)) / res;
    var content_led = content_color(in.look, in.color, led_uv);
    var content_smooth = content_color(in.look, in.color, in.uv);

    // --- cabinet seam (structural overlay; applied to both LODs) ---
    let seam = clamp(in.extra.y, 0.0, 0.5);
    if (seam > 0.001) {
        let pc = fract(in.uv * panels);
        let pd = min(min(pc.x, 1.0 - pc.x), min(pc.y, 1.0 - pc.y));
        let sa = fwidth(pd) + 1e-4;
        let seam_f = 1.0 - 0.6 * (1.0 - smoothstep(seam, seam + sa, pd));
        content_led = content_led * seam_f;
        content_smooth = content_smooth * seam_f;
    }
    // --- selection rim ---
    if (in.look.w > 0.5) {
        let b = min(min(in.uv.x, 1.0 - in.uv.x), min(in.uv.y, 1.0 - in.uv.y));
        let rim = (1.0 - smoothstep(0.0, 0.018, b)) * 0.85;
        let rc = vec3<f32>(0.25, 0.65, 1.0) * 4.0;
        content_led = mix(content_led, rc, rim);
        content_smooth = mix(content_smooth, rc, rim);
    }

    // `detail` collapses the LED structure to the smooth surface BEFORE the dots
    // go sub-pixel — without this the dot grid moirés when you zoom out.
    // Discrete-RGB runs at 3× the x frequency, so fade it earlier.
    let lod_freq = fw * (1.0 + 2.0 * step(1.5, shape));
    let detail = clamp((0.5 - lod_freq) / 0.35, 0.0, 1.0);

    let p = led_pixel(shape, content_led, cell, opacity, fw);
    let comp = clamp(p.lit / max(p.avg, 0.02), 0.0, 4.0); // energy-preserving gain

    let transparency = clamp(1.0 - opacity, 0.0, 1.0);
    if (transparency > 0.01) {
        // See-through / mesh wall: PRE-MULTIPLIED emissive (One / OneMinusSrcAlpha).
        let cov = mix(p.avg, p.lit, detail);
        let emit = mix(content_smooth, p.color, detail) * nits;
        return vec4<f32>(emit * cov, cov);
    }
    // Opaque wall: far → smooth surface; near → the LED's single quantised colour
    // as a lit dot / RGB sub-pixels on a dark grid (REPLACE blend, alpha ignored).
    let far_rgb = content_smooth * nits;
    let near_rgb = p.color * nits * comp;
    return vec4<f32>(mix(far_rgb, near_rgb, detail), 1.0);
}
