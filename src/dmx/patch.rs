//! The DMX patch: which universe + address each fixture occupies, and the
//! channel layout used both to decode incoming DMX and to label the universe
//! grid.
//!
//! The patch is a **side table** ([`PatchTable`]) owned by `DmxIo`, index-parallel
//! to `scene.fixtures` — deliberately NOT a field on [`Fixture`]:
//! `Scene::duplicate_fixture` clones a `Fixture` wholesale, so an embedded patch
//! would copy identical addresses into every array copy (an instant conflict);
//! and a patch on `Scene` would force a borrow split during decode. Keeping it
//! off both also leaves the MVR import/export round-trip completely untouched.

use std::hash::{Hash, Hasher};

use crate::scene::{Fixture, Scene};

/// Where a fixture's patch entry came from (display + auto-assign policy).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PatchSource {
    /// Imported from an MVR scene's `<Addresses>`.
    Mvr,
    /// A GDTF fixture, footprint known, address assigned in-app.
    Gdtf,
    /// A plain library fixture using the synthetic channel map.
    Synthetic,
    /// Edited by hand in the Patch panel.
    Manual,
}

impl PatchSource {
    #[allow(dead_code)] // shown in the fixture sheet on demand / round-trip metadata
    pub fn label(self) -> &'static str {
        match self {
            PatchSource::Mvr => "MVR",
            PatchSource::Gdtf => "GDTF",
            PatchSource::Synthetic => "Synth",
            PatchSource::Manual => "Manual",
        }
    }
}

/// One fixture's DMX patch entry.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Patch {
    /// 1-based universe.
    pub universe: u16,
    /// 1-based start channel within the universe (`1..=512`).
    pub address: u16,
    /// Channel count occupied (from the selected DMX mode, or the synthetic map).
    pub footprint: u16,
    /// Index into the GDTF fixture's `modes` (0 for synthetic / single-mode).
    pub mode_index: usize,
    /// Whether this entry participates in decode + occupies the grid. Unpatched
    /// GDTF/plain fixtures start `false` until [`PatchTable::auto_assign`] runs.
    pub enabled: bool,
    pub source: PatchSource,
}

/// One decoded channel slot within a fixture's footprint. Built by
/// [`channel_map`] and consumed by the universe grid's occupant labels.
#[derive(Clone, Debug)]
pub struct MappedChannel {
    /// 0-based byte offset of the coarse (MSB) byte within the footprint.
    pub offset: u16,
    /// Byte width: 1 = 8-bit, 2 = 16-bit, …
    pub width: u8,
    /// GDTF attribute (e.g. "Pan", "Dimmer", "Gobo1").
    pub attribute: String,
}

/// The full channel layout of a fixture in its patched mode — the single bridge
/// used by BOTH decode and the universe-grid occupant label.
#[derive(Clone, Debug, Default)]
pub struct ChannelMap {
    pub channels: Vec<MappedChannel>,
}

/// An overlapping-address conflict between two patched fixtures (advisory only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub a: usize,
    pub b: usize,
    pub universe: u16,
    /// Overlapping channel range, 1-based inclusive.
    pub range: (u16, u16),
}

/// Synthetic channel map for plain (non-GDTF) library fixtures: a simple
/// Dimmer / RGB / 16-bit Pan / 16-bit Tilt layout — `(attribute, 0-based offset,
/// width)`.
pub const SYNTH: &[(&str, u16, u8)] = &[
    ("Dimmer", 0, 1),
    ("ColorAdd_R", 1, 1),
    ("ColorAdd_G", 2, 1),
    ("ColorAdd_B", 3, 1),
    ("Pan", 4, 2),
    ("Tilt", 6, 2),
];

/// Footprint of the [`SYNTH`] map, in channels.
pub const SYNTH_FOOTPRINT: u16 = 8;

/// Channel count a fixture occupies in `mode_index` (GDTF mode footprint, or the
/// synthetic footprint for a plain fixture). Always `>= 1`.
pub fn footprint_for(fixture: &Fixture, mode_index: usize) -> u16 {
    fixture
        .gdtf
        .as_ref()
        .and_then(|g| g.modes.get(mode_index))
        .map(|m| m.footprint as u16)
        .unwrap_or(SYNTH_FOOTPRINT)
        .max(1)
}

