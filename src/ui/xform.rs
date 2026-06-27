//! Transform-tool options — **grid/increment snap** (#4) + **pivot-point selector**
//! (#5), `docs/RESEARCH-industry-patterns.md` §2.4 / backlog rows 4-5.
//!
//! Two orthogonal knobs the gizmo + modal G/R/S transforms read each frame:
//!
//! * [`SnapSettings`] — a toggle + per-type increments (default 1 m / 15° / 0.1×).
//!   When on, [`apply_transform`](super::panels) quantizes the COMMITTED amount,
//!   not the raw mouse delta: Move snaps each per-axis component to `step·round(v/
//!   step)`, Rotate the angle to the nearest `step`°, Scale the factor to the
//!   nearest `step`. Composes with the numeric entry (a typed value is snapped the
//!   same way). Holding **Ctrl** mid-drag inverts the toggle for that frame —
//!   matching Blender (snap normally on while Ctrl off, Ctrl forces it the other
//!   way) and Unreal's `FSnappingUtils` central gate.
//!
//! * [`PivotMode`] — how the pivot the rotate/scale spins/grows about is computed
//!   from the selection: Median (centroid, the prior hardcoded behaviour), Active
//!   element's origin, Individual Origins (each element about ITS OWN origin), or
//!   the world 3D-Cursor. Mirrors Blender's `V3D_AROUND_*` (transform_convert.cc
//!   `calc_pivot`). Individual-Origins is the only mode where the pivot is per-
//!   element, so [`TransformOp`](super::TransformOp) carries an `individual` flag
//!   the applier honours; the other three resolve to a single world pivot up front.
//!
//! The pure quantizers live here (no egui/scene deps) so the snap truth-table is
//! unit-tested directly.

/// How the transform pivot is derived from the current selection (§2.4 #5).
/// Mirrors Blender's pivot-point dropdown (`V3D_AROUND_*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PivotMode {
    /// Centroid of every selected element (the prior, hardcoded behaviour).
    #[default]
    Median,
    /// Each element transforms about ITS OWN origin (fixtures don't orbit a
    /// shared centre; a scattered rig scales/spins in place). The applier reads
    /// `TransformOp.individual`.
    Individual,
    /// The primary (active) element's origin — the first selected.
    Active,
    /// The world 3D-cursor point (movable; lives on `Ui::cursor_3d`).
    Cursor3d,
}

impl PivotMode {
    /// Dropdown order + the three non-cursor entries the header cycles through.
    pub const ALL: [PivotMode; 4] =
        [PivotMode::Median, PivotMode::Active, PivotMode::Individual, PivotMode::Cursor3d];

    /// Short menu label.
    pub fn label(self) -> &'static str {
        match self {
            PivotMode::Median => "Median Point",
            PivotMode::Individual => "Individual Origins",
            PivotMode::Active => "Active Element",
            PivotMode::Cursor3d => "3D Cursor",
        }
    }

    /// One-line hover hint.
    pub fn hint(self) -> &'static str {
        match self {
            PivotMode::Median => "Pivot about the selection centroid",
            PivotMode::Individual => "Each element transforms about its own origin",
            PivotMode::Active => "Pivot about the active (first) element's origin",
            PivotMode::Cursor3d => "Pivot about the world 3D cursor",
        }
    }

    /// True only for the per-element mode the applier special-cases.
    pub fn is_individual(self) -> bool {
        matches!(self, PivotMode::Individual)
    }
}

/// The basis the move/rotate/scale axis is expressed in (§2.4 #37 / row 37).
/// Mirrors Blender's transform-orientation dropdown (`V3D_ORIENT_*`,
/// `transform_orientations.cc` `applyTransformOrientation` /
/// `ED_transform_calc_orientation_from_type_ex`):
///
/// * `Global` — the world axes (the prior, only behaviour). Basis = identity.
/// * `Local` — the active element's OWN orientation basis (a head on a raked
///   truss moves along ITS up, not the world's). Basis = the primary selected
///   fixture's orientation `Quat` (or the geometry transform's rotation 3×3).
/// * `View` — the camera basis: X = screen-right, Y = screen-up, Z = toward the
///   viewer. A `View`-space move follows the screen plane. Basis columns =
///   `camera.view_basis()` (right, up, −forward).
///
/// The chosen orientation produces a 3×3 `basis` whose COLUMNS are the X/Y/Z
/// directions; [`apply_transform`](super::panels) maps the axis-lock (and the
/// numeric single-value default axis) through `basis * Axis::vec()`, so axis-lock
/// and numeric input compose with the orientation. Global resolves to the world
/// axes, leaving today's behaviour byte-identical.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TransformOrientation {
    /// World axes (identity basis) — the prior hardcoded behaviour.
    #[default]
    Global,
    /// The active element's own orientation basis (Quat / geometry rotation).
    Local,
    /// The camera basis (right / up / toward-viewer) — moves follow the screen.
    View,
}

