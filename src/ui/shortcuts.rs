//! The keyboard-shortcut registry — the SINGLE source of truth for every bind.
//!
//! Unified command registry (C1, #9 / #14): every user-triggerable command lives
//! once in [`COMMANDS`] as a [`Command`] (id + label + category + the [`Action`]
//! the keymap dispatches + an optional [`OpInvoke`] marking it a catalog op). The
//! keymaps bind *triggers to command ids* (not directly to an [`Action`]); the F3
//! palette + menus read the catalog projection ([`super::op::catalog`]); the
//! cheat-sheet reads the keymaps' resolved labels/categories. One table feeds all
//! four, so a key, a menu entry and a help row can never drift apart. [`Action`]
//! is kept as the thin keymap-dispatch shim (a full removal would touch every
//! `handle_shortcuts` arm + the viewport's modal block — out of scope for C1).
//!
//! Keymap-v2: the binds are organised into context-stacked **keymaps**
//! ([`KeyMap`] / [`Kmi`]) keyed by [`KeymapId`] (Global, Viewport, Modal).
//! Dispatch gathers the active keymaps **most-specific-first** and takes the
//! first match per physical trigger — so a Viewport bind (e.g. `S` = Scale)
//! cleanly MASKS a Global bind on the same key (`S` = quick-select) whenever the
//! viewport is focused, replacing the old ad-hoc `s_is_scale` guard. The modal
//! transform map ([`KeymapId::Modal`]) is consulted via [`poll_modal`] only while
//! a G/R/S op owns the viewport.
//!
//! Behaviour (in `handle_shortcuts` / `Ui::show` / `panels::viewport`) and the
//! cheat-sheet window (`windows/shortcuts.rs`) both read these keymaps, so a key
//! can never drift between what the app does and what the help screen advertises.
//! The *implementation* of a few viewport-owned actions (G/R/S transforms,
//! X/Y/Z axis lock) still lives in `panels::viewport` because it owns the live
//! mouse state — but those binds are REGISTERED here so they appear in the sheet.

use crate::renderer::camera::CameraView;
use crate::ui::{Axis, TransformKind};

/// Every distinct user-triggerable command. A handful (Transform/AxisLock) are
/// registered for the cheat sheet but dispatched inside `panels::viewport`.
#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    // View / framing.
    FrameSelection,
    FrameAll,
    View(CameraView),
    /// Numpad-0 "camera view" — registered for the cheat sheet; no-op for now.
    ViewCamera,
    /// numpad-5 — pure persp↔ortho toggle (no angle change).
    ToggleOrtho,
    /// numpad 2/4/6/8 — orbit by a fixed step (yaw_deg, pitch_deg).
    OrbitStep(f32, f32),
    /// `~` (backtick) — open the radial View pie at the cursor. Wired in S3.
    ViewPie,
    ToggleLabels,
    // Selection.
    SelectAll,
    Deselect,
    QuickSelect,
    Replace,
    // Object / transform.
    Delete,
    Duplicate,
    Nudge(Dir, f32),
    /// G/R/S — implementation lives in `panels::viewport`; registered here.
    Transform(TransformKind),
    /// X/Y/Z axis lock during a modal transform — impl in `panels::viewport`.
    AxisLock(Axis),
    AddMenu,
    Patch,
    Unpatch,
    /// N — toggle the viewport N-panel (Item/Transform sidebar). Impl in `show()`.
    ToggleNPanel,
    /// T — toggle the viewport T-panel (tool rail shell). Impl in `show()`.
    ToggleTPanel,
    // Edit / history.
    Undo,
    Redo,
    /// F3 / Space — open the operator-search palette (run any registered op by name).
    OperatorSearch,
    /// F9 — re-invoke the last registered op (Blender's "adjust last operation").
    AdjustLast,
    // App / file.
    Preferences,
    Save,
    SaveAs,
    Open,
    New,
}

/// Nudge directions (floor plane + height). The `f32` in [`Action::Nudge`] is the
/// step in metres (plain = 0.1, Shift = 1.0).
#[derive(Clone, Copy, PartialEq)]
pub enum Dir {
    XNeg,
    XPos,
    ZNeg,
    ZPos,
    YUp,
    YDown,
}

/// Tri-state modifier requirement: `Some(true)` = must be held, `Some(false)` =
/// must be released, `None` = don't-care. Every current bind pins the modifiers
/// it cares about and leaves the rest "off" (so plain `S` stays distinct from
/// `Cmd+S`), which is exactly the old exact-match behaviour — but the tri-state
/// gives keymap-v2 the room to grow "any-modifier" binds later without a rewrite.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Mods {
    pub command: Option<bool>,
    pub shift: Option<bool>,
    pub alt: Option<bool>,
}

impl Mods {
    /// The default the table uses: every modifier must be RELEASED. `.cmd()` /
    /// `.shift()` / `.alt()` flip individual requirements to "held".
    const fn none() -> Self {
        Self { command: Some(false), shift: Some(false), alt: Some(false) }
    }
    const fn cmd(mut self) -> Self {
        self.command = Some(true);
        self
    }
    const fn shift(mut self) -> Self {
        self.shift = Some(true);
        self
    }
    /// Does an egui modifier state satisfy this requirement?
    fn matches(&self, m: &egui::Modifiers) -> bool {
        self.command.is_none_or(|w| m.command == w)
            && self.shift.is_none_or(|w| m.shift == w)
            && self.alt.is_none_or(|w| m.alt == w)
    }
}

