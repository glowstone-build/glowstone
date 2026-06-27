//! The Unpatch confirm dialog (the viewport / Scene `U` key). On confirm the
//! caller disables the patch entries for the selected fixtures (committed after
//! the dock, where the patch table is reachable — mirrors `commit_delete`).
//! Esc / Cancel close without confirming; Enter confirms. State lives on `Ui`.

use crate::scene::SelKind;
use crate::ui::theme;

/// State of the open Unpatch dialog. `open == false` = closed.
#[derive(Default)]
pub struct UnpatchDialog {
    pub open: bool,
    /// How many entities will be unpatched — shown in the prompt.
    pub count: usize,
    /// Which selection kind is being unpatched (drives the confirm branch + noun).
    pub kind: SelKind,
}

/// Render the modeless Unpatch confirm. Returns `true` exactly once on confirm;
/// the caller then disables the selected fixtures' patch entries and the window
/// auto-closes. Esc / Cancel close without confirming.
pub fn unpatch_dialog_window(ctx: &egui::Context, dlg: &mut UnpatchDialog) -> bool {
    if !dlg.open {
        return false;
    }
    let mut confirm = false;
    let mut close = false;

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    // Enter confirms — but NOT a value-commit Enter leaking from another panel
    // (bug 8): skip when a text field was focused at frame start, and consume it.
    if !crate::ui::text_focus_active(ctx)
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        confirm = true;
    }

    egui::Window::new(format!("{}  Unpatch", theme::icon::PATCH))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "Unpatch {} {}{}?",
                dlg.count,
                super::patch_dialog::patch_noun(dlg.kind),
                if dlg.count == 1 { "" } else { "s" }
            ));
            ui.label(
                egui::RichText::new("They stop decoding and free their channels.")
                    .weak()
                    .small(),
            );
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Unpatch").clicked() {
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