impl TransformOrientation {
    /// Dropdown order, matching Blender's Global → Local → View.
    pub const ALL: [TransformOrientation; 3] =
        [TransformOrientation::Global, TransformOrientation::Local, TransformOrientation::View];

    /// Short menu / header label.
    pub fn label(self) -> &'static str {
        match self {
            TransformOrientation::Global => "Global",
            TransformOrientation::Local => "Local",
            TransformOrientation::View => "View",
        }
    }

    /// One-line hover hint.
    pub fn hint(self) -> &'static str {
        match self {
            TransformOrientation::Global => "Transform along the world axes",
            TransformOrientation::Local => "Transform along the active element's own axes",
            TransformOrientation::View => "Transform along the camera's screen axes",
        }
    }
}

/// What a Move snap targets (§2.4 #71 / row 71 — "snap fixture to scene
/// vertex / truss node" — generalized to a small snap-MODE selector beside the
/// header Snap toggle). Mirrors Blender's snap-element dropdown
/// (`SCE_SNAP_MODE_INCREMENT` / `_VERTEX` / `_FACE`, `transform_snap.cc`):
///
/// * `Increment` — the grid/increment quantizer (today's only behaviour): the
///   committed Move/Rotate/Scale amount rounds to the per-type step.
/// * `Vertex` — the moved origin jumps to the nearest OTHER entity origin
///   (fixture / geometry / screen), within a screen-space pixel threshold, so a
///   head clicks onto a truss node. Move only (Rotate/Scale fall back to
///   Increment-or-off, like Blender, since vertex snap is positional).
/// * `Surface` — the moved origin lands on the nearest geometry/ground hit under
///   the cursor (reuses `pick_world_point`), so a fixture drops onto a deck.
///
/// Vertex/Surface are absolute snaps (they REPLACE the dragged position, like
/// Blender's `snapObjectsTransform`); Increment quantizes the delta. Only `Move`
/// honours Vertex/Surface — Rotate/Scale keep the increment quantizer regardless
/// (a vertex/surface target has no meaning for an angle or a factor).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SnapMode {
    /// Grid/increment quantize the committed amount (the prior behaviour).
    #[default]
    Increment,
    /// Snap the moved origin to the nearest other entity origin (truss node).
    Vertex,
    /// Snap the moved origin onto the nearest geometry/ground hit under the cursor.
    Surface,
}

impl SnapMode {
    /// Dropdown order, mirroring Blender's Increment → Vertex → Face.
    pub const ALL: [SnapMode; 3] = [SnapMode::Increment, SnapMode::Vertex, SnapMode::Surface];

    /// Short menu / header label.
    pub fn label(self) -> &'static str {
        match self {
            SnapMode::Increment => "Grid",
            SnapMode::Vertex => "Vertex",
            SnapMode::Surface => "Surface",
        }
    }

    /// One-line hover hint.
    pub fn hint(self) -> &'static str {
        match self {
            SnapMode::Increment => "Snap the moved amount to the grid increment",
            SnapMode::Vertex => "Snap the moved origin to the nearest other object's origin",
            SnapMode::Surface => "Drop the moved origin onto the surface under the cursor",
        }
    }
}

/// Grid/increment snap config (§2.4 #4). `on` is the persistent toggle; the per-
/// type increments default to 1 m / 15° / 0.1× (Blender's defaults; the doc's
/// per-type 0.25 m / 15° / 10% is the #94 follow-up — these are sane round
/// starters and user-editable in the N-panel).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SnapSettings {
    /// Master toggle. Ctrl held mid-drag inverts this for the live frame.
    pub on: bool,
    /// What a Move snap targets (§2.4 #71): grid increment / vertex / surface.
    /// Rotate + Scale always use the increment quantizer regardless of this.
    pub mode: SnapMode,
    /// Move increment, metres.
    pub move_step: f32,
    /// Rotate increment, degrees.
    pub rotate_deg: f32,
    /// Scale increment (factor).
    pub scale_step: f32,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self { on: false, mode: SnapMode::Increment, move_step: 1.0, rotate_deg: 15.0, scale_step: 0.1 }
    }
}

