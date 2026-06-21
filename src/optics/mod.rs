//! The optical-chain model: per-fixture control values, the resolution of those
//! controls (through the GDTF's physical ranges + wheels) into a [`BeamOptics`]
//! the renderer packs into the GPU, and the supporting color/motion math.
//!
//! Light path modelled (source → out): LED/halogen engine → shutter/dimmer →
//! animation wheel → gobo wheels → color wheel / CMY / CTO → prism → frost →
//! focus → zoom lens. Each stage is resolved on the CPU here so the volumetric
//! beam (`volumetric.wgsl`) and the floor pool (`mesh.wgsl`) stay in lock-step.
//!
//! The wheel chain is **dynamic**: a fixture has any number of color/gobo/
//! prism/animation/frost components (the GDTF mode declares them — see
//! [`crate::gdtf::OpticalComponent`]). Controls and motion phases align with
//! the mode's component list; `resolve` folds the engaged components into the
//! renderer's fixed GPU lanes (two projected cookies, one animation glass,
//! prism facet expansion).

pub mod color;
pub mod motion;

pub use motion::WheelMotion;

use std::f32::consts::TAU;

use crate::gdtf::{GdtfFixture, OpticalComponent, WheelKind};

/// Controls for one wheel component, aligned with the GDTF mode's
/// [`components`](crate::gdtf::DmxMode::components) list.
#[derive(Clone, Copy, Debug)]
pub struct WheelControl {
    /// Slot selection (gobo/color wheels) or insertion amount
    /// (prism/frost/animation), `0..1`. 0 = open / removed.
    pub value: f32,
    /// Indexed rotation of the inserted element, `0..1` of a turn.
    pub index: f32,
    /// Continuous rotation/scroll speed, bipolar: 0.5 = stop.
    pub spin: f32,
    /// Shake amount `0..1` (gobo/colour wheel shake): the indexed element
    /// oscillates back and forth; higher = faster + wider. 0 = no shake.
    pub shake: f32,
}

impl Default for WheelControl {
    fn default() -> Self {
        Self { value: 0.0, index: 0.0, spin: 0.5, shake: 0.0 }
    }
}

/// The "console faders" for one fixture — normalised `0..1` control values,
/// edited in the UI and fed by DMX. Defaults are neutral (open white beam,
/// dimmer up, wheels open, prisms out, no strobe).
#[derive(Clone, Debug)]
pub struct OpticalControls {
    pub dimmer: f32,
    /// Beam angle: 0 = narrow end of the GDTF Zoom range, 1 = wide.
    pub zoom: f32,
    /// Focus position; 0.5 ≈ in-focus, extremes blur the gobo edge.
    pub focus: f32,
    /// Iris openness: 1 = fully open, 0 = closed to the GDTF minimum.
    pub iris: f32,
    /// Color-temperature-orange filter: 0 = off, 1 = full warm.
    pub cto: f32,
    /// Plus/minus-green tint (the CC axis orthogonal to CCT): -1 = magenta,
    /// 0 = neutral, +1 = green.
    pub green: f32,
    /// Subtractive cyan / magenta / yellow flags, each `0..1`.
    pub cmy: [f32; 3],
    /// Shutter open amount (1 = open) and strobe rate (0 = off).
    pub shutter: f32,
    pub strobe: f32,
    /// Chromatic-aberration amount (lens dispersion); 0 = none, 1 = strong.
    pub ca: f32,
    /// Per-component wheel controls, aligned with the mode's component list.
    /// Sized lazily by [`ensure_wheels`](Self::ensure_wheels).
    pub wheels: Vec<WheelControl>,
}

impl Default for OpticalControls {
    fn default() -> Self {
        Self {
            dimmer: 1.0,
            zoom: 0.3,
            focus: 0.5,
            iris: 1.0,
            cto: 0.0,
            green: 0.0,
            cmy: [0.0, 0.0, 0.0],
            shutter: 1.0,
            strobe: 0.0,
            ca: 0.25,
            wheels: Vec::new(),
        }
    }
}

impl OpticalControls {
    /// Size the wheel controls to a mode's component count (id-preserving).
    pub fn ensure_wheels(&mut self, n: usize) {
        if self.wheels.len() != n {
            self.wheels.resize(n, WheelControl::default());
        }
    }