/// Build the channel layout for `fixture` in `mode_index`: the GDTF mode's
/// *resolved* channels (instanced per `GeometryReference`, so a pixel
/// fixture's per-cell rows appear at their real addresses, labelled with the
/// cell instance) when the fixture has a GDTF definition, else the [`SYNTH`]
/// map. Virtual (offset-less) GDTF channels are skipped — they occupy no DMX
/// space.
pub fn channel_map(fixture: &Fixture, mode_index: usize) -> ChannelMap {
    if let Some(gdtf) = fixture.gdtf.as_ref()
        && let Some(mode) = gdtf.modes.get(mode_index)
    {
        let channels = mode
            .resolved
            .iter()
            .filter_map(|rc| {
                let ch = &mode.channels[rc.channel];
                // GDTF offsets are 1-based; the coarse byte is the smallest.
                let first = rc.offsets.iter().copied().min()?;
                let attribute = match &rc.instance {
                    Some(inst) => format!("{inst} · {}", ch.attribute),
                    None => ch.attribute.clone(),
                };
                Some(MappedChannel {
                    offset: first.saturating_sub(1) as u16,
                    width: ch.resolution.max(1),
                    attribute,
                })
            })
            .collect();
        return ChannelMap { channels };
    }
    ChannelMap {
        channels: SYNTH
            .iter()
            .map(|&(attr, off, w)| MappedChannel {
                offset: off,
                width: w,
                attribute: attr.to_string(),
            })
            .collect(),
    }
}

/// Internal: one fixture's patch plus a fingerprint of the fixture it was built
/// for, so [`PatchTable::sync`] can tell an *append* (keep entries) from a
/// *wholesale replacement* like an MVR import (rebuild entries).
#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    fp: u64,
    patch: Option<Patch>,
}

/// The scene's patch: one optional entry per fixture, index-parallel to
/// `scene.fixtures`.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct PatchTable {
    entries: Vec<Entry>,
}

impl PatchTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, i: usize) -> Option<&Patch> {
        self.entries.get(i).and_then(|e| e.patch.as_ref())
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut Patch> {
        self.entries.get_mut(i).and_then(|e| e.patch.as_mut())
    }

    /// Remove the patch entry for a deleted fixture, keeping the table aligned to
    /// `scene.fixtures` so the *other* fixtures' addresses survive (a plain
    /// `sync()` after a middle-delete would fingerprint-mismatch and reconcile the
    /// whole table, wiping manual/auto addressing). Callers must remove entries in
    /// descending index order, in lock-step with `scene.fixtures.remove(i)`.
    pub fn remove_at(&mut self, i: usize) {
        if i < self.entries.len() {
            self.entries.remove(i);
        }
    }

    /// Re-fit the entry for fixture `i` after it was REPLACED in place with a new
    /// fixture type (the Shift+R replace tool). Keeps its universe/address/enabled
    /// (so the rig's patch survives the swap) but refreshes the footprint, mode,
    /// and fingerprint so a later [`sync`](Self::sync) doesn't see a misalignment
    /// and rebuild the whole table. A larger new footprint may now overlap a
    /// neighbour → surfaced as a conflict for the user to re-patch.
    pub fn replace_at(&mut self, i: usize, fixture: &Fixture) {
        let Some(entry) = self.entries.get_mut(i) else { return };
        entry.fp = fingerprint(fixture);
        let fp = footprint_for(fixture, fixture.mode_index);
        match entry.patch.as_mut() {
            Some(p) => {
                p.footprint = fp;
                p.mode_index = fixture.mode_index;
            }
            None => entry.patch = reconcile_one(fixture),
        }
    }

    fn enabled_at(&self, i: usize) -> Option<&Patch> {
        self.get(i).filter(|p| p.enabled)
    }

