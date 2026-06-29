//! Declarative, type-driven property inspection — the Rust equivalent of UE4's
//! reflected `UPROPERTY` editor.
//!
//! A type that can be edited in the Inspector implements [`Inspect`] and *declares*
//! its editable properties through [`Props`]. The builder decides HOW each is drawn
//! from its TYPE + a little chained metadata, so the 2-column layout, alignment,
//! value-column width, filtering, "modified only", collapse persistence and the
//! revert-to-default arrow all live in ONE place instead of being copy-pasted into
//! every row:
//!
//! ```ignore
//! impl Inspect for Fixture {
//!     fn inspect(&mut self, p: &mut Props) {
//!         p.group("Transform", icon::INSPECTOR, true, |p| {
//!             p.vec3("Position", &mut self.position).speed(0.05);
//!             p.f32("Move speed", &mut self.move_speed).range(0.0..=1.0).slider().default(0.0);
//!         });
//!     }
//! }
//! ```
//!
//! `p.vec3` KNOWS to stack X/Y/Z; `p.f32(..).slider()` KNOWS the standardized slider
//! layout; `.default(x)` drives the reset arrow + the "modified only" gate. Adding a
//! property is one line and it is automatically aligned, filterable and resettable.
//!
//! Unlike the legacy `row`/`field_row` helpers (which require an enclosing
//! `egui::Grid` and a hand-maintained `&[&str]` filter index per category), a `Props`
//! row is a self-contained `ui.horizontal` sized from `available_width`, so widths are
//! exact and groups/sub-heads/advanced sections interleave freely. Each group also
//! auto-collects its row labels (a cheap declare-twice pass), so the filter index can
//! never drift out of sync with the rows actually rendered.

use std::ops::RangeInclusive;

use egui::{DragValue, RichText, Slider};

use super::{
    INSPECTOR_LABEL_W, INSPECTOR_SLIDER_READOUT, InspectorState, approx, approx_rgb, reset_arrow,
};

/// A type whose editable properties can be shown in the Inspector. Implementors only
/// DECLARE what their properties are + their type; [`Props`] renders them uniformly.
pub trait Inspect {
    fn inspect(&mut self, p: &mut Props);
}

/// Render an [`Inspect`] value's property grid into `ui`.
pub fn show(ui: &mut egui::Ui, state: &mut InspectorState, obj: &mut impl Inspect) {
    with(ui, state, |p| obj.inspect(p));
}

/// Run an arbitrary closure with a root [`Props`] — for inspectors that need
/// surrounding imperative logic (e.g. a decomposed transform recomposed afterward)
/// rather than a clean `impl Inspect`. Collapse toggles are applied to `state` +
/// persisted after the pass (so the builder itself only needs `&state`).
pub fn with(ui: &mut egui::Ui, state: &mut InspectorState, body: impl FnOnce(&mut Props)) {
    let mut pending: Vec<(String, bool)> = Vec::new();
    {
        let mut p = Props {
            state,
            pending: &mut pending,
            salt: String::new(),
            mode: PropMode::Render(ui),
        };
        body(&mut p);
    }
    if !pending.is_empty() {
        for (title, open) in pending {
            state.collapsed.insert(title, open);
        }
        state.save();
    }
}

/// The builder handed to [`Inspect::inspect`]. It runs each group body twice: once to
/// COLLECT its row labels (for filter-driven category hiding) and once to RENDER.
pub struct Props<'a> {
    state: &'a InspectorState,
    /// Collapse toggles raised this frame, applied to the (mutable) state after the pass.
    pending: &'a mut Vec<(String, bool)>,
    /// id-salt for a nested Advanced disclosure (the enclosing group's title).
    salt: String,
    mode: PropMode<'a>,
}

enum PropMode<'a> {
    /// Declare-only: push each row's label so the group knows its filter index.
    Collect(&'a mut Vec<String>),
    /// Render to egui.
    Render(&'a mut egui::Ui),
}

// --- value-cell layout ------------------------------------------------------

/// Margin kept to the right of the value cell so it stops just shy of the panel edge.
const VALUE_RIGHT_MARGIN: f32 = 8.0;

/// The value cell's width = the row's remaining width minus a small right margin.
/// Taken from the live `available_width` inside the row (after the label cell), so it
/// is exact regardless of dock/scroll/indent — no panel-width guessing.
fn value_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - VALUE_RIGHT_MARGIN).max(60.0)
}

