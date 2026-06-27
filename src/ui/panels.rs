//! The individual dock panels. Each is a plain function taking the egui `Ui`
//! plus whatever scene state it reads or edits.

use egui::{Color32, DragValue, Grid, RichText, Sense};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3};

use super::gizmo::{self, GizmoCtx, Handle};
use super::nav_gizmo;
use super::outliner::{fixture_order, SceneSort};
use super::shortcuts;
use super::theme;
use super::viewport_math::*;
use super::windows::{LabelMode, Preferences};
use super::{
    ActiveTool, Axis, DuplicateDialog, NumInput, TransformKind, TransformOp, TransformPrefs,
};
use crate::dmx::patch::channel_map;
use crate::dmx::{DmxConfig, DmxStatus, MergePolicy, PatchSource, PatchTable, PendingNetCmd, UniverseSnapshot};
use crate::renderer::camera::OrbitCamera;
use crate::scene::{apply_fixture_click, apply_select, ObjectRef, Scene, SelItem, SelectOp, Selection};

/// Universe is considered live if it updated within this window.
const DMX_STALE: std::time::Duration = std::time::Duration::from_millis(2500);

/// Per-frame drag edges the [`inspector`] reports up to [`Ui`](super::Ui) so a
/// slider / DragValue drag becomes ONE undo step (P0 #13). The inspector edits the
/// scene directly (its established live-edit model); these flags let the
/// post-dock consumer wrap the WHOLE gesture in a single [`op::DragTx`]
/// transaction — `started` snapshots the `before`, `stopped` pushes one step.
/// Detected at panel scope (no per-widget instrumentation): a numeric widget
/// inside the inspector's content rect began / ended a pointer drag this frame.
#[derive(Default, Clone, Copy)]
pub struct InspectorEdit {
    /// A numeric drag inside the inspector just began this frame.
    pub started: bool,
    /// A numeric drag inside the inspector was released this frame.
    pub stopped: bool,
}

/// Left tab: the scene outliner — every fixture and environment, selectable —
/// plus the global view/look controls.
/// Discovered live video sources for the LED-screen content pickers, refreshed
/// by the app each frame from the NDI + CITP clients.
#[derive(Default, Clone)]
pub struct ScreenSources {
    /// NDI source names (empty unless built with the `ndi` feature + a runtime).
    pub ndi: Vec<String>,
    /// Whether NDI receive is compiled in AND a runtime is present.
    pub ndi_available: bool,
    /// Discovered CITP media-server names.
    pub citp: Vec<String>,
}

/// Paint left-anchored text truncated with an ellipsis to `max_w` (single row) —
/// used by the dense list rows so a long name can't run under the right column.
pub(super) fn paint_truncated(
    painter: &egui::Painter,
    top_left: egui::Pos2,
    text: &str,
    size: f32,
    color: Color32,
    max_w: f32,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat { font_id: egui::FontId::proportional(size), color, ..Default::default() },
    );
    job.wrap = TextWrapping {
        max_width: max_w.max(8.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    painter.galley(top_left, galley, color);
}

/// Apply an in-progress modal transform to the scene from the current mouse
/// position. Reads the snapshot in `op.start`, so it's idempotent — called every
/// frame the op is live; cancelling restores from the same snapshot.
pub(super) fn apply_transform(
    op: &TransformOp,
    scene: &mut Scene,
    camera: &OrbitCamera,
    cur: egui::Pos2,
    // Live snap-enabled state (#4): `op.snap.on` XOR a held-Ctrl invert, resolved by
    // the caller this frame. Quantizes the COMMITTED amount (delta / angle / factor),
    // never the raw mouse delta — so it composes with the numeric entry too.
    snap_on: bool,
) {
    // Position/orientation are read directly by the renderer, so they need no
    // snap_movement() — and calling it every frame would freeze each fixture's
    // wheel-motion phase. (Cancel restores from the same snapshot the same way.)
    let d = cur - op.start_screen; // pixel delta (mouse-driven path)
    let (right, up, _fwd) = camera.view_basis();
    // A grabbed gizmo handle overrides the keyboard axis lock for this frame.
    let axis = op.active_axis();
    // #37: the transform-orientation basis (columns = X/Y/Z directions). The axis
    // lock + numeric-default axis map through this — Global = identity (world axes),
    // Local = the element's own basis, View = the camera basis. `axis_dir` gives the
    // world direction of an `Axis` in the chosen orientation.
    let basis = orientation_basis(op, camera);
    let axis_dir = |a: Axis| basis * a.vec();
    // Explicit-amount: a typed number OVERRIDES the mouse (Blender applyNumInput
    // returns true). Single value → along the active axis (Move falls back to
    // global X, Rotate to Y, Scale to uniform — matching the mouse-path defaults).
    let amount = op.num.value();
    // During a duplicate-grab the mouse ALWAYS drives the offset; a typed number is
    // the array clone-count (applied on confirm), not the move amount — so don't let
    // it override the drag here.
    let typed = op.num.active && !op.from_duplicate;
    // Build a picking ray from a screen position through the op's viewport rect
    // (#40 absolute drag + #71 Vertex/Surface snap both need world rays, not just
    // the pixel delta). Aspect derives from the stored rect.
    let aspect = op.viewport.width() / op.viewport.height().max(1.0);
    let ray_at = |p: egui::Pos2| -> (Vec3, Vec3) {
        let size = op.viewport.size().max(egui::vec2(1.0, 1.0));
        let uv = (p - op.viewport.min) / size;
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        camera.ray(ndc, aspect)
    };
    match op.kind {
        TransformKind::Move => {
            let mut world = if typed {
                // Metres along the active axis (in the chosen orientation); no lock →
                // the orientation's X (Blender's single-value-no-constraint default,
                // expressed in the active basis — world X for Global).
                let a = axis.map(axis_dir).unwrap_or_else(|| basis * Vec3::X);
                a * amount
            } else if op.gizmo_view && op.from_gizmo && op.viewport.area() > 0.0 {
                // VIEW-plane move (#72): the screen-parallel drag. The plane normal is
                // the camera forward, so the grabbed centre square slides on the plane
                // facing the viewer and STICKS to the cursor (a ray_plane_point
                // absolute drag, same machinery as the axis-pair plane handles).
                let n = camera.view_basis().2;
                let (ro0, rd0) = ray_at(op.start_screen);
                let (ro1, rd1) = ray_at(cur);
                match (
                    ray_plane_point(ro0, rd0, op.pivot, n),
                    ray_plane_point(ro1, rd1, op.pivot, n),
                ) {
                    (Some(from), Some(to)) => to - from,
                    _ => Vec3::ZERO,
                }
            } else if let Some(normal) = op.gizmo_plane_normal.filter(|_| op.from_gizmo && op.viewport.area() > 0.0) {
                // PLANE handle (#S2): intersect the start + live cursor rays with the
                // plane through the pivot whose normal is the held axis (in the active
                // orientation), and take the difference. The off-plane axis stays fixed
                // (the delta lies in the plane), and the grabbed quad STICKS to the
                // cursor at any camera angle. Falls back to no motion if either ray
                // misses (grazing the plane edge-on).
                let n = axis_dir(normal);
                let (ro0, rd0) = ray_at(op.start_screen);
                let (ro1, rd1) = ray_at(cur);
                match (
                    ray_plane_point(ro0, rd0, op.pivot, n),
                    ray_plane_point(ro1, rd1, op.pivot, n),
                ) {
                    (Some(from), Some(to)) => to - from,
                    _ => Vec3::ZERO,
                }
            } else if op.from_gizmo && axis.is_some() && op.viewport.area() > 0.0 {
                // #40 ray-plane ABSOLUTE drag: project the start + live cursor rays onto
                // the constraint axis line through the pivot; the world delta is the
                // difference, so the grabbed handle STICKS to the cursor at any camera
                // angle (vs the pixel-speed heuristic that drifts at grazing angles).
                let a = axis_dir(axis.unwrap());
                let (ro0, rd0) = ray_at(op.start_screen);
                let (ro1, rd1) = ray_at(cur);
                let from = ray_axis_closest_point(ro0, rd0, op.pivot, a);
                let to = ray_axis_closest_point(ro1, rd1, op.pivot, a);
                to - from
            } else {
                let speed = camera.distance * 0.0015;
                let mut w = right * (d.x * speed) + up * (-d.y * speed);
                if let Some(ax) = axis {
                    let a = axis_dir(ax); // the axis in the active orientation
                    w = a * w.dot(a); // lock to that (possibly tilted) axis
                }
                w
            };
            // SNAP. Vertex/Surface (#71) REPLACE the moved origin absolutely (Blender's
            // snapObjectsTransform): they only apply to Move, and they re-aim `world` so
            // the PRIMARY element's origin lands on the snap target — the rest of the
            // selection rides the same delta (rigid). Grid/Increment (the default and
            // every Rotate/Scale) quantizes the committed DELTA instead.
            let primary_p0 = op.start.first().map(|(_, p, _)| *p);
            let snapped_absolute: Option<Vec3> = if snap_on {
                match op.snap.mode {
                    super::SnapMode::Vertex => primary_p0.map(|p0| {
                        let vp = camera.view_proj(aspect);
                        let exclude: Vec<usize> = op.start.iter().map(|(i, _, _)| *i).collect();
                        // Threshold the live CURSOR (not the projected origin) so the snap
                        // engages when the pointer is over a node, Blender-style. When
                        // nothing is in range, keep the un-quantized free `world`.
                        match nearest_origin_screen(scene, vp, op.viewport, cur, &exclude, 18.0) {
                            Some(target) => target - p0,
                            None => world,
                        }
                    }),
                    super::SnapMode::Surface => {
                        let (ro, rd) = ray_at(cur);
                        // Surface needs a hit AND a primary origin; otherwise fall through
                        // to the free `world` (no quantize) so the drag still moves.
                        match (primary_p0, pick_world_point(scene, ro, rd)) {
                            (Some(p0), Some(hit)) => Some(hit - p0),
                            _ => Some(world),
                        }
                    }
                    super::SnapMode::Increment => None,
                }
            } else {
                None
            };
            if let Some(w) = snapped_absolute {
                // Vertex/Surface already produced an absolute world delta — no grid
                // quantize on top (the snap target IS the destination).
                world = w;
            } else {
                // #4 Grid/Increment: quantize the committed delta (composes with the typed
                // path; `snapped_absolute` is None ⇒ snap is off OR mode == Increment).
                // For a world-axis (Global, unconstrained or axis-locked) we snap per world
                // component as before; for an ORIENTED axis lock the delta lies along a
                // tilted direction, so we snap its scalar magnitude along that axis
                // (Blender snaps in the constraint space) rather than per world component.
                match axis {
                    Some(ax) if op.orientation != super::TransformOrientation::Global => {
                        let dir = axis_dir(ax);
                        let mag = crate::ui::xform::quantize(world.dot(dir), op.snap.move_step);
                        if snap_on {
                            world = dir * mag;
                        }
                    }
                    _ => world = op.snap.snap_move(world, snap_on),
                }
            }
            for (i, p0, _q) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    f.position = *p0 + world;
                }
            }
            for (i, m0) in &op.geo_start {
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = Mat4::from_translation(world) * *m0;
                }
            }
            // Screens ride the identical Mat4 path as geometry.
            for (i, m0) in &op.screen_start {
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = Mat4::from_translation(world) * *m0;
                }
            }
            // Pyro devices ride the identical Mat4 path too.
            for (i, m0) in &op.pyro_start {
                if let Some(p) = scene.pyro.get_mut(*i) {
                    p.transform = Mat4::from_translation(world) * *m0;
                }
            }
            // Fog volumes: slide the centre (size unchanged).
            for (i, c0, _sz) in &op.env_start {
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = *c0 + world;
                }
            }
        }
        TransformKind::Rotate => {
            // Typed degrees override the mouse-derived angle (radians); #4 snaps the
            // committed angle to the rotate increment (e.g. nearest 15°).
            let angle = if typed { amount.to_radians() } else { d.x * 0.01 };
            let angle = op.snap.snap_angle(angle, snap_on);
            // Rotate about the active axis IN THE CHOSEN ORIENTATION: Local spins about
            // the element's own axis (a raked head tilts about its local pitch axis),
            // View about the camera axis. No lock → the orientation's Y. A grabbed
            // screen-axis VIEW ring (#72) overrides everything and spins about the live
            // camera forward (Blender's white trackball ring).
            let raxis = if op.gizmo_view && op.from_gizmo {
                camera.view_basis().2
            } else {
                axis.map(axis_dir).unwrap_or_else(|| basis * Vec3::Y)
            };
            let rot = Quat::from_axis_angle(raxis, angle);
            for (i, p0, q0) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    // Individual Origins (#5): each fixture spins about ITS OWN origin
                    // (p0), so its position is unchanged and only orientation turns;
                    // else everything orbits the shared pivot.
                    let pivot = if op.individual { *p0 } else { op.pivot };
                    f.position = pivot + rot * (*p0 - pivot);
                    f.orientation = rot * *q0;
                }
            }
            for (i, m0) in &op.geo_start {
                // Individual Origins: pivot = each piece's own world-bounds centre at
                // op start (read off m0); else the shared pivot.
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * Mat4::from_quat(rot)
                    * Mat4::from_translation(-pivot);
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = about * *m0;
                }
            }
            for (i, m0) in &op.screen_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * Mat4::from_quat(rot)
                    * Mat4::from_translation(-pivot);
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = about * *m0;
                }
            }
            for (i, m0) in &op.pyro_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * Mat4::from_quat(rot)
                    * Mat4::from_translation(-pivot);
                if let Some(p) = scene.pyro.get_mut(*i) {
                    p.transform = about * *m0;
                }
            }
            for (i, c0, _sz) in &op.env_start {
                // Axis-aligned fog box: orbit the centre about the pivot (a no-op
                // for a lone box whose pivot IS its centre); the box stays
                // axis-aligned, so size is unchanged.
                let pivot = if op.individual { *c0 } else { op.pivot };
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = pivot + rot * (*c0 - pivot);
                }
            }
        }
        TransformKind::Scale => {
            // Typed 1 → ×1 (identity); clamp >0 (Blender NUM_NO_ZERO). #4 snaps the
            // committed factor to the scale increment (never to 0 → no collapse).
            let factor = if typed {
                amount.max(0.0001)
            } else {
                (1.0 + d.x * 0.005).max(0.01)
            };
            let factor = op.snap.snap_scale(factor, snap_on);
            for (i, p0, _q) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    // Individual Origins: scaling a point about ITSELF is a no-op, so
                    // a scattered fixture rig stays put (only geometry visibly grows).
                    let pivot = if op.individual { *p0 } else { op.pivot };
                    let off = *p0 - pivot;
                    let new = if let Some(ax) = op.axis {
                        // Scale only the locked axis IN THE CHOSEN ORIENTATION: decompose
                        // the offset along the (possibly tilted) axis direction.
                        let a = axis_dir(ax);
                        let comp = a * off.dot(a);
                        (off - comp) + comp * factor
                    } else {
                        off * factor
                    };
                    f.position = pivot + new;
                }
            }
            // Scale about the pivot in world space. Uniform → a plain scale matrix;
            // an axis lock builds a DIRECTIONAL scale `I + (factor-1)·d⊗d` so the
            // stretch follows the locked axis in the chosen orientation (a world-axis
            // direction reduces to the per-component scale this used to do).
            let scale_mat: Mat3 = match op.axis {
                Some(ax) => {
                    let d = axis_dir(ax);
                    Mat3::IDENTITY + (factor - 1.0) * Mat3::from_cols(d * d.x, d * d.y, d * d.z)
                }
                None => Mat3::from_diagonal(Vec3::splat(factor)),
            };
            let about4 = Mat4::from_mat3(scale_mat);
            for (i, m0) in &op.geo_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * about4
                    * Mat4::from_translation(-pivot);
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = about * *m0;
                }
            }
            for (i, m0) in &op.screen_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * about4
                    * Mat4::from_translation(-pivot);
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = about * *m0;
                }
            }
            for (i, m0) in &op.pyro_start {
                let pivot = if op.individual { geo_world_centre(*m0) } else { op.pivot };
                let about = Mat4::from_translation(pivot)
                    * about4
                    * Mat4::from_translation(-pivot);
                if let Some(p) = scene.pyro.get_mut(*i) {
                    p.transform = about * *m0;
                }
            }
            for (i, c0, sz0) in &op.env_start {
                // Fog box: scale the centre about the pivot and grow the size by the
                // same (directional or uniform) factor — a lone box scales in place.
                let pivot = if op.individual { *c0 } else { op.pivot };
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = pivot + scale_mat * (*c0 - pivot);
                    e.size = (scale_mat * *sz0).abs();
                }
            }
        }
    }
}

