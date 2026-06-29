//! Orbit camera and its GPU uniform.
//!
//! The camera math (here) is independent of wgpu; `renderer` owns the actual
//! uniform buffer / bind group and uploads [`CameraUniform`] each frame.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec2, Vec3, Vec4};

/// Camera data as the shaders see it (`grid.wgsl` / `mesh.wgsl` `Camera`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 4],
    /// x = viewport mode (see `ViewportMode::shader_code`); yzw reserved. Only the
    /// mesh shader reads it; grid/lens shaders bind a prefix of this buffer.
    pub render_mode: [f32; 4],
    /// World/HDRI: x = brightness, y = rotation (rad), z = ambient strength,
    /// w = has-HDRI flag (0/1). Read by the mesh IBL ambient + the sky pass.
    pub world: [f32; 4],
    /// Inverse view-projection — the sky pass reconstructs the per-pixel world
    /// ray from NDC. Appended last so grid/lens keep reading a valid prefix.
    pub inv_view_proj: [[f32; 4]; 4],
}

/// A camera that orbits a target point: drag to rotate, scroll to dolly.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OrbitCamera {
    /// Point the camera looks at and orbits around (world space).
    pub target: Vec3,
    /// Horizontal angle around the target, in radians.
    pub yaw: f32,
    /// Vertical angle, in radians. Clamped to avoid flipping over the poles.
    pub pitch: f32,
    /// Distance from the target, in metres.
    pub distance: f32,

    pub fov_y: f32,
    pub znear: f32,
    pub zfar: f32,

    /// Orthographic projection toggle (session UI state — numpad 5 / axis views).
    /// Serde-skipped: defaults to false on load.
    #[serde(skip)]
    pub ortho: bool,

    /// Projection blend used to *animate* the persp↔ortho flip: 0 = full
    /// perspective, 1 = full orthographic. Normally equals `ortho as f32`, but a
    /// running transition eases it across so axis-view jumps don't pop. Session
    /// state only (serde-skipped).
    #[serde(skip)]
    pub ortho_blend: f32,

    /// In-flight eased view transition (canned-view / frame / pie jump). When
    /// `Some`, [`OrbitCamera::advance`] drives the live fields toward the goal.
    /// Session state only (serde-skipped); a fresh load shows the saved pose.
    #[serde(skip)]
    pub anim: Option<CameraAnim>,

    /// Last viewport aspect (width / height) seen by [`OrbitCamera::uniform`].
    /// Cached so framing helpers can widen the fit radius for wide viewports
    /// without threading an aspect arg through every call site. Defaults to 16:9.
    #[serde(skip)]
    pub last_aspect: f32,
}

/// An eased camera transition: snapshot of the pose we left and the pose we want,
/// advanced by real `dt` toward the goal. Mirrors Blender's `SmoothView3DState`
/// (interp source→destination) and Unreal's single eased `FCurveSequence` lerp.
#[derive(Clone, Copy, Debug)]
pub struct CameraAnim {
    // Start pose (the live state when the jump was requested).
    from_target: Vec3,
    from_yaw: f32,
    from_pitch: f32,
    from_distance: f32,
    from_ortho_blend: f32,
    // Goal pose.
    to_target: Vec3,
    to_yaw: f32,
    to_pitch: f32,
    to_distance: f32,
    to_ortho_blend: f32,
    /// Seconds elapsed and total duration.
    elapsed: f32,
    duration: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            // Medium 3/4 shot framing the fixture (~4 m up) and its beam.
            target: Vec3::new(0.0, 2.7, -0.4),
            yaw: 0.6,
            pitch: 0.08,
            distance: 7.5,
            fov_y: 55.0_f32.to_radians(),
            znear: 0.05,
            zfar: 500.0,
            ortho: false,
            ortho_blend: 0.0,
            anim: None,
            last_aspect: 16.0 / 9.0,
        }
    }
}

/// A saved camera pose — the serializable subset of [`OrbitCamera`] a view
/// bookmark (S1) stores: where it looks (`target`), the orbit angles, the dolly
/// `distance`, the `fov_y`, and the `ortho` projection flag. Plain `f32`/array
/// fields so it round-trips cleanly through `bookmarks.json` (and could ride in
/// `.glow` later). Animation/aspect session state is deliberately excluded.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub struct CameraPose {
    pub target: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    #[serde(default)]
    pub ortho: bool,
}

