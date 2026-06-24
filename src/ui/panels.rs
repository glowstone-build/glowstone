//! The individual dock panels. Each is a plain function taking the egui `Ui`
//! plus whatever scene state it reads or edits.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, DragValue, Grid, RichText, Sense, Slider};
use glam::{Mat4, Quat, Vec2, Vec3};

use super::gizmo::{self, GizmoCtx, Handle};
use super::shortcuts;
use super::theme;
use super::windows::{LabelMode, Preferences, ProfileEditor};
use super::{
    ActiveTool, Axis, DuplicateDialog, GdtfTextures, SelectionGroup, TransformKind, TransformOp,
};
use crate::dmx::patch::channel_map;
use crate::dmx::{DmxConfig, DmxStatus, MergePolicy, PatchSource, PatchTable, PendingNetCmd, UniverseSnapshot};
use crate::gdtf::{GdtfFixture, WheelKind};
use crate::optics::{self, OpticField, OpticalControls};
use crate::renderer::camera::OrbitCamera;
use crate::scene::environment::Environment;
use crate::scene::screen::{LedScreen, PixelShape, ScreenContent, TestPattern};
use crate::scene::{apply_fixture_click, Fixture, Library, Scene, Selection};

/// Universe is considered live if it updated within this window.
const DMX_STALE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Left tab: the scene outliner — every fixture and environment, selectable —
/// plus the global view/look controls.
/// Discovered live video sources for the LED-screen content pickers, refreshed
/// by the app each frame from the NDI + CITP clients.
#[derive(Default, Clone)]
pub struct ScreenSources {
    /// NDI source names (empty unless built with the `ndi` feature + a runtime).
    pub ndi: Vec<String>,
    /// Whether NDI receive is compiled in AND a runtime is present.
    pub ndi_available: bool,
    /// Discovered CITP media-server names.
    pub citp: Vec<String>,
}

/// How the Scene panel's fixture list is ordered.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SceneSort {
    /// DMX patch (universe, address); unpatched fall to the end, by head number.
    Patch,
    Name,
    /// By fixture profile / type, then name.
    Type,
}

impl SceneSort {
    fn label(self) -> &'static str {
        match self {
            Self::Patch => "Patch",
            Self::Name => "Name",
            Self::Type => "Type",
        }
    }
}

/// The display order of fixture indices for the given sort.
fn fixture_order(scene: &Scene, patch: &PatchTable, sort: SceneSort) -> Vec<usize> {
    let mut order: Vec<usize> = (0..scene.fixtures.len()).collect();
    match sort {
        SceneSort::Patch => {
            // Patched first by (universe, address); unpatched after, by head
            // (MVR unit number) then insertion index.
            let key = |i: usize| -> (u8, u16, u16, i64, usize) {
                match patch.get(i).filter(|p| p.enabled) {
                    Some(p) => (0, p.universe, p.address, 0, i),
                    None => {
                        let head = scene.fixtures[i]
                            .mvr
                            .as_ref()
                            .map(|m| m.unit_number as i64)
                            .filter(|&n| n != 0)
                            .unwrap_or(i64::MAX);
                        (1, u16::MAX, u16::MAX, head, i)
                    }
                }
            };
            order.sort_by(|&a, &b| key(a).cmp(&key(b)));
        }
        SceneSort::Name => {
            order.sort_by(|&a, &b| {
                scene.fixtures[a].name.to_lowercase().cmp(&scene.fixtures[b].name.to_lowercase())
            });
        }
        SceneSort::Type => {
            order.sort_by(|&a, &b| {
                let fa = &scene.fixtures[a];
                let fb = &scene.fixtures[b];
                fa.profile
                    .to_lowercase()
                    .cmp(&fb.profile.to_lowercase())
                    .then(fa.name.to_lowercase().cmp(&fb.name.to_lowercase()))
            });
        }
    }
    order
}

#[allow(clippy::too_many_arguments)]
pub fn scene_outliner(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &mut Selection,
    patch: &PatchTable,
    anchor: &mut Option<usize>,
    sort: &mut SceneSort,
    search: &mut String,
    groups: &mut Vec<SelectionGroup>,
    group_name: &mut String,
) {
    use theme::icon;
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;
    const ROW_H: f32 = 34.0;

    ui.horizontal(|ui| {
        ui.heading("Scene");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let n = selection.fixtures.len() + selection.geometry.len() + selection.screens.len();
            if n > 0 {
                ui.label(RichText::new(format!("{n} selected")).small().color(accent));
            }
        });
    });

    // Name filter (fixtures + objects), like Blender's outliner search.
    ui.horizontal(|ui| {
        let has = !search.is_empty();
        let w = ui.available_width() - if has { 26.0 } else { 0.0 };
        ui.add(
            egui::TextEdit::singleline(search)
                .hint_text(format!("{}  Filter…", icon::SEARCH))
                .desired_width(w.max(40.0)),
        );
        if has && ui.small_button(icon::CLOSE).on_hover_text("Clear filter").clicked() {
            search.clear();
        }
    });
    let q = search.trim().to_lowercase();
    let matches = |name: &str| q.is_empty() || name.to_lowercase().contains(q.as_str());
    ui.separator();

    // Mark which fixtures are in an address conflict (computed once, not per row).
    let mut conflicted = vec![false; scene.fixtures.len()];
    for c in patch.conflicts() {
        // Guard: patch entry indices may transiently exceed the fixture count
        // (mid-import / before reconcile).
        if let Some(s) = conflicted.get_mut(c.a) {
            *s = true;
        }
        if let Some(s) = conflicted.get_mut(c.b) {
            *s = true;
        }
    }

    // ---- WORLD: the top of the scene hierarchy ----
    // A selectable container node (HDRI sky + image-based ambient; its inspector
    // is shown when picked) with the fog-box environments nested under it as
    // children, mirroring Blender's "World" sitting above the scene collection.
    // Framed in a faint surface to read as the top-level container.
    egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
    folder_header(icon::WORLD, "World", 0, true, &ink).show(ui, |ui| {
        // The World node itself: selecting it opens the world_inspector.
        let wrow = entity_row(
            ui,
            icon::WORLD,
            "World",
            "HDRI · ambient",
            "",
            false,
            selection.world,
            selection.world,
            false,
            false,
            &ink,
            accent,
        );
        if wrow.body.clicked() {
            *selection = Selection::world();
            *anchor = None;
        }
        // ENVIRONMENT children: fog boxes / volumes, indented under World.
        ui.indent("world-children", |ui| {
            folder_header(icon::ENVIRONMENT, "Environment", scene.environments.len(), true, &ink)
                .show(ui, |ui| {
                    if scene.environments.is_empty() {
                        ui.label(RichText::new("none — add a Fog Box from the Library").weak().small());
                    }
                    let mut click: Option<usize> = None;
                    for (i, env) in scene.environments.iter().enumerate() {
                        let sel = selection.environment == Some(i);
                        let row = entity_row(
                            ui,
                            icon::ENVIRONMENT,
                            env.name.as_str(),
                            "Fog volume",
                            "",
                            false,
                            sel,
                            sel,
                            false,
                            false,
                            &ink,
                            accent,
                        );
                        if row.body.clicked() {
                            click = Some(i);
                        }
                    }
                    if let Some(i) = click {
                        *selection = Selection::environment(i);
                        *anchor = None;
                    }
                });
        });
    });
    }); // World container frame
    ui.add_space(6.0);

    // ---- OBJECTS: imported MVR static geometry (stage / truss / set) ----
    // Selectable + transformable (G/R/S in the viewport); thousands of rows, so
    // the body is virtualised and the folder defaults closed.
    folder_header(icon::GEOMETRY, "Objects", scene.geometry.len(), false, &ink).show(ui, |ui| {
        if scene.geometry.is_empty() {
            ui.label(RichText::new("none — import an MVR scene").weak().small());
            return;
        }
        let order: Vec<usize> = (0..scene.geometry.len()).filter(|&i| matches(&scene.geometry[i].name)).collect();
        if order.is_empty() {
            ui.label(RichText::new("no match").weak().small());
            return;
        }
        let mut click: Option<(usize, bool)> = None;
        let mut vis: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("scene-objects")
            .max_height(240.0)
            .auto_shrink([false, true])
            .show_rows(ui, ROW_H, order.len(), |ui, range| {
                for di in range {
                    let i = order[di];
                    let g = &scene.geometry[i];
                    let kind = g.mvr.as_ref().map(|m| m.kind.as_str()).filter(|k| !k.is_empty()).unwrap_or("Object");
                    let row = entity_row(
                        ui,
                        icon::GEOMETRY,
                        &g.name,
                        kind,
                        "",
                        false,
                        selection.contains_geometry(i),
                        selection.primary_geometry() == Some(i),
                        g.hidden,
                        true,
                        &ink,
                        accent,
                    );
                    if row.eye_clicked {
                        vis = Some(i);
                    } else if row.body.clicked() {
                        let m = ui.input(|x| x.modifiers);
                        click = Some((i, m.command || m.ctrl));
                    }
                }
            });
        if let Some(i) = vis {
            if let Some(g) = scene.geometry.get_mut(i) {
                g.hidden = !g.hidden;
            }
        }
        if let Some((i, toggle)) = click {
            if toggle {
                selection.toggle_geometry(i);
            } else {
                *selection = Selection::geometry(i);
            }
            *anchor = None;
        }
    });

    // ---- SCREENS: LED video walls (emissive surfaces) ----
    folder_header(icon::SCREEN, "Screens", scene.screens.len(), false, &ink).show(ui, |ui| {
        if scene.screens.is_empty() {
            ui.label(RichText::new("none — add an LED Wall from the Library").weak().small());
            return;
        }
        let order: Vec<usize> =
            (0..scene.screens.len()).filter(|&i| matches(&scene.screens[i].name)).collect();
        if order.is_empty() {
            ui.label(RichText::new("no match").weak().small());
            return;
        }
        let mut click: Option<(usize, bool)> = None;
        let mut vis: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("scene-screens")
            .max_height(200.0)
            .auto_shrink([false, true])
            .show_rows(ui, ROW_H, order.len(), |ui, range| {
                for di in range {
                    let i = order[di];
                    let s = &scene.screens[i];
                    let [rx, ry] = s.resolution();
                    let sub = format!("{rx}×{ry} · {}", s.content.label());
                    let row = entity_row(
                        ui,
                        icon::SCREEN,
                        &s.name,
                        &sub,
                        "",
                        false,
                        selection.contains_screen(i),
                        selection.primary_screen() == Some(i),
                        s.hidden,
                        true,
                        &ink,
                        accent,
                    );
                    if row.eye_clicked {
                        vis = Some(i);
                    } else if row.body.clicked() {
                        let m = ui.input(|x| x.modifiers);
                        click = Some((i, m.command || m.ctrl));
                    }
                }
            });
        if let Some(i) = vis {
            if let Some(s) = scene.screens.get_mut(i) {
                s.hidden = !s.hidden;
            }
        }
        if let Some((i, toggle)) = click {
            if toggle {
                selection.toggle_screen(i);
            } else {
                *selection = Selection::screen(i);
            }
            *anchor = None;
        }
    });

    // ---- FIXTURES: sortable, virtualised, patch-ordered ----
    folder_header(icon::FIXTURE, "Fixtures", scene.fixtures.len(), true, &ink).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(theme::ico(icon::SORT).weak()).on_hover_text("Sort fixtures by");
            for s in [SceneSort::Patch, SceneSort::Name, SceneSort::Type] {
                ui.selectable_value(sort, s, s.label());
            }
        });
        if scene.fixtures.is_empty() {
            ui.label(RichText::new("none — add from the Library").weak().small());
            return;
        }
        let order: Vec<usize> =
            fixture_order(scene, patch, *sort).into_iter().filter(|&i| matches(&scene.fixtures[i].name)).collect();
        if order.is_empty() {
            ui.label(RichText::new("no match").weak().small());
            return;
        }
        let mut click: Option<(usize, bool, bool)> = None;
        let mut vis: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("scene-fixtures")
            .max_height(300.0)
            .auto_shrink([false, true])
            .show_rows(ui, ROW_H, order.len(), |ui, range| {
                for di in range {
                    let i = order[di];
                    let fixture = &scene.fixtures[i];
                    let patch_tag = match patch.get(i).filter(|p| p.enabled) {
                        Some(p) => format!("{}.{:03}", p.universe, p.address),
                        None => "unpatched".into(),
                    };
                    let row_icon = if fixture.is_laser { icon::COLOR } else { icon::FIXTURE };
                    let row = entity_row(
                        ui,
                        row_icon,
                        &fixture.name,
                        &fixture.profile,
                        &patch_tag,
                        conflicted[i],
                        selection.contains_fixture(i),
                        selection.primary_fixture() == Some(i),
                        fixture.hidden,
                        true,
                        &ink,
                        accent,
                    );
                    if row.eye_clicked {
                        vis = Some(i);
                    } else if row.body.clicked() {
                        let m = ui.input(|x| x.modifiers);
                        click = Some((i, m.shift, m.command || m.ctrl));
                    }
                }
            });
        if let Some(i) = vis {
            if let Some(f) = scene.fixtures.get_mut(i) {
                f.hidden = !f.hidden;
            }
        }
        if let Some((i, shift, toggle)) = click {
            if shift {
                // Range follows the VISIBLE (sorted) order, not raw scene indices:
                // select every fixture whose display row is between the anchor's
                // row and the clicked row.
                let click_pos = order.iter().position(|&x| x == i).unwrap_or(0);
                let anchor_pos = anchor
                    .and_then(|a| order.iter().position(|&x| x == a))
                    .unwrap_or(click_pos);
                let (lo, hi) = (anchor_pos.min(click_pos), anchor_pos.max(click_pos));
                selection.fixtures = order[lo..=hi].to_vec();
                selection.environment = None;
                selection.geometry.clear();
                // Establish an anchor if this was the first (anchorless) click, so
                // subsequent shift-clicks grow the range from here.
                if anchor.is_none() {
                    *anchor = Some(i);
                }
            } else {
                apply_fixture_click(selection, anchor, i, false, toggle, scene.fixtures.len());
            }
        }
    });

    // ---- GROUPS: saved named selections (console-style), recalled by click ----
    folder_header(icon::CATEGORY, "Groups", groups.len(), true, &ink).show(ui, |ui| {
        // Default name is the first "Group N" not already taken (so it can't
        // collide after a delete + re-save).
        let default_name = (1..)
            .map(|n| format!("Group {n}"))
            .find(|cand| !groups.iter().any(|g| &g.name == cand))
            .unwrap_or_else(|| "Group".into());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(group_name)
                    .desired_width(110.0)
                    .hint_text(&default_name),
            );
            let can_save = !selection.fixtures.is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new(format!("{}  Save", icon::ADD)))
                .on_hover_text("Save the current fixture selection as a group")
                .clicked()
            {
                let name = if group_name.trim().is_empty() { default_name } else { group_name.trim().to_string() };
                // Store sorted + deduped so recall order and the active-match are stable.
                let mut fixtures = selection.fixtures.clone();
                fixtures.sort_unstable();
                fixtures.dedup();
                groups.push(SelectionGroup { name, fixtures });
                group_name.clear();
            }
        });
        if groups.is_empty() {
            ui.label(RichText::new("none — select fixtures, then Save").weak().small());
        }
        // The current selection, sorted once, to highlight the matching group.
        let mut have = selection.fixtures.clone();
        have.sort_unstable();
        have.dedup();
        let mut recall: Option<usize> = None;
        let mut remove: Option<usize> = None;
        for (gi, g) in groups.iter().enumerate() {
            ui.horizontal(|ui| {
                // Groups are stored sorted+deduped, so compare directly (cheap).
                let active = !g.fixtures.is_empty() && g.fixtures == have;
                if ui
                    .selectable_label(active, format!("{}  ({})", g.name, g.fixtures.len()))
                    .on_hover_text("Recall this selection")
                    .clicked()
                {
                    recall = Some(gi);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(icon::TRASH).on_hover_text("Delete group").clicked() {
                        remove = Some(gi);
                    }
                });
            });
        }
        if let Some(gi) = recall {
            let n = scene.fixtures.len();
            selection.fixtures = groups[gi].fixtures.iter().copied().filter(|&i| i < n).collect();
            selection.environment = None;
            selection.geometry.clear();
            *anchor = None;
        }
        if let Some(gi) = remove {
            groups.remove(gi);
        }
    });

    // (Render/look controls live on the viewport overlay (Mode + Exposure), the
    // View menu (grid / gizmo / label toggles) and Preferences > Rendering — not
    // duplicated here, so the Scene panel stays a clean outliner.)
}

/// The World inspector: load an equirectangular HDRI (sky + image-based
/// ambient), set its brightness, ambient fill, yaw and whether it shows as the
/// viewport background. Shown in the Inspector when the World node is selected.
fn world_inspector(ui: &mut egui::Ui, world: &mut crate::scene::World, ink: &theme::Ink) {
    use theme::icon;
    ui.horizontal(|ui| {
        if ui
            .button(format!("{}  Load HDRI…", icon::IMAGE))
            .on_hover_text("Load an equirectangular environment map (.hdr / .png / .jpg)")
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Environment map", &["hdr", "exr", "png", "jpg", "jpeg"])
                .pick_file()
        {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    world.hdri = Some(std::sync::Arc::new(bytes));
                    world.hdri_name = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                }
                Err(e) => log::error!("load HDRI {}: {e}", path.display()),
            }
        }
        if world.hdri.is_some() && ui.button(theme::ico(icon::CLOSE)).on_hover_text("Remove the environment map").clicked() {
            world.hdri = None;
            world.hdri_name.clear();
        }
    });
    let name = if world.hdri.is_some() {
        if world.hdri_name.is_empty() { "loaded".to_string() } else { world.hdri_name.clone() }
    } else {
        "none (dark void)".to_string()
    };
    ui.label(RichText::new(name).weak().small());

    let enabled = world.hdri.is_some();
    Grid::new("world-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Brightness").on_hover_text("Overall world exposure (sky + ambient)");
        ui.add_enabled(enabled, Slider::new(&mut world.brightness, 0.0..=4.0));
        ui.end_row();
        ui.label("Ambient").on_hover_text("How strongly the environment lights the geometry");
        ui.add_enabled(enabled, Slider::new(&mut world.ambient, 0.0..=2.0));
        ui.end_row();
        ui.label("Rotation").on_hover_text("Turn the environment around the vertical axis");
        ui.add_enabled(enabled, Slider::new(&mut world.rotation, 0.0..=std::f32::consts::TAU).suffix(" rad"));
        ui.end_row();
        ui.label("Background");
        ui.add_enabled(enabled, egui::Checkbox::new(&mut world.show_background, "show sky"));
        ui.end_row();
    });
    if !enabled {
        ui.label(RichText::new("Load a map to light the scene from the environment.").weak().small().color(ink.muted));
    }
}

