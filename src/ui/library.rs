//! The content Library browser panel: the searchable/sortable catalogue of
//! fixture / environment / screen templates, multi-select + batch add, drag-to-
//! place, Recent/Favourites pins, and the per-row widget. Extracted from
//! [`super::panels`]; the dock routes the `Tab::Library` arm here and the
//! viewport's Enter path calls [`add_active_library_item`].

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, RichText, Sense};
use glam::Vec3;

use super::GdtfTextures;
use super::panels::{paint_truncated, placement_point};
use super::theme;
use crate::renderer::camera::OrbitCamera;
use crate::scene::{Library, Scene, Selection};

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

/// A coarse content-class filter chip, composed *with* the fuzzy search +
/// sort (S2). Unlike the per-maker/category sort, these partition the catalog by
/// the KIND of thing — what most users reach for first ("just the lasers", "only
/// screens"). `All` is the no-op default (every row passes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibChip {
    /// No filter — the whole catalog.
    All,
    /// Built-in + imported beam/profile fixtures (anything that isn't a laser).
    Fixtures,
    /// Laser-engine fixtures only.
    Lasers,
    /// LED-wall / screen components.
    Screens,
    /// Environment volumes.
    Environments,
    /// Imported GDTF definitions (vs. the built-in profiles).
    Imported,
}

impl LibChip {
    /// Left-to-right chip order.
    const ORDER: [LibChip; 6] = [
        LibChip::All,
        LibChip::Fixtures,
        LibChip::Lasers,
        LibChip::Screens,
        LibChip::Environments,
        LibChip::Imported,
    ];

    fn label(self) -> &'static str {
        match self {
            LibChip::All => "All",
            LibChip::Fixtures => "Fixtures",
            LibChip::Lasers => "Lasers",
            LibChip::Screens => "Screens",
            LibChip::Environments => "Environments",
            LibChip::Imported => "Imported",
        }
    }
}

/// Whether a catalog row passes the active content-class chip (pure, tested).
/// `All` admits everything; the rest gate on the row's [`LibKind`]/`accent`
/// (laser/transparent) flags so the predicate needs nothing but the row itself.
fn chip_matches(chip: LibChip, row: &LibRow) -> bool {
    match chip {
        LibChip::All => true,
        // Beam/profile fixtures: both imported GDTF and built-in NON-laser profiles.
        LibChip::Fixtures => match row.kind {
            LibKind::Gdtf(_) => true,
            LibKind::Fixture(_) => !row.accent, // accent ⇒ laser
            _ => false,
        },
        LibChip::Lasers => matches!(row.kind, LibKind::Fixture(_)) && row.accent,
        LibChip::Screens => matches!(row.kind, LibKind::Screen(_)),
        LibChip::Environments => matches!(row.kind, LibKind::Env(_)),
        LibChip::Imported => matches!(row.kind, LibKind::Gdtf(_)),
    }
}

/// Library panel UI state (search/sort + multi-select with a range anchor).
pub struct LibState {
    pub search: String,
    pub sort: LibSort,
    /// Active content-class filter chip (composed with search + sort).
    pub chip: LibChip,
    /// Selected rows, as indices into the current filtered+sorted list.
    pub selected: Vec<usize>,
    pub anchor: Option<usize>,
    /// The "active" (last-clicked) catalog row, as its STABLE lib-prefs key. Read
    /// from the viewport so pressing Enter there adds the highlighted library item
    /// even when the Library tab isn't the visible sidebar tab. A stable key (not a
    /// filtered-list index) so it survives search/sort/chip rebuilds.
    pub active: Option<String>,
}

impl Default for LibState {
    fn default() -> Self {
        Self {
            search: String::new(),
            sort: LibSort::Category,
            chip: LibChip::All,
            selected: Vec::new(),
            anchor: None,
            active: None,
        }
    }
}

/// One library entry — what it is, plus display metadata.
#[derive(Clone, Copy)]
enum LibKind {
    Gdtf(usize),
    Fixture(usize),
    Env(usize),
    Screen(usize),
}

impl LibKind {
    /// The catalog index into `library.gdtf` for a GDTF row (for thumbnail lookup).
    fn gdtf_index(self) -> Option<usize> {
        match self {
            LibKind::Gdtf(i) => Some(i),
            _ => None,
        }
    }

