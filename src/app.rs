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
    /// Whether to keep driving redraws (false while the window is occluded), so
    /// the live preview animates continuously instead of only on input.
    awake: bool,
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
            awake: true,
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
                    state.ui.selection = crate::scene::Selection::fixture(idx);
                    log::info!("imported GDTF: {path}");
                }
                Err(e) => log::error!("GDTF import failed: {e}"),
            }
        }

        // Headless optical contact sheet: PREVIZ_SHEET=dir (with PREVIZ_GDTF)
        // renders one screenshot per optical feature so the whole chain can be
        // verified without the UI. Dev harness, like PREVIZ_BENCH.
        if let Ok(dir) = std::env::var("PREVIZ_SHEET") {
            render_optics_sheet(self.state.as_mut().unwrap(), &dir);
            event_loop.exit();
            return;
        }

        // Headless animation check: PREVIZ_ANIM=dir (with PREVIZ_GDTF) sets a
        // spinning gobo / animation / colour / prism and renders a frame
        // sequence, advancing the scene between frames — to verify wheel motion.
        if let Ok(dir) = std::env::var("PREVIZ_ANIM") {
            render_anim_sequence(self.state.as_mut().unwrap(), &dir);
            event_loop.exit();
            return;
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
            WindowEvent::Occluded(occluded) => {
                // Idle while hidden; resume the continuous loop when visible.
                state.awake = !occluded;
                if !occluded {
                    state.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                // Render the frame; the next redraw is re-armed in `about_to_wait`
                // (requesting a redraw from inside RedrawRequested is unreliable
                // on some platforms, which froze the haze/wheel animation).
                state.render();
            }
            _ => {
                if response.repaint {
                    state.window.request_redraw();
                }
            }
        }
    }

    /// Re-arm the next frame so the live preview animates continuously (haze
    /// drift, wheel spin, gobo scroll) without needing input. Paced by vsync
    /// (the Fifo present in `render`). Idles while occluded.
    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state
            && state.awake
        {
            state.window.request_redraw();
        }
    }
}

