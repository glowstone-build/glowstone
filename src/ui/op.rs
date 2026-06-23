//! Undo / redo via full-document snapshots.
//!
//! The undo model is the SAME serialization the `.archie` format proves
//! ([`project`](super::project)): a [`DocSnapshot`] is a `bincode` dump of the
//! mutable document — the [`Scene`], the DMX [`PatchTable`], the [`CueEngine`],
//! the selection groups — plus the live [`Selection`]. A step stores BOTH ends
//! ([`UndoStep::before`] / `after`) so undo and redo are symmetric (no replay).
//!
//! THE CORRECTNESS TRAP (verified): `Fixture.gdtf: Option<Arc<GdtfFixture>>` is
//! `#[serde(skip)]` — it is re-linked from the bundled archive at `.archie` load,
//! never serialized. A bincode snapshot of the `Scene` therefore DROPS every
//! fixture's parsed-GDTF handle. So a snapshot ALSO keeps the handles out of band
//! ([`DocSnapshot::gdtf`], one cheap `Arc` clone per fixture in order); restore
//! reattaches `f.gdtf = saved[i]` and calls [`Fixture::sync_mode`] so per-mode
//! state (cells / wheels / motion phases) re-aligns. The screen-runtime frame
//! (`screen.rs` `serde(skip)`) is transient/derived — left `None`, it repopulates
//! next frame.
//!
//! The stack lives on [`Ui`](super::Ui), travels with the document in memory, and
//! is NOT serialized into `.archie` (a fresh open starts with an empty history).

use std::sync::Arc;

use crate::dmx::PatchTable;
use crate::gdtf::GdtfFixture;
use crate::scene::{Library, Scene, Selection};

use super::cues::CueEngine;
use super::SelectionGroup;

/// Max steps retained before the oldest is dropped.
const LIMIT_STEPS: usize = 64;
/// Soft cap on total snapshot bytes (~256 MB). Oldest steps drop past it.
const LIMIT_BYTES: usize = 256 * 1024 * 1024;

/// An immutable picture of the whole mutable document at one instant. The four
/// `Vec<u8>` blobs are positional `bincode` dumps of the same serde types the
/// `.archie` format round-trips; [`gdtf`](Self::gdtf) carries the parsed-GDTF
/// `Arc`s out of band (see the module docs — they are `serde(skip)`).
pub struct DocSnapshot {
    scene: Vec<u8>,
    patch: Vec<u8>,
    cues: Vec<u8>,
    groups: Vec<u8>,
    /// Live selection (cheap; not serialized — just cloned).
    selection: Selection,
    /// Out-of-band parsed-GDTF handles, aligned to `scene.fixtures` order. A
    /// fixture without a GDTF stores `None`. Pointer clones — cheap.
    gdtf: Vec<Option<Arc<GdtfFixture>>>,
}

impl DocSnapshot {
    /// Approximate retained size, for the byte-cap accounting.
    fn bytes(&self) -> usize {
        self.scene.len() + self.patch.len() + self.cues.len() + self.groups.len()
    }
}

/// Serialize the whole document into a [`DocSnapshot`], keeping the parsed-GDTF
/// `Arc`s out of band. Cheap relative to the scene size (one bincode pass) and
/// only called at operator boundaries, not per frame.
pub fn capture(
    scene: &Scene,
    patch: &PatchTable,
    cues: &CueEngine,
    groups: &[SelectionGroup],
    selection: &Selection,
) -> DocSnapshot {
    // bincode of these serde types cannot fail in practice (no custom impls that
    // error); fall back to empty blobs rather than panicking the editor.
    let enc = |r: Result<Vec<u8>, _>| r.unwrap_or_default();
    DocSnapshot {
        scene: enc(bincode::serialize(scene)),
        patch: enc(bincode::serialize(patch)),
        cues: enc(bincode::serialize(cues)),
        groups: enc(bincode::serialize(&groups.to_vec())),
        selection: selection.clone(),
        gdtf: scene.fixtures.iter().map(|f| f.gdtf.clone()).collect(),
    }
}