    /// Match the table to the scene. Appends a default (reconciled) entry for each
    /// newly-added fixture and truncates removed ones — but if any existing index
    /// no longer lines up with its fixture (an MVR import clears + repushes), the
    /// whole table is rebuilt via [`reconcile_from_scene`].
    ///
    /// [`reconcile_from_scene`]: Self::reconcile_from_scene
    pub fn sync(&mut self, scene: &Scene) {
        let n = scene.fixtures.len();
        let misaligned = self
            .entries
            .iter()
            .take(n)
            .enumerate()
            .any(|(i, e)| e.fp != fingerprint(&scene.fixtures[i]));
        if misaligned {
            self.reconcile_from_scene(scene);
            return;
        }
        self.entries.truncate(n);
        while self.entries.len() < n {
            let f = &scene.fixtures[self.entries.len()];
            self.entries.push(Entry {
                fp: fingerprint(f),
                patch: reconcile_one(f),
            });
        }
    }

    /// Rebuild every entry from the scene: MVR addresses, GDTF footprints, and the
    /// synthetic map. GDTF/plain fixtures start `enabled = false` until
    /// auto-assigned; MVR fixtures keep their imported address.
    pub fn reconcile_from_scene(&mut self, scene: &Scene) {
        self.entries.clear();
        self.entries.reserve(scene.fixtures.len());
        for f in &scene.fixtures {
            self.entries.push(Entry {
                fp: fingerprint(f),
                patch: reconcile_one(f),
            });
        }
    }

    /// Assign sequential addresses to every unpatched (`!enabled`) fixture,
    /// packing them from `start_universe`.`start_addr` and skipping channels
    /// already claimed by enabled entries (e.g. an MVR patch), wrapping to the
    /// next universe when a fixture won't fit. Returns the number assigned.
    pub fn auto_assign(&mut self, scene: &Scene, start_universe: u16, start_addr: u16) -> usize {
        self.sync(scene);
        let mut u = start_universe.max(1);
        let mut a = start_addr.max(1);
        let mut assigned = 0;
        for i in 0..self.entries.len() {
            let footprint = match &self.entries[i].patch {
                Some(p) if !p.enabled => p.footprint.max(1),
                _ => continue,
            };
            let (fu, fa) = self.next_free(u, a, footprint, i);
            if let Some(p) = self.entries[i].patch.as_mut() {
                p.universe = fu;
                p.address = fa;
                p.enabled = true;
            }
            u = fu;
            a = fa + footprint;
            if a > 512 {
                u += 1;
                a = 1;
            }
            assigned += 1;
        }
        assigned
    }

    /// Patch a specific set of fixtures (the Scene/viewport `P` dialog), packing
    /// them sequentially from `start_universe`.`start_addr` and skipping channels
    /// already claimed by OTHER enabled entries (so we don't clobber an existing
    /// rig). Each fixture is (re)enabled as it lands, so the next one packs after
    /// it. Indices that don't exist are ignored. Returns the number patched.
    pub fn assign_indices(
        &mut self,
        scene: &Scene,
        indices: &[usize],
        start_universe: u16,
        start_addr: u16,
    ) -> usize {
        self.sync(scene);
        let mut u = start_universe.max(1);
        let mut a = start_addr.max(1);
        let mut assigned = 0;
        for &i in indices {
            let footprint = match self.entries.get(i).and_then(|e| e.patch.as_ref()) {
                Some(p) => p.footprint.max(1),
                None => continue,
            };
            let (fu, fa) = self.next_free(u, a, footprint, i);
            if let Some(p) = self.entries[i].patch.as_mut() {
                p.universe = fu;
                p.address = fa;
                p.enabled = true;
            }
            u = fu;
            a = fa + footprint;
            if a > 512 {
                u += 1;
                a = 1;
            }
            assigned += 1;
        }
        assigned
    }

    /// Disable a fixture's patch entry (the `U` unpatch dialog) — it stops
    /// decoding + frees its grid channels while keeping the entry aligned to
    /// `scene.fixtures`. Returns true if an entry was disabled.
    pub fn unpatch(&mut self, i: usize) -> bool {
        match self.get_mut(i) {
            Some(p) if p.enabled => {
                p.enabled = false;
                true
            }
            _ => false,
        }
    }

