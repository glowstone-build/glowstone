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

use super::{PanelViewer, PivotMode, Tab, TransformOrientation};
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
    // Fullscreen toggle (far right) — OS window fullscreen, mirrors F11. Declared
    // first so, right-to-left, it sits at the very right edge of the header.
    if ui
        .button(RichText::new(theme::icon::FULLSCREEN).small())
        .on_hover_text("Toggle fullscreen — F11")
        .clicked()
    {
        *viewer.pending_fullscreen = true;
    }
    ui.separator();

    // T region toggle (right-most) — mirrors the T key (§2.2). Drawn first
    // (right_to_left layout) so it sits at the far right edge of the header. (The
    // N-panel button was removed with the inline inspector; `N` now toggles the
    // docked Inspector tab.)
    let regions = &mut *viewer.viewport_regions;
    if ui
        .selectable_label(regions.t_open, RichText::new(theme::icon::T_PANEL).small())
        .on_hover_text("T-panel (tool rail) — T")
        .clicked()
    {
        regions.t_open = !regions.t_open;
    }
    ui.separator();

    // Transform-tool options (§2.4 #4/#5): pivot-point selector + grid/increment
    // snap toggle & step. Drawn here (right_to_left) so they read, left→right:
    // [pivot ▾] [⊞ Snap] [step]. The gizmo + modal G/R/S read these when building a
    // TransformOp. Borrow ends before the next `viewer` use.
    transform_options(ui, &mut *viewer.xform);
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

/// The transform-tool options cluster (§2.4 #4 snap, #5 pivot). Right-to-left, so
/// declared in reverse on-screen order: the snap-increments menu (right), the Snap
/// toggle, then the pivot-point combo (left). All three per-type increments
/// (Move m / Rotate ° / Scale ×) live behind a compact "Step" menu so the dense
/// header stays uncluttered; the toggle + pivot are the constant-reach controls.
fn transform_options(ui: &mut egui::Ui, xform: &mut super::TransformPrefs) {
    // Per-type snap increments — tucked behind a menu (greyed while Snap is off).
    ui.add_enabled_ui(xform.snap.on, |ui| {
        ui.menu_button(RichText::new("Step").small(), |ui| {
            ui.label(RichText::new("Snap increments").small().weak());
            ui.add(
                egui::DragValue::new(&mut xform.snap.move_step)
                    .speed(0.05)
                    .range(0.001..=1000.0)
                    .prefix("Move  ")
                    .suffix(" m"),
            );
            ui.add(
                egui::DragValue::new(&mut xform.snap.rotate_deg)
                    .speed(1.0)
                    .range(0.1..=180.0)
                    .prefix("Rotate  ")
                    .suffix("°"),
            );
            ui.add(
                egui::DragValue::new(&mut xform.snap.scale_step)
                    .speed(0.01)
                    .range(0.001..=10.0)
                    .prefix("Scale  ")
                    .suffix("×"),
            );
        })
        .response
        .on_hover_text("Per-type snap increments (Move / Rotate / Scale)");
    });
    // Snap MODE selector (#71): what a Move snap targets — Grid / Vertex / Surface.
    // Greyed while Snap is off (it only matters with snapping live). Declared before
    // the toggle so, right-to-left, it sits to the RIGHT of the [⊞ Snap] button.
    ui.add_enabled_ui(xform.snap.on, |ui| {
        egui::ComboBox::from_id_salt("viewport-snap-mode")
            .selected_text(RichText::new(xform.snap.mode.label()).small())
            .width(76.0)
            .show_ui(ui, |ui| {
                for m in super::SnapMode::ALL {
                    if ui
                        .selectable_label(xform.snap.mode == m, m.label())
                        .on_hover_text(m.hint())
                        .clicked()
                    {
                        xform.snap.mode = m;
                    }
                }
            })
            .response
            .on_hover_text("Snap mode — Grid increment / Vertex (nearest origin) / Surface (under cursor)");
    });
    // Snap toggle (Ctrl held mid-drag inverts it — see apply_transform).
    if ui
        .selectable_label(xform.snap.on, RichText::new(format!("{} Snap", theme::icon::SNAP)).small())
        .on_hover_text("Grid/increment snap — hold Ctrl mid-drag to invert")
        .clicked()
    {
        xform.snap.on = !xform.snap.on;
    }
    // Pivot-point selector.
    egui::ComboBox::from_id_salt("viewport-pivot")
        .selected_text(RichText::new(xform.pivot.label()).small())
        .width(120.0)
        .show_ui(ui, |ui| {
            for m in PivotMode::ALL {
                if ui
                    .selectable_label(xform.pivot == m, m.label())
                    .on_hover_text(m.hint())
                    .clicked()
                {
                    xform.pivot = m;
                }
            }
        })
        .response
        .on_hover_text("Pivot point — what rotate/scale transforms about");
    // Transform-orientation selector (#37): the basis the move/rotate/scale axis is
    // expressed in (Global world axes / Local element axes / View camera axes).
    egui::ComboBox::from_id_salt("viewport-orient")
        .selected_text(RichText::new(xform.orientation.label()).small())
        .width(80.0)
        .show_ui(ui, |ui| {
            for o in TransformOrientation::ALL {
                if ui
                    .selectable_label(xform.orientation == o, o.label())
                    .on_hover_text(o.hint())
                    .clicked()
                {
                    xform.orientation = o;
                }
            }
        })
        .response
        .on_hover_text("Transform orientation — which axes move/rotate/scale follow");
}