fn row_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y.max(20.0)
}

/// A fixed label column + a value cell that FILLS the rest (DragValue/ComboBox/color
/// are cross-justify-aware). Returns whether the revert arrow was clicked.
fn field_shell(
    ui: &mut egui::Ui,
    label: &str,
    differs: bool,
    blank_label: bool,
    enabled: bool,
    value: impl FnOnce(&mut egui::Ui),
) -> bool {
    let h = row_height(ui);
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.set_min_height(h);
        ui.allocate_ui_with_layout(
            egui::vec2(INSPECTOR_LABEL_W, h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if !enabled {
                    ui.disable(); // grey the whole row's label, not just the widget
                }
                reset = reset_arrow(ui, differs);
                if !blank_label {
                    ui.add(egui::Label::new(label).truncate());
                }
            },
        );
        let vw = value_width(ui);
        ui.allocate_ui_with_layout(
            egui::vec2(vw, h),
            egui::Layout::top_down_justified(egui::Align::Min),
            value,
        );
    });
    reset
}

/// Like [`field_shell`] but the value cell hosts a SLIDER: the bar fills the cell minus
/// a fixed readout reserve so the slider's value lands on the same right edge as fields.
fn slider_shell(
    ui: &mut egui::Ui,
    label: &str,
    differs: bool,
    enabled: bool,
    value: impl FnOnce(&mut egui::Ui),
) -> bool {
    let h = row_height(ui);
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.set_min_height(h);
        ui.allocate_ui_with_layout(
            egui::vec2(INSPECTOR_LABEL_W, h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if !enabled {
                    ui.disable();
                }
                reset = reset_arrow(ui, differs);
                ui.add(egui::Label::new(label).truncate());
            },
        );
        let vw = value_width(ui);
        ui.allocate_ui_with_layout(
            egui::vec2(vw, h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().slider_width = (vw - INSPECTOR_SLIDER_READOUT).max(24.0);
                value(ui);
            },
        );
    });
    reset
}

// --- grouping ---------------------------------------------------------------

impl<'a> Props<'a> {
    /// Collect this body's row labels into a fresh Vec (the declare-only pass).
    fn collect_labels(&self, body: &mut dyn FnMut(&mut Props)) -> Vec<String> {
        let mut labels: Vec<String> = Vec::new();
        let mut throwaway = Vec::new();
        let mut c = Props {
            state: self.state,
            pending: &mut throwaway,
            salt: self.salt.clone(),
            mode: PropMode::Collect(&mut labels),
        };
        body(&mut c);
        labels
    }

    /// A collapsible category. `body` declares the rows; it is run once to collect its
    /// filter labels and once to render. Collapse state persists by `title`.
    pub fn group(
        &mut self,
        title: &str,
        icon: &str,
        default_open: bool,
        mut body: impl FnMut(&mut Props),
    ) {
        let labels = self.collect_labels(&mut body);
        match &mut self.mode {
            // Nested inside a declare-pass: fold this group's labels into the parent's.
            PropMode::Collect(parent) => parent.extend(labels),
            PropMode::Render(ui) => {
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                if !self.state.category_visible(&refs) {
                    return;
                }
                let open = self.state.open_state(title, default_open);
                let filtering = self.state.query().is_some();
                let header = if icon.is_empty() {
                    title.to_string()
                } else {
                    format!("{icon}  {title}")
                };
                let state = self.state;
                let pending = &mut *self.pending;
                let resp = egui::CollapsingHeader::new(header)
                    .id_salt(("inspector-cat", title))
                    .open(Some(open))
                    .show(ui, |ui| {
                        let mut inner = Props {
                            state,
                            pending,
                            salt: title.to_string(),
                            mode: PropMode::Render(ui),
                        };
                        body(&mut inner);
                    });
                // Filtering force-opens, so ignore clicks then (they'd fight the override).
                if resp.header_response.clicked() && !filtering {
                    self.pending.push((title.to_string(), !open));
                }
            }
        }
    }