/// The world-space translation of a geometry transform (its origin) — the
/// Individual-Origins pivot for static geometry. Uses the transform's own
/// translation (cheap, stable across the live drag; the AABB centre would drift as
/// the piece scales, which is wrong for an about-its-own-origin pivot).
#[inline]
fn geo_world_centre(m: Mat4) -> Vec3 {
    m.w_axis.truncate()
}

/// The net world translation the primary copy received during a duplicate-grab —
/// the spacing the array clone-count repeats along. Reads the primary element's
/// current origin minus its op-start snapshot (the move is a pure translation).
fn dup_grab_delta(op: &TransformOp, scene: &Scene) -> Vec3 {
    if let Some((i, p0, _)) = op.start.first() {
        scene.fixtures.get(*i).map(|f| f.position - *p0).unwrap_or(Vec3::ZERO)
    } else if let Some((i, m0)) = op.geo_start.first() {
        scene.geometry.get(*i).map(|g| (g.transform.w_axis - m0.w_axis).truncate()).unwrap_or(Vec3::ZERO)
    } else if let Some((i, m0)) = op.screen_start.first() {
        scene.screens.get(*i).map(|s| (s.transform.w_axis - m0.w_axis).truncate()).unwrap_or(Vec3::ZERO)
    } else if let Some((i, m0)) = op.pyro_start.first() {
        scene.pyro.get(*i).map(|p| (p.transform.w_axis - m0.w_axis).truncate()).unwrap_or(Vec3::ZERO)
    } else if let Some((i, c0, _)) = op.env_start.first() {
        scene.environments.get(*i).map(|e| e.center - *c0).unwrap_or(Vec3::ZERO)
    } else {
        Vec3::ZERO
    }
}

/// Drop the last `count` objects of `kind`'s Vec — the live Shift+D array clones,
/// which are always appended at the END (LIFO), so a tail-truncate removes exactly
/// them (on shrink or cancel) without disturbing the base copies' indices.
fn truncate_objects(scene: &mut Scene, kind: ObjectRef, count: usize) {
    match kind {
        ObjectRef::Fixture(_) => {
            let l = scene.fixtures.len();
            scene.fixtures.truncate(l.saturating_sub(count));
        }
        ObjectRef::Geometry(_) => {
            let l = scene.geometry.len();
            scene.geometry.truncate(l.saturating_sub(count));
        }
        ObjectRef::Screen(_) => {
            let l = scene.screens.len();
            scene.screens.truncate(l.saturating_sub(count));
        }
        ObjectRef::Pyro(_) => {
            let l = scene.pyro.len();
            scene.pyro.truncate(l.saturating_sub(count));
        }
        ObjectRef::Environment(_) => {
            let l = scene.environments.len();
            scene.environments.truncate(l.saturating_sub(count));
        }
    }
}

/// Place live array clone `k` (mirroring base copy `b`) at `base_home[b] + off` —
/// the base copy's op-start pose translated along the drag vector.
fn place_array_extra(scene: &mut Scene, op: &TransformOp, k: usize, b: usize, off: Vec3) {
    match op.dup_extra[k] {
        ObjectRef::Fixture(e) => {
            if let (Some((_, p0, q0)), Some(f)) = (op.start.get(b), scene.fixtures.get_mut(e)) {
                f.position = *p0 + off;
                f.orientation = *q0;
                f.snap_movement();
            }
        }
        ObjectRef::Geometry(e) => {
            if let (Some((_, m0)), Some(g)) = (op.geo_start.get(b), scene.geometry.get_mut(e)) {
                g.transform = Mat4::from_translation(off) * *m0;
            }
        }
        ObjectRef::Screen(e) => {
            if let (Some((_, m0)), Some(s)) = (op.screen_start.get(b), scene.screens.get_mut(e)) {
                s.transform = Mat4::from_translation(off) * *m0;
            }
        }
        ObjectRef::Pyro(e) => {
            if let (Some((_, m0)), Some(p)) = (op.pyro_start.get(b), scene.pyro.get_mut(e)) {
                p.transform = Mat4::from_translation(off) * *m0;
            }
        }
        ObjectRef::Environment(e) => {
            if let (Some((_, c0, sz0)), Some(env)) = (op.env_start.get(b), scene.environments.get_mut(e)) {
                env.center = *c0 + off;
                env.size = *sz0;
            }
        }
    }
}

/// Regenerate the LIVE Shift+D array each frame so the WHOLE array follows the
/// cursor while a clone-count is typed (not just one dragged copy). Grows/shrinks
/// the clone set to the typed count and repositions every clone along the current
/// drag vector. Clones live at the END of their kind's Vec (LIFO) so shrink/cancel
/// tail-truncates them. Skipped on the op's first frame (where the move's `before`
/// undo snapshot is captured — BEFORE any extras exist — so undo removes them).
pub(super) fn update_dup_array(op: &mut TransformOp, scene: &mut Scene) {
    if !op.from_duplicate || op.dup_base.is_empty() {
        return;
    }
    let base_len = op.dup_base.len();
    let n = if op.num.active { (op.num.value().round() as i64).clamp(1, 1000) as u32 } else { 1 };
    let desired = (n.saturating_sub(1) as usize) * base_len; // count of EXTRA clones (#2..N)
    if op.dup_extra.len() > desired {
        let remove = op.dup_extra.len() - desired;
        truncate_objects(scene, op.dup_base[0], remove);
        op.dup_extra.truncate(desired);
    }
    while op.dup_extra.len() < desired {
        let b = op.dup_extra.len() % base_len;
        match scene.duplicate_object(op.dup_base[b]) {
            Some(new) => op.dup_extra.push(new),
            None => break,
        }
    }
    let delta = dup_grab_delta(op, scene);
    for k in 0..op.dup_extra.len() {
        let b = k % base_len;
        let i = 2 + (k / base_len); // array index (#1 is the base copy)
        place_array_extra(scene, op, k, b, delta * i as f32);
    }
}

/// Live state for the Measure tool (§2.4) — a read-only two-point ruler that never
/// mutates the scene (so no op / no undo). `a` is the first point; once `b` is set the
/// segment + its distance label persist until a third click resets to a fresh `a`.
/// Esc clears both. Held on [`super::Ui`] so the measurement survives across frames /
/// tool switches; cleared lazily when the Measure tool isn't active.
#[derive(Clone, Copy, Default)]
pub struct MeasureState {
    /// First picked world point (set on the first click).
    pub a: Option<Vec3>,
    /// Second picked world point (set on the second click) — completes the ruler.
    pub b: Option<Vec3>,
}

impl MeasureState {
    /// Forget the current measurement (Esc, or when leaving the Measure tool).
    pub fn clear(&mut self) {
        self.a = None;
        self.b = None;
    }
}

/// Live state for the Aim tool (§2.4) — the lighting differentiator. While a drag is
/// in flight `Some(target)` holds the world point the selected heads are being aimed
/// at this frame (so the viewport can draw the target marker + aim lines). The undo
/// snapshot is taken on drag-start (via `transform_started`) and committed on release
/// (via `transform_finished`), exactly like the modal/gizmo transforms — but Aim
/// writes `pan`/`tilt` (the commanded slew targets), not position/orientation, so it
/// is NOT a [`TransformOp`]. `active()` lets the caller keep the pending undo snapshot
/// alive across the intermediate drag frames (when no `TransformOp` is in flight).
#[derive(Clone, Copy, Default)]
pub struct AimState {
    /// The world target under the cursor this frame while dragging; `None` when idle.
    target: Option<Vec3>,
}

impl AimState {
    /// Whether an aim drag is currently in flight (a snapshot is pending a commit).
    pub fn active(&self) -> bool {
        self.target.is_some()
    }
}

/// Where a viewport ray lands in the world, for the Measure tool: the nearest hit on
/// real surfaces (fixtures' bodies, screens, geometry AABBs, environment volumes),
/// falling back to the **ground plane y=0** when the ray misses everything (so you can
/// always measure floor distances). Returns the world-space hit point. Unlike `pick`
/// this wants a *point*, not a `Hit`, so it tracks the nearest `t` across all surfaces.
fn pick_world_point(scene: &Scene, ro: Vec3, rd: Vec3) -> Option<Vec3> {
    let mut best_t = f32::INFINITY;
    let mut consider = |t: f32| {
        if t > 0.0 && t < best_t {
            best_t = t;
        }
    };
    for f in &scene.fixtures {
        if !f.hidden && let Some(t) = ray_sphere(ro, rd, f.position, 0.5) {
            consider(t);
        }
    }
    for s in &scene.screens {
        if !s.hidden && let Some(t) = s.ray_hit(ro, rd) {
            consider(t);
        }
    }
    for g in &scene.geometry {
        if !g.hidden
            && let Some((lo, hi)) = g.world_bounds()
            && let Some(t) = ray_aabb(ro, rd, lo, hi)
        {
            consider(t);
        }
    }
    for e in &scene.environments {
        if e.hidden {
            continue; // outliner eye: a hidden fog box isn't a measure/aim target
        }
        if let Some(t) = ray_aabb(ro, rd, e.min(), e.max()) {
            consider(t);
        }
    }
    // Ground-plane fallback: intersect the ray with y=0 when it's heading downward
    // toward (or up toward) the floor. Guards a near-parallel ray (|rd.y| tiny).
    if rd.y.abs() > 1e-4 {
        let t = -ro.y / rd.y;
        consider(t);
    }
    if best_t.is_finite() {
        Some(ro + rd * best_t)
    } else {
        None
    }
}

/// Resolve where a newly-added object should land (#19 — place at cursor/camera,
/// not origin). Casts the viewport-centre ray (NDC `0,0`) into the scene and
/// returns its ground/surface hit (via [`pick_world_point`]); if that misses
/// (e.g. the camera looks at the sky), falls back to a point a sensible distance
/// in front of the camera; if even that degenerates, the origin. The whole
/// add+place is wrapped in ONE undo op by the caller. We use the view-centre ray
/// (not the mouse) because both add entry points — the Library "Add" button and
/// the Shift+A menu — fire from outside the viewport draw, where the live mouse
/// position relative to the viewport rect isn't reachable; the framed centre is
/// the stable, predictable "where I'm looking" anchor Unreal's PlacementMode uses.
pub fn placement_point(scene: &Scene, camera: &OrbitCamera) -> Vec3 {
    let aspect = camera.last_aspect;
    let (ro, rd) = camera.ray(Vec2::ZERO, aspect);
    if let Some(p) = pick_world_point(scene, ro, rd) {
        return p;
    }
    // Ground/surface miss → place in front of the camera at the orbit distance.
    let front = ro + rd * camera.distance.max(1.0);
    if front.is_finite() {
        front
    } else {
        Vec3::ZERO
    }
}

