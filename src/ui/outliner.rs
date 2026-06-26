//! The Scene outliner panel: the fixture/object/screen/environment hierarchy
//! tree, its sort + filter chrome, and the saved-selection Groups folder.
//! Extracted from [`super::panels`]; the dock routes the `Tab::Scene` arm here.

use egui::RichText;

use super::theme;
use super::SelectionGroup;
use crate::dmx::PatchTable;
use crate::scene::{Scene, Selection};

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
    pub(super) fn label(self) -> &'static str {
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
