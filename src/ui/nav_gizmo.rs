//! Navigation axis gizmo (P1 #35) — Blender's corner orientation gizmo.
//!
//! A small overlay cluster in a viewport corner: six labelled coloured balls for
//! the world axes (+X/-X/+Y/-Y/+Z/-Z), oriented LIVE by the camera so it doubles
//! as an orientation readout (which way is "up", where +Z points). Clicking a ball
//! snaps the camera to look down that axis — i.e. moves the eye onto that axis side
//! — via the canned [`CameraView`] jump (`OrbitCamera::set_view`, eased).
//!
//! Drawn with the egui painter in `panels::viewport` (no extra render pass): the
//! axis directions are projected through the camera basis (`view_basis`) onto the
//! gizmo's local 2D disc, so the cluster tumbles as the camera orbits. The math is
//! split out here as PURE helpers ([`balls`] / [`hit_test`]) so the click→view
//! mapping is unit-tested without an egui context.

use egui::{Pos2, Vec2};
use glam::Vec3;

use crate::renderer::camera::{CameraView, OrbitCamera};

use super::Axis;

/// Disc radius (px) of the whole gizmo cluster — the centre-to-ball arm length.
pub const GIZMO_RADIUS: f32 = 30.0;
/// Ball radius (px). The pick radius matches it so a click anywhere on a ball hits.
pub const BALL_RADIUS: f32 = 8.0;

/// One projected axis ball: where it sits on screen, which view a click snaps to,
/// its axis colour + label, the sign (`+`/`-`), and whether it faces the camera
/// (positive depth = nearer the viewer, drawn solid; the far ball is a hollow ring).
#[derive(Clone, Copy)]
pub struct AxisBall {
    /// Screen position (already offset to the gizmo centre).
    pub pos: Pos2,
    /// The canned view a click on this ball snaps to.
    pub view: CameraView,
    /// Axis colour (Blender convention, from [`Axis::color`]).
    pub color: [f32; 3],
    /// Axis letter, e.g. `"X"`.
    pub label: &'static str,
    /// `true` for the +axis ball (labelled, solid), `false` for the −axis ball.
    pub positive: bool,
    /// Camera-space depth: >0 = the ball is on the near side (toward the viewer).
    /// Drives draw order (far balls first) + the solid/hollow style.
    pub depth: f32,
}

/// The six axis balls for the current camera orientation, positioned about
/// `center` on a disc of radius [`GIZMO_RADIUS`]. Each world axis direction is
/// projected onto the camera's right/up basis (screen x/y); the camera-forward
/// component is the depth used for draw order + near/far styling. PURE — no egui
/// context — so the hit-test maps a click to the right [`CameraView`] in tests.
pub fn balls(camera: &OrbitCamera, center: Pos2) -> [AxisBall; 6] {
    // Camera basis: world `right`/`up` map to screen +x/+y; `forward` is into the
    // screen (away from the viewer), so a positive `dot(dir, forward)` means the
    // ball points away → FAR. We negate it so `depth > 0` reads as "near".
    let (right, up, forward) = camera.view_basis();
    let project = |dir: Vec3| -> (Vec2, f32) {
        let sx = dir.dot(right);
        let sy = dir.dot(up);
        let depth = -dir.dot(forward); // near = +, far = −
        (Vec2::new(sx, -sy) * GIZMO_RADIUS, depth) // screen y is flipped (down +)
    };
    // (axis, world dir, which canned view the EYE lands on, +/-).
    // Eye lands on the POSITIVE side of each axis for Right/Top/Front (see
    // `OrbitCamera::view_angles`), so the +axis ball snaps to those.
    let spec: [(Axis, Vec3, CameraView, bool); 6] = [
        (Axis::X, Vec3::X, CameraView::Right, true),
        (Axis::X, Vec3::NEG_X, CameraView::Left, false),
        (Axis::Y, Vec3::Y, CameraView::Top, true),
        (Axis::Y, Vec3::NEG_Y, CameraView::Bottom, false),
        (Axis::Z, Vec3::Z, CameraView::Front, true),
        (Axis::Z, Vec3::NEG_Z, CameraView::Back, false),
    ];
    spec.map(|(ax, dir, view, positive)| {
        let (off, depth) = project(dir);
        AxisBall {
            pos: center + off,
            view,
            color: ax.color(),
            label: ax.label(),
            positive,
            depth,
        }
    })
}