/// Central tab: the 3D scene, rendered offscreen and shown as a texture.
/// Drag to orbit, shift+drag to pan, scroll to zoom, click to select, `d` to
/// duplicate the selected fixture; G/R/S to move/rotate/scale the selection.
#[allow(clippy::too_many_arguments)]
pub fn viewport(
    ui: &mut egui::Ui,
    camera: &mut OrbitCamera,
    scene: &mut Scene,
    selection: &mut Selection,
    scene_anchor: &mut Option<usize>,
    viewport_focused: &mut bool,
    duplicate: &mut Option<DuplicateDialog>,
    texture: egui::TextureId,
    requested_px: &mut (u32, u32),
    fps: f32,
    prefs: &Preferences,
    // RenderSettings (Mode / Exposure / Grid / Beam-gizmo) are edited from the
    // Viewport HEADER (`ui::editor`) now, not from the viewport body (§2.2). The
    // one setting the body still READS is the internal render scale (it sizes the
    // offscreen target — merged in from the perf-overlay branch); passed by value.
    render_scale: f32,
    transform: &mut Option<TransformOp>,
    delete_requested: &mut bool,
    replace_requested: &mut bool,
    // Set when Enter is pressed with the viewport focused + no live transform: the
    // caller adds the Library tab's highlighted item (mirrors Enter in the Library).
    add_requested: &mut bool,
    // Modal-transform undo signals (set this frame): `started` = a G/R/S or gizmo
    // op just began (caller snapshots the `before` end); `finished` = it confirmed
    // (caller pushes the undo step). A cancel sets neither — the op already
    // restored in-place, so the caller just drops the pending `before`.
    transform_started: &mut bool,
    transform_finished: &mut bool,
    // The viewport's active tool (§2.4). Only `ActiveTool::Move` shows + handles the
    // screen-space xform gizmo; Select (and the not-yet-wired tools) keep plain
    // click-select. The spring-loaded modal G/R/S transforms stay available under
    // every tool — they OWN the viewport once started, regardless of the rail.
    active_tool: ActiveTool,
    // Transform-tool options (§2.4 #4/#5): grid/increment snap + pivot-point mode.
    // Read when building a TransformOp (gizmo + modal G/R/S) so the snap policy and
    // pivot are baked into the op; the header/N-panel write them.
    xform: TransformPrefs,
    // The world 3D-cursor point — the PivotMode::Cursor3d pivot (§2.4 #5). Drawn as a
    // small red/white crosshair-ring; repositioned by Shift-right-click (S1-3d-cursor).
    cursor_3d: &mut Vec3,
    // Set true when a Shift-RMB places the cursor this frame, so the caller's Add menu
    // can drop new objects AT the cursor (the "set this session" gate).
    cursor_3d_set: &mut bool,
    // The Measure tool's two-point ruler state (§2.4). Persists across frames so a
    // completed measurement stays drawn; only the Measure tool reads/writes it.
    measure: &mut MeasureState,
    // The Aim tool's in-flight drag state (§2.4). Holds the world target while a drag
    // aims the selected heads; only the Aim tool reads/writes it.
    aim: &mut AimState,
) {
    *transform_started = false;
    *transform_finished = false;
    // The active keymap overrides for this frame (published by `Ui::show`). This
    // free function's signature is fixed by its app.rs caller, so it reads the
    // process-wide snapshot instead of taking `&KeymapOverrides`. EMPTY by default
    // ⇒ the poll sites below behave exactly as the static defaults.
    let ov = shortcuts::active();
    let available = ui.available_size();
    let ppp = ui.pixels_per_point();

    // Internal render scale: render the offscreen targets below native and let the
    // egui image() draw upscale them (the LDR view is FilterMode::Linear). The single
    // biggest fps lever on Retina — every per-pixel pass (forward, SSAO, volumetric,
    // post) scales with scale². Snapped to 0.05 so a slider drag doesn't reallocate
    // targets every sub-pixel step (Viewport::resize early-returns on unchanged size).
    let scale = (render_scale.clamp(0.25, 1.0) * 20.0).round() / 20.0;
    *requested_px = (
        (available.x * ppp * scale).round().max(1.0) as u32,
        (available.y * ppp * scale).round().max(1.0) as u32,
    );

    let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
    // Record the live viewport aspect so frame-selected widens its fit radius for
    // wide viewports (the aspect-correction rule in OrbitCamera::frame_aabb).
    camera.set_aspect(rect.width() / rect.height().max(1.0));
    ui.painter().image(
        texture,
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    // Focus follows the most recent pointer press: inside the viewport focuses
    // it, anywhere else releases it (so the `d` shortcut only fires in here).
    // Capture the PRIOR focus first: a click that merely focuses the viewport (it
    // wasn't the active pane) must not also wipe the selection — see the click-select.
    let was_focused = *viewport_focused;
    if ui.input(|i| i.pointer.any_pressed())
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        *viewport_focused = rect.contains(p);
    }

    let aspect = rect.width() / rect.height().max(1.0);

    // --- 3D cursor (§2.4 #5) -------------------------------------------------
    // The world cursor point — the PivotMode::Cursor3d pivot. Drawn always (like
    // Blender's 3D cursor) as a small red/white crosshair-ring so the user can see
    // where a Cursor-pivot transform will spin/grow about. Read-only here (movable
    // is a follow-up); behind every interactive overlay so it never eats clicks.
    if let Some(sc) = OrbitCamera::project_to_screen(*cursor_3d, camera.view_proj(aspect), rect) {
        let p = ui.painter_at(rect);
        let r = 7.0;
        // Dashed-look crosshair: two short ticks per arm, in cursor red + white.
        let red = egui::Color32::from_rgb(230, 70, 70);
        let white = egui::Color32::from_rgb(235, 235, 235);
        p.circle_stroke(sc, r, egui::Stroke::new(1.0, red));
        for (i, (dx, dy)) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)].iter().enumerate() {
            let col = if i % 2 == 0 { red } else { white };
            let dir = egui::vec2(*dx, *dy);
            p.line_segment([sc + dir * (r - 1.0), sc + dir * (r + 5.0)], egui::Stroke::new(1.0, col));
        }
    }

    // --- Interactive transform-gizmo group (§2.4 GizmoGroup) ---
    // The ACTIVE TOOL selects which gizmo draws at the selection pivot: Move→arrows,
    // Rotate→rings, Scale→boxes (gizmo::for_tool is the single tool→group map; Select
    // and the non-transform tools return None → plain click-select). Each group
    // hit-tests its handles in screen space; grabbing one (a press within a few px)
    // starts the matching axis-locked TransformOp — reusing apply_transform via
    // `gizmo_hovered_axis`/`axis` so all three share the live-apply + undo path. The
    // grab is checked BEFORE orbit/select so dragging a handle never orbits the
    // camera; an empty-space press falls through untouched.
    // Shift+D grab-duplicate hand-off: `Ui::show` already cloned + re-selected the
    // copies (their own undo step) and set this flag. Pick it up and IMMEDIATELY
    // start a Move grab on the copies so they follow the cursor and commit on
    // click / Enter (Esc leaves them at the source, like Blender). A typed number
    // during the grab becomes the array clone-count (see the confirm path).
    let dupgrab_start = ui.ctx().data_mut(|d| {
        let id = egui::Id::new("glowstone.dupgrab.start");
        let v = d.get_temp::<bool>(id).unwrap_or(false);
        if v {
            d.remove::<bool>(id);
        }
        v
    });
    if dupgrab_start
        && transform.is_none()
        && selection.has_object()
        && let Some(cur) = ui.input(|i| i.pointer.latest_pos())
    {
        let fids: Vec<usize> =
            selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
        let gids: Vec<usize> =
            selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
        let sids: Vec<usize> =
            selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
        let pids: Vec<usize> =
            selection.pyro.iter().copied().filter(|&i| i < scene.pyro.len()).collect();
        let eids: Vec<usize> =
            selection.environment.into_iter().filter(|&i| i < scene.environments.len()).collect();
        let objs = obj_refs(&fids, &gids, &sids, &pids, &eids);
        if !objs.is_empty() {
            let pivot = compute_pivot(scene, &objs, xform.pivot, *cursor_3d);
            let (start, geo_start, screen_start, pyro_start, env_start) =
                snapshot_starts(scene, &fids, &gids, &sids, &pids, &eids);
            *transform = Some(TransformOp {
                kind: TransformKind::Move,
                axis: None,
                start_screen: cur,
                viewport: rect,
                pivot,
                start,
                geo_start,
                screen_start,
                pyro_start,
                env_start,
                gizmo_hovered_axis: None,
                gizmo_plane_normal: None,
                gizmo_view: false,
                from_gizmo: false,
                num: NumInput::default(),
                individual: xform.pivot.is_individual(),
                snap: xform.snap,
                orientation: xform.orientation,
                from_duplicate: true,
                dup_base: objs,
                dup_extra: Vec::new(),
            });
            *transform_started = true;
        }
    }

    // The gizmo's projected screen centre (set when it draws) — the selection-label
    // pill below reads it so it can DODGE the handles instead of overlapping them.
    let mut gizmo_screen: Option<egui::Pos2> = None;
    let gizmo_targets: bool = active_tool.shows_xform_gizmo()
        && transform.is_none()
        && *viewport_focused
        && !selection.world
        && selection.has_object();
    if gizmo_targets
        && let Some(group) = gizmo::for_tool(active_tool)
    {
        // #5: the gizmo draws at the mode-resolved pivot (Median centroid / Active /
        // 3D-cursor; Individual Origins also draws at the median, matching Blender).
        let fids: Vec<usize> =
            selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
        let gids: Vec<usize> =
            selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
        let sids: Vec<usize> =
            selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
        let pids: Vec<usize> =
            selection.pyro.iter().copied().filter(|&i| i < scene.pyro.len()).collect();
        let eids: Vec<usize> =
            selection.environment.into_iter().filter(|&i| i < scene.environments.len()).collect();
        let objs = obj_refs(&fids, &gids, &sids, &pids, &eids);
        if !objs.is_empty() {
            let gizmo_pivot = compute_pivot(scene, &objs, xform.pivot, *cursor_3d);
            gizmo_screen = OrbitCamera::project_to_screen(gizmo_pivot, camera.view_proj(aspect), rect);
            let cx = GizmoCtx {
                pivot: gizmo_pivot,
                vp: camera.view_proj(aspect),
                rect,
                // Arm/ring size scales with camera distance so handles stay a
                // readable pixel size regardless of zoom.
                arm: (camera.distance * 0.18).clamp(0.4, 4.0),
                // Camera forward = the VIEW axis (#72): screen-axis rotate ring +
                // view-plane move centre resolve from this.
                forward: camera.view_basis().2,
            };
            // Highlight the handle under the live pointer; on a press we hit-test the
            // press origin instead (so the grabbed handle is the one the drag began on).
            let hover_pt = ui.input(|i| i.pointer.latest_pos());
            let hover = hover_pt.and_then(|p| group.test_select(p, &cx));
            group.draw(&ui.painter_at(rect), &cx, hover);
            // A press that landed on a handle this frame starts the op.
            let press = ui.input(|i| i.pointer.press_origin());
            let grabbed: Option<Handle> = press.and_then(|p| group.test_select(p, &cx));
            if let Some(handle) = grabbed
                && response.drag_started()
                && let Some(cur) = ui.input(|i| i.pointer.latest_pos())
            {
                let start_spec = group.invoke(handle);
                // `fids`/`gids`/`sids`/`eids` are the validated, selection-order
                // indices computed above for the pivot — reused here for the
                // per-element snapshots (every kind, via `snapshot_starts`).
                let (start, geo_start, screen_start, pyro_start, env_start) =
                    snapshot_starts(scene, &fids, &gids, &sids, &pids, &eids);
                *transform = Some(TransformOp {
                    kind: start_spec.kind,
                    // Move locks via `gizmo_hovered_axis` (matching P3a); rotate/scale
                    // carry their axis in `axis` (apply_transform reads it directly,
                    // and the uniform-scale centre yields None = scale all axes).
                    axis: if start_spec.kind == TransformKind::Move { None } else { start_spec.axis },
                    start_screen: cur,
                    viewport: rect,
                    pivot: cx.pivot,
                    start,
                    geo_start,
                    screen_start,
                    pyro_start,
                    env_start,
                    gizmo_hovered_axis: if start_spec.kind == TransformKind::Move {
                        start_spec.axis
                    } else {
                        None
                    },
                    // A grabbed PLANE quad drives the two-axis ray_plane_point drag
                    // (the normal is the held axis). Move-only; mutually exclusive
                    // with the single-axis `gizmo_hovered_axis` lock above.
                    gizmo_plane_normal: start_spec.plane_normal,
                    // A grabbed VIEW handle (#72): screen-plane Move / view-axis
                    // Rotate, resolved from the live camera basis in apply_transform.
                    gizmo_view: start_spec.view,
                    from_gizmo: true,
                    num: NumInput::default(),
                    individual: xform.pivot.is_individual(),
                    snap: xform.snap,
                    orientation: xform.orientation,
                    from_duplicate: false,
                    dup_base: Vec::new(), dup_extra: Vec::new(),
                });
                *transform_started = true;
            }
        }
    }

    // --- Measure tool (§2.4): a read-only two-point ruler. ---
    // Click sets A (ray → nearest surface, else the y=0 ground plane); a second click
    // sets B; a third click resets to a fresh A. Esc clears. NEVER mutates the scene,
    // so there is no op / no undo. Runs BEFORE the click-select block and consumes the
    // click so measuring never also picks an object. Stale state is dropped when the
    // tool isn't active (so switching away clears the ruler).
    let mut consumed = transform.is_some();
    if active_tool == ActiveTool::Measure {
        // Esc clears the current measurement (decoded from the shared modal keymap so
        // the bind stays in the one registry, like the transform Cancel).
        if shortcuts::poll_modal(ui.ctx(), &ov).contains(&shortcuts::ModalAction::Cancel) {
            measure.clear();
        }
        if !consumed
            && response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
            let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let (ro, rd) = camera.ray(ndc, aspect);
            if let Some(p) = pick_world_point(scene, ro, rd) {
                match (measure.a, measure.b) {
                    // Fresh, or restarting after a completed measurement → new A.
                    (None, _) | (Some(_), Some(_)) => {
                        measure.a = Some(p);
                        measure.b = None;
                    }
                    // A set, B open → complete the ruler.
                    (Some(_), None) => measure.b = Some(p),
                }
            }
            consumed = true; // never fall through to click-select
        }
        // Draw the ruler: a dashed-ish polyline A→(B or live cursor) + endpoint dots
        // + a distance pill at the midpoint (metres/feet per prefs). With only A set we
        // preview to the cursor's ground/surface hit so the length reads live.
        if let Some(a) = measure.a {
            let painter = ui.painter_at(rect);
            let vp = camera.view_proj(aspect);
            // The far end: the committed B, else a live preview under the cursor.
            let live_b = measure.b.or_else(|| {
                ui.input(|i| i.pointer.latest_pos()).filter(|p| rect.contains(*p)).and_then(|pos| {
                    let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
                    let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
                    let (ro, rd) = camera.ray(ndc, aspect);
                    pick_world_point(scene, ro, rd)
                })
            });
            let sa = OrbitCamera::project_to_screen(a, vp, rect);
            let sb = live_b.and_then(|b| OrbitCamera::project_to_screen(b, vp, rect));
            let accent = egui::Color32::from_rgb(120, 220, 160);
            if let Some(sa) = sa {
                painter.circle_filled(sa, 4.0, accent);
            }
            if let (Some(sa), Some(sb)) = (sa, sb) {
                painter.line_segment([sa, sb], egui::Stroke::new(2.0, accent));
            }
            if let Some(sb) = sb {
                painter.circle_filled(sb, 4.0, accent);
            }
            // Distance label at the segment midpoint (or near A while only A is set).
            if let Some(b) = live_b {
                let metres = (b - a).length();
                let (val, unit) = prefs.len(metres);
                let mid = sa
                    .zip(sb)
                    .map(|(p, q)| p + (q - p) * 0.5)
                    .or(sa)
                    .unwrap_or_else(|| rect.center());
                theme::overlay_label(
                    &painter,
                    mid + egui::vec2(0.0, -14.0),
                    egui::Align2::CENTER_BOTTOM,
                    &format!("{val:.2}{unit}"),
                    Some(accent),
                );
            }
        }
    } else {
        // Left the Measure tool — forget any partial / completed ruler.
        measure.clear();
    }

    // --- Aim tool (§2.4): the lighting differentiator. ---
    // While the Aim tool is active and one or more fixtures are selected, a click-drag
    // in the viewport AIMS the selected heads at the world point under the cursor:
    // ray-pick the ground/geometry hit (like Measure), then for each selected fixture
    // solve the pan/tilt that points its beam axis there (`Fixture::aim_pan_tilt`, the
    // inverse of `beam_direction` — it writes the COMMANDED pan/tilt so the slew engine
    // drives the heads, cooperating with cues/motion rather than poking a quaternion).
    // Undo: one step per drag — snapshot on drag-start (`transform_started`), commit on
    // release (`transform_finished`), reusing the modal/gizmo undo pipeline verbatim.
    // Runs BEFORE the modal/orbit/select blocks and consumes the drag so aiming never
    // also orbits or click-selects. Non-fixture selections are left untouched.
    if active_tool == ActiveTool::Aim && transform.is_none() {
        // The selected, in-range fixtures we aim (geometry/screen selections ignored).
        let fids: Vec<usize> =
            selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
        if !consumed && !fids.is_empty() && *viewport_focused {
            // World target under the cursor (ground-plane fallback like Measure), used
            // both to aim and to draw the marker/lines this frame.
            let cursor_target = ui.input(|i| i.pointer.latest_pos()).filter(|p| rect.contains(*p)).and_then(
                |pos| {
                    let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
                    let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
                    let (ro, rd) = camera.ray(ndc, aspect);
                    pick_world_point(scene, ro, rd)
                },
            );
            // A press begins the aim drag → snapshot the undo `before` once.
            if response.drag_started() {
                *transform_started = true;
                aim.target = cursor_target.or(Some(Vec3::ZERO));
            }
            // While dragging, re-aim every selected head at the live target.
            if response.dragged()
                && aim.active()
                && let Some(target) = cursor_target
            {
                aim.target = Some(target);
                for &i in &fids {
                    if let Some((pan, tilt)) = scene.fixtures[i].aim_pan_tilt(target) {
                        scene.fixtures[i].pan = pan;
                        scene.fixtures[i].tilt = tilt;
                    }
                }
            }
            // Release commits the single undo step and ends the drag.
            if aim.active() && (response.drag_stopped() || !ui.input(|i| i.pointer.primary_down())) {
                *transform_finished = true;
                aim.target = None;
            }
            // The aim interaction owns any press/drag this frame (no orbit/select).
            if response.dragged() || response.drag_started() || response.clicked() {
                consumed = true;
            }
        }
        // Draw the aim viz while a drag is in flight: a small target marker at the
        // aimed point + a line from each selected head to it (so the designer sees the
        // throw). Soft amber to distinguish from the RGB gizmos and green ruler.
        if let Some(target) = aim.target {
            let painter = ui.painter_at(rect);
            let vp = camera.view_proj(aspect);
            let accent = egui::Color32::from_rgb(255, 180, 90);
            if let Some(st) = OrbitCamera::project_to_screen(target, vp, rect) {
                // A small crosshair-in-circle target marker.
                painter.circle_stroke(st, 7.0, egui::Stroke::new(2.0, accent));
                painter.line_segment([st - egui::vec2(10.0, 0.0), st + egui::vec2(10.0, 0.0)], egui::Stroke::new(1.5, accent));
                painter.line_segment([st - egui::vec2(0.0, 10.0), st + egui::vec2(0.0, 10.0)], egui::Stroke::new(1.5, accent));
                for &i in &fids {
                    if let Some(sf) = OrbitCamera::project_to_screen(scene.fixtures[i].position, vp, rect) {
                        painter.line_segment([sf, st], egui::Stroke::new(1.5, accent.gamma_multiply(0.7)));
                    }
                }
            }
        }
    } else {
        // Not the Aim tool (or a modal transform owns the viewport) — end any drag.
        aim.target = None;
    }

    // --- Modal transform (Blender G/R/S): when active it OWNS the viewport
    // (mouse drives the transform; orbit/select/zoom are suspended). ---
    if let Some(op) = transform.as_mut() {
        // The MODAL keymap owns the viewport now: X/Y/Z axis lock + Enter/Space
        // confirm + Esc cancel all decode from `poll_modal`, keeping the binds in
        // the one registry (and out of the plain press-keymaps) — no scattered raw
        // key reads here.
        let modal = shortcuts::poll_modal(ui.ctx(), &ov);
        // --- Modal numeric input (Blender editors/util/numinput.cc) ---
        // Typed digits/'.' OVERRIDE the mouse; '-' toggles sign; Backspace edits
        // and, when it empties the buffer, hands control back to the mouse. Read
        // Event::Text for locale-correct digits + accept Key::Period/Comma as '.'
        // (numpad-period) and Key::Minus for the sign toggle. This block lives
        // INSIDE `if let Some(op)` — the modal op owns the viewport, so no text
        // field can be focused (LOCKED DECISION 5 scope guard).
        ui.input(|i| {
            for ev in &i.events {
                if let egui::Event::Text(t) = ev {
                    for c in t.chars() {
                        if c.is_ascii_digit() {
                            if op.num.str.len() < 16 {
                                op.num.str.push(c);
                                op.num.active = true;
                            }
                        } else if c == '.' && !op.num.str.contains('.') {
                            op.num.str.push('.');
                            op.num.active = true;
                        }
                        // '-' is NOT inserted here (handled as a sign toggle below).
                    }
                }
            }
            // Numpad '.' may arrive as a key, not Text — accept Period/Comma too.
            if (i.key_pressed(egui::Key::Period) || i.key_pressed(egui::Key::Comma))
                && !op.num.str.contains('.')
            {
                op.num.str.push('.');
                op.num.active = true;
            }
            // '-' toggles the sign (Blender NUM_NEGATE); it activates numinput too.
            if i.key_pressed(egui::Key::Minus) {
                op.num.sign = !op.num.sign;
                op.num.active = true;
            }
            if i.key_pressed(egui::Key::Backspace) {
                op.num.str.pop();
                if op.num.str.is_empty() && !op.num.sign {
                    op.num.active = false; // empty → mouse takes over again
                }
            }
        });
        for m in &modal {
            let ax = match m {
                shortcuts::ModalAction::ConstrainX => Some(Axis::X),
                shortcuts::ModalAction::ConstrainY => Some(Axis::Y),
                shortcuts::ModalAction::ConstrainZ => Some(Axis::Z),
                _ => None,
            };
            if let Some(ax) = ax {
                op.axis = if op.axis == Some(ax) { None } else { Some(ax) };
            }
        }
        // Esc cancels; right-click cancels; pressing outside the viewport (focus
        // lost) also cancels, so a transform can never get stuck owning it.
        let cancel = modal.contains(&shortcuts::ModalAction::Cancel)
            || response.secondary_clicked()
            || !*viewport_focused;
        // A gizmo drag commits when the pointer is released; a modal G/R/S op
        // commits on a click or Enter/Space (Blender style — via poll_modal).
        let confirm = if op.from_gizmo {
            response.drag_stopped() || !ui.input(|i| i.pointer.primary_down())
        } else {
            modal.contains(&shortcuts::ModalAction::Confirm) || response.clicked()
        };
        if cancel {
            for (i, p0, q0) in &op.start {
                if let Some(f) = scene.fixtures.get_mut(*i) {
                    f.position = *p0;
                    f.orientation = *q0;
                }
            }
            for (i, m0) in &op.geo_start {
                if let Some(g) = scene.geometry.get_mut(*i) {
                    g.transform = *m0;
                }
            }
            for (i, m0) in &op.screen_start {
                if let Some(s) = scene.screens.get_mut(*i) {
                    s.transform = *m0;
                }
            }
            for (i, m0) in &op.pyro_start {
                if let Some(p) = scene.pyro.get_mut(*i) {
                    p.transform = *m0;
                }
            }
            for (i, c0, sz0) in &op.env_start {
                if let Some(e) = scene.environments.get_mut(*i) {
                    e.center = *c0;
                    e.size = *sz0;
                }
            }
            // Drop the live duplicate-array clones — cancelling a Shift+D grab leaves
            // only the base copies at the source (Blender). The base clone was its own
            // undo step (it stands; one undo removes it).
            if op.from_duplicate
                && !op.dup_extra.is_empty()
                && let Some(&kind) = op.dup_base.first()
            {
                truncate_objects(scene, kind, op.dup_extra.len());
            }
            *transform = None;
        } else {
            if let Some(cur) = ui.input(|i| i.pointer.latest_pos()) {
                // #4: snap is `op.snap.on` INVERTED while Ctrl/⌘ is held (Blender's
                // Ctrl-toggles-snap, Unreal's transient grid gate) — resolved live
                // each frame so a mid-drag Ctrl tap flips quantization on/off.
                let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                let snap_on = op.snap.on ^ ctrl;
                apply_transform(op, scene, camera, cur, snap_on);
                // #70: mark the snapped DESTINATION while the Move is live (pre-release)
                // so the user sees where the origin will land. Drawn after the apply, so
                // the primary element's post-snap origin is the marker point.
                if let Some(target) = snap_preview_point(op, scene, snap_on) {
                    draw_snap_marker(&ui.painter_at(rect), target, camera.view_proj(aspect), rect);
                }
            }
            // LIVE duplicate-array: after the base copies moved, (re)build the array
            // clones so the WHOLE array follows the cursor while a count is typed (not
            // just one dragged copy). Skipped on the op's first frame, where the move's
            // `before` undo snapshot is taken before any extras exist.
            if op.from_duplicate && !*transform_started {
                update_dup_array(op, scene);
            }
            // The key cluster (X/Y/Z · type number · Enter/Esc) is read LIVE from
            // the keymap so the pill can never drift from the binds (#23). When an
            // axis is locked the pill tints to that axis's colour so the constraint
            // is unmistakable; otherwise the neutral amber.
            if prefs.show_hint {
                let hint = op.hint(&shortcuts::modal_hint_keys());
                let tint = op
                    .active_axis()
                    .map(|a| {
                        let [r, g, b] = a.color();
                        egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
                    })
                    .unwrap_or(egui::Color32::from_rgb(255, 220, 120));
                theme::overlay_label(
                    &ui.painter_at(rect),
                    rect.center_top() + egui::vec2(0.0, 10.0),
                    egui::Align2::CENTER_TOP,
                    &hint,
                    Some(tint),
                );
            }
            if confirm {
                // The array is already LIVE (rebuilt each frame above); committing just
                // selects the whole result (base copies + array clones) so the user can
                // keep editing them as a unit.
                if op.from_duplicate {
                    let mut all = op.dup_base.clone();
                    all.extend(op.dup_extra.iter().copied());
                    *selection = Selection::from_object_refs(&all);
                }
                *transform = None;
                *transform_finished = true;
            }
        }
    } else if *viewport_focused && selection.has_object() {
        // Start a transform on G / R / S (over any placed object — fixtures,
        // static geometry, LED screens or the environment volume).
        // The binds (and their modifier guards — plain R only, since Shift+R is
        // "Replace") live in the central registry under the Viewport context.
        // Viewport context active, no transform in flight (this is the `else`
        // branch): the keymap stack resolves plain `S` to Scale (masking the Global
        // quick-select) and `R` to Rotate (Shift+R = Replace stays in Global).
        let kind = shortcuts::poll(
            ui.ctx(),
            shortcuts::ActiveContext { viewport_focused: true, transform_active: false },
            &ov,
        )
        .into_iter()
        .find_map(|a| match a {
            shortcuts::Action::Transform(k) => Some(k),
            _ => None,
        });
        if let Some(kind) = kind
            && let Some(cur) = ui.input(|i| i.pointer.latest_pos())
        {
            let fids: Vec<usize> =
                selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
            let gids: Vec<usize> =
                selection.geometry.iter().copied().filter(|&i| i < scene.geometry.len()).collect();
            let sids: Vec<usize> =
                selection.screens.iter().copied().filter(|&i| i < scene.screens.len()).collect();
            let pids: Vec<usize> =
                selection.pyro.iter().copied().filter(|&i| i < scene.pyro.len()).collect();
            let eids: Vec<usize> =
                selection.environment.into_iter().filter(|&i| i < scene.environments.len()).collect();
            let objs = obj_refs(&fids, &gids, &sids, &pids, &eids);
            if !objs.is_empty() {
                // #5: pivot per the chosen mode (Median / Active / 3D-Cursor; the
                // Individual flag makes apply_transform pivot each element about its
                // own origin). Per-element snapshots for the live re-apply / cancel.
                let pivot = compute_pivot(scene, &objs, xform.pivot, *cursor_3d);
                let (start, geo_start, screen_start, pyro_start, env_start) =
                    snapshot_starts(scene, &fids, &gids, &sids, &pids, &eids);
                *transform = Some(TransformOp {
                    kind,
                    axis: None,
                    start_screen: cur,
                    viewport: rect,
                    pivot,
                    start,
                    geo_start,
                    screen_start,
                    pyro_start,
                    env_start,
                    gizmo_hovered_axis: None,
                    gizmo_plane_normal: None,
                    gizmo_view: false,
                    from_gizmo: false,
                    num: NumInput::default(),
                    individual: xform.pivot.is_individual(),
                    snap: xform.snap,
                    orientation: xform.orientation,
                    from_duplicate: false,
                    dup_base: Vec::new(), dup_extra: Vec::new(),
                });
                *transform_started = true;
                consumed = true;
            }
        }
    }

    // --- Box / marquee select (#25) ------------------------------------------
    // ORBIT-vs-BOX RULE (matches Blender's Box-Select tool + UE marquee, adapted
    // to our LMB-orbit nav): under the SELECT tool, a left-drag that BEGAN over
    // EMPTY space (no gizmo handle — Select shows none — and nothing `pick()`s
    // under the press origin) rubber-bands a marquee; a drag that began over an
    // object, or a drag under ANY other tool, stays plain orbit/pan. So orbit is
    // always reachable: switch off the Select tool, or start the drag on an
    // object. The box-active decision is latched on drag-start (egui temp memory,
    // keyed by the viewport id) so mid-drag the cursor leaving empty space can't
    // flip it back to orbit. Modifiers: plain = Replace, Shift = Add, ⌘/Ctrl =
    // Subtract — ONE undo-free selection change committed on release.
    // Latched across the drag's frames in egui temp memory (keyed by viewport id):
    // the marquee anchor (press origin) once box-select has begun. `None` = not
    // currently marqueeing. Stashing the anchor here makes the release computation
    // independent of egui's `press_origin()` lifetime (cleared at button-up).
    let box_anchor_id = ui.id().with("viewport_box_anchor");
    let mut box_anchor: Option<egui::Pos2> = ui.data(|d| d.get_temp(box_anchor_id));
    if !consumed
        && active_tool == ActiveTool::Select
        && transform.is_none()
        && *viewport_focused
    {
        if response.drag_started()
            && let Some(press) = ui.input(|i| i.pointer.press_origin())
        {
            // Box only when the press landed on empty space; an object under the
            // press leaves the drag to orbit (no tweak-move under Select yet).
            let uv = (press - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
            let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let (ro, rd) = camera.ray(ndc, aspect);
            box_anchor = (rect.contains(press) && pick(scene, ro, rd).is_none()).then_some(press);
            ui.data_mut(|d| d.insert_temp(box_anchor_id, box_anchor));
        }
        if let Some(anchor) = box_anchor {
            let cur = ui.input(|i| i.pointer.latest_pos()).unwrap_or(anchor);
            let marquee = egui::Rect::from_two_pos(anchor, cur);
            if response.dragged() {
                // Draw the rubber-band from the anchor to the live cursor.
                let accent = theme::Palette::get(ui).accent;
                let painter = ui.painter_at(rect);
                painter.rect_filled(marquee, 0.0, accent.gamma_multiply(0.10));
                painter.rect_stroke(
                    marquee,
                    0.0,
                    egui::Stroke::new(1.0, accent),
                    egui::StrokeKind::Inside,
                );
                consumed = true; // suppress orbit/pan while marqueeing
            }
            if response.drag_stopped() {
                // Ignore a sub-pixel "drag" (really a click) — the click handler
                // below owns single-pick selection.
                if marquee.width() > 2.0 || marquee.height() > 2.0 {
                    let m = ui.input(|i| i.modifiers);
                    // Modifier → op (UE/CAD): plain = Replace, Shift = Add,
                    // ⌘/Ctrl = Subtract — ONE undo-free selection change.
                    let op = if m.command || m.ctrl {
                        SelectOp::Subtract
                    } else if m.shift {
                        SelectOp::Add
                    } else {
                        SelectOp::Replace
                    };
                    let vp = camera.view_proj(aspect);
                    let hits = marquee_hits(scene, vp, rect, marquee);
                    *selection = apply_select(selection, &hits, op);
                    *scene_anchor = None;
                    consumed = true; // don't also fire click-select this frame
                }
                ui.data_mut(|d| d.remove_temp::<egui::Pos2>(box_anchor_id));
            }
        }
    }

    // --- Navigation axis gizmo (#35) -----------------------------------------
    // Blender's corner orientation gizmo: six labelled axis balls oriented live by
    // the camera (a readout of which way the world axes point) that double as click
    // targets — clicking a ball snaps the camera to look down that axis (eased
    // `set_view`). Drawn top-right with the egui painter (no extra render pass);
    // hover highlights the ball under the pointer. Hit-tested BEFORE orbit so a
    // click on the cluster snaps the view instead of starting an orbit drag.
    // Suppressed (drawn AND click-handled) when the Gizmos overlay is off.
    if prefs.show_gizmos {
        // Cluster centre: top-right corner, inset by its radius (+ a little margin)
        // and tucked below the active-selection label that lives up there.
        // Align the cluster's RIGHT edge with the selection-label pill above it: the
        // pill sits at `rect.right - 10`, and the rightmost BALL reaches
        // `center.x + GIZMO_RADIUS + BALL_RADIUS`, so inset by R + BALL_RADIUS + 10.
        // Dropped further down (R + 46) so the balls clear the pill's bottom.
        let center = rect.right_top()
            + egui::vec2(
                -(nav_gizmo::GIZMO_RADIUS + nav_gizmo::BALL_RADIUS + 10.0),
                nav_gizmo::GIZMO_RADIUS + 46.0,
            );
        let balls = nav_gizmo::balls(camera, center);
        let hover_pos = ui.input(|i| i.pointer.hover_pos());
        let hovered = hover_pos.and_then(|p| nav_gizmo::hit_test(&balls, p));
        let painter = ui.painter_at(rect);
        // Faint backing disc so the cluster reads against any scene.
        painter.circle_filled(center, nav_gizmo::GIZMO_RADIUS + 6.0, egui::Color32::from_black_alpha(70));
        // Draw far balls first so near ones overlap them (painter order = depth).
        let mut order: Vec<usize> = (0..balls.len()).collect();
        order.sort_by(|&a, &b| balls[a].depth.partial_cmp(&balls[b].depth).unwrap_or(std::cmp::Ordering::Equal));
        for &i in &order {
            let b = balls[i];
            let [r, g, bl] = b.color;
            let base = egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (bl * 255.0) as u8);
            let hot = hovered == Some(b.view);
            // Connecting arm from centre to the ball.
            painter.line_segment(
                [center, b.pos],
                egui::Stroke::new(1.0, base.gamma_multiply(if b.depth >= 0.0 { 0.6 } else { 0.3 })),
            );
            if b.positive || hot {
                // Positive (and any hovered) balls are solid + labelled.
                let col = if hot { egui::Color32::WHITE } else { base };
                painter.circle_filled(b.pos, nav_gizmo::BALL_RADIUS, base);
                if hot {
                    painter.circle_stroke(b.pos, nav_gizmo::BALL_RADIUS, egui::Stroke::new(1.5, col));
                }
                painter.text(
                    b.pos,
                    egui::Align2::CENTER_CENTER,
                    b.label,
                    egui::FontId::monospace(10.0),
                    egui::Color32::from_gray(20),
                );
            } else {
                // Negative balls are hollow rings (so the cluster reads as a sphere).
                painter.circle_stroke(b.pos, nav_gizmo::BALL_RADIUS - 1.0, egui::Stroke::new(1.5, base.gamma_multiply(0.8)));
            }
        }
        // A click on a ball snaps the view + consumes the press (no orbit).
        if !consumed
            && response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(view) = nav_gizmo::hit_test(&balls, pos)
        {
            camera.set_view(view);
            consumed = true;
        }
    }

    if !consumed && response.dragged() {
        let delta = response.drag_delta();
        if ui.input(|i| i.modifiers.shift) {
            camera.pan(delta.x, delta.y);
        } else {
            camera.orbit(delta.x, delta.y);
        }
    }
    if !consumed && response.contains_pointer() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            // Zoom-to-cursor: anchor the dolly on the world point under the
            // pointer. Prefer the nearest picked surface; fall back to where the
            // cursor ray meets the ground plane (y=0). If neither resolves (cursor
            // off into the sky), pass None → plain dolly toward the target.
            let aspect = rect.width() / rect.height().max(1.0);
            let anchor = ui.input(|i| i.pointer.hover_pos()).and_then(|pos| {
                let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
                let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
                let (ro, rd) = camera.ray(ndc, aspect);
                pick_world_point(scene, ro, rd)
            });
            camera.zoom(scroll * 0.01, anchor);
        }
    }

    // Click to select: cast a ray through the cursor and pick the nearest object.
    // ⌘/Ctrl-click toggles into a multi-selection; Shift-click range-selects from
    // the anchor (same as the outliner). A drag with Shift pans, so a stationary
    // Shift-click still range-selects.
    if !consumed
        && response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let aspect = rect.width() / rect.height().max(1.0);
        let (ro, rd) = camera.ray(ndc, aspect);
        let m = ui.input(|i| i.modifiers);
        let toggle = m.command || m.ctrl;
        let hit = pick(scene, ro, rd);
        // A click that just FOCUSES the viewport (it wasn't the active pane) on EMPTY
        // space should only focus — never wipe the current selection. Once the
        // viewport is focused, an empty click deselects as usual; a click that hits an
        // object always selects it.
        if hit.is_none() && !was_focused && !toggle && !m.shift {
            // focus-only; leave the selection untouched
        } else if let Some(Hit::Fixture(i)) = hit {
            apply_fixture_click(selection, scene_anchor, i, m.shift, toggle, scene.fixtures.len());
        } else {
            // Modifier → op: plain = replace, ⌘/Ctrl = toggle, Shift = add.
            let op = if toggle {
                SelectOp::Toggle
            } else if m.shift {
                SelectOp::Add
            } else {
                SelectOp::Replace
            };
            let hits: &[SelItem] = match hit {
                Some(Hit::Geometry(i)) => &[SelItem::Geometry(i)],
                Some(Hit::Screen(i)) => &[SelItem::Screen(i)],
                Some(Hit::Pyro(i)) => &[SelItem::Pyro(i)],
                Some(Hit::Environment(i)) => &[SelItem::Environment(i)],
                Some(Hit::Fixture(_)) => unreachable!("fixture handled above"),
                None => &[],
            };
            *selection = apply_select(selection, hits, op);
            *scene_anchor = None;
        }
    }

    // --- 3D cursor place (Shift + right-click, S1-3d-cursor) -----------------
    // Blender's Shift-RMB world cursor: a Shift+right-click drops the 3D cursor onto
    // the ray's world hit (geometry / ground via `pick_world_point`, falling back to a
    // point in front of the camera when the ray escapes to the sky). Handled BEFORE
    // the plain right-click block so it sets the cursor instead of opening the context
    // menu, and it consumes the click so neither the menu nor select fires this frame.
    let shift_rclick = !consumed
        && response.secondary_clicked()
        && ui.input(|i| i.modifiers.shift)
        && *viewport_focused;
    if shift_rclick
        && let Some(pos) = response.interact_pointer_pos()
    {
        let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let (ro, rd) = camera.ray(ndc, aspect);
        // Ground/geometry hit, else a sensible point in front of the camera so the
        // cursor still lands somewhere visible when the ray misses the world.
        let p = pick_world_point(scene, ro, rd).unwrap_or_else(|| ro + rd * camera.distance.max(1.0));
        if p.is_finite() {
            *cursor_3d = p;
            *cursor_3d_set = true;
        }
        consumed = true; // never open the context menu / select on a cursor place
    }

    // Right-click: select the fixture under the cursor (if not already selected),
    // then open a context menu acting on the selection.
    if !consumed {
        if response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let uv = (pos - rect.min) / rect.size().max(egui::vec2(1.0, 1.0));
            let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let aspect = rect.width() / rect.height().max(1.0);
            let (ro, rd) = camera.ray(ndc, aspect);
            match pick(scene, ro, rd) {
                Some(Hit::Fixture(i)) if !selection.contains_fixture(i) => {
                    *selection = Selection::fixture(i);
                    *scene_anchor = Some(i);
                }
                Some(Hit::Geometry(i)) if !selection.contains_geometry(i) => {
                    *selection = Selection::geometry(i);
                    *scene_anchor = None;
                }
                Some(Hit::Screen(i)) if !selection.contains_screen(i) => {
                    *selection = Selection::screen(i);
                    *scene_anchor = None;
                }
                Some(Hit::Pyro(i)) if !selection.contains_pyro(i) => {
                    *selection = Selection::pyro(i);
                    *scene_anchor = None;
                }
                _ => {}
            }
        }
        response.context_menu(|ui| {
            ui.set_min_width(170.0);
            if !selection.geometry.is_empty() {
                // Static-geometry (Objects) selection menu.
                let n = selection.geometry.len();
                ui.label(egui::RichText::new(format!("{n} object{}", if n == 1 { "" } else { "s" })).small().weak());
                if ui.button(format!("{}  Frame selection", theme::icon::FRAME)).clicked() {
                    let mut lo = Vec3::splat(f32::INFINITY);
                    let mut hi = Vec3::splat(f32::NEG_INFINITY);
                    for &i in &selection.geometry {
                        if let Some((l, h)) = scene.geometry.get(i).and_then(|g| g.world_bounds()) {
                            lo = lo.min(l);
                            hi = hi.max(h);
                        }
                    }
                    if lo.is_finite() {
                        camera.frame_aabb(lo, hi);
                    }
                    ui.close();
                }
                if ui.button(format!("{}  Hide", theme::icon::EYE_OFF)).clicked() {
                    for &i in &selection.geometry {
                        if let Some(g) = scene.geometry.get_mut(i) {
                            g.hidden = true;
                        }
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Deselect").clicked() {
                    *selection = Selection::default();
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new(format!("{}  Delete", theme::icon::TRASH)).color(theme::CONFLICT))
                    .clicked()
                {
                    *delete_requested = true;
                    ui.close();
                }
            } else if !selection.screens.is_empty() {
                // LED-screen selection menu.
                let n = selection.screens.len();
                ui.label(egui::RichText::new(format!("{n} screen{}", if n == 1 { "" } else { "s" })).small().weak());
                if ui.button(format!("{}  Frame selection", theme::icon::FRAME)).clicked() {
                    let mut lo = Vec3::splat(f32::INFINITY);
                    let mut hi = Vec3::splat(f32::NEG_INFINITY);
                    for &i in &selection.screens {
                        if let Some(s) = scene.screens.get(i) {
                            let (l, h) = s.world_bounds();
                            lo = lo.min(l);
                            hi = hi.max(h);
                        }
                    }
                    if lo.is_finite() {
                        camera.frame_aabb(lo, hi);
                    }
                    ui.close();
                }
                if ui.button(format!("{}  Hide", theme::icon::EYE_OFF)).clicked() {
                    for &i in &selection.screens {
                        if let Some(s) = scene.screens.get_mut(i) {
                            s.hidden = true;
                        }
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Deselect").clicked() {
                    *selection = Selection::default();
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new(format!("{}  Delete", theme::icon::TRASH)).color(theme::CONFLICT))
                    .clicked()
                {
                    *delete_requested = true;
                    ui.close();
                }
            } else if !selection.pyro.is_empty() {
                // Pyro-device selection menu (mirrors the screens menu).
                let n = selection.pyro.len();
                ui.label(egui::RichText::new(format!("{n} pyro device{}", if n == 1 { "" } else { "s" })).small().weak());
                if ui.button(format!("{}  Frame selection", theme::icon::FRAME)).clicked() {
                    let mut lo = Vec3::splat(f32::INFINITY);
                    let mut hi = Vec3::splat(f32::NEG_INFINITY);
                    for &i in &selection.pyro {
                        if let Some(p) = scene.pyro.get(i) {
                            let (l, h) = p.world_bounds();
                            lo = lo.min(l);
                            hi = hi.max(h);
                        }
                    }
                    if lo.is_finite() {
                        camera.frame_aabb(lo, hi);
                    }
                    ui.close();
                }
                if ui.button(format!("{}  Hide", theme::icon::EYE_OFF)).clicked() {
                    for &i in &selection.pyro {
                        if let Some(p) = scene.pyro.get_mut(i) {
                            p.hidden = true;
                        }
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Deselect").clicked() {
                    *selection = Selection::default();
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new(format!("{}  Delete", theme::icon::TRASH)).color(theme::CONFLICT))
                    .clicked()
                {
                    *delete_requested = true;
                    ui.close();
                }
            } else if selection.fixtures.is_empty() {
                if ui.button(format!("{}  Select all", theme::icon::FIXTURE)).clicked() {
                    selection.fixtures = (0..scene.fixtures.len()).collect();
                    selection.environment = None;
                    selection.geometry.clear();
                    ui.close();
                }
            } else {
                ui.label(
                    egui::RichText::new(format!("{} selected", selection.fixtures.len())).small().weak(),
                );
                if ui.button("Select same type").clicked() {
                    select_same_type(scene, selection);
                    ui.close();
                }
                if ui.button(format!("{}  Frame selection", theme::icon::FRAME)).clicked() {
                    frame_selection(scene, selection, camera);
                    ui.close();
                }
                if duplicate.is_none() && ui.button(format!("{}  Duplicate / Array…", theme::icon::DUPLICATE)).clicked() {
                    if let Some(idx) = selection.primary_fixture() {
                        *duplicate = Some(super::duplicate_dialog_for(ui.ctx(), idx));
                    }
                    ui.close();
                }
                if ui
                    .button(format!("{}  Replace…", theme::icon::RESET))
                    .on_hover_text("Swap these fixtures for another project profile (Shift+R)")
                    .clicked()
                {
                    *replace_requested = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Deselect").clicked() {
                    *selection = Selection::default();
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new(format!("{}  Delete", theme::icon::TRASH)).color(theme::CONFLICT))
                    .clicked()
                {
                    // Committed after the dock so the patch/groups/cues remap too.
                    *delete_requested = true;
                    ui.close();
                }
            }
        });
    }

    // `d` (or Shift+D) opens the Duplicate dialog for the selected fixture. The
    // `!consumed && *viewport_focused` guard below already rules out a live/just-
    // started transform, so the poll asks the Viewport keymap with no modal active.
    let dup_pressed = shortcuts::poll(
        ui.ctx(),
        shortcuts::ActiveContext { viewport_focused: *viewport_focused, transform_active: false },
        &ov,
    )
    .iter()
    .any(|a| matches!(a, shortcuts::Action::Duplicate));
    if !consumed
        && *viewport_focused
        && duplicate.is_none()
        && dup_pressed
        && let Some(idx) = selection.primary_fixture()
    {
        *duplicate = Some(super::duplicate_dialog_for(ui.ctx(), idx));
    }

    // Enter (viewport focused, no live transform / dialog): add the Library tab's
    // highlighted item — pressing Enter in the viewport mirrors pressing it in the
    // Library pane. The modal-transform Enter (G/R/S confirm) is decoded only inside
    // the `Some(op)` branch above, and `transform.is_none()` here guards it, so this
    // can never steal a confirm. The actual add (key→row resolve + undo step) runs in
    // `Ui::show` via `add_requested`.
    if !consumed
        && *viewport_focused
        && transform.is_none()
        && duplicate.is_none()
        && !super::text_focus_active(ui.ctx())
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        *add_requested = true;
    }

    // Focus border.
    if *viewport_focused {
        ui.painter().rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::from_rgb(90, 170, 255)),
            egui::StrokeKind::Inside,
        );
    }

    theme::overlay_label(
        &ui.painter_at(rect),
        rect.left_bottom() + egui::vec2(8.0, -8.0),
        egui::Align2::LEFT_BOTTOM,
        if active_tool == ActiveTool::Measure {
            "measure: click two points for distance · esc: clear · scroll: zoom · shift+drag: pan"
        } else if active_tool == ActiveTool::Aim {
            "aim: drag to point selected head(s) at the cursor · scroll: zoom · shift+drag: pan"
        } else {
            "drag: orbit · shift+drag: pan · scroll: zoom · click: select · shift+rmb: 3d cursor · g/r/s: move/rotate/scale · d: duplicate"
        },
        None,
    );

    // Active selection label (top-right corner, like Blender's active-object
    // header): the primary selected object's name + how many more are selected.
    let sel_text: Option<String> = if let Some(ei) = selection.environment {
        scene.environments.get(ei).map(|e| e.name.clone())
    } else if !selection.geometry.is_empty() {
        let extra = selection.geometry.len().saturating_sub(1);
        selection.primary_geometry().and_then(|i| scene.geometry.get(i)).map(|g| {
            if extra > 0 { format!("{}  +{extra}", g.name) } else { g.name.clone() }
        })
    } else if !selection.screens.is_empty() {
        let extra = selection.screens.len().saturating_sub(1);
        selection.primary_screen().and_then(|i| scene.screens.get(i)).map(|s| {
            if extra > 0 { format!("{}  +{extra}", s.name) } else { s.name.clone() }
        })
    } else if !selection.pyro.is_empty() {
        let extra = selection.pyro.len().saturating_sub(1);
        selection.primary_pyro().and_then(|i| scene.pyro.get(i)).map(|p| {
            if extra > 0 { format!("{}  +{extra}", p.name) } else { p.name.clone() }
        })
    } else if !selection.fixtures.is_empty() {
        let extra = selection.fixtures.len().saturating_sub(1);
        selection.primary_fixture().and_then(|i| scene.fixtures.get(i)).map(|f| {
            if extra > 0 { format!("{}  +{extra}", f.name) } else { f.name.clone() }
        })
    } else {
        None
    };
    if let Some(text) = sel_text {
        // Drawn AFTER the move gizmo (above) so it sits on top; with a centred
        // object the gizmo handles can reach this corner, so the pill gets an
        // OPAQUE rounded background (not the shared translucent overlay) and stays
        // firmly in the top-right with padding — readable, never visually fighting
        // the handles. The gizmo's interaction is untouched (paint-only).
        let painter = ui.painter_at(rect);
        let fg = egui::Color32::from_gray(238);
        let font = egui::FontId::proportional(12.5);
        let galley = painter.layout_no_wrap(text.clone(), font, fg);
        let pad = egui::vec2(9.0, 5.0);
        let size = galley.size() + pad * 2.0;
        let anchor = rect.right_top() + egui::vec2(-10.0, 10.0);
        let mut bg = egui::Align2::RIGHT_TOP.anchor_size(anchor, size);
        // Dodge the move gizmo: if its handles reach the pill's corner, drop the pill
        // to just below them (the gizmo owns the object, so the label yields). Only
        // when there's room below — otherwise the opaque pill stays put + readable.
        if let Some(g) = gizmo_screen {
            const GIZMO_R: f32 = 60.0; // generous on-screen handle reach
            let near = egui::Rect::from_center_size(g, egui::vec2(GIZMO_R * 2.0, GIZMO_R * 2.0));
            let drop_to = g.y + GIZMO_R + 8.0;
            if bg.expand(4.0).intersects(near) && drop_to + size.y < rect.bottom() {
                bg = bg.translate(egui::vec2(0.0, drop_to - bg.top()));
            }
        }
        // Opaque fill + a hairline so it reads as a solid chip over the gizmo.
        painter.rect_filled(bg, 5.0, egui::Color32::from_rgb(26, 27, 31));
        painter.rect_stroke(
            bg,
            5.0,
            egui::Stroke::new(1.0, egui::Color32::from_black_alpha(120)),
            egui::StrokeKind::Inside,
        );
        painter.galley(bg.min + pad, galley, fg);
    }

    // Fixture labels, projected to screen (name / ID / DMX address).
    if prefs.show_labels {
        let aspect = (rect.width() / rect.height().max(1.0)).max(0.0001);
        let vp = camera.view_proj(aspect);
        let painter = ui.painter_at(rect);
        for (i, f) in scene.fixtures.iter().enumerate() {
            let selected = selection.contains_fixture(i);
            if prefs.labels_selected_only && !selected {
                continue;
            }
            // Label just above the fixture body.
            let world = f.position + Vec3::new(0.0, 0.35, 0.0);
            let clip = vp * world.extend(1.0);
            if clip.w <= 0.0 {
                continue; // behind camera
            }
            let ndc = clip.truncate() / clip.w;
            if ndc.x < -1.2 || ndc.x > 1.2 || ndc.y < -1.2 || ndc.y > 1.2 {
                continue;
            }
            let sx = rect.min.x + (ndc.x * 0.5 + 0.5) * rect.width();
            let sy = rect.min.y + (0.5 - ndc.y * 0.5) * rect.height();
            let text = match prefs.label_mode {
                LabelMode::Name => f.name.clone(),
                LabelMode::FixtureId => f
                    .mvr
                    .as_deref()
                    .map(|m| m.fixture_id.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| f.name.clone()),
                LabelMode::Address => f
                    .mvr
                    .as_deref()
                    .and_then(|m| m.addresses.first())
                    .map(|a| format!("{}.{:03}", a.universe(), a.channel()))
                    .unwrap_or_else(|| "—".into()),
            };
            // Selected label takes the accent; others sit quiet over the canvas.
            let col = if selected {
                theme::Palette::get(ui).accent
            } else {
                egui::Color32::from_white_alpha(180)
            };
            painter.text(
                egui::pos2(sx, sy),
                egui::Align2::CENTER_BOTTOM,
                text,
                egui::FontId::proportional(11.0),
                col,
            );
        }
    }

    // FPS HUD (top-left), color-coded off the semantic status tokens.
    if prefs.show_fps {
        let pal = theme::Palette::get(ui);
        let color = if fps >= 55.0 {
            pal.ok
        } else if fps >= 30.0 {
            pal.warn
        } else {
            pal.conflict
        };
        ui.painter().text(
            rect.left_top() + egui::vec2(8.0, 6.0),
            egui::Align2::LEFT_TOP,
            format!("{fps:.0} fps"),
            egui::FontId::monospace(13.0),
            color,
        );
    }

    // Projection + axis-view tag (Blender's top-left "User Perspective" gizmo
    // text). Sits just below the FPS HUD when that's shown, else at the top edge.
    {
        let y = if prefs.show_fps { 24.0 } else { 6.0 };
        ui.painter().text(
            rect.left_top() + egui::vec2(8.0, y),
            egui::Align2::LEFT_TOP,
            camera.view_tag(),
            egui::FontId::monospace(12.0),
            theme::ink(!ui.visuals().dark_mode).tertiary,
        );
    }

    // Scene STATISTICS overlay (Blender's bottom-left stats readout): a quiet,
    // monospace count of what's in the scene + the live selection. Off by default
    // (opt-in via View > Overlays > Statistics); kept faint so it never competes
    // with the canvas. Bottom-left so it clears the top-left FPS/view-tag stack.
    if prefs.show_stats {
        let selected = selection.fixtures.len()
            + selection.geometry.len()
            + selection.screens.len()
            + usize::from(selection.environment.is_some());
        let lines = stats_lines(
            scene.fixtures.len(),
            scene.geometry.len(),
            scene.screens.len(),
            scene.environments.len(),
            selected,
        );
        let painter = ui.painter_at(rect);
        let col = theme::ink(!ui.visuals().dark_mode).tertiary;
        let line_h = 14.0;
        // Anchor from the bottom edge up, so the block grows upward and the first
        // line always sits at a fixed bottom inset regardless of line count.
        let mut y = rect.bottom() - 8.0 - line_h * (lines.len() as f32 - 1.0);
        for line in &lines {
            painter.text(
                egui::pos2(rect.left() + 8.0, y),
                egui::Align2::LEFT_TOP,
                line,
                egui::FontId::monospace(11.0),
                col,
            );
            y += line_h;
        }
    }

    // The display Mode + Exposure controls (and the Grid / Beam-gizmo toggles)
    // now live in the per-editor Viewport HEADER (`ui::editor`), migrated off the
    // old floating "viewport-display-overlay" Area (§2.2). Advanced look settings
    // (bloom/beam/steps) stay in Preferences.
}

