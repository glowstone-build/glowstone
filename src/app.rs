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
    /// Live Art-Net / sACN DMX input: owns the patch + receive thread and decodes
    /// incoming universes into the fixtures each frame.
    dmx: crate::dmx::DmxIo,
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
        // Install the Phosphor icon font once; the full theme is applied per frame.
        crate::ui::theme::install_fonts(&egui_ctx);

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
            dmx: crate::dmx::DmxIo::new(),
            last_frame: Instant::now(),
            fps: 0.0,
            awake: true,
        });

        // Profiling: PREVIZ_STEPS overrides the volumetric max step budget.
        if let Some(s) = std::env::var("PREVIZ_STEPS").ok().and_then(|s| s.parse().ok()) {
            self.state.as_mut().unwrap().ui.settings.steps = s;
        }

        // Optional .archie project load for profiling: PREVIZ_OPEN=show.archie
        // loads a saved show (fixtures + geometry + bundled assets) and frames it.
        if let Ok(path) = std::env::var("PREVIZ_OPEN") {
            let state = self.state.as_mut().unwrap();
            state.ui.open_project(
                std::path::Path::new(&path),
                &mut state.scene,
                &mut state.camera,
                &mut state.dmx,
            );
            if let Some((c, r)) = state.scene.scene_frame() {
                state.camera.frame(c, r * 1.1);
            }
            log::info!(
                "opened project: {} fixtures, {} geometry",
                state.scene.fixtures.len(),
                state.scene.geometry.len()
            );
        }

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
                    // PREVIZ_GDTF_MODE=<index or name substring> selects the DMX
                    // mode (emitter layout + channel map) for the test fixture.
                    if let Ok(sel) = std::env::var("PREVIZ_GDTF_MODE") {
                        let f = &mut state.scene.fixtures[idx];
                        let gdtf = f.gdtf.clone().unwrap();
                        let mi = sel
                            .parse::<usize>()
                            .ok()
                            .filter(|&i| i < gdtf.modes.len())
                            .or_else(|| {
                                let s = sel.to_lowercase();
                                gdtf.modes.iter().position(|m| m.name.to_lowercase().contains(&s))
                            });
                        if let Some(mi) = mi {
                            f.mode_index = mi;
                            f.sync_mode();
                            log::info!("GDTF mode: [{}] {}", mi, gdtf.modes[mi].name);
                        } else {
                            log::warn!("PREVIZ_GDTF_MODE '{sel}' matched no mode");
                        }
                    }
                    // PREVIZ_PIXMAP=N drives a pixel-mapped pattern (alternating
                    // yellow + blue cells) and replicates the bar into a row of N,
                    // so the per-cell shaft look + the many-bar perf can be checked
                    // headlessly. Unpatched fixtures are skipped by the decode, so
                    // these cells persist to render.
                    if let Ok(v) = std::env::var("PREVIZ_PIXMAP") {
                        let nbars: usize = v.parse().unwrap_or(1).max(1);
                        let arc = state.scene.fixtures[idx].gdtf.clone().unwrap();
                        let mi = state.scene.fixtures[idx].mode_index;
                        let ncells = state.scene.fixtures[idx].emitters().len();
                        let setup = |f: &mut crate::scene::Fixture| {
                            f.cells = (0..ncells)
                                .map(|i| if i % 2 == 0 { [1.0, 0.8, 0.0] } else { [0.0, 0.1, 1.0] })
                                .collect();
                            f.tilt = -90.0;
                            f.snap_movement();
                        };
                        setup(&mut state.scene.fixtures[idx]);
                        for k in 1..nbars {
                            let pos = glam::Vec3::new(k as f32 * 1.4, 5.0, 0.0);
                            let j = state.scene.add_gdtf(arc.clone(), pos);
                            state.scene.fixtures[j].mode_index = mi;
                            state.scene.fixtures[j].sync_mode();
                            setup(&mut state.scene.fixtures[j]);
                        }
                    }
                    state.ui.selection = crate::scene::Selection::fixture(idx);
                    log::info!("imported GDTF: {path}");
                }
                Err(e) => log::error!("GDTF import failed: {e}"),
            }
        }

        // Optional laser-engine demo: PREVIZ_LASER replaces the scene with a fan
        // of R/G/B lasers aimed across the haze (verifies the laser render path).
        if std::env::var("PREVIZ_LASER").is_ok() {
            let state = self.state.as_mut().unwrap();
            let lib = crate::scene::Library::standard();
            state.scene.fixtures.clear();
            let lasers: Vec<_> = lib.fixtures.iter().filter(|p| p.laser).cloned().collect();
            for (k, p) in lasers.iter().cycle().take(9).enumerate() {
                let mut f = crate::scene::Fixture::from_profile(
                    p,
                    format!("{} {}", p.name, k),
                    glam::Vec3::new(-4.0 + k as f32, 6.0, -3.0),
                );
                f.intensity = 1.0;
                f.tilt = 55.0;
                f.pan = -40.0 + k as f32 * 10.0;
                f.snap_movement();
                state.scene.fixtures.push(f);
            }
            state.ui.selection = crate::scene::Selection::default();
        }

        // Optional MVR scene import for testing: PREVIZ_MVR=scene.mvr loads a full
        // scene (fixtures + static stage/truss geometry), replacing the demo
        // fixtures, and frames the camera on the rig.
        if let Ok(path) = std::env::var("PREVIZ_MVR") {
            match crate::mvr::MvrImport::load_path(std::path::Path::new(&path)) {
                Ok(import) => {
                    let state = self.state.as_mut().unwrap();
                    state.scene.import_mvr(import);
                    if let Some((center, radius)) = state.scene.scene_frame() {
                        state.camera.frame(center, radius * 1.15);
                    }
                    state.ui.selection = crate::scene::Selection::default();
                    {
                        let s = &state.scene;
                        let pts: Vec<glam::Vec3> = s
                            .fixtures
                            .iter()
                            .map(|f| f.position)
                            .chain(s.geometry.iter().map(|g| g.transform.w_axis.truncate()))
                            .collect();
                        if let Some(first) = pts.first() {
                            let (mut lo, mut hi) = (*first, *first);
                            for p in &pts {
                                lo = lo.min(*p);
                                hi = hi.max(*p);
                            }
                            log::info!("mvr bounds: min {lo:?} max {hi:?}");
                        }
                    }
                    log::info!("imported MVR: {path}");
                }
                Err(e) => log::error!("MVR import failed: {e}"),
            }
        }

        // PREVIZ_LOOK builds a designed multi-colour stage look on the imported
        // rig (using each fixture's CMY / zoom / gobo / prism / frost functions),
        // plus haze + camera. The PREVIZ_FOG / EXPOSURE / CAM_* knobs override it.
        if std::env::var("PREVIZ_LOOK").is_ok() {
            apply_stage_look(self.state.as_mut().unwrap());
        }

        // Dev knobs for the headless capture paths below: override exposure and
        // bring every fixture up to a level (so an imported, blacked-out rig is
        // visible in a verification screenshot without wiring DMX).
        if let Ok(v) = std::env::var("PREVIZ_EXPOSURE")
            && let Ok(v) = v.parse::<f32>()
        {
            self.state.as_mut().unwrap().ui.settings.exposure = v;
        }
        // PREVIZ_HDRI=path loads an equirectangular environment map into the world
        // (sky background + image-based ambient). PREVIZ_HDRI_BRIGHT scales it.
        if let Ok(path) = std::env::var("PREVIZ_HDRI") {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let w = &mut self.state.as_mut().unwrap().scene.world;
                    w.hdri = Some(std::sync::Arc::new(bytes));
                    w.hdri_name = std::path::Path::new(&path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or(path);
                    if let Ok(b) = std::env::var("PREVIZ_HDRI_BRIGHT").unwrap_or_default().parse::<f32>() {
                        w.brightness = b;
                    }
                }
                Err(e) => log::error!("PREVIZ_HDRI read {path}: {e}"),
            }
        }
        if let Ok(v) = std::env::var("PREVIZ_LEVELS")
            && let Ok(v) = v.parse::<f32>()
        {
            let state = self.state.as_mut().unwrap();
            // PREVIZ_LEVELS_N=N lights only ~N fixtures, spread evenly across the
            // rig (the rest stay blacked out) — a cleaner look than all-on.
            let total = state.scene.fixtures.len().max(1);
            let step = std::env::var("PREVIZ_LEVELS_N")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0 && n < total)
                .map(|n| (total / n).max(1))
                .unwrap_or(1);
            // PREVIZ_TILT / PREVIZ_PAN aim the lit fixtures (degrees).
            let tilt = std::env::var("PREVIZ_TILT").ok().and_then(|s| s.parse::<f32>().ok());
            let pan = std::env::var("PREVIZ_PAN").ok().and_then(|s| s.parse::<f32>().ok());
            for (i, f) in state.scene.fixtures.iter_mut().enumerate() {
                if i % step == 0 {
                    // The level lives in the dimmer (intensity is the master, =1);
                    // imported fixtures start at dimmer 0, so set the dimmer here.
                    f.optics.dimmer = v;
                    if let Some(t) = tilt {
                        f.tilt = t;
                    }
                    if let Some(p) = pan {
                        f.pan = p;
                    }
                } else {
                    f.optics.dimmer = 0.0;
                }
            }
        }

        // PREVIZ_FOG=density overrides the haze density of the first environment
        // (thinner haze → distinct beams instead of a uniform wash).
        if let Ok(d) = std::env::var("PREVIZ_FOG")
            && let Ok(d) = d.parse::<f32>()
            && let Some(env) = self.state.as_mut().unwrap().scene.environments.first_mut()
        {
            env.density = d;
        }

        // PREVIZ_MODE=beauty|unlit|wireframe selects the viewport display mode
        // (so the headless capture paths can verify each, like the other knobs).
        if let Ok(m) = std::env::var("PREVIZ_MODE") {
            use crate::scene::ViewportMode;
            self.state.as_mut().unwrap().ui.settings.mode = match m.to_lowercase().as_str() {
                "wireframe" | "wire" => ViewportMode::Wireframe,
                "unlit" | "flat" => ViewportMode::Unlit,
                _ => ViewportMode::Beauty,
            };
        }

        // Live DMX dev knobs: PREVIZ_DMX starts the real receiver; PREVIZ_DMX_FEED
        // / _INJECT push a synthetic universe set through the real decode path so
        // the rig can be driven headlessly (composes with PREVIZ_SCREENSHOT below).
        {
            let state = self.state.as_mut().unwrap();
            crate::dmx::apply_env_knobs(&mut state.dmx, &mut state.scene);
        }

        // Headless MVR export: PREVIZ_MVR_EXPORT=out.mvr writes the current scene
        // (typically after a PREVIZ_MVR import) back out and exits — for
        // round-trip verification.
        if let Ok(out) = std::env::var("PREVIZ_MVR_EXPORT") {
            match crate::mvr::export_path(&self.state.as_ref().unwrap().scene, std::path::Path::new(&out)) {
                Ok(()) => log::info!("exported MVR: {out}"),
                Err(e) => log::error!("MVR export failed: {e}"),
            }
            event_loop.exit();
            return;
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

        // Headless FULL-UI screenshot: PREVIZ_UI=path.png renders the whole
        // window (3D viewport + egui panels/menus/dock) offscreen to a PNG and
        // exits — so the interface can be verified without a visible window /
        // Screen-Recording permission. PREVIZ_UI_RES=WxH sets the size.
        if let Ok(path) = std::env::var("PREVIZ_UI") {
            let (w, h) = std::env::var("PREVIZ_UI_RES")
                .ok()
                .and_then(|r| {
                    let (w, h) = r.split_once('x')?;
                    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
                })
                .unwrap_or((1600u32, 1000u32));
            render_ui_screenshot(self.state.as_mut().unwrap(), &path, w, h);
            event_loop.exit();
            return;
        }

        // Headless wheel-transition check: PREVIZ_WHEEL=dir (with PREVIZ_GDTF)
        // steps a gobo/colour wheel between slots and scrolls CMY in, advancing
        // the scene between frames — to verify the physical split + gap + flag
        // slide (not a crossfade).
        if let Ok(dir) = std::env::var("PREVIZ_WHEEL") {
            render_wheel_sequence(self.state.as_mut().unwrap(), &dir);
            event_loop.exit();
            return;
        }

        // Headless screenshot path: PREVIZ_SCREENSHOT=path.png renders the
        // offscreen 3D view to a PNG and exits (no window needed). Handy for
        // verifying the renderer without a visible window / CI.
        if let Ok(path) = std::env::var("PREVIZ_SCREENSHOT") {
            let state = self.state.as_mut().unwrap();
            // Optional PREVIZ_RES=WIDTHxHEIGHT to render the screenshot at an
            // explicit resolution instead of the window size.
            if let Some((w, h)) = std::env::var("PREVIZ_RES").ok().and_then(|r| {
                let (w, h) = r.split_once('x')?;
                Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?))
            }) {
                state.renderer.resize_viewport((w.max(1), h.max(1)));
            }
            // PREVIZ_ZOOM scales the camera dolly distance (<1 = closer);
            // PREVIZ_CAM_Y nudges the look-at height (metres).
            if let Some(z) = std::env::var("PREVIZ_ZOOM").ok().and_then(|s| s.parse::<f32>().ok()) {
                state.camera.distance *= z;
            }
            if let Some(dy) = std::env::var("PREVIZ_CAM_Y").ok().and_then(|s| s.parse::<f32>().ok()) {
                state.camera.target.y += dy;
            }
            // Full camera override: PREVIZ_CAM_TARGET=x,y,z and PREVIZ_CAM_YAW /
            // _PITCH (radians) / _DIST (metres) for an explicit eye-level shot.
            let envf = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok());
            if let Some(t) = std::env::var("PREVIZ_CAM_TARGET").ok().and_then(|s| {
                let p: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (p.len() == 3).then(|| glam::Vec3::new(p[0], p[1], p[2]))
            }) {
                state.camera.target = t;
            }
            if let Some(y) = envf("PREVIZ_CAM_YAW") {
                state.camera.yaw = y;
            }
            if let Some(p) = envf("PREVIZ_CAM_PITCH") {
                state.camera.pitch = p;
            }
            if let Some(d) = envf("PREVIZ_CAM_DIST") {
                state.camera.distance = d;
            }
            // Headless render skips the per-frame motion integrator; settle the
            // posed heads so the screenshot shows the commanded pan/tilt.
            state.scene.snap_movement();
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
            // Bench at an explicit resolution + camera (PREVIZ_RES / PREVIZ_CAM_*)
            // so we can measure a realistic FOH shot, not just the auto far frame.
            if let Some((w, h)) = std::env::var("PREVIZ_RES").ok().and_then(|r| {
                let (w, h) = r.split_once('x')?;
                Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?))
            }) {
                state.renderer.resize_viewport((w.max(1), h.max(1)));
            }
            apply_cam_env(&mut state.camera);
            state.scene.snap_movement();
            // PREVIZ_BENCH_READBACK=1 keeps the full capture (incl. GPU→CPU copy);
            // default times render-only (what the live presenting app actually pays).
            let readback = std::env::var("PREVIZ_BENCH_READBACK").is_ok();
            for _ in 0..10 {
                if readback {
                    let _ = state.renderer.capture(&state.scene, &state.camera, &state.ui.settings);
                } else {
                    state.renderer.bench_render(&state.scene, &state.camera, &state.ui.settings);
                }
            }
            let t0 = Instant::now();
            for _ in 0..n {
                if readback {
                    let _ = state.renderer.capture(&state.scene, &state.camera, &state.ui.settings);
                } else {
                    state.renderer.bench_render(&state.scene, &state.camera, &state.ui.settings);
                }
            }
            let per = t0.elapsed().as_secs_f32() / n as f32;
            let (w, h) = state.renderer.viewport.size;
            log::info!(
                "BENCH {w}x{h}: {:.2} ms/frame = {:.0} fps ({})",
                per * 1000.0,
                1.0 / per,
                if readback { "incl. readback" } else { "render-only" }
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

/// Build a designed multi-colour stage look on the imported rig, exercising each
/// fixture's optical functions (CMY colour, zoom, gobo, prism, frost) plus a
/// fanned pan/tilt, and tune haze + camera for it. Lights a spread subset (~24)
/// so the beams read as a fan rather than a wash. Triggered by `PREVIZ_LOOK`.
fn apply_stage_look(state: &mut State) {
    use crate::optics::OpticalControls;

    // Atmosphere + tone. Thin haze + forward scattering keeps the beams crisp
    // shafts instead of flooding the fog with glow.
    if let Some(env) = state.scene.environments.first_mut() {
        env.density = 0.013;
        env.color = [0.72, 0.74, 0.82];
        // Moderate scattering so beam shafts read from the side, not only head-on.
        env.anisotropy = 0.35;
    }
    state.ui.settings.exposure = 0.3;
    state.ui.settings.bloom = 0.8;
    state.ui.settings.beam_intensity = 430.0;

    // A cool concert palette (blues/teal/lavender) with a single warm amber
    // accent — reads cleanly and avoids the hot-pink fog wash.
    let palette: [[f32; 3]; 6] = [
        [0.15, 0.40, 1.0], // blue
        [0.0, 0.85, 1.0],  // azure
        [0.10, 1.0, 0.70], // teal
        [1.0, 0.70, 0.20], // amber accent
        [0.55, 0.40, 1.0], // lavender
        [0.85, 0.95, 1.0], // cool white
    ];

    let n = state.scene.fixtures.len();
    // Split by capability so the textured beams come from real gobo fixtures.
    let gobo_idx: Vec<usize> = (0..n)
        .filter(|&i| {
            state.scene.fixtures[i]
                .gdtf
                .as_ref()
                .map(|g| g.has_attribute("Gobo1"))
                .unwrap_or(false)
        })
        .collect();
    let other_idx: Vec<usize> = (0..n).filter(|i| !gobo_idx.contains(i)).collect();
    // Evenly sample `count` indices from a list.
    let pick = |v: &[usize], count: usize| -> Vec<usize> {
        if v.is_empty() || count == 0 {
            return Vec::new();
        }
        let step = (v.len() / count).max(1);
        v.iter().copied().step_by(step).take(count).collect()
    };
    let gobo_lit = pick(&gobo_idx, 10);
    let color_lit = pick(&other_idx, 8);

    // White textured gobo beams (WIDE, fanned) — these are the shafts.
    use crate::gdtf::WheelKind;
    let g = gobo_lit.len().max(2);
    for (k, &i) in gobo_lit.iter().enumerate() {
        let t = k as f32 / (g - 1) as f32;
        let f = &mut state.scene.fixtures[i];
        f.intensity = 1.0;
        f.color = [1.0, 1.0, 1.0];
        f.pan = -55.0 + t * 110.0;
        f.tilt = 30.0 + (k % 3) as f32 * 8.0;
        f.optics = OpticalControls::default();
        f.sync_mode();
        f.optics.dimmer = 1.0;
        f.optics.zoom = 0.45; // wide so the cookie shows in the shaft, not just the floor
        if let Some(w) = f.wheel_control_mut(WheelKind::Gobo, 1) {
            w.value = 4.0 / 6.0;
        }
        if let Some(p) = f.wheel_phase_mut(WheelKind::Gobo, 1) {
            *p = 0.5 * t;
        }
    }
    // Colour wash beams (narrow) — fill behind the gobo shafts.
    let c = color_lit.len().max(2);
    for (k, &i) in color_lit.iter().enumerate() {
        let t = k as f32 / (c - 1) as f32;
        let col = palette[k % palette.len()];
        let f = &mut state.scene.fixtures[i];
        f.intensity = 1.0;
        f.color = col;
        f.pan = -50.0 + t * 100.0;
        f.tilt = 34.0 + (k % 2) as f32 * 8.0;
        f.optics = OpticalControls::default();
        f.sync_mode();
        f.optics.dimmer = 1.0;
        f.optics.cmy = [1.0 - col[0], 1.0 - col[1], 1.0 - col[2]];
        f.optics.zoom = 0.05 + (k % 3) as f32 * 0.05;
        if k % 3 == 0 {
            if let Some(w) = f.wheel_control_mut(WheelKind::Prism, 1) {
                w.value = 1.0;
            }
            if let Some(p) = f.wheel_phase_mut(WheelKind::Prism, 1) {
                *p = 0.5 * t;
            }
        }
    }
    log::info!(
        "stage look: {} gobo shafts + {} colour beams",
        gobo_lit.len(),
        color_lit.len()
    );

    // 3/4 audience-level camera framing the fan (overridable via PREVIZ_CAM_*).
    state.camera.target = glam::Vec3::new(-2.0, 3.0, -0.5);
    state.camera.yaw = 0.4;
    state.camera.pitch = -0.05;
    state.camera.distance = 14.0;
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
    // Wheel components are addressed by kind+number (the chain is dynamic).
    use crate::gdtf::WheelKind as K;
    fn wheel(f: &mut crate::scene::Fixture, kind: crate::gdtf::WheelKind, n: u32, value: f32, spin: f32, phase: f32) {
        if let Some(w) = f.wheel_control_mut(kind, n) {
            w.value = value;
            w.spin = spin;
        }
        if let Some(p) = f.wheel_phase_mut(kind, n) {
            *p = phase;
        }
    }
    let presets: [(&str, fn(&mut crate::scene::Fixture)); 17] = [
        ("01_neutral", |_f| {}),
        ("18_dim_via_blade", |f| {
            // The dimmer IS the shutter: dimming a blade fixture closes the blade.
            f.shutter = crate::optics::ShutterKind::Blade;
            f.optics.dimmer = 0.5; // half dim → blade half closed (blurred, rotated)
            f.optics.zoom = 0.3;
        }),
        ("19_shutter_sawtooth", |f| {
            f.shutter = crate::optics::ShutterKind::Sawtooth;
            f.optics.dimmer = 0.45;
            f.optics.zoom = 0.3;
        }),
        ("02_gobo_target", |f| { wheel(f, K::Gobo, 1, 5.0 / 6.0, 0.5, 0.0); f.optics.zoom = 0.25; }),
        ("03_gobo_vortex_spin", |f| {
            wheel(f, K::Gobo, 1, 4.0 / 6.0, 0.85, 0.8);
            f.optics.zoom = 0.25;
        }),
        ("04_gobo2_smokerings", |f| { wheel(f, K::Gobo, 2, 2.0 / 6.0, 0.5, 0.0); f.optics.zoom = 0.25; }),
        ("05_color_red", |f| { wheel(f, K::Color, 1, 1.0, 0.5, 0.0); }),
        ("06_cmy_magenta", |f| { f.optics.cmy = [0.0, 0.85, 0.0]; }),
        ("07_cto_warm", |f| { f.optics.cto = 1.0; }),
        ("08_prism5", |f| { wheel(f, K::Prism, 1, 1.0, 0.5, 0.0); f.optics.zoom = 0.0; }),
        ("08b_prism_gobo", |f| {
            wheel(f, K::Prism, 1, 1.0, 0.5, 0.0);
            wheel(f, K::Gobo, 1, 5.0 / 6.0, 0.5, 0.0);
            f.optics.zoom = 0.0;
        }),
        ("09_frost", |f| {
            wheel(f, K::Frost, 1, 0.85, 0.5, 0.0);
            wheel(f, K::Gobo, 1, 5.0 / 6.0, 0.5, 0.0);
            f.optics.zoom = 0.25;
        }),
        ("10_zoom_narrow", |f| { f.optics.zoom = 0.0; }),
        ("11_iris_closed", |f| { f.optics.iris = 0.25; }),
        ("12_animation", |f| {
            wheel(f, K::Animation, 1, 1.0, 0.9, 0.3);
            wheel(f, K::Gobo, 1, 5.0 / 6.0, 0.5, 0.0);
            f.optics.zoom = 0.25;
        }),
        ("13_chromatic_ab", |f| { f.optics.ca = 1.0; wheel(f, K::Gobo, 1, 5.0 / 6.0, 0.5, 0.0); f.optics.zoom = 0.12; }),
        ("14_combo", |f| {
            // Color + gobo + prism + frost together (stages compose).
            f.optics.cmy = [0.6, 0.0, 0.0]; // cyan tint
            wheel(f, K::Gobo, 1, 4.0 / 6.0, 0.5, 0.0); // vortex
            wheel(f, K::Prism, 1, 1.0, 0.5, 0.4); // 5-facet fan
            wheel(f, K::Frost, 1, 0.15, 0.5, 0.0);
            f.optics.zoom = 0.18;
        }),
    ];

    for (name, apply) in presets {
        {
            let f = &mut state.scene.fixtures[idx];
            f.optics = Default::default();
            f.motion = WheelMotion::default();
            f.sync_mode();
            f.pan = 0.0;
            f.tilt = 28.0;
            apply(f);
        }
        state.scene.snap_movement();
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
        f.sync_mode();
        f.tilt = 0.0; // straight down
        f.optics.zoom = 0.0; // narrow
        wheel(f, K::Prism, 1, 1.0, 0.5, 0.0);
    }
    state.scene.snap_movement();
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
    state.scene.snap_movement();
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
    state.scene.snap_movement();
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
        use crate::gdtf::WheelKind as K;
        let f = &mut state.scene.fixtures[idx];
        f.tilt = 30.0;
        f.optics = Default::default();
        f.sync_mode();
        f.optics.zoom = 0.02; // narrow so the prism copies separate cleanly
        // 5-facet prism, rotating, with a spinning vortex gobo per facet.
        if let Some(w) = f.wheel_control_mut(K::Prism, 1) {
            w.value = 1.0;
            w.spin = 0.92;
        }
        if let Some(w) = f.wheel_control_mut(K::Gobo, 1) {
            w.value = 4.0 / 6.0;
            w.spin = 0.95;
        }
    }
    state.scene.snap_movement(); // settle tilt before the (wheel-only) sequence
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

/// Render a wheel-transition sequence: a gobo wheel stepping between slots
/// (showing the split + dark holder gap sweep), plus CMY flags sliding in — to
/// verify the physical wheel/flag simulation animates instead of crossfading.
fn render_wheel_sequence(state: &mut State, dir: &str) {
    use crate::gdtf::WheelKind as K;
    let _ = std::fs::create_dir_all(dir);
    let Some(idx) = state.scene.fixtures.iter().position(|f| f.is_gdtf()) else {
        log::error!("PREVIZ_WHEEL needs a GDTF fixture (set PREVIZ_GDTF)");
        return;
    };
    if let Some(env) = state.scene.environments.first_mut() {
        env.density = 0.13;
    }
    {
        let f = &mut state.scene.fixtures[idx];
        f.tilt = 38.0;
        f.optics = Default::default();
        f.sync_mode();
        f.optics.zoom = 0.32;
        // Start at open; target a gobo slot so the wheel slews through the gap.
        if let Some(w) = f.wheel_control_mut(K::Gobo, 1) {
            w.value = 5.0 / 6.0;
        }
        f.snap_movement(); // settle pan/tilt; wheel positions start at 0 (open)
        f.motion.positions.iter_mut().for_each(|p| *p = 0.0);
    }
    // Look down the beam onto the floor pool, where the gobo split + holder gap
    // read clearly during the wheel's travel.
    // Close 3/4 view of the beam shaft; the gobo split + dark holder gap sweep
    // across the cone as the wheel travels.
    let mut cam = state.camera.clone();
    cam.target = glam::Vec3::new(0.0, 2.4, 0.9);
    cam.pitch = 0.12;
    cam.distance = 4.2;
    cam.yaw = 0.7;
    for frame in 0..10 {
        let (w, h, px) = state.renderer.capture(&state.scene, &cam, &state.ui.settings);
        if let Some(img) = image::RgbaImage::from_raw(w, h, px) {
            let _ = img.save(format!("{dir}/wheel_{frame:02}.png"));
        }
        // ~0.05 s between frames → catch the wheel mid-travel (split + gap sweep).
        state.scene.advance(0.05);
    }
    log::info!("wheel: wrote 10 frames to {dir}");
}

/// Render the whole egui UI + 3D viewport offscreen to a PNG (headless, no
/// visible window). Runs a few settle frames so the dock layout + viewport panel
/// size stabilise, then captures the final frame.
#[allow(deprecated)] // egui 0.34 Context::run — matches the live render() path
/// Apply headless camera-override env knobs (shared by the screenshot + bench
/// paths): PREVIZ_ZOOM, PREVIZ_CAM_Y, PREVIZ_CAM_TARGET=x,y,z, PREVIZ_CAM_YAW /
/// _PITCH (radians), PREVIZ_CAM_DIST (metres).
fn apply_cam_env(camera: &mut OrbitCamera) {
    let envf = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok());
    if let Some(z) = envf("PREVIZ_ZOOM") {
        camera.distance *= z;
    }
    if let Some(dy) = envf("PREVIZ_CAM_Y") {
        camera.target.y += dy;
    }
    if let Some(t) = std::env::var("PREVIZ_CAM_TARGET").ok().and_then(|s| {
        let p: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        (p.len() == 3).then(|| glam::Vec3::new(p[0], p[1], p[2]))
    }) {
        camera.target = t;
    }
    if let Some(y) = envf("PREVIZ_CAM_YAW") {
        camera.yaw = y;
    }
    if let Some(p) = envf("PREVIZ_CAM_PITCH") {
        camera.pitch = p;
    }
    if let Some(d) = envf("PREVIZ_CAM_DIST") {
        camera.distance = d;
    }
}

