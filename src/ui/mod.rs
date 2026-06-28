//! egui + egui_dock setup: the dock layout and the [`TabViewer`] that routes
//! each dock panel to its drawing function in [`panels`].

mod actions;
mod bookmarks;
mod chrome;
mod cues;
mod debug_hooks;
mod dock;
mod editor;
mod file_ops;
mod gizmo;
mod inspector;
mod lib_prefs;
mod library;
mod ops;
mod pies;
mod setup;
mod splash;
pub mod nav_gizmo;
mod notify;
pub(crate) mod op;
mod outliner;
mod panels;
mod pie;
mod render_panel;
pub use panels::ScreenSources;
pub use render_panel::{
    default_filename, save_image, RenderPhase, RenderRequest, RenderStatus, RenderUiState,
};
pub mod project;
mod share_window;
pub mod shortcuts;
pub mod theme;
mod tools;
mod tree;
mod viewport_math;
mod workspaces;
pub use tools::ActiveTool;
mod windows;
mod xform;
pub use xform::{PivotMode, SnapMode, SnapSettings, TransformOrientation, TransformPrefs};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui_dock::{DockArea, DockState, TabViewer};
use glam::Vec3;

use crate::renderer::camera::{CameraView, OrbitCamera};
use crate::scene::{Library, ObjectRef, RenderSettings, Scene, Selection, ViewportMode};
use windows::{Preferences, ProfileEditor};

/// Window within which consecutive arrow-key nudges coalesce into one undo step
/// (a held key repeats well inside this, so the whole drag is one undo).
const NUDGE_COALESCE: std::time::Duration = std::time::Duration::from_millis(600);

/// State of the open Duplicate dialog (the `d`-key array tool). `None` = closed.
#[derive(Clone)]
pub struct DuplicateDialog {
    /// Fixture being duplicated.
    pub fixture: usize,
    /// Per-copy translation (metres) — copy `i` is offset by `i × this`.
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Per-copy pan rotation (degrees) about world Y — copy `i` pans by `i × this`.
    pub y_angle: f32,
    /// Number of copies to make.
    pub count: u32,
}

/// Remembered Duplicate-dialog values so a fresh Duplicate reuses the LAST ones
/// instead of a fixed 36°/9 preset (which surprised users with a big fan). Starts
/// at the identity — zero offset, no fan, a single copy. Held in egui temp memory
/// so every open site (viewport + menu + catalog) and the confirm share it
/// without extra plumbing.
#[derive(Clone, Copy)]
pub(crate) struct DupDefaults {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub angle: f32,
    pub count: u32,
}

impl Default for DupDefaults {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, z: 0.0, angle: 0.0, count: 1 }
    }
}

/// Whether a text widget held keyboard focus at the START of this frame (set by
/// [`Ui::show`]). Global Enter readers (the library batch-add, the patch/unpatch/
/// add/operator dialogs) check this so committing a value box with Enter can't leak
/// into them and fire an unintended action (bug 8).
pub(crate) fn text_focus_active(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new("glowstone.text_focus")).unwrap_or(false))
}

/// The last-used Duplicate values (or all-zero defaults on first use).
pub(crate) fn dup_defaults(ctx: &egui::Context) -> DupDefaults {
    ctx.data(|d| d.get_temp::<DupDefaults>(egui::Id::new("glowstone.dup.defaults")).unwrap_or_default())
}

/// Remember the Duplicate values just used, so the next Duplicate reuses them.
pub(crate) fn set_dup_defaults(ctx: &egui::Context, v: DupDefaults) {
    ctx.data_mut(|m| m.insert_temp(egui::Id::new("glowstone.dup.defaults"), v));
}

/// Build a fresh [`DuplicateDialog`] for `fixture`, seeded from the remembered
/// last-used values (all-zero on first use — never a preset fan).
pub(crate) fn duplicate_dialog_for(ctx: &egui::Context, fixture: usize) -> DuplicateDialog {
    let d = dup_defaults(ctx);
    DuplicateDialog { fixture, x: d.x, y: d.y, z: d.z, y_angle: d.angle, count: d.count }
}

/// State of the open Replace-fixtures dialog (Shift+R). Swaps the selected
/// fixtures' type for a chosen project-library profile, in place (keeping each
/// fixture's position / aim / level). `None` = closed.
#[derive(Default)]
pub struct ReplaceDialog {
    /// Name filter over the project library.
    pub search: String,
    /// When ON, swap ONLY the visual model/beam and KEEP the fixture's identity —
    /// name, DMX patch + mode + optical state (bug 12). Default OFF: a full replace
    /// re-patches AND renames the fixture to the new profile's name.
    pub mesh_only: bool,
}

/// egui textures decoded from a GDTF fixture's images (thumbnail + wheel slot
/// media), cached per fixture type so they load once.
#[derive(Default)]
pub struct GdtfTextures {
    pub thumbnail: Option<egui::TextureHandle>,
    /// `wheels[wheel_index][slot_index]`.
    pub wheels: Vec<Vec<Option<egui::TextureHandle>>>,
    /// Inspector emitter-preview silhouettes, keyed by GDTF mode index.
    pub(crate) emitter_shapes: HashMap<usize, Vec<Option<EmitterPreviewCell>>>,
}

#[derive(Clone)]
pub(crate) struct EmitterPreviewCell {
    pub outline: Vec<[f32; 2]>,
    pub depth: f32,
    pub area: f32,
}

/// The set of dockable panels. Plain enum — each variant is one tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Tab {
    Viewport,
    Scene,
    Library,
    Inspector,
    /// Live 512-channel universe + patch grid.
    DmxMonitor,
    /// Art-Net / sACN connectivity settings + source status.
    Connectivity,
    /// Per-fixture DMX patch editor.
    Patch,
    /// Saved looks + crossfade playback (cue list).
    Cues,
    /// Live render progress + the finished still image (Save to Disk).
    Render,
}

/// A saved, named selection of fixtures (console-style "groups"). Recalled by
/// click in the Scene › Groups folder. Indices are filtered to valid range on
/// recall, so editing the rig afterwards can't crash a recall.
#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub struct SelectionGroup {
    pub name: String,
    pub fixtures: Vec<usize>,
}

/// An in-progress modal transform of the selected fixtures (Blender's G/R/S):
/// grab / rotate / scale driven by mouse motion, optionally axis-constrained,
/// confirmed by click/Enter or cancelled by Esc/right-click.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransformKind {
    Move,
    Rotate,
    Scale,
}

