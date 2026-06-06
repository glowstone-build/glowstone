//! The optical-chain model: per-fixture control values, the resolution of those
//! controls (through the GDTF's physical ranges + wheels) into a [`BeamOptics`]
//! the renderer packs into the GPU, and the supporting color/motion math.
//!
//! Light path modelled (source → out): LED/halogen engine → shutter/dimmer →
//! animation wheel → gobo wheels → color wheel / CMY / CTO → prism → frost →
//! focus → zoom lens. Each stage is resolved on the CPU here so the volumetric
//! beam (`volumetric.wgsl`) and the floor pool (`mesh.wgsl`) stay in lock-step.

pub mod color;
pub mod motion;

pub use motion::WheelMotion;

use std::f32::consts::TAU;

use crate::gdtf::GdtfFixture;

/// The "console faders" for one fixture — normalised `0..1` control values,
/// edited in the UI now and fed by DMX later. Defaults are neutral (open white
/// beam, dimmer up, wheels open, prisms out, no strobe).
#[derive(Clone, Copy, Debug)]
pub struct OpticalControls {
    pub dimmer: f32,
    /// Beam angle: 0 = narrow end of the GDTF Zoom range, 1 = wide.
    pub zoom: f32,
    /// Focus position; 0.5 ≈ in-focus, extremes blur the gobo edge.
    pub focus: f32,
    /// Iris openness: 1 = fully open, 0 = closed to the GDTF minimum.
    pub iris: f32,
    /// Frost / diffusion amount (softens + widens the beam).
    pub frost: f32,
    /// Color-temperature-orange filter: 0 = off, 1 = full warm.
    pub cto: f32,
    /// Subtractive cyan / magenta / yellow flags, each `0..1`.
    pub cmy: [f32; 3],
    /// Color-wheel position (fractional slot) and continuous spin (bipolar).
    pub color: f32,
    pub color_spin: f32,
    /// Gobo wheel 1: slot position, indexed rotation, continuous spin (bipolar).
    pub gobo1: f32,
    pub gobo1_index: f32,
    pub gobo1_rot: f32,
    /// Gobo wheel 2.
    pub gobo2: f32,
    pub gobo2_index: f32,
    pub gobo2_rot: f32,
    /// Animation wheel: insert amount and scroll speed (bipolar).
    pub anim: f32,
    pub anim_spin: f32,
    /// Prism 1 / 2: insert amount and rotation (bipolar).
    pub prism1: f32,
    pub prism1_rot: f32,
    pub prism2: f32,
    pub prism2_rot: f32,
    /// Shutter open amount (1 = open) and strobe rate (0 = off).
    pub shutter: f32,
    pub strobe: f32,
    /// Chromatic-aberration amount (lens dispersion); 0 = none, 1 = strong.
    pub ca: f32,
}

impl Default for OpticalControls {
    fn default() -> Self {
        Self {
            dimmer: 1.0,
            zoom: 0.3,
            focus: 0.5,
            iris: 1.0,
            frost: 0.0,
            cto: 0.0,
            cmy: [0.0, 0.0, 0.0],
            color: 0.0,
            color_spin: 0.5,
            gobo1: 0.0,
            gobo1_index: 0.0,
            gobo1_rot: 0.5,
            gobo2: 0.0,
            gobo2_index: 0.0,
            gobo2_rot: 0.5,
            anim: 0.0,
            anim_spin: 0.5,
            prism1: 0.0,
            prism1_rot: 0.5,
            prism2: 0.0,
            prism2_rot: 0.5,
            shutter: 1.0,
            strobe: 0.0,
            ca: 0.25,
        }
    }
}

/// A gobo/animation wheel selection: which wheel, the fractional slot (for
/// crossfading during a wheel move), and the projected-image rotation (radians).
#[derive(Clone, Debug)]
pub struct WheelSel {
    pub wheel: String,
    pub slot_frac: f32,
    pub rot: f32,
}

/// One prism facet copy: an angular deflection of the beam axis (in the lens
/// plane, `right`/`up` units) and its energy weight.
#[derive(Clone, Copy, Debug)]
pub struct PrismBeam {
    pub offset: [f32; 2],
    pub weight: f32,
}

/// Fully-resolved optics for one fixture this frame. The renderer turns this
/// into `FixtureGpu` lanes (mapping wheels → atlas layers, expanding prisms).
#[derive(Clone, Debug)]
pub struct BeamOptics {
    /// Linear-RGB beam tint *before* intensity/candela (source × CTO × CMY × wheel).
    pub tint: [f32; 3],
    /// tan(half of the current zoom angle) — the cone slope.
    pub tan_half: f32,
    /// Relative intensity from candela conservation (tight beams brighter).
    pub candela: f32,
    /// Iris radius fraction (1 = open).
    pub iris: f32,
    /// Frost amount `0..1`.
    pub frost: f32,
    /// Super-Gaussian edge order (high = hard flat-top, ~1.5 = soft Gaussian).
    pub n_order: f32,
    /// Focus distance in metres (the gobo is sharp here, blurred elsewhere).
    pub focus_dist: f32,
    /// Shutter/strobe gate `0..1`.
    pub shutter_gain: f32,
    /// Chromatic-aberration strength (beam-edge per-channel offset).
    pub ca_strength: f32,
    pub gobo1: Option<WheelSel>,
    pub gobo2: Option<WheelSel>,
    /// Animation wheel: (wheel name, scroll 0..1).
    pub anim: Option<(String, f32)>,
    /// Prism facet copies (empty = no prism).
    pub prism: Vec<PrismBeam>,
}

