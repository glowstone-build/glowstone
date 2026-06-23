//! Per-editor HEADER framework (`docs/RESEARCH-blender-framework.md` §2.2).
//!
//! egui_dock gives us the Area split/join/drag/maximize graph; what we add on top
//! is Blender's per-editor *header* region — a compact bar carved from each dock
//! leaf's `ui` BEFORE its main content. The header's first widget is always the
//! editor-type switcher (a menu over [`Tab::ALL`]) that rewrites `*tab` in place;
//! because `Tab: Serialize` lives in `DockState<Tab>`, swapping a leaf's editor
//! type persists for free. Right-aligned per-editor controls follow: the Viewport
//! absorbs the old floating display overlay (Mode + Exposure) and the View-menu
//! display toggles; other editors get a minimal icon+title+switcher header for now.

use egui::RichText;

use super::{PanelViewer, Tab};
use crate::scene::ViewportMode;
use crate::ui::theme;

/// Height the header bar reserves (matches the dense console chrome elsewhere).
const HEADER_H: f32 = 26.0;

/// Draw one editor's header bar at the top of its dock leaf, then return so the
/// caller can draw the main content in the remaining `ui`. The region Id is
/// salted with the tab so two leaves of the same editor type never clash.
pub(super) fn header(viewer: &mut PanelViewer, ui: &mut egui::Ui, tab: &mut Tab) {
    // Salt the region Id with the tab's title so two leaves of the same editor
    // type never clash (Tab isn't Hash; its title is a stable unique &str).
    let id = ui.id().with(("editor-header", tab.title()));
    // Snapshot the open-type set so the switcher can refuse a duplicate (cloned to
    // avoid borrowing `viewer` across the closure that also needs it mutably).
    let open = viewer.open_tabs.clone();
    egui::TopBottomPanel::top(id)
        .exact_height(HEADER_H)
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                // First widget — the editor-type switcher (Blender's spacetype menu).
                editor_type_switcher(ui, tab, &open);
                ui.separator();
                // Right-aligned per-editor controls.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Per-editor controls (only the Viewport has any for now).
                    if *tab == Tab::Viewport {
                        viewport_controls(ui, viewer);
                    }
                });
            });
        });
}

/// The editor-type switcher: a small icon menu over every [`Tab`] variant that
/// rewrites `*tab` in place, swapping which panel renders in this leaf.
fn editor_type_switcher(ui: &mut egui::Ui, tab: &mut Tab, open: &[Tab]) {
    let label = format!("{}  {}", tab.icon(), tab.title());
    ui.menu_button(RichText::new(label).small(), |ui| {
        for t in Tab::ALL {
            let selected = *tab == t;
            // Refuse a type that already has a leaf elsewhere: two leaves of one
            // type clash ids in egui_dock (leaf id derives from the title). The
            // current type stays selectable (switching to it is a harmless no-op).
            let available = selected || !open.contains(&t);
            let resp = ui.add_enabled(
                available,
                egui::SelectableLabel::new(selected, format!("{}  {}", t.icon(), t.title())),
            );
            let resp = if available && !selected {
                resp.on_hover_text("Switch this editor to this type")
            } else if !available {
                resp.on_hover_text("Already open in another panel")
            } else {
                resp
            };
            if resp.clicked() {
                *tab = t;
                ui.close();
            }
        }
    });
}

/// The Viewport header's right-aligned controls — the display Mode segmented
/// buttons + Exposure (migrated from the old floating "viewport-display-overlay"
/// `egui::Area`) and the View-menu display toggles (Grid / Beam gizmos). These
/// are the controls a designer reaches for constantly, kept where the eyes are.
/// (Drawn right-to-left, so widgets read in reverse declaration order on screen.)
fn viewport_controls(ui: &mut egui::Ui, viewer: &mut PanelViewer) {
    // N / T region toggles (right-most) — mirror the N/T keys (§2.2). Drawn first
    // (right_to_left layout) so they sit at the far right edge of the header.
    let regions = &mut *viewer.viewport_regions;
    if ui
        .selectable_label(regions.n_open, RichText::new(theme::icon::N_PANEL).small())
        .on_hover_text("N-panel (sidebar) — N")
        .clicked()
    {
        regions.n_open = !regions.n_open;
    }
    if ui
        .selectable_label(regions.t_open, RichText::new(theme::icon::T_PANEL).small())
        .on_hover_text("T-panel (tool rail) — T")
        .clicked()
    {
        regions.t_open = !regions.t_open;
    }
    ui.separator();

    let settings = &mut *viewer.settings;
    // Beam gizmos + grid toggles (right-most).
    ui.checkbox(&mut settings.show_beam_wireframes, RichText::new("Beam").small())
        .on_hover_text("Beam gizmo wireframes");
    ui.checkbox(&mut settings.show_grid, RichText::new("Grid").small());
    ui.separator();
    // Exposure.
    ui.add(
        egui::DragValue::new(&mut settings.exposure)
            .speed(0.01)
            .range(0.05..=8.0),
    )
    .on_hover_text("Exposure");
    ui.label(RichText::new("Exp").small().weak());
    ui.separator();
    // Display Mode segmented (declared last → drawn left-most of the controls).
    for m in ViewportMode::ALL.iter().rev() {
        if ui
            .selectable_label(settings.mode == *m, RichText::new(m.label()).small())
            .clicked()
        {
            settings.mode = *m;
        }
    }
}
