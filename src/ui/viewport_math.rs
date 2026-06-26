//! Pure viewport-interaction math: selection-frame helpers, the transform pivot
//! / orientation basis, ray↔axis / ray↔plane intersections, and the snap-target
//! preview + marker. Extracted from [`super::panels`] so the viewport tab + the
//! modal-transform applier (both still in `panels`) call these as `pub(super)`
//! helpers. No egui-panel UI here — only geometry the `viewport` / `apply_transform`
//! blocks consume.

use glam::{Mat4, Quat, Vec3};

use super::{PivotMode, TransformKind, TransformOp};
use crate::renderer::camera::OrbitCamera;
use crate::scene::{ObjectRef, Scene, Selection};

/// Extend the selection to every fixture sharing a profile with the current
/// selection ("Select same type").
pub(super) fn select_same_type(scene: &Scene, selection: &mut Selection) {
    let mut types: Vec<&str> =
        selection.fixtures.iter().filter_map(|&i| scene.fixtures.get(i)).map(|f| f.profile.as_str()).collect();
    types.sort_unstable();
    types.dedup();
    if types.is_empty() {
        return;
    }
    selection.fixtures = scene
        .fixtures
        .iter()
        .enumerate()
        .filter(|(_, f)| types.contains(&f.profile.as_str()))
        .map(|(i, _)| i)
        .collect();
    selection.environment = None;
}

/// Frame the camera on the selected fixtures (their AABB). No-op if nothing
/// selected.
pub(super) fn frame_selection(scene: &Scene, selection: &Selection, camera: &mut OrbitCamera) {
    let mut it = selection.fixtures.iter().filter_map(|&i| scene.fixtures.get(i)).map(|f| f.position);
    let Some(first) = it.next() else { return };
    let (mut lo, mut hi) = (first, first);
    for p in it {
        lo = lo.min(p);
        hi = hi.max(p);
    }
    camera.frame_aabb(lo - Vec3::splat(0.5), hi + Vec3::splat(0.5));
}

/// Compute the single world pivot the rotate/scale spins/grows about, per the
/// chosen [`PivotMode`] (#5). `objs` are the (validated) selected objects in
/// selection order, so `objs[0]` is the active element. Reads each object's
/// origin through the unified [`Scene::object_anchor`] so EVERY kind (fixtures,
/// geometry, screens, environment) contributes — no kind is excluded from the
/// centroid. Individual-Origins has no single pivot (the applier uses each
/// element's own anchor) so it returns the Median like the others. Empty
/// selection → origin.
pub(super) fn compute_pivot(scene: &Scene, objs: &[ObjectRef], mode: PivotMode, cursor_3d: Vec3) -> Vec3 {
    match mode {
        PivotMode::Cursor3d => cursor_3d,
        PivotMode::Active => objs.first().and_then(|&o| scene.object_anchor(o)).unwrap_or(Vec3::ZERO),
        // Median + Individual both seed from the centroid (Individual's per-element
        // pivots are applied in apply_transform via `op.individual`).
        PivotMode::Median | PivotMode::Individual => {
            let mut sum = Vec3::ZERO;
            let mut n = 0.0_f32;
            for &o in objs {
                if let Some(a) = scene.object_anchor(o) {
                    sum += a;
                    n += 1.0;
                }
            }
            if n > 0.0 { sum / n } else { Vec3::ZERO }
        }
    }
}

/// Flatten validated per-kind index slices into the unified [`ObjectRef`] list
/// (fixtures, then geometry, screens, environment) — the order the pivot's
/// "active" element and the gizmo read. The single place selection → object list
/// happens for the transform path.
pub(super) fn obj_refs(fids: &[usize], gids: &[usize], sids: &[usize], eids: &[usize]) -> Vec<ObjectRef> {
    fids.iter()
        .map(|&i| ObjectRef::Fixture(i))
        .chain(gids.iter().map(|&i| ObjectRef::Geometry(i)))
        .chain(sids.iter().map(|&i| ObjectRef::Screen(i)))
        .chain(eids.iter().map(|&i| ObjectRef::Environment(i)))
        .collect()
}

/// Capture the per-kind op-start snapshots for a transform: fixtures keep
/// (position, orientation); geometry + screens keep their world `Mat4`;
/// environments keep (centre, size). ONE source for both the gizmo-drag and the
/// modal G/R/S op-start so every kind is snapshotted — and thus live-applied and
/// cancel-restored — identically.
#[allow(clippy::type_complexity)]
pub(super) fn snapshot_starts(
    scene: &Scene,
    fids: &[usize],
    gids: &[usize],
    sids: &[usize],
    eids: &[usize],
) -> (Vec<(usize, Vec3, Quat)>, Vec<(usize, Mat4)>, Vec<(usize, Mat4)>, Vec<(usize, Vec3, Vec3)>) {
    let start = fids
        .iter()
        .map(|&i| (i, scene.fixtures[i].position, scene.fixtures[i].orientation))
        .collect();
    let geo_start = gids.iter().map(|&i| (i, scene.geometry[i].transform)).collect();
    let screen_start = sids.iter().map(|&i| (i, scene.screens[i].transform)).collect();
    let env_start =
        eids.iter().map(|&i| (i, scene.environments[i].center, scene.environments[i].size)).collect();
    (start, geo_start, screen_start, env_start)
}

