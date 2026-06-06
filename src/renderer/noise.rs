//! CPU generation of a **tiling** 3D value-noise volume.
//!
//! The volumetric shader samples this once or twice per raymarch step instead
//! of evaluating multi-octave FBM in-shader (dozens of hashes per sample). That
//! is the single biggest cost reduction for the raymarch — a texture fetch is
//! far cheaper than recomputing noise, and the result is higher quality and
//! seamlessly tiling (so it can be scrolled forever for drifting haze).

/// Generate a seamlessly tiling `size³` R8 FBM volume. Octave periods all
/// divide `size`, so each octave wraps and the whole volume tiles.
pub fn generate_fbm_volume(size: usize) -> Vec<u8> {
    // (lattice period, amplitude) — periods must divide `size`.
    let octaves: [(usize, f32); 4] = [(4, 0.5), (8, 0.28), (16, 0.17), (32, 0.10)];
    let amp_sum: f32 = octaves.iter().map(|o| o.1).sum();

    let mut data = vec![0u8; size * size * size];
    for z in 0..size {
        for y in 0..size {
            for x in 0..size {
                let mut v = 0.0;
                for &(period, amp) in &octaves {
                    v += amp * periodic_value_noise(x, y, z, size, period);
                }
                v /= amp_sum;
                let idx = (z * size + y) * size + x;
                data[idx] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    data
}

/// Value noise whose lattice wraps at `period` (which divides `size`), so the
/// resulting `size³` field tiles seamlessly.
fn periodic_value_noise(x: usize, y: usize, z: usize, size: usize, period: usize) -> f32 {
    let scale = period as f32 / size as f32;
    let (fx, fy, fz) = (x as f32 * scale, y as f32 * scale, z as f32 * scale);
    let (ix, iy, iz) = (fx.floor() as i32, fy.floor() as i32, fz.floor() as i32);
    let (tx, ty, tz) = (quintic(fx.fract()), quintic(fy.fract()), quintic(fz.fract()));

    let p = period as i32;
    let corner = |dx: i32, dy: i32, dz: i32| {
        lattice_value(
            (ix + dx).rem_euclid(p),
            (iy + dy).rem_euclid(p),
            (iz + dz).rem_euclid(p),
        )
    };

    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), tx);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), tx);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), tx);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), tx);
    lerp(lerp(x00, x10, ty), lerp(x01, x11, ty), tz)
}

/// Deterministic pseudo-random value in `0..=1` for an integer lattice point.
fn lattice_value(x: i32, y: i32, z: i32) -> f32 {
    let mut n = (x.wrapping_mul(374_761_393))
        ^ (y.wrapping_mul(668_265_263))
        ^ (z.wrapping_mul(1_274_126_177));
    n = (n ^ (n >> 13)).wrapping_mul(1_274_126_177);
    n = n ^ (n >> 16);
    (n as u32) as f32 / u32::MAX as f32
}

fn quintic(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