/// Linear map of a control fraction through a GDTF attribute's physical range,
/// falling back to `fallback` if the fixture lacks the attribute.
pub fn map_attr(gdtf: &GdtfFixture, attr: &str, t: f32, fallback: (f32, f32)) -> f32 {
    let (from, to) = gdtf.physical_range(attr).unwrap_or(fallback);
    from + (to - from) * t.clamp(0.0, 1.0)
}

/// Solid angle of a cone of full angle `deg`.
fn solid_angle(deg: f32) -> f32 {
    TAU * (1.0 - (deg.to_radians() * 0.5).cos())
}

/// The wheel a control attribute drives, via its GDTF channel-function link,
/// with a name-substring fallback for wheels not linked in the first mode.
fn wheel_for(gdtf: &GdtfFixture, attr: &str, name_hint: &str) -> Option<String> {
    if let Some(w) = gdtf.channel_function(attr).and_then(|f| f.wheel.clone()) {
        return Some(w);
    }
    gdtf.wheels
        .iter()
        .find(|w| w.name.to_lowercase().contains(name_hint))
        .map(|w| w.name.clone())
}

/// Crossfaded color-wheel tint (linear RGB): `control` (0..1) selects across the
/// slots, `phase` (0..1, from a continuous spin) scrolls the whole wheel.
fn color_wheel_tint(gdtf: &GdtfFixture, control: f32, phase: f32) -> [f32; 3] {
    let Some(w) = wheel_for(gdtf, "Color1", "color").and_then(|n| gdtf.wheel(&n).cloned()) else {
        return [1.0, 1.0, 1.0];
    };
    if w.slots.is_empty() {
        return [1.0, 1.0, 1.0];
    }
    let n = w.slots.len();
    let pos = control.clamp(0.0, 1.0) * (n as f32 - 1.0) + phase * n as f32;
    let f = pos.rem_euclid(n as f32); // wrap so a spin loops the wheel
    let i0 = (f.floor() as usize) % n;
    let i1 = (i0 + 1) % n;
    let frac = f.fract();
    let col = |i: usize| w.slots[i].color.unwrap_or([1.0, 1.0, 1.0]);
    let (a, b) = (col(i0), col(i1));
    [
        a[0] + (b[0] - a[0]) * frac,
        a[1] + (b[1] - a[1]) * frac,
        a[2] + (b[2] - a[2]) * frac,
    ]
}

/// Facet copies for an engaged prism: rotated lens-plane deflections + weight.
/// A real prism has a **fixed** wedge angle, so each facet deflects its copy by a
/// constant angle regardless of zoom — the copies separate cleanly on a narrow
/// beam and overlap on a wide one (exactly like a real fixture). The GDTF facet
/// offsets (whose scale varies by fixture) are normalised so the largest deflects
/// by `MAX_DEFLECT`.
fn prism_beams(gdtf: &GdtfFixture, attr: &str, name_hint: &str, rot: f32) -> Vec<PrismBeam> {
    /// Largest facet deflection, as tan(angle) in the lens plane (~15°).
    const MAX_DEFLECT: f32 = 0.27;
    let Some(w) = wheel_for(gdtf, attr, name_hint).and_then(|n| gdtf.wheel(&n).cloned()) else {
        return Vec::new();
    };
    let Some(slot) = w.slots.iter().find(|s| !s.facets.is_empty()) else {
        return Vec::new();
    };
    let n = slot.facets.len();
    let max_off = slot
        .facets
        .iter()
        .map(|&[x, y]| (x * x + y * y).sqrt())
        .fold(0.0_f32, f32::max)
        .max(1e-3);
    let spread = MAX_DEFLECT / max_off;
    let (s, c) = rot.sin_cos();
    // Energy splits across facets, but keep copies punchy (sub-linear falloff).
    let w_each = 1.0 / (n as f32).sqrt();
    slot.facets
        .iter()
        .map(|&[dx, dy]| PrismBeam {
            offset: [(c * dx - s * dy) * spread, (s * dx + c * dy) * spread],
            weight: w_each,
        })
        .collect()
}

