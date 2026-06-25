//! Gizmo groups — the on-pivot transform handles (`docs/RESEARCH-blender-framework.md` §2.4).
//!
//! Blender's model: a `wmGizmoGroupType` is a factory that instantiates handles at
//! the selection pivot; the *active tool* decides which group draws (Move→arrows,
//! Rotate→dial rings, Scale→boxes). A handle owns a `highlight_part` (the hovered
//! sub-handle) and `test_select`/`draw`/`invoke` callbacks. We mirror that with the
//! [`GizmoGroup`] trait: each group knows how to [hit-test](GizmoGroup::test_select)
//! a pointer against its handles, [draw](GizmoGroup::draw) itself (highlighting the
//! hovered handle), and on a press [invoke](GizmoGroup::invoke) a [`GizmoStart`] that
//! the caller turns into a [`TransformOp`] — **reusing the existing op + undo
//! pipeline verbatim** (one snapshot on drag-start, one undo step on release).
//!
//! All three groups are screen-space (projected at the pivot via `camera.view_proj`,
//! sized in pixels) and painted with the egui painter, exactly like the P3a move
//! handles — so `MoveGizmo` is behaviour-identical to today. The active tool selects
//! the group in [`for_tool`]; the spring-loaded keyboard G/R/S modal transforms are
//! untouched and stay available under every tool.

use egui::Pos2;
use glam::{Mat4, Vec3};

use crate::renderer::camera::OrbitCamera;

use super::panels::dist_point_segment;
use super::tools::GizmoKind;
use super::{ActiveTool, Axis, TransformKind};

/// Pixel radius within which a press counts as grabbing a handle. Matches the P3a
/// move-handle pick radius so the feel is unchanged.
const GRAB_PX: f32 = 7.0;

/// The per-frame context a gizmo group needs to draw + hit-test itself: the
/// selection pivot (world), the camera's `view_proj`, the viewport rect, and a
/// camera-distance-derived arm length (so handles stay a readable size at any zoom).
/// Mirrors the blueprint's `GizmoCtx` (sans the scene/selection borrows the caller
/// already holds).
pub struct GizmoCtx {
    /// Selection centroid — the handle origin in world space.
    pub pivot: Vec3,
    /// `camera.view_proj(aspect)` for this frame.
    pub vp: Mat4,
    /// The viewport image rect (handles project into here).
    pub rect: egui::Rect,
    /// Handle arm length in world units (camera-distance scaled, clamped).
    pub arm: f32,
}

/// A handle hit — which sub-part of a gizmo group the pointer is over. The blueprint
/// lists `Axis/Plane/View/AimTarget/MeasureEnd`; P3b needs the per-axis handles plus
/// the uniform (view) centre that the scale gizmo's middle box drives.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// A single coloured axis handle (arrow / ring / box).
    Axis(Axis),
    /// A two-axis PLANE handle (move gizmo only — the small quad between an axis
    /// pair near the pivot). The carried [`Axis`] is the plane *normal* — the axis
    /// held FIXED while the drag slides on the other two: `Plane(Z)` = the XY plane
    /// (Blender/UE's three corner quads). Drives a [`ray_plane_point`] absolute drag.
    Plane(Axis),
    /// The uniform centre handle (scale gizmo only — drag the middle box to scale
    /// every axis together; `axis = None` on the resulting op).
    Uniform,
}

/// What a handle-grab kicks off: the transform kind + the axis it locks (None =
/// uniform, for the scale centre). The caller builds the full [`TransformOp`] from
/// this (snapshotting the selection), so all the live-apply + undo machinery is the
/// shared path the modal transforms already use.
pub struct GizmoStart {
    pub kind: TransformKind,
    /// The locked axis, or `None` for a uniform (centre) scale.
    pub axis: Option<Axis>,
    /// For a Move PLANE handle: the plane *normal* (the held-fixed axis). `None`
    /// for every axis/uniform handle. When set the caller drives a two-axis
    /// `ray_plane_point` absolute drag instead of a single-axis projection.
    pub plane_normal: Option<Axis>,
}

