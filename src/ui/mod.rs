//! egui + egui_dock setup: the dock layout and the [`TabViewer`] that routes
//! each dock panel to its drawing function in [`panels`].

mod cues;
mod panels;
pub mod theme;
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
    /// Saved looks + crossfade playback (cue list).
    Cues,
}

/// A saved, named selection of fixtures (console-style "groups"). Recalled by
/// click in the Scene › Groups folder. Indices are filtered to valid range on
/// recall, so editing the rig afterwards can't crash a recall.
#[derive(Clone)]
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
    fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
}

pub struct TransformOp {
    pub kind: TransformKind,
    pub axis: Option<Axis>,
    /// Mouse position (screen) when the op started.
    pub start_screen: egui::Pos2,
    /// Selection centroid (pivot for rotate/scale).
    pub pivot: Vec3,
    /// Original (fixture index, position, orientation) snapshot, for live re-apply
    /// each frame and for cancel/restore.
    pub start: Vec<(usize, Vec3, glam::Quat)>,
}

impl TransformOp {
    /// The on-viewport status line shown while the op is in progress.
    pub fn hint(&self) -> String {
        let ax = match self.axis {
            Some(a) => format!(" · axis {}", a.label()),
            None => String::new(),
        };
        format!(
            "{}{}   X/Y/Z lock · click/Enter confirm · Esc cancel",
            self.kind.label(),
            ax
        )
    }
}

/// A workspace layout preset, switchable from the Window menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Workspace {
    Design,
    Patch,
    Visualize,
}

impl Workspace {
    const ALL: [(Workspace, &'static str); 3] = [
        (Workspace::Design, "Design"),
        (Workspace::Patch, "Patch"),
        (Workspace::Visualize, "Visualise"),
    ];
}

impl Tab {
    /// Panels shown in the Window menu (Viewport is fixed, so excluded there).
    const TOGGLEABLE: [Tab; 7] = [
        Tab::Scene,
        Tab::Library,
        Tab::Inspector,
        Tab::DmxMonitor,
        Tab::Patch,
        Tab::Cues,
        Tab::Connectivity,
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
    /// Library panel state (search / sort / multi-select).
    lib: panels::LibState,
    /// Anchor index for shift-range selection of scene fixtures (list + 3D).
    scene_anchor: Option<usize>,
    /// Sort order for the Scene panel's Fixtures folder.
    scene_sort: panels::SceneSort,
    /// Fixture-manager (Fixtures tab) state: filter / sort / bulk values.
    fm: panels::FmState,
    /// The `s` quick-select palette is open.
    quick_select: bool,
    /// In-progress modal transform (G/R/S), if any.
    transform: Option<TransformOp>,
    /// Saved fixture selection groups + the new-group name buffer.
    groups: Vec<SelectionGroup>,
    group_name: String,
    /// The cue list + crossfade engine.
    cues: cues::CueEngine,
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
            lib: panels::LibState::default(),
            scene_anchor: None,
            scene_sort: panels::SceneSort::Patch,
            fm: panels::FmState::default(),
            quick_select: false,
            transform: None,
            groups: Vec::new(),
            group_name: String::new(),
            cues: cues::CueEngine::default(),
        }
    }

    /// Advance any in-progress cue crossfade. Called once per real frame from
    /// `app::render`, after live DMX decode and before motion advance.
    pub fn tick_cues(&mut self, scene: &mut Scene, dt: f32) {
        self.cues.tick(scene, dt);
    }

    /// The default dock layout (also used by Window ▸ Reset Panel Layout).
    ///
    /// egui_dock's `fraction` is the share given to the side being split toward:
    /// `split_left(n, f)` makes the NEW left panel `f` of the width, `split_right`
    /// makes the new right panel `1 - f`, `split_below` the new bottom `1 - f`.
    /// (The old code passed 0.80 to `split_left` expecting the central to keep
    /// 80% — that made the Scene sidebar 80% wide, the startup-layout bug.)
    fn default_dock() -> DockState<Tab> {
        Self::workspace_dock(Workspace::Design)
    }

