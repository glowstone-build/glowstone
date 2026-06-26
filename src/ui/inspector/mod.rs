//! The Inspector dock panel: editable parameters for the current selection.
//!
//! Extracted from [`super::panels`] as a pure code move — the inspector property
//! infrastructure (reset-to-default rows, filter/collapse state, multi-edit
//! reductions) plus every per-selection editor (fixture / GDTF / bulk / world /
//! render-properties / environment / geometry / LED-screen) and the GDTF texture
//! loader they share. Edits flow straight into the scene (the established
//! direct-edit model); the drag-edge detection in [`inspector`] wraps a whole
//! slider/DragValue gesture into one undo step.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, DragValue, Grid, RichText, Sense, Slider};
use glam::{Mat4, Vec3};

mod environment;
mod fixture;
mod props;
pub use props::{Inspect, Props};

use super::panels::InspectorEdit;
use super::render_panel::{self, RenderPhase, RenderUiState};
use super::theme;
use super::windows::ProfileEditor;
use super::{GdtfTextures, ScreenSources};
use crate::dmx::PatchTable;
use crate::gdtf::{GdtfFixture, WheelKind};
use crate::optics::{self, OpticField, OpticalControls};
use crate::scene::environment::Environment;
use crate::scene::screen::{LedScreen, PixelShape, ScreenContent, TestPattern};
use crate::scene::{Fixture, Scene, Selection};

/// Persistent + transient Inspector UI state (S1): the property **filter** box and
/// each category's remembered **open/closed** state.
///
/// The filter is a fuzzy/substring match (via [`super::lib_prefs::fuzzy_score`])
/// run against each property ROW label; categories with no matching row hide
/// entirely, and an empty filter restores the full layout. The collapse map
/// remembers, per category title, whether the user has it expanded — so the
/// inspector reopens the way they left it. Only `collapsed` persists (to a small
/// JSON in the config dir, mirroring [`super::lib_prefs::LibraryPrefs`]); the
/// filter is per-session.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct InspectorState {
    /// Live row-label filter (not persisted — a fresh session starts unfiltered).
    #[serde(skip)]
    pub filter: String,
    /// Per-category remembered open state, keyed by the category's stable title
    /// (e.g. "Transform", "Optics"). Absent ⇒ fall back to the call-site default.
    #[serde(default)]
    pub collapsed: std::collections::BTreeMap<String, bool>,
    /// "Show only modified" header toggle (P2 #64): when on, every row that
    /// equals its default (the reset gutter is empty) is hidden, leaving just the
    /// properties the user has actually changed. Composes with [`filter`] (a row
    /// must pass BOTH). Per-session — a fresh session starts off.
    ///
    /// [`filter`]: Self::filter
    #[serde(skip)]
    pub show_modified: bool,
    /// PIN target (P2 #65): when `Some(id)`, the inspector stays locked to that
    /// fixture's [`EntityId`] even as the viewport/outliner selection changes, so
    /// you can park on one fixture and select others (e.g. to copy values). `None`
    /// follows the live selection. Per-session — never persisted.
    #[serde(skip)]
    pub pinned: Option<crate::scene::EntityId>,
    /// Inspector panel content width, captured once per frame (transient). Drives the
    /// FIXED 2-column row layout (label column `INSPECTOR_LABEL_W` + a value cell that
    /// fills the rest) so every section's columns line up and nothing overflows.
    #[serde(skip)]
    pub panel_w: f32,
}

/// Width of the inspector's fixed LABEL column (Blender-style 2-column rows). Every
/// section uses it so the value column starts at the same x everywhere.
const INSPECTOR_LABEL_W: f32 = 84.0;

/// Space reserved to the RIGHT of the value column = category indent (~18) + the
/// label/value grid gap (~12) + a small right margin (~4). Tuned so the value cell
/// fills nearly to the panel edge (Blender-tight), not the old over-wide gap.
const INSPECTOR_VALUE_PAD: f32 = 32.0;

/// The shared width of EVERY value cell (field, slider, combo) so they all fill to the
/// SAME right edge — one consistent column, like Blender. `panel_w` is the captured
/// [`InspectorState::panel_w`].
fn inspector_value_w(panel_w: f32) -> f32 {
    (panel_w - INSPECTOR_LABEL_W - INSPECTOR_VALUE_PAD).max(60.0)
}

/// Space a slider reserves to the right of its bar for the value readout (item gap +
/// a ~2-decimal number). Subtracted from the value cell so the bar fills the rest and
/// the readout's right edge lines up with the fields' right edge.
const INSPECTOR_SLIDER_READOUT: f32 = 42.0;

impl InspectorState {
    /// Load the persisted collapse map (config dir); missing/garbled ⇒ default.
    pub fn load() -> Self {
        let Some(p) = inspector_state_path() else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(&p) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Persist the collapse map (best-effort; a write failure is non-fatal).
    pub fn save(&self) {
        let Some(p) = inspector_state_path() else { return };
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&p, text);
        }
    }

    /// The trimmed, lowercased active query, or `None` when filtering is off.
    fn query(&self) -> Option<String> {
        let q = self.filter.trim().to_lowercase();
        (!q.is_empty()).then_some(q)
    }

    /// Whether a single property ROW (identified by its label) should be shown:
    /// always when no filter is active, else a fuzzy match against the label.
    pub fn row_visible(&self, label: &str) -> bool {
        match self.query() {
            None => true,
            Some(q) => super::lib_prefs::fuzzy_score(&q, &label.to_lowercase()).is_some(),
        }
    }

    /// Whether a single property ROW should render given BOTH gates (S1 + P2 #64):
    /// the live filter ([`row_visible`]) AND the "show only modified" toggle. With
    /// `show_modified` on, a row that equals its default (`!differs`) is hidden.
    /// Rows whose value HAS no default concept pass `differs = true` from the
    /// caller so they're never swallowed by the modified-only filter.
    ///
    /// [`row_visible`]: Self::row_visible
    pub fn row_shown(&self, label: &str, differs: bool) -> bool {
        self.row_visible(label) && (!self.show_modified || differs)
    }

    /// Whether a CATEGORY (with the given row labels) should be shown: always when
    /// no filter is active, else only if at least one of its rows matches. An empty
    /// row set hides under filtering (nothing to match).
    pub fn category_visible(&self, row_labels: &[&str]) -> bool {
        match self.query() {
            None => true,
            Some(q) => row_labels
                .iter()
                .any(|l| super::lib_prefs::fuzzy_score(&q, &l.to_lowercase()).is_some()),
        }
    }

    /// The effective open state for a category: the remembered value if the user
    /// has toggled it, else the call-site `default_open`. While a filter is active
    /// every visible category force-opens so matches aren't hidden behind a caret.
    fn open_state(&self, title: &str, default_open: bool) -> bool {
        if self.query().is_some() {
            return true;
        }
        self.collapsed.get(title).copied().unwrap_or(default_open)
    }
}

/// `<config>/inspector.json` — the per-user collapse store, alongside
/// `library.json` / `recent.json`.
fn inspector_state_path() -> Option<std::path::PathBuf> {
    let d = directories::ProjectDirs::from("dev", "Embedder", "previz")?;
    let dir = d.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("inspector.json"))
}

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
fn render_properties(
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

/// The fixture's provenance ("Built-in" / "GDTF Share" / "MVR" / …) as a clean,
/// colour-coded text tag — no floating dot (the dot-on-a-margin read as awful). Used
/// in the inspector header + (drawn directly) the library / Replace rows.
pub(crate) fn source_chip(ui: &mut egui::Ui, source: crate::gdtf::FixtureSource) {
    let [r, g, b] = source.color_rgb();
    ui.label(egui::RichText::new(source.label()).size(11.0).color(Color32::from_rgb(r, g, b)));
}

// ============================================================================
// Inspector property infrastructure (§2.2 — Unreal's per-property reset-to-default
// + Simple/Advanced split, Blender's auto-dim). These are shared, presentation-only
// helpers; they mutate the borrowed value in place (matching the inspector's
// established direct-edit model — slider/drag edits here are already non-undoable).
// ============================================================================

/// Float equality with a tolerance, so a value the user dragged back onto its
/// default doesn't keep showing the revert arrow from f32 dust.
fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4
}

/// Whether two RGB triples match (per-channel `approx`).
fn approx_rgb(a: [f32; 3], b: [f32; 3]) -> bool {
    approx(a[0], b[0]) && approx(a[1], b[1]) && approx(a[2], b[2])
}

/// Multi-edit value reduction (#7): the common value across a selection, or
/// `None` when they differ ("mixed"). `Some(v)` ⇒ all equal (within `approx`) ⇒
/// show the live widget seeded with `v`; `None` ⇒ show a "Multiple" placeholder.
/// Mirrors Unreal's `GetReadAddress` / `bAllValuesTheSame`. Empty ⇒ `None`.
fn common_f32(values: impl IntoIterator<Item = f32>) -> Option<f32> {
    let mut it = values.into_iter();
    let first = it.next()?;
    it.all(|v| approx(v, first)).then_some(first)
}

/// RGB variant of [`common_f32`] for the colour rows.
fn common_rgb(values: impl IntoIterator<Item = [f32; 3]>) -> Option<[f32; 3]> {
    let mut it = values.into_iter();
    let first = it.next()?;
    it.all(|v| approx_rgb(v, first)).then_some(first)
}

/// One multi-edit numeric row (#7): when the selection agrees, render the
/// `widget` (a [`DragValue`]/[`Slider`] built over the seed) and write the edited
/// value back to ALL via `write`; when it's mixed, draw a quiet "Multiple" button
/// that, on click, adopts the seed across the whole selection (so the next frame
/// shows a real widget). Only the touched field is written — siblings keep theirs.
fn bulk_f32_row(
    ui: &mut egui::Ui,
    state: &InspectorState,
    label: &str,
    common: Option<f32>,
    seed: f32,
    widget: impl FnOnce(&mut egui::Ui, &mut f32) -> egui::Response,
    mut write: impl FnMut(f32),
) {
    if !state.row_visible(label) {
        return;
    }
    ui.label(label);
    match common {
        Some(mut v) => {
            if widget(ui, &mut v).changed() {
                write(v);
            }
        }
        None => {
            // Mixed: a placeholder that unifies on click (adopts the seed value).
            if ui
                .add(egui::Button::new(RichText::new("— Multiple —").small().weak()))
                .on_hover_text("Values differ — click to set all to the active value")
                .clicked()
            {
                write(seed);
            }
        }
    }
    ui.end_row();
}

/// The "revert to default" gutter button (#6). Drawn ONLY when `differs` — a
/// quiet circular-arrow that snaps the field back to its template value. When the
/// value already matches its default, an equal-width blank keeps the label column
/// from jumping. Returns `true` on click (the caller does the reset, so it stays
/// one mutation). The default source is the GDTF/library template for fixtures,
/// `Default` for env/geometry — resolved by the caller.
fn reset_arrow(ui: &mut egui::Ui, differs: bool) -> bool {
    if differs {
        ui.add(egui::Button::new(RichText::new(theme::icon::UNDO).small()).frame(false))
            .on_hover_text("Reset to default")
            .clicked()
    } else {
        // Reserve the same footprint so labels don't shift when the arrow appears.
        ui.add_space(14.0);
        false
    }
}

/// A Grid label cell with a leading reset gutter (#6): `[↺] Label`. Returns
/// `true` when the revert arrow was clicked this frame. Pair it with the value
/// widget in the next column; the caller resets on a `true` return.
fn prop_label(ui: &mut egui::Ui, label: &str, differs: bool) -> bool {
    let mut clicked = false;
    let h = ui.spacing().interact_size.y;
    // Fixed-width label cell so EVERY section's first column lines up (the Blender
    // 2-column inspector). Reset gutter + label, the label truncated to the cell.
    ui.allocate_ui_with_layout(
        egui::vec2(INSPECTOR_LABEL_W, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            clicked = reset_arrow(ui, differs);
            ui.add(egui::Label::new(label).truncate());
        },
    );
    clicked
}

