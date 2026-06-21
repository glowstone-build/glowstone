//! Color science for the beam engine, in **linear** light.
//!
//! Everything here returns linear-sRGB triples (the renderer works in linear
//! HDR and tonemaps at the very end). Three jobs:
//!   1. Correlated-color-temperature → linear RGB for the LED/halogen source and
//!      the CTO warm filter (Krystek's Planckian-locus approximation).
//!   2. Subtractive **CMY** flag mixing (cyan/magenta/yellow dichroics).
//!   3. A passive **filter** transmittance from one white point to another
//!      (used for CTO) — the brightest channel passes fully, others attenuate.
//!
//! Critique fix: CCT→RGB is **luminance-normalised (Y=1)**, never max-normalised,
//! so CTO computed as a ratio of two whitepoints is a real warming filter rather
//! than an arbitrary clamp.

/// CIE 1931 XYZ (with Y as given) → linear sRGB (un-clamped, may go negative for
/// out-of-gamut chromaticities — callers clamp).
fn xyz_to_linear_rgb(x: f32, y: f32, z: f32) -> [f32; 3] {
    [
        3.2406 * x - 1.5372 * y - 0.4986 * z,
        -0.9689 * x + 1.8758 * y + 0.0415 * z,
        0.0557 * x - 0.2040 * y + 1.0570 * z,
    ]
}

/// Rec.709 relative luminance of a linear-RGB triple.
pub fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Scale a linear-RGB triple to unit luminance (clamping negatives to 0 first).
fn normalize_luminance(c: [f32; 3]) -> [f32; 3] {
    let c = [c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)];
    let l = luminance(c).max(1e-4);
    [c[0] / l, c[1] / l, c[2] / l]
}

/// CIE 1931 chromaticity `(x, y)` of a Planckian (blackbody) radiator at the
/// given correlated color temperature, via Krystek's rational fit (accurate
/// 1000–15000 K — covers stage lamps 2800–8000 K well).
pub fn cct_to_xy(cct: f32) -> (f32, f32) {
    let t = cct.clamp(1000.0, 15000.0);
    let t2 = t * t;
    // CIE 1960 UCS u, v.
    let u = (0.860117757 + 1.54118254e-4 * t + 1.28641212e-7 * t2)
        / (1.0 + 8.42420235e-4 * t + 7.08145163e-7 * t2);
    let v = (0.317398726 + 4.22806245e-5 * t + 4.20481691e-8 * t2)
        / (1.0 - 2.89741816e-5 * t + 1.61456053e-7 * t2);
    let denom = 2.0 * u - 8.0 * v + 4.0;
    (3.0 * u / denom, 2.0 * v / denom)
}

/// CCT → linear-sRGB white point, **normalised to unit luminance** so it can be
/// used both as a base source color and as a CTO filter end-point.
pub fn cct_to_linear_rgb(cct: f32) -> [f32; 3] {
    let (x, y) = cct_to_xy(cct);
    let y = y.max(1e-4);
    // xyY with Y = 1 → XYZ.
    let big_x = x / y;
    let big_z = (1.0 - x - y) / y;
    normalize_luminance(xyz_to_linear_rgb(big_x, 1.0, big_z))
}

/// A passive filter transmittance carrying a beam from `source` white to
/// `target` white (both linear, Y≈1): per-channel ratio, then scaled so the
/// least-absorbed channel transmits fully (`max == 1`). Energy can only drop.
pub fn filter_from_to(source: [f32; 3], target: [f32; 3]) -> [f32; 3] {
    let r = [
        (target[0] / source[0].max(1e-3)).clamp(0.0, 8.0),
        (target[1] / source[1].max(1e-3)).clamp(0.0, 8.0),
        (target[2] / source[2].max(1e-3)).clamp(0.0, 8.0),
    ];
    let m = r[0].max(r[1]).max(r[2]).max(1e-4);
    [r[0] / m, r[1] / m, r[2] / m]
}

/// Subtractive CMY dichroic transmittance, computed in the **optical-density**
/// (log10) domain with per-flag spectral shoulders and a convex insertion ramp.
///
/// Component-wise `1 − k·c` in linear light marches chroma straight to the axis
/// (greys out) and lacks the neighbour bleed a real dichroic has, so two-flag
/// combos land at the wrong hue. Here each flag absorbs its complement strongly
/// (`D_PEAK`) and bleeds a little into the two neighbour channels (`D_SHOULDER`),
/// keeping mid-insertion hue on a realistic curved path; densities add, so
/// stacked flags / CTO compose by summing `D`. `cmy` = cyan/magenta/yellow
/// insertion, each `0..1`. (Burns subtractive mixture ≈ geometric mean ≡ log-add.)
pub fn cmy_transmittance(cmy: [f32; 3]) -> [f32; 3] {
    const D_PEAK: f32 = 1.8; // full-flag peak density: 10^-1.8 ≈ 1.6% leak
    const D_SHOULDER: f32 = 0.10; // neighbour bleed at full insertion
    const GAMMA_INS: f32 = 1.6; // convex: saturation kicks in deeper into the fader
    let peaks = [
        [D_PEAK, D_SHOULDER, D_SHOULDER], // cyan ↘ R
        [D_SHOULDER, D_PEAK, D_SHOULDER], // magenta ↘ G
        [D_SHOULDER, D_SHOULDER, D_PEAK], // yellow ↘ B
    ];
    let mut d = [0.0f32; 3];
    for f in 0..3 {
        let ramp = cmy[f].clamp(0.0, 1.0).powf(GAMMA_INS);
        for ch in 0..3 {
            d[ch] += peaks[f][ch] * ramp;
        }
    }
    [10f32.powf(-d[0]), 10f32.powf(-d[1]), 10f32.powf(-d[2])]
}