/// A collapsible top-level Scene folder header: icon + title + count, styled as a
/// quiet section. Returns the `CollapsingHeader` to `.show(...)` a body on.
fn folder_header(
    icon: &str,
    title: &str,
    count: usize,
    default_open: bool,
    ink: &theme::Ink,
) -> egui::CollapsingHeader {
    let label = if count > 0 {
        format!("{icon}  {title}  ·  {count}")
    } else {
        format!("{icon}  {title}")
    };
    egui::CollapsingHeader::new(RichText::new(label).size(12.0).strong().color(ink.secondary))
        .id_salt(title)
        .default_open(default_open)
}

/// Left tab: the content library — categorized fixtures and environments you
/// can add to the scene.
/// How the library list is ordered.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibSort {
    Category,
    Name,
    Manufacturer,
}

impl LibSort {
    fn label(self) -> &'static str {
        match self {
            Self::Category => "Category",
            Self::Name => "Name",
            Self::Manufacturer => "Maker",
        }
    }
}

/// Library panel UI state (search/sort + multi-select with a range anchor).
pub struct LibState {
    pub search: String,
    pub sort: LibSort,
    /// Selected rows, as indices into the current filtered+sorted list.
    pub selected: Vec<usize>,
    pub anchor: Option<usize>,
}

impl Default for LibState {
    fn default() -> Self {
        Self { search: String::new(), sort: LibSort::Category, selected: Vec::new(), anchor: None }
    }
}

/// One library entry — what it is, plus display metadata.
enum LibKind {
    Gdtf(usize),
    Fixture(usize),
    Env(usize),
    Screen(usize),
}

struct LibRow {
    kind: LibKind,
    icon: &'static str,
    name: String,
    meta: String,
    category: String,
    accent: bool, // laser/colour entry → tint the icon
}

/// Build the flat row list from the library (Imported GDTF, then built-in
/// fixtures, then environments), each with display metadata.
fn library_rows(library: &Library) -> Vec<LibRow> {
    use theme::icon;
    let mut rows = Vec::new();
    for (i, g) in library.gdtf.iter().enumerate() {
        let beam = if g.beam.beam_type.is_empty() { "" } else { g.beam.beam_type.as_str() };
        rows.push(LibRow {
            kind: LibKind::Gdtf(i),
            icon: icon::FIXTURE,
            name: g.name.clone(),
            meta: format!("{} · {} · {} mode{}", g.manufacturer, beam, g.modes.len(), if g.modes.len() == 1 { "" } else { "s" }),
            category: if g.manufacturer.is_empty() { "Imported".into() } else { g.manufacturer.clone() },
            accent: false,
        });
    }
    for (i, p) in library.fixtures.iter().enumerate() {
        rows.push(LibRow {
            kind: LibKind::Fixture(i),
            icon: if p.laser { icon::COLOR } else { icon::FIXTURE },
            name: p.name.to_string(),
            meta: if p.laser { "Laser engine".into() } else { format!("{:.0}° beam", p.default_beam_angle) },
            category: p.category.to_string(),
            accent: p.laser,
        });
    }
    for (i, p) in library.environments.iter().enumerate() {
        let [w, h, d] = p.default_size;
        rows.push(LibRow {
            kind: LibKind::Env(i),
            icon: icon::ENVIRONMENT,
            name: p.name.to_string(),
            meta: format!("{w:.0} × {h:.0} × {d:.0} m"),
            category: if p.category.is_empty() { "Environment" } else { p.category }.to_string(),
            accent: false,
        });
    }
    for (i, p) in library.screens.iter().enumerate() {
        let pitch = p.cabinet_mm[0] / p.cabinet_px[0].max(1) as f32;
        rows.push(LibRow {
            kind: LibKind::Screen(i),
            icon: icon::SCREEN,
            name: p.name.to_string(),
            meta: format!(
                "{:.1}mm · {:.0}×{:.0}mm{}",
                pitch, p.cabinet_mm[0], p.cabinet_mm[1], if p.transparent { " · mesh" } else { "" }
            ),
            category: p.category.to_string(),
            accent: p.transparent,
        });
    }
    rows
}

/// Instantiate a library row into the scene; returns the resulting selection.
fn add_library_row(row: &LibRow, library: &Library, scene: &mut Scene) -> Selection {
    match row.kind {
        LibKind::Gdtf(i) => {
            let arc = library.gdtf[i].clone();
            Selection::fixture(scene.add_gdtf(arc, glam::Vec3::new(0.0, 4.0, 0.0)))
        }
        LibKind::Fixture(i) => Selection::fixture(scene.add_fixture(&library.fixtures[i])),
        LibKind::Env(i) => Selection::environment(scene.add_environment(&library.environments[i])),
        LibKind::Screen(i) => Selection::screen(scene.add_screen(&library.screens[i])),
    }
}

/// Left tab: the content library — a searchable, sortable list of fixture and
/// environment templates with multi-select (shift = range) and batch add.
#[allow(clippy::too_many_arguments)]
pub fn library_browser(
    ui: &mut egui::Ui,
    library: &mut Library,
    scene: &mut Scene,
    selection: &mut Selection,
    camera: &mut OrbitCamera,
    lib: &mut LibState,
    open_share: &mut bool,
) {
    use theme::icon;

    // --- header: import / export toolbar (icon buttons) ---
    ui.horizontal(|ui| {
        ui.heading("Library");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!("{}  Online", icon::ONLINE))
                .on_hover_text("Browse and download fixtures from the online GDTF Share library")
                .clicked()
            {
                *open_share = true;
            }
            ui.separator();
            let can_export = !scene.fixtures.is_empty() || !scene.geometry.is_empty();
            if ui.add_enabled(can_export, egui::Button::new(theme::ico(icon::EXPORT)))
                .on_hover_text("Export the scene to MVR")
                .clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("MVR scene", &["mvr"]).set_file_name("scene.mvr").save_file()
            {
                if let Err(e) = crate::mvr::export_path(scene, &path) {
                    log::error!("MVR export failed: {e}");
                }
            }
            if ui.button(theme::ico(icon::IMPORT_MVR)).on_hover_text("Import an MVR scene").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("MVR scene", &["mvr"]).pick_file()
            {
                match crate::mvr::MvrImport::load_path(&path) {
                    Ok(import) => {
                        scene.import_mvr(import);
                        if let Some((c, r)) = scene.scene_frame() {
                            camera.frame(c, r * 1.15);
                        }
                        *selection = Selection::default();
                    }
                    Err(e) => log::error!("MVR import failed: {e}"),
                }
            }
            if ui.button(theme::ico(icon::IMPORT_GDTF)).on_hover_text("Import a GDTF fixture into the library").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("GDTF fixture", &["gdtf"]).pick_file()
            {
                if let Err(e) = library.import_gdtf(&path) {
                    log::error!("GDTF import failed: {e}");
                }
            }
        });
    });

    // --- search + sort row ---
    ui.horizontal(|ui| {
        ui.label(theme::ico(icon::SEARCH).weak());
        let resp = ui.add(
            egui::TextEdit::singleline(&mut lib.search)
                .hint_text("Filter…")
                .desired_width(f32::INFINITY),
        );
        if resp.changed() {
            lib.selected.clear();
            lib.anchor = None;
        }
    });
    ui.horizontal(|ui| {
        ui.label(theme::ico(icon::SORT).weak()).on_hover_text("Sort by");
        for s in [LibSort::Category, LibSort::Name, LibSort::Manufacturer] {
            if ui.selectable_label(lib.sort == s, s.label()).clicked() {
                lib.sort = s;
                lib.selected.clear();
                lib.anchor = None;
            }
        }
    });

    // --- build, filter, sort ---
    let mut rows = library_rows(library);
    let q = lib.search.trim().to_lowercase();
    if !q.is_empty() {
        rows.retain(|r| {
            r.name.to_lowercase().contains(&q)
                || r.meta.to_lowercase().contains(&q)
                || r.category.to_lowercase().contains(&q)
        });
    }
    match lib.sort {
        LibSort::Name => rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        LibSort::Manufacturer => rows.sort_by(|a, b| {
            a.category.to_lowercase().cmp(&b.category.to_lowercase()).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }),
        LibSort::Category => {} // keep build order (already category-grouped)
    }
    lib.selected.retain(|&i| i < rows.len());

    // --- batch-add affordance ---
    let n_sel = lib.selected.len();
    ui.horizontal(|ui| {
        let label = if n_sel > 1 { format!("{}  Add {n_sel}", icon::ADD) } else { format!("{}  Add", icon::ADD) };
        if ui.add_enabled(n_sel > 0, egui::Button::new(label)).on_hover_text("Add the selected templates to the scene (Enter)").clicked()
            || (n_sel > 0 && ui.input(|i| i.key_pressed(egui::Key::Enter)))
        {
            let mut idxs = lib.selected.clone();
            idxs.sort_unstable();
            let mut last = None;
            for &ri in &idxs {
                if let Some(row) = rows.get(ri) {
                    last = Some(add_library_row(row, library, scene));
                }
            }
            if let Some(sel) = last {
                *selection = sel;
            }
        }
        ui.label(RichText::new(format!("{} items", rows.len())).weak().small());
    });
    ui.separator();

    // --- the list (rich, selectable rows; shift = range, ⌘/Ctrl = toggle) ---
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut last_cat = String::new();
        let mut add_now: Option<usize> = None;
        let mut clicked: Option<(usize, egui::Modifiers)> = None;
        for (ri, row) in rows.iter().enumerate() {
            if lib.sort == LibSort::Category && row.category != last_cat {
                last_cat = row.category.clone();
                ui.add_space(4.0);
                ui.label(RichText::new(row.category.to_uppercase()).size(10.0).strong().color(ink.tertiary));
            }
            let selected = lib.selected.contains(&ri);
            let row_resp = library_row_widget(ui, row, selected, &ink, accent);
            if row_resp.clicked() {
                clicked = Some((ri, ui.input(|i| i.modifiers)));
            }
            if row_resp.double_clicked() {
                add_now = Some(ri);
            }
        }
        // Apply a click (after the loop so we don't borrow rows mutably mid-iter).
        if let Some((ri, mods)) = clicked {
            apply_lib_click(lib, ri, &mods, rows.len());
        }
        if let Some(ri) = add_now
            && let Some(row) = rows.get(ri)
        {
            *selection = add_library_row(row, library, scene);
        }
    });
}

/// Range/toggle/replace selection logic for the library list.
fn apply_lib_click(lib: &mut LibState, ri: usize, mods: &egui::Modifiers, _len: usize) {
    if mods.shift {
        let a = lib.anchor.unwrap_or(ri);
        let (lo, hi) = (a.min(ri), a.max(ri));
        lib.selected = (lo..=hi).collect();
        // keep the anchor for chained shift-clicks
    } else if mods.command || mods.ctrl {
        if let Some(p) = lib.selected.iter().position(|&x| x == ri) {
            lib.selected.remove(p);
        } else {
            lib.selected.push(ri);
        }
        lib.anchor = Some(ri);
    } else {
        lib.selected = vec![ri];
        lib.anchor = Some(ri);
    }
}

/// Paint left-anchored text truncated with an ellipsis to `max_w` (single row) —
/// used by the dense list rows so a long name can't run under the right column.
fn paint_truncated(
    painter: &egui::Painter,
    top_left: egui::Pos2,
    text: &str,
    size: f32,
    color: Color32,
    max_w: f32,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat { font_id: egui::FontId::proportional(size), color, ..Default::default() },
    );
    job.wrap = TextWrapping {
        max_width: max_w.max(8.0),
        max_rows: 1,
        overflow_character: Some('…'),
        ..Default::default()
    };
    let galley = painter.layout_job(job);
    painter.galley(top_left, galley, color);
}

/// Outcome of one scene-outliner row: the body response (drives selection) plus
/// whether the visibility eye was clicked this frame (toggles hide).
struct RowOut {
    body: egui::Response,
    eye_clicked: bool,
}

/// A scene-outliner row, Blender-outliner style: an optional left **active** bar,
/// a type icon, the name + secondary line, a right-aligned patch tag / conflict
/// badge, and (when `show_eye`) a far-right **visibility eye** toggle. Full-width
/// clickable; the active (primary) row reads brighter than merely-selected rows,
/// and a hidden row is dimmed. Shared by fixtures, objects, and environments.
#[allow(clippy::too_many_arguments)]
fn entity_row(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    secondary: &str,
    patch_tag: &str,
    conflict: bool,
    selected: bool,
    active: bool,
    hidden: bool,
    show_eye: bool,
    ink: &theme::Ink,
    accent: Color32,
) -> RowOut {
    let h = 34.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    if selected {
        painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
        // The active (primary) item gets a full accent outline; passive
        // multi-selection members get a quieter one.
        let w = if active { 1.5 } else { 1.0 };
        let col = if active { accent } else { accent.gamma_multiply(0.6) };
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(w, col), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, visuals.widgets.hovered.bg_fill);
    }
    // Active marker: a left accent bar (Blender's active-object emphasis).
    if active {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.left_top(), egui::vec2(2.5, h)),
            0.0,
            accent,
        );
    }
    // Dim hidden rows so the eye state reads at a glance.
    let dim = if hidden { 0.45 } else { 1.0 };

    // Far-right visibility eye (its own hit-test, so it doesn't trigger select).
    let mut eye_clicked = false;
    let mut right_edge = 9.0;
    if show_eye {
        let eye_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 27.0, rect.top()),
            egui::vec2(24.0, h),
        );
        let eye = ui.interact(eye_rect, resp.id.with("eye"), Sense::click());
        let glyph = if hidden { theme::icon::EYE_OFF } else { theme::icon::EYE };
        let col = if hidden {
            ink.muted
        } else if eye.hovered() {
            ink.primary
        } else {
            ink.tertiary
        };
        painter.text(eye_rect.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(14.0), col);
        eye.clone().on_hover_text(if hidden { "Hidden — click to show" } else { "Visible — click to hide" });
        eye_clicked = eye.clicked();
        right_edge = 30.0;
    }

    painter.text(
        rect.left_center() + egui::vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        (if selected { accent } else { ink.secondary }).gamma_multiply(dim),
    );
    // Left text zone is bounded so a long name can't run under the right column.
    let text_w = (rect.width() - 30.0 - 60.0 - (right_edge - 9.0)).max(40.0);
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 4.0), name, 13.0, ink.primary.gamma_multiply(dim), text_w);
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 19.0), secondary, 10.5, ink.tertiary.gamma_multiply(dim), text_w);
    // Right column: patch tag on top, conflict badge below (left of the eye).
    if !patch_tag.is_empty() {
        let unpatched = patch_tag == "unpatched";
        painter.text(
            egui::pos2(rect.right() - right_edge, rect.top() + 6.0),
            egui::Align2::RIGHT_TOP,
            patch_tag,
            egui::FontId::monospace(10.0),
            if unpatched { ink.muted } else { ink.tertiary },
        );
    }
    if conflict {
        painter.text(
            egui::pos2(rect.right() - right_edge, rect.bottom() - 5.0),
            egui::Align2::RIGHT_BOTTOM,
            theme::icon::WARNING,
            egui::FontId::proportional(12.0),
            theme::CONFLICT,
        );
    }
    RowOut { body: resp, eye_clicked }
}

/// One library row: icon + name (strong) + dim meta, full-width clickable, with
/// selection highlight + hover. Returns the row response.
fn library_row_widget(
    ui: &mut egui::Ui,
    row: &LibRow,
    selected: bool,
    ink: &theme::Ink,
    accent: Color32,
) -> egui::Response {
    let h = 34.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    if selected {
        painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, visuals.widgets.hovered.bg_fill);
    }
    let icon_color = if row.accent { accent } else { ink.secondary };
    painter.text(
        rect.left_center() + egui::vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        row.icon,
        egui::FontId::proportional(16.0),
        icon_color,
    );
    let text_w = (rect.width() - 30.0 - 22.0).max(40.0);
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 4.0), &row.name, 13.0, ink.primary, text_w);
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 19.0), &row.meta, 10.5, ink.tertiary, text_w);
    // A "+" affordance on hover, right-aligned.
    if resp.hovered() {
        painter.text(
            rect.right_center() + egui::vec2(-9.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            theme::icon::ADD,
            egui::FontId::proportional(15.0),
            ink.secondary,
        );
    }
    resp.on_hover_text("Click to select · double-click to add · Shift = range")
}

/// Right tab: editable parameters for the current selection. Edits flow
/// straight into the scene, so the viewport updates on the next frame.
#[allow(clippy::too_many_arguments)]
pub fn inspector(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &Selection,
    patch: &mut PatchTable,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
    profile: &mut Option<ProfileEditor>,
    sources: &ScreenSources,
) {
    ui.heading("Inspector");
    ui.separator();

    // World is the top of the hierarchy: its HDRI sky + ambient controls.
    if selection.world {
        let ink = theme::ink(!ui.visuals().dark_mode);
        world_inspector(ui, &mut scene.world, &ink);
        return;
    }

    if let Some(env_id) = selection.environment {
        match scene.environments.get_mut(env_id) {
            Some(env) => environment_inspector(ui, env),
            None => {
                ui.label("Selection is no longer valid.");
            }
        }
        return;
    }

    // Static geometry (Objects) takes the Inspector when selected.
    let geo: Vec<usize> = selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
    if !geo.is_empty() {
        geometry_inspector(ui, scene, &geo);
        return;
    }

    // LED screens take the Inspector when selected.
    let scr: Vec<usize> = selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
    if let Some(&primary) = scr.first() {
        led_screen_inspector(ui, &mut scene.screens[primary], scr.len(), sources);
        return;
    }

    // Keep only still-valid fixture indices.
    let ids: Vec<usize> = selection
        .fixtures
        .iter()
        .copied()
        .filter(|&i| i < scene.fixtures.len())
        .collect();
    match ids.as_slice() {
        [] => {
            ui.label("Nothing selected.");
        }
        [id] => {
            let id = *id;
            let fixture = &mut scene.fixtures[id];
            if fixture.is_gdtf() {
                gdtf_inspector(ui, fixture, gdtf_textures, id, profile);
            } else {
                fixture_inspector(ui, fixture);
            }
        }
        many => bulk_inspector(ui, scene, patch, many),
    }
}