/// Build the quiet scene-statistics overlay text lines from the scene counts +
/// the live selection count. A category line is emitted ONLY when its count is
/// non-zero (so an empty scene shows nothing but "0 selected" stays useful), and
/// the trailing "selected" line is always present. Pure (no egui) so the count
/// logic is unit-testable. Order mirrors the outliner: fixtures, objects,
/// screens, environments, then selected.
pub(super) fn stats_lines(
    fixtures: usize,
    objects: usize,
    screens: usize,
    environments: usize,
    selected: usize,
) -> Vec<String> {
    // Singular/plural label for a count (e.g. "1 fixture" / "3 fixtures").
    let row = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut lines = Vec::new();
    if fixtures > 0 {
        lines.push(row(fixtures, "fixture", "fixtures"));
    }
    if objects > 0 {
        lines.push(row(objects, "object", "objects"));
    }
    if screens > 0 {
        lines.push(row(screens, "screen", "screens"));
    }
    if environments > 0 {
        lines.push(row(environments, "environment", "environments"));
    }
    lines.push(format!("{selected} selected"));
    lines
}

/// Bottom tab: Art-Net / sACN connectivity settings + live source status.
pub fn connectivity(
    ui: &mut egui::Ui,
    config: &mut DmxConfig,
    status: &DmxStatus,
    bind_ip_text: &mut String,
    universes_text: &mut String,
    pending: &mut PendingNetCmd,
    running: bool,
) {
    ui.horizontal(|ui| {
        let mut enabled = running;
        if ui
            .checkbox(&mut enabled, "Receive DMX")
            .on_hover_text("Bind the sockets and decode live DMX into the rig (input only)")
            .changed()
        {
            *pending = if enabled { PendingNetCmd::Start } else { PendingNetCmd::Stop };
        }
        if running {
            let bound = match (status.bound_artnet, status.bound_sacn) {
                (true, true) => "Art-Net + sACN",
                (true, false) => "Art-Net",
                (false, true) => "sACN",
                (false, false) => "no sockets bound",
            };
            super::status_dot(ui, theme::OK, &format!("{bound} · {} source(s)", status.sources.len()));
        } else {
            super::status_dot(ui, theme::IDLE, "stopped");
        }
    });
    ui.separator();

    Grid::new("dmx-connect")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Protocols");
            ui.horizontal(|ui| {
                ui.checkbox(&mut config.artnet, "Art-Net");
                ui.checkbox(&mut config.sacn, "sACN");
            });
            ui.end_row();

            ui.label("Bind interface");
            let valid = bind_ip_text.parse::<std::net::IpAddr>();
            let resp = ui.add(
                egui::TextEdit::singleline(bind_ip_text)
                    .desired_width(150.0)
                    .hint_text("0.0.0.0")
                    .text_color_opt(valid.is_err().then_some(theme::CONFLICT)),
            );
            if resp.changed()
                && let Ok(ip) = bind_ip_text.parse::<std::net::IpAddr>()
            {
                config.bind_ip = ip;
            }
            ui.end_row();

            ui.label("sACN universes");
            let resp = ui
                .add(
                    egui::TextEdit::singleline(universes_text)
                        .desired_width(150.0)
                        .hint_text("1,2,5-8"),
                )
                .on_hover_text(
                    "sACN multicast groups to join. Art-Net is broadcast — all \
                     universes are received regardless of this list.",
                );
            if resp.changed() {
                config.universes = crate::dmx::parse_universe_list(universes_text);
            }
            ui.end_row();

            ui.label("Merge");
            ui.horizontal(|ui| {
                for m in MergePolicy::ALL {
                    ui.selectable_value(&mut config.merge, m, m.label());
                }
            });
            ui.end_row();

            ui.label("Art-Net priority");
            ui.add(DragValue::new(&mut config.artnet_priority).range(0..=200));
            ui.end_row();
        });

    ui.horizontal(|ui| {
        if ui
            .add_enabled(running, egui::Button::new("Reapply"))
            .on_hover_text("Re-bind sockets / re-join multicast after a protocol or interface change")
            .clicked()
        {
            *pending = PendingNetCmd::Reapply;
        }
        ui.label(
            RichText::new("Universe/merge edits apply live; protocol/interface need Reapply.")
                .weak()
                .small(),
        );
    });

    ui.add_space(6.0);
    ui.label(RichText::new("SOURCES").small().strong());
    if status.sources.is_empty() {
        let msg = if running {
            "listening — no sources seen yet"
        } else {
            "not receiving"
        };
        ui.label(RichText::new(msg).weak().small());
        return;
    }
    egui::ScrollArea::vertical().max_height(170.0).show(ui, |ui| {
        Grid::new("dmx-sources")
            .num_columns(7)
            .striped(true)
            .spacing([12.0, 3.0])
            .show(ui, |ui| {
                for h in ["Proto", "Source", "Universes", "Prio", "FPS", "Lost", "Seen"] {
                    ui.strong(RichText::new(h).small());
                }
                ui.end_row();
                for s in &status.sources {
                    ui.label(RichText::new(s.proto.label()).small());
                    let name = if s.name.is_empty() {
                        s.label.clone()
                    } else {
                        format!("{} ({})", s.name, s.label)
                    };
                    ui.label(RichText::new(name).small());
                    ui.label(RichText::new(format_universes(&s.universes)).small());
                    ui.label(RichText::new(s.priority.to_string()).small());
                    let fps_col = if s.fps >= 30.0 {
                        theme::OK
                    } else if s.fps >= 10.0 {
                        theme::WARN
                    } else {
                        theme::CONFLICT
                    };
                    ui.colored_label(fps_col, RichText::new(format!("{:.0}", s.fps)).small());
                    ui.label(RichText::new(s.seq_errors.to_string()).small());
                    ui.label(RichText::new(format!("{:.1}s", s.age().as_secs_f32())).small());
                    ui.end_row();
                }
            });
    });
}

