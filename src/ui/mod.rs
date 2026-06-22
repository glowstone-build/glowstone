//! egui + egui_dock setup: the dock layout and the [`TabViewer`] that routes
//! each dock panel to its drawing function in [`panels`].

mod cues;
mod panels;
pub mod project;
mod share_window;
pub mod theme;
mod windows;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

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

/// State of the open Replace-fixtures dialog (Shift+R). Swaps the selected
/// fixtures' type for a chosen project-library profile, in place (keeping each
/// fixture's position / aim / level / name). `None` = closed.
#[derive(Default)]
pub struct ReplaceDialog {
    /// Name filter over the project library.
    pub search: String,
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
    /// Original (geometry index, world transform) snapshot — the static-geometry
    /// equivalent of `start`. Empty when transforming fixtures.
    pub geo_start: Vec<(usize, glam::Mat4)>,
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
    /// The open Replace-fixtures dialog (Shift+R), if any.
    replace: Option<ReplaceDialog>,
    /// Set by the viewport context menu's "Replace…" to open the dialog after the
    /// dock (where the library + patch are reachable).
    pending_replace: bool,
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
    /// Scene-outliner name filter (fixtures + objects).
    scene_search: String,
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
    /// The online GDTF Share fixture library + its window toggle.
    share: crate::share::Share,
    show_share: bool,
    /// A delete was requested (from a shortcut / menu / context menu) — committed
    /// once after the dock, where the patch is reachable, so the patch / groups /
    /// cues all get remapped in lock-step with the fixture removal.
    pending_delete: bool,
    /// Path of the currently-open `.archie` project (Save vs Save As). `None` =
    /// untitled / never saved.
    current_path: Option<PathBuf>,
    /// The welcome / recover splash is open (shown once at startup).
    show_splash: bool,
    /// Recent project paths (most-recent first) for the File menu + splash.
    recent: Vec<PathBuf>,
    /// Seconds since the last successful autosave (driven from `app`); the splash
    /// uses the autosave file's presence to offer crash recovery.
    autosave_timer: f32,
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
            replace: None,
            pending_replace: false,
            show_prefs: false,
            show_about: false,
            show_shortcuts: false,
            profile: None,
            lib: panels::LibState::default(),
            scene_anchor: None,
            scene_sort: panels::SceneSort::Patch,
            scene_search: String::new(),
            fm: panels::FmState::default(),
            quick_select: false,
            transform: None,
            groups: Vec::new(),
            group_name: String::new(),
            cues: cues::CueEngine::default(),
            pending_delete: false,
            share: crate::share::Share::new(),
            show_share: false,
            current_path: None,
            show_splash: true,
            recent: project::load_recent(),
            autosave_timer: 0.0,
        }
    }

    /// Advance any in-progress cue crossfade. Called once per real frame from
    /// `app::render`, after live DMX decode and before motion advance.
    pub fn tick_cues(&mut self, scene: &mut Scene, dt: f32) {
        self.cues.tick(scene, dt);
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
                log::info!("saved project: {}", path.display());
            }
            Err(e) => log::error!("save project: {e}"),
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
                log::info!("opened project: {}", path.display());
            }
            Err(e) => log::error!("open project {}: {e}", path.display()),
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
                        log::error!("re-parse GDTF {spec}: {e}");
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
                log::warn!("autosave: {e}");
            }
        }
    }

    /// Dismiss the welcome splash (headless screenshot hook).
    pub fn dismiss_splash(&mut self) {
        self.show_splash = false;
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
        let modal = egui::Modal::new(egui::Id::new("welcome-splash")).show(ctx, |ui| {
            ui.set_width(560.0);
            // Header: product + version (top-right).
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
            ui.label(egui::RichText::new("Open-source lighting previsualization").weak());
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);

            ui.columns(2, |cols| {
                // Left — start a session.
                cols[0].label(egui::RichText::new("Start").strong());
                cols[0].add_space(6.0);
                if cols[0].add_sized([220.0, 30.0], egui::Button::new("New Project")).clicked() {
                    action = Some(SplashAction::New);
                }
                if cols[0]
                    .add_sized([220.0, 30.0], egui::Button::new(format!("{}  Open…", theme::icon::IMPORT_MVR)))
                    .clicked()
                {
                    action = Some(SplashAction::Open);
                }
                if let Some(ap) = project::autosave_path() {
                    if ap.exists() {
                        cols[0].add_space(10.0);
                        if cols[0]
                            .add_sized([220.0, 30.0], egui::Button::new(format!("{}  Recover Last Session", theme::icon::FRAME)))
                            .on_hover_text("Reopen the auto-saved session from the last run")
                            .clicked()
                        {
                            action = Some(SplashAction::Recover(ap));
                        }
                    }
                }

                // Right — recent files.
                cols[1].label(egui::RichText::new("Recent Files").strong());
                cols[1].add_space(6.0);
                if self.recent.is_empty() {
                    cols[1].label(egui::RichText::new("No recent projects").weak().small());
                } else {
                    for p in self.recent.iter().take(8) {
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

            ui.add_space(10.0);
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

        // Project shortcuts (Ctrl/Cmd + S / Shift+S / O / N) — handled here where
        // scene + camera + dmx are all reachable in one place.
        let (do_save, do_save_as, do_open, do_new) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && !i.modifiers.shift && i.key_pressed(egui::Key::S),
                cmd && i.modifiers.shift && i.key_pressed(egui::Key::S),
                cmd && i.key_pressed(egui::Key::O),
                cmd && i.key_pressed(egui::Key::N),
            )
        });
        if do_save_as {
            self.save_project_as(scene, camera, dmx);
        } else if do_save {
            self.save_project(scene, camera, dmx);
        }
        if do_open {
            self.open_project_dialog(scene, camera, dmx);
        }
        if do_new {
            self.new_project(scene, camera, dmx);
        }

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
            scene_search: &mut self.scene_search,
            fm: &mut self.fm,
            transform: &mut self.transform,
            groups: &mut self.groups,
            group_name: &mut self.group_name,
            cues: &mut self.cues,
            delete_requested: &mut self.pending_delete,
            replace_requested: &mut self.pending_replace,
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
        };

        DockArea::new(&mut self.dock).show(ctx, &mut viewer);

        // Commit a requested delete now (viewer borrows released): the patch is
        // reachable here, so fixtures + patch + cues + groups are remapped together.
        if self.pending_delete {
            self.commit_delete(scene, dmx.patch_mut());
        }
        // The viewport context menu's "Replace…" opens the dialog here (after the
        // dock), where the library + patch are reachable.
        if self.pending_replace {
            self.pending_replace = false;
            if self.replace.is_none() && !self.selection.fixtures.is_empty() {
                self.replace = Some(ReplaceDialog::default());
            }
        }
        replace_window(ctx, &self.library, scene, &mut self.selection, dmx.patch_mut(), &mut self.replace);

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
        share_window::fixture_library_window(
            ctx,
            &mut self.show_share,
            &mut self.share,
            &mut self.library,
            scene,
            &mut self.selection,
        );
        // The welcome / recover splash sits above everything (it's the first
        // thing on a fresh launch).
        self.splash_window(ctx, scene, camera, dmx);
    }

    /// Force the quick-select palette open (headless screenshot hook).
    pub fn debug_open_quick_select(&mut self) {
        self.quick_select = true;
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
            self.selection = Selection { fixtures: pick, geometry: Vec::new(), environment: None };
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
            let s_is_scale = self.viewport_focused
                && (!self.selection.fixtures.is_empty() || !self.selection.geometry.is_empty());
            if i.key_pressed(Key::S) && !i.modifiers.command && !i.modifiers.ctrl && !s_is_scale {
                self.quick_select = true;
            }
            // `a` = select all fixtures; `l` = toggle fixture labels.
            if i.key_pressed(Key::A) && !i.modifiers.command && !i.modifiers.ctrl && !scene.fixtures.is_empty() {
                self.selection = Selection { fixtures: (0..scene.fixtures.len()).collect(), geometry: Vec::new(), environment: None };
            }
            if i.key_pressed(Key::L) && !i.modifiers.command {
                self.prefs.show_labels = !self.prefs.show_labels;
            }
            // Shift+R replaces the selected fixtures with another project profile.
            // (Plain R is the Rotate modal transform, handled in the viewport.)
            if i.key_pressed(Key::R)
                && i.modifiers.shift
                && !i.modifiers.command
                && !i.modifiers.ctrl
                && self.replace.is_none()
                && !self.selection.fixtures.is_empty()
            {
                self.replace = Some(ReplaceDialog::default());
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
        if del && (!self.selection.fixtures.is_empty() || !self.selection.geometry.is_empty()) {
            self.pending_delete = true; // committed after the dock (remaps patch/groups/cues)
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
    /// Commit a requested fixture deletion, remapping every index-keyed structure
    /// in lock-step so nothing is silently corrupted: the patch entries, the cue
    /// look lists, and the saved selection groups all follow the removal. Called
    /// once after the dock, where the patch is reachable.
    fn commit_delete(&mut self, scene: &mut Scene, patch: &mut crate::dmx::PatchTable) {
        self.pending_delete = false;
        // --- fixtures: remove + remap every index-keyed structure in lock-step ---
        let mut removed: Vec<usize> =
            self.selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
        removed.sort_unstable();
        removed.dedup();
        if !removed.is_empty() {
            // Descending so earlier indices stay valid as we remove.
            for &i in removed.iter().rev() {
                scene.fixtures.remove(i);
                patch.remove_at(i);
                self.cues.remove_fixture(i);
            }
            // Groups store arbitrary index references: remap each through the
            // shift, dropping deleted members, then drop any group left empty.
            for g in &mut self.groups {
                g.fixtures = g.fixtures.iter().filter_map(|&idx| remap_index(idx, &removed)).collect();
            }
            self.groups.retain(|g| !g.fixtures.is_empty());
        }
        // --- static geometry: a plain removal (no patch/cue/group keyed by it) ---
        let mut geo: Vec<usize> =
            self.selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
        geo.sort_unstable();
        geo.dedup();
        for &i in geo.iter().rev() {
            scene.geometry.remove(i);
        }
        if !removed.is_empty() || !geo.is_empty() {
            self.selection = Selection::default();
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
        dmx: &mut crate::dmx::DmxIo,
    ) {
        egui::TopBottomPanel::top("menu-bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                use theme::icon;
                ui.menu_button("File", |ui| {
                    if ui.button(format!("{}  New Project", icon::SCENE)).clicked() {
                        self.new_project(scene, camera, dmx);
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
                        self.pending_delete = true; // committed after the dock (remaps patch/groups/cues)
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
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let mut any = false;
                // GDTF fixtures in the project (imported + already placed) first.
                let gdtf: Vec<usize> = (0..gdtf_arcs.len())
                    .filter(|&gi| matches(&format!("{} {}", gdtf_arcs[gi].manufacturer, gdtf_arcs[gi].name)))
                    .collect();
                if !gdtf.is_empty() {
                    any = true;
                    theme::section(ui, "GDTF FIXTURES");
                    for gi in gdtf {
                        let g = &gdtf_arcs[gi];
                        let label = format!("{}  {} · {}", theme::icon::FIXTURE, g.manufacturer, g.name);
                        if ui.selectable_label(false, label).clicked() {
                            picked = Some(Picked::Gdtf(gi));
                        }
                    }
                    ui.add_space(6.0);
                }
                // Then the built-in placeholder profiles.
                let prof: Vec<usize> = (0..library.fixtures.len())
                    .filter(|&pi| matches(&format!("{} {}", library.fixtures[pi].category, library.fixtures[pi].name)))
                    .collect();
                if !prof.is_empty() {
                    any = true;
                    theme::section(ui, "BUILT-IN");
                    for pi in prof {
                        let p = &library.fixtures[pi];
                        let icon = if p.laser { theme::icon::COLOR } else { theme::icon::FIXTURE };
                        let label = format!("{icon}  {} · {}", p.category, p.name);
                        if ui.selectable_label(false, label).clicked() {
                            picked = Some(Picked::Profile(pi));
                        }
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
        for &i in &targets {
            // Snapshot the placement + level + name to carry across the swap.
            let (pos, orient, pan, tilt, name, dimmer) = {
                let f = &scene.fixtures[i];
                (f.position, f.orientation, f.pan, f.tilt, f.name.clone(), f.optics.dimmer)
            };
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
    fm: &'a mut panels::FmState,
    transform: &'a mut Option<TransformOp>,
    groups: &'a mut Vec<SelectionGroup>,
    group_name: &'a mut String,
    cues: &'a mut cues::CueEngine,
    delete_requested: &'a mut bool,
    replace_requested: &'a mut bool,
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
                self.delete_requested,
                self.replace_requested,
            ),
            Tab::Scene => panels::scene_outliner(
                ui,
                self.scene,
                self.selection,
                self.dmx_patch,
                self.scene_anchor,
                self.scene_sort,
                self.scene_search,
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
                self.open_share,
            ),
            Tab::Inspector => {
                panels::inspector(ui, self.scene, self.selection, self.dmx_patch, self.gdtf_textures, self.profile)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmx::DmxIo;
    use crate::scene::Scene;

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

        let mut ui = Ui::new();
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