    /// The lib-prefs identity for Recent/Favourites keying (#20).
    fn item(self) -> crate::ui::lib_prefs::LibItem {
        use crate::ui::lib_prefs::LibItem;
        match self {
            LibKind::Gdtf(i) => LibItem::Gdtf(i),
            LibKind::Fixture(i) => LibItem::Fixture(i),
            LibKind::Env(i) => LibItem::Env(i),
            LibKind::Screen(i) => LibItem::Screen(i),
        }
    }
}

struct LibRow {
    kind: LibKind,
    icon: &'static str,
    name: String,
    meta: String,
    category: String,
    accent: bool, // laser/colour entry → tint the icon
    /// Provenance for the coloured source chip (bug 11). `None` for non-fixture
    /// rows (environments / screens have no fixture source).
    source: Option<crate::gdtf::FixtureSource>,
}

/// Build the flat row list from the library (Imported GDTF, then built-in
/// fixtures, then environments), each with display metadata.
fn library_rows(library: &Library) -> Vec<LibRow> {
    use theme::icon;
    let mut rows = Vec::new();
    for (i, g) in library.gdtf.iter().enumerate() {
        let beam = if g.beam.beam_type.is_empty() {
            ""
        } else {
            g.beam.beam_type.as_str()
        };
        rows.push(LibRow {
            kind: LibKind::Gdtf(i),
            icon: icon::FIXTURE,
            name: g.name.clone(),
            meta: format!(
                "{} · {} · {} mode{}",
                g.manufacturer,
                beam,
                g.modes.len(),
                if g.modes.len() == 1 { "" } else { "s" }
            ),
            category: if g.manufacturer.is_empty() {
                "Imported".into()
            } else {
                g.manufacturer.clone()
            },
            accent: false,
            source: Some(g.source),
        });
    }
    for (i, p) in library.fixtures.iter().enumerate() {
        rows.push(LibRow {
            kind: LibKind::Fixture(i),
            icon: if p.laser { icon::COLOR } else { icon::FIXTURE },
            name: p.name.to_string(),
            meta: if p.laser {
                "Laser engine".into()
            } else {
                format!("{:.0}° beam", p.default_beam_angle)
            },
            category: p.category.to_string(),
            accent: p.laser,
            source: Some(crate::gdtf::FixtureSource::Builtin),
        });
    }
    for (i, p) in library.environments.iter().enumerate() {
        let [w, h, d] = p.default_size;
        rows.push(LibRow {
            kind: LibKind::Env(i),
            icon: icon::ENVIRONMENT,
            name: p.name.to_string(),
            meta: format!("{w:.0} × {h:.0} × {d:.0} m"),
            category: if p.category.is_empty() {
                "Environment"
            } else {
                p.category
            }
            .to_string(),
            accent: false,
            source: None,
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
                pitch,
                p.cabinet_mm[0],
                p.cabinet_mm[1],
                if p.transparent { " · mesh" } else { "" }
            ),
            category: p.category.to_string(),
            accent: p.transparent,
            source: None,
        });
    }
    rows
}

/// Instantiate a library row into the scene at `place` (the viewport
/// cursor/camera anchor, #19); returns the resulting selection.
fn add_library_row(row: &LibRow, library: &Library, scene: &mut Scene, place: Vec3) -> Selection {
    match row.kind {
        LibKind::Gdtf(i) => {
            let arc = library.gdtf[i].clone();
            Selection::fixture(scene.add_gdtf(arc, place))
        }
        LibKind::Fixture(i) => {
            Selection::fixture(scene.add_fixture_at(&library.fixtures[i], place))
        }
        LibKind::Env(i) => {
            Selection::environment(scene.add_environment_at(&library.environments[i], place))
        }
        LibKind::Screen(i) => Selection::screen(scene.add_screen_at(&library.screens[i], place)),
    }
}

