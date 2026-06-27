//! View / camera bookmarks (P1 #34) — numbered saved camera poses.
//!
//! A bookmark is a named [`CameraPose`](crate::renderer::camera::CameraPose) in a
//! numbered slot. Saving captures the live orbit pose (target / yaw / pitch /
//! distance / fov / ortho); recalling eases the camera there via the shared
//! `animate_to` transition (`OrbitCamera::apply_pose`). The set persists to
//! `bookmarks.json` in the per-user config dir, mirroring `lib_prefs::LibraryPrefs`
//! / the keymap overrides — loaded once at startup, saved synchronously on each
//! edit (the payload is tiny).
//!
//! Slots are 1-based for display (Blender's "View 1…", Unreal's Ctrl+1 bookmarks):
//! "Save view bookmark" drops the pose into the next free slot; the F3 palette /
//! Window strip recall or delete by number. Capped so the strip stays readable.

use std::path::PathBuf;

use crate::renderer::camera::CameraPose;

/// How many numbered bookmark slots we keep (Blender exposes 1–9; we match it so
/// the slot number stays a single keystroke for a future digit-bind).
pub const SLOT_CAP: usize = 9;

/// One saved view: a numbered, named camera pose.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    /// 1-based slot number (display + recall key). Stable across deletes of OTHER
    /// slots, so a recall-by-number keeps targeting the same shot.
    pub slot: usize,
    /// User-facing label (defaults to "View N"; rename is a later affordance).
    pub name: String,
    /// The saved camera pose.
    pub pose: CameraPose,
}

/// The persisted set of view bookmarks. Loaded once at startup; saved on every
/// mutation. EMPTY by default — a fresh install shows no bookmarks.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Bookmarks {
    /// Saved views, kept sorted by `slot` so the strip renders 1…9 in order.
    pub items: Vec<Bookmark>,
}

impl Bookmarks {
    /// Load from disk (config dir); a missing/garbled file yields the default
    /// (empty) set, so a fresh install / corrupt file never blocks startup.
    pub fn load() -> Self {
        let Some(p) = bookmarks_path() else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(&p) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Persist to disk (best-effort; a write failure is non-fatal — same policy as
    /// `lib_prefs`/keymap overrides).
    fn save(&self) {
        let Some(p) = bookmarks_path() else { return };
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&p, text);
        }
    }

    /// The lowest unused 1-based slot number, or `None` when all [`SLOT_CAP`] slots
    /// are taken. Used by [`save_pose`](Self::save_pose) to pick the next slot.
    pub fn next_free_slot(&self) -> Option<usize> {
        (1..=SLOT_CAP).find(|n| !self.items.iter().any(|b| b.slot == *n))
    }

    /// Save `pose` into the next free slot, returning the slot number (or `None`
    /// when full). Names it "View N"; keeps `items` slot-sorted; persists.
    pub fn save_pose(&mut self, pose: CameraPose) -> Option<usize> {
        let slot = self.next_free_slot()?;
        self.items.push(Bookmark { slot, name: format!("View {slot}"), pose });
        self.items.sort_by_key(|b| b.slot);
        self.save();
        Some(slot)
    }

    /// Save `pose` into an explicit `slot` (1-based), OVERWRITING any pose already
    /// there (a future digit "set bookmark N" re-save / overwrite affordance).
    /// Out-of-range slots are ignored. Persists. Returns whether anything was
    /// written. (Tested here; the overwrite UI lands with the digit binds.)
    #[allow(dead_code)]
    pub fn set_slot(&mut self, slot: usize, pose: CameraPose) -> bool {
        if !(1..=SLOT_CAP).contains(&slot) {
            return false;
        }
        if let Some(b) = self.items.iter_mut().find(|b| b.slot == slot) {
            b.pose = pose;
        } else {
            self.items.push(Bookmark { slot, name: format!("View {slot}"), pose });
            self.items.sort_by_key(|b| b.slot);
        }
        self.save();
        true
    }

    /// The pose saved in `slot` (1-based), or `None` if that slot is empty — the
    /// recall lookup (`OrbitCamera::apply_pose` consumes the returned pose).
    pub fn pose_in_slot(&self, slot: usize) -> Option<CameraPose> {
        self.items.iter().find(|b| b.slot == slot).map(|b| b.pose)
    }