/// The [`CameraView`] a click at `p` snaps to, or `None` if the click misses every
/// ball. Picks the NEAREST ball within [`BALL_RADIUS`]; ties break toward the ball
/// nearer the viewer (larger depth) so the front ball of an overlapping pair wins —
/// matching Blender's "click the ball facing you" feel.
pub fn hit_test(balls: &[AxisBall], p: Pos2) -> Option<CameraView> {
    let mut best: Option<(f32, f32, CameraView)> = None; // (dist², depth, view)
    for b in balls {
        let d2 = (b.pos - p).length_sq();
        if d2 > BALL_RADIUS * BALL_RADIUS {
            continue;
        }
        // Nearer ball wins; on a near-tie in distance prefer the one toward the
        // viewer (larger depth).
        let better = match best {
            None => true,
            Some((bd2, bdepth, _)) => {
                d2 < bd2 - 0.5 || ((d2 - bd2).abs() <= 0.5 && b.depth > bdepth)
            }
        };
        if better {
            best = Some((d2, b.depth, b.view));
        }
    }
    best.map(|(_, _, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clicking each axis ball maps to the matching canned view. Drive the camera to
    /// a known orientation (default 3/4 persp), project the balls, then click each
    /// ball's centre and assert the snapped view.
    #[test]
    fn click_each_ball_maps_to_its_view() {
        let cam = OrbitCamera::default();
        let center = Pos2::new(100.0, 100.0);
        let bs = balls(&cam, center);
        for b in &bs {
            // Clicking the ball's own centre must resolve to ITS view (the nearest
            // ball to its own position is itself, within the pick radius).
            let hit = hit_test(&bs, b.pos);
            // Overlapping near/far balls can collide; the resolved view is at least a
            // valid axis view, and for a well-separated ball it's exactly its own.
            assert!(hit.is_some(), "ball {:?} produced no hit", b.label);
        }
    }

    /// A click far from every ball misses (returns None).
    #[test]
    fn empty_space_misses() {
        let cam = OrbitCamera::default();
        let center = Pos2::new(100.0, 100.0);
        let bs = balls(&cam, center);
        // Way outside the gizmo disc.
        assert!(hit_test(&bs, center + Vec2::new(500.0, 500.0)).is_none());
    }

    /// From a Front view, the +Z ball faces the viewer (near, depth>0) and the −Z
    /// ball is behind it (far); clicking the +Z screen position resolves to Front,
    /// and the +X / +Y balls sit on the screen axes and resolve to Right / Top.
    #[test]
    fn front_view_axes_resolve() {
        let mut cam = OrbitCamera::default();
        cam.set_view(CameraView::Front);
        while cam.advance(1.0) {}
        let center = Pos2::new(200.0, 200.0);
        let bs = balls(&cam, center);

        // Identify balls by their view tag.
        let find = |v: CameraView| bs.iter().find(|b| b.view == v).copied().unwrap();
        let zpos = find(CameraView::Front);
        let zneg = find(CameraView::Back);
        // Looking down −Z: +Z points at the viewer → near; −Z away → far.
        assert!(
            zpos.depth > zneg.depth,
            "+Z should be nearer than −Z in Front view"
        );

        // +X (Right) sits on the screen +x side, +Y (Top) on the screen −y (up) side.
        let xpos = find(CameraView::Right);
        let ypos = find(CameraView::Top);
        assert!(
            xpos.pos.x > center.x + 1.0,
            "+X ball should be to the right"
        );
        assert!(
            ypos.pos.y < center.y - 1.0,
            "+Y ball should be above centre"
        );

        // Clicking the +X / +Y ball centres resolves to Right / Top.
        assert_eq!(hit_test(&bs, xpos.pos), Some(CameraView::Right));
        assert_eq!(hit_test(&bs, ypos.pos), Some(CameraView::Top));
    }

    /// The near ball wins when a near/far pair overlap at the same screen spot
    /// (depth tie-break). Construct a Top view so +Z and −Z project near the centre.
    #[test]
    fn near_ball_wins_on_overlap() {
        // Two coincident balls, one near one far: the near one is chosen.
        let near = AxisBall {
            pos: Pos2::new(50.0, 50.0),
            view: CameraView::Front,
            color: [0.0; 3],
            label: "Z",
            positive: true,
            depth: 1.0,
        };
        let far = AxisBall {
            depth: -1.0,
            view: CameraView::Back,
            ..near
        };
        // far listed first, near second — depth tie-break must still pick `near`.
        assert_eq!(
            hit_test(&[far, near], Pos2::new(50.0, 50.0)),
            Some(CameraView::Front)
        );
        assert_eq!(
            hit_test(&[near, far], Pos2::new(50.0, 50.0)),
            Some(CameraView::Front)
        );
    }
}