impl TransformKind {
    fn label(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub fn vec(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
    /// The Blender-convention axis colour (X red, Y green, Z blue). Single
    /// source of truth: the screen-space move gizmo, the modal axis-lock line,
    /// and the renderer's infinite constraint line all read this.
    pub fn color(self) -> [f32; 3] {
        match self {
            Self::X => [1.0, 0.25, 0.25],
            Self::Y => [0.35, 0.9, 0.35],
            Self::Z => [0.3, 0.6, 1.0],
        }
    }
}

/// Blender-style modal numeric entry for a transform op. A SINGLE typed value
/// (arch-viz simplification of Blender's up-to-3 components, editors/util/
/// numinput.cc) that maps onto the op's active axis: Move = metres, Rotate =
/// degrees, Scale = factor. `active` is the `hasNumInput()` gate — while true the
/// typed amount OVERRIDES the mouse; clearing the buffer (backspace past empty)
/// reverts `active` to false and hands control back to the mouse. `sign` mirrors
/// Blender's NUM_NEGATE: '-' toggles it rather than inserting a literal char.
#[derive(Default)]
pub struct NumInput {
    /// The literal typed digits/'.' (no sign char — see `sign`). Capped at 16.
    pub str: String,
    /// NUM_NEGATE: '-' toggles this; applied to the parsed value.
    pub sign: bool,
    /// == Blender hasNumInput(): typed value overrides the mouse while true.
    pub active: bool,
}

impl NumInput {
    /// Parse the buffer to a finite amount. Empty / lone '.' / parse-fail → 0.0;
    /// `sign` negates. Used by the explicit-amount apply path.
    pub fn value(&self) -> f32 {
        let v = self.str.parse::<f32>().unwrap_or(0.0);
        let v = if v.is_finite() { v } else { 0.0 };
        if self.sign { -v } else { v }
    }
    /// The typed readout for the header pill, e.g. `-4.0`. Shows a lone `-` when
    /// only the sign is set, so the user sees their keystroke land.
    pub fn display(&self) -> String {
        let s = if self.str.is_empty() { "0" } else { self.str.as_str() };
        if self.sign { format!("-{s}") } else { s.to_string() }
    }
}

pub struct TransformOp {
    pub kind: TransformKind,
    pub axis: Option<Axis>,
    /// Mouse position (screen) when the op started.
    pub start_screen: egui::Pos2,
    /// The viewport rect (screen px) the op runs in. Lets `apply_transform`
    /// reconstruct picking rays from `start_screen` / the live cursor for the
    /// #40 ray-plane ABSOLUTE gizmo drag and the #71 Vertex/Surface snap modes
    /// (which need a ray + screen-space threshold, not just a pixel delta).
    pub viewport: egui::Rect,
    /// Selection centroid (pivot for rotate/scale).
    pub pivot: Vec3,
    /// Original (fixture index, position, orientation) snapshot, for live re-apply
    /// each frame and for cancel/restore.
    pub start: Vec<(usize, Vec3, glam::Quat)>,
    /// Original (geometry index, world transform) snapshot — the static-geometry
    /// equivalent of `start`. Empty when transforming fixtures.
    pub geo_start: Vec<(usize, glam::Mat4)>,
    /// Original (LED-screen index, world transform) snapshot. Screens carry the
    /// SAME `Mat4` transform as static geometry, so they ride the identical
    /// move/rotate/scale math — this is what makes screens fully grabbable
    /// (G/R/S) like every other object. Empty unless screens are selected.
    pub screen_start: Vec<(usize, glam::Mat4)>,
    /// Original (pyro-device index, world transform) snapshot. Pyro devices carry
    /// the SAME `Mat4` transform as screens/geometry, so they ride the identical
    /// move/rotate/scale math. Empty unless pyro devices are selected.
    pub pyro_start: Vec<(usize, glam::Mat4)>,
    /// Original (environment index, center, size) snapshot. Fog volumes are
    /// axis-aligned (center + size, no orientation): Move slides the centre,
    /// Rotate orbits it about the pivot (a no-op for a lone box), and Scale grows
    /// the size in place. Empty unless an environment is selected.
    pub env_start: Vec<(usize, Vec3, Vec3)>,
    /// The axis the screen-space move gizmo is currently grabbed on. Takes
    /// precedence over `axis` (keyboard X/Y/Z lock) for the live frame, matching
    /// Blender's "click-a-handle overrides the typed constraint" behaviour. Not
    /// serialized (TransformOp is never persisted).
    pub gizmo_hovered_axis: Option<Axis>,
    /// Set when a Move op was started by grabbing a screen-space PLANE handle: the
    /// plane *normal* (the axis held FIXED). The drag then slides on the other two
    /// axes via a `ray_plane_point` absolute drag (the handle sticks to the cursor),
    /// instead of the single-axis projection a `gizmo_hovered_axis` lock drives.
    /// Mutually exclusive with `gizmo_hovered_axis`. Not serialized.
    pub gizmo_plane_normal: Option<Axis>,
    /// Set when the op was started by grabbing a screen-axis VIEW handle (#72): a
    /// Move on the screen-parallel plane (plane normal = camera forward) or a Rotate
    /// about the camera forward (Blender's white trackball ring). `apply_transform`
    /// resolves the camera basis live, so no world axis is stored. Mutually exclusive
    /// with the axis / plane locks. Not serialized.
    pub gizmo_view: bool,
    /// True when the op was started by dragging a screen-space gizmo handle (vs the
    /// modal G/R/S keys). A gizmo drag is driven by `drag_delta` and committed on
    /// pointer release; a modal op is driven by absolute mouse position and
    /// committed by a click/Enter.
    pub from_gizmo: bool,
    /// Modal numeric entry. Default = empty/inactive (mouse drives). Never
    /// serialized (TransformOp is never persisted).
    pub num: NumInput,
    /// Pivot policy (#5). When `false` (Median/Active/3D-Cursor) rotate/scale pivot
    /// about the single world `pivot`; when `true` (Individual Origins) each element
    /// pivots about ITS OWN origin/centre, captured per-element in `start`/`geo_start`.
    pub individual: bool,
    /// Grid/increment snap settings (#4) captured at op start — `apply_transform`
    /// quantizes the committed amount when `snap.on` (XOR the live Ctrl invert).
    pub snap: SnapSettings,
    /// Transform-orientation (#37): which basis the axis-lock + numeric-default axis
    /// map through. `apply_transform` resolves it to a 3×3 whose COLUMNS are the
    /// X/Y/Z directions — Global = identity (world axes, today's behaviour), Local =
    /// the active element's orientation (its `start` Quat snapshot, stable across the
    /// drag), View = the camera basis (`view_basis`, screen-aligned). Shown in the
    /// modal hint.
    pub orientation: TransformOrientation,
    /// True when this Move op was started by Shift+D (duplicate-then-grab). Two
    /// effects: the mouse always drives the OFFSET (a typed number is the array
    /// CLONE-COUNT, not the move amount), and on confirm the extra clones are
    /// arrayed along the drag vector. The clone itself was already pushed as its
    /// own undo step before the grab began, so cancel just leaves the copies at
    /// the source (Blender's Shift+D→Esc).
    pub from_duplicate: bool,
    /// The freshly-created copies (this op's targets) — the base set the array
    /// duplicates from when a clone-count is typed during a duplicate-grab.
    pub dup_base: Vec<ObjectRef>,
    /// The LIVE array clones (#2..N) shown while a count is typed mid-drag. Held
    /// at the END of their kind's scene Vec (LIFO), regenerated each frame from
    /// `dup_base` + the current drag delta, and dropped on cancel. Empty until a
    /// count > 1 is typed.
    pub dup_extra: Vec<ObjectRef>,
}

impl TransformOp {
    /// The axis actually constraining this op: the grabbed gizmo handle wins,
    /// else the keyboard lock. Read by apply_transform + the constraint-line viz
    /// so behaviour and visuals stay in sync.
    pub fn active_axis(&self) -> Option<Axis> {
        self.gizmo_hovered_axis.or(self.axis)
    }
}

impl TransformOp {
    /// The on-viewport status line shown while the op is in progress. `keys` is the
    /// live structured key hint from the keymap (`shortcuts::modal_hint_keys`) —
    /// passed in (not built here) so the registry stays the one source of truth for
    /// which keys do what (#23). While the user is typing a value the hint switches
    /// to the Blender-style typed readout and the key cluster is suppressed.
    pub fn hint(&self, keys: &str) -> String {
        // A duplicate-grab reads a typed number as the array clone-COUNT (the mouse
        // drives the offset), so its hint shows copies, never metres/degrees.
        if self.from_duplicate {
            let n = if self.num.active { (self.num.value().round() as i64).max(1) } else { 1 };
            return format!("Duplicate · {n} cop{}    drag: offset · type: count · Enter confirm · Esc cancel", if n == 1 { "y" } else { "ies" });
        }
        if self.num.active {
            // Blender-style typed readout: "Move X: 4.0 m" / "Rotate Z: -45°" /
            // "Scale: 2.0x". Axis label is blank when unconstrained.
            let axl = self
                .active_axis()
                .map(|a| format!(" {}", a.label()))
                .unwrap_or_default();
            let (val, unit) = match self.kind {
                TransformKind::Move => (self.num.display(), " m"),
                TransformKind::Rotate => (self.num.display(), "°"),
                TransformKind::Scale => (self.num.display(), "x"),
            };
            let orient = match self.orientation {
                TransformOrientation::Global => String::new(),
                o => format!(" ({})", o.label()),
            };
            return format!("{}{}{}: {}{}", self.kind.label(), axl, orient, val, unit);
        }
        let ax = match self.active_axis() {
            Some(a) => format!(" · axis {}", a.label()),
            None => String::new(),
        };
        // The orientation tag is suppressed for Global (the default world axes) so the
        // common case stays uncluttered; Local/View are surfaced.
        let orient = match self.orientation {
            TransformOrientation::Global => String::new(),
            o => format!(" · {}", o.label()),
        };
        // "Move · axis X · Local    X/Y/Z lock · type number · Enter confirm · Esc cancel"
        format!("{}{}{}    {keys}", self.kind.label(), ax, orient)
    }
}

impl Tab {
    /// Every editor type — the SpaceType registry the header's editor-type
    /// switcher offers (`docs/RESEARCH-blender-framework.md` §2.2).
    pub(crate) const ALL: [Tab; 9] = [
        Tab::Viewport,
        Tab::Scene,
        Tab::Library,
        Tab::Inspector,
        Tab::DmxMonitor,
        Tab::Connectivity,
        Tab::Patch,
        Tab::Cues,
        Tab::Render,
    ];

    /// Panels shown in the Window menu (Viewport is fixed, so excluded there).
    const TOGGLEABLE: [Tab; 8] = [
        Tab::Scene,
        Tab::Library,
        Tab::Inspector,
        Tab::DmxMonitor,
        Tab::Patch,
        Tab::Cues,
        Tab::Connectivity,
        Tab::Render,
    ];

    fn title(self) -> &'static str {
        match self {
            Tab::Viewport => "Viewport",
            Tab::Scene => "Scene",
            Tab::Library => "Library",
            Tab::Inspector => "Inspector",
            Tab::DmxMonitor => "DMX",
            Tab::Connectivity => "Connectivity",
            Tab::Patch => "Devices",
            Tab::Cues => "Cues",
            Tab::Render => "Render",
        }
    }

    /// Leading icon glyph for the dock tab.
    fn icon(self) -> &'static str {
        use theme::icon;
        match self {
            Tab::Viewport => icon::VIEWPORT,
            Tab::Scene => icon::SCENE,
            Tab::Library => icon::LIBRARY,
            Tab::Inspector => icon::INSPECTOR,
            Tab::DmxMonitor => icon::DMX,
            Tab::Connectivity => icon::CONNECT,
            Tab::Patch => icon::FIXTURE,
            Tab::Cues => icon::PROFILE,
            Tab::Render => icon::RENDER,
        }
    }
}

