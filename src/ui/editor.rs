//! Per-editor HEADER region (`docs/RESEARCH-blender-framework.md` §2.2).
//!
//! egui_dock gives us the Area split/join/drag/maximize graph AND a tab strip that
//! already names each leaf. So — unlike Blender, which has a header *instead of* a
//! tab — only the **Viewport** carries a header bar, holding the display controls
//! (Mode + Exposure + Grid/Beam + N/T toggles) where the eyes are. Every other
//! editor is named by its dock TAB alone: no header bar and no internal
//! `ui.heading`, so each panel's name appears exactly ONCE (the earlier
//! tab + header + heading triple-label is gone).

use egui::RichText;

use super::{PanelViewer, Tab};
use crate::scene::ViewportMode;
use crate::ui::theme;

/// Height the viewport header bar reserves (matches the dense console chrome).
const HEADER_H: f32 = 26.0;

/// Draw the Viewport's header bar (display controls), then return so the caller
/// draws the 3D content in the remaining `ui`. Non-viewport editors get NO header
/// — their dock tab is the single source of the panel's name.
pub(super) fn header(viewer: &mut PanelViewer, ui: &mut egui::Ui, tab: &mut Tab) {
    if *tab != Tab::Viewport {
        return;
    }
    egui::TopBottomPanel::top(ui.id().with("viewport-header"))
        .exact_height(HEADER_H)
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                // Right-aligned display controls (Blender keeps shading controls in
                // the viewport header).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    viewport_controls(ui, viewer);
                });
            });
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