/// A drawable + interactive transform-gizmo group at the selection pivot. The active
/// tool picks the concrete group ([`for_tool`]); the caller runs `test_select` →
/// `draw` → (on a press) `invoke` each frame, *before* orbit/select so a handle grab
/// never moves the camera.
pub trait GizmoGroup {
    /// The handle nearest `p` within the grab radius, or `None`. Used both to
    /// highlight on hover and to decide what a press grabs.
    fn test_select(&self, p: Pos2, cx: &GizmoCtx) -> Option<Handle>;
    /// Paint the group, highlighting `hover` (the handle under the pointer, if any).
    fn draw(&self, painter: &egui::Painter, cx: &GizmoCtx, hover: Option<Handle>);
    /// Map a grabbed handle to the transform it starts.
    fn invoke(&self, h: Handle) -> GizmoStart;
}

/// The gizmo group for the active tool, or `None` for tools that show no transform
/// gizmo (Select / Aim / Measure / Add — those keep plain click-select in P3b). The
/// tool→group mapping is data-driven: the tool's [`ToolDef`] declares a [`GizmoKind`]
/// (C2 / P0 #10) and this projects it to the concrete group implementor.
pub fn for_tool(tool: ActiveTool) -> Option<Box<dyn GizmoGroup>> {
    match tool.def().gizmo {
        GizmoKind::Move => Some(Box::new(MoveGizmo)),
        GizmoKind::Rotate => Some(Box::new(RotateGizmo)),
        GizmoKind::Scale => Some(Box::new(ScaleGizmo)),
        GizmoKind::None => None,
    }
}