/// Owns the dock layout and the cross-panel UI state (the content library,
/// the current selection, and the pixel size the viewport panel wants its 3D
/// target rendered at).
pub struct Ui {
    dock: DockState<Tab>,
    library: Library,
    /// egui textures for GDTF images, keyed by fixture-type (Arc pointer).
    gdtf_textures: HashMap<usize, GdtfTextures>,
    pub selection: Selection,
    pub settings: RenderSettings,
    pub prefs: Preferences,
    pub requested_viewport_px: (u32, u32),
    /// Cross-cut render state: the UI↔app mailbox + the finished still image
    /// (driven by the app's `RenderJob`). The Render dock tab + the World ▸
    /// Render Properties inspector read/write it.
    pub render: RenderUiState,
    /// Set when the user asks to toggle OS window fullscreen (F11 / the viewport
    /// header button / View menu). The app loop consumes it after the egui pass
    /// (only it can reach the winit window).
    pub pending_fullscreen_toggle: bool,
    /// True while a still render is converging (published by the app each frame).
    /// The scene must stay frozen for the render's temporal accumulation, so the
    /// inspector is made read-only and the deferred scene mutations (delete / patch
    /// / duplicate / outliner edits) are held until the render finishes.
    pub render_active: bool,
    /// Whether the 3D viewport currently has interaction focus (drives the focus
    /// border and whether the `d` shortcut opens the Duplicate dialog).
    viewport_focused: bool,
    /// The open Duplicate dialog, if any.
    duplicate: Option<DuplicateDialog>,
    /// The open Replace-fixtures dialog (Shift+R), if any.
    replace: Option<ReplaceDialog>,
    /// Set by the viewport context menu's "Replace…" to open the dialog after the
    /// dock (where the library + patch are reachable).
    pending_replace: bool,
    /// Open floating windows.
    show_prefs: bool,
    show_about: bool,
    show_shortcuts: bool,
    show_perf: bool,
    profile: Option<ProfileEditor>,
    /// Library panel state (search / sort / multi-select).
    lib: library::LibState,
    /// Anchor index for shift-range selection of scene fixtures (list + 3D).
    scene_anchor: Option<usize>,
    /// Sort order for the Scene panel's Fixtures folder.
    scene_sort: outliner::SceneSort,
    /// Scene-outliner name filter (fixtures + objects).
    scene_search: String,
    /// Scene-outliner type/state filter chips (catalog #62) — composes with the
    /// name filter to narrow the tree by entity kind + fixture state.
    scene_filter: tree::OutlinerFilter,
    /// Expanded outliner-tree nodes (absent = collapsed). Group/Root keys are
    /// constant; entity keys are stable EntityIds, so expand-state survives
    /// reorder. Held across frames on `Ui` (the tree is rebuilt each frame).
    scene_expanded: std::collections::HashSet<tree::NodeKey>,
    /// In-flight inline rename: the target node key + its edit buffer (one live).
    scene_rename: Option<(tree::NodeKey, String)>,
    /// Deferred outliner action (hide/rename) returned by the tree mid-dock and
    /// applied after the dock where the undo stack is reachable.
    pending_tree: tree::TreeAction,
    /// Discovered live video sources (NDI + CITP) for the LED-screen content
    /// pickers; the app refreshes this each frame.
    pub screen_sources: panels::ScreenSources,
    /// Fixture-manager (Devices tab) state: filter / sort / bulk values.
    fm: panels::FmState,
    /// The `s` quick-select palette is open.
    quick_select: bool,
    /// The Shift+A viewport Add menu (cursor-anchored entity picker).
    add_menu: windows::AddMenuState,
    /// The F3 operator-search palette (run any registered op by name).
    op_search: windows::OperatorSearchState,
    /// The `P` Patch dialog (assign sequential addresses to the selection).
    patch_dialog: windows::PatchDialog,
    /// The `U` Unpatch confirm dialog (disable the selection's patch entries).
    unpatch_dialog: windows::UnpatchDialog,
    /// In-progress modal transform (G/R/S), if any.
    transform: Option<TransformOp>,
    /// The document snapshot taken when a modal transform STARTED — its `before`
    /// end. Pushed (with the post-transform `after`) when the transform confirms;
    /// dropped without pushing when it cancels (Esc restores in-place). `None`
    /// whenever no transform is live.
    transform_before: Option<op::DocSnapshot>,
    /// Per-frame signals the viewport sets (read after the dock): a transform just
    /// began / just confirmed this frame. Transient — not serialized.
    transform_started: bool,
    transform_finished: bool,
    /// In-flight inspector slider/DragValue drag transaction (#13): captured at
    /// drag-start, pushed as ONE undo step on release. `None` between gestures.
    inspector_tx: Option<op::DragTx>,
    /// Per-frame drag edges the inspector reports (read after the dock) to drive
    /// the `inspector_tx` begin/finalize. Transient — not serialized.
    inspector_edit: panels::InspectorEdit,
    /// Persistent Inspector UI state (property filter + per-category collapse).
    /// Loaded from the config dir on launch; categories self-save on toggle.
    inspector_state: inspector::InspectorState,
    /// Timestamp of the last arrow-key nudge — a fresh nudge within
    /// [`NUDGE_COALESCE`] of it extends the top undo step instead of pushing a new
    /// one, so holding an arrow key collapses into a single undo. `None` = no
    /// nudge burst in progress.
    last_nudge: Option<std::time::Instant>,
    /// Arrow-key nudge accumulated this frame in `handle_shortcuts`; applied in
    /// `show()` where the patch is reachable (so it rides the undo stack).
    pending_nudge: Vec3,
    /// Saved fixture selection groups (persisted with the show; remapped on delete).
    groups: Vec<SelectionGroup>,
    /// The cue list + crossfade engine.
    cues: cues::CueEngine,
    /// Full-document undo / redo history (snapshots; not serialized into .glow).
    undo: op::UndoStack,
    /// The online GDTF Share fixture library + its window toggle.
    share: crate::share::Share,
    show_share: bool,
    /// A delete was requested (from a shortcut / menu / context menu) — committed
    /// once after the dock, where the patch is reachable, so the patch / groups /
    /// cues all get remapped in lock-step with the fixture removal.
    pending_delete: bool,
    /// Enter was pressed in the viewport with a Library item highlighted — add that
    /// item after the dock (where the library + scene are reachable). Mirrors Enter
    /// in the Library pane.
    pending_lib_add: bool,
    /// `B` (box-select) was pressed — set by `dispatch_action`, consumed by the
    /// viewport, which then latches into marquee mode for the next drag. A flag (not a
    /// viewport-local key poll) so it rides the single keymap→dispatch path.
    box_select_armed: bool,
    /// Path of the currently-open `.glow` project (Save vs Save As). `None` =
    /// untitled / never saved.
    current_path: Option<PathBuf>,
    /// The undo [`state_id`](op::UndoStack::state_id) at the last save / open / new —
    /// the "clean" anchor. The document is dirty (window-title `*`) when the live
    /// state id differs from this.
    saved_state_id: u64,
    /// The welcome / recover splash is open (shown at startup, on New, and from
    /// Window ▸ Welcome / the operator search).
    show_splash: bool,
    /// The welcome splash's hero image, decoded + uploaded once (lazily) and cached.
    welcome_tex: Option<egui::TextureHandle>,
    /// Recent project paths (most-recent first) for the File menu + splash.
    recent: Vec<PathBuf>,
    /// Seconds since the last successful autosave (driven from `app`); the splash
    /// uses the autosave file's presence to offer crash recovery.
    autosave_timer: f32,
    /// Viewport N-panel / T-panel open state (Blender's RGN_TYPE_UI / RGN_TYPE_TOOLS;
    /// blueprint §2.2). Toggled by the N / T keys + the viewport-header buttons.
    viewport_regions: ViewportRegions,
    /// The viewport's active tool (§2.4) — decides which gizmo draws (Move shows the
    /// screen-space xform gizmo; Select is plain click-select). Set from the T-panel
    /// tool rail. Default [`ActiveTool::Select`].
    active_tool: ActiveTool,
    /// Transform-tool options (§2.4 #4/#5): grid/increment snap + pivot-point mode.
    /// Read by the gizmo + modal G/R/S blocks when building a [`TransformOp`];
    /// written by the viewport header + N-panel. Transient (not save-persisted).
    xform: TransformPrefs,
    /// The world 3D-cursor point — the [`PivotMode::Cursor3d`] pivot (§2.4 #5).
    /// Starts at the origin; moved by Shift-right-click in the viewport (S1-3d-cursor)
    /// or the snap/reset commands. Transient (not persisted); the viewport draws it.
    cursor_3d: Vec3,
    /// Whether the 3D cursor has been positioned this session (via Shift-RMB / "Snap
    /// to selection"). When `true` the Add menu drops new objects AT the cursor instead
    /// of the view-centre placement default; "Reset cursor to origin" clears it.
    cursor_3d_set: bool,
    /// Persistent content-library prefs (§3 #20): the Recent + Favourites lists
    /// shared by the Library browser and the Add menu. Loaded once at startup,
    /// saved (synchronously, tiny payload) on each add/star toggle.
    lib_prefs: lib_prefs::LibraryPrefs,
    /// Numbered view/camera bookmarks (P1 #34): saved camera poses recalled with an
    /// eased jump. Persisted to `bookmarks.json` in the config dir (loaded once at
    /// startup, saved on each save/delete). Reachable from the Window menu strip +
    /// the registered `view.bookmark_*` commands (F3 palette / keymap).
    bookmarks: bookmarks::Bookmarks,
    /// User-savable workspaces (S1): the soft "modes" — saved records of
    /// {dock layout, default tool, overlay flags}. The four built-ins
    /// (Design/Patch/Focus/Visualise) regenerate when `workspaces.json` is absent.
    /// Switched from the tab strip / Window menu / `workspace.activate_*` commands;
    /// activating applies the layout + presets the tool + emphasises overlays — it
    /// does NOT lock anything (everything stays editable).
    workspaces: workspaces::Workspaces,
    /// The open "Save current as workspace…" dialog name buffer, if any. `None` =
    /// closed.
    save_workspace: Option<String>,
    /// Runtime keymap overrides (S1): a rebind/disable/add layer over the static
    /// [`shortcuts::KEYMAPS`] defaults, persisted to `keymap.json` in the config
    /// dir. EMPTY by default ⇒ the app dispatches exactly the shipped binds. Loaded
    /// once at startup; published to the process-wide active snapshot each frame so
    /// the fixed-signature viewport poll sites resolve against the same set.
    keymap_overrides: shortcuts::KeymapOverrides,
    /// Transient UI state for the Preferences › Keymap editor (S2): the in-flight
    /// "press a key to rebind" capture target + the command search filter. Held by
    /// the `Ui` so it persists across frames while the editor waits for a chord.
    keymap_editor: windows::KeymapEditorState,
    /// The Measure tool's two-point ruler (§2.4) — a read-only viewport measurement
    /// that persists across frames; cleared when the Measure tool isn't active.
    measure: panels::MeasureState,
    /// The Aim tool's in-flight drag (§2.4) — `active()` while a head-aim drag is
    /// underway, so the pending undo snapshot is kept alive across the drag frames.
    aim: panels::AimState,
    /// The `~` radial View pie (cursor-anchored axis-view / projection / frame
    /// picker). Opened by the ViewPie action at the pointer; closed on pick/cancel.
    view_pie: pie::PieState,
    /// The `Z` radial Shading pie (cursor-anchored display-mode picker + Grid /
    /// Stats toggles, Blender's Z pie). Opened by the ShadingPie action at the
    /// pointer; closed on pick / cancel.
    shading_pie: pie::PieState,
    /// Transient toasts + the persistent report log (§2.10). Every user-facing
    /// save / open / import / DMX-connect / undo moment reports here so it surfaces
    /// as a fading toast instead of a silent `log::*`. Ticked + drawn in `show()`.
    notify: notify::Notifier,
    /// Handle-based status-bar message stack (#21): a transient slot pushed/popped
    /// by handle, layered over the selection/units/fps content. Last push wins.
    status_msgs: notify::StatusStack,
    /// The report-log window (§2.10 #22): a persistent newest-first view of the
    /// `notify` history. Opened from the status-bar message area, the Window menu,
    /// or the `window.report_log` command (so it surfaces in the F3 palette).
    show_report_log: bool,
    /// Was the DMX I/O running last frame? Used to detect the connect/disconnect
    /// edge in `show()` (the actual start/stop happens on the DMX worker) and emit
    /// one toast per transition rather than every frame.
    dmx_was_running: bool,
}