    /// A workspace layout preset (à la Blender's workspace tabs / depence's
    /// Construction·ShowControl·Animation presets): each pre-arranges the panels
    /// for one stage of the lighting workflow.
    fn workspace_dock(ws: Workspace) -> DockState<Tab> {
        let mut dock = DockState::new(vec![Tab::Viewport]);
        let surface = dock.main_surface_mut();
        match ws {
            // DESIGN: balanced — outliner + library left, inspector right, the
            // Fixtures/DMX data as a bottom strip. The everyday layout.
            Workspace::Design => {
                let [c, _l] = surface.split_left(NodeIndex::root(), 0.17, vec![Tab::Scene, Tab::Library]);
                let [c, _i] = surface.split_right(c, 0.79, vec![Tab::Inspector]);
                surface.split_below(c, 0.70, vec![Tab::Patch, Tab::DmxMonitor, Tab::Cues, Tab::Connectivity]);
            }
            // PATCH: the systems tech — the Fixtures sheet + DMX dominate (a tall
            // bottom data area), the viewport just for orientation.
            Workspace::Patch => {
                let [c, _l] = surface.split_left(NodeIndex::root(), 0.16, vec![Tab::Scene]);
                let [c, _i] = surface.split_right(c, 0.80, vec![Tab::Inspector]);
                surface.split_below(c, 0.42, vec![Tab::Patch, Tab::DmxMonitor, Tab::Cues, Tab::Connectivity]);
            }
            // VISUALISE: the previz artist — maximise the viewport; thin Scene
            // (World + outliner) left, Inspector right, no data strip.
            Workspace::Visualize => {
                let [c, _l] = surface.split_left(NodeIndex::root(), 0.15, vec![Tab::Scene]);
                surface.split_right(c, 0.82, vec![Tab::Inspector]);
            }
        }
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
            lib: &mut self.lib,
            scene_anchor: &mut self.scene_anchor,
            scene_sort: &mut self.scene_sort,
            fm: &mut self.fm,
            transform: &mut self.transform,
            groups: &mut self.groups,
            group_name: &mut self.group_name,
            cues: &mut self.cues,
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
        windows::quick_select_window(ctx, scene, &mut self.selection, &mut self.quick_select);
    }