/// Bulk editor shown when several fixtures are selected: edits a shared property
/// on **all** of them at once (set-semantics, seeded from the first selected).
/// Categories are collapsible and the Optics / Wheels rows are **dynamic** — they
/// show the union of controls the selected fixtures actually expose, not a fixed
/// hardcoded list.
fn bulk_inspector(ui: &mut egui::Ui, scene: &mut Scene, patch: &mut PatchTable, ids: &[usize]) {
    let primary = ids[0];
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  {} fixtures", theme::icon::FIXTURE, ids.len())).strong());
    });
    ui.label(RichText::new("Bulk edit — changes apply to all selected.").weak().small());
    ui.separator();

    // --- DMX MODE (only when every selected fixture shares one profile, so a
    // single mode list applies to all). Drives the patch footprint; decode syncs
    // each fixture's active mode from the patch next frame.
    let p0 = scene.fixtures[primary].profile.clone();
    let same_profile = ids.iter().all(|&i| scene.fixtures[i].profile == p0);
    let ref_modes: Vec<String> = if same_profile {
        scene.fixtures[primary]
            .gdtf
            .as_ref()
            .map(|g| g.modes.iter().map(|m| m.name.clone()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if ref_modes.len() > 1 {
        let cur = patch.get(primary).map(|p| p.mode_index).unwrap_or(0);
        let cur_name = ref_modes.get(cur).cloned().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("DMX mode");
            let mut pick = None;
            egui::ComboBox::from_id_salt("bulk-mode")
                .selected_text(RichText::new(cur_name).small())
                .show_ui(ui, |ui| {
                    for (mi, name) in ref_modes.iter().enumerate() {
                        if ui.selectable_label(mi == cur, name).clicked() {
                            pick = Some(mi);
                        }
                    }
                });
            if let Some(mi) = pick {
                for &i in ids {
                    let f = &scene.fixtures[i];
                    if f.gdtf.as_ref().is_some_and(|g| mi < g.modes.len()) {
                        patch.set_mode(f, i, mi);
                    }
                }
            }
        });
        ui.separator();
    }

    // --- TRANSFORM ---
    egui::CollapsingHeader::new(format!("{}  Transform", theme::icon::INSPECTOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("bulk-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                let mut pan = scene.fixtures[primary].pan;
                ui.label("Pan");
                if ui.add(DragValue::new(&mut pan).speed(0.5).range(-270.0..=270.0).suffix("°")).changed() {
                    for &i in ids {
                        scene.fixtures[i].pan = pan;
                    }
                }
                ui.end_row();
                let mut tilt = scene.fixtures[primary].tilt;
                ui.label("Tilt");
                if ui.add(DragValue::new(&mut tilt).speed(0.5).range(-180.0..=180.0).suffix("°")).changed() {
                    for &i in ids {
                        scene.fixtures[i].tilt = tilt;
                    }
                }
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Nudge position (all)").small().strong());
            ui.horizontal(|ui| {
                let mut delta = glam::Vec3::ZERO;
                // Drag from zero applies a delta; the field snaps back each frame.
                for (axis, label) in [(0usize, "x"), (1, "y"), (2, "z")] {
                    let mut v = 0.0f32;
                    if ui.add(DragValue::new(&mut v).speed(0.05).prefix(format!("{label} "))).changed() {
                        delta[axis] += v;
                    }
                }
                if delta != glam::Vec3::ZERO {
                    for &i in ids {
                        scene.fixtures[i].position += delta;
                    }
                }
            });
        });

    // --- FIXTURE ---
    egui::CollapsingHeader::new(format!("{}  Fixture", theme::icon::COLOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("bulk-fixture").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                let mut dimmer = scene.fixtures[primary].optics.dimmer;
                ui.label("Dimmer");
                if ui.add(Slider::new(&mut dimmer, 0.0..=1.0)).changed() {
                    for &i in ids {
                        scene.fixtures[i].optics.dimmer = dimmer;
                    }
                }
                ui.end_row();
                let mut beam = scene.fixtures[primary].beam;
                ui.label("Beam");
                if ui
                    .add(Slider::new(&mut beam, 0.0..=4.0).text("vol"))
                    .on_hover_text("Volumetric beam intensity (0 = off)")
                    .changed()
                {
                    for &i in ids {
                        scene.fixtures[i].beam = beam;
                    }
                }
                ui.end_row();
                let mut color = scene.fixtures[primary].color;
                ui.label("Color");
                if ui.color_edit_button_rgb(&mut color).changed() {
                    for &i in ids {
                        scene.fixtures[i].color = color;
                    }
                }
                ui.end_row();
            });
        });

    // --- OPTICS (dynamic): only fields some selected fixture actually exposes ---
    let supports = |f: OpticField| {
        ids.iter().any(|&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
    };
    let beam: Vec<OpticField> = OpticField::BEAM.into_iter().filter(|&f| supports(f)).collect();
    let color: Vec<OpticField> = OpticField::COLOR.into_iter().filter(|&f| supports(f)).collect();
    if !beam.is_empty() || !color.is_empty() {
        egui::CollapsingHeader::new(format!("{}  Optics", theme::icon::INSPECTOR))
            .default_open(true)
            .show(ui, |ui| {
                if !beam.is_empty() {
                    ui.label(RichText::new("BEAM SHAPING").small().strong());
                    Grid::new("bulk-beam").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                        for f in beam {
                            bulk_opt_field(ui, scene, ids, f);
                        }
                    });
                }
                if !color.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("COLOR MIXING").small().strong());
                    Grid::new("bulk-color").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                        for f in color {
                            bulk_opt_field(ui, scene, ids, f);
                        }
                    });
                }
            });
    }

    // --- WHEELS (dynamic): the union of components across all selected fixtures ---
    let mut wheels: Vec<(WheelKind, u32, String)> = Vec::new();
    for &i in ids {
        let f = &scene.fixtures[i];
        if let Some(comps) = f.gdtf.as_ref().and_then(|g| g.modes.get(f.mode_index)).map(|m| &m.components) {
            for c in comps {
                if !wheels.iter().any(|(k, n, _)| *k == c.kind && *n == c.number) {
                    wheels.push((c.kind, c.number, c.attribute.clone()));
                }
            }
        }
    }
    if !wheels.is_empty() {
        egui::CollapsingHeader::new(format!("{}  Wheels", theme::icon::COLOR))
            .default_open(true)
            .show(ui, |ui| {
                Grid::new("bulk-wheels").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for (kind, number, label) in &wheels {
                        bulk_wheel(ui, scene, ids, *kind, *number, label);
                    }
                });
            });
    }
}

/// Bulk rows for one wheel component: value + spin sliders applied to the
/// matching component of every selected fixture.
fn bulk_wheel(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    ids: &[usize],
    kind: WheelKind,
    number: u32,
    label: &str,
) {
    // Seed from the first selected fixture that actually has this wheel (the
    // union may include wheels the primary doesn't have).
    let Some((mut value, mut spin)) = ids
        .iter()
        .find_map(|&i| scene.fixtures[i].wheel_control_mut(kind, number).map(|w| (w.value, w.spin)))
    else {
        return;
    };
    ui.label(label);
    if ui.add(Slider::new(&mut value, 0.0..=1.0)).changed() {
        for &i in ids {
            if let Some(w) = scene.fixtures[i].wheel_control_mut(kind, number) {
                w.value = value;
            }
        }
    }
    ui.end_row();
    ui.label(format!("{label} spin"));
    if ui.add(Slider::new(&mut spin, 0.0..=1.0).text("0.5=stop")).changed() {
        for &i in ids {
            if let Some(w) = scene.fixtures[i].wheel_control_mut(kind, number) {
                w.spin = spin;
            }
        }
    }
    ui.end_row();
}

/// One bulk optics slider for an [`OpticField`], written to every selected
/// fixture (range-aware: e.g. green tint is bipolar). Seeds from the first
/// selected fixture that actually exposes the field (the union may include a
/// control the primary doesn't have), falling back to the primary.
fn bulk_opt_field(ui: &mut egui::Ui, scene: &mut Scene, ids: &[usize], f: OpticField) {
    let seed = ids
        .iter()
        .copied()
        .find(|&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
        .unwrap_or(ids[0]);
    let mut v = f.get(&scene.fixtures[seed].optics);
    ui.label(f.label());
    if ui.add(Slider::new(&mut v, f.range())).changed() {
        for &i in ids {
            f.set(&mut scene.fixtures[i].optics, v);
        }
    }
    ui.end_row();
}

fn fixture_inspector(ui: &mut egui::Ui, fixture: &mut Fixture) {
    ui.horizontal(|ui| {
        ui.heading(fixture.name.as_str());
    });
    ui.label(RichText::new(format!("{} · {}", fixture.category, fixture.profile)).weak().small());
    ui.separator();

    egui::CollapsingHeader::new(format!("{}  Transform", theme::icon::INSPECTOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("fx-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Position");
                ui.horizontal(|ui| {
                    ui.add(DragValue::new(&mut fixture.position.x).speed(0.05).prefix("x "));
                    ui.add(DragValue::new(&mut fixture.position.y).speed(0.05).prefix("y "));
                    ui.add(DragValue::new(&mut fixture.position.z).speed(0.05).prefix("z "));
                });
                ui.end_row();
                ui.label("Pan");
                ui.add(DragValue::new(&mut fixture.pan).speed(0.5).range(-270.0..=270.0).suffix("°"));
                ui.end_row();
                ui.label("Tilt");
                ui.add(DragValue::new(&mut fixture.tilt).speed(0.5).range(-180.0..=180.0).suffix("°"));
                ui.end_row();
            });
        });

    egui::CollapsingHeader::new(format!("{}  Fixture", theme::icon::COLOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("fx-fixture").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Dimmer");
                ui.add(DragValue::new(&mut fixture.optics.dimmer).speed(0.005).range(0.0..=1.0));
                ui.end_row();
                ui.label("Beam");
                ui.add(DragValue::new(&mut fixture.beam).speed(0.01).range(0.0..=4.0))
                    .on_hover_text("Volumetric beam intensity (0 = off, 1 = normal)");
                ui.end_row();
                ui.label("Beam angle");
                ui.add(DragValue::new(&mut fixture.beam_angle).speed(0.2).range(2.0..=90.0).suffix("°"));
                ui.end_row();
                ui.label("Color");
                ui.color_edit_button_rgb(&mut fixture.color);
                ui.end_row();
            });
        });
}

fn environment_inspector(ui: &mut egui::Ui, env: &mut Environment) {
    ui.horizontal(|ui| {
        ui.heading(env.name.as_str());
    });
    ui.label(RichText::new(format!("{:?}", env.kind)).weak().small());
    ui.separator();

    egui::CollapsingHeader::new(format!("{}  Transform", theme::icon::INSPECTOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("env-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Center");
                ui.horizontal(|ui| {
                    ui.add(DragValue::new(&mut env.center.x).speed(0.1).prefix("x "));
                    ui.add(DragValue::new(&mut env.center.y).speed(0.1).prefix("y "));
                    ui.add(DragValue::new(&mut env.center.z).speed(0.1).prefix("z "));
                });
                ui.end_row();
                ui.label("Size");
                ui.horizontal(|ui| {
                    ui.add(DragValue::new(&mut env.size.x).speed(0.1).range(0.1..=500.0).prefix("w "));
                    ui.add(DragValue::new(&mut env.size.y).speed(0.1).range(0.1..=500.0).prefix("h "));
                    ui.add(DragValue::new(&mut env.size.z).speed(0.1).range(0.1..=500.0).prefix("d "));
                });
                ui.end_row();
            });
        });

    egui::CollapsingHeader::new(format!("{}  Volume", theme::icon::ENVIRONMENT))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("env-volume").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Density");
                ui.add(DragValue::new(&mut env.density).speed(0.005).range(0.0..=4.0));
                ui.end_row();
                ui.label("Anisotropy");
                ui.add(DragValue::new(&mut env.anisotropy).speed(0.005).range(-0.95..=0.95))
                    .on_hover_text("Henyey-Greenstein g (forward scattering > 0)");
                ui.end_row();
                ui.label("Uniformity");
                ui.add(egui::Slider::new(&mut env.uniformity, 0.0..=1.0))
                    .on_hover_text(
                        "1 = smooth even haze · 0 = clusters of smoke/clouds (dense \
                         pockets scatter brighter, with clear gaps between)",
                    );
                ui.end_row();
                ui.label("Cluster contrast");
                ui.add(egui::Slider::new(&mut env.cluster_contrast, 0.0..=1.0))
                    .on_hover_text(
                        "How much brighter/denser the clusters are vs the haze (and how \
                         clear the gaps). Higher = pockets pop harder. Pairs with low density.",
                    );
                ui.end_row();
                ui.label("Tint");
                ui.color_edit_button_rgb(&mut env.color);
                ui.end_row();
            });
        });
}

/// Inspector for a selected static-geometry object (an imported stage deck,
/// truss, or set piece): identity, visibility, and an editable world transform
/// (position / rotation / uniform scale), decomposed from its 4×4 and recomposed
/// only when a field changes (so a one-off non-uniform import isn't flattened).
fn geometry_inspector(ui: &mut egui::Ui, scene: &mut Scene, ids: &[usize]) {
    let primary = ids[0];
    let Some(g) = scene.geometry.get_mut(primary) else {
        ui.label("Selection is no longer valid.");
        return;
    };
    ui.heading(g.name.as_str());
    let kind = g.mvr.as_ref().map(|m| m.kind.as_str()).filter(|k| !k.is_empty()).unwrap_or("Object");
    ui.label(
        RichText::new(format!("{kind} · {} model{}", g.models.len(), if g.models.len() == 1 { "" } else { "s" }))
            .weak()
            .small(),
    );
    if ids.len() > 1 {
        ui.label(RichText::new(format!("{} objects — editing the active one", ids.len())).weak().small());
    }
    ui.separator();

    ui.horizontal(|ui| {
        let mut visible = !g.hidden;
        if ui.checkbox(&mut visible, "Visible").changed() {
            g.hidden = !visible;
        }
    });

    // Position is read/written via the translation column directly (lossless), so
    // a pure move never disturbs a non-uniform/sheared import. Rotation + scale
    // are decomposed for display and only re-composed (to a clean uniform basis)
    // when the user actually edits one of them.
    let (scale0, rot0, _trans0) = g.transform.to_scale_rotation_translation();
    let mut pos = g.transform.w_axis.truncate();
    let mut uscale = ((scale0.x + scale0.y + scale0.z) / 3.0).max(1e-3);
    let (ry, rx, rz) = rot0.to_euler(glam::EulerRot::YXZ);
    let (mut ey, mut ex, mut ez) = (ry.to_degrees(), rx.to_degrees(), rz.to_degrees());
    let mut pos_changed = false;
    let mut rs_changed = false;

    egui::CollapsingHeader::new(format!("{}  Transform", theme::icon::INSPECTOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("geo-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Position");
                ui.horizontal(|ui| {
                    pos_changed |= ui.add(DragValue::new(&mut pos.x).speed(0.05).prefix("x ")).changed();
                    pos_changed |= ui.add(DragValue::new(&mut pos.y).speed(0.05).prefix("y ")).changed();
                    pos_changed |= ui.add(DragValue::new(&mut pos.z).speed(0.05).prefix("z ")).changed();
                });
                ui.end_row();
                ui.label("Rotation");
                ui.horizontal(|ui| {
                    rs_changed |= ui.add(DragValue::new(&mut ex).speed(0.5).suffix("°").prefix("x ")).changed();
                    rs_changed |= ui.add(DragValue::new(&mut ey).speed(0.5).suffix("°").prefix("y ")).changed();
                    rs_changed |= ui.add(DragValue::new(&mut ez).speed(0.5).suffix("°").prefix("z ")).changed();
                });
                ui.end_row();
                ui.label("Scale");
                rs_changed |= ui.add(DragValue::new(&mut uscale).speed(0.005).range(0.001..=1000.0)).changed();
                ui.end_row();
            });
            if let Some((lo, hi)) = g.world_bounds() {
                let s = hi - lo;
                ui.label(
                    RichText::new(format!("size  {:.2} × {:.2} × {:.2} m", s.x, s.y, s.z)).weak().small(),
                );
            }
        });

    if rs_changed {
        let rot = glam::Quat::from_euler(glam::EulerRot::YXZ, ey.to_radians(), ex.to_radians(), ez.to_radians());
        g.transform = Mat4::from_scale_rotation_translation(Vec3::splat(uscale), rot, pos);
    } else if pos_changed {
        // Pure move: rewrite only the translation column, keeping the original
        // (possibly non-uniform) basis intact.
        g.transform.w_axis = pos.extend(1.0);
    }
}