/// Resolve a fixture's controls + motion into [`BeamOptics`] for this frame.
/// `time` drives the strobe; `nominal_angle` is the GDTF beam angle (the candela
/// reference, so the open beam keeps the existing exposure).
pub fn resolve(
    gdtf: &GdtfFixture,
    c: &OpticalControls,
    m: &WheelMotion,
    time: f32,
    nominal_angle: f32,
) -> BeamOptics {
    // --- zoom + candela conservation ---
    let zoom_deg = map_attr(gdtf, "Zoom", c.zoom, (nominal_angle, nominal_angle)).clamp(1.0, 120.0);
    let tan_half = (zoom_deg.to_radians() * 0.5).tan().max(1e-3);
    let candela = (solid_angle(nominal_angle) / solid_angle(zoom_deg).max(1e-5)).clamp(0.25, 16.0);

    // --- iris (openness 0..1 → physical [min,max], 1 = open) ---
    let iris = gdtf
        .physical_range("Iris")
        .map(|(a, b)| {
            let (lo, hi) = (a.min(b), a.max(b));
            lo + (hi - lo) * c.iris.clamp(0.0, 1.0)
        })
        .unwrap_or(1.0);

    // --- frost / focus / edge ---
    // Super-Gaussian order from the real field/beam ratio: when the 10% field
    // angle ≈ the 50% beam angle the profile is a hard flat-top (high order); a
    // wide soft field gives a low order. Frost then softens toward a Gaussian.
    let beam_a = gdtf.beam.beam_angle.max(1.0);
    let field_a = gdtf.beam.field_angle.max(beam_a);
    let ratio = (field_a.to_radians() * 0.5).tan() / (beam_a.to_radians() * 0.5).tan().max(1e-4);
    let base_n = if ratio > 1.0001 { (0.6004 / ratio.ln()).clamp(1.2, 12.0) } else { 12.0 };
    let frost = c.frost.clamp(0.0, 1.0);
    let n_order = (base_n + (1.5 - base_n) * frost).max(1.2);
    let focus_dist = map_attr(gdtf, "Focus1Distance", c.focus, (5.0, 15.0));
    let focus_defocus = (c.focus - 0.5).abs() * 2.0;
    // Lens dispersion: a tunable base plus extra fringing when out of focus
    // (longitudinal CA grows with focus error).
    let ca_strength = 0.02 + 0.12 * c.ca.clamp(0.0, 1.0) + 0.05 * focus_defocus;

    // --- shutter / strobe ---
    let shutter_gain = if c.strobe > 0.01 {
        let hz = map_attr(gdtf, "Shutter1Strobe", c.strobe, (0.5, 25.0));
        if (time * hz).fract() < 0.5 { c.shutter } else { 0.0 }
    } else {
        c.shutter.clamp(0.0, 1.0)
    };

    // --- color: source white × CTO × CMY × color wheel (all linear) ---
    let source = color::cct_to_linear_rgb(gdtf.beam.color_temp);
    let t_cto = if c.cto > 0.01 {
        let target_k = 6800.0 + (2800.0 - 6800.0) * c.cto.clamp(0.0, 1.0);
        color::filter_from_to(source, color::cct_to_linear_rgb(target_k))
    } else {
        [1.0, 1.0, 1.0]
    };
    let t_cmy = color::cmy_transmittance(c.cmy);
    let t_color = color_wheel_tint(gdtf, c.color, m.color_phase);
    let tint = [
        source[0] * t_cto[0] * t_cmy[0] * t_color[0],
        source[1] * t_cto[1] * t_cmy[1] * t_color[1],
        source[2] * t_cto[2] * t_cmy[2] * t_color[2],
    ];

    // --- gobo wheels ---
    let gobo = |attr: &str, hint: &str, sel: f32, index: f32, angle: f32| -> Option<WheelSel> {
        let wheel = wheel_for(gdtf, attr, hint)?;
        let n = gdtf.wheel(&wheel).map(|w| w.slots.len()).unwrap_or(0);
        if n == 0 {
            return None;
        }
        Some(WheelSel {
            wheel,
            slot_frac: sel.clamp(0.0, 1.0) * (n as f32 - 1.0),
            rot: angle + index * TAU,
        })
    };
    let gobo1 = gobo("Gobo1", "gobo", c.gobo1, c.gobo1_index, m.gobo1_angle);
    let gobo2 = gobo("Gobo2", "gobo2", c.gobo2, c.gobo2_index, m.gobo2_angle);

    // --- animation wheel ---
    let anim = if c.anim > 0.01 {
        wheel_for(gdtf, "AnimationWheel1", "anim").map(|w| (w, m.anim_scroll))
    } else {
        None
    };

    // --- prism (one at a time; prism 1 takes priority) ---
    let prism = if c.prism1 > 0.01 {
        prism_beams(gdtf, "Prism1", "prism1", m.prism1_angle)
    } else if c.prism2 > 0.01 {
        prism_beams(gdtf, "Prism2", "prism2", m.prism2_angle)
    } else {
        Vec::new()
    };

    BeamOptics {
        tint,
        tan_half,
        candela,
        iris,
        frost,
        n_order,
        focus_dist,
        shutter_gain,
        ca_strength,
        gobo1,
        gobo2,
        anim,
        prism,
    }
}
