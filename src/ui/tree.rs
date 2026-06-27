//! The Scene outliner's custom recursive hierarchy tree (S2).
//!
//! egui's `CollapsingHeader` could only give us a flat stack of sibling folders
//! ("Objects / Fixtures / …") — it has no notion of a single project root with
//! nested children, no Blender-style guide lines, restriction columns, or shared
//! flattened range-selection. So we BUILD OUR OWN: a derived `TreeRow` list is
//! rebuilt every frame from the flat `Scene` (exactly how Blender's
//! `tree_display_view_layer.cc` rebuilds its TreeElement tree on redraw — the
//! tree is a VIEW, never the storage), then each row is painted by hand with a
//! `Painter` + an allocated interact rect:
//!
//!   [indent by depth][disclosure ▸/▾][icon][name + secondary] …… [patch][eye]
//!
//! The result reads as ONE root "Scene" node with World / Fixtures / Objects /
//! Screens nested beneath it — a true hierarchy, not flat categories.
//!
// TODO(outliner): drag-reparent + user-created collections require persisted
// parent/membership (a .archie/bincode format bump) — DEFERRED. See Blender
// outliner_dragdrop.cc for INTO vs BEFORE/AFTER drop zones + the cycle guard
// (outliner_is_collection_dragged_into_itself). Also deferred: render/selectable
// restriction columns (new persisted bools → format bump); only the EYE ships,
// reusing the existing `hidden` field.

use std::collections::HashSet;

use egui::{Color32, Sense};

use super::outliner::{self, SceneSort};
use super::theme;
use crate::dmx::PatchTable;
use crate::scene::{apply_fixture_click, EntityId, Scene, Selection};

/// The outliner's type-filter chip (catalog #62): which entity kinds the tree
/// shows. `All` is the unfiltered default; the rest restrict to one collection
/// (the matching group container still draws so its header reads as the scope).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeChip {
    #[default]
    All,
    Fixtures,
    Objects,
    Screens,
    Pyro,
}

impl TypeChip {
    /// The chip strip's left-to-right order + labels.
    pub const ORDER: [TypeChip; 5] =
        [TypeChip::All, TypeChip::Fixtures, TypeChip::Objects, TypeChip::Screens, TypeChip::Pyro];
    pub fn label(self) -> &'static str {
        match self {
            TypeChip::All => "All",
            TypeChip::Fixtures => "Fixtures",
            TypeChip::Objects => "Objects",
            TypeChip::Screens => "Screens",
            TypeChip::Pyro => "Pyro",
        }
    }
    /// Whether the `Fixtures` group + its leaves should be visible.
    fn fixtures(self) -> bool {
        matches!(self, TypeChip::All | TypeChip::Fixtures)
    }
    fn objects(self) -> bool {
        matches!(self, TypeChip::All | TypeChip::Objects)
    }
    fn screens(self) -> bool {
        matches!(self, TypeChip::All | TypeChip::Screens)
    }
    fn pyro(self) -> bool {
        matches!(self, TypeChip::All | TypeChip::Pyro)
    }
    /// World/Environment only show when the type filter is unrestricted.
    fn world(self) -> bool {
        matches!(self, TypeChip::All)
    }
}

/// The outliner's STATE-filter chips (catalog #62): orthogonal toggles ANDed onto
/// the type chip + search. Each restricts a fixture row to a state — unpatched,
/// in the current selection, or address-conflicting. State chips only constrain
/// FIXTURES (the only kind carrying patch/conflict state); when any state chip is
/// on, non-fixture leaves are hidden so the result reads as a focused fixture
/// list. Multiple state chips compose as AND (Blender's filter stacking).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct StateChips {
    pub unpatched: bool,
    pub selected: bool,
    pub conflicts: bool,
}

impl StateChips {
    /// Any state chip active → the tree is in fixture-focus mode.
    fn any(self) -> bool {
        self.unpatched || self.selected || self.conflicts
    }
}

/// The full outliner chip-filter state held on `Ui` across frames.
#[derive(Clone, Copy, Default)]
pub struct OutlinerFilter {
    pub kind: TypeChip,
    pub state: StateChips,
}

impl OutlinerFilter {
    /// The pure per-fixture predicate (catalog #62) — testable without any egui.
    /// `patched` = the fixture has an enabled patch entry; `selected` = it's in
    /// the current selection; `conflict` = its address conflicts. ANDs every
    /// active state chip; an all-off state passes everything. The type chip is
    /// applied at the row-building level (whole groups), not here.
    pub fn fixture_passes(&self, patched: bool, selected: bool, conflict: bool) -> bool {
        if self.state.unpatched && patched {
            return false;
        }
        if self.state.selected && !selected {
            return false;
        }
        if self.state.conflicts && !conflict {
            return false;
        }
        true
    }
}

/// A logical hierarchy node, addressed by a SESSION-STABLE key so expand-state
/// and the range anchor survive add / delete reordering (Blender's
/// `TreeStoreElem` identity role). Group / Root keys are constants; entity keys
/// carry the stable `EntityId` assigned in S1.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKey {
    /// The single project root ("Scene").
    Root,
    /// The World node (HDRI sky + image-based ambient).
    World,
    /// The Environment container nested under World (fog volumes).
    EnvGroup,
    /// One of the entity group containers.
    Group(GroupKind),
    /// A leaf entity, keyed by its stable id.
    Entity(EntityId),
}

