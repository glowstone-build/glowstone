//! A progressive still-image **render job**.
//!
//! There is no separate offline renderer: a deliberate render reuses the live
//! pipeline. The job pins the offscreen viewport to the chosen output resolution
//! and the render-quality look ([`RenderConfig::render_settings`]), then lets the
//! ordinary frame loop run with the *scene frozen* (no DMX / motion / camera
//! advance). Because the camera + rig are static, the renderer's temporal
//! volumetric accumulation (EMA) converges a little more each frame — that's the
//! progressive refinement the Render tab shows. After `total` frames the app
//! reads the finished plate back with [`Renderer::read_viewport_ldr`].
//!
//! Owned by `State` (app.rs); this module is just the small state machine.

use std::time::Instant;

use crate::renderer::camera::OrbitCamera;
use crate::scene::{RenderConfig, RenderFormat, RenderSettings};

pub struct RenderJob {
    /// Output resolution of the offscreen render target.
    pub res: (u32, u32),
    /// The render-quality settings the pass uses (native scale, no overlays/selection).
    pub settings: RenderSettings,
    /// A SNAPSHOT of the camera at render start. The render always uses this, so the
    /// user can keep moving the live camera without resetting the render's
    /// accumulation (the live view is separate; Blender's render is camera-locked).
    pub camera: OrbitCamera,
    /// The FROZEN animation clock (seconds) at render start — every accumulation
    /// frame renders the fog + beams at this exact instant, so nothing drifts
    /// mid-render.
    pub anim_time: f32,
    /// Accumulation target: how many static frames to converge over.
    pub total: u32,
    /// Frames recorded so far (only frames that actually rendered advance the EMA).
    pub sample: u32,
    /// Wall-clock start, for the elapsed readout.
    pub started: Instant,
    /// Write the finished image to `out_path` on completion ("Render to image file").
    pub write_on_done: bool,
    pub out_path: String,
    pub format: RenderFormat,
}

impl RenderJob {
    /// `viewport` is the live look ([`RenderSettings`]) the render inherits (shared
    /// color/look — so the render matches the preview).
    pub fn new(
        cfg: &RenderConfig,
        viewport: &RenderSettings,
        camera: &OrbitCamera,
        anim_time: f32,
        started: Instant,
    ) -> Self {
        Self {
            res: cfg.output_size(),
            settings: cfg.render_settings(viewport),
            camera: camera.clone(),
            anim_time,
            total: cfg.max_samples.max(1),
            sample: 0,
            started,
            write_on_done: cfg.write_to_disk && !cfg.out_path.is_empty(),
            out_path: cfg.out_path.clone(),
            format: cfg.format,
        }
    }

    pub fn elapsed_s(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }
}