/// Bottom tab: the live 512-channel universe grid with patch occupants (replaces
/// the old DMX Monitor stub). Each cell shows the channel number, its live level,
/// and the patched fixture + attribute occupying it.
#[allow(deprecated)] // egui 0.34 show_tooltip_at_pointer — migrated project-wide later
pub fn dmx_universe_grid(
    ui: &mut egui::Ui,
    scene: &Scene,
    patch: &PatchTable,
    snapshot: &UniverseSnapshot,
    selected_universe: &mut u16,
    selection: &mut Selection,
) {
    // Universes present in the snapshot or referenced by the patch.
    let mut universes = patch.universes();
    for &u in snapshot.frames.keys() {
        if !universes.contains(&u) {
            universes.push(u);
        }
    }
    universes.sort_unstable();
    universes.dedup();
    if universes.is_empty() {
        universes.push(*selected_universe);
    }
    if !universes.contains(selected_universe) {
        *selected_universe = universes[0];
    }

    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;
    let u = *selected_universe;
    let live = snapshot.is_live(u, DMX_STALE);
    let nconf = patch.conflicts().len();

    // --- header: title · universe nav · live / conflict status ---
    ui.horizontal(|ui| {
        if ui.button(theme::ico(theme::icon::PREV)).clicked()
            && let Some(pos) = universes.iter().position(|x| x == selected_universe)
        {
            *selected_universe = universes[pos.saturating_sub(1)];
        }
        egui::ComboBox::from_id_salt("dmx-universe-select")
            .selected_text(format!("Universe {selected_universe}"))
            .show_ui(ui, |ui| {
                for &x in &universes {
                    ui.selectable_value(selected_universe, x, format!("Universe {x}"));
                }
            });
        if ui.button(theme::ico(theme::icon::NEXT)).clicked()
            && let Some(pos) = universes.iter().position(|x| x == selected_universe)
        {
            *selected_universe = universes[(pos + 1).min(universes.len() - 1)];
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if nconf > 0 {
                ui.colored_label(
                    theme::CONFLICT,
                    RichText::new(format!("{} {nconf} conflict{}", theme::icon::WARNING, if nconf == 1 { "" } else { "s" })),
                );
            }
            if live {
                let n = snapshot.frames.get(&u).map(|f| f.sources).unwrap_or(0);
                ui.colored_label(theme::OK, format!("• {n} src"));
            } else {
                ui.colored_label(ink.muted, "• idle");
            }
        });
    });

    // Per-channel occupant (fixture index + attribute) for the selected universe,
    // computed once so each of the 512 cells is a cheap lookup.
    let mut occ: Vec<Option<(usize, String)>> = vec![None; 512];
    let mut conflict_cells = [false; 512];
    for (i, fixture) in scene.fixtures.iter().enumerate() {
        let Some(p) = patch.get(i).filter(|p| p.enabled) else { continue };
        if p.universe != u {
            continue;
        }
        for mc in channel_map(fixture, p.mode_index).channels {
            for k in 0..mc.width as u16 {
                let ch = p.address.saturating_sub(1) + mc.offset + k;
                if let Some(slot) = occ.get_mut(ch as usize) {
                    if slot.is_none() {
                        *slot = Some((i, mc.attribute.clone()));
                    } else {
                        conflict_cells[ch as usize] = true;
                    }
                }
            }
        }
    }
    let active = (1..=512u16).filter(|&c| snapshot.level(u, c).unwrap_or(0) > 0).count();
    let patched = occ.iter().filter(|o| o.is_some()).count();

    // --- summary strip ---
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{active}")).monospace().strong().color(if active > 0 { accent } else { ink.muted }));
        ui.label(RichText::new("active").small().color(ink.tertiary));
        ui.add_space(8.0);
        ui.label(RichText::new(format!("{patched}")).monospace().strong().color(ink.secondary));
        ui.label(RichText::new("patched / 512").small().color(ink.tertiary));
    });
    ui.separator();

    let base_patched = ui.visuals().widgets.inactive.bg_fill;
    let base_empty = ui.visuals().extreme_bg_color;
    let border = ui.visuals().widgets.noninteractive.bg_stroke.color;

    const COLS: usize = 16;
    const ROWS: usize = 32;
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let avail = ui.available_width().max(360.0);
        let cell_w = (avail / COLS as f32).clamp(40.0, 96.0);
        let cell_h = 30.0;
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(cell_w * COLS as f32, cell_h * ROWS as f32),
            Sense::click(),
        );
        let painter = ui.painter_at(rect);
        for r in 0..ROWS {
            for c in 0..COLS {
                let ch = r * COLS + c; // 0-based channel index
                let cell = egui::Rect::from_min_size(
                    rect.min + egui::vec2(c as f32 * cell_w, r as f32 * cell_h),
                    egui::vec2(cell_w - 1.0, cell_h - 1.0),
                );
                let level = snapshot.level(u, (ch + 1) as u16).unwrap_or(0);
                let occupied = occ[ch].as_ref();
                let selected = occupied.is_some_and(|(fi, _)| selection.contains_fixture(*fi));
                let tint = occupied.map(|(fi, _)| fixture_tint(*fi)).unwrap_or(accent);

                // Base + a value-fill bar rising from the bottom (∝ level).
                painter.rect_filled(cell, 3.0, if occupied.is_some() { base_patched } else { base_empty });
                if level > 0 {
                    let frac = level as f32 / 255.0;
                    let fill = egui::Rect::from_min_max(
                        egui::pos2(cell.left(), cell.bottom() - cell.height() * frac),
                        cell.right_bottom(),
                    );
                    painter.rect_filled(fill, 0.0, tint.gamma_multiply(0.22 + 0.55 * frac));
                }
                // Fixture-identity stripe down the left edge.
                if occupied.is_some() {
                    painter.rect_filled(
                        egui::Rect::from_min_max(cell.left_top(), egui::pos2(cell.left() + 2.5, cell.bottom())),
                        0.0,
                        tint,
                    );
                }
                // Border / conflict / selection ring.
                painter.rect_stroke(cell, 3.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
                if conflict_cells[ch] {
                    painter.rect_stroke(cell, 3.0, egui::Stroke::new(1.5, theme::CONFLICT), egui::StrokeKind::Inside);
                } else if selected {
                    painter.rect_stroke(cell, 3.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
                }
                // Channel number (top-left) + value % (bottom-right), tabular.
                painter.text(
                    cell.left_top() + egui::vec2(4.0, 2.0),
                    egui::Align2::LEFT_TOP,
                    (ch + 1).to_string(),
                    egui::FontId::monospace(9.0),
                    ink.muted,
                );
                if level > 0 {
                    let pct = (level as f32 / 255.0 * 100.0).round() as u32;
                    painter.text(
                        cell.right_bottom() + egui::vec2(-4.0, -2.0),
                        egui::Align2::RIGHT_BOTTOM,
                        format!("{pct}"),
                        egui::FontId::monospace(11.5),
                        ink.primary,
                    );
                }
            }
        }
        // Hover tooltip with the channel's full occupant + value.
        if let Some(pos) = resp.hover_pos() {
            let rel = pos - rect.min;
            let (c, r) = ((rel.x / cell_w) as usize, (rel.y / cell_h) as usize);
            if c < COLS && r < ROWS {
                let ch = r * COLS + c;
                let level = snapshot.level(u, (ch + 1) as u16).unwrap_or(0);
                let pct = (level as f32 / 255.0 * 100.0).round() as u32;
                let detail = match &occ[ch] {
                    Some((fi, attr)) => {
                        let name = scene.fixtures[*fi].name.clone();
                        format!("Ch {} · {name} · {attr}\n{level}  ({pct}%)", ch + 1)
                    }
                    None => format!("Ch {} · unpatched\n{level}  ({pct}%)", ch + 1),
                };
                egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new("dmx-cell-tip"), |ui| {
                    ui.label(detail);
                });
            }
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let rel = pos - rect.min;
            let (c, r) = ((rel.x / cell_w) as usize, (rel.y / cell_h) as usize);
            // Select from the same occupancy map the grid is painted/hovered from
            // (so a click agrees with the cell's shown identity, including gaps).
            if c < COLS && r < ROWS
                && let Some((fi, _)) = &occ[r * COLS + c]
            {
                *selection = Selection::fixture(*fi);
            }
        }
    });
}

