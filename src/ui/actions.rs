use super::*;
use super::ops::{next_free_slot, patchable_count};

impl Ui {
    /// AABB of the current selection (or whole scene if nothing selected),
    /// padded a little, for the Frame commands. Unified across EVERY placed kind
    /// (fixtures + geometry + screens + environments) via `Scene::object_world_bounds`,
    /// so "Frame Selected" works on a lone object/screen/env, not just fixtures.
    pub(super) fn frame_bounds(&self, scene: &Scene, selection_only: bool) -> Option<(Vec3, Vec3)> {
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
            for i in 0..scene.pyro.len() {
                grow(scene.object_world_bounds(ObjectRef::Pyro(i)));
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
        for &i in &self.selection.pyro {
            if let Some(p) = scene.pyro.get(i) {
                sum += p.transform.w_axis.truncate();
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
            box_select_active: self.box_select_armed,
        }
    }

    pub(super) fn handle_shortcuts(
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
    pub(super) fn dispatch_action(
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
        let nudge_ok = self.viewport_focused
            && self.transform.is_none()
            && (!self.selection.fixtures.is_empty() || !self.selection.pyro.is_empty());
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
            // Arm Blender box-select: the next viewport drag rubber-bands a marquee.
            // Just a flag here (single dispatch path); the viewport consumes it.
            Action::BoxSelect => self.box_select_armed = true,
            // Select All (#88): every item of the ACTIVE kind (fixtures by default;
            // objects/screens when one of those is the current selection). Mirrors
            // Blender's `A` acting on the active mode's collection.
            Action::SelectAll => {
                let counts = (scene.fixtures.len(), scene.geometry.len(), scene.screens.len(), scene.pyro.len());
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
            // `H` — hide / reveal the WHOLE selection (every kind). Toggles: hides if
            // anything is still visible, else reveals. One undo step; keeps the
            // selection so H reveals it again.
            Action::ToggleHideSelection => {
                let s = &self.selection;
                let has_sel = !s.fixtures.is_empty()
                    || !s.geometry.is_empty()
                    || !s.screens.is_empty()
                    || !s.pyro.is_empty()
                    || s.environment.is_some();
                if has_sel {
                    let any_visible = s.fixtures.iter().any(|&i| scene.fixtures.get(i).is_some_and(|f| !f.hidden))
                        || s.geometry.iter().any(|&i| scene.geometry.get(i).is_some_and(|g| !g.hidden))
                        || s.screens.iter().any(|&i| scene.screens.get(i).is_some_and(|x| !x.hidden))
                        || s.pyro.iter().any(|&i| scene.pyro.get(i).is_some_and(|p| !p.hidden))
                        || s.environment.is_some_and(|i| scene.environments.get(i).is_some_and(|e| !e.hidden));
                    let hide = any_visible; // hide while anything's visible, else reveal
                    let before = self.undo_begin(scene, dmx.patch());
                    for &i in &self.selection.fixtures {
                        if let Some(f) = scene.fixtures.get_mut(i) { f.hidden = hide; }
                    }
                    for &i in &self.selection.geometry {
                        if let Some(g) = scene.geometry.get_mut(i) { g.hidden = hide; }
                    }
                    for &i in &self.selection.screens {
                        if let Some(x) = scene.screens.get_mut(i) { x.hidden = hide; }
                    }
                    for &i in &self.selection.pyro {
                        if let Some(p) = scene.pyro.get_mut(i) { p.hidden = hide; }
                    }
                    if let Some(i) = self.selection.environment {
                        if let Some(e) = scene.environments.get_mut(i) { e.hidden = hide; }
                    }
                    self.undo_push("Hide / Reveal", before, scene, dmx.patch());
                    self.notify.success(if hide { "Hid selection" } else { "Revealed selection" });
                }
            }
            // Invert (#88): flip membership within the active kind. Defaults to
            // fixtures when nothing is selected, so a bare Ctrl+I selects everything.
            Action::SelectInvert => {
                let counts = (scene.fixtures.len(), scene.geometry.len(), scene.screens.len(), scene.pyro.len());
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
                    || !self.selection.pyro.is_empty()
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
            // Patch / Unpatch (P/U) — works for whichever patchable device kind is
            // active. Open the dialog now; its confirm commits after the dock where
            // scene + patch are both reachable.
            Action::Patch => {
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
            Action::Unpatch => {
                let kind = self.selection.active_kind();
                let count = patchable_count(&self.selection, kind);
                if count > 0 {
                    self.unpatch_dialog = windows::UnpatchDialog { open: true, count, kind };
                }
            }
            // Renumber the selected fixtures' sequence by stage position (rows then
            // columns); all fixtures if none selected.
            Action::RenumberSequence => {
                let sel: Vec<usize> =
                    self.selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
                scene.renumber_sequences_by_position(&sel);
                let n = if sel.is_empty() { scene.fixtures.len() } else { sel.len() };
                self.notify.success(format!("Renumbered {n} fixtures by position"));
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
            // Fullscreen + still-render: the app applies the toggle / the render runs
            // from these flags (mirrors the raw F11 / F12 handlers).
            Action::ToggleFullscreen => self.pending_fullscreen_toggle = true,
            Action::RenderImage => {
                if self.render.status.phase != crate::ui::render_panel::RenderPhase::Rendering {
                    self.render.request_start();
                    self.ensure_render_tab_focused();
                }
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmx::DmxIo;
    use crate::scene::{Scene, SelKind};

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
        // Patch/Unpatch are kind-aware: a pyro selection opens the dialog tagged Pyro
        // (so the confirm packs the inline PyroPatch instead of the fixture PatchTable).
        let pyro_sel: &dyn Fn(&mut Ui) = &|ui: &mut Ui| ui.selection = Selection::pyro(0);
        let (ui, _, _, h) = run(Action::Patch, pyro_sel);
        assert!(
            h && ui.patch_dialog.open && ui.patch_dialog.kind == SelKind::Pyro,
            "Patch opens tagged Pyro for a pyro selection"
        );
        let (ui, _, _, h) = run(Action::Unpatch, pyro_sel);
        assert!(
            h && ui.unpatch_dialog.open && ui.unpatch_dialog.kind == SelKind::Pyro,
            "Unpatch opens tagged Pyro for a pyro selection"
        );
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
}
