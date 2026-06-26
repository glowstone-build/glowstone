//! World / Render-properties inspector editors. Pure code move out of
//! [`super`] (the inspector `mod.rs`): the World HDRI controls and the
//! Render-properties property tabs shown when the World root node is selected.

use super::super::render_panel::{self, RenderPhase, RenderUiState};
use super::theme;
use super::{category, prop_label, row, slider_row, InspectorState};
use crate::scene::Scene;
use egui::{DragValue, Grid, RichText, Slider};

/// The World inspector: load an equirectangular HDRI (sky + image-based
/// ambient), set its brightness, ambient fill, yaw and whether it shows as the
/// viewport background. Shown in the Inspector when the World node is selected.
fn world_inspector(ui: &mut egui::Ui, world: &mut crate::scene::World, ink: &theme::Ink, state: &InspectorState) {
    use theme::icon;
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

    let enabled = world.hdri.is_some();
    Grid::new("world-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        slider_row(ui, state, "Brightness", false, |ui| {
            ui.add_enabled(enabled, Slider::new(&mut world.brightness, 0.0..=4.0))
                .on_hover_text("Overall world exposure (sky + ambient)");
        });
        slider_row(ui, state, "Ambient", false, |ui| {
            ui.add_enabled(enabled, Slider::new(&mut world.ambient, 0.0..=2.0))
                .on_hover_text("How strongly the environment lights the geometry");
        });
        slider_row(ui, state, "Rotation", false, |ui| {
            ui.add_enabled(enabled, Slider::new(&mut world.rotation, 0.0..=std::f32::consts::TAU).suffix(" rad"))
                .on_hover_text("Turn the environment around the vertical axis");
        });
        row(ui, state, "Background", false, |ui| {
            ui.add_enabled(enabled, egui::Checkbox::new(&mut world.show_background, "show sky"));
        });
    });
    if !enabled {
        ui.label(RichText::new("Load a map to light the scene from the environment.").weak().small().color(ink.muted));
    }
}

