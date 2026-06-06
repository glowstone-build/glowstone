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

/// Subtractive CMY dichroic transmittance. Each flag `0..1` absorbs its
/// complementary primary (cyan↘red, magenta↘green, yellow↘blue) with a small
/// `leak` so a fully-inserted flag never reaches pure black (real dichroics).
pub fn cmy_transmittance(cmy: [f32; 3]) -> [f32; 3] {
    let leak = 0.02;
    let f = |x: f32| 1.0 - (1.0 - leak) * x.clamp(0.0, 1.0);
    [f(cmy[0]), f(cmy[1]), f(cmy[2])]
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
        let t = cmy_transmittance([1.0, 0.0, 0.0]); // full cyan
        assert!(t[0] < 0.05 && t[1] > 0.95 && t[2] > 0.95);
    }
}
