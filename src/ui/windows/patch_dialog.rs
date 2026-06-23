//! The Patch dialog (the viewport / Scene `P` key). Picks a starting universe +
//! address; on confirm the caller assigns sequential addresses to the selected
//! fixtures (committed after the dock, where the patch table is reachable —
//! mirrors `commit_delete`). Esc cancels, Enter confirms. State lives on `Ui`.

use crate::ui::theme;

/// State of the open Patch dialog. `open == false` = closed. The starting
/// universe/address are seeded to the next-free slot when the dialog opens.
#[derive(Default)]
pub struct PatchDialog {
    pub open: bool,
    /// How many fixtures will be (re)patched — shown in the prompt.
    pub count: usize,
    /// 1-based universe to start packing from.
    pub start_universe: u16,
    /// 1-based start channel to start packing from.
    pub start_address: u16,
}

/// Render the modeless Patch dialog. Returns `true` exactly once on confirm (the
/// frame the user clicks Patch / presses Enter); the caller then performs the
/// assignment and the window auto-closes. Esc / Cancel close without confirming.
pub fn patch_dialog_window(ctx: &egui::Context, dlg: &mut PatchDialog) -> bool {
    if !dlg.open {
        return false;
    }
    let mut confirm = false;
    let mut close = false;

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    // Enter confirms (unless a text field owns it — the dialog has none).
    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        confirm = true;
    }

    egui::Window::new(format!("{}  Patch", theme::icon::PATCH))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Patch {} fixture{} from:",
                dlg.count,
                if dlg.count == 1 { "" } else { "s" }
            ));
            ui.add_space(4.0);
            egui::Grid::new("patch-grid")
                .num_columns(2)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Universe");
                    let mut u = dlg.start_universe as i32;
                    if ui
                        .add(egui::DragValue::new(&mut u).speed(0.1).range(1..=63999))
                        .changed()
                    {
                        dlg.start_universe = u.clamp(1, 63999) as u16;
                    }
                    ui.end_row();
                    ui.label("Address");
                    let mut a = dlg.start_address as i32;
                    if ui
                        .add(egui::DragValue::new(&mut a).speed(0.5).range(1..=512))
                        .changed()
                    {
                        dlg.start_address = a.clamp(1, 512) as u16;
                    }
                    ui.end_row();
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Patch").clicked() {
                    confirm = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if confirm || close {
        dlg.open = false;
    }
    confirm
}
