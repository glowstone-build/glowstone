//! egui + egui_dock setup: the dock layout and the [`TabViewer`] that routes
//! each dock panel to its drawing function in [`panels`].

mod bookmarks;
mod cues;
mod editor;
mod gizmo;
mod lib_prefs;
pub mod nav_gizmo;
mod notify;
pub(crate) mod op;
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

/// One effect the `Z` Shading pie can apply: switch the viewport display
/// [`ViewportMode`], or flip one of the quick overlay toggles. Returned by
/// [`shading_pie_choices`] and applied by `Ui::apply_shading_choice`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShadingChoice {
    Mode(ViewportMode),
    ToggleGrid,
    ToggleStats,
}

/// The `Z` Shading-pie sectors, clockwise from straight up. The three display
/// modes sit at the cardinal-ish spokes (Beauty up, then Unlit / Wireframe); the
/// Grid + Stats toggles round out the ring. Returned as labelled [`pie::Choice`]
/// values so `pie::choose` maps sector→effect with no parallel-array bookkeeping.
/// A free fn (no `&self`) so the sector→effect layout is unit-testable.
fn shading_pie_choices() -> Vec<pie::Choice<ShadingChoice>> {
    use theme::icon;
    vec![
        pie::Choice::new(icon::VIEWPORT, "Beauty", ShadingChoice::Mode(ViewportMode::Beauty)),
        pie::Choice::new(icon::PATCH, "Grid", ShadingChoice::ToggleGrid),
        pie::Choice::new(icon::FIXTURE, "Unlit", ShadingChoice::Mode(ViewportMode::Unlit)),
        pie::Choice::new(icon::PERF, "Stats", ShadingChoice::ToggleStats),
        pie::Choice::new(icon::GEOMETRY, "Wireframe", ShadingChoice::Mode(ViewportMode::Wireframe)),
    ]
}

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
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new("previz.text_focus")).unwrap_or(false))
}

/// The last-used Duplicate values (or all-zero defaults on first use).
pub(crate) fn dup_defaults(ctx: &egui::Context) -> DupDefaults {
    ctx.data(|d| d.get_temp::<DupDefaults>(egui::Id::new("previz.dup.defaults")).unwrap_or_default())
}

