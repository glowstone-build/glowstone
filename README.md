# glowstone

An open-source **lighting previsualization** tool for live events
(concerts, festivals, theatre). It renders a 3D stage with lighting fixtures —
moving heads, washes, beams — and previews their output in real time.

This repository is the **initial scaffold**: a hand-written renderer with a
dockable UI, an orbit camera, and live-editable fixtures. It is intentionally
small and dependency-light so the renderer can grow into the real headline
feature — volumetric, ray-marched beams in haze.

> Status: scaffold. It runs and is fun to poke at, but it is not yet a usable
> glowstone tool. See [Roadmap](#roadmap).

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
    shift+drag to pan, scroll to zoom, click to select** a fixture (ray pick),
    and press **`d`** (while the viewport — which shows a focus border — is
    active) to open the **Duplicate** dialog: array the selected fixture by X/Y/Z
    offset and a per-copy Y angle (e.g. offset 0, Y angle 36°, count 9 fans the
    beams into a full circle). The Scene tab's **View** controls tune
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

## The beam engine

Imported GDTF fixtures are rendered through a **physically-motivated optical
chain** that mirrors a real moving head, source → lens:

```
LED/halogen engine → shutter/dimmer → animation wheel → gobo wheels →
color wheel / CMY / CTO → prism → frost → focus → zoom lens
```

Each frame the CPU resolves every fixture's controls (`src/optics`) into one
`BeamOptics` — folding the source white point (CCT → linear RGB), CTO + CMY +
color-wheel tint, candela conservation, super-Gaussian edge order (from the
GDTF field/beam angles), iris, frost, focus distance, shutter/strobe gate, the
gobo/animation atlas layers, and the prism facet fan. That packs into one
`FixtureGpu` consumed by **both** the volumetric beam (`volumetric.wgsl`) and the
floor pool (`mesh.wgsl`) via shared helpers (`optics.wgsl`), so the shaft and the
pool always agree. Highlights:

- **Projected gobos** (both wheels) and a scrolling **animation glass** are
  sampled as light cookies per raymarch step and per floor fragment, so a gobo
  throws a patterned shaft *and* a matching floor pattern. **Wheel motion**
  (gobo image spin, indexed rotation, slot cross-fade, animation scroll, color
  rainbow) is time-integrated each frame — gobo-scroll works out of the box.
- **Prism** replication is done by **beam-axis expansion**: each facet emits a
  deflected copy of the whole beam (reusing the cone + cookie + tint path), so a
  5-facet prism shows five separated aerial beams in haze *and* five floor pools,
  with the gobo replicated per facet; prism rotation spins the constellation.
- **Color**: subtractive CMY + CTO + a 7-slot color wheel, all in linear light.
- **Beam shaping**: zoom (with candela conservation — tight beams get brighter),
  iris, frost (softens + widens), focus (mip-LOD blur sharp at the focus distance).
- **Chromatic aberration**: tunable lateral dispersion — red and blue shift in
  opposite directions so one beam edge fringes amber and the other blue (plus
  extra fringing out of focus), in linear HDR before tonemap, energy-conserving.
- **Front lens**: a glassy, dusty, self-illuminated disc at the beam exit
  (`lens.wgsl`) — bright core, fresnel rim, dust speckle — so the fixture reads
  like a real lit lens up close, tinted by the colour chain and dimming with it.

All parameters are pulled from the GDTF (validated against the Ayrton Khamsin:
6800 K LED, 40000 lm, 25° beam, zoom 7.8–58°, two gobo wheels, animation wheel,
5-/4-facet prisms, CMY/CTO, frost, iris). The Inspector exposes a fader for every
stage the fixture actually has.

## MVR scene exchange

[MVR](https://gdtf-share.com) (*My Virtual Rig*) is how lighting consoles and CAD
tools exchange a whole show: an `.mvr` is a ZIP of a `GeneralSceneDescription.xml`
scene graph plus the resources it references — embedded `.gdtf` fixture
definitions and `.glb` 3D models for the stage, truss, and set.

**Import** (`Library ▸ Import MVR…`) walks the layer/`ChildList` hierarchy and
produces a flat scene:

- **Fixtures** resolve their embedded GDTF (parsed once and shared across the
  often-hundreds of instances), keep their real **hang orientation**, and carry
  the **patch** (DMX address + break, `FixtureID`, mode, class/position refs,
  custom commands) through for round-trip — shown in the Inspector. Imported
  rigs start **blacked out** (no DMX = no output); bring fixtures up in the
  Inspector or via a bulk selection.
- **Scene objects** (stage decks, truss, set pieces, screens) load their `.glb`
  meshes and draw as lit geometry that **occludes** the beams.

**Export** (`Export MVR…`) writes the scene back to a valid `.mvr` — regenerating
the XML (world→MVR matrices, metre→millimetre) and re-bundling every GDTF +
model — preserving UUIDs, addresses, modes, and placement so it round-trips
through other tools.

The one thing to get right is coordinates: MVR is **right-handed, +Z-up**, with
`<Matrix>` translations in **millimetres** while geometry vertices are in
**metres**; a `<Matrix>` is four 3-vectors `{u}{v}{w}{o}` whose `u/v/w` are the
rotation *columns*. The importer maps all of that onto the app's +Y-up metre
world with a single −90° X basis change (the same one the GDTF path uses), and
the exporter inverts it. `src/mvr.rs` carries the parser, the writer, and the
unit tests that pin the convention.

## Design

Pure **wgpu + winit**. No game engine, no ECS — the renderer is written by
hand. The scene is plain structs and `Vec`s.

```
src/
  main.rs            winit event loop; creates App; drives update/render
  app.rs             App: owns Renderer, Scene, Ui; orchestrates per frame
  gdtf.rs            GDTF fixture import (zip + XML + glTF models + wheels)
  mvr.rs             MVR scene import/export (scene graph, matrices, re-bundling)
  renderer/
    mod.rs           device/queue/surface/config; frame passes; instancing;
                     build_beam_gpus() resolves optics + prism beam-expansion
    camera.rs        orbit camera; view+projection uniform
    viewport.rs      offscreen color+depth target -> texture exposed to egui
    mesh.rs          vertex/instance types; geometry (floor/cylinder/cone/grid/
                     box); GPU mesh + growable buffer upload
    atlas.rs         gobo/animation texture_2d_array built from GDTF wheel media
    fixture_model.rs GLB load + articulated assembly + beam frame (lens basis)
    pipeline.rs      wgsl loading (+ shared optics.wgsl include) + pipelines
  scene/
    mod.rs           Scene: plain structs/Vecs (no ECS); Selection; advance(dt)
    fixture.rs       Fixture: position, pan, tilt, color, intensity, beam, optics
    environment.rs   Environment: fog box (center/size/density/g/tint)
    library.rs       content library: categorized fixture/env profiles
  optics/
    mod.rs           OpticalControls + resolve() -> BeamOptics (the optical chain)
    color.rs         CCT->linear RGB (Planckian), CTO filter, subtractive CMY
    motion.rs        WheelMotion: time-integrated wheel/gobo/prism/anim phases
  ui/
    mod.rs           egui + egui_dock setup; dock layout; TabViewer
    panels.rs        Scene / Library / Inspector / Viewport / DMX panels
  shaders/
    grid.wgsl        colored line list (grid, axes, wireframes, beams)
    optics.wgsl      shared beam-optics helpers (cookie projection, super-Gaussian
                     falloff, chromatic aberration, gobo sampling) for mesh+volume
    mesh.wgsl        instanced, lit meshes (floor + fixtures; emissive lens) — the
                     floor pool runs the same optical chain as the beam
    volumetric.wgsl  single-scattering beam raymarch with the full optical chain
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
- **Click** a fixture in the Viewport to select it; **⌘/Ctrl-click** adds/removes
  from a **multi-selection** (or pick/⌘-click in **Scene**). With several
  selected, the **Inspector** shows a **bulk editor** that writes a shared
  property to all at once. The focused Viewport shows a border.
- **`d`** in the Viewport opens **Duplicate** — array the selected fixture by
  offset + Y angle (Esc or Cancel to dismiss).
- The **Scene → View** panel has an **Origin grid** toggle; the fog-box border is
  always drawn. The live preview animates continuously (haze drift, wheel spin,
  gobo scroll) without needing input.

Logging is off by default. To see init details:

```sh
RUST_LOG=glowstone=debug,wgpu=warn cargo run --release
```

### Dev tooling

Two headless modes (no visible window needed) help develop/verify the renderer:

```sh
# Render the offscreen 3D view to a PNG and exit (handy for CI / screenshots):
GLOWSTONE_SCREENSHOT=shot.png cargo run --release

# Benchmark the render: time N offscreen frames and print ms/frame + fps:
GLOWSTONE_BENCH=120 RUST_LOG=glowstone=info cargo run --release

# Optical contact sheet: render one PNG per optical feature (gobo, prism, color,
# CMY, CTO, frost, zoom, iris, animation, chromatic aberration) for the imported
# fixture, to verify the whole beam chain without the UI:
GLOWSTONE_GDTF=fixture.gdtf GLOWSTONE_SHEET=out_dir cargo run --release

# Import an MVR scene and render it (the camera frames the rig). An imported rig
# is blacked out by default, so add GLOWSTONE_EXPOSURE (boost) and/or GLOWSTONE_LEVELS
# (bring all fixtures up) to see it; GLOWSTONE_MVR_EXPORT writes it back out:
GLOWSTONE_MVR=scene.mvr GLOWSTONE_EXPOSURE=30 GLOWSTONE_SCREENSHOT=shot.png cargo run --release
GLOWSTONE_MVR=scene.mvr GLOWSTONE_MVR_EXPORT=roundtrip.mvr cargo run --release
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
| `zip`, `roxmltree`, `gltf`, `image` | GDTF + MVR import/export (archive / XML / models / wheels) |
| `rfd` | native Import/Export file dialogs (GDTF, MVR) |

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
  and all DMX channels, then renders the **real articulated 3D model** through a
  full **optical-chain beam engine** (gobo/animation projection, color wheel /
  CMY / CTO, prism facet replication, frost, focus, iris, zoom, shutter/strobe,
  chromatic aberration — see [The beam engine](#the-beam-engine)). Tested with
  the Ayrton Khamsin. Still TODO: live per-channel DMX patch/control feeding the
  optical controls, GeometryReference instancing, framing-shutter blades.
- **MVR** import/export — *first cut implemented*: **Library ▸ Import MVR…**
  loads an `.mvr` scene (a ZIP of a `GeneralSceneDescription.xml` scene graph +
  embedded `.gdtf` fixtures + `.glb` stage/truss geometry) exported by a console
  or CAD tool (grandMA3, Vectorworks, Capture, Depence…). It places every
  **fixture** (resolving + sharing its embedded GDTF, with the real hang
  orientation and the DMX patch / FixtureID / mode preserved) and every static
  **scene object** (stage decks, truss, set, screens) as lit geometry that
  occludes the beams. **Export MVR…** writes the scene back out — fixtures,
  placement, patch, bundled GDTF + geometry — round-tripping UUIDs / addresses /
  modes / matrices. Coordinate handling (MVR is +Z-up, millimetres; geometry is
  metres) and the column-vector `<Matrix>` are converted to the app's +Y-up
  metre world. Tested with a 146-fixture / 52-object festival stage. Still TODO:
  Symdef/Symbol instancing, FocusPoint aim targets, GDTF-defined trusses,
  per-layer visibility toggles. See [MVR](#mvr-scene-exchange).
- **Live DMX input** — sACN / Art-Net feeding the fixture parameters.

## License

Dual-licensed under **MIT** or **Apache-2.0**, at your option.
