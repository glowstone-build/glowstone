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

use egui::{Color32, DragValue, Grid, RichText, Sense};
use glam::{Mat4, Vec3};

mod bulk;
mod environment;
mod fixture;
mod optics;
mod props;
mod pyro;
mod screen;
mod world;
pub use props::Inspect;

use super::panels::InspectorEdit;
use super::render_panel::RenderUiState;
use super::theme;
use super::windows::ProfileEditor;
use super::{EmitterPreviewCell, GdtfTextures, ScreenSources};
use crate::dmx::PatchTable;
use crate::gdtf::GdtfFixture;
use crate::optics::OpticalControls;
use crate::scene::environment::Environment;
use crate::scene::{Fixture, ObjectRef, Scene, Selection};

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
    let d = directories::ProjectDirs::from("build", "glowstone", "glowstone")?;
    let dir = d.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("inspector.json"))
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

#[derive(Clone, Copy)]
struct ObjectTransformValues {
    position: Vec3,
    rotation_deg: Vec3,
    uniform_scale: Option<f32>,
}

fn object_transform_values(scene: &Scene, obj: ObjectRef) -> Option<ObjectTransformValues> {
    let props = scene.object_transform_props(obj)?;
    let (ry, rx, rz) = props.rotation.to_euler(glam::EulerRot::YXZ);
    Some(ObjectTransformValues {
        position: props.position,
        rotation_deg: Vec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees()),
        uniform_scale: props.uniform_scale,
    })
}

fn common_transform_axis(
    scene: &Scene,
    refs: &[ObjectRef],
    value: impl Fn(ObjectTransformValues) -> f32 + Copy,
) -> Option<f32> {
    common_f32(refs.iter().filter_map(|&obj| object_transform_values(scene, obj).map(value)))
}

fn any_rotation_modified(scene: &Scene, refs: &[ObjectRef]) -> bool {
    refs.iter().filter_map(|&obj| object_transform_values(scene, obj)).any(|v| {
        !approx(v.rotation_deg.x, 0.0) || !approx(v.rotation_deg.y, 0.0) || !approx(v.rotation_deg.z, 0.0)
    })
}