/// A filter-aware two-column [`Grid`] ROW (S1). When the active filter excludes
/// `label`, the row is skipped wholesale (label + value widget, balanced
/// `end_row`); otherwise it renders the (revert-arrow) label, runs `value` for the
/// right cell, and returns whether the reset arrow was clicked. Centralizes the
/// "show this property?" decision so callers don't duplicate the predicate.
fn row(
    ui: &mut egui::Ui,
    state: &InspectorState,
    label: &str,
    differs: bool,
    value: impl FnOnce(&mut egui::Ui),
) -> bool {
    if !state.row_shown(label, differs) {
        return false;
    }
    let mut clicked = false;
    field_row(
        ui,
        state.panel_w,
        |ui| {
            clicked = reset_arrow(ui, differs);
            ui.add(egui::Label::new(label).truncate());
        },
        value,
    );
    clicked
}

/// The fixed 2-column inspector ROW layout (Blender-style): a fixed-width LABEL cell
/// + a VALUE cell that FILLS its width (justified) — so every section's columns line
/// up at the same x AND no widget (slider / field / combo) can overflow the panel.
/// The draggable DragValue/Slider input is untouched; widgets just fill the cell now.
/// Call inside a 2-column `Grid` (adds both cells + `end_row`); `panel_w` is
/// [`InspectorState::panel_w`]. Used by `row` AND the optics/wheel sliders that build
/// their own value widget. 48 ≈ category indent + grid spacing + a right margin, so
/// the value cell ends ~18px short of the panel edge (symmetric with the left).
fn field_row(
    ui: &mut egui::Ui,
    panel_w: f32,
    label: impl FnOnce(&mut egui::Ui),
    value: impl FnOnce(&mut egui::Ui),
) {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(INSPECTOR_LABEL_W, h),
        egui::Layout::left_to_right(egui::Align::Center),
        label,
    );
    let value_w = inspector_value_w(panel_w);
    // A justified cell of exactly `value_w`: egui's DragValue/ComboBox are
    // cross-justify-aware and FILL the cell, so every field reaches the same right
    // edge. (Sliders use `slider_field_row` instead — justify would eat their value.)
    ui.allocate_ui_with_layout(
        egui::vec2(value_w, h),
        egui::Layout::top_down_justified(egui::Align::Min),
        value,
    );
    ui.end_row();
}

/// Like [`field_row`] but for a SLIDER value. A justified cell eats the slider's value
/// (the bar grabs the whole width and the value spills past the edge), and `add_sized`
/// merely CENTERS a slider (egui sliders don't auto-expand). So instead this sets
/// `slider_width` to the cell MINUS a fixed readout reserve: the bar fills the cell and
/// the value lands right at the field column's right edge — one consistent column, no
/// glue, no cutoff. Same fixed label column as `field_row`.
fn slider_field_row(
    ui: &mut egui::Ui,
    panel_w: f32,
    label: impl FnOnce(&mut egui::Ui),
    value: impl FnOnce(&mut egui::Ui),
) {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(INSPECTOR_LABEL_W, h),
        egui::Layout::left_to_right(egui::Align::Center),
        label,
    );
    let value_w = inspector_value_w(panel_w);
    ui.allocate_ui_with_layout(
        egui::vec2(value_w, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            // bar = cell − (item gap + the ~2-decimal readout) so the value's right edge
            // lands on the same x as the fields' filled boxes.
            ui.spacing_mut().slider_width = (value_w - INSPECTOR_SLIDER_READOUT).max(24.0);
            value(ui);
        },
    );
    ui.end_row();
}

/// A filter-aware inspector slider ROW (the [`row`] equivalent for sliders): handles
/// row visibility + the reset arrow, then lays the slider out via [`slider_field_row`]
/// so its value lines up with the fields. Returns whether the reset arrow was clicked.
fn slider_row(
    ui: &mut egui::Ui,
    state: &InspectorState,
    label: &str,
    differs: bool,
    value: impl FnOnce(&mut egui::Ui),
) -> bool {
    if !state.row_shown(label, differs) {
        return false;
    }
    let mut clicked = false;
    slider_field_row(
        ui,
        state.panel_w,
        |ui| {
            clicked = reset_arrow(ui, differs);
            ui.add(egui::Label::new(label).truncate());
        },
        value,
    );
    clicked
}

/// A stacked vector property (Blender-style): the label on the first sub-row, then ONE
/// FULL-WIDTH draggable field per component (X/Y/Z). Stacking — rather than three
/// fields across one row — keeps every value readable at ANY panel width (3-across
/// fields get unreadably narrow and clip). Returns `true` if a value changed (a drag OR
/// the reset arrow, which zeroes all three) so the caller can recompose (e.g. euler →
/// quat). Call inside the 2-column inspector `Grid`.
#[allow(clippy::too_many_arguments)]
fn vec3_rows(
    ui: &mut egui::Ui,
    state: &InspectorState,
    label: &str,
    differs: bool,
    speed: f64,
    suffix: &str,
    x: &mut f32,
    y: &mut f32,
    z: &mut f32,
) -> bool {
    if !state.row_shown(label, differs) {
        return false;
    }
    let mut changed = false;
    let mut reset = false;
    // First sub-row carries the property label + reset gutter; the others are blank on
    // the left so the three fields stack under one heading.
    field_row(
        ui,
        state.panel_w,
        |ui| {
            reset = reset_arrow(ui, differs);
            ui.add(egui::Label::new(label).truncate());
        },
        |ui| {
            let mut dv = DragValue::new(x).speed(speed).prefix("X ");
            if !suffix.is_empty() {
                dv = dv.suffix(suffix);
            }
            changed |= ui.add(dv).changed();
        },
    );
    field_row(ui, state.panel_w, |_ui| {}, |ui| {
        let mut dv = DragValue::new(y).speed(speed).prefix("Y ");
        if !suffix.is_empty() {
            dv = dv.suffix(suffix);
        }
        changed |= ui.add(dv).changed();
    });
    field_row(ui, state.panel_w, |_ui| {}, |ui| {
        let mut dv = DragValue::new(z).speed(speed).prefix("Z ");
        if !suffix.is_empty() {
            dv = dv.suffix(suffix);
        }
        changed |= ui.add(dv).changed();
    });
    if reset {
        *x = 0.0;
        *y = 0.0;
        *z = 0.0;
        changed = true;
    }
    changed
}

/// Render one Inspector **category** (S1): a `CollapsingHeader` whose open/closed
/// state is remembered across sessions in [`InspectorState`] and which hides
/// entirely when the active filter matches none of its `row_labels`.
///
/// `title` is the stable category key (no icon) used both as the persistence key
/// and the filter scope; `header` is the rendered header text (icon + title +
/// optional count). Returns `true` if `body` ran (the category was visible) so
/// callers can chain. The collapse state is forced from the store via `.open(..)`
/// and the user's header click flips + persists it; while filtering, every visible
/// category force-opens so matches aren't hidden behind a caret.
fn category(
    ui: &mut egui::Ui,
    state: &mut InspectorState,
    title: &str,
    header: impl Into<egui::WidgetText>,
    default_open: bool,
    row_labels: &[&str],
    body: impl FnOnce(&mut egui::Ui, &InspectorState),
) -> bool {
    if !state.category_visible(row_labels) {
        return false;
    }
    let open = state.open_state(title, default_open);
    // Snapshot the filter-active flag before borrowing `state` shared in `body`.
    let filtering = state.query().is_some();
    let immut: &InspectorState = state;
    let resp = egui::CollapsingHeader::new(header)
        .id_salt(("inspector-cat", title))
        .open(Some(open))
        .show(ui, |ui| body(ui, immut));
    // A header click toggles the remembered state — but only when not filtering
    // (filtering force-opens; we don't want a click to fight the override).
    if resp.header_response.clicked() && !filtering {
        let next = !open;
        state.collapsed.insert(title.to_string(), next);
        state.save();
    }
    true
}

/// A nested, filter-aware "Advanced ▾" disclosure inside an inspector category
/// (#8 + S1): the common rows are shown by the caller unconditionally; the
/// power-user rows go in `body`, tucked behind this quiet, default-collapsed
/// caret. `salt` disambiguates the (per-category) collapse state. Hides entirely
/// when an active filter matches none of its `rows`, and force-opens (overriding
/// the default-collapsed caret) while a filter is active so matched rows aren't
/// buried.
fn advanced_section_filtered(
    ui: &mut egui::Ui,
    state: &InspectorState,
    salt: &str,
    rows: &[&str],
    body: impl FnOnce(&mut egui::Ui),
) {
    let filtering = state.query().is_some();
    if filtering && !state.category_visible(rows) {
        return;
    }
    ui.add_space(2.0);
    let mut h = egui::CollapsingHeader::new(RichText::new("Advanced").small().weak())
        .id_salt(("inspector-advanced", salt))
        .default_open(false);
    if filtering {
        h = h.open(Some(true));
    }
    h.show(ui, body);
}

/// The editable-property defaults for a placed fixture — the values the per-row
/// revert arrow snaps back to (#6). Sourced from the fixture's GDTF/library
/// template where it's recoverable from the instance, else the neutral
/// [`OpticalControls::default`]/struct constants. Fields whose template value
/// can't be recovered from the instance alone (e.g. a built-in fixture's beam
/// angle after the profile is gone) are `None` → no arrow shown for them.
struct FixtureDefaults {
    pan: f32,
    tilt: f32,
    dimmer: f32,
    beam: f32,
    beam_angle: Option<f32>,
    color: Option<[f32; 3]>,
}

impl FixtureDefaults {
    fn for_fixture(f: &Fixture) -> Self {
        // GDTF fixtures recover their template beam angle from the parsed profile;
        // both kinds share the neutral optics + level/beam constants.
        let beam_angle = f.gdtf.as_ref().map(|g| g.beam_angle.max(1.0));
        Self {
            pan: 0.0,
            tilt: 0.0,
            dimmer: OpticalControls::default().dimmer,
            beam: 1.0,
            beam_angle,
            // The emitted-colour default is white for GDTF (the master tint rest
            // value); a built-in's library tint isn't stored on the instance.
            color: f.gdtf.is_some().then_some([1.0, 1.0, 1.0]),
        }
    }
}

/// The fixture [`EntityId`] the PIN button (P2 #65) would lock onto: the
/// already-pinned id if held, else the single selected fixture (no world / env /
/// geometry / screen / multi-selection in play). `None` when there's nothing a
/// single-fixture pin could target — the button is then disabled.
fn current_pin_target(scene: &Scene, selection: &Selection, state: &InspectorState) -> Option<crate::scene::EntityId> {
    if let Some(id) = state.pinned {
        return Some(id);
    }
    if selection.world || selection.environment.is_some() {
        return None;
    }
    if !selection.geometry.is_empty() || !selection.screens.is_empty() {
        return None;
    }
    let ids: Vec<usize> = selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
    match ids.as_slice() {
        [i] => Some(scene.fixtures[*i].id),
        _ => None,
    }
}