impl DocSnapshot {
    /// Restore this snapshot into the live document, reattaching the parsed-GDTF
    /// `Arc`s out of band and re-syncing each fixture's per-mode state (the trap).
    pub fn restore(
        &self,
        scene: &mut Scene,
        patch: &mut PatchTable,
        cues: &mut CueEngine,
        groups: &mut Vec<SelectionGroup>,
        selection: &mut Selection,
    ) {
        if let Ok(s) = bincode::deserialize::<Scene>(&self.scene) {
            *scene = s;
        }
        if let Ok(p) = bincode::deserialize::<PatchTable>(&self.patch) {
            *patch = p;
        }
        if let Ok(c) = bincode::deserialize::<CueEngine>(&self.cues) {
            *cues = c;
        }
        if let Ok(g) = bincode::deserialize::<Vec<SelectionGroup>>(&self.groups) {
            *groups = g;
        }
        // Reattach the GDTF handles the bincode dropped, in fixture order, then
        // re-align per-mode state (cells / wheels / motion) for each.
        for (f, saved) in scene.fixtures.iter_mut().zip(self.gdtf.iter()) {
            f.gdtf = saved.clone();
            f.sync_mode();
        }
        *selection = self.selection.clone();
    }
}

/// One reversible edit: a name for the menu label plus both document ends.
pub struct UndoStep {
    pub name: String,
    before: DocSnapshot,
    after: DocSnapshot,
}

/// The undo / redo history. `cursor` is the number of steps currently APPLIED
/// (so `steps[cursor - 1]` is the last applied edit and `steps[cursor]` is the
/// next redoable one). Pushing a new edit truncates any redo tail.
#[derive(Default)]
pub struct UndoStack {
    steps: Vec<UndoStep>,
    cursor: usize,
    /// The last registered op that ran — see [`LastOp`] (set by `run_op`).
    last_op: Option<LastOp>,
}

impl UndoStack {
    /// Capture the document state to use as a step's `before` end. Call this
    /// BEFORE running a mutation, then pair it with [`push`](Self::push) after.
    pub fn begin(
        &self,
        scene: &Scene,
        patch: &PatchTable,
        cues: &CueEngine,
        groups: &[SelectionGroup],
        selection: &Selection,
    ) -> DocSnapshot {
        capture(scene, patch, cues, groups, selection)
    }

    /// Record a finished edit: `before` (from [`begin`](Self::begin)) and `after`
    /// (the post-mutation state). Truncates the redo tail, then enforces the step
    /// + byte caps by dropping from the oldest end.
    pub fn push(&mut self, name: impl Into<String>, before: DocSnapshot, after: DocSnapshot) {
        // Drop any redo tail past the cursor — a new edit forks the history.
        self.steps.truncate(self.cursor);
        self.steps.push(UndoStep { name: name.into(), before, after });
        self.cursor = self.steps.len();
        self.enforce_limits();
    }

    /// Replace the `after` end of the most-recently-pushed step in place, keeping
    /// its `before` and name. Used to COALESCE a burst of like edits (arrow-key
    /// nudges within a short window) into a single undo step: the first nudge
    /// [`push`](Self::push)es a step, each follow-up nudge amends its `after` so
    /// one undo reverts the whole burst back to the pre-burst `before`. No-op when
    /// the cursor isn't at the top (a redo tail exists / nothing pushed yet).
    pub fn amend_after(&mut self, after: DocSnapshot) -> bool {
        if self.cursor == 0 || self.cursor != self.steps.len() {
            return false;
        }
        if let Some(step) = self.steps.last_mut() {
            step.after = after;
            self.enforce_limits();
            true
        } else {
            false
        }
    }

    /// Name of the most-recently-pushed (top) step, if the cursor is at the top.
    /// Used by nudge coalescing to check whether the top step is the burst to
    /// extend (vs an unrelated edit that should not be amended).
    pub fn top_name(&self) -> Option<&str> {
        if self.cursor != self.steps.len() {
            return None;
        }
        self.steps.last().map(|s| s.name.as_str())
    }