/// A stable identity colour for a fixture index — golden-ratio hue spacing so
/// adjacent fixtures stay visually distinct in the DMX grid.
fn fixture_tint(i: usize) -> Color32 {
    let h = (i as f32 * 0.618_034).fract();
    hsv_to_color(h, 0.55, 0.95)
}

fn hsv_to_color(h: f32, s: f32, v: f32) -> Color32 {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Fixture-manager panel state: text filter, sort, and quick filters. Sort reuses
/// the scene outliner's [`SceneSort`] (Patch / Name / Type).
pub struct FmState {
    pub search: String,
    pub sort: SceneSort,
    pub conflicts_only: bool,
    pub unpatched_only: bool,
    pub bulk_universe: u16,
    pub bulk_address: u16,
}

impl Default for FmState {
    fn default() -> Self {
        Self {
            search: String::new(),
            sort: SceneSort::Patch,
            conflicts_only: false,
            unpatched_only: false,
            bulk_universe: 1,
            bulk_address: 1,
        }
    }
}

/// Bottom tab: the **Fixture Manager** — a data-dense, sortable, filterable table
/// of every fixture with multi-select (synced to the 3D/Inspector selection) and
/// bulk patch editing. Replaces the old one-row-at-a-time patch editor.
pub fn fixture_manager(
    ui: &mut egui::Ui,
    scene: &mut Scene,
    patch: &mut PatchTable,
    selection: &mut Selection,
    anchor: &mut Option<usize>,
    fm: &mut FmState,
) {
    use theme::icon;
    let ink = theme::ink(!ui.visuals().dark_mode);
    let accent = ui.visuals().selection.stroke.color;

    let mut conflicted = vec![false; scene.fixtures.len()];
    for c in patch.conflicts() {
        if let Some(s) = conflicted.get_mut(c.a) {
            *s = true;
        }
        if let Some(s) = conflicted.get_mut(c.b) {
            *s = true;
        }
    }
    let nconf = conflicted.iter().filter(|&&c| c).count();

    // --- header: title + selection count + reset ---
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}  Fixtures", icon::FIXTURE)).heading());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(theme::ico(icon::RESET)).on_hover_text("Reset addresses to the import (MVR/GDTF), discarding manual edits").clicked() {
                patch.reconcile_from_scene(scene);
            }
            let nsel = selection.fixtures.len();
            if nsel > 0 {
                ui.label(RichText::new(format!("{nsel} selected")).small().color(accent));
            }
        });
    });

    // --- filter row: search + sort + quick filters ---
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::ico(icon::SEARCH).weak());
        ui.add(egui::TextEdit::singleline(&mut fm.search).hint_text("Filter…").desired_width(120.0));
        ui.separator();
        ui.label(theme::ico(icon::SORT).weak());
        for s in [SceneSort::Patch, SceneSort::Name, SceneSort::Type, SceneSort::Sequence] {
            ui.selectable_value(&mut fm.sort, s, s.label());
        }
        ui.separator();
        // Renumber the selected fixtures' sequence by stage position (rows then
        // columns); with nothing selected, renumbers every fixture.
        if ui
            .button(format!("{}  Renumber", icon::SORT))
            .on_hover_text("Renumber sequence by position — rows (top→bottom) then columns (left→right). Selected only, or all if none selected.")
            .clicked()
        {
            let sel: Vec<usize> = selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
            scene.renumber_sequences_by_position(&sel);
        }
        ui.separator();
        ui.toggle_value(&mut fm.conflicts_only, format!("{} {nconf}", icon::WARNING))
            .on_hover_text("Show only fixtures with an address conflict");
        ui.toggle_value(&mut fm.unpatched_only, "unpatched").on_hover_text("Show only unpatched fixtures");
    });

    // --- bulk toolbar (only when fixtures are selected) ---
    let sel: Vec<usize> = selection.fixtures.iter().copied().filter(|&i| i < scene.fixtures.len()).collect();
    if !sel.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("Bulk · {}", sel.len())).small().strong().color(accent));
            ui.add(DragValue::new(&mut fm.bulk_universe).range(1..=63999).prefix("U "));
            ui.add(DragValue::new(&mut fm.bulk_address).range(1..=512).prefix("@ "));
            if ui.button("Patch seq").on_hover_text("Assign the selected fixtures sequentially from U.@ by footprint, in the order shown").clicked() {
                // Assign in the VISIBLE (sorted) order, not raw selection order, so
                // the sequence matches what the user sees.
                let seq: Vec<usize> =
                    fixture_order(scene, patch, fm.sort).into_iter().filter(|i| sel.contains(i)).collect();
                let (mut u, mut a) = (fm.bulk_universe.max(1), fm.bulk_address.clamp(1, 512));
                for &i in &seq {
                    let fp = patch.get(i).map(|p| p.footprint).unwrap_or(1).clamp(1, 512);
                    if a as u32 + fp as u32 - 1 > 512 {
                        u = (u + 1).min(63999); // next universe (clamped, no u16 wrap)
                        a = 1;
                    }
                    if let Some(p) = patch.get_mut(i) {
                        p.universe = u;
                        p.address = a;
                        p.enabled = true;
                        p.source = PatchSource::Manual;
                    }
                    a += fp;
                }
            }
            if ui.button("Set U").on_hover_text("Set the universe of all selected").clicked() {
                for &i in &sel {
                    if let Some(p) = patch.get_mut(i) {
                        p.universe = fm.bulk_universe;
                        p.enabled = true;
                        p.source = PatchSource::Manual;
                    }
                }
            }
            if ui.button("Enable").clicked() {
                for &i in &sel {
                    if let Some(p) = patch.get_mut(i) {
                        p.enabled = true;
                    }
                }
            }
            if ui.button("Disable").clicked() {
                for &i in &sel {
                    if let Some(p) = patch.get_mut(i) {
                        p.enabled = false;
                    }
                }
            }
            // Bulk DMX mode — only when every selected fixture is the same profile
            // (so one mode list applies to all). Drives the patch footprint; decode
            // syncs each fixture's active mode from the patch.
            let p0 = scene.fixtures[sel[0]].profile.clone();
            let same_profile = sel.iter().all(|&i| scene.fixtures[i].profile == p0);
            let ref_modes: Vec<String> = if same_profile {
                scene.fixtures[sel[0]]
                    .gdtf
                    .as_ref()
                    .map(|g| g.modes.iter().map(|m| m.name.clone()).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if !ref_modes.is_empty() {
                let cur = patch.get(sel[0]).map(|p| p.mode_index).unwrap_or(0);
                let cur_name = ref_modes.get(cur).cloned().unwrap_or_default();
                let mut pick = None;
                egui::ComboBox::from_id_salt("fm-bulk-mode")
                    .selected_text(RichText::new(format!("Mode: {cur_name}")).small())
                    .show_ui(ui, |ui| {
                        for (mi, name) in ref_modes.iter().enumerate() {
                            if ui.selectable_label(mi == cur, name).clicked() {
                                pick = Some(mi);
                            }
                        }
                    });
                if let Some(mi) = pick {
                    for &i in &sel {
                        let f = &scene.fixtures[i];
                        if f.gdtf.as_ref().is_some_and(|g| mi < g.modes.len()) {
                            patch.set_mode(f, i, mi);
                        }
                    }
                }
            }
        });
    }
    ui.separator();

    if scene.fixtures.is_empty() {
        ui.label(RichText::new("No fixtures — add from the Library or import an MVR.").weak().small());
        return;
    }

    // --- display order: sort then filter ---
    let q = fm.search.trim().to_lowercase();
    let order: Vec<usize> = fixture_order(scene, patch, fm.sort)
        .into_iter()
        .filter(|&i| {
            let f = &scene.fixtures[i];
            if !q.is_empty() && !f.name.to_lowercase().contains(&q) && !f.profile.to_lowercase().contains(&q) {
                return false;
            }
            if fm.conflicts_only && !conflicted[i] {
                return false;
            }
            if fm.unpatched_only && patch.get(i).is_some_and(|p| p.enabled) {
                return false;
            }
            true
        })
        .collect();

    let mut click: Option<(usize, bool, bool)> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        Grid::new("fixtures-grid").num_columns(8).striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
            for h in ["Seq", "Fixture", "Type", "Univ", "Addr", "Mode", "Ch", ""] {
                ui.strong(RichText::new(h).small().color(ink.tertiary));
            }
            ui.end_row();

            for &i in &order {
                // Seq cell (editable): the user-facing sequence/channel number, first
                // (console-style). Mutates the fixture, so it precedes the immutable
                // borrow used for the rest of the row.
                ui.add(DragValue::new(&mut scene.fixtures[i].sequence).range(1..=u32::MAX).speed(0.2));
                let fixture = &scene.fixtures[i];
                // Name cell: selects the row (syncs the 3D/Inspector selection);
                // shift = range, ⌘/Ctrl = toggle.
                let selected = selection.contains_fixture(i);
                let resp = ui.selectable_label(selected, RichText::new(fixture.name.as_str()).small());
                if resp.clicked() {
                    let m = ui.input(|x| x.modifiers);
                    click = Some((i, m.shift, m.command || m.ctrl));
                }
                ui.label(RichText::new(fixture.profile.as_str()).weak().small());

                // Universe / address.
                if let Some(p) = patch.get_mut(i) {
                    let mut edited = ui.add(DragValue::new(&mut p.universe).range(1..=63999).speed(0.1)).changed();
                    edited |= ui.add(DragValue::new(&mut p.address).range(1..=512).speed(0.2)).changed();
                    if edited {
                        p.enabled = true;
                        p.source = PatchSource::Manual;
                    }
                } else {
                    ui.label("");
                    ui.label("");
                }

                // Mode selector (GDTF modes; plain fixtures are synthetic).
                let mut new_mode = None;
                match fixture.gdtf.as_ref() {
                    Some(gdtf) if !gdtf.modes.is_empty() => {
                        let cur = patch.get(i).map(|p| p.mode_index).unwrap_or(0);
                        let cur_name = gdtf.modes.get(cur).map(|m| m.name.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt(("fm-mode", i))
                            .selected_text(RichText::new(cur_name).small())
                            .show_ui(ui, |ui| {
                                for (mi, m) in gdtf.modes.iter().enumerate() {
                                    if ui.selectable_label(mi == cur, &m.name).clicked() {
                                        new_mode = Some(mi);
                                    }
                                }
                            });
                    }
                    _ => {
                        ui.label(RichText::new("—").weak().small());
                    }
                }
                if let Some(mi) = new_mode {
                    patch.set_mode(fixture, i, mi);
                }

                ui.label(RichText::new(patch.get(i).map(|p| p.footprint.to_string()).unwrap_or_default()).small());
                if conflicted[i] {
                    ui.colored_label(theme::CONFLICT, theme::icon::WARNING).on_hover_text("Address conflict");
                } else if patch.get(i).is_some_and(|p| !p.enabled) {
                    ui.label(RichText::new("off").weak().small());
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
    });
    if let Some((i, shift, toggle)) = click {
        if shift {
            // Re-anchor if the shared anchor is stale (deleted, or filtered out of
            // the visible list) so the range can't span to a phantom row.
            if anchor.map_or(true, |a| !order.contains(&a)) {
                *anchor = Some(i);
            }
            let cpos = order.iter().position(|&x| x == i).unwrap_or(0);
            let apos = order.iter().position(|&x| Some(x) == *anchor).unwrap_or(cpos);
            let (lo, hi) = (apos.min(cpos), apos.max(cpos));
            selection.fixtures = order[lo..=hi].to_vec();
            selection.environment = None;
        } else {
            apply_fixture_click(selection, anchor, i, false, toggle, scene.fixtures.len());
        }
    }
}

