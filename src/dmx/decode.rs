//! Decode live universe bytes into fixture state — the inverse of
//! [`optics::map_attr`](crate::optics::map_attr)/[`resolve`](crate::optics::resolve).
//!
//! Once per frame, for each patched fixture whose universe is present + fresh, we
//! read the bytes at its address and write straight into the same
//! `pan`/`tilt`/`intensity`/`color`/`optics.*`/`cells` fields the Inspector edits
//! — so the renderer needs no changes. Two cases:
//!
//! - **GDTF fixtures**: walk the patched mode's *resolved* channels (instanced
//!   per `GeometryReference`, absolute offsets, 16-bit, shutter-vs-strobe and
//!   wheel-rotation sub-ranges). Wheel attributes route dynamically into the
//!   mode's component chain; per-cell color/dimmer channels compose into
//!   `fixture.cells` through the layer model below.
//! - **Plain fixtures**: the synthetic Dimmer/RGB/Pan16/Tilt16 map drives
//!   `fixture.color`/`pan`/`tilt`/`intensity` directly (their beam IS `fixture.color`).
//!
//! ## The cell layer model
//!
//! Channels group by `(target geometry, instance)`. A group with `ColorAdd_*`
//! channels is a **color layer** over the cells under that geometry — the
//! fixture-wide "Background" layer, a zone, or a single pixel — scaled by the
//! group's own Dimmer/Shutter. A cell's color is the per-channel **HTP (max)**
//! over the layers covering it (the lighting-console convention, and exactly
//! how a Spiider composes background + pixel content). A group with dimmers
//! but *no* color covering **all** cells is the fixture master (→
//! `fixture.intensity` / strobe); over a subset it multiplies those cells.
//!
//! Precedence: a fixture whose universe is absent/stale is left untouched (manual
//! edits stand) and its `live_mask` entry is `false`.

use std::time::Duration;

use crate::gdtf::{component_attr, ChannelFunction, DmxChannel, DmxMode, GdtfFixture, WheelRole};
use crate::scene::Fixture;

use super::patch::{PatchTable, SYNTH};
use super::universe::UniverseSnapshot;

/// Decode `snap` into `fixtures` according to `patch`. `live_mask` is rewritten:
/// `true` for each fixture driven by live DMX this frame.
pub fn apply(
    fixtures: &mut [Fixture],
    patch: &PatchTable,
    snap: &UniverseSnapshot,
    live_mask: &mut Vec<bool>,
    stale: Duration,
) {
    live_mask.clear();
    live_mask.resize(fixtures.len(), false);

    for (i, fixture) in fixtures.iter_mut().enumerate() {
        let Some(p) = patch.get(i) else { continue };
        // Keep the fixture's per-mode state (emitter cells, wheel controls,
        // motion phases) aligned with the patched mode even when no DMX is
        // flowing — the mode dropdown changes the active geometry root.
        if fixture.is_gdtf() && fixture.mode_index != p.mode_index {
            fixture.mode_index = p.mode_index;
            fixture.sync_mode();
        }
        if !p.enabled || !snap.is_live(p.universe, stale) {
            continue;
        }
        let Some(buf) = snap.get(p.universe) else { continue };
        let (address, mode_index) = (p.address, p.mode_index);

        // Cloning the Arc is a pointer copy; it detaches the GDTF borrow from the
        // `&mut fixture` we write into.
        match fixture.gdtf.clone() {
            Some(gdtf) => {
                let Some(mode) = gdtf.modes.get(mode_index) else { continue };
                fixture.optics.ensure_wheels(mode.components.len());
                dump_footprint(&fixture.name, p.universe, address, mode.footprint, buf);
                decode_gdtf(fixture, &gdtf, mode_index, mode, buf, address);
                live_mask[i] = true;
            }
            None => {
                apply_synthetic(fixture, buf, address);
                live_mask[i] = true;
            }
        }
    }
}

/// Dev hook: when `GLOWSTONE_DMX_DUMP` is set, log a live GDTF fixture's raw
/// footprint bytes (1-based DMX slot → value) whenever they change. Lets a
/// layered/multi-emitter fixture's real console output be captured and checked
/// against the decode — the only ground truth for "which channels the desk
/// drives". Off (zero cost) unless the env var is present.
fn dump_footprint(name: &str, universe: u16, address: u16, footprint: u32, buf: &[u8; 512]) {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static LAST: RefCell<HashMap<(u16, u16), Vec<u8>>> = RefCell::new(HashMap::new());
        static ON: bool = std::env::var("GLOWSTONE_DMX_DUMP").is_ok();
    }
    if !ON.with(|on| *on) {
        return;
    }
    let start = address.saturating_sub(1) as usize;
    let end = (start + footprint as usize).min(512);
    if start >= end {
        return;
    }
    let slice = buf[start..end].to_vec();
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        if last.get(&(universe, address)) == Some(&slice) {
            return;
        }
        let pairs: Vec<String> = slice
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, v)| *v != 0)
            .map(|(k, v)| format!("{}:{}", k + 1, v))
            .collect();
        log::info!(
            "DMX_DUMP {name:?} u{universe} @{address} ({} ch) non-zero: {}",
            footprint,
            pairs.join(" ")
        );
        last.insert((universe, address), slice);
    });
}

/// Per-group accumulator for the cell layer model (one `(geometry, instance)`).
#[derive(Clone, Default)]
struct Group {
    /// r/g/b/w/amber/lime channel values, present where the group has that channel.
    rgbwal: [Option<f32>; 6],
    dimmer: Option<f32>,
    /// Raw shutter channel + value; the master pass decodes strobe sub-ranges,
    /// layers reduce it to an open/close gate.
    shutter: Option<(usize, f32)>,
    /// Cells covered (from the widest row seen).
    cells: Vec<u16>,
}