/// The kind of input event a [`Trigger`] fires on. Only `Press` is wired today
/// (every keyboard bind is a key-press); `Release`/`Click`/`Drag` round out the
/// vocabulary the modal map + future mouse binds will use (the modal confirm
/// already distinguishes click/drag, but at the call site, not via this enum).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press,
    #[allow(dead_code)] // vocabulary for release-triggered binds (none yet).
    Release,
    #[allow(dead_code)] // mouse click bind (modal confirm uses it at call site).
    Click,
    #[allow(dead_code)] // mouse drag bind (gizmo drag uses it at call site).
    Drag,
}

/// A physical input + the exact modifier set required. `command` maps to Cmd on
/// macOS and Ctrl elsewhere (egui already folds these into `modifiers.command`).
#[derive(Clone, Copy)]
pub struct Trigger {
    pub key: egui::Key,
    pub mods: Mods,
    pub event: Event,
}

impl Trigger {
    const fn key(key: egui::Key) -> Self {
        Self { key, mods: Mods::none(), event: Event::Press }
    }
    const fn shift(mut self) -> Self {
        self.mods = self.mods.shift();
        self
    }
    const fn cmd(mut self) -> Self {
        self.mods = self.mods.cmd();
        self
    }
    /// Did this trigger fire this frame? (`Press` reads `key_pressed`; the other
    /// events are placeholders matched at their call sites for now.)
    fn fired(&self, i: &egui::InputState) -> bool {
        let key_ok = match self.event {
            Event::Press => i.key_pressed(self.key),
            Event::Release => i.key_released(self.key),
            // Mouse events aren't keyboard-pollable here; never auto-fire.
            Event::Click | Event::Drag => false,
        };
        key_ok && self.mods.matches(&i.modifiers)
    }
}

/// One keymap item: a [`Trigger`] bound — by command **id** (#14) — to an entry in
/// the unified [`COMMANDS`] registry. The item carries no label/category/action of
/// its own; it resolves them from its [`Command`] (so the keymap, menus, F3 palette
/// and cheat-sheet all read ONE source). [`action`](Self::action) /
/// [`category`](Self::category) / [`label`](Self::label) are thin accessors over
/// the resolved command, kept so the existing call sites (cheat-sheet, `poll`) read
/// the same as before the merge.
#[derive(Clone, Copy)]
pub struct Kmi {
    pub trigger: Trigger,
    /// The bound command's [`Command::id`] — the single dispatch key (#14).
    pub cmd: &'static str,
}

impl Kmi {
    /// The resolved [`Command`] this item binds (panics only on a registry typo,
    /// caught by the `every_binding_resolves` test).
    fn command(&self) -> &'static Command {
        command(self.cmd).expect("keymap binding references a known command id")
    }
    /// The [`Action`] the keymap dispatches for this bind (resolved from the
    /// command). `poll` / `poll_modal` route on this exactly as before.
    pub fn action(&self) -> Action {
        self.command().action
    }
    /// The cheat-sheet [`Category`] (resolved from the command).
    pub fn category(&self) -> Category {
        self.command().category
    }
    /// The cheat-sheet label (resolved from the command).
    pub fn label(&self) -> &'static str {
        self.command().label
    }
}

/// Which context a keymap belongs to. Dispatch stacks them most-specific-first:
/// `Modal` > `Viewport` > `Global`. A `Modal` map is only active while a transform
/// op owns the viewport; `Viewport` only while the 3D viewport is focused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeymapId {
    Global,
    Viewport,
    Modal,
}

/// A named, ordered set of [`Kmi`]s. `items()` is a flat slice over the registry
/// rows filtered to this keymap's id (the registry stays one table for review).
#[derive(Clone, Copy)]
pub struct KeyMap {
    pub id: KeymapId,
    pub items: &'static [Kmi],
}

/// Cheat-sheet grouping (also the display order top-to-bottom).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Navigation,
    Selection,
    Transform,
    Add,
    Object,
    View,
    File,
    App,
}

impl Category {
    /// Stable display order + heading text for the cheat sheet.
    pub const ORDER: [Category; 8] = [
        Category::Navigation,
        Category::Selection,
        Category::Transform,
        Category::Add,
        Category::Object,
        Category::View,
        Category::File,
        Category::App,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Category::Navigation => "NAVIGATION",
            Category::Selection => "SELECTION",
            Category::Transform => "TRANSFORM",
            Category::Add => "ADD",
            Category::Object => "OBJECT",
            Category::View => "VIEW",
            Category::File => "FILE",
            Category::App => "APP",
        }
    }
}

use egui::Key;

/// Compact constructor for a keymap row: a [`Trigger`] bound to a command `id`.
const fn kmi(trigger: Trigger, cmd: &'static str) -> Kmi {
    Kmi { trigger, cmd }
}

// ===========================================================================
// The unified Command registry (#9 / #14) — the SINGLE source of truth.
//
// Before this merge a "command" was split across two tables: the keymap's
// [`Kmi`] (Action + category + label) here and `op.rs`'s `CatalogOp`
// (id + label + category + invoke) there. They are now ONE descriptor: every
// user-triggerable command has a stable `id`, a cheat-sheet label/category, the
// [`Action`] the keymap dispatches, and an [`OpInvoke`] kind for the F3 palette /
// menus. The keymap binds *triggers to ids*; the catalog (`op::catalog()`) is a
// filtered VIEW over this table; the cheat-sheet + F3 read it too — so a key,
// a label and a menu entry can never drift apart.
// ===========================================================================

/// How the F3 operator-search palette / menus INVOKE a command through the op
/// pipeline. Re-exported from `op` so the one registry can name it without a
/// dependency cycle (the type itself lives next to the pipeline it drives).
pub use super::op::OpInvoke;

