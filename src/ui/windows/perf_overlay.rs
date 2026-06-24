//! Performance overlay — a flight-sim-style HUD (egui::Area, top-right) showing the
//! frame rate, a per-GPU-pass time breakdown (so you can see what's expensive), what
//! the renderer is actually drawing, and the quality↔perf levers inline so you can
//! react to the numbers without leaving the view.

use egui::{Color32, RichText, Sense, Slider};

use crate::renderer::{gpu_timer, PassTimings};
use crate::scene::RenderSettings;
use crate::ui::theme;

/// FPS / frame-ms three-tier colour (matches the existing FPS overlay).
fn fps_color(fps: f32) -> Color32 {
    if fps >= 55.0 {
        theme::OK
    } else if fps >= 30.0 {
        theme::WARN
    } else {
        theme::CONFLICT
    }
}

/// Per-pass ms colour, absolute tiers (a pass over ~4 ms is worth attention).
fn pass_color(ms: f32) -> Color32 {
    if ms < 1.5 {
        theme::OK
    } else if ms < 4.0 {
        theme::WARN
    } else {
        theme::CONFLICT
    }
}

pub fn perf_overlay_window(
    ctx: &egui::Context,
    open: &mut bool,
    timings: &PassTimings,
    settings: &mut RenderSettings,
) {
    let ink = theme::ink(!ctx.style().visuals.dark_mode);
    let sr = ctx.screen_rect();
    egui::Window::new(format!("{}  Performance", theme::icon::PERF))
        // `.open` gives the title-bar close [x] (clears `show_perf`); a Window is
        // draggable/collapsible by its title bar (vs the old fixed Area HUD).
        .open(open)
        .default_pos(egui::pos2(sr.right() - 252.0, 44.0))
        .default_width(236.0)
        .resizable(false)
        .show(ctx, |ui| {
            {
                    ui.set_max_width(232.0);

                    // --- header: big FPS + frame-ms + render resolution ---
                    let fps = if timings.frame_ms > 0.01 {
                        1000.0 / timings.frame_ms
                    } else {
                        0.0
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{fps:.0}"))
                                .size(26.0)
                                .monospace()
                                .strong()
                                .color(fps_color(fps)),
                        );
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(format!("{:.1} ms", timings.frame_ms))
                                    .monospace()
                                    .color(ink.secondary),
                            );
                            let (rw, rh) = timings.render_px;
                            ui.label(
                                RichText::new(format!("{rw}×{rh}"))
                                    .small()
                                    .monospace()
                                    .color(ink.muted),
                            );
                        });
                    });

                    ui.add_space(4.0);
                    // --- what's being rendered ---
                    ui.label(RichText::new("RENDERING").small().color(ink.muted));
                    let count = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(k).small().monospace().color(ink.muted));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(v).small().monospace().color(ink.secondary));
                            });
                        });
                    };
                    count(ui, "fixtures", format!("{}", timings.fixtures));
                    count(ui, "beams (lit)", format!("{}", timings.beams));
                    count(ui, "shadow maps", format!("{}", timings.shadow_maps));
                    count(ui, "geom draws", format!("{}", timings.geom_draws));

                    ui.add_space(4.0);
                    // --- per-pass GPU time breakdown ---
                    ui.label(RichText::new("GPU PASSES (ms)").small().color(ink.muted));
                    if timings.gpu_valid {
                        let full = timings
                            .passes
                            .iter()
                            .cloned()
                            .fold(2.0f32, f32::max); // bar full-scale = busiest pass (≥2ms)
                        for (i, (name, _sub)) in gpu_timer::BAR_LABELS.iter().enumerate() {
                            let ms = timings.passes[i];
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [82.0, 13.0],
                                    egui::Label::new(
                                        RichText::new(*name).small().monospace().color(if ms <= 0.001 {
                                            ink.muted
                                        } else {
                                            ink.secondary
                                        }),
                                    )
                                    .truncate(),
                                );
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(96.0, 9.0), Sense::hover());
                                // track
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    Color32::from_white_alpha(18),
                                );
                                let frac = (ms / full).clamp(0.0, 1.0);
                                if frac > 0.0 {
                                    let fill = egui::Rect::from_min_size(
                                        rect.min,
                                        egui::vec2(rect.width() * frac, rect.height()),
                                    );
                                    ui.painter().rect_filled(fill, 2.0, pass_color(ms));
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let txt = if ms <= 0.001 {
                                            "—".to_string()
                                        } else {
                                            format!("{ms:.2}")
                                        };
                                        ui.label(
                                            RichText::new(txt).small().monospace().color(ink.primary),
                                        );
                                    },
                                );
                            });
                        }
                    } else {
                        ui.label(
                            RichText::new("per-pass GPU timing unavailable on this adapter")
                                .small()
                                .italics()
                                .color(ink.muted),
                        );
                    }

                    ui.add_space(4.0);
                    // --- quality / performance levers ---
                    ui.label(RichText::new("QUALITY ↔ PERFORMANCE").small().color(ink.muted));
                    ui.spacing_mut().slider_width = 110.0;
                    ui.add(
                        Slider::new(&mut settings.render_scale, 0.5..=1.0)
                            .text("render scale")
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    )
                    .on_hover_text("Render below native and upscale — the biggest fps lever at high DPI.");
                    ui.add(Slider::new(&mut settings.shadow_max, 0..=8).text("shadow maps"))
                        .on_hover_text("Hero (per-beam) shadow maps. Each is a depth pass (~2-3 ms at Retina).");
                    ui.add(Slider::new(&mut settings.steps, 24..=176).text("raymarch steps"))
                        .on_hover_text("Volumetric beam quality. Fewer = faster, more = smoother beams.");
                    ui.add(Slider::new(&mut settings.bloom, 0.0..=1.5).text("bloom"));
                    ui.checkbox(&mut settings.froxel_volumetric, "froxel fog (mass beams)")
                        .on_hover_text("Compute fog grid for wide washes — faster on huge rigs, coarser beams.");
            }
        });
}