/// Compact a sorted universe list for the source table (e.g. `1,2,5`).
fn format_universes(us: &[u16]) -> String {
    us.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",")
}

/// What a viewport ray hit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Fixture(usize),
    Screen(usize),
    Pyro(usize),
    Geometry(usize),
    Environment(usize),
}

/// Shortest distance from a screen-space point `p` to the segment `a`..`b`. Used by
/// the gizmo groups to hit-test their projected axis handles / rings.
pub(super) fn dist_point_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 <= f32::EPSILON {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    let proj = a + ab * t;
    (p - proj).length()
}

/// Pick the object a world-space ray hits. Priority: **fixtures** (so you can
/// always click a head even when it sits inside set geometry or the fog box),
/// then **static geometry** (its world AABB), then the **environment** volumes.
fn pick(scene: &Scene, ro: Vec3, rd: Vec3) -> Option<Hit> {
    let mut best: Option<(f32, usize)> = None;
    for (i, f) in scene.fixtures.iter().enumerate() {
        if f.hidden {
            continue;
        }
        // Bounding sphere around the head; a bit generous so it's easy to click.
        if let Some(t) = ray_sphere(ro, rd, f.position, 0.5)
            && best.is_none_or(|(bt, _)| t < bt)
        {
            best = Some((t, i));
        }
    }
    if let Some((_, i)) = best {
        return Some(Hit::Fixture(i));
    }
    // LED screens: ray vs each oriented surface quad (cheaper + tighter than an AABB).
    let mut scr: Option<(f32, usize)> = None;
    for (i, s) in scene.screens.iter().enumerate() {
        if s.hidden {
            continue;
        }
        if let Some(t) = s.ray_hit(ro, rd)
            && scr.is_none_or(|(bt, _)| t < bt)
        {
            scr = Some((t, i));
        }
    }
    if let Some((_, i)) = scr {
        return Some(Hit::Screen(i));
    }
    // Pyro devices: ray vs a small body box at the nozzle (so they're clickable).
    let mut pyro: Option<(f32, usize)> = None;
    for (i, d) in scene.pyro.iter().enumerate() {
        if d.hidden {
            continue;
        }
        if let Some(t) = d.ray_hit(ro, rd)
            && pyro.is_none_or(|(bt, _)| t < bt)
        {
            pyro = Some((t, i));
        }
    }
    if let Some((_, i)) = pyro {
        return Some(Hit::Pyro(i));
    }
    // Static geometry: ray vs each object's world-space AABB.
    let mut geo: Option<(f32, usize)> = None;
    for (i, g) in scene.geometry.iter().enumerate() {
        if g.hidden {
            continue;
        }
        if let Some((lo, hi)) = g.world_bounds()
            && let Some(t) = ray_aabb(ro, rd, lo, hi)
            && geo.is_none_or(|(bt, _)| t < bt)
        {
            geo = Some((t, i));
        }
    }
    if let Some((_, i)) = geo {
        return Some(Hit::Geometry(i));
    }
    let mut env: Option<(f32, usize)> = None;
    for (i, e) in scene.environments.iter().enumerate() {
        if e.hidden {
            continue; // outliner eye: a hidden fog box isn't pickable
        }
        if let Some(t) = ray_aabb(ro, rd, e.min(), e.max())
            && env.is_none_or(|(bt, _)| t < bt)
        {
            env = Some((t, i));
        }
    }
    env.map(|(_, i)| Hit::Environment(i))
}