/// A unified command descriptor: the merge of the old keymap [`Kmi`] metadata and
/// the op `CatalogOp`. `id` is the stable dispatch key the keymap binds to (#14)
/// and the op pipeline / last-op slot use; `action` is how the keymap routes it;
/// `invoke` (when `Some`) marks it as a catalog operator the F3 palette lists and
/// dispatches (`Direct` runs, `Dialog` re-opens params). Commands with `invoke ==
/// None` are pure keymap actions (view/nav/selection) not in the op catalog.
#[derive(Clone, Copy)]
pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    pub category: Category,
    pub action: Action,
    pub invoke: Option<OpInvoke>,
}

/// Compact constructor for a non-catalog command (keymap action only).
const fn command_row(id: &'static str, label: &'static str, category: Category, action: Action) -> Command {
    Command { id, label, category, action, invoke: None }
}

/// Compact constructor for a catalog operator command (listed + dispatched by F3).
const fn op_row(
    id: &'static str,
    label: &'static str,
    category: Category,
    action: Action,
    invoke: OpInvoke,
) -> Command {
    Command { id, label, category, action, invoke: Some(invoke) }
}

/// Every command, keyed by `id` — the single source the keymap, menus, F3 palette
/// and cheat-sheet all read. The `label`/`category` here are what the cheat-sheet
/// and palette display; the `action` is what the keymap dispatches; `invoke` (when
/// set) makes it a catalog operator. Keymap rows ([`GLOBAL`]/[`VIEWPORT`]/[`MODAL`])
/// bind triggers to these ids; one command may carry several binds (e.g. nudge
/// plain + Shift, or undo + its alias). Ops carry a `<domain>.<verb>` id matching
/// the run sites; pure keymap commands use a `kmi.*` id (never dispatched as ops).
pub static COMMANDS: &[Command] = &[
    // --- View / framing ---
    command_row("view.frame_selection", "Frame selection", Category::View, Action::FrameSelection),
    command_row("view.frame_all", "Frame all", Category::View, Action::FrameAll),
    command_row("view.toggle_labels", "Toggle fixture labels", Category::View, Action::ToggleLabels),
    command_row("view.front", "Front view", Category::View, Action::View(CameraView::Front)),
    command_row("view.back", "Back view", Category::View, Action::View(CameraView::Back)),
    command_row("view.right", "Right view", Category::View, Action::View(CameraView::Right)),
    command_row("view.left", "Left view", Category::View, Action::View(CameraView::Left)),
    command_row("view.top", "Top view", Category::View, Action::View(CameraView::Top)),
    command_row("view.bottom", "Bottom view", Category::View, Action::View(CameraView::Bottom)),
    command_row("view.toggle_ortho", "Toggle ortho/persp", Category::View, Action::ToggleOrtho),
    command_row("view.camera", "Camera view", Category::View, Action::ViewCamera),
    command_row("view.orbit_up", "Orbit up", Category::View, Action::OrbitStep(0.0, 15.0)),
    command_row("view.orbit_down", "Orbit down", Category::View, Action::OrbitStep(0.0, -15.0)),
    command_row("view.orbit_left", "Orbit left", Category::View, Action::OrbitStep(-15.0, 0.0)),
    command_row("view.orbit_right", "Orbit right", Category::View, Action::OrbitStep(15.0, 0.0)),
    command_row("view.pie", "View pie (radial)", Category::View, Action::ViewPie),
    command_row("view.toggle_n_panel", "Toggle N-panel (sidebar)", Category::View, Action::ToggleNPanel),
    command_row("view.toggle_t_panel", "Toggle T-panel (tool rail)", Category::View, Action::ToggleTPanel),
    // --- Selection ---
    command_row("select.all", "Select all fixtures", Category::Selection, Action::SelectAll),
    command_row("select.quick", "Quick-select menu", Category::Selection, Action::QuickSelect),
    command_row("select.replace", "Replace selected fixtures", Category::Selection, Action::Replace),
    command_row("select.deselect", "Deselect all", Category::Selection, Action::Deselect),
    // --- Transform: nudge (plain = 0.1 m, Shift = 1.0 m). Each direction has a
    // plain command + a "(1 m)" command; the keymap binds the matching trigger. ---
    command_row("transform.nudge_x_neg", "Nudge -X (Shift = 1 m)", Category::Transform, Action::Nudge(Dir::XNeg, 0.1)),
    command_row("transform.nudge_x_pos", "Nudge +X (Shift = 1 m)", Category::Transform, Action::Nudge(Dir::XPos, 0.1)),
    command_row("transform.nudge_z_neg", "Nudge -Z (Shift = 1 m)", Category::Transform, Action::Nudge(Dir::ZNeg, 0.1)),
    command_row("transform.nudge_z_pos", "Nudge +Z (Shift = 1 m)", Category::Transform, Action::Nudge(Dir::ZPos, 0.1)),
    command_row("transform.nudge_y_up", "Nudge +height (Shift = 1 m)", Category::Transform, Action::Nudge(Dir::YUp, 0.1)),
    command_row("transform.nudge_y_down", "Nudge -height (Shift = 1 m)", Category::Transform, Action::Nudge(Dir::YDown, 0.1)),
    command_row("transform.nudge_x_neg_1m", "Nudge -X (1 m)", Category::Transform, Action::Nudge(Dir::XNeg, 1.0)),
    command_row("transform.nudge_x_pos_1m", "Nudge +X (1 m)", Category::Transform, Action::Nudge(Dir::XPos, 1.0)),
    command_row("transform.nudge_z_neg_1m", "Nudge -Z (1 m)", Category::Transform, Action::Nudge(Dir::ZNeg, 1.0)),
    command_row("transform.nudge_z_pos_1m", "Nudge +Z (1 m)", Category::Transform, Action::Nudge(Dir::ZPos, 1.0)),
    command_row("transform.nudge_y_up_1m", "Nudge +height (1 m)", Category::Transform, Action::Nudge(Dir::YUp, 1.0)),
    command_row("transform.nudge_y_down_1m", "Nudge -height (1 m)", Category::Transform, Action::Nudge(Dir::YDown, 1.0)),
    // --- Transform: modal G/R/S (impl in panels::viewport; registered here) ---
    command_row("transform.move", "Move selection", Category::Transform, Action::Transform(TransformKind::Move)),
    command_row("transform.rotate", "Rotate selection", Category::Transform, Action::Transform(TransformKind::Rotate)),
    command_row("transform.scale", "Scale selection", Category::Transform, Action::Transform(TransformKind::Scale)),
    // --- Transform: modal axis lock (impl in panels::viewport; MODAL map) ---
    command_row("transform.axis_x", "Lock X axis (during transform)", Category::Transform, Action::AxisLock(Axis::X)),
    command_row("transform.axis_y", "Lock Y axis (during transform)", Category::Transform, Action::AxisLock(Axis::Y)),
    command_row("transform.axis_z", "Lock Z axis (during transform)", Category::Transform, Action::AxisLock(Axis::Z)),
    // --- Add ---
    command_row("kmi.add_menu", "Add menu (at cursor)", Category::Add, Action::AddMenu),
    op_row("object.add", "Add Object…", Category::Add, Action::AddMenu, OpInvoke::Dialog),
    // --- Object / history ---
    op_row("fixture.duplicate", "Duplicate / Array…", Category::Object, Action::Duplicate, OpInvoke::Dialog),
    op_row("fixture.patch", "Patch Fixtures…", Category::Object, Action::Patch, OpInvoke::Dialog),
    op_row("fixture.unpatch", "Unpatch Fixtures", Category::Object, Action::Unpatch, OpInvoke::Direct),
    op_row("object.delete", "Delete Selected", Category::Object, Action::Delete, OpInvoke::Direct),
    // The bare Patch/Unpatch keymap labels differ from the catalog labels above, so
    // they get their own keymap-only commands (the P/U binds point here).
    command_row("kmi.patch", "Patch selected fixtures", Category::Object, Action::Patch),
    command_row("kmi.unpatch", "Unpatch selected fixtures", Category::Object, Action::Unpatch),
    command_row("kmi.duplicate", "Duplicate / array", Category::Object, Action::Duplicate),
    command_row("kmi.duplicate_alias", "Duplicate (alias)", Category::Object, Action::Duplicate),
    command_row("kmi.delete", "Delete selected", Category::Object, Action::Delete),
    command_row("kmi.delete_alias", "Delete selected (alias)", Category::Object, Action::Delete),
    command_row("edit.undo", "Undo", Category::Object, Action::Undo),
    command_row("edit.redo", "Redo", Category::Object, Action::Redo),
    command_row("edit.redo_alias", "Redo (alias)", Category::Object, Action::Redo),
    command_row("edit.operator_search", "Operator search", Category::Object, Action::OperatorSearch),
    command_row("edit.adjust_last", "Adjust last operation", Category::Object, Action::AdjustLast),
    // Aliases for view framing (Period / Home share Action with the primary binds).
    command_row("view.frame_selection_alias", "Frame selection (alias)", Category::View, Action::FrameSelection),
    command_row("view.frame_all_alias", "Frame all (alias)", Category::View, Action::FrameAll),
    // --- File ---
    command_row("file.save", "Save project", Category::File, Action::Save),
    command_row("file.save_as", "Save project as…", Category::File, Action::SaveAs),
    command_row("file.open", "Open project", Category::File, Action::Open),
    command_row("file.new", "New project", Category::File, Action::New),
    // --- App ---
    command_row("app.preferences", "Preferences", Category::App, Action::Preferences),
];