fn render_ui_screenshot(state: &mut State, path: &str, w: u32, h: u32) {
    // The welcome splash would otherwise cover every headless UI screenshot;
    // PREVIZ_UI_SPLASH=1 keeps it up so the splash itself can be captured.
    if std::env::var("PREVIZ_UI_SPLASH").is_err() {
        state.ui.dismiss_splash();
    }
    if let Ok(title) = std::env::var("PREVIZ_UI_TAB") {
        state.ui.focus_tab_by_title(&title);
    }
    if std::env::var("PREVIZ_UI_QS").is_ok() {
        state.ui.debug_open_quick_select();
    }
    if std::env::var("PREVIZ_UI_PROFILE").is_ok() {
        state.ui.debug_open_profile(&state.scene);
    }
    if std::env::var("PREVIZ_UI_REPLACE").is_ok() {
        state.ui.debug_open_replace(&state.scene);
    }
    if let Ok(v) = std::env::var("PREVIZ_UI_BULK") {
        state.ui.debug_select_n(&state.scene, v.parse().unwrap_or(3));
    }
    // PREVIZ_UI_SHARE opens the online Fixture Library window; =demo injects rows.
    if let Ok(v) = std::env::var("PREVIZ_UI_SHARE") {
        state.ui.debug_open_share(v == "demo");
    }
    if std::env::var("PREVIZ_UI_SELECT").is_ok() {
        state.ui.debug_select_first_gdtf(&state.scene);
    }
    if let Ok(n) = std::env::var("PREVIZ_UI_SELECT_N") {
        state.ui.debug_select_n(&state.scene, n.parse().unwrap_or(4));
    }
    let mut jobs: Vec<egui::ClippedPrimitive> = Vec::new();
    let mut sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [w, h], pixels_per_point: 1.0 };
    for i in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(w as f32, h as f32),
            )),
            ..Default::default()
        };
        let viewport_texture = state.renderer.viewport.texture_id;
        let egui_ctx = state.egui_ctx.clone();
        let out = egui_ctx.run(raw, |ctx| {
            ctx.set_pixels_per_point(1.0);
            state.ui.show(
                ctx,
                &mut state.scene,
                &mut state.camera,
                &mut state.dmx,
                viewport_texture,
                60.0,
            );
        });
        // Settle the atlas + size the 3D viewport to the panel's request.
        state.renderer.apply_egui_textures(&out.textures_delta);
        state.renderer.resize_viewport(state.ui.requested_viewport_px);
        if i == 2 {
            sd = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [w, h],
                pixels_per_point: out.pixels_per_point,
            };
            jobs = egui_ctx.tessellate(out.shapes, out.pixels_per_point);
        }
    }
    let empty = egui::TexturesDelta::default();
    let (rw, rh, px) = state.renderer.capture_ui(
        (w, h),
        &state.scene,
        &state.camera,
        &state.ui.selection,
        &state.ui.settings,
        &jobs,
        &empty,
        &sd,
    );
    let Some(mut img) = image::RgbaImage::from_raw(rw, rh, px) else {
        log::error!("UI screenshot: bad buffer");
        return;
    };
    // PREVIZ_UI_CROP="x,y,w,h" crops to a region (for inspecting one panel).
    if let Ok(spec) = std::env::var("PREVIZ_UI_CROP") {
        let nums: Vec<u32> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if let [x, y, cw, ch] = nums[..] {
            let cw = cw.min(rw.saturating_sub(x));
            let ch = ch.min(rh.saturating_sub(y));
            img = image::imageops::crop(&mut img, x, y, cw, ch).to_image();
        }
    }
    match img.save(path) {
        Ok(()) => log::info!("UI screenshot: {path} ({}x{})", img.width(), img.height()),
        Err(e) => log::error!("UI screenshot save {path}: {e}"),
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

        // Live DMX: apply any deferred connectivity command, pull the latest
        // universes from the receive thread, and decode them into the fixtures —
        // BEFORE advancing motion, so DMX-driven spin values feed this frame's
        // wheel-motion integration.
        self.dmx.apply_pending();
        self.dmx.poll();
        self.dmx.decode(&mut self.scene);

        // Crossfade any in-progress cue AFTER DMX decode (so an offline cue
        // overrides the rest state) and BEFORE motion advance (so its pan/tilt
        // feeds this frame's slew).
        self.ui.tick_cues(&mut self.scene, dt);

        // Advance time-based wheel motion once per real frame (not in the
        // renderer, which also runs for headless capture).
        self.scene.advance(dt);

        // Crash-recovery autosave (debounced inside; writes to the cache dir).
        self.ui.autosave_tick(&self.scene, &self.camera, &self.dmx, dt);

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let viewport_texture = self.renderer.viewport.texture_id;
        let egui_ctx = self.egui_ctx.clone();

        // Build the UI. The closure borrows the scene/camera/ui fields; egui_ctx
        // is a separate (cloned) handle so there's no borrow conflict.
        let mut full_output = egui_ctx.run(raw_input, |ctx| {
            self.ui.show(
                ctx,
                &mut self.scene,
                &mut self.camera,
                &mut self.dmx,
                viewport_texture,
                fps,
            );
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