/// Right tab: editable parameters for the current selection. Edits flow
/// straight into the scene, so the viewport updates on the next frame.
#[allow(clippy::too_many_arguments)]
pub fn inspector(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &Selection,
    patch: &mut PatchTable,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
    profile: &mut Option<ProfileEditor>,
    sources: &ScreenSources,
    state: &mut InspectorState,
    edit: &mut InspectorEdit,
    render_ui: &mut RenderUiState,
    settings: &mut crate::scene::RenderSettings,
) {
    // The dock + category/grid indents give the inspector a LEFT margin, but the
    // grids / sliders / checkboxes otherwise run FLUSH to the right edge (asymmetric —
    // the user's repeated note). Reserve a matching RIGHT margin here at the entry
    // point so it covers the filter/header AND every body path (incl. Render
    // Properties); the content then breathes the same on the left and the right.
    const RIGHT_PAD: f32 = 6.0;
    ui.set_max_width((ui.available_width() - RIGHT_PAD).max(140.0));
    // Filter box (S1): a fuzzy/substring row-label filter across all categories.
    // Sits above the scrolling body so it stays visible while scanning matches.
    ui.horizontal(|ui| {
        ui.label(RichText::new(theme::icon::SEARCH).size(13.0));
        ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .hint_text("Filter properties…")
                .desired_width(f32::INFINITY),
        );
    });
    // Header toggles (P2 #64 + #65): "show only modified" and the inspector PIN.
    ui.horizontal(|ui| {
        if !state.filter.trim().is_empty() && ui.small_button(format!("{}  clear", theme::icon::CLOSE)).clicked() {
            state.filter.clear();
        }
        ui.toggle_value(&mut state.show_modified, "Modified only")
            .on_hover_text("Hide every property still at its default value");
        // The pin targets the single fixture currently shown (live selection or
        // the already-pinned one). Disabled when no single fixture is in play.
        let pin_target = current_pin_target(scene, selection, state);
        let pinned = state.pinned.is_some();
        let resp = ui.add_enabled(
            pinned || pin_target.is_some(),
            egui::SelectableLabel::new(pinned, format!("{}  Pin", theme::icon::PROFILE)),
        );
        if resp.on_hover_text("Lock the inspector to this fixture across selection changes").clicked() {
            state.pinned = if pinned { None } else { pin_target };
        }
    });
    // The pin banner: "pinned: <name> [x]" — visible while the lock is held.
    if let Some(id) = state.pinned {
        match scene.fixture_index_of(id) {
            Some(idx) => {
                let name = scene.fixtures[idx].name.clone();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}  pinned: {name}", theme::icon::PROFILE)).small().strong());
                    if ui.small_button(theme::icon::CLOSE).on_hover_text("Unpin — follow selection").clicked() {
                        state.pinned = None;
                    }
                });
            }
            // The pinned fixture was deleted: drop the stale lock silently.
            None => state.pinned = None,
        }
    }
    ui.separator();

    // Render the body in a scope so its content rect is known, then derive the
    // drag edges (#13) from egui's global drag state intersected with that rect —
    // a slider/DragValue drag INSIDE the inspector becomes one undo step without
    // instrumenting every widget. `inspector_body` is the prior function body
    // verbatim (its early returns become early returns from the closure).
    let resp = ui.scope(|ui| inspector_body(ui, scene, selection, patch, gdtf_textures, profile, sources, state, render_ui, settings));
    let content = resp.response.rect;
    let ctx = ui.ctx();
    // A widget id is "in the inspector" when its last-frame rect lies within the
    // panel content rect (read_response gives the rect; missing ⇒ not ours).
    let in_panel = |id: egui::Id| ctx.read_response(id).is_some_and(|r| content.contains(r.rect.center()));
    *edit = InspectorEdit {
        started: ctx.drag_started_id().is_some_and(in_panel),
        stopped: ctx.drag_stopped_id().is_some_and(in_panel),
    };
}

/// The inspector content (extracted so [`inspector`] can wrap it for drag-edge
/// detection, #13). Edits flow straight into the scene as before.
#[allow(clippy::too_many_arguments)]
fn inspector_body(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    selection: &Selection,
    patch: &mut PatchTable,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
    profile: &mut Option<ProfileEditor>,
    sources: &ScreenSources,
    state: &mut InspectorState,
    render_ui: &mut RenderUiState,
    settings: &mut crate::scene::RenderSettings,
) {
    // Width fit (responsiveness fix): the dock disables horizontal scroll for the
    // Inspector, so any over-wide widget would clip and leak. Pin the body to the
    // panel width and shrink egui 0.34's default-wide Slider so its bar fits the
    // value column at a narrow (~280px) inspector — no horizontal overflow. The
    // value column is roughly the panel minus the label/arrow gutter; clamp the
    // slider bar to it (floored so it stays grabbable).
    ui.set_max_width(ui.available_width());
    let avail = ui.available_width();
    // Capture the panel width once so every fixed 2-column row (label + value cell)
    // lines up + fits, whatever the section.
    state.panel_w = avail;
    ui.spacing_mut().slider_width = (avail - 140.0).clamp(60.0, 220.0);
    // Tighten the inter-widget gap + the min interactive width so the multi-field
    // rows (Position/Rotation x/y/z DragValues, the colour well) pack into the
    // value column at a narrow width instead of pushing past the panel edge.
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.spacing_mut().interact_size.x = 24.0;

    // PIN (P2 #65): while a fixture is pinned, the inspector ignores the live
    // selection and stays on that fixture (resolved by stable id, so it survives
    // reorders). A stale pin (deleted fixture) is cleared by the header banner,
    // so here it simply falls through to the selection path.
    if let Some(pin) = state.pinned {
        if let Some(idx) = scene.fixture_index_of(pin) {
            let fixture = &mut scene.fixtures[idx];
            if fixture.is_gdtf() {
                gdtf_inspector(ui, fixture, gdtf_textures, idx, profile, state);
            } else {
                fixture_inspector(ui, fixture, state);
            }
            return;
        }
    }

    // World is the top of the hierarchy: the render properties (resolution /
    // sampling / output) + the shared look/world controls live here.
    if selection.world {
        render_properties(ui, scene, settings, state, render_ui);
        return;
    }

    if let Some(env_id) = selection.environment {
        match scene.environments.get_mut(env_id) {
            Some(env) => environment_inspector(ui, env, state),
            None => {
                ui.label("Selection is no longer valid.");
            }
        }
        return;
    }

    // Static geometry (Objects) takes the Inspector when selected.
    let geo: Vec<usize> = selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
    if !geo.is_empty() {
        geometry_inspector(ui, scene, &geo, state);
        return;
    }

    // LED screens take the Inspector when selected.
    let scr: Vec<usize> = selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
    if let Some(&primary) = scr.first() {
        led_screen_inspector(ui, &mut scene.screens[primary], scr.len(), sources, state);
        return;
    }

    // Keep only still-valid fixture indices.
    let ids: Vec<usize> = selection
        .fixtures
        .iter()
        .copied()
        .filter(|&i| i < scene.fixtures.len())
        .collect();
    match ids.as_slice() {
        [] => {
            ui.label("Nothing selected.");
        }
        [id] => {
            let id = *id;
            let fixture = &mut scene.fixtures[id];
            if fixture.is_gdtf() {
                gdtf_inspector(ui, fixture, gdtf_textures, id, profile, state);
            } else {
                fixture_inspector(ui, fixture, state);
            }
        }
        many => bulk_inspector(ui, scene, patch, many, state),
    }
}

/// Bulk editor shown when several fixtures are selected: edits a shared property
/// on **all** of them at once (set-semantics, seeded from the first selected).
/// Categories are collapsible and the Optics / Wheels rows are **dynamic** — they
/// show the union of controls the selected fixtures actually expose, not a fixed
/// hardcoded list.
fn bulk_inspector(ui: &mut egui::Ui, scene: &mut Scene, patch: &mut PatchTable, ids: &[usize], state: &mut InspectorState) {
    let primary = ids[0];
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  {} fixtures", theme::icon::FIXTURE, ids.len())).strong());
    });
    ui.label(RichText::new("Bulk edit — changes apply to all selected.").weak().small());
    ui.separator();

    // --- DMX MODE (only when every selected fixture shares one profile, so a
    // single mode list applies to all). Drives the patch footprint; decode syncs
    // each fixture's active mode from the patch next frame.
    let p0 = scene.fixtures[primary].profile.clone();
    let same_profile = ids.iter().all(|&i| scene.fixtures[i].profile == p0);
    let ref_modes: Vec<String> = if same_profile {
        scene.fixtures[primary]
            .gdtf
            .as_ref()
            .map(|g| g.modes.iter().map(|m| m.name.clone()).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if ref_modes.len() > 1 {
        let cur = patch.get(primary).map(|p| p.mode_index).unwrap_or(0);
        let cur_name = ref_modes.get(cur).cloned().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("DMX mode");
            let mut pick = None;
            egui::ComboBox::from_id_salt("bulk-mode")
                .selected_text(RichText::new(cur_name).small())
                .show_ui(ui, |ui| {
                    for (mi, name) in ref_modes.iter().enumerate() {
                        if ui.selectable_label(mi == cur, name).clicked() {
                            pick = Some(mi);
                        }
                    }
                });
            if let Some(mi) = pick {
                for &i in ids {
                    let f = &scene.fixtures[i];
                    if f.gdtf.as_ref().is_some_and(|g| mi < g.modes.len()) {
                        patch.set_mode(f, i, mi);
                    }
                }
            }
        });
        ui.separator();
    }

    // --- TRANSFORM ---
    category(
        ui,
        state,
        "Transform",
        format!("{}  Transform", theme::icon::INSPECTOR),
        true,
        &["Pan", "Tilt", "Nudge position", "Nudge rotation"],
        |ui, fs| {
            Grid::new("bulk-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                let pan = common_f32(ids.iter().map(|&i| scene.fixtures[i].pan));
                bulk_f32_row(
                    ui,
                    fs,
                    "Pan",
                    pan,
                    scene.fixtures[primary].pan,
                    |ui, v| ui.add(DragValue::new(v).speed(0.5).range(-270.0..=270.0).suffix("°")),
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].pan = v),
                );
                let tilt = common_f32(ids.iter().map(|&i| scene.fixtures[i].tilt));
                bulk_f32_row(
                    ui,
                    fs,
                    "Tilt",
                    tilt,
                    scene.fixtures[primary].tilt,
                    |ui, v| ui.add(DragValue::new(v).speed(0.5).range(-180.0..=180.0).suffix("°")),
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].tilt = v),
                );
            });
            if fs.row_visible("Nudge position") {
                ui.add_space(4.0);
                ui.label(RichText::new("Nudge position (all)").small().strong());
                ui.horizontal(|ui| {
                    let mut delta = glam::Vec3::ZERO;
                    // Drag from zero applies a delta; the field snaps back each frame.
                    for (axis, label) in [(0usize, "x"), (1, "y"), (2, "z")] {
                        let mut v = 0.0f32;
                        if ui.add(DragValue::new(&mut v).speed(0.05).prefix(format!("{label} "))).changed() {
                            delta[axis] += v;
                        }
                    }
                    if delta != glam::Vec3::ZERO {
                        for &i in ids {
                            scene.fixtures[i].position += delta;
                        }
                    }
                });
            }
            if fs.row_visible("Nudge rotation") {
                ui.add_space(4.0);
                ui.label(RichText::new("Nudge rotation (all · individual origins)").small().strong());
                ui.horizontal(|ui| {
                    // Drag from zero applies a delta rotation about each fixture's OWN
                    // origin (orientation only), so the rig keeps its arrangement and
                    // each head tilts in place — never collapsed onto one pivot.
                    let mut d = glam::Vec3::ZERO; // euler degrees (x, y, z)
                    for (axis, label) in [(0usize, "x"), (1, "y"), (2, "z")] {
                        let mut v = 0.0f32;
                        if ui
                            .add(DragValue::new(&mut v).speed(0.5).suffix("°").prefix(format!("{label} ")))
                            .changed()
                        {
                            d[axis] += v;
                        }
                    }
                    if d != glam::Vec3::ZERO {
                        let delta = glam::Quat::from_euler(
                            glam::EulerRot::YXZ,
                            d.y.to_radians(),
                            d.x.to_radians(),
                            d.z.to_radians(),
                        );
                        for &i in ids {
                            scene.fixtures[i].orientation =
                                (delta * scene.fixtures[i].orientation).normalize();
                        }
                    }
                });
            }
        },
    );

    // --- FIXTURE ---
    category(
        ui,
        state,
        "Fixture",
        format!("{}  Fixture", theme::icon::COLOR),
        true,
        &["Dimmer", "Beam", "Color"],
        |ui, fs| {
            Grid::new("bulk-fixture").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                let dimmer = common_f32(ids.iter().map(|&i| scene.fixtures[i].optics.dimmer));
                bulk_f32_row(
                    ui,
                    fs,
                    "Dimmer",
                    dimmer,
                    scene.fixtures[primary].optics.dimmer,
                    |ui, v| ui.add(Slider::new(v, 0.0..=1.0)),
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].optics.dimmer = v),
                );
                let beam = common_f32(ids.iter().map(|&i| scene.fixtures[i].beam));
                bulk_f32_row(
                    ui,
                    fs,
                    "Beam",
                    beam,
                    scene.fixtures[primary].beam,
                    |ui, v| {
                        ui.add(Slider::new(v, 0.0..=4.0).text("vol"))
                            .on_hover_text("Volumetric beam intensity (0 = off)")
                    },
                    |v| ids.iter().for_each(|&i| scene.fixtures[i].beam = v),
                );
                // Colour: same mixed/unify pattern, but the widget is a colour well.
                if fs.row_visible("Color") {
                    ui.label("Color");
                    match common_rgb(ids.iter().map(|&i| scene.fixtures[i].color)) {
                        Some(mut color) => {
                            if ui.color_edit_button_rgb(&mut color).changed() {
                                for &i in ids {
                                    scene.fixtures[i].color = color;
                                }
                            }
                        }
                        None => {
                            let seed = scene.fixtures[primary].color;
                            if ui
                                .add(egui::Button::new(RichText::new("— Multiple —").small().weak()))
                                .on_hover_text("Colours differ — click to set all to the active colour")
                                .clicked()
                            {
                                for &i in ids {
                                    scene.fixtures[i].color = seed;
                                }
                            }
                        }
                    }
                    ui.end_row();
                }
            });
        },
    );

    // --- OPTICS (dynamic): only fields some selected fixture actually exposes ---
    let supports = |f: OpticField| {
        ids.iter().any(|&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
    };
    let beam: Vec<OpticField> = OpticField::BEAM.into_iter().filter(|&f| supports(f)).collect();
    let color: Vec<OpticField> = OpticField::COLOR.into_iter().filter(|&f| supports(f)).collect();
    if !beam.is_empty() || !color.is_empty() {
        let optic_labels: Vec<&str> = beam.iter().chain(&color).map(|f| f.label()).collect();
        category(
            ui,
            state,
            "Optics",
            format!("{}  Optics", theme::icon::INSPECTOR),
            true,
            &optic_labels,
            |ui, fs| {
                if !beam.is_empty() && fs.category_visible(&beam.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                    ui.label(RichText::new("BEAM SHAPING").small().strong());
                    Grid::new("bulk-beam").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                        for f in &beam {
                            bulk_opt_field(ui, fs, scene, ids, *f);
                        }
                    });
                }
                if !color.is_empty() && fs.category_visible(&color.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                    ui.add_space(4.0);
                    ui.label(RichText::new("COLOR MIXING").small().strong());
                    Grid::new("bulk-color").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                        for f in &color {
                            bulk_opt_field(ui, fs, scene, ids, *f);
                        }
                    });
                }
            },
        );
    }

    // --- WHEELS (dynamic): the union of components across all selected fixtures ---
    let mut wheels: Vec<(WheelKind, u32, String)> = Vec::new();
    for &i in ids {
        let f = &scene.fixtures[i];
        if let Some(comps) = f.gdtf.as_ref().and_then(|g| g.modes.get(f.mode_index)).map(|m| &m.components) {
            for c in comps {
                if !wheels.iter().any(|(k, n, _)| *k == c.kind && *n == c.number) {
                    wheels.push((c.kind, c.number, c.attribute.clone()));
                }
            }
        }
    }
    if !wheels.is_empty() {
        let wheel_labels: Vec<&str> = wheels.iter().map(|(_, _, l)| l.as_str()).collect();
        category(
            ui,
            state,
            "Wheels",
            format!("{}  Wheels", theme::icon::COLOR),
            true,
            &wheel_labels,
            |ui, fs| {
                Grid::new("bulk-wheels").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for (kind, number, label) in &wheels {
                        bulk_wheel(ui, fs, scene, ids, *kind, *number, label);
                    }
                });
            },
        );
    }
}

