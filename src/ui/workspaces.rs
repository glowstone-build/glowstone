//! User-savable **workspaces** (S1) — the soft "modes" of the lighting workflow.
//!
//! A workspace is a SAVED RECORD, not a hardcoded layout: a name, a serialized
//! [`DockState<Tab>`] (the panel arrangement), a default [`ActiveTool`], and the
//! set of viewport overlay flags to set on activation. Activating one APPLIES the
//! layout, SETS the default tool, and emphasises the recorded overlays — it does
//! NOT lock or gate anything (the design note: modes are soft contexts; everything
//! stays editable). It is a starting arrangement, nothing more.
//!
//! The four built-ins (Design / Patch / Focus / Visualise) are generated as this
//! same record shape, so a built-in and a user workspace are interchangeable. The
//! user's set persists to `workspaces.json` in the per-user config dir, mirroring
//! `bookmarks.json` / `keymap.json` — loaded once at startup, saved on each
//! save/delete. When the file is ABSENT (fresh install / corrupt) the built-ins
//! regenerate, so the workflow presets are always present.
//!
//! "Save current as workspace…" captures the LIVE dock layout + the current active
//! tool + the current overlay flags under a name, appending (or overwriting a
//! same-named) record — so a user can shape a layout and keep it.

use std::path::PathBuf;

use egui_dock::{DockState, NodeIndex};

use super::Tab;
use super::tools::ActiveTool;

/// How many workspaces get a stable indexed `workspace.activate_N` command (for the
/// F3 palette / keymap). The tab strip + Window menu can switch ANY workspace; only
/// the first [`SLOT_CAP`] get a bindable command id (mirrors the bookmark slot cap).
/// Read by the `slot_commands_cover_cap` test that pins the registered command set to
/// this cap; not referenced at runtime (the commands are listed statically).
#[allow(dead_code)]
pub const SLOT_CAP: usize = 9;

/// The viewport overlay flags a workspace records + restores on activation. A subset
/// of [`super::Preferences`] / [`crate::scene::RenderSettings`] — exactly the bits a
/// workspace "emphasises". Captured from the live UI by [`Workspaces::capture_overlays`]
/// and re-applied by [`Ui`](super::Ui) on activate. NOT a lock — the user can still
/// toggle any of these afterward.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Overlays {
    /// Fixture name/patch labels (`Preferences::show_labels`).
    pub labels: bool,
    /// Scene-statistics corner overlay (`Preferences::show_stats`).
    pub stats: bool,
    /// Origin grid + world axes (`RenderSettings::show_grid`).
    pub grid: bool,
    /// Navigation + transform gizmos (`Preferences::show_gizmos`).
    pub gizmos: bool,
}

/// One saved workspace record — the whole feature's unit of persistence.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Workspace {
    /// User-facing name (the tab-strip / Window-menu label). Unique within the set
    /// (a same-named save overwrites).
    pub name: String,
    /// The serialized panel arrangement applied on activation. Serialized through
    /// [`dock_serde`] which zeroes the layout rects first: egui_dock stores each
    /// node's rect as a bare [`egui::Rect`] that is [`Rect::NOTHING`] (infinite)
    /// until the dock is first drawn, and serde_json writes `f32::INFINITY` as
    /// `null` then FAILS to read it back ("invalid type: null, expected f32"). The
    /// rects are recomputed every `show()`, so zeroing them on the way out is loss-
    /// less and makes the JSON round-trip cleanly.
    #[serde(with = "dock_serde")]
    pub dock: DockState<Tab>,
    /// The viewport tool made active on activation (soft default — switchable after).
    pub default_tool: ActiveTool,
    /// The overlay flags emphasised on activation.
    pub overlays: Overlays,
    /// `true` for the four shipped presets. User saves are `false`. Built-ins can't
    /// be deleted from the UI (they regenerate anyway); the flag drives that guard.
    #[serde(default)]
    pub builtin: bool,
}

/// The persisted set of workspaces + the live active index. Loaded once at startup;
/// saved on each save/delete. When the file is missing the built-ins seed it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Workspaces {
    /// All workspaces in display (tab-strip) order: the built-ins first, then user
    /// saves in save order.
    pub items: Vec<Workspace>,
    /// Index into `items` of the currently active workspace (clamped on load).
    #[serde(default)]
    pub active: usize,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self {
            items: builtins(),
            active: 0,
        }
    }
}