/// Inspector for a selected LED screen: identity, transform, the parametric
/// cabinet grid (with a live derived-resolution readout), surface photometry,
/// and the content source. Phase 1 covers Test Pattern + Solid Colour content;
/// the cabinet is editable directly (the panel TYPE is set from the Library).
fn led_screen_inspector(ui: &mut egui::Ui, s: &mut LedScreen, count: usize, sources: &ScreenSources) {
    ui.heading(s.name.as_str());
    let [rx, ry] = s.resolution();
    let [mw, mh] = s.size_m();
    ui.label(
        RichText::new(format!("{} · {} × {} px · {:.2} × {:.2} m", s.panel_type, rx, ry, mw, mh))
            .weak()
            .small(),
    );
    if count > 1 {
        ui.label(RichText::new(format!("{count} screens — editing the active one")).weak().small());
    }
    ui.separator();

    ui.horizontal(|ui| {
        let mut visible = !s.hidden;
        if ui.checkbox(&mut visible, "Visible").changed() {
            s.hidden = !visible;
        }
    });

    // --- Transform (position / rotation / uniform scale, lossless like geometry) ---
    let (scale0, rot0, _t0) = s.transform.to_scale_rotation_translation();
    let mut pos = s.transform.w_axis.truncate();
    let mut uscale = ((scale0.x + scale0.y + scale0.z) / 3.0).max(1e-3);
    let (ryr, rxr, rzr) = rot0.to_euler(glam::EulerRot::YXZ);
    let (mut ey, mut ex, mut ez) = (ryr.to_degrees(), rxr.to_degrees(), rzr.to_degrees());
    let mut pos_changed = false;
    let mut rs_changed = false;
    egui::CollapsingHeader::new(format!("{}  Transform", theme::icon::INSPECTOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("led-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Position");
                ui.horizontal(|ui| {
                    pos_changed |= ui.add(DragValue::new(&mut pos.x).speed(0.05).prefix("x ")).changed();
                    pos_changed |= ui.add(DragValue::new(&mut pos.y).speed(0.05).prefix("y ")).changed();
                    pos_changed |= ui.add(DragValue::new(&mut pos.z).speed(0.05).prefix("z ")).changed();
                });
                ui.end_row();
                ui.label("Rotation");
                ui.horizontal(|ui| {
                    rs_changed |= ui.add(DragValue::new(&mut ex).speed(0.5).suffix("°").prefix("x ")).changed();
                    rs_changed |= ui.add(DragValue::new(&mut ey).speed(0.5).suffix("°").prefix("y ")).changed();
                    rs_changed |= ui.add(DragValue::new(&mut ez).speed(0.5).suffix("°").prefix("z ")).changed();
                });
                ui.end_row();
                ui.label("Scale");
                rs_changed |= ui.add(DragValue::new(&mut uscale).speed(0.005).range(0.001..=1000.0)).changed();
                ui.end_row();
            });
        });
    if rs_changed {
        let rot = glam::Quat::from_euler(glam::EulerRot::YXZ, ey.to_radians(), ex.to_radians(), ez.to_radians());
        s.transform = Mat4::from_scale_rotation_translation(Vec3::splat(uscale), rot, pos);
    } else if pos_changed {
        s.transform.w_axis = pos.extend(1.0);
    }

    // --- Panel: one cabinet's size + native pixels (pitch is derived) ---
    egui::CollapsingHeader::new(format!("{}  Panel", theme::icon::SCREEN))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("led-panel").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Cabinet (mm)");
                ui.horizontal(|ui| {
                    ui.add(DragValue::new(&mut s.cabinet_mm[0]).speed(1.0).range(50.0..=2000.0).prefix("w "));
                    ui.add(DragValue::new(&mut s.cabinet_mm[1]).speed(1.0).range(50.0..=2000.0).prefix("h "));
                });
                ui.end_row();
                ui.label("Pixels / cabinet");
                ui.horizontal(|ui| {
                    let mut px = s.cabinet_px[0] as i32;
                    let mut py = s.cabinet_px[1] as i32;
                    if ui.add(DragValue::new(&mut px).speed(1.0).range(8..=1024).prefix("x ")).changed() {
                        s.cabinet_px[0] = px.max(1) as u32;
                    }
                    if ui.add(DragValue::new(&mut py).speed(1.0).range(8..=1024).prefix("y ")).changed() {
                        s.cabinet_px[1] = py.max(1) as u32;
                    }
                });
                ui.end_row();
                ui.label("Pitch");
                ui.label(RichText::new(format!("{:.2} mm", s.pitch_mm())).weak());
                ui.end_row();
            });
        });

    // --- Array: panels wide × high → live derived total resolution + size ---
    egui::CollapsingHeader::new(format!("{}  Array", theme::icon::PATCH))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("led-array").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Panels");
                ui.horizontal(|ui| {
                    let mut w = s.panels_wide as i32;
                    let mut h = s.panels_high as i32;
                    if ui.add(DragValue::new(&mut w).speed(0.1).range(1..=64).prefix("w ")).changed() {
                        s.panels_wide = w.max(1) as u32;
                    }
                    if ui.add(DragValue::new(&mut h).speed(0.1).range(1..=64).prefix("h ")).changed() {
                        s.panels_high = h.max(1) as u32;
                    }
                });
                ui.end_row();
                ui.label("Gap");
                ui.add(DragValue::new(&mut s.gap_mm).speed(0.1).range(0.0..=50.0).suffix(" mm"));
                ui.end_row();
            });
            let [rx, ry] = s.resolution();
            let [mw, mh] = s.size_m();
            let mpx = (rx as f64 * ry as f64) / 1_000_000.0;
            ui.label(
                RichText::new(format!("{rx} × {ry} px  ·  {mpx:.2} Mpx  ·  {mw:.2} × {mh:.2} m"))
                    .strong()
                    .small(),
            );
        });

    // --- Surface: photometry + transparency + curvature ---
    egui::CollapsingHeader::new(format!("{}  Surface", theme::icon::COLOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("led-surface").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Brightness");
                ui.add(DragValue::new(&mut s.nits).speed(10.0).range(50.0..=8000.0).suffix(" nits"));
                ui.end_row();
                ui.label("Light emit");
                ui.add(DragValue::new(&mut s.emit).speed(0.02).range(0.0..=4.0))
                    .on_hover_text("How much the wall lights the scene + haze (0 = none)");
                ui.end_row();
                ui.label("Gamma");
                ui.add(DragValue::new(&mut s.gamma).speed(0.01).range(1.0..=3.0));
                ui.end_row();
                ui.label("Transparency");
                let mut transp = 1.0 - s.opacity;
                if ui.add(Slider::new(&mut transp, 0.0..=1.0)).on_hover_text("See-through / mesh LED").changed() {
                    s.opacity = (1.0 - transp).clamp(0.0, 1.0);
                }
                ui.end_row();
                ui.label("Curvature");
                ui.add(DragValue::new(&mut s.curvature_deg).speed(0.5).range(-60.0..=60.0).suffix("°"))
                    .on_hover_text("Horizontal arc subtended across the wall");
                ui.end_row();
                ui.label("Pixel");
                egui::ComboBox::from_id_salt("led-pixel-shape")
                    .selected_text(s.pixel_shape.label())
                    .show_ui(ui, |ui| {
                        for sh in PixelShape::ALL {
                            ui.selectable_value(&mut s.pixel_shape, sh, sh.label());
                        }
                    })
                    .response
                    .on_hover_text("LED package shape seen up close (SMD round/square, or discrete RGB sub-pixels)");
                ui.end_row();
            });
        });

    // --- Content: the source shown on the surface ---
    egui::CollapsingHeader::new(format!("{}  Content", theme::icon::IMAGE))
        .default_open(true)
        .show(ui, |ui| {
            #[derive(PartialEq, Clone, Copy)]
            enum Kind {
                Test,
                Solid,
                Image,
                Ndi,
                Citp,
                Dmx,
            }
            let cur = match &s.content {
                ScreenContent::TestPattern(_) => Kind::Test,
                ScreenContent::SolidColor(_) => Kind::Solid,
                ScreenContent::Image { .. } => Kind::Image,
                ScreenContent::Ndi { .. } => Kind::Ndi,
                ScreenContent::Citp { .. } => Kind::Citp,
                ScreenContent::PixelMapDmx(_) => Kind::Dmx,
            };
            let mut sel = cur;
            ui.horizontal(|ui| {
                ui.label("Source");
                egui::ComboBox::from_id_salt("led-source").selected_text(s.content.label()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, Kind::Test, "Test Pattern");
                    ui.selectable_value(&mut sel, Kind::Solid, "Solid Colour");
                    ui.selectable_value(&mut sel, Kind::Image, "Image…");
                    ui.selectable_value(&mut sel, Kind::Ndi, "NDI");
                    ui.selectable_value(&mut sel, Kind::Citp, "CITP");
                    ui.selectable_value(&mut sel, Kind::Dmx, "Pixel-map DMX");
                });
            });
            if sel != cur {
                s.frame = None; // drop any live frame from the previous source
                s.content = match sel {
                    Kind::Test => ScreenContent::TestPattern(TestPattern::Grid),
                    Kind::Solid => ScreenContent::SolidColor([0.1, 0.4, 0.9]),
                    Kind::Image => {
                        ScreenContent::Image { name: String::new(), bytes: std::sync::Arc::new(Vec::new()) }
                    }
                    Kind::Ndi => ScreenContent::Ndi { source: String::new() },
                    Kind::Citp => ScreenContent::Citp { source: String::new() },
                    Kind::Dmx => ScreenContent::PixelMapDmx(crate::scene::screen::PixelMap::default()),
                };
            }
            ui.add_space(2.0);
            match &mut s.content {
                ScreenContent::TestPattern(tp) => {
                    ui.horizontal(|ui| {
                        ui.label("Pattern");
                        for p in TestPattern::ALL {
                            ui.selectable_value(tp, p, p.label());
                        }
                    });
                }
                ScreenContent::SolidColor(c) => {
                    ui.horizontal(|ui| {
                        ui.label("Colour");
                        ui.color_edit_button_rgb(c);
                    });
                }
                ScreenContent::Image { name, bytes } => {
                    ui.horizontal(|ui| {
                        if ui.button("Choose image…").clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "gif", "webp", "tga", "exr", "hdr"])
                                .pick_file()
                        {
                            match std::fs::read(&path) {
                                Ok(b) => {
                                    *bytes = std::sync::Arc::new(b);
                                    *name = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("image")
                                        .to_string();
                                }
                                Err(e) => log::error!("read screen image: {e}"),
                            }
                        }
                        let label = if name.is_empty() { "no image".to_string() } else { name.clone() };
                        ui.label(RichText::new(label).weak());
                    });
                }
                ScreenContent::Ndi { source } => {
                    ui.horizontal(|ui| {
                        ui.label("Source");
                        ui.text_edit_singleline(source);
                    });
                    if !sources.ndi.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Discovered:").weak().small());
                            for name in &sources.ndi {
                                if ui.small_button(name).clicked() {
                                    *source = name.clone();
                                }
                            }
                        });
                    }
                    let hint = if sources.ndi_available {
                        "NDI source name (e.g. \"HOST (Output 1)\"). Pick a discovered source above."
                    } else {
                        "NDI runtime not available (build with `--features ndi` + install the NDI runtime)."
                    };
                    ui.label(RichText::new(hint).weak().small());
                }
                ScreenContent::Citp { source } => {
                    ui.horizontal(|ui| {
                        ui.label("Source");
                        ui.text_edit_singleline(source);
                    });
                    if !sources.citp.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Servers:").weak().small());
                            for name in &sources.citp {
                                if ui.small_button(name).clicked() {
                                    *source = name.clone();
                                }
                            }
                        });
                    }
                    ui.label(
                        RichText::new("CITP/MSEX media-server stream as \"server | layer\" (servers auto-discovered on the LAN).")
                            .weak()
                            .small(),
                    );
                }
                ScreenContent::PixelMapDmx(pm) => {
                    Grid::new("led-pixelmap").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                        ui.label("Grid");
                        ui.horizontal(|ui| {
                            let mut c = pm.cols as i32;
                            let mut r = pm.rows as i32;
                            if ui.add(DragValue::new(&mut c).speed(0.1).range(1..=64).prefix("cols ")).changed() {
                                pm.cols = c.max(1) as u32;
                            }
                            if ui.add(DragValue::new(&mut r).speed(0.1).range(1..=64).prefix("rows ")).changed() {
                                pm.rows = r.max(1) as u32;
                            }
                        });
                        ui.end_row();
                        ui.label("Patch");
                        ui.horizontal(|ui| {
                            let mut u = pm.universe as i32;
                            let mut a = pm.start_address as i32;
                            if ui.add(DragValue::new(&mut u).speed(0.1).range(0..=63999).prefix("univ ")).changed() {
                                pm.universe = u.clamp(0, 63999) as u16;
                            }
                            if ui.add(DragValue::new(&mut a).speed(0.5).range(1..=512).prefix("addr ")).changed() {
                                pm.start_address = a.clamp(1, 512) as u16;
                            }
                        });
                        ui.end_row();
                    });
                    let chans = pm.cols * pm.rows * 3;
                    ui.label(
                        RichText::new(format!(
                            "{}×{} cells · {chans} ch (RGB) · low-res only — use NDI/CITP/media for hi-res",
                            pm.cols, pm.rows
                        ))
                        .weak()
                        .small(),
                    );
                }
            }
        });
}

/// Inspector for an imported GDTF fixture: identity + thumbnail, editable
/// instance params, wheels (with slot images), and the DMX modes/channels.
fn gdtf_inspector(
    ui: &mut egui::Ui,
    fixture: &mut Fixture,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
    fixture_id: usize,
    profile: &mut Option<ProfileEditor>,
) {
    let gdtf = fixture.gdtf.clone().expect("gdtf");
    let key = Arc::as_ptr(&gdtf) as usize;
    let tex = gdtf_textures
        .entry(key)
        .or_insert_with(|| load_gdtf_textures(ui.ctx(), &gdtf));

    ui.horizontal(|ui| {
        ui.heading(gdtf.name.as_str());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!("{}  Profile…", theme::icon::PROFILE))
                .on_hover_text("Open the full fixture profile editor")
                .clicked()
            {
                *profile = Some(ProfileEditor::new(fixture_id));
            }
        });
    });
    ui.label(
        RichText::new(format!("{} · {}", gdtf.manufacturer, gdtf.long_name))
            .weak()
            .small(),
    );

    if let Some(thumb) = &tex.thumbnail {
        let s = thumb.size_vec2();
        let w = 200.0_f32.min(ui.available_width());
        let h = w * s.y / s.x.max(1.0);
        ui.add_space(4.0);
        ui.image((thumb.id(), egui::vec2(w, h)));
    }

    // Physical source / beam spec from the GDTF Beam geometry.
    let b = &gdtf.beam;
    ui.label(
        RichText::new(format!(
            "{} engine · {:.0} K · CRI {:.0} · {:.0} lm · {:.0} W",
            b.lamp_type, b.color_temp, b.cri, b.luminous_flux, b.power
        ))
        .weak()
        .small(),
    );
    ui.label(
        RichText::new(format!(
            "{} · beam {:.0}° / field {:.0}° · throw {:.2}",
            b.beam_type, b.beam_angle, b.field_angle, b.throw_ratio
        ))
        .weak()
        .small(),
    );
    // Multi-emitter summary: cell count + the live per-cell colors (driven by
    // per-pixel DMX; the Color picker below multiplies all of them manually).
    let emitters = fixture.emitters();
    if emitters.len() > 1 {
        let visible = emitters.iter().filter(|e| e.merged_into.is_none()).count();
        ui.label(
            RichText::new(format!(
                "{} emitters · {} {} · per-cell DMX in mode \"{}\"",
                visible,
                emitters[0].beam.beam_type,
                if emitters.len() > visible { "(+1 overlay)" } else { "" },
                gdtf.modes
                    .get(fixture.mode_index)
                    .map(|m| m.name.as_str())
                    .unwrap_or("?"),
            ))
            .weak()
            .small(),
        );
        ui.horizontal_wrapped(|ui| {
            for (i, em) in emitters.iter().enumerate() {
                if em.merged_into.is_some() {
                    continue;
                }
                let c = fixture.cells.get(i).copied().unwrap_or([1.0, 1.0, 1.0]);
                let level = (fixture.intensity * fixture.optics.dimmer).clamp(0.0, 1.0);
                let col = egui::Color32::from_rgb(
                    ((c[0].min(1.0) * level).powf(1.0 / 2.2) * 255.0) as u8,
                    ((c[1].min(1.0) * level).powf(1.0 / 2.2) * 255.0) as u8,
                    ((c[2].min(1.0) * level).powf(1.0 / 2.2) * 255.0) as u8,
                );
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                ui.painter().rect_filled(rect, 7.0, col);
                ui.painter().rect_stroke(
                    rect,
                    7.0,
                    egui::Stroke::new(1.0, egui::Color32::from_gray(70)),
                    egui::StrokeKind::Inside,
                );
                resp.on_hover_text(&em.name);
            }
        });
    }

    // MVR patch identity (FixtureID, DMX address, mode) when imported from a scene.
    if let Some(m) = fixture.mvr.as_deref() {
        let id = if m.fixture_id.is_empty() { "—" } else { m.fixture_id.as_str() };
        let addr = m
            .addresses
            .first()
            .map(|a| format!("{}.{:03}", a.universe(), a.channel()))
            .unwrap_or_else(|| "—".into());
        let mode = if m.gdtf_mode.is_empty() { "—" } else { m.gdtf_mode.as_str() };
        ui.label(
            RichText::new(format!("MVR · ID {id} · addr {addr} · {mode}"))
                .weak()
                .small(),
        )
        .on_hover_text("Fixture ID · DMX universe.channel · mode, from the imported MVR patch");
    }

    ui.separator();
    egui::CollapsingHeader::new(format!("{}  Transform", theme::icon::INSPECTOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("gdtf-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Position");
                ui.horizontal(|ui| {
                    ui.add(DragValue::new(&mut fixture.position.x).speed(0.05).prefix("x "));
                    ui.add(DragValue::new(&mut fixture.position.y).speed(0.05).prefix("y "));
                    ui.add(DragValue::new(&mut fixture.position.z).speed(0.05).prefix("z "));
                });
                ui.end_row();
                ui.label("Pan");
                ui.add(DragValue::new(&mut fixture.pan).speed(0.5).range(-270.0..=270.0).suffix("°"))
                    .on_hover_text(format!("commanded · now {:.0}°", fixture.pan_actual));
                ui.end_row();
                ui.label("Tilt");
                ui.add(DragValue::new(&mut fixture.tilt).speed(0.5).range(-135.0..=135.0).suffix("°"))
                    .on_hover_text(format!("commanded · now {:.0}°", fixture.tilt_actual));
                ui.end_row();
                ui.label("Move speed")
                    .on_hover_text("Pan/tilt motor speed: 0 = fastest (snap), 1 = slowest");
                ui.add(Slider::new(&mut fixture.move_speed, 0.0..=1.0));
                ui.end_row();
            });
        });

    egui::CollapsingHeader::new(format!("{}  Fixture", theme::icon::COLOR))
        .default_open(true)
        .show(ui, |ui| {
            Grid::new("gdtf-fixture").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                ui.label("Dimmer");
                ui.add(DragValue::new(&mut fixture.optics.dimmer).speed(0.005).range(0.0..=1.0));
                ui.end_row();
                ui.label("Beam");
                ui.add(DragValue::new(&mut fixture.beam).speed(0.01).range(0.0..=4.0))
                    .on_hover_text("Volumetric beam intensity (0 = off, 1 = normal)");
                ui.end_row();
                ui.label("Color");
                ui.color_edit_button_rgb(&mut fixture.color);
                ui.end_row();
            });
        });

    optics_section(ui, fixture, &gdtf);

    egui::CollapsingHeader::new(format!("Wheels ({})", gdtf.wheels.len()))
        .default_open(false)
        .show(ui, |ui| {
            for (wi, wheel) in gdtf.wheels.iter().enumerate() {
                ui.label(RichText::new(&wheel.name).strong().small());
                ui.horizontal_wrapped(|ui| {
                    for (si, slot) in wheel.slots.iter().enumerate() {
                        let handle = tex
                            .wheels
                            .get(wi)
                            .and_then(|w| w.get(si))
                            .and_then(|h| h.as_ref());
                        let size = egui::vec2(42.0, 42.0);
                        if let Some(h) = handle {
                            ui.image((h.id(), size)).on_hover_text(&slot.name);
                        } else {
                            let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
                            let col = slot
                                .color
                                .map(|c| {
                                    egui::Color32::from_rgb(
                                        (c[0] * 255.0) as u8,
                                        (c[1] * 255.0) as u8,
                                        (c[2] * 255.0) as u8,
                                    )
                                })
                                .unwrap_or(egui::Color32::from_gray(40));
                            ui.painter().rect_filled(rect, 4.0, col);
                            resp.on_hover_text(&slot.name);
                        }
                    }
                });
                ui.add_space(4.0);
            }
        });

    egui::CollapsingHeader::new(format!("DMX modes ({})", gdtf.modes.len()))
        .show(ui, |ui| {
            for mode in &gdtf.modes {
                egui::CollapsingHeader::new(format!("{} — {} ch", mode.name, mode.footprint))
                    .id_salt(&mode.name)
                    .show(ui, |ui| {
                        Grid::new(format!("dmx-{}", mode.name))
                            .num_columns(3)
                            .striped(true)
                            .spacing([10.0, 2.0])
                            .show(ui, |ui| {
                                ui.strong("Addr");
                                ui.strong("Attribute");
                                ui.strong("Function");
                                ui.end_row();
                                for ch in &mode.channels {
                                    let addr = ch
                                        .offsets
                                        .first()
                                        .map(|o| o.to_string())
                                        .unwrap_or_else(|| "—".into());
                                    ui.monospace(addr);
                                    ui.label(&ch.attribute);
                                    ui.label(&ch.function);
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
}

/// The optical-chain control bank for a GDTF fixture: sliders for every stage
/// the fixture actually exposes (disabled if the GDTF lacks that attribute).
/// Drives `fixture.optics`, which the renderer resolves into the beam each frame.
/// One data-driven optics slider row: label + range-aware slider (disabled when
/// the fixture doesn't expose it), with optional trailing text (e.g. zoom °).
fn optic_field_row(
    ui: &mut egui::Ui,
    o: &mut OpticalControls,
    f: OpticField,
    enabled: bool,
    text: Option<String>,
) {
    let resp = ui.label(f.label());
    if f == OpticField::Green {
        resp.on_hover_text("Plus/minus-green (CC axis): −1 magenta … +1 green");
    }
    let mut v = f.get(o);
    let mut slider = Slider::new(&mut v, f.range());
    if let Some(t) = text {
        slider = slider.text(t);
    }
    if ui.add_enabled(enabled, slider).changed() {
        f.set(o, v);
    }
    ui.end_row();
}

fn optics_section(ui: &mut egui::Ui, fixture: &mut Fixture, gdtf: &GdtfFixture) {
    let beam_angle = fixture.beam_angle;
    // The dynamic wheel chain of the active mode (any number of color/gobo/
    // prism/animation/frost components).
    let components: Vec<crate::gdtf::OpticalComponent> = gdtf
        .modes
        .get(fixture.mode_index)
        .map(|m| m.components.clone())
        .unwrap_or_default();
    fixture.optics.ensure_wheels(components.len());

    egui::CollapsingHeader::new("Optics")
        .default_open(true)
        .show(ui, |ui| {
            let zoom_deg = optics::map_attr(gdtf, "Zoom", fixture.optics.zoom, (beam_angle, beam_angle));
            // Shutter blade style — OUR editable model (GDTF lacks blade geometry).
            // Only shown for fixtures that actually have a shutter (or already set
            // one), so a plain PAR/wash isn't offered a blade it can't use.
            if crate::optics::OpticField::Shutter.supported(gdtf)
                || fixture.shutter != crate::optics::ShutterKind::None
            {
                ui.horizontal(|ui| {
                    ui.label("Shutter blades");
                    egui::ComboBox::from_id_salt("shutter-kind")
                        .selected_text(fixture.shutter.label())
                        .show_ui(ui, |ui| {
                            for k in crate::optics::ShutterKind::ALL {
                                ui.selectable_value(&mut fixture.shutter, k, k.label());
                            }
                        });
                });
            }
            let o = &mut fixture.optics;
            // Data-driven rows (gated by the fixture's GDTF attributes) so single
            // and bulk editing enumerate the SAME control set — see `OpticField`.
            ui.label(RichText::new("BEAM SHAPING").small().strong());
            Grid::new("optics-beam").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                for f in OpticField::BEAM {
                    optic_field_row(ui, o, f, f.supported(gdtf), (f == OpticField::Zoom).then(|| format!("{zoom_deg:.0}°")));
                }
            });

            ui.add_space(4.0);
            ui.label(RichText::new("COLOR MIXING").small().strong());
            Grid::new("optics-color").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                for f in OpticField::COLOR {
                    optic_field_row(ui, o, f, f.supported(gdtf), None);
                }
            });

            // One block per wheel component, generated from the GDTF chain.
            if !components.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("WHEELS").small().strong());
                Grid::new("optics-wheels").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for (i, comp) in components.iter().enumerate() {
                        let Some(w) = o.wheels.get_mut(i) else { continue };
                        let value_label = match comp.kind {
                            WheelKind::Gobo | WheelKind::Color => "select",
                            WheelKind::Prism | WheelKind::Animation | WheelKind::Frost => "insert",
                        };
                        let name = comp
                            .wheel
                            .as_deref()
                            .map(|n| format!("{} · {n}", comp.attribute))
                            .unwrap_or_else(|| comp.attribute.clone());
                        ui.label(RichText::new(name).strong());
                        ui.add(Slider::new(&mut w.value, 0.0..=1.0).text(value_label));
                        ui.end_row();
                        // Prism always exposes rotation (index + spin) even when the
                        // profile didn't flag a dedicated Pos/PosRotate function.
                        if comp.has_index || comp.kind == WheelKind::Prism {
                            ui.label("  index");
                            ui.add(Slider::new(&mut w.index, 0.0..=1.0));
                            ui.end_row();
                        }
                        if comp.has_spin || matches!(comp.kind, WheelKind::Color | WheelKind::Animation | WheelKind::Prism) {
                            ui.label("  spin");
                            ui.add(Slider::new(&mut w.spin, 0.0..=1.0).text("0.5=stop"));
                            ui.end_row();
                        }
                        if matches!(comp.kind, WheelKind::Gobo | WheelKind::Color) {
                            ui.label("  shake");
                            ui.add(Slider::new(&mut w.shake, 0.0..=1.0))
                                .on_hover_text("Oscillate the indexed element");
                            ui.end_row();
                        }
                    }
                });
            }
        });
}