/// Bulk rows for one wheel component: value + spin sliders applied to the
/// matching component of every selected fixture.
fn bulk_wheel(
    ui: &mut egui::Ui,
    state: &InspectorState,
    scene: &mut Scene,
    ids: &[usize],
    kind: WheelKind,
    number: u32,
    label: &str,
) {
    // Seed from the first selected fixture that actually has this wheel (the
    // union may include wheels the primary doesn't have).
    let Some((seed_value, seed_spin)) = ids
        .iter()
        .find_map(|&i| scene.fixtures[i].wheel_control_mut(kind, number).map(|w| (w.value, w.spin)))
    else {
        return;
    };
    // Mixed-value detection (#7) over only the fixtures that HAVE this wheel.
    let value = common_f32(
        ids.iter().filter_map(|&i| scene.fixtures[i].wheel_control_mut(kind, number).map(|w| w.value)),
    );
    bulk_f32_row(ui, state, label, value, seed_value, |ui, v| ui.add(Slider::new(v, 0.0..=1.0)), |v| {
        for &i in ids {
            if let Some(w) = scene.fixtures[i].wheel_control_mut(kind, number) {
                w.value = v;
            }
        }
    });
    let spin = common_f32(
        ids.iter().filter_map(|&i| scene.fixtures[i].wheel_control_mut(kind, number).map(|w| w.spin)),
    );
    bulk_f32_row(
        ui,
        state,
        &format!("{label} spin"),
        spin,
        seed_spin,
        |ui, v| ui.add(Slider::new(v, 0.0..=1.0).text("0.5=stop")),
        |v| {
            for &i in ids {
                if let Some(w) = scene.fixtures[i].wheel_control_mut(kind, number) {
                    w.spin = v;
                }
            }
        },
    );
}

/// One bulk optics slider for an [`OpticField`], written to every selected
/// fixture (range-aware: e.g. green tint is bipolar). Seeds from the first
/// selected fixture that actually exposes the field (the union may include a
/// control the primary doesn't have), falling back to the primary.
fn bulk_opt_field(ui: &mut egui::Ui, state: &InspectorState, scene: &mut Scene, ids: &[usize], f: OpticField) {
    let seed = ids
        .iter()
        .copied()
        .find(|&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
        .unwrap_or(ids[0]);
    // Mixed-value detection (#7) over only the fixtures that EXPOSE this field.
    let common = common_f32(
        ids.iter()
            .filter(|&&i| scene.fixtures[i].gdtf.as_ref().is_some_and(|g| f.supported(g)))
            .map(|&i| f.get(&scene.fixtures[i].optics)),
    );
    bulk_f32_row(
        ui,
        state,
        f.label(),
        common,
        f.get(&scene.fixtures[seed].optics),
        |ui, v| ui.add(Slider::new(v, f.range())),
        |v| {
            for &i in ids {
                f.set(&mut scene.fixtures[i].optics, v);
            }
        },
    );
}

fn fixture_inspector(ui: &mut egui::Ui, fixture: &mut Fixture, state: &mut InspectorState) {
    ui.horizontal(|ui| {
        ui.heading(fixture.name.as_str());
        // Provenance chip (Built-in / GDTF / MVR) — right-aligned beside the name.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            source_chip(ui, fixture.source);
        });
    });
    ui.label(RichText::new(format!("{} · {}", fixture.category, fixture.profile)).weak().small());
    ui.separator();

    // The editable property grid (Transform + Fixture) is declared by `impl Inspect
    // for Fixture` and rendered uniformly by the property builder.
    props::show(ui, state, fixture);
}

fn environment_inspector(ui: &mut egui::Ui, env: &mut Environment, state: &mut InspectorState) {
    ui.horizontal(|ui| {
        ui.heading(env.name.as_str());
    });
    ui.label(RichText::new(format!("{:?}", env.kind)).weak().small());
    ui.separator();

    // Editable grid declared by `impl Inspect for Environment`.
    props::show(ui, state, env);
}

/// Inspector for a selected static-geometry object (an imported stage deck,
/// truss, or set piece): identity, visibility, and an editable world transform
/// (position / rotation / uniform scale), decomposed from its 4×4 and recomposed
/// only when a field changes (so a one-off non-uniform import isn't flattened).
fn geometry_inspector(ui: &mut egui::Ui, scene: &mut Scene, ids: &[usize], state: &mut InspectorState) {
    let primary = ids[0];
    let Some(g) = scene.geometry.get_mut(primary) else {
        ui.label("Selection is no longer valid.");
        return;
    };
    ui.heading(g.name.as_str());
    let kind = g.mvr.as_ref().map(|m| m.kind.as_str()).filter(|k| !k.is_empty()).unwrap_or("Object");
    ui.label(
        RichText::new(format!("{kind} · {} model{}", g.models.len(), if g.models.len() == 1 { "" } else { "s" }))
            .weak()
            .small(),
    );
    if ids.len() > 1 {
        ui.label(RichText::new(format!("{} objects — editing the active one", ids.len())).weak().small());
    }
    ui.separator();

    ui.horizontal(|ui| {
        let mut visible = !g.hidden;
        if ui.checkbox(&mut visible, "Visible").changed() {
            g.hidden = !visible;
        }
    });

    // Position is read/written via the translation column directly (lossless), so
    // a pure move never disturbs a non-uniform/sheared import. Rotation + scale
    // are decomposed for display and only re-composed (to a clean uniform basis)
    // when the user actually edits one of them.
    let (scale0, rot0, _trans0) = g.transform.to_scale_rotation_translation();
    let mut pos = g.transform.w_axis.truncate();
    let mut uscale = ((scale0.x + scale0.y + scale0.z) / 3.0).max(1e-3);
    let mut rot = rot0;
    let bounds = g.world_bounds();
    let mut pos_changed = false;
    let mut rs_changed = false;

    // Position is lossless (translation column only); Rotation (→identity) + Scale
    // (→unit) recompose to a clean uniform basis only when the user edits one.
    props::with(ui, state, |p| {
        p.group("Transform", theme::icon::INSPECTOR, true, |p| {
            pos_changed |= p.vec3("Position", &mut pos).speed(0.05).show();
            rs_changed |= p.rotation("Rotation", &mut rot);
            rs_changed |= p
                .f32("Scale", &mut uscale)
                .speed(0.005)
                .range(0.001..=1000.0)
                .default(1.0)
                .show();
            if let Some((lo, hi)) = bounds {
                let s = hi - lo;
                p.note(format!("size  {:.2} × {:.2} × {:.2} m", s.x, s.y, s.z));
            }
        });
    });

    if rs_changed {
        g.transform = Mat4::from_scale_rotation_translation(Vec3::splat(uscale), rot, pos);
    } else if pos_changed {
        // Pure move: rewrite only the translation column, keeping the original
        // (possibly non-uniform) basis intact.
        g.transform.w_axis = pos.extend(1.0);
    }
}

