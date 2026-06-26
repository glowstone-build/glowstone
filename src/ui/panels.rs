//! The individual dock panels. Each is a plain function taking the egui `Ui`
//! plus whatever scene state it reads or edits.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, DragValue, Grid, RichText, Sense};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3};

use super::gizmo::{self, GizmoCtx, Handle};
use super::nav_gizmo;
use super::shortcuts;
use super::theme;
use super::windows::{LabelMode, Preferences};
use super::{
    ActiveTool, Axis, DuplicateDialog, GdtfTextures, NumInput, PivotMode, SelectionGroup,
    TransformKind, TransformOp, TransformPrefs,
};
use crate::dmx::patch::channel_map;
use crate::dmx::{DmxConfig, DmxStatus, MergePolicy, PatchSource, PatchTable, PendingNetCmd, UniverseSnapshot};
use crate::renderer::camera::OrbitCamera;
use crate::scene::{apply_fixture_click, apply_select, Library, ObjectRef, Scene, SelItem, SelectOp, Selection};

/// Universe is considered live if it updated within this window.
const DMX_STALE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Per-frame drag edges the [`inspector`] reports up to [`Ui`](super::Ui) so a
/// slider / DragValue drag becomes ONE undo step (P0 #13). The inspector edits the
/// scene directly (its established live-edit model); these flags let the
/// post-dock consumer wrap the WHOLE gesture in a single [`op::DragTx`]
/// transaction — `started` snapshots the `before`, `stopped` pushes one step.
/// Detected at panel scope (no per-widget instrumentation): a numeric widget
/// inside the inspector's content rect began / ended a pointer drag this frame.
#[derive(Default, Clone, Copy)]
pub struct InspectorEdit {
    /// A numeric drag inside the inspector just began this frame.
    pub started: bool,
    /// A numeric drag inside the inspector was released this frame.
    pub stopped: bool,
}

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
pub(super) fn fixture_order(scene: &Scene, patch: &PatchTable, sort: SceneSort) -> Vec<usize> {
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
    filter: &mut super::tree::OutlinerFilter,
    expanded: &mut std::collections::HashSet<super::tree::NodeKey>,
    rename: &mut Option<(super::tree::NodeKey, String)>,
    pending: &mut super::tree::TreeAction,
    groups: &mut Vec<SelectionGroup>,
    group_name: &mut String,
) {
    use theme::icon;
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;

    ui.horizontal(|ui| {
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

    // Type + state FILTER CHIPS (catalog #62): a compact toggle row that narrows
    // the tree by entity kind (All/Fixtures/Objects/Screens — mutually exclusive)
    // and by fixture state (Unpatched/Selected/Conflicts — composable toggles),
    // ANDed onto the fuzzy search above. Matches the dense console look (small
    // selectable labels, no emoji).
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for chip in super::tree::TypeChip::ORDER {
            if ui.selectable_label(filter.kind == chip, RichText::new(chip.label()).small()).clicked() {
                filter.kind = chip;
            }
        }
        ui.separator();
        // State chips: each toggles a fixture-state predicate. A muted tint + the
        // CONFLICT colour on the Conflicts chip keep them readable at a glance.
        if ui
            .selectable_label(filter.state.unpatched, RichText::new("Unpatched").small())
            .on_hover_text("Only fixtures with no enabled patch")
            .clicked()
        {
            filter.state.unpatched = !filter.state.unpatched;
        }
        if ui
            .selectable_label(filter.state.selected, RichText::new("Selected").small())
            .on_hover_text("Only the current selection")
            .clicked()
        {
            filter.state.selected = !filter.state.selected;
        }
        let conflict_txt = if filter.state.conflicts {
            RichText::new("Conflicts").small().color(theme::CONFLICT)
        } else {
            RichText::new("Conflicts").small()
        };
        if ui.selectable_label(filter.state.conflicts, conflict_txt).on_hover_text("Only address-conflicting fixtures").clicked() {
            filter.state.conflicts = !filter.state.conflicts;
        }
    });
    ui.separator();

    // The project HIERARCHY: one custom recursive tree under a single "Scene"
    // root (World/Fixtures/Objects/Screens nested beneath), replacing the old
    // flat CollapsingHeader folders. See src/ui/tree.rs.
    ui.horizontal(|ui| {
        ui.label(theme::ico(icon::SORT).weak()).on_hover_text("Sort fixtures by");
        for s in [SceneSort::Patch, SceneSort::Name, SceneSort::Type] {
            ui.selectable_value(sort, s, s.label());
        }
    });
    egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(4, 4))
        .show(ui, |ui| {
            let act = super::tree::scene_tree(ui, scene, selection, patch, anchor, *sort, search, *filter, expanded, rename);
            // Defer hide/rename (need an undo step) to the post-dock consumer.
            if !matches!(act, super::tree::TreeAction::None) {
                *pending = act;
            }
        });
    ui.add_space(6.0);

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
        let beam = if g.beam.beam_type.is_empty() { "" } else { g.beam.beam_type.as_str() };
        rows.push(LibRow {
            kind: LibKind::Gdtf(i),
            icon: icon::FIXTURE,
            name: g.name.clone(),
            meta: format!("{} · {} · {} mode{}", g.manufacturer, beam, g.modes.len(), if g.modes.len() == 1 { "" } else { "s" }),
            category: if g.manufacturer.is_empty() { "Imported".into() } else { g.manufacturer.clone() },
            accent: false,
            source: Some(g.source),
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
            category: if p.category.is_empty() { "Environment" } else { p.category }.to_string(),
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
                pitch, p.cabinet_mm[0], p.cabinet_mm[1], if p.transparent { " · mesh" } else { "" }
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
        LibKind::Fixture(i) => Selection::fixture(scene.add_fixture_at(&library.fixtures[i], place)),
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
    let row = rows
        .iter()
        .find(|r| crate::ui::lib_prefs::entry_key(library, r.kind.item()).as_deref() == Some(active))?;
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

    // --- content-class chips (compose with search + sort) ---
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for c in LibChip::ORDER {
            if ui.selectable_label(lib.chip == c, RichText::new(c.label()).small()).clicked() {
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
    let mut rows: Vec<LibRow> =
        all_rows.into_iter().filter(|r| chip_matches(lib.chip, r)).collect();
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
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        rows = scored.into_iter().map(|(_, r)| r).collect();
    } else {
        match lib.sort {
            LibSort::Name => rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            LibSort::Manufacturer => rows.sort_by(|a, b| {
                a.category.to_lowercase().cmp(&b.category.to_lowercase()).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
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
    let catalog = if pinned { library_rows(library) } else { Vec::new() };
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
        catalog.iter().filter(|r| lib_prefs.is_favourite(&key_of(r))).map(clone_lib_row).collect()
    } else {
        Vec::new()
    };

    // --- batch-add affordance ---
    let mut add_keys: Vec<String> = Vec::new(); // recent keys to record after the borrow ends
    let n_sel = lib.selected.len();
    ui.horizontal(|ui| {
        let label = if n_sel > 1 { format!("{}  Add {n_sel}", icon::ADD) } else { format!("{}  Add", icon::ADD) };
        if ui.add_enabled(n_sel > 0, egui::Button::new(label)).on_hover_text("Add the selected templates to the scene at the cursor (Enter)").clicked()
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
        ui.label(RichText::new(format!("{} items", rows.len())).weak().small());
    });
    ui.separator();

    // --- thumbnails: decode every imported GDTF's thumbnail into the SHARED
    // cache (keyed by the Arc pointer, same as the inspector), then build a cheap
    // catalog-index → TextureId lookup the row widget can read while `ui` is
    // borrowed mutably. Decoding is one-shot per type (entry-or-insert). (S2a)
    let mut thumb_ids: HashMap<usize, egui::TextureId> = HashMap::new();
    for (gi, g) in library.gdtf.iter().enumerate() {
        let key = Arc::as_ptr(g) as usize;
        let tex = gdtf_textures.entry(key).or_insert_with(|| super::inspector::load_gdtf_textures(ui.ctx(), g));
        if let Some(t) = &tex.thumbnail {
            thumb_ids.insert(gi, t.id());
        }
    }
    let thumb_of = |row: &LibRow| row.kind.gdtf_index().and_then(|gi| thumb_ids.get(&gi).copied());

    // --- the list (rich, selectable rows; shift = range, ⌘/Ctrl = toggle) ---
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;
    // Channels drained after the scroll closure (it can't borrow library/scene mut).
    let mut add_now: Option<LibRow> = None; // a pinned/double-click add (owns the row)
    let mut clicked: Option<(usize, egui::Modifiers)> = None;
    let mut toggle_fav: Option<String> = None;
    let mut drop_add: Option<LibRow> = None; // a row dragged out into the viewport
    let mut dragging: Option<String> = None; // label of the row being dragged (cursor pill)
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
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
                let (resp, star) = library_row_widget(ui, row, false, starred, &ink, accent, thumb_of(row));
                if star {
                    toggle_fav = Some(key);
                } else if resp.clicked() || resp.double_clicked() {
                    add_now = Some(clone_lib_row(row));
                }
                if resp.dragged() {
                    dragging = Some(row.name.clone());
                }
                if resp.drag_stopped()
                    && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !panel_rect.contains(p))
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
            let (row_resp, star) = library_row_widget(ui, row, selected, starred, &ink, accent, thumb_of(row));
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
                && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !panel_rect.contains(p))
            {
                drop_add = Some(clone_lib_row(row));
            }
        }
        // Apply a select-click (after the loop so we don't borrow rows mid-iter).
        if let Some((ri, mods)) = clicked {
            apply_lib_click(lib, ri, &mods, rows.len());
            // Remember the highlighted row by its STABLE key so the viewport's Enter
            // can add it (see `add_active_library_item`).
            lib.active = rows.get(ri).map(|r| key_of(r));
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
            let galley = painter.layout_no_wrap(text, font, theme::ink(!ui.visuals().dark_mode).primary);
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

/// Paint left-anchored text truncated with an ellipsis to `max_w` (single row) —
/// used by the dense list rows so a long name can't run under the right column.
pub(super) fn paint_truncated(
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
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    painter.galley(top_left, galley, color);
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
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    if selected {
        painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Inside);
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
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 4.0), &row.name, 13.0, ink.primary, text_w);
    // Reserve room on the meta line for the colour-coded source chip + action
    // gutter (bug 11 + the chip/icon overlap fix): chip ~70px + the 44px gutter.
    let meta_w = if row.source.is_some() { (text_w - 110.0).max(30.0) } else { text_w };
    paint_truncated(&painter, rect.left_top() + egui::vec2(30.0, 19.0), &row.meta, 10.5, ink.tertiary, meta_w);
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
    (resp.on_hover_text("Click to select · double-click or drag to viewport to add · star = favourite"), star_clicked)
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
/// The world origin/centre of one selected fixture (its position) or geometry (its
/// world-bounds centre, falling back to the transform's translation). The single
/// Compute the single world pivot the rotate/scale spins/grows about, per the
/// chosen [`PivotMode`] (#5). `objs` are the (validated) selected objects in
/// selection order, so `objs[0]` is the active element. Reads each object's
/// origin through the unified [`Scene::object_anchor`] so EVERY kind (fixtures,
/// geometry, screens, environment) contributes — no kind is excluded from the
/// centroid. Individual-Origins has no single pivot (the applier uses each
/// element's own anchor) so it returns the Median like the others. Empty
/// selection → origin.
fn compute_pivot(scene: &Scene, objs: &[ObjectRef], mode: PivotMode, cursor_3d: Vec3) -> Vec3 {
    match mode {
        PivotMode::Cursor3d => cursor_3d,
        PivotMode::Active => objs.first().and_then(|&o| scene.object_anchor(o)).unwrap_or(Vec3::ZERO),
        // Median + Individual both seed from the centroid (Individual's per-element
        // pivots are applied in apply_transform via `op.individual`).
        PivotMode::Median | PivotMode::Individual => {
            let mut sum = Vec3::ZERO;
            let mut n = 0.0_f32;
            for &o in objs {
                if let Some(a) = scene.object_anchor(o) {
                    sum += a;
                    n += 1.0;
                }
            }
            if n > 0.0 { sum / n } else { Vec3::ZERO }
        }
    }
}

/// Flatten validated per-kind index slices into the unified [`ObjectRef`] list
/// (fixtures, then geometry, screens, environment) — the order the pivot's
/// "active" element and the gizmo read. The single place selection → object list
/// happens for the transform path.
fn obj_refs(fids: &[usize], gids: &[usize], sids: &[usize], eids: &[usize]) -> Vec<ObjectRef> {
    fids.iter()
        .map(|&i| ObjectRef::Fixture(i))
        .chain(gids.iter().map(|&i| ObjectRef::Geometry(i)))
        .chain(sids.iter().map(|&i| ObjectRef::Screen(i)))
        .chain(eids.iter().map(|&i| ObjectRef::Environment(i)))
        .collect()
}

/// Capture the per-kind op-start snapshots for a transform: fixtures keep
/// (position, orientation); geometry + screens keep their world `Mat4`;
/// environments keep (centre, size). ONE source for both the gizmo-drag and the
/// modal G/R/S op-start so every kind is snapshotted — and thus live-applied and
/// cancel-restored — identically.
#[allow(clippy::type_complexity)]
fn snapshot_starts(
    scene: &Scene,
    fids: &[usize],
    gids: &[usize],
    sids: &[usize],
    eids: &[usize],
) -> (Vec<(usize, Vec3, Quat)>, Vec<(usize, Mat4)>, Vec<(usize, Mat4)>, Vec<(usize, Vec3, Vec3)>) {
    let start = fids
        .iter()
        .map(|&i| (i, scene.fixtures[i].position, scene.fixtures[i].orientation))
        .collect();
    let geo_start = gids.iter().map(|&i| (i, scene.geometry[i].transform)).collect();
    let screen_start = sids.iter().map(|&i| (i, scene.screens[i].transform)).collect();
    let env_start =
        eids.iter().map(|&i| (i, scene.environments[i].center, scene.environments[i].size)).collect();
    (start, geo_start, screen_start, env_start)
}

/// Resolve the transform-orientation (#37) to a 3×3 whose COLUMNS are the X/Y/Z
/// directions the axis-lock + numeric default map through (`basis * Axis::vec()`).
/// Mirrors Blender's `applyTransformOrientation` / `ED_transform_calc_orientation`:
///
/// * Global → identity (the world axes; today's behaviour, byte-identical).
/// * Local → the active element's OWN orientation. For a fixture that's its
///   `start` Quat snapshot (stable across the drag); for geometry-only selections
///   the geometry transform's rotation. Falls back to identity if the basis is
///   degenerate (no selection / zero-scale geometry).
/// * View → the camera basis: column 0 = screen-right, 1 = screen-up, 2 = toward
///   the viewer (`-forward`), so a View-axis move follows the screen plane.
fn orientation_basis(op: &TransformOp, camera: &OrbitCamera) -> glam::Mat3 {
    use super::TransformOrientation as TO;
    match op.orientation {
        TO::Global => glam::Mat3::IDENTITY,
        TO::View => {
            let (right, up, fwd) = camera.view_basis();
            // Columns = X(right) / Y(up) / Z(toward viewer = -forward).
            glam::Mat3::from_cols(right, up, -fwd)
        }
        TO::Local => {
            // Prefer the active fixture's orientation (its op-start Quat snapshot);
            // else the first geometry/screen piece's rotation from its op-start
            // matrix (screens share geometry's Mat4 transform).
            if let Some((_, _, q)) = op.start.first() {
                glam::Mat3::from_quat(*q)
            } else if let Some((_, m0)) = op.geo_start.first().or_else(|| op.screen_start.first()) {
                let b = glam::Mat3::from_mat4(*m0);
                // Normalize the columns so non-uniform scale doesn't skew the axes;
                // a degenerate column falls back to the world axis.
                let col = |i: usize, fallback: Vec3| {
                    let c = b.col(i);
                    c.try_normalize().unwrap_or(fallback)
                };
                glam::Mat3::from_cols(col(0, Vec3::X), col(1, Vec3::Y), col(2, Vec3::Z))
            } else {
                glam::Mat3::IDENTITY
            }
        }
    }
}

/// The point on the infinite axis line `p + t·axis` CLOSEST to the ray
/// `ro + s·rd` — the #40 ray-plane absolute drag for a single-axis Move. Standard
/// closest-points-between-two-lines (UE `GetAbsoluteTranslationDelta` / Blender
/// `transform_constraints.cc applyAxisConstraintVec` project the cursor ray onto
/// the constraint axis the same way). The handle "sticks" to the cursor because
/// the returned point tracks the cursor's projection along the axis at any camera
/// angle, instead of a fixed pixels→metres speed that drifts at grazing angles.
///
/// `axis` need not be unit (it's normalized here). When the ray is (near-)parallel
/// to the axis the cross-product denominator collapses → returns `p` (no motion),
/// so a degenerate viewing angle can't fling the handle to infinity.
fn ray_axis_closest_point(ro: Vec3, rd: Vec3, p: Vec3, axis: Vec3) -> Vec3 {
    let a = axis.normalize_or_zero();
    let r = rd.normalize_or_zero();
    if a == Vec3::ZERO || r == Vec3::ZERO {
        return p;
    }
    // Closest points between two lines (Ericson, Real-Time Collision Detection):
    // axis line = p + s·a, ray line = ro + u·r, both directions unit. Solve for the
    // parameter `s` along the axis that minimizes the gap, then return p + s·a.
    let rel = p - ro;
    let b = a.dot(r); // = cos∠ between axis and ray
    let c = a.dot(rel);
    let f = r.dot(rel);
    let denom = 1.0 - b * b; // = |a×r|² for unit a,r
    if denom.abs() < 1e-6 {
        return p; // axis ∥ ray — undefined projection, hold position
    }
    let s = (b * f - c) / denom; // parameter along the axis
    p + a * s
}

/// Intersect the ray `ro + t·rd` with the plane through `p` with `normal` — the
/// #40 absolute drag for a PLANE-constrained / screen-plane Move (and a building
/// block for future two-axis gizmo handles). Returns the world hit, or `None` when
/// the ray is parallel to the plane (or points away from it: `t ≤ 0`).
// Drives the move gizmo's PLANE handles (#S2): the absolute two-axis drag that keeps
// the grabbed quad stuck to the cursor while the off-plane axis stays fixed.
fn ray_plane_point(ro: Vec3, rd: Vec3, p: Vec3, normal: Vec3) -> Option<Vec3> {
    let n = normal.normalize_or_zero();
    let denom = rd.dot(n);
    if denom.abs() < 1e-6 {
        return None; // ray ∥ plane
    }
    let t = (p - ro).dot(n) / denom;
    if t <= 0.0 {
        return None;
    }
    Some(ro + rd * t)
}

/// The nearest OTHER entity origin to `moved` within `max_dist_px` on screen — the
/// #71 Vertex snap target. Scans fixtures, geometry origins and screen origins
/// (skipping `exclude` — the fixtures being moved — and hidden ones), projecting
/// each to screen and keeping the closest to the live cursor `cursor_px`. Screen-
/// space thresholding (Blender's snap is screen-radius based) means the snap only
/// engages when the cursor is genuinely over a node, regardless of world scale.
/// Returns the WORLD origin to snap to, or `None` when nothing is in range.
fn nearest_origin_screen(
    scene: &Scene,
    vp: Mat4,
    rect: egui::Rect,
    cursor_px: egui::Pos2,
    exclude: &[usize],
    max_dist_px: f32,
) -> Option<Vec3> {
    let mut best: Option<(f32, Vec3)> = None;
    let mut consider = |world: Vec3| {
        if let Some(sp) = OrbitCamera::project_to_screen(world, vp, rect) {
            let d = sp.distance(cursor_px);
            if d <= max_dist_px && best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, world));
            }
        }
    };
    for (i, f) in scene.fixtures.iter().enumerate() {
        if !f.hidden && !exclude.contains(&i) {
            consider(f.position);
        }
    }
    for g in &scene.geometry {
        if !g.hidden {
            consider(g.transform.w_axis.truncate());
        }
    }
    for s in &scene.screens {
        if !s.hidden {
            consider(s.transform.w_axis.truncate());
        }
    }
    best.map(|(_, w)| w)
}