/// The three flat entity collections that become group nodes under Root.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKind {
    Fixtures,
    Objects,
    Screens,
    Pyro,
}

/// A deferred outliner action returned to the caller. The tree is drawn mid-dock
/// where the undo stack isn't reachable, so anything that needs an undo step
/// (hide toggles, renames) is RETURNED and applied post-dock — mirroring the
/// existing `pending_delete` / `pending_nudge` pattern. Plain selection edits are
/// field-disjoint from undo and done in-widget directly on `selection`.
#[derive(Default, Clone)]
pub enum TreeAction {
    #[default]
    None,
    /// Visibility eye toggled on an entity leaf or a whole group → caller flips
    /// `hidden` and wraps it in ONE undo step.
    ToggleHidden(NodeKey),
    /// Inline rename committed → caller writes the new name + one undo step.
    Rename(NodeKey, String),
}

// --- row metrics -----------------------------------------------------------
const ROW_H: f32 = 20.0; // single-line row — Blender UI_UNIT_Y density
const INDENT: f32 = 14.0; // px per depth level (~Blender UI_UNIT_X step)
const PAD_X: f32 = 6.0; // left gutter before depth 0
const DISCLOSURE_W: f32 = 14.0; // disclosure-triangle cell, immediately left of the icon
const ICON_DX: f32 = 3.0; // gap between the disclosure cell and the type icon
const ICON_W: f32 = 16.0; // type-icon advance width
const ICON_GAP: f32 = 3.0; // gap between the icon and the name text
// Right-aligned metadata columns. Blender keeps the left side name-only and right-
// aligns ALL metadata, so the name reads with room (no truncation). The eye is the
// only always-present column; the fixture patch + channel chips are measured, so
// non-fixture rows reserve just EDGE_PAD + EYE_W on the right.
const EDGE_PAD: f32 = 6.0; // gutter at the very right edge
const EYE_W: f32 = 18.0; // visibility-eye cell (every row)
const COL_GAP: f32 = 6.0; // gap between right-aligned chips

/// What a flattened row represents (carries the data-array index for leaves).
#[derive(Clone, Copy)]
enum RowKind {
    Root,
    World,
    EnvGroup,
    Group(GroupKind),
    Fixture(usize),
    Object(usize),
    Screen(usize),
    Pyro(usize),
    Environment(usize),
}

/// Aggregate visibility of a row. Leaves are only ever `Shown` / `Hidden`;
/// container rows can be `Mixed` when some-but-not-all children are hidden
/// (Blender draws the restriction icon greyed/half in that case — we tint the
/// eye and use the open-eye glyph so the parent still toggles "hide remaining").
#[derive(Clone, Copy, PartialEq, Eq)]
enum VisState {
    Shown,
    Hidden,
    Mixed,
}

impl VisState {
    /// Fold child states into a parent aggregate.
    fn fold(states: impl Iterator<Item = bool>) -> VisState {
        let (mut any_hidden, mut any_shown, mut empty) = (false, false, true);
        for h in states {
            empty = false;
            if h {
                any_hidden = true;
            } else {
                any_shown = true;
            }
        }
        if empty || (!any_hidden) {
            VisState::Shown
        } else if !any_shown {
            VisState::Hidden
        } else {
            VisState::Mixed
        }
    }
    /// Rows dim their content only when fully hidden (mixed parents stay legible).
    fn dim(self) -> bool {
        self == VisState::Hidden
    }
}

/// One fully-resolved, drawable row in visible (expanded) depth-first order.
/// This Vec is ALSO the shift-range order for fixtures.
struct TreeRow {
    key: NodeKey,
    kind: RowKind,
    depth: u8,
    icon: &'static str,
    label: String,
    secondary: String,
    has_children: bool,
    /// Entity hidden, or the aggregate visibility for a group / container row.
    vis: VisState,
    /// Fixture patch tag ("U.AAA" / "unpatched"); empty otherwise.
    patch_tag: String,
    conflict: bool,
    /// Renameable rows (entity leaves) carry their current name for inline edit.
    renameable: bool,
}