/// Inspector for a selected LED screen: identity, transform, the parametric
/// cabinet grid (with a live derived-resolution readout), surface photometry,
/// and the content source. Phase 1 covers Test Pattern + Solid Colour content;
/// the cabinet is editable directly (the panel TYPE is set from the Library).
fn led_screen_inspector(ui: &mut egui::Ui, s: &mut LedScreen, count: usize, sources: &ScreenSources, state: &mut InspectorState) {
    ui.heading(s.name.as_str());
    let [rx, ry] = s.resolution();
    let [mw, mh] = s.size_m();
    ui.label(
        RichText::new(format!("{} · {} × {} px · {:.2} × {:.2} m", s.panel_type, rx, ry, mw, mh))
            .weak()
            .small(),
    );
    if count > 1 {
        ui.label(RichText::new(format!("{count} screens — editing the active one")).weak().small());
    }
    ui.separator();

    ui.horizontal(|ui| {
        let mut visible = !s.hidden;
        if ui.checkbox(&mut visible, "Visible").changed() {
            s.hidden = !visible;
        }
    });

    // --- Transform (position / rotation / uniform scale, lossless like geometry) ---
    let (scale0, rot0, _t0) = s.transform.to_scale_rotation_translation();
    let mut pos = s.transform.w_axis.truncate();
    let mut uscale = ((scale0.x + scale0.y + scale0.z) / 3.0).max(1e-3);
    let (ryr, rxr, rzr) = rot0.to_euler(glam::EulerRot::YXZ);
    let (mut ey, mut ex, mut ez) = (ryr.to_degrees(), rxr.to_degrees(), rzr.to_degrees());
    let mut pos_changed = false;
    let mut rs_changed = false;
    category(
        ui,
        state,
        "Transform",
        format!("{}  Transform", theme::icon::INSPECTOR),
        true,
        &["Position", "Rotation", "Scale"],
        |ui, fs| {
            Grid::new("led-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                // Position/Rotation STACK X/Y/Z (Blender-style) — readable at any width.
                pos_changed |= vec3_rows(
                    ui, fs, "Position", false, 0.05, "",
                    &mut pos.x, &mut pos.y, &mut pos.z,
                );
                rs_changed |= vec3_rows(ui, fs, "Rotation", false, 0.5, "°", &mut ex, &mut ey, &mut ez);
                row(ui, fs, "Scale", false, |ui| {
                    rs_changed |= ui.add(DragValue::new(&mut uscale).speed(0.005).range(0.001..=1000.0)).changed();
                });
            });
        },
    );
    if rs_changed {
        let rot = glam::Quat::from_euler(glam::EulerRot::YXZ, ey.to_radians(), ex.to_radians(), ez.to_radians());
        s.transform = Mat4::from_scale_rotation_translation(Vec3::splat(uscale), rot, pos);
    } else if pos_changed {
        s.transform.w_axis = pos.extend(1.0);
    }

    // --- Panel: one cabinet's size + native pixels (pitch is derived) ---
    category(
        ui,
        state,
        "Panel",
        format!("{}  Panel", theme::icon::SCREEN),
        true,
        &["Cabinet (mm)", "Pixels / cabinet", "Pitch"],
        |ui, fs| {
            Grid::new("led-panel").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                row(ui, fs, "Cabinet (mm)", false, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(DragValue::new(&mut s.cabinet_mm[0]).speed(1.0).range(50.0..=2000.0).prefix("w "));
                        ui.add(DragValue::new(&mut s.cabinet_mm[1]).speed(1.0).range(50.0..=2000.0).prefix("h "));
                    });
                });
                row(ui, fs, "Pixels / cabinet", false, |ui| {
                    ui.horizontal(|ui| {
                        let mut px = s.cabinet_px[0] as i32;
                        let mut py = s.cabinet_px[1] as i32;
                        if ui.add(DragValue::new(&mut px).speed(1.0).range(8..=1024).prefix("x ")).changed() {
                            s.cabinet_px[0] = px.max(1) as u32;
                        }
                        if ui.add(DragValue::new(&mut py).speed(1.0).range(8..=1024).prefix("y ")).changed() {
                            s.cabinet_px[1] = py.max(1) as u32;
                        }
                    });
                });
                row(ui, fs, "Pitch", false, |ui| {
                    ui.label(RichText::new(format!("{:.2} mm", s.pitch_mm())).weak());
                });
            });
        },
    );

    // --- Array: panels wide × high → live derived total resolution + size ---
    category(
        ui,
        state,
        "Array",
        format!("{}  Array", theme::icon::PATCH),
        true,
        &["Panels", "Gap"],
        |ui, fs| {
            Grid::new("led-array").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                row(ui, fs, "Panels", false, |ui| {
                    ui.horizontal(|ui| {
                        let mut w = s.panels_wide as i32;
                        let mut h = s.panels_high as i32;
                        if ui.add(DragValue::new(&mut w).speed(0.1).range(1..=64).prefix("w ")).changed() {
                            s.panels_wide = w.max(1) as u32;
                        }
                        if ui.add(DragValue::new(&mut h).speed(0.1).range(1..=64).prefix("h ")).changed() {
                            s.panels_high = h.max(1) as u32;
                        }
                    });
                });
                row(ui, fs, "Gap", false, |ui| {
                    ui.add(DragValue::new(&mut s.gap_mm).speed(0.1).range(0.0..=50.0).suffix(" mm"));
                });
            });
            let [rx, ry] = s.resolution();
            let [mw, mh] = s.size_m();
            let mpx = (rx as f64 * ry as f64) / 1_000_000.0;
            ui.label(
                RichText::new(format!("{rx} × {ry} px  ·  {mpx:.2} Mpx  ·  {mw:.2} × {mh:.2} m"))
                    .strong()
                    .small(),
            );
        },
    );

    // --- Surface: photometry + transparency + curvature ---
    category(
        ui,
        state,
        "Surface",
        format!("{}  Surface", theme::icon::COLOR),
        true,
        &["Brightness", "Light emit", "Gamma", "Transparency", "Curvature", "Pixel"],
        |ui, fs| {
            Grid::new("led-surface").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                row(ui, fs, "Brightness", false, |ui| {
                    ui.add(DragValue::new(&mut s.nits).speed(10.0).range(50.0..=8000.0).suffix(" nits"));
                });
                row(ui, fs, "Light emit", false, |ui| {
                    ui.add(DragValue::new(&mut s.emit).speed(0.02).range(0.0..=4.0))
                        .on_hover_text("How much the wall lights the scene + haze (0 = none)");
                });
                row(ui, fs, "Gamma", false, |ui| {
                    ui.add(DragValue::new(&mut s.gamma).speed(0.01).range(1.0..=3.0));
                });
                slider_row(ui, fs, "Transparency", false, |ui| {
                    let mut transp = 1.0 - s.opacity;
                    if ui.add(Slider::new(&mut transp, 0.0..=1.0)).on_hover_text("See-through / mesh LED").changed() {
                        s.opacity = (1.0 - transp).clamp(0.0, 1.0);
                    }
                });
                row(ui, fs, "Curvature", false, |ui| {
                    ui.add(DragValue::new(&mut s.curvature_deg).speed(0.5).range(-60.0..=60.0).suffix("°"))
                        .on_hover_text("Horizontal arc subtended across the wall");
                });
                row(ui, fs, "Pixel", false, |ui| {
                    egui::ComboBox::from_id_salt("led-pixel-shape")
                        .selected_text(s.pixel_shape.label())
                        .show_ui(ui, |ui| {
                            for sh in PixelShape::ALL {
                                ui.selectable_value(&mut s.pixel_shape, sh, sh.label());
                            }
                        })
                        .response
                        .on_hover_text("LED package shape seen up close (SMD round/square, or discrete RGB sub-pixels)");
                });
            });
        },
    );

    // --- Content: the source shown on the surface ---
    // Whole-category filter only (the body is combo/dynamic, not grid rows).
    category(
        ui,
        state,
        "Content",
        format!("{}  Content", theme::icon::IMAGE),
        true,
        &["Content", "Source", "Pattern", "Colour", "Image", "Grid", "Patch"],
        |ui, _fs| {
            #[derive(PartialEq, Clone, Copy)]
            enum Kind {
                Test,
                Solid,
                Image,
                Ndi,
                Citp,
                Dmx,
            }
            let cur = match &s.content {
                ScreenContent::TestPattern(_) => Kind::Test,
                ScreenContent::SolidColor(_) => Kind::Solid,
                ScreenContent::Image { .. } => Kind::Image,
                ScreenContent::Ndi { .. } => Kind::Ndi,
                ScreenContent::Citp { .. } => Kind::Citp,
                ScreenContent::PixelMapDmx(_) => Kind::Dmx,
            };
            let mut sel = cur;
            ui.horizontal(|ui| {
                ui.label("Source");
                egui::ComboBox::from_id_salt("led-source").selected_text(s.content.label()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, Kind::Test, "Test Pattern");
                    ui.selectable_value(&mut sel, Kind::Solid, "Solid Colour");
                    ui.selectable_value(&mut sel, Kind::Image, "Image…");
                    ui.selectable_value(&mut sel, Kind::Ndi, "NDI");
                    ui.selectable_value(&mut sel, Kind::Citp, "CITP");
                    ui.selectable_value(&mut sel, Kind::Dmx, "Pixel-map DMX");
                });
            });
            if sel != cur {
                s.frame = None; // drop any live frame from the previous source
                s.content = match sel {
                    Kind::Test => ScreenContent::TestPattern(TestPattern::Grid),
                    Kind::Solid => ScreenContent::SolidColor([0.1, 0.4, 0.9]),
                    Kind::Image => {
                        ScreenContent::Image { name: String::new(), bytes: std::sync::Arc::new(Vec::new()) }
                    }
                    Kind::Ndi => ScreenContent::Ndi { source: String::new() },
                    Kind::Citp => ScreenContent::Citp { source: String::new() },
                    Kind::Dmx => ScreenContent::PixelMapDmx(crate::scene::screen::PixelMap::default()),
                };
            }
            ui.add_space(2.0);
            match &mut s.content {
                ScreenContent::TestPattern(tp) => {
                    ui.horizontal(|ui| {
                        ui.label("Pattern");
                        for p in TestPattern::ALL {
                            ui.selectable_value(tp, p, p.label());
                        }
                    });
                }
                ScreenContent::SolidColor(c) => {
                    ui.horizontal(|ui| {
                        ui.label("Colour");
                        ui.color_edit_button_rgb(c);
                    });
                }
                ScreenContent::Image { name, bytes } => {
                    ui.horizontal(|ui| {
                        if ui.button("Choose image…").clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "gif", "webp", "tga", "exr", "hdr"])
                                .pick_file()
                        {
                            match std::fs::read(&path) {
                                Ok(b) => {
                                    *bytes = std::sync::Arc::new(b);
                                    *name = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("image")
                                        .to_string();
                                }
                                Err(e) => log::error!("read screen image: {e}"),
                            }
                        }
                        let label = if name.is_empty() { "no image".to_string() } else { name.clone() };
                        ui.label(RichText::new(label).weak());
                    });
                }
                ScreenContent::Ndi { source } => {
                    ui.horizontal(|ui| {
                        ui.label("Source");
                        ui.text_edit_singleline(source);
                    });
                    if !sources.ndi.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Discovered:").weak().small());
                            for name in &sources.ndi {
                                if ui.small_button(name).clicked() {
                                    *source = name.clone();
                                }
                            }
                        });
                    }
                    let hint = if sources.ndi_available {
                        "NDI source name (e.g. \"HOST (Output 1)\"). Pick a discovered source above."
                    } else {
                        "NDI runtime not available (build with `--features ndi` + install the NDI runtime)."
                    };
                    ui.label(RichText::new(hint).weak().small());
                }
                ScreenContent::Citp { source } => {
                    ui.horizontal(|ui| {
                        ui.label("Source");
                        ui.text_edit_singleline(source);
                    });
                    if !sources.citp.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Servers:").weak().small());
                            for name in &sources.citp {
                                if ui.small_button(name).clicked() {
                                    *source = name.clone();
                                }
                            }
                        });
                    }
                    ui.label(
                        RichText::new("CITP/MSEX media-server stream as \"server | layer\" (servers auto-discovered on the LAN).")
                            .weak()
                            .small(),
                    );
                }
                ScreenContent::PixelMapDmx(pm) => {
                    Grid::new("led-pixelmap").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                        ui.label("Grid");
                        ui.horizontal(|ui| {
                            let mut c = pm.cols as i32;
                            let mut r = pm.rows as i32;
                            if ui.add(DragValue::new(&mut c).speed(0.1).range(1..=64).prefix("cols ")).changed() {
                                pm.cols = c.max(1) as u32;
                            }
                            if ui.add(DragValue::new(&mut r).speed(0.1).range(1..=64).prefix("rows ")).changed() {
                                pm.rows = r.max(1) as u32;
                            }
                        });
                        ui.end_row();
                        ui.label("Patch");
                        ui.horizontal(|ui| {
                            let mut u = pm.universe as i32;
                            let mut a = pm.start_address as i32;
                            if ui.add(DragValue::new(&mut u).speed(0.1).range(0..=63999).prefix("univ ")).changed() {
                                pm.universe = u.clamp(0, 63999) as u16;
                            }
                            if ui.add(DragValue::new(&mut a).speed(0.5).range(1..=512).prefix("addr ")).changed() {
                                pm.start_address = a.clamp(1, 512) as u16;
                            }
                        });
                        ui.end_row();
                    });
                    let chans = pm.cols * pm.rows * 3;
                    ui.label(
                        RichText::new(format!(
                            "{}×{} cells · {chans} ch (RGB) · low-res only — use NDI/CITP/media for hi-res",
                            pm.cols, pm.rows
                        ))
                        .weak()
                        .small(),
                    );
                }
            }
        },
    );
}