impl Workspaces {
    /// Load from `workspaces.json`; a missing/garbled file yields the built-in set
    /// (so the workflow presets are always present). If the file loaded but somehow
    /// holds no built-ins (hand-edited), they're re-prepended so they never vanish.
    pub fn load() -> Self {
        let Some(p) = workspaces_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return Self::default();
        };
        let mut ws: Self = match serde_json::from_str(&text) {
            Ok(ws) => ws,
            Err(_) => return Self::default(),
        };
        ws.ensure_builtins();
        // Always open in the Design workspace (the default working layout), regardless
        // of which workspace was active when the app last closed.
        ws.active = ws
            .items
            .iter()
            .position(|w| w.name == "Design")
            .unwrap_or(0);
        ws
    }

    /// Persist to disk (best-effort; a write failure is non-fatal — same policy as
    /// `bookmarks`/keymap overrides).
    fn save(&self) {
        let Some(p) = workspaces_path() else { return };
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&p, text);
        }
    }

    /// Re-add any missing built-in (by name), keeping them at the front in canonical
    /// order. Used on load so a hand-edited / partial file can never drop a preset.
    fn ensure_builtins(&mut self) {
        for (i, b) in builtins().into_iter().enumerate() {
            if !self.items.iter().any(|w| w.name == b.name) {
                self.items.insert(i.min(self.items.len()), b);
            }
        }
    }

    /// The active workspace record.
    pub fn active(&self) -> &Workspace {
        &self.items[self.active.min(self.items.len() - 1)]
    }

    /// Set the active index (clamped), persisting the choice so the app reopens on
    /// the same workspace.
    pub fn set_active(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.active = idx;
            self.save();
        }
    }

    /// Save `name` (overwriting a same-named record) capturing the supplied live
    /// layout + tool + overlays, mark it active, and persist. Returns the index. A
    /// save over a built-in keeps the `builtin` flag false on the new user copy
    /// UNLESS it overwrites the built-in slot (then it stays a built-in shape but
    /// with the captured layout — Blender lets you re-save a default workspace).
    pub fn save_current(
        &mut self,
        name: &str,
        dock: DockState<Tab>,
        default_tool: ActiveTool,
        overlays: Overlays,
    ) -> usize {
        let name = name.trim();
        let name = if name.is_empty() { "Workspace" } else { name };
        if let Some(i) = self.items.iter().position(|w| w.name == name) {
            let builtin = self.items[i].builtin;
            self.items[i] = Workspace {
                name: name.to_string(),
                dock,
                default_tool,
                overlays,
                builtin,
            };
            self.active = i;
            self.save();
            return i;
        }
        self.items.push(Workspace {
            name: name.to_string(),
            dock,
            default_tool,
            overlays,
            builtin: false,
        });
        self.active = self.items.len() - 1;
        self.save();
        self.active
    }

    /// Delete the workspace at `idx` (a no-op for built-ins / out-of-range / the last
    /// remaining record). Re-clamps `active` and persists. Returns whether removed.
    pub fn delete(&mut self, idx: usize) -> bool {
        if idx >= self.items.len() || self.items[idx].builtin || self.items.len() <= 1 {
            return false;
        }
        self.items.remove(idx);
        if self.active >= self.items.len() {
            self.active = self.items.len() - 1;
        }
        self.save();
        true
    }

    /// Bundle the current overlay flags into an [`Overlays`] record (the capture side
    /// of "save current as workspace"). A free helper so the field mapping lives in
    /// one place.
    pub fn capture_overlays(labels: bool, stats: bool, grid: bool, gizmos: bool) -> Overlays {
        Overlays {
            labels,
            stats,
            grid,
            gizmos,
        }
    }
}

/// Build the canonical built-in workspaces (Design / Patch / Focus / Visualise) as
/// records. Regenerated when `workspaces.json` is absent; also used to backfill any
/// missing preset on load. Each pre-arranges the panels + presets a sensible default
/// tool + overlay emphasis for one stage of the lighting workflow.
pub fn builtins() -> Vec<Workspace> {
    vec![
        // DESIGN: the everyday balanced layout — outliner + library left, inspector
        // right, the Fixtures/DMX data as a bottom strip. Move tool + full overlays.
        Workspace {
            name: "Design".into(),
            dock: design_dock(),
            default_tool: ActiveTool::Move,
            overlays: Overlays {
                labels: true,
                stats: false,
                grid: true,
                gizmos: true,
            },
            builtin: true,
        },
        // PATCH: the systems tech — the Fixtures sheet + DMX dominate a tall bottom
        // data area; the viewport just orients. Select tool; grid on, labels on.
        Workspace {
            name: "Patch".into(),
            dock: patch_dock(),
            default_tool: ActiveTool::Select,
            overlays: Overlays {
                labels: true,
                stats: false,
                grid: true,
                gizmos: true,
            },
            builtin: true,
        },
        // FOCUS: aim / select-oriented — the focusing session. Thin Scene + Inspector
        // flanking a big viewport; the Aim tool active; labels + gizmos on so heads
        // and beams read, grid off to declutter the look.
        Workspace {
            name: "Focus".into(),
            dock: focus_dock(),
            default_tool: ActiveTool::Aim,
            overlays: Overlays {
                labels: true,
                stats: false,
                grid: false,
                gizmos: true,
            },
            builtin: true,
        },
        // VISUALISE: the glowstone artist — maximise the viewport, minimal chrome. Select
        // tool; overlays mostly off (clean render look), gizmos off.
        Workspace {
            name: "Visualise".into(),
            dock: visualise_dock(),
            default_tool: ActiveTool::Select,
            overlays: Overlays {
                labels: false,
                stats: false,
                grid: true,
                gizmos: false,
            },
            builtin: true,
        },
    ]
}