    /// Force the quick-select palette open (headless screenshot hook).
    pub fn debug_open_quick_select(&mut self) {
        self.quick_select = true;
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
            self.selection = Selection { fixtures: pick, environment: None };
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
            // `s` opens the quick-select palette — UNLESS the viewport is focused
            // with a selection, where `s` means Scale (Blender modal transform,
            // handled in panels::viewport). So: something to scale → scale; nothing
            // to scale → open the select palette.
            let s_is_scale = self.viewport_focused && !self.selection.fixtures.is_empty();
            if i.key_pressed(Key::S) && !i.modifiers.command && !i.modifiers.ctrl && !s_is_scale {
                self.quick_select = true;
            }
            // `a` = select all fixtures; `l` = toggle fixture labels.
            if i.key_pressed(Key::A) && !i.modifiers.command && !i.modifiers.ctrl && !scene.fixtures.is_empty() {
                self.selection = Selection { fixtures: (0..scene.fixtures.len()).collect(), environment: None };
            }
            if i.key_pressed(Key::L) && !i.modifiers.command {
                self.prefs.show_labels = !self.prefs.show_labels;
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

        // Arrow keys nudge the selected fixtures on the floor plane (Shift = 1 m,
        // else 0.1 m), but only when the viewport has focus so they don't fight
        // panel scrolling. PageUp/Down nudge height.
        if self.viewport_focused && self.transform.is_none() && !self.selection.fixtures.is_empty() {
            let (mut dx, mut dz, mut dy) = (0.0f32, 0.0f32, 0.0f32);
            let step = ctx.input(|i| {
                use egui::Key;
                let s = if i.modifiers.shift { 1.0 } else { 0.1 };
                if i.key_pressed(Key::ArrowLeft) { dx -= s; }
                if i.key_pressed(Key::ArrowRight) { dx += s; }
                if i.key_pressed(Key::ArrowUp) { dz -= s; }
                if i.key_pressed(Key::ArrowDown) { dz += s; }
                if i.key_pressed(Key::PageUp) { dy += s; }
                if i.key_pressed(Key::PageDown) { dy -= s; }
                Vec3::new(dx, dy, dz)
            });
            if step != Vec3::ZERO {
                for &fi in &self.selection.fixtures {
                    if let Some(f) = scene.fixtures.get_mut(fi) {
                        f.position += step;
                        f.snap_movement();
                    }
                }
            }
        }
    }

    /// Remove every selected fixture and clear the selection.
    fn delete_selected(&mut self, scene: &mut Scene) {
        panels::delete_selected_fixtures(scene, &mut self.selection, &mut self.scene_anchor);
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
                use theme::icon;
                ui.menu_button("File", |ui| {
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
                            if let Err(e) = crate::mvr::export_path(scene, &path) {
                                log::error!("export MVR: {e}");
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
                    if ui.add_enabled(self.selection.primary_fixture().is_some(), egui::Button::new(format!("{}  Duplicate / Array…", icon::DUPLICATE))).clicked() {
                        if let Some(idx) = self.selection.primary_fixture() {
                            self.duplicate = Some(DuplicateDialog { fixture: idx, x: 0.0, y: 0.0, z: 0.0, y_angle: 36.0, count: 9 });
                        }
                        ui.close();
                    }
                    if ui.add_enabled(!self.selection.fixtures.is_empty(), egui::Button::new(format!("{}  Delete Selected", icon::TRASH))).clicked() {
                        self.delete_selected(scene);
                        ui.close();
                    }
                    if ui.button(format!("{}  Deselect All", icon::DESELECT)).clicked() {
                        self.selection = Selection::default();
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
                    if ui.add_enabled(has, egui::Button::new(format!("{}  Edit Profile…", icon::PROFILE))).clicked() {
                        if let Some(i) = self.selection.primary_fixture() {
                            self.profile = Some(ProfileEditor::new(i));
                        }
                        ui.close();
                    }
                });
                ui.menu_button("Window", |ui| {
                    ui.menu_button(format!("{}  Workspace", icon::LAYOUT), |ui| {
                        for (ws, name) in Workspace::ALL {
                            if ui.button(name).clicked() {
                                self.dock = Self::workspace_dock(ws);
                                ui.close();
                            }
                        }
                    });
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
                        self.dock = Self::default_dock();
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
    lib: &'a mut panels::LibState,
    scene_anchor: &'a mut Option<usize>,
    scene_sort: &'a mut panels::SceneSort,
    fm: &'a mut panels::FmState,
    transform: &'a mut Option<TransformOp>,
    groups: &'a mut Vec<SelectionGroup>,
    group_name: &'a mut String,
    cues: &'a mut cues::CueEngine,
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
        format!("{}  {}", tab.icon(), tab.title()).into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Viewport => panels::viewport(
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
                self.settings,
                self.transform,
            ),
            Tab::Scene => panels::scene_outliner(
                ui,
                self.scene,
                self.selection,
                self.dmx_patch,
                self.dmx_live_mask,
                self.scene_anchor,
                self.scene_sort,
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
            ),
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
            Tab::Patch => panels::fixture_manager(
                ui,
                self.scene,
                self.dmx_patch,
                self.selection,
                self.scene_anchor,
                self.dmx_live_mask,
                self.fm,
            ),
            Tab::Cues => cues::cue_panel(ui, self.cues, self.scene),
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
