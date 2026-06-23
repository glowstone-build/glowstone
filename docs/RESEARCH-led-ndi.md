# LED screens & NDI — research and design

**Executive summary.** Add a new first-class scene kind, `LedScreen`, that models a video wall as a parametric grid of cabinets (pitch + cabinet-mm + panels-wide/high → *derived* total resolution and physical size). The visible surface is driven by **one crisp content texture** — NDI feed, still image, procedural test pattern, or solid colour — sampled onto the panel-grid quad via a new emissive material path on the existing mesh pipeline; it is emphatically **not** the multi-emitter `Fixture.cells` / per-cell DMX composite (that path stays for Spiider / LED bars only, and at screen resolution it is the "-69fps" trap). The wall still **emits a bit of light and feeds the haze**, but that contribution is deliberately cheap and low-frequency: a 1×1–2×2 blurred summary mip of the content becomes 1–4 synthetic `FixtureGpu` rows pushed into the *existing* beam/volumetric storage buffer — no new pass, no new buffer, no shadow map, cost comparable to one extra PAR can. NDI receive uses the **grafton-ndi** crate (Apache-2.0, NDI 6 SDK, macOS CI-tested) on a finder thread + receiver thread → latest-frame ring → `queue.write_texture`, with the closed SDK runtime dynamically loaded (never vendored). We ship a small library of generic cabinet types (indoor P3.9/P2.6, outdoor P4.8/P10, fine-pitch XR, transparent/mesh, floor tile, curved). Pixel-mapped Art-Net/sACN is a strictly secondary path for genuinely low-res creative walls; full video must come from NDI/media because DMX channel limits make per-pixel video impossible.

Throughout, the recurring theme is **performance-first lighting**: two representations of the same content — (1) a crisp full-res surface, (2) a tiny blurred summary that drives a cheap area-light + a handful of volumetric samples.

> **Status — ALL PHASES + CITP implemented.** Verified by build + 81 unit tests + headless screenshots (`PREVIZ_LED[_CONTENT|_IMAGE|_CURVE]`).
> - **Phase 1** — `Scene.screens: Vec<LedScreen>` (`src/scene/screen.rs`), generic component library (`ScreenProfile`), `led_screen_inspector` + library rows + outliner "Screens" folder + ray-pick + delete, and the renderer (`src/shaders/wall.wgsl` + `wall_pipeline`): one emissive quad per wall with a **distance-aware LED dot mask** (`fwidth`-driven — pixels resolve up close, solid far) + the cheap **blurred-summary area light** injected as a wide "plain" beam into the existing volumetric buffer. `.archie` FORMAT 2→3.
> - **Shared content-texture pipeline** — per-screen `ScreenRuntime` texture cache + a content bind group (group 1) on the wall pipeline; procedural walls bind a 1×1 placeholder. Live sources hand the renderer a runtime `LedScreen.frame` (RGBA8); `Image` is decoded by the renderer.
> - **Phase 2** — `ScreenContent::Image` (decode+upload, file picker) + **curved walls** (tessellated quad bent into a horizontal arc in the VS).
> - **Phase 3 (NDI)** — feature-flagged `grafton-ndi` behind `--features ndi` (`src/ndi/`): finder + per-source receiver threads → RGBA frames. The **default build is SDK-free** (graceful no-op stub); `--features ndi` needs the NDI 6 SDK (untestable in this sandbox — verified the feature resolves to grafton-ndi 1.0 and the stub path).
> - **Phase 4 (pixel-map DMX)** — `ScreenContent::PixelMapDmx` (`src/dmx/decode.rs::apply_screens`): a tiny `cols×rows` RGB grid from Art-Net/sACN → a small texture (SECONDARY, low-res). Unit-tested.
> - **CITP/MSEX** — pure-Rust `src/citp/`: base header + PINF/PLoc discovery + MSEX `CInf`/`RqSt`/`StFr` codec (unit-tested), plus a discovery + per-source streaming client → frames. `ScreenContent::Citp`.
> - **Phase 5 (MVR)** — `<VideoScreen>` import ↔ `LedScreen` export round-trip (`src/mvr.rs`), full fidelity via `previz*` attributes + a `<Sources>` child for foreign readers. Unit-tested.
> - **Content source pickers** in the inspector show discovered NDI + CITP sources.
> - **Physical-accuracy pass (fixes from live testing):** (a) **content is quantised to the LED grid** — each physical LED samples the content once at its centre and emits that single colour (the RGB sub-pixels show R/G/B of *that* colour). Previously the shader sampled the content continuously per fragment, so detail appeared *inside* each LED ("pixels inside pixels"); the smooth full-res content is now used only at the far LOD where LEDs are sub-pixel. (b) **Coloured area-light** — the wall's haze/floor contribution is no longer a single grey point: a small grid of emitters is sampled across the screen face (a downsampled per-region summary for image/live content, procedural for test patterns), each coloured by its region and given a localized cone, so a gradient/bars wall throws the *right colours* onto the floor + into the fog instead of a white wash.
> - **Polish pass (fixes from live testing):** (1) **real see-through transparency** — transparent/mesh walls draw in a second premultiplied-alpha pass (depth-test on, depth-write off) so the scene shows through the gaps; (2) **moiré fixed** — the LED dot mask now collapses to a solid surface *before* the dots go sub-pixel (no more phantom dots when zooming out); (3) **wall light + volumetrics now work** — the synthetic area-light had `iris = 0` which cropped the volumetric beam to zero; opened it, aimed the wash forward-and-down, and tuned the source size/cone so the screen casts a contained coloured glow in haze + a floor wash (scaled by the inspector `emit` slider); (4) **pixel-shape option** — `PixelShape` (SMD round / SMD square / discrete RGB sub-pixels) in the data model + inspector + `wall.wgsl`. FORMAT bumped 3→4. **Caveat:** image bytes don't round-trip through MVR (filename only); display gamma is stored but not yet applied.

---

## NDI protocol

NDI® (Network Device Interface) is a royalty-free protocol from Vizrt NDI AB for low-latency broadcast-quality video/audio/metadata over standard Gigabit Ethernet. Our role is **receive-only**: discover sources, connect to one, get decoded frames to upload to a GPU texture. The protocol is closed; the only sanctioned path is the official **NDI SDK** (a C-ABI shared library, `libndi`). Every Rust binding is a thin FFI wrapper over it.