pub(super) fn object_transform_bulk(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    refs: &[ObjectRef],
    state: &mut InspectorState,
) {
    let refs: Vec<ObjectRef> = refs
        .iter()
        .copied()
        .filter(|&obj| scene.object_transform_props(obj).is_some())
        .collect();
    if refs.is_empty() {
        return;
    }
    let Some(primary) = object_transform_values(scene, refs[0]) else { return };
    let scale_common = common_f32(
        refs.iter()
            .filter_map(|&obj| object_transform_values(scene, obj).and_then(|v| v.uniform_scale)),
    );
    let scale_seed = primary.uniform_scale;
    let scale_supported = scale_seed.is_some()
        && refs
            .iter()
            .all(|&obj| object_transform_values(scene, obj).is_some_and(|v| v.uniform_scale.is_some()));
    let row_labels: &[&str] = if scale_supported {
        &["Position", "Rotation", "Scale"]
    } else {
        &["Position", "Rotation"]
    };

    category(
        ui,
        state,
        "Transform",
        format!("{}  Transform", theme::icon::INSPECTOR),
        true,
        row_labels,
        |ui, fs| {
            Grid::new("bulk-object-transform")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .striped(true)
                .show(ui, |ui| {
                    let position_common = [
                        common_transform_axis(scene, &refs, |v| v.position.x),
                        common_transform_axis(scene, &refs, |v| v.position.y),
                        common_transform_axis(scene, &refs, |v| v.position.z),
                    ];
                    bulk_transform_vec3_rows(
                        ui,
                        fs,
                        "Position",
                        primary.position,
                        position_common,
                        0.05,
                        "",
                        false,
                        scene,
                        &refs,
                        |scene, obj, axis, value| scene.set_object_position_axis(obj, axis, value),
                    );

                    let rotation_common = [
                        common_transform_axis(scene, &refs, |v| v.rotation_deg.x),
                        common_transform_axis(scene, &refs, |v| v.rotation_deg.y),
                        common_transform_axis(scene, &refs, |v| v.rotation_deg.z),
                    ];
                    bulk_transform_vec3_rows(
                        ui,
                        fs,
                        "Rotation",
                        primary.rotation_deg,
                        rotation_common,
                        0.5,
                        "°",
                        any_rotation_modified(scene, &refs),
                        scene,
                        &refs,
                        |scene, obj, axis, value| scene.set_object_rotation_axis_deg(obj, axis, value),
                    );

                    if scale_supported {
                        bulk_transform_scale_row(
                            ui,
                            fs,
                            scene,
                            &refs,
                            scale_common,
                            scale_seed.unwrap_or(1.0),
                        );
                    }
                });
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn bulk_transform_vec3_rows(
    ui: &mut egui::Ui,
    state: &InspectorState,
    label: &str,
    seed: Vec3,
    common: [Option<f32>; 3],
    speed: f64,
    suffix: &'static str,
    reset_to_zero: bool,
    scene: &mut Scene,
    refs: &[ObjectRef],
    mut write_axis: impl FnMut(&mut Scene, ObjectRef, usize, f32),
) {
    if !state.row_shown(label, true) {
        return;
    }
    let axes = [("X", seed.x), ("Y", seed.y), ("Z", seed.z)];
    let mut reset = false;
    for (axis, (prefix, seed_value)) in axes.into_iter().enumerate() {
        let mixed = common[axis].is_none();
        let mut value = common[axis].unwrap_or(seed_value);
        field_row(
            ui,
            state.panel_w,
            |ui| {
                if axis == 0 {
                    reset = reset_arrow(ui, reset_to_zero);
                    ui.add(egui::Label::new(label).truncate());
                }
            },
            |ui| {
                ui.horizontal(|ui| {
                    let mut drag = DragValue::new(&mut value).speed(speed).prefix(format!("{prefix} "));
                    if !suffix.is_empty() {
                        drag = drag.suffix(suffix);
                    }
                    if ui.add(drag).changed() {
                        for &obj in refs {
                            write_axis(scene, obj, axis, value);
                        }
                    }
                    if mixed {
                        ui.label(RichText::new("mixed").weak().small());
                    }
                });
            },
        );
    }
    if reset {
        for &obj in refs {
            for axis in 0..3 {
                write_axis(scene, obj, axis, 0.0);
            }
        }
    }
}

fn bulk_transform_scale_row(
    ui: &mut egui::Ui,
    state: &InspectorState,
    scene: &mut Scene,
    refs: &[ObjectRef],
    common: Option<f32>,
    seed: f32,
) {
    let differs = refs
        .iter()
        .filter_map(|&obj| object_transform_values(scene, obj).and_then(|v| v.uniform_scale))
        .any(|v| !approx(v, 1.0));
    if !state.row_shown("Scale", differs) {
        return;
    }
    let mixed = common.is_none();
    let mut value = common.unwrap_or(seed).max(0.001);
    let mut reset = false;
    field_row(
        ui,
        state.panel_w,
        |ui| {
            reset = reset_arrow(ui, differs);
            ui.add(egui::Label::new("Scale").truncate());
        },
        |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add(DragValue::new(&mut value).speed(0.005).range(0.001..=1000.0))
                    .changed()
                {
                    for &obj in refs {
                        scene.set_object_uniform_scale(obj, value);
                    }
                }
                if mixed {
                    ui.label(RichText::new("mixed").weak().small());
                }
            });
        },
    );
    if reset {
        for &obj in refs {
            scene.set_object_uniform_scale(obj, 1.0);
        }
    }
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
                gdtf_inspector(ui, fixture, patch, gdtf_textures, idx, profile, state);
            } else {
                fixture_inspector(ui, fixture, state);
            }
            return;
        }
    }

    // World is the top of the hierarchy: the render properties (resolution /
    // sampling / output) + the shared look/world controls live here.
    if selection.world {
        world::render_properties(ui, scene, settings, state, render_ui);
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
        if geo.len() > 1 {
            let refs = geo.iter().map(|&i| ObjectRef::Geometry(i)).collect::<Vec<_>>();
            object_transform_bulk(ui, scene, &refs, state);
        }
        geometry_inspector(ui, scene, &geo, state, geo.len() == 1);
        return;
    }

    // LED screens take the Inspector when selected.
    let scr: Vec<usize> = selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
    if let Some(&primary) = scr.first() {
        if scr.len() > 1 {
            let refs = scr.iter().map(|&i| ObjectRef::Screen(i)).collect::<Vec<_>>();
            object_transform_bulk(ui, scene, &refs, state);
        }
        screen::led_screen_inspector(ui, &mut scene.screens[primary], scr.len(), sources, state, scr.len() == 1);
        return;
    }

    // Pyro devices (CO2 cannons + cold-spark machines) take the Inspector.
    let pyro: Vec<usize> = selection.pyro.iter().copied().filter(|&i| i < scene.pyro.len()).collect();
    if let Some(&primary) = pyro.first() {
        if pyro.len() > 1 {
            let refs = pyro.iter().map(|&i| ObjectRef::Pyro(i)).collect::<Vec<_>>();
            object_transform_bulk(ui, scene, &refs, state);
            // Bulk: edit the active device, then propagate every effect/look field the
            // user just changed to the rest of the selection (snapshot → inspect → diff
            // → apply). Identity and the DMX patch stay per-device.
            let before = scene.pyro[primary].clone();
            pyro_inspector(ui, &mut scene.pyro[primary], pyro.len(), state, false);
            let after = scene.pyro[primary].clone();
            for &i in pyro.iter().skip(1) {
                apply_pyro_bulk_delta(&before, &after, &mut scene.pyro[i]);
            }
        } else {
            pyro_inspector(ui, &mut scene.pyro[primary], pyro.len(), state, true);
        }
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
                gdtf_inspector(ui, fixture, patch, gdtf_textures, id, profile, state);
            } else {
                fixture_inspector(ui, fixture, state);
            }
        }
        many => bulk::bulk_inspector(ui, scene, patch, many, state),
    }
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

