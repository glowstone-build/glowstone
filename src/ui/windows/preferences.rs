//! App-wide preferences: the [`Preferences`] config model (+ the viewport
//! [`LabelMode`]) and the modeless **Preferences** window. Applied live each
//! frame; not yet persisted to disk.

use egui::{Grid, RichText, Slider};

use crate::dmx::DmxConfig;
use crate::scene::{RenderSettings, ViewportMode};
use crate::ui::shortcuts::{self, KeymapOverrides};
use crate::ui::theme;

/// Transient state for the Keymap editor's "press a key to rebind" capture. Held
/// across frames by the owning [`Ui`] (it persists while the window waits for the
/// user to press a chord). `capturing` is the command id whose row is mid-capture,
/// or `None` when idle. `search` filters the command list.
#[derive(Default)]
pub struct KeymapEditorState {
    /// The command id currently capturing a new chord (Esc cancels), or `None`.
    pub capturing: Option<String>,
    /// Free-text filter over command labels / ids / categories.
    pub search: String,
}

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
    /// Quiet Blender-style scene STATS corner overlay (fixtures / objects /
    /// screens / environments + selected count). Off by default — it's an opt-in,
    /// unobtrusive readout (Blender's "Statistics" overlay also ships off).
    #[serde(default)]
    pub show_stats: bool,
    /// The navigation axis gizmo + transform gizmo "Gizmos" overlay toggle. On by
    /// default (matches the existing always-drawn behaviour).
    #[serde(default = "default_true")]
    pub show_gizmos: bool,
    /// The modal-transform hint line (the G/R/S key-cluster pill). On by default.
    #[serde(default = "default_true")]
    pub show_hint: bool,
}