/// Look up a [`Command`] by its `id` — the unified registry's single accessor.
pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// The Global keymap — fires whenever the egui context has keyboard focus and no
/// text field is editing (and, for keys it shares with a more-specific map, only
/// when that map is inactive — see [`gather`]).
pub static GLOBAL: &[Kmi] = &[
    // --- View / framing ---
    kmi(Trigger::key(Key::F), "view.frame_selection"),
    kmi(Trigger::key(Key::F).shift(), "view.frame_all"),
    kmi(Trigger::key(Key::Period), "view.frame_selection_alias"),
    kmi(Trigger::key(Key::Home), "view.frame_all_alias"),
    kmi(Trigger::key(Key::L), "view.toggle_labels"),
    // --- Selection ---
    kmi(Trigger::key(Key::A), "select.all"),
    kmi(Trigger::key(Key::S), "select.quick"),
    kmi(Trigger::key(Key::R).shift(), "select.replace"),
    kmi(Trigger::key(Key::Escape), "select.deselect"),
    // --- Transform: nudge (plain = 0.1 m, Shift = 1.0 m). Plain rows surface in
    // the cheat sheet; the Shift rows share the "(Shift = 1 m)" label suffix. ---
    kmi(Trigger::key(Key::ArrowLeft), "transform.nudge_x_neg"),
    kmi(Trigger::key(Key::ArrowRight), "transform.nudge_x_pos"),
    kmi(Trigger::key(Key::ArrowUp), "transform.nudge_z_neg"),
    kmi(Trigger::key(Key::ArrowDown), "transform.nudge_z_pos"),
    kmi(Trigger::key(Key::PageUp), "transform.nudge_y_up"),
    kmi(Trigger::key(Key::PageDown), "transform.nudge_y_down"),
    kmi(Trigger::key(Key::ArrowLeft).shift(), "transform.nudge_x_neg_1m"),
    kmi(Trigger::key(Key::ArrowRight).shift(), "transform.nudge_x_pos_1m"),
    kmi(Trigger::key(Key::ArrowUp).shift(), "transform.nudge_z_neg_1m"),
    kmi(Trigger::key(Key::ArrowDown).shift(), "transform.nudge_z_pos_1m"),
    kmi(Trigger::key(Key::PageUp).shift(), "transform.nudge_y_up_1m"),
    kmi(Trigger::key(Key::PageDown).shift(), "transform.nudge_y_down_1m"),
    // --- Object ---
    kmi(Trigger::key(Key::Delete), "kmi.delete"),
    kmi(Trigger::key(Key::Backspace), "kmi.delete_alias"),
    kmi(Trigger::key(Key::P), "kmi.patch"),
    kmi(Trigger::key(Key::U), "kmi.unpatch"),
    // --- Edit / history ---
    kmi(Trigger::key(Key::Z).cmd(), "edit.undo"),
    kmi(Trigger::key(Key::Z).cmd().shift(), "edit.redo"),
    kmi(Trigger::key(Key::Y).cmd(), "edit.redo_alias"),
    kmi(Trigger::key(Key::F3), "edit.operator_search"),
    // Blender-style spacebar search. Global context only — the transform MODAL map
    // keeps Space = confirm mid-G/R/S (global binds are suppressed during a modal op).
    kmi(Trigger::key(Key::Space), "edit.operator_search"),
    kmi(Trigger::key(Key::F9), "edit.adjust_last"),
    // --- File ---
    kmi(Trigger::key(Key::S).cmd(), "file.save"),
    kmi(Trigger::key(Key::S).cmd().shift(), "file.save_as"),
    kmi(Trigger::key(Key::O).cmd(), "file.open"),
    kmi(Trigger::key(Key::N).cmd(), "file.new"),
    // --- App ---
    kmi(Trigger::key(Key::Comma).cmd(), "app.preferences"),
];