fn meta_label(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) {
    ui.label(RichText::new(text).size(12.0).color(color));
}

fn meta_value(ui: &mut egui::Ui, text: impl Into<String>, color: Color32, strong: bool) {
    let mut text = RichText::new(text).size(12.0).color(color);
    if strong {
        text = text.strong();
    }
    ui.label(text);
}

fn meta_sep(ui: &mut egui::Ui, color: Color32) {
    ui.label(RichText::new("·").size(12.0).color(color));
}

fn centered_thumbnail(ui: &mut egui::Ui, thumb: &egui::TextureHandle) {
    let source = thumb.size_vec2();
    if source.x <= 0.0 || source.y <= 0.0 {
        return;
    }
    let max_w = ui.available_width().min(230.0);
    let max_h = 220.0;
    let scale = (max_w / source.x).min(max_h / source.y).max(0.1);
    let size = source * scale;
    ui.add_space(6.0);
    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
        ui.image((thumb.id(), size));
    });
    ui.add_space(4.0);
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
fn geometry_inspector(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    ids: &[usize],
    state: &mut InspectorState,
    show_transform: bool,
) {
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
        ui.label(
            RichText::new(format!("{} objects — transform applies to all; other edits affect the active object", ids.len()))
                .weak()
                .small(),
        );
    }
    ui.separator();

    ui.horizontal(|ui| {
        let mut visible = !g.hidden;
        if ui.checkbox(&mut visible, "Visible").changed() {
            g.hidden = !visible;
        }
    });

    if show_transform {
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
}

