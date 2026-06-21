//! egui + egui_dock setup: the dock layout and the [`TabViewer`] that routes
//! each dock panel to its drawing function in [`panels`].

mod panels;
mod windows;

use std::collections::HashMap;

use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
use glam::Vec3;

use crate::renderer::camera::{CameraView, OrbitCamera};
use crate::scene::{Library, RenderSettings, Scene, Selection};
use windows::{Preferences, ProfileEditor};

/// State of the open Duplicate dialog (the `d`-key array tool). `None` = closed.
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

/// egui textures decoded from a GDTF fixture's images (thumbnail + wheel slot
/// media), cached per fixture type so they load once.
#[derive(Default)]
pub struct GdtfTextures {
    pub thumbnail: Option<egui::TextureHandle>,
    /// `wheels[wheel_index][slot_index]`.
    pub wheels: Vec<Vec<Option<egui::TextureHandle>>>,
}

/// The set of dockable panels. Plain enum — each variant is one tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
}

impl Tab {
    /// Panels shown in the Window menu (Viewport is fixed, so excluded there).
    const TOGGLEABLE: [Tab; 6] = [
        Tab::Scene,
        Tab::Library,
        Tab::Inspector,
        Tab::DmxMonitor,
        Tab::Patch,
        Tab::Connectivity,
    ];

    fn title(self) -> &'static str {
        match self {
            Tab::Viewport => "Viewport",
            Tab::Scene => "Scene",
            Tab::Library => "Library",
            Tab::Inspector => "Inspector",
            Tab::DmxMonitor => "DMX Universe",
            Tab::Connectivity => "Connectivity",
            Tab::Patch => "Patch",
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
    /// Whether the 3D viewport currently has interaction focus (drives the focus
    /// border and whether the `d` shortcut opens the Duplicate dialog).
    viewport_focused: bool,
    /// The open Duplicate dialog, if any.
    duplicate: Option<DuplicateDialog>,
    /// Open floating windows.
    show_prefs: bool,
    show_about: bool,
    show_shortcuts: bool,
    profile: Option<ProfileEditor>,
}

impl Ui {
    pub fn new() -> Self {
        Self {
            dock: Self::default_dock(),
            library: Library::standard(),
            gdtf_textures: HashMap::new(),
            selection: Selection::fixture(0),
            settings: RenderSettings::default(),
            prefs: Preferences::default(),
            requested_viewport_px: (1, 1),
            viewport_focused: false,
            duplicate: None,
            show_prefs: false,
            show_about: false,
            show_shortcuts: false,
            profile: None,
        }
    }

    /// The default dock layout (also used by Window ▸ Reset Panel Layout).
    fn default_dock() -> DockState<Tab> {
        // Central "Viewport", then split off the surrounding panels.
        // `fraction` is the share the *old* (central) node keeps after a split.
        let mut dock = DockState::new(vec![Tab::Viewport]);
        let surface = dock.main_surface_mut();
        let [center, _left] =
            surface.split_left(NodeIndex::root(), 0.80, vec![Tab::Scene, Tab::Library]);
        let [center, _inspector] = surface.split_right(center, 0.76, vec![Tab::Inspector]);
        let [_viewport, _dmx] = surface.split_below(
            center,
            0.74,
            vec![Tab::DmxMonitor, Tab::Patch, Tab::Connectivity],
        );
        dock
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
    ) {
        // Theme/accent/DPI live every frame (cheap; egui dedups).
        self.prefs.apply_theme(ctx);
        // Global shortcuts + drag-dropped .gdtf/.mvr files.
        self.handle_shortcuts(ctx, scene, camera);
        self.handle_dropped_files(ctx, scene, camera);

        // Chrome MUST be reserved before the dock (it fills the CentralPanel).
        self.menu_bar(ctx, scene, camera, dmx);
        self.status_bar(ctx, scene, dmx, fps);

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
            dmx_patch: dmxv.patch,
            dmx_snapshot: dmxv.snapshot,
            dmx_status: dmxv.status,
            dmx_config: dmxv.config,
            dmx_live_mask: dmxv.live_mask,
            dmx_selected_universe: dmxv.selected_universe,
            dmx_bind_ip_text: dmxv.bind_ip_text,
            dmx_universes_text: dmxv.universes_text,
            dmx_pending: dmxv.pending,
            dmx_running: dmxv.running,
            fps,
        };

        DockArea::new(&mut self.dock).show(ctx, &mut viewer);