    /// A nested, default-collapsed "Advanced" disclosure for power-user rows. `sub`
    /// disambiguates multiple Advanced sections within ONE group (e.g. "beam"/"color"
    /// in Optics) so their disclosure ids don't collide.
    pub fn advanced(&mut self, sub: &str, mut body: impl FnMut(&mut Props)) {
        let labels = self.collect_labels(&mut body);
        let group_salt = self.salt.clone();
        match &mut self.mode {
            // The parent group's label index must include advanced rows too.
            PropMode::Collect(parent) => parent.extend(labels),
            PropMode::Render(ui) => {
                let filtering = self.state.query().is_some();
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                if filtering && !self.state.category_visible(&refs) {
                    return;
                }
                ui.add_space(2.0);
                let state = self.state;
                let pending = &mut *self.pending;
                let mut ch = egui::CollapsingHeader::new(RichText::new("Advanced").small().weak())
                    .id_salt(("inspector-advanced", group_salt.as_str(), sub))
                    .default_open(false);
                if filtering {
                    ch = ch.open(Some(true)); // surface matched advanced rows
                }
                ch.show(ui, |ui| {
                    let mut inner = Props {
                        state,
                        pending,
                        salt: group_salt,
                        mode: PropMode::Render(ui),
                    };
                    body(&mut inner);
                });
            }
        }
    }

    /// Whether any of `labels` passes the active filter — for gating a sub-heading so
    /// it doesn't show above a section whose rows the filter all hid.
    pub fn any_visible(&self, labels: &[&str]) -> bool {
        labels.iter().any(|l| self.state.row_visible(l))
    }

    /// A small section sub-heading (e.g. "BEAM SHAPING"). Not a filterable row.
    pub fn subhead(&mut self, text: &str) {
        if let PropMode::Render(ui) = &mut self.mode {
            ui.add_space(2.0);
            ui.label(RichText::new(text).small().weak().strong());
        }
    }

    /// A read-only descriptive note (weak/small). Not a filterable row.
    pub fn note(&mut self, text: impl Into<String>) {
        if let PropMode::Render(ui) = &mut self.mode {
            ui.label(RichText::new(text.into()).small().weak());
        }
    }

    // --- field declarations -------------------------------------------------