/// Canned orthographic-style viewpoints (set via the View menu / shortcuts).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CameraView {
    Perspective,
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
}

impl CameraView {
    pub const ALL: [CameraView; 7] = [
        Self::Perspective,
        Self::Top,
        Self::Bottom,
        Self::Front,
        Self::Back,
        Self::Left,
        Self::Right,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Perspective => "Perspective",
            Self::Top => "Top",
            Self::Bottom => "Bottom",
            Self::Front => "Front",
            Self::Back => "Back",
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }
}

impl OrbitCamera {
    /// Keep pitch a hair away from straight up/down.
    const PITCH_LIMIT: f32 = 1.5533; // ~89 degrees in radians

    /// Default transition length, seconds. Short enough to feel snappy on canned
    /// jumps, long enough to read the motion (Blender ~smooth_viewtx, UE CubicOut).
    const ANIM_SECS: f32 = 0.25;

    /// Begin an eased transition toward an explicit goal pose. The live fields are
    /// the start; the animation interpolates from here to the goal over
    /// [`Self::ANIM_SECS`]. Yaw takes the shortest arc; ortho flips blend across.
    /// A near-identical goal applies instantly (no animation churn).
    fn animate_to(
        &mut self,
        to_target: Vec3,
        to_yaw: f32,
        to_pitch: f32,
        to_distance: f32,
        to_ortho: bool,
    ) {
        let to_pitch = to_pitch.clamp(-Self::PITCH_LIMIT, Self::PITCH_LIMIT);
        let to_distance = to_distance.clamp(0.5, 200.0);
        let to_ortho_blend = if to_ortho { 1.0 } else { 0.0 };
        // Unwrap the goal yaw to the revolution nearest the current yaw so the
        // shortest arc is taken instead of spinning the long way round.
        let to_yaw = self.yaw + shortest_angle(self.yaw, to_yaw);

        // If we're already essentially there, snap (and clear any stale anim).
        let close = (to_target - self.target).length() < 1e-3
            && shortest_angle(self.yaw, to_yaw).abs() < 1e-3
            && (to_pitch - self.pitch).abs() < 1e-3
            && (to_distance - self.distance).abs() < 1e-3
            && (to_ortho_blend - self.ortho_blend).abs() < 1e-3;
        self.ortho = to_ortho;
        if close {
            self.target = to_target;
            self.yaw = to_yaw;
            self.pitch = to_pitch;
            self.distance = to_distance;
            self.ortho_blend = to_ortho_blend;
            self.anim = None;
            return;
        }
        self.anim = Some(CameraAnim {
            from_target: self.target,
            from_yaw: self.yaw,
            from_pitch: self.pitch,
            from_distance: self.distance,
            from_ortho_blend: self.ortho_blend,
            to_target,
            to_yaw,
            to_pitch,
            to_distance,
            to_ortho_blend,
            elapsed: 0.0,
            duration: Self::ANIM_SECS,
        });
    }

    /// Advance any in-flight transition by `dt` seconds, writing the eased pose
    /// into the live fields. Returns `true` while an animation is running (the
    /// caller should keep requesting redraws). Cheap no-op when idle.
    pub fn advance(&mut self, dt: f32) -> bool {
        let Some(a) = self.anim.as_mut() else {
            return false;
        };
        a.elapsed += dt.max(0.0);
        let t = (a.elapsed / a.duration).clamp(0.0, 1.0);
        let e = ease_out_cubic(t);
        self.target = a.from_target.lerp(a.to_target, e);
        self.yaw = a.from_yaw + (a.to_yaw - a.from_yaw) * e;
        self.pitch = a.from_pitch + (a.to_pitch - a.from_pitch) * e;
        // Distance eases geometrically (constant perceived zoom rate).
        self.distance = a.from_distance * (a.to_distance / a.from_distance).powf(e);
        self.ortho_blend = a.from_ortho_blend + (a.to_ortho_blend - a.from_ortho_blend) * e;
        if t >= 1.0 {
            // Land exactly on the goal, then retire the animation.
            self.target = a.to_target;
            self.yaw = a.to_yaw;
            self.pitch = a.to_pitch;
            self.distance = a.to_distance;
            self.ortho_blend = a.to_ortho_blend;
            self.anim = None;
            return false;
        }
        true
    }

