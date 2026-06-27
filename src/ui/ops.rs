use super::*;

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

impl Ui {
    // --- undo / redo ----------------------------------------------------

    /// Capture the whole document as the `before` end of an undo step — call
    /// BEFORE running a mutation, then pair with [`undo_push`](Self::undo_push)
    /// after. Borrows `self` immutably so it can read cues/groups alongside
    /// `scene` + `patch` (the snapshot keeps the parsed-GDTF `Arc`s out of band).
    pub(super) fn undo_begin(&self, scene: &Scene, patch: &crate::dmx::PatchTable) -> op::DocSnapshot {
        self.undo.begin(scene, patch, &self.cues, &self.groups, &self.selection)
    }

    /// Record a finished edit (`before` from [`undo_begin`](Self::undo_begin),
    /// `after` = the post-mutation document).
    pub(super) fn undo_push(
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
    pub(super) fn do_undo(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
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
    pub(super) fn do_redo(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
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
    pub(super) fn run_op(
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
    pub(super) fn apply_nudge(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
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
        // Pyro devices translate their transform (nozzle = origin), same as the
        // gizmo's ObjectRef::Pyro path — so arrow-nudge moves a selected pyro too.
        for &pi in &self.selection.pyro {
            if let Some(p) = scene.pyro.get_mut(pi) {
                p.transform.w_axis += nudge.extend(0.0);
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
    pub(super) fn apply_tree_action(
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
                        } else if let Some(i) = scene.pyro_index_of(id) {
                            scene.pyro[i].hidden = !scene.pyro[i].hidden;
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
                    NodeKey::Group(GroupKind::Pyro) => {
                        let hide = scene.pyro.iter().any(|d| !d.hidden);
                        for d in &mut scene.pyro {
                            d.hidden = hide;
                        }
                        !scene.pyro.is_empty()
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
                        } else if let Some(i) = scene.pyro_index_of(id) {
                            scene.pyro[i].name = name;
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
    pub(super) fn op_runnable(&self, id: &str) -> bool {
        match id {
            // Always available (opens the entity picker).
            "object.add" => true,
            // Needs a primary fixture to duplicate.
            "fixture.duplicate" => self.selection.primary_fixture().is_some(),
            // Patch / unpatch operate on the active patchable kind (fixtures or pyro).
            "fixture.patch" | "fixture.unpatch" => {
                patchable_count(&self.selection, self.selection.active_kind()) > 0
            }
            // Delete acts on any selected entity (fixtures / geometry / screens / pyro).
            "object.delete" => {
                !self.selection.fixtures.is_empty()
                    || !self.selection.geometry.is_empty()
                    || !self.selection.screens.is_empty()
                    || !self.selection.pyro.is_empty()
            }
            // Renumber works on the selection, or ALL fixtures when nothing is selected,
            // so it's always available.
            "fixture.renumber" => true,
            // Any other catalog op is runnable by default — only the ones above gate on
            // a precondition. (The old blanket `false` silently DISABLED every op not
            // listed here, e.g. `fixture.renumber`, making them look missing from the
            // palette.)
            _ => true,
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
                    let kind = self.selection.active_kind();
                    let count = patchable_count(&self.selection, kind);
                    if count > 0 {
                        let (u, a) = next_free_slot(dmx.patch_mut(), scene);
                        self.patch_dialog = windows::PatchDialog {
                            open: true,
                            count,
                            kind,
                            start_universe: u,
                            start_address: a,
                        };
                    }
                }
                _ => {}
            },
            // Direct — run immediately through the operator pipeline.
            op::OpInvoke::Direct => match id {
                "fixture.unpatch" => {
                    let kind = self.selection.active_kind();
                    self.run_op(
                        "fixture.unpatch",
                        "Unpatch",
                        op::OpFlags::UNDO | op::OpFlags::REGISTER,
                        scene,
                        dmx,
                        true,
                        |cx| {
                            unpatch_selection(cx, kind);
                            op::OpStatus::Finished
                        },
                    );
                }
                "fixture.renumber" => {
                    self.run_op(
                        "fixture.renumber",
                        "Renumber Sequence",
                        op::OpFlags::UNDO | op::OpFlags::REGISTER,
                        scene,
                        dmx,
                        true,
                        |cx| {
                            // The selected fixtures, or ALL when none are selected.
                            let sel: Vec<usize> = cx
                                .selection
                                .fixtures
                                .iter()
                                .copied()
                                .filter(|&i| i < cx.scene.fixtures.len())
                                .collect();
                            cx.scene.renumber_sequences_by_position(&sel);
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
    pub(super) fn run_palette_command(
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
    pub(super) fn adjust_last_op(&mut self, ctx: &egui::Context, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
        let Some((id, label)) = self.undo.last_op().map(|l| (l.id, l.label.clone())) else {
            return;
        };
        if self.undo.undo_name() == Some(label.as_str()) {
            self.do_undo(scene, dmx);
        }
        self.run_catalog_op(ctx, id, scene, dmx);
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

    /// Commit a requested fixture deletion through the operator pipeline. The
    /// actual remap-in-lock-step body is the free [`delete_selection`] operator
    /// (so it edits the shared [`OpCtx`] surface); this wrapper clears the request
    /// flag, runs it under [`run_op`](Self::run_op) (which snapshots + pushes
    /// undo), and resets the UI-only scene-anchor when something was removed.
    /// Called once after the dock, where the patch is reachable.
    pub(super) fn commit_delete(&mut self, scene: &mut Scene, dmx: &mut crate::dmx::DmxIo) {
        self.pending_delete = false;
        let poll = !self.selection.fixtures.is_empty()
            || !self.selection.geometry.is_empty()
            || !self.selection.screens.is_empty()
            || !self.selection.pyro.is_empty();
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
    // --- pyro devices: a plain removal (patched inline, not in PatchTable) ---
    let mut pyro: Vec<usize> =
        cx.selection.pyro.iter().copied().filter(|&i| i < cx.scene.pyro.len()).collect();
    pyro.sort_unstable();
    pyro.dedup();
    for &i in pyro.iter().rev() {
        cx.scene.pyro.remove(i);
    }
    if !removed.is_empty() || !geo.is_empty() || !scr.is_empty() || !pyro.is_empty() {
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
pub(super) fn next_free_slot(patch: &mut crate::dmx::PatchTable, scene: &Scene) -> (u16, u16) {
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

/// How many of the active selection are DMX-patchable via the Patch dialog
/// (fixtures + pyro; objects/screens are not patchable there → 0, guarding the
/// dialog off).
pub(super) fn patchable_count(sel: &Selection, kind: crate::scene::SelKind) -> usize {
    use crate::scene::SelKind;
    match kind {
        SelKind::Fixtures => sel.fixtures.len(),
        SelKind::Pyro => sel.pyro.len(),
        _ => 0,
    }
}

/// Sequentially patch pyro devices (inline patch, via the [`Patchable`] trait)
/// from a start slot, packing onto the next universe at 512 — the inline-kind twin
/// of [`crate::dmx::PatchTable::assign_indices`]. Pyro lives in its own universes
/// by convention, so this packs without cross-fixture clash-avoidance.
///
/// [`Patchable`]: crate::dmx::patch::Patchable
pub(super) fn patch_pyro_inline(scene: &mut Scene, ids: &[usize], start_u: u16, start_a: u16) {
    use crate::dmx::patch::Patchable;
    let (mut u, mut a) = (start_u.max(1), start_a.max(1));
    for &i in ids {
        if let Some(d) = scene.pyro.get_mut(i) {
            d.set_patch(u, a);
            a += d.footprint().max(1);
            if a > 512 {
                u += 1;
                a = 1;
            }
        }
    }
}

/// Unpatch the active patchable selection (kind-aware): fixtures clear via the
/// side PatchTable, pyro clears its inline [`PyroPatch`](crate::scene::pyro::PyroPatch)
/// through the [`Patchable`](crate::dmx::patch::Patchable) trait. The captured scene
/// snapshot covers `scene.pyro`, so undo works for both.
pub(super) fn unpatch_selection(cx: &mut op::OpCtx, kind: crate::scene::SelKind) {
    use crate::scene::SelKind;
    match kind {
        SelKind::Pyro => {
            use crate::dmx::patch::Patchable;
            for &i in &cx.selection.pyro {
                if let Some(d) = cx.scene.pyro.get_mut(i) {
                    d.clear_patch();
                }
            }
        }
        _ => {
            for &fi in &cx.selection.fixtures {
                cx.patch.unpatch(fi);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmx::DmxIo;
    use crate::scene::Scene;

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
}