/// Open state for the viewport's side regions — the N-panel (Item/Transform
/// sidebar, reuses `inspector::inspector` verbatim) and the T-panel (tool-rail shell,
/// Phase 3 fills it with the ActiveTool buttons). Blueprint §2.2 / RGN_TYPE_UI +
/// RGN_TYPE_TOOLS. Default both off; the lead's note (auto-on in Focus/Visualise
/// contexts) is a later phase.
#[derive(Clone, Copy, Default)]
struct ViewportRegions {
    t_open: bool,
}

impl Ui {
    /// Advance any in-progress cue crossfade. Called once per real frame from
    /// `app::render`, after live DMX decode and before motion advance.
    pub fn tick_cues(&mut self, scene: &mut Scene, dt: f32) {
        self.cues.tick(scene, dt);
    }

    /// True when the document has unsaved edits (the live undo state differs from the
    /// last save / open / new anchor).
    pub fn is_dirty(&self) -> bool {
        self.undo.state_id() != self.saved_state_id
    }

    /// The window title: `glowstone - <name>`, with a `*` marking unsaved changes and
    /// `untitled` for a never-saved document. E.g. `glowstone - show.glow` (clean),
    /// `glowstone -*show.glow` (unsaved), `glowstone - untitled` (new).
    pub fn window_title(&self) -> String {
        let name = self
            .current_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string());
        let sep = if self.is_dirty() { "*" } else { " " };
        format!("glowstone -{sep}{name}")
    }

    /// Raise a success toast from outside the Ui (e.g. the app loop reporting a
    /// finished render). Mirrors the in-Ui `notify` calls.
    pub fn notify_success(&mut self, msg: impl Into<String>) {
        self.notify.success(msg);
    }

    /// Raise an error toast from outside the Ui.
    pub fn notify_error(&mut self, msg: impl Into<String>) {
        self.notify.error(msg);
    }

    /// Build the whole docked UI for one egui frame.
    ///
    /// `DockArea::show` wraps the dock in a full-window `CentralPanel` and
    /// self-applies `Style::from_egui`, so it lays out correctly without us
    /// managing a panel or style. It is deprecated only in favor of eframe's
    /// `App::ui`; for a non-eframe app this ctx-based call is the right one.
    #[allow(deprecated)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
        viewport_texture: egui::TextureId,
        fps: f32,
        timings: &crate::renderer::PassTimings,
    ) {
        // Theme/accent/DPI live every frame (cheap; egui dedups).
        self.prefs.apply_theme(ctx);
        // Bug 8 — Enter-leak guard: snapshot whether a text widget holds keyboard
        // focus at the START of the frame (before any widget runs). When the user
        // commits a value box with Enter, the field is still focused HERE but
        // surrenders focus mid-frame, leaving the Enter in the event queue; unguarded
        // global readers (batch-add, dialog confirms) would otherwise see it as a
        // fresh keypress. They gate on `text_focus_active()` so the commit can't leak.
        let text_focus = ctx.memory(|m| m.focused().is_some());
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("glowstone.text_focus"), text_focus));
        // Age + retire expired toasts (dt from egui — `show()` takes no dt param).
        self.notify.tick(ctx.input(|i| i.stable_dt));
        // DMX connect/disconnect edge: the actual bind happens on the DMX worker,
        // so we detect the running-state transition here and toast it ONCE per edge
        // (not every frame). `is_running()` reflects the worker's current state.
        let dmx_running = dmx.is_running();
        if dmx_running != self.dmx_was_running {
            if dmx_running {
                self.notify.success("DMX input connected");
            } else {
                self.notify.info("DMX input stopped");
            }
            self.dmx_was_running = dmx_running;
        }
        // Publish the active keymap overrides for this frame so the fixed-signature
        // viewport poll sites (`panels::viewport`, whose app.rs caller is off-limits)
        // resolve against the SAME overrides this Ui polls with. EMPTY by default ⇒
        // those sites see the static defaults, unchanged.
        shortcuts::publish_active(&self.keymap_overrides);
        // Global shortcuts — ONE poll, ONE dispatch path (S1). `handle_shortcuts`
        // routes every non-modal Action through `dispatch_action` (the single source
        // of truth), which handles File/Patch/Unpatch/Undo/Redo/OperatorSearch/
        // AdjustLast directly here (scene + camera + dmx reachable) and defers the
        // Delete / Patch-commit / nudge writes through `self` flags as before.
        self.handle_shortcuts(ctx, scene, camera, dmx);
        // Apply any arrow-key nudge collected above (here the patch is reachable, so
        // it rides the undo stack — a held-key burst coalesces into one step).
        self.apply_nudge(scene, dmx);
        self.handle_dropped_files(ctx, scene, camera);

        // Shift+D — grab-duplicate (Blender). Clone the selection NOW as its own
        // undo step (so cancelling the grab leaves the copies at the source, like
        // Blender's Shift+D→Esc), re-select the copies, then flag the viewport to
        // start a Move grab on them THIS frame. Done before the dock so the clone
        // lands before `panels::viewport` reads the flag and snapshots the move's
        // `before` end. Dispatch_action reports DuplicateGrab unhandled, so this is
        // its only handler.
        if self.viewport_focused
            && self.transform.is_none()
            && self.selection.has_object()
            && shortcuts::poll(
                ctx,
                shortcuts::ActiveContext { viewport_focused: true, transform_active: false, box_select_active: false },
                &shortcuts::active(),
            )
            .iter()
            .any(|a| matches!(a, shortcuts::Action::DuplicateGrab))
        {
            let refs = self.selection.object_refs();
            self.run_op(
                "object.duplicate_grab",
                "Duplicate",
                op::OpFlags::UNDO | op::OpFlags::REGISTER,
                scene,
                dmx,
                true,
                |cx| {
                    let copies = cx.scene.duplicate_objects(&refs);
                    if copies.is_empty() {
                        return op::OpStatus::Cancelled;
                    }
                    *cx.selection = Selection::from_object_refs(&copies);
                    op::OpStatus::Finished
                },
            );
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("glowstone.dupgrab.start"), true));
        }

        // Chrome MUST be reserved before the dock (it fills the CentralPanel).
        self.menu_bar(ctx, scene, camera, dmx);
        // The workspace tab strip (Blender's workspace tabs) — switch the soft "mode"
        // with one click. Reserved below the menu bar, above the dock.
        self.workspace_strip(ctx);
        self.status_bar(ctx, scene, dmx, fps);

        // Reset the per-frame inspector drag edges before the dock OR-accumulates
        // this frame's signals into them (#13).
        self.inspector_edit = panels::InspectorEdit::default();

        // One call hands back all the disjoint DMX borrows the panels need.
        let dmxv = dmx.view();
        let mut viewer = PanelViewer {
            scene,
            camera,
            library: &mut self.library,
            gdtf_textures: &mut self.gdtf_textures,
            selection: &mut self.selection,
            settings: &mut self.settings,
            prefs: &self.prefs,
            viewport_texture,
            requested_viewport_px: &mut self.requested_viewport_px,
            viewport_focused: &mut self.viewport_focused,
            box_select_armed: &mut self.box_select_armed,
            duplicate: &mut self.duplicate,
            profile: &mut self.profile,
            lib: &mut self.lib,
            scene_anchor: &mut self.scene_anchor,
            scene_sort: &mut self.scene_sort,
            scene_search: &mut self.scene_search,
            scene_filter: &mut self.scene_filter,
            scene_expanded: &mut self.scene_expanded,
            scene_rename: &mut self.scene_rename,
            pending_tree: &mut self.pending_tree,
            fm: &mut self.fm,
            transform: &mut self.transform,
            transform_started: &mut self.transform_started,
            transform_finished: &mut self.transform_finished,
            inspector_edit: &mut self.inspector_edit,
            inspector_state: &mut self.inspector_state,
            cues: &mut self.cues,
            delete_requested: &mut self.pending_delete,
            replace_requested: &mut self.pending_replace,
            add_requested: &mut self.pending_lib_add,
            open_share: &mut self.show_share,
            dmx_patch: dmxv.patch,
            dmx_snapshot: dmxv.snapshot,
            dmx_status: dmxv.status,
            dmx_config: dmxv.config,
            dmx_selected_universe: dmxv.selected_universe,
            dmx_bind_ip_text: dmxv.bind_ip_text,
            dmx_universes_text: dmxv.universes_text,
            dmx_pending: dmxv.pending,
            dmx_running: dmxv.running,
            fps,
            screen_sources: &self.screen_sources,
            viewport_regions: &mut self.viewport_regions,
            active_tool: &mut self.active_tool,
            xform: &mut self.xform,
            cursor_3d: &mut self.cursor_3d,
            cursor_3d_set: &mut self.cursor_3d_set,
            lib_prefs: &mut self.lib_prefs,
            measure: &mut self.measure,
            aim: &mut self.aim,
            render: &mut self.render,
            pending_fullscreen: &mut self.pending_fullscreen_toggle,
            render_active: self.render_active,
        };

        DockArea::new(&mut self.dock).show(ctx, &mut viewer);

        // A render just started (from the inspector Render button or the Render
        // tab) — pop the Render tab to the front so the result is front-and-centre.
        // PEEK only: the app loop still consumes `render.request` to start the job.
        if matches!(self.render.request, Some(RenderRequest::Start)) {
            self.ensure_render_tab_focused();
        }
        // F11 toggles OS window fullscreen; F12 renders. Both respect the text-input
        // guard (so typing into an inspector field doesn't fire them), matching the
        // rest of the keymap (`shortcuts::poll`).
        let typing = ctx.egui_wants_keyboard_input();
        if !typing && ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            self.pending_fullscreen_toggle = true;
        }
        if !typing
            && ctx.input(|i| i.key_pressed(egui::Key::F12))
            && self.render.status.phase != RenderPhase::Rendering
        {
            self.render.request_start();
            self.ensure_render_tab_focused();
        }

        // Modal-transform undo (viewer borrows released — scene + patch reachable):
        // the viewport drives the live G/R/S op, so its confirm/cancel is routed
        // through the undo stack HERE. A start snapshots the `before` end; a confirm
        // pushes (before, after); a cancel (the op already restored in-place — the
        // transform field went back to None without a confirm) drops `before`.
        if self.transform_started {
            self.transform_before = Some(self.undo_begin(scene, dmx.patch()));
        }
        if self.transform_finished {
            if let Some(before) = self.transform_before.take() {
                self.undo_push("Transform", before, scene, dmx.patch());
                self.undo.set_last_op("transform.apply", "Transform");
            }
        } else if self.transform.is_none() && !self.aim.active() {
            // Op ended without confirming (cancelled / focus lost) — discard. An Aim
            // drag has no `TransformOp` in flight, so guard on `aim.active()` too or its
            // mid-drag frames would drop the pending snapshot before the release commit.
            self.transform_before = None;
        }

        // Inspector slider/DragValue undo transaction (#13): the inspector edits the
        // scene live each frame (no push) — the same begin→preview→finalize shape as
        // the gizmo. On drag-START snapshot `before` into `inspector_tx`; on RELEASE
        // push ONE step for the whole gesture. A drag both starting AND stopping in
        // one frame (a tiny nudge) still gets begin-then-finalize in order.
        if self.inspector_edit.started {
            let before = self.undo_begin(scene, dmx.patch());
            self.inspector_tx = Some(op::DragTx::begin(before, "inspector.edit", "Edit Value"));
        }
        if self.inspector_edit.stopped
            && let Some(tx) = self.inspector_tx.take()
        {
            let after = self.undo_begin(scene, dmx.patch());
            self.undo.finalize_drag(tx, after);
        }

        // Commit a requested delete now (viewer borrows released): the patch is
        // reachable here, so fixtures + patch + cues + groups are remapped together.
        // Held while a still render converges (a scene edit would reset its
        // accumulation) — the flag persists, so it commits once the render finishes.
        if self.pending_delete && !self.render_active {
            self.commit_delete(scene, dmx);
        }
        // Apply a deferred outliner action (hide toggle / rename) as ONE undo step,
        // now that the undo stack + patch are reachable (the tree itself can't touch
        // undo mid-dock, so it returned the intent — mirrors pending_delete). Also
        // held during a render (the pending action stays queued until it finishes).
        if !self.render_active {
            let tree_action = std::mem::replace(&mut self.pending_tree, tree::TreeAction::None);
            self.apply_tree_action(tree_action, scene, dmx);
        }
        // The viewport context menu's "Replace…" opens the dialog here (after the
        // dock), where the library + patch are reachable.
        if self.pending_replace {
            self.pending_replace = false;
            if self.replace.is_none() && !self.selection.fixtures.is_empty() {
                self.replace = Some(ReplaceDialog::default());
            }
        }
        // Enter in the viewport adds the Library tab's highlighted item — resolved +
        // added here (after the dock, where the library + scene are reachable), so it
        // mirrors pressing Enter in the Library pane even when Library isn't the
        // visible sidebar tab. Placed at the 3D cursor when set, else the camera
        // anchor. Held during a render (a scene edit would reset its accumulation).
        if self.pending_lib_add && !self.render_active {
            self.pending_lib_add = false;
            if let Some(active) = self.lib.active.clone() {
                let cursor = self.cursor_3d_set.then_some(self.cursor_3d);
                if let Some(sel) = library::add_active_library_item(&self.library, scene, camera, &active, cursor) {
                    self.selection = sel;
                }
            }
        }
        replace_window(ctx, &self.library, scene, &mut self.selection, dmx.patch_mut(), &mut self.replace);

        // Patch / Unpatch dialogs (committed here, where the patch is reachable).
        // On confirm, P assigns sequential addresses to the selected devices from
        // the chosen start slot; U disables their patch entries.
        if windows::patch_dialog_window(ctx, &mut self.patch_dialog) {
            let (u, a) = (self.patch_dialog.start_universe, self.patch_dialog.start_address);
            let kind = self.patch_dialog.kind;
            self.run_op(
                "fixture.patch",
                "Patch",
                op::OpFlags::UNDO | op::OpFlags::REGISTER,
                scene,
                dmx,
                true,
                |cx| {
                    ops::patch_selection(cx, kind, u, a);
                    op::OpStatus::Finished
                },
            );
        }
        if windows::unpatch_dialog_window(ctx, &mut self.unpatch_dialog) {
            let kind = self.unpatch_dialog.kind;
            self.run_op(
                "fixture.unpatch",
                "Unpatch",
                op::OpFlags::UNDO | op::OpFlags::REGISTER,
                scene,
                dmx,
                true,
                |cx| {
                    ops::unpatch_selection(cx, kind);
                    op::OpStatus::Finished
                },
            );
        }

        // Floating windows (viewer borrows released — scene/selection free again).
        // Duplicate mutates the scene; on confirm, run it through the operator
        // pipeline so the (before, after) undo step is pushed uniformly.
        if let Some(d) = duplicate_window(ctx, &mut self.duplicate) {
            // Remember these values so the NEXT Duplicate reuses them (bug 3).
            set_dup_defaults(ctx, DupDefaults { x: d.x, y: d.y, z: d.z, angle: d.y_angle, count: d.count });
            self.run_op(
                "fixture.duplicate",
                "Duplicate",
                op::OpFlags::UNDO | op::OpFlags::REGISTER,
                scene,
                dmx,
                true,
                |cx| {
                    match cx.scene.duplicate_fixture(
                        d.fixture,
                        Vec3::new(d.x, d.y, d.z),
                        d.y_angle,
                        d.count,
                    ) {
                        Some(first) => {
                            *cx.selection = Selection::fixture(first);
                            op::OpStatus::Finished
                        }
                        None => op::OpStatus::Cancelled,
                    }
                },
            );
        }
        windows::profile_editor_window(
            ctx,
            scene,
            &mut self.selection,
            &mut self.gdtf_textures,
            &mut self.profile,
            &self.prefs,
        );
        windows::preferences_window(
            ctx,
            &mut self.show_prefs,
            &mut self.prefs,
            &mut self.settings,
            dmx.config_mut(),
            &mut self.keymap_overrides,
            &mut self.keymap_editor,
        );
        windows::about_window(ctx, &mut self.show_about);
        windows::shortcuts_window(ctx, &mut self.show_shortcuts);
        // The "Save current as workspace…" modal (S1) — captures the live layout +
        // tool + overlays under a name when confirmed.
        self.save_workspace_dialog(ctx);
        // The report-log history window (§2.10 #22) — the persistent companion to
        // the fading toasts drawn below; lists past notifications newest-first.
        self.notify.draw_log_window(ctx, &mut self.show_report_log);
        windows::perf_overlay_window(ctx, &mut self.show_perf, timings, &mut self.settings);
        windows::quick_select_window(ctx, scene, &mut self.selection, &mut self.quick_select);
        // The F3 operator-search palette (S2: lists + runs the WHOLE registry, not
        // just the 5 catalog ops). Precompute each command's runnable state (the
        // window borrows `self.op_search` mutably, so `op_runnable`'s `&self` read
        // can't be live inside the closure) and dispatch the chosen command after.
        // A catalog op (`invoke.is_some()`) reports its `poll`; a pure-action command
        // (view / nav / selection / file) is always enabled.
        let runnable: Vec<(&'static str, bool)> = shortcuts::palette_commands()
            .iter()
            .map(|c| (c.id, c.invoke.is_none() || self.op_runnable(c.id)))
            .collect();
        let picked = windows::operator_search_window(ctx, &mut self.op_search, |id| {
            runnable.iter().find(|(cid, _)| *cid == id).map(|(_, ok)| *ok).unwrap_or(false)
        });
        if let Some(id) = picked {
            self.run_palette_command(ctx, id, scene, camera, dmx);
        }
        // The Shift+A Add menu: on a pick, drop the entity into the scene + select
        // it (the menu itself stays library-only, decoupled from the mutation). The
        // menu also owns starring (mutates + persists `lib_prefs` directly).
        if let Some(action) =
            windows::add_menu_window(ctx, &self.library, &mut self.add_menu, &mut self.lib_prefs, &mut self.gdtf_textures)
        {
            use windows::AddAction;
            // #19: drop at the viewport cursor/camera anchor, not the origin. When the
            // 3D cursor has been positioned this session (Shift-RMB / Snap to selection)
            // it wins as the placement origin (Blender's "Add at 3D cursor"); otherwise
            // fall back to the view-centre ground/surface anchor.
            let place = if self.cursor_3d_set {
                self.cursor_3d
            } else {
                panels::placement_point(scene, camera)
            };
            // #20: the stable key to record in Recent (resolved against the live
            // library so an index can't drift it to the wrong entry).
            let item = match action {
                AddAction::Fixture(i) => lib_prefs::LibItem::Fixture(i),
                AddAction::Gdtf(i) => lib_prefs::LibItem::Gdtf(i),
                AddAction::Screen(i) => lib_prefs::LibItem::Screen(i),
                AddAction::Pyro(i) => lib_prefs::LibItem::Pyro(i),
                AddAction::Environment(i) => lib_prefs::LibItem::Env(i),
            };
            let key = lib_prefs::entry_key(&self.library, item);
            let status = self.run_op(
                "object.add",
                "Add",
                op::OpFlags::UNDO | op::OpFlags::REGISTER,
                scene,
                dmx,
                true,
                |cx| {
                    let new: Option<Selection> = match action {
                        AddAction::Fixture(i) => cx
                            .library
                            .fixtures
                            .get(i)
                            .cloned()
                            .map(|p| Selection::fixture(cx.scene.add_fixture_at(&p, place))),
                        AddAction::Gdtf(i) => cx
                            .library
                            .gdtf
                            .get(i)
                            .cloned()
                            .map(|g| Selection::fixture(cx.scene.add_gdtf(g, place))),
                        AddAction::Screen(i) => cx
                            .library
                            .screens
                            .get(i)
                            .cloned()
                            .map(|p| Selection::screen(cx.scene.add_screen_at(&p, place))),
                        AddAction::Pyro(i) => cx
                            .library
                            .pyro
                            .get(i)
                            .cloned()
                            .map(|p| Selection::pyro(cx.scene.add_pyro_at(&p, place))),
                        AddAction::Environment(i) => cx
                            .library
                            .environments
                            .get(i)
                            .cloned()
                            .map(|p| Selection::environment(cx.scene.add_environment_at(&p, place))),
                    };
                    match new {
                        Some(sel) => {
                            *cx.selection = sel;
                            op::OpStatus::Finished
                        }
                        None => op::OpStatus::Cancelled,
                    }
                },
            );
            if status == op::OpStatus::Finished
                && let Some(k) = key
            {
                self.lib_prefs.push_recent(&k);
            }
        }
        // Online GDTF-Share catalogue: stocks the project Library ONLY (no longer
        // instantiates into the scene — bug 9). So it no longer borrows scene/selection.
        share_window::fixture_library_window(
            ctx,
            &mut self.show_share,
            &mut self.share,
            &mut self.library,
        );
        // The `~` radial View pie (above the dock, anchored at the cursor). On a
        // pick it applies straight to the camera; cancel / Esc just closes it.
        self.view_pie(ctx, camera, scene);
        // The `Z` radial Shading pie (above the dock, anchored at the cursor). On a
        // pick it applies the display mode / overlay toggle immediately.
        self.shading_pie(ctx);

        // Passive status-bar hint (#21 grey slot): advertise an in-flight modal
        // transform; clear it otherwise so the hint never goes stale. (The bar was
        // already drawn this frame; the slot is read again next frame — a one-frame
        // lag that's imperceptible for a passive hint.)
        match &self.transform {
            Some(op) => self.status_msgs.set_hint(format!("{} in progress", op.kind.label())),
            None => self.status_msgs.clear_hint(),
        }

        // Transient toasts overlay the dock (foreground order), below only the
        // modal splash so a fresh launch isn't cluttered.
        self.notify.draw(ctx);

        // The welcome / recover splash sits above everything (it's the first
        // thing on a fresh launch).
        self.splash_window(ctx, scene, camera, dmx);
    }
}