/// Decode one GDTF fixture from its mode's resolved channel rows.
fn decode_gdtf(
    fixture: &mut Fixture,
    gdtf: &GdtfFixture,
    mode_index: usize,
    mode: &DmxMode,
    buf: &[u8; 512],
    address: u16,
) {
    let n_cells = mode.emitters.len();
    let mut groups: Vec<Group> = Vec::new();

    // Shake is recomputed from the live channels each frame; clear it first so a
    // shake commanded on one channel of a component isn't cancelled by another
    // channel's static range (and stale shake can't persist once it ends).
    for w in &mut fixture.optics.wheels {
        w.shake = 0.0;
    }

    for rc in &mode.resolved {
        let ch = &mode.channels[rc.channel];
        // Virtual channels (no footprint) read their GDTF default.
        let v01 = if rc.offsets.is_empty() {
            ch.default
        } else {
            let Some(first) = rc.offsets.iter().copied().min() else { continue };
            let start = address + (first as u16).saturating_sub(1);
            match read_chan(buf, start, ch.resolution) {
                Some(v) => v,
                None => continue,
            }
        };

        // Wheel components (any number of gobo/prism/color/animation/frost
        // wheels) route by attribute into the mode-aligned control list.
        if let Some((kind, number, role)) = component_attr(&ch.attribute) {
            if let Some(ci) = mode.component_index(kind, number) {
                let slots = mode.components.get(ci).map(|c| c.slots).unwrap_or(0);
                apply_wheel(fixture, ci, role, ch, v01, slots);
                continue;
            }
        }

        // Cell color/level layers (per-pixel, zone, or fixture-wide).
        let cell_slot = match ch.attribute.as_str() {
            "ColorAdd_R" => Some(0),
            "ColorAdd_G" => Some(1),
            "ColorAdd_B" => Some(2),
            "ColorAdd_W" | "ColorAdd_WW" | "ColorAdd_CW" => Some(3),
            "ColorAdd_A" | "ColorAdd_Amber" => Some(4),
            "ColorAdd_L" | "ColorAdd_Lime" | "ColorAdd_G_Y" => Some(5),
            _ => None,
        };
        let is_layer_attr = cell_slot.is_some()
            || matches!(ch.attribute.as_str(), "Dimmer" | "Shutter1");
        if n_cells > 0 && is_layer_attr {
            let g = rc.group as usize;
            if groups.len() <= g {
                groups.resize(g + 1, Group::default());
            }
            let group = &mut groups[g];
            if group.cells.len() < rc.cells.len() {
                group.cells = rc.cells.clone();
            }
            if let Some(slot) = cell_slot {
                group.rgbwal[slot] = Some(v01);
                continue;
            }
            match ch.attribute.as_str() {
                // A VIRTUAL (no DMX footprint) per-cell Dimmer is not DMX-controlled;
                // its GDTF default is often 0, which would scale the cell's colour to
                // black even at RGB 100% (the Astera AX2-100 "RGB RGB" pixel modes do
                // exactly this). The RGB IS the level there, so ignore a virtual cell
                // dimmer (treat it as full); only a real DMX dimmer gates the cell.
                "Dimmer" if !rc.offsets.is_empty() => group.dimmer = Some(v01),
                "Shutter1" if !rc.offsets.is_empty() => group.shutter = Some((rc.channel, v01)),
                _ => {}
            }
            continue;
        }

        // Per-emitter pan/tilt: a fixture whose individual heads articulate
        // (e.g. the Volero Wave's eight tilting bodies) has one Pan/Tilt channel
        // PER head, each targeting that head's geometry and covering only its own
        // cell(s). Route those into the per-cell angle arrays so each head aims
        // independently; the renderer's `assemble` rotates each head axis by its
        // own value. A single-yoke fixture's Pan/Tilt covers ALL cells (or the
        // fixture has one emitter) → it falls through to the fixture-wide handler.
        if n_cells > 1
            && !rc.cells.is_empty()
            && rc.cells.len() < n_cells
            && matches!(ch.attribute.as_str(), "Pan" | "Tilt")
        {
            let pan = ch.attribute == "Pan";
            let deg = if pan {
                pan_deg(gdtf, mode_index, v01)
            } else {
                tilt_deg(gdtf, mode_index, v01)
            };
            let arr = if pan { &mut fixture.cell_pan } else { &mut fixture.cell_tilt };
            if arr.len() < n_cells {
                arr.resize(n_cells, 0.0);
            }
            for &c in &rc.cells {
                if (c as usize) < n_cells {
                    arr[c as usize] = deg;
                }
            }
            continue;
        }

        // Everything else: fixture-master attributes (pan/tilt/zoom/CMY/…).
        apply_gdtf_channel(fixture, gdtf, mode_index, ch, v01);
    }

    if n_cells == 0 {
        return;
    }

    // --- compose the layers into cells + the fixture master ---
    // Additive emitters fold by chromaticity vector (white = source CCT, plus
    // amber/lime if present) — not a flat W add — so a pure-amber pixel reads amber.
    let emitters = crate::optics::color::Emitters {
        white: crate::optics::color::cct_to_linear_rgb(gdtf.beam.color_temp),
        ..Default::default()
    };
    let all = |g: &Group| g.cells.len() >= n_cells;
    let has_color = |g: &Group| g.rgbwal.iter().any(|c| c.is_some());
    let gate = |g: &Group| -> f32 {
        g.shutter
            .map(|(ci, v)| shutter_open_gate(&mode.channels[ci], v))
            .unwrap_or(1.0)
    };
    let all_cells: Vec<u16> = (0..n_cells as u16).collect();

    // Whether this mode has any PER-CELL colour channel (a true pixel fixture:
    // a colour layer covering a strict subset of cells, e.g. the Spiider's
    // Lens1/2/3). Used to tell a pixel fixture (master 0 == blackout) apart from
    // a layered wash whose heads are lit by FULL-coverage layers only.
    let has_per_cell_color =
        groups.iter().any(|g| has_color(g) && !g.cells.is_empty() && g.cells.len() < n_cells);
    // Whether a FULL-COVERAGE colour layer exists — a colour group covering every
    // cell (empty `cells` is the authoring shorthand for "all", as is `>= n_cells`).
    // This is the layer that actually CARRIES the head level in a layered wash
    // (the Volero's BG/SHP). A colour-LESS multi-emitter fixture — a blinder /
    // wash bar gated purely by one master Dimmer (e.g. ROXX Cluster "1CH DWE",
    // four lamps + one Dimmer, no colour) — has NONE, so its master 0 must stay a
    // real blackout and is NOT overridden below.
    let has_full_coverage_color =
        groups.iter().any(|g| has_color(g) && (g.cells.is_empty() || g.cells.len() >= n_cells));

    // Master = dimmer/shutter-only group covering every cell. The LAST such
    // group in channel order wins (Robe puts the master after zone dimmers);
    // its shutter keeps the full strobe sub-range decode.
    let mut master_dimmer = false;
    let mut master_level = 0.0_f32;
    for g in groups.iter().filter(|g| all(g) && !has_color(g)) {
        if let Some(d) = g.dimmer {
            // The DMX Dimmer drives the fixture's dimmer (the level); `intensity`
            // is a UI-only master (left at 1.0). See `apply_gdtf_channel`.
            fixture.optics.dimmer = d;
            master_level = d;
            master_dimmer = true;
        }
        if let Some((ci, v)) = g.shutter {
            apply_shutter(fixture, &mode.channels[ci], v);
        }
    }
    // A pixel fixture (e.g. an LED bar in a 3-ch-per-cell RGB mode) carries its
    // whole level in the per-cell colour — there's NO fixture-master Dimmer
    // channel. So drive `optics.dimmer` to full here; otherwise the import
    // default (0, to keep an un-driven rig dark) would gate every lit cell to
    // black even at RGB 100%. (Fixtures WITH a master dimmer set it above.)
    if !master_dimmer {
        fixture.optics.dimmer = 1.0;
    } else if n_cells > 1 && has_full_coverage_color && !has_per_cell_color && master_level <= 1e-4 {
        // Layered wash whose heads are lit by a fixture-wide full-coverage colour
        // layer (e.g. the Volero Wave "Advanced" mode: the eight heads take their
        // colour from the BG/SHP wash, while a SEPARATE 16-bit "Grand" master sits
        // parked at 0 unless the console maps it). Letting that parked master gate
        // the lit heads to black is the "Dimmer reads 0.00, heads dark even though
        // the desk shows them on" bug. The wash layer already carries the real
        // per-head level (in `cells`), so treat the parked master as full.
        //
        // Both guards are required and neither alone suffices:
        //  - `has_full_coverage_color`: a colour-LESS multi-emitter fixture (a
        //    blinder/wash gated only by a master Dimmer, e.g. ROXX "1CH DWE") has
        //    no wash layer to carry the level — master 0 is a genuine blackout and
        //    must NOT be overridden (else it blasts white when faded to zero).
        //  - `!has_per_cell_color`: a true pixel fixture (Spiider/Astera) keeps
        //    master 0 == blackout, so this never regresses its pixel modes.
        // (Where a fixture really fades intensity via this master, the wash layer's
        // own dimmer still blacks the cells out, so blackout stays reachable.)
        fixture.optics.dimmer = 1.0;
    }

    // Color layers, HTP per channel; each scaled by its own dimmer/shutter.
    // A color group with no cells under it (authoring quirk) covers everything.
    let mut cells = vec![[0.0_f32; 3]; n_cells];
    let mut covered = vec![false; n_cells];
    for g in groups.iter().filter(|g| has_color(g)) {
        let scale = g.dimmer.unwrap_or(1.0) * gate(g);
        let lv = |i: usize| g.rgbwal[i].unwrap_or(0.0);
        let folded = crate::optics::color::fold_rgbwal(
            [lv(0), lv(1), lv(2), lv(3), lv(4), lv(5)],
            &emitters,
        );
        let rgb = [folded[0] * scale, folded[1] * scale, folded[2] * scale];
        let targets = if g.cells.is_empty() { &all_cells } else { &g.cells };
        for &c in targets {
            let c = c as usize;
            if c < n_cells {
                covered[c] = true;
                for k in 0..3 {
                    cells[c][k] = cells[c][k].max(rgb[k]);
                }
            }
        }
    }
    // Subset dimmer/shutter-only groups. Two cases per covered cell:
    //  • the cell is already lit by a COLOUR layer → this group is a ZONE master
    //    (or a section strobe gate) that SCALES it — multiply;
    //  • the cell has NO colour layer → it's a MONOCHROME emitter (an LED tube /
    //    strobe pixel whose own Dimmer IS its level), so SET it to the source
    //    white at that level and mark it covered.
    // Without the monochrome case, per-pixel single-colour dimmers were multiplied
    // into a zeroed, never-"covered" accumulator and discarded — e.g. the 60
    // warm-white pixels of a ROXX Cluster (and any mono LED strip/blinder/bar in a
    // per-pixel mode) could not be individually controlled at all.
    let white = emitters.white;
    for g in groups.iter().filter(|g| !all(g) && !has_color(g)) {
        if g.dimmer.is_none() && g.shutter.is_none() {
            continue;
        }
        let scale = g.dimmer.unwrap_or(1.0) * gate(g);
        for &c in &g.cells {
            let c = c as usize;
            if c >= n_cells {
                continue;
            }
            if covered[c] {
                for k in 0..3 {
                    cells[c][k] *= scale;
                }
            } else {
                for k in 0..3 {
                    cells[c][k] = white[k] * scale;
                }
                covered[c] = true;
            }
        }
    }
    // Cells no color layer touches keep their manual value (usually white) —
    // a mode without color channels must not black the fixture out.
    fixture.cells.resize(n_cells, [1.0, 1.0, 1.0]);
    for (i, c) in cells.into_iter().enumerate() {
        if covered[i] {
            fixture.cells[i] = c;
        }
    }
    // HTP-merge occluded emitters (e.g. the Spiider flower) into their front
    // cell — they share one physical aperture.
    for i in 0..n_cells {
        if let Some(front) = mode.emitters[i].merged_into {
            let v = fixture.cells[i];
            let f = &mut fixture.cells[front as usize];
            for k in 0..3 {
                f[k] = f[k].max(v[k]);
            }
        }
    }
}