/// Gather every visible fixture / geometry object / LED screen whose
/// screen-projected anchor point falls inside the marquee `marquee` (#25). The
/// rule is **loose** (Blender's default: an object is hit if its *centre* lands
/// in the rect — fixtures use their origin, geometry/screens their world-bounds
/// centre), which is forgiving for the many tiny fixture dots in a rig. Hidden
/// and behind-camera entities are skipped (`project_to_screen` returns `None`
/// when `w <= 0`). Environments are excluded — they're single-only volumes the
/// marquee shouldn't sweep up. Pure given `vp`/`rect`, so it's unit-testable.
fn marquee_hits(scene: &Scene, vp: glam::Mat4, rect: egui::Rect, marquee: egui::Rect) -> Vec<SelItem> {
    let mut hits = Vec::new();
    let inside = |p: Vec3| {
        OrbitCamera::project_to_screen(p, vp, rect)
            .is_some_and(|s| marquee.contains(s))
    };
    for (i, f) in scene.fixtures.iter().enumerate() {
        if !f.hidden && inside(f.position) {
            hits.push(SelItem::Fixture(i));
        }
    }
    for (i, g) in scene.geometry.iter().enumerate() {
        let c = g
            .world_bounds()
            .map(|(lo, hi)| (lo + hi) * 0.5)
            .unwrap_or_else(|| g.transform.w_axis.truncate());
        if !g.hidden && inside(c) {
            hits.push(SelItem::Geometry(i));
        }
    }
    for (i, s) in scene.screens.iter().enumerate() {
        if !s.hidden && inside(s.world_center()) {
            hits.push(SelItem::Screen(i));
        }
    }
    for (i, d) in scene.pyro.iter().enumerate() {
        if !d.hidden && inside(d.world_nozzle()) {
            hits.push(SelItem::Pyro(i));
        }
    }
    hits
}

/// Nearest positive ray–sphere intersection distance, if any.
fn ray_sphere(ro: Vec3, rd: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let oc = ro - center;
    let b = oc.dot(rd);
    let c = oc.dot(oc) - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let t = -b - s;
    if t > 0.0 {
        Some(t)
    } else {
        let t2 = -b + s;
        (t2 > 0.0).then_some(t2)
    }
}

/// Nearest positive ray–AABB intersection distance (slab test), if any.
fn ray_aabb(ro: Vec3, rd: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = rd.recip(); // inf for parallel components is fine
    let t0 = (min - ro) * inv;
    let t1 = (max - ro) * inv;
    let tmin = t0.min(t1);
    let tmax = t0.max(t1);
    let near = tmin.x.max(tmin.y).max(tmin.z);
    let far = tmax.x.min(tmax.y).min(tmax.z);
    if far < near.max(0.0) {
        return None;
    }
    Some(if near > 0.0 { near } else { far })
}


#[cfg(test)]
mod pick_tests {
    use super::*;
    use crate::ui::PivotMode;

    #[test]
    fn ray_sphere_front_and_back() {
        let ro = Vec3::new(0.0, 0.0, -5.0);
        let rd = Vec3::new(0.0, 0.0, 1.0);
        let t = ray_sphere(ro, rd, Vec3::ZERO, 1.0).expect("hit");
        assert!((t - 4.0).abs() < 1e-3);
        // Sphere behind the ray origin: no hit.
        assert!(ray_sphere(Vec3::new(0.0, 0.0, 5.0), rd, Vec3::ZERO, 1.0).is_none());
        // Ray missing the sphere sideways.
        assert!(ray_sphere(ro, rd, Vec3::new(3.0, 0.0, 0.0), 1.0).is_none());
    }

    #[test]
    fn ray_aabb_hit() {
        let t = ray_aabb(
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::splat(-1.0),
            Vec3::splat(1.0),
        )
        .expect("hit");
        assert!((t - 4.0).abs() < 1e-3);
    }

    #[test]
    fn pick_prefers_fixture_over_fog_box() {
        // Demo scene: one fixture at (0,4,0) inside a large fog box.
        let scene = Scene::demo();
        let f = scene.fixtures[0].position;
        // Ray from in front of the fixture, aimed at it.
        let ro = f + Vec3::new(0.0, 0.0, 6.0);
        let rd = (f - ro).normalize();
        assert_eq!(pick(&scene, ro, rd), Some(Hit::Fixture(0)));
    }

    #[test]
    fn marquee_selects_projected_in_rect() {
        // Project a fixture to screen with the default camera, draw a marquee
        // around its projected dot, and assert it's caught (#25). A marquee in the
        // opposite corner must NOT catch it (loose centre-in-rect rule).
        let scene = Scene::demo(); // one fixture at (0,4,0)
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0));
        let aspect = rect.width() / rect.height();
        let vp = cam.view_proj(aspect);
        let dot = OrbitCamera::project_to_screen(scene.fixtures[0].position, vp, rect)
            .expect("fixture projects in front of the camera");

        // A 40px box centred on the dot encloses its centre → hit.
        let around = egui::Rect::from_center_size(dot, egui::vec2(40.0, 40.0));
        let hits = marquee_hits(&scene, vp, rect, around);
        assert!(hits.contains(&SelItem::Fixture(0)), "dot-in-rect → selected");

        // A tiny box far from the dot → no hit.
        let far = egui::Rect::from_min_size(
            dot + egui::vec2(300.0, 300.0),
            egui::vec2(8.0, 8.0),
        );
        let none = marquee_hits(&scene, vp, rect, far);
        assert!(!none.contains(&SelItem::Fixture(0)), "off-dot rect → not selected");

        // A hidden fixture is skipped even when its dot is inside the marquee.
        let mut hidden = Scene::demo();
        hidden.fixtures[0].hidden = true;
        assert!(
            marquee_hits(&hidden, vp, rect, around).is_empty(),
            "hidden fixtures are not marquee-pickable"
        );
    }

    #[test]
    fn measure_point_falls_back_to_ground_plane() {
        // An empty scene: a downward ray from y=10 must land on y=0 (the floor).
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let ro = Vec3::new(2.0, 10.0, 3.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let p = pick_world_point(&scene, ro, rd).expect("ground hit");
        assert!((p.y - 0.0).abs() < 1e-3, "expected y=0, got {}", p.y);
        assert!((p.x - 2.0).abs() < 1e-3 && (p.z - 3.0).abs() < 1e-3);
    }

    #[test]
    fn placement_point_lands_on_ground_when_looking_down() {
        // A camera angled down at an empty scene: the centre ray hits the y=0
        // floor (#19 — place at the ground hit, not the origin).
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let mut cam = OrbitCamera::default();
        cam.set_view(crate::renderer::camera::CameraView::Top); // straight down
        cam.set_aspect(16.0 / 9.0);
        let p = placement_point(&scene, &cam);
        assert!(p.y.abs() < 1e-2, "expected a ground (y≈0) hit, got y={}", p.y);
    }

    #[test]
    fn placement_point_falls_back_in_front_when_ray_misses_ground() {
        // A camera tilted UP so the centre ray never meets the floor: placement
        // must fall back to a finite point in front of the camera, not panic/origin.
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let mut cam = OrbitCamera::default();
        cam.pitch = -1.2; // look upward, away from the floor
        cam.set_aspect(16.0 / 9.0);
        let p = placement_point(&scene, &cam);
        assert!(p.is_finite(), "placement must be finite, got {p:?}");
    }

    #[test]
    fn measure_point_prefers_nearer_surface_over_ground() {
        // A fixture sphere between the camera and the floor wins over the y=0 plane.
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let demo = Scene::demo();
        scene.fixtures.push(demo.fixtures[0].clone());
        scene.fixtures[0].position = Vec3::new(0.0, 4.0, 0.0);
        scene.fixtures[0].hidden = false;
        let ro = Vec3::new(0.0, 10.0, 0.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let p = pick_world_point(&scene, ro, rd).expect("hit");
        // Should hit the top of the fixture sphere (~y=4.5), not the floor (y=0).
        assert!(p.y > 3.0, "expected fixture hit near y=4.5, got {}", p.y);
    }

    /// S1-3d-cursor: a Shift+RMB place resolves the cursor to the ray's world hit.
    /// This mirrors the viewport's placement expression (pick_world_point, else a
    /// point in front of the camera) so the interactive wiring's math is pinned.
    #[test]
    fn shift_rclick_sets_cursor_to_ground_hit() {
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        // Downward ray from above empty space → the y=0 floor under (5, 0, -2).
        let ro = Vec3::new(5.0, 8.0, -2.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let dist = 12.0_f32;
        let p = pick_world_point(&scene, ro, rd).unwrap_or_else(|| ro + rd * dist.max(1.0));
        assert!((p.y).abs() < 1e-3, "cursor lands on the floor, got y={}", p.y);
        assert!((p.x - 5.0).abs() < 1e-3 && (p.z + 2.0).abs() < 1e-3);
    }

    /// S1-3d-cursor: when the ray escapes to the sky (no hit) the cursor falls back to
    /// a finite point in front of the camera, never NaN/origin.
    #[test]
    fn shift_rclick_cursor_falls_back_in_front() {
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        // Upward ray that never meets the y=0 plane.
        let ro = Vec3::new(0.0, 2.0, 0.0);
        let rd = Vec3::new(0.0, 1.0, 0.0);
        let dist = 10.0_f32;
        let p = pick_world_point(&scene, ro, rd).unwrap_or_else(|| ro + rd * dist.max(1.0));
        assert!(p.is_finite(), "fallback cursor must be finite, got {p:?}");
        assert!(p.y > 2.0, "fallback is in front of the camera, got {p:?}");
    }

    /// S1-3d-cursor: the Cursor3d pivot mode reads the supplied cursor point verbatim
    /// (so a Cursor-pivot rotate/scale spins/grows about it), and is independent of
    /// the selection's own centroid.
    #[test]
    fn pivot_cursor3d_uses_the_cursor_point() {
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        let demo = Scene::demo();
        scene.fixtures.push(demo.fixtures[0].clone());
        scene.fixtures[0].position = Vec3::new(10.0, 0.0, 10.0);
        let cursor = Vec3::new(-3.0, 1.5, 4.0);
        let pivot = compute_pivot(&scene, &[ObjectRef::Fixture(0)], PivotMode::Cursor3d, cursor);
        assert_eq!(pivot, cursor, "Cursor3d pivot ignores selection, uses the cursor");
        // The Median pivot, by contrast, is the fixture's own position.
        let median = compute_pivot(&scene, &[ObjectRef::Fixture(0)], PivotMode::Median, cursor);
        assert!((median - Vec3::new(10.0, 0.0, 10.0)).length() < 1.0, "median ≠ cursor");
    }

    /// S1-viewport-overlays: the stats overlay emits a line per non-empty category
    /// (in outliner order) plus an always-present selected line, with correct
    /// singular/plural agreement and live counts.
    #[test]
    fn stats_lines_counts_and_pluralise() {
        // Mixed scene: a singular, a plural, a zero (skipped) and selection.
        let lines = stats_lines(1, 3, 0, 2, 4);
        assert_eq!(
            lines,
            vec![
                "1 fixture".to_string(),   // singular
                "3 objects".to_string(),   // plural
                // screens == 0 → skipped entirely
                "2 environments".to_string(),
                "4 selected".to_string(), // always present, last
            ]
        );
    }

    /// An empty scene shows only the "0 selected" line (every category zero ⇒
    /// skipped), so the overlay never adds noise to a blank canvas.
    #[test]
    fn stats_lines_empty_scene_is_just_selected() {
        assert_eq!(stats_lines(0, 0, 0, 0, 0), vec!["0 selected".to_string()]);
    }

    /// One of everything reads in the singular and keeps outliner order.
    #[test]
    fn stats_lines_singular_order() {
        assert_eq!(
            stats_lines(1, 1, 1, 1, 1),
            vec![
                "1 fixture".to_string(),
                "1 object".to_string(),
                "1 screen".to_string(),
                "1 environment".to_string(),
                "1 selected".to_string(),
            ]
        );
    }
}