/// The public entry point: draw the whole Scene hierarchy tree. Mutates
/// `selection` / `anchor` / `expanded` / `rename` in place; returns a deferred
/// [`TreeAction`] for anything needing an undo step.
#[allow(clippy::too_many_arguments)]
pub fn scene_tree(
    ui: &mut egui::Ui,
    scene: &Scene,
    selection: &mut Selection,
    patch: &PatchTable,
    anchor: &mut Option<usize>,
    sort: SceneSort,
    search: &str,
    filter: OutlinerFilter,
    expanded: &mut HashSet<NodeKey>,
    rename: &mut Option<(NodeKey, String)>,
) -> TreeAction {
    // Build the derived, flattened tree for THIS frame.
    let q = search.trim().to_lowercase();
    let matches = |name: &str| q.is_empty() || name.to_lowercase().contains(q.as_str());

    // Address conflicts, computed once (ported from the old scene_outliner).
    let mut conflicted = vec![false; scene.fixtures.len()];
    for c in patch.conflicts() {
        if let Some(s) = conflicted.get_mut(c.a) {
            *s = true;
        }
        if let Some(s) = conflicted.get_mut(c.b) {
            *s = true;
        }
    }

    // The fixture data-indices in flattened visible order — the shift-range
    // domain, and the set a Fixtures-group click selects. Composes the fuzzy
    // search with the chip filters (type chip gates the whole kind below; the
    // per-fixture state chips run through `OutlinerFilter::fixture_passes`).
    let visible_fixtures: Vec<usize> = if filter.kind.fixtures() {
        outliner::fixture_order(scene, patch, sort)
            .into_iter()
            .filter(|&i| matches(&scene.fixtures[i].name))
            .filter(|&i| {
                let patched = patch.get(i).is_some_and(|p| p.enabled);
                let selected = selection.contains_fixture(i);
                let conflict = conflicted.get(i).copied().unwrap_or(false);
                filter.fixture_passes(patched, selected, conflict)
            })
            .collect()
    } else {
        Vec::new()
    };
    // Non-fixture kinds carry no patch/conflict/selection-only state, so any
    // active state chip puts the tree in fixture-focus mode and hides them.
    let show_others = !filter.state.any();
    let visible_objects: Vec<usize> = if filter.kind.objects() && show_others {
        (0..scene.geometry.len()).filter(|&i| matches(&scene.geometry[i].name)).collect()
    } else {
        Vec::new()
    };
    let visible_screens: Vec<usize> = if filter.kind.screens() && show_others {
        (0..scene.screens.len()).filter(|&i| matches(&scene.screens[i].name)).collect()
    } else {
        Vec::new()
    };
    let visible_pyro: Vec<usize> = if filter.kind.pyro() && show_others {
        (0..scene.pyro.len()).filter(|&i| matches(&scene.pyro[i].name)).collect()
    } else {
        Vec::new()
    };
    let visible_envs: Vec<usize> = if filter.kind.world() && show_others {
        (0..scene.environments.len()).filter(|&i| matches(&scene.environments[i].name)).collect()
    } else {
        Vec::new()
    };

    let rows = build_rows(
        scene,
        patch,
        filter,
        expanded,
        &conflicted,
        &visible_fixtures,
        &visible_objects,
        &visible_screens,
        &visible_pyro,
        &visible_envs,
    );

    let mut action = TreeAction::None;
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;

    // One outer scroll region, VIRTUALIZED via `show_rows`: only the on-screen
    // slice of the flattened Vec is allocated + interacted each frame, so a fully
    // expanded Objects group with thousands of MVR meshes costs only ~visible-row
    // work, not thousands of widget allocations (the perf gate for large rigs).
    let line_col = ink.tertiary.gamma_multiply(0.20);
    let total = rows.len();
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("scene-tree").show_rows(
        ui,
        ROW_H,
        total,
        |ui, range| {
            let full_w = ui.available_width();
            // `show_rows` parks the cursor at the first VISIBLE row's y; recover the
            // virtual content top so guide-line geometry references absolute indices
            // even for off-screen ancestors/descendants.
            let content_top = ui.cursor().top() - range.start as f32 * ROW_H;
            let left = ui.max_rect().left();
            let row_y = |idx: usize| content_top + idx as f32 * ROW_H;

            // ---- hierarchy guide lines (behind the rows) ----
            // For each expanded container, stroke a vertical from just below its row
            // to the centre of its LAST contiguous descendant (greater depth).
            // Mirrors outliner_draw.cc's hierarchy_lines pass. We only emit segments
            // whose [container, last-descendant] span overlaps the visible row band
            // so a huge collapsed-into-view list doesn't paint thousands of off-
            // screen lines.
            for i in 0..total {
                let r = &rows[i];
                if !r.has_children {
                    continue;
                }
                let mut last = i;
                let mut j = i + 1;
                while j < total && rows[j].depth > r.depth {
                    last = j;
                    j += 1;
                }
                if last == i {
                    continue; // expanded but empty — no line
                }
                // Cull lines entirely outside the visible band (with a 1-row margin).
                if last + 1 < range.start || i > range.end + 1 {
                    continue;
                }
                let x = left + PAD_X + r.depth as f32 * INDENT + DISCLOSURE_W * 0.5;
                let y0 = row_y(i) + ROW_H * 0.5 + ROW_H * 0.25;
                let y1 = row_y(last) + ROW_H * 0.5;
                ui.painter().line_segment([egui::pos2(x, y0), egui::pos2(x, y1)], egui::Stroke::new(1.0, line_col));
            }

            // ---- the on-screen rows ----
            for i in range.clone() {
                draw_row(
                    ui,
                    &rows[i],
                    i,
                    full_w,
                    &ink,
                    accent,
                    scene,
                    selection,
                    anchor,
                    expanded,
                    rename,
                    &visible_fixtures,
                    &visible_objects,
                    &visible_screens,
                    &visible_pyro,
                    &mut action,
                );
            }

        },
    );

    // Click on the empty band BELOW the tree's content clears the selection
    // (Blender outliner). Handled outside `show_rows` (which owns its content
    // height exactly) by interacting with whatever scroll space is left over.
    let rest = ui.available_size_before_wrap();
    if rest.y > 2.0 {
        let (_id, rrect) = ui.allocate_space(egui::vec2(ui.available_width(), rest.y));
        let resp = ui.interact(rrect, ui.id().with("tree-empty"), Sense::click());
        if resp.clicked() {
            *selection = Selection::default();
            *anchor = None;
        }
    }

    action
}