/// Route one wheel-component channel value into the dynamic control list.
/// `Value` roles get the rotation-subrange treatment: a value landing in a
/// continuous-rotation channel function drives the spin instead of the select.
fn apply_wheel(fixture: &mut Fixture, ci: usize, role: WheelRole, ch: &DmxChannel, v01: f32, slots: u32) {
    let Some(ctl) = fixture.optics.wheels.get_mut(ci) else {
        return;
    };
    // A "shake" channel-function sub-range oscillates the indexed element. It can
    // appear on any of the component's channels; handle it before the role.
    if let Some((idx, f)) = active_function(ch, v01) {
        if is_shake(f) {
            ctl.shake = subrange_t(ch, idx, v01).max(0.1);
            return;
        }
    }
    match role {
        WheelRole::Value => {
            if let Some((idx, f)) = active_function(ch, v01) {
                if is_rotation(f) {
                    ctl.spin = 0.5 + 0.5 * subrange_t(ch, idx, v01);
                    return;
                }
                // SLOT SELECT: pick the slot the GDTF profile fixes for this DMX
                // value (its ChannelSet WheelSlotIndex), not a naive scale across
                // the whole channel. Stored as slot/(slots-1) so the wheel parks on
                // the profile slot. Rotation/shake sub-ranges were handled above.
                if slots >= 1 {
                    let slot = select_slot(f, ch, idx, v01, slots);
                    ctl.value = if slots > 1 { slot as f32 / (slots as f32 - 1.0) } else { 0.0 };
                    return;
                }
            }
            ctl.value = v01;
        }
        WheelRole::Index => ctl.index = v01,
        WheelRole::Spin => ctl.spin = v01,
    }
}

/// Map a DMX value within a select channel-function to a wheel slot index, using
/// the function's `<ChannelSet>` rows (the profile's exact fixation) when present,
/// else linearly across the function's own sub-range (not the whole channel).
fn select_slot(f: &ChannelFunction, ch: &DmxChannel, idx: usize, v01: f32, slots: u32) -> u32 {
    let last = slots - 1;
    // Profile slot links: the last ChannelSet whose DMXFrom <= v01 with a real
    // WheelSlotIndex (1-based) wins. (Index 0 = "no link" → skip.)
    if !f.sets.is_empty() {
        // The active ChannelSet = the one with the HIGHEST dmx_from still ≤ v01
        // (order-independent — the GDTF spec doesn't mandate ascending rows).
        let mut chosen: Option<i32> = None;
        let mut best_from = f32::NEG_INFINITY;
        for cs in &f.sets {
            if cs.slot >= 1 && cs.dmx_from <= v01 + 1e-6 && cs.dmx_from >= best_from {
                best_from = cs.dmx_from;
                chosen = Some(cs.slot - 1);
            }
        }
        if let Some(s) = chosen {
            return (s.max(0) as u32).min(last);
        }
    }
    // No slot links: map this function's own sub-range linearly across the slots.
    (subrange_t(ch, idx, v01) * last as f32).round() as u32
}

/// Shutter open gate for layer groups: closed sub-ranges → 0, else 1.
fn shutter_open_gate(ch: &DmxChannel, v01: f32) -> f32 {
    let Some((_, f)) = active_function(ch, v01) else {
        return if v01 > 0.0 { 1.0 } else { 0.0 };
    };
    let name = f.name.to_lowercase();
    if name.contains("close") || name.contains("blackout") || name.contains("off") {
        0.0
    } else {
        1.0
    }
}

/// Read a `width`-byte channel starting at 1-based `start` (MSB-first), normalized
/// to `0..1` over its full range (`value / (2^(8·width) − 1)`, so full-scale = 1.0).
/// `None` if the channel runs past the universe.
fn read_chan(buf: &[u8; 512], start: u16, width: u8) -> Option<f32> {
    let w = width.max(1) as usize;
    let idx = (start as usize).checked_sub(1)?;
    if idx + w > 512 {
        return None;
    }
    let mut value: u32 = 0;
    for k in 0..w {
        value = (value << 8) | buf[idx + k] as u32;
    }
    let max = ((1u64 << (8 * w)) - 1) as f32;
    Some(value as f32 / max)
}

/// Drive one fixture-master GDTF channel into the fixture's fields. `v01` is
/// the channel value normalized over its full range. Wheel components and
/// per-cell color/dimmer layers are routed *before* this (see `decode_gdtf`);
/// the ColorAdd/Dimmer arms here are the fallback for fixtures without any
/// emitter geometry.
fn apply_gdtf_channel(
    fixture: &mut Fixture,
    gdtf: &GdtfFixture,
    mode_index: usize,
    ch: &DmxChannel,
    v01: f32,
) {
    match ch.attribute.as_str() {
        "Pan" => fixture.pan = pan_deg(gdtf, mode_index, v01),
        "Tilt" => fixture.tilt = tilt_deg(gdtf, mode_index, v01),
        // Motor speed: 0 = fastest ("tracking"), up = slower. Drives the slew.
        "PositionMSpeed" => fixture.move_speed = v01,
        // CMY flag motor speed (0 = fastest); drives the colour-flag slide.
        "ColorMixMSpeed" => fixture.optics.color_mix_speed = v01,
        // The Dimmer channel IS the fixture's dimmer/level (the Inspector's Dimmer
        // and the renderer's `intensity × dimmer`). `intensity` is a UI-only master.
        "Dimmer" => fixture.optics.dimmer = v01,
        // Subtractive colour mixing drives the beam tint directly.
        "ColorSub_C" => fixture.optics.cmy[0] = v01,
        "ColorSub_M" => fixture.optics.cmy[1] = v01,
        "ColorSub_Y" => fixture.optics.cmy[2] = v01,
        // Additive RGB without emitter cells: convert to the subtractive amount
        // the optics model uses (full R = no cyan filtering, etc.).
        "ColorAdd_R" => fixture.optics.cmy[0] = 1.0 - v01,
        "ColorAdd_G" => fixture.optics.cmy[1] = 1.0 - v01,
        "ColorAdd_B" => fixture.optics.cmy[2] = 1.0 - v01,
        "CTO" | "CTC" | "CTB" => fixture.optics.cto = v01,
        // Plus/minus-green tint: DMX 0..1 → magenta..green around neutral 0.5.
        "Tint" | "GreenMagenta" | "MagentaGreen" | "Green" => {
            fixture.optics.green = v01 * 2.0 - 1.0
        }
        "Zoom" => fixture.optics.zoom = v01,
        "Focus1" | "Focus2" => fixture.optics.focus = v01,
        "Iris" => fixture.optics.iris = v01,
        "Shutter1" => apply_shutter(fixture, ch, v01),
        _ => {} // Unmodelled attribute — leave the fixture as-is.
    }
}

/// Shutter: route by which channel-function sub-range the value lands in —
/// strobe-rate, closed, or open — rather than a flat normalize.
fn apply_shutter(fixture: &mut Fixture, ch: &DmxChannel, v01: f32) {
    let o = &mut fixture.optics;
    let Some((idx, f)) = active_function(ch, v01) else {
        o.shutter = if v01 > 0.0 { 1.0 } else { 0.0 };
        o.strobe = 0.0;
        return;
    };
    let attr = f.attribute.to_lowercase();
    let name = f.name.to_lowercase();
    if attr.contains("strobe") || name.contains("strobe") {
        o.shutter = 1.0;
        o.strobe = subrange_t(ch, idx, v01).max(0.02);
    } else if name.contains("close") || name.contains("blackout") || name.contains("off") {
        o.shutter = 0.0;
        o.strobe = 0.0;
    } else {
        o.shutter = 1.0;
        o.strobe = 0.0;
    }
}