        // Floating windows (viewer borrows released — scene/selection free again).
        duplicate_window(ctx, scene, &mut self.selection, &mut self.duplicate);
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
        );
        windows::about_window(ctx, &mut self.show_about);
        windows::shortcuts_window(ctx, &mut self.show_shortcuts);
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
            self.dock.push_to_focused_leaf(tab);
        }
    }

    /// AABB of the current selection (or whole scene if nothing selected),
    /// padded a little, for the Frame commands.
    fn frame_bounds(&self, scene: &Scene, selection_only: bool) -> Option<(Vec3, Vec3)> {
        let idx: Vec<usize> = if selection_only && !self.selection.fixtures.is_empty() {
            self.selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect()
        } else {
            (0..scene.fixtures.len()).collect()
        };
        let mut pts: Vec<Vec3> = idx.iter().map(|&i| scene.fixtures[i].position).collect();
        if !selection_only {
            pts.extend(scene.geometry.iter().map(|g| g.transform.w_axis.truncate()));
        }
        let first = *pts.first()?;
        let (mut lo, mut hi) = (first, first);
        for p in &pts {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let pad = Vec3::splat(1.0);
        Some((lo - pad, hi + pad))
    }

    /// Keyboard shortcuts handled globally (camera framing/views, delete, etc.).
    fn handle_shortcuts(&mut self, ctx: &egui::Context, scene: &mut Scene, camera: &mut OrbitCamera) {
        // Don't steal keys while a text field has focus.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let del = ctx.input(|i| {
            use egui::Key;
            if i.key_pressed(Key::F) {
                let sel = !i.modifiers.shift;
                if let Some((lo, hi)) = self.frame_bounds(scene, sel) {
                    camera.frame_aabb(lo, hi);
                }
            }
            if i.modifiers.command && i.key_pressed(Key::Comma) {
                self.show_prefs = true;
            }
            for (key, view) in [
                (Key::Num5, CameraView::Perspective),
                (Key::Num7, CameraView::Top),
                (Key::Num1, CameraView::Front),
                (Key::Num3, CameraView::Right),
            ] {
                if i.key_pressed(key) {
                    camera.set_view(view);
                }
            }
            i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)
        });
        if del && !self.selection.fixtures.is_empty() {
            self.delete_selected(scene);
        }
    }

    /// Remove every selected fixture and clear the selection.
    fn delete_selected(&mut self, scene: &mut Scene) {
        let mut ids = self.selection.fixtures.clone();
        ids.sort_unstable();
        for &i in ids.iter().rev() {
            if i < scene.fixtures.len() {
                scene.fixtures.remove(i);
            }
        }
        self.selection = Selection::default();
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
                        let f = scene.add_gdtf(arc, Vec3::new(0.0, 4.0, 0.0));
                        self.selection = Selection::fixture(f);
                    }
                    Err(e) => log::error!("drop GDTF: {e}"),
                },
                Some(ext) if ext == "mvr" => match crate::mvr::MvrImport::load_path(&path) {
                    Ok(import) => {
                        scene.import_mvr(import);
                        if let Some((c, r)) = scene.scene_frame() {
                            camera.frame(c, r * 1.15);
                        }
                        self.selection = Selection::default();
                    }
                    Err(e) => log::error!("drop MVR: {e}"),
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
    fn menu_bar(
        &mut self,
        ctx: &egui::Context,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        _dmx: &mut crate::dmx::DmxIo,
    ) {
        egui::TopBottomPanel::top("menu-bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Import GDTF Fixture…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("GDTF", &["gdtf"]).pick_file() {
                            if let Ok(idx) = self.library.import_gdtf(&path) {
                                let arc = self.library.gdtf[idx].clone();
                                let f = scene.add_gdtf(arc, Vec3::new(0.0, 4.0, 0.0));
                                self.selection = Selection::fixture(f);
                            }
                        }
                        ui.close();
                    }
                    if ui.button("Import MVR Scene…").clicked() {
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
                    if ui.add_enabled(can_export, egui::Button::new("Export MVR Scene…")).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("MVR", &["mvr"]).set_file_name("scene.mvr").save_file() {
                            if let Err(e) = crate::mvr::export_path(scene, &path) {
                                log::error!("export MVR: {e}");
                            }
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Preferences…").clicked() {
                        self.show_prefs = true;
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.add_enabled(self.selection.primary_fixture().is_some(), egui::Button::new("Duplicate / Array…")).clicked() {
                        if let Some(idx) = self.selection.primary_fixture() {
                            self.duplicate = Some(DuplicateDialog { fixture: idx, x: 0.0, y: 0.0, z: 0.0, y_angle: 36.0, count: 9 });
                        }
                        ui.close();
                    }
                    if ui.add_enabled(!self.selection.fixtures.is_empty(), egui::Button::new("Delete Selected")).clicked() {
                        self.delete_selected(scene);
                        ui.close();
                    }
                    if ui.button("Deselect All").clicked() {
                        self.selection = Selection::default();
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.menu_button("Camera", |ui| {
                        for v in CameraView::ALL {
                            if ui.button(v.label()).clicked() {
                                camera.set_view(v);
                                ui.close();
                            }
                        }
                    });
                    if ui.button("Frame Selection  (F)").clicked() {
                        if let Some((lo, hi)) = self.frame_bounds(scene, true) {
                            camera.frame_aabb(lo, hi);
                        }
                        ui.close();
                    }
                    if ui.button("Frame All  (Shift+F)").clicked() {
                        if let Some((lo, hi)) = self.frame_bounds(scene, false) {
                            camera.frame_aabb(lo, hi);
                        }
                        ui.close();
                    }
                    ui.separator();
                    ui.label("Display mode");
                    for m in crate::scene::ViewportMode::ALL {
                        if ui.selectable_label(self.settings.mode == m, m.label()).clicked() {
                            self.settings.mode = m;
                        }
                    }
                    ui.separator();
                    ui.checkbox(&mut self.prefs.show_labels, "Fixture labels");
                    ui.checkbox(&mut self.settings.show_grid, "Grid");
                    ui.checkbox(&mut self.prefs.show_fps, "FPS overlay");
                    ui.checkbox(&mut self.settings.show_beam_wireframes, "Beam gizmos");
                });
                ui.menu_button("Fixture", |ui| {
                    let has = self.selection.primary_fixture().map(|i| i < scene.fixtures.len() && scene.fixtures[i].is_gdtf()).unwrap_or(false);
                    if ui.add_enabled(has, egui::Button::new("Edit Profile…")).clicked() {
                        if let Some(i) = self.selection.primary_fixture() {
                            self.profile = Some(ProfileEditor::new(i));
                        }
                        ui.close();
                    }
                });
                ui.menu_button("Window", |ui| {
                    for tab in Tab::TOGGLEABLE {
                        let mut open = self.is_tab_open(tab);
                        if ui.checkbox(&mut open, tab.title()).changed() {
                            self.toggle_tab(tab);
                        }
                    }
                    ui.separator();
                    if ui.button("Reset Panel Layout").clicked() {
                        self.dock = Self::default_dock();
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("Keyboard Shortcuts").clicked() {
                        self.show_shortcuts = true;
                        ui.close();
                    }
                    if ui.button("About previz").clicked() {
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
        &self,
        ctx: &egui::Context,
        scene: &Scene,
        dmx: &crate::dmx::DmxIo,
        fps: f32,
    ) {
        egui::TopBottomPanel::bottom("status-bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let sel = match self.selection.fixtures.len() {
                    0 => "nothing selected".to_string(),
                    1 => scene
                        .fixtures
                        .get(self.selection.fixtures[0])
                        .map(|f| format!("{} · {}", f.name, f.profile))
                        .unwrap_or_default(),
                    n => format!("{n} fixtures selected"),
                };
                ui.label(sel);
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
                    ui.colored_label(dot, "●");
                    ui.label(txt);
                    ui.separator();
                    ui.label(if self.prefs.units_feet { "ft" } else { "m" });
                });
            });
        });
    }
}