/// Draws the Duplicate dialog and returns the confirmed [`DuplicateDialog`]
/// parameters on "Duplicate" (so the caller applies the mutation through the
/// operator pipeline / undo); returns `None` while open, on cancel, or on Esc.
/// This fn no longer touches the scene itself — it only manages dialog state.
fn duplicate_window(
    ctx: &egui::Context,
    dialog: &mut Option<DuplicateDialog>,
) -> Option<DuplicateDialog> {
    let mut do_dup = false;
    let mut close = false;

    if let Some(d) = dialog.as_mut() {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        egui::Window::new("Duplicate")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("duplicate-grid")
                    .num_columns(2)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("X offset");
                        ui.add(egui::DragValue::new(&mut d.x).speed(0.05).suffix(" m"));
                        ui.end_row();
                        ui.label("Y offset");
                        ui.add(egui::DragValue::new(&mut d.y).speed(0.05).suffix(" m"));
                        ui.end_row();
                        ui.label("Z offset");
                        ui.add(egui::DragValue::new(&mut d.z).speed(0.05).suffix(" m"));
                        ui.end_row();
                        ui.label("Y angle");
                        ui.add(egui::DragValue::new(&mut d.y_angle).speed(0.5).suffix("°"));
                        ui.end_row();
                        ui.label("Number of duplicates");
                        ui.add(egui::DragValue::new(&mut d.count).speed(1.0).range(1..=500));
                        ui.end_row();
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Duplicate").clicked() {
                        do_dup = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
    }

    let mut confirmed: Option<DuplicateDialog> = None;
    if do_dup {
        confirmed = dialog.clone();
        close = true;
    }
    if close {
        *dialog = None;
    }
    confirmed
}