/// The Viewport keymap — only active while the 3D viewport is the focused panel.
/// Its `S` (Scale) and `R` (Rotate) mask the Global `S` (quick-select) and the
/// `R`-family there; `D` / Shift+A are viewport-only. The X/Y/Z axis-lock binds
/// live in the MODAL map (they only mean anything mid-transform).
pub static VIEWPORT: &[Kmi] = &[
    kmi(Trigger::key(Key::G), "transform.move"),
    kmi(Trigger::key(Key::R), "transform.rotate"),
    kmi(Trigger::key(Key::S), "transform.scale"),
    kmi(Trigger::key(Key::A).shift(), "kmi.add_menu"),
    kmi(Trigger::key(Key::D), "kmi.duplicate"),
    kmi(Trigger::key(Key::D).shift(), "kmi.duplicate_alias"),
    kmi(Trigger::key(Key::N), "view.toggle_n_panel"),
    kmi(Trigger::key(Key::T), "view.toggle_t_panel"),
    // --- Numpad camera navigation (Blender view3d_navigate_axis*.cc). egui maps
    // the numpad digits onto the same Num* variants as the top-row digits, so
    // these fire on either; the Viewport context + text-field guard keep them out
    // of the way. Ctrl flips an axis view to its opposite (Front↔Back, etc.). ---
    kmi(Trigger::key(Key::Num1), "view.front"),
    kmi(Trigger::key(Key::Num1).cmd(), "view.back"),
    kmi(Trigger::key(Key::Num3), "view.right"),
    kmi(Trigger::key(Key::Num3).cmd(), "view.left"),
    kmi(Trigger::key(Key::Num7), "view.top"),
    kmi(Trigger::key(Key::Num7).cmd(), "view.bottom"),
    kmi(Trigger::key(Key::Num5), "view.toggle_ortho"),
    kmi(Trigger::key(Key::Num8), "view.orbit_up"),
    kmi(Trigger::key(Key::Num2), "view.orbit_down"),
    kmi(Trigger::key(Key::Num4), "view.orbit_left"),
    kmi(Trigger::key(Key::Num6), "view.orbit_right"),
    // `~` opens the radial View pie at the cursor (wired in a later stage).
    kmi(Trigger::key(Key::Backtick), "view.pie"),
];

/// The Modal transform keymap — only active while a G/R/S op owns the viewport.
/// X/Y/Z constrain to an axis; Enter/Space confirm; Esc cancels. Consumed via
/// [`poll_modal`], NOT [`poll`] (these binds must never fire outside a transform).
pub static MODAL: &[Kmi] = &[
    kmi(Trigger::key(Key::X), "transform.axis_x"),
    kmi(Trigger::key(Key::Y), "transform.axis_y"),
    kmi(Trigger::key(Key::Z), "transform.axis_z"),
];

/// All keymaps, used by the cheat sheet to walk every registered bind.
pub static KEYMAPS: &[KeyMap] = &[
    KeyMap { id: KeymapId::Global, items: GLOBAL },
    KeyMap { id: KeymapId::Viewport, items: VIEWPORT },
    KeyMap { id: KeymapId::Modal, items: MODAL },
];

/// Modal-transform actions, decoded from the [`MODAL`] keymap by [`poll_modal`].
/// The viewport's live transform block consumes THESE instead of raw key reads
/// for X/Y/Z/Enter/Esc, so the binds stay in the one registry + cheat sheet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModalAction {
    ConstrainX,
    ConstrainY,
    ConstrainZ,
    Confirm,
    Cancel,
}

