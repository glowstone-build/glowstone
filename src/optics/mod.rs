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
    /// Subtractive cyan / magenta / yellow flag insertion targets, each `0..1`
    /// (the rendered insertions slew toward these — see [`WheelMotion::cmy`]).
    pub cmy: [f32; 3],
    /// CMY flag motor speed `0..1` from GDTF `ColorMixMSpeed` (0 = fastest).
    pub color_mix_speed: f32,
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
            color_mix_speed: 0.0,
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

/// A scalar optical control with the metadata the inspector needs to render it
/// **data-driven** (single + bulk, instead of a hardcoded slider list): a label,
/// the GDTF attribute that gates it, its value range, and get/set accessors.
/// This is what lets group editing enumerate the controls a fixture actually has.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OpticField {
    Dimmer,
    Zoom,
    Focus,
    Iris,
    Ca,
    Shutter,
    Strobe,
    Cto,
    Green,
    Cyan,
    Magenta,
    Yellow,
}

impl OpticField {
    /// Beam-shaping controls, in display order.
    pub const BEAM: [OpticField; 7] = [
        OpticField::Dimmer,
        OpticField::Zoom,
        OpticField::Focus,
        OpticField::Iris,
        OpticField::Ca,
        OpticField::Shutter,
        OpticField::Strobe,
    ];
    /// Colour-mixing controls, in display order.
    pub const COLOR: [OpticField; 5] = [
        OpticField::Cto,
        OpticField::Green,
        OpticField::Cyan,
        OpticField::Magenta,
        OpticField::Yellow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OpticField::Dimmer => "Dimmer",
            OpticField::Zoom => "Zoom",
            OpticField::Focus => "Focus",
            OpticField::Iris => "Iris",
            OpticField::Ca => "Chromatic ab.",
            OpticField::Shutter => "Shutter",
            OpticField::Strobe => "Strobe",
            OpticField::Cto => "CTO (warm)",
            OpticField::Green => "Tint ±green",
            OpticField::Cyan => "Cyan",
            OpticField::Magenta => "Magenta",
            OpticField::Yellow => "Yellow",
        }
    }

    /// The control's value range (most are 0..1; the green tint is bipolar).
    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            OpticField::Green => -1.0..=1.0,
            _ => 0.0..=1.0,
        }
    }

    pub fn get(self, o: &OpticalControls) -> f32 {
        match self {
            OpticField::Dimmer => o.dimmer,
            OpticField::Zoom => o.zoom,
            OpticField::Focus => o.focus,
            OpticField::Iris => o.iris,
            OpticField::Ca => o.ca,
            OpticField::Shutter => o.shutter,
            OpticField::Strobe => o.strobe,
            OpticField::Cto => o.cto,
            OpticField::Green => o.green,
            OpticField::Cyan => o.cmy[0],
            OpticField::Magenta => o.cmy[1],
            OpticField::Yellow => o.cmy[2],
        }
    }

    pub fn set(self, o: &mut OpticalControls, v: f32) {
        match self {
            OpticField::Dimmer => o.dimmer = v,
            OpticField::Zoom => o.zoom = v,
            OpticField::Focus => o.focus = v,
            OpticField::Iris => o.iris = v,
            OpticField::Ca => o.ca = v,
            OpticField::Shutter => o.shutter = v,
            OpticField::Strobe => o.strobe = v,
            OpticField::Cto => o.cto = v,
            OpticField::Green => o.green = v,
            OpticField::Cyan => o.cmy[0] = v,
            OpticField::Magenta => o.cmy[1] = v,
            OpticField::Yellow => o.cmy[2] = v,
        }
    }

    /// Whether a GDTF fixture exposes this control. Dimmer / chromatic-aberration
    /// / green-tint are synthesized by the renderer and always available; the rest
    /// gate on the relevant GDTF attribute.
    pub fn supported(self, gdtf: &crate::gdtf::GdtfFixture) -> bool {
        let has = |a: &str| gdtf.has_attribute(a);
        match self {
            OpticField::Dimmer | OpticField::Ca | OpticField::Green => true,
            OpticField::Zoom => has("Zoom"),
            OpticField::Focus => has("Focus1"),
            OpticField::Iris => has("Iris"),
            OpticField::Shutter | OpticField::Strobe => has("Shutter1"),
            OpticField::Cto => has("CTO"),
            OpticField::Cyan | OpticField::Magenta | OpticField::Yellow => {
                has("ColorSub_C") || has("ColorSub_M") || has("ColorSub_Y")
            }
        }
    }
}

