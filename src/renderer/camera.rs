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
}

/// A camera that orbits a target point: drag to rotate, scroll to dolly.
#[derive(Clone, Debug)]
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
        }
    }
}

impl OrbitCamera {
    /// Keep pitch a hair away from straight up/down.
    const PITCH_LIMIT: f32 = 1.5533; // ~89 degrees in radians

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

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    /// Right-handed, reverse-Z-free perspective with the `0..1` depth range
    /// wgpu expects (`perspective_rh`, not the `_gl` variant).
    pub fn proj_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y, aspect.max(0.0001), self.znear, self.zfar)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj_matrix(aspect) * self.view_matrix()
    }

    /// Build a world-space picking ray for a normalized-device-coordinate point
    /// (`ndc` in `-1..1`, y up). Returns `(origin, unit direction)`.
    pub fn ray(&self, ndc: Vec2, aspect: f32) -> (Vec3, Vec3) {
        let inv = self.view_proj(aspect).inverse();
        let near = inv * Vec4::new(ndc.x, ndc.y, 0.0, 1.0);
        let far = inv * Vec4::new(ndc.x, ndc.y, 1.0, 1.0);
        let np = near.truncate() / near.w;
        let fp = far.truncate() / far.w;
        (np, (fp - np).normalize_or_zero())
    }

    pub fn uniform(&self, aspect: f32) -> CameraUniform {
        let eye = self.eye();
        CameraUniform {
            view_proj: self.view_proj(aspect).to_cols_array_2d(),
            eye: Vec4::new(eye.x, eye.y, eye.z, 1.0).to_array(),
        }
    }
}