    /// Delete the bookmark in `slot` (1-based), persisting. Returns whether a slot
    /// was actually removed. Other slots keep their numbers (so recall-by-number is
    /// stable across a delete).
    pub fn delete_slot(&mut self, slot: usize) -> bool {
        let before = self.items.len();
        self.items.retain(|b| b.slot != slot);
        let removed = self.items.len() != before;
        if removed {
            self.save();
        }
        removed
    }
}

/// `<config>/bookmarks.json` — the per-user store, alongside `library.json` /
/// `keymap.json`.
fn bookmarks_path() -> Option<PathBuf> {
    let d = directories::ProjectDirs::from("build", "glowstone", "glowstone")?;
    let dir = d.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("bookmarks.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f32) -> CameraPose {
        CameraPose { target: [x, 1.0, -2.0], yaw: 0.5, pitch: 0.1, distance: 8.0, fov_y: 0.9, ortho: false }
    }

    /// A bookmark set round-trips through JSON unchanged (the persistence path).
    #[test]
    fn bookmarks_round_trip_through_json() {
        let mut b = Bookmarks::default();
        b.items.push(Bookmark { slot: 1, name: "FOH".into(), pose: pose(3.0) });
        b.items.push(Bookmark { slot: 2, name: "Side".into(), pose: pose(-4.0) });
        let text = serde_json::to_string(&b).unwrap();
        let back: Bookmarks = serde_json::from_str(&text).unwrap();
        assert_eq!(back.items.len(), 2);
        assert_eq!(back.items[0].slot, 1);
        assert_eq!(back.items[0].name, "FOH");
        assert_eq!(back.items[0].pose, pose(3.0));
        assert_eq!(back.items[1].pose, pose(-4.0));
    }

    /// Saving fills the lowest free slot; recall returns that exact pose.
    #[test]
    fn save_fills_next_slot_and_recall_round_trips() {
        let mut b = Bookmarks::default();
        assert_eq!(b.save_pose(pose(1.0)), Some(1));
        assert_eq!(b.save_pose(pose(2.0)), Some(2));
        // The recalled pose matches what was saved into that slot.
        assert_eq!(b.pose_in_slot(1), Some(pose(1.0)));
        assert_eq!(b.pose_in_slot(2), Some(pose(2.0)));
        assert_eq!(b.pose_in_slot(3), None);
    }

    /// Delete frees a slot WITHOUT renumbering the others; the next save reuses the
    /// freed number (lowest-free rule).
    #[test]
    fn delete_frees_slot_without_renumbering() {
        let mut b = Bookmarks::default();
        b.save_pose(pose(1.0));
        b.save_pose(pose(2.0));
        b.save_pose(pose(3.0));
        assert!(b.delete_slot(2));
        // Slot 1 and 3 keep their numbers/poses.
        assert_eq!(b.pose_in_slot(1), Some(pose(1.0)));
        assert_eq!(b.pose_in_slot(3), Some(pose(3.0)));
        assert_eq!(b.pose_in_slot(2), None);
        // The next save reuses the freed slot 2 (lowest free).
        assert_eq!(b.save_pose(pose(9.0)), Some(2));
        assert_eq!(b.pose_in_slot(2), Some(pose(9.0)));
        // Deleting a non-existent slot is a no-op.
        assert!(!b.delete_slot(7));
    }

    /// Slots cap at [`SLOT_CAP`]; a full set returns `None` from `save_pose`.
    #[test]
    fn slots_cap_out() {
        let mut b = Bookmarks::default();
        for _ in 0..SLOT_CAP {
            assert!(b.save_pose(pose(0.0)).is_some());
        }
        assert_eq!(b.next_free_slot(), None);
        assert_eq!(b.save_pose(pose(0.0)), None);
    }

    /// `set_slot` overwrites an occupied slot in place and rejects out-of-range.
    #[test]
    fn set_slot_overwrites_and_bounds_check() {
        let mut b = Bookmarks::default();
        assert!(b.set_slot(3, pose(1.0)));
        assert_eq!(b.pose_in_slot(3), Some(pose(1.0)));
        assert!(b.set_slot(3, pose(2.0))); // overwrite
        assert_eq!(b.pose_in_slot(3), Some(pose(2.0)));
        assert!(!b.set_slot(0, pose(0.0))); // out of range
        assert!(!b.set_slot(SLOT_CAP + 1, pose(0.0)));
    }
}