/// Inspector for a selected stage **pyro device** (CO2 cannon or cold-spark
/// machine): the heading/Visible chrome + the decomposed transform here, then the
/// kind-aware Effect/Movement/Quality/DMX grid via `impl Inspect for PyroDevice`.
/// Mirrors [`geometry_inspector`] (the device stores a `Mat4`, decomposed/recomposed
/// on edit) — when patched + receiving live DMX the Effect values are console-driven,
/// otherwise they are the free-run preview values.
fn pyro_inspector(
    ui: &mut egui::Ui,
    d: &mut crate::scene::PyroDevice,
    count: usize,
    state: &mut InspectorState,
    show_transform: bool,
) {
    ui.heading(d.name.as_str());
    ui.label(RichText::new(format!("{} · {}", d.kind.label(), d.profile_name)).weak().small());
    if count > 1 {
        ui.label(
            RichText::new(format!("{count} devices — edits apply to all selected"))
                .weak()
                .small(),
        );
    }
    ui.separator();

    ui.horizontal(|ui| {
        let mut visible = !d.hidden;
        if ui.checkbox(&mut visible, "Visible").changed() {
            d.hidden = !visible;
        }
    });

    // Transform (position + rotation; nozzle at origin, fires +Y). Position is
    // lossless (translation column only); Rotation recomposes to a clean basis only
    // when the user edits it (no scale — the device transform is rigid).
    let mut pos = d.transform.w_axis.truncate();
    let mut rot = glam::Quat::from_mat4(&d.transform);
    let mut pos_changed = false;
    let mut rot_changed = false;
    props::with(ui, state, |p| {
        if show_transform {
            p.group("Transform", theme::icon::INSPECTOR, true, |p| {
                pos_changed |= p.vec3("Position", &mut pos).speed(0.05).show();
                rot_changed |= p.rotation("Rotation", &mut rot);
            });
        }
        d.inspect(p);
    });
    if rot_changed {
        d.transform = Mat4::from_rotation_translation(rot, pos);
    } else if pos_changed {
        d.transform.w_axis = pos.extend(1.0);
    }
}

/// Bulk-edit propagation for pyro: copy to `target` every effect/look/quality field the
/// user just changed on the active device (`before` → `after`). Only fields that ACTUALLY
/// changed are copied, so editing one knob in a multi-selection nudges only that knob on
/// the rest. Transform is handled by [`object_transform_bulk`]; per-device identity,
/// kind, and the inline DMX patch stay local to each device.
#[allow(clippy::float_cmp)] // exact change-detection — propagate only what the user touched
fn apply_pyro_bulk_delta(
    before: &crate::scene::PyroDevice,
    after: &crate::scene::PyroDevice,
    target: &mut crate::scene::PyroDevice,
) {
    macro_rules! prop {
        ($($f:ident),+ $(,)?) => { $(
            if before.$f != after.$f {
                target.$f = after.$f;
            }
        )+ };
    }
    prop!(
        height_m, throw_m, speed, density, cone_deg, color_t0_k, color_t1_k, brightness,
        opacity, tint, spin_rpm, pan, tilt, max_particles, quality, dissipation,
        viewport_hq, thickness, mode, hidden,
    );
}