/// Resolve the transform-orientation (#37) to a 3×3 whose COLUMNS are the X/Y/Z
/// directions the axis-lock + numeric default map through (`basis * Axis::vec()`).
/// Mirrors Blender's `applyTransformOrientation` / `ED_transform_calc_orientation`:
///
/// * Global → identity (the world axes; today's behaviour, byte-identical).
/// * Local → the active element's OWN orientation. For a fixture that's its
///   `start` Quat snapshot (stable across the drag); for geometry-only selections
///   the geometry transform's rotation. Falls back to identity if the basis is
///   degenerate (no selection / zero-scale geometry).
/// * View → the camera basis: column 0 = screen-right, 1 = screen-up, 2 = toward
///   the viewer (`-forward`), so a View-axis move follows the screen plane.
pub(super) fn orientation_basis(op: &TransformOp, camera: &OrbitCamera) -> glam::Mat3 {
    use super::TransformOrientation as TO;
    match op.orientation {
        TO::Global => glam::Mat3::IDENTITY,
        TO::View => {
            let (right, up, fwd) = camera.view_basis();
            // Columns = X(right) / Y(up) / Z(toward viewer = -forward).
            glam::Mat3::from_cols(right, up, -fwd)
        }
        TO::Local => {
            // Prefer the active fixture's orientation (its op-start Quat snapshot);
            // else the first geometry/screen piece's rotation from its op-start
            // matrix (screens share geometry's Mat4 transform).
            if let Some((_, _, q)) = op.start.first() {
                glam::Mat3::from_quat(*q)
            } else if let Some((_, m0)) = op.geo_start.first().or_else(|| op.screen_start.first()) {
                let b = glam::Mat3::from_mat4(*m0);
                // Normalize the columns so non-uniform scale doesn't skew the axes;
                // a degenerate column falls back to the world axis.
                let col = |i: usize, fallback: Vec3| {
                    let c = b.col(i);
                    c.try_normalize().unwrap_or(fallback)
                };
                glam::Mat3::from_cols(col(0, Vec3::X), col(1, Vec3::Y), col(2, Vec3::Z))
            } else {
                glam::Mat3::IDENTITY
            }
        }
    }
}

/// The point on the infinite axis line `p + t·axis` CLOSEST to the ray
/// `ro + s·rd` — the #40 ray-plane absolute drag for a single-axis Move. Standard
/// closest-points-between-two-lines (UE `GetAbsoluteTranslationDelta` / Blender
/// `transform_constraints.cc applyAxisConstraintVec` project the cursor ray onto
/// the constraint axis the same way). The handle "sticks" to the cursor because
/// the returned point tracks the cursor's projection along the axis at any camera
/// angle, instead of a fixed pixels→metres speed that drifts at grazing angles.
///
/// `axis` need not be unit (it's normalized here). When the ray is (near-)parallel
/// to the axis the cross-product denominator collapses → returns `p` (no motion),
/// so a degenerate viewing angle can't fling the handle to infinity.
pub(super) fn ray_axis_closest_point(ro: Vec3, rd: Vec3, p: Vec3, axis: Vec3) -> Vec3 {
    let a = axis.normalize_or_zero();
    let r = rd.normalize_or_zero();
    if a == Vec3::ZERO || r == Vec3::ZERO {
        return p;
    }
    // Closest points between two lines (Ericson, Real-Time Collision Detection):
    // axis line = p + s·a, ray line = ro + u·r, both directions unit. Solve for the
    // parameter `s` along the axis that minimizes the gap, then return p + s·a.
    let rel = p - ro;
    let b = a.dot(r); // = cos∠ between axis and ray
    let c = a.dot(rel);
    let f = r.dot(rel);
    let denom = 1.0 - b * b; // = |a×r|² for unit a,r
    if denom.abs() < 1e-6 {
        return p; // axis ∥ ray — undefined projection, hold position
    }
    let s = (b * f - c) / denom; // parameter along the axis
    p + a * s
}

/// Intersect the ray `ro + t·rd` with the plane through `p` with `normal` — the
/// #40 absolute drag for a PLANE-constrained / screen-plane Move (and a building
/// block for future two-axis gizmo handles). Returns the world hit, or `None` when
/// the ray is parallel to the plane (or points away from it: `t ≤ 0`).
// Drives the move gizmo's PLANE handles (#S2): the absolute two-axis drag that keeps
// the grabbed quad stuck to the cursor while the off-plane axis stays fixed.
pub(super) fn ray_plane_point(ro: Vec3, rd: Vec3, p: Vec3, normal: Vec3) -> Option<Vec3> {
    let n = normal.normalize_or_zero();
    let denom = rd.dot(n);
    if denom.abs() < 1e-6 {
        return None; // ray ∥ plane
    }
    let t = (p - ro).dot(n) / denom;
    if t <= 0.0 {
        return None;
    }
    Some(ro + rd * t)
}

