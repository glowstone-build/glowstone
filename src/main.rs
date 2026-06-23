//! previz — an open-source lighting previsualization tool for live events.
//!
//! Pure wgpu + winit (no game engine, no ECS). This binary wires up the winit
//! event loop and hands control to [`app::App`].

mod app;
mod citp;
mod dmx;
mod gdtf;
mod mvr;
mod ndi;
mod optics;
mod renderer;
mod scene;
mod share;
mod ui;

use winit::event_loop::EventLoop;

fn main() {
    // Quiet by default; `RUST_LOG=previz=debug,wgpu=warn cargo run` to dig in.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,previz=info"),
    )
    .init();

    let event_loop = EventLoop::new().expect("failed to create event loop");

    // We drive redraws explicitly (request_redraw after each frame), so the
    // default `Wait` control flow is correct — the loop sleeps until there's a
    // redraw or input to service.
    let mut app = app::App::default();
    event_loop.run_app(&mut app).expect("event loop error");
}
