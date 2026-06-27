use super::*;

impl Ui {
    /// The top menu bar (File / Edit / View / Fixture / Window / Help).
    // egui 0.34 deprecates `Panel::show(ctx)` mid-migration (the replacement
    // `show_inside` needs a Ui, not the root Context — DockArea uses the same
    // path); the ctx-based root panel is still correct here.
    #[allow(deprecated)]
    pub(super) fn menu_bar(
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
                    if ui.button(format!("{}  About glowstone", icon::INFO)).clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    /// The bottom status bar (selection · units · DMX · fixtures · FPS).
    #[allow(deprecated)]
    pub(super) fn status_bar(
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
