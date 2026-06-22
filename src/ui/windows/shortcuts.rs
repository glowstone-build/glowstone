//! The keyboard-shortcuts cheat sheet window.

use egui::{Frame, Grid, RichText};

use crate::ui::theme;

/// The keyboard-shortcuts cheat sheet.
pub fn shortcuts_window(ctx: &egui::Context, open: &mut bool) {
    let mut keep = *open;
    egui::Window::new("Keyboard Shortcuts")
        .open(&mut keep)
        .resizable(false)
        .show(ctx, |ui| {
            // Grouped by intent so the sheet scans top-to-bottom. All entries
            // from the original flat grid are preserved, re-bucketed.
            let sections: [(&str, &[(&str, &str)]); 5] = [
                (
                    "NAVIGATION",
                    &[("Orbit / Pan / Zoom", "drag / shift+drag / scroll")],
                ),
                (
                    "SELECTION",
                    &[
                        ("Select fixture / object", "click  (⌘/Ctrl = multi, Shift = range)"),
                        ("Select all fixtures", "A"),
                        ("Quick-select menu (empty selection)", "S"),
                        ("Replace selected fixtures", "Shift+R"),
                        ("Deselect all", "Esc"),
                    ],
                ),
                (
                    "TRANSFORM",
                    &[
                        ("Move / Rotate / Scale selection", "G / R / S   (then X·Y·Z to lock)"),
                        ("  confirm / cancel transform", "click·Enter / Esc·right-click"),
                        ("Nudge selected (floor / height)", "arrows / PageUp·Down  (Shift = 1 m)"),
                        ("Duplicate / Array", "D"),
                        ("Delete selected", "Delete / Backspace"),
                    ],
                ),
                (
                    "VIEW",
                    &[
                        ("Frame selection / all", "F / Shift+F"),
                        ("Top / Front / Right / Persp view", "numpad 7 / 1 / 3 / 5"),
                        ("Toggle fixture labels", "L"),
                    ],
                ),
                ("APP", &[("Preferences", "⌘/Ctrl+,")]),
            ];

            let ink = theme::ink(!ui.visuals().dark_mode);
            let pill = ui.visuals().extreme_bg_color;

            for (i, (title, rows)) in sections.iter().enumerate() {
                if i > 0 {
                    ui.add_space(8.0);
                }
                theme::section(ui, title);
                ui.add_space(2.0);
                Grid::new(*title)
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for (action, key) in rows.iter() {
                            ui.label(RichText::new(*action).color(ink.secondary));
                            // The key as a monospace badge on a quiet pill.
                            Frame::new()
                                .fill(pill)
                                .inner_margin(egui::Margin::symmetric(6, 1))
                                .corner_radius(egui::CornerRadius::same(3))
                                .show(ui, |ui| {
                                    ui.label(RichText::new(*key).monospace().color(ink.primary));
                                });
                            ui.end_row();
                        }
                    });
            }
        });
    *open = keep;
}