/// Draw a clean inline status dot + label: a small allocated cell with a
/// painter-drawn filled circle, a breathing gap, then the label in the same
/// colour. The bundled fonts ship no round glyph (●/○ render as tofu), and a
/// naked "•" reads tiny and jammed against the words — this keeps it legible.
/// Used inside `ui.horizontal` rows; mirror the spec in `panels.rs`.
pub(crate) fn status_dot(ui: &mut egui::Ui, color: egui::Color32, label: &str) {
    // Small inline cell just wide enough for the circle; vertically centred.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, ui.spacing().interact_size.y), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
    ui.add_space(5.0); // breathing room before the words
    ui.label(egui::RichText::new(label).size(11.0).color(color));
}

/// The Replace-fixtures dialog (Shift+R): pick a profile from the **project
/// library** (imported GDTFs first, then the built-in profiles) and swap every
/// selected fixture's type for it — in place, keeping each fixture's position,
/// aim, level, and instance name, and re-fitting its patch footprint.
fn replace_window(
    ctx: &egui::Context,
    library: &Library,
    scene: &mut Scene,
    selection: &mut Selection,
    patch: &mut crate::dmx::PatchTable,
    dialog: &mut Option<ReplaceDialog>,
) {
    enum Picked {
        Gdtf(usize),
        Profile(usize),
    }

    let Some(d) = dialog.as_mut() else { return };
    let targets: Vec<usize> =
        selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
    if targets.is_empty() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        *dialog = None;
        return;
    }

    // Candidate GDTF profiles "available in the project": the imported library
    // PLUS the distinct types already placed in the scene (an MVR import puts its
    // fixture types on the fixtures, not in `library.gdtf`), deduped by Arc.
    let mut gdtf_arcs: Vec<Arc<crate::gdtf::GdtfFixture>> = library.gdtf.clone();
    for f in &scene.fixtures {
        if let Some(g) = &f.gdtf {
            let p = Arc::as_ptr(g);
            if !gdtf_arcs.iter().any(|a| Arc::as_ptr(a) == p) {
                gdtf_arcs.push(g.clone());
            }
        }
    }

    let mut picked: Option<Picked> = None;
    let mut close = false;
    let q = d.search.trim().to_lowercase();
    let matches = |s: &str| q.is_empty() || s.to_lowercase().contains(q.as_str());

    egui::Window::new(format!("{}  Replace fixtures", theme::icon::DUPLICATE))
        .collapsible(false)
        .resizable(true)
        .default_size([380.0, 460.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Replace {} fixture{} with…",
                    targets.len(),
                    if targets.len() == 1 { "" } else { "s" }
                ))
                .strong(),
            );
            ui.add(
                egui::TextEdit::singleline(&mut d.search)
                    .hint_text(format!("{}  Filter project library…", theme::icon::SEARCH))
                    .desired_width(f32::INFINITY),
            );
            ui.checkbox(&mut d.mesh_only, "Replace mesh/model only")
                .on_hover_text("Swap just the visual model + beam; keep the name, DMX patch, mode and levels. Off = full replace (re-patches and renames to the new type).");
            ui.separator();
            // Cap the LIST at a sensible height and scroll it — the filter box,
            // checkbox and header above stay pinned so the dialog stays compact
            // instead of growing to full screen height on a long library.
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height(360.0)
                .show(ui, |ui| {
                let mut any = false;
                // Grouped EXACTLY like the Library browser: imported GDTFs by
                // MANUFACTURER, then built-ins by CATEGORY — same `theme::section`
                // headers (the old "GDTF FIXTURES" / "BUILT-IN" scheme read as a
                // different categorisation from the Library, which was jarring).
                let mut gdtf: Vec<usize> = (0..gdtf_arcs.len())
                    .filter(|&gi| matches(&format!("{} {}", gdtf_arcs[gi].manufacturer, gdtf_arcs[gi].name)))
                    .collect();
                gdtf.sort_by(|&a, &b| {
                    let (ga, gb) = (&gdtf_arcs[a], &gdtf_arcs[b]);
                    ga.manufacturer
                        .to_lowercase()
                        .cmp(&gb.manufacturer.to_lowercase())
                        .then(ga.name.to_lowercase().cmp(&gb.name.to_lowercase()))
                });
                let mut last = String::new();
                for gi in gdtf {
                    any = true;
                    let g = &gdtf_arcs[gi];
                    let cat = if g.manufacturer.is_empty() { "Imported".to_string() } else { g.manufacturer.clone() };
                    if cat != last {
                        last = cat.clone();
                        theme::section(ui, &cat.to_uppercase());
                    }
                    let src = g.source;
                    ui.horizontal(|ui| {
                        if ui.selectable_label(false, format!("{}  {}", theme::icon::FIXTURE, g.name)).clicked() {
                            picked = Some(Picked::Gdtf(gi));
                        }
                        inspector::source_chip(ui, src);
                    });
                }
                // Then the built-in profiles, grouped by category (Generic / Laser / …).
                let mut prof: Vec<usize> = (0..library.fixtures.len())
                    .filter(|&pi| matches(&format!("{} {}", library.fixtures[pi].category, library.fixtures[pi].name)))
                    .collect();
                prof.sort_by(|&a, &b| {
                    let (pa, pb) = (&library.fixtures[a], &library.fixtures[b]);
                    pa.category
                        .to_lowercase()
                        .cmp(&pb.category.to_lowercase())
                        .then(pa.name.to_lowercase().cmp(&pb.name.to_lowercase()))
                });
                let mut last_c = String::new();
                for pi in prof {
                    any = true;
                    let p = &library.fixtures[pi];
                    if p.category != last_c {
                        last_c = p.category.to_string();
                        theme::section(ui, &p.category.to_uppercase());
                    }
                    let icon = if p.laser { theme::icon::COLOR } else { theme::icon::FIXTURE };
                    if ui.selectable_label(false, format!("{icon}  {}", p.name)).clicked() {
                        picked = Some(Picked::Profile(pi));
                    }
                }
                if !any {
                    ui.label(egui::RichText::new("no match").weak().small());
                }
            });
            ui.separator();
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });

    if let Some(p) = picked {
        if d.mesh_only {
            // "Mesh/model only": borrow JUST the picked GDTF's 3D MODEL and leave
            // EVERYTHING else on each target untouched — its profile, channels, DMX
            // patch + address, mode, name and optics all stay. For a fixture that has
            // all its data but no real 3D mesh, this finally gives it a model. A
            // built-in pick has no rich GDTF model to lend, so it clears the override.
            let model: Option<Arc<crate::gdtf::GdtfFixture>> = match &p {
                Picked::Gdtf(gi) => Some(gdtf_arcs[*gi].clone()),
                Picked::Profile(_) => None,
            };
            for &i in &targets {
                scene.fixtures[i].model_src = model.clone();
            }
        } else {
            // Full replace RENAMES each fixture to the new type (bug 12 — it should
            // read as its new type) and re-patches; only placement, aim + level carry
            // across. The rebuilt fixture's `model_src` defaults None, dropping any
            // previously-borrowed model.
            let new_base = match &p {
                Picked::Gdtf(gi) => gdtf_arcs[*gi].name.to_string(),
                Picked::Profile(pi) => library.fixtures[*pi].name.to_string(),
            };
            let many = targets.len() > 1;
            for (j, &i) in targets.iter().enumerate() {
                let (pos, orient, pan, tilt, dimmer) = {
                    let f = &scene.fixtures[i];
                    (f.position, f.orientation, f.pan, f.tilt, f.optics.dimmer)
                };
                let name = if many { format!("{new_base} {}", j + 1) } else { new_base.clone() };
                let mut nf = match &p {
                    Picked::Gdtf(gi) => crate::scene::Fixture::from_gdtf(gdtf_arcs[*gi].clone(), name, pos),
                    Picked::Profile(pi) => crate::scene::Fixture::from_profile(&library.fixtures[*pi], name, pos),
                };
                nf.orientation = orient;
                nf.pan = pan;
                nf.tilt = tilt;
                nf.optics.dimmer = dimmer; // keep it lit at the level it had
                nf.snap_movement();
                scene.fixtures[i] = nf;
                patch.replace_at(i, &scene.fixtures[i]);
            }
        }
        close = true;
    }
    if close {
        *dialog = None;
    }
}