/// Build the flattened, depth-first row list for the currently-expanded state.
/// Containers come before their leaves at each level (Blender ordering).
#[allow(clippy::too_many_arguments)]
fn build_rows(
    scene: &Scene,
    patch: &PatchTable,
    filter: OutlinerFilter,
    expanded: &HashSet<NodeKey>,
    conflicted: &[bool],
    visible_fixtures: &[usize],
    visible_objects: &[usize],
    visible_screens: &[usize],
    visible_pyro: &[usize],
    visible_envs: &[usize],
) -> Vec<TreeRow> {
    use theme::icon;
    let mut rows = Vec::new();
    let open = |k: NodeKey| expanded.contains(&k);

    // 1) Root — the project.
    let total = scene.fixtures.len() + scene.geometry.len() + scene.screens.len() + scene.environments.len();
    rows.push(TreeRow {
        key: NodeKey::Root,
        kind: RowKind::Root,
        depth: 0,
        icon: icon::SCENE,
        label: "Scene".into(),
        secondary: format!("{total} items"),
        has_children: true,
        vis: VisState::fold(
            scene
                .fixtures
                .iter()
                .map(|f| f.hidden)
                .chain(scene.geometry.iter().map(|g| g.hidden))
                .chain(scene.screens.iter().map(|s| s.hidden))
                .chain(scene.environments.iter().map(|e| e.hidden)),
        ),
        patch_tag: String::new(),
        conflict: false,
        renameable: false,
    });
    if !open(NodeKey::Root) {
        return rows;
    }

    // 2) World → Environment → environment leaves. Hidden when a type chip
    // restricts to one entity kind or any state chip narrows to fixtures.
    let world_has = !scene.environments.is_empty();
    if filter.kind.world() && !filter.state.any() {
        rows.push(TreeRow {
            key: NodeKey::World,
            kind: RowKind::World,
            depth: 1,
            icon: icon::WORLD,
            label: "World".into(),
            secondary: "HDRI · ambient".into(),
            has_children: world_has,
            vis: VisState::fold(scene.environments.iter().map(|e| e.hidden)),
            patch_tag: String::new(),
            conflict: false,
            renameable: false,
        });
        if world_has && open(NodeKey::World) {
            rows.push(TreeRow {
                key: NodeKey::EnvGroup,
                kind: RowKind::EnvGroup,
                depth: 2,
                icon: icon::ENVIRONMENT,
                label: "Environment".into(),
                secondary: count_str(scene.environments.len()),
                has_children: !scene.environments.is_empty(),
                vis: VisState::fold(scene.environments.iter().map(|e| e.hidden)),
                patch_tag: String::new(),
                conflict: false,
                renameable: false,
            });
            if open(NodeKey::EnvGroup) {
                for &i in visible_envs {
                    let e = &scene.environments[i];
                    rows.push(TreeRow {
                        key: NodeKey::Entity(e.id),
                        kind: RowKind::Environment(i),
                        depth: 3,
                        icon: icon::ENVIRONMENT,
                        label: e.name.clone(),
                        secondary: "Fog volume".into(),
                        has_children: false,
                        vis: leaf_vis(e.hidden),
                        patch_tag: String::new(),
                        conflict: false,
                        renameable: true,
                    });
                }
            }
        }
    }

    // 3) Fixtures group → fixture leaves (patch tag + conflict badge). Gated by
    // the Fixtures type chip; state chips filter the leaves (via visible_fixtures).
    if filter.kind.fixtures() {
        push_group(
            &mut rows,
            GroupKind::Fixtures,
            icon::FIXTURE,
            "Fixtures",
            visible_fixtures.len(),
            VisState::fold(scene.fixtures.iter().map(|f| f.hidden)),
        );
        if open(NodeKey::Group(GroupKind::Fixtures)) {
            for &i in visible_fixtures {
                let f = &scene.fixtures[i];
                let patch_tag = match patch.get(i).filter(|p| p.enabled) {
                    Some(p) => format!("{}.{:03}", p.universe, p.address),
                    None => "none".into(),
                };
                let row_icon = if f.is_laser { icon::COLOR } else { icon::FIXTURE };
                rows.push(TreeRow {
                    key: NodeKey::Entity(f.id),
                    kind: RowKind::Fixture(i),
                    depth: 2,
                    icon: row_icon,
                    label: f.name.clone(),
                    secondary: f.profile.clone(),
                    has_children: false,
                    vis: leaf_vis(f.hidden),
                    patch_tag,
                    conflict: conflicted.get(i).copied().unwrap_or(false),
                    renameable: true,
                });
            }
        }
    }

    // 4) Objects group → geometry leaves. Hidden by a non-Objects type chip or any
    // state chip (objects carry no patch/conflict state, so the tree narrows away).
    if filter.kind.objects() && !filter.state.any() {
        push_group(
            &mut rows,
            GroupKind::Objects,
            icon::GEOMETRY,
            "Objects",
            visible_objects.len(),
            VisState::fold(scene.geometry.iter().map(|g| g.hidden)),
        );
        if open(NodeKey::Group(GroupKind::Objects)) {
            for &i in visible_objects {
                let g = &scene.geometry[i];
                let kind = g.mvr.as_ref().map(|m| m.kind.as_str()).filter(|k| !k.is_empty()).unwrap_or("Object");
                rows.push(TreeRow {
                    key: NodeKey::Entity(g.id),
                    kind: RowKind::Object(i),
                    depth: 2,
                    icon: icon::GEOMETRY,
                    label: g.name.clone(),
                    secondary: kind.to_string(),
                    has_children: false,
                    vis: leaf_vis(g.hidden),
                    patch_tag: String::new(),
                    conflict: false,
                    renameable: true,
                });
            }
        }
    }

    // 5) Screens group → LED-screen leaves. Gated like Objects.
    if filter.kind.screens() && !filter.state.any() {
        push_group(
            &mut rows,
            GroupKind::Screens,
            icon::SCREEN,
            "Screens",
            visible_screens.len(),
            VisState::fold(scene.screens.iter().map(|x| x.hidden)),
        );
        if open(NodeKey::Group(GroupKind::Screens)) {
            for &i in visible_screens {
                let s = &scene.screens[i];
                let [rx, ry] = s.resolution();
                rows.push(TreeRow {
                    key: NodeKey::Entity(s.id),
                    kind: RowKind::Screen(i),
                    depth: 2,
                    icon: icon::SCREEN,
                    label: s.name.clone(),
                    secondary: format!("{rx}×{ry} · {}", s.content.label()),
                    has_children: false,
                    vis: leaf_vis(s.hidden),
                    patch_tag: String::new(),
                    conflict: false,
                    renameable: true,
                });
            }
        }
    }

    // 6) Pyro group → CO2 cannon + cold-spark leaves. Gated like Objects/Screens.
    if filter.kind.pyro() && !filter.state.any() {
        push_group(
            &mut rows,
            GroupKind::Pyro,
            icon::PYRO,
            "Pyro",
            visible_pyro.len(),
            VisState::fold(scene.pyro.iter().map(|x| x.hidden)),
        );
        if open(NodeKey::Group(GroupKind::Pyro)) {
            for &i in visible_pyro {
                let d = &scene.pyro[i];
                let patch_tag = match d.patch {
                    Some(p) => format!("{}.{:03}", p.universe, p.address),
                    None => "none".into(),
                };
                rows.push(TreeRow {
                    key: NodeKey::Entity(d.id),
                    kind: RowKind::Pyro(i),
                    depth: 2,
                    icon: icon::PYRO,
                    label: d.name.clone(),
                    secondary: d.kind.label().to_string(),
                    has_children: false,
                    vis: leaf_vis(d.hidden),
                    patch_tag,
                    conflict: false,
                    renameable: true,
                });
            }
        }
    }

    rows
}

