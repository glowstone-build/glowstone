//! Headless synthetic DMX feed (no socket) for the `PREVIZ_DMX_*` dev harness.
//!
//! Builds a deterministic [`UniverseSnapshot`] that flows through the SAME
//! `poll()` → `decode()` path as a live console, so patch + decode + render can be
//! verified end-to-end with `PREVIZ_SCREENSHOT` and no hardware. Two entry points:
//! [`look`] encodes a designed lit look per patched fixture (the inverse of
//! decode), and [`inject_spec`] sets explicit `universe,channel,value` triples.

use std::collections::HashMap;
use std::time::Instant;

use crate::gdtf::DmxChannel;
use crate::scene::Scene;

use super::patch::{PatchTable, SYNTH};
use super::universe::{UniverseFrame, UniverseSnapshot};

/// A small concert palette (linear RGB), cycled per fixture.
const PALETTE: [[f32; 3]; 6] = [
    [0.15, 0.40, 1.00], // blue
    [0.00, 0.85, 1.00], // azure
    [0.10, 1.00, 0.70], // teal
    [1.00, 0.70, 0.20], // amber
    [0.55, 0.40, 1.00], // lavender
    [0.85, 0.95, 1.00], // cool white
];

/// Encode a designed lit look into the universes the patch references. `kind`:
/// `"ramp"` fills universe 1 with a 0..255 ramp; anything else lights every
/// enabled fixture (dimmer up, shutter/iris open, a fanned pan, a palette colour).
pub fn look(scene: &Scene, patch: &PatchTable, kind: &str) -> UniverseSnapshot {
    let n = scene.fixtures.len().max(1);
    let mut bufs: HashMap<u16, [u8; 512]> = HashMap::new();

    if kind.eq_ignore_ascii_case("ramp") {
        let b = bufs.entry(1).or_insert([0; 512]);
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = (i % 256) as u8;
        }
    } else {
        for (i, fixture) in scene.fixtures.iter().enumerate() {
            let Some(p) = patch.get(i) else { continue };
            if !p.enabled {
                continue;
            }
            let buf = bufs.entry(p.universe).or_insert([0; 512]);
            write_fixture(buf, fixture, p.address, p.mode_index, i, n);
        }
    }

    snapshot_from(bufs)
}

/// Parse `"u,ch,val; u,ch,val"` (or `@path` to a file of such lines) into a
/// snapshot. Channels are 1-based; out-of-range entries are skipped.
pub fn inject_spec(spec: &str) -> UniverseSnapshot {
    let owned;
    let text = if let Some(path) = spec.strip_prefix('@') {
        owned = std::fs::read_to_string(path.trim()).unwrap_or_default();
        owned.as_str()
    } else {
        spec
    };

    let mut bufs: HashMap<u16, [u8; 512]> = HashMap::new();
    for entry in text.split([';', '\n']) {
        let parts: Vec<&str> = entry.split(',').map(str::trim).collect();
        if let [u, ch, val] = parts.as_slice()
            && let (Ok(u), Ok(ch), Ok(v)) = (u.parse::<u16>(), ch.parse::<u16>(), val.parse::<u8>())
            && (1..=512).contains(&ch)
        {
            bufs.entry(u).or_insert([0; 512])[(ch - 1) as usize] = v;
        }
    }
    snapshot_from(bufs)
}

fn snapshot_from(bufs: HashMap<u16, [u8; 512]>) -> UniverseSnapshot {
    let mut snap = UniverseSnapshot { frames: HashMap::new() };
    let now = Instant::now();
    for (u, levels) in bufs {
        snap.frames.insert(u, UniverseFrame { levels, sources: 1, last_update: now });
    }
    snap
}

/// Write one fixture's designed look into its universe buffer.
fn write_fixture(buf: &mut [u8; 512], fixture: &crate::scene::Fixture, address: u16, mode_index: usize, i: usize, n: usize) {
    let mut put = |offset0: u16, width: u8, v01: f32| {
        let start = (address as usize).saturating_sub(1) + offset0 as usize;
        let coarse = (v01.clamp(0.0, 1.0) * 255.0).round() as u8;
        for k in 0..width as usize {
            if let Some(slot) = buf.get_mut(start + k) {
                *slot = if k == 0 { coarse } else { 0 };
            }
        }
    };

    match fixture.gdtf.as_ref().and_then(|g| g.modes.get(mode_index)) {
        Some(mode) => {
            for ch in &mode.channels {
                let Some(first) = ch.offsets.iter().copied().min() else { continue };
                if let Some(v01) = gdtf_value(&ch.attribute, ch, i, n) {
                    put((first - 1) as u16, ch.resolution.max(1), v01);
                }
            }
        }
        None => {
            for &(attr, off, w) in SYNTH {
                if let Some(v01) = synth_value(attr, i, n) {
                    put(off, w, v01);
                }
            }
        }
    }
}