/// egui colour for an axis at a given alpha, from the single [`Axis::color`] source.
fn axis_color(ax: Axis, alpha: u8) -> egui::Color32 {
    let [r, g, b] = ax.color();
    egui::Color32::from_rgba_unmultiplied((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, alpha)
}

// --------------------------------------------------------------------------------
// Move — RGB axis arrows (behaviour-identical to the P3a screen-space move gizmo).
// --------------------------------------------------------------------------------

/// Three RGB axis handles at the pivot (translate along that world axis) plus three
/// PLANE quads between the axis pairs (translate ON that plane). Blender/UE's move
/// gizmo: the corner quads stick to the cursor via a `ray_plane_point` absolute drag.
pub struct MoveGizmo;

/// The fraction of `arm` at which a plane quad's near corner sits from the pivot, and
/// its half-extent (px). Small squares hugging the pivot corner of each axis pair, so
/// they don't fight the axis arms (which run the full arm length).
const PLANE_OFFSET: f32 = 0.32;
const PLANE_HALF_PX: f32 = 6.0;

impl MoveGizmo {
    /// Screen endpoints (origin, tip) of the axis arm, or `None` if either end is
    /// behind the camera.
    fn arm_screen(ax: Axis, cx: &GizmoCtx) -> Option<(Pos2, Pos2)> {
        let origin = OrbitCamera::project_to_screen(cx.pivot, cx.vp, cx.rect)?;
        let tip = OrbitCamera::project_to_screen(cx.pivot + ax.vec() * cx.arm, cx.vp, cx.rect)?;
        Some((origin, tip))
    }

    /// The two in-plane axes for a plane whose normal is `normal` (e.g. normal Z →
    /// (X, Y) → the XY plane). Ordered X<Y<Z so the colour/label is deterministic.
    fn plane_axes(normal: Axis) -> (Axis, Axis) {
        match normal {
            Axis::X => (Axis::Y, Axis::Z),
            Axis::Y => (Axis::X, Axis::Z),
            Axis::Z => (Axis::X, Axis::Y),
        }
    }

    /// The screen-space CENTRE of the plane quad whose normal is `normal`: the world
    /// point a small step out along each in-plane axis, projected. `None` if behind
    /// the camera.
    fn plane_center(normal: Axis, cx: &GizmoCtx) -> Option<Pos2> {
        let (u, v) = Self::plane_axes(normal);
        let w = cx.pivot + (u.vec() + v.vec()) * (cx.arm * PLANE_OFFSET);
        OrbitCamera::project_to_screen(w, cx.vp, cx.rect)
    }

    /// The screen rect of the plane quad (centred on [`plane_center`]).
    fn plane_rect(c: Pos2) -> egui::Rect {
        egui::Rect::from_center_size(c, egui::vec2(PLANE_HALF_PX * 2.0, PLANE_HALF_PX * 2.0))
    }
}

impl GizmoGroup for MoveGizmo {
    fn test_select(&self, p: Pos2, cx: &GizmoCtx) -> Option<Handle> {
        // Plane quads first: they sit near the pivot where the axis arms also pass,
        // so a cursor over a quad should grab the PLANE (the larger, more specific
        // target) rather than the arm line it overlaps.
        for normal in [Axis::Z, Axis::X, Axis::Y] {
            if let Some(c) = Self::plane_center(normal, cx)
                && Self::plane_rect(c).contains(p)
            {
                return Some(Handle::Plane(normal));
            }
        }
        for ax in [Axis::X, Axis::Y, Axis::Z] {
            if let Some((o, t)) = Self::arm_screen(ax, cx)
                && dist_point_segment(p, o, t) <= GRAB_PX
            {
                return Some(Handle::Axis(ax));
            }
        }
        None
    }

    fn draw(&self, painter: &egui::Painter, cx: &GizmoCtx, hover: Option<Handle>) {
        let Some(origin) = OrbitCamera::project_to_screen(cx.pivot, cx.vp, cx.rect) else { return };
        for ax in [Axis::X, Axis::Y, Axis::Z] {
            let Some((_, tip)) = Self::arm_screen(ax, cx) else { continue };
            let hot = hover == Some(Handle::Axis(ax));
            let col = if hot { axis_color(ax, 255) } else { axis_color(ax, 150) };
            painter.line_segment([origin, tip], egui::Stroke::new(if hot { 3.0 } else { 2.0 }, col));
            painter.circle_filled(tip, if hot { 5.0 } else { 4.0 }, col);
        }
        // Plane quads: a translucent fill tinted toward the normal's axis colour, a
        // brighter outline, and full opacity when hovered (Blender's plane handles).
        for normal in [Axis::X, Axis::Y, Axis::Z] {
            let Some(c) = Self::plane_center(normal, cx) else { continue };
            let hot = hover == Some(Handle::Plane(normal));
            let r = Self::plane_rect(c);
            let fill = axis_color(normal, if hot { 150 } else { 70 });
            let edge = axis_color(normal, if hot { 255 } else { 170 });
            painter.rect_filled(r, 1.0, fill);
            painter.rect_stroke(r, 1.0, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        }
        painter.circle_filled(origin, 3.0, egui::Color32::from_gray(220));
    }

    fn invoke(&self, h: Handle) -> GizmoStart {
        match h {
            Handle::Plane(normal) => {
                GizmoStart { kind: TransformKind::Move, axis: None, plane_normal: Some(normal) }
            }
            Handle::Axis(a) => {
                GizmoStart { kind: TransformKind::Move, axis: Some(a), plane_normal: None }
            }
            Handle::Uniform => GizmoStart { kind: TransformKind::Move, axis: None, plane_normal: None },
        }
    }
}

// --------------------------------------------------------------------------------
// Rotate — three RGB dial rings; grab one to rotate about that axis.
// --------------------------------------------------------------------------------

/// Segment count for a ring polyline — enough to read as a circle at handle size.
const RING_SEGMENTS: usize = 48;

/// Three RGB rings at the pivot (one per world axis); drag a ring to rotate about
/// its axis (reuses `apply_transform`'s Rotate math via the axis-locked op).
pub struct RotateGizmo;

impl RotateGizmo {
    /// The projected screen points of axis `ax`'s ring (a circle in the plane normal
    /// to `ax`, radius = arm). Skips points behind the camera; an empty/short result
    /// means the ring isn't usefully visible this frame.
    fn ring_screen(ax: Axis, cx: &GizmoCtx) -> Vec<Pos2> {
        // Two unit vectors spanning the plane perpendicular to the ring axis.
        let n = ax.vec();
        let u = if n.x.abs() < 0.9 { n.cross(Vec3::X) } else { n.cross(Vec3::Y) }.normalize_or_zero();
        let v = n.cross(u).normalize_or_zero();
        let mut pts = Vec::with_capacity(RING_SEGMENTS + 1);
        for k in 0..=RING_SEGMENTS {
            let a = k as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
            let w = cx.pivot + (u * a.cos() + v * a.sin()) * cx.arm;
            if let Some(p) = OrbitCamera::project_to_screen(w, cx.vp, cx.rect) {
                pts.push(p);
            }
        }
        pts
    }

    /// Min distance from `p` to the ring polyline of `ax`.
    fn ring_dist(p: Pos2, ax: Axis, cx: &GizmoCtx) -> f32 {
        let pts = Self::ring_screen(ax, cx);
        if pts.len() < 2 {
            return f32::INFINITY;
        }
        pts.windows(2).map(|w| dist_point_segment(p, w[0], w[1])).fold(f32::INFINITY, f32::min)
    }
}

impl GizmoGroup for RotateGizmo {
    fn test_select(&self, p: Pos2, cx: &GizmoCtx) -> Option<Handle> {
        // Pick the closest ring within the grab radius (rings overlap near the
        // pivot, so nearest-wins avoids ambiguity).
        let mut best: Option<(Axis, f32)> = None;
        for ax in [Axis::X, Axis::Y, Axis::Z] {
            let d = Self::ring_dist(p, ax, cx);
            if d <= GRAB_PX && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((ax, d));
            }
        }
        best.map(|(ax, _)| Handle::Axis(ax))
    }

    fn draw(&self, painter: &egui::Painter, cx: &GizmoCtx, hover: Option<Handle>) {
        for ax in [Axis::X, Axis::Y, Axis::Z] {
            let pts = Self::ring_screen(ax, cx);
            if pts.len() < 2 {
                continue;
            }
            let hot = hover == Some(Handle::Axis(ax));
            let col = if hot { axis_color(ax, 255) } else { axis_color(ax, 150) };
            painter.add(egui::Shape::line(pts, egui::Stroke::new(if hot { 3.0 } else { 1.5 }, col)));
        }
        if let Some(origin) = OrbitCamera::project_to_screen(cx.pivot, cx.vp, cx.rect) {
            painter.circle_filled(origin, 3.0, egui::Color32::from_gray(220));
        }
    }

    fn invoke(&self, h: Handle) -> GizmoStart {
        // Rotate exposes only axis rings — Plane never originates here.
        let axis = match h {
            Handle::Axis(a) => Some(a),
            Handle::Plane(_) | Handle::Uniform => None,
        };
        GizmoStart { kind: TransformKind::Rotate, axis, plane_normal: None }
    }
}

// --------------------------------------------------------------------------------
// Scale — RGB axis boxes + a uniform centre box.
// --------------------------------------------------------------------------------

/// Half-size (px) of a square scale-handle box drawn at an arm tip / the centre.
const BOX_HALF: f32 = 5.0;

/// Three RGB axis handles ending in small boxes (drag → scale that axis) plus a
/// centre box (drag → uniform scale). Reuses `apply_transform`'s Scale math; the
/// centre maps to an axis-less op (uniform) exactly as a keyboard `S` with no lock.
pub struct ScaleGizmo;

impl ScaleGizmo {
    fn arm_screen(ax: Axis, cx: &GizmoCtx) -> Option<(Pos2, Pos2)> {
        let origin = OrbitCamera::project_to_screen(cx.pivot, cx.vp, cx.rect)?;
        let tip = OrbitCamera::project_to_screen(cx.pivot + ax.vec() * cx.arm, cx.vp, cx.rect)?;
        Some((origin, tip))
    }

    /// Square box centred at `c` with half-extent `BOX_HALF`.
    fn box_rect(c: Pos2, half: f32) -> egui::Rect {
        egui::Rect::from_center_size(c, egui::vec2(half * 2.0, half * 2.0))
    }
}

impl GizmoGroup for ScaleGizmo {
    fn test_select(&self, p: Pos2, cx: &GizmoCtx) -> Option<Handle> {
        // Centre box first (it sits over the arm origins) → uniform scale.
        if let Some(origin) = OrbitCamera::project_to_screen(cx.pivot, cx.vp, cx.rect)
            && Self::box_rect(origin, BOX_HALF + 1.0).contains(p)
        {
            return Some(Handle::Uniform);
        }
        for ax in [Axis::X, Axis::Y, Axis::Z] {
            if let Some((o, t)) = Self::arm_screen(ax, cx) {
                // The box at the tip, or the arm line itself, both grab the axis.
                if Self::box_rect(t, BOX_HALF + 1.0).contains(p) || dist_point_segment(p, o, t) <= GRAB_PX {
                    return Some(Handle::Axis(ax));
                }
            }
        }
        None
    }

    fn draw(&self, painter: &egui::Painter, cx: &GizmoCtx, hover: Option<Handle>) {
        let Some(origin) = OrbitCamera::project_to_screen(cx.pivot, cx.vp, cx.rect) else { return };
        for ax in [Axis::X, Axis::Y, Axis::Z] {
            let Some((_, tip)) = Self::arm_screen(ax, cx) else { continue };
            let hot = hover == Some(Handle::Axis(ax));
            let col = if hot { axis_color(ax, 255) } else { axis_color(ax, 150) };
            painter.line_segment([origin, tip], egui::Stroke::new(if hot { 3.0 } else { 2.0 }, col));
            painter.rect_filled(Self::box_rect(tip, if hot { BOX_HALF + 1.0 } else { BOX_HALF }), 0.0, col);
        }
        // Uniform centre box: white-ish, brighter when hovered.
        let uhot = hover == Some(Handle::Uniform);
        let ucol = if uhot { egui::Color32::WHITE } else { egui::Color32::from_gray(220) };
        painter.rect_filled(Self::box_rect(origin, if uhot { BOX_HALF + 1.0 } else { BOX_HALF }), 0.0, ucol);
    }

    fn invoke(&self, h: Handle) -> GizmoStart {
        // Scale exposes axis boxes + the uniform centre — Plane never originates here.
        let axis = match h {
            Handle::Axis(a) => Some(a),
            Handle::Plane(_) | Handle::Uniform => None,
        };
        GizmoStart { kind: TransformKind::Scale, axis, plane_normal: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a GizmoCtx whose pivot sits at the camera target (so it projects to the
    /// rect centre and every handle is on-screen) with a readable arm length.
    fn ctx() -> GizmoCtx {
        let cam = OrbitCamera::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let aspect = rect.width() / rect.height();
        GizmoCtx { pivot: cam.target, vp: cam.view_proj(aspect), rect, arm: 2.0 }
    }

    /// The plane→in-plane-axes map is the two axes that are NOT the normal.
    #[test]
    fn plane_axes_excludes_the_normal() {
        assert!(MoveGizmo::plane_axes(Axis::Z) == (Axis::X, Axis::Y));
        assert!(MoveGizmo::plane_axes(Axis::X) == (Axis::Y, Axis::Z));
        assert!(MoveGizmo::plane_axes(Axis::Y) == (Axis::X, Axis::Z));
    }

    /// A cursor sitting on a plane quad picks the PLANE handle — even though the
    /// quad overlaps the axis arms near the pivot, the plane is the preferred target.
    #[test]
    fn pick_prefers_plane_over_axis() {
        let cx = ctx();
        let g = MoveGizmo;
        for normal in [Axis::X, Axis::Y, Axis::Z] {
            let c = MoveGizmo::plane_center(normal, &cx).expect("plane on screen");
            // Dead-centre of the quad → the plane handle.
            assert!(
                g.test_select(c, &cx) == Some(Handle::Plane(normal)),
                "cursor on the {} plane quad should grab the plane handle",
                normal.label()
            );
        }
    }

    /// Grabbing a plane handle starts a Move op whose `plane_normal` is the held axis
    /// (and no single-axis lock); grabbing an arm stays a single-axis Move.
    #[test]
    fn invoke_plane_carries_normal() {
        let g = MoveGizmo;
        let s = g.invoke(Handle::Plane(Axis::Z));
        assert!(s.kind == TransformKind::Move);
        assert!(s.plane_normal == Some(Axis::Z));
        assert!(s.axis.is_none());

        let a = g.invoke(Handle::Axis(Axis::X));
        assert!(a.axis == Some(Axis::X));
        assert!(a.plane_normal.is_none());
    }

    /// Empty space (well outside the gizmo) hits nothing.
    #[test]
    fn pick_empty_space_misses() {
        let cx = ctx();
        let g = MoveGizmo;
        assert!(g.test_select(egui::pos2(5.0, 5.0), &cx).is_none());
    }
}