/// **Render Properties** — shown in the Inspector when the World root node is
/// selected. Mirrors Blender's Render + Output property tabs (and the Redshift
/// reference layout): a fixed engine/actions header, then collapsible Output /
/// Sampling / Globals / Global Illumination / Optimisations / System sections
/// plus greyed Motion-Blur / Caustics stubs. Built from the SAME house helpers
/// as the fixture inspector — [`category`] (persisted collapse + search-filter)
/// and [`row`] — so the inspector search box and collapse state keep working.
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

    // A small "Viewport" / "Render" sub-section header (Blender splits Sampling into
    // a Viewport and a Render sub-panel — this is the in-line equivalent).
    let subhead = |ui: &mut egui::Ui, text: &str| {
        ui.add_space(2.0);
        ui.label(RichText::new(text).small().strong().color(ink.tertiary));
    };
    // Render the viewport resolution scale (a 0.5..1.0 fraction) as a percentage.
    let pct = |v: f64, _: std::ops::RangeInclusive<usize>| format!("{:.0}%", v * 100.0);

    // --- Fixed header: engine, backend, actions, display (non-collapsing) ---
    Grid::new("render-engine-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        prop_label(ui, "Engine", false);
        egui::ComboBox::from_id_salt("render-engine")
            .selected_text("Previz Raymarch")
            .show_ui(ui, |ui| {
                ui.selectable_label(true, "Previz Raymarch");
            });
        ui.end_row();

        // GPU backend — the dropdown lists only backends available on this machine
        // (a single entry on macOS = Metal). Switching takes effect on restart.
        if !render_ui.gpu_available.is_empty() {
            prop_label(ui, "Backend", false);
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
            ui.end_row();
        }
    });
    // Hint when the chosen backend differs from the running one.
    if !render_ui.gpu_selected.is_empty() && render_ui.gpu_selected != render_ui.gpu_active {
        ui.label(
            RichText::new(format!("Restart to switch to {}", render_ui.gpu_selected))
                .small()
                .color(theme::WARN),
        );
    }
    ui.add_space(4.0);

    // The prominent Render | Animation pair (Animation is a greyed stub).
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

    Grid::new("render-display-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        prop_label(ui, "Display", false);
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
        ui.end_row();
    });
    ui.checkbox(&mut scene.render.write_to_disk, "Render to image file")
        .on_hover_text("Write the result to the output file as soon as the render completes");
    ui.add_space(4.0);

    // --- Output -------------------------------------------------------------
    let out_labels = ["Resolution X", "Resolution Y", "Scale", "Format", "File"];
    category(ui, state, "Output", format!("{}  Output", icon::IMAGE), true, &out_labels, |ui, fs| {
        Grid::new("render-output-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            row(ui, fs, "Resolution X", false, |ui| {
                ui.add(DragValue::new(&mut scene.render.res_x).range(16..=8192).speed(2.0).suffix(" px"));
            });
            row(ui, fs, "Resolution Y", false, |ui| {
                ui.add(DragValue::new(&mut scene.render.res_y).range(16..=8192).speed(2.0).suffix(" px"));
            });
            slider_row(ui, fs, "Scale", false, |ui| {
                // Range matches RenderConfig::output_size's clamp (up to 400% supersample).
                ui.add(Slider::new(&mut scene.render.resolution_percentage, 10..=400).suffix(" %"));
            });
            row(ui, fs, "Format", false, |ui| {
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
            row(ui, fs, "File", false, |ui| {
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
        });
        let (w, h) = scene.render.output_size();
        ui.label(RichText::new(format!("Renders at {w}×{h} px")).weak().small().color(ink.muted));
    });

    // --- Sampling (Viewport vs Render, like Blender's two sub-panels) -------
    let samp_labels = ["Scale", "Auto FPS", "Target", "Steps", "Samples", "Quality"];
    category(ui, state, "Sampling", format!("{}  Sampling", icon::PERF), true, &samp_labels, |ui, fs| {
        subhead(ui, "Viewport (preview)");
        let auto = settings.auto_resolution;
        Grid::new("render-samp-vp").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            slider_row(ui, fs, "Scale", false, |ui| {
                // Disabled while Auto FPS drives it; range goes down to 25%.
                ui.add_enabled(
                    !auto,
                    Slider::new(&mut settings.render_scale, 0.25..=1.0)
                        .step_by(0.05)
                        .custom_formatter(pct),
                )
                .on_hover_text("Live preview resolution scale — lower is faster (also in the Performance overlay)");
            });
            row(ui, fs, "Auto FPS", false, |ui| {
                ui.checkbox(&mut settings.auto_resolution, "")
                    .on_hover_text("Dynamic resolution — auto-adjust the scale to hold the target frame rate");
            });
            row(ui, fs, "Target", false, |ui| {
                ui.add_enabled(
                    auto,
                    DragValue::new(&mut settings.fps_target).range(20.0..=240.0).speed(1.0).suffix(" fps"),
                )
                .on_hover_text("Frame rate the dynamic scaler aims to hold");
            });
            row(ui, fs, "Steps", false, |ui| {
                ui.add(DragValue::new(&mut settings.steps).range(8..=256).speed(1.0))
                    .on_hover_text("Preview volumetric raymarch steps");
            });
        });
        subhead(ui, "Render (final)");
        Grid::new("render-samp-final").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            row(ui, fs, "Quality", false, |ui| {
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
            row(ui, fs, "Samples", false, |ui| {
                if ui
                    .add(DragValue::new(&mut scene.render.max_samples).range(1..=512).speed(1.0))
                    .on_hover_text("Progressive accumulation passes — more = cleaner volumetrics")
                    .changed()
                {
                    scene.render.quality = crate::scene::QualityPreset::Custom;
                }
            });
            row(ui, fs, "Steps", false, |ui| {
                if ui
                    .add(DragValue::new(&mut scene.render.volumetric_steps).range(8..=256).speed(1.0))
                    .on_hover_text("Render volumetric raymarch steps")
                    .changed()
                {
                    scene.render.quality = crate::scene::QualityPreset::Custom;
                }
            });
        });
    });

    // --- Performance (Viewport vs Render) -----------------------------------
    let perf_labels = ["Shadow maps", "Froxel", "Overlays"];
    category(ui, state, "Performance", format!("{}  Performance", icon::SETTINGS), false, &perf_labels, |ui, fs| {
        subhead(ui, "Viewport (preview)");
        Grid::new("render-perf-vp").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            row(ui, fs, "Shadow maps", false, |ui| {
                ui.add(DragValue::new(&mut settings.shadow_max).range(0..=16).speed(1.0))
                    .on_hover_text("Preview hero shadow maps — fewer is faster");
            });
            row(ui, fs, "Froxel", false, |ui| {
                ui.checkbox(&mut settings.froxel_volumetric, "")
                    .on_hover_text("Use the froxel fog grid for wide/dim beams in the preview");
            });
        });
        subhead(ui, "Render (final)");
        Grid::new("render-perf-final").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            row(ui, fs, "Shadow maps", false, |ui| {
                ui.add(DragValue::new(&mut scene.render.shadow_max).range(0..=16).speed(1.0))
                    .on_hover_text("Render hero shadow maps");
            });
            row(ui, fs, "Froxel", false, |ui| {
                ui.checkbox(&mut scene.render.froxel_volumetric, "")
                    .on_hover_text("Use the froxel fog grid for the render");
            });
            row(ui, fs, "Overlays", false, |ui| {
                ui.checkbox(&mut scene.render.show_overlays, "")
                    .on_hover_text("Include the origin grid + gizmos in the render (off = clean plate)");
            });
        });
    });

    // --- Color Management (SHARED with the viewport — preview matches render) -
    let color_labels = ["Exposure", "Bloom", "Beam intensity", "Gobo sharpness", "Chroma haze"];
    category(ui, state, "Color Management", format!("{}  Color Management", icon::COLOR), true, &color_labels, |ui, fs| {
        ui.label(RichText::new("Shared with the viewport — what you preview is what you render.").small().weak().color(ink.muted));
        Grid::new("render-color-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            slider_row(ui, fs, "Exposure", false, |ui| {
                ui.add(Slider::new(&mut settings.exposure, 0.05..=8.0));
            });
            slider_row(ui, fs, "Bloom", false, |ui| {
                ui.add(Slider::new(&mut settings.bloom, 0.0..=2.0));
            });
            slider_row(ui, fs, "Beam intensity", false, |ui| {
                ui.add(Slider::new(&mut settings.beam_intensity, 0.0..=2000.0));
            });
            slider_row(ui, fs, "Gobo sharpness", false, |ui| {
                ui.add(Slider::new(&mut settings.gobo_sharpness, 0.0..=2.0));
            });
            slider_row(ui, fs, "Chroma haze", false, |ui| {
                ui.add(Slider::new(&mut settings.chroma_haze, 0.0..=2.0))
                    .on_hover_text("Lift saturated dim hues in haze so they read without going neon");
            });
        });
    });

    // --- World (SHARED: HDRI sky + ambient + fog) ---------------------------
    let world_labels = ["Brightness", "Ambient", "Rotation", "Background", "Fog density"];
    category(ui, state, "World", format!("{}  World", icon::WORLD), true, &world_labels, |ui, fs| {
        world_inspector(ui, &mut scene.world, &ink, fs);
        ui.add_space(4.0);
        Grid::new("render-world-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            slider_row(ui, fs, "Fog density", false, |ui| {
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
    });

    // --- Motion Blur (stub) -------------------------------------------------
    let mb_labels = ["Motion blur"];
    category(ui, state, "Motion Blur", format!("{}  Motion Blur", icon::ANIMATION), false, &mb_labels, |ui, _fs| {
        ui.add_enabled_ui(false, |ui| {
            ui.checkbox(&mut scene.render.motion_blur, "Enable");
        });
        ui.label(RichText::new("Not yet supported by the raymarch engine").weak().small().color(ink.muted));
    });

    // --- Caustics (stub) ----------------------------------------------------
    let ca_labels = ["Caustics"];
    category(ui, state, "Caustics", format!("{}  Caustics", icon::COLOR), false, &ca_labels, |ui, _fs| {
        ui.add_enabled_ui(false, |ui| {
            ui.checkbox(&mut scene.render.caustics, "Enable");
        });
        ui.label(RichText::new("Not implemented").weak().small().color(ink.muted));
    });
}