/// Transient per-frame borrow bundle handed to egui_dock.
struct PanelViewer<'a> {
    scene: &'a mut Scene,
    camera: &'a mut OrbitCamera,
    library: &'a mut Library,
    gdtf_textures: &'a mut HashMap<usize, GdtfTextures>,
    selection: &'a mut Selection,
    settings: &'a mut RenderSettings,
    prefs: &'a Preferences,
    viewport_texture: egui::TextureId,
    requested_viewport_px: &'a mut (u32, u32),
    viewport_focused: &'a mut bool,
    box_select_armed: &'a mut bool,
    duplicate: &'a mut Option<DuplicateDialog>,
    profile: &'a mut Option<ProfileEditor>,
    lib: &'a mut library::LibState,
    scene_anchor: &'a mut Option<usize>,
    scene_sort: &'a mut outliner::SceneSort,
    scene_search: &'a mut String,
    scene_filter: &'a mut tree::OutlinerFilter,
    scene_expanded: &'a mut std::collections::HashSet<tree::NodeKey>,
    scene_rename: &'a mut Option<(tree::NodeKey, String)>,
    pending_tree: &'a mut tree::TreeAction,
    fm: &'a mut panels::FmState,
    transform: &'a mut Option<TransformOp>,
    /// Per-frame signals the viewport sets for the modal-transform undo wiring.
    transform_started: &'a mut bool,
    transform_finished: &'a mut bool,
    /// Per-frame drag edges the inspector reports for the slider-drag undo
    /// transaction (#13). Read after the dock to begin/finalize `inspector_tx`.
    inspector_edit: &'a mut panels::InspectorEdit,
    /// Persistent Inspector filter + collapse state (the single docked Inspector tab).
    inspector_state: &'a mut inspector::InspectorState,
    cues: &'a mut cues::CueEngine,
    delete_requested: &'a mut bool,
    replace_requested: &'a mut bool,
    add_requested: &'a mut bool,
    open_share: &'a mut bool,
    // Live DMX borrows (from `DmxIo::view`).
    dmx_patch: &'a mut crate::dmx::PatchTable,
    dmx_snapshot: &'a crate::dmx::UniverseSnapshot,
    dmx_status: &'a crate::dmx::DmxStatus,
    dmx_config: &'a mut crate::dmx::DmxConfig,
    dmx_selected_universe: &'a mut u16,
    dmx_bind_ip_text: &'a mut String,
    dmx_universes_text: &'a mut String,
    dmx_pending: &'a mut crate::dmx::PendingNetCmd,
    dmx_running: bool,
    fps: f32,
    /// Discovered live video sources for the LED-screen content pickers.
    screen_sources: &'a panels::ScreenSources,
    /// Viewport N-panel / T-panel open state (§2.2) — read to decide which side
    /// regions to carve, and flipped by the header's N/T toggle buttons.
    viewport_regions: &'a mut ViewportRegions,
    /// The viewport's active tool (§2.4) — the T-panel rail sets it; `viewport()`
    /// reads `shows_xform_gizmo()` to gate the screen-space move gizmo.
    active_tool: &'a mut ActiveTool,
    /// Transform-tool options (§2.4 #4/#5): snap + pivot mode. The header writes
    /// them; `viewport()` reads them when building a [`TransformOp`].
    xform: &'a mut TransformPrefs,
    /// The world 3D-cursor point ([`PivotMode::Cursor3d`] pivot, §2.4 #5). Moved by
    /// Shift-right-click in `viewport()` (S1-3d-cursor).
    cursor_3d: &'a mut Vec3,
    /// Whether the 3D cursor has been set this session — `viewport()` flips it true
    /// on a Shift-RMB place, so the Add menu can place AT the cursor.
    cursor_3d_set: &'a mut bool,
    /// Persistent content-library prefs (§3 #20): Recent + Favourites for the
    /// Library browser. `library_browser()` reads them to render the pinned
    /// sections and writes (front-insert on add, star toggle) through them.
    lib_prefs: &'a mut lib_prefs::LibraryPrefs,
    /// The Measure tool's persistent two-point ruler (§2.4); `viewport()` reads/writes
    /// it when the Measure tool is active and clears it otherwise.
    measure: &'a mut panels::MeasureState,
    /// The Aim tool's in-flight drag (§2.4); `viewport()` sets it while aiming heads.
    aim: &'a mut panels::AimState,
    /// Cross-cut render mailbox (Render tab + Render Properties read/write it).
    render: &'a mut RenderUiState,
    /// Set true by the viewport header's fullscreen button (consumed by the app).
    pending_fullscreen: &'a mut bool,
    /// True while a still render is converging — the inspector goes read-only so a
    /// stray edit can't reset the render's accumulation.
    render_active: bool,
}

