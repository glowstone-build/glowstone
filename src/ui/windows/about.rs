//! The About box — identity, version, and a one-line description.

use egui::RichText;

use crate::ui::theme;

/// The About box.
pub fn about_window(ctx: &egui::Context, open: &mut bool) {
    let mut keep = *open;
    egui::Window::new("About glowstone")
        .open(&mut keep)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            let ink = theme::ink(!ui.visuals().dark_mode);
            let accent = ui.visuals().selection.stroke.color;

            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                // App mark — a large accent glyph above the wordmark.
                ui.label(RichText::new(theme::icon::VIEWPORT).size(48.0).color(accent));
                ui.add_space(2.0);
                ui.label(RichText::new("glowstone").heading().size(26.0).strong().color(ink.primary));
                ui.label(RichText::new("Stage-lighting previsualization").color(ink.secondary));
            });

            ui.add_space(12.0);

            // Version + tech stack — small, weak, monospace metadata rows.
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!("version {}", env!("CARGO_PKG_VERSION")))
                        .monospace()
                        .small()
                        .color(ink.tertiary),
                );
                ui.label(
                    RichText::new("wgpu 29 · winit · egui · egui_dock")
                        .monospace()
                        .small()
                        .color(ink.muted),
                );
                ui.label(
                    RichText::new("GDTF + MVR import/export")
                        .monospace()
                        .small()
                        .color(ink.muted),
                );
            });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            // Description block.
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(
                        "Physically-motivated optical-chain beam engine,\nmulti-emitter fixtures, live Art-Net / sACN.",
                    )
                    .small()
                    .color(ink.secondary),
                );
            });
            ui.add_space(4.0);
        });
    *open = keep;
}
