//! World / Render-properties inspector editors. Pure code move out of
//! [`super`] (the inspector `mod.rs`): the World HDRI controls and the
//! Render-properties property tabs shown when the World root node is selected.

use super::super::render_panel::{self, RenderPhase, RenderUiState};
use super::props::{self, Props};
use super::theme;
use super::InspectorState;
use crate::scene::Scene;
use egui::{DragValue, RichText, Slider};

/// The World inspector: load an equirectangular HDRI (sky + image-based
/// ambient), set its brightness, ambient fill, yaw and whether it shows as the
/// viewport background. Declared through the [`Props`] builder so it composes
/// inside the Render-properties World group. Shown in the Inspector when the
/// World node is selected.
fn world_inspector(p: &mut Props, world: &mut crate::scene::World, ink: &theme::Ink) {
    use theme::icon;

    // HDRI Load / Remove buttons — a full-width header row (no label column).
    p.custom_block(&["Brightness", "Ambient", "Rotation", "Background"], |ui| {
        ui.horizontal(|ui| {
            if ui
                .button(format!("{}  Load HDRI…", icon::IMAGE))
                .on_hover_text("Load an equirectangular environment map (.hdr / .png / .jpg)")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Environment map", &["hdr", "exr", "png", "jpg", "jpeg"])
                    .pick_file()
            {
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        world.hdri = Some(std::sync::Arc::new(bytes));
                        world.hdri_name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                    }
                    Err(e) => log::error!("load HDRI {}: {e}", path.display()),
                }
            }
            if world.hdri.is_some() && ui.button(theme::ico(icon::CLOSE)).on_hover_text("Remove the environment map").clicked() {
                world.hdri = None;
                world.hdri_name.clear();
            }
        });
        let name = if world.hdri.is_some() {
            if world.hdri_name.is_empty() { "loaded".to_string() } else { world.hdri_name.clone() }
        } else {
            "none (dark void)".to_string()
        };
        ui.label(RichText::new(name).weak().small());
    });

    let enabled = world.hdri.is_some();
    p.f32("Brightness", &mut world.brightness)
        .range(0.0..=4.0)
        .slider()
        .enabled(enabled)
        .tip("Overall world exposure (sky + ambient)");
    p.f32("Ambient", &mut world.ambient)
        .range(0.0..=2.0)
        .slider()
        .enabled(enabled)
        .tip("How strongly the environment lights the geometry");
    p.f32("Rotation", &mut world.rotation)
        .range(0.0..=std::f32::consts::TAU)
        .suffix(" rad")
        .slider()
        .enabled(enabled)
        .tip("Turn the environment around the vertical axis");
    p.custom("Background", enabled, |ui| {
        ui.checkbox(&mut world.show_background, "show sky");
    });
    if !enabled {
        p.custom_block(&["Brightness", "Ambient", "Rotation", "Background"], |ui| {
            ui.label(RichText::new("Load a map to light the scene from the environment.").weak().small().color(ink.muted));
        });
    }
}