/// Draw the viewport T-panel tool rail (§2.4): a vertical radio column of icon
/// toggle-buttons over [`ActiveTool::TOOLBAR`], highlighting the active tool;
/// clicking sets `*active`. Square-ish buttons in a dense, centred column to match
/// the console chrome. A faint separator splits the transform trio from the
/// (still-stubbed) lighting tools so the rail reads in groups.
fn tool_rail(ui: &mut egui::Ui, active: &mut ActiveTool) {
    ui.add_space(4.0);
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        for tool in ActiveTool::TOOLBAR {
            // A thin group separator before the lighting tools (Aim onward).
            if tool == ActiveTool::Aim {
                ui.add_space(2.0);
                ui.separator();
                ui.add_space(2.0);
            }
            let selected = *active == tool;
            let resp = ui
                .add_sized(
                    [30.0, 28.0],
                    egui::SelectableLabel::new(
                        selected,
                        egui::RichText::new(tool.icon()).size(16.0),
                    ),
                )
                .on_hover_text(tool.tooltip());
            if resp.clicked() {
                *active = tool;
            }
        }
    });
}

impl TabViewer for PanelViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        format!("{}  {}", tab.icon(), tab.title()).into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        // Per-editor header bar carved from the leaf's `ui` BEFORE the main content
        // (§2.2): editor-type switcher + right-aligned per-editor controls. May
        // rewrite `*tab` (the type switcher), so re-match `*tab` below.
        editor::header(self, ui, tab);
        match tab {
            Tab::Viewport => {
                // §2.2: carve the N-panel (right sidebar) + T-panel (left tool rail)
                // from the leaf's `ui` BEFORE the main viewport content, so the
                // texture/main region shrinks to the space between them and still
                // orbits/selects/gizmos correctly. Region Ids are salted with the
                // tab title so two Viewport leaves never clash.
                let id_base = ui.id().with(tab.title());
                if self.viewport_regions.t_open {
                    // T-panel tool rail (§2.4): a vertical radio column of icon
                    // toggle-buttons, one per `ActiveTool`, with the active tool
                    // highlighted. Clicking sets `active_tool` — which `viewport()`
                    // reads to decide whether the screen-space xform gizmo draws.
                    egui::SidePanel::left(id_base.with("t-panel"))
                        .resizable(false)
                        .exact_width(40.0)
                        .show_inside(ui, |ui| {
                            tool_rail(ui, self.active_tool);
                        });
                }
                // (The inline N-panel inspector was REMOVED: it duplicated the docked
                // Inspector tab AND, as a layout `SidePanel`, shrank the viewport leaf
                // so opening/resizing it shifted the camera aspect — bug 5. The docked
                // Inspector is now the single inspector; `N` toggles that tab.)
                panels::viewport(
                    ui,
                    self.camera,
                    self.scene,
                    self.selection,
                    self.scene_anchor,
                    self.viewport_focused,
                    self.box_select_armed,
                    self.duplicate,
                    self.viewport_texture,
                    self.requested_viewport_px,
                    self.fps,
                    self.prefs,
                    self.settings.render_scale,
                    self.transform,
                    self.delete_requested,
                    self.replace_requested,
                    self.add_requested,
                    self.transform_started,
                    self.transform_finished,
                    *self.active_tool,
                    *self.xform,
                    self.cursor_3d,
                    self.cursor_3d_set,
                    self.measure,
                    self.aim,
                );
            }
            Tab::Scene => outliner::scene_outliner(
                ui,
                self.scene,
                self.selection,
                self.dmx_patch,
                self.scene_anchor,
                self.scene_sort,
                self.scene_search,
                self.scene_filter,
                self.scene_expanded,
                self.scene_rename,
                self.pending_tree,
            ),
            Tab::Library => library::library_browser(
                ui,
                self.library,
                self.scene,
                self.selection,
                self.camera,
                self.lib,
                self.lib_prefs,
                self.open_share,
                self.gdtf_textures,
            ),
            Tab::Inspector => {
                let mut e = panels::InspectorEdit::default();
                let render_active = self.render_active;
                ui.add_enabled_ui(!render_active, |ui| {
                    inspector::inspector(ui, self.scene, self.selection, self.dmx_patch, self.gdtf_textures, self.profile, self.screen_sources, self.inspector_state, &mut e, self.render, self.settings);
                });
                self.inspector_edit.started |= e.started;
                self.inspector_edit.stopped |= e.stopped;
            }
            Tab::DmxMonitor => panels::dmx_universe_grid(
                ui,
                self.scene,
                self.dmx_patch,
                self.dmx_snapshot,
                self.dmx_selected_universe,
                self.selection,
            ),
            Tab::Connectivity => panels::connectivity(
                ui,
                self.dmx_config,
                self.dmx_status,
                self.dmx_bind_ip_text,
                self.dmx_universes_text,
                self.dmx_pending,
                self.dmx_running,
            ),
            Tab::Patch => panels::fixture_manager(
                ui,
                self.scene,
                self.dmx_patch,
                self.selection,
                self.scene_anchor,
                self.fm,
            ),
            Tab::Cues => cues::cue_panel(ui, self.cues, self.scene),
            Tab::Render => render_panel::render_tab(ui, &self.scene.render, self.render),
        }
    }

    /// Fixed scaffold layout — panels aren't closeable.
    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }

    /// The viewport + render preview draw an opaque image and manage their own
    /// content rect, so they must not be wrapped in a scroll area.
    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        // `[horizontal, vertical]`. The Inspector / outliner / list panels must FIT
        // their width and never scroll sideways (the inspector horizontal-scroll bug);
        // only the genuinely WIDE data tables (the 512-channel DMX grid + the Devices
        // schedule) keep horizontal scroll.
        match tab {
            Tab::Viewport | Tab::Render => [false, false], // draw their own image
            Tab::DmxMonitor | Tab::Patch => [true, true], // wide tables
            _ => [false, true], // fit width, vertical scroll only
        }
    }
}