    /// The (yaw, pitch) orbit angles for a canned view.
    fn view_angles(view: CameraView) -> (f32, f32) {
        match view {
            CameraView::Perspective => (0.6, 0.08),
            CameraView::Top => (0.0, Self::PITCH_LIMIT),
            CameraView::Bottom => (0.0, -Self::PITCH_LIMIT),
            CameraView::Front => (0.0, 0.0),
            CameraView::Back => (std::f32::consts::PI, 0.0),
            CameraView::Right => (std::f32::consts::FRAC_PI_2, 0.0),
            CameraView::Left => (-std::f32::consts::FRAC_PI_2, 0.0),
        }
    }

    /// Finish any in-flight transition instantly (no interpolation). Headless
    /// capture paths use this so a one-shot screenshot lands on the final pose
    /// instead of the animation's first frame.
    pub fn skip_anim(&mut self) {
        if self.anim.is_some() {
            // A single huge step snaps to the goal and retires the animation.
            self.advance(f32::MAX);
        }
    }

    /// Animate the orbit angles to a canned view (keeps target + distance).
    pub fn set_view(&mut self, view: CameraView) {
        let (yaw, pitch) = Self::view_angles(view);
        // Axis views go ortho; Perspective restores persp (Blender AUTOPERSP).
        let ortho = view != CameraView::Perspective;
        self.animate_to(self.target, yaw, pitch, self.distance, ortho);
    }

    /// Snapshot the live orbit pose — target / yaw / pitch / distance / fov /
    /// ortho — as a plain [`CameraPose`]. Used by view bookmarks (S1) to save the
    /// current shot to a numbered slot. Excludes session-only animation state.
    pub fn pose(&self) -> CameraPose {
        CameraPose {
            target: self.target.to_array(),
            yaw: self.yaw,
            pitch: self.pitch,
            distance: self.distance,
            fov_y: self.fov_y,
            ortho: self.ortho,
        }
    }

    /// Recall a saved [`CameraPose`] (view bookmark): eases the live camera to the
    /// stored target / angles / distance / projection via the shared `animate_to`
    /// transition, and restores the saved FOV. A near-identical pose snaps (no
    /// animation churn), exactly like a canned-view jump.
    pub fn apply_pose(&mut self, p: &CameraPose) {
        self.fov_y = p.fov_y;
        self.animate_to(
            Vec3::from_array(p.target),
            p.yaw,
            p.pitch,
            p.distance,
            p.ortho,
        );
    }

    /// numpad-5: pure persp↔ortho toggle, no angle change (Blender
    /// viewpersportho_exec, view3d_edit.cc). Eases the projection blend.
    pub fn toggle_ortho(&mut self) {
        let to = !self.ortho;
        self.animate_to(self.target, self.yaw, self.pitch, self.distance, to);
    }

    /// The corner viewport tag, Blender-style: projection + axis-view name when the
    /// current orbit angles snap to a canned axis view (e.g. "Ortho · Front"), else
    /// just the projection + "User" (free orbit). Used by the viewport overlay.
    pub fn view_tag(&self) -> String {
        let proj = if self.ortho { "Ortho" } else { "Persp" };
        // Match the current yaw/pitch against the canned axis views (small epsilon).
        const EPS: f32 = 0.02;
        let approx = |a: f32, b: f32| (a - b).abs() < EPS;
        let name = CameraView::ALL.iter().copied().find(|&v| {
            if v == CameraView::Perspective {
                return false;
            }
            let (yaw, pitch) = Self::view_angles(v);
            // Compare on the wrapped yaw delta so e.g. Back (π) matches -π too.
            shortest_angle(self.yaw, yaw).abs() < EPS && approx(pitch, self.pitch)
        });
        match name {
            Some(v) => format!("{proj} · {}", v.label()),
            None => format!("{proj} · User"),
        }
    }

    /// numpad 2/4/6/8: orbit by a fixed step (degrees). Reuses `orbit()`'s sign
    /// convention by feeding pixel-equivalent deltas (deg→rad / SENSITIVITY).
    pub fn orbit_step(&mut self, yaw_deg: f32, pitch_deg: f32) {
        const SENSITIVITY: f32 = 0.005; // keep in sync with orbit()
        self.orbit(
            yaw_deg.to_radians() / SENSITIVITY,
            pitch_deg.to_radians() / SENSITIVITY,
        );
    }