pub(super) fn load_gdtf_textures(ctx: &egui::Context, gdtf: &GdtfFixture) -> GdtfTextures {
    let thumbnail = gdtf
        .thumbnail
        .as_ref()
        .and_then(|b| decode_texture(ctx, "gdtf-thumb", b));
    let wheels = gdtf
        .wheels
        .iter()
        .map(|w| {
            w.slots
                .iter()
                .map(|s| {
                    s.media
                        .as_ref()
                        .and_then(|b| decode_texture(ctx, &s.name, b))
                })
                .collect()
        })
        .collect();
    GdtfTextures { thumbnail, wheels }
}

fn decode_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    // Downscale large wheel/thumbnail images for the panel.
    let img = img.thumbnail(256, 256);
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

/// Extend the selection to every fixture sharing a profile with the current
/// selection ("Select same type").
fn select_same_type(scene: &Scene, selection: &mut Selection) {
    let mut types: Vec<&str> =
        selection.fixtures.iter().filter_map(|&i| scene.fixtures.get(i)).map(|f| f.profile.as_str()).collect();
    types.sort_unstable();
    types.dedup();
    if types.is_empty() {
        return;
    }
    selection.fixtures = scene
        .fixtures
        .iter()
        .enumerate()
        .filter(|(_, f)| types.contains(&f.profile.as_str()))
        .map(|(i, _)| i)
        .collect();
    selection.environment = None;
}

/// Frame the camera on the selected fixtures (their AABB). No-op if nothing
/// selected.
fn frame_selection(scene: &Scene, selection: &Selection, camera: &mut OrbitCamera) {
    let mut it = selection.fixtures.iter().filter_map(|&i| scene.fixtures.get(i)).map(|f| f.position);
    let Some(first) = it.next() else { return };
    let (mut lo, mut hi) = (first, first);
    for p in it {
        lo = lo.min(p);
        hi = hi.max(p);
    }
    camera.frame_aabb(lo - Vec3::splat(0.5), hi + Vec3::splat(0.5));
}

/// Apply an in-progress modal transform to the scene from the current mouse
/// position. Reads the snapshot in `op.start`, so it's idempotent — called every
/// frame the op is live; cancelling restores from the same snapshot.
fn apply_transform(op: &TransformOp, scene: &mut Scene, camera: &OrbitCamera, cur: egui::Pos2) {
    // Position/orientation are read directly by the renderer, so they need no
    // snap_movement() — and calling it every frame would freeze each fixture's
    // wheel-motion phase. (Cancel restores from the same snapshot the same way.)
    let d = cur - op.start_screen; // pixel delta
    let (right, up, _fwd) = camera.view_basis();
    // A grabbed gizmo handle overrides the keyboard axis lock for this frame.
    let axis = op.active_axis();
    match op.kind {
        TransformKind::Move => {
            let speed = camera.distance * 0.0015;
            let mut world = right * (d.x * speed) + up * (-d.y * speed);
            if let Some(ax) = axis {
                let a = ax.vec();
                world = a * world.dot(a); // lock to one axis
            }
            for (i, p0, _q) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    f.position = *p0 + world;
                }
            }
            for (i, m0) in &op.geo_start {
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = Mat4::from_translation(world) * *m0;
                }
            }
        }
        TransformKind::Rotate => {
            let angle = d.x * 0.01;
            let raxis = axis.map(|a| a.vec()).unwrap_or(Vec3::Y);
            let rot = Quat::from_axis_angle(raxis, angle);
            for (i, p0, q0) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    f.position = op.pivot + rot * (*p0 - op.pivot);
                    f.orientation = rot * *q0;
                }
            }
            if !op.geo_start.is_empty() {
                let about = Mat4::from_translation(op.pivot)
                    * Mat4::from_quat(rot)
                    * Mat4::from_translation(-op.pivot);
                for (i, m0) in &op.geo_start {
                    if let Some(g) = scene.geometry.get_mut(*i) {
                        g.transform = about * *m0;
                    }
                }
            }
        }
        TransformKind::Scale => {
            let factor = (1.0 + d.x * 0.005).max(0.01);
            for (i, p0, _q) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    let off = *p0 - op.pivot;
                    let new = if let Some(ax) = op.axis {
                        let a = ax.vec();
                        let comp = a * off.dot(a);
                        (off - comp) + comp * factor // scale only the locked axis
                    } else {
                        off * factor
                    };
                    f.position = op.pivot + new;
                }
            }
            if !op.geo_start.is_empty() {
                // Scale about the pivot in world space — uniform, or only the
                // locked axis.
                let s = match op.axis {
                    Some(ax) => Vec3::ONE + ax.vec() * (factor - 1.0),
                    None => Vec3::splat(factor),
                };
                let about = Mat4::from_translation(op.pivot)
                    * Mat4::from_scale(s)
                    * Mat4::from_translation(-op.pivot);
                for (i, m0) in &op.geo_start {
                    if let Some(g) = scene.geometry.get_mut(*i) {
                        g.transform = about * *m0;
                    }
                }
            }
        }
    }
}

/// Live state for the Measure tool (§2.4) — a read-only two-point ruler that never
/// mutates the scene (so no op / no undo). `a` is the first point; once `b` is set the
/// segment + its distance label persist until a third click resets to a fresh `a`.
/// Esc clears both. Held on [`super::Ui`] so the measurement survives across frames /
/// tool switches; cleared lazily when the Measure tool isn't active.
#[derive(Clone, Copy, Default)]
pub struct MeasureState {
    /// First picked world point (set on the first click).
    pub a: Option<Vec3>,
    /// Second picked world point (set on the second click) — completes the ruler.
    pub b: Option<Vec3>,
}

impl MeasureState {
    /// Forget the current measurement (Esc, or when leaving the Measure tool).
    pub fn clear(&mut self) {
        self.a = None;
        self.b = None;
    }
}

/// Live state for the Aim tool (§2.4) — the lighting differentiator. While a drag is
/// in flight `Some(target)` holds the world point the selected heads are being aimed
/// at this frame (so the viewport can draw the target marker + aim lines). The undo
/// snapshot is taken on drag-start (via `transform_started`) and committed on release
/// (via `transform_finished`), exactly like the modal/gizmo transforms — but Aim
/// writes `pan`/`tilt` (the commanded slew targets), not position/orientation, so it
/// is NOT a [`TransformOp`]. `active()` lets the caller keep the pending undo snapshot
/// alive across the intermediate drag frames (when no `TransformOp` is in flight).
#[derive(Clone, Copy, Default)]
pub struct AimState {
    /// The world target under the cursor this frame while dragging; `None` when idle.
    target: Option<Vec3>,
}

impl AimState {
    /// Whether an aim drag is currently in flight (a snapshot is pending a commit).
    pub fn active(&self) -> bool {
        self.target.is_some()
    }
}

/// Where a viewport ray lands in the world, for the Measure tool: the nearest hit on
/// real surfaces (fixtures' bodies, screens, geometry AABBs, environment volumes),
/// falling back to the **ground plane y=0** when the ray misses everything (so you can
/// always measure floor distances). Returns the world-space hit point. Unlike `pick`
/// this wants a *point*, not a `Hit`, so it tracks the nearest `t` across all surfaces.
fn pick_world_point(scene: &Scene, ro: Vec3, rd: Vec3) -> Option<Vec3> {
    let mut best_t = f32::INFINITY;
    let mut consider = |t: f32| {
        if t > 0.0 && t < best_t {
            best_t = t;
        }
    };
    for f in &scene.fixtures {
        if !f.hidden && let Some(t) = ray_sphere(ro, rd, f.position, 0.5) {
            consider(t);
        }
    }
    for s in &scene.screens {
        if !s.hidden && let Some(t) = s.ray_hit(ro, rd) {
            consider(t);
        }
    }
    for g in &scene.geometry {
        if !g.hidden
            && let Some((lo, hi)) = g.world_bounds()
            && let Some(t) = ray_aabb(ro, rd, lo, hi)
        {
            consider(t);
        }
    }
    for e in &scene.environments {
        if let Some(t) = ray_aabb(ro, rd, e.min(), e.max()) {
            consider(t);
        }
    }
    // Ground-plane fallback: intersect the ray with y=0 when it's heading downward
    // toward (or up toward) the floor. Guards a near-parallel ray (|rd.y| tiny).
    if rd.y.abs() > 1e-4 {
        let t = -ro.y / rd.y;
        consider(t);
    }
    if best_t.is_finite() {
        Some(ro + rd * best_t)
    } else {
        None
    }
}

