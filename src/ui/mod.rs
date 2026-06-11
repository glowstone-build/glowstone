//! egui + egui_dock setup: the dock layout and the [`TabViewer`] that routes
//! each dock panel to its drawing function in [`panels`].

mod panels;

use std::collections::HashMap;

use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
use glam::Vec3;

use crate::renderer::camera::OrbitCamera;
use crate::scene::{Library, RenderSettings, Scene, Selection};

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
    pub requested_viewport_px: (u32, u32),
    /// Whether the 3D viewport currently has interaction focus (drives the focus
    /// border and whether the `d` shortcut opens the Duplicate dialog).
    viewport_focused: bool,
    /// The open Duplicate dialog, if any.
    duplicate: Option<DuplicateDialog>,
}

impl Ui {
    pub fn new() -> Self {
        // Central "Viewport", then split off the surrounding panels.
        // `fraction` is the share the *old* (central) node keeps after a split.
        let mut dock = DockState::new(vec![Tab::Viewport]);
        let surface = dock.main_surface_mut();
        let [center, _left] =
            surface.split_left(NodeIndex::root(), 0.80, vec![Tab::Scene, Tab::Library]);
        let [center, _inspector] = surface.split_right(center, 0.76, vec![Tab::Inspector]);
        // The bottom strip groups the live universe grid, the patch editor, and
        // the connectivity settings as three tabs.
        let [_viewport, _dmx] = surface.split_below(
            center,
            0.74,
            vec![Tab::DmxMonitor, Tab::Patch, Tab::Connectivity],
        );

        Self {
            dock,
            library: Library::standard(),
            gdtf_textures: HashMap::new(),
            selection: Selection::fixture(0),
            settings: RenderSettings::default(),
            requested_viewport_px: (1, 1),
            viewport_focused: false,
            duplicate: None,
        }
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
        // One call hands back all the disjoint DMX borrows the panels need.
        let dmx = dmx.view();
        // Split self's fields so the viewer can borrow them disjointly from the
        // dock state.
        let mut viewer = PanelViewer {
            scene,
            camera,
            library: &mut self.library,
            gdtf_textures: &mut self.gdtf_textures,
            selection: &mut self.selection,
            settings: &mut self.settings,
            viewport_texture,
            requested_viewport_px: &mut self.requested_viewport_px,
            viewport_focused: &mut self.viewport_focused,
            duplicate: &mut self.duplicate,
            dmx_patch: dmx.patch,
            dmx_snapshot: dmx.snapshot,
            dmx_status: dmx.status,
            dmx_config: dmx.config,
            dmx_live_mask: dmx.live_mask,
            dmx_selected_universe: dmx.selected_universe,
            dmx_bind_ip_text: dmx.bind_ip_text,
            dmx_universes_text: dmx.universes_text,
            dmx_pending: dmx.pending,
            dmx_running: dmx.running,
            fps,
        };

        DockArea::new(&mut self.dock).show(ctx, &mut viewer);

        // The Duplicate dialog floats above the dock (the viewer's borrows are
        // released now, so the scene/selection are free again).
        duplicate_window(ctx, scene, &mut self.selection, &mut self.duplicate);
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
    viewport_texture: egui::TextureId,
    requested_viewport_px: &'a mut (u32, u32),
    viewport_focused: &'a mut bool,
    duplicate: &'a mut Option<DuplicateDialog>,
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
                panels::inspector(ui, self.scene, self.selection, self.gdtf_textures)
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