    /// A draggable scalar (the default) — chain `.slider()` for a bar, `.range()`,
    /// `.speed()`, `.suffix()`, `.default()`, `.enabled()`, `.tip()`.
    pub fn f32<'p>(&'p mut self, label: &str, value: &'p mut f32) -> NumField<'p, 'a> {
        NumField {
            p: self,
            label: label.to_string(),
            value,
            range: None,
            speed: 0.1,
            suffix: "",
            decimals: None,
            default: None,
            slider: false,
            no_clamp: false,
            enabled: true,
            tip: None,
            text_value: None,
            shown: false,
        }
    }

    /// A stacked X/Y/Z vector (Blender-style). Chain `.speed()`/`.suffix()`/`.range()`/
    /// `.prefixes()` (e.g. W/H/D for a size).
    pub fn vec3<'p>(&'p mut self, label: &str, value: &'p mut glam::Vec3) -> Vec3Field<'p, 'a> {
        Vec3Field {
            p: self,
            label: label.to_string(),
            value,
            speed: 0.1,
            suffix: "",
            prefixes: ["X", "Y", "Z"],
            range: None,
            shown: false,
        }
    }

    /// A rotation, edited as stacked X/Y/Z euler degrees (YXZ) and recomposed to the
    /// quaternion; the revert arrow snaps back to identity. Returns whether it changed.
    pub fn rotation(&mut self, label: &str, value: &mut glam::Quat) -> bool {
        let (ry, rx, rz) = value.to_euler(glam::EulerRot::YXZ);
        let (mut ex, mut ey, mut ez) = (rx.to_degrees(), ry.to_degrees(), rz.to_degrees());
        let differs = !approx(ex, 0.0) || !approx(ey, 0.0) || !approx(ez, 0.0);
        if self.vec3_raw(
            label,
            differs,
            0.5,
            "°",
            ["X", "Y", "Z"],
            None,
            &mut ex,
            &mut ey,
            &mut ez,
        ) {
            *value = glam::Quat::from_euler(
                glam::EulerRot::YXZ,
                ey.to_radians(),
                ex.to_radians(),
                ez.to_radians(),
            );
            true
        } else {
            false
        }
    }

    /// An RGB colour swatch. `default = Some` shows a revert arrow (else none).
    pub fn color(&mut self, label: &str, value: &mut [f32; 3], default: Option<[f32; 3]>) -> bool {
        let differs = default.is_some_and(|d| !approx_rgb(*value, d));
        let state = self.state;
        match &mut self.mode {
            PropMode::Collect(labels) => {
                labels.push(label.to_string());
                false
            }
            PropMode::Render(ui) => {
                if !state.row_shown(label, differs) {
                    return false;
                }
                let mut changed = false;
                let reset = field_shell(ui, label, differs, false, true, |ui| {
                    changed = ui.color_edit_button_rgb(value).changed();
                });
                if let (true, Some(d)) = (reset, default) {
                    *value = d;
                    changed = true;
                }
                changed
            }
        }
    }

    /// A dropdown over `options` (value, display). Returns whether the selection changed.
    pub fn combo<T: PartialEq + Clone>(
        &mut self,
        label: &str,
        value: &mut T,
        options: &[(T, &'static str)],
    ) -> bool {
        let state = self.state;
        match &mut self.mode {
            PropMode::Collect(labels) => {
                labels.push(label.to_string());
                false
            }
            PropMode::Render(ui) => {
                if !state.row_shown(label, false) {
                    return false;
                }
                let mut changed = false;
                let current = options
                    .iter()
                    .find(|(v, _)| v == value)
                    .map(|(_, t)| *t)
                    .unwrap_or("");
                field_shell(ui, label, false, false, true, |ui| {
                    egui::ComboBox::from_id_salt(("inspector-combo", label))
                        .width(value_width(ui))
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (v, t) in options {
                                if ui.selectable_label(value == v, *t).clicked() && value != v {
                                    *value = v.clone();
                                    changed = true;
                                }
                            }
                        });
                });
                changed
            }
        }
    }

    /// A labelled row whose value cell is rendered by an arbitrary egui closure — the
    /// escape hatch for compound/bespoke controls (a w×h pair, a combo with side
    /// effects, a file picker) that don't map to a single typed field. `label` keeps
    /// the fixed 2-column alignment + filtering.
    pub fn custom(&mut self, label: &str, enabled: bool, body: impl FnOnce(&mut egui::Ui)) {
        let state = self.state;
        match &mut self.mode {
            PropMode::Collect(labels) => labels.push(label.to_string()),
            PropMode::Render(ui) => {
                if state.row_shown(label, false) {
                    field_shell(ui, label, false, false, enabled, body);
                }
            }
        }
    }

    /// A FULL-WIDTH bespoke block (no label column) — for sections that aren't rows at
    /// all (e.g. the LED content-source editor). `labels` are registered for the
    /// enclosing group's filter index; `body` renders only when at least one matches.
    pub fn custom_block(&mut self, labels: &[&str], body: impl FnOnce(&mut egui::Ui)) {
        let state = self.state;
        match &mut self.mode {
            PropMode::Collect(collected) => collected.extend(labels.iter().map(|l| l.to_string())),
            PropMode::Render(ui) => {
                if labels.iter().any(|l| state.row_visible(l)) {
                    body(ui);
                }
            }
        }
    }

    /// The shared stacked-vector renderer (X/Y/Z). Reset zeroes all three. Returns
    /// whether any component changed (drag or reset).
    #[allow(clippy::too_many_arguments)]
    fn vec3_raw(
        &mut self,
        label: &str,
        differs: bool,
        speed: f64,
        suffix: &'static str,
        prefixes: [&'static str; 3],
        range: Option<RangeInclusive<f32>>,
        x: &mut f32,
        y: &mut f32,
        z: &mut f32,
    ) -> bool {
        let state = self.state;
        match &mut self.mode {
            PropMode::Collect(labels) => {
                labels.push(label.to_string());
                false
            }
            PropMode::Render(ui) => {
                if !state.row_shown(label, differs) {
                    return false;
                }
                let mut changed = false;
                let comps: [(&str, &mut f32); 3] =
                    [(prefixes[0], x), (prefixes[1], y), (prefixes[2], z)];
                let mut reset = false;
                for (i, (axis, comp)) in comps.into_iter().enumerate() {
                    let r = field_shell(
                        ui,
                        label,
                        if i == 0 { differs } else { false },
                        i != 0,
                        true,
                        |ui| {
                            let mut d =
                                DragValue::new(comp).speed(speed).prefix(format!("{axis} "));
                            if let Some(r) = &range {
                                d = d.range(r.clone());
                            }
                            if !suffix.is_empty() {
                                d = d.suffix(suffix);
                            }
                            if ui.add(d).changed() {
                                changed = true;
                            }
                        },
                    );
                    if i == 0 {
                        reset = r;
                    }
                }
                if reset {
                    *x = 0.0;
                    *y = 0.0;
                    *z = 0.0;
                    changed = true;
                }
                changed
            }
        }
    }
}