/// Central tab: the 3D scene, rendered offscreen and shown as a texture.
/// Drag to orbit, shift+drag to pan, scroll to zoom, click to select, `d` to
/// duplicate the selected fixture; G/R/S to move/rotate/scale the selection.
#[allow(clippy::too_many_arguments)]
pub fn viewport(
    ui: &mut egui::Ui,
    camera: &mut OrbitCamera,
    scene: &mut Scene,
    selection: &mut Selection,
    scene_anchor: &mut Option<usize>,
    viewport_focused: &mut bool,
    duplicate: &mut Option<DuplicateDialog>,
    texture: egui::TextureId,
    requested_px: &mut (u32, u32),
    fps: f32,
    prefs: &Preferences,
    // RenderSettings (Mode / Exposure / Grid / Beam-gizmo) are edited from the
    // Viewport HEADER (`ui::editor`) now, not from the viewport body (§2.2).
    transform: &mut Option<TransformOp>,
    delete_requested: &mut bool,
    replace_requested: &mut bool,
    // Modal-transform undo signals (set this frame): `started` = a G/R/S or gizmo
    // op just began (caller snapshots the `before` end); `finished` = it confirmed
    // (caller pushes the undo step). A cancel sets neither — the op already
    // restored in-place, so the caller just drops the pending `before`.
    transform_started: &mut bool,
    transform_finished: &mut bool,
    // The viewport's active tool (§2.4). Only `ActiveTool::Move` shows + handles the
    // screen-space xform gizmo; Select (and the not-yet-wired tools) keep plain
    // click-select. The spring-loaded modal G/R/S transforms stay available under
    // every tool — they OWN the viewport once started, regardless of the rail.
    active_tool: ActiveTool,
    // The Measure tool's two-point ruler state (§2.4). Persists across frames so a
    // completed measurement stays drawn; only the Measure tool reads/writes it.
    measure: &mut MeasureState,
    // The Aim tool's in-flight drag state (§2.4). Holds the world target while a drag
    // aims the selected heads; only the Aim tool reads/writes it.
    aim: &mut AimState,
) {
    *transform_started = false;
    *transform_finished = false;
    let available = ui.available_size();
    let ppp = ui.pixels_per_point();

    *requested_px = (
        (available.x * ppp).round().max(1.0) as u32,
        (available.y * ppp).round().max(1.0) as u32,
    );

    let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
    ui.painter().image(
        texture,
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    // Focus follows the most recent pointer press: inside the viewport focuses
    // it, anywhere else releases it (so the `d` shortcut only fires in here).
    if ui.input(|i| i.pointer.any_pressed())
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        *viewport_focused = rect.contains(p);
    }

    let aspect = rect.width() / rect.height().max(1.0);

    // --- Interactive transform-gizmo group (§2.4 GizmoGroup) ---
    // The ACTIVE TOOL selects which gizmo draws at the selection pivot: Move→arrows,
    // Rotate→rings, Scale→boxes (gizmo::for_tool is the single tool→group map; Select
    // and the non-transform tools return None → plain click-select). Each group
    // hit-tests its handles in screen space; grabbing one (a press within a few px)
    // starts the matching axis-locked TransformOp — reusing apply_transform via
    // `gizmo_hovered_axis`/`axis` so all three share the live-apply + undo path. The
    // grab is checked BEFORE orbit/select so dragging a handle never orbits the
    // camera; an empty-space press falls through untouched.
    let gizmo_targets: bool = active_tool.shows_xform_gizmo()
        && transform.is_none()
        && *viewport_focused
        && !selection.world
        && (!selection.fixtures.is_empty() || !selection.geometry.is_empty());
    if gizmo_targets
        && let Some(group) = gizmo::for_tool(active_tool)
    {
        // Centroid of every selected target (fixture origins + geometry bbox centres).
        let mut sum = Vec3::ZERO;
        let mut n = 0.0_f32;
        for &i in &selection.fixtures {
            if let Some(f) = scene.fixtures.get(i) {
                sum += f.position;
                n += 1.0;
            }
        }
        for &i in &selection.geometry {
            if let Some(g) = scene.geometry.get(i) {
                let c = g
                    .world_bounds()
                    .map(|(lo, hi)| (lo + hi) * 0.5)
                    .unwrap_or_else(|| g.transform.w_axis.truncate());
                sum += c;
                n += 1.0;
            }
        }
        if n > 0.0 {
            let cx = GizmoCtx {
                pivot: sum / n,
                vp: camera.view_proj(aspect),
                rect,
                // Arm/ring size scales with camera distance so handles stay a
                // readable pixel size regardless of zoom.
                arm: (camera.distance * 0.18).clamp(0.4, 4.0),
            };
            // Highlight the handle under the live pointer; on a press we hit-test the
            // press origin instead (so the grabbed handle is the one the drag began on).
            let hover_pt = ui.input(|i| i.pointer.latest_pos());
            let hover = hover_pt.and_then(|p| group.test_select(p, &cx));
            group.draw(&ui.painter_at(rect), &cx, hover);
            // A press that landed on a handle this frame starts the op.
            let press = ui.input(|i| i.pointer.press_origin());
            let grabbed: Option<Handle> = press.and_then(|p| group.test_select(p, &cx));
            if let Some(handle) = grabbed
                && response.drag_started()
                && let Some(cur) = ui.input(|i| i.pointer.latest_pos())
            {
                let start_spec = group.invoke(handle);
                let fids: Vec<usize> = selection
                    .fixtures
                    .iter()
                    .copied()
                    .filter(|&i| i < scene.fixtures.len())
                    .collect();
                let gids: Vec<usize> = selection
                    .geometry
                    .iter()
                    .copied()
                    .filter(|&i| i < scene.geometry.len())
                    .collect();
                let start: Vec<(usize, Vec3, Quat)> = fids
                    .iter()
                    .map(|&i| (i, scene.fixtures[i].position, scene.fixtures[i].orientation))
                    .collect();
                let geo_start: Vec<(usize, Mat4)> =
                    gids.iter().map(|&i| (i, scene.geometry[i].transform)).collect();
                *transform = Some(TransformOp {
                    kind: start_spec.kind,
                    // Move locks via `gizmo_hovered_axis` (matching P3a); rotate/scale
                    // carry their axis in `axis` (apply_transform reads it directly,
                    // and the uniform-scale centre yields None = scale all axes).
                    axis: if start_spec.kind == TransformKind::Move { None } else { start_spec.axis },
                    start_screen: cur,
                    pivot: cx.pivot,
                    start,
                    geo_start,
                    gizmo_hovered_axis: if start_spec.kind == TransformKind::Move {
                        start_spec.axis
                    } else {
                        None
                    },
                    from_gizmo: true,
                });
                *transform_started = true;
            }
        }
    }

    // --- Measure tool (§2.4): a read-only two-point ruler. ---
    // Click sets A (ray → nearest surface, else the y=0 ground plane); a second click
    // sets B; a third click resets to a fresh A. Esc clears. NEVER mutates the scene,
    // so there is no op / no undo. Runs BEFORE the click-select block and consumes the
    // click so measuring never also picks an object. Stale state is dropped when the
    // tool isn't active (so switching away clears the ruler).
    let mut consumed = transform.is_some();
    if active_tool == ActiveTool::Measure {
        // Esc clears the current measurement (decoded from the shared modal keymap so
        // the bind stays in the one registry, like the transform Cancel).
        if shortcuts::poll_modal(ui.ctx()).contains(&shortcuts::ModalAction::Cancel) {
            measure.clear();
        }
        if !consumed
            && response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
            let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let (ro, rd) = camera.ray(ndc, aspect);
            if let Some(p) = pick_world_point(scene, ro, rd) {
                match (measure.a, measure.b) {
                    // Fresh, or restarting after a completed measurement → new A.
                    (None, _) | (Some(_), Some(_)) => {
                        measure.a = Some(p);
                        measure.b = None;
                    }
                    // A set, B open → complete the ruler.
                    (Some(_), None) => measure.b = Some(p),
                }
            }
            consumed = true; // never fall through to click-select
        }
        // Draw the ruler: a dashed-ish polyline A→(B or live cursor) + endpoint dots
        // + a distance pill at the midpoint (metres/feet per prefs). With only A set we
        // preview to the cursor's ground/surface hit so the length reads live.
        if let Some(a) = measure.a {
            let painter = ui.painter_at(rect);
            let vp = camera.view_proj(aspect);
            // The far end: the committed B, else a live preview under the cursor.
            let live_b = measure.b.or_else(|| {
                ui.input(|i| i.pointer.latest_pos()).filter(|p| rect.contains(*p)).and_then(|pos| {
                    let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
                    let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
                    let (ro, rd) = camera.ray(ndc, aspect);
                    pick_world_point(scene, ro, rd)
                })
            });
            let sa = OrbitCamera::project_to_screen(a, vp, rect);
            let sb = live_b.and_then(|b| OrbitCamera::project_to_screen(b, vp, rect));
            let accent = egui::Color32::from_rgb(120, 220, 160);
            if let Some(sa) = sa {
                painter.circle_filled(sa, 4.0, accent);
            }
            if let (Some(sa), Some(sb)) = (sa, sb) {
                painter.line_segment([sa, sb], egui::Stroke::new(2.0, accent));
            }
            if let Some(sb) = sb {
                painter.circle_filled(sb, 4.0, accent);
            }
            // Distance label at the segment midpoint (or near A while only A is set).
            if let Some(b) = live_b {
                let metres = (b - a).length();
                let (val, unit) = prefs.len(metres);
                let mid = sa
                    .zip(sb)
                    .map(|(p, q)| p + (q - p) * 0.5)
                    .or(sa)
                    .unwrap_or_else(|| rect.center());
                theme::overlay_label(
                    &painter,
                    mid + egui::vec2(0.0, -14.0),
                    egui::Align2::CENTER_BOTTOM,
                    &format!("{val:.2}{unit}"),
                    Some(accent),
                );
            }
        }
    } else {
        // Left the Measure tool — forget any partial / completed ruler.
        measure.clear();
    }

    // --- Aim tool (§2.4): the lighting differentiator. ---
    // While the Aim tool is active and one or more fixtures are selected, a click-drag
    // in the viewport AIMS the selected heads at the world point under the cursor:
    // ray-pick the ground/geometry hit (like Measure), then for each selected fixture
    // solve the pan/tilt that points its beam axis there (`Fixture::aim_pan_tilt`, the
    // inverse of `beam_direction` — it writes the COMMANDED pan/tilt so the slew engine
    // drives the heads, cooperating with cues/motion rather than poking a quaternion).
    // Undo: one step per drag — snapshot on drag-start (`transform_started`), commit on
    // release (`transform_finished`), reusing the modal/gizmo undo pipeline verbatim.
    // Runs BEFORE the modal/orbit/select blocks and consumes the drag so aiming never
    // also orbits or click-selects. Non-fixture selections are left untouched.
    if active_tool == ActiveTool::Aim && transform.is_none() {
        // The selected, in-range fixtures we aim (geometry/screen selections ignored).
        let fids: Vec<usize> =
            selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
        if !consumed && !fids.is_empty() && *viewport_focused {
            // World target under the cursor (ground-plane fallback like Measure), used
            // both to aim and to draw the marker/lines this frame.
            let cursor_target = ui.input(|i| i.pointer.latest_pos()).filter(|p| rect.contains(*p)).and_then(
                |pos| {
                    let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
                    let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
                    let (ro, rd) = camera.ray(ndc, aspect);
                    pick_world_point(scene, ro, rd)
                },
            );
            // A press begins the aim drag → snapshot the undo `before` once.
            if response.drag_started() {
                *transform_started = true;
                aim.target = cursor_target.or(Some(Vec3::ZERO));
            }
            // While dragging, re-aim every selected head at the live target.
            if response.dragged()
                && aim.active()
                && let Some(target) = cursor_target
            {
                aim.target = Some(target);
                for &i in &fids {
                    if let Some((pan, tilt)) = scene.fixtures[i].aim_pan_tilt(target) {
                        scene.fixtures[i].pan = pan;
                        scene.fixtures[i].tilt = tilt;
                    }
                }
            }
            // Release commits the single undo step and ends the drag.
            if aim.active() && (response.drag_stopped() || !ui.input(|i| i.pointer.primary_down())) {
                *transform_finished = true;
                aim.target = None;
            }
            // The aim interaction owns any press/drag this frame (no orbit/select).
            if response.dragged() || response.drag_started() || response.clicked() {
                consumed = true;
            }
        }
        // Draw the aim viz while a drag is in flight: a small target marker at the
        // aimed point + a line from each selected head to it (so the designer sees the
        // throw). Soft amber to distinguish from the RGB gizmos and green ruler.
        if let Some(target) = aim.target {
            let painter = ui.painter_at(rect);
            let vp = camera.view_proj(aspect);
            let accent = egui::Color32::from_rgb(255, 180, 90);
            if let Some(st) = OrbitCamera::project_to_screen(target, vp, rect) {
                // A small crosshair-in-circle target marker.
                painter.circle_stroke(st, 7.0, egui::Stroke::new(2.0, accent));
                painter.line_segment([st - egui::vec2(10.0, 0.0), st + egui::vec2(10.0, 0.0)], egui::Stroke::new(1.5, accent));
                painter.line_segment([st - egui::vec2(0.0, 10.0), st + egui::vec2(0.0, 10.0)], egui::Stroke::new(1.5, accent));
                for &i in &fids {
                    if let Some(sf) = OrbitCamera::project_to_screen(scene.fixtures[i].position, vp, rect) {
                        painter.line_segment([sf, st], egui::Stroke::new(1.5, accent.gamma_multiply(0.7)));
                    }
                }
            }
        }
    } else {
        // Not the Aim tool (or a modal transform owns the viewport) — end any drag.
        aim.target = None;
    }

    // --- Modal transform (Blender G/R/S): when active it OWNS the viewport
    // (mouse drives the transform; orbit/select/zoom are suspended). ---
    if let Some(op) = transform.as_mut() {
        // The MODAL keymap owns the viewport now: X/Y/Z axis lock + Enter/Space
        // confirm + Esc cancel all decode from `poll_modal`, keeping the binds in
        // the one registry (and out of the plain press-keymaps) — no scattered raw
        // key reads here.
        let modal = shortcuts::poll_modal(ui.ctx());
        for m in &modal {
            let ax = match m {
                shortcuts::ModalAction::ConstrainX => Some(Axis::X),
                shortcuts::ModalAction::ConstrainY => Some(Axis::Y),
                shortcuts::ModalAction::ConstrainZ => Some(Axis::Z),
                _ => None,
            };
            if let Some(ax) = ax {
                op.axis = if op.axis == Some(ax) { None } else { Some(ax) };
            }
        }
        // Esc cancels; right-click cancels; pressing outside the viewport (focus
        // lost) also cancels, so a transform can never get stuck owning it.
        let cancel = modal.contains(&shortcuts::ModalAction::Cancel)
            || response.secondary_clicked()
            || !*viewport_focused;
        // A gizmo drag commits when the pointer is released; a modal G/R/S op
        // commits on a click or Enter/Space (Blender style — via poll_modal).
        let confirm = if op.from_gizmo {
            response.drag_stopped() || !ui.input(|i| i.pointer.primary_down())
        } else {
            modal.contains(&shortcuts::ModalAction::Confirm) || response.clicked()
        };
        if cancel {
            for (i, p0, q0) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    f.position = *p0;
                    f.orientation = *q0;
                }
            }
            for (i, m0) in &op.geo_start {
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = *m0;
                }
            }
            *transform = None;
        } else {
            if let Some(cur) = ui.input(|i| i.pointer.latest_pos()) {
                apply_transform(op, scene, camera, cur);
            }
            let hint = op.hint();
            theme::overlay_label(
                &ui.painter_at(rect),
                rect.center_top() + egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_TOP,
                &hint,
                Some(egui::Color32::from_rgb(255, 220, 120)),
            );
            if confirm {
                *transform = None;
                *transform_finished = true;
            }
        }
    } else if *viewport_focused && (!selection.fixtures.is_empty() || !selection.geometry.is_empty()) {
        // Start a transform on G / R / S (over fixtures and/or static geometry).
        // The binds (and their modifier guards — plain R only, since Shift+R is
        // "Replace") live in the central registry under the Viewport context.
        // Viewport context active, no transform in flight (this is the `else`
        // branch): the keymap stack resolves plain `S` to Scale (masking the Global
        // quick-select) and `R` to Rotate (Shift+R = Replace stays in Global).
        let kind = shortcuts::poll(
            ui.ctx(),
            shortcuts::ActiveContext { viewport_focused: true, transform_active: false },
        )
        .into_iter()
        .find_map(|a| match a {
            shortcuts::Action::Transform(k) => Some(k),
            _ => None,
        });
        if let Some(kind) = kind
            && let Some(cur) = ui.input(|i| i.pointer.latest_pos())
        {
            let fids: Vec<usize> =
                selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
            let gids: Vec<usize> =
                selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
            if !fids.is_empty() || !gids.is_empty() {
                // Pivot = centroid of every selected target (fixture origins +
                // geometry bbox centres).
                let mut sum = Vec3::ZERO;
                let mut n = 0.0_f32;
                let start: Vec<(usize, Vec3, Quat)> = fids
                    .iter()
                    .map(|&i| {
                        sum += scene.fixtures[i].position;
                        n += 1.0;
                        (i, scene.fixtures[i].position, scene.fixtures[i].orientation)
                    })
                    .collect();
                let geo_start: Vec<(usize, Mat4)> = gids
                    .iter()
                    .map(|&i| {
                        let g = &scene.geometry[i];
                        let c = g
                            .world_bounds()
                            .map(|(lo, hi)| (lo + hi) * 0.5)
                            .unwrap_or_else(|| g.transform.w_axis.truncate());
                        sum += c;
                        n += 1.0;
                        (i, g.transform)
                    })
                    .collect();
                let pivot = if n > 0.0 { sum / n } else { Vec3::ZERO };
                *transform = Some(TransformOp {
                    kind,
                    axis: None,
                    start_screen: cur,
                    pivot,
                    start,
                    geo_start,
                    gizmo_hovered_axis: None,
                    from_gizmo: false,
                });
                *transform_started = true;
                consumed = true;
            }
        }
    }

    if !consumed && response.dragged() {
        let delta = response.drag_delta();
        if ui.input(|i| i.modifiers.shift) {
            camera.pan(delta.x, delta.y);
        } else {
            camera.orbit(delta.x, delta.y);
        }
    }
    if !consumed && response.contains_pointer() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            camera.zoom(scroll * 0.01);
        }
    }

    // Click to select: cast a ray through the cursor and pick the nearest object.
    // ⌘/Ctrl-click toggles into a multi-selection; Shift-click range-selects from
    // the anchor (same as the outliner). A drag with Shift pans, so a stationary
    // Shift-click still range-selects.
    if !consumed
        && response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let aspect = rect.width() / rect.height().max(1.0);
        let (ro, rd) = camera.ray(ndc, aspect);
        let m = ui.input(|i| i.modifiers);
        let toggle = m.command || m.ctrl;
        match pick(scene, ro, rd) {
            Some(Hit::Fixture(i)) => apply_fixture_click(selection, scene_anchor, i, m.shift, toggle, scene.fixtures.len()),
            Some(Hit::Geometry(i)) => {
                if toggle {
                    selection.toggle_geometry(i);
                } else {
                    *selection = Selection::geometry(i);
                }
                *scene_anchor = None;
            }
            Some(Hit::Screen(i)) => {
                if toggle {
                    selection.toggle_screen(i);
                } else {
                    *selection = Selection::screen(i);
                }
                *scene_anchor = None;
            }
            Some(Hit::Environment(i)) => *selection = Selection::environment(i),
            None if !(toggle || m.shift) => *selection = Selection::default(),
            None => {}
        }
    }

    // Right-click: select the fixture under the cursor (if not already selected),
    // then open a context menu acting on the selection.
    if !consumed {
        if response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
            let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let aspect = rect.width() / rect.height().max(1.0);
            let (ro, rd) = camera.ray(ndc, aspect);
            match pick(scene, ro, rd) {
                Some(Hit::Fixture(i)) if !selection.contains_fixture(i) => {
                    *selection = Selection::fixture(i);
                    *scene_anchor = Some(i);
                }
                Some(Hit::Geometry(i)) if !selection.contains_geometry(i) => {
                    *selection = Selection::geometry(i);
                    *scene_anchor = None;
                }
                Some(Hit::Screen(i)) if !selection.contains_screen(i) => {
                    *selection = Selection::screen(i);
                    *scene_anchor = None;
                }
                _ => {}
            }
        }
        response.context_menu(|ui| {
            ui.set_min_width(170.0);
            if !selection.geometry.is_empty() {
                // Static-geometry (Objects) selection menu.
                let n = selection.geometry.len();
                ui.label(egui::RichText::new(format!("{n} object{}", if n == 1 { "" } else { "s" })).small().weak());
                if ui.button(format!("{}  Frame selection", theme::icon::FRAME)).clicked() {
                    let mut lo = Vec3::splat(f32::INFINITY);
                    let mut hi = Vec3::splat(f32::NEG_INFINITY);
                    for &i in &selection.geometry {
                        if let Some((l, h)) = scene.geometry.get(i).and_then(|g| g.world_bounds()) {
                            lo = lo.min(l);
                            hi = hi.max(h);
                        }
                    }
                    if lo.is_finite() {
                        camera.frame_aabb(lo, hi);
                    }
                    ui.close();
                }
                if ui.button(format!("{}  Hide", theme::icon::EYE_OFF)).clicked() {
                    for &i in &selection.geometry {
                        if let Some(g) = scene.geometry.get_mut(i) {
                            g.hidden = true;
                        }
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Deselect").clicked() {
                    *selection = Selection::default();
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new(format!("{}  Delete", theme::icon::TRASH)).color(theme::CONFLICT))
                    .clicked()
                {
                    *delete_requested = true;
                    ui.close();
                }
            } else if !selection.screens.is_empty() {
                // LED-screen selection menu.
                let n = selection.screens.len();
                ui.label(egui::RichText::new(format!("{n} screen{}", if n == 1 { "" } else { "s" })).small().weak());
                if ui.button(format!("{}  Frame selection", theme::icon::FRAME)).clicked() {
                    let mut lo = Vec3::splat(f32::INFINITY);
                    let mut hi = Vec3::splat(f32::NEG_INFINITY);
                    for &i in &selection.screens {
                        if let Some(s) = scene.screens.get(i) {
                            let (l, h) = s.world_bounds();
                            lo = lo.min(l);
                            hi = hi.max(h);
                        }
                    }
                    if lo.is_finite() {
                        camera.frame_aabb(lo, hi);
                    }
                    ui.close();
                }
                if ui.button(format!("{}  Hide", theme::icon::EYE_OFF)).clicked() {
                    for &i in &selection.screens {
                        if let Some(s) = scene.screens.get_mut(i) {
                            s.hidden = true;
                        }
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Deselect").clicked() {
                    *selection = Selection::default();
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new(format!("{}  Delete", theme::icon::TRASH)).color(theme::CONFLICT))
                    .clicked()
                {
                    *delete_requested = true;
                    ui.close();
                }
            } else if selection.fixtures.is_empty() {
                if ui.button(format!("{}  Select all", theme::icon::FIXTURE)).clicked() {
                    selection.fixtures = (0..scene.fixtures.len()).collect();
                    selection.environment = None;
                    selection.geometry.clear();
                    ui.close();
                }
            } else {
                ui.label(
                    egui::RichText::new(format!("{} selected", selection.fixtures.len())).small().weak(),
                );
                if ui.button("Select same type").clicked() {
                    select_same_type(scene, selection);
                    ui.close();
                }
                if ui.button(format!("{}  Frame selection", theme::icon::FRAME)).clicked() {
                    frame_selection(scene, selection, camera);
                    ui.close();
                }
                if duplicate.is_none() && ui.button(format!("{}  Duplicate / Array…", theme::icon::DUPLICATE)).clicked() {
                    if let Some(idx) = selection.primary_fixture() {
                        *duplicate =
                            Some(DuplicateDialog { fixture: idx, x: 0.0, y: 0.0, z: 0.0, y_angle: 36.0, count: 9 });
                    }
                    ui.close();
                }
                if ui
                    .button(format!("{}  Replace…", theme::icon::RESET))
                    .on_hover_text("Swap these fixtures for another project profile (Shift+R)")
                    .clicked()
                {
                    *replace_requested = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Deselect").clicked() {
                    *selection = Selection::default();
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new(format!("{}  Delete", theme::icon::TRASH)).color(theme::CONFLICT))
                    .clicked()
                {
                    // Committed after the dock so the patch/groups/cues remap too.
                    *delete_requested = true;
                    ui.close();
                }
            }
        });
    }

    // `d` (or Shift+D) opens the Duplicate dialog for the selected fixture. The
    // `!consumed && *viewport_focused` guard below already rules out a live/just-
    // started transform, so the poll asks the Viewport keymap with no modal active.
    let dup_pressed = shortcuts::poll(
        ui.ctx(),
        shortcuts::ActiveContext { viewport_focused: *viewport_focused, transform_active: false },
    )
    .iter()
    .any(|a| matches!(a, shortcuts::Action::Duplicate));
    if !consumed
        && *viewport_focused
        && duplicate.is_none()
        && dup_pressed
        && let Some(idx) = selection.primary_fixture()
    {
        *duplicate = Some(DuplicateDialog {
            fixture: idx,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            y_angle: 36.0,
            count: 9,
        });
    }

    // Focus border.
    if *viewport_focused {
        ui.painter().rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(90, 170, 255)),
            egui::StrokeKind::Inside,
        );
    }

    theme::overlay_label(
        &ui.painter_at(rect),
        rect.left_bottom() + egui::vec2(8.0, -8.0),
        egui::Align2::LEFT_BOTTOM,
        if active_tool == ActiveTool::Measure {
            "measure: click two points for distance · esc: clear · scroll: zoom · shift+drag: pan"
        } else if active_tool == ActiveTool::Aim {
            "aim: drag to point selected head(s) at the cursor · scroll: zoom · shift+drag: pan"
        } else {
            "drag: orbit · shift+drag: pan · scroll: zoom · click: select · g/r/s: move/rotate/scale · d: duplicate"
        },
        None,
    );

    // Active selection label (top-right corner, like Blender's active-object
    // header): the primary selected object's name + how many more are selected.
    let sel_text: Option<String> = if let Some(ei) = selection.environment {
        scene.environments.get(ei).map(|e| e.name.clone())
    } else if !selection.geometry.is_empty() {
        let extra = selection.geometry.len().saturating_sub(1);
        selection.primary_geometry().and_then(|i| scene.geometry.get(i)).map(|g| {
            if extra > 0 { format!("{}  +{extra}", g.name) } else { g.name.clone() }
        })
    } else if !selection.screens.is_empty() {
        let extra = selection.screens.len().saturating_sub(1);
        selection.primary_screen().and_then(|i| scene.screens.get(i)).map(|s| {
            if extra > 0 { format!("{}  +{extra}", s.name) } else { s.name.clone() }
        })
    } else if !selection.fixtures.is_empty() {
        let extra = selection.fixtures.len().saturating_sub(1);
        selection.primary_fixture().and_then(|i| scene.fixtures.get(i)).map(|f| {
            if extra > 0 { format!("{}  +{extra}", f.name) } else { f.name.clone() }
        })
    } else {
        None
    };
    if let Some(text) = sel_text {
        let painter = ui.painter_at(rect);
        theme::overlay_label(
            &painter,
            rect.right_top() + egui::vec2(-10.0, 10.0),
            egui::Align2::RIGHT_TOP,
            &text,
            None,
        );
    }

    // Fixture labels, projected to screen (name / ID / DMX address).
    if prefs.show_labels {
        let aspect = (rect.width() / rect.height().max(1.0)).max(0.0001);
        let vp = camera.view_proj(aspect);
        let painter = ui.painter_at(rect);
        for (i, f) in scene.fixtures.iter().enumerate() {
            let selected = selection.contains_fixture(i);
            if prefs.labels_selected_only && !selected {
                continue;
            }
            // Label just above the fixture body.
            let world = f.position + Vec3::new(0.0, 0.35, 0.0);
            let clip = vp * world.extend(1.0);
            if clip.w <= 0.0 {
                continue; // behind camera
            }
            let ndc = clip.truncate() / clip.w;
            if ndc.x < -1.2 || ndc.x > 1.2 || ndc.y < -1.2 || ndc.y > 1.2 {
                continue;
            }
            let sx = rect.min.x + (ndc.x * 0.5 + 0.5) * rect.width();
            let sy = rect.min.y + (0.5 - ndc.y * 0.5) * rect.height();
            let text = match prefs.label_mode {
                LabelMode::Name => f.name.clone(),
                LabelMode::FixtureId => f
                    .mvr
                    .as_deref()
                    .map(|m| m.fixture_id.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| f.name.clone()),
                LabelMode::Address => f
                    .mvr
                    .as_deref()
                    .and_then(|m| m.addresses.first())
                    .map(|a| format!("{}.{:03}", a.universe(), a.channel()))
                    .unwrap_or_else(|| "—".into()),
            };
            let col = if selected {
                egui::Color32::from_rgb(120, 200, 255)
            } else {
                egui::Color32::from_white_alpha(180)
            };
            painter.text(
                egui::pos2(sx, sy),
                egui::Align2::CENTER_BOTTOM,
                text,
                egui::FontId::proportional(11.0),
                col,
            );
        }
    }

    // FPS HUD (top-left), color-coded.
    if prefs.show_fps {
        let color = if fps >= 55.0 {
            egui::Color32::from_rgb(120, 230, 120)
        } else if fps >= 30.0 {
            egui::Color32::from_rgb(235, 215, 110)
        } else {
            egui::Color32::from_rgb(235, 120, 110)
        };
        ui.painter().text(
            rect.left_top() + egui::vec2(8.0, 6.0),
            egui::Align2::LEFT_TOP,
            format!("{fps:.0} fps"),
            egui::FontId::monospace(13.0),
            color,
        );
    }

    // The display Mode + Exposure controls (and the Grid / Beam-gizmo toggles)
    // now live in the per-editor Viewport HEADER (`ui::editor`), migrated off the
    // old floating "viewport-display-overlay" Area (§2.2). Advanced look settings
    // (bloom/beam/steps) stay in Preferences.
}