/// A physically-simulated wheel selection: which wheel, the continuous slewed
/// **position** in slot units (the stepper's actual angle — wraps at `n_slots`),
/// the slot count, the holder-gap fraction (gobo ≈ 0.18, colour ≈ 0.02), and the
/// projected-image rotation (radians, gobos only). The shader maps each beam
/// fragment to a wheel slot from `position`, producing the split + gap.
#[derive(Clone, Debug)]
pub struct WheelSel {
    pub wheel: String,
    pub position: f32,
    pub n_slots: f32,
    pub gap: f32,
    pub rot: f32,
    /// True = a colour wheel (sampled from the packed colour strip); false = a
    /// gobo wheel (sampled from the atlas image block, with holder gap + image rot).
    pub is_color: bool,
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
    /// Every engaged wheel this frame (gobo + colour), in chain order — a DYNAMIC
    /// count. The renderer flattens these into the per-fixture GPU wheel buffer;
    /// the shader folds them in any number. Parked wheels are excluded (their
    /// colour is already folded into `tint`).
    pub wheels: Vec<WheelSel>,
    /// Slewed CMY flag insertions (cyan/magenta/yellow, each 0..1) — the shader
    /// renders them as sliding graduated dichroic flags per fragment.
    pub cmy: [f32; 3],
    /// Animation wheel: (wheel name, scroll 0..1).
    pub anim: Option<(String, f32)>,
    /// Prism facet copies (empty = no prism; stacked prisms compose).
    pub prism: Vec<PrismBeam>,
    /// A real (non-open) gobo is in the beam. The gobo disc is ALWAYS emitted (it
    /// physically lives in the shaft), so this — not "any gobo wheel present" — is
    /// what gates gobo-only effects like CA damping.
    pub gobo_engaged: bool,
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

/// Facet copies for an engaged prism: rotated lens-plane deflections + weight.
/// A real prism has a **fixed** wedge angle, so each facet deflects its copy by a
/// constant angle regardless of zoom — the copies separate cleanly on a narrow
/// beam and overlap on a wide one (exactly like a real fixture). The GDTF facet
/// offsets (whose scale varies by fixture) are normalised so the largest deflects
/// by `MAX_DEFLECT`.
/// The first integer 2..=16 in a name ("8-Facet Circular Prism", "5-Circular") —
/// the facet count, since most GDTF prism slots encode it in the name and omit
/// the `<Facet>` geometry.
fn facet_count_from_name(name: &str) -> Option<usize> {
    let mut num = String::new();
    for ch in name.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else if !num.is_empty() {
            break;
        }
    }
    num.parse::<usize>().ok().filter(|n| (2..=16).contains(n))
}

/// Facet count inferred from a WHEEL name, guarding against the component index
/// ("Prism2" is the 2nd prism, NOT a 2-facet prism). Only a leading count
/// ("4 Prism") or an explicit "…facet…" qualifier counts.
fn facet_count_from_wheel(wheel: &str) -> Option<usize> {
    let lw = wheel.to_lowercase();
    if lw.contains("facet") {
        return facet_count_from_name(wheel);
    }
    // A leading standalone number ("4 Prism") — not a trailing index ("Prism2").
    wheel.split_whitespace().next().and_then(|t| t.parse::<usize>().ok()).filter(|n| (2..=16).contains(n))
}

/// Names that denote the "no prism" pass-through slot.
fn is_open_name(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    n.is_empty()
        || n == "-"
        || ["open", "out", "empty", "closed", "off", "none", "no prism"].iter().any(|k| n.contains(k))
}

/// Whether a wheel slot is an engaged prism (has facet geometry or a facet-count
/// name) rather than the "open" pass-through.
fn is_prism_slot(slot: &crate::gdtf::WheelSlot, wheel: &str) -> bool {
    if is_open_name(&slot.name) {
        return false;
    }
    !slot.facets.is_empty() || facet_count_from_name(&slot.name).is_some() || facet_count_from_wheel(wheel).is_some()
}

