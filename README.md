# previz

An open-source **lighting previsualization** tool for live events
(concerts, festivals, theatre). It renders a 3D stage with lighting fixtures —
moving heads, washes, beams — and previews their output in real time.

This repository is the **initial scaffold**: a hand-written renderer with a
dockable UI, an orbit camera, and live-editable fixtures. It is intentionally
small and dependency-light so the renderer can grow into the real headline
feature — volumetric, ray-marched beams in haze.

> Status: scaffold. It runs and is fun to poke at, but it is not yet a usable
> previz tool. See [Roadmap](#roadmap).

## What it does today

- Opens a window, initializes wgpu, and renders at vsync with a clean exit.
- A default test scene: a diffuse floor, one **PAR can** 4 m up aimed down at
  45° at full intensity, and a large **fog box** environment at the origin (the
  bounds the volumetric beams will eventually fill).
- A small **content library** of fixtures and environments you can instantiate.
- A dockable workspace (drag the tabs to rearrange):
  - **Scene / Library** (left, tabbed) — the *Scene* tab outlines every fixture
    and environment (click to select); the *Library* tab lists categorized
    profiles (Fixtures ▸ Generic ▸ PAR Can, Environments ▸ Fog Box) with an
    **+** to add them, and an **Import GDTF…** button to load a real fixture.
  - **Inspector** (right) — edit the selected fixture (pan / tilt / color /
    intensity / beam / position) or environment (center / size / density /
    anisotropy / tint); for GDTF fixtures it also shows the thumbnail, the
    wheels (with gobo/color slot images), and every DMX mode + channel. Edits
    update the 3D view live.
  - **Viewport** (center) — the 3D scene with **real-time volumetric beams**:
    each fixture casts a raymarched light shaft through animated haze (disc
    source, soft edges, smoke wisps, bloom), lights the floor where it lands,
    and is occluded by geometry. An FPS HUD sits top-left. **Drag to orbit,
    shift+drag to pan, scroll to zoom.** The Scene tab's **View** controls tune
    exposure / bloom / beam intensity / raymarch steps and toggle the beam
    wireframe gizmo; the Fog Box inspector tunes density / anisotropy / tint.
  - **DMX Monitor** (bottom) — a stub patch table previewing the channels each
    fixture would occupy (no live DMX yet).

The headline feature — **hyper-realistic volumetric beams in haze** — has a
working first cut (Phase 0/1 of the R&D plan): an HDR pipeline with a
half-resolution single-scattering raymarch (Henyey–Greenstein phase, a
precomputed tiling 3D noise volume for the haze, energy-conserving integration,
depth-aware compositing) plus bloom and ACES tonemapping. The froxel grid,
temporal reprojection, and clustered culling for thousands of fixtures are the
documented next phases. Full design + math + plan:
[`docs/RESEARCH-volumetrics.md`](docs/RESEARCH-volumetrics.md).

## Design

Pure **wgpu + winit**. No game engine, no ECS — the renderer is written by
hand. The scene is plain structs and `Vec`s.

```
src/
  main.rs            winit event loop; creates App; drives update/render
  app.rs             App: owns Renderer, Scene, Ui; orchestrates per frame
  renderer/
    mod.rs           device/queue/surface/config; frame passes; instancing
    camera.rs        orbit camera; view+projection uniform
    viewport.rs      offscreen color+depth target -> texture exposed to egui
    mesh.rs          vertex/instance types; geometry (floor/cylinder/cone/grid/
                     box); GPU mesh + growable buffer upload
    pipeline.rs      wgsl loading + render pipelines
  scene/
    mod.rs           Scene: plain structs/Vecs (no ECS); Selection
    fixture.rs       Fixture: position, pan, tilt, color, intensity, beam
    environment.rs   Environment: fog box (center/size/density/g/tint)
    library.rs       content library: categorized fixture/env profiles
  ui/
    mod.rs           egui + egui_dock setup; dock layout; TabViewer
    panels.rs        Scene / Library / Inspector / Viewport / DMX panels
  shaders/
    grid.wgsl        colored line list (grid, axes, wireframes, beams)
    mesh.wgsl        instanced, lit meshes (floor + fixtures; emissive lens)
docs/
  RESEARCH-volumetrics.md   R&D + plan for the volumetric beam/fog renderer
```

### The CPU → GPU boundary

The renderer keeps a deliberately narrow seam between CPU and GPU:

- The **CPU** computes per-fixture parameters every frame — a model matrix from
  pan/tilt/position, plus color/intensity/selection — and writes them into a
  per-frame **instance buffer**.
- The **GPU** does all rendering: instanced draws of the floor and the fixture
  bodies (grouped by geometry), plus a line pass for the grid/axes and the
  per-frame dynamic lines (fog-box wireframes + beam indicators).

Nothing about the scene is persisted on the GPU between frames. When DMX or GDTF
land, they write into the same `Fixture` / `Environment` fields the GPU already
consumes, so the rendering path doesn't change.

### How the 3D view reaches the UI

The scene is rendered into an **offscreen texture** sized to the Viewport panel
(`renderer/viewport.rs`). That texture is registered with `egui-wgpu` and drawn
into the panel as an image. Each frame:

1. egui builds the docked UI; the Viewport panel reports the pixel size it wants
   and reads drag/scroll to drive the camera.
2. The renderer resizes the offscreen target if needed, renders the 3D scene
   into it, then paints egui (including the Viewport image) into the window
   surface — in that order, so the panel samples a freshly rendered frame.

## Build & run

Requires a recent stable Rust toolchain (edition 2024; MSRV **1.92**).

```sh
# from the repository root
cargo run --release
```

`--release` is recommended — the wgpu/naga/egui stacks are slow in a pure debug
build. (The dev profile here already bumps optimization a little; release is
still smoother.)

Controls:

- **Drag** inside the Viewport — orbit; **shift+drag** — pan; **scroll** — zoom.
- Select an item in **Scene** (or add one from **Library**), then edit it in
  **Inspector**; the viewport updates live.

Logging is off by default. To see init details:

```sh
RUST_LOG=previz=debug,wgpu=warn cargo run --release
```

### Dev tooling

Two headless modes (no visible window needed) help develop/verify the renderer:

```sh
# Render the offscreen 3D view to a PNG and exit (handy for CI / screenshots):
PREVIZ_SCREENSHOT=shot.png cargo run --release

# Benchmark the render: time N offscreen frames and print ms/frame + fps:
PREVIZ_BENCH=120 RUST_LOG=previz=info cargo run --release
```

### Platform notes

Developed and tested on macOS (Metal) / Apple Silicon. wgpu also targets
Vulkan (Linux/Windows) and DX12 (Windows); those backends are selected
automatically but haven't been exercised here yet.

## Dependencies

Minimal and idiomatic; minimal `unsafe` (only what wgpu/bytemuck require).

| crate | role |
|---|---|
| `winit` | window + input |
| `wgpu` | rendering |
| `egui`, `egui-winit`, `egui-wgpu` | UI, drawn into the same wgpu surface |
| `egui_dock` | dockable panels |
| `glam` | math |
| `bytemuck` | POD casts for vertex/uniform buffers |
| `pollster` | block on async wgpu init |
| `env_logger`, `log` | logging |
| `zip`, `roxmltree`, `gltf`, `image` | GDTF import (archive / XML / models / wheels) |
| `rfd` | native "Import GDTF…" file dialog |

Exact versions are pinned in `Cargo.toml`.

## Roadmap

These are intentionally **not** built yet, but the code leaves clean seams for
them:

- **Volumetric beams** — the headline feature: froxel fog + raymarched beams in
  haze, scattering through the fog box. Research and a phased plan are in
  [`docs/RESEARCH-volumetrics.md`](docs/RESEARCH-volumetrics.md). The
  `Environment` (density / anisotropy / tint) and `Fixture` (beam angle) fields
  are the inputs it will read.
- **GDTF** import — *first cut implemented*: **Library ▸ Import GDTF…** loads a
  `.gdtf` file (ZIP of `description.xml` + glTF models + wheel images). It parses
  the fixture identity, wheels, DMX modes/channels, the geometry hierarchy, and
  the glTF (GLB) part meshes, then renders the **real articulated 3D model**
  (base → pan yoke → tilt head) in the viewport with the volumetric beam coming
  from the Beam geometry. The Inspector shows the thumbnail, wheel slot images,
  and all DMX channels. Tested with the Ayrton Khamsin. Still TODO: per-channel
  DMX patch/control, GeometryReference instancing, gobo projection in the beam.
- **MVR** import/export — scene exchange with consoles and other previz tools.
- **Live DMX input** — sACN / Art-Net feeding the fixture parameters.

## License

Dual-licensed under **MIT** or **Apache-2.0**, at your option.