/// Bottom tab: Art-Net / sACN connectivity settings + live source status.
pub fn connectivity(
    ui: &mut egui::Ui,
    config: &mut DmxConfig,
    status: &DmxStatus,
    bind_ip_text: &mut String,
    universes_text: &mut String,
    pending: &mut PendingNetCmd,
    running: bool,
) {
    ui.horizontal(|ui| {
        ui.heading("Connectivity");
        let mut enabled = running;
        if ui
            .checkbox(&mut enabled, "Receive DMX")
            .on_hover_text("Bind the sockets and decode live DMX into the rig (input only)")
            .changed()
        {
            *pending = if enabled { PendingNetCmd::Start } else { PendingNetCmd::Stop };
        }
        if running {
            let bound = match (status.bound_artnet, status.bound_sacn) {
                (true, true) => "Art-Net + sACN",
                (true, false) => "Art-Net",
                (false, true) => "sACN",
                (false, false) => "no sockets bound",
            };
            ui.colored_label(
                Color32::from_rgb(120, 210, 120),
                format!("● {bound} · {} source(s)", status.sources.len()),
            );
        } else {
            ui.colored_label(Color32::from_gray(140), "○ stopped");
        }
    });
    ui.separator();

    Grid::new("dmx-connect")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Protocols");
            ui.horizontal(|ui| {
                ui.checkbox(&mut config.artnet, "Art-Net");
                ui.checkbox(&mut config.sacn, "sACN");
            });
            ui.end_row();

            ui.label("Bind interface");
            let valid = bind_ip_text.parse::<std::net::IpAddr>();
            let resp = ui.add(
                egui::TextEdit::singleline(bind_ip_text)
                    .desired_width(150.0)
                    .hint_text("0.0.0.0")
                    .text_color_opt(valid.is_err().then_some(Color32::from_rgb(230, 120, 110))),
            );
            if resp.changed()
                && let Ok(ip) = bind_ip_text.parse::<std::net::IpAddr>()
            {
                config.bind_ip = ip;
            }
            ui.end_row();

            ui.label("sACN universes");
            let resp = ui
                .add(
                    egui::TextEdit::singleline(universes_text)
                        .desired_width(150.0)
                        .hint_text("1,2,5-8"),
                )
                .on_hover_text(
                    "sACN multicast groups to join. Art-Net is broadcast — all \
                     universes are received regardless of this list.",
                );
            if resp.changed() {
                config.universes = crate::dmx::parse_universe_list(universes_text);
            }
            ui.end_row();

            ui.label("Merge");
            ui.horizontal(|ui| {
                for m in MergePolicy::ALL {
                    ui.selectable_value(&mut config.merge, m, m.label());
                }
            });
            ui.end_row();

            ui.label("Art-Net priority");
            ui.add(DragValue::new(&mut config.artnet_priority).range(0..=200));
            ui.end_row();
        });

    ui.horizontal(|ui| {
        if ui
            .add_enabled(running, egui::Button::new("Reapply"))
            .on_hover_text("Re-bind sockets / re-join multicast after a protocol or interface change")
            .clicked()
        {
            *pending = PendingNetCmd::Reapply;
        }
        ui.label(
            RichText::new("Universe/merge edits apply live; protocol/interface need Reapply.")
                .weak()
                .small(),
        );
    });

    ui.add_space(6.0);
    ui.label(RichText::new("SOURCES").small().strong());
    if status.sources.is_empty() {
        let msg = if running {
            "listening — no sources seen yet"
        } else {
            "not receiving"
        };
        ui.label(RichText::new(msg).weak().small());
        return;
    }
    egui::ScrollArea::vertical().max_height(170.0).show(ui, |ui| {
        Grid::new("dmx-sources")
            .num_columns(7)
            .striped(true)
            .spacing([12.0, 3.0])
            .show(ui, |ui| {
                for h in ["Proto", "Source", "Universes", "Prio", "FPS", "Lost", "Seen"] {
                    ui.strong(RichText::new(h).small());
                }
                ui.end_row();
                for s in &status.sources {
                    ui.label(RichText::new(s.proto.label()).small());
                    let name = if s.name.is_empty() {
                        s.label.clone()
                    } else {
                        format!("{} ({})", s.name, s.label)
                    };
                    ui.label(RichText::new(name).small());
                    ui.label(RichText::new(format_universes(&s.universes)).small());
                    ui.label(RichText::new(s.priority.to_string()).small());
                    let fps_col = if s.fps >= 30.0 {
                        Color32::from_rgb(120, 210, 120)
                    } else if s.fps >= 10.0 {
                        Color32::from_rgb(230, 210, 110)
                    } else {
                        Color32::from_rgb(230, 120, 110)
                    };
                    ui.colored_label(fps_col, RichText::new(format!("{:.0}", s.fps)).small());
                    ui.label(RichText::new(s.seq_errors.to_string()).small());
                    ui.label(RichText::new(format!("{:.1}s", s.age().as_secs_f32())).small());
                    ui.end_row();
                }
            });
    });
}

/// Bottom tab: the live 512-channel universe grid with patch occupants (replaces
/// the old DMX Monitor stub). Each cell shows the channel number, its live level,
/// and the patched fixture + attribute occupying it.
#[allow(deprecated)] // egui 0.34 show_tooltip_at_pointer — migrated project-wide later
pub fn dmx_universe_grid(
    ui: &mut egui::Ui,
    scene: &Scene,
    patch: &PatchTable,
    snapshot: &UniverseSnapshot,
    selected_universe: &mut u16,
    selection: &mut Selection,
) {
    // Universes present in the snapshot or referenced by the patch.
    let mut universes = patch.universes();
    for &u in snapshot.frames.keys() {
        if !universes.contains(&u) {
            universes.push(u);
        }
    }
    universes.sort_unstable();
    universes.dedup();
    if universes.is_empty() {
        universes.push(*selected_universe);
    }
    if !universes.contains(selected_universe) {
        *selected_universe = universes[0];
    }

    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;
    let u = *selected_universe;
    let live = snapshot.is_live(u, DMX_STALE);
    let nconf = patch.conflicts().len();

    // --- header: title · universe nav · live / conflict status ---
    ui.horizontal(|ui| {
        ui.label(RichText::new(theme::icon::DMX).size(16.0).color(ink.secondary));
        ui.heading("DMX Universe");
        ui.add_space(6.0);
        if ui.button(theme::ico(theme::icon::PREV)).clicked()
            && let Some(pos) = universes.iter().position(|x| x == selected_universe)
        {
            *selected_universe = universes[pos.saturating_sub(1)];
        }
        egui::ComboBox::from_id_salt("dmx-universe-select")
            .selected_text(format!("Universe {selected_universe}"))
            .show_ui(ui, |ui| {
                for &x in &universes {
                    ui.selectable_value(selected_universe, x, format!("Universe {x}"));
                }
            });
        if ui.button(theme::ico(theme::icon::NEXT)).clicked()
            && let Some(pos) = universes.iter().position(|x| x == selected_universe)
        {
            *selected_universe = universes[(pos + 1).min(universes.len() - 1)];
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if nconf > 0 {
                ui.colored_label(
                    theme::CONFLICT,
                    RichText::new(format!("{} {nconf} conflict{}", theme::icon::WARNING, if nconf == 1 { "" } else { "s" })),
                );
            }
            if live {
                let n = snapshot.frames.get(&u).map(|f| f.sources).unwrap_or(0);
                ui.colored_label(theme::OK, format!("● {n} src"));
            } else {
                ui.colored_label(ink.muted, "○ idle");
            }
        });
    });

    // Per-channel occupant (fixture index + attribute) for the selected universe,
    // computed once so each of the 512 cells is a cheap lookup.
    let mut occ: Vec<Option<(usize, String)>> = vec![None; 512];
    let mut conflict_cells = [false; 512];
    for (i, fixture) in scene.fixtures.iter().enumerate() {
        let Some(p) = patch.get(i).filter(|p| p.enabled) else { continue };
        if p.universe != u {
            continue;
        }
        for mc in channel_map(fixture, p.mode_index).channels {
            for k in 0..mc.width as u16 {
                let ch = p.address.saturating_sub(1) + mc.offset + k;
                if let Some(slot) = occ.get_mut(ch as usize) {
                    if slot.is_none() {
                        *slot = Some((i, mc.attribute.clone()));
                    } else {
                        conflict_cells[ch as usize] = true;
                    }
                }
            }
        }
    }
    let active = (1..=512u16).filter(|&c| snapshot.level(u, c).unwrap_or(0) > 0).count();
    let patched = occ.iter().filter(|o| o.is_some()).count();

    // --- summary strip ---
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{active}")).monospace().strong().color(if active > 0 { accent } else { ink.muted }));
        ui.label(RichText::new("active").small().color(ink.tertiary));
        ui.add_space(8.0);
        ui.label(RichText::new(format!("{patched}")).monospace().strong().color(ink.secondary));
        ui.label(RichText::new("patched / 512").small().color(ink.tertiary));
    });
    ui.separator();

    let base_patched = ui.visuals().widgets.inactive.bg_fill;
    let base_empty = ui.visuals().extreme_bg_color;
    let border = ui.visuals().widgets.noninteractive.bg_stroke.color;

    const COLS: usize = 16;
    const ROWS: usize = 32;
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let avail = ui.available_width().max(360.0);
        let cell_w = (avail / COLS as f32).clamp(40.0, 96.0);
        let cell_h = 30.0;
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(cell_w * COLS as f32, cell_h * ROWS as f32),
            Sense::click(),
        );
        let painter = ui.painter_at(rect);
        for r in 0..ROWS {
            for c in 0..COLS {
                let ch = r * COLS + c; // 0-based channel index
                let cell = egui::Rect::from_min_size(
                    rect.min + egui::vec2(c as f32 * cell_w, r as f32 * cell_h),
                    egui::vec2(cell_w - 1.0, cell_h - 1.0),
                );
                let level = snapshot.level(u, (ch + 1) as u16).unwrap_or(0);
                let occupied = occ[ch].as_ref();
                let selected = occupied.is_some_and(|(fi, _)| selection.contains_fixture(*fi));
                let tint = occupied.map(|(fi, _)| fixture_tint(*fi)).unwrap_or(accent);

                // Base + a value-fill bar rising from the bottom (∝ level).
                painter.rect_filled(cell, 3.0, if occupied.is_some() { base_patched } else { base_empty });
                if level > 0 {
                    let frac = level as f32 / 255.0;
                    let fill = egui::Rect::from_min_max(
                        egui::pos2(cell.left(), cell.bottom() - cell.height() * frac),
                        cell.right_bottom(),
                    );
                    painter.rect_filled(fill, 0.0, tint.gamma_multiply(0.22 + 0.55 * frac));
                }
                // Fixture-identity stripe down the left edge.
                if occupied.is_some() {
                    painter.rect_filled(
                        egui::Rect::from_min_max(cell.left_top(), egui::pos2(cell.left() + 2.5, cell.bottom())),
                        0.0,
                        tint,
                    );
                }
                // Border / conflict / selection ring.
                painter.rect_stroke(cell, 3.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
                if conflict_cells[ch] {
                    painter.rect_stroke(cell, 3.0, egui::Stroke::new(1.5, theme::CONFLICT), egui::StrokeKind::Inside);
                } else if selected {
                    painter.rect_stroke(cell, 3.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
                }
                // Channel number (top-left) + value % (bottom-right), tabular.
                painter.text(
                    cell.left_top() + egui::vec2(4.0, 2.0),
                    egui::Align2::LEFT_TOP,
                    (ch + 1).to_string(),
                    egui::FontId::monospace(9.0),
                    ink.muted,
                );
                if level > 0 {
                    let pct = (level as f32 / 255.0 * 100.0).round() as u32;
                    painter.text(
                        cell.right_bottom() + egui::vec2(-4.0, -2.0),
                        egui::Align2::RIGHT_BOTTOM,
                        format!("{pct}"),
                        egui::FontId::monospace(11.5),
                        ink.primary,
                    );
                }
            }
        }
        // Hover tooltip with the channel's full occupant + value.
        if let Some(pos) = resp.hover_pos() {
            let rel = pos - rect.min;
            let (c, r) = ((rel.x / cell_w) as usize, (rel.y / cell_h) as usize);
            if c < COLS && r < ROWS {
                let ch = r * COLS + c;
                let level = snapshot.level(u, (ch + 1) as u16).unwrap_or(0);
                let pct = (level as f32 / 255.0 * 100.0).round() as u32;
                let detail = match &occ[ch] {
                    Some((fi, attr)) => {
                        let name = scene.fixtures[*fi].name.clone();
                        format!("Ch {} · {name} · {attr}\n{level}  ({pct}%)", ch + 1)
                    }
                    None => format!("Ch {} · unpatched\n{level}  ({pct}%)", ch + 1),
                };
                egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new("dmx-cell-tip"), |ui| {
                    ui.label(detail);
                });
            }
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let rel = pos - rect.min;
            let (c, r) = ((rel.x / cell_w) as usize, (rel.y / cell_h) as usize);
            // Select from the same occupancy map the grid is painted/hovered from
            // (so a click agrees with the cell's shown identity, including gaps).
            if c < COLS && r < ROWS
                && let Some((fi, _)) = &occ[r * COLS + c]
            {
                *selection = Selection::fixture(*fi);
            }
        }
    });
}

/// A stable identity colour for a fixture index — golden-ratio hue spacing so
/// adjacent fixtures stay visually distinct in the DMX grid.
fn fixture_tint(i: usize) -> Color32 {
    let h = (i as f32 * 0.618_034).fract();
    hsv_to_color(h, 0.55, 0.95)
}

