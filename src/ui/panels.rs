//! The individual dock panels. Each is a plain function taking the egui `Ui`
//! plus whatever scene state it reads or edits.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, DragValue, Grid, RichText, Sense, Slider};
use glam::{Vec2, Vec3};

use super::theme;
use super::windows::{LabelMode, Preferences, ProfileEditor};
use super::{DuplicateDialog, GdtfTextures};
use crate::dmx::patch::channel_map;
use crate::dmx::{DmxConfig, DmxStatus, MergePolicy, PatchSource, PatchTable, PendingNetCmd, UniverseSnapshot};
use crate::gdtf::{GdtfFixture, WheelKind};
use crate::optics::{self, OpticField, OpticalControls};
use crate::renderer::camera::OrbitCamera;
use crate::scene::environment::Environment;
use crate::scene::{apply_fixture_click, Fixture, Library, RenderSettings, Scene, Selection, ViewportMode};

/// Universe is considered live if it updated within this window.
const DMX_STALE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Left tab: the scene outliner — every fixture and environment, selectable —
/// plus the global view/look controls.
/// How the Scene panel's fixture list is ordered.
#[derive(Clone, Copy, PartialEq, Eq)]
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

pub fn scene_outliner(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &mut Selection,
    patch: &PatchTable,
    live_mask: &[bool],
    anchor: &mut Option<usize>,
    sort: &mut SceneSort,
) {
    use theme::icon;
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;
    const ROW_H: f32 = 34.0;

    ui.horizontal(|ui| {
        ui.heading("Scene");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let nsel = selection.fixtures.len();
            if nsel > 0 {
                ui.label(RichText::new(format!("{nsel} selected")).small().color(accent));
            }
        });
    });
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

    // ---- OBJECTS: imported MVR static geometry (stage / truss / set) ----
    // Read-only; can be thousands of rows, so the body is virtualised and the
    // folder defaults closed.
    folder_header(icon::GEOMETRY, "Objects", scene.geometry.len(), false, &ink).show(ui, |ui| {
        if scene.geometry.is_empty() {
            ui.label(RichText::new("none — import an MVR scene").weak().small());
        } else {
            egui::ScrollArea::vertical()
                .id_salt("scene-objects")
                .max_height(220.0)
                .auto_shrink([false, true])
                .show_rows(ui, ROW_H, scene.geometry.len(), |ui, range| {
                    for i in range {
                        let g = &scene.geometry[i];
                        let kind = g.mvr.as_ref().map(|m| m.kind.as_str()).filter(|k| !k.is_empty()).unwrap_or("Object");
                        entity_row(ui, icon::GEOMETRY, &g.name, kind, "", false, false, false, &ink, accent);
                    }
                });
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
        let order = fixture_order(scene, patch, *sort);
        let mut click: Option<(usize, bool, bool)> = None;
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
                    let live = live_mask.get(i).copied().unwrap_or(false);
                    let row_icon = if fixture.is_laser { icon::COLOR } else { icon::FIXTURE };
                    let resp = entity_row(
                        ui,
                        row_icon,
                        &fixture.name,
                        &fixture.profile,
                        &patch_tag,
                        conflicted[i],
                        live,
                        selection.contains_fixture(i),
                        &ink,
                        accent,
                    );
                    if resp.clicked() {
                        let m = ui.input(|x| x.modifiers);
                        click = Some((i, m.shift, m.command || m.ctrl));
                    }
                }
            });
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

    // ---- ENVIRONMENT: fog boxes / world ----
    folder_header(icon::ENVIRONMENT, "Environment", scene.environments.len(), true, &ink).show(ui, |ui| {
        if scene.environments.is_empty() {
            ui.label(RichText::new("none — add a Fog Box from the Library").weak().small());
        }
        for (i, env) in scene.environments.iter().enumerate() {
            let resp = entity_row(
                ui,
                icon::ENVIRONMENT,
                env.name.as_str(),
                "Fog volume",
                "",
                false,
                false,
                selection.environment == Some(i),
                &ink,
                accent,
            );
            if resp.clicked() {
                *selection = Selection::environment(i);
            }
        }
    });

    // ---- WORLD: HDRI environment (sky background + image-based ambient) ----
    folder_header(icon::WORLD, "World", 0, true, &ink).show(ui, |ui| {
        world_controls(ui, &mut scene.world, &ink);
    });

    // (Render/look controls live on the viewport overlay (Mode + Exposure), the
    // View menu (grid / gizmo / label toggles) and Preferences > Rendering — not
    // duplicated here, so the Scene panel stays a clean outliner.)
}