/// Additive-emitter chromaticities for the RGB(W/A/L) fold (linear-sRGB,
/// Y-normalised). White defaults to the source CCT; amber ≈ 590 nm, lime ≈ 565 nm.
#[derive(Clone, Copy, Debug)]
pub struct Emitters {
    pub white: [f32; 3],
    pub amber: [f32; 3],
    pub lime: [f32; 3],
    pub w_share: f32,
    pub a_share: f32,
    pub l_share: f32,
}

impl Default for Emitters {
    fn default() -> Self {
        Self {
            white: [1.0, 1.0, 1.0],
            amber: [1.0, 0.42, 0.0],
            lime: [0.55, 1.0, 0.10],
            w_share: 1.0,
            a_share: 0.7,
            l_share: 1.1,
        }
    }
}

/// Fold additive emitters `[r, g, b, w, a, l]` (`0..1`) into one linear-sRGB
/// tint. Each extra emitter (white/amber/lime) contributes its OWN chromaticity
/// vector scaled by its level + lumen share — NOT a flat add to R,G,B (which
/// desaturates and shifts hue: a pure-amber command must read amber, not white).
pub fn fold_rgbwal(levels: [f32; 6], e: &Emitters) -> [f32; 3] {
    let [r, g, b, w, a, l] = levels;
    let mut o = [r, g, b];
    let add = |o: &mut [f32; 3], c: [f32; 3], k: f32| {
        o[0] += c[0] * k;
        o[1] += c[1] * k;
        o[2] += c[2] * k;
    };
    add(&mut o, e.white, w * e.w_share);
    add(&mut o, e.amber, a * e.a_share);
    add(&mut o, e.lime, l * e.l_share);
    o
}

/// Plus/minus-green correction (the CC / "tint" axis orthogonal to CCT): `t > 0`
/// adds green, `t < 0` adds magenta. A Duv-style nudge — green up/down with the
/// red+blue compensated oppositely — then renormalised so it only shifts hue,
/// not luminance. `t ∈ [-1, 1]`.
pub fn green_tint(rgb: [f32; 3], t: f32) -> [f32; 3] {
    const K: f32 = 0.15;
    let t = t.clamp(-1.0, 1.0);
    let g = 1.0 + K * t;
    let rb = 1.0 - 0.5 * K * t;
    let out = [rgb[0] * rb, rgb[1] * g, rgb[2] * rb];
    // Preserve luminance (tint is a chroma shift, not a level change).
    let l0 = luminance(rgb).max(1e-4);
    let l1 = luminance(out).max(1e-4);
    let s = l0 / l1;
    [out[0] * s, out[1] * s, out[2] * s]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d65_is_near_white() {
        let w = cct_to_linear_rgb(6500.0);
        // Unit luminance and roughly neutral.
        assert!((luminance(w) - 1.0).abs() < 1e-3);
        assert!(w[0] > 0.8 && w[0] < 1.3 && w[2] > 0.8 && w[2] < 1.3);
    }

    #[test]
    fn cto_warms() {
        // Warming a 6800 K source toward 2800 K must pass red and cut blue.
        let src = cct_to_linear_rgb(6800.0);
        let warm = cct_to_linear_rgb(2800.0);
        let t = filter_from_to(src, warm);
        assert!(t[0] > t[2], "red {} should exceed blue {}", t[0], t[2]);
        assert!((t[0] - 1.0).abs() < 1e-3, "brightest channel passes fully");
        assert!(t[2] < 0.6, "blue strongly attenuated, got {}", t[2]);
    }

    #[test]
    fn cmy_subtracts() {
        // Full cyan strongly cuts red, with a small realistic shoulder on G/B.
        let t = cmy_transmittance([1.0, 0.0, 0.0]);
        assert!(t[0] < 0.05, "red strongly absorbed, got {}", t[0]);
        assert!(t[1] > 0.7 && t[2] > 0.7, "green/blue mostly pass: {t:?}");
        // None at all = clear glass.
        let open = cmy_transmittance([0.0, 0.0, 0.0]);
        assert!(open.iter().all(|&c| c > 0.999), "open = clear: {open:?}");
        // Convex ramp: half insertion is brighter than a linear-density midpoint
        // (10^-0.9 ≈ 0.126), i.e. saturation kicks in deeper into the fader.
        let half = cmy_transmittance([0.5, 0.0, 0.0]);
        assert!(half[0] > 0.2, "convex ramp keeps half-insertion bright: {}", half[0]);
    }

    #[test]
    fn rgbw_fold_keeps_amber_amber() {
        let e = Emitters::default();
        let amber = fold_rgbwal([0.0, 0.0, 0.0, 0.0, 1.0, 0.0], &e);
        assert!(amber[0] > amber[1] && amber[1] > amber[2], "amber reads warm: {amber:?}");
        // White emitter folds neutral-ish (not a hue shift).
        let white = fold_rgbwal([0.0, 0.0, 0.0, 1.0, 0.0, 0.0], &e);
        assert!((white[0] - white[2]).abs() < 0.2, "W ≈ neutral: {white:?}");
    }

    #[test]
    fn green_tint_shifts_without_luma_change() {
        let base = [0.8, 0.8, 0.8];
        let g = green_tint(base, 1.0);
        assert!(g[1] > g[0] && g[1] > g[2], "plus-green lifts green: {g:?}");
        let m = green_tint(base, -1.0);
        assert!(m[1] < m[0], "minus-green adds magenta: {m:?}");
        assert!((luminance(g) - luminance(base)).abs() < 1e-3, "luma preserved");
    }
}