/// Decode the synthetic Dimmer/RGB/Pan16/Tilt16 map for a plain fixture.
fn apply_synthetic(fixture: &mut Fixture, buf: &[u8; 512], address: u16) {
    for &(attr, off, width) in SYNTH {
        let Some(v01) = read_chan(buf, address + off, width) else { continue };
        match attr {
            "Dimmer" => fixture.optics.dimmer = v01,
            "ColorAdd_R" => fixture.color[0] = v01,
            "ColorAdd_G" => fixture.color[1] = v01,
            "ColorAdd_B" => fixture.color[2] = v01,
            "Pan" => fixture.pan = -270.0 + 540.0 * v01,
            "Tilt" => fixture.tilt = -135.0 + 270.0 * v01,
            _ => {}
        }
    }
}

/// Pan angle (degrees) for `v01`, mapped through the **patched mode's** Pan
/// physical range (falling back to ±270 if absent or implausibly small).
fn pan_deg(gdtf: &GdtfFixture, mode_index: usize, v01: f32) -> f32 {
    let (from, to) = attr_range(gdtf, mode_index, "Pan")
        .filter(|(a, b)| (b - a).abs() >= 1.0)
        .unwrap_or((-270.0, 270.0));
    from + (to - from) * v01
}

/// Tilt angle (degrees) for `v01`, mapped through the patched mode's Tilt range
/// (falling back to ±135).
fn tilt_deg(gdtf: &GdtfFixture, mode_index: usize, v01: f32) -> f32 {
    let (from, to) = attr_range(gdtf, mode_index, "Tilt")
        .filter(|(a, b)| (b - a).abs() >= 1.0)
        .unwrap_or((-135.0, 135.0));
    from + (to - from) * v01
}

/// The physical `(from, to)` range for `attr` in `mode_index` — mode-aware,
/// unlike [`GdtfFixture::physical_range`](crate::gdtf::GdtfFixture::physical_range)
/// which only consults the first mode.
fn attr_range(gdtf: &GdtfFixture, mode_index: usize, attr: &str) -> Option<(f32, f32)> {
    gdtf.modes.get(mode_index)?.channels.iter().find_map(|c| {
        c.functions
            .iter()
            .find(|f| f.attribute == attr)
            .map(|f| (f.physical_from, f.physical_to))
    })
}

/// The channel function whose DMX sub-range contains `v01` (the last whose
/// `dmx_from <= v01`), plus its index. Functions are in ascending DMX order.
fn active_function(ch: &DmxChannel, v01: f32) -> Option<(usize, &ChannelFunction)> {
    let mut best: Option<(usize, &ChannelFunction)> = None;
    for (i, f) in ch.functions.iter().enumerate() {
        if f.dmx_from <= v01 + 1e-6 {
            best = Some((i, f));
        }
    }
    best.or_else(|| ch.functions.first().map(|f| (0, f)))
}

/// Normalize `v01` within channel-function `idx`'s sub-range to `0..1`.
fn subrange_t(ch: &DmxChannel, idx: usize, v01: f32) -> f32 {
    let from = ch.functions[idx].dmx_from;
    let to = ch.functions.get(idx + 1).map(|f| f.dmx_from).unwrap_or(1.0);
    ((v01 - from) / (to - from).max(1e-6)).clamp(0.0, 1.0)
}

/// Whether a channel function is a continuous-rotation/spin sub-range.
fn is_rotation(f: &ChannelFunction) -> bool {
    let a = f.attribute.to_lowercase();
    let n = f.name.to_lowercase();
    (a.contains("rotat") || a.contains("spin") || n.contains("rotat") || n.contains("spin"))
        && !is_shake(f)
}

/// Whether a channel function is a wheel-shake (oscillation) sub-range.
fn is_shake(f: &ChannelFunction) -> bool {
    f.attribute.to_lowercase().contains("shake") || f.name.to_lowercase().contains("shake")
}

/// Drive every pixel-map-DMX LED screen's content from the live snapshot: a small
/// `cols × rows` RGB grid read from Art-Net/sACN starting at `universe`/
/// `start_address` (walking universe boundaries), uploaded as a `ScreenFrame`.
/// This is the SECONDARY, low-res content path — it builds a tiny grid texture,
/// NOT a per-screen-pixel composite, and never touches `Fixture.cells`. Absent /
/// stale channels read 0 (the wall shows black, like a real wall with no signal).
pub fn apply_screens(screens: &mut [crate::scene::LedScreen], snap: &UniverseSnapshot) {
    use crate::scene::screen::{ScreenContent, ScreenFrame};
    for s in screens.iter_mut() {
        let ScreenContent::PixelMapDmx(pm) = &s.content else {
            continue;
        };
        // Clamp the grid so a crafted .glow / MVR file can't force a huge
        // allocation (the UI caps at 64; this is a hard safety bound).
        let cols = pm.cols.clamp(1, 256);
        let rows = pm.rows.clamp(1, 256);
        let base = pm.start_address.saturating_sub(1) as u32; // 0-based channel offset
        let base_univ = pm.universe;
        // Resolve a 0-based global channel offset to a level (0 if absent), walking
        // 512-channel universe boundaries.
        let level_at = |global_ch0: u32| -> u8 {
            let univ = base_univ.wrapping_add((global_ch0 / 512) as u16);
            let ch = (global_ch0 % 512) as u16 + 1; // 1-based
            snap.level(univ, ch).unwrap_or(0)
        };
        let mut rgba = vec![0u8; (cols * rows * 4) as usize];
        for k in 0..(cols * rows) {
            let o = (k * 4) as usize;
            let b0 = base + k * 3;
            rgba[o] = level_at(b0);
            rgba[o + 1] = level_at(b0 + 1);
            rgba[o + 2] = level_at(b0 + 2);
            rgba[o + 3] = 255;
        }
        // Skip the GPU re-upload when nothing changed (a tiny static grid).
        if let Some(prev) = &s.frame
            && prev.width == cols
            && prev.height == rows
            && prev.rgba == rgba
        {
            continue;
        }
        let generation = s.frame.as_ref().map(|f| f.generation.wrapping_add(1)).unwrap_or(1);
        s.frame = Some(std::sync::Arc::new(ScreenFrame { width: cols, height: rows, rgba, generation }));
    }
}