    /// First `(universe, address)` at or after `(u, a)` where `footprint` channels
    /// fit without overlapping any enabled entry (excluding fixture `skip`).
    fn next_free(&self, mut u: u16, mut a: u16, footprint: u16, skip: usize) -> (u16, u16) {
        u = u.max(1);
        a = a.max(1);
        loop {
            if a + footprint - 1 > 512 {
                u += 1;
                a = 1;
                continue;
            }
            let (lo, hi) = (a, a + footprint - 1);
            let clash = self.entries.iter().enumerate().any(|(j, e)| {
                if j == skip {
                    return false;
                }
                match &e.patch {
                    Some(p) if p.enabled && p.universe == u => {
                        let plo = p.address;
                        let phi = p.address + p.footprint.saturating_sub(1);
                        lo <= phi && plo <= hi
                    }
                    _ => false,
                }
            });
            if !clash {
                return (u, a);
            }
            a += 1;
        }
    }

    /// Change a fixture's patched mode, recomputing footprint and flagging Manual.
    pub fn set_mode(&mut self, fixture: &Fixture, i: usize, mode_index: usize) {
        let footprint = footprint_for(fixture, mode_index);
        if let Some(p) = self.get_mut(i) {
            p.mode_index = mode_index;
            p.footprint = footprint;
            p.source = PatchSource::Manual;
        }
    }

    /// Every distinct universe referenced by an enabled entry, sorted.
    pub fn universes(&self) -> Vec<u16> {
        let mut us: Vec<u16> = self
            .entries
            .iter()
            .filter_map(|e| e.patch.as_ref())
            .filter(|p| p.enabled)
            .map(|p| p.universe)
            .collect();
        us.sort_unstable();
        us.dedup();
        us
    }

    /// The fixture index + in-footprint offset (0-based) occupying `channel`
    /// (1-based) in `universe`, among enabled entries. The first match wins.
    /// (Footprint-span lookup; the DMX grid instead uses a per-channel map that
    /// excludes gaps — kept for tests / future callers.)
    #[allow(dead_code)]
    pub fn occupant(&self, universe: u16, channel: u16) -> Option<(usize, u16)> {
        self.entries.iter().enumerate().find_map(|(i, e)| {
            let p = e.patch.as_ref()?;
            if p.enabled && p.universe == universe && channel >= p.address && channel < p.address + p.footprint {
                Some((i, channel - p.address))
            } else {
                None
            }
        })
    }

    /// All overlapping enabled-entry pairs (advisory; never auto-resolved).
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out = Vec::new();
        let n = self.entries.len();
        for i in 0..n {
            let Some(pi) = self.enabled_at(i) else { continue };
            let (ilo, ihi) = (pi.address, pi.address + pi.footprint.saturating_sub(1));
            for j in (i + 1)..n {
                let Some(pj) = self.enabled_at(j) else { continue };
                if pj.universe != pi.universe {
                    continue;
                }
                let (jlo, jhi) = (pj.address, pj.address + pj.footprint.saturating_sub(1));
                if ilo <= jhi && jlo <= ihi {
                    out.push(Conflict {
                        a: i,
                        b: j,
                        universe: pi.universe,
                        range: (ilo.max(jlo), ihi.min(jhi)),
                    });
                }
            }
        }
        out
    }
}

/// Seed one fixture's patch entry: MVR address first, else a GDTF footprint, else
/// the synthetic map. GDTF/plain entries start disabled (await auto-assign).
fn reconcile_one(fixture: &Fixture) -> Option<Patch> {
    if let Some(meta) = fixture.mvr.as_deref()
        && let Some(addr) = meta.addresses.first()
    {
        let mode_index = fixture
            .gdtf
            .as_ref()
            .and_then(|g| g.modes.iter().position(|m| m.name == meta.gdtf_mode))
            .unwrap_or(0);
        return Some(Patch {
            universe: addr.universe() as u16,
            address: addr.channel() as u16,
            footprint: footprint_for(fixture, mode_index),
            mode_index,
            enabled: true,
            source: PatchSource::Mvr,
        });
    }
    let (footprint, source) = if fixture.gdtf.is_some() {
        (footprint_for(fixture, fixture.mode_index), PatchSource::Gdtf)
    } else {
        (SYNTH_FOOTPRINT, PatchSource::Synthetic)
    };
    Some(Patch {
        universe: 1,
        address: 1,
        footprint,
        mode_index: fixture.mode_index,
        enabled: false,
        source,
    })
}