// --- NumField (scalar f32 builder) ------------------------------------------

/// A chained scalar-property declaration; renders on drop (or on an explicit
/// [`NumField::show`] when the caller needs the change result, e.g. an indirect setter).
pub struct NumField<'p, 'a> {
    p: &'p mut Props<'a>,
    label: String,
    value: &'p mut f32,
    range: Option<RangeInclusive<f32>>,
    speed: f64,
    suffix: &'static str,
    decimals: Option<usize>,
    default: Option<f32>,
    slider: bool,
    /// When set, a `.slider()` never CLAMPS to its range — the range only suggests a
    /// span for the bar; the user can type/drag any value past it
    /// ([`egui::SliderClamping::Never`]). Used by the pyro Effect/Movement sliders.
    no_clamp: bool,
    enabled: bool,
    tip: Option<String>,
    /// Slider readout override (e.g. zoom shown as "58°" instead of the raw 0–1).
    text_value: Option<String>,
    shown: bool,
}

impl<'p, 'a> NumField<'p, 'a> {
    pub fn range(mut self, r: RangeInclusive<f32>) -> Self {
        self.range = Some(r);
        self
    }
    pub fn speed(mut self, s: f64) -> Self {
        self.speed = s;
        self
    }
    pub fn suffix(mut self, s: &'static str) -> Self {
        self.suffix = s;
        self
    }
    pub fn decimals(mut self, n: usize) -> Self {
        self.decimals = Some(n);
        self
    }
    /// The revert-to-default target; presence enables the reset arrow + modified-gate.
    pub fn default(mut self, d: f32) -> Self {
        self.default = Some(d);
        self
    }
    /// As [`Self::default`] but from an `Option` (None ⇒ no arrow, e.g. a built-in
    /// fixture whose template beam-angle isn't recoverable).
    pub fn default_opt(mut self, d: Option<f32>) -> Self {
        self.default = d;
        self
    }
    pub fn slider(mut self) -> Self {
        self.slider = true;
        self
    }
    /// Make this slider's range a SUGGESTION, not a clamp — the user can type or drag
    /// any value past the bar's ends ([`egui::SliderClamping::Never`]). No effect on a
    /// non-slider field. Used by the pyro Effect/Movement sliders.
    pub fn no_clamp(mut self) -> Self {
        self.no_clamp = true;
        self
    }
    pub fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
    /// Override a slider's numeric readout with derived text (e.g. zoom "58°").
    pub fn text_value(mut self, t: impl Into<String>) -> Self {
        self.text_value = Some(t.into());
        self
    }
    /// A hover tooltip. Accepts a static str or an owned `String` (e.g. a live
    /// "commanded · now 45°" read-out).
    pub fn tip(mut self, t: impl Into<String>) -> Self {
        self.tip = Some(t.into());
        self
    }

    /// Render now and return whether the value changed (drag or reset) — for callers
    /// that must react (e.g. write through an indirect setter).
    pub fn show(mut self) -> bool {
        self.render()
    }