// --- the built-in dock layouts (unchanged geometry from the old `workspace_dock`) ---
//
// egui_dock's `fraction` is the share given to the side being split toward.

fn design_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Viewport]);
    let surface = dock.main_surface_mut();
    let [c, _l] = surface.split_left(NodeIndex::root(), 0.17, vec![Tab::Scene, Tab::Library]);
    let [c, _i] = surface.split_right(c, 0.79, vec![Tab::Inspector]);
    surface.split_below(
        c,
        0.70,
        vec![Tab::Patch, Tab::DmxMonitor, Tab::Cues, Tab::Connectivity],
    );
    dock
}

fn patch_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Viewport]);
    let surface = dock.main_surface_mut();
    let [c, _l] = surface.split_left(NodeIndex::root(), 0.16, vec![Tab::Scene]);
    let [c, _i] = surface.split_right(c, 0.80, vec![Tab::Inspector]);
    surface.split_below(
        c,
        0.42,
        vec![Tab::Patch, Tab::DmxMonitor, Tab::Cues, Tab::Connectivity],
    );
    dock
}

fn focus_dock() -> DockState<Tab> {
    // Aim-oriented: a thin Scene outliner left, Inspector right, the viewport wide in
    // between — no bottom data strip (focusing is a 3D task).
    let mut dock = DockState::new(vec![Tab::Viewport]);
    let surface = dock.main_surface_mut();
    let [c, _l] = surface.split_left(NodeIndex::root(), 0.15, vec![Tab::Scene]);
    surface.split_right(c, 0.80, vec![Tab::Inspector]);
    dock
}

fn visualise_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Viewport]);
    let surface = dock.main_surface_mut();
    let [c, _l] = surface.split_left(NodeIndex::root(), 0.15, vec![Tab::Scene]);
    surface.split_right(c, 0.82, vec![Tab::Inspector]);
    dock
}