/// Push a depth-1 group container row (Fixtures / Objects / Screens). A group
/// with zero children still shows its header (Blender keeps empty collections).
fn push_group(rows: &mut Vec<TreeRow>, kind: GroupKind, icon: &'static str, label: &str, count: usize, vis: VisState) {
    rows.push(TreeRow {
        key: NodeKey::Group(kind),
        kind: RowKind::Group(kind),
        depth: 1,
        icon,
        label: label.to_string(),
        secondary: count_str(count),
        has_children: count > 0,
        vis,
        patch_tag: String::new(),
        conflict: false,
        renameable: false,
    });
}

/// A leaf's two-state visibility (leaves are never `Mixed`).
fn leaf_vis(hidden: bool) -> VisState {
    if hidden { VisState::Hidden } else { VisState::Shown }
}

fn count_str(n: usize) -> String {
    if n == 0 { "empty".into() } else { format!("{n}") }
}

/// The fixture's colour in the DMX universe grid (golden-ratio hue) — reused on the
/// outliner's far-left patch column so a fixture reads the SAME colour in both
/// places. Mirrors `panels::fixture_tint`/`hsv_to_color`; kept local to avoid a
/// cross-module `pub` (fold into a shared colour util later).
fn fixture_tint(i: usize) -> Color32 {
    let h = (i as f32 * 0.618_034).fract();
    let (s, v) = (0.55_f32, 0.95_f32);
    let hh = (h * 6.0).floor();
    let f = h * 6.0 - hh;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match (hh as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Draw + interact one flattened row. Selection edits happen in-widget; hide /
/// rename are deferred via `action`.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &mut egui::Ui,
    row: &TreeRow,
    index: usize,
    full_w: f32,
    ink: &theme::Ink,
    accent: Color32,
    scene: &Scene,
    selection: &mut Selection,
    anchor: &mut Option<usize>,
    expanded: &mut HashSet<NodeKey>,
    rename: &mut Option<(NodeKey, String)>,
    visible_fixtures: &[usize],
    visible_objects: &[usize],
    visible_screens: &[usize],
    visible_pyro: &[usize],
    action: &mut TreeAction,
) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(full_w, ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);

    // Is this row selected / the active (primary) one?
    let (selected, active) = row_selection_state(row, selection);

    // ---- background ----
    // Even/odd zebra striping (Blender outliner `TH_ROW_ALTERNATE`): a whisper-quiet
    // tint on odd rows so long fixture lists are easy to scan. Selection + hover paint
    // OVER it. Keyed by the ABSOLUTE visible-row index so the banding stays stable as
    // the virtualized list scrolls.
    if index % 2 == 1 {
        let zebra = if ui.visuals().dark_mode {
            Color32::from_white_alpha(16)
        } else {
            Color32::from_black_alpha(20)
        };
        painter.rect_filled(rect, 0.0, zebra);
    }
    if selected {
        painter.rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
        let w = if active { 1.5 } else { 1.0 };
        let col = if active { accent } else { accent.gamma_multiply(0.6) };
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(w, col), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }
    if active {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.left_top(), egui::vec2(2.5, ROW_H)),
            0.0,
            accent,
        );
    }
    let dim = if row.vis.dim() { 0.45 } else { 1.0 };

    // ---- left geometry: indent → disclosure → icon → name. No left patch column
    // (metadata is right-aligned now), so the name reads with Blender-like room.
    let content_x = rect.left() + PAD_X + row.depth as f32 * INDENT;
    let disc_rect = egui::Rect::from_min_size(egui::pos2(content_x, rect.top()), egui::vec2(DISCLOSURE_W, ROW_H));
    let icon_x = content_x + DISCLOSURE_W + ICON_DX;
    let mut name_x = icon_x + ICON_W + ICON_GAP;

    // ---- disclosure triangle ----
    if row.has_children {
        let glyph = if expanded.contains(&row.key) { theme::icon::TREE_OPEN } else { theme::icon::TREE_CLOSED };
        painter.text(
            disc_rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(12.0),
            ink.tertiary,
        );
    }

    // ---- type icon ----
    painter.text(
        egui::pos2(icon_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        row.icon,
        egui::FontId::proportional(15.0),
        (if selected { accent } else { ink.secondary }).gamma_multiply(dim),
    );

    // ---- leading SEQUENCE number (lighting-console style: the channel number reads
    // before the name) on fixture rows; shifts the name right by its width.
    if let RowKind::Fixture(i) = row.kind {
        let g = painter.layout_no_wrap(
            scene.fixtures[i].sequence.to_string(),
            egui::FontId::monospace(11.0),
            ink.tertiary.gamma_multiply(dim),
        );
        let gw = g.size().x;
        painter.galley(egui::pos2(name_x, rect.center().y - g.size().y * 0.5), g, ink.tertiary);
        name_x += gw + ICON_GAP * 1.5;
    }

    // ---- RIGHT-ALIGNED metadata columns, marched leftward from the right edge:
    // [eye] (always) → [channel "Nch"] → [patch "uni.addr"] (the last two fixtures
    // only, measured, so non-fixture rows reserve only the eye). `reserved_left`
    // becomes the right boundary the name must not cross.
    let mut cursor_r = rect.right() - EDGE_PAD;
    let eye_rect = egui::Rect::from_min_size(egui::pos2(cursor_r - EYE_W, rect.top()), egui::vec2(EYE_W, ROW_H));
    cursor_r -= EYE_W;

    if let RowKind::Fixture(i) = row.kind {
        // Channel footprint ("47ch").
        let g = painter.layout_no_wrap(
            format!("{}ch", crate::dmx::patch::footprint_for(&scene.fixtures[i], scene.fixtures[i].mode_index)),
            egui::FontId::monospace(10.0),
            ink.tertiary.gamma_multiply(dim),
        );
        cursor_r -= COL_GAP + g.size().x;
        painter.galley(egui::pos2(cursor_r, rect.center().y - g.size().y * 0.5), g, ink.tertiary);
    }
    if !row.patch_tag.is_empty() && row.patch_tag != "none" {
        // Patch "uni.addr" chip in the fixture's DMX-pane tint (same golden-ratio hue
        // as the DMX grid), conflict-red. Shown ONLY when actually patched — an
        // unpatched fixture is signalled by the absence of an address (a "none" word
        // would just crowd the row and push the name into truncation; the Unpatched
        // filter chip + the DMX pane are where you go looking for unpatched fixtures).
        let col = if row.conflict {
            theme::CONFLICT
        } else if let RowKind::Fixture(i) = row.kind {
            fixture_tint(i)
        } else {
            ink.tertiary
        };
        let g = painter.layout_no_wrap(row.patch_tag.clone(), egui::FontId::monospace(10.0), col.gamma_multiply(dim));
        cursor_r -= COL_GAP + g.size().x;
        painter.galley(egui::pos2(cursor_r, rect.center().y - g.size().y * 0.5), g, col);
    }
    let reserved_left = cursor_r - COL_GAP;

    // ---- visibility eye (own hit-test) ----
    let eye = ui.interact(eye_rect, resp.id.with("eye"), Sense::click());
    // Mixed (some-but-not-all children hidden) keeps the open eye but tints it
    // muted/accent so the parent reads as "partly hidden" (Blender greys the
    // restriction icon); clicking it hides the remaining visible children.
    let glyph = if row.vis == VisState::Hidden { theme::icon::EYE_OFF } else { theme::icon::EYE };
    let eye_col = match row.vis {
        VisState::Hidden => ink.muted,
        VisState::Mixed => {
            if eye.hovered() {
                ink.primary
            } else {
                ink.muted.gamma_multiply(1.4)
            }
        }
        VisState::Shown => {
            if eye.hovered() {
                ink.primary
            } else {
                ink.tertiary
            }
        }
    };
    painter.text(eye_rect.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(13.0), eye_col);
    eye.clone().on_hover_text(match row.vis {
        VisState::Hidden => "Hidden — click to show",
        VisState::Mixed => "Partly hidden — click to hide the rest",
        VisState::Shown => "Visible — click to hide",
    });

    // ---- name (or inline rename editor) ---- elided to the room left of the
    // right-aligned metadata columns, so it never overflows into them.
    let renaming = rename.as_ref().is_some_and(|(k, _)| *k == row.key);
    let name_avail = (reserved_left - name_x).max(24.0);
    if renaming {
        // A real allocated TextEdit (painter text can't host a cursor). One live
        // at a time; commit on Enter / focus loss, cancel on Esc.
        if let Some((_, buf)) = rename.as_mut() {
            let edit_rect = egui::Rect::from_min_max(
                egui::pos2(name_x, rect.top() + 3.0),
                egui::pos2(reserved_left, rect.bottom() - 3.0),
            );
            let mut commit = false;
            let mut cancel = false;
            ui.scope_builder(egui::UiBuilder::new().max_rect(edit_rect), |ui| {
                let te = ui.put(edit_rect, egui::TextEdit::singleline(buf).margin(egui::vec2(2.0, 0.0)));
                te.request_focus();
                if te.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    } else {
                        commit = true;
                    }
                }
            });
            if commit {
                let name = buf.trim().to_string();
                if !name.is_empty() {
                    *action = TreeAction::Rename(row.key, name);
                }
                *rename = None;
            } else if cancel {
                *rename = None;
            }
        }
    } else {
        // Single line: the type icon already conveys the kind, so the row shows
        // just the NAME (Blender's outliner does the same). The secondary type
        // string moves to the row tooltip instead of a cramped second line.
        super::panels::paint_truncated(
            &painter,
            egui::pos2(name_x, rect.top() + 2.0),
            &row.label,
            13.0,
            ink.primary.gamma_multiply(dim),
            name_avail,
        );
        if !row.secondary.is_empty() {
            resp.clone().on_hover_text(&row.secondary);
        }
    }

    // ---- interaction ----
    if renaming {
        return; // editor owns the row this frame
    }
    if eye.clicked() {
        *action = TreeAction::ToggleHidden(row.key);
        return;
    }
    // Disclosure click toggles expand, never selects.
    let disc = ui.interact(disc_rect, resp.id.with("disc"), Sense::click());
    if row.has_children && disc.clicked() {
        toggle_expand(expanded, row.key);
        return;
    }
    // Double-click a renameable name → start inline rename.
    if row.renameable && resp.double_clicked() {
        *rename = Some((row.key, row.label.clone()));
        return;
    }
    if resp.clicked() {
        let m = ui.input(|x| x.modifiers);
        let cmd = m.command || m.ctrl;
        let shift = m.shift;
        select_row(
            row,
            selection,
            anchor,
            scene,
            shift,
            cmd,
            visible_fixtures,
            visible_objects,
            visible_screens,
            visible_pyro,
        );
    }
}

