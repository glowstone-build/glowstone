//! egui + egui_dock setup: the dock layout and the [`TabViewer`] that routes
//! each dock panel to its drawing function in [`panels`].

mod panels;

use std::collections::HashMap;

use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};

use crate::renderer::camera::OrbitCamera;
use crate::scene::{Library, RenderSettings, Scene, Selection};

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
    DmxMonitor,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Viewport => "Viewport",
            Tab::Scene => "Scene",
            Tab::Library => "Library",
            Tab::Inspector => "Inspector",
            Tab::DmxMonitor => "DMX Monitor",
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
        let [_viewport, _dmx] = surface.split_below(center, 0.74, vec![Tab::DmxMonitor]);

        Self {
            dock,
            library: Library::standard(),
            gdtf_textures: HashMap::new(),
            selection: Selection::Fixture(0),
            settings: RenderSettings::default(),
            requested_viewport_px: (1, 1),
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
        viewport_texture: egui::TextureId,
        fps: f32,
    ) {
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
            fps,
        };

        DockArea::new(&mut self.dock).show(ctx, &mut viewer);
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
                self.viewport_texture,
                self.requested_viewport_px,
                self.fps,
            ),
            Tab::Scene => panels::scene_outliner(ui, self.scene, self.selection, self.settings),
            Tab::Library => {
                panels::library_browser(ui, self.library, self.scene, self.selection)
            }
            Tab::Inspector => {
                panels::inspector(ui, self.scene, *self.selection, self.gdtf_textures)
            }
            Tab::DmxMonitor => panels::dmx_monitor(ui, self.scene),
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