/// Resolve the Library tab's `active` row (a stable lib-prefs key) and add it to the
/// scene — the path the viewport's Enter uses, so "add the highlighted library item"
/// works even when the Library tab isn't the visible sidebar tab. `cursor` is the 3D
/// cursor when the user has placed one, else the camera/ground anchor is used.
/// Returns the new selection, or `None` if the key no longer resolves to a catalog row.
pub(crate) fn add_active_library_item(
    library: &Library,
    scene: &mut Scene,
    camera: &OrbitCamera,
    active: &str,
    cursor: Option<Vec3>,
) -> Option<Selection> {
    let rows = library_rows(library);
    let row = rows.iter().find(|r| {
        crate::ui::lib_prefs::entry_key(library, r.kind.item()).as_deref() == Some(active)
    })?;
    let place = cursor.unwrap_or_else(|| placement_point(scene, camera));
    Some(add_library_row(row, library, scene, place))
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
    lib_prefs: &mut crate::ui::lib_prefs::LibraryPrefs,
    open_share: &mut bool,
    // Per-GDTF-type decoded textures (thumbnail + wheel media), keyed by the
    // GDTF Arc pointer — the SAME cache the inspector fills, so a thumbnail
    // decoded for a library row is reused once the fixture is placed (S2).
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
) {
    use crate::ui::lib_prefs;
    use theme::icon;

    // The panel's screen rect — a drag released OUTSIDE it (over the viewport)
    // becomes a drop-to-place (S2b).
    let panel_rect = ui.max_rect();

    // --- header: import / export toolbar (icon buttons) ---
    ui.horizontal(|ui| {
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
            if ui
                .add_enabled(can_export, egui::Button::new(theme::ico(icon::EXPORT)))
                .on_hover_text("Export the scene to MVR")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("MVR scene", &["mvr"])
                    .set_file_name("scene.mvr")
                    .save_file()
                && let Err(e) = crate::mvr::export_path(scene, &path)
            {
                log::error!("MVR export failed: {e}");
            }
            if ui
                .button(theme::ico(icon::IMPORT_MVR))
                .on_hover_text("Import an MVR scene")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("MVR scene", &["mvr"])
                    .pick_file()
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
            if ui
                .button(theme::ico(icon::IMPORT_GDTF))
                .on_hover_text("Import a GDTF fixture into the library")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("GDTF fixture", &["gdtf"])
                    .pick_file()
                && let Err(e) = library.import_gdtf(&path)
            {
                log::error!("GDTF import failed: {e}");
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
        ui.label(theme::ico(icon::SORT).weak())
            .on_hover_text("Sort by");
        for s in [LibSort::Category, LibSort::Name, LibSort::Manufacturer] {
            if ui.selectable_label(lib.sort == s, s.label()).clicked() {
                lib.sort = s;
                lib.selected.clear();
                lib.anchor = None;
            }
        }
    });

    // --- content-class chips (compose with search + sort) ---
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for c in LibChip::ORDER {
            if ui
                .selectable_label(lib.chip == c, RichText::new(c.label()).small())
                .clicked()
            {
                lib.chip = c;
                lib.selected.clear();
                lib.anchor = None;
            }
        }
    });

    // --- build, fuzzy-filter, sort ---
    // The full catalog (in catalog order), each row tagged with its stable
    // lib-prefs key so Recent/Favourites can resolve + render it.
    let all_rows = library_rows(library);
    let key_of = |row: &LibRow| lib_prefs::entry_key(library, row.kind.item()).unwrap_or_default();

    // Content-class chip first (cheap partition), THEN fuzzy/sort over what's left.
    let mut rows: Vec<LibRow> = all_rows
        .into_iter()
        .filter(|r| chip_matches(lib.chip, r))
        .collect();
    let q = lib.search.trim().to_lowercase();
    let fuzzy = !q.is_empty();
    if fuzzy {
        // Fuzzy + recency scorer (#20, shared `lib_prefs::fuzzy_score`): score the
        // best of name/meta/category, drop non-matches, best-first.
        let mut scored: Vec<(i32, LibRow)> = rows
            .into_iter()
            .filter_map(|r| {
                let s = [&r.name, &r.meta, &r.category]
                    .iter()
                    .filter_map(|h| lib_prefs::fuzzy_score(&q, &h.to_lowercase()))
                    .max();
                s.map(|s| (s, r))
            })
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        rows = scored.into_iter().map(|(_, r)| r).collect();
    } else {
        match lib.sort {
            LibSort::Name => rows.sort_by_key(|a| a.name.to_lowercase()),
            LibSort::Manufacturer => rows.sort_by(|a, b| {
                a.category
                    .to_lowercase()
                    .cmp(&b.category.to_lowercase())
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }),
            LibSort::Category => {} // keep build order (already category-grouped)
        }
    }
    lib.selected.retain(|&i| i < rows.len());

    // Pinned Recent + Favourites rows, resolved from prefs against the FULL live
    // catalog (not the chip-filtered subset — a starred screen still shows while
    // the Lasers chip is active). Only shown when not searching (a query searches
    // the full list).
    let pinned = !fuzzy;
    let catalog = if pinned {
        library_rows(library)
    } else {
        Vec::new()
    };
    let recent_rows: Vec<LibRow> = if pinned {
        lib_prefs
            .recent
            .iter()
            .filter_map(|k| catalog.iter().find(|r| &key_of(r) == k).map(clone_lib_row))
            .collect()
    } else {
        Vec::new()
    };
    let fav_rows: Vec<LibRow> = if pinned {
        catalog
            .iter()
            .filter(|r| lib_prefs.is_favourite(&key_of(r)))
            .map(clone_lib_row)
            .collect()
    } else {
        Vec::new()
    };

    // --- batch-add affordance ---
    let mut add_keys: Vec<String> = Vec::new(); // recent keys to record after the borrow ends
    let n_sel = lib.selected.len();
    ui.horizontal(|ui| {
        let label = if n_sel > 1 {
            format!("{}  Add {n_sel}", icon::ADD)
        } else {
            format!("{}  Add", icon::ADD)
        };
        if ui
            .add_enabled(n_sel > 0, egui::Button::new(label))
            .on_hover_text("Add the selected templates to the scene at the cursor (Enter)")
            .clicked()
            || (n_sel > 0
                && !super::text_focus_active(ui.ctx())
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)))
        {
            let place = placement_point(scene, camera);
            let mut idxs = lib.selected.clone();
            idxs.sort_unstable();
            let mut last = None;
            for &ri in &idxs {
                if let Some(row) = rows.get(ri) {
                    last = Some(add_library_row(row, library, scene, place));
                    add_keys.push(key_of(row));
                }
            }
            if let Some(sel) = last {
                *selection = sel;
            }
        }
        ui.label(
            RichText::new(format!("{} items", rows.len()))
                .weak()
                .small(),
        );
    });
    ui.separator();

    // --- thumbnails: decode every imported GDTF's thumbnail into the SHARED
    // cache (keyed by the Arc pointer, same as the inspector), then build a cheap
    // catalog-index → TextureId lookup the row widget can read while `ui` is
    // borrowed mutably. Decoding is one-shot per type (entry-or-insert). (S2a)
    let mut thumb_ids: HashMap<usize, egui::TextureId> = HashMap::new();
    for (gi, g) in library.gdtf.iter().enumerate() {
        let key = Arc::as_ptr(g) as usize;
        let tex = gdtf_textures
            .entry(key)
            .or_insert_with(|| super::inspector::load_gdtf_textures(ui.ctx(), g));
        if let Some(t) = &tex.thumbnail {
            thumb_ids.insert(gi, t.id());
        }
    }
    let thumb_of = |row: &LibRow| {
        row.kind
            .gdtf_index()
            .and_then(|gi| thumb_ids.get(&gi).copied())
    };

    // --- the list (rich, selectable rows; shift = range, ⌘/Ctrl = toggle) ---
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;
    // Channels drained after the scroll closure (it can't borrow library/scene mut).
    let mut add_now: Option<LibRow> = None; // a pinned/double-click add (owns the row)
    let mut clicked: Option<(usize, egui::Modifiers)> = None;
    let mut toggle_fav: Option<String> = None;
    let mut drop_add: Option<LibRow> = None; // a row dragged out into the viewport
    let mut dragging: Option<String> = None; // label of the row being dragged (cursor pill)
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Pinned pseudo-categories: Recent + Favourites (#20).
            for (title, items) in [("RECENT", &recent_rows), ("FAVOURITES", &fav_rows)] {
                if items.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(RichText::new(title).size(10.0).strong().color(ink.tertiary));
                for row in items {
                    let key = key_of(row);
                    let starred = lib_prefs.is_favourite(&key);
                    let (resp, star) =
                        library_row_widget(ui, row, false, starred, &ink, accent, thumb_of(row));
                    if star {
                        toggle_fav = Some(key);
                    } else if resp.clicked() || resp.double_clicked() {
                        add_now = Some(clone_lib_row(row));
                    }
                    if resp.dragged() {
                        dragging = Some(row.name.clone());
                    }
                    if resp.drag_stopped()
                        && ui
                            .input(|i| i.pointer.interact_pos())
                            .is_some_and(|p| !panel_rect.contains(p))
                    {
                        drop_add = Some(clone_lib_row(row));
                    }
                }
                ui.add_space(2.0);
                ui.separator();
            }

            // The full catalog (category-grouped when sorted by Category + not searching).
            let mut last_cat = String::new();
            for (ri, row) in rows.iter().enumerate() {
                if !fuzzy && lib.sort == LibSort::Category && row.category != last_cat {
                    last_cat = row.category.clone();
                    // Same header style as the Replace dialog (theme::section) so the
                    // Library and Replace lists read as one consistent categorisation.
                    theme::section(ui, &row.category.to_uppercase());
                }
                let key = key_of(row);
                let selected = lib.selected.contains(&ri);
                let starred = lib_prefs.is_favourite(&key);
                let (row_resp, star) =
                    library_row_widget(ui, row, selected, starred, &ink, accent, thumb_of(row));
                if star {
                    toggle_fav = Some(key);
                } else if row_resp.clicked() {
                    clicked = Some((ri, ui.input(|i| i.modifiers)));
                }
                if row_resp.double_clicked() {
                    add_now = Some(clone_lib_row(row));
                }
                // Drag-to-place: while a row is dragged show a cursor pill; on release
                // OUTSIDE the panel (i.e. over the viewport) drop it into the scene.
                if row_resp.dragged() {
                    dragging = Some(row.name.clone());
                }
                if row_resp.drag_stopped()
                    && ui
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|p| !panel_rect.contains(p))
                {
                    drop_add = Some(clone_lib_row(row));
                }
            }
            // Apply a select-click (after the loop so we don't borrow rows mid-iter).
            if let Some((ri, mods)) = clicked {
                apply_lib_click(lib, ri, &mods, rows.len());
                // Remember the highlighted row by its STABLE key so the viewport's Enter
                // can add it (see `add_active_library_item`).
                lib.active = rows.get(ri).map(&key_of);
            }
        });

    // Drain the deferred channels now the scroll closure's borrows are released.
    if let Some(key) = toggle_fav {
        lib_prefs.toggle_favourite(&key);
    }
    if let Some(row) = add_now {
        let place = placement_point(scene, camera);
        *selection = add_library_row(&row, library, scene, place);
        add_keys.push(key_of(&row));
    }
    // A row dragged out of the panel and released over the viewport: add it at the
    // current placement point (one undo step, same as Enter/double-click). egui
    // doesn't give the viewport the drop position cross-widget, so we drop at the
    // camera/cursor anchor — consistent with the other add paths (S2b).
    if let Some(row) = drop_add {
        let place = placement_point(scene, camera);
        *selection = add_library_row(&row, library, scene, place);
        add_keys.push(key_of(&row));
    }
    // While dragging a row, show its name in a small pill at the cursor + a "move"
    // cursor, so the drag-to-place gesture reads as a drag (S2b).
    if let Some(name) = dragging {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(p) = ui.ctx().input(|i| i.pointer.interact_pos()) {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("lib-drag-pill"),
            ));
            let text = format!("{}  {}", icon::ADD, name);
            let font = egui::FontId::proportional(12.0);
            let galley =
                painter.layout_no_wrap(text, font, theme::ink(!ui.visuals().dark_mode).primary);
            let pad = egui::vec2(8.0, 4.0);
            let at = p + egui::vec2(14.0, 6.0);
            let bg = egui::Rect::from_min_size(at, galley.size() + pad * 2.0);
            painter.rect_filled(bg, 5.0, ui.visuals().widgets.active.bg_fill);
            painter.rect_stroke(
                bg,
                5.0,
                egui::Stroke::new(1.0, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Inside,
            );
            painter.galley(at + pad, galley, Color32::WHITE);
        }
    }
    for k in add_keys {
        if !k.is_empty() {
            lib_prefs.push_recent(&k);
        }
    }
}