/// The contexts active this frame, most-specific-first. `viewport` adds the
/// Viewport map; an in-progress transform owns the viewport so its plain G/R/S/A
/// etc. are suppressed (the modal map is polled separately via [`poll_modal`]).
#[derive(Clone, Copy)]
pub struct ActiveContext {
    pub viewport_focused: bool,
    pub transform_active: bool,
}

/// The keymap registered under `id` (resolved from [`KEYMAPS`] by its `id` field).
fn by_id(id: KeymapId) -> &'static [Kmi] {
    KEYMAPS.iter().find(|km| km.id == id).map(|km| km.items).unwrap_or(&[])
}

// Static context stacks for [`gather`] (most-specific-first), as keymap ids.
static STACK_VIEWPORT: [KeymapId; 2] = [KeymapId::Viewport, KeymapId::Global];
static STACK_GLOBAL: [KeymapId; 1] = [KeymapId::Global];
static STACK_NONE: [KeymapId; 0] = [];

/// Gather the keymap ids active under `cx`, most-specific-first. While a transform
/// owns the viewport, NO press-keymap is active here (the viewport's plain binds
/// must not fire mid-op; axis-lock/confirm/cancel go through [`poll_modal`]).
fn gather(cx: ActiveContext) -> &'static [KeymapId] {
    match (cx.viewport_focused, cx.transform_active) {
        (_, true) => &STACK_NONE,
        (true, false) => &STACK_VIEWPORT,
        (false, false) => &STACK_GLOBAL,
    }
}

/// Read this frame's input and return the [`Action`]s that matched, with the
/// active keymaps stacked most-specific-first and **first match per key winning**
/// — so a focused-viewport `S` resolves to Scale (Viewport) and the Global `S`
/// (quick-select) on the same physical key is masked. Returns empty while a text
/// field has keyboard focus (so typing never triggers shortcuts) or while a modal
/// transform owns the viewport (use [`poll_modal`] there).
pub fn poll(ctx: &egui::Context, cx: ActiveContext) -> Vec<Action> {
    if ctx.egui_wants_keyboard_input() {
        return Vec::new();
    }
    let stack = gather(cx);
    ctx.input(|i| {
        let mut out: Vec<Action> = Vec::new();
        // Track which physical keys a more-specific map already claimed, so a less
        // specific map can't re-fire the same key (first match wins).
        let mut claimed: Vec<egui::Key> = Vec::new();
        for id in stack {
            for kmi in by_id(*id) {
                if kmi.trigger.fired(i) && !claimed.contains(&kmi.trigger.key) {
                    claimed.push(kmi.trigger.key);
                    out.push(kmi.action());
                }
            }
        }
        out
    })
}

/// Poll the [`MODAL`] keymap for the live-transform block. Only the modal binds
/// (X/Y/Z axis lock) come from the registry; Confirm/Cancel are reported too,
/// reading Enter/Space (confirm) and Esc (cancel) so the viewport can route ALL
/// modal keys through one call instead of scattered raw `key_pressed` reads.
pub fn poll_modal(ctx: &egui::Context) -> Vec<ModalAction> {
    ctx.input(|i| {
        let mut out: Vec<ModalAction> = Vec::new();
        for kmi in MODAL {
            if kmi.trigger.fired(i)
                && let Action::AxisLock(ax) = kmi.action()
            {
                out.push(match ax {
                    Axis::X => ModalAction::ConstrainX,
                    Axis::Y => ModalAction::ConstrainY,
                    Axis::Z => ModalAction::ConstrainZ,
                });
            }
        }
        if i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space) {
            out.push(ModalAction::Confirm);
        }
        if i.key_pressed(egui::Key::Escape) {
            out.push(ModalAction::Cancel);
        }
        out
    })
}

/// The live key label bound to a [`ModalAction`] in the [`MODAL`] keymap, for the
/// in-viewport transform hint pill (#23). The axis-lock keys are read from the
/// registry so a rebind there flows straight into the on-screen hint (no drift
/// between what the app does and what the pill advertises). Confirm/Cancel aren't
/// in the keymap table (they read Enter/Space/Esc directly in [`poll_modal`]), so
/// they return their fixed glyphs.
pub fn modal_key_label(action: ModalAction) -> String {
    let want_axis = match action {
        ModalAction::ConstrainX => Some(Axis::X),
        ModalAction::ConstrainY => Some(Axis::Y),
        ModalAction::ConstrainZ => Some(Axis::Z),
        ModalAction::Confirm => return "Enter".into(),
        ModalAction::Cancel => return "Esc".into(),
    };
    // Find the MODAL item whose AxisLock matches and label its trigger.
    MODAL
        .iter()
        .find_map(|k| match k.action() {
            Action::AxisLock(ax) if Some(ax) == want_axis => Some(key_label(&k.trigger)),
            _ => None,
        })
        .unwrap_or_default()
}

/// The structured modal-transform hint segments (#23): the live axis-lock keys
/// joined as one "X/Y/Z" cluster plus the confirm/cancel glyphs, all read from
/// the keymap so the pill never drifts from the binds. Returns the constraint
/// hint string the viewport composes into the transform pill, e.g.
/// `"X/Y/Z lock · type number · Enter confirm · Esc cancel"`.
pub fn modal_hint_keys() -> String {
    let x = modal_key_label(ModalAction::ConstrainX);
    let y = modal_key_label(ModalAction::ConstrainY);
    let z = modal_key_label(ModalAction::ConstrainZ);
    let confirm = modal_key_label(ModalAction::Confirm);
    let cancel = modal_key_label(ModalAction::Cancel);
    format!("{x}/{y}/{z} lock · type number · {confirm} confirm · {cancel} cancel")
}

