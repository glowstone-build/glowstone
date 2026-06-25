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
#[derive(Clone, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
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
    /// Serde-skipped: defaults to false on load, no .archie version bump.
    #[serde(skip)]
    pub ortho: bool,
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
        }
    }
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

    /// Snap the orbit angles to a canned view (keeps target + distance).
    pub fn set_view(&mut self, view: CameraView) {
        let (yaw, pitch) = match view {
            CameraView::Perspective => (0.6, 0.08),
            CameraView::Top => (0.0, Self::PITCH_LIMIT),
            CameraView::Bottom => (0.0, -Self::PITCH_LIMIT),
            CameraView::Front => (0.0, 0.0),
            CameraView::Back => (std::f32::consts::PI, 0.0),
            CameraView::Right => (std::f32::consts::FRAC_PI_2, 0.0),
            CameraView::Left => (-std::f32::consts::FRAC_PI_2, 0.0),
        };
        self.yaw = yaw;
        self.pitch = pitch.clamp(-Self::PITCH_LIMIT, Self::PITCH_LIMIT);
        // Axis views go ortho; Perspective restores persp (Blender AUTOPERSP).
        self.ortho = view != CameraView::Perspective;
    }

    /// numpad-5: pure persp↔ortho toggle, no angle change (Blender
    /// viewpersportho_exec, view3d_edit.cc).
    pub fn toggle_ortho(&mut self) {
        self.ortho = !self.ortho;
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

    /// Frame an explicit AABB (min/max): aim at its centre and dolly to fit.
    pub fn frame_aabb(&mut self, min: Vec3, max: Vec3) {
        let center = (min + max) * 0.5;
        let radius = ((max - min).length() * 0.5).max(0.6);
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
    /// makes the scene follow the cursor.
    pub fn orbit(&mut self, delta_x: f32, delta_y: f32) {
        const SENSITIVITY: f32 = 0.005;
        self.yaw -= delta_x * SENSITIVITY;
        self.pitch = (self.pitch + delta_y * SENSITIVITY)
            .clamp(-Self::PITCH_LIMIT, Self::PITCH_LIMIT);
    }

    /// Frame a bounding sphere (`center`, `radius`): aim at the center and dolly
    /// out far enough to fit it in view. Used after an MVR import to frame the
    /// whole rig instead of the default single-fixture shot.
    pub fn frame(&mut self, center: Vec3, radius: f32) {
        self.target = center;
        let half_fov = (self.fov_y * 0.5).max(0.1);
        // Upper bound tracks the far plane so even a large imported rig fits.
        let max = (self.zfar * 0.9).max(2.0);
        self.distance = (radius / half_fov.sin()).clamp(2.0, max);
    }

    /// Dolly in/out. `scroll` is a wheel delta; positive zooms in.
    pub fn zoom(&mut self, scroll: f32) {
        let factor = (1.0 - scroll * 0.1).clamp(0.5, 1.5);
        self.distance = (self.distance * factor).clamp(0.5, 200.0);
    }

    /// Pan the target across the view plane (right-drag / shift-drag later).
    pub fn pan(&mut self, delta_x: f32, delta_y: f32) {
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
        if self.ortho {
            let h = self.distance * (self.fov_y * 0.5).tan();
            let w = h * aspect;
            let far = self.zfar * 0.5;
            Mat4::orthographic_rh(-w, w, -h, h, -far, far)
        } else {
            Mat4::perspective_rh(self.fov_y, aspect, self.znear, self.zfar)
        }
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
        let mut cam = OrbitCamera::default();
        cam.ortho = true;
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
        let mut cam = OrbitCamera::default();
        cam.ortho = true;
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

    /// An ortho ray through the image centre still hits geometry at the target
    /// (the perspective ray does too — framing is preserved on toggle).
    #[test]
    fn ortho_center_ray_passes_through_target_plane() {
        let mut cam = OrbitCamera::default();
        cam.ortho = true;
        let (o, d) = cam.ray(Vec2::ZERO, 16.0 / 9.0);
        // The target lies on the centre ray: (target - o) is parallel to d.
        let to_target = cam.target - o;
        let along = d * to_target.dot(d);
        assert!((to_target - along).length() < 1e-3);
    }
}