**Tiers & codecs.**
- **Full / High-Bandwidth NDI** — NewTek **SpeedHQ**, an intra-frame (I-frame-only) DCT codec, 8-bit 4:2:2 (4:2:2:4 with alpha), visually near-lossless, sub-frame latency, symmetric. ~100–160 Mbps at 1080p60 (commonly "~125 Mbps"). The SDK encodes and decodes SpeedHQ **in software on all desktop platforms** (incl. Apple-silicon NEON) — for a receiver, no app-side codec work.
- **NDI|HX (HX1/HX2/HX3)** — long-GOP H.264/H.265, 4:2:0, much lower bitrate for constrained devices. On macOS the standard SDK **decodes HX via VideoToolbox automatically**, so a darwin receiver still does no manual decoding; you just get a CPU-side YUV/BGRA buffer.
- **NDI Bridge** tunnels streams across WAN; not relevant to a receiver.

**Discovery.** Two mechanisms (concurrent): **mDNS / Bonjour** (zero-config, service type `_ndi._tcp.` on UDP 5353; link-local) and the **NDI Discovery Server** (centralized, TCP 5959, persistent socket, crosses subnets). The SDK's `NDIlib_find_*` API returns `(name, url)` source tuples.

**Transport.** Default since NDI 5 is **Reliable UDP (RUDP)** — UDP + ACK/retransmit + aggregate congestion control, ~one UDP port per process. TCP (single/multi) and FEC multicast are alternatives. Full-NDI LAN latency is sub-frame.

**Frame model.** `NDIlib_recv_capture_v3` returns a video / audio / metadata / status frame or times out. Video (`NDIlib_video_frame_v2_t`): `xres,yres`, `FourCC`, rational `frame_rate_N/D`, `frame_format_type` (progressive/interleaved/field), timecode/timestamp, `p_data`, `line_stride_in_bytes`. **FourCC** of interest: **UYVY** (8-bit 4:2:2, fastest, no-conversion), **UYVA** (+alpha), **P216/PA16** (16-bit), **BGRA/BGRX/RGBA/RGBX** (packed, conversion cost). `NDIlib_recv_color_format_e` (`fastest`/`best`/`BGRX_BGRA`/…) controls what you receive; `NDIlib_recv_bandwidth_e` lets you request a low-res proxy stream for previews. Colour is BT.709 (HD); NDI 6 adds HDR (PQ/HLG) + up to 16-bit. Tally and PTZ are receiver-side conveniences we can ignore for v1.

---

## NDI integration in Rust

### Recommended crate: `grafton-ndi` (verified)

All Rust NDI crates are FFI wrappers over the same closed SDK — none reimplement NDI, all need the SDK/runtime present.

| Crate | Status (June 2026) | Receive | macOS | License |
|---|---|---|---|---|
| **grafton-ndi** | **v1.0.0, June 15 2026, actively maintained** *(corrected from initial "0.13.x" — it is now API-stable 1.0)* | `Finder` + `Receiver` + `FrameSync`, zero-copy borrowed frames | **Yes, CI-tested** | **Apache-2.0** |
| `ndi` (== `ndi-rs`) | v0.1.2, Aug 2022, abandoned. *(corrected: `ndi` and `ndi-rs` are the SAME `sp4ghet/ndi-rs` repo, not two crates)* | yes | **No** | non-standard |
| `ndi-sys` | 2 releases in 2022, raw bindgen only | raw symbols | — | MIT |
| `ndi-sdk` | v0.2.0, May 2025, thin | raw | — | MIT |
| `ndi-sdk-sys` | v0.1.2, June 2026 | raw | — | **GPL-3.0-or-later — incompatible, avoid** *(corrected: copyleft, would not slot into our MIT/Apache app)* |
| `gst-plugin-ndi` | v0.15.2, MPL-2.0, maintained | `ndisrc` | yes | MPL-2.0 (heavy; needs GStreamer) |

**Pick `grafton-ndi` v1.0.0.** It is the only crate that is current + API-stable, cross-platform with **CI-tested macOS** (our darwin target), exposes a real `Finder`+`Receiver`+`FrameSync`+zero-copy borrowed frames, targets the NDI 6.x SDK via runtime dynamic loading, and is Apache-2.0. Integration note: it needs **Rust 1.87+** and the NDI SDK 6.x headers at build time (bindgen); the runtime is resolved by the OS dynamic linker at run time, so a **missing runtime is a load-time failure, not a clean `Option`** — wrap init in an explicit presence check and degrade the inspector gracefully ("NDI runtime not found").

### Receiver thread → ring → wgpu texture

NDI capture is CPU-side and blocking, so:

1. **Finder thread** owns `Finder`, polls `current_sources()` once/sec, publishes `Vec<String>` source names via `Arc<Mutex<…>>` + `ctx.request_repaint()` — exactly the share-worker pattern (`src/share.rs:704-731`) and the DMX recv-thread pattern (`src/dmx/net.rs:198-238`).
2. **Receiver thread** (one per active NDI screen): `Receiver::capture()` blocks until a frame; writes the **latest** frame into a small latest-wins triple-buffer / `Mutex<Option<Frame>>` (previz wants newest, not a queue, like Art-Net). With grafton-ndi you can borrow the SDK buffer (saves one memcpy) but the borrow dies on the next capture, so copy once into the owned ring slot for cross-thread handoff.
3. **Render thread** takes the newest frame each tick and, **only if a new one arrived** (track a frame counter), uploads via `queue.write_texture` (origin 0, `bytes_per_row = w*4`).

**YUV handling.** True GPU zero-copy is **not** available — NDI hands you host memory, so there is always one host→device upload. Two routes:
- **BGRA**: ask the SDK for BGRA, texture `Bgra8Unorm`, straight upload, sample directly (SDK did YUV→RGB). Simplest. ~8.3 MB/frame at 1080p, ~500 MB/s at 60fps — well within budget and what `world.rs` already does for HDRIs.
- **UYVY (preferred bandwidth)**: upload the packed UYVY as a half-width `Rgba8Unorm` (texel = U,Y0,V,Y1) and do **BT.709 YUV→RGB in the wall fragment shader** — halves upload bytes, a 3×3 matrix multiply per pixel is negligible at the surface's crisp resolution.

### Licensing — clear-eyed (VERIFIED against the Nov-2024 NDI License Agreement)

**Yes, an MIT OR Apache-2.0 app can ship NDI receive on Win/macOS/Linux, free, with conditions.**