/// Remember the Duplicate values just used, so the next Duplicate reuses them.
pub(crate) fn set_dup_defaults(ctx: &egui::Context, v: DupDefaults) {
    ctx.data_mut(|m| m.insert_temp(egui::Id::new("previz.dup.defaults"), v));
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

/// What the welcome splash's buttons request (applied after the modal closure,
/// where `scene` / `dmx` are mutably reachable again).
enum SplashAction {
    New,
    Open,
    OpenPath(PathBuf),
    Recover(PathBuf),
    Dismiss,
}

/// egui textures decoded from a GDTF fixture's images (thumbnail + wheel slot
/// media), cached per fixture type so they load once.
#[derive(Default)]
pub struct GdtfTextures {
    pub thumbnail: Option<egui::TextureHandle>,
    /// `wheels[wheel_index][slot_index]`.
    pub wheels: Vec<Vec<Option<egui::TextureHandle>>>,
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
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SelectionGroup {
    pub name: String,
    pub fixtures: Vec<usize>,
}

/// Map a fixture index through a removal of `removed` (a sorted, deduped set of
/// deleted indices): `None` if `idx` was itself removed, else `idx` shifted down
/// by the number of removed indices below it.
fn remap_index(idx: usize, removed: &[usize]) -> Option<usize> {
    if removed.binary_search(&idx).is_ok() {
        return None;
    }
    let below = removed.partition_point(|&r| r < idx);
    Some(idx - below)
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
            Tab::Patch => "Fixtures",
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
    lib: panels::LibState,
    /// Anchor index for shift-range selection of scene fixtures (list + 3D).
    scene_anchor: Option<usize>,
    /// Sort order for the Scene panel's Fixtures folder.
    scene_sort: panels::SceneSort,
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
    /// Fixture-manager (Fixtures tab) state: filter / sort / bulk values.
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
    inspector_state: panels::InspectorState,
    /// Timestamp of the last arrow-key nudge — a fresh nudge within
    /// [`NUDGE_COALESCE`] of it extends the top undo step instead of pushing a new
    /// one, so holding an arrow key collapses into a single undo. `None` = no
    /// nudge burst in progress.
    last_nudge: Option<std::time::Instant>,
    /// Arrow-key nudge accumulated this frame in `handle_shortcuts`; applied in
    /// `show()` where the patch is reachable (so it rides the undo stack).
    pending_nudge: Vec3,
    /// Saved fixture selection groups + the new-group name buffer.
    groups: Vec<SelectionGroup>,
    group_name: String,
    /// The cue list + crossfade engine.
    cues: cues::CueEngine,
    /// Full-document undo / redo history (snapshots; not serialized into .archie).
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
    /// Path of the currently-open `.archie` project (Save vs Save As). `None` =
    /// untitled / never saved.
    current_path: Option<PathBuf>,
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
    /// or the snap/reset commands. Transient (no save-format bump); the viewport draws it.
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
/// sidebar, reuses `panels::inspector` verbatim) and the T-panel (tool-rail shell,
/// Phase 3 fills it with the ActiveTool buttons). Blueprint §2.2 / RGN_TYPE_UI +
/// RGN_TYPE_TOOLS. Default both off; the lead's note (auto-on in Focus/Visualise
/// contexts) is a later phase.
#[derive(Clone, Copy, Default)]
struct ViewportRegions {
    t_open: bool,
}

impl Ui {
    pub fn new() -> Self {
        // Load the user's workspaces (built-ins if the file is absent); the active
        // one's saved dock layout is the startup layout and its default tool the
        // startup tool. Overlay flags are deliberately LEFT at the user's saved
        // Preferences defaults here — they're only re-emphasised on an explicit
        // workspace ACTIVATION (a user switch), so startup prefs stay deterministic.
        let workspaces = workspaces::Workspaces::load();
        let active = workspaces.active().clone();
        Self {
            dock: active.dock.clone(),
            library: Library::standard(),
            gdtf_textures: HashMap::new(),
            selection: Selection::fixture(0),
            settings: RenderSettings::default(),
            prefs: Preferences::default(),
            requested_viewport_px: (1, 1),
            render: RenderUiState::default(),
            pending_fullscreen_toggle: false,
            render_active: false,
            viewport_focused: false,
            duplicate: None,
            replace: None,
            pending_replace: false,
            // Debug hook (S2): PREVIZ_UI_PREFS opens the Preferences window at
            // startup so the headless PREVIZ_UI screenshot can capture the keymap
            // editor without app.rs (off-limits) needing a dedicated flag.
            show_prefs: std::env::var_os("PREVIZ_UI_PREFS").is_some(),
            show_about: false,
            show_shortcuts: false,
            show_perf: std::env::var("PREVIZ_PERF").is_ok(),
            profile: None,
            lib: panels::LibState::default(),
            scene_anchor: None,
            scene_sort: panels::SceneSort::Patch,
            scene_search: String::new(),
            scene_filter: tree::OutlinerFilter::default(),
            scene_expanded: {
                // Sensible default expand-state: the project + its top groups open,
                // so fixtures are visible at a glance (and the headless screenshot
                // is deterministic).
                use tree::{GroupKind, NodeKey};
                let mut s = std::collections::HashSet::new();
                s.insert(NodeKey::Root);
                s.insert(NodeKey::World);
                s.insert(NodeKey::EnvGroup);
                s.insert(NodeKey::Group(GroupKind::Fixtures));
                s.insert(NodeKey::Group(GroupKind::Objects));
                s.insert(NodeKey::Group(GroupKind::Screens));
                s
            },
            scene_rename: None,
            pending_tree: tree::TreeAction::None,
            screen_sources: panels::ScreenSources::default(),
            fm: panels::FmState::default(),
            quick_select: false,
            add_menu: windows::AddMenuState::default(),
            op_search: windows::OperatorSearchState::default(),
            patch_dialog: windows::PatchDialog::default(),
            unpatch_dialog: windows::UnpatchDialog::default(),
            transform: None,
            transform_before: None,
            transform_started: false,
            transform_finished: false,
            inspector_tx: None,
            inspector_edit: panels::InspectorEdit::default(),
            inspector_state: panels::InspectorState::load(),
            last_nudge: None,
            pending_nudge: Vec3::ZERO,
            groups: Vec::new(),
            group_name: String::new(),
            cues: cues::CueEngine::default(),
            undo: op::UndoStack::default(),
            pending_delete: false,
            pending_lib_add: false,
            share: crate::share::Share::new(),
            show_share: false,
            current_path: None,
            show_splash: true,
            welcome_tex: None,
            recent: project::load_recent(),
            autosave_timer: 0.0,
            viewport_regions: ViewportRegions::default(),
            active_tool: active.default_tool,
            xform: TransformPrefs::default(),
            cursor_3d: Vec3::ZERO,
            cursor_3d_set: false,
            lib_prefs: lib_prefs::LibraryPrefs::load(),
            bookmarks: bookmarks::Bookmarks::load(),
            workspaces,
            save_workspace: None,
            keymap_overrides: shortcuts::KeymapOverrides::load(),
            keymap_editor: windows::KeymapEditorState::default(),
            measure: panels::MeasureState::default(),
            aim: panels::AimState::default(),
            view_pie: pie::PieState::default(),
            shading_pie: pie::PieState::default(),
            notify: notify::Notifier::default(),
            status_msgs: notify::StatusStack::default(),
            // Debug hook (S3): PREVIZ_UI_LOG opens the report-log window at startup
            // so the headless PREVIZ_UI screenshot can capture it (mirrors the
            // PREVIZ_UI_PREFS prefs-window hook) without touching app.rs.
            show_report_log: std::env::var_os("PREVIZ_UI_LOG").is_some(),
            dmx_was_running: false,
        }
    }

    /// Advance any in-progress cue crossfade. Called once per real frame from
    /// `app::render`, after live DMX decode and before motion advance.
    pub fn tick_cues(&mut self, scene: &mut Scene, dt: f32) {
        self.cues.tick(scene, dt);
    }

    // --- undo / redo ----------------------------------------------------

    /// Capture the whole document as the `before` end of an undo step — call
    /// BEFORE running a mutation, then pair with [`undo_push`](Self::undo_push)
    /// after. Borrows `self` immutably so it can read cues/groups alongside
    /// `scene` + `patch` (the snapshot keeps the parsed-GDTF `Arc`s out of band).
    fn undo_begin(&self, scene: &Scene, patch: &crate::dmx::PatchTable) -> op::DocSnapshot {
        self.undo.begin(scene, patch, &self.cues, &self.groups, &self.selection)
    }

    /// Record a finished edit (`before` from [`undo_begin`](Self::undo_begin),
    /// `after` = the post-mutation document).
    fn undo_push(
        &mut self,
        name: &str,
        before: op::DocSnapshot,
        scene: &Scene,
        patch: &crate::dmx::PatchTable,
    ) {
        let after = self.undo.begin(scene, patch, &self.cues, &self.groups, &self.selection);
        self.undo.push(name, before, after);
    }

    /// Step back one edit, restoring scene + patch + cues + groups + selection.
    /// (Field-disjoint borrows: `self.undo` vs the other `self` fields.)
    fn do_undo(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
        // Toast what we reversed (read the name BEFORE undo moves the cursor).
        let name = self.undo.undo_name().map(str::to_owned);
        self.undo.undo(
            scene,
            dmx.patch_mut(),
            &mut self.cues,
            &mut self.groups,
            &mut self.selection,
        );
        match name {
            Some(n) => self.notify.info(format!("Undo: {n}")),
            None => self.notify.info("Nothing to undo"),
        }
    }

    /// Step forward one edit (the redo direction).
    fn do_redo(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
        let name = self.undo.redo_name().map(str::to_owned);
        self.undo.redo(
            scene,
            dmx.patch_mut(),
            &mut self.cues,
            &mut self.groups,
            &mut self.selection,
        );
        match name {
            Some(n) => self.notify.info(format!("Redo: {n}")),
            None => self.notify.info("Nothing to redo"),
        }
    }

    /// Run a closure-operator (the lightweight path the four migrated mutators
    /// use). Wraps its arguments in an [`op::ClosureOp`] and dispatches through
    /// [`run_operator`](Self::run_operator), so inline and (future) struct
    /// operators share one pipeline. `poll` is pre-computed by the caller.
    fn run_op(
        &mut self,
        id: &'static str,
        label: &'static str,
        flags: op::OpFlags,
        scene: &mut Scene,
        dmx: &mut crate::dmx::DmxIo,
        poll: bool,
        exec: impl FnOnce(&mut op::OpCtx) -> op::OpStatus,
    ) -> op::OpStatus {
        let op = op::ClosureOp { id, label, flags, poll, exec: Some(exec) };
        self.run_operator(op, scene, dmx)
    }

    /// Run any [`op::Operator`] under Blender's "system pushes undo after Finished"
    /// rule (`docs/RESEARCH-blender-framework.md` §2.1): if `poll` is false this is
    /// a no-op; otherwise snapshot BEFORE, run `exec`, and on [`op::OpStatus::Finished`]
    /// push a (before, after) step when the op carries [`op::OpFlags::UNDO`] and
    /// record the last op when it carries [`op::OpFlags::REGISTER`]. A `Cancelled`
    /// / `PassThrough` op pushes nothing. `exec` receives an [`op::OpCtx`] bundling
    /// the four mutable doc parts + selection + the read-only content library, so
    /// every operator edits through the same surface a snapshot captures.
    fn run_operator(
        &mut self,
        mut op: impl op::Operator,
        scene: &mut Scene,
        dmx: &mut crate::dmx::DmxIo,
    ) -> op::OpStatus {
        let flags = op.flags();
        let (id, label) = (op.id(), op.label().to_string());
        // poll() against the doc surface; a false poll is a clean no-op.
        let poll = {
            let cx = op::OpCtx {
                scene,
                patch: dmx.patch_mut(),
                cues: &mut self.cues,
                groups: &mut self.groups,
                selection: &mut self.selection,
                library: &self.library,
            };
            op.poll(&cx)
        };
        if !poll {
            return op::OpStatus::PassThrough;
        }
        // Snapshot the whole document as the step's `before` end first.
        let before = self.undo_begin(scene, dmx.patch());
        // Assemble the field-disjoint mutable doc surface and run the edit.
        let status = {
            let mut cx = op::OpCtx {
                scene,
                patch: dmx.patch_mut(),
                cues: &mut self.cues,
                groups: &mut self.groups,
                selection: &mut self.selection,
                library: &self.library,
            };
            op.exec(&mut cx)
        };
        if status == op::OpStatus::Finished {
            if flags.contains(op::OpFlags::UNDO) {
                self.undo_push(&label, before, scene, dmx.patch());
            }
            if flags.contains(op::OpFlags::REGISTER) {
                self.undo.set_last_op(id, label);
            }
        }
        status
    }

    /// Apply this frame's accumulated arrow-key nudge (set by `handle_shortcuts`)
    /// to the selected fixtures, coalescing a burst into a SINGLE undo step: the
    /// first nudge of a burst pushes a step; subsequent nudges within
    /// [`NUDGE_COALESCE`] amend that step's `after` end so one undo reverts the
    /// whole drag. Called from `show()` where the patch is reachable.
    fn apply_nudge(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
        let nudge = std::mem::replace(&mut self.pending_nudge, Vec3::ZERO);
        if nudge == Vec3::ZERO {
            return;
        }
        let now = std::time::Instant::now();
        // Coalesce when the previous nudge was recent AND the top undo step is
        // still our nudge burst (an intervening edit forfeits the coalesce).
        let coalesce = self
            .last_nudge
            .is_some_and(|t| now.duration_since(t) <= NUDGE_COALESCE)
            && self.undo.top_name() == Some("Nudge");

        // The `before` end: for a fresh burst, snapshot the pre-move document; for
        // a coalescing nudge we keep the existing step's `before` (so only `after`
        // is amended below).
        let before = (!coalesce).then(|| self.undo_begin(scene, dmx.patch()));

        for &fi in &self.selection.fixtures {
            if let Some(f) = scene.fixtures.get_mut(fi) {
                f.position += nudge;
                f.snap_movement();
            }
        }

        if coalesce {
            let after = self.undo.begin(scene, dmx.patch(), &self.cues, &self.groups, &self.selection);
            self.undo.amend_after(after);
        } else if let Some(before) = before {
            self.undo_push("Nudge", before, scene, dmx.patch());
            self.undo.set_last_op("transform.nudge", "Nudge");
        }
        self.last_nudge = Some(now);
    }

    /// Apply a deferred outliner [`tree::TreeAction`] as a SINGLE undo step.
    /// Toggling a GROUP's eye flips every child's `hidden` in one step (Blender's
    /// shift-children behaviour, made the default): if any child is visible →
    /// hide all, else show all. Id→index resolution treats a stale id as a no-op.
    fn apply_tree_action(
        &mut self,
        action: tree::TreeAction,
        scene: &mut Scene,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        use tree::{GroupKind, NodeKey, TreeAction};
        match action {
            TreeAction::None => {}
            TreeAction::ToggleHidden(key) => {
                let before = self.undo_begin(scene, dmx.patch());
                let changed = match key {
                    NodeKey::Entity(id) => {
                        // Resolve across all four entity collections (id is unique).
                        if let Some(i) = scene.fixture_index_of(id) {
                            scene.fixtures[i].hidden = !scene.fixtures[i].hidden;
                            true
                        } else if let Some(i) = scene.geometry_index_of(id) {
                            scene.geometry[i].hidden = !scene.geometry[i].hidden;
                            true
                        } else if let Some(i) = scene.screen_index_of(id) {
                            scene.screens[i].hidden = !scene.screens[i].hidden;
                            true
                        } else if let Some(i) = scene.environment_index_of(id) {
                            scene.environments[i].hidden = !scene.environments[i].hidden;
                            true
                        } else {
                            false // stale id (deleted) — no-op
                        }
                    }
                    NodeKey::Group(GroupKind::Fixtures) => {
                        let hide = scene.fixtures.iter().any(|f| !f.hidden);
                        for f in &mut scene.fixtures {
                            f.hidden = hide;
                        }
                        !scene.fixtures.is_empty()
                    }
                    NodeKey::Group(GroupKind::Objects) => {
                        let hide = scene.geometry.iter().any(|g| !g.hidden);
                        for g in &mut scene.geometry {
                            g.hidden = hide;
                        }
                        !scene.geometry.is_empty()
                    }
                    NodeKey::Group(GroupKind::Screens) => {
                        let hide = scene.screens.iter().any(|s| !s.hidden);
                        for s in &mut scene.screens {
                            s.hidden = hide;
                        }
                        !scene.screens.is_empty()
                    }
                    NodeKey::EnvGroup | NodeKey::World => {
                        // Toggle all environments (World/HDRI has no hidden field).
                        let hide = scene.environments.iter().any(|e| !e.hidden);
                        for e in &mut scene.environments {
                            e.hidden = hide;
                        }
                        !scene.environments.is_empty()
                    }
                    NodeKey::Root => false,
                };
                if changed {
                    self.undo_push("Toggle visibility", before, scene, dmx.patch());
                    self.undo.set_last_op("object.hide", "Toggle visibility");
                }
            }
            TreeAction::Rename(key, name) => {
                let before = self.undo_begin(scene, dmx.patch());
                let changed = match key {
                    NodeKey::Entity(id) => {
                        if let Some(i) = scene.fixture_index_of(id) {
                            scene.fixtures[i].name = name;
                            true
                        } else if let Some(i) = scene.geometry_index_of(id) {
                            scene.geometry[i].name = name;
                            true
                        } else if let Some(i) = scene.screen_index_of(id) {
                            scene.screens[i].name = name;
                            true
                        } else if let Some(i) = scene.environment_index_of(id) {
                            scene.environments[i].name = name;
                            true
                        } else {
                            false
                        }
                    }
                    _ => false, // only entity leaves are renameable
                };
                if changed {
                    self.undo_push("Rename", before, scene, dmx.patch());
                    self.undo.set_last_op("object.rename", "Rename");
                }
            }
        }
    }

    // --- operator search (F3) + adjust last (F9) ------------------------

    /// Whether a catalog op's `poll` passes right now — drives the search
    /// palette's greyed/enabled state (and the F9 adjust-last guard). Each arm
    /// mirrors the poll the op's run site uses (selection requirements etc.).
    fn op_runnable(&self, id: &str) -> bool {
        match id {
            // Always available (opens the entity picker).
            "object.add" => true,
            // Needs a primary fixture to duplicate.
            "fixture.duplicate" => self.selection.primary_fixture().is_some(),
            // Patch / unpatch operate on the selected fixtures.
            "fixture.patch" | "fixture.unpatch" => !self.selection.fixtures.is_empty(),
            // Delete acts on any selected entity (fixtures / geometry / screens).
            "object.delete" => {
                !self.selection.fixtures.is_empty()
                    || !self.selection.geometry.is_empty()
                    || !self.selection.screens.is_empty()
            }
            _ => false,
        }
    }

    /// Dispatch a catalog operator chosen from the F3 search palette or re-invoked
    /// by F9 (adjust last). `Direct` ops run immediately through [`run_op`]; `Dialog`
    /// ops re-open their parameter dialog (the dialog's confirm then runs the op as
    /// usual — Blender's "adjust last operation" flow: tweak params, re-exec). A
    /// failing `poll` is a clean no-op (the palette already greys those out).
    fn run_catalog_op(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        scene: &mut Scene,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        if !self.op_runnable(id) {
            return;
        }
        // Resolve the descriptor so the `invoke` kind (Dialog vs Direct) drives the
        // path — Dialog ops open their parameter dialog (its confirm runs the op),
        // Direct ops run immediately. The `id` then selects the specific dialog /
        // closure, keeping each op's single real run-site intact.
        let Some(entry) = op::catalog_op(id) else { return };
        match entry.invoke {
            // Parameterized — open the dialog (its confirm runs the op).
            op::OpInvoke::Dialog => match id {
                "object.add" => {
                    // Anchor the Add menu at the screen centre (keyboard-invoked).
                    #[allow(deprecated)] // egui 0.34 screen_rect — content_rect migration later
                    let anchor = ctx.screen_rect().center();
                    self.add_menu.show_at(anchor);
                }
                "fixture.duplicate" => {
                    if let Some(idx) = self.selection.primary_fixture() {
                        self.duplicate = Some(duplicate_dialog_for(ctx, idx));
                    }
                }
                "fixture.patch" => {
                    let (u, a) = next_free_slot(dmx.patch_mut(), scene);
                    self.patch_dialog = windows::PatchDialog {
                        open: true,
                        count: self.selection.fixtures.len(),
                        start_universe: u,
                        start_address: a,
                    };
                }
                _ => {}
            },
            // Direct — run immediately through the operator pipeline.
            op::OpInvoke::Direct => match id {
                "fixture.unpatch" => {
                    self.run_op(
                        "fixture.unpatch",
                        "Unpatch",
                        op::OpFlags::UNDO | op::OpFlags::REGISTER,
                        scene,
                        dmx,
                        true,
                        |cx| {
                            for &fi in &cx.selection.fixtures {
                                cx.patch.unpatch(fi);
                            }
                            op::OpStatus::Finished
                        },
                    );
                }
                "object.delete" => {
                    self.pending_delete = true; // committed after the dock (remaps patch/groups/cues)
                }
                _ => {}
            },
        }
    }

    /// Dispatch a command chosen from the F3 operator-search palette (S2). Resolves
    /// the `id` back to its [`Command`](shortcuts::Command): a catalog op
    /// (`invoke.is_some()`) routes through [`run_catalog_op`](Self::run_catalog_op)
    /// (its existing dialog/direct path); any other command routes its
    /// [`Action`](shortcuts::Action) through [`dispatch_action`](Self::dispatch_action)
    /// — so picking "Top View" / "Frame Selection" / "Toggle Grid" actually runs it.
    /// The palette only ever offers `palette_runnable` ids, so `dispatch_action`
    /// never returns `false` here (the viewport-modal actions are excluded upstream).
    fn run_palette_command(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        let Some(cmd) = shortcuts::command(id) else { return };
        if cmd.invoke.is_some() {
            self.run_catalog_op(ctx, id, scene, dmx);
        } else {
            self.dispatch_action(ctx, cmd.action(), scene, camera, dmx);
        }
    }

    /// F9 "Adjust Last Operation": re-invoke the last registered op so the re-exec
    /// REPLACES its result instead of stacking a second step. If the top undo step
    /// IS that op (it carried `UNDO`), undo it first; a `Dialog` op then re-opens
    /// pre-filled and its confirm pushes the replacement (truncating the redo). The
    /// guard (compare the top step name to the last op) keeps a future
    /// REGISTER-without-UNDO op from undoing an unrelated earlier step. The F3
    /// palette deliberately does NOT route through here — it runs the op fresh.
    /// (If a re-opened dialog is cancelled, the prior result stays undone but is
    /// recoverable with Redo — acceptable for adjust-last.)
    fn adjust_last_op(&mut self, ctx: &egui::Context, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
        let Some((id, label)) = self.undo.last_op().map(|l| (l.id, l.label.clone())) else {
            return;
        };
        if self.undo.undo_name() == Some(label.as_str()) {
            self.do_undo(scene, dmx);
        }
        self.run_catalog_op(ctx, id, scene, dmx);
    }

    // --- `.archie` project save / load ----------------------------------

    /// Gather + serialise the whole project to `path` (bundling GDTF archive
    /// bytes; model + HDRI bytes ride along inside the serialised `Scene`).
    fn write_project(
        &self,
        path: &std::path::Path,
        scene: &Scene,
        camera: &OrbitCamera,
        dmx: &crate::dmx::DmxIo,
    ) -> Result<(), String> {
        let fixture_specs: Vec<Option<String>> = scene
            .fixtures
            .iter()
            .map(|f| f.gdtf.as_ref().map(|g| g.spec.clone()).filter(|s| !s.is_empty()))
            .collect();
        let mut gdtf_assets: HashMap<String, Vec<u8>> = HashMap::new();
        for f in &scene.fixtures {
            if let Some(g) = &f.gdtf {
                if !g.spec.is_empty() {
                    if let Some(raw) = &g.raw {
                        gdtf_assets.entry(g.spec.clone()).or_insert_with(|| raw.as_ref().clone());
                    }
                }
            }
        }
        let pr = project::ProjectRef {
            format: project::FORMAT,
            scene,
            fixture_specs,
            gdtf_assets,
            camera,
            settings: &self.settings,
            prefs: &self.prefs,
            groups: &self.groups,
            cues: &self.cues,
            scene_sort: self.scene_sort,
            patch: dmx.patch(),
            dmx_config: dmx.config(),
        };
        project::write(path, &pr)
    }

    /// Push the active modal transform's axis-constraint into RenderSettings so
    /// the renderer can draw the infinite Blender-style axis line through the
    /// pivot. Call once per frame before `Renderer::render`. Cleared when no op /
    /// no axis is active. (`axis_hint` is `#[serde(skip)]` — runtime-only.)
    pub fn sync_axis_hint(&mut self) {
        self.settings.axis_hint = self.transform.as_ref().and_then(|op| {
            let ax = op.active_axis()?;
            Some((op.pivot, ax.color(), ax.vec()))
        });
    }

    /// Save to the current path, or fall back to Save As if untitled.
    fn save_project(&mut self, scene: &Scene, camera: &OrbitCamera, dmx: &crate::dmx::DmxIo) {
        match self.current_path.clone() {
            Some(path) => self.save_to(&path, scene, camera, dmx),
            None => self.save_project_as(scene, camera, dmx),
        }
    }

    /// Prompt for a destination, then save (forcing the `.archie` extension).
    fn save_project_as(&mut self, scene: &Scene, camera: &OrbitCamera, dmx: &crate::dmx::DmxIo) {
        let name = self
            .current_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("show.{}", project::EXT));
        if let Some(mut path) = rfd::FileDialog::new()
            .add_filter("Archie project", &[project::EXT])
            .set_file_name(&name)
            .save_file()
        {
            if path.extension().and_then(|e| e.to_str()) != Some(project::EXT) {
                path.set_extension(project::EXT);
            }
            self.save_to(&path, scene, camera, dmx);
        }
    }

    fn save_to(
        &mut self,
        path: &std::path::Path,
        scene: &Scene,
        camera: &OrbitCamera,
        dmx: &crate::dmx::DmxIo,
    ) {
        match self.write_project(path, scene, camera, dmx) {
            Ok(()) => {
                self.current_path = Some(path.to_path_buf());
                project::push_recent(path);
                self.recent = project::load_recent();
                let name = path.file_name().map(|s| s.to_string_lossy().into_owned());
                self.notify.success(format!("Saved {}", name.as_deref().unwrap_or("project")));
            }
            Err(e) => self.notify.error(format!("Save failed: {e}")),
        }
    }

    /// Prompt for a `.archie` file, then open it.
    fn open_project_dialog(
        &mut self,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Archie project", &[project::EXT])
            .pick_file()
        {
            self.open_project(&path, scene, camera, dmx);
        }
    }

    /// Open a project file, replacing the current scene + UI/DMX state.
    pub fn open_project(
        &mut self,
        path: &std::path::Path,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        match project::read(path) {
            Ok(p) => {
                self.apply_project(p, scene, camera, dmx);
                self.current_path = Some(path.to_path_buf());
                project::push_recent(path);
                self.recent = project::load_recent();
                self.show_splash = false;
                let name = path.file_name().map(|s| s.to_string_lossy().into_owned());
                self.notify.success(format!(
                    "Opened {} · {} fixtures",
                    name.as_deref().unwrap_or("project"),
                    scene.fixtures.len()
                ));
            }
            Err(e) => self.notify.error(format!("Open failed: {e}")),
        }
    }

    fn apply_project(
        &mut self,
        p: project::Project,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        *scene = p.scene;
        // serde-skip zeroed every EntityId on load → reassign stable ids before
        // the outliner addresses any row (the Fixture.gdtf snapshot-trap class).
        scene.ensure_ids();
        // Re-link each fixture's GDTF by re-parsing the bundled archive (one parse
        // per unique spec, Arc-shared so the renderer's per-type model cache and
        // the GPU wheel atlas stay deduped).
        let mut cache: HashMap<String, Arc<crate::gdtf::GdtfFixture>> = HashMap::new();
        for (i, f) in scene.fixtures.iter_mut().enumerate() {
            let Some(spec) = p.fixture_specs.get(i).cloned().flatten() else { continue };
            let arc = if let Some(a) = cache.get(&spec) {
                a.clone()
            } else if let Some(bytes) = p.gdtf_assets.get(&spec) {
                match crate::gdtf::GdtfFixture::load_bytes(bytes) {
                    Ok(mut g) => {
                        g.spec = spec.clone();
                        g.raw = Some(Arc::new(bytes.clone()));
                        let a = Arc::new(g);
                        cache.insert(spec.clone(), a.clone());
                        a
                    }
                    Err(e) => {
                        self.notify.warn(format!("Could not re-link GDTF {spec}: {e}"));
                        continue;
                    }
                }
            } else {
                continue;
            };
            f.gdtf = Some(arc);
            f.sync_mode();
        }
        // Register the project's fixture types in the library (Replace / add picker).
        for a in cache.values() {
            if !self.library.gdtf.iter().any(|g| g.spec == a.spec) {
                self.library.gdtf.push(a.clone());
            }
        }
        *camera = p.camera;
        self.settings = p.settings;
        self.prefs = p.prefs;
        self.groups = p.groups;
        self.cues = p.cues;
        self.scene_sort = p.scene_sort;
        *dmx.patch_mut() = p.patch;
        *dmx.config_mut() = p.dmx_config;
        self.selection = Selection::default();
    }

    /// Start a fresh, empty project.
    fn new_project(&mut self, scene: &mut Scene, camera: &mut OrbitCamera, dmx: &mut crate::dmx::DmxIo) {
        *scene = Scene::default();
        *dmx.patch_mut() = crate::dmx::PatchTable::default();
        self.groups.clear();
        self.cues = cues::CueEngine::default();
        self.selection = Selection::default();
        self.current_path = None;
        camera.frame(Vec3::ZERO, 12.0);
        self.show_splash = false;
    }

    /// Periodic crash-recovery autosave — writes the whole project to the cache
    /// dir every ~20 s when there's content. Driven from `app::render` with `dt`.
    pub fn autosave_tick(
        &mut self,
        scene: &Scene,
        camera: &OrbitCamera,
        dmx: &crate::dmx::DmxIo,
        dt: f32,
    ) {
        self.autosave_timer += dt;
        if self.autosave_timer < 20.0 {
            return;
        }
        self.autosave_timer = 0.0;
        if scene.fixtures.is_empty() && scene.geometry.is_empty() {
            return;
        }
        if let Some(path) = project::autosave_path() {
            if let Err(e) = self.write_project(&path, scene, camera, dmx) {
                self.notify.warn(format!("Autosave failed: {e}"));
            }
        }
    }

    /// Dismiss the welcome splash (headless screenshot hook).
    pub fn dismiss_splash(&mut self) {
        self.show_splash = false;
    }

    /// Open the welcome splash — Window ▸ Welcome, the New command, and the operator
    /// search all route here.
    pub fn show_welcome(&mut self) {
        self.show_splash = true;
    }

    /// The welcome hero image, decoded from the bundled JPEG and uploaded to the GPU
    /// once (lazily, then cached). `None` only if the embedded image fails to decode.
    fn welcome_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.welcome_tex.is_none() {
            static BYTES: &[u8] = include_bytes!("welcome.jpg");
            // Cap the longest side well under the GPU/egui max-texture-side (2048) —
            // the splash shows it ~580 px wide, so this never costs visible quality and
            // guards against a "texture too large" panic if the asset is ever swapped.
            // `resize` only ever downscales (it preserves aspect, fitting the box).
            let img = image::load_from_memory(BYTES)
                .ok()?
                .resize(1600, 1600, image::imageops::FilterType::Lanczos3)
                .to_rgba8();
            let (w, h) = img.dimensions();
            let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
            self.welcome_tex = Some(ctx.load_texture("welcome-hero", color, egui::TextureOptions::LINEAR));
        }
        self.welcome_tex.clone()
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

    /// The Blender-style welcome / recover splash, shown once at startup.
    fn splash_window(
        &mut self,
        ctx: &egui::Context,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        if !self.show_splash {
            return;
        }
        let mut action: Option<SplashAction> = None;
        // Pull the hero texture + recent list out before the modal closure so the
        // closure borrows neither `self` (welcome_texture needs &mut self) nor the
        // recent Vec mutably.
        let hero = self.welcome_texture(ctx);
        let recent: Vec<PathBuf> = self.recent.iter().take(8).cloned().collect();
        let autosave = project::autosave_path().filter(|p| p.exists());
        let modal = egui::Modal::new(egui::Id::new("welcome-splash")).show(ctx, |ui| {
            ui.set_width(580.0);
            // ---- hero image (full width) with the wordmark + version overlaid on a
            // bottom scrim, like Blender's splash artwork. ----
            if let Some(tex) = &hero {
                let w = ui.available_width();
                let sz = tex.size();
                let h = w * sz[1] as f32 / sz[0] as f32;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
                let painter = ui.painter_at(rect);
                // Round ALL FOUR corners so the artwork reads as a rounded card inside
                // the modal (the dialog itself rounds at ~7px). The bottom corners must
                // round too — the semi-transparent scrim can't hide a square image
                // corner behind it.
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                let radius = egui::CornerRadius::same(6);
                painter.add(
                    egui::epaint::RectShape::filled(rect, radius, egui::Color32::WHITE)
                        .with_texture(tex.id(), uv),
                );
                let scrim = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.bottom() - 46.0),
                    rect.right_bottom(),
                );
                // Match the dark strip to the image's bottom edge: round its BOTTOM
                // corners by the same radius so the scrim hugs the rounded outline.
                let bottom = egui::CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 };
                painter.add(egui::epaint::RectShape::filled(
                    scrim,
                    bottom,
                    egui::Color32::from_black_alpha(150),
                ));
                painter.text(
                    egui::pos2(rect.left() + 16.0, rect.bottom() - 23.0),
                    egui::Align2::LEFT_CENTER,
                    "previz",
                    egui::FontId::proportional(26.0),
                    egui::Color32::WHITE,
                );
                painter.text(
                    egui::pos2(rect.right() - 16.0, rect.bottom() - 23.0),
                    egui::Align2::RIGHT_CENTER,
                    format!("v{} alpha", env!("CARGO_PKG_VERSION")),
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_white_alpha(210),
                );
            } else {
                // Fallback if the bundled image fails to decode: the plain text header.
                ui.horizontal(|ui| {
                    ui.heading("previz");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("v{} alpha", env!("CARGO_PKG_VERSION")))
                                .weak()
                                .small(),
                        );
                    });
                });
            }

            ui.add_space(14.0);
            ui.columns(2, |cols| {
                // Left — start a session.
                cols[0].label(egui::RichText::new("New").strong());
                cols[0].add_space(6.0);
                if cols[0]
                    .add_sized([240.0, 30.0], egui::Button::new(format!("{}  New Project", theme::icon::SCENE)))
                    .clicked()
                {
                    action = Some(SplashAction::New);
                }
                if cols[0]
                    .add_sized([240.0, 30.0], egui::Button::new(format!("{}  Open…", theme::icon::IMPORT_MVR)))
                    .clicked()
                {
                    action = Some(SplashAction::Open);
                }
                if let Some(ap) = &autosave {
                    cols[0].add_space(10.0);
                    if cols[0]
                        .add_sized([240.0, 30.0], egui::Button::new(format!("{}  Recover Last Session", theme::icon::FRAME)))
                        .on_hover_text("Reopen the auto-saved session from the last run")
                        .clicked()
                    {
                        action = Some(SplashAction::Recover(ap.clone()));
                    }
                }

                // Right — recent files.
                cols[1].label(egui::RichText::new("Recent Files").strong());
                cols[1].add_space(6.0);
                if recent.is_empty() {
                    cols[1].label(egui::RichText::new("No recent projects").weak().small());
                } else {
                    for p in &recent {
                        let name = p
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if cols[1]
                            .add(egui::Button::new(format!("{}  {}", theme::icon::PROFILE, name)).frame(false))
                            .on_hover_text(p.display().to_string())
                            .clicked()
                        {
                            action = Some(SplashAction::OpenPath(p.clone()));
                        }
                    }
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Continue without a project").clicked() {
                    action = Some(SplashAction::Dismiss);
                }
            });
        });

        if modal.should_close() {
            self.show_splash = false;
        }
        match action {
            Some(SplashAction::New) => self.new_project(scene, camera, dmx),
            Some(SplashAction::Open) => self.open_project_dialog(scene, camera, dmx),
            Some(SplashAction::OpenPath(p)) => self.open_project(&p, scene, camera, dmx),
            Some(SplashAction::Recover(p)) => {
                self.open_project(&p, scene, camera, dmx);
                // A recovered session is untitled — don't let Save clobber the
                // autosave file; force Save As on the next save.
                self.current_path = None;
            }
            Some(SplashAction::Dismiss) => self.show_splash = false,
            None => {}
        }
    }

    /// The default dock layout (also used by Window ▸ Reset Panel Layout).
    ///
    /// egui_dock's `fraction` is the share given to the side being split toward:
    /// `split_left(n, f)` makes the NEW left panel `f` of the width, `split_right`
    /// makes the new right panel `1 - f`, `split_below` the new bottom `1 - f`.
    /// (The old code passed 0.80 to `split_left` expecting the central to keep
    /// 80% — that made the Scene sidebar 80% wide, the startup-layout bug.)
    fn default_dock(&self) -> DockState<Tab> {
        self.workspaces.active().dock.clone()
    }

    /// Map a workspace's recorded [`Overlays`] onto the live overlay flags. The
    /// single source of the field mapping (called on workspace ACTIVATION). NOT a
    /// lock — the user can still toggle any of these afterward; a workspace just
    /// presets the emphasis (the design note: soft contexts, no gating).
    fn apply_overlays(prefs: &mut Preferences, settings: &mut RenderSettings, ov: workspaces::Overlays) {
        prefs.show_labels = ov.labels;
        prefs.show_stats = ov.stats;
        settings.show_grid = ov.grid;
        prefs.show_gizmos = ov.gizmos;
    }

    /// Activate the workspace at `idx`: apply its saved dock layout, preset its
    /// default tool, and emphasise its overlay flags. A no-op for an out-of-range
    /// index. Records the choice as active (persisted) so the app reopens here. This
    /// changes the STARTING arrangement only — nothing is locked or gated.
    fn activate_workspace(&mut self, idx: usize) {
        let Some(ws) = self.workspaces.items.get(idx).cloned() else { return };
        self.dock = ws.dock.clone();
        self.active_tool = ws.default_tool;
        Self::apply_overlays(&mut self.prefs, &mut self.settings, ws.overlays);
        self.workspaces.set_active(idx);
        self.notify.info(format!("Workspace: {}", ws.name));
    }

    /// Capture the LIVE dock layout + current tool + current overlay flags as a
    /// workspace named `name` (overwriting a same-named record), then activate it.
    /// The "Save current as workspace…" commit.
    fn save_current_workspace(&mut self, name: &str) {
        let overlays = workspaces::Workspaces::capture_overlays(
            self.prefs.show_labels,
            self.prefs.show_stats,
            self.settings.show_grid,
            self.prefs.show_gizmos,
        );
        let idx = self.workspaces.save_current(name, self.dock.clone(), self.active_tool, overlays);
        self.notify.success(format!("Saved workspace: {}", self.workspaces.items[idx].name));
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
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("previz.text_focus"), text_focus));
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
                shortcuts::ActiveContext { viewport_focused: true, transform_active: false },
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
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("previz.dupgrab.start"), true));
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
            groups: &mut self.groups,
            group_name: &mut self.group_name,
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
                if let Some(sel) = panels::add_active_library_item(&self.library, scene, camera, &active, cursor) {
                    self.selection = sel;
                }
            }
        }
        replace_window(ctx, &self.library, scene, &mut self.selection, dmx.patch_mut(), &mut self.replace);

        // Patch / Unpatch dialogs (committed here, where the patch is reachable).
        // On confirm, P assigns sequential addresses to the selected fixtures from
        // the chosen start slot; U disables the selected fixtures' patch entries.
        if windows::patch_dialog_window(ctx, &mut self.patch_dialog) {
            let (u, a) = (self.patch_dialog.start_universe, self.patch_dialog.start_address);
            self.run_op(
                "fixture.patch",
                "Patch",
                op::OpFlags::UNDO | op::OpFlags::REGISTER,
                scene,
                dmx,
                true,
                |cx| {
                    cx.patch.assign_indices(cx.scene, &cx.selection.fixtures, u, a);
                    op::OpStatus::Finished
                },
            );
        }
        if windows::unpatch_dialog_window(ctx, &mut self.unpatch_dialog) {
            self.run_op(
                "fixture.unpatch",
                "Unpatch",
                op::OpFlags::UNDO | op::OpFlags::REGISTER,
                scene,
                dmx,
                true,
                |cx| {
                    for &fi in &cx.selection.fixtures {
                        cx.patch.unpatch(fi);
                    }
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

    /// Draw + resolve the `~` View pie. Sectors are laid out clock-face from the
    /// top so the cardinal axis views land where the user expects (Top up, Bottom
    /// down, Left/Right to the sides); the diagonals carry Front/Back, the
    /// Persp/Ortho toggle, and Frame Selected. A pick applies to the camera.
    fn view_pie(&mut self, ctx: &egui::Context, camera: &mut OrbitCamera, scene: &Scene) {
        use theme::icon;

        // One sector per entry; index == sector. Order = clockwise from straight up.
        #[derive(Clone, Copy)]
        enum Choice {
            View(CameraView),
            ToggleOrtho,
            FrameSelected,
        }
        let items = [
            (icon::VIEWPORT, "Top", Choice::View(CameraView::Top)),
            (icon::VIEWPORT, "Right", Choice::View(CameraView::Right)),
            (icon::CAMERA, "Persp/Ortho", Choice::ToggleOrtho),
            (icon::VIEWPORT, "Back", Choice::View(CameraView::Back)),
            (icon::VIEWPORT, "Bottom", Choice::View(CameraView::Bottom)),
            (icon::FRAME, "Frame Sel.", Choice::FrameSelected),
            (icon::VIEWPORT, "Front", Choice::View(CameraView::Front)),
            (icon::VIEWPORT, "Left", Choice::View(CameraView::Left)),
        ];
        let sectors: Vec<pie::PieItem> =
            items.iter().map(|(ic, lbl, _)| pie::PieItem::new(ic, *lbl)).collect();
        let accent = theme::accent(&self.prefs);
        if let Some(i) = pie::Pie::new(&sectors).accent(accent).show(ctx, &mut self.view_pie) {
            match items[i].2 {
                Choice::View(v) => camera.set_view(v),
                Choice::ToggleOrtho => camera.toggle_ortho(),
                Choice::FrameSelected => {
                    if let Some((lo, hi)) = self.frame_bounds(scene, true) {
                        camera.frame_aabb(lo, hi);
                    }
                }
            }
        }
    }

    /// Draw + resolve the `Z` Shading pie (Blender's Z pie). Sectors pick the
    /// viewport display Mode (Beauty / Unlit / Wireframe) plus two quick overlay
    /// toggles (Grid, Stats); a pick applies immediately to `settings` / `prefs`.
    /// Built via the generic [`pie::choose`] helper so the enum/toggle mapping is a
    /// one-liner over labelled values.
    fn shading_pie(&mut self, ctx: &egui::Context) {
        let accent = theme::accent(&self.prefs);
        if let Some(choice) = pie::choose(ctx, &mut self.shading_pie, accent, shading_pie_choices()) {
            self.apply_shading_choice(choice);
        }
    }

    /// Apply one resolved [`ShadingChoice`] from the Z pie. Split out (and given a
    /// pure `&mut self`) so the sector→effect mapping is unit-testable without an
    /// egui context.
    fn apply_shading_choice(&mut self, choice: ShadingChoice) {
        match choice {
            ShadingChoice::Mode(m) => self.settings.mode = m,
            ShadingChoice::ToggleGrid => self.settings.show_grid = !self.settings.show_grid,
            ShadingChoice::ToggleStats => self.prefs.show_stats = !self.prefs.show_stats,
        }
    }

    /// Force the quick-select palette open (headless screenshot hook).
    pub fn debug_open_quick_select(&mut self) {
        self.quick_select = true;
    }

    /// Force the F3 operator-search palette open (headless screenshot hook).
    pub fn debug_open_op_search(&mut self) {
        self.op_search.show();
    }

    /// Open the `~` View pie at the screen centre (headless screenshot hook).
    pub fn debug_open_view_pie(&mut self) {
        self.view_pie.open_at(egui::Pos2::new(750.0, 475.0));
    }

    /// Open the `Z` Shading pie at the screen centre (headless screenshot hook).
    /// Wired into the PREVIZ_UI harness by the lead (in the off-limits app.rs);
    /// dead in the default build until then.
    #[allow(dead_code)]
    pub fn debug_open_shading_pie(&mut self) {
        self.shading_pie.open_at(egui::Pos2::new(750.0, 475.0));
    }

    /// Open the online Fixture Library window (headless hook). `demo` injects fake
    /// catalogue rows so the browse view renders without real credentials.
    pub fn debug_open_share(&mut self, demo: bool) {
        self.show_share = true;
        if demo {
            self.share.debug_demo();
        }
    }

    /// Select the first fixture and open the Replace dialog (headless hook).
    pub fn debug_open_replace(&mut self, scene: &Scene) {
        if !scene.fixtures.is_empty() {
            self.selection = Selection::fixture(0);
            self.replace = Some(ReplaceDialog::default());
        }
    }

    /// Open the profile editor for the first GDTF fixture (headless hook).
    pub fn debug_open_profile(&mut self, scene: &Scene) {
        if let Some(i) = scene.fixtures.iter().position(|f| f.is_gdtf()) {
            self.selection = Selection::fixture(i);
            self.profile = Some(ProfileEditor::new(i));
        }
    }

    /// Select the first GDTF fixture (headless hook for inspector screenshots).
    pub fn debug_select_first_gdtf(&mut self, scene: &Scene) {
        if let Some(i) = scene.fixtures.iter().position(|f| f.is_gdtf()) {
            self.selection = Selection::fixture(i);
        }
    }

    /// Select the World root so the Inspector shows the Render Properties.
    /// Headless hook (`PREVIZ_UI_WORLD`) for the render-inspector screenshot.
    pub fn debug_select_world(&mut self) {
        self.selection = Selection::world();
    }

    /// Multi-select up to `n` fixtures sharing the profile of the first fixture
    /// that has a wheel chain (so the bulk Wheels section is exercised); falls
    /// back to the first `n` GDTF fixtures. Headless hook for bulk screenshots.
    pub fn debug_select_n(&mut self, scene: &Scene, n: usize) {
        let with_wheels = scene.fixtures.iter().position(|f| {
            f.gdtf
                .as_ref()
                .and_then(|g| g.modes.get(f.mode_index))
                .is_some_and(|m| !m.components.is_empty())
        });
        let pick: Vec<usize> = match with_wheels {
            Some(seed) => {
                let prof = scene.fixtures[seed].profile.clone();
                scene
                    .fixtures
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.profile == prof)
                    .map(|(i, _)| i)
                    .take(n)
                    .collect()
            }
            None => scene.fixtures.iter().enumerate().filter(|(_, f)| f.is_gdtf()).map(|(i, _)| i).take(n).collect(),
        };
        if !pick.is_empty() {
            self.selection = Selection { fixtures: pick, geometry: Vec::new(), screens: Vec::new(), environment: None, world: false };
        }
    }

    /// Make the named tab the active one in its leaf (used by the headless UI
    /// screenshot path to capture a specific panel).
    pub fn focus_tab_by_title(&mut self, title: &str) {
        let tabs = [
            Tab::Viewport,
            Tab::Scene,
            Tab::Library,
            Tab::Inspector,
            Tab::DmxMonitor,
            Tab::Patch,
            Tab::Cues,
            Tab::Connectivity,
            Tab::Render,
        ];
        for tab in tabs {
            if tab.title() == title
                && let Some(path) = self.dock.find_tab(&tab)
            {
                let _ = self.dock.set_active_tab(path);
                return;
            }
        }
    }

    /// Open the Render tab if it isn't already, then focus it. Called when a
    /// render starts so the result is front-and-centre (Blender pops the Image
    /// Editor; we focus the dockable Render tab).
    pub fn ensure_render_tab_focused(&mut self) {
        if let Some(path) = self.dock.find_tab(&Tab::Render) {
            let _ = self.dock.set_active_tab(path);
            return;
        }
        // Not open yet — add it beside the Viewport (the large central leaf) so the
        // render is front-and-centre, not crammed into whatever narrow side panel
        // happened to hold focus when Render was clicked.
        if let Some(vp) = self.dock.find_tab(&Tab::Viewport) {
            self.dock.set_focused_node_and_surface(vp.node_path());
        }
        self.dock.push_to_focused_leaf(Tab::Render);
    }

    /// Whether a dock tab is currently open.
    fn is_tab_open(&self, tab: Tab) -> bool {
        self.dock.find_tab(&tab).is_some()
    }

    /// Show or hide a dock tab.
    fn toggle_tab(&mut self, tab: Tab) {
        if let Some(path) = self.dock.find_tab(&tab) {
            self.dock.remove_tab(path);
        } else {
            // Re-open on the panel's natural EDGE as a whole side pane — NOT merged as a
            // tab into the focused (viewport) leaf. So `N` brings the Inspector back as
            // the right sidebar, Scene/Library return on the left, etc. Splitting the
            // root spans the full height of that edge.
            let root = egui_dock::NodeIndex::root();
            match tab {
                Tab::Scene | Tab::Library => {
                    self.dock.main_surface_mut().split_left(root, 0.17, vec![tab]);
                }
                Tab::Inspector => {
                    self.dock.main_surface_mut().split_right(root, 0.80, vec![tab]);
                }
                Tab::Patch | Tab::DmxMonitor | Tab::Cues | Tab::Connectivity => {
                    self.dock.main_surface_mut().split_below(root, 0.7, vec![tab]);
                }
                // Viewport / Render: no canonical edge — fall back to the focused leaf.
                _ => {
                    self.dock.push_to_focused_leaf(tab);
                }
            }
        }
    }

    /// AABB of the current selection (or whole scene if nothing selected),
    /// padded a little, for the Frame commands. Unified across EVERY placed kind
    /// (fixtures + geometry + screens + environments) via `Scene::object_world_bounds`,
    /// so "Frame Selected" works on a lone object/screen/env, not just fixtures.
    fn frame_bounds(&self, scene: &Scene, selection_only: bool) -> Option<(Vec3, Vec3)> {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        let mut any = false;
        let mut grow = |b: Option<(Vec3, Vec3)>| {
            if let Some((blo, bhi)) = b {
                lo = lo.min(blo);
                hi = hi.max(bhi);
                any = true;
            }
        };

        if selection_only && self.selection.has_object() {
            // Frame the selection, whatever its kind(s) — union each object's AABB.
            for r in self.selection.object_refs() {
                grow(scene.object_world_bounds(r));
            }
        } else {
            // Whole scene: fixtures + placed geometry + screens. Environments (the
            // fog box) are excluded here — they span the whole set and would zoom
            // the camera right out; a user framing one explicitly hits the path above.
            for i in 0..scene.fixtures.len() {
                grow(scene.object_world_bounds(ObjectRef::Fixture(i)));
            }
            for i in 0..scene.geometry.len() {
                grow(scene.object_world_bounds(ObjectRef::Geometry(i)));
            }
            for i in 0..scene.screens.len() {
                grow(scene.object_world_bounds(ObjectRef::Screen(i)));
            }
        }

        if !any {
            return None;
        }
        let pad = Vec3::splat(1.0);
        Some((lo - pad, hi + pad))
    }

    /// The median world point of the current selection (fixtures + geometry +
    /// screens). `None` when nothing addressable is selected — the "Snap cursor to
    /// selection" command then leaves the cursor where it is. Geometry/screen
    /// anchors are the translation component of their world transform.
    fn selection_median(&self, scene: &Scene) -> Option<Vec3> {
        let mut sum = Vec3::ZERO;
        let mut n = 0.0_f32;
        for &i in &self.selection.fixtures {
            if let Some(f) = scene.fixtures.get(i) {
                sum += f.position;
                n += 1.0;
            }
        }
        for &i in &self.selection.geometry {
            if let Some(g) = scene.geometry.get(i) {
                sum += g.transform.w_axis.truncate();
                n += 1.0;
            }
        }
        for &i in &self.selection.screens {
            if let Some(s) = scene.screens.get(i) {
                sum += s.transform.w_axis.truncate();
                n += 1.0;
            }
        }
        (n > 0.0).then(|| sum / n)
    }

    /// Keyboard shortcuts handled globally (camera framing/views, delete, etc.).
    /// Every bind comes from the central [`shortcuts`] registry; this is the
    /// dispatcher for the `Global` context. The viewport-owned transform binds
    /// (G/R/S, X/Y/Z) are registered there too but dispatched in `panels::viewport`.
    /// The keymap contexts active this frame (most-specific-first gating lives in
    /// `shortcuts::gather`). Viewport binds only apply while the 3D viewport holds
    /// focus; a live G/R/S transform owns the viewport, suppressing the plain
    /// press-keymaps (its keys route through `shortcuts::poll_modal`).
    fn active_context(&self) -> shortcuts::ActiveContext {
        shortcuts::ActiveContext {
            viewport_focused: self.viewport_focused,
            transform_active: self.transform.is_some(),
        }
    }

    fn handle_shortcuts(
        &mut self,
        ctx: &egui::Context,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        // Keymap-v2: gather the active contexts (most-specific-first) and let the
        // stack do the gating. A focused viewport's `S` (Scale, Viewport map) now
        // MASKS the Global `S` (quick-select) automatically — no `s_is_scale`
        // guard needed. `poll` returns empty while a text field has focus.
        //
        // S1: the per-Action effect now lives in ONE place — `dispatch_action` —
        // so this loop just polls + delegates. The deferred-commit pattern is
        // preserved inside `dispatch_action` (nudge accumulates into
        // `pending_nudge`, Delete/Patch/Unpatch defer to after the dock).
        let cx = self.active_context();
        // Reset the per-frame nudge accumulator before dispatch re-fills it; show()
        // applies it where the patch is reachable so a held-key burst coalesces.
        self.pending_nudge = Vec3::ZERO;
        for a in shortcuts::poll(ctx, cx, &self.keymap_overrides) {
            self.dispatch_action(ctx, a, scene, camera, dmx);
        }
    }

    /// The SINGLE source of truth for every NON-viewport-modal [`Action`]'s effect
    /// (S1). Both poll sites in [`show`](Self::show) — the framing/selection/nudge
    /// set and the file/patch/edit set — delegate here, so a shortcut, a menu entry
    /// and the F3 palette all reach the same code. Returns `true` if the action was
    /// handled, `false` for the viewport-owned modal ones (G/R/S transform grab +
    /// X/Y/Z axis lock), which need the live viewport mouse state and are dispatched
    /// in `panels::viewport`.
    ///
    /// Behaviour-preserving: the framing/selection/nudge/view actions take effect
    /// immediately; Delete defers to `pending_delete` (committed after the dock so
    /// patch/groups/cues remap in lock-step); Patch/Unpatch open their dialogs whose
    /// confirm commits after the dock; nudge accumulates into `pending_nudge`.
    fn dispatch_action(
        &mut self,
        ctx: &egui::Context,
        action: shortcuts::Action,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) -> bool {
        use shortcuts::{Action, Dir};
        // Arrow nudges only act when the viewport has focus and no transform is in
        // progress (so they don't fight panel scrolling or the live G/R/S op).
        let nudge_ok =
            self.viewport_focused && self.transform.is_none() && !self.selection.fixtures.is_empty();
        match action {
            // --- View / framing -------------------------------------------------
            Action::FrameSelection => {
                if let Some((lo, hi)) = self.frame_bounds(scene, true) {
                    camera.frame_aabb(lo, hi);
                }
            }
            Action::FrameAll => {
                if let Some((lo, hi)) = self.frame_bounds(scene, false) {
                    camera.frame_aabb(lo, hi);
                }
            }
            Action::View(view) => camera.set_view(view),
            // numpad-5: pure persp↔ortho toggle (no angle change).
            Action::ToggleOrtho => camera.toggle_ortho(),
            // numpad 2/4/6/8: orbit by a fixed step (yaw_deg, pitch_deg).
            Action::OrbitStep(yaw, pitch) => camera.orbit_step(yaw, pitch),
            Action::ViewCamera => {} // registered for the cheat sheet; no-op.
            // `~` — open the radial View pie at the cursor (axis views +
            // projection toggle + frame selected). The keymap stack already
            // gates this to a focused viewport with no live transform / text
            // field, so no extra guard here. The pie itself is drawn + resolved
            // after the dock, in `show`.
            Action::ViewPie => {
                #[allow(deprecated)] // egui 0.34 screen_rect — content_rect migration later
                let anchor = ctx.pointer_latest_pos().unwrap_or_else(|| ctx.screen_rect().center());
                self.view_pie.open_at(anchor);
            }
            // `Z` — open the radial Shading pie at the cursor (display mode + grid /
            // stats toggles). Same deferred-draw pattern as the View pie: opened
            // here, drawn + resolved after the dock in `show`.
            Action::ShadingPie => {
                #[allow(deprecated)] // egui 0.34 screen_rect — content_rect migration later
                let anchor = ctx.pointer_latest_pos().unwrap_or_else(|| ctx.screen_rect().center());
                self.shading_pie.open_at(anchor);
            }
            // --- View bookmarks (P1 #34) ---------------------------------------
            // Save the live camera pose into the next free numbered slot; recall a
            // slot eases the camera there (`apply_pose` reuses `animate_to`).
            Action::SaveBookmark => match self.bookmarks.save_pose(camera.pose()) {
                Some(slot) => self.notify.success(format!("Saved view bookmark {slot}")),
                None => self.notify.warn("All view bookmark slots are full"),
            },
            Action::RecallBookmark(slot) => match self.bookmarks.pose_in_slot(slot) {
                Some(pose) => camera.apply_pose(&pose),
                None => self.notify.info(format!("View bookmark {slot} is empty")),
            },
            Action::ToggleLabels => self.prefs.show_labels = !self.prefs.show_labels,
            Action::ToggleStats => self.prefs.show_stats = !self.prefs.show_stats,
            Action::ToggleGrid => self.settings.show_grid = !self.settings.show_grid,
            Action::ToggleGizmos => self.prefs.show_gizmos = !self.prefs.show_gizmos,
            Action::ToggleHint => self.prefs.show_hint = !self.prefs.show_hint,
            // N / T — toggle the viewport's side regions (§2.2). Viewport-only
            // binds (the keymap stack already gates them to a focused viewport
            // and `poll` is silent while a text field has focus).
            // N now shows/hides the single docked Inspector tab (the inline N-panel
            // inspector was removed — it duplicated this and stretched the viewport).
            Action::ToggleNPanel => self.toggle_tab(Tab::Inspector),
            Action::ToggleTPanel => {
                self.viewport_regions.t_open = !self.viewport_regions.t_open;
            }
            // --- Selection ------------------------------------------------------
            // Context gating (Viewport `S` = Scale masks this when the viewport
            // is focused) means QuickSelect only reaches here when it should.
            Action::QuickSelect => self.quick_select = true,
            // Select All (#88): every item of the ACTIVE kind (fixtures by default;
            // objects/screens when one of those is the current selection). Mirrors
            // Blender's `A` acting on the active mode's collection.
            Action::SelectAll => {
                let counts = (scene.fixtures.len(), scene.geometry.len(), scene.screens.len());
                let kind = self.selection.active_kind();
                self.selection.select_all_of(kind, counts);
                self.scene_anchor = None;
            }
            // Deselect / None (#88): clear the selection. Bound to Alt+A and Escape;
            // Escape only reaches here when no dialog / pie / quick-select consumed it
            // first (the keymap poll is silent while a text field is focused), so this
            // is the "nothing else wanted it" global clear (Blender's Alt+A).
            Action::Deselect => {
                self.selection = Selection::default();
                self.scene_anchor = None;
            }
            // Invert (#88): flip membership within the active kind. Defaults to
            // fixtures when nothing is selected, so a bare Ctrl+I selects everything.
            Action::SelectInvert => {
                let counts = (scene.fixtures.len(), scene.geometry.len(), scene.screens.len());
                let kind = self.selection.active_kind();
                self.selection.invert_within(kind, counts);
                self.scene_anchor = None;
            }
            Action::Replace => {
                if self.replace.is_none() && !self.selection.fixtures.is_empty() {
                    self.replace = Some(ReplaceDialog::default());
                }
            }
            // --- Object / transform ---------------------------------------------
            Action::Delete => {
                if !self.selection.fixtures.is_empty()
                    || !self.selection.geometry.is_empty()
                    || !self.selection.screens.is_empty()
                {
                    // committed after the dock (remaps patch/groups/cues)
                    self.pending_delete = true;
                }
            }
            Action::Nudge(dir, step) => {
                if nudge_ok {
                    self.pending_nudge += match dir {
                        Dir::XNeg => Vec3::new(-step, 0.0, 0.0),
                        Dir::XPos => Vec3::new(step, 0.0, 0.0),
                        Dir::ZNeg => Vec3::new(0.0, 0.0, -step),
                        Dir::ZPos => Vec3::new(0.0, 0.0, step),
                        Dir::YUp => Vec3::new(0.0, step, 0.0),
                        Dir::YDown => Vec3::new(0.0, -step, 0.0),
                    };
                }
            }
            // Shift+A (Viewport keymap): open the cursor-anchored Add menu.
            // The keymap stack only surfaces this while the viewport is focused
            // and no transform is live, so no extra guard is needed here.
            Action::AddMenu => {
                // Anchor on the live cursor; fall back to the viewport-ish
                // screen centre if the pointer position is unknown (keyboard).
                #[allow(deprecated)] // egui 0.34 screen_rect — content_rect migration later
                let anchor = ctx.pointer_latest_pos().unwrap_or_else(|| ctx.screen_rect().center());
                self.add_menu.show_at(anchor);
            }
            // Patch / Unpatch (P/U) — only meaningful with fixtures selected. Open
            // the dialog now (the patch is reachable here); its confirm commits the
            // mutation after the dock, like the old `do_patch` / `do_unpatch` path.
            Action::Patch => {
                if !self.selection.fixtures.is_empty() {
                    let (u, a) = next_free_slot(dmx.patch_mut(), scene);
                    self.patch_dialog = windows::PatchDialog {
                        open: true,
                        count: self.selection.fixtures.len(),
                        start_universe: u,
                        start_address: a,
                    };
                }
            }
            Action::Unpatch => {
                if !self.selection.fixtures.is_empty() {
                    self.unpatch_dialog = windows::UnpatchDialog {
                        open: true,
                        count: self.selection.fixtures.len(),
                    };
                }
            }
            // --- 3D cursor (S1-3d-cursor) --------------------------------------
            // Snap the world cursor to the selection median (Blender's Shift+S →
            // "Cursor to Selected"); mark it set so Add places there. A no-op when
            // nothing addressable is selected (cursor stays put).
            Action::SnapCursorToSelection => {
                if let Some(p) = self.selection_median(scene) {
                    self.cursor_3d = p;
                    self.cursor_3d_set = true;
                    self.notify.info(format!("Cursor {}  selection", theme::icon::ARROW_RIGHT));
                }
            }
            // Reset the world cursor to the origin and forget the "set this session"
            // flag, so Add returns to its view-centre placement default.
            Action::ResetCursor => {
                self.cursor_3d = Vec3::ZERO;
                self.cursor_3d_set = false;
                self.notify.info(format!("Cursor {}  origin", theme::icon::ARROW_RIGHT));
            }
            // --- Edit / history -------------------------------------------------
            Action::Undo => self.do_undo(scene, dmx),
            Action::Redo => self.do_redo(scene, dmx),
            Action::OperatorSearch => self.op_search.show(),
            // F9 "adjust last operation": re-invoke the last registered op by id (a
            // parameterized op re-opens its dialog so the user can tweak + re-exec).
            Action::AdjustLast => self.adjust_last_op(ctx, scene, dmx),
            // --- App / file -----------------------------------------------------
            Action::Preferences => self.show_prefs = true,
            // Toggle (not just open) so the same key/palette pick closes it again.
            Action::ToggleReportLog => self.show_report_log = !self.show_report_log,
            // Re-open the welcome / recover splash (Window ▸ Welcome + operator search).
            Action::ShowWelcome => self.show_welcome(),
            // --- Workspaces (S1) — soft "modes" ---------------------------------
            // Activate the saved workspace at `idx` (layout + tool + overlay
            // emphasis; no locking). An out-of-range slot is a clean no-op (fewer
            // workspaces than the 9 registered commands).
            Action::ActivateWorkspace(idx) => self.activate_workspace(idx),
            // Open the "Save current as workspace…" dialog seeded with the active
            // workspace's name.
            Action::SaveWorkspace => {
                self.save_workspace = Some(self.workspaces.active().name.clone());
            }
            Action::Save => self.save_project(scene, camera, dmx),
            Action::SaveAs => self.save_project_as(scene, camera, dmx),
            Action::Open => self.open_project_dialog(scene, camera, dmx),
            // "New" re-opens the welcome so the user picks how to start (a blank
            // project, open, or a recent) — the splash's own "New Project" button is
            // what actually creates the blank scene (`new_project`).
            Action::New => self.show_welcome(),
            // --- Viewport-owned actions — NOT handled here ----------------------
            // G/R/S grab + X/Y/Z axis lock need the live viewport mouse state, and
            // Duplicate (D / Shift+D) is gated on the viewport's `consumed`/focus
            // state — all are dispatched in `panels::viewport`. Report unhandled so
            // the palette never offers the modal ones as dead picks. (Duplicate IS
            // a palette op, but via its `fixture.duplicate` dialog catalog entry,
            // not this keymap Action.)
            Action::Transform(_) | Action::AxisLock(_) | Action::Duplicate | Action::DuplicateGrab => {
                return false;
            }
        }
        true
    }

    /// Commit a requested fixture deletion through the operator pipeline. The
    /// actual remap-in-lock-step body is the free [`delete_selection`] operator
    /// (so it edits the shared [`OpCtx`] surface); this wrapper clears the request
    /// flag, runs it under [`run_op`](Self::run_op) (which snapshots + pushes
    /// undo), and resets the UI-only scene-anchor when something was removed.
    /// Called once after the dock, where the patch is reachable.
    fn commit_delete(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
        self.pending_delete = false;
        let poll = !self.selection.fixtures.is_empty()
            || !self.selection.geometry.is_empty()
            || !self.selection.screens.is_empty();
        let status = self.run_op(
            "object.delete",
            "Delete",
            op::OpFlags::UNDO | op::OpFlags::REGISTER,
            scene,
            dmx,
            poll,
            delete_selection,
        );
        if status == op::OpStatus::Finished {
            self.scene_anchor = None;
        }
    }

    /// Import any `.gdtf` / `.mvr` files dropped onto the window.
    fn handle_dropped_files(&mut self, ctx: &egui::Context, scene: &mut Scene, camera: &mut OrbitCamera) {
        // The common case is nothing dropped — avoid the per-frame allocation.
        if ctx.input(|i| i.raw.dropped_files.is_empty()) {
            return;
        }
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
        });
        for path in dropped {
            match path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()) {
                Some(ext) if ext == "gdtf" => match self.library.import_gdtf(&path) {
                    Ok(idx) => {
                        let arc = self.library.gdtf[idx].clone();
                        let name = arc.name.clone();
                        let f = scene.add_gdtf(arc, Vec3::new(0.0, 4.0, 0.0));
                        self.selection = Selection::fixture(f);
                        self.notify.success(format!("Imported {name}"));
                    }
                    Err(e) => self.notify.error(format!("Import GDTF failed: {e}")),
                },
                Some(ext) if ext == "mvr" => match crate::mvr::MvrImport::load_path(&path) {
                    Ok(import) => {
                        let before = scene.fixtures.len();
                        scene.import_mvr(import);
                        if let Some((c, r)) = scene.scene_frame() {
                            camera.frame(c, r * 1.15);
                        }
                        self.selection = Selection::default();
                        self.notify
                            .success(format!("Imported MVR · {} fixtures", scene.fixtures.len() - before));
                    }
                    Err(e) => self.notify.error(format!("Import MVR failed: {e}")),
                },
                _ => {}
            }
        }
    }

    /// The top menu bar (File / Edit / View / Fixture / Window / Help).
    // egui 0.34 deprecates `Panel::show(ctx)` mid-migration (the replacement
    // `show_inside` needs a Ui, not the root Context — DockArea uses the same
    // path); the ctx-based root panel is still correct here.
    #[allow(deprecated)]
    /// The workspace tab strip (Blender's workspace tabs / depence's preset row):
    /// one selectable tab per saved workspace + a "+" to save the current arrangement
    /// as a new workspace. Clicking a tab activates that workspace (layout + tool +
    /// overlay emphasis; NO locking). Reserved below the menu bar, above the dock.
    fn workspace_strip(&mut self, ctx: &egui::Context) {
        use theme::icon;
        let active = self.workspaces.active;
        let mut activate: Option<usize> = None;
        let mut save_new = false;
        egui::TopBottomPanel::top("workspace-strip").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                // Snapshot the names so the activate borrow doesn't overlap iteration.
                let names: Vec<String> = self.workspaces.items.iter().map(|w| w.name.clone()).collect();
                for (i, name) in names.into_iter().enumerate() {
                    #[allow(deprecated)] // egui 0.34 SelectableLabel — Button::selectable migration later
                    if ui.selectable_label(i == active, name).clicked() {
                        activate = Some(i);
                    }
                }
                ui.separator();
                if ui
                    .small_button(icon::ADD)
                    .on_hover_text("Save current arrangement as a new workspace")
                    .clicked()
                {
                    save_new = true;
                }
            });
        });
        if let Some(i) = activate {
            // Re-activating the current workspace is harmless (re-applies its saved
            // layout) — a cheap "reset this workspace" gesture.
            self.activate_workspace(i);
        }
        if save_new {
            self.save_workspace = Some(String::new());
        }
    }

    /// The "Save current as workspace…" modal: a name field + Save/Cancel. On Save
    /// it captures the live layout + tool + overlays under the typed name (overwriting
    /// a same-named record). Drawn after the dock in `show`.
    fn save_workspace_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.save_workspace.take() else { return };
        let mut open = true;
        let mut commit = false;
        let mut cancel = false;
        egui::Window::new("Save Workspace")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label("Workspace name");
                let resp = ui.text_edit_singleline(&mut name);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if commit && !name.trim().is_empty() {
            self.save_current_workspace(&name);
            // dialog closed (not re-stored)
        } else if cancel || !open {
            // dropped (not re-stored)
        } else {
            // Keep the dialog open with the in-progress name.
            self.save_workspace = Some(name);
        }
    }

    fn menu_bar(
        &mut self,
        ctx: &egui::Context,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        egui::TopBottomPanel::top("menu-bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                use theme::icon;
                ui.menu_button("File", |ui| {
                    // "New" re-opens the welcome chooser (its New Project button is what
                    // actually creates the blank scene) — see Action::New.
                    if ui.button(format!("{}  New Project", icon::SCENE)).clicked() {
                        self.show_welcome();
                        ui.close();
                    }
                    if ui.button(format!("{}  Open Project…", icon::IMPORT_MVR)).clicked() {
                        self.open_project_dialog(scene, camera, dmx);
                        ui.close();
                    }
                    if !self.recent.is_empty() {
                        let recent = self.recent.clone();
                        ui.menu_button(format!("{}  Open Recent", icon::PROFILE), |ui| {
                            for p in &recent {
                                let name = p
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                if ui.button(name).on_hover_text(p.display().to_string()).clicked() {
                                    self.open_project(p, scene, camera, dmx);
                                    ui.close();
                                }
                            }
                        });
                    }
                    ui.separator();
                    if ui.button(format!("{}  Save  (Ctrl+S)", icon::EXPORT)).clicked() {
                        self.save_project(scene, camera, dmx);
                        ui.close();
                    }
                    if ui.button(format!("{}  Save As…", icon::EXPORT)).clicked() {
                        self.save_project_as(scene, camera, dmx);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("{}  Import GDTF Fixture…", icon::IMPORT_GDTF)).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("GDTF", &["gdtf"]).pick_file() {
                            if let Ok(idx) = self.library.import_gdtf(&path) {
                                let arc = self.library.gdtf[idx].clone();
                                let f = scene.add_gdtf(arc, Vec3::new(0.0, 4.0, 0.0));
                                self.selection = Selection::fixture(f);
                            }
                        }
                        ui.close();
                    }
                    if ui.button(format!("{}  Import MVR Scene…", icon::IMPORT_MVR)).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("MVR", &["mvr"]).pick_file() {
                            if let Ok(import) = crate::mvr::MvrImport::load_path(&path) {
                                scene.import_mvr(import);
                                if let Some((c, r)) = scene.scene_frame() {
                                    camera.frame(c, r * 1.15);
                                }
                                self.selection = Selection::default();
                            }
                        }
                        ui.close();
                    }
                    let can_export = !scene.fixtures.is_empty() || !scene.geometry.is_empty();
                    if ui.add_enabled(can_export, egui::Button::new(format!("{}  Export MVR Scene…", icon::EXPORT))).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("MVR", &["mvr"]).set_file_name("scene.mvr").save_file() {
                            match crate::mvr::export_path(scene, &path) {
                                Ok(()) => self.notify.success("Exported MVR scene"),
                                Err(e) => self.notify.error(format!("Export MVR failed: {e}")),
                            }
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("{}  Preferences…", icon::SETTINGS)).clicked() {
                        self.show_prefs = true;
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    // Undo / Redo — labelled with the edit name, greyed at the ends.
                    let undo_label = match self.undo.undo_name() {
                        Some(n) => format!("{}  Undo {n}", icon::UNDO),
                        None => format!("{}  Undo", icon::UNDO),
                    };
                    if ui.add_enabled(self.undo.can_undo(), egui::Button::new(undo_label)).clicked() {
                        self.do_undo(scene, dmx);
                        ui.close();
                    }
                    let redo_label = match self.undo.redo_name() {
                        Some(n) => format!("{}  Redo {n}", icon::REDO),
                        None => format!("{}  Redo", icon::REDO),
                    };
                    if ui.add_enabled(self.undo.can_redo(), egui::Button::new(redo_label)).clicked() {
                        self.do_redo(scene, dmx);
                        ui.close();
                    }
                    ui.separator();
                    // Operator search (F3) — run any registered op by name.
                    if ui.button(format!("{}  Run Operator…  (F3)", icon::SEARCH)).clicked() {
                        self.op_search.show();
                        ui.close();
                    }
                    // Adjust last operation (F9) — re-invoke the last registered op
                    // (parameterized ops re-open their dialog to tweak + re-exec).
                    let adjust = self.undo.last_op().map(|l| l.label.clone());
                    let adjust_label = match &adjust {
                        Some(l) => format!("{}  Adjust Last: {l}  (F9)", icon::RESET),
                        None => format!("{}  Adjust Last Operation  (F9)", icon::RESET),
                    };
                    if ui
                        .add_enabled(adjust.is_some(), egui::Button::new(adjust_label))
                        .clicked()
                    {
                        self.adjust_last_op(ctx, scene, dmx);
                        ui.close();
                    }
                    ui.separator();
                    if ui.add_enabled(self.selection.primary_fixture().is_some(), egui::Button::new(format!("{}  Duplicate / Array…", icon::DUPLICATE))).clicked() {
                        if let Some(idx) = self.selection.primary_fixture() {
                            self.duplicate = Some(duplicate_dialog_for(ctx, idx));
                        }
                        ui.close();
                    }
                    if ui.add_enabled(!self.selection.fixtures.is_empty(), egui::Button::new(format!("{}  Delete Selected", icon::TRASH))).clicked() {
                        self.pending_delete = true; // committed after the dock (remaps patch/groups/cues)
                        ui.close();
                    }
                    ui.separator();
                    // Select All / Invert / None (#88) — route through dispatch_action
                    // so the menu, the keymap (A / Ctrl+I / Alt+A) and the F3 palette
                    // all share one effect. These act within the ACTIVE selection kind.
                    if ui.button(format!("{}  Select All  (A)", icon::TOOL_SELECT)).clicked() {
                        self.dispatch_action(ctx, shortcuts::Action::SelectAll, scene, camera, dmx);
                        ui.close();
                    }
                    if ui.button(format!("{}  Invert Selection  (Ctrl+I)", icon::TOOL_SELECT)).clicked() {
                        self.dispatch_action(ctx, shortcuts::Action::SelectInvert, scene, camera, dmx);
                        ui.close();
                    }
                    if ui.button(format!("{}  Deselect All  (Alt+A)", icon::DESELECT)).clicked() {
                        self.dispatch_action(ctx, shortcuts::Action::Deselect, scene, camera, dmx);
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.menu_button(format!("{}  Camera", icon::CAMERA), |ui| {
                        for v in CameraView::ALL {
                            if ui.button(v.label()).clicked() {
                                camera.set_view(v);
                                ui.close();
                            }
                        }
                    });
                    if ui.button(format!("{}  Frame Selection  (F)", icon::FRAME)).clicked() {
                        if let Some((lo, hi)) = self.frame_bounds(scene, true) {
                            camera.frame_aabb(lo, hi);
                        }
                        ui.close();
                    }
                    if ui.button(format!("{}  Frame All  (Shift+F)", icon::FRAME)).clicked() {
                        if let Some((lo, hi)) = self.frame_bounds(scene, false) {
                            camera.frame_aabb(lo, hi);
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("{}  Toggle Fullscreen  (F11)", icon::FULLSCREEN)).clicked() {
                        self.pending_fullscreen_toggle = true;
                        ui.close();
                    }
                    ui.separator();
                    // Blender-style Overlays submenu: a coherent set of quiet,
                    // toggleable viewport HUD bits, each backed by a registered
                    // command so they're also in the F3 palette / keymap. Grid lives
                    // in RenderSettings; the rest are prefs-bound overlays.
                    ui.menu_button(format!("{}  Overlays", icon::EYE), |ui| {
                        ui.checkbox(&mut self.prefs.show_stats, "Statistics");
                        ui.checkbox(&mut self.prefs.show_labels, "Fixture labels");
                        ui.checkbox(&mut self.settings.show_grid, "Grid + axes");
                        ui.checkbox(&mut self.prefs.show_gizmos, "Navigation gizmo");
                        ui.checkbox(&mut self.prefs.show_hint, "Transform hint line");
                        ui.separator();
                        ui.checkbox(&mut self.prefs.show_fps, "FPS overlay");
                    });
                });
                ui.menu_button("Fixture", |ui| {
                    if ui.button(format!("{}  Online Library…", icon::ONLINE)).clicked() {
                        self.show_share = true;
                        ui.close();
                    }
                    ui.separator();
                    let has = self.selection.primary_fixture().map(|i| i < scene.fixtures.len() && scene.fixtures[i].is_gdtf()).unwrap_or(false);
                    if ui.add_enabled(has, egui::Button::new(format!("{}  Edit Profile…", icon::PROFILE))).clicked() {
                        if let Some(i) = self.selection.primary_fixture() {
                            self.profile = Some(ProfileEditor::new(i));
                        }
                        ui.close();
                    }
                });
                ui.menu_button("Render", |ui| {
                    let rendering = self.render.status.phase == RenderPhase::Rendering;
                    if ui
                        .add_enabled(!rendering, egui::Button::new(format!("{}  Render Image  (F12)", icon::RENDER_GO)))
                        .clicked()
                    {
                        self.render.request_start();
                        self.ensure_render_tab_focused();
                        ui.close();
                    }
                    ui.add_enabled(false, egui::Button::new(format!("{}  Render Animation", icon::ANIMATION)))
                        .on_hover_text("Animation rendering — coming soon");
                    if rendering
                        && ui.button(format!("{}  Cancel Render", icon::RENDER_STOP)).clicked()
                    {
                        self.render.request_cancel();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("{}  Open Render View", icon::RENDER)).clicked() {
                        self.ensure_render_tab_focused();
                        ui.close();
                    }
                });
                ui.menu_button("Window", |ui| {
                    ui.menu_button(format!("{}  Workspace", icon::LAYOUT), |ui| {
                        // Switch to any saved workspace (built-in or user) — applies its
                        // layout + tool + overlay emphasis (no locking). The active one
                        // is checkmarked. A delete affordance for user workspaces.
                        let active = self.workspaces.active;
                        let mut activate: Option<usize> = None;
                        let mut delete: Option<usize> = None;
                        let rows: Vec<(usize, String, bool)> = self
                            .workspaces
                            .items
                            .iter()
                            .enumerate()
                            .map(|(i, w)| (i, w.name.clone(), w.builtin))
                            .collect();
                        for (i, name, builtin) in rows {
                            ui.horizontal(|ui| {
                                let mark = if i == active { "●  " } else { "    " };
                                if ui.button(format!("{mark}{name}")).clicked() {
                                    activate = Some(i);
                                }
                                if !builtin
                                    && ui.small_button(icon::TRASH).on_hover_text("Delete workspace").clicked()
                                {
                                    delete = Some(i);
                                }
                            });
                        }
                        ui.separator();
                        if ui.button(format!("{}  Save Current as Workspace…", icon::EXPORT)).clicked() {
                            // Seed the name buffer with the active workspace's name.
                            self.save_workspace = Some(self.workspaces.active().name.clone());
                            ui.close();
                        }
                        if let Some(i) = activate {
                            self.activate_workspace(i);
                            ui.close();
                        }
                        if let Some(i) = delete {
                            self.workspaces.delete(i);
                        }
                    });
                    // View bookmarks strip (P1 #34): save the current shot to the next
                    // free numbered slot; recall (eased) or delete a saved slot.
                    ui.menu_button(format!("{}  View Bookmarks", icon::CAMERA), |ui| {
                        let full = self.bookmarks.next_free_slot().is_none();
                        if ui
                            .add_enabled(!full, egui::Button::new(format!("{}  Save Current View", icon::FRAME)))
                            .clicked()
                        {
                            if let Some(slot) = self.bookmarks.save_pose(camera.pose()) {
                                self.notify.success(format!("Saved view bookmark {slot}"));
                            }
                            ui.close();
                        }
                        if self.bookmarks.items.is_empty() {
                            ui.separator();
                            ui.label(egui::RichText::new("No saved views").small().weak());
                        } else {
                            ui.separator();
                            // Snapshot the slots so the recall/delete borrow doesn't
                            // overlap the iteration over `self.bookmarks.items`.
                            let rows: Vec<(usize, String)> =
                                self.bookmarks.items.iter().map(|b| (b.slot, b.name.clone())).collect();
                            for (slot, name) in rows {
                                ui.horizontal(|ui| {
                                    if ui.button(format!("{slot}.  {name}")).clicked() {
                                        if let Some(pose) = self.bookmarks.pose_in_slot(slot) {
                                            camera.apply_pose(&pose);
                                        }
                                        ui.close();
                                    }
                                    if ui.small_button(icon::TRASH).on_hover_text("Delete bookmark").clicked() {
                                        self.bookmarks.delete_slot(slot);
                                    }
                                });
                            }
                        }
                    });
                    ui.separator();
                    ui.checkbox(&mut self.show_perf, format!("{}  Performance", icon::PERF));
                    ui.checkbox(&mut self.show_report_log, format!("{}  Report Log", icon::LOG));
                    if ui.button(format!("{}  Welcome", icon::INFO)).clicked() {
                        self.show_welcome();
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Panels").small().weak());
                    for tab in Tab::TOGGLEABLE {
                        let mut open = self.is_tab_open(tab);
                        if ui.checkbox(&mut open, format!("{}  {}", tab.icon(), tab.title())).changed() {
                            self.toggle_tab(tab);
                        }
                    }
                    ui.separator();
                    if ui.button(format!("{}  Reset Layout", icon::LAYOUT)).clicked() {
                        // Re-apply the active workspace's saved layout (revert any live
                        // panel dragging) without changing its tool/overlay emphasis.
                        let dock = self.default_dock();
                        self.dock = dock;
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button(format!("{}  Keyboard Shortcuts", icon::KEYBOARD)).clicked() {
                        self.show_shortcuts = true;
                        ui.close();
                    }
                    if ui.button(format!("{}  About previz", icon::INFO)).clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    /// The bottom status bar (selection · units · DMX · fixtures · FPS).
    #[allow(deprecated)]
    fn status_bar(
        &mut self,
        ctx: &egui::Context,
        scene: &Scene,
        dmx: &crate::dmx::DmxIo,
        fps: f32,
    ) {
        egui::TopBottomPanel::bottom("status-bar").show(ctx, |ui| {
            let pal = theme::Palette::get(ui);
            ui.horizontal(|ui| {
                // A small log glyph opens the report-log history; placed first so
                // it's a stable affordance regardless of which message is showing.
                if ui
                    .add(egui::Button::new(theme::icon::LOG).frame(false))
                    .on_hover_text("Report log")
                    .clicked()
                {
                    self.show_report_log = !self.show_report_log;
                }
                // Left slot precedence (Unreal `PushStatusBarMessage` model): a
                // pushed transient message (top of the handle stack) MASKS the
                // selection summary; otherwise the selection summary shows. The grey
                // passive hint is appended after, separated by a thin rule. Clicking
                // the message area also opens the log (the whole left slot is the
                // affordance, matching Blender's clickable info line).
                let msg_resp = match self.status_msgs.top() {
                    Some(msg) => ui.colored_label(pal.accent, msg),
                    None => {
                        let sel = match self.selection.fixtures.len() {
                            0 => "nothing selected".to_string(),
                            1 => scene
                                .fixtures
                                .get(self.selection.fixtures[0])
                                .map(|f| format!("{} · {}", f.name, f.profile))
                                .unwrap_or_default(),
                            n => format!("{n} fixtures selected"),
                        };
                        ui.label(sel)
                    }
                };
                if msg_resp
                    .on_hover_text("Open report log")
                    .interact(egui::Sense::click())
                    .clicked()
                {
                    self.show_report_log = true;
                }
                if let Some(hint) = self.status_msgs.hint() {
                    ui.separator();
                    ui.colored_label(pal.ink_tertiary, hint);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{fps:.0} fps"));
                    ui.separator();
                    ui.label(format!("{} fixtures", scene.fixtures.len()));
                    ui.separator();
                    let (dot, txt) = if dmx.is_running() {
                        (egui::Color32::from_rgb(120, 210, 120), "DMX live")
                    } else {
                        (egui::Color32::from_gray(120), "DMX off")
                    };
                    // Painter-drawn status dot: a small inline cell + filled circle,
                    // a breathing gap, then the label — the bundled fonts have no
                    // round glyph, and a naked "•" sits too tight against the words.
                    status_dot(ui, dot, txt);
                    ui.separator();
                    ui.label(if self.prefs.units_feet { "ft" } else { "m" });
                });
            });
        });
    }
}

/// The `object.delete` operator body: remove every selected fixture / geometry /
/// screen, remapping every index-keyed structure (patch entries, cue look lists,
/// selection groups) in lock-step so nothing is silently corrupted. Edits the
/// shared [`op::OpCtx`] surface so it can run through [`Ui::run_op`]. Returns
/// `Finished` if anything was removed, `Cancelled` otherwise (so no undo step is
/// pushed for an empty delete).
fn delete_selection(cx: &mut op::OpCtx) -> op::OpStatus {
    // --- fixtures: remove + remap every index-keyed structure in lock-step ---
    let mut removed: Vec<usize> =
        cx.selection.fixtures.iter().copied().filter(|&i| i < cx.scene.fixtures.len()).collect();
    removed.sort_unstable();
    removed.dedup();
    if !removed.is_empty() {
        // Descending so earlier indices stay valid as we remove.
        for &i in removed.iter().rev() {
            cx.scene.fixtures.remove(i);
            cx.patch.remove_at(i);
            cx.cues.remove_fixture(i);
        }
        // Groups store arbitrary index references: remap each through the
        // shift, dropping deleted members, then drop any group left empty.
        for g in cx.groups.iter_mut() {
            g.fixtures = g.fixtures.iter().filter_map(|&idx| remap_index(idx, &removed)).collect();
        }
        cx.groups.retain(|g| !g.fixtures.is_empty());
    }
    // --- static geometry: a plain removal (no patch/cue/group keyed by it) ---
    let mut geo: Vec<usize> =
        cx.selection.geometry.iter().copied().filter(|&i| i < cx.scene.geometry.len()).collect();
    geo.sort_unstable();
    geo.dedup();
    for &i in geo.iter().rev() {
        cx.scene.geometry.remove(i);
    }
    // --- LED screens: a plain removal (no patch/cue/group keyed by it) ---
    let mut scr: Vec<usize> =
        cx.selection.screens.iter().copied().filter(|&i| i < cx.scene.screens.len()).collect();
    scr.sort_unstable();
    scr.dedup();
    for &i in scr.iter().rev() {
        cx.scene.screens.remove(i);
    }
    if !removed.is_empty() || !geo.is_empty() || !scr.is_empty() {
        *cx.selection = Selection::default();
        op::OpStatus::Finished
    } else {
        op::OpStatus::Cancelled
    }
}

/// The modal Duplicate dialog: array the selected fixture by offset + Y angle.
/// A sensible default start slot for the Patch dialog: the channel just past the
/// last enabled patch entry (so a new batch packs onto the end of the rig),
/// wrapping to the next universe at 512. Falls back to universe 1 / address 1 on
/// an empty patch. `assign_indices` then finds the first non-clashing slot from
/// here, so this only needs to be a good starting guess.
fn next_free_slot(patch: &mut crate::dmx::PatchTable, scene: &Scene) -> (u16, u16) {
    patch.sync(scene);
    let mut best: Option<(u16, u16)> = None; // (universe, address-just-past-end)
    for i in 0..scene.fixtures.len() {
        if let Some(p) = patch.get(i).filter(|p| p.enabled) {
            let end = p.address + p.footprint.max(1); // first free channel after it
            let cand = (p.universe, end);
            if best.is_none_or(|b| cand > b) {
                best = Some(cand);
            }
        }
    }
    match best {
        Some((u, a)) if a <= 512 => (u, a),
        Some((u, _)) => (u + 1, 1),
        None => (1, 1),
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
                        panels::source_chip(ui, src);
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

impl Default for Ui {
    fn default() -> Self {
        Self::new()
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
    duplicate: &'a mut Option<DuplicateDialog>,
    profile: &'a mut Option<ProfileEditor>,
    lib: &'a mut panels::LibState,
    scene_anchor: &'a mut Option<usize>,
    scene_sort: &'a mut panels::SceneSort,
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
    inspector_state: &'a mut panels::InspectorState,
    groups: &'a mut Vec<SelectionGroup>,
    group_name: &'a mut String,
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
            Tab::Scene => panels::scene_outliner(
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
                self.groups,
                self.group_name,
            ),
            Tab::Library => panels::library_browser(
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
                    panels::inspector(ui, self.scene, self.selection, self.dmx_patch, self.gdtf_textures, self.profile, self.screen_sources, self.inspector_state, &mut e, self.render, self.settings);
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
        // only the genuinely WIDE data tables (the 512-channel DMX grid + the Fixtures
        // schedule) keep horizontal scroll.
        match tab {
            Tab::Viewport | Tab::Render => [false, false], // draw their own image
            Tab::DmxMonitor | Tab::Patch => [true, true], // wide tables
            _ => [false, true], // fit width, vertical scroll only
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmx::DmxIo;
    use crate::scene::Scene;

    /// N (ToggleNPanel) hides the Inspector, then re-opens it as its OWN side pane —
    /// NOT merged as a tab into the viewport's leaf (the reported bug: it reopened
    /// full-screen-as-a-tab over the viewport instead of returning to the sidebar).
    #[test]
    fn n_panel_reopens_as_its_own_pane_not_a_viewport_tab() {
        let mut ui = Ui::new();
        assert!(ui.dock.find_tab(&Tab::Inspector).is_some(), "default has an Inspector");
        ui.toggle_tab(Tab::Inspector); // hide
        assert!(ui.dock.find_tab(&Tab::Inspector).is_none(), "N hides it");
        ui.toggle_tab(Tab::Inspector); // re-open
        let insp = ui.dock.find_tab(&Tab::Inspector).expect("N re-opens the Inspector");
        let vp = ui.dock.find_tab(&Tab::Viewport).expect("viewport present");
        assert_ne!(
            (insp.surface, insp.node),
            (vp.surface, vp.node),
            "Inspector must reopen in its OWN leaf, not the viewport's tab strip",
        );
    }

    /// F9 "Adjust Last Operation" must REPLACE the last op's result, not stack a
    /// second one (the Phase-1 review's must-fix). Run a Direct op (unpatch), then
    /// adjust-last: the prior step is undone and the op re-run, so exactly ONE undo
    /// step remains. With the bug (no undo before re-exec) two would, and one undo
    /// would leave the stack still non-empty.
    #[test]
    fn adjust_last_replaces_not_stacks() {
        let ctx = egui::Context::default();
        let mut ui = Ui::new();
        let mut scene = Scene::default(); // demo scene: one fixture
        let mut dmx = DmxIo::new();
        ui.selection = Selection::fixture(0); // so unpatch's poll passes

        // Run the Direct unpatch op once → one undo step named "Unpatch".
        ui.run_catalog_op(&ctx, "fixture.unpatch", &mut scene, &mut dmx);
        assert!(ui.undo.can_undo());
        assert_eq!(ui.undo.undo_name(), Some("Unpatch"));

        // Adjust-last (F9): undo the prior result, then re-run → still ONE step.
        ui.adjust_last_op(&ctx, &mut scene, &mut dmx);
        assert!(ui.undo.can_undo(), "the replacement op is undoable");

        // Undo the single step → no further undo (proves replace, not stack).
        ui.do_undo(&mut scene, &mut dmx);
        assert!(!ui.undo.can_undo(), "adjust-last must replace, not stack a 2nd unpatch");
    }

    /// S1 parity: `dispatch_action` is the SINGLE source of truth for every
    /// non-viewport-modal Action. This pins that each Action previously handled in
    /// the two `show()` match sites still routes to its observable effect through
    /// the unified entry point — and that the viewport-owned ones (G/R/S transform,
    /// X/Y/Z axis lock, Duplicate) report unhandled (so the palette never offers
    /// them as dead picks). If a future edit drops or mis-wires an arm, this fails.
    #[test]
    fn dispatch_action_routes_every_global_action() {
        use crate::renderer::camera::CameraView;
        use shortcuts::{Action, Dir};
        let ctx = egui::Context::default();
        let mut cam = OrbitCamera::default();

        // Helper: fresh Ui + demo scene (one fixture) + dmx, dispatch ONE action.
        let run = |a: Action, prep: &dyn Fn(&mut Ui)| -> (Ui, Scene, DmxIo, bool) {
            let mut ui = Ui::new();
            let mut scene = Scene::default();
            let mut dmx = DmxIo::new();
            prep(&mut ui);
            let mut cam = OrbitCamera::default();
            let handled = ui.dispatch_action(&ctx, a, &mut scene, &mut cam, &mut dmx);
            (ui, scene, dmx, handled)
        };
        let sel = |ui: &mut Ui| ui.selection = Selection::fixture(0);
        let noop: &dyn Fn(&mut Ui) = &|_: &mut Ui| {};

        // --- View / framing: handled, and the camera mutators reach the camera. ---
        cam.ortho = false;
        assert!(Ui::new().dispatch_action(&ctx, Action::ToggleOrtho, &mut Scene::default(), &mut cam, &mut DmxIo::new()));
        assert!(cam.ortho, "ToggleOrtho flips the camera projection");
        assert!(Ui::new().dispatch_action(&ctx, Action::View(CameraView::Front), &mut Scene::default(), &mut cam, &mut DmxIo::new()));
        assert!(Ui::new().dispatch_action(&ctx, Action::OrbitStep(15.0, 0.0), &mut Scene::default(), &mut cam, &mut DmxIo::new()));
        assert!(Ui::new().dispatch_action(&ctx, Action::FrameSelection, &mut Scene::default(), &mut cam, &mut DmxIo::new()));
        assert!(Ui::new().dispatch_action(&ctx, Action::FrameAll, &mut Scene::default(), &mut cam, &mut DmxIo::new()));
        assert!(Ui::new().dispatch_action(&ctx, Action::ViewCamera, &mut Scene::default(), &mut cam, &mut DmxIo::new()));

        // ToggleLabels flips the pref; ViewPie opens the pie; N/T toggle regions.
        let (ui, _, _, h) = run(Action::ToggleLabels, noop);
        assert!(h && ui.prefs.show_labels != Preferences::default().show_labels);
        // Overlay toggles (View > Overlays) flip their backing flag.
        let (ui, _, _, h) = run(Action::ToggleStats, noop);
        assert!(h && ui.prefs.show_stats != Preferences::default().show_stats);
        let (ui, _, _, h) = run(Action::ToggleGrid, noop);
        assert!(h && !ui.settings.show_grid, "ToggleGrid flips the render-settings grid flag");
        let (ui, _, _, h) = run(Action::ToggleGizmos, noop);
        assert!(h && ui.prefs.show_gizmos != Preferences::default().show_gizmos);
        let (ui, _, _, h) = run(Action::ToggleHint, noop);
        assert!(h && ui.prefs.show_hint != Preferences::default().show_hint);
        let (ui, _, _, h) = run(Action::ViewPie, noop);
        assert!(h && ui.view_pie.open, "ViewPie opens the radial pie");
        let (ui, _, _, h) = run(Action::ShadingPie, noop);
        assert!(h && ui.shading_pie.open, "ShadingPie opens the radial shading pie");
        let (_ui, _, _, h) = run(Action::ToggleNPanel, noop);
        assert!(h, "ToggleNPanel handled (toggles the docked Inspector tab)");
        let (ui, _, _, h) = run(Action::ToggleTPanel, noop);
        assert!(h && ui.viewport_regions.t_open);

        // --- Selection ---
        let (ui, _, _, h) = run(Action::QuickSelect, noop);
        assert!(h && ui.quick_select, "QuickSelect arms the menu");
        let (ui, sc, _, h) = run(Action::SelectAll, noop);
        assert!(h && ui.selection.fixtures == (0..sc.fixtures.len()).collect::<Vec<_>>(), "SelectAll selects every fixture (active kind)");
        // Deselect (#88: Alt+A / Esc) now clears the selection.
        let (ui, _, _, h) = run(Action::Deselect, &sel);
        assert!(h && ui.selection == Selection::default(), "Deselect clears the selection");
        // Invert (#88) within the active kind flips membership: from {0} it must
        // include everything BUT 0; on the single-fixture demo that's empty.
        let (ui, sc, _, h) = run(Action::SelectInvert, &sel);
        let want: Vec<usize> = (0..sc.fixtures.len()).filter(|&i| i != 0).collect();
        assert!(h && ui.selection.fixtures == want, "Invert flips membership within the active kind");
        // Invert from empty selects all (active kind defaults to fixtures).
        let clear: &dyn Fn(&mut Ui) = &|ui: &mut Ui| ui.selection = Selection::default();
        let (ui, sc, _, h) = run(Action::SelectInvert, clear);
        assert!(h && ui.selection.fixtures == (0..sc.fixtures.len()).collect::<Vec<_>>(), "Invert from empty selects all");
        let (ui, _, _, h) = run(Action::Replace, &sel);
        assert!(h && ui.replace.is_some(), "Replace opens its dialog with a selection");

        // --- Object: Delete defers to pending_delete; Add opens the menu. ---
        let (ui, _, _, h) = run(Action::Delete, &sel);
        assert!(h && ui.pending_delete, "Delete defers to the after-dock commit");
        let (ui, _, _, h) = run(Action::AddMenu, noop);
        assert!(h && ui.add_menu.open, "AddMenu opens the cursor menu");

        // Nudge accumulates into pending_nudge only when the viewport drives it.
        let (ui, _, _, h) = run(Action::Nudge(Dir::XPos, 0.1), &|ui: &mut Ui| {
            ui.selection = Selection::fixture(0);
            ui.viewport_focused = true; // nudge_ok needs viewport focus + selection
        });
        assert!(h && ui.pending_nudge.x > 0.0, "Nudge accumulates into pending_nudge");
        // Without viewport focus, nudge is a guarded no-op (still handled).
        let (ui, _, _, h) = run(Action::Nudge(Dir::XPos, 0.1), &sel);
        assert!(h && ui.pending_nudge == Vec3::ZERO, "Nudge guarded off without viewport focus");

        // --- Patch / Unpatch open their dialogs (commit defers after the dock). ---
        let (ui, _, _, h) = run(Action::Patch, &sel);
        assert!(h && ui.patch_dialog.open, "Patch opens its dialog with a selection");
        let (ui, _, _, h) = run(Action::Unpatch, &sel);
        assert!(h && ui.unpatch_dialog.open, "Unpatch opens its dialog with a selection");
        // Empty selection: P/U are guarded no-ops (still reported handled).
        let clear = &|ui: &mut Ui| ui.selection = Selection::default();
        let (ui, _, _, h) = run(Action::Patch, clear);
        assert!(h && !ui.patch_dialog.open, "Patch guarded off with no selection");

        // --- 3D cursor: snap to selection moves + marks set; reset zeroes + clears. ---
        // The demo `Scene::default()` has fixtures, so fixture 0 resolves → the cursor
        // snaps to its position and is marked set.
        let (ui, scene, _, h) = run(Action::SnapCursorToSelection, &|ui: &mut Ui| {
            ui.selection = Selection::fixture(0);
        });
        assert!(h && ui.cursor_3d_set, "SnapCursorToSelection moves the cursor + marks set");
        assert_eq!(ui.cursor_3d, scene.fixtures[0].position, "cursor snaps to the fixture");
        // No addressable selection → the median is None, so the cursor stays unset.
        let (ui, _, _, h) = run(Action::SnapCursorToSelection, &|ui: &mut Ui| {
            ui.selection = Selection::default();
        });
        assert!(h && !ui.cursor_3d_set, "empty selection → cursor stays unset");
        let (ui, _, _, h) = run(Action::ResetCursor, &|ui: &mut Ui| {
            ui.cursor_3d = Vec3::new(3.0, 4.0, 5.0);
            ui.cursor_3d_set = true;
        });
        assert!(h && ui.cursor_3d == Vec3::ZERO && !ui.cursor_3d_set, "ResetCursor zeroes + clears");

        // --- Edit / history + App / file all reach their effect (handled). ---
        let (ui, _, _, h) = run(Action::OperatorSearch, noop);
        assert!(h && ui.op_search.open, "OperatorSearch opens the palette");
        let (ui, _, _, h) = run(Action::Preferences, noop);
        assert!(h && ui.show_prefs, "Preferences opens the prefs window");
        // Undo/Redo/AdjustLast/New are handled (their effect needs IO / a file
        // picker — just assert they route, not no-op-fall-through). Save/SaveAs/Open
        // are omitted here: they spawn a native file dialog (no headless effect).
        assert!(run(Action::Undo, noop).3, "Undo routes through dispatch_action");
        assert!(run(Action::Redo, noop).3, "Redo routes through dispatch_action");
        assert!(run(Action::AdjustLast, noop).3, "AdjustLast routes through dispatch_action");
        assert!(run(Action::New, noop).3, "New routes through dispatch_action");

        // --- View bookmarks (P1 #34): save fills a slot; recall eases the camera. ---
        let (ui, _, _, h) = run(Action::SaveBookmark, noop);
        assert!(h && ui.bookmarks.pose_in_slot(1).is_some(), "SaveBookmark fills slot 1");
        // Recall an empty slot is a handled no-op (just a notify); recall a saved one
        // routes (the camera mutation is exercised by the camera-side pose tests).
        assert!(run(Action::RecallBookmark(1), noop).3, "RecallBookmark routes (empty slot)");

        // --- Viewport-owned: NOT handled here (dispatched in panels::viewport). ---
        assert!(!run(Action::Transform(TransformKind::Move), noop).3, "Transform is viewport-owned");
        assert!(!run(Action::AxisLock(Axis::X), noop).3, "AxisLock is viewport-owned");
        assert!(!run(Action::Duplicate, noop).3, "Duplicate is viewport-owned");
    }

    /// The Z Shading pie lays out exactly the three display modes + the Grid /
    /// Stats toggles, and each sector's value applies the effect it advertises.
    #[test]
    fn shading_pie_sectors_map_to_modes_and_toggles() {
        let choices = shading_pie_choices();
        // Every display mode is offered exactly once, in ViewportMode::ALL order.
        let modes: Vec<ViewportMode> = choices
            .iter()
            .filter_map(|c| match c.value {
                ShadingChoice::Mode(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(modes, ViewportMode::ALL.to_vec(), "all three modes, once each");
        // Both quick toggles are present.
        assert!(choices.iter().any(|c| c.value == ShadingChoice::ToggleGrid), "Grid toggle present");
        assert!(choices.iter().any(|c| c.value == ShadingChoice::ToggleStats), "Stats toggle present");
        // No sector is unlabelled.
        assert!(choices.iter().all(|c| !c.label.is_empty()), "every sector is labelled");

        // Applying a Mode choice sets settings.mode; the toggles flip their flag.
        let mut ui = Ui::new();
        ui.settings.mode = ViewportMode::Beauty;
        ui.apply_shading_choice(ShadingChoice::Mode(ViewportMode::Wireframe));
        assert_eq!(ui.settings.mode, ViewportMode::Wireframe, "Mode sector switches display mode");

        let grid0 = ui.settings.show_grid;
        ui.apply_shading_choice(ShadingChoice::ToggleGrid);
        assert_ne!(ui.settings.show_grid, grid0, "Grid sector flips the grid flag");

        let stats0 = ui.prefs.show_stats;
        ui.apply_shading_choice(ShadingChoice::ToggleStats);
        assert_ne!(ui.prefs.show_stats, stats0, "Stats sector flips the stats overlay flag");
    }

    /// The generic `pie::choose` helper returns the picked sector's VALUE (not its
    /// index), moved out by index — the seam the Z pie relies on. (Drives the
    /// mapping logic without an egui frame by checking the choice list is non-empty
    /// and each value is what the layout fn declared; the angle→index resolution is
    /// covered by pie.rs's `sector_at` tests.)
    #[test]
    fn shading_pie_choices_are_stable_order() {
        let choices = shading_pie_choices();
        assert_eq!(choices.len(), 5, "3 modes + Grid + Stats");
        assert_eq!(choices[0].value, ShadingChoice::Mode(ViewportMode::Beauty));
    }

    #[test]
    fn remap_index_after_delete() {
        // Delete fixtures 1 and 3 from a 5-fixture scene (0..5).
        let removed = [1usize, 3];
        assert_eq!(remap_index(0, &removed), Some(0));
        assert_eq!(remap_index(1, &removed), None); // deleted
        assert_eq!(remap_index(2, &removed), Some(1)); // one below removed
        assert_eq!(remap_index(3, &removed), None); // deleted
        assert_eq!(remap_index(4, &removed), Some(2)); // two below removed
    }

    /// Extract one GDTF archive's bytes from the bundled Basic Festival MVR.
    fn gdtf_bytes(member: &str) -> Option<Vec<u8>> {
        let candidates = [
            format!("{}/.context/attachments/05W1Dh/Basic Festival.mvr", env!("CARGO_MANIFEST_DIR")),
            format!("{}/Downloads/Basic Festival/Basic Festival.mvr", std::env::var("HOME").unwrap_or_default()),
        ];
        for path in candidates {
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else { continue };
            let Ok(mut f) = zip.by_name(member) else { continue };
            let mut buf = Vec::new();
            if std::io::Read::read_to_end(&mut f, &mut buf).is_ok() {
                return Some(buf);
            }
        }
        None
    }

    /// Round-trip a real GDTF fixture through `.archie`: save bundles the archive
    /// bytes, open re-parses + re-links them, and per-fixture state (cells, beam,
    /// dimmer) plus the camera survive the trip.
    #[test]
    fn archie_save_load_relinks_gdtf_and_state() {
        let member = "Astera LED Technology@AX2-100 PixelBar.gdtf";
        let Some(bytes) = gdtf_bytes(member) else {
            eprintln!("skip archie_save_load: Basic Festival.mvr not found");
            return;
        };
        let mut g = crate::gdtf::GdtfFixture::load_bytes(&bytes).expect("parse gdtf");
        g.spec = member.to_string();
        g.raw = Some(Arc::new(bytes));

        let ui = Ui::new();
        let mut scene = Scene::default();
        let base = scene.fixtures.len();
        let idx = scene.add_gdtf(Arc::new(g), Vec3::new(1.0, 4.0, -2.0));
        scene.fixtures[idx].beam = 0.5;
        scene.fixtures[idx].optics.dimmer = 0.8;
        let cells = scene.fixtures[idx].cells.len();
        let mut camera = OrbitCamera::default();
        camera.distance = 21.0;
        let dmx = DmxIo::new();

        let path = std::env::temp_dir().join("previz-archie-relink-test.archie");
        ui.write_project(&path, &scene, &camera, &dmx).expect("write project");

        // Open into a completely fresh app state.
        let project = project::read(&path).expect("read project");
        let _ = std::fs::remove_file(&path);
        let mut ui2 = Ui::new();
        let mut scene2 = Scene::default();
        let mut camera2 = OrbitCamera::default();
        let mut dmx2 = DmxIo::new();
        ui2.apply_project(project, &mut scene2, &mut camera2, &mut dmx2);

        assert_eq!(scene2.fixtures.len(), base + 1);
        let f = &scene2.fixtures[idx];
        let linked = f.gdtf.as_ref().expect("gdtf re-linked");
        assert_eq!(linked.spec, member);
        assert!(linked.raw.is_some(), "archive bytes restored for re-save");
        assert_eq!(f.cells.len(), cells, "per-cell colours preserved");
        assert!((f.beam - 0.5).abs() < 1e-6, "beam intensity round-trips");
        assert!((f.optics.dimmer - 0.8).abs() < 1e-6, "dimmer round-trips");
        assert!((camera2.distance - 21.0).abs() < 1e-6, "camera round-trips");
    }
}