/// Whether `row` is currently selected, and whether it's the active (primary).
fn row_selection_state(row: &TreeRow, selection: &Selection) -> (bool, bool) {
    match row.kind {
        RowKind::World => (selection.world, selection.world),
        RowKind::Fixture(i) => (selection.contains_fixture(i), selection.primary_fixture() == Some(i)),
        RowKind::Object(i) => (selection.contains_geometry(i), selection.primary_geometry() == Some(i)),
        RowKind::Screen(i) => (selection.contains_screen(i), selection.primary_screen() == Some(i)),
        RowKind::Pyro(i) => (selection.contains_pyro(i), selection.primary_pyro() == Some(i)),
        RowKind::Environment(i) => {
            let s = selection.environment == Some(i);
            (s, s)
        }
        RowKind::Root | RowKind::EnvGroup | RowKind::Group(_) => (false, false),
    }
}

/// Resolve a row click into a selection update, honouring modifiers. Shift-range
/// runs over the flattened VISIBLE order (fixtures only this phase).
#[allow(clippy::too_many_arguments)]
fn select_row(
    row: &TreeRow,
    selection: &mut Selection,
    anchor: &mut Option<usize>,
    scene: &Scene,
    shift: bool,
    cmd: bool,
    visible_fixtures: &[usize],
    visible_objects: &[usize],
    visible_screens: &[usize],
    visible_pyro: &[usize],
) {
    match row.kind {
        RowKind::Root => {
            *selection = Selection::default();
            *anchor = None;
        }
        RowKind::World => {
            *selection = Selection::world();
            *anchor = None;
        }
        RowKind::EnvGroup => { /* container with no aggregate select */ }
        RowKind::Group(GroupKind::Fixtures) => {
            // Select all visible fixtures (replace), or toggle membership on cmd.
            if cmd && selection.fixtures == visible_fixtures {
                selection.fixtures.clear();
            } else {
                selection.fixtures = visible_fixtures.to_vec();
                selection.geometry.clear();
                selection.screens.clear();
                selection.pyro.clear();
                selection.environment = None;
                selection.world = false;
            }
            *anchor = visible_fixtures.first().copied();
        }
        RowKind::Group(GroupKind::Objects) => {
            selection.geometry = visible_objects.to_vec();
            selection.fixtures.clear();
            selection.screens.clear();
            selection.pyro.clear();
            selection.environment = None;
            selection.world = false;
            *anchor = None;
        }
        RowKind::Group(GroupKind::Screens) => {
            selection.screens = visible_screens.to_vec();
            selection.fixtures.clear();
            selection.geometry.clear();
            selection.pyro.clear();
            selection.environment = None;
            selection.world = false;
            *anchor = None;
        }
        RowKind::Group(GroupKind::Pyro) => {
            selection.pyro = visible_pyro.to_vec();
            selection.fixtures.clear();
            selection.geometry.clear();
            selection.screens.clear();
            selection.environment = None;
            selection.world = false;
            *anchor = None;
        }
        RowKind::Fixture(i) => {
            if shift {
                // Range over VISIBLE order — set_fixture_range assumes contiguous
                // data indices, which sort/filter break, so build the slice by hand.
                let click_pos = visible_fixtures.iter().position(|&x| x == i).unwrap_or(0);
                let anchor_pos = anchor
                    .and_then(|a| visible_fixtures.iter().position(|&x| x == a))
                    .unwrap_or(click_pos);
                let (lo, hi) = (anchor_pos.min(click_pos), anchor_pos.max(click_pos));
                selection.fixtures = visible_fixtures[lo..=hi].to_vec();
                selection.geometry.clear();
                selection.screens.clear();
                selection.environment = None;
                selection.world = false;
                if anchor.is_none() {
                    *anchor = Some(i);
                }
            } else {
                apply_fixture_click(selection, anchor, i, false, cmd, scene.fixtures.len());
            }
        }
        RowKind::Object(i) => {
            if cmd {
                selection.toggle_geometry(i);
            } else {
                *selection = Selection::geometry(i);
            }
            *anchor = None;
        }
        RowKind::Screen(i) => {
            if cmd {
                selection.toggle_screen(i);
            } else {
                *selection = Selection::screen(i);
            }
            *anchor = None;
        }
        RowKind::Pyro(i) => {
            if shift {
                // Range over VISIBLE order — same as the Fixture arm. The shared
                // `anchor` holds a bare data-index; if it was set by another kind it
                // simply won't be found in visible_pyro → unwrap_or(click_pos) → a
                // safe single-row select (no cross-kind range, no panic).
                let click_pos = visible_pyro.iter().position(|&x| x == i).unwrap_or(0);
                let anchor_pos = anchor
                    .and_then(|a| visible_pyro.iter().position(|&x| x == a))
                    .unwrap_or(click_pos);
                let (lo, hi) = (anchor_pos.min(click_pos), anchor_pos.max(click_pos));
                selection.pyro = visible_pyro[lo..=hi].to_vec();
                selection.fixtures.clear();
                selection.geometry.clear();
                selection.screens.clear();
                selection.environment = None;
                selection.world = false;
                if anchor.is_none() {
                    *anchor = Some(i);
                }
            } else if cmd {
                selection.toggle_pyro(i);
                *anchor = Some(i);
            } else {
                *selection = Selection::pyro(i);
                *anchor = Some(i);
            }
        }
        RowKind::Environment(i) => {
            // Environment has no toggle helper — plain select only.
            *selection = Selection::environment(i);
            *anchor = None;
        }
    }
}