fn prism_beams(gdtf: &GdtfFixture, wheel: &str, slot_idx: usize, rot: f32) -> Vec<PrismBeam> {
    /// Largest facet deflection, as tan(angle) in the lens plane (~15°).
    const MAX_DEFLECT: f32 = 0.27;
    let Some(w) = gdtf.wheel(wheel) else {
        return Vec::new();
    };
    // Only the SELECTED slot engages — the "open" slot passes the beam through.
    let Some(slot) = w.slots.get(slot_idx).filter(|s| is_prism_slot(s, wheel)) else {
        return Vec::new();
    };
    // Facet count: explicit <Facet> geometry, else the count named on the slot or
    // the wheel ("4 Prism"), else a sensible default.
    let n = if !slot.facets.is_empty() {
        slot.facets.len()
    } else {
        // Prefer the slot name ("4-Facet Circular Prism") over the wheel name; the
        // wheel-name reader rejects a trailing component index ("Prism2").
        facet_count_from_name(&slot.name).or_else(|| facet_count_from_wheel(wheel)).unwrap_or(3)
    }
    .max(2);

    let (s, c) = rot.sin_cos();
    // Energy splits across facets, but keep copies punchy (sub-linear falloff).
    let w_each = 1.0 / (n as f32).sqrt();
    let max_off = slot.facets.iter().map(|&[x, y]| (x * x + y * y).sqrt()).fold(0.0_f32, f32::max);
    if !slot.facets.is_empty() && max_off > 1e-3 {
        // Usable GDTF facet offsets: normalise so the largest deflects by MAX_DEFLECT.
        let spread = MAX_DEFLECT / max_off;
        slot.facets
            .iter()
            .map(|&[dx, dy]| PrismBeam {
                offset: [(c * dx - s * dy) * spread, (s * dx + c * dy) * spread],
                weight: w_each,
            })
            .collect()
    } else {
        // No usable facet geometry (most GDTF prisms): synthesize a regular n-facet
        // ring — what a circular prism physically does — spaced around a circle of
        // MAX_DEFLECT and turned by the rotation phase.
        (0..n)
            .map(|k| {
                let a = rot + TAU * (k as f32) / (n as f32);
                PrismBeam { offset: [MAX_DEFLECT * a.cos(), MAX_DEFLECT * a.sin()], weight: w_each }
            })
            .collect()
    }
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

    // --- walk the dynamic wheel chain → PHYSICAL wheel descriptors ---
    // Gobo/colour wheels become a continuous slewed `position` (slot units) the
    // shader samples per-fragment, producing the split + holder gap; we no longer
    // fold colour into the tint. CMY likewise becomes per-fragment (sliding flags).
    // Every engaged wheel (gobo + colour), in chain order — a dynamic count.
    let mut wheels: Vec<WheelSel> = Vec::new();
    // A colour wheel parked on a slot is spatially uniform → folded into the
    // tint here (free per fragment); it only goes spatial (split) while moving.
    let mut color_fold = [1.0_f32, 1.0, 1.0];
    let mut anim: Option<(String, f32)> = None;
    let mut prism: Vec<PrismBeam> = Vec::new();
    let mut frost = 0.0_f32;
    let mut gobo_engaged = false;
    /// Holder-gap fractions from `research-wheel-optics.md`.
    const GOBO_GAP: f32 = 0.18;
    const COLOR_GAP: f32 = 0.02;
    for (i, comp) in components.iter().enumerate() {
        let ctl = c.wheel(i);
        let n = comp.slots as f32;
        let position = m.position(i); // slewed continuous slot position
        match comp.kind {
            WheelKind::Color => {
                // Every colour wheel (not just the first); parked ones fold to
                // tint, moving ones split spatially.
                if comp.slots >= 1 && comp.wheel.is_some() {
                    let wname = comp.wheel.clone().unwrap();
                    let scrolling = (ctl.spin - 0.5).abs() > 0.02;
                    let settled = !scrolling && (position - position.round()).abs() < 0.02;
                    if settled {
                        // Parked on a slot: uniform → fold the dichroic colour in.
                        let slot = (position.round() as i32).rem_euclid(comp.slots as i32) as usize;
                        let col = gdtf.wheel(&wname).and_then(|w| w.slots.get(slot)).and_then(|s| s.color);
                        let t = color::dichroic_transmittance(col);
                        color_fold = [color_fold[0] * t[0], color_fold[1] * t[1], color_fold[2] * t[2]];
                    } else {
                        // Moving / scrolling: spatial split across the beam.
                        wheels.push(WheelSel {
                            wheel: wname,
                            position,
                            n_slots: n.max(1.0),
                            gap: COLOR_GAP,
                            rot: 0.0,
                            is_color: true,
                        });
                    }
                }
            }
            WheelKind::Gobo => {
                // The gobo disc is ALWAYS physically in the shaft — emit it every
                // frame (the open slot is just a clear hole). Selecting a gobo then
                // ROTATES the disc from open to that slot instead of the wheel
                // popping into existence. CA/pattern effects gate on `gobo_engaged`
                // (a real, non-open slot) so an open beam stays clean.
                if comp.slots >= 1
                    && let Some(w) = comp.wheel.clone()
                {
                    if ctl.value > 0.005 {
                        gobo_engaged = true;
                    }
                    wheels.push(WheelSel {
                        wheel: w,
                        position,
                        n_slots: n.max(1.0),
                        gap: GOBO_GAP,
                        // Gobo IMAGE rotation (separate from the wheel position):
                        // indexed turn + continuous spin phase + shake sway.
                        rot: m.phase(i) + ctl.index * TAU + m.shake_offset(i, ctl.shake),
                        is_color: false,
                    });
                }
            }
            WheelKind::Animation => {
                if ctl.value > 0.01
                    && anim.is_none()
                    && let Some(w) = comp.wheel.clone().or_else(|| wheel_name(gdtf, comp))
                {
                    anim = Some((w, m.phase(i)));
                }
            }
            WheelKind::Prism => {
                // `value` is the selected slot (like gobo/colour): DMX maps it to the
                // profile slot, the inspector slider selects it. Engagement is decided
                // entirely by whether that slot is a prism slot (prism_beams returns
                // empty on the "open" slot), so there's no separate insertion gate to
                // fight the slot-fraction semantics.
                if let Some(w) = comp.wheel.clone().or_else(|| wheel_name(gdtf, comp)) {
                    let slot_idx =
                        if comp.slots > 1 { (ctl.value.clamp(0.0, 1.0) * (comp.slots as f32 - 1.0)).round() as usize } else { 0 };
                    let set = prism_beams(gdtf, &w, slot_idx, m.phase(i) + ctl.index * TAU);
                    prism = compose_prisms(prism, set);
                }
            }
            WheelKind::Frost => {
                frost = frost.max(ctl.value.clamp(0.0, 1.0));
            }
        }
    }
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

    // --- color: source white × CTO (linear). CMY + colour wheel are now
    // per-fragment in the shader, so they're NOT folded into the tint here. ---
    let source = color::cct_to_linear_rgb(gdtf.beam.color_temp);
    let t_cto = if c.cto > 0.01 {
        let target_k = 6800.0 + (2800.0 - 6800.0) * c.cto.clamp(0.0, 1.0);
        color::filter_from_to(source, color::cct_to_linear_rgb(target_k))
    } else {
        [1.0, 1.0, 1.0]
    };
    let mut tint = [
        source[0] * t_cto[0] * color_fold[0],
        source[1] * t_cto[1] * color_fold[1],
        source[2] * t_cto[2] * color_fold[2],
    ];

    // CMY: render the sliding flags PER-FRAGMENT only while they're actually
    // moving (the visible sweep); once settled they're spatially uniform, so
    // fold them into the tint on the CPU — free per fragment, and the common
    // case (held colour). `m.cmy` is the slewed insertion; `c.cmy` the target.
    let cmy_sliding = (0..3).any(|k| (m.cmy[k] - c.cmy[k]).abs() > 0.01);
    let cmy_spatial = if cmy_sliding {
        [m.cmy[0], m.cmy[1], m.cmy[2]]
    } else {
        let t = color::cmy_transmittance(m.cmy);
        tint = [tint[0] * t[0], tint[1] * t[1], tint[2] * t[2]];
        [0.0, 0.0, 0.0]
    };

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
        wheels,
        cmy: cmy_spatial,
        anim,
        prism,
        gobo_engaged,
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

#[cfg(test)]
mod prism_tests {
    use super::*;
    use crate::gdtf::WheelSlot;

    fn slot(name: &str, facets: Vec<[f32; 2]>) -> WheelSlot {
        WheelSlot { name: name.to_string(), color: None, media: None, facets }
    }

    #[test]
    fn facet_count_prefers_real_count_not_component_index() {
        // Slot name carries the real count.
        assert_eq!(facet_count_from_name("4-Facet Circular Prism"), Some(4));
        assert_eq!(facet_count_from_name("8-Facet"), Some(8));
        assert_eq!(facet_count_from_name("Open"), None);
        // Wheel-name reader: leading count or "facet" qualifier only — NOT a
        // trailing component index.
        assert_eq!(facet_count_from_wheel("4 Prism"), Some(4));
        assert_eq!(facet_count_from_wheel("8 Facet Prism"), Some(8));
        assert_eq!(facet_count_from_wheel("Prism2"), None); // component index, not 2 facets
        assert_eq!(facet_count_from_wheel("Prism1"), None);
    }

    #[test]
    fn open_slot_is_not_a_prism() {
        assert!(is_open_name("Open"));
        assert!(is_open_name("Out"));
        assert!(is_open_name("Empty"));
        assert!(is_open_name("-"));
        assert!(!is_open_name("4-Facet Circular Prism"));
        // The open slot of a digit-named wheel must NOT synthesize a phantom prism.
        assert!(!is_prism_slot(&slot("Out", vec![]), "Prism2"));
        assert!(!is_prism_slot(&slot("Open", vec![]), "4 Prism"));
        // A real prism slot engages (by name or by facet geometry).
        assert!(is_prism_slot(&slot("4-Facet Circular Prism", vec![]), "4 Prism"));
        assert!(is_prism_slot(&slot("Prism", vec![[0.1, 0.0]]), "Prism2"));
    }
}