/// Inspector for an imported GDTF fixture: identity + thumbnail, editable
/// instance params, wheels (with slot images), and the DMX modes/channels.
fn gdtf_inspector(
    ui: &mut egui::Ui,
    fixture: &mut Fixture,
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
    fixture_id: usize,
    profile: &mut Option<ProfileEditor>,
    state: &mut InspectorState,
) {
    let gdtf = fixture.gdtf.clone().expect("gdtf");
    let key = Arc::as_ptr(&gdtf) as usize;
    let tex = gdtf_textures
        .entry(key)
        .or_insert_with(|| load_gdtf_textures(ui.ctx(), &gdtf));

    ui.horizontal(|ui| {
        ui.heading(gdtf.name.as_str());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!("{}  Profile…", theme::icon::PROFILE))
                .on_hover_text("Open the full fixture profile editor")
                .clicked()
            {
                *profile = Some(ProfileEditor::new(fixture_id));
            }
            // Provenance chip (GDTF / MVR) — left of the Profile button.
            source_chip(ui, fixture.source);
        });
    });
    ui.label(
        RichText::new(format!("{} · {}", gdtf.manufacturer, gdtf.long_name))
            .weak()
            .small(),
    );

    if let Some(thumb) = &tex.thumbnail {
        let s = thumb.size_vec2();
        let w = 200.0_f32.min(ui.available_width());
        let h = w * s.y / s.x.max(1.0);
        ui.add_space(4.0);
        ui.image((thumb.id(), egui::vec2(w, h)));
    }

    // Physical source / beam spec from the GDTF Beam geometry.
    let b = &gdtf.beam;
    ui.label(
        RichText::new(format!(
            "{} engine · {:.0} K · CRI {:.0} · {:.0} lm · {:.0} W",
            b.lamp_type, b.color_temp, b.cri, b.luminous_flux, b.power
        ))
        .weak()
        .small(),
    );
    ui.label(
        RichText::new(format!(
            "{} · beam {:.0}° / field {:.0}° · throw {:.2}",
            b.beam_type, b.beam_angle, b.field_angle, b.throw_ratio
        ))
        .weak()
        .small(),
    );
    // Multi-emitter summary: cell count + the live per-cell colors (driven by
    // per-pixel DMX; the Color picker below multiplies all of them manually).
    let emitters = fixture.emitters();
    if emitters.len() > 1 {
        let visible = emitters.iter().filter(|e| e.merged_into.is_none()).count();
        ui.label(
            RichText::new(format!(
                "{} emitters · {} {} · per-cell DMX in mode \"{}\"",
                visible,
                emitters[0].beam.beam_type,
                if emitters.len() > visible { "(+1 overlay)" } else { "" },
                gdtf.modes
                    .get(fixture.mode_index)
                    .map(|m| m.name.as_str())
                    .unwrap_or("?"),
            ))
            .weak()
            .small(),
        );
        ui.horizontal_wrapped(|ui| {
            for (i, em) in emitters.iter().enumerate() {
                if em.merged_into.is_some() {
                    continue;
                }
                let c = fixture.cells.get(i).copied().unwrap_or([1.0, 1.0, 1.0]);
                let level = (fixture.intensity * fixture.optics.dimmer).clamp(0.0, 1.0);
                let col = egui::Color32::from_rgb(
                    ((c[0].min(1.0) * level).powf(1.0 / 2.2) * 255.0) as u8,
                    ((c[1].min(1.0) * level).powf(1.0 / 2.2) * 255.0) as u8,
                    ((c[2].min(1.0) * level).powf(1.0 / 2.2) * 255.0) as u8,
                );
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                ui.painter().rect_filled(rect, 7.0, col);
                ui.painter().rect_stroke(
                    rect,
                    7.0,
                    egui::Stroke::new(1.0, egui::Color32::from_gray(70)),
                    egui::StrokeKind::Inside,
                );
                resp.on_hover_text(&em.name);
            }
        });
    }

    // MVR patch identity (FixtureID, DMX address, mode) when imported from a scene.
    if let Some(m) = fixture.mvr.as_deref() {
        let id = if m.fixture_id.is_empty() { "—" } else { m.fixture_id.as_str() };
        let addr = m
            .addresses
            .first()
            .map(|a| format!("{}.{:03}", a.universe(), a.channel()))
            .unwrap_or_else(|| "—".into());
        let mode = if m.gdtf_mode.is_empty() { "—" } else { m.gdtf_mode.as_str() };
        ui.label(
            RichText::new(format!("MVR · ID {id} · addr {addr} · {mode}"))
                .weak()
                .small(),
        )
        .on_hover_text("Fixture ID · DMX universe.channel · mode, from the imported MVR patch");
    }

    ui.separator();
    let def = FixtureDefaults::for_fixture(fixture);
    category(
        ui,
        state,
        "Transform",
        format!("{}  Transform", theme::icon::INSPECTOR),
        true,
        &["Position", "Rotation", "Move speed"],
        |ui, fs| {
            Grid::new("gdtf-transform").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                // Position/Rotation STACK X/Y/Z (Blender-style) — readable at any width.
                vec3_rows(
                    ui, fs, "Position", false, 0.05, "",
                    &mut fixture.position.x, &mut fixture.position.y, &mut fixture.position.z,
                );
                // Rotation = the rig HANG orientation, separate from the live Pan/Tilt.
                // Euler (YXZ) for display; recomposed on edit; revert arrow → identity.
                let (ry, rx, rz) = fixture.orientation.to_euler(glam::EulerRot::YXZ);
                let (mut ey, mut ex, mut ez) = (ry.to_degrees(), rx.to_degrees(), rz.to_degrees());
                let differs = !approx(ex, 0.0) || !approx(ey, 0.0) || !approx(ez, 0.0);
                if vec3_rows(ui, fs, "Rotation", differs, 0.5, "°", &mut ex, &mut ey, &mut ez) {
                    fixture.orientation =
                        glam::Quat::from_euler(glam::EulerRot::YXZ, ey.to_radians(), ex.to_radians(), ez.to_radians());
                }
                if slider_row(ui, fs, "Move speed", !approx(fixture.move_speed, 0.0), |ui| {
                    ui.add(Slider::new(&mut fixture.move_speed, 0.0..=1.0).max_decimals(2))
                        .on_hover_text("Pan/tilt motor speed: 0 = fastest (snap), 1 = slowest");
                }) {
                    fixture.move_speed = 0.0;
                }
            });
        },
    );

    category(
        ui,
        state,
        "Fixture",
        format!("{}  Fixture", theme::icon::COLOR),
        true,
        &["Pan", "Tilt", "Dimmer", "Color", "Beam"],
        |ui, fs| {
            Grid::new("gdtf-fixture").num_columns(2).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                // Pan/Tilt: the live-aim head angles (moved here from Transform — they're
                // a fixture property, not the rig placement). Each reverts to rest 0.
                if row(ui, fs, "Pan", !approx(fixture.pan, def.pan), |ui| {
                    ui.add(DragValue::new(&mut fixture.pan).speed(0.5).range(-270.0..=270.0).suffix("°"))
                        .on_hover_text(format!("commanded · now {:.0}°", fixture.pan_actual));
                }) {
                    fixture.pan = def.pan;
                }
                if row(ui, fs, "Tilt", !approx(fixture.tilt, def.tilt), |ui| {
                    ui.add(DragValue::new(&mut fixture.tilt).speed(0.5).range(-135.0..=135.0).suffix("°"))
                        .on_hover_text(format!("commanded · now {:.0}°", fixture.tilt_actual));
                }) {
                    fixture.tilt = def.tilt;
                }
                if row(ui, fs, "Dimmer", !approx(fixture.optics.dimmer, def.dimmer), |ui| {
                    ui.add(DragValue::new(&mut fixture.optics.dimmer).speed(0.005).range(0.0..=1.0));
                }) {
                    fixture.optics.dimmer = def.dimmer;
                }
                let color_differs = def.color.is_some_and(|d| !approx_rgb(fixture.color, d));
                if row(ui, fs, "Color", color_differs, |ui| {
                    ui.color_edit_button_rgb(&mut fixture.color);
                }) {
                    if let Some(d) = def.color {
                        fixture.color = d;
                    }
                }
            });
            advanced_section_filtered(ui, fs, "gdtf-fixture", &["Beam"], |ui| {
                Grid::new("gdtf-fixture-adv").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    if row(ui, fs, "Beam", !approx(fixture.beam, def.beam), |ui| {
                        ui.add(DragValue::new(&mut fixture.beam).speed(0.01).range(0.0..=4.0))
                            .on_hover_text("Volumetric beam intensity (0 = off, 1 = normal)");
                    }) {
                        fixture.beam = def.beam;
                    }
                });
            });
        },
    );

    optics_section(ui, fixture, &gdtf, state);

    // Wheel slot gallery — labels are the wheel names; the row filter scopes which
    // wheels show, the category hides if none match.
    let wheel_labels: Vec<&str> = gdtf.wheels.iter().map(|w| w.name.as_str()).collect();
    category(
        ui,
        state,
        "Wheels",
        format!("Wheels ({})", gdtf.wheels.len()),
        false,
        &wheel_labels,
        |ui, fs| {
            for (wi, wheel) in gdtf.wheels.iter().enumerate() {
                if !fs.row_visible(&wheel.name) {
                    continue;
                }
                ui.label(RichText::new(&wheel.name).strong().small());
                ui.horizontal_wrapped(|ui| {
                    for (si, slot) in wheel.slots.iter().enumerate() {
                        let handle = tex
                            .wheels
                            .get(wi)
                            .and_then(|w| w.get(si))
                            .and_then(|h| h.as_ref());
                        let size = egui::vec2(42.0, 42.0);
                        if let Some(h) = handle {
                            ui.image((h.id(), size)).on_hover_text(&slot.name);
                        } else {
                            let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
                            let col = slot
                                .color
                                .map(|c| {
                                    egui::Color32::from_rgb(
                                        (c[0] * 255.0) as u8,
                                        (c[1] * 255.0) as u8,
                                        (c[2] * 255.0) as u8,
                                    )
                                })
                                .unwrap_or(egui::Color32::from_gray(40));
                            ui.painter().rect_filled(rect, 4.0, col);
                            resp.on_hover_text(&slot.name);
                        }
                    }
                });
                ui.add_space(4.0);
            }
        },
    );

    // DMX modes — a reference table (per-channel attributes). Whole-category
    // filter: matches on "DMX modes" plus each mode name + every attribute, so a
    // query like "dmx", a mode name, or a channel attribute keeps it visible.
    let mut dmx_labels: Vec<&str> = vec!["DMX modes"];
    for m in &gdtf.modes {
        dmx_labels.push(m.name.as_str());
        for ch in &m.channels {
            dmx_labels.push(ch.attribute.as_str());
        }
    }
    category(
        ui,
        state,
        "DMX modes",
        format!("DMX modes ({})", gdtf.modes.len()),
        false,
        &dmx_labels,
        |ui, _fs| {
            for mode in &gdtf.modes {
                egui::CollapsingHeader::new(format!("{} — {} ch", mode.name, mode.footprint))
                    .id_salt(&mode.name)
                    .show(ui, |ui| {
                        Grid::new(format!("dmx-{}", mode.name))
                            .num_columns(3)
                            .striped(true)
                            .spacing([10.0, 2.0])
                            .show(ui, |ui| {
                                ui.strong("Addr");
                                ui.strong("Attribute");
                                ui.strong("Function");
                                ui.end_row();
                                for ch in &mode.channels {
                                    let addr = ch
                                        .offsets
                                        .first()
                                        .map(|o| o.to_string())
                                        .unwrap_or_else(|| "—".into());
                                    ui.monospace(addr);
                                    ui.label(&ch.attribute);
                                    ui.label(&ch.function);
                                    ui.end_row();
                                }
                            });
                    });
            }
        },
    );
}