fn default_true() -> bool {
    true
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
            show_stats: false,
            show_gizmos: true,
            show_hint: true,
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

/// The Preferences window. Returns nothing; mutates `prefs`/`settings`/`config`
/// and the keymap `overrides` (persisted to `keymap.json` on each edit).
pub fn preferences_window(
    ctx: &egui::Context,
    open: &mut bool,
    prefs: &mut Preferences,
    settings: &mut RenderSettings,
    config: &mut DmxConfig,
    overrides: &mut KeymapOverrides,
    keymap_state: &mut KeymapEditorState,
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
                            // One continuous density control: drives the egui
                            // zoom (fonts + every pixel literal) in theme::apply,
                            // which snaps to whole device pixels for crisp 1px
                            // hairlines. Shown as a percentage of the 1.0 base.
                            ui.add(
                                Slider::new(&mut prefs.ui_scale, 0.7..=2.0)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                    .custom_parser(|s| {
                                        s.trim().trim_end_matches('%').parse::<f64>().ok().map(|v| v / 100.0)
                                    }),
                            );
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
                    ui.add_space(2.0);
                    ui.checkbox(&mut prefs.show_stats, "Scene statistics");
                    ui.checkbox(&mut settings.show_grid, "Grid + world axes");
                    ui.checkbox(&mut prefs.show_gizmos, "Navigation gizmo");
                    ui.checkbox(&mut prefs.show_hint, "Transform hint line");
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

                            ui.label("Chroma in haze");
                            ui.add(Slider::new(&mut settings.chroma_haze, 0.0..=3.0));
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
                    hint(ui, "Chroma in haze lifts saturated beam colours (blue/red) so they read in fog; 0 = off.");
                    hint(ui, "Hybrid froxel: a compute fog grid (no banding, every beam casts shadows, cost decoupled from beam count) carries the wide beams; the sharp moving-head shafts stay crisp. Off = pure raymarch.");
                });

            egui::CollapsingHeader::new(format!("{}  {}", theme::icon::CONNECT, "DMX / Network"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.checkbox(&mut config.artnet, "Art-Net");
                    ui.checkbox(&mut config.sacn, "sACN");
                    hint(ui, "Start receiving in the Connectivity panel.");
                });

            egui::CollapsingHeader::new(format!("{}  {}", theme::icon::KEYBOARD, "Keymap"))
                // Collapsed by default; the headless PREVIZ_UI_PREFS debug run opens
                // it so the screenshot captures the editor content.
                .default_open(std::env::var_os("PREVIZ_UI_PREFS").is_some())
                .show(ui, |ui| {
                    keymap_editor(ui, overrides, keymap_state);
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

/// The searchable, category-grouped keymap editor (S2). One row per command in the
/// unified registry, showing its EFFECTIVE shortcut; per row a rebind affordance
/// (captures the next chord, Esc cancels), a reset-to-default and a disable toggle.
/// Conflicting rows (two enabled commands sharing a trigger in one context) are
/// drawn in the conflict colour with a tooltip. Every edit mutates `overrides` and
/// persists to `keymap.json` immediately (the edit helpers call `save()`).
fn keymap_editor(ui: &mut egui::Ui, overrides: &mut KeymapOverrides, state: &mut KeymapEditorState) {
    let pal = theme::Palette::get(ui);
    let ink = theme::ink(!ui.visuals().dark_mode);

    // --- Capture: while a row is mid-rebind, swallow the next chord. ----------
    if let Some(cmd_id) = state.capturing.clone() {
        // Esc cancels (capture_trigger returns None on Esc); a real chord rebinds.
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if esc {
            state.capturing = None;
        } else if let Some(trigger) = shortcuts::capture_trigger(ui.ctx()) {
            // Rebinding a command that HAS a static default updates `rebind`; a
            // catalog-/menu-only command (no default) gets an `added` bind instead so
            // it actually fires. The rebind helper also clears a stale disable (rebind
            // wins), so the captured key takes effect immediately + persists.
            if shortcuts::keymap_of(&cmd_id).is_some() {
                overrides.disabled.remove(&cmd_id);
                overrides.rebind(&cmd_id, &trigger);
            } else {
                // Drop any prior added bind for this id, then add the captured one in
                // the Global context (the only sensible home for a non-bound command).
                overrides.added.retain(|a| a.cmd != cmd_id);
                overrides.added.push(shortcuts::AddedBind {
                    keymap: shortcuts::SerKeymapId::from_id(shortcuts::KeymapId::Global),
                    trigger: shortcuts::SerTrigger::from_trigger(&trigger),
                    cmd: cmd_id.clone(),
                });
                overrides.save();
            }
            state.capturing = None;
        }
        // Keep repainting so the capture resolves the instant a key lands.
        ui.ctx().request_repaint();
    }

    // --- Header: search + "Reset all". ---------------------------------------
    ui.horizontal(|ui| {
        ui.label(theme::icon::SEARCH);
        ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .hint_text("Filter commands…")
                .desired_width(200.0),
        );
        if !state.search.is_empty() && ui.button(theme::icon::CLOSE).clicked() {
            state.search.clear();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled_ui(!overrides.is_empty(), |ui| {
                if ui
                    .button(format!("{}  {}", theme::icon::RESET, "Reset all"))
                    .on_hover_text("Clear every custom keybinding and restore the shipped defaults")
                    .clicked()
                {
                    overrides.reset_all();
                    state.capturing = None;
                }
            });
        });
    });

    let conflicting = shortcuts::conflicting_ids(overrides);
    let needle = state.search.to_lowercase();
    let matches = |c: &shortcuts::Command| {
        if needle.is_empty() {
            return true;
        }
        c.label.to_lowercase().contains(&needle)
            || c.id.to_lowercase().contains(&needle)
            || c.category.title().to_lowercase().contains(&needle)
    };

    ui.add_space(2.0);
    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
        for cat in shortcuts::Category::ORDER {
            let rows: Vec<&shortcuts::Command> = shortcuts::COMMANDS
                .iter()
                .filter(|c| c.category == cat && matches(c))
                .collect();
            if rows.is_empty() {
                continue;
            }
            ui.add_space(4.0);
            ui.label(RichText::new(cat.title()).small().strong().color(ink.muted));
            Grid::new(("keymap-grid", cat.title()))
                .num_columns(3)
                .spacing([10.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    for cmd in rows {
                        keymap_row(ui, overrides, state, cmd, &conflicting, &pal);
                        ui.end_row();
                    }
                });
        }
    });
}