/// #70 Snap-target PREVIEW: the WORLD destination the PRIMARY moved element's origin
/// will land on, for the snap marker drawn DURING a live Move drag (before release).
/// Returns `None` unless the op is a Move with snap currently engaged — Rotate/Scale
/// and snap-off drags have no destination marker.
///
/// `apply_transform` has already moved the primary element to its snapped destination
/// this frame, so the live origin (`op.start.first()` → `scene.fixtures[i].position`,
/// or the first geometry's translation) IS the destination — for Vertex it's the
/// snapped node, for Surface the cursor surface hit, for Increment the quantized
/// origin. Reading it back (rather than recomputing the snap) keeps the marker exactly
/// consistent with where the element actually goes. Pure + cheap → unit-testable.
/// `snap_on` is the live-resolved snap state (`op.snap.on` XOR a held Ctrl), so a
/// mid-drag Ctrl tap hides/shows the marker in lockstep with the actual snapping.
fn snap_preview_point(op: &TransformOp, scene: &Scene, snap_on: bool) -> Option<Vec3> {
    if op.kind != TransformKind::Move || !snap_on {
        return None;
    }
    if let Some((i, _, _)) = op.start.first() {
        return scene.fixtures.get(*i).map(|f| f.position);
    }
    if let Some((i, _)) = op.geo_start.first() {
        return scene.geometry.get(*i).map(|g| g.transform.w_axis.truncate());
    }
    None
}

/// Draw the #70 snap-destination marker: a small ring + cross at the projected snap
/// point, tinted to read as "this is where it lands". Screen-space sized so it stays
/// legible at any zoom. No-op when the point is behind the camera / off the rect math.
fn draw_snap_marker(painter: &egui::Painter, world: Vec3, vp: Mat4, rect: egui::Rect) {
    let Some(c) = OrbitCamera::project_to_screen(world, vp, rect) else { return };
    let col = egui::Color32::from_rgb(120, 230, 255); // cyan — the "snap" accent
    let r = 6.0;
    painter.circle_stroke(c, r, egui::Stroke::new(1.5, col));
    // A small plus through the centre so the exact point reads even over busy geometry.
    let x = r + 3.0;
    painter.line_segment([c - egui::vec2(x, 0.0), c + egui::vec2(x, 0.0)], egui::Stroke::new(1.0, col));
    painter.line_segment([c - egui::vec2(0.0, x), c + egui::vec2(0.0, x)], egui::Stroke::new(1.0, col));
}