/// Render one screenshot per optical feature into `dir` (dev verification of the
/// full beam chain). Requires a GDTF fixture in the scene (set `PREVIZ_GDTF`).
fn render_optics_sheet(state: &mut State, dir: &str) {
    use crate::optics::WheelMotion;

    let _ = std::fs::create_dir_all(dir);
    let Some(idx) = state.scene.fixtures.iter().position(|f| f.is_gdtf()) else {
        log::error!("PREVIZ_SHEET needs a GDTF fixture (set PREVIZ_GDTF)");
        return;
    };
    // Heavier haze for the contact sheet so the beam-in-fog look is visible.
    if let Some(env) = state.scene.environments.first_mut() {
        env.density = 0.14;
    }

    // Each preset configures the fixture's optics + a fixed motion phase.
    let presets: [(&str, fn(&mut crate::scene::Fixture)); 15] = [
        ("01_neutral", |_f| {}),
        ("02_gobo_target", |f| { f.optics.gobo1 = 5.0 / 6.0; f.optics.zoom = 0.25; }),
        ("03_gobo_vortex_spin", |f| {
            f.optics.gobo1 = 4.0 / 6.0; f.optics.gobo1_rot = 0.85; f.optics.zoom = 0.25;
            f.motion.gobo1_angle = 0.8;
        }),
        ("04_gobo2_smokerings", |f| { f.optics.gobo2 = 2.0 / 6.0; f.optics.zoom = 0.25; }),
        ("05_color_red", |f| { f.optics.color = 1.0; }),
        ("06_cmy_magenta", |f| { f.optics.cmy = [0.0, 0.85, 0.0]; }),
        ("07_cto_warm", |f| { f.optics.cto = 1.0; }),
        ("08_prism5", |f| { f.optics.prism1 = 1.0; f.optics.zoom = 0.0; }),
        ("08b_prism_gobo", |f| { f.optics.prism1 = 1.0; f.optics.gobo1 = 5.0 / 6.0; f.optics.zoom = 0.0; }),
        ("09_frost", |f| { f.optics.frost = 0.85; f.optics.gobo1 = 5.0 / 6.0; f.optics.zoom = 0.25; }),
        ("10_zoom_narrow", |f| { f.optics.zoom = 0.0; }),
        ("11_iris_closed", |f| { f.optics.iris = 0.25; }),
        ("12_animation", |f| {
            f.optics.anim = 1.0; f.optics.anim_spin = 0.9; f.optics.gobo1 = 5.0 / 6.0;
            f.optics.zoom = 0.25; f.motion.anim_scroll = 0.3;
        }),
        ("13_chromatic_ab", |f| { f.optics.ca = 1.0; f.optics.gobo1 = 5.0 / 6.0; f.optics.zoom = 0.12; }),
        ("14_combo", |f| {
            // Color + gobo + prism + frost together (stages compose).
            f.optics.cmy = [0.6, 0.0, 0.0];     // cyan tint
            f.optics.gobo1 = 4.0 / 6.0;          // vortex
            f.optics.prism1 = 1.0;               // 5-facet fan
            f.optics.frost = 0.15;
            f.optics.zoom = 0.18;
            f.motion.prism1_angle = 0.4;
        }),
    ];

    for (name, apply) in presets {
        {
            let f = &mut state.scene.fixtures[idx];
            f.optics = Default::default();
            f.motion = WheelMotion::default();
            f.pan = 0.0;
            f.tilt = 28.0;
            apply(f);
        }
        let (w, h, px) = state
            .renderer
            .capture(&state.scene, &state.camera, &state.ui.settings);
        match image::RgbaImage::from_raw(w, h, px) {
            Some(img) => {
                let path = format!("{dir}/sheet_{name}.png");
                match img.save(&path) {
                    Ok(()) => log::info!("sheet: wrote {path}"),
                    Err(e) => log::error!("sheet: {path}: {e}"),
                }
            }
            None => log::error!("sheet: bad buffer for {name}"),
        }
    }

    // Overhead prism shot: confirm the facet copies separate into distinct pools.
    {
        let f = &mut state.scene.fixtures[idx];
        f.optics = Default::default();
        f.tilt = 0.0; // straight down
        f.optics.zoom = 0.0; // narrow
        f.optics.prism1 = 1.0;
    }
    let mut cam = state.camera.clone();
    cam.target = glam::Vec3::new(0.0, 0.0, 0.0);
    cam.pitch = 1.3; // look down
    cam.distance = 9.0;
    let (w, h, px) = state.renderer.capture(&state.scene, &cam, &state.ui.settings);
    if let Some(img) = image::RgbaImage::from_raw(w, h, px) {
        let _ = img.save(format!("{dir}/sheet_16_prism_top.png"));
    }

    // Lens close-up: the glass/dust front-lens material.
    {
        let f = &mut state.scene.fixtures[idx];
        f.optics = Default::default();
        f.pan = 0.0;
        f.tilt = 35.0;
        f.optics.zoom = 0.3;
    }
    {
        let frame = state.scene.fixtures[idx].position;
        let mut cam = state.camera.clone();
        // Look up the beam axis, face-on into the lens.
        cam.target = frame + glam::Vec3::new(0.0, -0.45, -0.45);
        cam.distance = 1.8;
        cam.pitch = -0.85;
        cam.yaw = std::f32::consts::PI;
        let (w, h, px) = state.renderer.capture(&state.scene, &cam, &state.ui.settings);
        if let Some(img) = image::RgbaImage::from_raw(w, h, px) {
            let _ = img.save(format!("{dir}/sheet_17_lens.png"));
        }
    }

    // Array demo: duplicate the fixture into a 36°/9 fan (the `d`-key dialog).
    {
        let f = &mut state.scene.fixtures[idx];
        f.optics = Default::default();
        f.optics.zoom = 0.08;
        f.tilt = 38.0;
    }
    state
        .scene
        .duplicate_fixture(idx, glam::Vec3::new(0.0, 0.0, 0.0), 36.0, 9);
    let (w, h, px) = state
        .renderer
        .capture(&state.scene, &state.camera, &state.ui.settings);
    if let Some(img) = image::RgbaImage::from_raw(w, h, px) {
        let _ = img.save(format!("{dir}/sheet_15_duplicate_fan.png"));
    }
}

/// Render a wheel-motion sequence (advancing the scene between frames) to verify
/// that gobo/colour/animation/prism wheels actually animate over time.
fn render_anim_sequence(state: &mut State, dir: &str) {
    let _ = std::fs::create_dir_all(dir);
    let Some(idx) = state.scene.fixtures.iter().position(|f| f.is_gdtf()) else {
        log::error!("PREVIZ_ANIM needs a GDTF fixture (set PREVIZ_GDTF)");
        return;
    };
    if let Some(env) = state.scene.environments.first_mut() {
        env.density = 0.12;
    }
    {
        let f = &mut state.scene.fixtures[idx];
        f.tilt = 30.0;
        f.optics = Default::default();
        f.optics.zoom = 0.02; // narrow so the prism copies separate cleanly
        f.optics.prism1 = 1.0; // 5-facet prism …
        f.optics.prism1_rot = 0.92; // … rotating, so the fan revolves
        f.optics.gobo1 = 4.0 / 6.0; // vortex gobo, replicated per facet
        f.optics.gobo1_rot = 0.95; // and spinning
    }
    for frame in 0..6 {
        let (w, h, px) = state
            .renderer
            .capture(&state.scene, &state.camera, &state.ui.settings);
        if let Some(img) = image::RgbaImage::from_raw(w, h, px) {
            let _ = img.save(format!("{dir}/anim_{frame:02}.png"));
        }
        // Advance ~0.33 s of motion between captured frames.
        for _ in 0..3 {
            state.scene.advance(0.11);
        }
    }
    log::info!("anim: wrote 6 frames to {dir}");
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

        // Advance time-based wheel motion once per real frame (not in the
        // renderer, which also runs for headless capture).
        self.scene.advance(dt);

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
            &self.ui.selection,
            &self.ui.settings,
            &paint_jobs,
            &full_output.textures_delta,
            &screen_descriptor,
        )
    }
}