- **Proprietary SDK, not relicensable.** The SDK (source, binaries, docs) is Confidential under the EULA; you may **not vendor the closed `libndi` binaries into the OSS tree**. The carve-out is narrow: **only the NDI header files are MIT-licensed for open-source projects**, used solely to **dynamically load** the runtime. *(corrected from initial draft, which said "headers + runtime are MIT" — it is headers only.)*
- **Runtime: user installs, or you bundle.** Either link users to the official redistributable — **`ndi.link/NDIRedistV6`** for NDI 6 *(corrected: initial draft cited the stale V5 URL; env var `NDI_RUNTIME_DIR_V6`)* — or bundle it in your installer (keep it current; if silent-install, cover NDI's EULA terms in yours). Bundled libs go in your **app folder, not system paths**. Cleanest OSS path: ship nothing, dynamically load if present, link to the redistributable.
- **No fee / no registration / royalty-free** for receive. But acceptance of the EULA is still required, and §3.d pass-through terms mean any distribution that bundles the runtime must carry NDI's copyright notice + prohibit reverse-engineering NDI in your EULA. Your own code stays MIT/Apache.
- **Branding (mandatory).** Write **"NDI®"** (registered symbol on first use per doc); include **"NDI® is a registered trademark of Vizrt NDI AB."**; put a **link to ndi.video near where NDI is used/selected**; use "NDI" only as a *compatibility identifier* ("works with NDI®"), never in the product name (that requires contacting NDI first); don't redistribute NDI Tools.
- **Also worth knowing:** the EULA's **30-day SDK-currency rule at release** (§2.b) and the **appliance/embedded exclusion** (§1.b — desktop previz qualifies; a locked-down hardware box would not). Plain SpeedHQ receive triggers **no H.264/H.265/AAC codec obligation** (those only attach if you ship your own codec).

### Alternatives & where DMX fits

- **Syphon** (macOS) / **Spout** (Windows): same-machine **true GPU texture share** (IOSurface / DX shared texture), sub-ms, no readback — ideal local ingest from Resolume/TouchDesigner/Blender. Different APIs; no maintained Rust crate confirmed (you'd FFI them). Strongest combo for a cross-platform previz tool is **NDI (network, all OSes) + Syphon (macOS local) + Spout (Windows local)**.
- **DeckLink/SDI**: hardware capture, closed Blackmagic SDK; heavy.
- **File / HAP video**: deterministic offline playback; HAP is GPU-friendly (BC1/BC3).
- **Art-Net / sACN per-pixel** (already in app): right for pixel-mapped low-res arrays, **not** real video — see [DMX / pixel-mapping](#dmx--pixel-mapping).

---

## How other tools model LED walls

Two camps: **visualizers/CAD** (Vectorworks, Capture, depence) build a physical wall from cabinet/pitch and texture it; **media servers/mappers** (disguise, Resolume, Notch, TouchDesigner, MadMapper) treat the wall as a UV-mapped surface fed by a raster and sliced to LED-processor outputs.

- **disguise** — a **Screen** = any mesh with a UV map; the UV grid **is** the hardware output to the LED processor (must be rectangles). **Resolution is a manual "Resolution" property on the screen object** (canvas px), set to match the physical aspect — *(corrected from initial research, which said resolution lives only on the mapping and which framed disguise as a cabinet→derived-resolution builder. disguise has NO parametric cabinet/pitch builder; resolution is manual.)* Content (media, generators, Notch, NDI) flows in as layers via Direct/UV/Feed mappings.
- **Capture** — dedicated **LED panel library** simulating standard *and transparent* panels with **correct pixel pitch** (for visual realism). **Output Resolution is set manually to match the LED processor** (not auto-derived from pitch×size). A **Patch** property patches the media player to DMX so a console drives it. *(verified verbatim.)*
- **depence²** — a **Video-Wall Material** on scenic geometry: pitch + pixel size, **screen intensity in nits**, **AlphaMask** for transparent walls; content via integrated media server or external NDI.
- **grandMA3 / MA3D** — either a textured 3D model with image/video material, **or** a pixel-mapped multi-instance fixture (one cell = one sub-fixture) driven by the Pixel Mapper. No first-class parametric LED-wall object.
- **Resolume / TouchDesigner / Notch / MadMapper** — pure raster/slice/UV models with corner-pin/Bézier warp; no physical cabinet concept (TD builds the pixel map by hand; MadMapper LED is a DMX/SPI **Fixture** with 1:1 antialias off).
- **Vectorworks Spotlight** — the richest **cabinet-catalog → auto-resolution** model (assign a manufacturer panel symbol; pitch/counts/resolution/power/weight fall out; straight/circular/curved). **This is the parametric model archie adopts** — but note it is *Vectorworks'* pattern, not disguise's/Capture's.

**Union of input fields worth exposing.** Cabinet (w×h mm, pitch mm → derived px/cabinet, or enter px directly, or a panel preset); Array (cols×rows → **derived total px + physical m**, gap/bezel mm); resolution override; look/photometry (nits, gamma, white point, transparency/AlphaMask, off-state colour); geometry (flat/curved arc radius or per-column angle/freeform, module shape); UV/raster (auto grid, per-region rect for feed/slice); content/source (media, generator, NDI, Spout/Syphon, capture, solid; scale handling); addressing (optional DMX patch; FixtureID/stream name for MVR).

**MVR/GDTF support (VERIFIED).** MVR **does** define a native **`<VideoScreen>`** node (attributes `uuid`, `name`, `multipatch`; children incl. `<Geometries>`, `<Sources>`→`<Source linkedGeometry="…" type="NDI|File|CITP|CaptureDevice">`, `ScaleHandeling` (literal misspelling, enum `ScaleKeepRatio|ScaleIgnoreRatio|KeepSizeCenter`), plus standard `<Addresses>`/`<GDTFSpec>`/`<FixtureID>`). GDTF defines a **`<Display>`** geometry: "a self-emitting surface used to display visual media," with a **`Texture`** attribute = the named texture in the Model file swapped out for the media resource (plus `MediaServerLayer`/`Camera`/`Master`). **But neither has any native pixel-pitch or cabinet-grid concept** — they model *surface + texture-swap + source*, not the physical cabinet build. So the parametric LED wall (pitch, cabinet grid, derived resolution, nits) is genuinely **archie-only**.

---

## LED wall hardware & generic component library

**Pitch → pixels → metres.** Pixel pitch is the centre-to-centre LED distance (mm). Smaller pitch = more px/m = closer viewing (rule of thumb: min viewing distance in m ≈ pitch in mm). The core chain:

```
px_per_cabinet  = cabinet_mm / pitch_mm           (e.g. 500 / 3.90625 = exactly 128)
total_canvas_px = (cols × px_per_cab_w) × (rows × px_per_cab_h)
physical_size_m = (cols × cab_w_mm)   × (rows × cab_h_mm)   [+ gaps]
```

Pitches are reverse-engineered from clean counts on standard cabinets: P3.9 = 500/128 = 3.90625, P2.6 = 500/192, P4.81 = 500/104. **Physical metres are decoupled from resolution** — a P10 8×4.5 m wall is only 800×450 px; a P3.9 one is 2048×1152. Standard cabinets: **500×500, 500×1000, 600×600, 600×1200** (+broadcast-oddball 600×337.5). Brightness: indoor ~800–1500 nits (fine-pitch broadcast 600–1200), outdoor 4000–7000+. Rental tiles are **seamless** (inter-cabinet gap == pitch), so for opaque walls treat the canvas as continuous — **no LCD-style bezel**. The exceptions where structure is visible: **floor tiles** and **transparent** panels.

**Transparent / mesh LED is the modeling-critical type.** Sparse LED strips with wide gaps (or LEDs in glass) give **~65–90% transparency** so you see the stage/sky *through* it; pitches are coarse (P3.9–P15.6) and often dual (e.g. P3.9×7.8, horizontal≠vertical). It must render with **per-pixel alpha** — the cabinet is mostly empty, content composites over what's behind, structure barely occludes. A plain opaque emissive quad would be wrong.

### Generic component types to ship (VERIFIED numbers)

| Generic type | Pitch (mm) | Cabinet (mm) | px/cabinet | Nits | Env | Notes |
|---|---|---|---|---|---|---|
| **Indoor standard** | 3.9 | 500×500 | 128×128 | 1200 | Indoor | The default. Opaque emissive. |
| **Indoor high-res** | 2.6 | 500×500 | 192×192 | 1500 | Indoor | Concert/conference (ROE Carbon-class). |
| **Broadcast / XR fine-pitch (COB)** | 1.5625 | **500×500** | 320×320 | 1000 | Indoor | Virtual production; ≥3840 Hz; flat smooth. *(real XR ≈ P1.5; 500/320 = 1.5625 keeps the count integer like the other rows — the catalog label rounds to "P1.56".)* |
| **Outdoor medium** | 4.8 | 500×1000 | 104×208 | 4500 | Outdoor | IP65; festival/stadium. |
| **Outdoor long-throw** | 10 | 960×960 | 96×96 | 6000 | Outdoor | Billboard / building side. |
| **Transparent / mesh** | 7.8 (3.9–15.6) | 1000×500 | 128×64 | 4500 | Both | 65–90% transparency; **alpha required**; default `opacity≈0.35`. |
| **Curved / creative** | 5.77 | 600×1200 | 104×208 | 6000 | Both | ROE Carbon-class; curvable ±(15° concave/10° convex). |
| **LED floor tile** | 4.81 | 500×500 | 104×104 | 1500 | Indoor | Load-bearing; visible inter-pixel structure. |
| **LED strip / batten** | ~10 | 300×N | linear | 1500 | Both | Creative linear element. |

*(Hardware figures verified against vendor spec sheets: P3.9→128, P2.6→192, P4.81→104×208, P5.77→104×208 are all exact and confirmed. One stray claim corrected — "CB5 = 7.5 kg" is wrong; CB5 is 600×1200 / 13.85 kg / 6000 nits; 7.5 kg is a 500×500 indoor panel.)*

---

## Proposed data model in archie

**Recommendation: a new `screens: Vec<LedScreen>` on `Scene`.** A `Scene` is a flat owner of one `Vec` per *kind* (`fixtures`, `environments`, `geometry`, + `world`/`mvr`, `src/scene/mod.rs:315-326`). An LED wall is a fourth distinct kind — an emissive, content-driven surface that is neither a light (Fixture), an occluder (SceneGeometry), nor a media volume (Environment). Rejected alternatives: reusing **`SceneGeometry`** (its identity is "imported glTF mesh that occludes beams"; a wall is a generated parametric quad that *emits*), **`Environment`** (an AABB media box, not an oriented quad), and — **forbidden by design constraint B** — the multi-emitter **`Fixture.cells`** path (`src/scene/fixture.rs:86`; recomposing millions of `[f32;3]` per frame at 1080p/4K is the "-69fps" path; cells stays for Spiider/LED bars only).

New file `src/scene/screen.rs`, `pub mod screen; pub use screen::LedScreen;` alongside `src/scene/mod.rs:5-11`:

```rust
//! An LED video wall: a parametric grid of panels driven by ONE content
//! texture sampled crisply onto the surface. NOT a multi-emitter fixture —
//! the cells/DMX cell-layer path (src/scene/fixture.rs:86, src/dmx/decode.rs)
//! is NEVER used here.
use glam::Mat4;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LedScreen {
    pub name: String,
    pub panel_type: String,        // library component, e.g. "Indoor 3.9mm 500×500"
    pub transform: Mat4,           // world placement (Y-up, m); same as SceneGeometry.transform

    // physical cabinet grid
    pub cabinet_mm: [f32; 2],      // one cabinet face (W,H) mm
    pub cabinet_px: [u32; 2],      // native px/cabinet, e.g. [128,128]
    pub panels_wide: u32,
    pub panels_high: u32,
    pub gap_mm: f32,               // inter-cabinet bezel; 0 = seamless
    pub curvature_deg: f32,        // horizontal arc across whole wall; 0 = flat

    // look / photometry
    pub nits: f32,                 // peak brightness; scales surface AND light summary
    pub opacity: f32,              // 0..1 see-through (1 = opaque); transparent/mesh
    pub gamma: f32,                // default 2.2
    pub emit: f32,                 // per-screen volumetric/area-light multiplier (cf. Fixture.beam)
    pub hidden: bool,              // outliner eye toggle (cf. SceneGeometry.hidden)

    pub content: ScreenContent,    // stable descriptor only — NO runtime frames/textures
    pub mvr: Option<crate::mvr::MvrObjectMeta>, // round-trip metadata if imported
}

/// Only stable, serializable descriptors live here. Decoded NDI frames, the GPU
/// texture, and the blurred lighting summary are RUNTIME-ONLY (ScreenRuntime).
/// Mirrors World keeping hdri *bytes*+*name* but never the decoded texture
/// (src/scene/mod.rs:286-300; decode in renderer ensure_world ~mod.rs:594-624).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum ScreenContent {
    TestPattern(TestPattern),                 // Grid / Bars / Gradient
    SolidColor([f32; 3]),                     // linear RGB; summary IS the colour
    Image { name: String, bytes: std::sync::Arc<Vec<u8>> }, // bundled like World.hdri
    Ndi { source_name: String },              // stable name only; frames never serialized
    PixelMapDmx(PixelMap),                    // SECONDARY low-res creative path
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TestPattern { Grid, Bars, Gradient }

/// Deliberately tiny DMX control grid (creative walls only). cols*rows is small
/// (cap e.g. ≤4096). RGB per cell over a few channels; inline patch, NOT routed
/// through PatchTable, NOT through the per-cell cells composite.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PixelMap { pub cols: u32, pub rows: u32, pub universe: u16, pub start_address: u16 }

impl LedScreen {
    pub fn resolution(&self) -> [u32; 2] {
        [self.panels_wide * self.cabinet_px[0], self.panels_high * self.cabinet_px[1]]
    }
    pub fn size_m(&self) -> [f32; 2] {
        let g = self.gap_mm;
        [(self.panels_wide as f32 * self.cabinet_mm[0]
            + self.panels_wide.saturating_sub(1) as f32 * g) / 1000.0,
         (self.panels_high as f32 * self.cabinet_mm[1]
            + self.panels_high.saturating_sub(1) as f32 * g) / 1000.0]
    }
    pub fn pitch_mm(&self) -> f32 { self.cabinet_mm[0] / self.cabinet_px[0].max(1) as f32 }
}
```

**Runtime cache (never serialized), renderer-side, keyed by screen index** — same idea as `ensure_world`/`gdtf_cache` (`src/renderer/mod.rs:588`):

```rust
struct ScreenRuntime {
    texture: wgpu::Texture,   // full-res crisp surface content
    bind_group: wgpu::BindGroup,
    content_key: u64,         // image Arc ptr / solid hash / NDI frame gen
    avg_color: [f32; 3],      // 1×1 summary → soft area-light term
    low_grid: [[f32; 3]; 4],  // 2×2 representative emitters → volumetric samples
}
```

**serde / bincode.** The whole `Scene` derives serde (`src/scene/mod.rs:315`); `ScreenContent::Image` bundles bytes exactly like `World.hdri`, NDI persists only `source_name`, and the runtime texture/summary are rebuilt on load (like `WorldTexture` from `hdri` bytes). The `#[serde(skip)]` boundary mirrors `Fixture.gdtf` (`src/scene/fixture.rs:28-29`). **Caveat:** bincode is positional with a hard `FORMAT` gate (`src/ui/project.rs:31` `FORMAT=2`, rejected at `:99-103`). Adding `screens` to `Scene` breaks old files unless we **bump `FORMAT`** and add a tolerant default-empty migration. This is the same cost any new `Scene` field carries.

**Selection / outliner / delete.** Selection is "exactly one *kind* active" with a `Vec<usize>` per kind (`src/scene/mod.rs:174-185`). Add `pub screens: Vec<usize>`; add `Selection::screen(i)`, `toggle_screen`, `contains_screen`, `primary_screen` (mirror the geometry variants at `:193-241`); every existing constructor/toggle gains `self.screens.clear()`. `Hit` enum (`src/ui/panels.rs:3061-3064`) gains `Screen(usize)`; `pick` (`:3070-3110`) ray-tests each non-hidden screen's oriented quad (cheaper than the geometry AABB) and a surface should win over a fog box behind it; wire the `Selection::screen` arm alongside geometry/environment (`:2088-2096`, drag-select `:2113-2117`). `commit_delete` (`src/ui/mod.rs:1062-1095`) gains a plain descending-remove block (like geometry at `:1083-1090`) plus `!screens.is_empty()` in the reset guard — screens carry no patch/cue/group refs (the `PixelMapDmx` patch is inline, not in `PatchTable`), so no remap needed.

---

## Rendering

previz's renderer already has every primitive needed. The visible surface is a **new emissive material flag on the existing mesh pipeline**; the content texture uploads exactly the way `world.rs`/`atlas.rs` already do `queue.write_texture`; and the lighting/volumetric contribution is **one (or a few) extra `FixtureGpu` rows** fed into the same `array<Fixture>` storage buffer the beams already loop. We are **not** adding a parallel light engine.

### The visible surface — one emissive quad, not per-pixel geometry

**Reuse the mesh pipeline with a new material flag — do NOT add per-pixel geometry.** The mesh path already has emissive: `MeshVertex.emissive` (`mesh.rs:21`) drives `emit = in.color * intensity * 24.0; rgb = mix(lit, emit, emissive)` (`mesh.wgsl:233-234`). That `24.0` HDR multiplier is what makes lens bloom; an LED surface is the same, but its colour comes from a **sampled content texture** instead of a flat instance colour.

Two changes: (1) **add a UV** — don't widen every `MeshVertex`; draw the wall as `panels_wide*panels_high` **instances of a unit quad** and carry each panel's UV sub-rect (`uv_min`,`uv_scale`) in the `MeshInstance` row (`mesh.rs:63`), using local `position.xy` as the in-panel UV. Two triangles per panel, no per-pixel anything. (2) **Bind the content texture** in its **own pipeline** = the mesh shader recompiled with an extra group 3 `{content_tex, content_samp}` + an `is_screen` flag, built next to `mesh_pipeline` (`pipeline.rs:112`) with layout `[camera, light, world, content]`. The wall is the one place a near-duplicate pipeline is justified, because it draws separately from the lit-mesh batch anyway. Fragment:

```wgsl
let c = textureSample(content_tex, content_samp, uv).rgb;   // YUV→RGB here if UYVY
return vec4(c * nits_scale, 1.0);   // HDR → blooms/tonemaps for free
```

Define `nits_scale = nits / REF_NITS` against a reference white (`REF_NITS ≈ 1500`), folded into the same HDR scale as the mesh `24.0` emissive multiplier — so a 1500-nit and a 6000-nit wall stay *relatively* correct (4× brighter) and the bloom knee (`post.wgsl:33`) behaves. v1 ingests **BT.709 SDR only**; NDI 6 HDR (PQ/HLG) is out of scope for now (tonemap-to-709 or ignore), which keeps the nits/gamma fields a defined meaning.

**Pass order** (`mod.rs record_scene`): draw the wall **inside the existing forward pass** (`mod.rs:1604-1720`), after MVR geometry (`:1692`) and before the lens discs, with depth-write **on** — so the volumetric raymarch stops at it (`volumetric.wgsl:122-134` clips `t_surface` from `depth_tex`), beams occlude correctly, and it blooms in the `Rgba16Float` HDR target (`viewport.rs:59`). Transparent walls are the exception (below).

### CPU frame → texture upload

Near-copy of `world::WorldTexture` + `atlas.rs:162 write_mip`. One `Rgba8Unorm` (or `Bgra8Unorm` for BGRA NDI) texture sized to content resolution, `TEXTURE_BINDING | COPY_DST`; HDR-ness comes from the `nits_scale` multiply, not the texel format. Per-frame `queue.write_texture` **only when a new frame arrived** (frame counter), so a still image or 30fps feed on a 60fps render uploads at most once per source frame. wgpu stages `write_texture`, so a single texture is safe vs. an in-flight read; add a second ping-pong texture only if profiling shows a stall. **Skip a full mip chain** for the crisp surface (viewed roughly head-on); add a GPU mip-blit only if oblique minification aliasing appears. **Resident cost:** each feed's surface texture is ~8.3 MB (1080p) / ~33 MB (4K) BGRA8 plus the per-frame upload bandwidth — a provisional cap of **~4 concurrent full-res feeds** before an LOD/proxy scheme is warranted (the SDK's low-bandwidth proxy stream is the lever for secondary/off-screen walls), mirroring the 60 klm fixture-cap precedent.

### Grid / bezel / transparency / curve look

All in the fragment shader / per-panel instancing — none of it touches the lighting summary. **Bezel/seam**: `smoothstep` border fade toward dark near panel UV edges, width from `pitch_mm` (opaque seamless walls draw it faint or off). **Pixel-pitch dot/louvre**: distance-aware procedural dot mask (`fract(uv*pixel_count)` radial falloff), faded out at distance with `fwidth(uv)` — appears only at close range, a few ALU ops. **Transparency** (`opacity < 1`): a separate small alpha pipeline variant (blend `SrcAlpha`/`OneMinusSrcAlpha`, depth-test on, **depth-write off**) drawn after opaque depth — so beams and fog **show through** mesh walls (exactly right for see-through LED, since the volumetric pass keys off depth). Final alpha is **structural** — `alpha = dot_mask * opacity` (the LED dots/strips are opaque, the gaps fully transparent), *not* the content texture's A channel; reserve content-alpha (UYVA) for the rare keyed-feed case. **Curve**: place the panel instances on an arc at CPU build time (each panel's `model` matrix rotated by its column's arc angle); UVs stay the flat grid — matching how real curved walls are driven (processor sends a flat canvas, cabinets are physically curved). No shader/UV change. **Test pattern / solid / fallback**: generated entirely in-shader from UV via a `content_kind` uniform (0 solid, 1 procedural pattern, 2 sampled) — no texture, no library, and the **safe default for a freshly placed wall** (LED processors boot to a test pattern).

### Lighting & volumetric contribution (cheap/blurred)

This is the central, non-optional, deliberately-cheap part — and the one place the **"-69fps"** risk lives. The design makes a wall cost **a few synthetic beams, not a per-pixel integral.**

**Mechanism: the wall becomes 1–4 synthetic `FixtureGpu` rows fed into the existing beam machinery.** Both the haze raymarch and surface lighting already loop a single `array<Fixture>`/`array<Light>` storage buffer (`volumetric.wgsl:35` / `mesh.wgsl:65`), built CPU-side in `record_scene` (the `build_beam_gpus` loop, `mod.rs:1231-1267`). Adding a wall is: after that loop, **push a few more `FixtureGpu` rows** whose `color` comes from the wall's **blurred-summary mip**. The shaders are oblivious — they just see more entries in `arrayLength(&fixtures)`. **Zero new passes, zero new buffers, zero new bind groups, no shadow map.**

- **(a) One soft area light from the average colour — analytic, ~free. Ship always.** One `FixtureGpu` at the wall centre, `dir` = wall normal, a **very wide** `tan_half` (broad emitter, not a spotlight), few-metre `range`, `color` = the **1×1 average mip** × `nits_scale` × an emission-coupling factor (~0.1–0.3 — a wall lights the room far less than its on-axis brightness suggests). Mark it **plain** (`extra.z = -1` sentinel, `mod.rs:251`/`mesh.wgsl:188`) so it **skips the entire gobo/cookie/shutter chain** — the dominant per-step saving, exactly what a textureless area emitter wants. `misc.w = -1` = no shadow map (saves a whole depth pass).
- **(b) Optional 2×2 representative emitters.** For a wall where the average loses left/right or top/bottom variation (sunset gradient, split-screen), use the **2×2 mip**: four rows, one per quadrant centre on the wall plane, each coloured by its mip texel — directional variation in the haze (room glows warm on one side, cool on the other) for 4 beams instead of 1. The runtime summary **caps at 2×2** (`low_grid: [[f32;3];4]`); "blur ~90%" means a 4×4 mip is already overkill. Default to (a) the 1×1 area light; opt into the 2×2 quadrant emitters only when the wall has strong left/right or top/bottom variation.
- **(c) Surface wrap/diffuse is already there** — `mesh.wgsl:210` half-Lambert (`dot(n,l)*0.5+0.5`) lights adjacent geometry softly even edge-on; the wall emitters inherit it. Give them large `lens_r`/wide `tan_half` so the floor pool (`mesh.wgsl:166-176`) is broad and soft, not a sharp disc.

**Why it's nearly free.** The existing **per-fixture radial pre-cull** (`volumetric.wgsl:234`, `mesh.wgsl:175`) reduces off-beam samples to a single dot-product reject; the **plain fast-path** skips the optics chain; there's no new pass/buffer/shadow map. A wall adds 1–4 cheap rejects + a phase eval only for samples in front of it — marginal cost ≈ **one extra PAR can**.

**Why the blur is correct, not a compromise.** Volumetrics render at **half resolution** (`viewport.rs:159-167`) and the composite Gaussian-blurs the result. Driving the haze from a 1×1–2×2 mip is **matched to the output bandwidth** — resolving content per-pixel into fog would be `pixels × steps × screen_pixels` work for detail the half-res + blur throws away. The summary mip is the correct LOD. **Where the summary comes from:** `SolidColor` — the colour itself (free). `Image`/`TestPattern` — derive once on load via a strided 2×2/4×4 box downsample, cache by `content_key`. `Ndi` — per-frame: a strided CPU downsample of the just-received frame to 16 texels is a fast linear scan (or a one-pass GPU blit if profiling demands keeping it off the CPU thread).

**File anchors:** `src/renderer/mod.rs` (forward pass 1604-1720, beam build 1231-1267, volumetric 1764-1809), `mesh.rs` (MeshVertex/Instance, emissive 21/63), `pipeline.rs` (mesh_pipeline 112), `atlas.rs` (write_texture), `world.rs` (image→texture+mips), `viewport.rs` (HDR_FORMAT 59), `shaders/mesh.wgsl` (emissive 233, half-Lambert 210, plain 188), `shaders/volumetric.wgsl` (beam loop 201, plain 251, radial pre-cull 234), `shaders/post.wgsl` (bloom knee 33).

---

## Inspector & UX

`inspector()` (`panels.rs:933`) dispatches to a new **`led_screen_inspector()`** right after the `selection.geometry` block (`:954-959`), guarded by `filter(|&i| i < scene.screens.len())`. Modeled on `geometry_inspector` (`:1327-1402`) for transform decompose and `environment_inspector` (`:1277-1321`) for the Grid idiom. Header: `ui.heading(name)` + weak subtitle + `Visible` checkbox (`:1333-1350`).

- **Transform** — `CollapsingHeader("{INSPECTOR}  Transform").default_open(true)`. The lossless decompose/recompose from geometry_inspector (`:1356-1401`): Position 3× `DragValue.speed(0.05).prefix("x ")`; Rotation 3× `.speed(0.5).suffix("°")`; uniform Scale `.speed(0.005).range(0.001..=1000.0)`; recompose only on edit (`rs_changed`/`pos_changed`) so curved imports aren't flattened. Append the live `size W×H×D m` weak label.
- **Panel** — Grid `num_columns(2).striped(true)`. "Type" → `ComboBox::from_id_salt("led-component")` over `library.screens` (the ComboBox idiom at `:1018-1026`); selecting writes `panel_type` and resets cabinet-mm/cabinet-px to type defaults. "Cabinet" → two `DragValue.range(50.0..=2000.0).suffix(" mm")` (derived-from-type, editable for custom). "Pitch" → **weak read-only label** `pitch_mm()` unless "Custom" type, then editable `DragValue.range(0.7..=40.0).suffix(" mm")` (source of truth is `cabinet_px`; editing a custom pitch rewrites `cabinet_px = round(cabinet_mm / pitch)` so `resolution()` stays integer).
- **Array** — "Panels wide"/"Panels high" → `DragValue.speed(0.2).range(1..=64)`. Derived **`"{} × {} px"`** label `ui.label(RichText::new(format!("{} × {} px", res[0], res[1])).weak().small())` (immediate-mode, live-updates), plus a weak `0.13 Mpx · 2.00 × 1.00 m` line (the 4×2 default of the Indoor 3.9 mm 500×500 / 128 px panel: 512×256 px) from `resolution()`/`size_m()`.
- **Surface** — "Brightness" `DragValue.range(50.0..=5000.0).suffix(" nits")` default 1500; "Gamma" `.range(1.0..=3.0)` default 2.2; "Bezel" `.range(0.0..=20.0).suffix(" mm")`; "Transparency" `Slider::new(&mut (1.0-opacity), 0.0..=1.0)`; "Curvature" `.range(-30.0..=30.0).suffix("°")`.
- **Content** — `.default_open(true)`. A source `ComboBox` (id_salt "led-source") over `ScreenContent`, then source-specific rows: **TestPattern** → second ComboBox; **Solid** → `ui.color_edit_button_rgb` (`:1271`); **Image…** → `Button("Choose image…")` → `rfd::FileDialog…add_filter("Image", &["png","jpg","jpeg","exr","hdr"])` (mirrors `:604-625`), store bytes like `World.hdri`; **NDI** → `ComboBox` over `ndi.names()` from the finder thread + a weak "Searching… / N sources" status, `add_enabled(false, …)` + "NDI runtime not found" when the finder failed to init; **Pixel-map DMX** → grid + patch widgets, **last in the enum**, with a weak "low-res creative walls only — use NDI/media for hi-res" hover note.

**Library registration** (`src/scene/library.rs`). Add a third profile family beside `FixtureProfile`/`EnvironmentProfile`:

```rust
pub struct ScreenProfile {
    pub name: &'static str, pub category: &'static str,  // "LED Wall"
    pub cabinet_mm: [f32; 2], pub cabinet_px: [u32; 2],
    pub gap_mm: f32, pub transparent: bool, pub default_nits: f32,
}
pub struct Library { /* … */ pub screens: Vec<ScreenProfile> }
```

Register the generic types in `Library::standard()` next to the `FogBox` profile (`library.rs:99-107`). `LibKind` (`panels.rs:507-511`) gains `Screen(usize)`; `library_rows()` (`:524-560`) grows a fourth loop with `meta: format!("{}mm · {:.0}×{:.0}mm{}", pitch, w, h, if transparent {" · mesh"} else {""})`; `add_library_row()` (`:563-572`) gains `LibKind::Screen(i) => Selection::screen(scene.add_screen(&library.screens[i]))`; `Scene::add_screen` mirrors `add_environment` (`mod.rs:489-497`) — name "LED Wall N", default 4×2 array upright at back-of-stage. The browser groups by `row.category` automatically, so "LED WALL" appears with no extra code. The outliner gets a "Screens" folder beside Objects/Fixtures/Environment with the same eye-toggle (`hidden`).

**MVR note.** Full screen fidelity (component, array, content, pixel-map) round-trips **only through `.archie`**; MVR is geometry-and-placement only (see below).

---

## DMX / pixel-mapping

**The PRIMARY high-res path is NDI/media as a single crisp texture (constraints A/B). DMX is SECONDARY and low-res-only.** A 4K wall is ~8.3M pixels; one DMX universe is 512 channels ≈ 170 RGB cells, so a full 64-universe rig tops out at ~10k cells, and a 1080p frame (~2.07M px) would need ~12,200 universes — past sACN's 63,999 / Art-Net's 32,768 caps and bandwidth-limited long before. **DMX cannot carry video.** This is the channel-limit reason full video must come from NDI/media.

So `PixelMapDmx` builds a **small control grid, never a per-screen-pixel composite, and never the `cells` path**. The per-cell HTP cell-layer composite in `src/dmx/decode.rs:17-27` stays exclusively for Spiider/LED-bar fixtures (constraint B). Instead, each frame a dedicated step reads `UniverseSnapshot` (`src/dmx/universe.rs:43-65`, `snap.get(universe)`/`snap.is_live`) for `cols*rows` RGB triples starting at `base_universe`/`base_address` (walking universe boundaries), writes them into a tiny `cols × rows` RGBA8 CPU buffer, and uploads it **once per frame** to a small texture. The renderer bilinearly upscales that onto the wall (cheap) and downsamples it further for the area-light summary — the constraint-D blur is **free** here because the grid is already tiny. Keep this **inline on `PixelMap` (`universe`,`start_address`), out of `PatchTable`** (which is index-parallel to `scene.fixtures`, `patch.rs:1-10`) so it can't corrupt fixture decode; clamp `cols*rows` to a sane cap with a warning. Canonical layout: RGB, 3 ch/cell.

---

## Phased implementation plan

**Phase 1 — Static wall geometry + test pattern + grid look + inspector + library.**
Files: new `src/scene/screen.rs`; `src/scene/mod.rs` (`screens` field, `Selection::screen`/toggles, `add_screen`, bump `FORMAT` + default-empty migration in `src/ui/project.rs`); `src/scene/library.rs` (`ScreenProfile` + `standard()`); `src/ui/panels.rs` (`led_screen_inspector`, `Hit::Screen`+pick, `LibKind::Screen`, library rows); `src/ui/mod.rs` (`commit_delete` arm); renderer (new content pipeline in `pipeline.rs`, per-panel instanced emissive draw + procedural test-pattern/solid/bezel in a new wall WGSL, drawn in the forward pass). **Accept:** place a wall from the library, see a procedural test pattern + grid/bezel, edit panels/pitch/nits in the inspector with a live "W×H px" label, select/move/delete it, save+reload `.archie`.

**Phase 2 — Image / solid content + curve.**
Files: `screen.rs` (`ScreenContent::Image`/`SolidColor` wiring), `panels.rs` (file dialog + colour picker), renderer (decode image like `world.rs`, upload, curved per-panel arc placement). **Accept:** load a PNG/EXR onto a wall, set a solid colour, bend a wall to an arc; nearby geometry/haze pick up the correct soft wash colour (the 1×1/2×2 summary path lands here).

**Phase 3 — NDI receive.**
Files: new `src/ndi/finder.rs` + `src/ndi/receiver.rs` (grafton-ndi behind a `feature = "ndi"`, dynamic-load presence check), app struct owns `NdiSources` (lazy `ensure_started` + `request_repaint`), `panels.rs` NDI ComboBox, renderer per-frame `write_texture` from the latest-frame ring + UYVY→RGB shader path + per-frame summary downsample. Add the `NDI®` attribution + ndi.video link in the UI. **Accept:** pick a live NDI source, see it crisp on the wall at video rate, the room/haze get a cheap blurred wash; pulling the runtime makes the picker degrade gracefully.

**Phase 4 — Pixel-map DMX (secondary).**
Files: `screen.rs` (`PixelMapDmx`/`PixelMap`), `panels.rs` (grid+patch widgets + perf note), a decode step reading `UniverseSnapshot` into a tiny grid texture (inline patch, **not** `PatchTable`, **not** `decode.rs` cells). **Accept:** drive a small (≤16×16) creative wall from Art-Net/sACN; cells composite untouched; FPS unaffected.

**Phase 5 — MVR round-trip (if feasible).**
Files: `src/mvr.rs`. Import: a `<VideoScreen>` keeps coming in as `SceneGeometry` by default (no behaviour change), optionally promoted to an `LedScreen` (transform from the node; pitch/array inferred or defaulted). Export: emit a `<VideoScreen>` node + generated quad `Geometry3D` at the right size/place; content/surface params have no portable MVR form and persist only in `.archie`. **Accept:** export a wall, re-import it as a placed screen of the right size; document that fidelity beyond geometry+placement is `.archie`-only.

---

## Open questions & risks

1. **`FORMAT` bump / migration.** Adding `screens` trips the hard version gate (`src/ui/project.rs:99-103`). Bump `FORMAT` to 3 with a tolerant default-empty path, or accept that old `.archie` files won't load? (Recommend the migration.)
2. **NDI dependency policy.** Feature-flagged (`feature = "ndi"`) so the OSS default build has no NDI dep, with a runtime presence check (missing runtime is a load-time failure, not a clean `Option`). Ship NDI on by default or opt-in?
3. **Simultaneous-feed budget.** How many concurrent 1080p/4K NDI feeds before GPU memory / upload bandwidth needs an LOD/proxy scheme or a hard cap (à la the 60 klm fixture cap)? The SDK's low-bandwidth proxy stream is the obvious lever for off-screen/secondary walls.
4. **Emission-coupling constant.** The nits→synthetic-emitter `color` scale (~0.1–0.3) needs a perceptual tuning pass against reference photos — a wall should light a performer's front noticeably without flooding the stage like a giant softbox.
5. **Transparent-wall + fog ordering.** Exact pass placement of alpha-blended see-through walls relative to the volumetric composite (`mod.rs:1766-1809`): should beams scatter in front, behind, or both? Depth-write policy (off, for see-through) decides this and differs from opaque walls — needs prototyping.
6. **Per-frame summary: CPU vs GPU.** Is a strided CPU 2×2/4×4 downsample cheap enough every NDI frame, or do we want a one-pass GPU blit to keep it off the CPU thread entirely?
7. **MVR promotion.** Should import auto-promote `<VideoScreen>` to `LedScreen` (inferring pitch/array) or keep it static geometry with an opt-in promote? (Recommend static-by-default for v1.)
8. **Curvature richness.** A single horizontal-arc scalar covers the common case; do we eventually need per-axis / freeform (bent corners, concave+convex) curvature? Note `size_m()` returns the **flat developed** panel extent — a curved wall's chord differs from its arc length, and the inspector size label + area-light centre placement assume the flat plane (curvature is a render-time placement only).
9. **Single source per wall (v1 non-goal).** Each `LedScreen` owns one whole content source; a shared logical canvas spanning several walls and sub-region / feed mapping (disguise-style "Feed/UV") is explicitly future work, not v1.