/// The modal Duplicate dialog: array the selected fixture by offset + Y angle.
fn duplicate_window(
    ctx: &egui::Context,
    scene: &mut Scene,
    selection: &mut Selection,
    dialog: &mut Option<DuplicateDialog>,
) {
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

    if do_dup {
        if let Some(d) = dialog.as_ref()
            && let Some(first) = scene.duplicate_fixture(
                d.fixture,
                Vec3::new(d.x, d.y, d.z),
                d.y_angle,
                d.count,
            )
        {
            *selection = Selection::fixture(first);
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
    // Live DMX borrows (from `DmxIo::view`).
    dmx_patch: &'a mut crate::dmx::PatchTable,
    dmx_snapshot: &'a crate::dmx::UniverseSnapshot,
    dmx_status: &'a crate::dmx::DmxStatus,
    dmx_config: &'a mut crate::dmx::DmxConfig,
    dmx_live_mask: &'a [bool],
    dmx_selected_universe: &'a mut u16,
    dmx_bind_ip_text: &'a mut String,
    dmx_universes_text: &'a mut String,
    dmx_pending: &'a mut crate::dmx::PendingNetCmd,
    dmx_running: bool,
    fps: f32,
}

impl TabViewer for PanelViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Viewport => panels::viewport(
                ui,
                self.camera,
                self.scene,
                self.selection,
                self.viewport_focused,
                self.duplicate,
                self.viewport_texture,
                self.requested_viewport_px,
                self.fps,
                self.prefs,
            ),
            Tab::Scene => panels::scene_outliner(
                ui,
                self.scene,
                self.selection,
                self.settings,
                self.dmx_patch,
                self.dmx_live_mask,
            ),
            Tab::Library => {
                panels::library_browser(ui, self.library, self.scene, self.selection, self.camera)
            }
            Tab::Inspector => {
                panels::inspector(ui, self.scene, self.selection, self.gdtf_textures, self.profile)
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
            Tab::Patch => panels::patch_editor(ui, self.scene, self.dmx_patch),
        }
    }

    /// Fixed scaffold layout — panels aren't closeable.
    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }

    /// The viewport draws an opaque image and handles its own scroll-to-zoom,
    /// so it must not be wrapped in a scroll area.
    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            Tab::Viewport => [false, false],
            _ => [true, true],
        }
    }
}