/// Designed value (0..1) for a GDTF attribute. `None` leaves the channel at 0
/// (neutral for gobo/prism/frost/CTO/colour-mix).
fn gdtf_value(attr: &str, ch: &DmxChannel, i: usize, n: usize) -> Option<f32> {
    match attr {
        "Dimmer" => Some(1.0),
        "Shutter1" => Some(open_shutter(ch)),
        "Iris" => Some(1.0),  // 1 = open (0 would close the beam)
        "Focus1" | "Focus2" => Some(0.5),
        "Zoom" => Some(0.4),
        "Pan" => Some(fan(i, n)),
        "Tilt" => Some(0.61), // ~30° downward for a ±135 fixture
        "ColorSub_C" => Some(1.0 - PALETTE[i % PALETTE.len()][0]),
        "ColorSub_M" => Some(1.0 - PALETTE[i % PALETTE.len()][1]),
        "ColorSub_Y" => Some(1.0 - PALETTE[i % PALETTE.len()][2]),
        "ColorAdd_R" => Some(PALETTE[i % PALETTE.len()][0]),
        "ColorAdd_G" => Some(PALETTE[i % PALETTE.len()][1]),
        "ColorAdd_B" => Some(PALETTE[i % PALETTE.len()][2]),
        _ => None,
    }
}

/// Synthetic-map value for a plain fixture (Dimmer/RGB/Pan/Tilt).
fn synth_value(attr: &str, i: usize, n: usize) -> Option<f32> {
    match attr {
        "Dimmer" => Some(1.0),
        "ColorAdd_R" => Some(PALETTE[i % PALETTE.len()][0]),
        "ColorAdd_G" => Some(PALETTE[i % PALETTE.len()][1]),
        "ColorAdd_B" => Some(PALETTE[i % PALETTE.len()][2]),
        "Pan" => Some(fan(i, n)),
        "Tilt" => Some(0.61),
        _ => None,
    }
}

/// A fanned pan fraction across the rig (0.35..0.65 of the channel range).
fn fan(i: usize, n: usize) -> f32 {
    if n <= 1 {
        0.5
    } else {
        0.35 + 0.30 * (i as f32 / (n - 1) as f32)
    }
}

/// An "open" value (0..1) for a shutter channel: the midpoint of the function
/// named like "open" (or the first non-closed/strobe one), else fully on.
fn open_shutter(ch: &DmxChannel) -> f32 {
    let pick = ch
        .functions
        .iter()
        .position(|f| f.name.to_lowercase().contains("open"))
        .or_else(|| {
            ch.functions.iter().position(|f| {
                let n = f.name.to_lowercase();
                !n.contains("clos") && !n.contains("strob") && !n.contains("black")
            })
        });
    match pick {
        Some(idx) => {
            let from = ch.functions[idx].dmx_from;
            let to = ch.functions.get(idx + 1).map(|f| f.dmx_from).unwrap_or(1.0);
            (from + to) * 0.5
        }
        None => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Fixture, Library, Scene};
    use glam::Vec3;

    #[test]
    fn inject_spec_sets_channels() {
        let snap = inject_spec("1,1,255; 1,2,128; 2,10,64");
        assert_eq!(snap.level(1, 1), Some(255));
        assert_eq!(snap.level(1, 2), Some(128));
        assert_eq!(snap.level(2, 10), Some(64));
        assert_eq!(snap.level(1, 3), Some(0));
    }

    #[test]
    fn look_lights_patched_plain_fixtures() {
        let lib = Library::standard();
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene
            .fixtures
            .push(Fixture::from_profile(&lib.fixtures[0], "PAR", Vec3::ZERO));
        let mut patch = PatchTable::new();
        patch.auto_assign(&scene, 1, 1); // fixture 0 -> universe 1, ch 1..8

        let snap = look(&scene, &patch, "look");
        // Synthetic map: Dimmer is channel 1 of the fixture's footprint.
        assert_eq!(snap.level(1, 1), Some(255), "dimmer driven up");
        // Pan coarse (channel 5) fanned to ~0.5 for a single fixture -> ~128.
        assert!(snap.level(1, 5).unwrap() > 100);
    }
}