/// **Render Properties** — shown in the Inspector when the World root node is
/// selected. Mirrors Blender's Render + Output property tabs (and the Redshift
/// reference layout): a fixed engine/actions header, then collapsible Output /
/// Sampling / Globals / Global Illumination / Optimisations / System sections
/// plus greyed Motion-Blur / Caustics stubs. Built from the SAME house builder
/// as the fixture inspector — [`Props`] (persisted collapse + search-filter) — so
/// the inspector search box and collapse state keep working.
///
/// Edits flow into `scene.render` ([`RenderConfig`](crate::scene::RenderConfig)),
/// the recipe a render job reads; the Render button raises a Start request the
/// app loop picks up (it owns the renderer + the Render dock tab).
pub(super) fn render_properties(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    settings: &mut crate::scene::RenderSettings,
    state: &mut InspectorState,
    render_ui: &mut RenderUiState,
) {
    use crate::scene::{RenderDisplay, RenderFormat};
    use theme::icon;

    let ink = theme::ink(!ui.visuals().dark_mode);
    let rendering = render_ui.status.phase == RenderPhase::Rendering;

    // Render the viewport resolution scale (a 0.5..1.0 fraction) as a percentage.
    let pct = |v: f64, _: std::ops::RangeInclusive<usize>| format!("{:.0}%", v * 100.0);

    props::with(ui, state, |p| {
        // --- Fixed header: engine, backend, actions, display (non-collapsing) ---
        p.custom("Engine", true, |ui| {
            egui::ComboBox::from_id_salt("render-engine")
                .selected_text("Previz Raymarch")
                .show_ui(ui, |ui| {
                    ui.selectable_label(true, "Previz Raymarch");
                });
        });

        // GPU backend — the dropdown lists only backends available on this machine
        // (a single entry on macOS = Metal). Switching takes effect on restart.
        if !render_ui.gpu_available.is_empty() {
            p.custom("Backend", true, |ui| {
                let options = render_ui.gpu_available.clone();
                let current = if render_ui.gpu_selected.is_empty() {
                    render_ui.gpu_active.clone()
                } else {
                    render_ui.gpu_selected.clone()
                };
                egui::ComboBox::from_id_salt("render-backend")
                    .selected_text(&current)
                    .show_ui(ui, |ui| {
                        for b in &options {
                            if ui.selectable_label(&current == b, b).clicked() {
                                render_ui.gpu_selected = b.clone();
                            }
                        }
                    });
            });
        }

        // The prominent Render | Animation pair (Animation is a greyed stub), plus
        // the restart hint when the chosen backend differs from the running one.
        p.custom_block(&["Render"], |ui| {
            // Hint when the chosen backend differs from the running one.
            if !render_ui.gpu_selected.is_empty() && render_ui.gpu_selected != render_ui.gpu_active {
                ui.label(
                    RichText::new(format!("Restart to switch to {}", render_ui.gpu_selected))
                        .small()
                        .color(theme::WARN),
                );
            }
            ui.add_space(4.0);

            ui.columns(2, |cols| {
                cols[0].add_enabled_ui(!rendering, |ui| {
                    let w = ui.available_width();
                    if ui
                        .add_sized(
                            [w, 28.0],
                            egui::Button::new(RichText::new(format!("{}  Render", icon::RENDER_GO)).strong()),
                        )
                        .on_hover_text("Render the current view as a still image")
                        .clicked()
                    {
                        render_ui.request_start();
                    }
                });
                cols[1].add_enabled_ui(false, |ui| {
                    let w = ui.available_width();
                    ui.add_sized([w, 28.0], egui::Button::new(format!("{}  Animation", icon::ANIMATION)))
                        .on_hover_text("Animation rendering — coming soon");
                });
            });
            ui.add_space(4.0);
        });

        p.custom("Display", true, |ui| {
            egui::ComboBox::from_id_salt("render-display")
                .selected_text(scene.render.display.label())
                .show_ui(ui, |ui| {
                    for d in RenderDisplay::ALL {
                        ui.add_enabled_ui(d.enabled(), |ui| {
                            if ui.selectable_label(scene.render.display == d, d.label()).clicked() {
                                scene.render.display = d;
                            }
                        });
                    }
                });
        });
        p.custom_block(&["Render to image file"], |ui| {
            ui.checkbox(&mut scene.render.write_to_disk, "Render to image file")
                .on_hover_text("Write the result to the output file as soon as the render completes");
            ui.add_space(4.0);
        });

        // --- Output ---------------------------------------------------------
        p.group("Output", icon::IMAGE, true, |p| {
            // res_x / res_y / resolution_percentage are u32: edit through DragValue<u32>
            // / Slider<u32> verbatim so the integer types + suffixes stay bit-identical.
            p.custom("Resolution X", true, |ui| {
                ui.add(DragValue::new(&mut scene.render.res_x).range(16..=8192).speed(2.0).suffix(" px"));
            });
            p.custom("Resolution Y", true, |ui| {
                ui.add(DragValue::new(&mut scene.render.res_y).range(16..=8192).speed(2.0).suffix(" px"));
            });
            // Scale: a u32 percentage slider with " %" suffix — a custom widget so the
            // integer type + suffix stay bit-identical.
            p.custom("Scale", true, |ui| {
                // Range matches RenderConfig::output_size's clamp (up to 400% supersample).
                ui.spacing_mut().slider_width = (ui.available_width() - super::INSPECTOR_SLIDER_READOUT).max(24.0);
                ui.add(Slider::new(&mut scene.render.resolution_percentage, 10..=400).suffix(" %"));
            });
            p.custom("Format", true, |ui| {
                egui::ComboBox::from_id_salt("render-format")
                    .selected_text(scene.render.format.label())
                    .show_ui(ui, |ui| {
                        for f in RenderFormat::ALL {
                            if ui.selectable_label(scene.render.format == f, f.label()).clicked() {
                                scene.render.format = f;
                            }
                        }
                    });
            });
            p.custom("File", true, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button(theme::ico(icon::IMPORT_GDTF))
                        .on_hover_text("Choose the output file")
                        .clicked()
                        && let Some(p) = rfd::FileDialog::new()
                            .add_filter(scene.render.format.label(), &[scene.render.format.ext()])
                            .set_file_name(render_panel::default_filename(&scene.render))
                            .save_file()
                    {
                        scene.render.out_path = p.to_string_lossy().into_owned();
                    }
                    let shown = if scene.render.out_path.is_empty() {
                        "— not set".to_string()
                    } else {
                        std::path::Path::new(&scene.render.out_path)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| scene.render.out_path.clone())
                    };
                    ui.label(RichText::new(shown).weak().small());
                });
            });
            let (w, h) = scene.render.output_size();
            p.custom_block(&["Resolution X", "Resolution Y", "Scale", "Format", "File"], |ui| {
                ui.label(RichText::new(format!("Renders at {w}×{h} px")).weak().small().color(ink.muted));
            });
        });

        // --- Sampling (Viewport vs Render, like Blender's two sub-panels) ---
        p.group("Sampling", icon::PERF, true, |p| {
            p.subhead("Viewport (preview)");
            let auto = settings.auto_resolution;
            // Disabled while Auto FPS drives it; range goes down to 25%. Custom because
            // of the .step_by + custom percent formatter (not expressible via `.f32`).
            p.custom("Scale", true, |ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - super::INSPECTOR_SLIDER_READOUT).max(24.0);
                ui.add_enabled(
                    !auto,
                    Slider::new(&mut settings.render_scale, 0.25..=1.0)
                        .step_by(0.05)
                        .custom_formatter(pct),
                )
                .on_hover_text("Live preview resolution scale — lower is faster (also in the Performance overlay)");
            });
            p.custom("Auto FPS", true, |ui| {
                ui.checkbox(&mut settings.auto_resolution, "")
                    .on_hover_text("Dynamic resolution — auto-adjust the scale to hold the target frame rate");
            });
            p.f32("Target", &mut settings.fps_target)
                .speed(1.0)
                .range(20.0..=240.0)
                .suffix(" fps")
                .enabled(auto)
                .tip("Frame rate the dynamic scaler aims to hold");
            // Preview volumetric raymarch steps (u32).
            p.custom("Steps", true, |ui| {
                ui.add(DragValue::new(&mut settings.steps).range(8..=256).speed(1.0))
                    .on_hover_text("Preview volumetric raymarch steps");
            });

            p.subhead("Render (final)");
            p.custom("Quality", true, |ui| {
                egui::ComboBox::from_id_salt("render-quality")
                    .selected_text(scene.render.quality.label())
                    .show_ui(ui, |ui| {
                        for q in crate::scene::QualityPreset::ALL {
                            if ui.selectable_label(scene.render.quality == q, q.label()).clicked() {
                                scene.render.quality = q;
                                scene.render.apply_quality();
                            }
                        }
                    });
            });
            p.custom("Samples", true, |ui| {
                if ui
                    .add(DragValue::new(&mut scene.render.max_samples).range(1..=512).speed(1.0))
                    .on_hover_text("Progressive accumulation passes — more = cleaner volumetrics")
                    .changed()
                {
                    scene.render.quality = crate::scene::QualityPreset::Custom;
                }
            });
            p.custom("Steps", true, |ui| {
                if ui
                    .add(DragValue::new(&mut scene.render.volumetric_steps).range(8..=256).speed(1.0))
                    .on_hover_text("Render volumetric raymarch steps")
                    .changed()
                {
                    scene.render.quality = crate::scene::QualityPreset::Custom;
                }
            });
        });

        // --- Performance (Viewport vs Render) -------------------------------
        p.group("Performance", icon::SETTINGS, false, |p| {
            p.subhead("Viewport (preview)");
            p.custom("Shadow maps", true, |ui| {
                ui.add(DragValue::new(&mut settings.shadow_max).range(0..=16).speed(1.0))
                    .on_hover_text("Preview hero shadow maps — fewer is faster");
            });
            p.custom("Froxel", true, |ui| {
                ui.checkbox(&mut settings.froxel_volumetric, "")
                    .on_hover_text("Use the froxel fog grid for wide/dim beams in the preview");
            });

            p.subhead("Render (final)");
            p.custom("Shadow maps", true, |ui| {
                ui.add(DragValue::new(&mut scene.render.shadow_max).range(0..=16).speed(1.0))
                    .on_hover_text("Render hero shadow maps");
            });
            p.custom("Froxel", true, |ui| {
                ui.checkbox(&mut scene.render.froxel_volumetric, "")
                    .on_hover_text("Use the froxel fog grid for the render");
            });
            p.custom("Overlays", true, |ui| {
                ui.checkbox(&mut scene.render.show_overlays, "")
                    .on_hover_text("Include the origin grid + gizmos in the render (off = clean plate)");
            });
        });

        // --- Color Management (SHARED with the viewport — preview matches render) -
        p.group("Color Management", icon::COLOR, true, |p| {
            p.custom_block(&["Exposure", "Bloom", "Beam intensity", "Gobo sharpness", "Chroma haze"], |ui| {
                ui.label(RichText::new("Shared with the viewport — what you preview is what you render.").small().weak().color(ink.muted));
            });
            p.f32("Exposure", &mut settings.exposure).range(0.05..=8.0).slider();
            p.f32("Bloom", &mut settings.bloom).range(0.0..=2.0).slider();
            p.f32("Beam intensity", &mut settings.beam_intensity).range(0.0..=2000.0).slider();
            p.f32("Gobo sharpness", &mut settings.gobo_sharpness).range(0.0..=2.0).slider();
            p.f32("Chroma haze", &mut settings.chroma_haze)
                .range(0.0..=2.0)
                .slider()
                .tip("Lift saturated dim hues in haze so they read without going neon");
        });

        // --- World (SHARED: HDRI sky + ambient + fog) -----------------------
        p.group("World", icon::WORLD, true, |p| {
            world_inspector(p, &mut scene.world, &ink);
            // Fog density: drive the active (non-hidden) fog volume, or a "no fog
            // volume" placeholder — a custom slider (indirect target + placeholder).
            p.custom("Fog density", true, |ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - super::INSPECTOR_SLIDER_READOUT).max(24.0);
                match scene.environments.iter_mut().find(|e| !e.hidden) {
                    Some(env) => {
                        ui.add(Slider::new(&mut env.density, 0.0..=2.0))
                            .on_hover_text("Haze density of the active fog volume");
                    }
                    None => {
                        ui.label(RichText::new("no fog volume").weak().small());
                    }
                }
            });
        });

        // --- Motion Blur (stub) ---------------------------------------------
        p.group("Motion Blur", icon::ANIMATION, false, |p| {
            p.custom_block(&["Motion blur"], |ui| {
                ui.add_enabled_ui(false, |ui| {
                    ui.checkbox(&mut scene.render.motion_blur, "Enable");
                });
                ui.label(RichText::new("Not yet supported by the raymarch engine").weak().small().color(ink.muted));
            });
        });

        // --- Caustics (stub) ------------------------------------------------
        p.group("Caustics", icon::COLOR, false, |p| {
            p.custom_block(&["Caustics"], |ui| {
                ui.add_enabled_ui(false, |ui| {
                    ui.checkbox(&mut scene.render.caustics, "Enable");
                });
                ui.label(RichText::new("Not implemented").weak().small().color(ink.muted));
            });
        });
    });
}