/// The World / environment controls: load an equirectangular HDRI (sky +
/// image-based ambient), set its brightness, ambient fill, yaw and whether it
/// shows as the viewport background.
fn world_controls(ui: &mut egui::Ui, world: &mut crate::scene::World, ink: &theme::Ink) {
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
    }
}

/// Left tab: the content library — a searchable, sortable list of fixture and
/// environment templates with multi-select (shift = range) and batch add.
pub fn library_browser(
    ui: &mut egui::Ui,
    library: &mut Library,
    scene: &mut Scene,
    selection: &mut Selection,
    camera: &mut OrbitCamera,
    lib: &mut LibState,
) {
    use theme::icon;

    // --- header: import / export toolbar (icon buttons) ---
    ui.horizontal(|ui| {
        ui.heading("Library");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

/// A scene-outliner row: icon + name + secondary line, with a right-aligned
/// patch tag and conflict/live status badges. Full-width clickable with a
/// selection highlight. Shared by fixtures and environments.
#[allow(clippy::too_many_arguments)]
fn entity_row(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    secondary: &str,
    patch_tag: &str,
    conflict: bool,
    live: bool,
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
    painter.text(
        rect.left_center() + egui::vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        if selected { accent } else { ink.secondary },
    );
    // Left text zone is bounded so a long name can't run under the right column.
    let text_w = (rect.width() - 30.0 - 64.0).max(40.0);
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 4.0), name, 13.0, ink.primary, text_w);
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 19.0), secondary, 10.5, ink.tertiary, text_w);
    // Right column: patch tag on top, status badges below.
    if !patch_tag.is_empty() {
        let unpatched = patch_tag == "unpatched";
        painter.text(
            rect.right_top() + egui::vec2(-9.0, 6.0),
            egui::Align2::RIGHT_TOP,
            patch_tag,
            egui::FontId::monospace(10.0),
            if unpatched { ink.muted } else { ink.tertiary },
        );
    }
    let mut x = rect.right() - 9.0;
    if live {
        painter.text(
            egui::pos2(x, rect.bottom() - 5.0),
            egui::Align2::RIGHT_BOTTOM,
            "● LIVE",
            egui::FontId::proportional(9.5),
            theme::LIVE,
        );
        x -= 44.0;
    }
    if conflict {
        painter.text(
            egui::pos2(x, rect.bottom() - 5.0),
            egui::Align2::RIGHT_BOTTOM,
            theme::icon::WARNING,
            egui::FontId::proportional(12.0),
            theme::CONFLICT,
        );
    }
    resp
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
pub fn inspector(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &Selection,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
    profile: &mut Option<ProfileEditor>,
) {
    ui.heading("Inspector");
    ui.separator();

    if let Some(env_id) = selection.environment {
        match scene.environments.get_mut(env_id) {
            Some(env) => environment_inspector(ui, env),
            None => {
                ui.label("Selection is no longer valid.");
            }
        }
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
        many => bulk_inspector(ui, scene, many),
    }
}

