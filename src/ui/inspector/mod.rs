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
pub use props::{Inspect, Props};

use super::panels::InspectorEdit;
use super::render_panel::RenderUiState;
use super::theme;
use super::windows::ProfileEditor;
use super::{GdtfTextures, ScreenSources};
use crate::dmx::PatchTable;
use crate::gdtf::GdtfFixture;
use crate::optics::OpticalControls;
use crate::scene::environment::Environment;
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
    let d = directories::ProjectDirs::from("dev", "Embedder", "glowstone")?;
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
        geometry_inspector(ui, scene, &geo, state);
        return;
    }

    // LED screens take the Inspector when selected.
    let scr: Vec<usize> = selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
    if let Some(&primary) = scr.first() {
        screen::led_screen_inspector(ui, &mut scene.screens[primary], scr.len(), sources, state);
        return;
    }

    // Pyro devices (CO2 cannons + cold-spark machines) take the Inspector.
    let pyro: Vec<usize> = selection.pyro.iter().copied().filter(|&i| i < scene.pyro.len()).collect();
    if let Some(&primary) = pyro.first() {
        pyro_inspector(ui, &mut scene.pyro[primary], pyro.len(), state);
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

/// Inspector for a selected stage **pyro device** (CO2 cannon or cold-spark
/// machine): the heading/Visible chrome + the decomposed transform here, then the
/// kind-aware Effect/Movement/Quality/DMX grid via `impl Inspect for PyroDevice`.
/// Mirrors [`geometry_inspector`] (the device stores a `Mat4`, decomposed/recomposed
/// on edit) — when patched + receiving live DMX the Effect values are console-driven,
/// otherwise they are the free-run preview values.
fn pyro_inspector(ui: &mut egui::Ui, d: &mut crate::scene::PyroDevice, count: usize, state: &mut InspectorState) {
    ui.heading(d.name.as_str());
    ui.label(RichText::new(format!("{} · {}", d.kind.label(), d.profile_name)).weak().small());
    if count > 1 {
        ui.label(RichText::new(format!("{count} devices — editing the active one")).weak().small());
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
        p.group("Transform", theme::icon::INSPECTOR, true, |p| {
            pos_changed |= p.vec3("Position", &mut pos).speed(0.05).show();
            rot_changed |= p.rotation("Rotation", &mut rot);
        });
        d.inspect(p);
    });
    if rot_changed {
        d.transform = Mat4::from_rotation_translation(rot, pos);
    } else if pos_changed {
        d.transform.w_axis = pos.extend(1.0);
    }
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