/// One command row: label · effective-shortcut chip (rebind on click) · reset +
/// disable. Conflicting rows draw their chip in the conflict colour with a tooltip.
fn keymap_row(
    ui: &mut egui::Ui,
    overrides: &mut KeymapOverrides,
    state: &mut KeymapEditorState,
    cmd: &shortcuts::Command,
    conflicting: &std::collections::BTreeSet<String>,
    pal: &theme::Palette,
) {
    let id = cmd.id;
    let disabled = overrides.disabled.contains(id) && !overrides.rebind.contains_key(id);
    let capturing = state.capturing.as_deref() == Some(id);
    let in_conflict = conflicting.contains(id);
    let overridden = overrides.rebind.contains_key(id)
        || overrides.disabled.contains(id)
        || overrides.added.iter().any(|a| a.cmd == id);

    // Col 1 — label (a tiny dot marks a customised row).
    ui.horizontal(|ui| {
        let mut label = RichText::new(cmd.label);
        if disabled {
            label = label.color(pal.ink_muted).strikethrough();
        }
        ui.label(label).on_hover_text(id);
        if overridden {
            ui.label(RichText::new("•").small().color(pal.accent))
                .on_hover_text("Customised (differs from the shipped default)");
        }
    });

    // Col 2 — the effective shortcut, click to rebind (Esc cancels mid-capture).
    let chip = if capturing {
        "Press a key…  (Esc cancels)".to_string()
    } else {
        shortcuts::shortcut_for(id, overrides).unwrap_or_else(|| "—".into())
    };
    let mut chip_text = RichText::new(chip).monospace();
    if capturing {
        chip_text = chip_text.color(pal.accent);
    } else if in_conflict {
        chip_text = chip_text.color(pal.conflict);
    } else if disabled {
        chip_text = chip_text.color(pal.ink_muted);
    }
    let resp = ui.add(egui::Button::new(chip_text).min_size(egui::vec2(150.0, 0.0)));
    let resp = if in_conflict {
        resp.on_hover_text("Conflict: another command in this context shares this shortcut")
    } else {
        resp.on_hover_text("Click, then press a key chord to rebind")
    };
    if resp.clicked() {
        state.capturing = if capturing { None } else { Some(id.to_string()) };
    }

    // Col 3 — reset + disable.
    ui.horizontal(|ui| {
        ui.add_enabled_ui(overridden, |ui| {
            if ui
                .button(theme::icon::RESET)
                .on_hover_text("Reset to default")
                .clicked()
            {
                overrides.reset(id);
                if capturing {
                    state.capturing = None;
                }
            }
        });
        // Disable only applies to commands that HAVE a default bind (nothing to
        // suppress otherwise). A rebound command can't also be disabled (rebind
        // wins), so the toggle disables the EFFECTIVE bind by clearing the rebind.
        let has_default = shortcuts::keymap_of(id).is_some();
        ui.add_enabled_ui(has_default, |ui| {
            let mut off = disabled;
            if ui
                .toggle_value(&mut off, theme::icon::EYE_OFF)
                .on_hover_text(if disabled { "Disabled — click to re-enable" } else { "Disable this shortcut" })
                .clicked()
            {
                if off {
                    // Disabling: drop any rebind so the disable actually takes (a
                    // rebind would otherwise win), then suppress the default.
                    overrides.rebind.remove(id);
                    overrides.set_disabled(id, true);
                } else {
                    overrides.set_disabled(id, false);
                }
                if capturing {
                    state.capturing = None;
                }
            }
        });
    });
}
