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

use super::panels::{self, SceneSort};
use super::theme;
use crate::dmx::PatchTable;
use crate::scene::{apply_fixture_click, EntityId, Scene, Selection};

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
const ROW_H: f32 = 34.0; // dense two-line row (name + secondary), matches entity_row
const INDENT: f32 = 14.0; // px per depth level
const PAD_X: f32 = 4.0; // left gutter before depth 0
const DISCLOSURE_W: f32 = 16.0; // disclosure-triangle cell width
const ICON_DX: f32 = 6.0; // gap between disclosure cell and the type icon
const TEXT_DX: f32 = 21.0; // gap between icon origin and the name text

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
    // domain, and the set a Fixtures-group click selects.
    let visible_fixtures: Vec<usize> =
        panels::fixture_order(scene, patch, sort).into_iter().filter(|&i| matches(&scene.fixtures[i].name)).collect();
    let visible_objects: Vec<usize> =
        (0..scene.geometry.len()).filter(|&i| matches(&scene.geometry[i].name)).collect();
    let visible_screens: Vec<usize> =
        (0..scene.screens.len()).filter(|&i| matches(&scene.screens[i].name)).collect();
    let visible_envs: Vec<usize> =
        (0..scene.environments.len()).filter(|&i| matches(&scene.environments[i].name)).collect();

    let rows = build_rows(
        scene,
        patch,
        expanded,
        &conflicted,
        &visible_fixtures,
        &visible_objects,
        &visible_screens,
        &visible_envs,
    );

    let mut action = TreeAction::None;
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;

    // One outer scroll region, VIRTUALIZED via `show_rows`: only the on-screen
    // slice of the flattened Vec is allocated + interacted each frame, so a fully
    // expanded Objects group with thousands of MVR meshes costs only ~visible-row
    // work, not thousands of widget allocations (the perf gate for large rigs).
    let line_col = ink.tertiary.gamma_multiply(0.5);
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
                let x = left + PAD_X + (r.depth as f32 + 1.0) * INDENT + DISCLOSURE_W * 0.5;
                let y0 = row_y(i) + ROW_H * 0.5 + ROW_H * 0.30;
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
    expanded: &HashSet<NodeKey>,
    conflicted: &[bool],
    visible_fixtures: &[usize],
    visible_objects: &[usize],
    visible_screens: &[usize],
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

    // 2) World → Environment → environment leaves.
    let world_has = !scene.environments.is_empty();
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

    // 3) Fixtures group → fixture leaves (patch tag + conflict badge).
    push_group(
        &mut rows,
        GroupKind::Fixtures,
        icon::FIXTURE,
        "Fixtures",
        scene.fixtures.len(),
        VisState::fold(scene.fixtures.iter().map(|f| f.hidden)),
    );
    if open(NodeKey::Group(GroupKind::Fixtures)) {
        for &i in visible_fixtures {
            let f = &scene.fixtures[i];
            let patch_tag = match patch.get(i).filter(|p| p.enabled) {
                Some(p) => format!("{}.{:03}", p.universe, p.address),
                None => "unpatched".into(),
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

    // 4) Objects group → geometry leaves.
    push_group(
        &mut rows,
        GroupKind::Objects,
        icon::GEOMETRY,
        "Objects",
        scene.geometry.len(),
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

    // 5) Screens group → LED-screen leaves.
    push_group(
        &mut rows,
        GroupKind::Screens,
        icon::SCREEN,
        "Screens",
        scene.screens.len(),
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
    action: &mut TreeAction,
) {
    let _ = index;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(full_w, ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);

    // Is this row selected / the active (primary) one?
    let (selected, active) = row_selection_state(row, selection);

    // ---- background ----
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

    // ---- geometry ----
    let content_x = rect.left() + PAD_X + row.depth as f32 * INDENT;
    let disc_rect = egui::Rect::from_min_size(egui::pos2(content_x, rect.top()), egui::vec2(DISCLOSURE_W, ROW_H));
    let icon_x = content_x + DISCLOSURE_W + ICON_DX;
    let text_x = icon_x + TEXT_DX;

    // ---- disclosure triangle ----
    if row.has_children {
        let glyph = if expanded.contains(&row.key) { theme::icon::TREE_OPEN } else { theme::icon::TREE_CLOSED };
        painter.text(
            disc_rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(13.0),
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

    // ---- far-right visibility eye (own hit-test) ----
    let eye_rect =
        egui::Rect::from_min_size(egui::pos2(rect.right() - 27.0, rect.top()), egui::vec2(24.0, ROW_H));
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
    painter.text(eye_rect.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(14.0), eye_col);
    eye.clone().on_hover_text(match row.vis {
        VisState::Hidden => "Hidden — click to show",
        VisState::Mixed => "Partly hidden — click to hide the rest",
        VisState::Shown => "Visible — click to hide",
    });
    let right_edge = 30.0;

    // ---- name + secondary (or inline rename editor) ----
    let renaming = rename.as_ref().is_some_and(|(k, _)| *k == row.key);
    let text_w = (rect.right() - text_x - 56.0 - right_edge).max(40.0);
    if renaming {
        // A real allocated TextEdit (painter text can't host a cursor). One live
        // at a time; commit on Enter / focus loss, cancel on Esc.
        if let Some((_, buf)) = rename.as_mut() {
            let edit_rect = egui::Rect::from_min_max(
                egui::pos2(text_x, rect.top() + 5.0),
                egui::pos2(rect.right() - right_edge - 4.0, rect.bottom() - 5.0),
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
        super::panels::paint_truncated(
            &painter,
            egui::pos2(text_x, rect.top() + 4.0),
            &row.label,
            13.0,
            ink.primary.gamma_multiply(dim),
            text_w,
        );
        super::panels::paint_truncated(
            &painter,
            egui::pos2(text_x, rect.top() + 19.0),
            &row.secondary,
            10.5,
            ink.tertiary.gamma_multiply(dim),
            text_w,
        );
    }

    // ---- right column: patch tag + conflict (fixtures) ----
    if !row.patch_tag.is_empty() {
        let unpatched = row.patch_tag == "unpatched";
        painter.text(
            egui::pos2(rect.right() - right_edge, rect.top() + 6.0),
            egui::Align2::RIGHT_TOP,
            &row.patch_tag,
            egui::FontId::monospace(10.0),
            if unpatched { ink.muted } else { ink.tertiary },
        );
    }
    if row.conflict {
        painter.text(
            egui::pos2(rect.right() - right_edge, rect.bottom() - 5.0),
            egui::Align2::RIGHT_BOTTOM,
            theme::icon::WARNING,
            egui::FontId::proportional(12.0),
            theme::CONFLICT,
        );
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
                selection.environment = None;
                selection.world = false;
            }
            *anchor = visible_fixtures.first().copied();
        }
        RowKind::Group(GroupKind::Objects) => {
            selection.geometry = visible_objects.to_vec();
            selection.fixtures.clear();
            selection.screens.clear();
            selection.environment = None;
            selection.world = false;
            *anchor = None;
        }
        RowKind::Group(GroupKind::Screens) => {
            selection.screens = visible_screens.to_vec();
            selection.fixtures.clear();
            selection.geometry.clear();
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