    /// Drop oldest steps until both the count and byte caps are satisfied.
    fn enforce_limits(&mut self) {
        while self.steps.len() > LIMIT_STEPS {
            self.steps.remove(0);
            self.cursor = self.cursor.saturating_sub(1);
        }
        let mut total: usize = self.steps.iter().map(|s| s.before.bytes() + s.after.bytes()).sum();
        while total > LIMIT_BYTES && self.steps.len() > 1 {
            let dropped = self.steps.remove(0);
            total -= dropped.before.bytes() + dropped.after.bytes();
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    /// Whether an [`undo`](Self::undo) would do anything.
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    /// Whether a [`redo`](Self::redo) would do anything.
    pub fn can_redo(&self) -> bool {
        self.cursor < self.steps.len()
    }

    /// Name of the edit that [`undo`](Self::undo) would reverse (for the menu).
    pub fn undo_name(&self) -> Option<&str> {
        self.cursor.checked_sub(1).and_then(|i| self.steps.get(i)).map(|s| s.name.as_str())
    }

    /// Name of the edit that [`redo`](Self::redo) would re-apply (for the menu).
    pub fn redo_name(&self) -> Option<&str> {
        self.steps.get(self.cursor).map(|s| s.name.as_str())
    }

    /// Step back one edit: restore the `before` end of the last applied step.
    pub fn undo(
        &mut self,
        scene: &mut Scene,
        patch: &mut PatchTable,
        cues: &mut CueEngine,
        groups: &mut Vec<SelectionGroup>,
        selection: &mut Selection,
    ) {
        if !self.can_undo() {
            return;
        }
        self.cursor -= 1;
        self.steps[self.cursor].before.restore(scene, patch, cues, groups, selection);
    }

    /// Step forward one edit: restore the `after` end of the next step.
    pub fn redo(
        &mut self,
        scene: &mut Scene,
        patch: &mut PatchTable,
        cues: &mut CueEngine,
        groups: &mut Vec<SelectionGroup>,
        selection: &mut Selection,
    ) {
        if !self.can_redo() {
            return;
        }
        self.steps[self.cursor].after.restore(scene, patch, cues, groups, selection);
        self.cursor += 1;
    }
}

// ===========================================================================
// The operator pipeline (P1b).
//
// Generalises the four ad-hoc capture→mutate→push sites from P1a onto a single
// Blender-style rule: an operator declares whether it pushes undo / registers as
// the last op, [`Ui::run_op`](super::Ui::run_op) snapshots BEFORE, runs `exec`,
// and — only if `exec` returns [`OpStatus::Finished`] — pushes (before, after).
// A `Cancelled` op pushes nothing. Modal operators (the viewport G/R/S
// transform) are driven outside this loop: invoke captures `before`, the modal
// re-applies live each frame with no push, confirm pushes, Esc restores `before`.
// ===========================================================================

/// What the operator framework should do around an [`Operator::exec`] that
/// returns [`OpStatus::Finished`]. Mirrors Blender's `OPTYPE_*` subset we use.
/// A tiny hand-rolled bitset (no `bitflags` dep): combine with `|`, test with
/// [`contains`](Self::contains).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OpFlags(u8);

impl OpFlags {
    /// Push a (before, after) step onto the undo stack after a finished exec.
    pub const UNDO: OpFlags = OpFlags(0b001);
    /// Record this as the "last op" (for a future redo-last / adjust panel).
    pub const REGISTER: OpFlags = OpFlags(0b010);
    /// Internal/helper op — never surfaced in menus or the last-op slot. No
    /// migrated op carries it yet (scaffold for non-undoable helper ops).
    #[allow(dead_code)]
    pub const INTERNAL: OpFlags = OpFlags(0b100);

    /// Whether every bit in `other` is set in `self`.
    pub fn contains(self, other: OpFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for OpFlags {
    type Output = OpFlags;
    fn bitor(self, rhs: OpFlags) -> OpFlags {
        OpFlags(self.0 | rhs.0)
    }
}

/// The outcome of running an [`Operator::exec`]. Only `Finished` triggers the
/// undo push; `Cancelled` leaves the document (and history) untouched.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpStatus {
    /// The edit completed and mutated the document — push undo (if `UNDO`).
    Finished,
    /// The op declined / was aborted — no mutation, no undo push.
    Cancelled,
    /// A modal op is still running (driven outside `run_op`, e.g. the viewport
    /// transform, which is dispatched in `panels::viewport`, not through an
    /// `Operator`). No migrated op returns it yet — it's the framework contract
    /// for a future modal `Operator`.
    #[allow(dead_code)]
    RunningModal,
    /// The op did nothing and the event should fall through to other handlers.
    PassThrough,
}

/// The mutable document surface an operator edits. Bundles exactly the parts a
/// [`DocSnapshot`] captures, plus the read-only content [`Library`] some ops
/// (Add) pull profiles from. Field-disjoint borrows assembled by the caller.
pub struct OpCtx<'a> {
    pub scene: &'a mut Scene,
    pub patch: &'a mut PatchTable,
    pub cues: &'a mut CueEngine,
    pub groups: &'a mut Vec<SelectionGroup>,
    pub selection: &'a mut Selection,
    pub library: &'a Library,
}

/// A registered command. The four migrated sites use closure-ops
/// ([`Ui::run_op`](super::Ui::run_op) takes the `id` / `flags` / closure inline)
/// rather than one struct per command; this trait is the shape the framework
/// dispatches and the spec ([`docs/RESEARCH-blender-framework.md`] §2.1) names.
pub trait Operator {
    /// Stable identifier (e.g. `"object.delete"`), for the last-op slot + logs.
    fn id(&self) -> &'static str;
    /// Human label for the undo step + menus (e.g. `"Delete"`).
    fn label(&self) -> &'static str;
    /// Undo / register / internal behaviour.
    fn flags(&self) -> OpFlags;
    /// Whether the op may run right now (e.g. a selection exists). A false poll
    /// is a no-op: no snapshot, no exec, no push.
    fn poll(&self, cx: &OpCtx) -> bool;
    /// Perform the edit and report what happened.
    fn exec(&mut self, cx: &mut OpCtx) -> OpStatus;
}

/// An [`Operator`] built from an inline closure — the lightweight path the four
/// migrated mutators (Add / Delete / Patch / Unpatch / Duplicate) use instead of
/// one named struct each. [`Ui::run_op`](super::Ui::run_op) wraps its arguments
/// in this and dispatches it through [`Ui::run_operator`](super::Ui::run_operator),
/// so the same trait drives both inline and (future) struct operators. `poll` is
/// pre-computed by the caller (it usually depends on UI state the closure can't
/// see), so the trait `poll` just returns it.
pub struct ClosureOp<F: FnOnce(&mut OpCtx) -> OpStatus> {
    pub id: &'static str,
    pub label: &'static str,
    pub flags: OpFlags,
    pub poll: bool,
    /// `Option` so `exec` can take the `FnOnce` by value exactly once.
    pub exec: Option<F>,
}

impl<F: FnOnce(&mut OpCtx) -> OpStatus> Operator for ClosureOp<F> {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn flags(&self) -> OpFlags {
        self.flags
    }
    fn poll(&self, _cx: &OpCtx) -> bool {
        self.poll
    }
    fn exec(&mut self, cx: &mut OpCtx) -> OpStatus {
        match self.exec.take() {
            Some(f) => f(cx),
            None => OpStatus::Cancelled,
        }
    }
}

/// The last successfully-run registering operator. Read at runtime (P1d) by the
/// F9 "Adjust Last Operation" affordance, which re-invokes this op by `id`.
#[derive(Clone)]
pub struct LastOp {
    pub id: &'static str,
    pub label: String,
}

impl UndoStack {
    /// The last registered op that ran (`REGISTER` flag), if any — drives the F9
    /// adjust-last-operation affordance.
    pub fn last_op(&self) -> Option<&LastOp> {
        self.last_op.as_ref()
    }

    /// Record the last registered op (called by [`Ui::run_op`](super::Ui::run_op)).
    pub fn set_last_op(&mut self, id: &'static str, label: impl Into<String>) {
        self.last_op = Some(LastOp { id, label: label.into() });
    }
}

// ===========================================================================
// The operator catalog (P1d) — the searchable registry of REGISTER operators.
//
// The migrated mutators are inline closures at their commit sites in
// `Ui::show` (they need egui / dialog state the catalog can't hold), so the
// catalog does NOT own the closures: it is a static table of *descriptors*
// (id + label + category + how to invoke) that the F3 operator-search palette
// and the F9 adjust-last affordance render + dispatch. `Ui::run_catalog_op`
// maps a descriptor's `id` back to the real run-site (direct run vs re-open the
// op's dialog), so there is still exactly ONE place each op actually executes.
// ===========================================================================

/// How the operator-search palette / adjust-last affordance should INVOKE a
/// catalog entry. Parameterized ops re-open their dialog (Blender's "adjust last
/// operation": tweak params, then undo-previous + re-exec); direct ops just run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpInvoke {
    /// Run immediately through `run_op` (no parameters to tweak).
    Direct,
    /// Open the op's parameter dialog (Duplicate / Patch); the dialog's confirm
    /// runs the op as usual. This is the "adjust last operation" path.
    Dialog,
}

/// A searchable / re-invokable operator descriptor. Mirrors the `id` / `label` a
/// [`ClosureOp`] carries at its run site, plus the cheat-sheet [`category`] and
/// the [`invoke`] kind the palette + F9 use to dispatch it.
#[derive(Clone, Copy)]
pub struct CatalogOp {
    pub id: &'static str,
    pub label: &'static str,
    pub category: super::shortcuts::Category,
    pub invoke: OpInvoke,
}

/// Every REGISTER operator the F3 search palette lists, in display order. The
/// `id`s match the ones the run sites pass to [`Ui::run_op`](super::Ui::run_op),
/// so [`Ui::run_catalog_op`](super::Ui::run_catalog_op) can route each back to
/// its single real invocation point.
pub static CATALOG: &[CatalogOp] = &[
    CatalogOp {
        id: "object.add",
        label: "Add Object…",
        category: super::shortcuts::Category::Add,
        invoke: OpInvoke::Dialog,
    },
    CatalogOp {
        id: "fixture.duplicate",
        label: "Duplicate / Array…",
        category: super::shortcuts::Category::Object,
        invoke: OpInvoke::Dialog,
    },
    CatalogOp {
        id: "fixture.patch",
        label: "Patch Fixtures…",
        category: super::shortcuts::Category::Object,
        invoke: OpInvoke::Dialog,
    },
    CatalogOp {
        id: "fixture.unpatch",
        label: "Unpatch Fixtures",
        category: super::shortcuts::Category::Object,
        invoke: OpInvoke::Direct,
    },
    CatalogOp {
        id: "object.delete",
        label: "Delete Selected",
        category: super::shortcuts::Category::Object,
        invoke: OpInvoke::Direct,
    },
];

/// Look up a catalog entry by its operator `id` (for F9 adjust-last).
pub fn catalog_op(id: &str) -> Option<&'static CatalogOp> {
    CATALOG.iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Mat4, Vec3};

    use crate::gdtf::{
        BeamData, DmxMode, EmitterDef, GdtfFixture, Geometry, GeometryKind,
    };
    use crate::scene::fixture::Fixture;
    use crate::scene::Library;

    /// A minimal GDTF with two emitters so a fixture built from it carries two
    /// `cells` — the per-mode state that `sync_mode` must rebuild on restore.
    fn test_gdtf() -> GdtfFixture {
        let geometry = Geometry {
            name: "Base".into(),
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
            merged_into: None,
        };
        GdtfFixture {
            name: "Test".into(),
            manufacturer: "Test".into(),
            long_name: "Test".into(),
            short_name: "T".into(),
            description: String::new(),
            thumbnail: None,
            wheels: Vec::new(),
            models: Vec::new(),
            geometry: geometry.clone(),
            roots: vec![geometry.clone()],
            modes: vec![DmxMode {
                name: "Standard".into(),
                geometry: "Base".into(),
                channels: Vec::new(),
                emitters: vec![emitter("Cell 1"), emitter("Cell 2")],
                resolved: Vec::new(),
                components: Vec::new(),
                footprint: 0,
            }],
            beam_angle: 15.0,
            beam: BeamData::default(),
            spec: String::new(),
            raw: None,
        }
    }

    /// add → undo restores the fixture count, and a GDTF-backed fixture keeps its
    /// parsed-GDTF link + per-mode cells across both undo AND redo (the trap).
    #[test]
    fn add_undo_redo_preserves_gdtf_link_and_cells() {
        let mut scene = Scene::default();
        let mut patch = PatchTable::default();
        let mut cues = CueEngine::default();
        let mut groups: Vec<SelectionGroup> = Vec::new();
        let mut selection = Selection::default();
        let mut stack = UndoStack::default();

        let base = scene.fixtures.len();

        // Add a GDTF-backed fixture (the mutation we wrap with undo).
        let before = stack.begin(&scene, &patch, &cues, &groups, &selection);
        let gdtf = Arc::new(test_gdtf());
        scene.fixtures.push(Fixture::from_gdtf(gdtf, "GDTF Fix", Vec3::ZERO));
        selection = Selection::fixture(scene.fixtures.len() - 1);
        let after = stack.begin(&scene, &patch, &cues, &groups, &selection);
        stack.push("Add Fixture", before, after);

        assert_eq!(scene.fixtures.len(), base + 1);
        assert!(scene.fixtures.last().unwrap().is_gdtf());
        assert_eq!(scene.fixtures.last().unwrap().cells.len(), 2);

        // Undo: fixture gone, count back to base.
        stack.undo(&mut scene, &mut patch, &mut cues, &mut groups, &mut selection);
        assert_eq!(scene.fixtures.len(), base);

        // Redo: fixture back, GDTF link reattached out of band, cells rebuilt.
        stack.redo(&mut scene, &mut patch, &mut cues, &mut groups, &mut selection);
        assert_eq!(scene.fixtures.len(), base + 1);
        let f = scene.fixtures.last().unwrap();
        assert!(f.is_gdtf(), "GDTF Arc reattached after redo");
        assert_eq!(f.cells.len(), 2, "per-mode cells rebuilt by sync_mode");
    }

    /// The stack caps the number of retained steps (oldest dropped past the cap).
    #[test]
    fn stack_caps_step_count() {
        let scene = Scene::default();
        let patch = PatchTable::default();
        let cues = CueEngine::default();
        let groups: Vec<SelectionGroup> = Vec::new();
        let selection = Selection::default();
        let mut stack = UndoStack::default();
        for i in 0..(LIMIT_STEPS + 10) {
            let b = stack.begin(&scene, &patch, &cues, &groups, &selection);
            let a = stack.begin(&scene, &patch, &cues, &groups, &selection);
            stack.push(format!("edit {i}"), b, a);
        }
        assert!(stack.steps.len() <= LIMIT_STEPS, "step count capped");
        assert!(stack.can_undo());
    }

    /// A trivial struct-operator that adds one (empty) fixture, used to drive the
    /// push-after-Finished rule the same way `Ui::run_operator` does.
    struct AddBlankFixture {
        status: OpStatus,
    }
    impl Operator for AddBlankFixture {
        fn id(&self) -> &'static str {
            "test.add_blank"
        }
        fn label(&self) -> &'static str {
            "Add Blank"
        }
        fn flags(&self) -> OpFlags {
            OpFlags::UNDO | OpFlags::REGISTER
        }
        fn poll(&self, _cx: &OpCtx) -> bool {
            true
        }
        fn exec(&mut self, cx: &mut OpCtx) -> OpStatus {
            if self.status == OpStatus::Finished {
                cx.scene.fixtures.push(Fixture::from_gdtf(
                    Arc::new(test_gdtf()),
                    "Op Fix",
                    Vec3::ZERO,
                ));
            }
            self.status
        }
    }

    /// Driver mirroring `Ui::run_operator`'s push-after-Finished rule, so the
    /// operator pipeline is exercised without the egui-bound `Ui`.
    fn drive(
        op: &mut impl Operator,
        stack: &mut UndoStack,
        scene: &mut Scene,
        patch: &mut PatchTable,
        cues: &mut CueEngine,
        groups: &mut Vec<SelectionGroup>,
        selection: &mut Selection,
    ) -> OpStatus {
        let mut cx = OpCtx { scene, patch, cues, groups, selection, library: &Library::standard() };
        if !op.poll(&cx) {
            return OpStatus::PassThrough;
        }
        let before = capture(cx.scene, cx.patch, cx.cues, cx.groups, cx.selection);
        let status = op.exec(&mut cx);
        if status == OpStatus::Finished {
            let after = capture(cx.scene, cx.patch, cx.cues, cx.groups, cx.selection);
            if op.flags().contains(OpFlags::UNDO) {
                stack.push(op.label(), before, after);
            }
            if op.flags().contains(OpFlags::REGISTER) {
                stack.set_last_op(op.id(), op.label());
            }
        }
        status
    }

    /// A Finished operator pushes one undo step + records the last op; a Cancelled
    /// / RunningModal one pushes nothing and leaves the document untouched.
    #[test]
    fn operator_push_after_finished_rule() {
        let mut scene = Scene::default();
        let mut patch = PatchTable::default();
        let mut cues = CueEngine::default();
        let mut groups: Vec<SelectionGroup> = Vec::new();
        let mut selection = Selection::default();
        let mut stack = UndoStack::default();
        let base = scene.fixtures.len();

        // Finished → mutates + pushes + registers.
        let mut op = AddBlankFixture { status: OpStatus::Finished };
        let st = drive(&mut op, &mut stack, &mut scene, &mut patch, &mut cues, &mut groups, &mut selection);
        assert_eq!(st, OpStatus::Finished);
        assert_eq!(scene.fixtures.len(), base + 1);
        assert_eq!(stack.steps.len(), 1);
        assert_eq!(stack.last_op().map(|l| l.id), Some("test.add_blank"));

        // Cancelled → no mutation, no push.
        let mut op = AddBlankFixture { status: OpStatus::Cancelled };
        let st = drive(&mut op, &mut stack, &mut scene, &mut patch, &mut cues, &mut groups, &mut selection);
        assert_eq!(st, OpStatus::Cancelled);
        assert_eq!(scene.fixtures.len(), base + 1, "Cancelled op mutated nothing");
        assert_eq!(stack.steps.len(), 1, "Cancelled op pushed no step");

        // RunningModal → likewise no push (the modal op pushes itself on confirm).
        let mut op = AddBlankFixture { status: OpStatus::RunningModal };
        drive(&mut op, &mut stack, &mut scene, &mut patch, &mut cues, &mut groups, &mut selection);
        assert_eq!(stack.steps.len(), 1, "RunningModal pushed no step");

        // Undo reverts the one Finished step.
        stack.undo(&mut scene, &mut patch, &mut cues, &mut groups, &mut selection);
        assert_eq!(scene.fixtures.len(), base);
    }

    /// `OpFlags` is a real bitset: combined flags contain each member, and the
    /// `INTERNAL` bit is distinct from the others.
    #[test]
    fn op_flags_bitset() {
        let f = OpFlags::UNDO | OpFlags::REGISTER;
        assert!(f.contains(OpFlags::UNDO));
        assert!(f.contains(OpFlags::REGISTER));
        assert!(!f.contains(OpFlags::INTERNAL));
        assert!(OpFlags::INTERNAL.contains(OpFlags::INTERNAL));
    }

    /// The operator catalog is consistent: every entry's `id` matches a known run
    /// site, lookup round-trips, and the parameterized ops are flagged `Dialog`.
    #[test]
    fn catalog_lookup_and_invoke_kinds() {
        // Lookup round-trips for a known id and returns None for an unknown one.
        assert!(catalog_op("fixture.duplicate").is_some());
        assert!(catalog_op("nope.missing").is_none());
        // The parameterized ops open dialogs; the rest run directly.
        assert_eq!(catalog_op("fixture.duplicate").map(|c| c.invoke), Some(OpInvoke::Dialog));
        assert_eq!(catalog_op("fixture.patch").map(|c| c.invoke), Some(OpInvoke::Dialog));
        assert_eq!(catalog_op("object.add").map(|c| c.invoke), Some(OpInvoke::Dialog));
        assert_eq!(catalog_op("object.delete").map(|c| c.invoke), Some(OpInvoke::Direct));
        assert_eq!(catalog_op("fixture.unpatch").map(|c| c.invoke), Some(OpInvoke::Direct));
        // No duplicate ids in the catalog (the palette would list one twice).
        for (i, a) in CATALOG.iter().enumerate() {
            for b in CATALOG.iter().skip(i + 1) {
                assert_ne!(a.id, b.id, "duplicate catalog id {}", a.id);
            }
        }
    }

    /// A burst of nudges coalesces into ONE step: the first pushes, follow-ups
    /// amend the top `after`, so a single undo reverts the whole burst.
    #[test]
    fn nudge_coalesce_into_one_step() {
        let scene = Scene::default();
        let patch = PatchTable::default();
        let cues = CueEngine::default();
        let groups: Vec<SelectionGroup> = Vec::new();
        let selection = Selection::default();
        let mut stack = UndoStack::default();

        // First nudge of the burst pushes a step named "Nudge".
        let before = stack.begin(&scene, &patch, &cues, &groups, &selection);
        let after = stack.begin(&scene, &patch, &cues, &groups, &selection);
        stack.push("Nudge", before, after);
        assert_eq!(stack.steps.len(), 1);
        assert_eq!(stack.top_name(), Some("Nudge"));

        // Follow-up nudges amend the top `after` rather than pushing more steps.
        for _ in 0..5 {
            assert_eq!(stack.top_name(), Some("Nudge"), "still the burst");
            let after = stack.begin(&scene, &patch, &cues, &groups, &selection);
            assert!(stack.amend_after(after), "amends the top step in place");
        }
        assert_eq!(stack.steps.len(), 1, "burst coalesced into one step");
    }
}