/// The nearest OTHER entity origin to `moved` within `max_dist_px` on screen — the
/// #71 Vertex snap target. Scans fixtures, geometry origins and screen origins
/// (skipping `exclude` — the fixtures being moved — and hidden ones), projecting
/// each to screen and keeping the closest to the live cursor `cursor_px`. Screen-
/// space thresholding (Blender's snap is screen-radius based) means the snap only
/// engages when the cursor is genuinely over a node, regardless of world scale.
/// Returns the WORLD origin to snap to, or `None` when nothing is in range.
pub(super) fn nearest_origin_screen(
    scene: &Scene,
    vp: Mat4,
    rect: egui::Rect,
    cursor_px: egui::Pos2,
    exclude: &[usize],
    max_dist_px: f32,
) -> Option<Vec3> {
    let mut best: Option<(f32, Vec3)> = None;
    let mut consider = |world: Vec3| {
        if let Some(sp) = OrbitCamera::project_to_screen(world, vp, rect) {
            let d = sp.distance(cursor_px);
            if d <= max_dist_px && best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, world));
            }
        }
    };
    for (i, f) in scene.fixtures.iter().enumerate() {
        if !f.hidden && !exclude.contains(&i) {
            consider(f.position);
        }
    }
    for g in &scene.geometry {
        if !g.hidden {
            consider(g.transform.w_axis.truncate());
        }
    }
    for s in &scene.screens {
        if !s.hidden {
            consider(s.transform.w_axis.truncate());
        }
    }
    best.map(|(_, w)| w)
}

/// #70 Snap-target PREVIEW: the WORLD destination the PRIMARY moved element's origin
/// will land on, for the snap marker drawn DURING a live Move drag (before release).
/// Returns `None` unless the op is a Move with snap currently engaged — Rotate/Scale
/// and snap-off drags have no destination marker.
///
/// `apply_transform` has already moved the primary element to its snapped destination
/// this frame, so the live origin (`op.start.first()` → `scene.fixtures[i].position`,
/// or the first geometry's translation) IS the destination — for Vertex it's the
/// snapped node, for Surface the cursor surface hit, for Increment the quantized
/// origin. Reading it back (rather than recomputing the snap) keeps the marker exactly
/// consistent with where the element actually goes. Pure + cheap → unit-testable.
/// `snap_on` is the live-resolved snap state (`op.snap.on` XOR a held Ctrl), so a
/// mid-drag Ctrl tap hides/shows the marker in lockstep with the actual snapping.
pub(super) fn snap_preview_point(op: &TransformOp, scene: &Scene, snap_on: bool) -> Option<Vec3> {
    if op.kind != TransformKind::Move || !snap_on {
        return None;
    }
    if let Some((i, _, _)) = op.start.first() {
        return scene.fixtures.get(*i).map(|f| f.position);
    }
    if let Some((i, _)) = op.geo_start.first() {
        return scene.geometry.get(*i).map(|g| g.transform.w_axis.truncate());
    }
    None
}

