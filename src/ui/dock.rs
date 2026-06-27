use super::*;

impl Ui {
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
    pub(super) fn is_tab_open(&self, tab: Tab) -> bool {
        self.dock.find_tab(&tab).is_some()
    }

    /// Show or hide a dock tab.
    pub(super) fn toggle_tab(&mut self, tab: Tab) {
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
    pub(super) fn activate_workspace(&mut self, idx: usize) {
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

    /// The workspace tab strip (Blender's workspace tabs / depence's preset row):
    /// one selectable tab per saved workspace + a "+" to save the current arrangement
    /// as a new workspace. Clicking a tab activates that workspace (layout + tool +
    /// overlay emphasis; NO locking). Reserved below the menu bar, above the dock.
    pub(super) fn workspace_strip(&mut self, ctx: &egui::Context) {
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
    pub(super) fn save_workspace_dialog(&mut self, ctx: &egui::Context) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