/// Decode the inline-patched stage-pyro devices (CO2 cannons + cold-spark
/// machines), mirroring [`apply_screens`] — they are patched directly on the
/// device (a [`PyroPatch`](crate::scene::pyro::PyroPatch)), NOT through the
/// fixture `PatchTable`. Per the verified DMX footprints (see
/// `docs/RESEARCH-pyro.md` §2): Arm/Safety windows, a high-window Blast/Spark
/// trigger, height + macros. Writes the runtime control fields (`driven`,
/// `armed`, `fire`, live `height_m`/`pan`/`tilt`/`spin_rpm`) the renderer's sim
/// reads. A device whose universe is NOT present in the snapshot stays
/// **un-driven** so it free-runs at its `density` (the glowstone-preview default).
pub fn apply_pyro(pyro: &mut [crate::scene::PyroDevice], snap: &UniverseSnapshot) {
    use crate::scene::pyro::{PyroKind, PyroMode};
    for d in pyro.iter_mut() {
        let Some(patch) = d.patch else {
            d.driven = false;
            continue;
        };
        // Only obey DMX when the patched universe is actually being received;
        // otherwise free-run so a freshly-patched device still previews.
        if snap.get(patch.universe).is_none() {
            d.driven = false;
            continue;
        }
        d.driven = true;
        let read = |off: u16| -> u8 { snap.level(patch.universe, patch.address + off).unwrap_or(0) };
        let unit = |v: u8| v as f32 / 255.0;

        match d.kind {
            PyroKind::Co2Jet => {
                if d.mode == PyroMode::Minimal {
                    // 1ch: Blast (200–255 fires); no separate arm.
                    d.armed = true;
                    d.fire = if read(0) >= 200 { 1.0 } else { 0.0 };
                } else {
                    // 7ch: Arm, Blast, Intensity, Duration, Pan, Tilt, Speed.
                    let arm = read(0);
                    let blast = read(1);
                    let intensity = read(2);
                    d.armed = (100..=199).contains(&arm);
                    let amt = unit(intensity).max(0.4); // intensity scales plume output
                    d.fire = if d.armed && blast >= 200 { amt } else { 0.0 };
                    d.pan = (unit(read(4)) - 0.5) * 180.0; // ±90°
                    d.tilt = (unit(read(5)) - 0.5) * 120.0; // ±60°
                }
            }
            PyroKind::ColdSpark => {
                // 3ch/5ch share Safety, Spark, Height; 5ch adds Function + Oscillation.
                let safety = read(0);
                let spark = read(1);
                let height = read(2);
                d.armed = (50..=200).contains(&safety);
                d.fire = if d.armed && spark >= 10 { unit(spark) } else { 0.0 };
                // Height channel → 1.5..6 m (the device's working apex).
                d.height_m = 1.5 + unit(height) * (6.0 - 1.5);
                if d.mode == PyroMode::Rich {
                    let func = read(3);
                    let osc = read(4);
                    if (20..=40).contains(&func) {
                        d.armed = false; // EMERGENCY STOP
                        d.fire = 0.0;
                    } else if (60..=80).contains(&func) {
                        d.fire = 0.0; // Clear Material (suppress spray)
                    }
                    d.spin_rpm = if (16..=135).contains(&osc) {
                        (osc - 16) as f32 / 119.0 * 100.0 // CW
                    } else if osc >= 136 {
                        -((osc - 136) as f32 / 119.0 * 100.0) // CCW
                    } else {
                        0.0
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmx::patch::{Patch, PatchSource, PatchTable};
    use crate::dmx::universe::{UniverseFrame, UniverseSnapshot};
    use crate::gdtf::{BeamData, DmxMode, Geometry, GeometryKind};
    use crate::scene::{Fixture, Library, Scene};
    use glam::{Mat4, Vec3};
    use std::sync::Arc;

    #[test]
    fn pixelmap_screen_reads_rgb_grid_from_dmx() {
        use crate::scene::screen::{LedScreen, PixelMap, ScreenContent};
        use crate::scene::ScreenProfile;
        // 2×1 grid in universe 1 from address 1: cell0 RGB = ch1..3, cell1 = ch4..6.
        let mut levels = [0u8; 512];
        levels[0] = 255; // cell0 R
        levels[4] = 200; // cell1 G (ch5)
        let snap = snapshot_with(1, levels);
        let prof = ScreenProfile {
            name: "T",
            category: "LED Wall",
            cabinet_mm: [500.0, 500.0],
            cabinet_px: [128, 128],
            gap_mm: 0.0,
            transparent: false,
            default_nits: 1200.0,
        };
        let mut screen = LedScreen::from_profile(&prof, "W", Mat4::IDENTITY);
        screen.content =
            ScreenContent::PixelMapDmx(PixelMap { cols: 2, rows: 1, universe: 1, start_address: 1 });
        let mut screens = vec![screen];
        apply_screens(&mut screens, &snap);
        let f = screens[0].frame.as_ref().expect("frame produced");
        assert_eq!((f.width, f.height), (2, 1));
        assert_eq!(&f.rgba[0..4], &[255, 0, 0, 255], "cell0 = red");
        assert_eq!(&f.rgba[4..8], &[0, 200, 0, 255], "cell1 = green");
        // A second identical decode must NOT bump the generation (no re-upload).
        let gen0 = f.generation;
        apply_screens(&mut screens, &snap);
        assert_eq!(screens[0].frame.as_ref().unwrap().generation, gen0);
    }

    fn snapshot_with(universe: u16, levels: [u8; 512]) -> UniverseSnapshot {
        let mut snap = UniverseSnapshot::default();
        snap.frames.insert(
            universe,
            UniverseFrame { levels, sources: 1, last_update: std::time::Instant::now() },
        );
        snap
    }

    fn plain_fixture() -> Fixture {
        let lib = Library::standard();
        Fixture::from_profile(&lib.fixtures[0], "PAR", Vec3::ZERO)
    }

    /// A patch table with one manual, enabled entry for fixture 0.
    fn one_patch(universe: u16, address: u16, footprint: u16, mode_index: usize) -> PatchTable {
        // Reconcile from a 1-fixture scene then overwrite entry 0.
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.fixtures.push(plain_fixture());
        let mut t = PatchTable::new();
        t.sync(&scene);
        let p = t.get_mut(0).unwrap();
        *p = Patch { universe, address, footprint, mode_index, enabled: true, source: PatchSource::Manual };
        t
    }

    #[test]
    fn synthetic_decode_drives_color_pan_tilt_dimmer() {
        let mut levels = [0u8; 512];
        levels[0] = 128; // Dimmer ch1
        levels[1] = 255; // R ch2
        levels[2] = 0; // G ch3
        levels[3] = 0; // B ch4
        levels[4] = 0xFF; // Pan coarse ch5
        levels[5] = 0xFF; // Pan fine ch6  -> 0xFFFF -> +270
        levels[6] = 0x00; // Tilt coarse ch7
        levels[7] = 0x00; // Tilt fine ch8 -> 0 -> -135
        let snap = snapshot_with(1, levels);
        let patch = one_patch(1, 1, 8, 0);
        let mut fixtures = vec![plain_fixture()];
        let mut live = Vec::new();
        apply(&mut fixtures, &patch, &snap, &mut live, Duration::from_secs(2));

        let f = &fixtures[0];
        // Synthetic Dimmer drives the fixture dimmer (level), not the master.
        assert!((f.optics.dimmer - 128.0 / 255.0).abs() < 1e-4);
        assert_eq!(f.color, [1.0, 0.0, 0.0]);
        assert!((f.pan - 270.0).abs() < 1e-3, "pan {}", f.pan);
        assert!((f.tilt + 135.0).abs() < 1e-3, "tilt {}", f.tilt);
        assert_eq!(live, vec![true]);
    }

    #[test]
    fn sixteen_bit_midscale_is_center() {
        let mut levels = [0u8; 512];
        levels[4] = 0x80; // Pan coarse
        levels[5] = 0x00; // Pan fine -> 0x8000/0xFFFF ~= 0.50001
        let snap = snapshot_with(1, levels);
        let patch = one_patch(1, 1, 8, 0);
        let mut fixtures = vec![plain_fixture()];
        let mut live = Vec::new();
        apply(&mut fixtures, &patch, &snap, &mut live, Duration::from_secs(2));
        // -270 + 540 * ~0.5 ~= ~0.
        assert!(fixtures[0].pan.abs() < 0.1, "pan {}", fixtures[0].pan);
    }

    #[test]
    fn absent_universe_leaves_fixture_untouched() {
        let snap = snapshot_with(1, [255; 512]);
        let patch = one_patch(2, 1, 8, 0); // patched to universe 2 (not present)
        let mut fixtures = vec![plain_fixture()];
        let before = (fixtures[0].pan, fixtures[0].tilt, fixtures[0].intensity, fixtures[0].color);
        let mut live = Vec::new();
        apply(&mut fixtures, &patch, &snap, &mut live, Duration::from_secs(2));
        let after = (fixtures[0].pan, fixtures[0].tilt, fixtures[0].intensity, fixtures[0].color);
        assert_eq!(before, after, "stale/absent universe must not move the fixture");
        assert_eq!(live, vec![false]);
    }

    // --- GDTF decode -------------------------------------------------------

    fn cf(attribute: &str, name: &str, dmx_from: f32, from: f32, to: f32) -> ChannelFunction {
        ChannelFunction {
            attribute: attribute.to_string(),
            name: name.to_string(),
            dmx_from,
            physical_from: from,
            physical_to: to,
            wheel: None,
            sets: Vec::new(),
        }
    }

    fn chan(attr: &str, offset: u32, resolution: u8, functions: Vec<ChannelFunction>) -> DmxChannel {
        let offsets: Vec<u32> = (0..resolution as u32).map(|k| offset + k).collect();
        DmxChannel {
            geometry: String::new(),
            offsets,
            dmx_break: Some(1),
            default: 0.0,
            attribute: attr.to_string(),
            function: functions.first().map(|f| f.name.clone()).unwrap_or_default(),
            sets: Vec::new(),
            resolution,
            functions,
        }
    }

    #[test]
    fn slot_select_uses_profile_channel_sets() {
        use crate::gdtf::ChannelSet;
        // One select function spanning the channel, with the profile's slot links
        // (WheelSlotIndex is 1-based; 1 = first/open slot).
        let mut f = cf("Gobo1", "Select", 0.0, 0.0, 1.0);
        f.sets = vec![
            ChannelSet { dmx_from: 0.0, slot: 1 },  // → slot 0 (open)
            ChannelSet { dmx_from: 0.25, slot: 2 }, // → slot 1
            ChannelSet { dmx_from: 0.5, slot: 3 },  // → slot 2
            ChannelSet { dmx_from: 0.75, slot: 4 }, // → slot 3
        ];
        let ch = chan("Gobo1", 1, 1, vec![f]);
        assert_eq!(select_slot(&ch.functions[0], &ch, 0, 0.10, 4), 0);
        assert_eq!(select_slot(&ch.functions[0], &ch, 0, 0.30, 4), 1);
        assert_eq!(select_slot(&ch.functions[0], &ch, 0, 0.60, 4), 2);
        assert_eq!(select_slot(&ch.functions[0], &ch, 0, 0.90, 4), 3);
    }

    #[test]
    fn slot_select_linear_within_subrange_without_sets() {
        // Gobo select occupies DMX 0..0.5, continuous rotation 0.5..1. With no
        // ChannelSets the select maps linearly across its OWN sub-range, not the
        // whole channel.
        let ch = chan(
            "Gobo1",
            1,
            1,
            vec![cf("Gobo1", "Select", 0.0, 0.0, 1.0), cf("Gobo1PosRotate", "Rotate CW", 0.5, 0.0, 1.0)],
        );
        assert_eq!(select_slot(&ch.functions[0], &ch, 0, 0.0, 5), 0); // t=0
        assert_eq!(select_slot(&ch.functions[0], &ch, 0, 0.25, 5), 2); // t=0.5 → round(0.5·4)
        assert_eq!(select_slot(&ch.functions[0], &ch, 0, 0.499, 5), 4); // t≈1 → last slot
        // A value in the rotation sub-range is a rotation, not a slot.
        assert!(is_rotation(&ch.functions[1]));
    }

    /// A minimal GDTF: Pan(1), Tilt(2), Dimmer(3), Shutter1(4), ColorSub_C(5).
    fn test_gdtf() -> GdtfFixture {
        let channels = vec![
            chan("Pan", 1, 1, vec![cf("Pan", "Pan", 0.0, -270.0, 270.0)]),
            chan("Tilt", 2, 1, vec![cf("Tilt", "Tilt", 0.0, -135.0, 135.0)]),
            chan("Dimmer", 3, 1, vec![cf("Dimmer", "Dimmer", 0.0, 0.0, 100.0)]),
            chan(
                "Shutter1",
                4,
                1,
                vec![
                    cf("Shutter1", "Closed", 0.0, 0.0, 0.0),
                    cf("Shutter1", "Open", 8.0 / 256.0, 0.0, 0.0),
                    cf("Shutter1Strobe", "Strobe", 16.0 / 256.0, 1.0, 25.0),
                ],
            ),
            chan("ColorSub_C", 5, 1, vec![cf("ColorSub_C", "Cyan", 0.0, 0.0, 1.0)]),
        ];
        let geometry = Geometry {
            name: "Base".into(),
            kind: GeometryKind::Geometry,
            model: None,
            matrix: Mat4::IDENTITY,
            children: Vec::new(),
            beam: None,
            reference: None,
        };
        let resolved = channels
            .iter()
            .enumerate()
            .map(|(i, c)| crate::gdtf::ResolvedChannel {
                channel: i,
                offsets: c.offsets.clone(),
                instance: None,
                cells: Vec::new(),
                group: i as u16,
            })
            .collect();
        GdtfFixture {
            source: crate::gdtf::FixtureSource::Import,
            name: "Test".into(),
            manufacturer: "Test".into(),
            long_name: "Test".into(),
            short_name: "T".into(),
            description: String::new(),
            thumbnail: None,
            wheels: Vec::new(),
            models: Vec::new(),
            geometry: geometry.clone(),
            roots: vec![geometry],
            modes: vec![DmxMode {
                name: "Standard".into(),
                geometry: "Base".into(),
                channels,
                emitters: Vec::new(),
                resolved,
                components: Vec::new(),
                footprint: 5,
            }],
            beam_angle: 15.0,
            beam: BeamData::default(),
            spec: String::new(),
            raw: None,
        }
    }

    fn gdtf_fixture() -> Fixture {
        Fixture::from_gdtf(Arc::new(test_gdtf()), "T", Vec3::ZERO)
    }

    #[test]
    fn gdtf_decode_pan_dimmer_color_and_strobe() {
        let mut levels = [0u8; 512];
        levels[0] = 255; // Pan -> +270
        levels[2] = 128; // Dimmer -> ~0.5
        levels[3] = 200; // Shutter -> in strobe sub-range
        levels[4] = 255; // ColorSub_C -> cmy[0] = 1.0
        let snap = snapshot_with(1, levels);
        let patch = one_patch(1, 1, 5, 0);
        let mut fixtures = vec![gdtf_fixture()];
        let mut live = Vec::new();
        apply(&mut fixtures, &patch, &snap, &mut live, Duration::from_secs(2));

        let f = &fixtures[0];
        assert!((f.pan - 270.0).abs() < 1e-3, "pan {}", f.pan);
        assert!((f.optics.dimmer - 128.0 / 255.0).abs() < 1e-4);
        assert!((f.optics.cmy[0] - 1.0).abs() < 1e-4);
        assert!((f.optics.shutter - 1.0).abs() < 1e-4, "open during strobe");
        assert!(f.optics.strobe > 0.0, "strobe engaged, got {}", f.optics.strobe);
        assert_eq!(live, vec![true]);
    }

    #[test]
    fn gdtf_shutter_closed_range() {
        let snap = snapshot_with(1, [0u8; 512]); // Shutter byte 0 -> Closed
        let patch = one_patch(1, 1, 5, 0);
        let mut fixtures = vec![gdtf_fixture()];
        let mut live = Vec::new();
        apply(&mut fixtures, &patch, &snap, &mut live, Duration::from_secs(2));
        assert_eq!(fixtures[0].optics.shutter, 0.0, "byte 0 closes the shutter");
        assert_eq!(fixtures[0].optics.strobe, 0.0);
    }

    // --- per-cell decode against the real Robe Spiider -----------------------

    fn load_spiider() -> Option<GdtfFixture> {
        let path = format!(
            "{}/Downloads/Basic Festival/Basic Festival.mvr",
            std::env::var("HOME").unwrap_or_default()
        );
        let bytes = std::fs::read(&path).ok()?;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())).ok()?;
        let mut f = zip.by_name("Robe Lighting@Robin Spiider.gdtf").ok()?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut buf).ok()?;
        GdtfFixture::load_bytes(&buf).ok()
    }

    /// Mode 8 (Pixel RGBW): drive pixel 1 red, pixel 19 blue, background dark,
    /// master full → exactly those cells light in those colors. Skips when the
    /// test MVR is absent.
    #[test]
    fn spiider_per_cell_decode() {
        let Some(gdtf) = load_spiider() else {
            eprintln!("skip: Basic Festival MVR not found");
            return;
        };
        let mode8 = gdtf
            .modes
            .iter()
            .position(|m| m.name.starts_with("Mode 8"))
            .expect("mode 8");
        let mut fixture = Fixture::from_gdtf(Arc::new(gdtf), "Spiider", Vec3::ZERO);
        fixture.mode_index = mode8;
        fixture.sync_mode();
        let n_cells = fixture.cells.len();
        assert_eq!(n_cells, 20);

        let mut levels = [0u8; 512];
        // Master dimmer (Head, offsets 33,34 16-bit) full.
        levels[32] = 0xFF;
        levels[33] = 0xFF;
        // Master shutter (Head, offset 32) open (96+ = "Open").
        levels[31] = 96;
        // Background RGBW (8..15, 16-bit each) all 0 → background layer dark.
        // Pixel 1 (Lens1 ColorAdd_R at 35) full red.
        levels[34] = 0xFF;
        // Pixel 19 = 12th Zone3 instance: Lens3 ColorAdd_B at 65 + (45-1) = 109.
        levels[108] = 0xFF;

        let snap = snapshot_with(1, levels);
        let patch = {
            let mut scene = Scene::demo();
            scene.fixtures.clear();
            scene.fixtures.push(fixture.clone());
            let mut t = PatchTable::new();
            t.sync(&scene);
            let p = t.get_mut(0).unwrap();
            *p = Patch {
                universe: 1,
                address: 1,
                footprint: 110,
                mode_index: mode8,
                enabled: true,
                source: PatchSource::Manual,
            };
            t
        };
        let mut fixtures = vec![fixture];
        let mut live = Vec::new();
        apply(&mut fixtures, &patch, &snap, &mut live, Duration::from_secs(2));
        assert_eq!(live, vec![true]);

        let f = &fixtures[0];
        assert!((f.optics.dimmer - 1.0).abs() < 1e-3, "master dimmer up, got {}", f.optics.dimmer);
        assert!(f.optics.shutter > 0.5, "shutter open");

        // Cell order: P1 Zone1, P2..P7 Zone2, P8..P19 Zone3, then the flower.
        let c0 = f.cells[0];
        assert!(c0[0] > 0.95 && c0[1] < 0.05 && c0[2] < 0.05, "pixel 1 red: {c0:?}");
        let c18 = f.cells[18];
        assert!(c18[2] > 0.95 && c18[0] < 0.05, "pixel 19 blue: {c18:?}");
        // All other pixels dark (background layer at 0).
        let dark = (1..18).all(|i| f.cells[i].iter().all(|&v| v < 0.05));
        assert!(dark, "undriven pixels dark: {:?}", &f.cells[1..18]);

        // Now background full warm white, pixels still driving → HTP wins per channel.
        let mut levels2 = levels;
        levels2[7] = 0xFF; // bg R coarse
        levels2[9] = 0x80; // bg G coarse
        levels2[16] = 96; // bg shutter (17) open — closed by default, like the real head
        levels2[17] = 0xFF; // bg dimmer (18,19) coarse
        let snap2 = snapshot_with(1, levels2);
        apply(&mut fixtures, &patch, &snap2, &mut live, Duration::from_secs(2));
        let f = &fixtures[0];
        let c5 = f.cells[5];
        assert!(c5[0] > 0.95 && (c5[1] - 0.5).abs() < 0.05, "bg layer on idle pixel: {c5:?}");
        let c0 = f.cells[0];
        assert!(c0[0] > 0.95 && (c0[1] - 0.5).abs() < 0.05, "HTP of bg + red pixel: {c0:?}");

        eprintln!("Spiider per-cell decode OK: {:?}…", &f.cells[..3]);
    }

    /// The Clay Paky Volero Wave "Advanced" (37ch) mode: eight INDEPENDENTLY
    /// tilting heads lit by fixture-wide BG/SHP wash layers, plus a separate
    /// 16-bit "Grand" master. Covers the two bugs this fixture exposed:
    ///   1. per-head Tilt — each of the eight Tilt channels drives only its own
    ///      head (`cell_tilt`), and `assemble` aims each head's beam separately;
    ///   2. the parked "Grand" master at 0 must NOT black out heads lit by the
    ///      wash layers (the "Dimmer 0.00, heads dark but the desk shows them on"
    ///      report). Skips when the GDTF isn't present.
    #[test]
    fn volero_advanced_per_head_tilt_and_layered_dimmer() {
        let path = format!(
            "{}/Downloads/Clay_Paky@Volero_Wave@ClayPaky_FW_2_0_008_1_0_010.gdtf",
            std::env::var("HOME").unwrap_or_default()
        );
        let Ok(g) = GdtfFixture::load_path(std::path::Path::new(&path)) else {
            eprintln!("skip: volero gdtf not found");
            return;
        };
        let adv = g.modes.iter().position(|m| m.name == "Advanced").expect("advanced");
        assert_eq!(g.modes[adv].emitters.len(), 8, "eight heads");
        let fp = g.modes[adv].footprint as u16;
        assert_eq!(fp, 37, "37-channel mode");
        let garc = Arc::new(g);

        // Decode one universe against a fresh Advanced-mode fixture (import default
        // dimmer 0, like an MVR-imported rig before any live DMX).
        let run = |levels: [u8; 512]| -> Fixture {
            let mut fixture = Fixture::from_gdtf(garc.clone(), "V", Vec3::ZERO);
            fixture.mode_index = adv;
            fixture.sync_mode();
            fixture.optics.dimmer = 0.0;
            let snap = snapshot_with(1, levels);
            let patch = one_patch(1, 1, fp, adv);
            let mut fx = vec![fixture];
            let mut live = Vec::new();
            apply(&mut fx, &patch, &snap, &mut live, Duration::from_secs(2));
            fx.pop().unwrap()
        };
        let lit = |c: [f32; 3]| c.iter().any(|&v| v > 0.3);

        // --- Per-head tilt: drive each head's Tilt channel (offsets 30..37) to a
        // distinct value; each head must aim independently. ---
        let mut a = [0u8; 512];
        for b in 0..4 { a[b] = 255; } // BG RGBW
        a[5] = 105; a[6] = 255;       // BG shutter open + dimmer
        a[7] = 105; a[8] = 255; a[9] = 255; // Base shutter + grand dimmer (16-bit)
        for h in 0..8 { a[29 + h] = (h as u8) * 30; } // per-head tilts (offsets 30..37)
        let fa = run(a);
        assert_eq!(fa.cell_tilt.len(), 8, "one tilt per head");
        let distinct_tilts: std::collections::HashSet<i32> =
            fa.cell_tilt.iter().map(|t| t.round() as i32).collect();
        assert_eq!(distinct_tilts.len(), 8, "eight distinct head tilts: {:?}", fa.cell_tilt);
        assert!((fa.tilt).abs() < 1e-3, "the bar BODY does not tilt (only heads do): {}", fa.tilt);

        // `assemble` must turn the per-head tilts into eight distinct beam aims.
        let root = glam::Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let asm = crate::renderer::fixture_model::assemble(
            fa.gdtf.as_ref().unwrap(), adv, root, fa.pan_actual, fa.tilt_actual, &fa.cell_pan, &fa.cell_tilt,
        );
        let dirs: Vec<Vec3> = asm.beams.iter().map(|b| b.dir).collect();
        assert_eq!(dirs.len(), 8, "eight emitter frames");
        let distinct_dirs = dirs.windows(2).filter(|w| w[0].dot(w[1]) < 0.999).count();
        assert_eq!(distinct_dirs, 7, "every adjacent head aims differently: {dirs:?}");

        // --- Layered dimmer: BG wash driven, Base "Grand" master parked at 0.
        // The heads must light (the bug was dimmer→0 gating them to black). ---
        let mut b = [0u8; 512];
        for k in 0..4 { b[k] = 255; }
        b[5] = 105; b[6] = 255; // BG shutter + dimmer up
        b[7] = 105;             // Base shutter open, Base GRAND dimmer left at 0
        let fb = run(b);
        assert!(fb.optics.dimmer > 0.5, "parked grand master must not gate lit heads: {}", fb.optics.dimmer);
        assert!(lit(fb.cells[0]) && lit(fb.cells[7]), "heads lit from the BG wash: {:?}", &fb.cells[..]);

        // --- No colour layer driven (only the grand master up): heads stay dark.
        // The grand master alone can't conjure colour — correct blackout. ---
        let mut c = [0u8; 512];
        c[7] = 105; c[8] = 255; c[9] = 255; // Base shutter + grand dimmer, BG/SHP = 0
        let fc = run(c);
        assert!(!lit(fc.cells[0]), "no wash colour driven → heads dark: {:?}", fc.cells[0]);
    }

    /// A colour-LESS multi-emitter fixture — the ROXX Cluster B4-FC blinder in
    /// its "1CH DWE" mode (four lamps gated by ONE root `Master` Dimmer, no
    /// colour) — must keep `master 0 == blackout`. Guards against the layered-
    /// dimmer fix wrongly forcing a parked master to full on a fixture that has no
    /// wash layer to carry the level (it would blast white when faded to zero).
    /// Skips when the test MVR is absent.
    #[test]
    fn colorless_blinder_master_zero_is_blackout() {
        let Some(gdtf) = load_gdtf_from_mvr("ROXX@CLUSTER B4-FC.gdtf") else {
            eprintln!("skip: Basic Festival MVR not found");
            return;
        };
        let mi = gdtf.modes.iter().position(|m| m.name == "1CH DWE").expect("1CH DWE mode");
        assert!(gdtf.modes[mi].emitters.len() > 1, "multi-emitter blinder");
        let fp = gdtf.modes[mi].footprint as u16;
        let garc = Arc::new(gdtf);

        let dimmer_at = |level: u8| -> f32 {
            let mut fixture = Fixture::from_gdtf(garc.clone(), "B", Vec3::ZERO);
            fixture.mode_index = mi;
            fixture.sync_mode();
            fixture.optics.dimmer = 0.0;
            let mut levels = [0u8; 512];
            levels[0] = level; // the single Master Dimmer channel
            let snap = snapshot_with(1, levels);
            let patch = one_patch(1, 1, fp, mi);
            let mut fx = vec![fixture];
            let mut live = Vec::new();
            apply(&mut fx, &patch, &snap, &mut live, Duration::from_secs(2));
            fx[0].optics.dimmer
        };
        // Master parked at 0 must stay a real blackout (the regression: the
        // layered-dimmer fix forced it to 1.0 and the blinder blasted white).
        assert!(dimmer_at(0) < 1e-3, "colourless blinder at master 0 must be dark, got {}", dimmer_at(0));
        // And it must track the master up — no 0→full discontinuity.
        assert!((dimmer_at(255) - 1.0).abs() < 1e-3, "master full lights it, got {}", dimmer_at(255));
        assert!((dimmer_at(128) - 128.0 / 255.0).abs() < 1e-2, "master mid tracks linearly, got {}", dimmer_at(128));
    }

    /// Load a GDTF member from the Basic Festival MVR (repo `.context` copy first,
    /// then the user's Downloads). `None` if neither is present.
    fn load_gdtf_from_mvr(member: &str) -> Option<GdtfFixture> {
        let candidates = [
            format!("{}/.context/attachments/05W1Dh/Basic Festival.mvr", env!("CARGO_MANIFEST_DIR")),
            format!("{}/Downloads/Basic Festival/Basic Festival.mvr", std::env::var("HOME").unwrap_or_default()),
        ];
        for path in candidates {
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())) else { continue };
            let Ok(mut f) = zip.by_name(member) else { continue };
            let mut buf = Vec::new();
            if std::io::Read::read_to_end(&mut f, &mut buf).is_ok()
                && let Ok(g) = GdtfFixture::load_bytes(&buf)
            {
                return Some(g);
            }
        }
        None
    }

    /// The Astera AX2-100 "RGB RGB" pixel modes give each cell a per-cell RGB plus
    /// a VIRTUAL `Dimmer` (no DMX footprint, GDTF default 0). Driving the RGB to full
    /// must light every cell — the virtual dimmer's 0 default must NOT scale the cell
    /// colour to black. (Regression for the "all RGB at 100% but the bar is black"
    /// bug.) Skips when the test MVR is absent.
    #[test]
    fn pixelbar_virtual_cell_dimmer_lights() {
        let Some(gdtf) = load_gdtf_from_mvr("Astera LED Technology@AX2-100 PixelBar.gdtf") else {
            eprintln!("skip: Basic Festival MVR not found");
            return;
        };
        // A many-cell ColorAdd mode whose per-cell Dimmer is virtual (no offset).
        let Some(mode_i) = gdtf.modes.iter().position(|m| {
            m.emitters.len() >= 8
                && m.channels.iter().any(|c| c.attribute == "ColorAdd_R")
                && m.resolved.iter().any(|rc| {
                    m.channels[rc.channel].attribute == "Dimmer" && rc.offsets.is_empty()
                })
        }) else {
            eprintln!("skip: no pixel mode with a virtual cell dimmer");
            return;
        };
        let fp = gdtf.modes[mode_i].footprint as u16;

        let mut fixture = Fixture::from_gdtf(Arc::new(gdtf), "Bar", Vec3::ZERO);
        fixture.mode_index = mode_i;
        fixture.sync_mode();
        fixture.optics.dimmer = 0.0; // MVR-import "dark until driven" default.
        let n_cells = fixture.cells.len();

        let snap = snapshot_with(1, [255u8; 512]); // all channels full
        let patch = one_patch(1, 1, fp, mode_i);
        let mut fixtures = vec![fixture];
        let mut live = Vec::new();
        apply(&mut fixtures, &patch, &snap, &mut live, Duration::from_secs(2));
        assert_eq!(live, vec![true]);

        let f = &fixtures[0];
        let lit = f.cells.iter().filter(|c| c.iter().all(|&v| v > 0.9)).count();
        assert_eq!(lit, n_cells, "every cell lit white at RGB full, got {lit}/{n_cells}: {:?}", &f.cells[..n_cells.min(3)]);
        // No master dimmer in these modes → decode drives the level to full too.
        assert!((f.optics.dimmer - 1.0).abs() < 1e-3, "dimmer raised, got {}", f.optics.dimmer);
    }

    /// A synthetic monochrome pixel fixture — two colourless emitters, each with
    /// its OWN per-cell `Dimmer` plus a fixture-wide master `Dimmer` (the ROXX
    /// Cluster tube structure). Driving the per-cell dimmers must light each cell
    /// to the source white at ITS OWN level. (Regression: a colourless per-cell
    /// dimmer was multiplied into a zeroed, never-"covered" accumulator and
    /// discarded, so the cells stayed stuck at rest-white and ignored the dimmer.)
    #[test]
    fn per_cell_mono_dimmer_lights_each_cell() {
        use crate::gdtf::{Aperture, EmitterDef, ResolvedChannel};
        let dim = |geom: &str, off: u32| DmxChannel {
            geometry: geom.into(),
            offsets: vec![off],
            dmx_break: Some(1),
            default: 0.0,
            attribute: "Dimmer".into(),
            function: "Dimmer".into(),
            sets: Vec::new(),
            resolution: 1,
            functions: vec![cf("Dimmer", "Dimmer", 0.0, 0.0, 100.0)],
        };
        let rc = |ch: usize, off: u32, cells: Vec<u16>, group: u16| ResolvedChannel {
            channel: ch,
            offsets: vec![off],
            instance: None,
            cells,
            group,
        };
        let geometry = Geometry {
            name: "Root".into(),
            kind: GeometryKind::Geometry,
            model: None,
            matrix: Mat4::IDENTITY,
            children: Vec::new(),
            beam: None,
            reference: None,
        };
        let emitter = |n: &str| EmitterDef {
            name: n.into(),
            beam: BeamData::default(),
            aperture: Aperture::default(),
            pos: [0.0, 0.0],
            merged_into: None,
        };
        let gdtf = Arc::new(GdtfFixture {
            source: crate::gdtf::FixtureSource::Import,
            name: "Mono".into(),
            manufacturer: "T".into(),
            long_name: "Mono".into(),
            short_name: "M".into(),
            description: String::new(),
            thumbnail: None,
            wheels: Vec::new(),
            models: Vec::new(),
            geometry: geometry.clone(),
            roots: vec![geometry],
            modes: vec![DmxMode {
                name: "Std".into(),
                geometry: "Root".into(),
                channels: vec![dim("Root", 1), dim("Px0", 2), dim("Px1", 3)],
                emitters: vec![emitter("Px0"), emitter("Px1")],
                resolved: vec![
                    rc(0, 1, vec![0, 1], 0), // master (all cells)
                    rc(1, 2, vec![0], 1),    // per-cell dimmer, cell 0
                    rc(2, 3, vec![1], 2),    // per-cell dimmer, cell 1
                ],
                components: Vec::new(),
                footprint: 3,
            }],
            beam_angle: 15.0,
            beam: BeamData::default(),
            spec: String::new(),
            raw: None,
        });

        let run = |master: u8, d0: u8, d1: u8| -> Fixture {
            let mut fixture = Fixture::from_gdtf(gdtf.clone(), "M", Vec3::ZERO);
            fixture.sync_mode();
            fixture.optics.dimmer = 0.0;
            let mut levels = [0u8; 512];
            levels[0] = master;
            levels[1] = d0;
            levels[2] = d1;
            let snap = snapshot_with(1, levels);
            let patch = one_patch(1, 1, 3, 0);
            let mut fx = vec![fixture];
            let mut live = Vec::new();
            apply(&mut fx, &patch, &snap, &mut live, Duration::from_secs(2));
            fx.pop().unwrap()
        };

        // Master full; cell 0 at ~half, cell 1 at full. The master drives the
        // fixture dimmer; each cell carries its OWN per-pixel level as white.
        let f = run(255, 128, 255);
        assert!((f.optics.dimmer - 1.0).abs() < 1e-3, "master → dimmer: {}", f.optics.dimmer);
        let (c0, c1) = (f.cells[0], f.cells[1]);
        assert!(c1.iter().all(|&v| v > 0.6), "cell 1 lit white at full: {c1:?}");
        for k in 0..3 {
            assert!((c0[k] - 0.5 * c1[k]).abs() < 0.06, "cell 0 ≈ half cell 1 (k{k}): {c0:?} vs {c1:?}");
        }
        // Per-cell dimmer 0 darkens ONLY that cell.
        let f = run(255, 0, 255);
        assert!(f.cells[0].iter().all(|&v| v < 1e-3), "cell 0 dark at its dimmer 0: {:?}", f.cells[0]);
        assert!(f.cells[1].iter().any(|&v| v > 0.6), "cell 1 still lit: {:?}", f.cells[1]);
    }
}
