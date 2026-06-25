//! The keyboard-shortcut registry — the SINGLE source of truth for every bind.
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
    /// F3 — open the operator-search palette (run any registered op by name).
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

/// One keymap item: a [`Trigger`] bound to an [`Action`], plus its cheat-sheet
/// metadata ([`Category`] + label). Step-pairs (plain/Shift nudge) are two items.
#[derive(Clone, Copy)]
pub struct Kmi {
    pub trigger: Trigger,
    pub action: Action,
    pub category: Category,
    pub label: &'static str,
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

/// Compact constructor for a registry row.
const fn kmi(trigger: Trigger, action: Action, category: Category, label: &'static str) -> Kmi {
    Kmi { trigger, action, category, label }
}

/// The Global keymap — fires whenever the egui context has keyboard focus and no
/// text field is editing (and, for keys it shares with a more-specific map, only
/// when that map is inactive — see [`gather`]).
pub static GLOBAL: &[Kmi] = &[
    // --- View / framing ---
    kmi(Trigger::key(Key::F), Action::FrameSelection, Category::View, "Frame selection"),
    kmi(Trigger::key(Key::F).shift(), Action::FrameAll, Category::View, "Frame all"),
    kmi(Trigger::key(Key::Period), Action::FrameSelection, Category::View, "Frame selection (alias)"),
    kmi(Trigger::key(Key::Home), Action::FrameAll, Category::View, "Frame all (alias)"),
    kmi(Trigger::key(Key::L), Action::ToggleLabels, Category::View, "Toggle fixture labels"),
    // --- Selection ---
    kmi(Trigger::key(Key::A), Action::SelectAll, Category::Selection, "Select all fixtures"),
    kmi(Trigger::key(Key::S), Action::QuickSelect, Category::Selection, "Quick-select menu"),
    kmi(Trigger::key(Key::R).shift(), Action::Replace, Category::Selection, "Replace selected fixtures"),
    kmi(Trigger::key(Key::Escape), Action::Deselect, Category::Selection, "Deselect all"),
    // --- Transform: nudge (plain = 0.1 m, Shift = 1.0 m). Plain rows surface in
    // the cheat sheet; the Shift rows share the "(Shift = 1 m)" label suffix. ---
    kmi(Trigger::key(Key::ArrowLeft), Action::Nudge(Dir::XNeg, 0.1), Category::Transform, "Nudge -X (Shift = 1 m)"),
    kmi(Trigger::key(Key::ArrowRight), Action::Nudge(Dir::XPos, 0.1), Category::Transform, "Nudge +X (Shift = 1 m)"),
    kmi(Trigger::key(Key::ArrowUp), Action::Nudge(Dir::ZNeg, 0.1), Category::Transform, "Nudge -Z (Shift = 1 m)"),
    kmi(Trigger::key(Key::ArrowDown), Action::Nudge(Dir::ZPos, 0.1), Category::Transform, "Nudge +Z (Shift = 1 m)"),
    kmi(Trigger::key(Key::PageUp), Action::Nudge(Dir::YUp, 0.1), Category::Transform, "Nudge +height (Shift = 1 m)"),
    kmi(Trigger::key(Key::PageDown), Action::Nudge(Dir::YDown, 0.1), Category::Transform, "Nudge -height (Shift = 1 m)"),
    kmi(Trigger::key(Key::ArrowLeft).shift(), Action::Nudge(Dir::XNeg, 1.0), Category::Transform, "Nudge -X (1 m)"),
    kmi(Trigger::key(Key::ArrowRight).shift(), Action::Nudge(Dir::XPos, 1.0), Category::Transform, "Nudge +X (1 m)"),
    kmi(Trigger::key(Key::ArrowUp).shift(), Action::Nudge(Dir::ZNeg, 1.0), Category::Transform, "Nudge -Z (1 m)"),
    kmi(Trigger::key(Key::ArrowDown).shift(), Action::Nudge(Dir::ZPos, 1.0), Category::Transform, "Nudge +Z (1 m)"),
    kmi(Trigger::key(Key::PageUp).shift(), Action::Nudge(Dir::YUp, 1.0), Category::Transform, "Nudge +height (1 m)"),
    kmi(Trigger::key(Key::PageDown).shift(), Action::Nudge(Dir::YDown, 1.0), Category::Transform, "Nudge -height (1 m)"),
    // --- Object ---
    kmi(Trigger::key(Key::Delete), Action::Delete, Category::Object, "Delete selected"),
    kmi(Trigger::key(Key::Backspace), Action::Delete, Category::Object, "Delete selected (alias)"),
    kmi(Trigger::key(Key::P), Action::Patch, Category::Object, "Patch selected fixtures"),
    kmi(Trigger::key(Key::U), Action::Unpatch, Category::Object, "Unpatch selected fixtures"),
    // --- Edit / history ---
    kmi(Trigger::key(Key::Z).cmd(), Action::Undo, Category::Object, "Undo"),
    kmi(Trigger::key(Key::Z).cmd().shift(), Action::Redo, Category::Object, "Redo"),
    kmi(Trigger::key(Key::Y).cmd(), Action::Redo, Category::Object, "Redo (alias)"),
    kmi(Trigger::key(Key::F3), Action::OperatorSearch, Category::Object, "Operator search"),
    kmi(Trigger::key(Key::F9), Action::AdjustLast, Category::Object, "Adjust last operation"),
    // --- File ---
    kmi(Trigger::key(Key::S).cmd(), Action::Save, Category::File, "Save project"),
    kmi(Trigger::key(Key::S).cmd().shift(), Action::SaveAs, Category::File, "Save project as…"),
    kmi(Trigger::key(Key::O).cmd(), Action::Open, Category::File, "Open project"),
    kmi(Trigger::key(Key::N).cmd(), Action::New, Category::File, "New project"),
    // --- App ---
    kmi(Trigger::key(Key::Comma).cmd(), Action::Preferences, Category::App, "Preferences"),
];

/// The Viewport keymap — only active while the 3D viewport is the focused panel.
/// Its `S` (Scale) and `R` (Rotate) mask the Global `S` (quick-select) and the
/// `R`-family there; `D` / Shift+A are viewport-only. The X/Y/Z axis-lock binds
/// live in the MODAL map (they only mean anything mid-transform).
pub static VIEWPORT: &[Kmi] = &[
    kmi(Trigger::key(Key::G), Action::Transform(TransformKind::Move), Category::Transform, "Move selection"),
    kmi(Trigger::key(Key::R), Action::Transform(TransformKind::Rotate), Category::Transform, "Rotate selection"),
    kmi(Trigger::key(Key::S), Action::Transform(TransformKind::Scale), Category::Transform, "Scale selection"),
    kmi(Trigger::key(Key::A).shift(), Action::AddMenu, Category::Add, "Add menu (at cursor)"),
    kmi(Trigger::key(Key::D), Action::Duplicate, Category::Object, "Duplicate / array"),
    kmi(Trigger::key(Key::D).shift(), Action::Duplicate, Category::Object, "Duplicate (alias)"),
    kmi(Trigger::key(Key::N), Action::ToggleNPanel, Category::View, "Toggle N-panel (sidebar)"),
    kmi(Trigger::key(Key::T), Action::ToggleTPanel, Category::View, "Toggle T-panel (tool rail)"),
    // --- Numpad camera navigation (Blender view3d_navigate_axis*.cc). egui maps
    // the numpad digits onto the same Num* variants as the top-row digits, so
    // these fire on either; the Viewport context + text-field guard keep them out
    // of the way. Ctrl flips an axis view to its opposite (Front↔Back, etc.). ---
    kmi(Trigger::key(Key::Num1), Action::View(CameraView::Front), Category::View, "Front view"),
    kmi(Trigger::key(Key::Num1).cmd(), Action::View(CameraView::Back), Category::View, "Back view"),
    kmi(Trigger::key(Key::Num3), Action::View(CameraView::Right), Category::View, "Right view"),
    kmi(Trigger::key(Key::Num3).cmd(), Action::View(CameraView::Left), Category::View, "Left view"),
    kmi(Trigger::key(Key::Num7), Action::View(CameraView::Top), Category::View, "Top view"),
    kmi(Trigger::key(Key::Num7).cmd(), Action::View(CameraView::Bottom), Category::View, "Bottom view"),
    kmi(Trigger::key(Key::Num5), Action::ToggleOrtho, Category::View, "Toggle ortho/persp"),
    kmi(Trigger::key(Key::Num8), Action::OrbitStep(0.0, 15.0), Category::View, "Orbit up"),
    kmi(Trigger::key(Key::Num2), Action::OrbitStep(0.0, -15.0), Category::View, "Orbit down"),
    kmi(Trigger::key(Key::Num4), Action::OrbitStep(-15.0, 0.0), Category::View, "Orbit left"),
    kmi(Trigger::key(Key::Num6), Action::OrbitStep(15.0, 0.0), Category::View, "Orbit right"),
    // `~` opens the radial View pie at the cursor (wired in a later stage).
    kmi(Trigger::key(Key::Backtick), Action::ViewPie, Category::View, "View pie (radial)"),
];

/// The Modal transform keymap — only active while a G/R/S op owns the viewport.
/// X/Y/Z constrain to an axis; Enter/Space confirm; Esc cancels. Consumed via
/// [`poll_modal`], NOT [`poll`] (these binds must never fire outside a transform).
pub static MODAL: &[Kmi] = &[
    kmi(Trigger::key(Key::X), Action::AxisLock(Axis::X), Category::Transform, "Lock X axis (during transform)"),
    kmi(Trigger::key(Key::Y), Action::AxisLock(Axis::Y), Category::Transform, "Lock Y axis (during transform)"),
    kmi(Trigger::key(Key::Z), Action::AxisLock(Axis::Z), Category::Transform, "Lock Z axis (during transform)"),
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
                    out.push(kmi.action);
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
                && let Action::AxisLock(ax) = kmi.action
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
        .find_map(|k| match k.action {
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
            matches!(first_s.map(|k| k.action), Some(Action::Transform(TransformKind::Scale))),
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
}