/// EditCondition-style inertness (P2 #66): whether an optic control, though the
/// fixture *exposes* it, is doing nothing at the fixture's current settings and so
/// should be greyed/disabled. The driver is the live fixture state, mirroring how
/// Blender greys a property whose effect another value has nullified:
///
/// * Every beam-shaping / colour control is inert when the lamp emits no light —
///   the dimmer is at 0 (nothing in the beam to zoom/iris/tint), OR the per-fixture
///   volumetric beam is killed (`beam == 0`) so there's no shaft to shape.
/// * The strobe rate is also inert when the shutter gate is fully closed
///   (`shutter == 0`) — a closed blackout can't strobe.
///
/// A pure predicate so it's unit-testable independent of egui.
fn optic_inert(fixture: &Fixture, f: OpticField) -> bool {
    // Only the DIMMER (the master level) makes the fixture dark. `fixture.beam`
    // merely scales the volumetric shaft/floor-pool — the lens still emits + shapes
    // surfaces with beam=0, so optics stay live (see Fixture.beam docs).
    let dark = approx(fixture.optics.dimmer, 0.0);
    if dark {
        return true;
    }
    // Strobe needs an open (or at least non-zero) shutter to pulse.
    matches!(f, OpticField::Strobe) && approx(fixture.optics.shutter, 0.0)
}

/// The optical-chain control bank for a GDTF fixture: sliders for every stage
/// the fixture actually exposes (disabled if the GDTF lacks that attribute).
/// Drives `fixture.optics`, which the renderer resolves into the beam each frame.
/// One data-driven optics slider row: label + range-aware slider (disabled when
/// the fixture doesn't expose it), with optional trailing text (e.g. zoom °).
fn optic_field_row(
    ui: &mut egui::Ui,
    state: &InspectorState,
    o: &mut OpticalControls,
    def: &OpticalControls,
    f: OpticField,
    enabled: bool,
    inert: bool,
    text: Option<String>,
) {
    // Per-property reset (#6): the arrow shows when this control left its neutral
    // default and is reachable (an unsupported/greyed row never differs anyway).
    let differs = enabled && !approx(f.get(o), f.get(def));
    // P2 #64: "show only modified" swallows rows still at their default. The
    // caller already filtered by label visibility, but the modified-only gate
    // depends on the live value, so it's applied here.
    if state.show_modified && !differs {
        return;
    }
    // P2 #66 (EditCondition): an inert control (supported but currently doing
    // nothing — e.g. beam shaping with the dimmer at 0) is greyed AND disabled,
    // so it reads as present-but-not-acting rather than absent.
    let enabled = enabled && !inert;
    let mut v = f.get(o);
    // Keep the readout narrow (was 5+ decimals) so it fits the value cell; when there's
    // a derived label (e.g. zoom degrees "58°") show ONLY that, not the raw 0-1 value.
    let mut slider = Slider::new(&mut v, f.range()).max_decimals(2);
    if let Some(t) = text {
        slider = slider.text(t).show_value(false);
    }
    let mut reset = false;
    let mut changed = false;
    slider_field_row(
        ui,
        state.panel_w,
        |ui| {
            reset = reset_arrow(ui, differs);
            // Grey the label of an inert control so the whole row reads disabled.
            let text = if inert { RichText::new(f.label()).weak() } else { RichText::new(f.label()) };
            let lbl = ui.add(egui::Label::new(text).truncate());
            if inert {
                lbl.on_hover_text("Inactive at the current settings (no light to shape)");
            } else if f == OpticField::Green {
                lbl.on_hover_text("Plus/minus-green (CC axis): −1 magenta … +1 green");
            }
        },
        |ui| {
            changed = ui.add_enabled(enabled, slider).changed();
        },
    );
    if changed {
        f.set(o, v);
    }
    if reset {
        f.set(o, f.get(def));
    }
}

fn optics_section(ui: &mut egui::Ui, fixture: &mut Fixture, gdtf: &GdtfFixture, state: &mut InspectorState) {
    let beam_angle = fixture.beam_angle;
    // The dynamic wheel chain of the active mode (any number of color/gobo/
    // prism/animation/frost components).
    let components: Vec<crate::gdtf::OpticalComponent> = gdtf
        .modes
        .get(fixture.mode_index)
        .map(|m| m.components.clone())
        .unwrap_or_default();
    fixture.optics.ensure_wheels(components.len());

    // The full label universe of this optics bank (used to decide whether the
    // category survives the filter): every exposed optic field, the wheel
    // component names, plus the shutter-blade picker.
    const BEAM_COMMON: [OpticField; 3] = [OpticField::Zoom, OpticField::Focus, OpticField::Iris];
    const BEAM_ADV: [OpticField; 3] = [OpticField::Ca, OpticField::Shutter, OpticField::Strobe];
    const COLOR_COMMON: [OpticField; 4] =
        [OpticField::Cto, OpticField::Cyan, OpticField::Magenta, OpticField::Yellow];
    const COLOR_ADV: [OpticField; 1] = [OpticField::Green];
    let mut all_labels: Vec<&str> = vec!["Shutter blades"];
    for f in BEAM_COMMON.iter().chain(&BEAM_ADV).chain(&COLOR_COMMON).chain(&COLOR_ADV) {
        all_labels.push(f.label());
    }
    for comp in &components {
        all_labels.push(comp.wheel.as_deref().unwrap_or(comp.attribute.as_str()));
    }

    category(
        ui,
        state,
        "Optics",
        "Optics",
        true,
        &all_labels,
        |ui, fs| {
            let zoom_deg = optics::map_attr(gdtf, "Zoom", fixture.optics.zoom, (beam_angle, beam_angle));
            // Shutter blade style — OUR editable model (GDTF lacks blade geometry).
            // Only shown for fixtures that actually have a shutter (or already set
            // one), so a plain PAR/wash isn't offered a blade it can't use.
            if (crate::optics::OpticField::Shutter.supported(gdtf)
                || fixture.shutter != crate::optics::ShutterKind::None)
                && fs.row_visible("Shutter blades")
            {
                ui.horizontal(|ui| {
                    ui.label("Shutter blades");
                    egui::ComboBox::from_id_salt("shutter-kind")
                        .selected_text(fixture.shutter.label())
                        .show_ui(ui, |ui| {
                            for k in crate::optics::ShutterKind::ALL {
                                ui.selectable_value(&mut fixture.shutter, k, k.label());
                            }
                        });
                });
            }
            let def = OpticalControls::default();
            // P2 #66: pre-evaluate the EditCondition for every optic field BEFORE
            // the mutable borrow of `fixture.optics`, so the live-edit loops can
            // read inertness without re-borrowing the fixture. `optic_inert` is the
            // single (unit-tested) source of the predicate.
            let inert_of: std::collections::HashMap<&'static str, bool> = OpticField::BEAM
                .iter()
                .chain(&OpticField::COLOR)
                .map(|&f| (f.label(), optic_inert(fixture, f)))
                .collect();
            let inert = |f: OpticField| inert_of.get(f.label()).copied().unwrap_or(false);
            let o = &mut fixture.optics;
            // Data-driven rows (gated by the fixture's GDTF attributes) so single
            // and bulk editing enumerate the SAME control set — see `OpticField`.
            // Simple/Advanced split (#8): the everyday controls show up front; the
            // power-user shaping (chromatic ab. / shutter / strobe / ±green tint)
            // tucks behind a per-section "Advanced" caret.

            if fs.category_visible(&BEAM_COMMON.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                ui.label(RichText::new("BEAM SHAPING").small().strong());
                Grid::new("optics-beam").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for f in BEAM_COMMON {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), (f == OpticField::Zoom).then(|| format!("{zoom_deg:.0}°")));
                        }
                    }
                });
            }
            advanced_section_filtered(ui, fs, "optics-beam", &BEAM_ADV.iter().map(|f| f.label()).collect::<Vec<_>>(), |ui| {
                Grid::new("optics-beam-adv").num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
                    for f in BEAM_ADV {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), None);
                        }
                    }
                });
            });

            if fs.category_visible(&COLOR_COMMON.iter().map(|f| f.label()).collect::<Vec<_>>()) {
                ui.add_space(4.0);
                ui.label(RichText::new("COLOR MIXING").small().strong());
                Grid::new("optics-color").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for f in COLOR_COMMON {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), None);
                        }
                    }
                });
            }
            advanced_section_filtered(ui, fs, "optics-color", &COLOR_ADV.iter().map(|f| f.label()).collect::<Vec<_>>(), |ui| {
                Grid::new("optics-color-adv").num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
                    for f in COLOR_ADV {
                        if fs.row_visible(f.label()) {
                            optic_field_row(ui, fs, o, &def, f, f.supported(gdtf), inert(f), None);
                        }
                    }
                });
            });

            // One block per wheel component, generated from the GDTF chain.
            let wheel_names: Vec<&str> =
                components.iter().map(|c| c.wheel.as_deref().unwrap_or(c.attribute.as_str())).collect();
            if !components.is_empty() && fs.category_visible(&wheel_names) {
                ui.add_space(4.0);
                ui.label(RichText::new("WHEELS").small().strong());
                Grid::new("optics-wheels").num_columns(2).spacing([10.0, 5.0]).striped(true).show(ui, |ui| {
                    for (i, comp) in components.iter().enumerate() {
                        let Some(w) = o.wheels.get_mut(i) else { continue };
                        let match_name = comp.wheel.as_deref().unwrap_or(comp.attribute.as_str());
                        if !fs.row_visible(match_name) {
                            continue;
                        }
                        let value_label = match comp.kind {
                            WheelKind::Gobo | WheelKind::Color => "select",
                            WheelKind::Prism | WheelKind::Animation | WheelKind::Frost => "insert",
                        };
                        let name = comp
                            .wheel
                            .as_deref()
                            .map(|n| format!("{} · {n}", comp.attribute))
                            .unwrap_or_else(|| comp.attribute.clone());
                        slider_field_row(
                            ui,
                            fs.panel_w,
                            |ui| {
                                ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                            },
                            |ui| {
                                ui.add(Slider::new(&mut w.value, 0.0..=1.0).max_decimals(2))
                                    .on_hover_text(value_label);
                            },
                        );
                        // Prism always exposes rotation (index + spin) even when the
                        // profile didn't flag a dedicated Pos/PosRotate function.
                        if comp.has_index || comp.kind == WheelKind::Prism {
                            slider_field_row(ui, fs.panel_w, |ui| { ui.label("index"); }, |ui| {
                                ui.add(Slider::new(&mut w.index, 0.0..=1.0).max_decimals(2));
                            });
                        }
                        if comp.has_spin || matches!(comp.kind, WheelKind::Color | WheelKind::Animation | WheelKind::Prism) {
                            slider_field_row(ui, fs.panel_w, |ui| { ui.label("spin"); }, |ui| {
                                ui.add(Slider::new(&mut w.spin, 0.0..=1.0).max_decimals(2))
                                    .on_hover_text("0.5 = stopped · below CCW · above CW");
                            });
                        }
                        if matches!(comp.kind, WheelKind::Gobo | WheelKind::Color) {
                            slider_field_row(ui, fs.panel_w, |ui| { ui.label("shake"); }, |ui| {
                                ui.add(Slider::new(&mut w.shake, 0.0..=1.0).max_decimals(2))
                                    .on_hover_text("Oscillate the indexed element");
                            });
                        }
                    }
                });
            }
        },
    );
}

