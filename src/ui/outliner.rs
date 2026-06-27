//! The Scene outliner panel: the fixture/object/screen/environment hierarchy
//! tree, with its sort + filter chrome.
//! Extracted from [`super::panels`]; the dock routes the `Tab::Scene` arm here.

use egui::RichText;

use super::theme;
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


    // (Render/look controls live on the viewport overlay (Mode + Exposure), the
    // View menu (grid / gizmo / label toggles) and Preferences > Rendering — not
    // duplicated here, so the Scene panel stays a clean outliner.)
}