/// Inspector for an imported GDTF fixture: identity + thumbnail, editable
/// instance params, wheels (with slot images), and the DMX modes/channels.
fn gdtf_inspector(
    ui: &mut egui::Ui,
    fixture: &mut Fixture,
    patch: &PatchTable,
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

    let pal = theme::Palette::get(ui);
    let ink = pal.ink;
    let secondary = pal.ink_secondary;
    let tertiary = pal.ink_tertiary;
    let muted = pal.ink_muted;
    let patch_entry = patch.get(fixture_id);
    let mode_index = patch_entry.map(|p| p.mode_index).unwrap_or(fixture.mode_index);
    let mode_name = gdtf.modes.get(mode_index).map(|m| m.name.as_str()).unwrap_or("unknown mode");
    let patch_conflict = patch
        .conflicts()
        .iter()
        .any(|c| c.a == fixture_id || c.b == fixture_id);

    ui.horizontal(|ui| {
        ui.heading(RichText::new(gdtf.name.as_str()).color(ink));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(format!("{}  Profile…", theme::icon::PROFILE))
                .on_hover_text("Open the full fixture profile editor")
                .clicked()
            {
                *profile = Some(ProfileEditor::new(fixture_id));
            }
        });
    });
    let [sr, sg, sb] = fixture.source.color_rgb();
    ui.horizontal_wrapped(|ui| {
        meta_value(ui, gdtf.manufacturer.as_str(), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, gdtf.long_name.as_str(), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, fixture.source.label(), Color32::from_rgb(sr, sg, sb), false);
    });

    ui.horizontal_wrapped(|ui| {
        meta_label(ui, "Patch", tertiary);
        match patch_entry {
            Some(p) if p.enabled => {
                let end = p.address.saturating_add(p.footprint.saturating_sub(1)).min(512);
                let patch_color = if patch_conflict { pal.conflict } else { pal.accent };
                meta_value(ui, format!("U{} {:03}-{:03}", p.universe, p.address, end), patch_color, true);
                meta_sep(ui, muted);
                meta_value(ui, format!("{} ch", p.footprint), secondary, false);
                meta_sep(ui, muted);
                meta_value(ui, p.source.label(), muted, false);
                if patch_conflict {
                    meta_sep(ui, muted);
                    meta_value(ui, "address conflict", pal.conflict, true);
                }
            }
            Some(p) => {
                meta_value(ui, format!("unpatched · {} ch reserved", p.footprint), pal.warn, true);
            }
            None => {
                meta_value(ui, "no patch entry", pal.warn, true);
            }
        }
        meta_sep(ui, muted);
        meta_label(ui, "DMX", tertiary);
        meta_value(ui, mode_name, secondary, false);
    });

    if let Some(thumb) = &tex.thumbnail {
        centered_thumbnail(ui, thumb);
    }

    // Physical source / beam spec from the GDTF Beam geometry.
    let b = &gdtf.beam;
    ui.horizontal_wrapped(|ui| {
        meta_value(ui, format!("{} engine", b.lamp_type), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, format!("{:.0} K", b.color_temp), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, format!("CRI {:.0}", b.cri), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, format!("{:.0} lm", b.luminous_flux), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, format!("{:.0} W", b.power), secondary, false);
    });
    ui.horizontal_wrapped(|ui| {
        meta_value(ui, b.beam_type.as_str(), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, format!("beam {:.0}° / field {:.0}°", b.beam_angle, b.field_angle), secondary, false);
        meta_sep(ui, muted);
        meta_value(ui, format!("throw {:.2}", b.throw_ratio), secondary, false);
    });
    // Multi-emitter summary: cell count + the live per-cell colors (driven by
    // per-pixel DMX; the Color picker below multiplies all of them manually).
    let emitters = fixture.emitters();
    if emitters.len() > 1 {
        let visible = emitters.iter().filter(|e| e.merged_into.is_none()).count();
        ui.horizontal_wrapped(|ui| {
            meta_value(ui, format!("{visible} emitters"), secondary, false);
            if emitters.len() > visible {
                meta_sep(ui, muted);
                let overlays = emitters.len() - visible;
                meta_value(ui, format!("{overlays} overlay{}", if overlays == 1 { "" } else { "s" }), muted, false);
            }
            meta_sep(ui, muted);
            meta_value(ui, emitters[0].beam.beam_type.as_str(), secondary, false);
            meta_sep(ui, muted);
            meta_value(ui, "per-cell DMX", pal.accent, true);
        });
        let emitter_mode_index = fixture.mode_index;
        let emitter_shapes = tex
            .emitter_shapes
            .entry(emitter_mode_index)
            .or_insert_with(|| build_emitter_preview_shapes(&gdtf, emitter_mode_index));
        emitter_layout_preview(ui, fixture, emitters, emitter_shapes);
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
        ui.horizontal_wrapped(|ui| {
            meta_label(ui, "MVR", tertiary);
            meta_value(ui, format!("ID {id}"), muted, false);
            meta_sep(ui, muted);
            meta_value(ui, format!("addr {addr}"), muted, false);
            meta_sep(ui, muted);
            meta_value(ui, mode, muted, false);
        })
        .response
        .on_hover_text("Fixture ID · DMX universe.channel · mode, from the imported MVR patch");
    }

    ui.separator();

    // Transform + Fixture grid: the SAME declarative `impl Inspect for Fixture` the
    // built-in inspector uses (it is GDTF-aware: ±135° tilt, white-master Color default,
    // no editable Beam angle, live commanded/now Pan-Tilt tips).
    props::show(ui, state, fixture);

    optics::optics_section(ui, fixture, &gdtf, state);

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

