//! The application: ties together the winit window, the wgpu [`Renderer`], the
//! egui state, and the [`Scene`]. It owns the per-frame update/render loop.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::renderer::Renderer;
use crate::renderer::camera::OrbitCamera;
use crate::scene::Scene;
use crate::ui::Ui;

/// Everything that only exists once the window (and therefore the GPU surface)
/// has been created. winit may call `resumed` before/after this, so it lives
/// behind an `Option`.
struct State {
    window: Arc<Window>,
    renderer: Renderer,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    scene: Scene,
    camera: OrbitCamera,
    ui: Ui,
    last_frame: Instant,
    fps: f32,
}

#[derive(Default)]
pub struct App {
    state: Option<State>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            // Already initialized (e.g. resumed after suspend on mobile).
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("previz — lighting previsualization")
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("failed to create window"),
        );

        // wgpu init is async; block on it once at startup.
        let renderer = pollster::block_on(Renderer::new(window.clone()));

        let egui_ctx = egui::Context::default();
        egui_ctx.set_visuals(egui::Visuals::dark());

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        self.state = Some(State {
            window: window.clone(),
            renderer,
            egui_ctx,
            egui_state,
            scene: Scene::demo(),
            camera: OrbitCamera::default(),
            ui: Ui::new(),
            last_frame: Instant::now(),
            fps: 0.0,
        });

        // Optional GDTF auto-import for testing: PREVIZ_GDTF=path.gdtf loads a
        // fixture, clears the demo scene, and selects it.
        if let Ok(path) = std::env::var("PREVIZ_GDTF") {
            match crate::gdtf::GdtfFixture::load_path(std::path::Path::new(&path)) {
                Ok(fixture) => {
                    let state = self.state.as_mut().unwrap();
                    state.scene.fixtures.clear();
                    let idx = state
                        .scene
                        .add_gdtf(std::sync::Arc::new(fixture), glam::Vec3::new(0.0, 4.0, 0.0));
                    state.scene.fixtures[idx].tilt = 20.0;
                    state.ui.selection = crate::scene::Selection::Fixture(idx);
                    log::info!("imported GDTF: {path}");
                }
                Err(e) => log::error!("GDTF import failed: {e}"),
            }
        }

        // Headless screenshot path: PREVIZ_SCREENSHOT=path.png renders the
        // offscreen 3D view to a PNG and exits (no window needed). Handy for
        // verifying the renderer without a visible window / CI.
        if let Ok(path) = std::env::var("PREVIZ_SCREENSHOT") {
            let state = self.state.as_mut().unwrap();
            let (w, h, pixels) =
                state
                    .renderer
                    .capture(&state.scene, &state.camera, &state.ui.settings);
            match image::RgbaImage::from_raw(w, h, pixels) {
                Some(img) => match img.save(&path) {
                    Ok(()) => log::info!("wrote screenshot {path} ({w}x{h})"),
                    Err(e) => log::error!("failed to write {path}: {e}"),
                },
                None => log::error!("screenshot buffer was the wrong size"),
            }
            event_loop.exit();
            return;
        }

        // Headless benchmark: PREVIZ_BENCH=N times N offscreen frames.
        if let Ok(n) = std::env::var("PREVIZ_BENCH") {
            let n: u32 = n.parse().unwrap_or(120);
            let state = self.state.as_mut().unwrap();
            for _ in 0..10 {
                let _ = state.renderer.capture(&state.scene, &state.camera, &state.ui.settings);
            }
            let t0 = Instant::now();
            for _ in 0..n {
                let _ = state.renderer.capture(&state.scene, &state.camera, &state.ui.settings);
            }
            let per = t0.elapsed().as_secs_f32() / n as f32;
            let (w, h) = state.renderer.viewport.size;
            log::info!(
                "BENCH {w}x{h}: {:.2} ms/frame = {:.0} fps (incl. readback)",
                per * 1000.0,
                1.0 / per
            );
            event_loop.exit();
            return;
        }

        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if window_id != state.window.id() {
            return;
        }

        // egui gets first look at every event (it drives the panels + the
        // viewport's orbit/zoom interaction).
        let response = state.egui_state.on_window_event(&state.window, &event);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.renderer.resize_surface((size.width, size.height));
                state.window.request_redraw();
            }
            WindowEvent::Occluded(false) => {
                // Window is visible again — restart the live-preview loop.
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                // Keep a live preview running, paced by vsync (Fifo) in present.
                // Only re-arm while we're actually presenting, so a minimized or
                // occluded window idles instead of busy-looping.
                if state.render() {
                    state.window.request_redraw();
                }
            }
            _ => {
                if response.repaint {
                    state.window.request_redraw();
                }
            }
        }
    }
}

impl State {
    /// Returns `true` if a frame was presented (see [`Renderer::render`]).
    //
    // `Context::run` is deprecated in favor of `run_ui` (which hands a bare
    // `&mut Ui`), but egui_dock's `DockArea::show` is built around a `&Context`
    // and does the full-window `CentralPanel` wrapping itself — so the ctx-based
    // path is the correct one for a non-eframe app. We opt into the deprecation.
    #[allow(deprecated)]
    fn render(&mut self) -> bool {
        // Frame timing for the FPS HUD (smoothed).
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        if dt > 0.0 {
            let inst = 1.0 / dt;
            self.fps = if self.fps == 0.0 { inst } else { self.fps * 0.9 + inst * 0.1 };
        }
        let fps = self.fps;

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let viewport_texture = self.renderer.viewport.texture_id;
        let egui_ctx = self.egui_ctx.clone();

        // Build the UI. The closure borrows the scene/camera/ui fields; egui_ctx
        // is a separate (cloned) handle so there's no borrow conflict.
        let mut full_output = egui_ctx.run(raw_input, |ctx| {
            self.ui
                .show(ctx, &mut self.scene, &mut self.camera, viewport_texture, fps);
        });

        self.egui_state.handle_platform_output(
            &self.window,
            std::mem::take(&mut full_output.platform_output),
        );

        // Match the offscreen 3D target to the size the viewport panel wants.
        self.renderer.resize_viewport(self.ui.requested_viewport_px);

        let paint_jobs = egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.renderer.config.width, self.renderer.config.height],
            pixels_per_point: full_output.pixels_per_point,
        };

        self.window.pre_present_notify();
        self.renderer.render(
            &self.scene,
            &self.camera,
            self.ui.selection,
            &self.ui.settings,
            &paint_jobs,
            &full_output.textures_delta,
            &screen_descriptor,
        )
    }
}