    fn render(&mut self) -> bool {
        if self.shown {
            return false;
        }
        self.shown = true;
        // A disabled field never shows the revert arrow (matches the optics rows: a
        // greyed/unsupported control can't "differ" actionably).
        let differs = self.enabled && self.default.is_some_and(|d| !approx(*self.value, d));
        let state = self.p.state;
        let label: &str = &self.label;
        match &mut self.p.mode {
            PropMode::Collect(labels) => {
                labels.push(label.to_string());
                false
            }
            PropMode::Render(ui) => {
                if !state.row_shown(label, differs) {
                    return false;
                }
                let value = &mut *self.value;
                let range = self.range.clone();
                let tip = self.tip.clone();
                let text_value = self.text_value.clone();
                let (speed, suffix, decimals, slider, enabled, no_clamp) = (
                    self.speed,
                    self.suffix,
                    self.decimals,
                    self.slider,
                    self.enabled,
                    self.no_clamp,
                );
                let mut changed = false;
                let build = |ui: &mut egui::Ui| {
                    let resp = if slider {
                        let mut s = Slider::new(value, range.unwrap_or(0.0..=1.0));
                        if no_clamp {
                            // The range only suggests a span — typing/dragging past it is allowed.
                            s = s.clamping(egui::SliderClamping::Never);
                        }
                        if let Some(n) = decimals {
                            s = s.max_decimals(n);
                        }
                        if let Some(t) = text_value {
                            s = s.text(t).show_value(false);
                        }
                        ui.add_enabled(enabled, s)
                    } else {
                        let mut d = DragValue::new(value).speed(speed);
                        if let Some(r) = range {
                            d = d.range(r);
                        }
                        if !suffix.is_empty() {
                            d = d.suffix(suffix);
                        }
                        if let Some(n) = decimals {
                            d = d.max_decimals(n);
                        }
                        ui.add_enabled(enabled, d)
                    };
                    changed = resp.changed();
                    if let Some(t) = tip {
                        resp.on_hover_text(t);
                    }
                };
                let reset = if slider {
                    slider_shell(ui, label, differs, enabled, build)
                } else {
                    field_shell(ui, label, differs, false, enabled, build)
                };
                if let (true, Some(d)) = (reset, self.default) {
                    *self.value = d;
                    changed = true;
                }
                changed
            }
        }
    }
}

impl Drop for NumField<'_, '_> {
    fn drop(&mut self) {
        if !self.shown {
            self.render();
        }
    }
}

// --- Vec3Field (stacked vector builder) -------------------------------------

/// A chained stacked-vector declaration; renders on drop.
pub struct Vec3Field<'p, 'a> {
    p: &'p mut Props<'a>,
    label: String,
    value: &'p mut glam::Vec3,
    speed: f64,
    suffix: &'static str,
    prefixes: [&'static str; 3],
    range: Option<RangeInclusive<f32>>,
    shown: bool,
}

impl<'p, 'a> Vec3Field<'p, 'a> {
    pub fn speed(mut self, s: f64) -> Self {
        self.speed = s;
        self
    }
    /// Per-component prefixes (default X/Y/Z) — e.g. `["W", "H", "D"]` for a size.
    pub fn prefixes(mut self, p: [&'static str; 3]) -> Self {
        self.prefixes = p;
        self
    }
    pub fn range(mut self, r: RangeInclusive<f32>) -> Self {
        self.range = Some(r);
        self
    }

    /// Render now and return whether any component changed.
    pub fn show(mut self) -> bool {
        self.render()
    }

    fn render(&mut self) -> bool {
        if self.shown {
            return false;
        }
        self.shown = true;
        let label = self.label.clone();
        let (speed, suffix, prefixes) = (self.speed, self.suffix, self.prefixes);
        let range = self.range.clone();
        let (mut x, mut y, mut z) = (self.value.x, self.value.y, self.value.z);
        // A plain vector (Position/Center/Size) has no recoverable default → no revert
        // arrow (`differs = false`); reset-to-identity rotations go through `Props::rotation`.
        let changed = self.p.vec3_raw(
            &label, false, speed, suffix, prefixes, range, &mut x, &mut y, &mut z,
        );
        if changed {
            *self.value = glam::vec3(x, y, z);
        }
        changed
    }
}

impl Drop for Vec3Field<'_, '_> {
    fn drop(&mut self) {
        self.render();
    }
}