/// Bulk editor shown when several fixtures are selected: edits a shared property
/// on **all** of them at once (set-semantics, seeded from the first selected).
/// Categories are collapsible and the Optics / Wheels rows are **dynamic** — they
/// show the union of controls the selected fixtures actually expose, not a fixed
/// hardcoded list.
fn bulk_inspector(ui: &mut egui::Ui, scene: &mut Scene, ids: &[usize]) {
    let primary = ids[0];
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  {} fixtures", theme::icon::FIXTURE, ids.len())).strong());
    });
    ui.label(RichText::new("Bulk edit — changes apply to all selected.").weak().small());
    ui.separator();

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
                let mut intensity = scene.fixtures[primary].intensity;
                ui.label("Intensity");
                if ui.add(Slider::new(&mut intensity, 0.0..=1.0)).changed() {
                    for &i in ids {
                        scene.fixtures[i].intensity = intensity;
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
                ui.label("Intensity");
                ui.add(DragValue::new(&mut fixture.intensity).speed(0.005).range(0.0..=1.0));
                ui.end_row();
                ui.label("Beam");
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
                ui.label("Tint");
                ui.color_edit_button_rgb(&mut env.color);
                ui.end_row();
            });
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
                let level = fixture.intensity.clamp(0.0, 1.0);
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
                ui.label("Intensity");
                ui.add(DragValue::new(&mut fixture.intensity).speed(0.005).range(0.0..=1.0));
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
                        if comp.has_index {
                            ui.label("  index");
                            ui.add(Slider::new(&mut w.index, 0.0..=1.0));
                            ui.end_row();
                        }
                        if comp.has_spin || matches!(comp.kind, WheelKind::Color | WheelKind::Animation) {
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

/// Central tab: the 3D scene, rendered offscreen and shown as a texture.
/// Drag to orbit, shift+drag to pan, scroll to zoom, click to select, `d` to
/// duplicate the selected fixture.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn viewport(
    ui: &mut egui::Ui,
    camera: &mut OrbitCamera,
    scene: &Scene,
    selection: &mut Selection,
    scene_anchor: &mut Option<usize>,
    viewport_focused: &mut bool,
    duplicate: &mut Option<DuplicateDialog>,
    texture: egui::TextureId,
    requested_px: &mut (u32, u32),
    fps: f32,
    prefs: &Preferences,
    settings: &mut RenderSettings,
) {
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

    if response.dragged() {
        let delta = response.drag_delta();
        if ui.input(|i| i.modifiers.shift) {
            camera.pan(delta.x, delta.y);
        } else {
            camera.orbit(delta.x, delta.y);
        }
    }
    if response.contains_pointer() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            camera.zoom(scroll * 0.01);
        }
    }

    // Click to select: cast a ray through the cursor and pick the nearest object.
    // ⌘/Ctrl-click toggles into a multi-selection; Shift-click range-selects from
    // the anchor (same as the outliner). A drag with Shift pans, so a stationary
    // Shift-click still range-selects.
    if response.clicked()
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
            Some(Hit::Environment(i)) => *selection = Selection::environment(i),
            None if !(toggle || m.shift) => *selection = Selection::default(),
            None => {}
        }
    }

    // `d` opens the Duplicate dialog for the selected fixture.
    if *viewport_focused
        && duplicate.is_none()
        && ui.input(|i| i.key_pressed(egui::Key::D))
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

    ui.painter().text(
        rect.left_bottom() + egui::vec2(8.0, -6.0),
        egui::Align2::LEFT_BOTTOM,
        "drag: orbit · shift+drag: pan · scroll: zoom · click: select · d: duplicate",
        egui::FontId::proportional(11.0),
        egui::Color32::from_white_alpha(110),
    );

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

    // Display overlay (top-left, on the viewport where the eyes are — Blender's
    // shading buttons live here too): the display Mode + exposure, the two
    // controls a designer reaches for constantly. Advanced look settings
    // (bloom/beam/steps) stay in Preferences; toggles in the View menu.
    egui::Area::new(egui::Id::new("viewport-display-overlay"))
        .fixed_pos(rect.left_top() + egui::vec2(8.0, if prefs.show_fps { 28.0 } else { 8.0 }))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::symmetric(6, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for m in ViewportMode::ALL {
                            if ui.selectable_label(settings.mode == m, m.label()).clicked() {
                                settings.mode = m;
                            }
                        }
                        ui.separator();
                        ui.label(RichText::new("Exp").small());
                        ui.add(DragValue::new(&mut settings.exposure).speed(0.01).range(0.05..=8.0));
                    });
                });
        });
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
                ui.colored_label(theme::LIVE, format!("● LIVE · {n} src"));
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
    live_mask: &[bool],
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
            if ui.button("Patch seq").on_hover_text("Assign the selected fixtures sequentially from U.@ by footprint").clicked() {
                let (mut u, mut a) = (fm.bulk_universe, fm.bulk_address);
                for &i in &sel {
                    let fp = patch.get(i).map(|p| p.footprint).unwrap_or(1).max(1);
                    if a as u32 + fp as u32 - 1 > 512 {
                        u += 1;
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
                // shift = range, ⌘/Ctrl = toggle. Live fixtures get a green dot.
                let selected = selection.contains_fixture(i);
                let live = live_mask.get(i).copied().unwrap_or(false);
                let dot = if live { "● " } else { "" };
                let resp = ui.selectable_label(selected, RichText::new(format!("{dot}{}", fixture.name)).small());
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
            let cpos = order.iter().position(|&x| x == i).unwrap_or(0);
            let apos = anchor.and_then(|a| order.iter().position(|&x| x == a)).unwrap_or(cpos);
            let (lo, hi) = (apos.min(cpos), apos.max(cpos));
            selection.fixtures = order[lo..=hi].to_vec();
            selection.environment = None;
            if anchor.is_none() {
                *anchor = Some(i);
            }
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
    Environment(usize),
}

/// Pick the object a world-space ray hits. Fixtures take priority (so you can
/// always click a head even when it sits inside the fog box); only if none is
/// hit do we test the environment volumes.
fn pick(scene: &Scene, ro: Vec3, rd: Vec3) -> Option<Hit> {
    let mut best: Option<(f32, usize)> = None;
    for (i, f) in scene.fixtures.iter().enumerate() {
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
}