/// Round `v` to the nearest multiple of `step`. A non-positive `step` is a no-op
/// (returns `v` unchanged) so a zeroed N-panel field can't divide-by-zero or
/// collapse the value. The single quantizer all three transform kinds share.
#[inline]
pub fn quantize(v: f32, step: f32) -> f32 {
    if step > 0.0 {
        (v / step).round() * step
    } else {
        v
    }
}

impl SnapSettings {
    /// Snap a per-axis Move world delta — each component independently (Blender's
    /// `snap_increment_apply` snaps in the constraint space, which for our world-
    /// axis locks is component-wise). `enabled` is the live, Ctrl-inverted state.
    pub fn snap_move(&self, world: glam::Vec3, enabled: bool) -> glam::Vec3 {
        if !enabled {
            return world;
        }
        glam::Vec3::new(
            quantize(world.x, self.move_step),
            quantize(world.y, self.move_step),
            quantize(world.z, self.move_step),
        )
    }

    /// Snap a Rotate angle (radians in/out) to the nearest `rotate_deg` step.
    pub fn snap_angle(&self, radians: f32, enabled: bool) -> f32 {
        if !enabled {
            return radians;
        }
        quantize(radians.to_degrees(), self.rotate_deg).to_radians()
    }

    /// Snap a Scale factor to the nearest `scale_step`, never below one step (a
    /// snapped scale of 0 would collapse geometry to the pivot).
    pub fn snap_scale(&self, factor: f32, enabled: bool) -> f32 {
        if !enabled {
            return factor;
        }
        quantize(factor, self.scale_step).max(self.scale_step.max(0.0001))
    }
}

/// Per-viewport transform-tool options held on [`Ui`](super::Ui) (transient — not
/// persisted, just live UI state). The gizmo
/// + modal blocks read `pivot`/`snap`; the header + N-panel write them.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct TransformPrefs {
    pub pivot: PivotMode,
    /// The basis the move/rotate/scale axis is expressed in (§2.4 #37).
    pub orientation: TransformOrientation,
    pub snap: SnapSettings,
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn quantize_rounds_to_nearest_step() {
        assert_eq!(quantize(1.4, 1.0), 1.0);
        assert_eq!(quantize(1.6, 1.0), 2.0);
        assert_eq!(quantize(-1.6, 1.0), -2.0);
        // 0.1 grid.
        assert!((quantize(0.34, 0.1) - 0.3).abs() < 1e-6);
        // Non-positive step is a no-op (no divide-by-zero / collapse).
        assert_eq!(quantize(0.37, 0.0), 0.37);
        assert_eq!(quantize(0.37, -1.0), 0.37);
    }

    #[test]
    fn snap_move_quantizes_each_axis_when_enabled() {
        let s = SnapSettings { on: true, move_step: 1.0, ..Default::default() };
        // Disabled → identity even though `on` is true (Ctrl-inverted off).
        let raw = Vec3::new(1.4, -0.6, 2.5);
        assert_eq!(s.snap_move(raw, false), raw);
        // Enabled → component-wise round to the 1 m grid.
        assert_eq!(s.snap_move(raw, true), Vec3::new(1.0, -1.0, 3.0));
    }

    #[test]
    fn snap_angle_rounds_to_degree_step() {
        let s = SnapSettings { rotate_deg: 15.0, ..Default::default() };
        // 20° → nearest 15° = 15°.
        let got = s.snap_angle(20f32.to_radians(), true).to_degrees();
        assert!((got - 15.0).abs() < 1e-3, "got {got}");
        // 23° → 30°.
        let got = s.snap_angle(23f32.to_radians(), true).to_degrees();
        assert!((got - 30.0).abs() < 1e-3, "got {got}");
        // Disabled passes through.
        let r = 0.123_f32;
        assert!((s.snap_angle(r, false) - r).abs() < 1e-9);
    }

    #[test]
    fn snap_scale_rounds_and_never_collapses() {
        let s = SnapSettings { scale_step: 0.1, ..Default::default() };
        assert!((s.snap_scale(1.04, true) - 1.0).abs() < 1e-6);
        assert!((s.snap_scale(1.36, true) - 1.4).abs() < 1e-6);
        // A near-zero factor clamps up to one step, never 0 (no collapse).
        assert!(s.snap_scale(0.0, true) >= 0.1 - 1e-6);
    }
}