/// Draw the fixture's emitters at their TRUE 2D positions on the face. When the
/// renderer has a real beam-node lens mesh, the preview uses that mesh's 2D
/// silhouette; otherwise it falls back to the same disc / rounded-rect aperture
/// mask the renderer uses for synthetic lens billboards.
fn emitter_layout_preview(
    ui: &mut egui::Ui,
    fixture: &crate::scene::Fixture,
    emitters: &[crate::gdtf::EmitterDef],
    mesh_shapes: &[Option<EmitterPreviewCell>],
) {
    // Face bounds, skipping coaxial overlays.
    let mut lo = [f32::MAX, f32::MAX];
    let mut hi = [f32::MIN, f32::MIN];
    let mut any = false;
    for (i, e) in emitters.iter().enumerate().filter(|(_, e)| e.merged_into.is_none()) {
        any = true;
        if let Some(cell) = mesh_shapes.get(i).and_then(|s| s.as_ref()) {
            for &[x, y] in &cell.outline {
                lo[0] = lo[0].min(x);
                lo[1] = lo[1].min(y);
                hi[0] = hi[0].max(x);
                hi[1] = hi[1].max(y);
            }
        } else {
            lo[0] = lo[0].min(e.pos[0] - e.aperture.half_w);
            lo[1] = lo[1].min(e.pos[1] - e.aperture.half_h);
            hi[0] = hi[0].max(e.pos[0] + e.aperture.half_w);
            hi[1] = hi[1].max(e.pos[1] + e.aperture.half_h);
        }
    }
    if !any {
        return;
    }
    let span_x = (hi[0] - lo[0]).max(1e-3);
    let span_y = (hi[1] - lo[1]).max(1e-3);
    let outer_w = ui.available_width().max(80.0);
    let max_face_w = (outer_w - 24.0).max(40.0).min(220.0);
    let outer_h = (max_face_w * span_y / span_x + 18.0).clamp(76.0, 160.0);
    ui.add_space(2.0);
    let (canvas, _) = ui.allocate_exact_size(egui::vec2(outer_w, outer_h), Sense::hover());
    let painter = ui.painter_at(canvas);
    let pal = theme::Palette::get(ui);
    painter.rect_filled(canvas, 3.0, pal.input);
    painter.rect_stroke(canvas, 3.0, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
    let max_face_h = (canvas.height() - 16.0).max(40.0);
    let scale = (max_face_w / span_x).min(max_face_h / span_y);
    let face = egui::Rect::from_center_size(canvas.center(), egui::vec2(span_x * scale, span_y * scale));
    let level = (fixture.intensity * fixture.optics.dimmer).clamp(0.0, 1.0);
    // World (right, up) → canvas pixel. `up` is +y, screen y is down, so flip.
    let map = |x: f32, y: f32| -> egui::Pos2 {
        egui::pos2(
            face.left() + (x - lo[0]) * scale,
            face.top() + (hi[1] - y) * scale,
        )
    };
    let stroke = egui::Stroke::new(1.0, pal.border);
    let mut cells: Vec<(EmitterPreviewPaintShape, Color32)> = Vec::new();
    for (i, e) in emitters.iter().enumerate() {
        if e.merged_into.is_some() {
            continue;
        }
        let c = fixture.cells.get(i).copied().unwrap_or([1.0, 1.0, 1.0]);
        // Off → dark grey (shows the cell exists); fades to the emitted colour
        // with the master level so the preview tracks the live look.
        let lit = |ch: f32| -> u8 {
            let on = (ch.min(1.0) * level).powf(1.0 / 2.2);
            (egui::lerp(0.27_f32..=on.max(0.0), level) * 255.0).clamp(0.0, 255.0) as u8
        };
        let col = Color32::from_rgb(lit(c[0]), lit(c[1]), lit(c[2]));
        if let Some(cell) = mesh_shapes.get(i).and_then(|s| s.as_ref()) {
            let outline = cell.outline.iter().map(|&[x, y]| map(x, y)).collect::<Vec<_>>();
            if outline.len() >= 3 {
                cells.push((
                    EmitterPreviewPaintShape::Polygon { outline, depth: cell.depth, area: cell.area },
                    col,
                ));
                continue;
            }
        }

        let a = map(e.pos[0] - e.aperture.half_w, e.pos[1] + e.aperture.half_h);
        let b = map(e.pos[0] + e.aperture.half_w, e.pos[1] - e.aperture.half_h);
        let rect = egui::Rect::from_two_pos(a, b);
        let cell = rect.shrink((rect.width().min(rect.height()) * 0.08).clamp(0.2, 1.2));
        if e.aperture.round {
            cells.push((
                EmitterPreviewPaintShape::Circle {
                    center: cell.center(),
                    radius: cell.width().min(cell.height()) * 0.5,
                    depth: 0.0,
                },
                col,
            ));
        } else {
            let round = (cell.width().min(cell.height()) * 0.06).clamp(1.0, 3.0);
            cells.push((EmitterPreviewPaintShape::Rect { rect: cell, round, depth: 0.0 }, col));
        }
    }
    cells.sort_by(|a, b| {
        a.0.depth()
            .total_cmp(&b.0.depth())
            .then_with(|| b.0.area().total_cmp(&a.0.area()))
    });
    for (shape, col) in &cells {
        shape.fill(&painter, *col);
    }
    for (shape, _) in &cells {
        shape.stroke(&painter, stroke);
    }
}

enum EmitterPreviewPaintShape {
    Polygon { outline: Vec<egui::Pos2>, depth: f32, area: f32 },
    Circle { center: egui::Pos2, radius: f32, depth: f32 },
    Rect { rect: egui::Rect, round: f32, depth: f32 },
}

impl EmitterPreviewPaintShape {
    fn depth(&self) -> f32 {
        match self {
            Self::Polygon { depth, .. } | Self::Circle { depth, .. } | Self::Rect { depth, .. } => *depth,
        }
    }

    fn area(&self) -> f32 {
        match self {
            Self::Polygon { area, .. } => *area,
            Self::Circle { radius, .. } => std::f32::consts::PI * radius * radius,
            Self::Rect { rect, .. } => rect.width() * rect.height(),
        }
    }

    fn fill(&self, painter: &egui::Painter, color: Color32) {
        match self {
            Self::Polygon { outline, .. } => {
                painter.add(egui::Shape::convex_polygon(outline.clone(), color, egui::Stroke::NONE));
            }
            Self::Circle { center, radius, .. } => {
                painter.circle_filled(*center, *radius, color);
            }
            Self::Rect { rect, round, .. } => {
                painter.rect_filled(*rect, *round, color);
            }
        }
    }

    fn stroke(&self, painter: &egui::Painter, stroke: egui::Stroke) {
        match self {
            Self::Polygon { outline, .. } => {
                if outline.len() >= 3 {
                    painter.add(egui::Shape::closed_line(outline.clone(), stroke));
                }
            }
            Self::Circle { center, radius, .. } => {
                painter.circle_stroke(*center, *radius, stroke);
            }
            Self::Rect { rect, round, .. } => {
                painter.rect_stroke(*rect, *round, stroke, egui::StrokeKind::Inside);
            }
        }
    }
}

fn build_emitter_preview_shapes(gdtf: &GdtfFixture, mode_index: usize) -> Vec<Option<EmitterPreviewCell>> {
    let emitters = gdtf.emitters(mode_index);
    if emitters.is_empty() {
        return Vec::new();
    }

    #[derive(Clone)]
    struct PolarEmitter {
        idx: usize,
        r: f32,
        theta: f32,
    }

    #[derive(Clone)]
    struct Ring {
        r: f32,
        items: Vec<PolarEmitter>,
    }

    let mut polar = Vec::new();
    for (idx, e) in emitters.iter().enumerate() {
        if e.merged_into.is_some() {
            continue;
        }
        let x = e.pos[0];
        let y = e.pos[1];
        if !(x.is_finite() && y.is_finite()) {
            continue;
        }
        let r = (x * x + y * y).sqrt();
        let theta = y.atan2(x).rem_euclid(std::f32::consts::TAU);
        polar.push(PolarEmitter { idx, r, theta });
    }
    if polar.len() < 8 {
        return vec![None; emitters.len()];
    }
    let max_r = polar.iter().map(|p| p.r).fold(0.0_f32, f32::max);
    if max_r < 1e-4 {
        return vec![None; emitters.len()];
    }

    polar.sort_by(|a, b| a.r.total_cmp(&b.r));
    let ring_tol = (max_r * 0.08).max(1e-4);
    let mut rings: Vec<Ring> = Vec::new();
    for p in polar {
        match rings.last_mut() {
            Some(ring) if (p.r - ring.r).abs() <= ring_tol => {
                let n = ring.items.len() as f32;
                ring.r = (ring.r * n + p.r) / (n + 1.0);
                ring.items.push(p);
            }
            _ => rings.push(Ring { r: p.r, items: vec![p] }),
        }
    }
    if !rings.iter().any(|r| r.items.len() >= 6) {
        return vec![None; emitters.len()];
    }

    rings.sort_by(|a, b| a.r.total_cmp(&b.r));
    let mut out = vec![None; emitters.len()];
    for ri in 0..rings.len() {
        let ring = &rings[ri];
        let inner = if ri == 0 {
            0.0
        } else {
            (rings[ri - 1].r + ring.r) * 0.5
        };
        let outer = if ri + 1 < rings.len() {
            (ring.r + rings[ri + 1].r) * 0.5
        } else {
            (ring.r + (ring.r - inner).max(max_r * 0.08)).max(max_r * 0.1)
        };

        let mut items = ring.items.clone();
        items.sort_by(|a, b| a.theta.total_cmp(&b.theta));
        if items.len() == 1 && inner <= max_r * 0.08 {
            let outline = circle_polygon(outer, 40);
            let area = polygon_area_2d(&outline);
            out[items[0].idx] = Some(EmitterPreviewCell { outline, depth: 0.0, area });
            continue;
        }
        if items.len() < 2 {
            let e = &emitters[items[0].idx];
            let outline = ellipse_polygon(e.pos[0], e.pos[1], e.aperture.half_w, e.aperture.half_h, 28);
            let area = polygon_area_2d(&outline);
            out[items[0].idx] = Some(EmitterPreviewCell { outline, depth: 0.0, area });
            continue;
        }

        for i in 0..items.len() {
            let theta = items[i].theta;
            let prev = if i == 0 {
                items[items.len() - 1].theta - std::f32::consts::TAU
            } else {
                items[i - 1].theta
            };
            let next = if i + 1 == items.len() {
                items[0].theta + std::f32::consts::TAU
            } else {
                items[i + 1].theta
            };
            let start = (prev + theta) * 0.5;
            let end = (theta + next) * 0.5;
            let outline = annular_sector_polygon(inner, outer, start, end);
            let area = polygon_area_2d(&outline);
            out[items[i].idx] = Some(EmitterPreviewCell { outline, depth: 0.0, area });
        }
    }
    out
}

fn circle_polygon(radius: f32, steps: usize) -> Vec<[f32; 2]> {
    (0..steps)
        .map(|i| {
            let t = i as f32 / steps as f32 * std::f32::consts::TAU;
            [t.cos() * radius, t.sin() * radius]
        })
        .collect()
}

fn ellipse_polygon(cx: f32, cy: f32, rx: f32, ry: f32, steps: usize) -> Vec<[f32; 2]> {
    (0..steps)
        .map(|i| {
            let t = i as f32 / steps as f32 * std::f32::consts::TAU;
            [cx + t.cos() * rx, cy + t.sin() * ry]
        })
        .collect()
}

fn annular_sector_polygon(inner: f32, outer: f32, start: f32, end: f32) -> Vec<[f32; 2]> {
    let span = (end - start).abs().max(1e-4);
    let steps = ((span / std::f32::consts::TAU) * 48.0).ceil().clamp(2.0, 10.0) as usize;
    let mut pts = Vec::with_capacity((steps + 1) * 2);
    for s in 0..=steps {
        let t = start + (end - start) * (s as f32 / steps as f32);
        pts.push([t.cos() * outer, t.sin() * outer]);
    }
    if inner > 1e-4 {
        for s in (0..=steps).rev() {
            let t = start + (end - start) * (s as f32 / steps as f32);
            pts.push([t.cos() * inner, t.sin() * inner]);
        }
    } else {
        pts.push([0.0, 0.0]);
    }
    pts
}

fn polygon_area_2d(points: &[[f32; 2]]) -> f32 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| a[0] * b[1] - a[1] * b[0])
        .sum::<f32>()
        .abs()
        * 0.5
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
    GdtfTextures { thumbnail, wheels, emitter_shapes: HashMap::new() }
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
    use crate::optics::OpticField;

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