/// Draw the #70 snap-destination marker: a small ring + cross at the projected snap
/// point, tinted to read as "this is where it lands". Screen-space sized so it stays
/// legible at any zoom. No-op when the point is behind the camera / off the rect math.
pub(super) fn draw_snap_marker(painter: &egui::Painter, world: Vec3, vp: Mat4, rect: egui::Rect) {
    let Some(c) = OrbitCamera::project_to_screen(world, vp, rect) else { return };
    let col = egui::Color32::from_rgb(120, 230, 255); // cyan — the "snap" accent
    let r = 6.0;
    painter.circle_stroke(c, r, egui::Stroke::new(1.5, col));
    // A small plus through the centre so the exact point reads even over busy geometry.
    let x = r + 3.0;
    painter.line_segment([c - egui::vec2(x, 0.0), c + egui::vec2(x, 0.0)], egui::Stroke::new(1.0, col));
    painter.line_segment([c - egui::vec2(0.0, x), c + egui::vec2(0.0, x)], egui::Stroke::new(1.0, col));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::panels::{apply_transform, update_dup_array};
    use crate::ui::{Axis, NumInput, SnapSettings, TransformOrientation};

    fn make_op(kind: TransformKind, axis: Option<Axis>, pivot: Vec3, idx: usize, p0: Vec3) -> TransformOp {
        make_op_q(kind, axis, pivot, idx, p0, Quat::IDENTITY, TransformOrientation::Global)
    }

    /// Like [`make_op`] but with an explicit start-orientation Quat (the Local-basis
    /// source) and a transform orientation (#37).
    fn make_op_q(
        kind: TransformKind,
        axis: Option<Axis>,
        pivot: Vec3,
        idx: usize,
        p0: Vec3,
        q: Quat,
        orientation: TransformOrientation,
    ) -> TransformOp {
        TransformOp { kind, axis, start_screen: egui::pos2(0.0, 0.0), viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)), pivot, start: vec![(idx, p0, q)], geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(), gizmo_hovered_axis: None, gizmo_plane_normal: None, gizmo_view: false, from_gizmo: false, num: NumInput::default(), individual: false, snap: SnapSettings::default(), from_duplicate: false, dup_base: Vec::new(), dup_extra: Vec::new(), orientation }
    }

    #[test]
    fn shift_d_array_is_live_and_reversible() {
        let mut scene = Scene::demo(); // one fixture at index 0
        // Simulate Shift+D's clone: a base copy of fixture 0.
        let base = scene.duplicate_object(ObjectRef::Fixture(0)).unwrap();
        let ObjectRef::Fixture(bi) = base else { panic!("expected a fixture") };
        let home = scene.fixtures[bi].position;
        let n_before = scene.fixtures.len();
        // The grab has moved the base copy by `delta` from its op-start home.
        let delta = Vec3::new(2.0, 0.0, 0.0);
        scene.fixtures[bi].position = home + delta;

        let mut op = make_op(TransformKind::Move, None, Vec3::ZERO, bi, home);
        op.from_duplicate = true;
        op.dup_base = vec![base];
        // Type "3" → an array of 3 (the base copy #1 + two LIVE extras #2/#3).
        op.num = NumInput { str: "3".into(), sign: false, active: true };

        update_dup_array(&mut op, &mut scene);
        assert_eq!(op.dup_extra.len(), 2, "count 3 → 2 extra clones");
        assert_eq!(scene.fixtures.len(), n_before + 2, "extras pushed onto the scene");
        let e0 = match op.dup_extra[0] {
            ObjectRef::Fixture(i) => i,
            _ => panic!("fixture"),
        };
        let e1 = match op.dup_extra[1] {
            ObjectRef::Fixture(i) => i,
            _ => panic!("fixture"),
        };
        // #1 stays at home+delta; extras are evenly spaced at home+2·delta, home+3·delta.
        assert!((scene.fixtures[bi].position - (home + delta)).length() < 1e-3);
        assert!((scene.fixtures[e0].position - (home + delta * 2.0)).length() < 1e-3);
        assert!((scene.fixtures[e1].position - (home + delta * 3.0)).length() < 1e-3);

        // Shrinking the count back to 1 tail-truncates the live extras cleanly.
        op.num = NumInput::default();
        update_dup_array(&mut op, &mut scene);
        assert!(op.dup_extra.is_empty(), "count 1 → no extras");
        assert_eq!(scene.fixtures.len(), n_before, "extras removed from the scene");
    }

    #[test]
    fn move_axis_lock_keeps_other_axes() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        apply_transform(&o, &mut scene, &cam, egui::pos2(120.0, 40.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!(d.y.abs() < 1e-4, "y leaked: {}", d.y);
        assert!(d.z.abs() < 1e-4, "z leaked: {}", d.z);
    }

    /// A move started by grabbing a PLANE handle (#S2) slides ON that plane: the
    /// off-plane (normal) coordinate of every element stays fixed, while the two
    /// in-plane coordinates track the cursor (ray_plane_point absolute drag).
    #[test]
    fn plane_drag_keeps_off_axis_fixed() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // Plane normal Z → the XY plane: Z must not move.
        let o = TransformOp {
            kind: TransformKind::Move,
            axis: None,
            start_screen: egui::pos2(400.0, 300.0),
            viewport: rect,
            pivot: p0,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: Some(Axis::Z),
            gizmo_view: false,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap: SnapSettings::default(),
            orientation: TransformOrientation::Global,
            from_duplicate: false,
            dup_base: Vec::new(), dup_extra: Vec::new(),
        };
        // Drag to a clearly different screen point so the in-plane delta is nonzero.
        apply_transform(&o, &mut scene, &cam, egui::pos2(560.0, 180.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!(d.z.abs() < 1e-4, "off-plane Z leaked: {}", d.z);
        // The drag actually moved the fixture in the plane (not a no-op).
        assert!(d.length() > 1e-3, "plane drag produced no motion: {d:?}");
    }

    /// #72 VIEW-plane move: a centre-square drag slides the fixture on the screen-
    /// parallel plane (normal = camera forward), so the moved offset has NO component
    /// along the camera forward (it stays on the view plane) and the drag is nonzero.
    #[test]
    fn view_plane_move_stays_on_screen_plane() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let o = TransformOp {
            kind: TransformKind::Move,
            axis: None,
            start_screen: egui::pos2(400.0, 300.0),
            viewport: rect,
            pivot: p0,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(),
            screen_start: Vec::new(),
            env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: true,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap: SnapSettings::default(),
            from_duplicate: false,
            dup_base: Vec::new(),
            dup_extra: Vec::new(),
            orientation: TransformOrientation::Global,
        };
        apply_transform(&o, &mut scene, &cam, egui::pos2(560.0, 180.0), false);
        let d = scene.fixtures[0].position - p0;
        let fwd = cam.view_basis().2;
        assert!(d.dot(fwd).abs() < 1e-3, "view-plane move leaked along camera forward: {}", d.dot(fwd));
        assert!(d.length() > 1e-3, "view-plane drag produced no motion: {d:?}");
    }

    /// #72 VIEW-axis rotate: a view-ring drag spins the fixture about the CAMERA
    /// FORWARD. With the pivot offset from the fixture, the position rotates about the
    /// camera-forward axis through the pivot, so the offset's component ALONG forward
    /// is preserved (rotation about an axis can't move points along that axis).
    #[test]
    fn view_rotate_spins_about_camera_forward() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let pivot = p0 + Vec3::new(1.5, 0.5, 0.0);
        let o = TransformOp {
            kind: TransformKind::Rotate,
            axis: None,
            start_screen: egui::pos2(0.0, 0.0),
            viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
            pivot,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(),
            screen_start: Vec::new(),
            env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: true,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap: SnapSettings::default(),
            from_duplicate: false,
            dup_base: Vec::new(),
            dup_extra: Vec::new(),
            orientation: TransformOrientation::Global,
        };
        apply_transform(&o, &mut scene, &cam, egui::pos2(120.0, 0.0), false);
        let fwd = cam.view_basis().2;
        let p1 = scene.fixtures[0].position;
        // The rotation actually moved the fixture (nonzero drag).
        assert!((p1 - p0).length() > 1e-3, "view rotate produced no motion");
        // Offset-from-pivot length is preserved, and its forward-component is unchanged
        // (rotation about the forward axis through the pivot).
        let r0 = p0 - pivot;
        let r1 = p1 - pivot;
        assert!((r0.length() - r1.length()).abs() < 1e-3, "radius changed under view rotate");
        assert!((r0.dot(fwd) - r1.dot(fwd)).abs() < 1e-3, "moved along the camera-forward axis");
    }

    /// #70 snap preview: while a Move is live with Vertex snap engaged, the preview
    /// marker point equals the snap TARGET (the destination the origin lands on) — and
    /// is `None` when snap is off or the op isn't a Move.
    #[test]
    fn snap_preview_matches_snapped_destination() {
        let mut scene = Scene::demo();
        // Need a second fixture as a snap target node.
        scene.duplicate_fixture(0, Vec3::new(5.0, 0.0, 2.0), 0.0, 1).expect("dup");
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let aspect = rect.width() / rect.height();
        let p0 = scene.fixtures[0].position;
        let target = scene.fixtures[1].position;
        // Cursor placed over the target fixture's projected origin so Vertex snaps to it.
        let cursor = OrbitCamera::project_to_screen(target, cam.view_proj(aspect), rect)
            .expect("target on screen");
        let mut snap = SnapSettings::default();
        snap.on = true;
        snap.mode = crate::ui::SnapMode::Vertex;
        let o = TransformOp {
            kind: TransformKind::Move,
            axis: None,
            start_screen: egui::pos2(400.0, 300.0),
            viewport: rect,
            pivot: p0,
            start: vec![(0, p0, scene.fixtures[0].orientation)],
            geo_start: Vec::new(),
            screen_start: Vec::new(),
            env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: false,
            from_gizmo: true,
            num: NumInput::default(),
            individual: false,
            snap,
            from_duplicate: false,
            dup_base: Vec::new(),
            dup_extra: Vec::new(),
            orientation: TransformOrientation::Global,
        };
        apply_transform(&o, &mut scene, &cam, cursor, true);
        let marker = snap_preview_point(&o, &scene, true).expect("preview while Move+snap");
        // The marker is the primary origin's snapped destination = the target node.
        assert!((marker - target).length() < 1e-3, "preview {marker:?} != target {target:?}");
        // Snap off → no marker; Rotate → no marker.
        assert!(snap_preview_point(&o, &scene, false).is_none());
        let rot = make_op(TransformKind::Rotate, None, p0, 0, p0);
        assert!(snap_preview_point(&rot, &scene, true).is_none());
    }

    #[test]
    fn rotate_y_preserves_distance_and_height() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 + Vec3::new(2.0, 0.0, 0.0);
        let o = make_op(TransformKind::Rotate, Some(Axis::Y), pivot, 0, p0);
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(80.0, 0.0), false);
        let before = (p0 - pivot).length();
        let after = (scene.fixtures[0].position - pivot).length();
        assert!((before - after).abs() < 1e-3, "radius changed {before} -> {after}");
        assert!((scene.fixtures[0].position.y - p0.y).abs() < 1e-4, "Y rotation changed height");
    }

    #[test]
    fn scale_expands_from_pivot() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(3.0, 0.0, 0.0);
        let o = make_op(TransformKind::Scale, None, pivot, 0, p0);
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(200.0, 0.0), false);
        let before = (p0 - pivot).length();
        let after = (scene.fixtures[0].position - pivot).length();
        assert!(after > before, "expected expansion {before} -> {after}");
    }

    #[test]
    fn scale_factor_floor_keeps_geometry_nonzero() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(3.0, 0.0, 0.0);
        let o = make_op(TransformKind::Scale, None, pivot, 0, p0);
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(-100000.0, 0.0), false);
        let after = (scene.fixtures[0].position - pivot).length();
        assert!(after > 0.0, "geometry collapsed to the pivot");
    }

    #[test]
    fn numinput_value_parses() {
        let n = NumInput { str: "4.5".into(), sign: false, active: true };
        assert!((n.value() - 4.5).abs() < 1e-6);
        let n = NumInput { str: "4".into(), sign: true, active: true };
        assert!((n.value() + 4.0).abs() < 1e-6);
        assert_eq!(NumInput::default().value(), 0.0);
        assert_eq!(NumInput { str: ".".into(), sign: false, active: true }.value(), 0.0);
    }

    #[test]
    fn numinput_display_shows_sign() {
        assert_eq!(NumInput { str: "4.0".into(), sign: false, active: true }.display(), "4.0");
        assert_eq!(NumInput { str: "45".into(), sign: true, active: true }.display(), "-45");
        // Lone sign before any digit still renders, so the keystroke lands.
        assert_eq!(NumInput { str: String::new(), sign: true, active: true }.display(), "-0");
    }

    #[test]
    fn typed_move_overrides_mouse_exact_metres() {
        // G,X,"4",Enter → move +4 m on global X regardless of mouse position.
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.num = NumInput { str: "4".into(), sign: false, active: true };
        // A wild mouse position must be ignored once numinput is active.
        apply_transform(&o, &mut scene, &cam, egui::pos2(9999.0, -9999.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x - 4.0).abs() < 1e-4, "expected +4 on X, got {}", d.x);
        assert!(d.y.abs() < 1e-4 && d.z.abs() < 1e-4, "leaked off X: {d:?}");
    }

    #[test]
    fn typed_move_no_axis_falls_back_to_global_x() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let mut o = make_op(TransformKind::Move, None, p0, 0, p0);
        o.num = NumInput { str: "2.5".into(), sign: true, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x + 2.5).abs() < 1e-4, "expected -2.5 on X, got {}", d.x);
    }

    #[test]
    fn typed_rotate_uses_degrees() {
        // R,Y,"90" → quarter turn about Y; pivot offset on +X maps to +Z (RH).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(1.0, 0.0, 0.0); // fixture sits at pivot + X
        let mut o = make_op(TransformKind::Rotate, Some(Axis::Y), pivot, 0, p0);
        o.num = NumInput { str: "90".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        let off = scene.fixtures[0].position - pivot;
        // +X (1,0,0) rotated 90° about +Y → (0,0,-1) in glam's RH convention.
        assert!((off.x).abs() < 1e-4, "x not zeroed: {}", off.x);
        assert!((off.z + 1.0).abs() < 1e-4, "expected z=-1, got {}", off.z);
    }

    #[test]
    fn typed_scale_factor_is_exact() {
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(3.0, 0.0, 0.0);
        let mut o = make_op(TransformKind::Scale, None, pivot, 0, p0);
        o.num = NumInput { str: "2".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(12345.0, 0.0), false);
        let before = (p0 - pivot).length();
        let after = (scene.fixtures[0].position - pivot).length();
        assert!((after - before * 2.0).abs() < 1e-4, "expected ×2, {before} -> {after}");
    }

    // --- #4 snap: quantization composes with the typed-amount apply path ------
    #[test]
    fn snap_move_quantizes_typed_amount() {
        // G,X,"1.4",Enter with a 1 m snap grid → lands on exactly +1 m (round).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.num = NumInput { str: "1.4".into(), sign: false, active: true };
        o.snap = SnapSettings { on: true, move_step: 1.0, ..Default::default() };
        // snap_on = true (caller would XOR Ctrl; here passed directly).
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), true);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x - 1.0).abs() < 1e-4, "expected snapped +1 m, got {}", d.x);
    }

    #[test]
    fn snap_off_passes_typed_amount_through() {
        // Same op but snap_on=false (e.g. Ctrl inverted it off) → exact 1.4 m.
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.num = NumInput { str: "1.4".into(), sign: false, active: true };
        o.snap = SnapSettings { on: true, move_step: 1.0, ..Default::default() };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        let d = scene.fixtures[0].position - p0;
        assert!((d.x - 1.4).abs() < 1e-4, "expected exact 1.4 m, got {}", d.x);
    }

    #[test]
    fn snap_rotate_quantizes_to_15_degrees() {
        // R,Y,"20",Enter with a 15° grid → snaps to 15° (quarter-ish turn).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let pivot = p0 - Vec3::new(1.0, 0.0, 0.0);
        let mut o = make_op(TransformKind::Rotate, Some(Axis::Y), pivot, 0, p0);
        o.num = NumInput { str: "20".into(), sign: false, active: true };
        o.snap = SnapSettings { on: true, rotate_deg: 15.0, ..Default::default() };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), true);
        // Offset length is preserved by rotation; check the snapped angle: +X (len 1)
        // rotated 15° about +Y → z = -sin(15°).
        let off = scene.fixtures[0].position - pivot;
        let expect_z = -(15f32.to_radians()).sin();
        assert!((off.z - expect_z).abs() < 1e-3, "expected 15° snap (z={expect_z}), got {}", off.z);
    }

    // --- #5 Individual Origins: each element transforms about its OWN origin ----
    #[test]
    fn individual_origins_rotate_keeps_each_position() {
        // Two fixtures at different spots. With Individual Origins, a rotate spins
        // each about ITSELF: positions are UNCHANGED, only orientations turn. A
        // Median pivot, by contrast, would orbit both about their centroid.
        let mut scene = Scene::demo();
        // Ensure a second fixture exists at a distinct position.
        scene.duplicate_fixture(0, Vec3::new(6.0, 0.0, 0.0), 0.0, 1).expect("dup");
        assert!(scene.fixtures.len() >= 2);
        let p0a = scene.fixtures[0].position;
        let p0b = scene.fixtures[1].position;
        let q0a = scene.fixtures[0].orientation;
        // Median of the two — the pivot the op carries (ignored when individual).
        let median = (p0a + p0b) * 0.5;
        let mut o = TransformOp {
            kind: TransformKind::Rotate,
            axis: Some(Axis::Y),
            start_screen: egui::pos2(0.0, 0.0),
            viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
            pivot: median,
            start: vec![(0, p0a, q0a), (1, p0b, scene.fixtures[1].orientation)],
            geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: false,
            from_gizmo: false,
            num: NumInput { str: "90".into(), sign: false, active: true },
            individual: true,
            snap: SnapSettings::default(),
            orientation: TransformOrientation::Global,
            from_duplicate: false,
            dup_base: Vec::new(), dup_extra: Vec::new(),
        };
        o.num.active = true;
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        // Positions unchanged (each spun about itself)...
        assert!((scene.fixtures[0].position - p0a).length() < 1e-4, "fixture 0 moved");
        assert!((scene.fixtures[1].position - p0b).length() < 1e-4, "fixture 1 moved");
        // ...but the orientation DID rotate.
        assert!(
            scene.fixtures[0].orientation.angle_between(q0a) > 0.1,
            "orientation did not rotate"
        );
    }

    #[test]
    fn median_rotate_orbits_about_centroid() {
        // Same two fixtures, but Median pivot → both ORBIT the shared centroid, so
        // their positions move (the contrast to Individual Origins above).
        let mut scene = Scene::demo();
        scene.duplicate_fixture(0, Vec3::new(6.0, 0.0, 0.0), 0.0, 1).expect("dup");
        let p0a = scene.fixtures[0].position;
        let p0b = scene.fixtures[1].position;
        let median = (p0a + p0b) * 0.5;
        let o = TransformOp {
            kind: TransformKind::Rotate,
            axis: Some(Axis::Y),
            start_screen: egui::pos2(0.0, 0.0),
            viewport: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
            pivot: median,
            start: vec![
                (0, p0a, scene.fixtures[0].orientation),
                (1, p0b, scene.fixtures[1].orientation),
            ],
            geo_start: Vec::new(), screen_start: Vec::new(), env_start: Vec::new(),
            gizmo_hovered_axis: None,
            gizmo_plane_normal: None,
            gizmo_view: false,
            from_gizmo: false,
            num: NumInput { str: "90".into(), sign: false, active: true },
            individual: false,
            snap: SnapSettings::default(),
            orientation: TransformOrientation::Global,
            from_duplicate: false,
            dup_base: Vec::new(), dup_extra: Vec::new(),
        };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        // At least one position moved (they orbited the centroid).
        assert!(
            (scene.fixtures[0].position - p0a).length() > 0.1
                || (scene.fixtures[1].position - p0b).length() > 0.1,
            "median rotate did not orbit"
        );
    }

    // --- #37 transform orientations -----------------------------------------
    #[test]
    fn local_rotate_spins_about_elements_own_axis() {
        // A fixture yawed 90° about world +Y has its LOCAL +X pointing along world
        // −Z. A typed Local rotate locked to X must turn the orientation about THAT
        // local axis, not world X — so the resulting spin axis is world −Z.
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let q = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2); // local X → world -Z
        let mut o = make_op_q(
            TransformKind::Rotate,
            Some(Axis::X),
            p0,
            0,
            p0,
            q,
            TransformOrientation::Local,
        );
        o.num = NumInput { str: "90".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &OrbitCamera::default(), egui::pos2(0.0, 0.0), false);
        // The applied delta rotation = orientation_after * orientation_before⁻¹.
        let delta = scene.fixtures[0].orientation * q.inverse();
        let (axis, angle) = delta.to_axis_angle();
        // Spun 90° about the LOCAL X = world (q * X) = (0,0,-1) (sign-agnostic).
        let local_x = q * Vec3::X;
        assert!((angle - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "angle {angle}");
        assert!(
            axis.dot(local_x).abs() > 0.999,
            "spin axis {axis:?} not aligned to local X {local_x:?}"
        );
        // And it is NOT world X (which Global would have used).
        assert!(axis.dot(Vec3::X).abs() < 1e-2, "leaked onto world X: {axis:?}");
    }

    #[test]
    fn view_move_follows_the_screen_plane() {
        // A View-space X (screen-right) move must land in the camera's right/up plane
        // — i.e. it has NO component along the camera forward axis (it slides across
        // the screen, never toward/away from the viewer).
        let mut scene = Scene::demo();
        let p0 = scene.fixtures[0].position;
        let cam = OrbitCamera::default();
        let (right, up, fwd) = cam.view_basis();
        let mut o = make_op(TransformKind::Move, Some(Axis::X), p0, 0, p0);
        o.orientation = TransformOrientation::View;
        o.num = NumInput { str: "3".into(), sign: false, active: true };
        apply_transform(&o, &mut scene, &cam, egui::pos2(0.0, 0.0), false);
        let d = scene.fixtures[0].position - p0;
        // Moved 3 m along screen-right, and stayed in the screen plane.
        assert!((d.dot(right) - 3.0).abs() < 1e-3, "expected +3 along screen-right, got {}", d.dot(right));
        assert!(d.dot(fwd).abs() < 1e-3, "leaked toward the viewer: {}", d.dot(fwd));
        assert!(d.dot(up).abs() < 1e-3, "leaked onto screen-up: {}", d.dot(up));
    }

    #[test]
    fn global_orientation_matches_world_axes() {
        // Sanity: Global == identity basis, so an oriented op reduces to the old
        // world-axis behaviour byte-for-byte (no regression).
        let mut a = Scene::demo();
        let mut b = Scene::demo();
        let p0 = a.fixtures[0].position;
        let cam = OrbitCamera::default();
        let mut o = make_op(TransformKind::Move, Some(Axis::Z), p0, 0, p0);
        o.orientation = TransformOrientation::Global;
        o.num = NumInput { str: "2".into(), sign: false, active: true };
        apply_transform(&o, &mut a, &cam, egui::pos2(0.0, 0.0), false);
        let d = a.fixtures[0].position - p0;
        assert!((d.z - 2.0).abs() < 1e-4 && d.x.abs() < 1e-4 && d.y.abs() < 1e-4, "global Z move wrong: {d:?}");
        // Untouched control scene stays put.
        assert_eq!(b.fixtures[0].position, p0);
        let _ = &mut b;
    }

    // --- S2 #40 ray-plane absolute drag math --------------------------------
    #[test]
    fn ray_axis_projection_sticks_to_the_cursor() {
        // A ray aimed straight down at (5,*,0) projected onto the world-X axis line
        // through the origin must land at x=5 (the cursor's foot on the axis),
        // regardless of the ray's height — the "handle sticks to the cursor" core.
        let ro = Vec3::new(5.0, 10.0, 0.0);
        let rd = Vec3::new(0.0, -1.0, 0.0);
        let p = ray_axis_closest_point(ro, rd, Vec3::ZERO, Vec3::X);
        assert!((p.x - 5.0).abs() < 1e-4, "expected x=5 on the axis, got {}", p.x);
        // The result lies ON the axis line (y=z=0).
        assert!(p.y.abs() < 1e-4 && p.z.abs() < 1e-4, "off the X axis line: {p:?}");
        // A second cursor further along maps further along — monotone, no drift.
        let q = ray_axis_closest_point(Vec3::new(9.0, 3.0, 0.0), rd, Vec3::ZERO, Vec3::X);
        assert!((q.x - 9.0).abs() < 1e-4, "expected x=9, got {}", q.x);
    }

    #[test]
    fn ray_axis_parallel_holds_position() {
        // A ray PARALLEL to the constraint axis has no well-defined projection — the
        // helper must return the pivot (no motion) rather than flinging to infinity.
        let ro = Vec3::new(0.0, 2.0, 0.0);
        let rd = Vec3::X; // parallel to the X axis
        let p = ray_axis_closest_point(ro, rd, Vec3::new(1.0, 0.0, 0.0), Vec3::X);
        assert!((p - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-4, "should hold the pivot, got {p:?}");
    }

    #[test]
    fn ray_plane_intersects() {
        // Ray down the −Y from (2,5,3) meets the y=0 plane (normal +Y) at (2,0,3).
        let hit = ray_plane_point(
            Vec3::new(2.0, 5.0, 3.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::ZERO,
            Vec3::Y,
        )
        .expect("should hit the plane");
        assert!((hit - Vec3::new(2.0, 0.0, 3.0)).length() < 1e-4, "wrong plane hit: {hit:?}");
        // A ray parallel to the plane misses (None).
        assert!(ray_plane_point(Vec3::new(0.0, 5.0, 0.0), Vec3::X, Vec3::ZERO, Vec3::Y).is_none());
    }

    // --- S2 #71 Vertex snap: nearest other origin within the screen threshold ---
    #[test]
    fn vertex_snap_picks_nearest_other_origin() {
        // Two fixtures: 0 (being moved) and 1 (a target node). Looking straight down
        // the −Y with fixture 1 directly under the cursor, the nearest-origin query
        // returns fixture 1's world origin (and never fixture 0, which is excluded).
        let mut scene = Scene::demo();
        scene.fixtures.clear();
        scene.screens.clear();
        scene.geometry.clear();
        scene.environments.clear();
        let demo = Scene::demo();
        scene.fixtures.push(demo.fixtures[0].clone()); // 0: moved
        scene.fixtures.push(demo.fixtures[0].clone()); // 1: target node
        scene.fixtures[0].position = Vec3::new(0.0, 0.0, 0.0);
        scene.fixtures[0].hidden = false;
        let target = Vec3::new(4.0, 0.0, -2.0);
        scene.fixtures[1].position = target;
        scene.fixtures[1].hidden = false;

        let mut cam = OrbitCamera::default();
        cam.target = target; // centre the view on the node so it projects to rect-centre
        cam.set_aspect(1.0);
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 600.0));
        let vp = cam.view_proj(1.0);
        // Cursor sitting on the projected target node.
        let cursor = OrbitCamera::project_to_screen(target, vp, rect).expect("target on screen");
        let got = nearest_origin_screen(&scene, vp, rect, cursor, &[0], 18.0).expect("a node in range");
        assert!((got - target).length() < 1e-3, "expected the node origin {target:?}, got {got:?}");

        // Excluding fixture 1 too (no other nodes) → nothing in range.
        let none = nearest_origin_screen(&scene, vp, rect, cursor, &[0, 1], 18.0);
        assert!(none.is_none(), "expected no snap target, got {none:?}");

        // A cursor far from any node → out of the pixel threshold → None.
        let far = nearest_origin_screen(&scene, vp, rect, cursor + egui::vec2(300.0, 0.0), &[0], 18.0);
        assert!(far.is_none(), "cursor off all nodes should not snap, got {far:?}");
    }
}