/// A human-readable label for a trigger, used by the cheat sheet. Uses Cmd glyph
/// on macOS, "Ctrl" elsewhere; special-cases the named keys (arrows, Numpad…).
pub fn key_label(t: &Trigger) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if t.mods.command == Some(true) {
        parts.push(if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" });
    }
    if t.mods.alt == Some(true) {
        parts.push(if cfg!(target_os = "macos") { "⌥" } else { "Alt" });
    }
    if t.mods.shift == Some(true) {
        parts.push("Shift");
    }
    let key = key_name(t.key);
    parts.push(&key);
    parts.join("+")
}

/// Display name for a single [`egui::Key`] (the named ones the table uses).
fn key_name(key: egui::Key) -> String {
    match key {
        Key::ArrowLeft => "←".into(),
        Key::ArrowRight => "→".into(),
        Key::ArrowUp => "↑".into(),
        Key::ArrowDown => "↓".into(),
        Key::PageUp => "PageUp".into(),
        Key::PageDown => "PageDown".into(),
        Key::Home => "Home".into(),
        Key::Period => ".".into(),
        Key::Comma => ",".into(),
        Key::Escape => "Esc".into(),
        Key::Delete => "Del".into(),
        Key::Backspace => "Backspace".into(),
        Key::Enter => "Enter".into(),
        Key::Num0 => "Numpad 0".into(),
        Key::Num1 => "Numpad 1".into(),
        Key::Num3 => "Numpad 3".into(),
        Key::Num5 => "Numpad 5".into(),
        Key::Num7 => "Numpad 7".into(),
        other => other.name().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_label_smoke() {
        // A handful of representative triggers produce sane, non-empty labels.
        assert!(!key_label(&Trigger::key(Key::S).cmd()).is_empty());
        assert_eq!(key_label(&Trigger::key(Key::F)), "F");
        assert!(key_label(&Trigger::key(Key::F).shift()).contains("Shift"));
        assert_eq!(key_label(&Trigger::key(Key::Home)), "Home");
    }

    #[test]
    fn modal_hint_reads_live_keys() {
        // The axis-lock labels come from the MODAL keymap (so a rebind flows into
        // the on-screen pill); confirm/cancel are the fixed Enter/Esc glyphs.
        assert_eq!(modal_key_label(ModalAction::ConstrainX), "X");
        assert_eq!(modal_key_label(ModalAction::ConstrainY), "Y");
        assert_eq!(modal_key_label(ModalAction::ConstrainZ), "Z");
        assert_eq!(modal_key_label(ModalAction::Confirm), "Enter");
        assert_eq!(modal_key_label(ModalAction::Cancel), "Esc");
        // The composed hint advertises every modal key + the typed-number path.
        let h = modal_hint_keys();
        assert!(h.contains("X/Y/Z lock"), "axis cluster present: {h}");
        assert!(h.contains("type number"));
        assert!(h.contains("Enter confirm") && h.contains("Esc cancel"));
    }

    #[test]
    fn no_duplicate_binds() {
        // Within a single keymap, no two items may share key + modifiers, or poll()
        // would dispatch both for one keypress.
        for map in KEYMAPS {
            let items = map.items;
            for (a_idx, a) in items.iter().enumerate() {
                for b in items.iter().skip(a_idx + 1) {
                    let same = a.trigger.key == b.trigger.key && a.trigger.mods == b.trigger.mods;
                    assert!(!same, "duplicate bind on key {:?} within one keymap", a.trigger.key);
                }
            }
        }
    }

    #[test]
    fn viewport_s_masks_global_s() {
        // The keymap stack must put Viewport before Global so a focused-viewport
        // `S` resolves to Scale, not quick-select. (Pure structural check — no egui
        // input needed: assert the gather order + that both maps bind plain `S`.)
        let cx = ActiveContext { viewport_focused: true, transform_active: false };
        let stack = gather(cx);
        assert_eq!(stack, &[KeymapId::Viewport, KeymapId::Global], "viewport stacks first");
        let first_s =
            by_id(stack[0]).iter().find(|k| k.trigger.key == Key::S && k.trigger.mods == Mods::none());
        assert!(
            matches!(first_s.map(|k| k.action()), Some(Action::Transform(TransformKind::Scale))),
            "most-specific map's plain S must be Scale"
        );
        // And the modal map is never in the plain press stack.
        assert!(!stack.contains(&KeymapId::Modal));
    }

    #[test]
    fn transform_suppresses_press_maps() {
        // While a transform owns the viewport, no press-keymap is active (modal keys
        // route through poll_modal instead).
        let cx = ActiveContext { viewport_focused: true, transform_active: true };
        assert!(gather(cx).is_empty());
    }

    // --- C1 unified-command registry parity (#9 / #14) -------------------

    #[test]
    fn every_binding_resolves_to_a_command() {
        // Every keymap row binds a trigger to a command *id* (#14); each id MUST
        // resolve in the unified COMMANDS table (a typo would panic at dispatch).
        for map in KEYMAPS {
            for kmi in map.items {
                assert!(
                    command(kmi.cmd).is_some(),
                    "keymap binding references unknown command id {:?}",
                    kmi.cmd
                );
            }
        }
    }

    #[test]
    fn command_ids_unique() {
        // The registry is keyed by id — duplicates would make `command()` ambiguous
        // and let the catalog list one op twice.
        for (i, a) in COMMANDS.iter().enumerate() {
            for b in COMMANDS.iter().skip(i + 1) {
                assert_ne!(a.id, b.id, "duplicate command id {}", a.id);
            }
        }
    }

    #[test]
    fn no_duplicate_triggers_across_a_keymap() {
        // Sharper than `no_duplicate_binds`: re-stated as the C1 parity guard the
        // spec asks for — within any one keymap, no two rows may share key + mods,
        // or poll() would dispatch both commands for a single keypress.
        for map in KEYMAPS {
            for (i, a) in map.items.iter().enumerate() {
                for b in map.items.iter().skip(i + 1) {
                    let same = a.trigger.key == b.trigger.key && a.trigger.mods == b.trigger.mods;
                    assert!(!same, "duplicate trigger on {:?} in keymap {:?}", a.trigger.key, map.id);
                }
            }
        }
    }

    #[test]
    fn prior_key_set_still_resolves_to_its_action() {
        // Pin that the C1 merge preserved EVERY pre-existing bind's behaviour: each
        // (keymap, key, mods) → Action mapping from before the unification still
        // resolves to the exact same Action through the new id-indirection. If a
        // command id were mis-wired this catches it. (Representative cross-section of
        // every category + both modifier states + the modal map.)
        let want: &[(KeymapId, Trigger, Action)] = &[
            // View / nav
            (KeymapId::Global, Trigger::key(Key::F), Action::FrameSelection),
            (KeymapId::Global, Trigger::key(Key::F).shift(), Action::FrameAll),
            (KeymapId::Global, Trigger::key(Key::L), Action::ToggleLabels),
            (KeymapId::Viewport, Trigger::key(Key::Num1), Action::View(CameraView::Front)),
            (KeymapId::Viewport, Trigger::key(Key::Num1).cmd(), Action::View(CameraView::Back)),
            (KeymapId::Viewport, Trigger::key(Key::Num5), Action::ToggleOrtho),
            (KeymapId::Viewport, Trigger::key(Key::Backtick), Action::ViewPie),
            (KeymapId::Viewport, Trigger::key(Key::N), Action::ToggleNPanel),
            (KeymapId::Viewport, Trigger::key(Key::T), Action::ToggleTPanel),
            // Selection
            (KeymapId::Global, Trigger::key(Key::A), Action::SelectAll),
            (KeymapId::Global, Trigger::key(Key::S), Action::QuickSelect),
            (KeymapId::Global, Trigger::key(Key::R).shift(), Action::Replace),
            (KeymapId::Global, Trigger::key(Key::Escape), Action::Deselect),
            // Transform: nudge (plain + shift) + modal G/R/S
            (KeymapId::Global, Trigger::key(Key::ArrowLeft), Action::Nudge(Dir::XNeg, 0.1)),
            (KeymapId::Global, Trigger::key(Key::ArrowLeft).shift(), Action::Nudge(Dir::XNeg, 1.0)),
            (KeymapId::Global, Trigger::key(Key::PageUp), Action::Nudge(Dir::YUp, 0.1)),
            (KeymapId::Viewport, Trigger::key(Key::G), Action::Transform(TransformKind::Move)),
            (KeymapId::Viewport, Trigger::key(Key::R), Action::Transform(TransformKind::Rotate)),
            (KeymapId::Viewport, Trigger::key(Key::S), Action::Transform(TransformKind::Scale)),
            // Add / Object / history
            (KeymapId::Viewport, Trigger::key(Key::A).shift(), Action::AddMenu),
            (KeymapId::Viewport, Trigger::key(Key::D), Action::Duplicate),
            (KeymapId::Viewport, Trigger::key(Key::D).shift(), Action::Duplicate),
            (KeymapId::Global, Trigger::key(Key::Delete), Action::Delete),
            (KeymapId::Global, Trigger::key(Key::Backspace), Action::Delete),
            (KeymapId::Global, Trigger::key(Key::P), Action::Patch),
            (KeymapId::Global, Trigger::key(Key::U), Action::Unpatch),
            (KeymapId::Global, Trigger::key(Key::Z).cmd(), Action::Undo),
            (KeymapId::Global, Trigger::key(Key::Z).cmd().shift(), Action::Redo),
            (KeymapId::Global, Trigger::key(Key::Y).cmd(), Action::Redo),
            (KeymapId::Global, Trigger::key(Key::F3), Action::OperatorSearch),
            (KeymapId::Global, Trigger::key(Key::F9), Action::AdjustLast),
            // File / App
            (KeymapId::Global, Trigger::key(Key::S).cmd(), Action::Save),
            (KeymapId::Global, Trigger::key(Key::S).cmd().shift(), Action::SaveAs),
            (KeymapId::Global, Trigger::key(Key::O).cmd(), Action::Open),
            (KeymapId::Global, Trigger::key(Key::N).cmd(), Action::New),
            (KeymapId::Global, Trigger::key(Key::Comma).cmd(), Action::Preferences),
            // Modal axis lock
            (KeymapId::Modal, Trigger::key(Key::X), Action::AxisLock(Axis::X)),
            (KeymapId::Modal, Trigger::key(Key::Y), Action::AxisLock(Axis::Y)),
            (KeymapId::Modal, Trigger::key(Key::Z), Action::AxisLock(Axis::Z)),
        ];
        for (map_id, trig, action) in want {
            let found = by_id(*map_id).iter().find(|k| {
                k.trigger.key == trig.key && k.trigger.mods == trig.mods
            });
            let got = found.map(|k| k.action());
            assert!(
                got == Some(*action),
                "bind {:?} in {:?} must resolve to its prior Action (got {:?})",
                trig.key,
                map_id,
                got.is_some()
            );
        }
    }
}