fn hsv_to_color(h: f32, s: f32, v: f32) -> Color32 {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Fixture-manager panel state: text filter, sort, and quick filters. Sort reuses
/// the scene outliner's [`SceneSort`] (Patch / Name / Type).
pub struct FmState {
    pub search: String,
    pub sort: SceneSort,
    pub conflicts_only: bool,
    pub unpatched_only: bool,
    pub bulk_universe: u16,
    pub bulk_address: u16,
}

impl Default for FmState {
    fn default() -> Self {
        Self {
            search: String::new(),
            sort: SceneSort::Patch,
            conflicts_only: false,
            unpatched_only: false,
            bulk_universe: 1,
            bulk_address: 1,
        }
    }
}

/// Bottom tab: the **Fixture Manager** — a data-dense, sortable, filterable table
/// of every fixture with multi-select (synced to the 3D/Inspector selection) and
/// bulk patch editing. Replaces the old one-row-at-a-time patch editor.
pub fn fixture_manager(
    ui: &mut egui::Ui,
    scene: &Scene,
    patch: &mut PatchTable,
    selection: &mut Selection,
    anchor: &mut Option<usize>,
    fm: &mut FmState,
) {
    use theme::icon;
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;

    let mut conflicted = vec![false; scene.fixtures.len()];
    for c in patch.conflicts() {
        if let Some(s) = conflicted.get_mut(c.a) {
            *s = true;
        }
        if let Some(s) = conflicted.get_mut(c.b) {
            *s = true;
        }
    }
    let nconf = conflicted.iter().filter(|&&c| c).count();

    // --- header: title + selection count + reset ---
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  Fixtures", icon::FIXTURE)).heading());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(theme::ico(icon::RESET)).on_hover_text("Reset addresses to the import (MVR/GDTF), discarding manual edits").clicked() {
                patch.reconcile_from_scene(scene);
            }
            let nsel = selection.fixtures.len();
            if nsel > 0 {
                ui.label(RichText::new(format!("{nsel} selected")).small().color(accent));
            }
        });
    });

    // --- filter row: search + sort + quick filters ---
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::ico(icon::SEARCH).weak());
        ui.add(egui::TextEdit::singleline(&mut fm.search).hint_text("Filter…").desired_width(120.0));
        ui.separator();
        ui.label(theme::ico(icon::SORT).weak());
        for s in [SceneSort::Patch, SceneSort::Name, SceneSort::Type] {
            ui.selectable_value(&mut fm.sort, s, s.label());
        }
        ui.separator();
        ui.toggle_value(&mut fm.conflicts_only, format!("{} {nconf}", icon::WARNING))
            .on_hover_text("Show only fixtures with an address conflict");
        ui.toggle_value(&mut fm.unpatched_only, "unpatched").on_hover_text("Show only unpatched fixtures");
    });

    // --- bulk toolbar (only when fixtures are selected) ---
    let sel: Vec<usize> = selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
    if !sel.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("Bulk · {}", sel.len())).small().strong().color(accent));
            ui.add(DragValue::new(&mut fm.bulk_universe).range(1..=63999).prefix("U "));
            ui.add(DragValue::new(&mut fm.bulk_address).range(1..=512).prefix("@ "));
            if ui.button("Patch seq").on_hover_text("Assign the selected fixtures sequentially from U.@ by footprint, in the order shown").clicked() {
                // Assign in the VISIBLE (sorted) order, not raw selection order, so
                // the sequence matches what the user sees.
                let seq: Vec<usize> =
                    fixture_order(scene, patch, fm.sort).into_iter().filter(|i| sel.contains(i)).collect();
                let (mut u, mut a) = (fm.bulk_universe.max(1), fm.bulk_address.clamp(1, 512));
                for &i in &seq {
                    let fp = patch.get(i).map(|p| p.footprint).unwrap_or(1).clamp(1, 512);
                    if a as u32 + fp as u32 - 1 > 512 {
                        u = (u + 1).min(63999); // next universe (clamped, no u16 wrap)
                        a = 1;
                    }
                    if let Some(p) = patch.get_mut(i) {
                        p.universe = u;
                        p.address = a;
                        p.enabled = true;
                        p.source = PatchSource::Manual;
                    }
                    a += fp;
                }
            }
            if ui.button("Set U").on_hover_text("Set the universe of all selected").clicked() {
                for &i in &sel {
                    if let Some(p) = patch.get_mut(i) {
                        p.universe = fm.bulk_universe;
                        p.enabled = true;
                        p.source = PatchSource::Manual;
                    }
                }
            }
            if ui.button("Enable").clicked() {
                for &i in &sel {
                    if let Some(p) = patch.get_mut(i) {
                        p.enabled = true;
                    }
                }
            }
            if ui.button("Disable").clicked() {
                for &i in &sel {
                    if let Some(p) = patch.get_mut(i) {
                        p.enabled = false;
                    }
                }
            }
            // Bulk DMX mode — only when every selected fixture is the same profile
            // (so one mode list applies to all). Drives the patch footprint; decode
            // syncs each fixture's active mode from the patch.
            let p0 = scene.fixtures[sel[0]].profile.clone();
            let same_profile = sel.iter().all(|&i| scene.fixtures[i].profile == p0);
            let ref_modes: Vec<String> = if same_profile {
                scene.fixtures[sel[0]]
                    .gdtf
                    .as_ref()
                    .map(|g| g.modes.iter().map(|m| m.name.clone()).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if !ref_modes.is_empty() {
                let cur = patch.get(sel[0]).map(|p| p.mode_index).unwrap_or(0);
                let cur_name = ref_modes.get(cur).cloned().unwrap_or_default();
                let mut pick = None;
                egui::ComboBox::from_id_salt("fm-bulk-mode")
                    .selected_text(RichText::new(format!("Mode: {cur_name}")).small())
                    .show_ui(ui, |ui| {
                        for (mi, name) in ref_modes.iter().enumerate() {
                            if ui.selectable_label(mi == cur, name).clicked() {
                                pick = Some(mi);
                            }
                        }
                    });
                if let Some(mi) = pick {
                    for &i in &sel {
                        let f = &scene.fixtures[i];
                        if f.gdtf.as_ref().is_some_and(|g| mi < g.modes.len()) {
                            patch.set_mode(f, i, mi);
                        }
                    }
                }
            }
        });
    }
    ui.separator();

    if scene.fixtures.is_empty() {
        ui.label(RichText::new("No fixtures — add from the Library or import an MVR.").weak().small());
        return;
    }

    // --- display order: sort then filter ---
    let q = fm.search.trim().to_lowercase();
    let order: Vec<usize> = fixture_order(scene, patch, fm.sort)
        .into_iter()
        .filter(|&i| {
            let f = &scene.fixtures[i];
            if !q.is_empty() && !f.name.to_lowercase().contains(&q) && !f.profile.to_lowercase().contains(&q) {
                return false;
            }
            if fm.conflicts_only && !conflicted[i] {
                return false;
            }
            if fm.unpatched_only && patch.get(i).is_some_and(|p| p.enabled) {
                return false;
            }
            true
        })
        .collect();

    let mut click: Option<(usize, bool, bool)> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        Grid::new("fixtures-grid").num_columns(7).striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
            for h in ["Fixture", "Type", "Univ", "Addr", "Mode", "Ch", ""] {
                ui.strong(RichText::new(h).small().color(ink.tertiary));
            }
            ui.end_row();

            for &i in &order {
                let fixture = &scene.fixtures[i];
                // Name cell: selects the row (syncs the 3D/Inspector selection);
                // shift = range, ⌘/Ctrl = toggle.
                let selected = selection.contains_fixture(i);
                let resp = ui.selectable_label(selected, RichText::new(fixture.name.as_str()).small());
                if resp.clicked() {
                    let m = ui.input(|x| x.modifiers);
                    click = Some((i, m.shift, m.command || m.ctrl));
                }
                ui.label(RichText::new(fixture.profile.as_str()).weak().small());

                // Universe / address.
                if let Some(p) = patch.get_mut(i) {
                    let mut edited = ui.add(DragValue::new(&mut p.universe).range(1..=63999).speed(0.1)).changed();
                    edited |= ui.add(DragValue::new(&mut p.address).range(1..=512).speed(0.2)).changed();
                    if edited {
                        p.enabled = true;
                        p.source = PatchSource::Manual;
                    }
                } else {
                    ui.label("");
                    ui.label("");
                }

                // Mode selector (GDTF modes; plain fixtures are synthetic).
                let mut new_mode = None;
                match fixture.gdtf.as_ref() {
                    Some(gdtf) if !gdtf.modes.is_empty() => {
                        let cur = patch.get(i).map(|p| p.mode_index).unwrap_or(0);
                        let cur_name = gdtf.modes.get(cur).map(|m| m.name.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt(("fm-mode", i))
                            .selected_text(RichText::new(cur_name).small())
                            .show_ui(ui, |ui| {
                                for (mi, m) in gdtf.modes.iter().enumerate() {
                                    if ui.selectable_label(mi == cur, &m.name).clicked() {
                                        new_mode = Some(mi);
                                    }
                                }
                            });
                    }
                    _ => {
                        ui.label(RichText::new("—").weak().small());
                    }
                }
                if let Some(mi) = new_mode {
                    patch.set_mode(fixture, i, mi);
                }

                ui.label(RichText::new(patch.get(i).map(|p| p.footprint.to_string()).unwrap_or_default()).small());
                if conflicted[i] {
                    ui.colored_label(theme::CONFLICT, theme::icon::WARNING).on_hover_text("Address conflict");
                } else if patch.get(i).is_some_and(|p| !p.enabled) {
                    ui.label(RichText::new("off").weak().small());
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
    });
    if let Some((i, shift, toggle)) = click {
        if shift {
            // Re-anchor if the shared anchor is stale (deleted, or filtered out of
            // the visible list) so the range can't span to a phantom row.
            if anchor.map_or(true, |a| !order.contains(&a)) {
                *anchor = Some(i);
            }
            let cpos = order.iter().position(|&x| x == i).unwrap_or(0);
            let apos = order.iter().position(|&x| Some(x) == *anchor).unwrap_or(cpos);
            let (lo, hi) = (apos.min(cpos), apos.max(cpos));
            selection.fixtures = order[lo..=hi].to_vec();
            selection.environment = None;
        } else {
            apply_fixture_click(selection, anchor, i, false, toggle, scene.fixtures.len());
        }
    }
}

/// Compact a sorted universe list for the source table (e.g. `1,2,5`).
fn format_universes(us: &[u16]) -> String {
    us.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",")
}

/// What a viewport ray hit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Fixture(usize),
    Screen(usize),
    Geometry(usize),
    Environment(usize),
}

/// Shortest distance from a screen-space point `p` to the segment `a`..`b`. Used by
/// the gizmo groups to hit-test their projected axis handles / rings.
pub(super) fn dist_point_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 <= f32::EPSILON {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    let proj = a + ab * t;
    (p - proj).length()
}

/// Pick the object a world-space ray hits. Priority: **fixtures** (so you can
/// always click a head even when it sits inside set geometry or the fog box),
/// then **static geometry** (its world AABB), then the **environment** volumes.
fn pick(scene: &Scene, ro: Vec3, rd: Vec3) -> Option<Hit> {
    let mut best: Option<(f32, usize)> = None;
    for (i, f) in scene.fixtures.iter().enumerate() {
        if f.hidden {
            continue;
        }
        // Bounding sphere around the head; a bit generous so it's easy to click.
        if let Some(t) = ray_sphere(ro, rd, f.position, 0.5)
            && best.is_none_or(|(bt, _)| t < bt)
        {
            best = Some((t, i));
        }
    }
    if let Some((_, i)) = best {
        return Some(Hit::Fixture(i));
    }
    // LED screens: ray vs each oriented surface quad (cheaper + tighter than an AABB).
    let mut scr: Option<(f32, usize)> = None;
    for (i, s) in scene.screens.iter().enumerate() {
        if s.hidden {
            continue;
        }
        if let Some(t) = s.ray_hit(ro, rd)
            && scr.is_none_or(|(bt, _)| t < bt)
        {
            scr = Some((t, i));
        }
    }
    if let Some((_, i)) = scr {
        return Some(Hit::Screen(i));
    }
    // Static geometry: ray vs each object's world-space AABB.
    let mut geo: Option<(f32, usize)> = None;
    for (i, g) in scene.geometry.iter().enumerate() {
        if g.hidden {
            continue;
        }
        if let Some((lo, hi)) = g.world_bounds()
            && let Some(t) = ray_aabb(ro, rd, lo, hi)
            && geo.is_none_or(|(bt, _)| t < bt)
        {
            geo = Some((t, i));
        }
    }
    if let Some((_, i)) = geo {
        return Some(Hit::Geometry(i));
    }
    let mut env: Option<(f32, usize)> = None;
    for (i, e) in scene.environments.iter().enumerate() {
        if let Some(t) = ray_aabb(ro, rd, e.min(), e.max())
            && env.is_none_or(|(bt, _)| t < bt)
        {
            env = Some((t, i));
        }
    }
    env.map(|(_, i)| Hit::Environment(i))
}

/// Nearest positive ray–sphere intersection distance, if any.
fn ray_sphere(ro: Vec3, rd: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let oc = ro - center;
    let b = oc.dot(rd);
    let c = oc.dot(oc) - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let t = -b - s;
    if t > 0.0 {
        Some(t)
    } else {
        let t2 = -b + s;
        (t2 > 0.0).then_some(t2)
    }
}

/// Nearest positive ray–AABB intersection distance (slab test), if any.
fn ray_aabb(ro: Vec3, rd: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = rd.recip(); // inf for parallel components is fine
    let t0 = (min - ro) * inv;
    let t1 = (max - ro) * inv;
    let tmin = t0.min(t1);
    let tmax = t0.max(t1);
    let near = tmin.x.max(tmin.y).max(tmin.z);
    let far = tmax.x.min(tmax.y).min(tmax.z);
    if far < near.max(0.0) {
        return None;
    }
    Some(if near > 0.0 { near } else { far })
}


#[cfg(test)]
mod pick_tests {
    use super::*;

    #[test]
    fn ray_sphere_front_and_back() {
        let ro = Vec3::new(0.0, 0.0, -5.0);
        let rd = Vec3::new(0.0, 0.0, 1.0);
        let t = ray_sphere(ro, rd, Vec3::ZERO, 1.0).expect("hit");
        assert!((t - 4.0).abs() < 1e-3);
        // Sphere behind the ray origin: no hit.
        assert!(ray_sphere(Vec3::new(0.0, 0.0, 5.0), rd, Vec3::ZERO, 1.0).is_none());
        // Ray missing the sphere sideways.
        assert!(ray_sphere(ro, rd, Vec3::new(3.0, 0.0, 0.0), 1.0).is_none());
    }

    #[test]
    fn ray_aabb_hit() {
        let t = ray_aabb(
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::splat(-1.0),
            Vec3::splat(1.0),
        )
        .expect("hit");
        assert!((t - 4.0).abs() < 1e-3);
    }

    #[test]
    fn pick_prefers_fixture_over_fog_box() {
        // Demo scene: one fixture at (0,4,0) inside a large fog box.
        let scene = Scene::demo();
        let f = scene.fixtures[0].position;
        // Ray from in front of the fixture, aimed at it.
        let ro = f + Vec3::new(0.0, 0.0, 6.0);
        let rd = (f - ro).normalize();
        assert_eq!(pick(&scene, ro, rd), Some(Hit::Fixture(0)));
    }

    #[test]
    fn measure_point_falls_back_to_ground_plane() {
        // An empty scene: a downward ray from y=10 must land on y=0 (the floor).
        let scene = Scene { fixtures: vec![], screens: vec![], geometry: vec![], environments: vec![], ..Scene::demo() };
        let ro = Vec3::new(2.0, 10.0, 3.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let p = pick_world_point(&scene, ro, rd).expect("ground hit");
        assert!((p.y - 0.0).abs() < 1e-3, "expected y=0, got {}", p.y);
        assert!((p.x - 2.0).abs() < 1e-3 && (p.z - 3.0).abs() < 1e-3);
    }

    #[test]
    fn measure_point_prefers_nearer_surface_over_ground() {
        // A fixture sphere between the camera and the floor wins over the y=0 plane.
        let mut scene = Scene { fixtures: vec![], screens: vec![], geometry: vec![], environments: vec![], ..Scene::demo() };
        let demo = Scene::demo();
        scene.fixtures.push(demo.fixtures[0].clone());
        scene.fixtures[0].position = Vec3::new(0.0, 4.0, 0.0);
        scene.fixtures[0].hidden = false;
        let ro = Vec3::new(0.0, 10.0, 0.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let p = pick_world_point(&scene, ro, rd).expect("hit");
        // Should hit the top of the fixture sphere (~y=4.5), not the floor (y=0).
        assert!(p.y > 3.0, "expected fixture hit near y=4.5, got {}", p.y);
    }
}

#[cfg(test)]
mod transform_tests {
    use super::*;
    use crate::ui::Axis;

    fn make_op(kind: TransformKind, axis: Option<Axis>, pivot: Vec3, idx: usize, p0: Vec3) -> TransformOp {
        TransformOp { kind, axis, start_screen: egui::pos2(0.0, 0.0), pivot, start: vec![(idx, p0, Quat::IDENTITY)], geo_start: Vec::new(), gizmo_hovered_axis: None, from_gizmo: false }
    }

    #[test]
    fn move_axis_lock_keeps_other_axes() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        apply_transform(&o, &mut scene, &cam, egui::pos2(120.0, 40.0));
        let d = scene.fixtures[0].position - p0;
        assert!(d.y.abs() < 1e-4, "y leaked: {}", d.y);
        assert!(d.z.abs() < 1e-4, "z leaked: {}", d.z);
    }

    #[test]
    fn rotate_y_preserves_distance_and_height() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 + Vec3::new(2.0, 0.0, 0.0);
        let o = make_op(TransformKind::Rotate, Some(Axis::Y), pivot, 0, p0);
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(80.0, 0.0));
        let before = (p0 - pivot).length();
        let after = (scene.fixtures[0].position - pivot).length();
        assert!((before - after).abs() < 1e-3, "radius changed {before} -> {after}");
        assert!((scene.fixtures[0].position.y - p0.y).abs() < 1e-4, "Y rotation changed height");
    }

    #[test]
    fn scale_expands_from_pivot() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(3.0, 0.0, 0.0);
        let o = make_op(TransformKind::Scale, None, pivot, 0, p0);
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(200.0, 0.0));
        let before = (p0 - pivot).length();
        let after = (scene.fixtures[0].position - pivot).length();
        assert!(after > before, "expected expansion {before} -> {after}");
    }

    #[test]
    fn scale_factor_floor_keeps_geometry_nonzero() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(3.0, 0.0, 0.0);
        let o = make_op(TransformKind::Scale, None, pivot, 0, p0);
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(-100000.0, 0.0));
        let after = (scene.fixtures[0].position - pivot).length();
        assert!(after > 0.0, "geometry collapsed to the pivot");
    }
}
