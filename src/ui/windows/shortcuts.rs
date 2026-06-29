//! The keyboard-shortcuts cheat sheet window — rendered ENTIRELY from the central
//! [`shortcuts::KEYMAPS`] registry, grouped by [`shortcuts::Category`]. There is
//! no hardcoded list here: adding an item to a keymap updates this sheet too.

use egui::{Frame, Grid, RichText};

use crate::ui::shortcuts::{self, Category};
use crate::ui::theme;

/// The keyboard-shortcuts cheat sheet.
pub fn shortcuts_window(ctx: &egui::Context, open: &mut bool) {
    let mut keep = *open;
    let max_h = ctx.input(|i| i.content_rect().height()) * 0.8;
    egui::Window::new("Keyboard Shortcuts")
        .vscroll(true)
        .max_height(max_h)
        .open(&mut keep)
        .resizable(false)
        .show(ctx, |ui| {
            let ink = theme::ink(!ui.visuals().dark_mode);
            let pill = ui.visuals().extreme_bg_color;

            // The registry has no row for mouse navigation, so show it first as a
            // small fixed preamble (it isn't a key bind), then everything else
            // comes straight from BINDINGS grouped by category.
            theme::section(ui, Category::Navigation.title());
            ui.add_space(2.0);
            key_grid(
                ui,
                "nav-mouse",
                ink.secondary,
                ink.primary,
                pill,
                &[
                    (
                        "Orbit / Pan / Zoom",
                        "drag / Shift+drag / scroll".to_string(),
                    ),
                    (
                        "Select (multi / range)",
                        "click / ⌘·Ctrl·Shift+click".to_string(),
                    ),
                ],
            );

            let mut first = true;
            for cat in Category::ORDER {
                if cat == Category::Navigation {
                    continue; // handled above (mouse preamble)
                }
                // Collect this category's rows from every keymap (Global +
                // Viewport + Modal), preserving keymap + in-keymap order.
                let rows: Vec<(&str, String)> = shortcuts::KEYMAPS
                    .iter()
                    .flat_map(|km| km.items.iter())
                    .filter(|kmi| kmi.category() == cat)
                    .map(|kmi| (kmi.label(), shortcuts::key_label(&kmi.trigger)))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                if first {
                    ui.add_space(8.0);
                }
                first = false;
                theme::section(ui, cat.title());
                ui.add_space(2.0);
                key_grid(ui, cat.title(), ink.secondary, ink.primary, pill, &rows);
                ui.add_space(8.0);
            }
        });
    *open = keep;
}

/// A two-column grid: action description + a monospace key badge on a quiet pill.
fn key_grid(
    ui: &mut egui::Ui,
    id: &str,
    action_ink: egui::Color32,
    key_ink: egui::Color32,
    pill: egui::Color32,
    rows: &[(&str, String)],
) {
    Grid::new(id)
        .num_columns(2)
        .spacing([24.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (action, key) in rows {
                ui.label(RichText::new(*action).color(action_ink));
                Frame::new()
                    .fill(pill)
                    .inner_margin(egui::Margin::symmetric(6, 1))
                    .corner_radius(egui::CornerRadius::same(3))
                    .show(ui, |ui| {
                        ui.label(RichText::new(key).monospace().color(key_ink));
                    });
                ui.end_row();
            }
        });
}