/// frame the op is live; cancelling restores from the same snapshot.
fn apply_transform(
    op: &TransformOp,
    scene: &mut Scene,
    camera: &OrbitCamera,
    cur: egui::Pos2,
    // Live snap-enabled state (#4): `op.snap.on` XOR a held-Ctrl invert, resolved by
    // the caller this frame. Quantizes the COMMITTED amount (delta / angle / factor),
    // never the raw mouse delta — so it composes with the numeric entry too.
    snap_on: bool,
) {
    // Position/orientation are read directly by the renderer, so they need no
    // snap_movement() — and calling it every frame would freeze each fixture's
    // wheel-motion phase. (Cancel restores from the same snapshot the same way.)
    let d = cur - op.start_screen; // pixel delta (mouse-driven path)
    let (right, up, _fwd) = camera.view_basis();
    // A grabbed gizmo handle overrides the keyboard axis lock for this frame.
    let axis = op.active_axis();
    // #37: the transform-orientation basis (columns = X/Y/Z directions). The axis
    // lock + numeric-default axis map through this — Global = identity (world axes),
    // Local = the element's own basis, View = the camera basis. `axis_dir` gives the
    // world direction of an `Axis` in the chosen orientation.
    let basis = orientation_basis(op, camera);
    let axis_dir = |a: Axis| basis * a.vec();
    // Explicit-amount: a typed number OVERRIDES the mouse (Blender applyNumInput
    // returns true). Single value → along the active axis (Move falls back to
    // global X, Rotate to Y, Scale to uniform — matching the mouse-path defaults).
    let amount = op.num.value();
    // During a duplicate-grab the mouse ALWAYS drives the offset; a typed number is
    // the array clone-count (applied on confirm), not the move amount — so don't let
    // it override the drag here.
    let typed = op.num.active && !op.from_duplicate;
    // Build a picking ray from a screen position through the op's viewport rect
    // (#40 absolute drag + #71 Vertex/Surface snap both need world rays, not just
    // the pixel delta). Aspect derives from the stored rect.
    let aspect = op.viewport.width() / op.viewport.height().max(1.0);
    let ray_at = |p: egui::Pos2| -> (Vec3, Vec3) {
        let size = op.viewport.size().max(egui::vec2(1.0, 1.0));
        let uv = (p - op.viewport.min) / size;
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        camera.ray(ndc, aspect)
    };
    match op.kind {
        TransformKind::Move => {
            let mut world = if typed {
                // Metres along the active axis (in the chosen orientation); no lock →
                // the orientation's X (Blender's single-value-no-constraint default,
                // expressed in the active basis — world X for Global).
                let a = axis.map(axis_dir).unwrap_or_else(|| basis * Vec3::X);
                a * amount
            } else if op.gizmo_view && op.from_gizmo && op.viewport.area() > 0.0 {
                // VIEW-plane move (#72): the screen-parallel drag. The plane normal is
                // the camera forward, so the grabbed centre square slides on the plane
                // facing the viewer and STICKS to the cursor (a ray_plane_point
                // absolute drag, same machinery as the axis-pair plane handles).
                let n = camera.view_basis().2;
                let (ro0, rd0) = ray_at(op.start_screen);
                let (ro1, rd1) = ray_at(cur);
                match (
                    ray_plane_point(ro0, rd0, op.pivot, n),
                    ray_plane_point(ro1, rd1, op.pivot, n),
                ) {
                    (Some(from), Some(to)) => to - from,
                    _ => Vec3::ZERO,
                }
            } else if let Some(normal) = op.gizmo_plane_normal.filter(|_| op.from_gizmo && op.viewport.area() > 0.0) {
                // PLANE handle (#S2): intersect the start + live cursor rays with the
                // plane through the pivot whose normal is the held axis (in the active
                // orientation), and take the difference. The off-plane axis stays fixed
                // (the delta lies in the plane), and the grabbed quad STICKS to the
                // cursor at any camera angle. Falls back to no motion if either ray
                // misses (grazing the plane edge-on).
                let n = axis_dir(normal);
                let (ro0, rd0) = ray_at(op.start_screen);
                let (ro1, rd1) = ray_at(cur);
                match (
                    ray_plane_point(ro0, rd0, op.pivot, n),
                    ray_plane_point(ro1, rd1, op.pivot, n),
                ) {
                    (Some(from), Some(to)) => to - from,
                    _ => Vec3::ZERO,
                }
            } else if op.from_gizmo && axis.is_some() && op.viewport.area() > 0.0 {
                // #40 ray-plane ABSOLUTE drag: project the start + live cursor rays onto
                // the constraint axis line through the pivot; the world delta is the
                // difference, so the grabbed handle STICKS to the cursor at any camera
                // angle (vs the pixel-speed heuristic that drifts at grazing angles).
                let a = axis_dir(axis.unwrap());
                let (ro0, rd0) = ray_at(op.start_screen);
                let (ro1, rd1) = ray_at(cur);
                let from = ray_axis_closest_point(ro0, rd0, op.pivot, a);
                let to = ray_axis_closest_point(ro1, rd1, op.pivot, a);
                to - from
            } else {
                let speed = camera.distance * 0.0015;
                let mut w = right * (d.x * speed) + up * (-d.y * speed);
                if let Some(ax) = axis {
                    let a = axis_dir(ax); // the axis in the active orientation
                    w = a * w.dot(a); // lock to that (possibly tilted) axis
                }
                w
            };
            // SNAP. Vertex/Surface (#71) REPLACE the moved origin absolutely (Blender's
            // snapObjectsTransform): they only apply to Move, and they re-aim `world` so
            // the PRIMARY element's origin lands on the snap target — the rest of the
            // selection rides the same delta (rigid). Grid/Increment (the default and
            // every Rotate/Scale) quantizes the committed DELTA instead.
            let primary_p0 = op.start.first().map(|(_, p, _)| *p);
            let snapped_absolute: Option<Vec3> = if snap_on {
                match op.snap.mode {
                    super::SnapMode::Vertex => primary_p0.map(|p0| {
                        let vp = camera.view_proj(aspect);
                        let exclude: Vec<usize> = op.start.iter().map(|(i, _, _)| *i).collect();
                        // Threshold the live CURSOR (not the projected origin) so the snap
                        // engages when the pointer is over a node, Blender-style. When
                        // nothing is in range, keep the un-quantized free `world`.
                        match nearest_origin_screen(scene, vp, op.viewport, cur, &exclude, 18.0) {
                            Some(target) => target - p0,
                            None => world,
                        }
                    }),
                    super::SnapMode::Surface => {
                        let (ro, rd) = ray_at(cur);
                        // Surface needs a hit AND a primary origin; otherwise fall through
                        // to the free `world` (no quantize) so the drag still moves.
                        match (primary_p0, pick_world_point(scene, ro, rd)) {
                            (Some(p0), Some(hit)) => Some(hit - p0),
                            _ => Some(world),
                        }
                    }
                    super::SnapMode::Increment => None,
                }
            } else {
                None
            };
            if let Some(w) = snapped_absolute {
                // Vertex/Surface already produced an absolute world delta — no grid
                // quantize on top (the snap target IS the destination).
                world = w;
            } else {
                // #4 Grid/Increment: quantize the committed delta (composes with the typed
                // path; `snapped_absolute` is None ⇒ snap is off OR mode == Increment).
                // For a world-axis (Global, unconstrained or axis-locked) we snap per world
                // component as before; for an ORIENTED axis lock the delta lies along a
                // tilted direction, so we snap its scalar magnitude along that axis
                // (Blender snaps in the constraint space) rather than per world component.
                match axis {
                    Some(ax) if op.orientation != super::TransformOrientation::Global => {
                        let dir = axis_dir(ax);
                        let mag = crate::ui::xform::quantize(world.dot(dir), op.snap.move_step);
                        if snap_on {
                            world = dir * mag;
                        }
                    }
                    _ => world = op.snap.snap_move(world, snap_on),
                }
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
            // Screens ride the identical Mat4 path as geometry.
            for (i, m0) in &op.screen_start {
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = Mat4::from_translation(world) * *m0;
                }
            }
            // Fog volumes: slide the centre (size unchanged).
            for (i, c0, _sz) in &op.env_start {
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = *c0 + world;
                }
            }
        }
        TransformKind::Rotate => {
            // Typed degrees override the mouse-derived angle (radians); #4 snaps the
            // committed angle to the rotate increment (e.g. nearest 15°).
            let angle = if typed { amount.to_radians() } else { d.x * 0.01 };
            let angle = op.snap.snap_angle(angle, snap_on);
            // Rotate about the active axis IN THE CHOSEN ORIENTATION: Local spins about
            // the element's own axis (a raked head tilts about its local pitch axis),
            // View about the camera axis. No lock → the orientation's Y. A grabbed
            // screen-axis VIEW ring (#72) overrides everything and spins about the live
            // camera forward (Blender's white trackball ring).
            let raxis = if op.gizmo_view && op.from_gizmo {
                camera.view_basis().2
            } else {
                axis.map(axis_dir).unwrap_or_else(|| basis * Vec3::Y)
            };
            let rot = Quat::from_axis_angle(raxis, angle);
            for (i, p0, q0) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    // Individual Origins (#5): each fixture spins about ITS OWN origin
                    // (p0), so its position is unchanged and only orientation turns;
                    // else everything orbits the shared pivot.
                    let pivot = if op.individual { *p0 } else { op.pivot };
                    f.position = pivot + rot * (*p0 - pivot);
                    f.orientation = rot * *q0;
                }
            }
            for (i, m0) in &op.geo_start {
                // Individual Origins: pivot = each piece's own world-bounds centre at
                // op start (read off m0); else the shared pivot.
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * Mat4::from_quat(rot)
                    * Mat4::from_translation(-pivot);
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = about * *m0;
                }
            }
            for (i, m0) in &op.screen_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * Mat4::from_quat(rot)
                    * Mat4::from_translation(-pivot);
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = about * *m0;
                }
            }
            for (i, c0, _sz) in &op.env_start {
                // Axis-aligned fog box: orbit the centre about the pivot (a no-op
                // for a lone box whose pivot IS its centre); the box stays
                // axis-aligned, so size is unchanged.
                let pivot = if op.individual { *c0 } else { op.pivot };
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = pivot + rot * (*c0 - pivot);
                }
            }
        }
        TransformKind::Scale => {
            // Typed 1 → ×1 (identity); clamp >0 (Blender NUM_NO_ZERO). #4 snaps the
            // committed factor to the scale increment (never to 0 → no collapse).
            let factor = if typed {
                amount.max(0.0001)
            } else {
                (1.0 + d.x * 0.005).max(0.01)
            };
            let factor = op.snap.snap_scale(factor, snap_on);
            for (i, p0, _q) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    // Individual Origins: scaling a point about ITSELF is a no-op, so
                    // a scattered fixture rig stays put (only geometry visibly grows).
                    let pivot = if op.individual { *p0 } else { op.pivot };
                    let off = *p0 - pivot;
                    let new = if let Some(ax) = op.axis {
                        // Scale only the locked axis IN THE CHOSEN ORIENTATION: decompose
                        // the offset along the (possibly tilted) axis direction.
                        let a = axis_dir(ax);
                        let comp = a * off.dot(a);
                        (off - comp) + comp * factor
                    } else {
                        off * factor
                    };
                    f.position = pivot + new;
                }
            }
            // Scale about the pivot in world space. Uniform → a plain scale matrix;
            // an axis lock builds a DIRECTIONAL scale `I + (factor-1)·d⊗d` so the
            // stretch follows the locked axis in the chosen orientation (a world-axis
            // direction reduces to the per-component scale this used to do).
            let scale_mat: Mat3 = match op.axis {
                Some(ax) => {
                    let d = axis_dir(ax);
                    Mat3::IDENTITY + (factor - 1.0) * Mat3::from_cols(d * d.x, d * d.y, d * d.z)
                }
                None => Mat3::from_diagonal(Vec3::splat(factor)),
            };
            let about4 = Mat4::from_mat3(scale_mat);
            for (i, m0) in &op.geo_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * about4
                    * Mat4::from_translation(-pivot);
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = about * *m0;
                }
            }
            for (i, m0) in &op.screen_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * about4
                    * Mat4::from_translation(-pivot);
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = about * *m0;
                }
            }
            for (i, c0, sz0) in &op.env_start {
                // Fog box: scale the centre about the pivot and grow the size by the
                // same (directional or uniform) factor — a lone box scales in place.
                let pivot = if op.individual { *c0 } else { op.pivot };
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = pivot + scale_mat * (*c0 - pivot);
                    e.size = (scale_mat * *sz0).abs();
                }
            }
        }
    }
}

/// The world-space translation of a geometry transform (its origin) — the
/// Individual-Origins pivot for static geometry. Uses the transform's own
/// translation (cheap, stable across the live drag; the AABB centre would drift as
/// the piece scales, which is wrong for an about-its-own-origin pivot).
#[inline]
fn geo_world_centre(m: Mat4) -> Vec3 {
    m.w_axis.truncate()
}

/// The net world translation the primary copy received during a duplicate-grab —
/// the spacing the array clone-count repeats along. Reads the primary element's
/// current origin minus its op-start snapshot (the move is a pure translation).
fn dup_grab_delta(op: &TransformOp, scene: &Scene) -> Vec3 {
    if let Some((i, p0, _)) = op.start.first() {
        scene.fixtures.get(*i).map(|f| f.position - *p0).unwrap_or(Vec3::ZERO)
    } else if let Some((i, m0)) = op.geo_start.first() {
        scene.geometry.get(*i).map(|g| (g.transform.w_axis - m0.w_axis).truncate()).unwrap_or(Vec3::ZERO)
    } else if let Some((i, m0)) = op.screen_start.first() {
        scene.screens.get(*i).map(|s| (s.transform.w_axis - m0.w_axis).truncate()).unwrap_or(Vec3::ZERO)
    } else if let Some((i, c0, _)) = op.env_start.first() {
        scene.environments.get(*i).map(|e| e.center - *c0).unwrap_or(Vec3::ZERO)
    } else {
        Vec3::ZERO
    }
}

/// Drop the last `count` objects of `kind`'s Vec — the live Shift+D array clones,
/// which are always appended at the END (LIFO), so a tail-truncate removes exactly
/// them (on shrink or cancel) without disturbing the base copies' indices.
fn truncate_objects(scene: &mut Scene, kind: ObjectRef, count: usize) {
    match kind {
        ObjectRef::Fixture(_) => {
            let l = scene.fixtures.len();
            scene.fixtures.truncate(l.saturating_sub(count));
        }
        ObjectRef::Geometry(_) => {
            let l = scene.geometry.len();
            scene.geometry.truncate(l.saturating_sub(count));
        }
        ObjectRef::Screen(_) => {
            let l = scene.screens.len();
            scene.screens.truncate(l.saturating_sub(count));
        }
        ObjectRef::Environment(_) => {
            let l = scene.environments.len();
            scene.environments.truncate(l.saturating_sub(count));
        }
    }
}

/// Place live array clone `k` (mirroring base copy `b`) at `base_home[b] + off` —
/// the base copy's op-start pose translated along the drag vector.
fn place_array_extra(scene: &mut Scene, op: &TransformOp, k: usize, b: usize, off: Vec3) {
    match op.dup_extra[k] {
        ObjectRef::Fixture(e) => {
            if let (Some((_, p0, q0)), Some(f)) = (op.start.get(b), scene.fixtures.get_mut(e)) {
                f.position = *p0 + off;
                f.orientation = *q0;
                f.snap_movement();
            }
        }
        ObjectRef::Geometry(e) => {
            if let (Some((_, m0)), Some(g)) = (op.geo_start.get(b), scene.geometry.get_mut(e)) {
                g.transform = Mat4::from_translation(off) * *m0;
            }
        }
        ObjectRef::Screen(e) => {
            if let (Some((_, m0)), Some(s)) = (op.screen_start.get(b), scene.screens.get_mut(e)) {
                s.transform = Mat4::from_translation(off) * *m0;
            }
        }
        ObjectRef::Environment(e) => {
            if let (Some((_, c0, sz0)), Some(env)) = (op.env_start.get(b), scene.environments.get_mut(e)) {
                env.center = *c0 + off;
                env.size = *sz0;
            }
        }
    }
}