    /// The control for component `i`, defaulting when not yet sized.
    pub fn wheel(&self, i: usize) -> WheelControl {
        self.wheels.get(i).copied().unwrap_or_default()
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

/// Fully-resolved optics for one fixture this frame — the *shared* part of the
/// chain (wheels/color/shutter). Per-emitter cone shape comes from
/// [`emitter_cone`]; the renderer turns both into `FixtureGpu` lanes (mapping
/// wheels → atlas layers, expanding prisms).
#[derive(Clone, Debug)]
pub struct BeamOptics {
    /// Linear-RGB beam tint *before* intensity/candela (source × CTO × CMY × wheels).
    pub tint: [f32; 3],
    /// Iris radius fraction (1 = open).
    pub iris: f32,
    /// Frost amount `0..1` (max over engaged frost components).
    pub frost: f32,
    /// Focus distance in metres (the gobo is sharp here, blurred elsewhere).
    pub focus_dist: f32,
    /// Shutter/strobe gate `0..1`.
    pub shutter_gain: f32,
    /// Chromatic-aberration strength (beam-edge per-channel offset).
    pub ca_strength: f32,
    /// Up to two engaged gobo wheels (the GPU's projected-cookie lanes).
    pub gobo1: Option<WheelSel>,
    pub gobo2: Option<WheelSel>,
    /// Animation wheel: (wheel name, scroll 0..1).
    pub anim: Option<(String, f32)>,
    /// Prism facet copies (empty = no prism; stacked prisms compose).
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

/// The wheel a component drives: its GDTF channel-function link, with a
/// kind-based name fallback for wheels not linked in the mode's functions.
fn wheel_name(gdtf: &GdtfFixture, comp: &OpticalComponent) -> Option<String> {
    if let Some(w) = &comp.wheel {
        return Some(w.clone());
    }
    let hint = match comp.kind {
        WheelKind::Color => "color",
        WheelKind::Gobo => "gobo",
        WheelKind::Prism => "prism",
        WheelKind::Animation => "anim",
        WheelKind::Frost => return None,
    };
    let mut hits = gdtf
        .wheels
        .iter()
        .filter(|w| w.name.to_lowercase().contains(hint));
    let first = hits.nth((comp.number.max(1) - 1) as usize);
    first.map(|w| w.name.clone())
}

/// Crossfaded color-wheel tint (linear RGB): `control` (0..1) selects across the
/// slots, `phase` (0..1, from a continuous spin) scrolls the whole wheel.
fn color_wheel_tint(gdtf: &GdtfFixture, wheel: &str, control: f32, phase: f32) -> [f32; 3] {
    let Some(w) = gdtf.wheel(wheel) else {
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
fn prism_beams(gdtf: &GdtfFixture, wheel: &str, rot: f32) -> Vec<PrismBeam> {
    /// Largest facet deflection, as tan(angle) in the lens plane (~15°).
    const MAX_DEFLECT: f32 = 0.27;
    let Some(w) = gdtf.wheel(wheel) else {
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

/// Stack a second engaged prism onto an existing facet set: deflections add
/// (small-angle), weights multiply (renormalised sub-linearly like a single
/// prism). Copy count is capped to keep the GPU beam list bounded.
fn compose_prisms(a: Vec<PrismBeam>, b: Vec<PrismBeam>) -> Vec<PrismBeam> {
    const MAX_COPIES: usize = 24;
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    // Incoming weights are 1/√n per set, so the products are already the
    // 1/√(total) a single combined prism would get.
    let mut out: Vec<PrismBeam> = a
        .iter()
        .flat_map(|p| {
            b.iter().map(move |q| PrismBeam {
                offset: [p.offset[0] + q.offset[0], p.offset[1] + q.offset[1]],
                weight: p.weight * q.weight,
            })
        })
        .collect();
    if out.len() > MAX_COPIES {
        out.sort_by(|x, y| y.weight.partial_cmp(&x.weight).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(MAX_COPIES);
    }
    out
}

/// Whether a wheel component is engaged enough to light up its GPU lane.
fn engaged(ctl: &WheelControl) -> bool {
    ctl.value > 0.005 || (ctl.spin - 0.5).abs() > 0.02
}

/// Resolve a fixture's shared controls + motion into [`BeamOptics`] for this
/// frame. `mode_index` selects the GDTF mode whose component chain the
/// controls align with; `time` drives the strobe. Per-emitter cone shape and
/// brightness live in [`emitter_cone`].
pub fn resolve(
    gdtf: &GdtfFixture,
    mode_index: usize,
    c: &OpticalControls,
    m: &WheelMotion,
    time: f32,
) -> BeamOptics {
    let components: &[OpticalComponent] = gdtf
        .modes
        .get(mode_index)
        .map(|md| md.components.as_slice())
        .unwrap_or(&[]);

    // --- iris (openness 0..1 → physical [min,max], 1 = open) ---
    let iris = gdtf
        .physical_range("Iris")
        .map(|(a, b)| {
            let (lo, hi) = (a.min(b), a.max(b));
            lo + (hi - lo) * c.iris.clamp(0.0, 1.0)
        })
        .unwrap_or(1.0);

    // --- walk the dynamic wheel chain ---
    let mut tint_wheels = [1.0_f32, 1.0, 1.0];
    let mut gobos: Vec<WheelSel> = Vec::new();
    let mut anim: Option<(String, f32)> = None;
    let mut prism: Vec<PrismBeam> = Vec::new();
    let mut frost = 0.0_f32;
    for (i, comp) in components.iter().enumerate() {
        let ctl = c.wheel(i);
        let phase = m.phase(i);
        match comp.kind {
            WheelKind::Color => {
                if let Some(w) = wheel_name(gdtf, comp) {
                    // Shake sways the slot position back and forth across the wheel.
                    let shake = m.shake_offset(i, ctl.shake) / TAU; // turns → slot fraction
                    let sel = (ctl.value + shake).clamp(0.0, 1.0);
                    let t = color_wheel_tint(gdtf, &w, sel, phase);
                    tint_wheels = [tint_wheels[0] * t[0], tint_wheels[1] * t[1], tint_wheels[2] * t[2]];
                }
            }
            WheelKind::Gobo => {
                if engaged(&ctl)
                    && let Some(w) = wheel_name(gdtf, comp)
                {
                    let n = gdtf.wheel(&w).map(|x| x.slots.len()).unwrap_or(0);
                    if n > 0 {
                        gobos.push(WheelSel {
                            wheel: w,
                            slot_frac: ctl.value.clamp(0.0, 1.0) * (n as f32 - 1.0),
                            // Image rotation: spin phase + indexed turn + shake sway.
                            rot: phase + ctl.index * TAU + m.shake_offset(i, ctl.shake),
                        });
                    }
                }
            }
            WheelKind::Animation => {
                if ctl.value > 0.01
                    && anim.is_none()
                    && let Some(w) = wheel_name(gdtf, comp)
                {
                    // Phase wraps 0..1 for scrolls (see WheelMotion::advance).
                    anim = Some((w, phase));
                }
            }
            WheelKind::Prism => {
                if ctl.value > 0.01
                    && let Some(w) = wheel_name(gdtf, comp)
                {
                    let set = prism_beams(gdtf, &w, phase + ctl.index * TAU);
                    prism = compose_prisms(prism, set);
                }
            }
            WheelKind::Frost => {
                frost = frost.max(ctl.value.clamp(0.0, 1.0));
            }
        }
    }
    let mut gobo_it = gobos.into_iter();
    let gobo1 = gobo_it.next();
    let gobo2 = gobo_it.next();

    // --- focus / dispersion ---
    // GDTF Focus1 is normalised 0..1 (no metres); map it in reciprocal distance
    // so equal knob steps give equal *perceived* focus change and focus≈0 reaches
    // past the hyperfocal point (a far-focused beam stays sharp on the floor).
    // Prefer a real Focus1Distance (Length) when the fixture exposes one.
    let focus_dist = match gdtf.physical_range("Focus1Distance") {
        Some((a, b)) if (b - a).abs() > 0.5 => {
            let (lo, hi) = (a.min(b), a.max(b));
            lo + (hi - lo) * (1.0 - c.focus.clamp(0.0, 1.0))
        }
        _ => {
            const NEAR: f32 = 2.0;
            const FAR: f32 = 40.0;
            let inv = (1.0 / FAR) + ((1.0 / NEAR) - (1.0 / FAR)) * c.focus.clamp(0.0, 1.0);
            1.0 / inv
        }
    };
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

    // --- color: source white × CTO × CMY × color wheels (all linear) ---
    let source = color::cct_to_linear_rgb(gdtf.beam.color_temp);
    let t_cto = if c.cto > 0.01 {
        let target_k = 6800.0 + (2800.0 - 6800.0) * c.cto.clamp(0.0, 1.0);
        color::filter_from_to(source, color::cct_to_linear_rgb(target_k))
    } else {
        [1.0, 1.0, 1.0]
    };
    let t_cmy = color::cmy_transmittance(c.cmy);
    let mut tint = [
        source[0] * t_cto[0] * t_cmy[0] * tint_wheels[0],
        source[1] * t_cto[1] * t_cmy[1] * tint_wheels[1],
        source[2] * t_cto[2] * t_cmy[2] * tint_wheels[2],
    ];
    // Plus/minus-green correction (orthogonal CC axis).
    if c.green.abs() > 1e-3 {
        tint = color::green_tint(tint, c.green);
    }

    BeamOptics {
        tint,
        iris,
        frost,
        focus_dist,
        shutter_gain,
        ca_strength,
        gobo1,
        gobo2,
        anim,
        prism,
    }
}

/// Per-emitter cone for a multi-emitter fixture: shared zoom optics drive every
/// cell, but each emitter has its own flux (its `<Beam>`), edge order
/// (its BeamType + field/beam ratio) and luminance.
pub struct EmitterCone {
    pub tan_half: f32,
    /// Beam radiance factor: rated flux concentrated into the current cone,
    /// anchored so the verified 40 klm hero head at 25° equals the established
    /// exposure (1.0). Tight zoom → brighter; a 579 lm wash pixel → faint.
    pub brightness: f32,
    /// Face-luminance concentration (zoom only — luminance is flux/area·Ω, so
    /// a small cell is not dimmer *per area* than a big lens).
    pub face_gain: f32,
    pub n_order: f32,
    /// Per spec: BeamType None/Glow emits from the face only — draw no shaft.
    pub shaft: bool,
}

/// Anchor: rated flux and full beam angle whose combination = brightness 1.0
/// (the Ayrton Khamsin the renderer's exposure was tuned against).
const FLUX_REF: f32 = 40_000.0;
const ANGLE_REF: f32 = 25.0;
/// Previz ceiling on one fixture's total rated flux. GDTF files in the wild
/// duplicate group totals onto every pixel (a Roxx S2 sums to >1 Mlm); scaling
/// the cells back to a plausible fixture total tames those without touching
/// honest files (Spiider 11.6 klm, Astera 71 klm → mild trim, Khamsin 40 klm).
pub const FIXTURE_FLUX_CAP: f32 = 60_000.0;

/// One emitter's rated flux, with the unspecified case split across the cells.
pub fn emitter_flux(beam: &crate::gdtf::BeamData, n_emitters: usize) -> f32 {
    if beam.luminous_flux > 1.0 {
        beam.luminous_flux
    } else {
        10_000.0 / n_emitters.max(1) as f32
    }
}

/// Resolve the cone for one emitter. `frost` comes from the fixture-level
/// resolve (frost components soften every cell); `n_emitters` splits the
/// nominal fixture flux when the GDTF omits per-cell `LuminousFlux`;
/// `flux_norm` is the fixture-total cap factor (≤ 1, from [`FIXTURE_FLUX_CAP`]).
pub fn emitter_cone(
    gdtf: &GdtfFixture,
    beam: &crate::gdtf::BeamData,
    c: &OpticalControls,
    frost: f32,
    n_emitters: usize,
    flux_norm: f32,
) -> EmitterCone {
    let nominal = beam.beam_angle.max(1.0);
    let zoom_deg = map_attr(gdtf, "Zoom", c.zoom, (nominal, nominal)).clamp(1.0, 150.0);
    let tan_half = (zoom_deg.to_radians() * 0.5).tan().max(1e-3);

    // Radiance ∝ flux / solid angle, in units of the reference head. This is
    // absolute (no per-emitter "nominal" anchor): a wide blinder pixel with a
    // default-25° Beam entry can't inflate itself via angle ratios.
    let flux = emitter_flux(beam, n_emitters) * flux_norm.clamp(0.0, 1.0);
    let concentration = (solid_angle(ANGLE_REF) / solid_angle(zoom_deg).max(1e-5)).clamp(0.05, 24.0);
    let brightness = ((flux / FLUX_REF) * concentration).clamp(0.002, 24.0);
    let face_gain = concentration.clamp(0.25, 16.0);

    let beam_a = nominal;
    let field_a = beam.field_angle.max(beam_a);
    let ratio = (field_a.to_radians() * 0.5).tan() / (beam_a.to_radians() * 0.5).tan().max(1e-4);
    let ratio_n = if ratio > 1.0001 { (0.6004 / ratio.ln()).clamp(1.2, 12.0) } else { 12.0 };
    // BeamType drives the edge when the author left beam == field (most LED
    // washes do): Wash/Fresnel/PC read soft, Spot/Rectangle keep the hard edge.
    let type_n = match beam.beam_type.as_str() {
        "Spot" | "Rectangle" | "None" => ratio_n,
        "Glow" => 1.4,
        // Wash / Fresnel / PC (and the spec default): soft shoulder.
        _ => ratio_n.min(2.0),
    };
    let n_order = (type_n + (1.5 - type_n) * frost.clamp(0.0, 1.0)).max(1.2);
    let shaft = !matches!(beam.beam_type.as_str(), "None" | "Glow");
    EmitterCone { tan_half, brightness, face_gain, n_order, shaft }
}