/// Shallow clone of a `LibRow` (so a pinned/recent reference can outlive the
/// borrow of the catalog vector). `LibKind` is Copy; the strings clone.
fn clone_lib_row(r: &LibRow) -> LibRow {
    LibRow {
        kind: r.kind,
        icon: r.icon,
        name: r.name.clone(),
        meta: r.meta.clone(),
        category: r.category.clone(),
        accent: r.accent,
        source: r.source,
    }
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

/// One library row: icon + name (strong) + dim meta, full-width clickable, with
/// selection highlight + hover + a trailing Favourites star (#20). Returns the row
/// response plus whether the *star* (rather than the row body) was clicked — the
/// caller routes a star click to a fav-toggle instead of a select/add.
fn library_row_widget(
    ui: &mut egui::Ui,
    row: &LibRow,
    selected: bool,
    starred: bool,
    ink: &theme::Ink,
    accent: Color32,
    // Decoded GDTF thumbnail for this row, if any — drawn in place of the glyph
    // icon when present, falling back to the icon otherwise (S2a).
    thumb: Option<egui::TextureId>,
) -> (egui::Response, bool) {
    let h = 34.0;
    // click_and_drag so a row can be DRAGGED out into the viewport to place it
    // (S2b); a plain click still selects, a double-click still adds.
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    if selected {
        painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, accent),
            egui::StrokeKind::Inside,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, visuals.widgets.hovered.bg_fill);
    }
    // Leading visual: the GDTF thumbnail when we have one, else the kind glyph.
    match thumb {
        Some(id) => {
            let s = 24.0;
            let tl = egui::pos2(rect.left() + 4.0, rect.center().y - s / 2.0);
            let img = egui::Rect::from_min_size(tl, egui::vec2(s, s));
            painter.image(
                id,
                img,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            let icon_color = if row.accent { accent } else { ink.secondary };
            painter.text(
                rect.left_center() + egui::vec2(9.0, 0.0),
                egui::Align2::LEFT_CENTER,
                row.icon,
                egui::FontId::proportional(16.0),
                icon_color,
            );
        }
    }
    let text_w = (rect.width() - 30.0 - 40.0).max(40.0);
    paint_truncated(
        &painter,
        rect.left_top() + egui::vec2(30.0, 4.0),
        &row.name,
        13.0,
        ink.primary,
        text_w,
    );
    // Reserve room on the meta line for the colour-coded source chip + action
    // gutter (bug 11 + the chip/icon overlap fix): chip ~70px + the 44px gutter.
    let meta_w = if row.source.is_some() {
        (text_w - 110.0).max(30.0)
    } else {
        text_w
    };
    paint_truncated(
        &painter,
        rect.left_top() + egui::vec2(30.0, 19.0),
        &row.meta,
        10.5,
        ink.tertiary,
        meta_w,
    );
    // Source provenance: a clean colour-coded TEXT tag (no floating dot), right-
    // aligned at a fixed gutter and VERTICALLY CENTERED so it reads as a consistent
    // right-hand column across rows (the dot-on-a-margin looked awful). Sits left of
    // the +/★ action gutter so they never collide.
    const ACTION_GUTTER: f32 = 44.0;
    if let Some(src) = row.source {
        let [cr, cg, cb] = src.color_rgb();
        painter.text(
            egui::pos2(rect.right() - ACTION_GUTTER, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            src.label(),
            egui::FontId::proportional(11.0),
            Color32::from_rgb(cr, cg, cb),
        );
    }
    // A "+" affordance on hover (left of the star), right-aligned in the gutter.
    if resp.hovered() {
        painter.text(
            rect.right_center() + egui::vec2(-28.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            theme::icon::ADD,
            egui::FontId::proportional(15.0),
            ink.secondary,
        );
    }
    // The star: always visible when starred, on hover otherwise. Its hit zone is
    // the right ~24px of the row; a click there toggles the favourite.
    let star_zone = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 24.0, rect.top()),
        rect.right_bottom(),
    );
    let mut star_clicked = false;
    if starred || resp.hovered() {
        let tint = if starred { accent } else { ink.tertiary };
        painter.text(
            rect.right_center() + egui::vec2(-10.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            theme::icon::STAR,
            egui::FontId::proportional(14.0),
            tint,
        );
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && star_zone.contains(p)
    {
        star_clicked = true;
    }
    (
        resp.on_hover_text(
            "Click to select · double-click or drag to viewport to add · star = favourite",
        ),
        star_clicked,
    )
}

#[cfg(test)]
mod chip_tests {
    use super::*;

    // --- library content-class chip predicate (S2c) ---------------------------
    fn row(kind: LibKind, accent: bool) -> LibRow {
        LibRow {
            kind,
            icon: "",
            name: String::new(),
            meta: String::new(),
            category: String::new(),
            accent,
            source: None,
        }
    }

    #[test]
    fn chip_all_admits_everything() {
        for r in [
            row(LibKind::Gdtf(0), false),
            row(LibKind::Fixture(0), false),
            row(LibKind::Fixture(0), true),
            row(LibKind::Screen(0), false),
            row(LibKind::Env(0), false),
        ] {
            assert!(chip_matches(LibChip::All, &r));
        }
    }

    #[test]
    fn chip_fixtures_includes_gdtf_and_non_laser_profiles_only() {
        // Imported GDTF + a built-in NON-laser profile pass.
        assert!(chip_matches(
            LibChip::Fixtures,
            &row(LibKind::Gdtf(0), false)
        ));
        assert!(chip_matches(
            LibChip::Fixtures,
            &row(LibKind::Fixture(0), false)
        ));
        // A laser profile (accent) is NOT a "fixture" under this chip.
        assert!(!chip_matches(
            LibChip::Fixtures,
            &row(LibKind::Fixture(1), true)
        ));
        // Screens / environments are excluded.
        assert!(!chip_matches(
            LibChip::Fixtures,
            &row(LibKind::Screen(0), false)
        ));
        assert!(!chip_matches(
            LibChip::Fixtures,
            &row(LibKind::Env(0), false)
        ));
    }

    #[test]
    fn chip_lasers_is_only_accented_profiles() {
        assert!(chip_matches(
            LibChip::Lasers,
            &row(LibKind::Fixture(0), true)
        ));
        assert!(!chip_matches(
            LibChip::Lasers,
            &row(LibKind::Fixture(0), false)
        ));
        // A GDTF is never classed as a laser by this chip (accent is irrelevant).
        assert!(!chip_matches(LibChip::Lasers, &row(LibKind::Gdtf(0), true)));
    }

    #[test]
    fn chip_screens_environments_imported_partition_by_kind() {
        assert!(chip_matches(
            LibChip::Screens,
            &row(LibKind::Screen(0), false)
        ));
        assert!(!chip_matches(
            LibChip::Screens,
            &row(LibKind::Env(0), false)
        ));
        assert!(chip_matches(
            LibChip::Environments,
            &row(LibKind::Env(0), false)
        ));
        assert!(!chip_matches(
            LibChip::Environments,
            &row(LibKind::Screen(0), false)
        ));
        // Imported = GDTF rows only (a built-in profile, even non-laser, is not).
        assert!(chip_matches(
            LibChip::Imported,
            &row(LibKind::Gdtf(0), false)
        ));
        assert!(!chip_matches(
            LibChip::Imported,
            &row(LibKind::Fixture(0), false)
        ));
    }
}

#[cfg(test)]
mod library_add_tests {
    use super::*;
    use crate::renderer::camera::OrbitCamera;
    use crate::scene::{Library, Scene};

    /// The viewport's Enter path: a stable library key resolves to a catalog row and
    /// adds exactly one entity; an unknown key is a clean no-op.
    #[test]
    fn add_active_library_item_adds_the_keyed_row() {
        let library = Library::default(); // the standard library has builtin rows
        let rows = library_rows(&library);
        assert!(!rows.is_empty(), "the standard library should expose rows");
        let key = crate::ui::lib_prefs::entry_key(&library, rows[0].kind.item())
            .expect("a catalog row has a stable key");

        let mut scene = Scene::default();
        let camera = OrbitCamera::default();
        let count = |s: &Scene| {
            s.fixtures.len() + s.geometry.len() + s.screens.len() + s.environments.len()
        };
        let before = count(&scene);
        let sel =
            add_active_library_item(&library, &mut scene, &camera, &key, Some(glam::Vec3::ZERO));
        assert!(sel.is_some(), "a resolvable key adds + returns a selection");
        assert_eq!(count(&scene), before + 1, "exactly one entity is added");

        // An unknown key resolves to nothing → no add, no panic (cursor None is fine,
        // placement_point is only reached once a row resolves).
        let mid = count(&scene);
        assert!(
            add_active_library_item(&library, &mut scene, &camera, "nope::missing", None).is_none()
        );
        assert_eq!(count(&scene), mid, "an unknown key adds nothing");
    }
}