/// `#[serde(with)]` adapter for the [`Workspace::dock`] field. egui_dock's
/// [`DockState`] is itself `Serialize`/`Deserialize`, but a freshly-built (un-drawn)
/// layout holds [`Rect::NOTHING`] node rects whose `f32::INFINITY` coords serde_json
/// emits as `null` and then refuses to read back. This adapter zeroes every node's
/// rect before serializing (the rects are recomputed each `show()`, so this is
/// loss-less) so the JSON round-trips. Deserialization is the plain derive.
mod dock_serde {
    use super::*;
    use egui::Rect;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(dock: &DockState<Tab>, s: S) -> Result<S::Ok, S::Error> {
        // Clone + zero the node rects so no infinite float reaches serde_json. Only
        // the main surface is built by our presets, so iterating it covers the layout.
        // Both the node `rect` (set via `set_rect`) AND a leaf's separate `viewport`
        // rect default to `Rect::NOTHING`, so both must be zeroed.
        let mut clone = dock.clone();
        for node in clone.main_surface_mut().iter_mut() {
            node.set_rect(Rect::ZERO);
            if let Some(leaf) = node.get_leaf_mut() {
                leaf.viewport = Rect::ZERO;
            }
        }
        clone.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DockState<Tab>, D::Error> {
        DockState::<Tab>::deserialize(d)
    }
}

/// `<config>/workspaces.json` — the per-user store, alongside `bookmarks.json` /
/// `keymap.json`.
fn workspaces_path() -> Option<PathBuf> {
    let d = directories::ProjectDirs::from("build", "glowstone", "glowstone")?;
    let dir = d.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("workspaces.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-ins are present, named, and flagged as built-in (the regenerate-on-
    /// missing-file contract: `Default`/`load`-with-no-file yields these four).
    #[test]
    fn builtins_present_when_file_missing() {
        let ws = Workspaces::default();
        let names: Vec<&str> = ws.items.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Design", "Patch", "Focus", "Visualise"]);
        assert!(ws.items.iter().all(|w| w.builtin), "all four are built-in");
        // Focus is the new aim/select-oriented built-in with the Aim tool.
        let focus = ws.items.iter().find(|w| w.name == "Focus").unwrap();
        assert_eq!(focus.default_tool, ActiveTool::Aim);
    }

    /// A whole set (built-ins + a user save) round-trips through JSON unchanged,
    /// including the serialized DockState, the default tool, and the overlay flags.
    #[test]
    fn round_trips_through_json() {
        let mut ws = Workspaces::default();
        let overlays = Overlays {
            labels: false,
            stats: true,
            grid: false,
            gizmos: true,
        };
        let idx = ws.save_current("My Focus", focus_dock(), ActiveTool::Rotate, overlays);
        assert_eq!(idx, 4); // appended after the four built-ins
        assert_eq!(ws.active, 4);

        let text = serde_json::to_string(&ws).unwrap();
        let back: Workspaces = serde_json::from_str(&text).unwrap();
        assert_eq!(back.items.len(), 5);
        assert_eq!(back.active, 4);
        let saved = &back.items[4];
        assert_eq!(saved.name, "My Focus");
        assert_eq!(saved.default_tool, ActiveTool::Rotate);
        assert_eq!(saved.overlays, overlays);
        assert!(!saved.builtin);
        // The serialized dock survived (tab count + main-surface shape preserved).
        let before_tabs: Vec<_> = ws.items[4].dock.iter_all_tabs().collect();
        let after_tabs: Vec<_> = saved.dock.iter_all_tabs().collect();
        assert_eq!(before_tabs.len(), after_tabs.len());
    }

    /// Saving under an existing name OVERWRITES that record (no duplicate row) and
    /// preserves the built-in flag when overwriting a built-in slot.
    #[test]
    fn save_overwrites_same_name() {
        let mut ws = Workspaces::default();
        let n0 = ws.items.len();
        let ov = Overlays {
            labels: true,
            stats: true,
            grid: true,
            gizmos: false,
        };
        let idx = ws.save_current("Design", visualise_dock(), ActiveTool::Scale, ov);
        assert_eq!(idx, 0, "overwrites the existing Design slot");
        assert_eq!(ws.items.len(), n0, "no new row");
        assert_eq!(ws.items[0].default_tool, ActiveTool::Scale);
        assert!(
            ws.items[0].builtin,
            "overwriting a built-in slot keeps it built-in"
        );
    }

    /// Built-ins can't be deleted; user workspaces can, and `active` re-clamps.
    #[test]
    fn delete_guards_builtins_and_clamps_active() {
        let mut ws = Workspaces::default();
        let ov = Overlays {
            labels: true,
            stats: false,
            grid: true,
            gizmos: true,
        };
        ws.save_current("Mine", focus_dock(), ActiveTool::Move, ov);
        assert_eq!(ws.active, 4);
        // A built-in delete is refused.
        assert!(!ws.delete(0));
        assert_eq!(ws.items.len(), 5);
        // The user workspace deletes; active re-clamps into range.
        assert!(ws.delete(4));
        assert_eq!(ws.items.len(), 4);
        assert!(ws.active < ws.items.len());
    }

    /// Each of the [`SLOT_CAP`] indexed slots has a registered `window.workspace_N`
    /// command (so the F3 palette + keymap can reach the first [`SLOT_CAP`]
    /// workspaces), and they map to the matching 0-based [`ActivateWorkspace`] index.
    #[test]
    fn slot_commands_cover_cap() {
        use crate::ui::shortcuts::{Action, command};
        for n in 1..=SLOT_CAP {
            let id = format!("window.workspace_{n}");
            let cmd = command(&id).unwrap_or_else(|| panic!("missing command {id}"));
            assert!(
                cmd.action == Action::ActivateWorkspace(n - 1),
                "{id} should activate workspace index {}",
                n - 1
            );
        }
    }

    /// `ensure_builtins` backfills a preset dropped from a hand-edited file, keeping
    /// the others (the load-resilience contract).
    #[test]
    fn load_backfills_missing_builtins() {
        let mut ws = Workspaces::default();
        ws.items.retain(|w| w.name != "Focus"); // simulate a file missing Focus
        assert!(!ws.items.iter().any(|w| w.name == "Focus"));
        ws.ensure_builtins();
        assert!(
            ws.items.iter().any(|w| w.name == "Focus"),
            "Focus backfilled"
        );
    }
}