fn toggle_expand(expanded: &mut HashSet<NodeKey>, key: NodeKey) {
    if !expanded.remove(&key) {
        expanded.insert(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The type chip gates whole entity kinds; `All` shows everything, each kind
    /// shows only itself, and World/Environment only appear under `All`.
    #[test]
    fn type_chip_gates_kinds() {
        assert!(TypeChip::All.fixtures() && TypeChip::All.objects() && TypeChip::All.screens() && TypeChip::All.world());
        assert!(TypeChip::Fixtures.fixtures() && !TypeChip::Fixtures.objects() && !TypeChip::Fixtures.world());
        assert!(TypeChip::Objects.objects() && !TypeChip::Objects.fixtures());
        assert!(TypeChip::Screens.screens() && !TypeChip::Screens.fixtures());
        assert!(!TypeChip::Fixtures.world(), "World only shows under the All chip");
    }

    /// No state chip → every fixture passes (the predicate is the identity).
    #[test]
    fn state_chips_off_pass_all() {
        let f = OutlinerFilter::default();
        for &patched in &[true, false] {
            for &sel in &[true, false] {
                for &conf in &[true, false] {
                    assert!(f.fixture_passes(patched, sel, conf));
                }
            }
        }
    }

    /// Each state chip restricts to its state; the chips compose as AND.
    #[test]
    fn state_chips_filter_and_compose() {
        let unpatched = OutlinerFilter { state: StateChips { unpatched: true, ..Default::default() }, ..Default::default() };
        assert!(unpatched.fixture_passes(false, false, false), "an unpatched fixture passes the Unpatched chip");
        assert!(!unpatched.fixture_passes(true, false, false), "a patched fixture fails the Unpatched chip");

        let selected = OutlinerFilter { state: StateChips { selected: true, ..Default::default() }, ..Default::default() };
        assert!(selected.fixture_passes(false, true, false));
        assert!(!selected.fixture_passes(false, false, false), "an unselected fixture fails the Selected chip");

        let conflicts = OutlinerFilter { state: StateChips { conflicts: true, ..Default::default() }, ..Default::default() };
        assert!(conflicts.fixture_passes(false, false, true));
        assert!(!conflicts.fixture_passes(false, false, false));

        // AND composition: Selected + Conflicts needs BOTH.
        let both = OutlinerFilter {
            state: StateChips { selected: true, conflicts: true, ..Default::default() },
            ..Default::default()
        };
        assert!(both.fixture_passes(false, true, true), "passes only when selected AND conflicting");
        assert!(!both.fixture_passes(false, true, false));
        assert!(!both.fixture_passes(false, false, true));
    }

    /// A state chip puts the tree in fixture-focus mode (`any()` is true), which the
    /// row builder uses to hide non-fixture kinds.
    #[test]
    fn any_state_chip_signals_fixture_focus() {
        assert!(!StateChips::default().any());
        assert!(StateChips { unpatched: true, ..Default::default() }.any());
        assert!(StateChips { conflicts: true, ..Default::default() }.any());
    }
}
