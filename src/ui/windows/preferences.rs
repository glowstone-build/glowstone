//! App-wide preferences: the [`Preferences`] config model (+ the viewport
//! [`LabelMode`]) and the modeless **Preferences** window. Applied live each
//! frame; not yet persisted to disk.

use egui::{Grid, RichText, Slider};

use crate::dmx::DmxConfig;
use crate::scene::{RenderSettings, ViewportMode};
use crate::ui::theme;

/// What a fixture label shows in the viewport overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum LabelMode {
    Name,
    FixtureId,
    Address,
}

impl LabelMode {
    pub const ALL: [LabelMode; 3] = [Self::Name, Self::FixtureId, Self::Address];
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::FixtureId => "Fixture ID",
            Self::Address => "DMX address",
        }
    }
}

/// App-wide user preferences (theme, units, viewport overlays, DMX defaults).
/// Applied live each frame; not yet persisted to disk.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Preferences {
    /// Display lengths in feet (else metres). Drives DragValue suffixes + status.
    pub units_feet: bool,
    pub theme_light: bool,
    pub accent: [f32; 3],
    pub ui_scale: f32,
    /// Viewport overlays.
    pub show_labels: bool,
    pub labels_selected_only: bool,
    pub label_mode: LabelMode,
    pub show_fps: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            units_feet: false,
            theme_light: false,
            accent: [0.36, 0.66, 1.0],
            ui_scale: 1.0,
            show_labels: true,
            labels_selected_only: false,
            label_mode: LabelMode::Name,
            show_fps: true,
        }
    }
}

impl Preferences {
    /// Metres → display value + unit suffix per the current units setting.
    pub fn len(&self, metres: f32) -> (f32, &'static str) {
        if self.units_feet {
            (metres * 3.280_84, " ft")
        } else {
            (metres, " m")
        }
    }

    /// Apply the full app theme (visuals + spacing + type scale + DPI) for this
    /// frame. Delegates to [`theme::apply`] — the single source of tokens.
    pub fn apply_theme(&self, ctx: &egui::Context) {
        theme::apply(ctx, self);
    }
}

/// The Preferences window. Returns nothing; mutates `prefs`/`settings`/`config`.
pub fn preferences_window(
    ctx: &egui::Context,
    open: &mut bool,
    prefs: &mut Preferences,
    settings: &mut RenderSettings,
    config: &mut DmxConfig,
) {
    let mut keep = *open;
    egui::Window::new("Preferences")
        .open(&mut keep)
        .resizable(true)
        .default_width(440.0)
        .show(ctx, |ui| {
            let ink = theme::ink(!ui.visuals().dark_mode);
            // A faint one-line hint, ink-aware (readable in both themes).
            let hint = |ui: &mut egui::Ui, text: &str| {
                ui.label(RichText::new(text).small().color(ink.muted));
            };

            egui::CollapsingHeader::new(format!("{}  {}", theme::icon::SETTINGS, "General"))
                .default_open(true)
                .show(ui, |ui| {
                    Grid::new("prefs-general")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Units");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut prefs.units_feet, false, "Metres");
                                ui.selectable_value(&mut prefs.units_feet, true, "Feet");
                            });
                            ui.end_row();
                        });
                });

            egui::CollapsingHeader::new(format!("{}  {}", theme::icon::COLOR, "Theme & Display"))
                .default_open(true)
                .show(ui, |ui| {
                    Grid::new("prefs-theme")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Theme");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut prefs.theme_light, false, "Dark");
                                ui.selectable_value(&mut prefs.theme_light, true, "Light");
                            });
                            ui.end_row();

                            ui.label("Accent");
                            ui.color_edit_button_rgb(&mut prefs.accent);
                            ui.end_row();

                            ui.label("UI scale");
                            ui.add(Slider::new(&mut prefs.ui_scale, 0.7..=2.0));
                            ui.end_row();
                        });
                    hint(ui, "Scales the entire interface — fonts, controls and spacing.");
                });

            egui::CollapsingHeader::new(format!("{}  {}", theme::icon::EYE, "Viewport"))
                .default_open(true)
                .show(ui, |ui| {
                    ui.checkbox(&mut prefs.show_labels, "Fixture labels");
                    ui.add_enabled_ui(prefs.show_labels, |ui| {
                        ui.indent("labels-opts", |ui| {
                            ui.checkbox(&mut prefs.labels_selected_only, "Selected only");
                            ui.horizontal(|ui| {
                                ui.label("Show");
                                for m in LabelMode::ALL {
                                    ui.selectable_value(&mut prefs.label_mode, m, m.label());
                                }
                            });
                        });
                    });
                    ui.add_space(2.0);
                    ui.checkbox(&mut prefs.show_fps, "FPS overlay");
                });

            egui::CollapsingHeader::new(format!("{}  {}", theme::icon::INSPECTOR, "Rendering"))
                .default_open(false)
                .show(ui, |ui| {
                    Grid::new("prefs-render")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Exposure");
                            ui.add(Slider::new(&mut settings.exposure, 0.05..=8.0));
                            ui.end_row();

                            ui.label("Bloom");
                            ui.add(Slider::new(&mut settings.bloom, 0.0..=3.0));
                            ui.end_row();

                            ui.label("Beam intensity");
                            ui.add(Slider::new(&mut settings.beam_intensity, 0.0..=4000.0));
                            ui.end_row();

                            ui.label("Volumetric steps");
                            ui.add(Slider::new(&mut settings.steps, 8..=192));
                            ui.end_row();

                            ui.label("Gobo sharpen");
                            ui.add(Slider::new(&mut settings.gobo_sharpness, 0.0..=2.0));
                            ui.end_row();

                            ui.label("Display mode");
                            ui.horizontal(|ui| {
                                for m in ViewportMode::ALL {
                                    ui.selectable_value(&mut settings.mode, m, m.label());
                                }
                            });
                            ui.end_row();
                        });
                    hint(ui, "Higher volumetric step counts sharpen fog at a frame-rate cost.");
                });

            egui::CollapsingHeader::new(format!("{}  {}", theme::icon::CONNECT, "DMX / Network"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.checkbox(&mut config.artnet, "Art-Net");
                    ui.checkbox(&mut config.sacn, "sACN");
                    hint(ui, "Start receiving in the Connectivity panel.");
                });

            ui.add_space(4.0);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(format!("{}  {}", theme::icon::RESET, "Reset to defaults"))
                    .clicked()
                {
                    *prefs = Preferences::default();
                }
            });
        });
    *open = keep;
}