/// Regenerate the LIVE Shift+D array each frame so the WHOLE array follows the
/// cursor while a clone-count is typed (not just one dragged copy). Grows/shrinks
/// the clone set to the typed count and repositions every clone along the current
/// drag vector. Clones live at the END of their kind's Vec (LIFO) so shrink/cancel
/// tail-truncates them. Skipped on the op's first frame (where the move's `before`
/// undo snapshot is captured — BEFORE any extras exist — so undo removes them).
fn update_dup_array(op: &mut TransformOp, scene: &mut Scene) {
    if !op.from_duplicate || op.dup_base.is_empty() {
        return;
    }
    let base_len = op.dup_base.len();
    let n = if op.num.active { (op.num.value().round() as i64).clamp(1, 1000) as u32 } else { 1 };
    let desired = (n.saturating_sub(1) as usize) * base_len; // count of EXTRA clones (#2..N)
    if op.dup_extra.len() > desired {
        let remove = op.dup_extra.len() - desired;
        truncate_objects(scene, op.dup_base[0], remove);
        op.dup_extra.truncate(desired);
    }
    while op.dup_extra.len() < desired {
        let b = op.dup_extra.len() % base_len;
        match scene.duplicate_object(op.dup_base[b]) {
            Some(new) => op.dup_extra.push(new),
            None => break,
        }
    }
    let delta = dup_grab_delta(op, scene);
    for k in 0..op.dup_extra.len() {
        let b = k % base_len;
        let i = 2 + (k / base_len); // array index (#1 is the base copy)
        place_array_extra(scene, op, k, b, delta * i as f32);
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
        if e.hidden {
            continue; // outliner eye: a hidden fog box isn't a measure/aim target
        }
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

/// Resolve where a newly-added object should land (#19 — place at cursor/camera,
/// not origin). Casts the viewport-centre ray (NDC `0,0`) into the scene and
/// returns its ground/surface hit (via [`pick_world_point`]); if that misses
/// (e.g. the camera looks at the sky), falls back to a point a sensible distance
/// in front of the camera; if even that degenerates, the origin. The whole
/// add+place is wrapped in ONE undo op by the caller. We use the view-centre ray
/// (not the mouse) because both add entry points — the Library "Add" button and
/// the Shift+A menu — fire from outside the viewport draw, where the live mouse
/// position relative to the viewport rect isn't reachable; the framed centre is
/// the stable, predictable "where I'm looking" anchor Unreal's PlacementMode uses.
pub fn placement_point(scene: &Scene, camera: &OrbitCamera) -> Vec3 {
    let aspect = camera.last_aspect;
    let (ro, rd) = camera.ray(Vec2::ZERO, aspect);
    if let Some(p) = pick_world_point(scene, ro, rd) {
        return p;
    }
    // Ground/surface miss → place in front of the camera at the orbit distance.
    let front = ro + rd * camera.distance.max(1.0);
    if front.is_finite() {
        front
    } else {
        Vec3::ZERO
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
    // Viewport HEADER (`ui::editor`) now, not from the viewport body (§2.2). The
    // one setting the body still READS is the internal render scale (it sizes the
    // offscreen target — merged in from the perf-overlay branch); passed by value.
    render_scale: f32,
    transform: &mut Option<TransformOp>,
    delete_requested: &mut bool,
    replace_requested: &mut bool,
    // Set when Enter is pressed with the viewport focused + no live transform: the
    // caller adds the Library tab's highlighted item (mirrors Enter in the Library).
    add_requested: &mut bool,
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
    // Transform-tool options (§2.4 #4/#5): grid/increment snap + pivot-point mode.
    // Read when building a TransformOp (gizmo + modal G/R/S) so the snap policy and
    // pivot are baked into the op; the header/N-panel write them.
    xform: TransformPrefs,
    // The world 3D-cursor point — the PivotMode::Cursor3d pivot (§2.4 #5). Drawn as a
    // small red/white crosshair-ring; repositioned by Shift-right-click (S1-3d-cursor).
    cursor_3d: &mut Vec3,
    // Set true when a Shift-RMB places the cursor this frame, so the caller's Add menu
    // can drop new objects AT the cursor (the "set this session" gate).
    cursor_3d_set: &mut bool,
    // The Measure tool's two-point ruler state (§2.4). Persists across frames so a
    // completed measurement stays drawn; only the Measure tool reads/writes it.
    measure: &mut MeasureState,
    // The Aim tool's in-flight drag state (§2.4). Holds the world target while a drag
    // aims the selected heads; only the Aim tool reads/writes it.
    aim: &mut AimState,
) {
    *transform_started = false;
    *transform_finished = false;
    // The active keymap overrides for this frame (published by `Ui::show`). This
    // free function's signature is fixed by its app.rs caller, so it reads the
    // process-wide snapshot instead of taking `&KeymapOverrides`. EMPTY by default
    // ⇒ the poll sites below behave exactly as the static defaults.
    let ov = shortcuts::active();
    let available = ui.available_size();
    let ppp = ui.pixels_per_point();

    // Internal render scale: render the offscreen targets below native and let the
    // egui image() draw upscale them (the LDR view is FilterMode::Linear). The single
    // biggest fps lever on Retina — every per-pixel pass (forward, SSAO, volumetric,
    // post) scales with scale². Snapped to 0.05 so a slider drag doesn't reallocate
    // targets every sub-pixel step (Viewport::resize early-returns on unchanged size).
    let scale = (render_scale.clamp(0.25, 1.0) * 20.0).round() / 20.0;
    *requested_px = (
        (available.x * ppp * scale).round().max(1.0) as u32,
        (available.y * ppp * scale).round().max(1.0) as u32,
    );

    let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
    // Record the live viewport aspect so frame-selected widens its fit radius for
    // wide viewports (the aspect-correction rule in OrbitCamera::frame_aabb).
    camera.set_aspect(rect.width() / rect.height().max(1.0));
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

    // --- 3D cursor (§2.4 #5) -------------------------------------------------
    // The world cursor point — the PivotMode::Cursor3d pivot. Drawn always (like
    // Blender's 3D cursor) as a small red/white crosshair-ring so the user can see
    // where a Cursor-pivot transform will spin/grow about. Read-only here (movable
    // is a follow-up); behind every interactive overlay so it never eats clicks.
    if let Some(sc) = OrbitCamera::project_to_screen(*cursor_3d, camera.view_proj(aspect), rect) {
        let p = ui.painter_at(rect);
        let r = 7.0;
        // Dashed-look crosshair: two short ticks per arm, in cursor red + white.
        let red = egui::Color32::from_rgb(230, 70, 70);
        let white = egui::Color32::from_rgb(235, 235, 235);
        p.circle_stroke(sc, r, egui::Stroke::new(1.0, red));
        for (i, (dx, dy)) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)].iter().enumerate() {
            let col = if i % 2 == 0 { red } else { white };
            let dir = egui::vec2(*dx, *dy);
            p.line_segment([sc + dir * (r - 1.0), sc + dir * (r + 5.0)], egui::Stroke::new(1.0, col));
        }
    }

    // --- Interactive transform-gizmo group (§2.4 GizmoGroup) ---
    // The ACTIVE TOOL selects which gizmo draws at the selection pivot: Move→arrows,
    // Rotate→rings, Scale→boxes (gizmo::for_tool is the single tool→group map; Select
    // and the non-transform tools return None → plain click-select). Each group
    // hit-tests its handles in screen space; grabbing one (a press within a few px)
    // starts the matching axis-locked TransformOp — reusing apply_transform via
    // `gizmo_hovered_axis`/`axis` so all three share the live-apply + undo path. The
    // grab is checked BEFORE orbit/select so dragging a handle never orbits the
    // camera; an empty-space press falls through untouched.
    // Shift+D grab-duplicate hand-off: `Ui::show` already cloned + re-selected the
    // copies (their own undo step) and set this flag. Pick it up and IMMEDIATELY
    // start a Move grab on the copies so they follow the cursor and commit on
    // click / Enter (Esc leaves them at the source, like Blender). A typed number
    // during the grab becomes the array clone-count (see the confirm path).
    let dupgrab_start = ui.ctx().data_mut(|d| {
        let id = egui::Id::new("previz.dupgrab.start");
        let v = d.get_temp::<bool>(id).unwrap_or(false);
        if v {
            d.remove::<bool>(id);
        }
        v
    });
    if dupgrab_start
        && transform.is_none()
        && selection.has_object()
        && let Some(cur) = ui.input(|i| i.pointer.latest_pos())
    {
        let fids: Vec<usize> =
            selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
        let gids: Vec<usize> =
            selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
        let sids: Vec<usize> =
            selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
        let eids: Vec<usize> =
            selection.environment.into_iter().filter(|&i| i < scene.environments.len()).collect();
        let objs = obj_refs(&fids, &gids, &sids, &eids);
        if !objs.is_empty() {
            let pivot = compute_pivot(scene, &objs, xform.pivot, *cursor_3d);
            let (start, geo_start, screen_start, env_start) =
                snapshot_starts(scene, &fids, &gids, &sids, &eids);
            *transform = Some(TransformOp {
                kind: TransformKind::Move,
                axis: None,
                start_screen: cur,
                viewport: rect,
                pivot,
                start,
                geo_start,
                screen_start,
                env_start,
                gizmo_hovered_axis: None,
                gizmo_plane_normal: None,
                gizmo_view: false,
                from_gizmo: false,
                num: NumInput::default(),
                individual: xform.pivot.is_individual(),
                snap: xform.snap,
                orientation: xform.orientation,
                from_duplicate: true,
                dup_base: objs,
                dup_extra: Vec::new(),
            });
            *transform_started = true;
        }
    }

    // The gizmo's projected screen centre (set when it draws) — the selection-label
    // pill below reads it so it can DODGE the handles instead of overlapping them.
    let mut gizmo_screen: Option<egui::Pos2> = None;
    let gizmo_targets: bool = active_tool.shows_xform_gizmo()
        && transform.is_none()
        && *viewport_focused
        && !selection.world
        && selection.has_object();
    if gizmo_targets
        && let Some(group) = gizmo::for_tool(active_tool)
    {
        // #5: the gizmo draws at the mode-resolved pivot (Median centroid / Active /
        // 3D-cursor; Individual Origins also draws at the median, matching Blender).
        let fids: Vec<usize> =
            selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
        let gids: Vec<usize> =
            selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
        let sids: Vec<usize> =
            selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
        let eids: Vec<usize> =
            selection.environment.into_iter().filter(|&i| i < scene.environments.len()).collect();
        let objs = obj_refs(&fids, &gids, &sids, &eids);
        if !objs.is_empty() {
            let gizmo_pivot = compute_pivot(scene, &objs, xform.pivot, *cursor_3d);
            gizmo_screen = OrbitCamera::project_to_screen(gizmo_pivot, camera.view_proj(aspect), rect);
            let cx = GizmoCtx {
                pivot: gizmo_pivot,
                vp: camera.view_proj(aspect),
                rect,
                // Arm/ring size scales with camera distance so handles stay a
                // readable pixel size regardless of zoom.
                arm: (camera.distance * 0.18).clamp(0.4, 4.0),
                // Camera forward = the VIEW axis (#72): screen-axis rotate ring +
                // view-plane move centre resolve from this.
                forward: camera.view_basis().2,
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
                // `fids`/`gids`/`sids`/`eids` are the validated, selection-order
                // indices computed above for the pivot — reused here for the
                // per-element snapshots (every kind, via `snapshot_starts`).
                let (start, geo_start, screen_start, env_start) =
                    snapshot_starts(scene, &fids, &gids, &sids, &eids);
                *transform = Some(TransformOp {
                    kind: start_spec.kind,
                    // Move locks via `gizmo_hovered_axis` (matching P3a); rotate/scale
                    // carry their axis in `axis` (apply_transform reads it directly,
                    // and the uniform-scale centre yields None = scale all axes).
                    axis: if start_spec.kind == TransformKind::Move { None } else { start_spec.axis },
                    start_screen: cur,
                    viewport: rect,
                    pivot: cx.pivot,
                    start,
                    geo_start,
                    screen_start,
                    env_start,
                    gizmo_hovered_axis: if start_spec.kind == TransformKind::Move {
                        start_spec.axis
                    } else {
                        None
                    },
                    // A grabbed PLANE quad drives the two-axis ray_plane_point drag
                    // (the normal is the held axis). Move-only; mutually exclusive
                    // with the single-axis `gizmo_hovered_axis` lock above.
                    gizmo_plane_normal: start_spec.plane_normal,
                    // A grabbed VIEW handle (#72): screen-plane Move / view-axis
                    // Rotate, resolved from the live camera basis in apply_transform.
                    gizmo_view: start_spec.view,
                    from_gizmo: true,
                    num: NumInput::default(),
                    individual: xform.pivot.is_individual(),
                    snap: xform.snap,
                    orientation: xform.orientation,
                    from_duplicate: false,
                    dup_base: Vec::new(), dup_extra: Vec::new(),
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
        if shortcuts::poll_modal(ui.ctx(), &ov).contains(&shortcuts::ModalAction::Cancel) {
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
        let modal = shortcuts::poll_modal(ui.ctx(), &ov);
        // --- Modal numeric input (Blender editors/util/numinput.cc) ---
        // Typed digits/'.' OVERRIDE the mouse; '-' toggles sign; Backspace edits
        // and, when it empties the buffer, hands control back to the mouse. Read
        // Event::Text for locale-correct digits + accept Key::Period/Comma as '.'
        // (numpad-period) and Key::Minus for the sign toggle. This block lives
        // INSIDE `if let Some(op)` — the modal op owns the viewport, so no text
        // field can be focused (LOCKED DECISION 5 scope guard).
        ui.input(|i| {
            for ev in &i.events {
                if let egui::Event::Text(t) = ev {
                    for c in t.chars() {
                        if c.is_ascii_digit() {
                            if op.num.str.len() < 16 {
                                op.num.str.push(c);
                                op.num.active = true;
                            }
                        } else if c == '.' && !op.num.str.contains('.') {
                            op.num.str.push('.');
                            op.num.active = true;
                        }
                        // '-' is NOT inserted here (handled as a sign toggle below).
                    }
                }
            }
            // Numpad '.' may arrive as a key, not Text — accept Period/Comma too.
            if (i.key_pressed(egui::Key::Period) || i.key_pressed(egui::Key::Comma))
                && !op.num.str.contains('.')
            {
                op.num.str.push('.');
                op.num.active = true;
            }
            // '-' toggles the sign (Blender NUM_NEGATE); it activates numinput too.
            if i.key_pressed(egui::Key::Minus) {
                op.num.sign = !op.num.sign;
                op.num.active = true;
            }
            if i.key_pressed(egui::Key::Backspace) {
                op.num.str.pop();
                if op.num.str.is_empty() && !op.num.sign {
                    op.num.active = false; // empty → mouse takes over again
                }
            }
        });
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
            for (i, m0) in &op.screen_start {
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = *m0;
                }
            }
            for (i, c0, sz0) in &op.env_start {
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = *c0;
                    e.size = *sz0;
                }
            }
            // Drop the live duplicate-array clones — cancelling a Shift+D grab leaves
            // only the base copies at the source (Blender). The base clone was its own
            // undo step (it stands; one undo removes it).
            if op.from_duplicate
                && !op.dup_extra.is_empty()
                && let Some(&kind) = op.dup_base.first()
            {
                truncate_objects(scene, kind, op.dup_extra.len());
            }
            *transform = None;
        } else {
            if let Some(cur) = ui.input(|i| i.pointer.latest_pos()) {
                // #4: snap is `op.snap.on` INVERTED while Ctrl/⌘ is held (Blender's
                // Ctrl-toggles-snap, Unreal's transient grid gate) — resolved live
                // each frame so a mid-drag Ctrl tap flips quantization on/off.
                let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                let snap_on = op.snap.on ^ ctrl;
                apply_transform(op, scene, camera, cur, snap_on);
                // #70: mark the snapped DESTINATION while the Move is live (pre-release)
                // so the user sees where the origin will land. Drawn after the apply, so
                // the primary element's post-snap origin is the marker point.
                if let Some(target) = snap_preview_point(op, scene, snap_on) {
                    draw_snap_marker(&ui.painter_at(rect), target, camera.view_proj(aspect), rect);
                }
            }
            // LIVE duplicate-array: after the base copies moved, (re)build the array
            // clones so the WHOLE array follows the cursor while a count is typed (not
            // just one dragged copy). Skipped on the op's first frame, where the move's
            // `before` undo snapshot is taken before any extras exist.
            if op.from_duplicate && !*transform_started {
                update_dup_array(op, scene);
            }
            // The key cluster (X/Y/Z · type number · Enter/Esc) is read LIVE from
            // the keymap so the pill can never drift from the binds (#23). When an
            // axis is locked the pill tints to that axis's colour so the constraint
            // is unmistakable; otherwise the neutral amber.
            if prefs.show_hint {
                let hint = op.hint(&shortcuts::modal_hint_keys());
                let tint = op
                    .active_axis()
                    .map(|a| {
                        let [r, g, b] = a.color();
                        egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
                    })
                    .unwrap_or(egui::Color32::from_rgb(255, 220, 120));
                theme::overlay_label(
                    &ui.painter_at(rect),
                    rect.center_top() + egui::vec2(0.0, 10.0),
                    egui::Align2::CENTER_TOP,
                    &hint,
                    Some(tint),
                );
            }
            if confirm {
                // The array is already LIVE (rebuilt each frame above); committing just
                // selects the whole result (base copies + array clones) so the user can
                // keep editing them as a unit.
                if op.from_duplicate {
                    let mut all = op.dup_base.clone();
                    all.extend(op.dup_extra.iter().copied());
                    *selection = Selection::from_object_refs(&all);
                }
                *transform = None;
                *transform_finished = true;
            }
        }
    } else if *viewport_focused && selection.has_object() {
        // Start a transform on G / R / S (over any placed object — fixtures,
        // static geometry, LED screens or the environment volume).
        // The binds (and their modifier guards — plain R only, since Shift+R is
        // "Replace") live in the central registry under the Viewport context.
        // Viewport context active, no transform in flight (this is the `else`
        // branch): the keymap stack resolves plain `S` to Scale (masking the Global
        // quick-select) and `R` to Rotate (Shift+R = Replace stays in Global).
        let kind = shortcuts::poll(
            ui.ctx(),
            shortcuts::ActiveContext { viewport_focused: true, transform_active: false },
            &ov,
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
            let sids: Vec<usize> =
                selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
            let eids: Vec<usize> =
                selection.environment.into_iter().filter(|&i| i < scene.environments.len()).collect();
            let objs = obj_refs(&fids, &gids, &sids, &eids);
            if !objs.is_empty() {
                // #5: pivot per the chosen mode (Median / Active / 3D-Cursor; the
                // Individual flag makes apply_transform pivot each element about its
                // own origin). Per-element snapshots for the live re-apply / cancel.
                let pivot = compute_pivot(scene, &objs, xform.pivot, *cursor_3d);
                let (start, geo_start, screen_start, env_start) =
                    snapshot_starts(scene, &fids, &gids, &sids, &eids);
                *transform = Some(TransformOp {
                    kind,
                    axis: None,
                    start_screen: cur,
                    viewport: rect,
                    pivot,
                    start,
                    geo_start,
                    screen_start,
                    env_start,
                    gizmo_hovered_axis: None,
                    gizmo_plane_normal: None,
                    gizmo_view: false,
                    from_gizmo: false,
                    num: NumInput::default(),
                    individual: xform.pivot.is_individual(),
                    snap: xform.snap,
                    orientation: xform.orientation,
                    from_duplicate: false,
                    dup_base: Vec::new(), dup_extra: Vec::new(),
                });
                *transform_started = true;
                consumed = true;
            }
        }
    }

    // --- Box / marquee select (#25) ------------------------------------------
    // ORBIT-vs-BOX RULE (matches Blender's Box-Select tool + UE marquee, adapted
    // to our LMB-orbit nav): under the SELECT tool, a left-drag that BEGAN over
    // EMPTY space (no gizmo handle — Select shows none — and nothing `pick()`s
    // under the press origin) rubber-bands a marquee; a drag that began over an
    // object, or a drag under ANY other tool, stays plain orbit/pan. So orbit is
    // always reachable: switch off the Select tool, or start the drag on an
    // object. The box-active decision is latched on drag-start (egui temp memory,
    // keyed by the viewport id) so mid-drag the cursor leaving empty space can't
    // flip it back to orbit. Modifiers: plain = Replace, Shift = Add, ⌘/Ctrl =
    // Subtract — ONE undo-free selection change committed on release.
    // Latched across the drag's frames in egui temp memory (keyed by viewport id):
    // the marquee anchor (press origin) once box-select has begun. `None` = not
    // currently marqueeing. Stashing the anchor here makes the release computation
    // independent of egui's `press_origin()` lifetime (cleared at button-up).
    let box_anchor_id = ui.id().with("viewport_box_anchor");
    let mut box_anchor: Option<egui::Pos2> = ui.data(|d| d.get_temp(box_anchor_id));
    if !consumed
        && active_tool == ActiveTool::Select
        && transform.is_none()
        && *viewport_focused
    {
        if response.drag_started()
            && let Some(press) = ui.input(|i| i.pointer.press_origin())
        {
            // Box only when the press landed on empty space; an object under the
            // press leaves the drag to orbit (no tweak-move under Select yet).
            let uv = (press - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
            let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let (ro, rd) = camera.ray(ndc, aspect);
            box_anchor = (rect.contains(press) && pick(scene, ro, rd).is_none()).then_some(press);
            ui.data_mut(|d| d.insert_temp(box_anchor_id, box_anchor));
        }
        if let Some(anchor) = box_anchor {
            let cur = ui.input(|i| i.pointer.latest_pos()).unwrap_or(anchor);
            let marquee = egui::Rect::from_two_pos(anchor, cur);
            if response.dragged() {
                // Draw the rubber-band from the anchor to the live cursor.
                let accent = theme::Palette::get(ui).accent;
                let painter = ui.painter_at(rect);
                painter.rect_filled(marquee, 0.0, accent.gamma_multiply(0.10));
                painter.rect_stroke(
                    marquee,
                    0.0,
                    egui::Stroke::new(1.0, accent),
                    egui::StrokeKind::Inside,
                );
                consumed = true; // suppress orbit/pan while marqueeing
            }
            if response.drag_stopped() {
                // Ignore a sub-pixel "drag" (really a click) — the click handler
                // below owns single-pick selection.
                if marquee.width() > 2.0 || marquee.height() > 2.0 {
                    let m = ui.input(|i| i.modifiers);
                    // Modifier → op (UE/CAD): plain = Replace, Shift = Add,
                    // ⌘/Ctrl = Subtract — ONE undo-free selection change.
                    let op = if m.command || m.ctrl {
                        SelectOp::Subtract
                    } else if m.shift {
                        SelectOp::Add
                    } else {
                        SelectOp::Replace
                    };
                    let vp = camera.view_proj(aspect);
                    let hits = marquee_hits(scene, vp, rect, marquee);
                    *selection = apply_select(selection, &hits, op);
                    *scene_anchor = None;
                    consumed = true; // don't also fire click-select this frame
                }
                ui.data_mut(|d| d.remove_temp::<egui::Pos2>(box_anchor_id));
            }
        }
    }

    // --- Navigation axis gizmo (#35) -----------------------------------------
    // Blender's corner orientation gizmo: six labelled axis balls oriented live by
    // the camera (a readout of which way the world axes point) that double as click
    // targets — clicking a ball snaps the camera to look down that axis (eased
    // `set_view`). Drawn top-right with the egui painter (no extra render pass);
    // hover highlights the ball under the pointer. Hit-tested BEFORE orbit so a
    // click on the cluster snaps the view instead of starting an orbit drag.
    // Suppressed (drawn AND click-handled) when the Gizmos overlay is off.
    if prefs.show_gizmos {
        // Cluster centre: top-right corner, inset by its radius (+ a little margin)
        // and tucked below the active-selection label that lives up there.
        // Align the cluster's RIGHT edge with the selection-label pill above it: the
        // pill sits at `rect.right - 10`, and the rightmost BALL reaches
        // `center.x + GIZMO_RADIUS + BALL_RADIUS`, so inset by R + BALL_RADIUS + 10.
        // Dropped further down (R + 46) so the balls clear the pill's bottom.
        let center = rect.right_top()
            + egui::vec2(
                -(nav_gizmo::GIZMO_RADIUS + nav_gizmo::BALL_RADIUS + 10.0),
                nav_gizmo::GIZMO_RADIUS + 46.0,
            );
        let balls = nav_gizmo::balls(camera, center);
        let hover_pos = ui.input(|i| i.pointer.hover_pos());
        let hovered = hover_pos.and_then(|p| nav_gizmo::hit_test(&balls, p));
        let painter = ui.painter_at(rect);
        // Faint backing disc so the cluster reads against any scene.
        painter.circle_filled(center, nav_gizmo::GIZMO_RADIUS + 6.0, egui::Color32::from_black_alpha(70));
        // Draw far balls first so near ones overlap them (painter order = depth).
        let mut order: Vec<usize> = (0..balls.len()).collect();
        order.sort_by(|&a, &b| balls[a].depth.partial_cmp(&balls[b].depth).unwrap_or(std::cmp::Ordering::Equal));
        for &i in &order {
            let b = balls[i];
            let [r, g, bl] = b.color;
            let base = egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (bl * 255.0) as u8);
            let hot = hovered == Some(b.view);
            // Connecting arm from centre to the ball.
            painter.line_segment(
                [center, b.pos],
                egui::Stroke::new(1.0, base.gamma_multiply(if b.depth >= 0.0 { 0.6 } else { 0.3 })),
            );
            if b.positive || hot {
                // Positive (and any hovered) balls are solid + labelled.
                let col = if hot { egui::Color32::WHITE } else { base };
                painter.circle_filled(b.pos, nav_gizmo::BALL_RADIUS, base);
                if hot {
                    painter.circle_stroke(b.pos, nav_gizmo::BALL_RADIUS, egui::Stroke::new(1.5, col));
                }
                painter.text(
                    b.pos,
                    egui::Align2::CENTER_CENTER,
                    b.label,
                    egui::FontId::monospace(10.0),
                    egui::Color32::from_gray(20),
                );
            } else {
                // Negative balls are hollow rings (so the cluster reads as a sphere).
                painter.circle_stroke(b.pos, nav_gizmo::BALL_RADIUS - 1.0, egui::Stroke::new(1.5, base.gamma_multiply(0.8)));
            }
        }
        // A click on a ball snaps the view + consumes the press (no orbit).
        if !consumed
            && response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(view) = nav_gizmo::hit_test(&balls, pos)
        {
            camera.set_view(view);
            consumed = true;
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
            // Zoom-to-cursor: anchor the dolly on the world point under the
            // pointer. Prefer the nearest picked surface; fall back to where the
            // cursor ray meets the ground plane (y=0). If neither resolves (cursor
            // off into the sky), pass None → plain dolly toward the target.
            let aspect = rect.width() / rect.height().max(1.0);
            let anchor = ui.input(|i| i.pointer.hover_pos()).and_then(|pos| {
                let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
                let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
                let (ro, rd) = camera.ray(ndc, aspect);
                pick_world_point(scene, ro, rd)
            });
            camera.zoom(scroll * 0.01, anchor);
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
        let hit = pick(scene, ro, rd);
        // Fixtures keep the anchor-based Shift = inclusive-range click (it needs
        // the shared `scene_anchor` the pure SelectOp has no notion of); every
        // other case routes through the ONE `apply_select` truth table (#24).
        if let Some(Hit::Fixture(i)) = hit {
            apply_fixture_click(selection, scene_anchor, i, m.shift, toggle, scene.fixtures.len());
        } else {
            // Modifier → op: plain = replace, ⌘/Ctrl = toggle, Shift = add.
            let op = if toggle {
                SelectOp::Toggle
            } else if m.shift {
                SelectOp::Add
            } else {
                SelectOp::Replace
            };
            let hits: &[SelItem] = match hit {
                Some(Hit::Geometry(i)) => &[SelItem::Geometry(i)],
                Some(Hit::Screen(i)) => &[SelItem::Screen(i)],
                Some(Hit::Environment(i)) => &[SelItem::Environment(i)],
                Some(Hit::Fixture(_)) => unreachable!("fixture handled above"),
                None => &[],
            };
            *selection = apply_select(selection, hits, op);
            *scene_anchor = None;
        }
    }

    // --- 3D cursor place (Shift + right-click, S1-3d-cursor) -----------------
    // Blender's Shift-RMB world cursor: a Shift+right-click drops the 3D cursor onto
    // the ray's world hit (geometry / ground via `pick_world_point`, falling back to a
    // point in front of the camera when the ray escapes to the sky). Handled BEFORE
    // the plain right-click block so it sets the cursor instead of opening the context
    // menu, and it consumes the click so neither the menu nor select fires this frame.
    let shift_rclick = !consumed
        && response.secondary_clicked()
        && ui.input(|i| i.modifiers.shift)
        && *viewport_focused;
    if shift_rclick
        && let Some(pos) = response.interact_pointer_pos()
    {
        let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let (ro, rd) = camera.ray(ndc, aspect);
        // Ground/geometry hit, else a sensible point in front of the camera so the
        // cursor still lands somewhere visible when the ray misses the world.
        let p = pick_world_point(scene, ro, rd).unwrap_or_else(|| ro + rd * camera.distance.max(1.0));
        if p.is_finite() {
            *cursor_3d = p;
            *cursor_3d_set = true;
        }
        consumed = true; // never open the context menu / select on a cursor place
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
                        *duplicate = Some(super::duplicate_dialog_for(ui.ctx(), idx));
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
        &ov,
    )
    .iter()
    .any(|a| matches!(a, shortcuts::Action::Duplicate));
    if !consumed
        && *viewport_focused
        && duplicate.is_none()
        && dup_pressed
        && let Some(idx) = selection.primary_fixture()
    {
        *duplicate = Some(super::duplicate_dialog_for(ui.ctx(), idx));
    }

    // Enter (viewport focused, no live transform / dialog): add the Library tab's
    // highlighted item — pressing Enter in the viewport mirrors pressing it in the
    // Library pane. The modal-transform Enter (G/R/S confirm) is decoded only inside
    // the `Some(op)` branch above, and `transform.is_none()` here guards it, so this
    // can never steal a confirm. The actual add (key→row resolve + undo step) runs in
    // `Ui::show` via `add_requested`.
    if !consumed
        && *viewport_focused
        && transform.is_none()
        && duplicate.is_none()
        && !super::text_focus_active(ui.ctx())
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        *add_requested = true;
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
            "drag: orbit · shift+drag: pan · scroll: zoom · click: select · shift+rmb: 3d cursor · g/r/s: move/rotate/scale · d: duplicate"
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
        // Drawn AFTER the move gizmo (above) so it sits on top; with a centred
        // object the gizmo handles can reach this corner, so the pill gets an
        // OPAQUE rounded background (not the shared translucent overlay) and stays
        // firmly in the top-right with padding — readable, never visually fighting
        // the handles. The gizmo's interaction is untouched (paint-only).
        let painter = ui.painter_at(rect);
        let fg = egui::Color32::from_gray(238);
        let font = egui::FontId::proportional(12.5);
        let galley = painter.layout_no_wrap(text.clone(), font, fg);
        let pad = egui::vec2(9.0, 5.0);
        let size = galley.size() + pad * 2.0;
        let anchor = rect.right_top() + egui::vec2(-10.0, 10.0);
        let mut bg = egui::Align2::RIGHT_TOP.anchor_size(anchor, size);
        // Dodge the move gizmo: if its handles reach the pill's corner, drop the pill
        // to just below them (the gizmo owns the object, so the label yields). Only
        // when there's room below — otherwise the opaque pill stays put + readable.
        if let Some(g) = gizmo_screen {
            const GIZMO_R: f32 = 60.0; // generous on-screen handle reach
            let near = egui::Rect::from_center_size(g, egui::vec2(GIZMO_R * 2.0, GIZMO_R * 2.0));
            let drop_to = g.y + GIZMO_R + 8.0;
            if bg.expand(4.0).intersects(near) && drop_to + size.y < rect.bottom() {
                bg = bg.translate(egui::vec2(0.0, drop_to - bg.top()));
            }
        }
        // Opaque fill + a hairline so it reads as a solid chip over the gizmo.
        painter.rect_filled(bg, 5.0, egui::Color32::from_rgb(26, 27, 31));
        painter.rect_stroke(
            bg,
            5.0,
            egui::Stroke::new(1.0, egui::Color32::from_black_alpha(120)),
            egui::StrokeKind::Inside,
        );
        painter.galley(bg.min + pad, galley, fg);
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
            // Selected label takes the accent; others sit quiet over the canvas.
            let col = if selected {
                theme::Palette::get(ui).accent
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

    // FPS HUD (top-left), color-coded off the semantic status tokens.
    if prefs.show_fps {
        let pal = theme::Palette::get(ui);
        let color = if fps >= 55.0 {
            pal.ok
        } else if fps >= 30.0 {
            pal.warn
        } else {
            pal.conflict
        };
        ui.painter().text(
            rect.left_top() + egui::vec2(8.0, 6.0),
            egui::Align2::LEFT_TOP,
            format!("{fps:.0} fps"),
            egui::FontId::monospace(13.0),
            color,
        );
    }

    // Projection + axis-view tag (Blender's top-left "User Perspective" gizmo
    // text). Sits just below the FPS HUD when that's shown, else at the top edge.
    {
        let y = if prefs.show_fps { 24.0 } else { 6.0 };
        ui.painter().text(
            rect.left_top() + egui::vec2(8.0, y),
            egui::Align2::LEFT_TOP,
            camera.view_tag(),
            egui::FontId::monospace(12.0),
            theme::ink(!ui.visuals().dark_mode).tertiary,
        );
    }

    // Scene STATISTICS overlay (Blender's bottom-left stats readout): a quiet,
    // monospace count of what's in the scene + the live selection. Off by default
    // (opt-in via View > Overlays > Statistics); kept faint so it never competes
    // with the canvas. Bottom-left so it clears the top-left FPS/view-tag stack.
    if prefs.show_stats {
        let selected = selection.fixtures.len()
            + selection.geometry.len()
            + selection.screens.len()
            + usize::from(selection.environment.is_some());
        let lines = stats_lines(
            scene.fixtures.len(),
            scene.geometry.len(),
            scene.screens.len(),
            scene.environments.len(),
            selected,
        );
        let painter = ui.painter_at(rect);
        let col = theme::ink(!ui.visuals().dark_mode).tertiary;
        let line_h = 14.0;
        // Anchor from the bottom edge up, so the block grows upward and the first
        // line always sits at a fixed bottom inset regardless of line count.
        let mut y = rect.bottom() - 8.0 - line_h * (lines.len() as f32 - 1.0);
        for line in &lines {
            painter.text(
                egui::pos2(rect.left() + 8.0, y),
                egui::Align2::LEFT_TOP,
                line,
                egui::FontId::monospace(11.0),
                col,
            );
            y += line_h;
        }
    }

    // The display Mode + Exposure controls (and the Grid / Beam-gizmo toggles)
    // now live in the per-editor Viewport HEADER (`ui::editor`), migrated off the
    // old floating "viewport-display-overlay" Area (§2.2). Advanced look settings
    // (bloom/beam/steps) stay in Preferences.
}

/// Build the quiet scene-statistics overlay text lines from the scene counts +
/// the live selection count. A category line is emitted ONLY when its count is
/// non-zero (so an empty scene shows nothing but "0 selected" stays useful), and
/// the trailing "selected" line is always present. Pure (no egui) so the count
/// logic is unit-testable. Order mirrors the outliner: fixtures, objects,
/// screens, environments, then selected.
pub(super) fn stats_lines(
    fixtures: usize,
    objects: usize,
    screens: usize,
    environments: usize,
    selected: usize,
) -> Vec<String> {
    // Singular/plural label for a count (e.g. "1 fixture" / "3 fixtures").
    let row = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut lines = Vec::new();
    if fixtures > 0 {
        lines.push(row(fixtures, "fixture", "fixtures"));
    }
    if objects > 0 {
        lines.push(row(objects, "object", "objects"));
    }
    if screens > 0 {
        lines.push(row(screens, "screen", "screens"));
    }
    if environments > 0 {
        lines.push(row(environments, "environment", "environments"));
    }
    lines.push(format!("{selected} selected"));
    lines
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
            super::status_dot(ui, theme::OK, &format!("{bound} · {} source(s)", status.sources.len()));
        } else {
            super::status_dot(ui, theme::IDLE, "stopped");
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
                    .text_color_opt(valid.is_err().then_some(theme::CONFLICT)),
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
                        theme::OK
                    } else if s.fps >= 10.0 {
                        theme::WARN
                    } else {
                        theme::CONFLICT
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
                ui.colored_label(theme::OK, format!("• {n} src"));
            } else {
                ui.colored_label(ink.muted, "• idle");
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
        if e.hidden {
            continue; // outliner eye: a hidden fog box isn't pickable
        }
        if let Some(t) = ray_aabb(ro, rd, e.min(), e.max())
            && env.is_none_or(|(bt, _)| t < bt)
        {
            env = Some((t, i));
        }
    }
    env.map(|(_, i)| Hit::Environment(i))
}

/// Gather every visible fixture / geometry object / LED screen whose
/// screen-projected anchor point falls inside the marquee `marquee` (#25). The
/// rule is **loose** (Blender's default: an object is hit if its *centre* lands
/// in the rect — fixtures use their origin, geometry/screens their world-bounds
/// centre), which is forgiving for the many tiny fixture dots in a rig. Hidden
/// and behind-camera entities are skipped (`project_to_screen` returns `None`
/// when `w <= 0`). Environments are excluded — they're single-only volumes the
/// marquee shouldn't sweep up. Pure given `vp`/`rect`, so it's unit-testable.
fn marquee_hits(scene: &Scene, vp: glam::Mat4, rect: egui::Rect, marquee: egui::Rect) -> Vec<SelItem> {
    let mut hits = Vec::new();
    let inside = |p: Vec3| {
        OrbitCamera::project_to_screen(p, vp, rect)
            .is_some_and(|s| marquee.contains(s))
    };
    for (i, f) in scene.fixtures.iter().enumerate() {
        if !f.hidden && inside(f.position) {
            hits.push(SelItem::Fixture(i));
        }
    }
    for (i, g) in scene.geometry.iter().enumerate() {
        let c = g
            .world_bounds()
            .map(|(lo, hi)| (lo + hi) * 0.5)
            .unwrap_or_else(|| g.transform.w_axis.truncate());
        if !g.hidden && inside(c) {
            hits.push(SelItem::Geometry(i));
        }
    }
    for (i, s) in scene.screens.iter().enumerate() {
        if !s.hidden && inside(s.world_center()) {
            hits.push(SelItem::Screen(i));
        }
    }
    hits
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
    fn marquee_selects_projected_in_rect() {
        // Project a fixture to screen with the default camera, draw a marquee
        // around its projected dot, and assert it's caught (#25). A marquee in the
        // opposite corner must NOT catch it (loose centre-in-rect rule).
        let scene = Scene::demo(); // one fixture at (0,4,0)
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let aspect = rect.width() / rect.height();
        let vp = cam.view_proj(aspect);
        let dot = OrbitCamera::project_to_screen(scene.fixtures[0].position, vp, rect)
            .expect("fixture projects in front of the camera");

        // A 40px box centred on the dot encloses its centre → hit.
        let around = egui::Rect::from_center_size(dot, egui::vec2(40.0, 40.0));
        let hits = marquee_hits(&scene, vp, rect, around);
        assert!(hits.contains(&SelItem::Fixture(0)), "dot-in-rect → selected");

        // A tiny box far from the dot → no hit.
        let far = egui::Rect::from_min_size(
            dot + egui::vec2(300.0, 300.0),
            egui::vec2(8.0, 8.0),
        );
        let none = marquee_hits(&scene, vp, rect, far);
        assert!(!none.contains(&SelItem::Fixture(0)), "off-dot rect → not selected");

        // A hidden fixture is skipped even when its dot is inside the marquee.
        let mut hidden = Scene::demo();
        hidden.fixtures[0].hidden = true;
        assert!(
            marquee_hits(&hidden, vp, rect, around).is_empty(),
            "hidden fixtures are not marquee-pickable"
        );
    }

    #[test]
    fn measure_point_falls_back_to_ground_plane() {
        // An empty scene: a downward ray from y=10 must land on y=0 (the floor).
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let ro = Vec3::new(2.0, 10.0, 3.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let p = pick_world_point(&scene, ro, rd).expect("ground hit");
        assert!((p.y - 0.0).abs() < 1e-3, "expected y=0, got {}", p.y);
        assert!((p.x - 2.0).abs() < 1e-3 && (p.z - 3.0).abs() < 1e-3);
    }

    #[test]
    fn placement_point_lands_on_ground_when_looking_down() {
        // A camera angled down at an empty scene: the centre ray hits the y=0
        // floor (#19 — place at the ground hit, not the origin).
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let mut cam = OrbitCamera::default();
        cam.set_view(crate::renderer::camera::CameraView::Top); // straight down
        cam.set_aspect(16.0 / 9.0);
        let p = placement_point(&scene, &cam);
        assert!(p.y.abs() < 1e-2, "expected a ground (y≈0) hit, got y={}", p.y);
    }

    #[test]
    fn placement_point_falls_back_in_front_when_ray_misses_ground() {
        // A camera tilted UP so the centre ray never meets the floor: placement
        // must fall back to a finite point in front of the camera, not panic/origin.
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let mut cam = OrbitCamera::default();
        cam.pitch = -1.2; // look upward, away from the floor
        cam.set_aspect(16.0 / 9.0);
        let p = placement_point(&scene, &cam);
        assert!(p.is_finite(), "placement must be finite, got {p:?}");
    }

    #[test]
    fn measure_point_prefers_nearer_surface_over_ground() {
        // A fixture sphere between the camera and the floor wins over the y=0 plane.
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
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

    /// S1-3d-cursor: a Shift+RMB place resolves the cursor to the ray's world hit.
    /// This mirrors the viewport's placement expression (pick_world_point, else a
    /// point in front of the camera) so the interactive wiring's math is pinned.
    #[test]
    fn shift_rclick_sets_cursor_to_ground_hit() {
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        // Downward ray from above empty space → the y=0 floor under (5, 0, -2).
        let ro = Vec3::new(5.0, 8.0, -2.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let dist = 12.0_f32;
        let p = pick_world_point(&scene, ro, rd).unwrap_or_else(|| ro + rd * dist.max(1.0));
        assert!((p.y).abs() < 1e-3, "cursor lands on the floor, got y={}", p.y);
        assert!((p.x - 5.0).abs() < 1e-3 && (p.z + 2.0).abs() < 1e-3);
    }

    /// S1-3d-cursor: when the ray escapes to the sky (no hit) the cursor falls back to
    /// a finite point in front of the camera, never NaN/origin.
    #[test]
    fn shift_rclick_cursor_falls_back_in_front() {
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        // Upward ray that never meets the y=0 plane.
        let ro = Vec3::new(0.0, 2.0, 0.0);
        let rd = Vec3::new(0.0, 1.0, 0.0);
        let dist = 10.0_f32;
        let p = pick_world_point(&scene, ro, rd).unwrap_or_else(|| ro + rd * dist.max(1.0));
        assert!(p.is_finite(), "fallback cursor must be finite, got {p:?}");
        assert!(p.y > 2.0, "fallback is in front of the camera, got {p:?}");
    }

    /// S1-3d-cursor: the Cursor3d pivot mode reads the supplied cursor point verbatim
    /// (so a Cursor-pivot rotate/scale spins/grows about it), and is independent of
    /// the selection's own centroid.
    #[test]
    fn pivot_cursor3d_uses_the_cursor_point() {
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        let demo = Scene::demo();
        scene.fixtures.push(demo.fixtures[0].clone());
        scene.fixtures[0].position = Vec3::new(10.0, 0.0, 10.0);
        let cursor = Vec3::new(-3.0, 1.5, 4.0);
        let pivot = compute_pivot(&scene, &[ObjectRef::Fixture(0)], PivotMode::Cursor3d, cursor);
        assert_eq!(pivot, cursor, "Cursor3d pivot ignores selection, uses the cursor");
        // The Median pivot, by contrast, is the fixture's own position.
        let median = compute_pivot(&scene, &[ObjectRef::Fixture(0)], PivotMode::Median, cursor);
        assert!((median - Vec3::new(10.0, 0.0, 10.0)).length() < 1.0, "median ≠ cursor");
    }

    /// S1-viewport-overlays: the stats overlay emits a line per non-empty category
    /// (in outliner order) plus an always-present selected line, with correct
    /// singular/plural agreement and live counts.
    #[test]
    fn stats_lines_counts_and_pluralise() {
        // Mixed scene: a singular, a plural, a zero (skipped) and selection.
        let lines = stats_lines(1, 3, 0, 2, 4);
        assert_eq!(
            lines,
            vec![
                "1 fixture".to_string(),   // singular
                "3 objects".to_string(),   // plural
                // screens == 0 → skipped entirely
                "2 environments".to_string(),
                "4 selected".to_string(), // always present, last
            ]
        );
    }

    /// An empty scene shows only the "0 selected" line (every category zero ⇒
    /// skipped), so the overlay never adds noise to a blank canvas.
    #[test]
    fn stats_lines_empty_scene_is_just_selected() {
        assert_eq!(stats_lines(0, 0, 0, 0, 0), vec!["0 selected".to_string()]);
    }

    /// One of everything reads in the singular and keeps outliner order.
    #[test]
    fn stats_lines_singular_order() {
        assert_eq!(
            stats_lines(1, 1, 1, 1, 1),
            vec![
                "1 fixture".to_string(),
                "1 object".to_string(),
                "1 screen".to_string(),
                "1 environment".to_string(),
                "1 selected".to_string(),
            ]
        );
    }

    // --- library content-class chip predicate (S2c) ---------------------------
    fn row(kind: LibKind, accent: bool) -> LibRow {
        LibRow { kind, icon: "", name: String::new(), meta: String::new(), category: String::new(), accent, source: None }
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
        assert!(chip_matches(LibChip::Fixtures, &row(LibKind::Gdtf(0), false)));
        assert!(chip_matches(LibChip::Fixtures, &row(LibKind::Fixture(0), false)));
        // A laser profile (accent) is NOT a "fixture" under this chip.
        assert!(!chip_matches(LibChip::Fixtures, &row(LibKind::Fixture(1), true)));
        // Screens / environments are excluded.
        assert!(!chip_matches(LibChip::Fixtures, &row(LibKind::Screen(0), false)));
        assert!(!chip_matches(LibChip::Fixtures, &row(LibKind::Env(0), false)));
    }

    #[test]
    fn chip_lasers_is_only_accented_profiles() {
        assert!(chip_matches(LibChip::Lasers, &row(LibKind::Fixture(0), true)));
        assert!(!chip_matches(LibChip::Lasers, &row(LibKind::Fixture(0), false)));
        // A GDTF is never classed as a laser by this chip (accent is irrelevant).
        assert!(!chip_matches(LibChip::Lasers, &row(LibKind::Gdtf(0), true)));
    }

    #[test]
    fn chip_screens_environments_imported_partition_by_kind() {
        assert!(chip_matches(LibChip::Screens, &row(LibKind::Screen(0), false)));
        assert!(!chip_matches(LibChip::Screens, &row(LibKind::Env(0), false)));
        assert!(chip_matches(LibChip::Environments, &row(LibKind::Env(0), false)));
        assert!(!chip_matches(LibChip::Environments, &row(LibKind::Screen(0), false)));
        // Imported = GDTF rows only (a built-in profile, even non-laser, is not).
        assert!(chip_matches(LibChip::Imported, &row(LibKind::Gdtf(0), false)));
        assert!(!chip_matches(LibChip::Imported, &row(LibKind::Fixture(0), false)));
    }
}

#[cfg(test)]
mod transform_tests {
    use super::*;
    use crate::ui::{Axis, SnapSettings, TransformOrientation};

    fn make_op(kind: TransformKind, axis: Option<Axis>, pivot: Vec3, idx: usize, p0: Vec3) -> TransformOp {
        make_op_q(kind, axis, pivot, idx, p0, Quat::IDENTITY, TransformOrientation::Global)
    }

    /// Like [`make_op`] but with an explicit start-orientation Quat (the Local-basis
    /// source) and a transform orientation (#37).
    fn make_op_q(
        kind: TransformKind,
        axis: Option<Axis>,
        pivot: Vec3,
        idx: usize,
        p0: Vec3,
        q: Quat,
        orientation: TransformOrientation,
    ) -> TransformOp {
        TransformOp { kind, axis, start_screen: egui::pos2(0.0, 0.0), viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)), pivot, start: vec![(idx, p0, q)], geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(), gizmo_hovered_axis: None, gizmo_plane_normal: None, gizmo_view: false, from_gizmo: false, num: NumInput::default(), individual: false, snap: SnapSettings::default(), from_duplicate: false, dup_base: Vec::new(), dup_extra: Vec::new(), orientation }
    }

    #[test]
    fn shift_d_array_is_live_and_reversible() {
        let mut scene = Scene::demo(); // one fixture at index 0
        // Simulate Shift+D's clone: a base copy of fixture 0.
        let base = scene.duplicate_object(ObjectRef::Fixture(0)).unwrap();
        let ObjectRef::Fixture(bi) = base else { panic!("expected a fixture") };
        let home = scene.fixtures[bi].position;
        let n_before = scene.fixtures.len();
        // The grab has moved the base copy by `delta` from its op-start home.
        let delta = Vec3::new(2.0, 0.0, 0.0);
        scene.fixtures[bi].position = home + delta;

        let mut op = make_op(TransformKind::Move, None, Vec3::ZERO, bi, home);
        op.from_duplicate = true;
        op.dup_base = vec![base];
        // Type "3" → an array of 3 (the base copy #1 + two LIVE extras #2/#3).
        op.num = NumInput { str: "3".into(), sign: false, active: true };

        update_dup_array(&mut op, &mut scene);
        assert_eq!(op.dup_extra.len(), 2, "count 3 → 2 extra clones");
        assert_eq!(scene.fixtures.len(), n_before + 2, "extras pushed onto the scene");
        let e0 = match op.dup_extra[0] {
            ObjectRef::Fixture(i) => i,
            _ => panic!("fixture"),
        };
        let e1 = match op.dup_extra[1] {
            ObjectRef::Fixture(i) => i,
            _ => panic!("fixture"),
        };
        // #1 stays at home+delta; extras are evenly spaced at home+2·delta, home+3·delta.
        assert!((scene.fixtures[bi].position - (home + delta)).length() < 1e-3);
        assert!((scene.fixtures[e0].position - (home + delta * 2.0)).length() < 1e-3);
        assert!((scene.fixtures[e1].position - (home + delta * 3.0)).length() < 1e-3);

        // Shrinking the count back to 1 tail-truncates the live extras cleanly.
        op.num = NumInput::default();
        update_dup_array(&mut op, &mut scene);
        assert!(op.dup_extra.is_empty(), "count 1 → no extras");
        assert_eq!(scene.fixtures.len(), n_before, "extras removed from the scene");
    }

    #[test]
    fn move_axis_lock_keeps_other_axes() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        apply_transform(&o, &mut scene, &cam, egui::pos2(120.0, 40.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!(d.y.abs() < 1e-4, "y leaked: {}", d.y);
        assert!(d.z.abs() < 1e-4, "z leaked: {}", d.z);
    }

    /// A move started by grabbing a PLANE handle (#S2) slides ON that plane: the
    /// off-plane (normal) coordinate of every element stays fixed, while the two
    /// in-plane coordinates track the cursor (ray_plane_point absolute drag).
    #[test]
    fn plane_drag_keeps_off_axis_fixed() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // Plane normal Z → the XY plane: Z must not move.
        let o = TransformOp {
            kind: TransformKind::Move,
            axis: None,
            start_screen: egui::pos2(400.0, 300.0),
            viewport: rect,
            pivot: p0,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: Some(Axis::Z),
            gizmo_view: false,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap: SnapSettings::default(),
            orientation: TransformOrientation::Global,
            from_duplicate: false,
            dup_base: Vec::new(), dup_extra: Vec::new(),
        };
        // Drag to a clearly different screen point so the in-plane delta is nonzero.
        apply_transform(&o, &mut scene, &cam, egui::pos2(560.0, 180.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!(d.z.abs() < 1e-4, "off-plane Z leaked: {}", d.z);
        // The drag actually moved the fixture in the plane (not a no-op).
        assert!(d.length() > 1e-3, "plane drag produced no motion: {d:?}");
    }

    /// #72 VIEW-plane move: a centre-square drag slides the fixture on the screen-
    /// parallel plane (normal = camera forward), so the moved offset has NO component
    /// along the camera forward (it stays on the view plane) and the drag is nonzero.
    #[test]
    fn view_plane_move_stays_on_screen_plane() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let o = TransformOp {
            kind: TransformKind::Move,
            axis: None,
            start_screen: egui::pos2(400.0, 300.0),
            viewport: rect,
            pivot: p0,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(),
            screen_start: Vec::new(),
            env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: true,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap: SnapSettings::default(),
            from_duplicate: false,
            dup_base: Vec::new(),
            dup_extra: Vec::new(),
            orientation: TransformOrientation::Global,
        };
        apply_transform(&o, &mut scene, &cam, egui::pos2(560.0, 180.0), false);
        let d = scene.fixtures[0].position - p0;
        let fwd = cam.view_basis().2;
        assert!(d.dot(fwd).abs() < 1e-3, "view-plane move leaked along camera forward: {}", d.dot(fwd));
        assert!(d.length() > 1e-3, "view-plane drag produced no motion: {d:?}");
    }

    /// #72 VIEW-axis rotate: a view-ring drag spins the fixture about the CAMERA
    /// FORWARD. With the pivot offset from the fixture, the position rotates about the
    /// camera-forward axis through the pivot, so the offset's component ALONG forward
    /// is preserved (rotation about an axis can't move points along that axis).
    #[test]
    fn view_rotate_spins_about_camera_forward() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let pivot = p0 + Vec3::new(1.5, 0.5, 0.0);
        let o = TransformOp {
            kind: TransformKind::Rotate,
            axis: None,
            start_screen: egui::pos2(0.0, 0.0),
            viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
            pivot,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(),
            screen_start: Vec::new(),
            env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: true,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap: SnapSettings::default(),
            from_duplicate: false,
            dup_base: Vec::new(),
            dup_extra: Vec::new(),
            orientation: TransformOrientation::Global,
        };
        apply_transform(&o, &mut scene, &cam, egui::pos2(120.0, 0.0), false);
        let fwd = cam.view_basis().2;
        let p1 = scene.fixtures[0].position;
        // The rotation actually moved the fixture (nonzero drag).
        assert!((p1 - p0).length() > 1e-3, "view rotate produced no motion");
        // Offset-from-pivot length is preserved, and its forward-component is unchanged
        // (rotation about the forward axis through the pivot).
        let r0 = p0 - pivot;
        let r1 = p1 - pivot;
        assert!((r0.length() - r1.length()).abs() < 1e-3, "radius changed under view rotate");
        assert!((r0.dot(fwd) - r1.dot(fwd)).abs() < 1e-3, "moved along the camera-forward axis");
    }

    /// #70 snap preview: while a Move is live with Vertex snap engaged, the preview
    /// marker point equals the snap TARGET (the destination the origin lands on) — and
    /// is `None` when snap is off or the op isn't a Move.
    #[test]
    fn snap_preview_matches_snapped_destination() {
        let mut scene = Scene::demo();
        // Need a second fixture as a snap target node.
        scene.duplicate_fixture(0, Vec3::new(5.0, 0.0, 2.0), 0.0, 1).expect("dup");
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let aspect = rect.width() / rect.height();
        let p0 = scene.fixtures[0].position;
        let target = scene.fixtures[1].position;
        // Cursor placed over the target fixture's projected origin so Vertex snaps to it.
        let cursor = OrbitCamera::project_to_screen(target, cam.view_proj(aspect), rect)
            .expect("target on screen");
        let mut snap = SnapSettings::default();
        snap.on = true;
        snap.mode = crate::ui::SnapMode::Vertex;
        let o = TransformOp {
            kind: TransformKind::Move,
            axis: None,
            start_screen: egui::pos2(400.0, 300.0),
            viewport: rect,
            pivot: p0,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(),
            screen_start: Vec::new(),
            env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: false,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap,
            from_duplicate: false,
            dup_base: Vec::new(),
            dup_extra: Vec::new(),
            orientation: TransformOrientation::Global,
        };
        apply_transform(&o, &mut scene, &cam, cursor, true);
        let marker = snap_preview_point(&o, &scene, true).expect("preview while Move+snap");
        // The marker is the primary origin's snapped destination = the target node.
        assert!((marker - target).length() < 1e-3, "preview {marker:?} != target {target:?}");
        // Snap off → no marker; Rotate → no marker.
        assert!(snap_preview_point(&o, &scene, false).is_none());
        let rot = make_op(TransformKind::Rotate, None, p0, 0, p0);
        assert!(snap_preview_point(&rot, &scene, true).is_none());
    }

    #[test]
    fn rotate_y_preserves_distance_and_height() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 + Vec3::new(2.0, 0.0, 0.0);
        let o = make_op(TransformKind::Rotate, Some(Axis::Y), pivot, 0, p0);
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(80.0, 0.0), false);
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
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(200.0, 0.0), false);
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
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(-100000.0, 0.0), false);
        let after = (scene.fixtures[0].position - pivot).length();
        assert!(after > 0.0, "geometry collapsed to the pivot");
    }

    #[test]
    fn numinput_value_parses() {
        let n = NumInput { str: "4.5".into(), sign: false, active: true };
        assert!((n.value() - 4.5).abs() < 1e-6);
        let n = NumInput { str: "4".into(), sign: true, active: true };
        assert!((n.value() + 4.0).abs() < 1e-6);
        assert_eq!(NumInput::default().value(), 0.0);
        assert_eq!(NumInput { str: ".".into(), sign: false, active: true }.value(), 0.0);
    }

    #[test]
    fn numinput_display_shows_sign() {
        assert_eq!(NumInput { str: "4.0".into(), sign: false, active: true }.display(), "4.0");
        assert_eq!(NumInput { str: "45".into(), sign: true, active: true }.display(), "-45");
        // Lone sign before any digit still renders, so the keystroke lands.
        assert_eq!(NumInput { str: String::new(), sign: true, active: true }.display(), "-0");
    }

    #[test]
    fn typed_move_overrides_mouse_exact_metres() {
        // G,X,"4",Enter → move +4 m on global X regardless of mouse position.
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.num = NumInput { str: "4".into(), sign: false, active: true };
        // A wild mouse position must be ignored once numinput is active.
        apply_transform(&o, &mut scene, &cam, egui::pos2(9999.0, -9999.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x - 4.0).abs() < 1e-4, "expected +4 on X, got {}", d.x);
        assert!(d.y.abs() < 1e-4 && d.z.abs() < 1e-4, "leaked off X: {d:?}");
    }

    #[test]
    fn typed_move_no_axis_falls_back_to_global_x() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let mut o = make_op(TransformKind::Move, None, p0, 0, p0);
        o.num = NumInput { str: "2.5".into(), sign: true, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x + 2.5).abs() < 1e-4, "expected -2.5 on X, got {}", d.x);
    }

    #[test]
    fn typed_rotate_uses_degrees() {
        // R,Y,"90" → quarter turn about Y; pivot offset on +X maps to +Z (RH).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(1.0, 0.0, 0.0); // fixture sits at pivot + X
        let mut o = make_op(TransformKind::Rotate, Some(Axis::Y), pivot, 0, p0);
        o.num = NumInput { str: "90".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        let off = scene.fixtures[0].position - pivot;
        // +X (1,0,0) rotated 90° about +Y → (0,0,-1) in glam's RH convention.
        assert!((off.x).abs() < 1e-4, "x not zeroed: {}", off.x);
        assert!((off.z + 1.0).abs() < 1e-4, "expected z=-1, got {}", off.z);
    }

    #[test]
    fn typed_scale_factor_is_exact() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(3.0, 0.0, 0.0);
        let mut o = make_op(TransformKind::Scale, None, pivot, 0, p0);
        o.num = NumInput { str: "2".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(12345.0, 0.0), false);
        let before = (p0 - pivot).length();
        let after = (scene.fixtures[0].position - pivot).length();
        assert!((after - before * 2.0).abs() < 1e-4, "expected ×2, {before} -> {after}");
    }

    // --- #4 snap: quantization composes with the typed-amount apply path ------
    #[test]
    fn snap_move_quantizes_typed_amount() {
        // G,X,"1.4",Enter with a 1 m snap grid → lands on exactly +1 m (round).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.num = NumInput { str: "1.4".into(), sign: false, active: true };
        o.snap = SnapSettings { on: true, move_step: 1.0, ..Default::default() };
        // snap_on = true (caller would XOR Ctrl; here passed directly).
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), true);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x - 1.0).abs() < 1e-4, "expected snapped +1 m, got {}", d.x);
    }

    #[test]
    fn snap_off_passes_typed_amount_through() {
        // Same op but snap_on=false (e.g. Ctrl inverted it off) → exact 1.4 m.
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.num = NumInput { str: "1.4".into(), sign: false, active: true };
        o.snap = SnapSettings { on: true, move_step: 1.0, ..Default::default() };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x - 1.4).abs() < 1e-4, "expected exact 1.4 m, got {}", d.x);
    }

    #[test]
    fn snap_rotate_quantizes_to_15_degrees() {
        // R,Y,"20",Enter with a 15° grid → snaps to 15° (quarter-ish turn).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(1.0, 0.0, 0.0);
        let mut o = make_op(TransformKind::Rotate, Some(Axis::Y), pivot, 0, p0);
        o.num = NumInput { str: "20".into(), sign: false, active: true };
        o.snap = SnapSettings { on: true, rotate_deg: 15.0, ..Default::default() };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), true);
        // Offset length is preserved by rotation; check the snapped angle: +X (len 1)
        // rotated 15° about +Y → z = -sin(15°).
        let off = scene.fixtures[0].position - pivot;
        let expect_z = -(15f32.to_radians()).sin();
        assert!((off.z - expect_z).abs() < 1e-3, "expected 15° snap (z={expect_z}), got {}", off.z);
    }

    // --- #5 Individual Origins: each element transforms about its OWN origin ----
    #[test]
    fn individual_origins_rotate_keeps_each_position() {
        // Two fixtures at different spots. With Individual Origins, a rotate spins
        // each about ITSELF: positions are UNCHANGED, only orientations turn. A
        // Median pivot, by contrast, would orbit both about their centroid.
        let mut scene = Scene::demo();
        // Ensure a second fixture exists at a distinct position.
        scene.duplicate_fixture(0, Vec3::new(6.0, 0.0, 0.0), 0.0, 1).expect("dup");
        assert!(scene.fixtures.len() >= 2);
        let p0a = scene.fixtures[0].position;
        let p0b = scene.fixtures[1].position;
        let q0a = scene.fixtures[0].orientation;
        // Median of the two — the pivot the op carries (ignored when individual).
        let median = (p0a + p0b) * 0.5;
        let mut o = TransformOp {
            kind: TransformKind::Rotate,
            axis: Some(Axis::Y),
            start_screen: egui::pos2(0.0, 0.0),
            viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
            pivot: median,
            start: vec![(0, p0a, q0a), (1, p0b, scene.fixtures[1].orientation)],
            geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: false,
            from_gizmo: false,
            num: NumInput { str: "90".into(), sign: false, active: true },
            individual: true,
            snap: SnapSettings::default(),
            orientation: TransformOrientation::Global,
            from_duplicate: false,
            dup_base: Vec::new(), dup_extra: Vec::new(),
        };
        o.num.active = true;
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        // Positions unchanged (each spun about itself)...
        assert!((scene.fixtures[0].position - p0a).length() < 1e-4, "fixture 0 moved");
        assert!((scene.fixtures[1].position - p0b).length() < 1e-4, "fixture 1 moved");
        // ...but the orientation DID rotate.
        assert!(
            scene.fixtures[0].orientation.angle_between(q0a) > 0.1,
            "orientation did not rotate"
        );
    }

    #[test]
    fn median_rotate_orbits_about_centroid() {
        // Same two fixtures, but Median pivot → both ORBIT the shared centroid, so
        // their positions move (the contrast to Individual Origins above).
        let mut scene = Scene::demo();
        scene.duplicate_fixture(0, Vec3::new(6.0, 0.0, 0.0), 0.0, 1).expect("dup");
        let p0a = scene.fixtures[0].position;
        let p0b = scene.fixtures[1].position;
        let median = (p0a + p0b) * 0.5;
        let o = TransformOp {
            kind: TransformKind::Rotate,
            axis: Some(Axis::Y),
            start_screen: egui::pos2(0.0, 0.0),
            viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
            pivot: median,
            start: vec![
                (0, p0a, scene.fixtures[0].orientation),
                (1, p0b, scene.fixtures[1].orientation),
            ],
            geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: false,
            from_gizmo: false,
            num: NumInput { str: "90".into(), sign: false, active: true },
            individual: false,
            snap: SnapSettings::default(),
            orientation: TransformOrientation::Global,
            from_duplicate: false,
            dup_base: Vec::new(), dup_extra: Vec::new(),
        };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        // At least one position moved (they orbited the centroid).
        assert!(
            (scene.fixtures[0].position - p0a).length() > 0.1
                || (scene.fixtures[1].position - p0b).length() > 0.1,
            "median rotate did not orbit"
        );
    }

    // --- #37 transform orientations -----------------------------------------
    #[test]
    fn local_rotate_spins_about_elements_own_axis() {
        // A fixture yawed 90° about world +Y has its LOCAL +X pointing along world
        // −Z. A typed Local rotate locked to X must turn the orientation about THAT
        // local axis, not world X — so the resulting spin axis is world −Z.
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let q = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2); // local X → world -Z
        let mut o = make_op_q(
            TransformKind::Rotate,
            Some(Axis::X),
            p0,
            0,
            p0,
            q,
            TransformOrientation::Local,
        );
        o.num = NumInput { str: "90".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        // The applied delta rotation = orientation_after * orientation_before⁻¹.
        let delta = scene.fixtures[0].orientation * q.inverse();
        let (axis, angle) = delta.to_axis_angle();
        // Spun 90° about the LOCAL X = world (q * X) = (0,0,-1) (sign-agnostic).
        let local_x = q * Vec3::X;
        assert!((angle - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "angle {angle}");
        assert!(
            axis.dot(local_x).abs() > 0.999,
            "spin axis {axis:?} not aligned to local X {local_x:?}"
        );
        // And it is NOT world X (which Global would have used).
        assert!(axis.dot(Vec3::X).abs() < 1e-2, "leaked onto world X: {axis:?}");
    }

    #[test]
    fn view_move_follows_the_screen_plane() {
        // A View-space X (screen-right) move must land in the camera's right/up plane
        // — i.e. it has NO component along the camera forward axis (it slides across
        // the screen, never toward/away from the viewer).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let (right, up, fwd) = cam.view_basis();
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.orientation = TransformOrientation::View;
        o.num = NumInput { str: "3".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &cam, egui::pos2(0.0, 0.0), false);
        let d = scene.fixtures[0].position - p0;
        // Moved 3 m along screen-right, and stayed in the screen plane.
        assert!((d.dot(right) - 3.0).abs() < 1e-3, "expected +3 along screen-right, got {}", d.dot(right));
        assert!(d.dot(fwd).abs() < 1e-3, "leaked toward the viewer: {}", d.dot(fwd));
        assert!(d.dot(up).abs() < 1e-3, "leaked onto screen-up: {}", d.dot(up));
    }

    #[test]
    fn global_orientation_matches_world_axes() {
        // Sanity: Global == identity basis, so an oriented op reduces to the old
        // world-axis behaviour byte-for-byte (no regression).
        let mut a = Scene::demo();
        let mut b = Scene::demo();
        let p0 = a.fixtures[0].position;
        let cam = OrbitCamera::default();
        let mut o = make_op(TransformKind::Move, Some(Axis::Z), p0, 0, p0);
        o.orientation = TransformOrientation::Global;
        o.num = NumInput { str: "2".into(), sign: false, active: true };
        apply_transform(&o, &mut a, &cam, egui::pos2(0.0, 0.0), false);
        let d = a.fixtures[0].position - p0;
        assert!((d.z - 2.0).abs() < 1e-4 && d.x.abs() < 1e-4 && d.y.abs() < 1e-4, "global Z move wrong: {d:?}");
        // Untouched control scene stays put.
        assert_eq!(b.fixtures[0].position, p0);
        let _ = &mut b;
    }

    // --- S2 #40 ray-plane absolute drag math --------------------------------
    #[test]
    fn ray_axis_projection_sticks_to_the_cursor() {
        // A ray aimed straight down at (5,*,0) projected onto the world-X axis line
        // through the origin must land at x=5 (the cursor's foot on the axis),
        // regardless of the ray's height — the "handle sticks to the cursor" core.
        let ro = Vec3::new(5.0, 10.0, 0.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let p = ray_axis_closest_point(ro, rd, Vec3::ZERO, Vec3::X);
        assert!((p.x - 5.0).abs() < 1e-4, "expected x=5 on the axis, got {}", p.x);
        // The result lies ON the axis line (y=z=0).
        assert!(p.y.abs() < 1e-4 && p.z.abs() < 1e-4, "off the X axis line: {p:?}");
        // A second cursor further along maps further along — monotone, no drift.
        let q = ray_axis_closest_point(Vec3::new(9.0, 3.0, 0.0), rd, Vec3::ZERO, Vec3::X);
        assert!((q.x - 9.0).abs() < 1e-4, "expected x=9, got {}", q.x);
    }

    #[test]
    fn ray_axis_parallel_holds_position() {
        // A ray PARALLEL to the constraint axis has no well-defined projection — the
        // helper must return the pivot (no motion) rather than flinging to infinity.
        let ro = Vec3::new(0.0, 2.0, 0.0);
        let rd = Vec3::X; // parallel to the X axis
        let p = ray_axis_closest_point(ro, rd, Vec3::new(1.0, 0.0, 0.0), Vec3::X);
        assert!((p - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-4, "should hold the pivot, got {p:?}");
    }

    #[test]
    fn ray_plane_intersects() {
        // Ray down the −Y from (2,5,3) meets the y=0 plane (normal +Y) at (2,0,3).
        let hit = ray_plane_point(
            Vec3::new(2.0, 5.0, 3.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::ZERO,
            Vec3::Y,
        )
        .expect("should hit the plane");
        assert!((hit - Vec3::new(2.0, 0.0, 3.0)).length() < 1e-4, "wrong plane hit: {hit:?}");
        // A ray parallel to the plane misses (None).
        assert!(ray_plane_point(Vec3::new(0.0, 5.0, 0.0), Vec3::X, Vec3::ZERO, Vec3::Y).is_none());
    }

    // --- S2 #71 Vertex snap: nearest other origin within the screen threshold ---
    #[test]
    fn vertex_snap_picks_nearest_other_origin() {
        // Two fixtures: 0 (being moved) and 1 (a target node). Looking straight down
        // the −Y with fixture 1 directly under the cursor, the nearest-origin query
        // returns fixture 1's world origin (and never fixture 0, which is excluded).
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let demo = Scene::demo();
        scene.fixtures.push(demo.fixtures[0].clone()); // 0: moved
        scene.fixtures.push(demo.fixtures[0].clone()); // 1: target node
        scene.fixtures[0].position = Vec3::new(0.0, 0.0, 0.0);
        scene.fixtures[0].hidden = false;
        let target = Vec3::new(4.0, 0.0, -2.0);
        scene.fixtures[1].position = target;
        scene.fixtures[1].hidden = false;

        let mut cam = OrbitCamera::default();
        cam.target = target; // centre the view on the node so it projects to rect-centre
        cam.set_aspect(1.0);
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 600.0));
        let vp = cam.view_proj(1.0);
        // Cursor sitting on the projected target node.
        let cursor = OrbitCamera::project_to_screen(target, vp, rect).expect("target on screen");
        let got = nearest_origin_screen(&scene, vp, rect, cursor, &[0], 18.0).expect("a node in range");
        assert!((got - target).length() < 1e-3, "expected the node origin {target:?}, got {got:?}");

        // Excluding fixture 1 too (no other nodes) → nothing in range.
        let none = nearest_origin_screen(&scene, vp, rect, cursor, &[0, 1], 18.0);
        assert!(none.is_none(), "expected no snap target, got {none:?}");

        // A cursor far from any node → out of the pixel threshold → None.
        let far = nearest_origin_screen(&scene, vp, rect, cursor + egui::vec2(300.0, 0.0), &[0], 18.0);
        assert!(far.is_none(), "cursor off all nodes should not snap, got {far:?}");
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
        let count =
            |s: &Scene| s.fixtures.len() + s.geometry.len() + s.screens.len() + s.environments.len();
        let before = count(&scene);
        let sel = add_active_library_item(&library, &mut scene, &camera, &key, Some(glam::Vec3::ZERO));
        assert!(sel.is_some(), "a resolvable key adds + returns a selection");
        assert_eq!(count(&scene), before + 1, "exactly one entity is added");

        // An unknown key resolves to nothing → no add, no panic (cursor None is fine,
        // placement_point is only reached once a row resolves).
        let mid = count(&scene);
        assert!(add_active_library_item(&library, &mut scene, &camera, "nope::missing", None).is_none());
        assert_eq!(count(&scene), mid, "an unknown key adds nothing");
    }
}