/// A cheap identity hash so [`PatchTable::sync`] can detect when entries no
/// longer line up with the fixtures (a wholesale replacement vs an append).
fn fingerprint(f: &Fixture) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    f.name.hash(&mut h);
    f.profile.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvr::{MvrAddress, MvrFixtureMeta};
    use crate::scene::{Library, Scene};
    use glam::Vec3;

    /// A scene of `n` plain PAR cans (synthetic map, footprint 8).
    fn plain_scene(n: usize) -> Scene {
        let lib = Library::standard();
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        for i in 0..n {
            scene
                .fixtures
                .push(Fixture::from_profile(&lib.fixtures[0], format!("PAR {i}"), Vec3::ZERO));
        }
        scene
    }

    #[test]
    fn synthetic_channel_map_layout() {
        let scene = plain_scene(1);
        let map = channel_map(&scene.fixtures[0], 0);
        let got: Vec<(&str, u16, u8)> = map
            .channels
            .iter()
            .map(|c| (c.attribute.as_str(), c.offset, c.width))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Dimmer", 0, 1),
                ("ColorAdd_R", 1, 1),
                ("ColorAdd_G", 2, 1),
                ("ColorAdd_B", 3, 1),
                ("Pan", 4, 2),
                ("Tilt", 6, 2),
            ]
        );
        assert_eq!(footprint_for(&scene.fixtures[0], 0), SYNTH_FOOTPRINT);
    }

    #[test]
    fn reconcile_from_mvr_address() {
        let scene = {
            let lib = Library::standard();
            let mut s = Scene::demo();
            s.fixtures.clear();
            let mut f = Fixture::from_profile(&lib.fixtures[0], "Imported", Vec3::ZERO);
            f.mvr = Some(Box::new(MvrFixtureMeta {
                // absolute 513 -> universe 2, channel 1 (1-based MvrAddress math).
                addresses: vec![MvrAddress { break_id: 0, absolute: 513 }],
                ..Default::default()
            }));
            s.fixtures.push(f);
            s
        };
        let mut table = PatchTable::new();
        table.reconcile_from_scene(&scene);
        let p = table.get(0).expect("patch");
        assert_eq!((p.universe, p.address), (2, 1));
        assert_eq!(p.footprint, SYNTH_FOOTPRINT);
        assert!(p.enabled);
        assert_eq!(p.source, PatchSource::Mvr);
    }

    #[test]
    fn auto_assign_packs_and_wraps() {
        // Footprint 8 each; 64 fixtures fill exactly universe 1 (512 ch), the 65th
        // wraps to universe 2.
        let scene = plain_scene(65);
        let mut table = PatchTable::new();
        table.auto_assign(&scene, 1, 1);
        assert_eq!((table.get(0).unwrap().universe, table.get(0).unwrap().address), (1, 1));
        assert_eq!((table.get(1).unwrap().universe, table.get(1).unwrap().address), (1, 9));
        assert_eq!((table.get(63).unwrap().universe, table.get(63).unwrap().address), (1, 505));
        let last = table.get(64).unwrap();
        assert_eq!((last.universe, last.address), (2, 1), "65th fixture wraps to universe 2");
        assert!(table.conflicts().is_empty(), "sequential pack has no overlaps");
    }

    #[test]
    fn auto_assign_skips_existing_enabled_ranges() {
        // One MVR fixture pinned at u1 ch1..8, then a plain fixture auto-assigned:
        // it must skip past the MVR range to ch 9.
        let scene = {
            let lib = Library::standard();
            let mut s = Scene::demo();
            s.fixtures.clear();
            let mut mvr = Fixture::from_profile(&lib.fixtures[0], "Desk", Vec3::ZERO);
            mvr.mvr = Some(Box::new(MvrFixtureMeta {
                addresses: vec![MvrAddress { break_id: 0, absolute: 1 }],
                ..Default::default()
            }));
            s.fixtures.push(mvr);
            s.fixtures
                .push(Fixture::from_profile(&lib.fixtures[0], "New", Vec3::ZERO));
            s
        };
        let mut table = PatchTable::new();
        table.auto_assign(&scene, 1, 1);
        assert_eq!((table.get(0).unwrap().universe, table.get(0).unwrap().address), (1, 1));
        assert_eq!((table.get(1).unwrap().universe, table.get(1).unwrap().address), (1, 9));
        assert!(table.conflicts().is_empty());
    }

    #[test]
    fn conflicts_detects_overlap_not_adjacency() {
        let scene = plain_scene(2);
        let mut table = PatchTable::new();
        table.sync(&scene);
        // Adjacent: [1,8] and [9,16] — no conflict.
        {
            let p = table.get_mut(0).unwrap();
            (p.universe, p.address, p.enabled) = (1, 1, true);
        }
        {
            let p = table.get_mut(1).unwrap();
            (p.universe, p.address, p.enabled) = (1, 9, true);
        }
        assert!(table.conflicts().is_empty());
        // Overlap: move the second to ch 5 -> [5,12] overlaps [1,8].
        table.get_mut(1).unwrap().address = 5;
        let c = table.conflicts();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].range, (5, 8));
        assert_eq!((c[0].a, c[0].b), (0, 1));
    }

    #[test]
    fn sync_appends_for_new_fixtures_preserving_edits() {
        let lib = Library::standard();
        let mut scene = plain_scene(1);
        let mut table = PatchTable::new();
        table.auto_assign(&scene, 1, 1);
        // User edits fixture 0's address.
        table.get_mut(0).unwrap().address = 100;
        // Add a fixture; sync should append, not wipe the edit.
        scene
            .fixtures
            .push(Fixture::from_profile(&lib.fixtures[0], "PAR new", Vec3::ZERO));
        table.sync(&scene);
        assert!(table.get(2).is_none(), "table synced to 2 fixtures");
        assert_eq!(table.get(0).unwrap().address, 100, "edit preserved across append");
        assert!(!table.get(1).unwrap().enabled, "new fixture starts unpatched");
    }

    #[test]
    fn assign_indices_patches_only_selection_sequentially() {
        let scene = plain_scene(4);
        let mut table = PatchTable::new();
        table.sync(&scene);
        // Patch only fixtures 1 and 3 from universe 1, address 1 (footprint 8).
        let n = table.assign_indices(&scene, &[1, 3], 1, 1);
        assert_eq!(n, 2);
        let p1 = table.get(1).unwrap();
        assert_eq!((p1.universe, p1.address, p1.enabled), (1, 1, true));
        let p3 = table.get(3).unwrap();
        assert_eq!((p3.universe, p3.address, p3.enabled), (1, 9, true));
        // The unselected fixtures stay unpatched.
        assert!(!table.get(0).unwrap().enabled);
        assert!(!table.get(2).unwrap().enabled);
    }

    #[test]
    fn assign_indices_skips_existing_enabled_entries() {
        let scene = plain_scene(2);
        let mut table = PatchTable::new();
        table.sync(&scene);
        // Fixture 0 already patched at 1.1 (footprint 8 -> claims 1..8).
        table.assign_indices(&scene, &[0], 1, 1);
        // Patching fixture 1 from 1.1 must skip past fixture 0 -> 1.9.
        table.assign_indices(&scene, &[1], 1, 1);
        let p1 = table.get(1).unwrap();
        assert_eq!((p1.universe, p1.address), (1, 9));
    }

    #[test]
    fn unpatch_disables_entry() {
        let scene = plain_scene(1);
        let mut table = PatchTable::new();
        table.auto_assign(&scene, 1, 1);
        assert!(table.get(0).unwrap().enabled);
        assert!(table.unpatch(0));
        assert!(!table.get(0).unwrap().enabled);
        // Idempotent: a second unpatch is a no-op.
        assert!(!table.unpatch(0));
    }

    #[test]
    fn occupant_maps_channel_to_fixture_and_offset() {
        let scene = plain_scene(2);
        let mut table = PatchTable::new();
        table.auto_assign(&scene, 1, 1); // fixture 0 -> 1..8, fixture 1 -> 9..16
        assert_eq!(table.occupant(1, 1), Some((0, 0)));
        assert_eq!(table.occupant(1, 5), Some((0, 4)));
        assert_eq!(table.occupant(1, 9), Some((1, 0)));
        assert_eq!(table.occupant(1, 17), None);
        assert_eq!(table.universes(), vec![1]);
    }
}