pub(crate) fn load_gdtf_textures(ctx: &egui::Context, gdtf: &GdtfFixture) -> GdtfTextures {
    let thumbnail = gdtf
        .thumbnail
        .as_ref()
        .and_then(|b| decode_texture(ctx, "gdtf-thumb", b));
    let wheels = gdtf
        .wheels
        .iter()
        .map(|w| {
            w.slots
                .iter()
                .map(|s| {
                    s.media
                        .as_ref()
                        .and_then(|b| decode_texture(ctx, &s.name, b))
                })
                .collect()
        })
        .collect();
    GdtfTextures { thumbnail, wheels }
}

fn decode_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    // Downscale large wheel/thumbnail images for the panel.
    let img = img.thumbnail(256, 256);
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}


/// S3-properties (#6 reset-to-default, #7 multi-edit mixed detection): the pure
/// reductions + default resolution the inspector rows render off of.
#[cfg(test)]
mod property_tests {
    use super::*;

    #[test]
    fn common_f32_agrees_only_when_all_equal() {
        // All equal (within tolerance) → Some(value).
        assert_eq!(common_f32([1.0, 1.0, 1.0]), Some(1.0));
        assert_eq!(common_f32([0.5, 0.5 + 5e-5]), Some(0.5));
        // Any divergence → None ("mixed" placeholder).
        assert_eq!(common_f32([1.0, 0.0]), None);
        // Empty selection → None (no value to seed).
        assert_eq!(common_f32(std::iter::empty()), None);
        // Single value → that value.
        assert_eq!(common_f32([0.3]), Some(0.3));
    }

    #[test]
    fn common_rgb_per_channel() {
        assert_eq!(common_rgb([[1.0, 0.0, 0.0], [1.0, 0.0, 0.0]]), Some([1.0, 0.0, 0.0]));
        // One channel differs → mixed.
        assert_eq!(common_rgb([[1.0, 0.0, 0.0], [1.0, 0.0, 0.5]]), None);
    }

    #[test]
    fn non_gdtf_fixture_defaults_have_no_recoverable_template() {
        // A built-in fixture can't recover its profile beam-angle/colour from the
        // instance alone → those reset arrows stay hidden (None), but the level /
        // beam constants are always known.
        let f = &Scene::demo().fixtures[0];
        assert!(!f.is_gdtf());
        let d = FixtureDefaults::for_fixture(f);
        assert_eq!(d.beam_angle, None);
        assert_eq!(d.color, None);
        assert_eq!(d.dimmer, OpticalControls::default().dimmer);
        assert_eq!(d.beam, 1.0);
        assert_eq!(d.pan, 0.0);
        assert_eq!(d.tilt, 0.0);
    }

    // --- P2 #64: "show only modified" ---------------------------------------

    #[test]
    fn show_modified_hides_default_rows_keeps_changed() {
        let mut st = InspectorState::default();
        // Off: every row shows regardless of differs.
        assert!(st.row_shown("Dimmer", false));
        assert!(st.row_shown("Zoom", true));
        // On: only rows that differ from their default survive.
        st.show_modified = true;
        assert!(!st.row_shown("Dimmer", false)); // at default → hidden
        assert!(st.row_shown("Zoom", true)); // changed → shown
    }

    #[test]
    fn show_modified_composes_with_filter() {
        let mut st = InspectorState::default();
        st.show_modified = true;
        st.filter = "zoom".into();
        // Must pass BOTH gates: matches the filter AND differs from default.
        assert!(st.row_shown("Zoom", true)); // matches + modified
        assert!(!st.row_shown("Zoom", false)); // matches but at default
        assert!(!st.row_shown("Dimmer", true)); // modified but filtered out
    }

    // --- P2 #65: inspector PIN ----------------------------------------------

    #[test]
    fn pin_keeps_target_through_selection_change() {
        let mut scene = Scene::demo();
        let mut extra = scene.fixtures[0].clone();
        extra.id = 0; // zeroed so ensure_ids hands it a fresh, distinct id
        scene.fixtures.push(extra);
        scene.ensure_ids();
        let pinned_id = scene.fixtures[0].id;
        let other_id = scene.fixtures[1].id;
        assert_ne!(pinned_id, other_id);

        let mut st = InspectorState::default();
        // Pin fixture 0 while it's the single selection.
        st.pinned = current_pin_target(&scene, &Selection::fixture(0), &st);
        assert_eq!(st.pinned, Some(pinned_id));

        // Selection moves to fixture 1; the pin target still resolves to fixture 0
        // (current_pin_target returns the held id, ignoring the new selection).
        let sel2 = Selection::fixture(1);
        assert_eq!(current_pin_target(&scene, &sel2, &st), Some(pinned_id));
        // And it resolves to the original index regardless of what's selected.
        assert_eq!(scene.fixture_index_of(st.pinned.unwrap()), Some(0));
    }

    #[test]
    fn pin_target_requires_single_fixture() {
        let mut scene = Scene::demo();
        let mut extra = scene.fixtures[0].clone();
        extra.id = 0;
        scene.fixtures.push(extra);
        scene.ensure_ids();
        let st = InspectorState::default();
        // Nothing selected → no target.
        assert_eq!(current_pin_target(&scene, &Selection::default(), &st), None);
        // World / multi-fixture → no single-fixture target.
        let mut world = Selection::default();
        world.world = true;
        assert_eq!(current_pin_target(&scene, &world, &st), None);
        let multi = Selection { fixtures: vec![0, 1], ..Default::default() };
        assert_eq!(current_pin_target(&scene, &multi, &st), None);
        // A single fixture → its id.
        assert_eq!(current_pin_target(&scene, &Selection::fixture(0), &st), Some(scene.fixtures[0].id));
    }

    // --- P2 #66: EditCondition gray-out predicate ---------------------------

    #[test]
    fn optic_inert_when_dark_or_shutter_closed() {
        let mut f = Scene::demo().fixtures[0].clone();
        // Lit, open shutter → every control is live.
        f.optics.dimmer = 1.0;
        f.beam = 1.0;
        f.optics.shutter = 1.0;
        assert!(!optic_inert(&f, OpticField::Zoom));
        assert!(!optic_inert(&f, OpticField::Cyan));
        assert!(!optic_inert(&f, OpticField::Strobe));

        // Dimmer at 0: nothing in the beam → all controls inert.
        f.optics.dimmer = 0.0;
        assert!(optic_inert(&f, OpticField::Zoom));
        assert!(optic_inert(&f, OpticField::Cyan));
        f.optics.dimmer = 1.0;

        // Per-fixture beam (volumetric shaft) killed → the light still emits + shapes
        // surfaces, so optics stay LIVE (beam only scales the haze, not the fixture).
        f.beam = 0.0;
        assert!(!optic_inert(&f, OpticField::Zoom));
        f.beam = 1.0;

        // Lit but the shutter is fully closed → strobe (only) is inert.
        f.optics.shutter = 0.0;
        assert!(optic_inert(&f, OpticField::Strobe));
        assert!(!optic_inert(&f, OpticField::Zoom));
    }

    #[test]
    fn reset_differs_predicate_matches_default() {
        // The arrow-visibility predicate the rows use: shows iff the live value
        // left its default (tolerant equality avoids f32-dust false positives).
        let def = OpticalControls::default();
        let mut o = def.clone();
        assert!(!(!approx(OpticField::Zoom.get(&o), OpticField::Zoom.get(&def))));
        OpticField::Zoom.set(&mut o, def.zoom + 0.2);
        assert!(!approx(OpticField::Zoom.get(&o), OpticField::Zoom.get(&def)));
        // Reset writes the default back → predicate clears.
        OpticField::Zoom.set(&mut o, OpticField::Zoom.get(&def));
        assert!(!(!approx(OpticField::Zoom.get(&o), OpticField::Zoom.get(&def))));
    }

    // --- S1: Inspector filter predicate + collapse persistence -------------

    #[test]
    fn empty_filter_shows_every_row_and_category() {
        let st = InspectorState::default();
        // No query ⇒ everything visible (full layout restored).
        assert!(st.row_visible("Pan"));
        assert!(st.row_visible("Anything at all"));
        assert!(st.category_visible(&["Pan", "Tilt"]));
        // Even a category with no rows shows when unfiltered.
        assert!(st.category_visible(&[]));
    }

    #[test]
    fn filter_hides_non_matching_rows() {
        let mut st = InspectorState::default();
        st.filter = "dim".into();
        // Fuzzy/substring match on the label (case-insensitive).
        assert!(st.row_visible("Dimmer"));
        assert!(!st.row_visible("Pan"));
        assert!(!st.row_visible("Tilt"));
        // Whitespace-only filter is treated as no filter.
        st.filter = "   ".into();
        assert!(st.row_visible("Pan"));
    }

    #[test]
    fn filter_hides_category_with_no_matching_row() {
        let mut st = InspectorState::default();
        st.filter = "zoom".into();
        // Optics has Zoom → visible; Transform has none → hidden.
        assert!(st.category_visible(&["Zoom", "Focus", "Iris"]));
        assert!(!st.category_visible(&["Pan", "Tilt", "Position"]));
        // A category with no rows can never match under a filter.
        assert!(!st.category_visible(&[]));
    }

    #[test]
    fn filter_force_opens_visible_categories() {
        let mut st = InspectorState::default();
        // Off-filter a stored-collapsed category stays collapsed.
        st.collapsed.insert("Transform".into(), false);
        assert!(!st.open_state("Transform", true));
        // With a filter active, a visible category force-opens so matches show.
        st.filter = "pan".into();
        assert!(st.open_state("Transform", true));
    }

    #[test]
    fn collapse_state_round_trips_through_json() {
        // The remembered open/closed map survives a serialize → deserialize cycle
        // (the on-disk persistence format), while the live filter does NOT (it's
        // serde-skipped — a fresh session always starts unfiltered).
        let mut st = InspectorState::default();
        st.filter = "transient".into();
        st.collapsed.insert("Transform".into(), false);
        st.collapsed.insert("Optics".into(), true);

        let json = serde_json::to_string(&st).expect("serialize");
        let back: InspectorState = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.collapsed.get("Transform").copied(), Some(false));
        assert_eq!(back.collapsed.get("Optics").copied(), Some(true));
        assert_eq!(back.collapsed.len(), 2);
        // The filter is per-session: it round-trips to empty, not "transient".
        assert!(back.filter.is_empty());

        // And the restored map drives `open_state` exactly as before the round-trip.
        assert!(!back.open_state("Transform", true)); // stored false wins over default
        assert!(back.open_state("Optics", false)); // stored true wins over default
        assert!(back.open_state("Wheels", true)); // unknown → falls back to default
    }
}