    /// Frame an explicit AABB (min/max): aim at its centre and dolly to fit. A
    /// wide viewport gets a wider fit radius so a wide selection isn't clipped at
    /// the sides (the vertical FOV is the limiting axis; widen by aspect>1).
    pub fn frame_aabb(&mut self, min: Vec3, max: Vec3) {
        let center = (min + max) * 0.5;
        let mut radius = ((max - min).length() * 0.5).max(0.6);
        // Aspect correction: when the viewport is wider than tall, the horizontal
        // FOV exceeds the vertical one, so a sphere sized for the vertical FOV
        // over-fills width-wise only if narrow — but a *wide* selection needs the
        // radius scaled by the aspect to keep its extents inside the narrower
        // vertical FOV. Only widen (aspect>1); never tighten a tall viewport.
        let aspect = self.last_aspect.max(0.0001);
        if aspect > 1.0 {
            radius *= aspect;
        }
        self.frame(center, radius * 1.1);
    }

    /// World-space eye position derived from the orbit angles.
    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let offset = Vec3::new(cp * sy, sp, cp * cy) * self.distance;
        self.target + offset
    }

    /// Rotate around the target. `delta` is in pixels (drag delta); the sign
    /// makes the scene follow the cursor. Manual orbit cancels any running
    /// transition (the user grabbed control) without snapping the pose.
    pub fn orbit(&mut self, delta_x: f32, delta_y: f32) {
        const SENSITIVITY: f32 = 0.005;
        self.anim = None;
        self.yaw -= delta_x * SENSITIVITY;
        self.pitch =
            (self.pitch + delta_y * SENSITIVITY).clamp(-Self::PITCH_LIMIT, Self::PITCH_LIMIT);
    }

    /// Frame a bounding sphere (`center`, `radius`): aim at the center and dolly
    /// out far enough to fit it in view. Used after an MVR import to frame the
    /// whole rig instead of the default single-fixture shot.
    pub fn frame(&mut self, center: Vec3, radius: f32) {
        let half_fov = (self.fov_y * 0.5).max(0.1);
        // Upper bound tracks the far plane so even a large imported rig fits.
        let max = (self.zfar * 0.9).max(2.0);
        let distance = (radius / half_fov.sin()).clamp(2.0, max);
        // Animate to the new target+distance (keeps the current orbit angles +
        // projection). Set-view/pie use animate_to directly; framing reuses it.
        self.animate_to(center, self.yaw, self.pitch, distance, self.ortho);
    }

    /// Dolly in/out. `scroll` is a wheel delta; positive zooms in. When an
    /// `anchor` world point is given (the point under the cursor), the target
    /// slides toward/away from it as we dolly, so the cursor's ground point stays
    /// put — "zoom to cursor" (Blender `zoom_to_pos`). Without an anchor it's a
    /// plain dolly toward the existing target.
    pub fn zoom(&mut self, scroll: f32, anchor: Option<Vec3>) {
        self.anim = None;
        let factor = (1.0 - scroll * 0.1).clamp(0.5, 1.5);
        let new_distance = (self.distance * factor).clamp(0.5, 200.0);
        if let Some(anchor) = anchor {
            // Move the target a fraction of the way to (or from) the anchor equal
            // to the fraction the distance changed: the anchor's screen position
            // is preserved as the frustum scales about it.
            let shrink = new_distance / self.distance; // <1 zooming in
            self.target = anchor + (self.target - anchor) * shrink;
        }
        self.distance = new_distance;
    }

    /// Pan the target across the view plane (right-drag / shift-drag later).
    pub fn pan(&mut self, delta_x: f32, delta_y: f32) {
        self.anim = None;
        let forward = (self.target - self.eye()).normalize();
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward).normalize();
        let speed = self.distance * 0.0015;
        self.target += (-right * delta_x + up * delta_y) * speed;
    }

    /// The camera's world-space basis: (right, up, forward). Used to map a
    /// screen-space drag onto the view plane (modal transforms).
    pub fn view_basis(&self) -> (Vec3, Vec3, Vec3) {
        let forward = (self.target - self.eye()).normalize_or_zero();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        (right, up, forward)
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    /// Right-handed, reverse-Z-free perspective with the `0..1` depth range
    /// wgpu expects (`perspective_rh`, not the `_gl` variant). When `ortho`, an
    /// orthographic_rh sized so the framed content matches the perspective
    /// framing at the same distance: half-height `= distance·tan(fov_y/2)` (the
    /// extent the perspective frustum spans at the target plane). Near/far are
    /// symmetric about the target (Blender camera.cc `clip_start = -clip_end`) so
    /// geometry between eye and target isn't front-clipped.
    pub fn proj_matrix(&self, aspect: f32) -> Mat4 {
        let aspect = aspect.max(0.0001);
        let persp = Mat4::perspective_rh(self.fov_y, aspect, self.znear, self.zfar);
        // During a transition the eased `ortho_blend` drives the cross-fade;
        // at rest the logical `ortho` flag is authoritative (so a direct
        // `cam.ortho = true` still gives a pure ortho projection). The common
        // path returns one pure projection without building the other matrix.
        let blend = if self.anim.is_some() {
            self.ortho_blend.clamp(0.0, 1.0)
        } else if self.ortho {
            1.0
        } else {
            0.0
        };
        if blend <= 0.0 {
            return persp;
        }
        let h = self.distance * (self.fov_y * 0.5).tan();
        let w = h * aspect;
        let far = self.zfar * 0.5;
        let ortho = Mat4::orthographic_rh(-w, w, -h, h, -far, far);
        if blend >= 1.0 {
            return ortho;
        }
        // Element-wise lerp of the two projections — a cheap, monotone cross-fade
        // that reads as a smooth flip (a true frustum morph is overkill here).
        Mat4::from_cols(
            persp.x_axis.lerp(ortho.x_axis, blend),
            persp.y_axis.lerp(ortho.y_axis, blend),
            persp.z_axis.lerp(ortho.z_axis, blend),
            persp.w_axis.lerp(ortho.w_axis, blend),
        )
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj_matrix(aspect) * self.view_matrix()
    }

    /// Build a world-space picking ray for a normalized-device-coordinate point
    /// (`ndc` in `-1..1`, y up). Returns `(origin, unit direction)`.
    pub fn ray(&self, ndc: Vec2, aspect: f32) -> (Vec3, Vec3) {
        // Inverse-VP unproject works for BOTH persp + ortho: an ortho
        // inv_view_proj yields parallel rays (origin varies across the image
        // plane, direction constant = forward). Do NOT special-case ortho here.
        let inv = self.view_proj(aspect).inverse();
        let near = inv * Vec4::new(ndc.x, ndc.y, 0.0, 1.0);
        let far = inv * Vec4::new(ndc.x, ndc.y, 1.0, 1.0);
        let np = near.truncate() / near.w;
        let fp = far.truncate() / far.w;
        (np, (fp - np).normalize_or_zero())
    }

    /// Project a world-space point to a screen position inside `rect` using a
    /// precomputed `view_proj` (`vp`). Returns `None` when the point is behind the
    /// camera (`w <= 0`). Mirrors the fixture-label math in `panels::viewport`; the
    /// screen-space move gizmo uses it to place + hit-test its axis handles.
    pub fn project_to_screen(point: Vec3, vp: Mat4, rect: egui::Rect) -> Option<egui::Pos2> {
        let clip = vp * point.extend(1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(egui::pos2(
            rect.min.x + (ndc.x * 0.5 + 0.5) * rect.width(),
            rect.min.y + (0.5 - ndc.y * 0.5) * rect.height(),
        ))
    }

    /// Record the live viewport aspect (width / height) so framing helpers can
    /// widen the fit for wide viewports. Call once per frame from the panel that
    /// owns the viewport rect (it has both the rect and a `&mut` camera).
    pub fn set_aspect(&mut self, aspect: f32) {
        if aspect.is_finite() && aspect > 0.0 {
            self.last_aspect = aspect;
        }
    }

    pub fn uniform(&self, aspect: f32) -> CameraUniform {
        let eye = self.eye();
        let vp = self.view_proj(aspect);
        CameraUniform {
            view_proj: vp.to_cols_array_2d(),
            eye: Vec4::new(eye.x, eye.y, eye.z, 1.0).to_array(),
            render_mode: [0.0; 4],
            world: [0.0; 4],
            inv_view_proj: vp.inverse().to_cols_array_2d(),
        }
    }
}

/// Cubic ease-out (Unreal `ECurveEaseFunction::CubicOut`): fast start, gentle
/// settle. `1 - (1-t)^3` on `t ∈ [0,1]`.
#[inline]
fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// The signed shortest angular delta (radians) to rotate `from` onto `to`, in
/// `(-π, π]`. Used so a yaw jump takes the short arc instead of unwinding the
/// long way (Blender quaternion shortest-path, expressed on the yaw scalar).
#[inline]
fn shortest_angle(from: f32, to: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let mut d = (to - from) % TAU;
    if d > PI {
        d -= TAU;
    } else if d < -PI {
        d += TAU;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis views switch to ortho; Perspective restores persp (Blender AUTOPERSP).
    #[test]
    fn axis_views_go_ortho_persp_restores() {
        let mut cam = OrbitCamera::default();
        assert!(!cam.ortho);
        cam.set_view(CameraView::Front);
        assert!(cam.ortho);
        cam.set_view(CameraView::Top);
        assert!(cam.ortho);
        cam.set_view(CameraView::Perspective);
        assert!(!cam.ortho);
    }

    #[test]
    fn toggle_ortho_flips_without_changing_angles() {
        let mut cam = OrbitCamera::default();
        let (yaw, pitch) = (cam.yaw, cam.pitch);
        cam.toggle_ortho();
        assert!(cam.ortho);
        assert_eq!((cam.yaw, cam.pitch), (yaw, pitch));
        cam.toggle_ortho();
        assert!(!cam.ortho);
    }

    /// The ortho projection is parallel: w is constant across the clip volume, so
    /// the proj matrix has no perspective divide (last row = (0,0,0,1)).
    #[test]
    fn ortho_proj_is_parallel() {
        let mut cam = OrbitCamera {
            ortho: true,
            ..Default::default()
        };
        let p = cam.proj_matrix(16.0 / 9.0);
        let row3 = p.row(3);
        assert!((row3.x).abs() < 1e-6);
        assert!((row3.y).abs() < 1e-6);
        assert!((row3.z).abs() < 1e-6);
        assert!((row3.w - 1.0).abs() < 1e-6);
        // Persp, by contrast, has a perspective divide (last row z = -1).
        cam.ortho = false;
        let pp = cam.proj_matrix(16.0 / 9.0);
        assert!((pp.row(3).z + 1.0).abs() < 1e-6);
    }

    /// In ortho, picking rays are parallel: different image points give the same
    /// direction (= forward) but distinct origins.
    #[test]
    fn ortho_rays_are_parallel() {
        let cam = OrbitCamera {
            ortho: true,
            ..Default::default()
        };
        let aspect = 16.0 / 9.0;
        let (o0, d0) = cam.ray(Vec2::new(-0.5, -0.5), aspect);
        let (o1, d1) = cam.ray(Vec2::new(0.5, 0.5), aspect);
        // Parallel: directions equal.
        assert!((d0 - d1).length() < 1e-4, "dirs differ: {d0} vs {d1}");
        // Direction matches the camera forward.
        let (_, _, fwd) = cam.view_basis();
        assert!((d0 - fwd).length() < 1e-4, "dir not forward: {d0} vs {fwd}");
        // Distinct origins across the image plane.
        assert!((o0 - o1).length() > 0.1, "origins not spread: {o0} vs {o1}");
    }

    /// orbit_step nudges the angles by the requested degrees (the numpad path).
    #[test]
    fn orbit_step_changes_angles_by_degrees() {
        let mut cam = OrbitCamera::default();
        let (y0, p0) = (cam.yaw, cam.pitch);
        cam.orbit_step(0.0, 15.0);
        // pitch up by 15°; yaw unchanged.
        assert!((cam.pitch - (p0 + 15.0_f32.to_radians())).abs() < 1e-4);
        assert!((cam.yaw - y0).abs() < 1e-4);
        cam.orbit_step(-15.0, 0.0);
        // orbit() subtracts yaw, but orbit_step feeds a deg→delta that, with the
        // negative arg, increases yaw by 15°.
        assert!((cam.yaw - (y0 + 15.0_f32.to_radians())).abs() < 1e-4);
    }

    /// Drive any in-flight transition to completion (one big dt step).
    fn settle(cam: &mut OrbitCamera) {
        while cam.advance(1.0) {}
    }

    /// The corner tag reports the projection + the snapped axis-view name (or
    /// User), once the eased transition the setters start has settled.
    #[test]
    fn view_tag_reports_proj_and_view() {
        let mut cam = OrbitCamera::default();
        cam.set_view(CameraView::Front);
        settle(&mut cam);
        assert_eq!(cam.view_tag(), "Ortho · Front");
        cam.toggle_ortho();
        settle(&mut cam);
        assert_eq!(cam.view_tag(), "Persp · Front");
        cam.set_view(CameraView::Perspective);
        settle(&mut cam);
        assert_eq!(cam.view_tag(), "Persp · User");
        // A free orbit off any axis reads as User.
        cam.set_view(CameraView::Top);
        settle(&mut cam);
        cam.orbit_step(7.0, 0.0);
        assert!(cam.view_tag().ends_with("User"));
    }

    /// ease_out_cubic is pinned at the ends and monotone in between.
    #[test]
    fn ease_out_cubic_endpoints_and_monotone() {
        assert!((ease_out_cubic(0.0) - 0.0).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        let mut prev = 0.0;
        for k in 1..=20 {
            let v = ease_out_cubic(k as f32 / 20.0);
            assert!(v >= prev, "not monotone at {k}: {v} < {prev}");
            prev = v;
        }
        // Ease-OUT: more than half the distance covered by the midpoint.
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    /// shortest_angle takes the short arc and stays in (-π, π].
    #[test]
    fn shortest_angle_takes_short_arc() {
        use std::f32::consts::PI;
        // 350° → really -10°, not +350°.
        let d = shortest_angle(0.0, 350.0_f32.to_radians());
        assert!((d - (-10.0_f32).to_radians()).abs() < 1e-4, "got {d}");
        // Symmetric.
        assert!(shortest_angle(0.0, PI - 0.01).abs() <= PI);
        assert!(shortest_angle(0.0, -PI + 0.01).abs() <= PI);
    }

    /// A canned-view jump lands EXACTLY on the target pose after the animation
    /// completes (eased start→goal end-state, not the in-flight values).
    #[test]
    fn anim_lands_on_target_pose() {
        let mut cam = OrbitCamera::default();
        cam.set_view(CameraView::Top);
        // Mid-flight: not yet at the goal.
        cam.advance(OrbitCamera::ANIM_SECS * 0.5);
        assert!(cam.anim.is_some());
        let (gy, gp) = OrbitCamera::view_angles(CameraView::Top);
        assert!(
            (cam.pitch - gp).abs() > 1e-3,
            "should not be there mid-flight"
        );
        // Settle.
        settle(&mut cam);
        assert!(cam.anim.is_none(), "animation should retire");
        assert!((cam.yaw - gy).abs() < 1e-4);
        assert!((cam.pitch - gp).abs() < 1e-4);
        assert!(cam.ortho);
        assert!((cam.ortho_blend - 1.0).abs() < 1e-4);
    }

    /// frame() eases the distance/target and ends exactly framed.
    #[test]
    fn frame_eases_and_lands() {
        let mut cam = OrbitCamera::default();
        let center = Vec3::new(10.0, 1.0, -3.0);
        cam.frame(center, 5.0);
        assert!(cam.anim.is_some());
        settle(&mut cam);
        assert!((cam.target - center).length() < 1e-3);
        // Distance fits the radius for the vertical FOV.
        let want = 5.0 / (cam.fov_y * 0.5).max(0.1).sin();
        assert!(
            (cam.distance - want).abs() < 1e-2,
            "{} vs {want}",
            cam.distance
        );
    }

    /// frame_aabb widens the fit radius for a wide viewport (aspect>1) and leaves
    /// a tall viewport (aspect<1) untouched — the aspect-correction rule.
    #[test]
    fn frame_aabb_widens_for_wide_viewport() {
        let lo = Vec3::new(-2.0, -1.0, -1.0);
        let hi = Vec3::new(2.0, 1.0, 1.0);
        let mut wide = OrbitCamera::default();
        wide.set_aspect(2.0);
        wide.frame_aabb(lo, hi);
        settle(&mut wide);

        let mut tall = OrbitCamera::default();
        tall.set_aspect(0.5);
        tall.frame_aabb(lo, hi);
        settle(&mut tall);

        // Wider viewport ⇒ camera pulls further back so the wide AABB fits.
        assert!(
            wide.distance > tall.distance,
            "{} !> {}",
            wide.distance,
            tall.distance
        );
    }

    /// Zoom-to-cursor keeps the anchor world point fixed on screen: after a dolly
    /// about the anchor, the anchor still projects to the same NDC.
    #[test]
    fn zoom_to_cursor_keeps_anchor_fixed_on_screen() {
        let mut cam = OrbitCamera::default();
        let aspect = 16.0 / 9.0;
        // Pick a world point that's NOT the target so the test is meaningful.
        let anchor = cam.target + Vec3::new(1.5, -0.7, 0.9);
        let before = cam.view_proj(aspect) * anchor.extend(1.0);
        let ndc_before = before.truncate() / before.w;

        cam.zoom(1.0, Some(anchor)); // dolly in toward the cursor anchor

        let after = cam.view_proj(aspect) * anchor.extend(1.0);
        let ndc_after = after.truncate() / after.w;
        // x/y NDC of the anchor are preserved (z/depth naturally changes).
        assert!(
            (ndc_before.x - ndc_after.x).abs() < 1e-3,
            "x moved: {} vs {}",
            ndc_before.x,
            ndc_after.x
        );
        assert!(
            (ndc_before.y - ndc_after.y).abs() < 1e-3,
            "y moved: {} vs {}",
            ndc_before.y,
            ndc_after.y
        );
        // And we actually got closer to the anchor.
        assert!(cam.distance < OrbitCamera::default().distance);
    }

    /// A plain zoom (no anchor) dollies toward the existing target, leaving it put.
    #[test]
    fn zoom_without_anchor_keeps_target() {
        let mut cam = OrbitCamera::default();
        let t0 = cam.target;
        cam.zoom(1.0, None);
        assert!((cam.target - t0).length() < 1e-6);
        assert!(cam.distance < OrbitCamera::default().distance);
    }

    /// A saved pose recalls EXACTLY: `apply_pose` eases the live camera to the
    /// stored target / angles / distance / fov / projection (the bookmark path).
    #[test]
    fn pose_recall_sets_target_pose() {
        // Author a distinct pose to save.
        let cam = OrbitCamera {
            target: Vec3::new(4.0, 1.5, -2.0),
            yaw: 1.1,
            pitch: 0.3,
            distance: 12.0,
            fov_y: 0.7,
            ortho: true,
            ..Default::default()
        };
        let saved = cam.pose();

        // Move the camera somewhere else, then recall.
        let mut other = OrbitCamera::default();
        other.apply_pose(&saved);
        settle(&mut other);
        assert!((Vec3::from_array(saved.target) - other.target).length() < 1e-4);
        assert!((other.yaw - saved.yaw).abs() < 1e-4);
        assert!((other.pitch - saved.pitch).abs() < 1e-4);
        assert!((other.distance - saved.distance).abs() < 1e-3);
        assert!((other.fov_y - saved.fov_y).abs() < 1e-6);
        assert!(other.ortho);
    }

    /// A pose round-trips through JSON unchanged (the bookmark-persistence path).
    #[test]
    fn pose_round_trips_through_json() {
        let cam = OrbitCamera {
            target: Vec3::new(-1.0, 9.0, 3.0),
            yaw: -0.4,
            distance: 22.0,
            ..Default::default()
        };
        let p = cam.pose();
        let text = serde_json::to_string(&p).unwrap();
        let back: CameraPose = serde_json::from_str(&text).unwrap();
        assert_eq!(p, back);
    }

    /// An ortho ray through the image centre still hits geometry at the target
    /// (the perspective ray does too — framing is preserved on toggle).
    #[test]
    fn ortho_center_ray_passes_through_target_plane() {
        let cam = OrbitCamera {
            ortho: true,
            ..Default::default()
        };
        let (o, d) = cam.ray(Vec2::ZERO, 16.0 / 9.0);
        // The target lies on the centre ray: (target - o) is parallel to d.
        let to_target = cam.target - o;
        let along = d * to_target.dot(d);
        assert!((to_target - along).length() < 1e-3);
    }
}
