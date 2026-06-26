//! The renderer: owns the wgpu device/queue/surface, the scene pipelines, the
//! offscreen HDR viewport target, the volumetric + post passes, and the egui
//! paint pass.
//!
//! Per frame the CPU fills a camera uniform, per-object instance rows, the
//! dynamic line geometry, and the volumetric uniforms (camera inverse, fog box,
//! fixtures as spotlights); the GPU renders the forward scene into an HDR
//! target, raymarches volumetric beams into it, then blooms + tonemaps it down
//! to the LDR texture egui shows. See `docs/RESEARCH-volumetrics.md`.

mod atlas;
pub mod camera;
pub mod fixture_model;
pub mod gpu_timer;
pub mod mesh;
mod noise;
mod pipeline;
mod shadow;
pub mod viewport;
mod world;

pub use gpu_timer::PassTimings;

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use winit::window::Window;

use crate::optics;
use crate::scene::library::FixtureGeometry;
use crate::scene::screen::{LedScreen, ScreenContent};
use crate::scene::{Fixture, RenderSettings, Scene, Selection, ViewportMode};
use camera::{CameraUniform, OrbitCamera};
use mesh::{GpuMesh, GrowBuffer, LensInstance, LineVertex, MeshInstance, WallInstance};
use viewport::Viewport;

/// Camera + scene data for the volumetric raymarch (mirrors `Volumetric` in
/// `volumetric.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VolumetricUniform {
    inv_view_proj: [[f32; 4]; 4],
    eye_time: [f32; 4],
    fog_min_density: [f32; 4],
    fog_max_g: [f32; 4],
    albedo_beam: [f32; 4],
    counts: [f32; 4],
    /// x = Helmholtz–Kohlrausch chroma read-up strength (saturated beams read more
    /// strongly in haze); yzw reserved. Mirrors `chroma` in volumetric.wgsl.
    chroma: [f32; 4],
    /// Tiled light culling: x = tiles_x, y = tiles_y, z = tile size (full-res px),
    /// w = wide-light count (unused by shader). The ray's screen tile bounds which
    /// beams it marches. Mirrors `tile` in volumetric.wgsl.
    tile: [f32; 4],
}

/// Temporal-accumulation uniform (mirrors `TemporalU` in vol_temporal.wgsl). Drives
/// the EMA reproject-resolve that smooths the jittered half-res volumetric.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TemporalUniform {
    cur_inv_view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],    // xyz = eye, w = far distance
    params: [f32; 4], // x = history opacity; yzw reserved
}

/// Froxel volumetric uniform (mirrors `Froxel` in froxel.wgsl / `FroxelU` in
/// post.wgsl). `dims.w` = shared shadow layer; `planes` = near/far ray distance.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FroxelUniform {
    inv_view_proj: [[f32; 4]; 4],
    eye_time: [f32; 4],
    fog_min_density: [f32; 4],
    fog_max_g: [f32; 4],
    albedo_beam: [f32; 4],
    dims: [f32; 4],
    planes: [f32; 4],
}

/// Froxel-volumetric resources (PREVIZ_FROXEL). A frustum-aligned 3D grid:
/// `inject` (compute) writes per-cell scatter+extinction into `inject_tex`,
/// `integrate` (compute) marches +Z into `result_tex`, and a fragment composite
/// trilinearly samples `result_tex`. Created only when the adapter supports
/// rgba16float storage textures.
struct FroxelState {
    dims: (u32, u32, u32),
    inject_view: wgpu::TextureView,
    result_view: wgpu::TextureView,
    compute_layout: wgpu::BindGroupLayout,
    inject_pipeline: wgpu::ComputePipeline,
    integrate_pipeline: wgpu::ComputePipeline,
    uniform: wgpu::Buffer,
    sampler: wgpu::Sampler,
    composite_layout: wgpu::BindGroupLayout,
    composite_pipeline: wgpu::RenderPipeline,
}

impl FroxelState {
    fn new(device: &wgpu::Device) -> Self {
        let dims = (160u32, 90u32, 64u32);
        let tex = |label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: dims.0,
                        height: dims.1,
                        depth_or_array_layers: dims.2,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let inject_view = tex("froxel-inject");
        let result_view = tex("froxel-result");
        let compute_layout = pipeline::froxel_compute_layout(device);
        let (inject_pipeline, integrate_pipeline) =
            pipeline::froxel_compute_pipelines(device, &compute_layout);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("froxel-uniform"),
            size: std::mem::size_of::<FroxelUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("froxel-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let composite_layout = pipeline::froxel_composite_layout(device);
        let composite_pipeline = pipeline::froxel_composite_pipeline(device, &composite_layout);
        Self {
            dims,
            inject_view,
            result_view,
            compute_layout,
            inject_pipeline,
            integrate_pipeline,
            uniform,
            sampler,
            composite_layout,
            composite_pipeline,
        }
    }
}

/// One beam as the GPU sees it — a disc spotlight plus its full optical state
/// (mirrors `Fixture` in `volumetric.wgsl` / `Light` in `mesh.wgsl`). A fixture
/// with an active prism contributes several of these (one per facet copy).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FixtureGpu {
    pos_range: [f32; 4],   // xyz = lens pos, w = range (m)
    dir_cos: [f32; 4],     // xyz = beam dir (unit), w = tan(half zoom angle)
    color: [f32; 4],       // rgb = tint*intensity*candela*shutter, w = lens radius (m)
    cookie_r: [f32; 4],    // xyz = lens-plane right basis, w = wheel-buffer offset
    cookie_u: [f32; 4],    // xyz = lens-plane up basis,    w = wheel count (dynamic chain)
    // x = anim layer (<0 none), y = anim scroll. z/w are shutter (close, kind) on
    // single-emitter beams; on a PLAIN multi-emitter cell they are repurposed:
    // z = -1 plain-beam sentinel (skip the cookie chain), w = per-cell HDR whiteness.
    extra: [f32; 4],
    shape: [f32; 4],       // x = super-Gaussian order, y = focus dist (m), z = iris frac, w = frost 0..1
    misc: [f32; 4],        // x = CA strength, y = laser flag, z = atlas layer count, w = shadow layer
    cmyf: [f32; 4],        // CMY flag insertions: c, m, y, unused (spatial sliding dichroic flags)
}

/// One physical wheel in a fixture's chain (a DYNAMIC count per fixture, indexed
/// by `FixtureGpu.cookie_r.w` offset + `cookie_u.w` count). Mirrors `WheelGpu`
/// in `optics.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WheelGpu {
    d: [f32; 4], // base atlas layer (<0 none), position (slot units), n_slots, gap
    m: [f32; 4], // x = kind (0 = gobo image, 1 = colour strip), y = gobo image rotation (rad), z/w unused
}

impl FixtureGpu {
    /// A disabled (zero-radiance) beam — used to keep the storage buffer's bound
    /// length ≥ 1 when the scene has no fixtures.
    fn disabled() -> Self {
        let mut f = Self::zeroed();
        f.extra[0] = -1.0; // no anim
        f.misc[3] = -1.0; // no shadow
        f.cookie_u[3] = 0.0; // no wheels
        f
    }
}

/// SSAO params (mirrors `Ao` in `ssao.wgsl`): near, far, world-radius-in-px-at-1m,
/// intensity.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct AoUniform {
    params: [f32; 4],
}

/// Tonemap controls (mirrors `Post` in `post.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostUniform {
    exposure: f32,
    bloom: f32,
    _pad: [f32; 2],
}

/// Per-screen GPU content (image / NDI / CITP / pixel-map), cached by screen
/// index. Procedural walls (solid / test pattern) have no entry and bind the
/// shared placeholder. `content_key` detects when to re-upload.
struct ScreenRuntime {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    content_key: u64,
    size: (u32, u32),
    /// A small `SUMMARY_W × SUMMARY_H` per-region average of the content (linear
    /// RGB, texture order: row 0 = top), driving the per-region area-light samples.
    summary: Vec<[f32; 3]>,
}

const SUMMARY_W: usize = 8;
const SUMMARY_H: usize = 4;

/// Tiled light culling (Gem 3): screen-tile size in full-res pixels. The forward
/// fragment shader and the volumetric raymarch both index this grid to loop only
/// the beams overlapping their pixel's tile. 32 px is the sweet spot — tight enough
/// to cull spots hard, coarse enough that the CPU scatter stays sub-millisecond.
const LIGHT_TILE_PX: u32 = 32;
/// Below this lit-fixture count, culling is skipped (every tile gets all lights, i.e.
/// today's brute-force behaviour) — the build/extra-binding overhead isn't worth it.
const TILE_MIN_LIGHTS: usize = 16;

/// Box-downsample an RGBA8 (sRGB) frame to a `SUMMARY_W × SUMMARY_H` grid of
/// linear-RGB averages (row 0 = top) for the per-region area-light samples.
fn summarize_rgba(rgba: &[u8], w: u32, h: u32) -> Vec<[f32; 3]> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![[0.0f32; 3]; SUMMARY_W * SUMMARY_H];
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return out;
    }
    let lin = |c: u8| {
        let f = c as f32 / 255.0;
        f * f // cheap sRGB → ~linear
    };
    for gy in 0..SUMMARY_H {
        let y0 = gy * h / SUMMARY_H;
        let y1 = (((gy + 1) * h / SUMMARY_H).max(y0 + 1)).min(h);
        for gx in 0..SUMMARY_W {
            let x0 = gx * w / SUMMARY_W;
            let x1 = (((gx + 1) * w / SUMMARY_W).max(x0 + 1)).min(w);
            let (mut r, mut g, mut b, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let sy = ((y1 - y0) / 4).max(1);
            let sx = ((x1 - x0) / 4).max(1);
            let mut yy = y0;
            while yy < y1 {
                let mut xx = x0;
                while xx < x1 {
                    let o = (yy * w + xx) * 4;
                    r += lin(rgba[o]);
                    g += lin(rgba[o + 1]);
                    b += lin(rgba[o + 2]);
                    n += 1.0;
                    xx += sx;
                }
                yy += sy;
            }
            let inv = 1.0 / n.max(1.0);
            out[gy * SUMMARY_W + gx] = [r * inv, g * inv, b * inv];
        }
    }
    out
}

/// Linear-RGB content colour at surface UV `(u, v)` for the area-light samples:
/// a live/decoded frame uses its downsampled summary; procedural content is
/// evaluated directly.
fn screen_light_color(s: &LedScreen, rt: Option<&ScreenRuntime>, u: f32, v: f32) -> [f32; 3] {
    if let Some(rt) = rt
        && rt.summary.len() == SUMMARY_W * SUMMARY_H
    {
        let gx = (u.clamp(0.0, 0.999) * SUMMARY_W as f32) as usize;
        // Texture order has row 0 = top, but wall v = 0 is the bottom edge.
        let gy = ((1.0 - v).clamp(0.0, 0.999) * SUMMARY_H as f32) as usize;
        return rt.summary[gy * SUMMARY_W + gx];
    }
    s.sample_content(u, v)
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    /// The scene animation clock (seconds) used for ALL time-driven animation —
    /// volumetric fog drift (`eye_time.w`) and beam/wheel resolve. Set by the
    /// caller each frame (NOT wall-clock), so it advances with the logical scene
    /// time and can be FROZEN: a still render holds it fixed across every
    /// accumulation frame, and a headless capture is deterministic.
    anim_time: f32,

    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    line_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    /// Depth-only pre-pass + `Equal`-test main pass (cuts the per-fragment light-loop
    /// overdraw). Gated on light count: only worth its draw overhead when the fragment
    /// shader is expensive (many lit fixtures). Beauty/Unlit only; Wireframe keeps
    /// the single Less+write path.
    mesh_depth_prepass: wgpu::RenderPipeline,
    mesh_depth_equal: wgpu::RenderPipeline,
    /// Wireframe variant of the mesh pipeline (None if the GPU lacks line polygon mode).
    mesh_wire_pipeline: Option<wgpu::RenderPipeline>,
    lens_pipeline: wgpu::RenderPipeline,
    /// LED-wall surfaces (emissive textured quads; camera + content bind groups).
    wall_pipeline: wgpu::RenderPipeline,
    /// Transparent / mesh LED walls (premultiplied alpha, no depth write).
    wall_alpha_pipeline: wgpu::RenderPipeline,
    /// Selection silhouette: a mask pass marks selected surfaces into a full-res R8
    /// target (depth-tested, occlusion-correct), then an edge-detect composite adds
    /// an amber outline into the HDR before bloom. Two mask pipelines cover the two
    /// instance layouts (MeshInstance: fixtures + scene geometry; WallInstance: LED
    /// screens). Camera-only pipeline layout (the mask shader reads group 0 only).
    sel_mask_mesh_pipeline: wgpu::RenderPipeline,
    sel_mask_wall_pipeline: wgpu::RenderPipeline,
    sel_outline_pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for a wall's content texture (group 1).
    wall_content_layout: wgpu::BindGroupLayout,
    /// 1×1 placeholder content bound for procedural (solid/test-pattern) walls.
    wall_placeholder_bg: wgpu::BindGroup,
    #[allow(dead_code)]
    wall_placeholder_tex: wgpu::Texture,
    /// Sampler for wall content textures (linear, clamp).
    content_sampler: wgpu::Sampler,
    /// Per-screen content texture cache (image / live frames), keyed by screen index.
    screen_runtime: HashMap<usize, ScreenRuntime>,
    light_layout: wgpu::BindGroupLayout,

    grid_mesh: GpuMesh,
    floor_mesh: GpuMesh,
    cylinder_mesh: GpuMesh,
    cone_mesh: GpuMesh,
    disc_mesh: GpuMesh,
    /// Unit quad for LED-wall surfaces.
    quad_mesh: GpuMesh,

    floor_instances: GrowBuffer,
    fixture_instances: GrowBuffer,
    lens_instances: GrowBuffer,
    wall_instances: GrowBuffer,
    dynamic_lines: GrowBuffer,

    // Imported GDTF fixture models: per-fixture-type (Arc ptr) cache of part
    // meshes (keyed by model name), plus a per-frame instance buffer.
    gdtf_cache: HashMap<usize, HashMap<String, GpuMesh>>,
    gdtf_instances: GrowBuffer,

    // Imported MVR static geometry (stage/truss/set): cache of baked meshes
    // keyed by the model blob's Arc pointer, plus a per-frame instance buffer.
    // `None` = a model that failed to bake (unsupported/empty); cached so it is
    // not re-parsed (and re-warned) every frame.
    scene_geom_cache: HashMap<usize, Option<GpuMesh>>,
    /// Local-space vertex AABB per baked scene mesh (keyed like `scene_geom_cache`),
    /// for frustum-culling shadow casters: a narrow hero beam sees almost none of a
    /// 5000-object crowd, so culling cuts the shadow-pass draw count ~100×.
    scene_geom_bounds: HashMap<usize, ([f32; 3], [f32; 3])>,
    scene_geom_instances: GrowBuffer,

    // Placeholder cone bodies for GDTF fixtures whose 3D models didn't bake
    // (absent / unsupported model format) — so the fixture is still visible.
    gdtf_placeholder_instances: GrowBuffer,

    // World/HDRI environment: the equirect map + the sky background pipeline.
    // `world_key` caches the loaded map's Arc pointer (0 = placeholder).
    sky_pipeline: wgpu::RenderPipeline,
    world_bgl: wgpu::BindGroupLayout,
    world_tex: world::WorldTexture,
    world_bind_group: wgpu::BindGroup,
    world_key: usize,
    /// Whether the current world map actually DECODED (not just bytes present) —
    /// gates the sky pass + the IBL flag so a failed/unsupported map degrades to
    /// the dark void instead of a white flood.
    world_loaded: bool,

    // Gobo/animation texture atlas (built from GDTF wheel media on first load).
    gobo_atlas: atlas::GoboAtlas,

    // Per-beam shadow maps for the hero (sharp moving-head) beams.
    shadow: shadow::ShadowMaps,
    /// Froxel volumetric (PREVIZ_FROXEL); `None` if the adapter lacks rgba16float
    /// storage textures.
    froxel: Option<FroxelState>,
    /// Per-pass GPU timing for the perf overlay; `None` if the adapter lacks
    /// `TIMESTAMP_QUERY` (the overlay then shows only CPU frame-ms + scene counts).
    gpu_timers: Option<gpu_timer::GpuTimers>,
    /// Latest per-pass timings + scene counts, read by the perf overlay each frame.
    pub last_timings: PassTimings,

    // Volumetric raymarch (rendered at half resolution, then upsampled).
    volumetric_pipeline: wgpu::RenderPipeline,
    volumetric_layout: wgpu::BindGroupLayout,
    volumetric_uniform: wgpu::Buffer,

    // Temporal accumulation (EMA) of the half-res volumetric — smooths the per-frame
    // jittered raymarch into a stable, super-sampled beam (no banding/dither).
    vol_temporal_layout: wgpu::BindGroupLayout,
    vol_temporal_pipeline: wgpu::RenderPipeline,
    temporal_uniform: wgpu::Buffer,
    vol_linear_sampler: wgpu::Sampler,
    /// Previous frame's camera view-proj (for reprojection) + the accumulation state.
    prev_view_proj: glam::Mat4,
    frame_index: u32,
    /// How many consecutive static frames have accumulated (resets on motion); ramps
    /// the history weight n/(n+1) up to a cap.
    accum_frames: u32,
    /// Hash of last frame's full beam + wheel state — ANY change (pan/tilt, colour,
    /// dimmer, gobo/prism rotation, scroll) → drop history so moving beams don't ghost.
    prev_beam_sig: u64,
    /// Whether last frame actually wrote a valid EMA we can reproject from (false on
    /// the first frame, after a resize, or a froxel-only frame with no hero raymarch).
    ema_valid: bool,
    /// Viewport size last frame; a change recreates the EMA targets (history reset).
    prev_size: (u32, u32),
    fixtures_storage: GrowBuffer,
    /// Tiled light culling (Gem 3): per-screen-tile CSR light lists, shared by the
    /// forward mesh pass and the volumetric raymarch. `tile_offsets[t]..[t+1]` slices
    /// `tile_lights` into the beam indices overlapping tile `t`. CPU-built each frame.
    tile_offsets: GrowBuffer,
    tile_lights: GrowBuffer,
    /// Flattened per-fixture wheel chains (shared by the volumetric + mesh passes).
    wheels_storage: GrowBuffer,
    composite_pipeline: wgpu::RenderPipeline,
    /// Layout for the depth-aware volumetric composite (vol + sampler + scene depth).
    composite_upsample_layout: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    noise_texture: wgpu::Texture,
    noise_view: wgpu::TextureView,
    noise_sampler: wgpu::Sampler,

    // Screen-space AO (Unlit mode only): multiply-blended onto the HDR.
    ssao_pipeline: wgpu::RenderPipeline,
    ssao_layout: wgpu::BindGroupLayout,
    ao_uniform: wgpu::Buffer,

    // Post (bloom + tonemap).
    bloom_bright: wgpu::RenderPipeline,
    bloom_blur_h: wgpu::RenderPipeline,
    bloom_blur_v: wgpu::RenderPipeline,
    tonemap_pipeline: wgpu::RenderPipeline,
    single_tex_layout: wgpu::BindGroupLayout,
    tonemap_layout: wgpu::BindGroupLayout,
    post_uniform: wgpu::Buffer,
    post_sampler: wgpu::Sampler,

    egui_renderer: egui_wgpu::Renderer,
    pub viewport: Viewport,
    /// A SEPARATE offscreen target for a deliberate still render (Blender keeps the
    /// real-time viewport and the F12 render result wholly separate). Created lazily
    /// at the chosen output resolution — independent of the live `viewport`, so a
    /// render never disturbs the interactive preview's size or content. `None` when
    /// no render has run this session.
    render_view: Option<Viewport>,
    /// The GPU backend this renderer is running on (Metal / Vulkan / DX12 / GL).
    active_backend: wgpu::Backend,
    /// Every backend that has a usable adapter on this machine — the options the
    /// Engine ▸ Backend dropdown offers (a single entry on macOS = Metal).
    available_backends: Vec<wgpu::Backend>,
}

/// Human label for a GPU backend — the dropdown text + the on-disk pref token.
pub fn backend_label(b: wgpu::Backend) -> &'static str {
    match b {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "DX12",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        wgpu::Backend::Noop => "None",
    }
}

fn backend_from_label(s: &str) -> Option<wgpu::Backend> {
    Some(match s {
        "Vulkan" => wgpu::Backend::Vulkan,
        "Metal" => wgpu::Backend::Metal,
        "DX12" => wgpu::Backend::Dx12,
        "OpenGL" => wgpu::Backend::Gl,
        "WebGPU" => wgpu::Backend::BrowserWebGpu,
        _ => return None,
    })
}

/// `<config>/gpu-backend.txt` — the machine-global GPU backend preference (read at
/// startup, before any project loads; per-project prefs can't choose the backend).
fn gpu_pref_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("dev", "Embedder", "previz")
        .map(|d| d.config_dir().join("gpu-backend.txt"))
}

/// The user's preferred backend, if one was saved (and is parseable).
pub fn read_gpu_pref() -> Option<wgpu::Backend> {
    let s = std::fs::read_to_string(gpu_pref_path()?).ok()?;
    backend_from_label(s.trim())
}

/// Persist the chosen backend label (takes effect on the next launch).
pub fn write_gpu_pref(label: &str) {
    if let Some(p) = gpu_pref_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, label);
    }
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Self {
        // Backend selection. On Windows the NVIDIA Vulkan driver (581.95) faults the
        // device the moment we render to / present the swapchain surface — the 3D
        // offscreen render is fine, but the on-screen surface is unusable, so the
        // window only ever showed dropped frames. DX12 is the native, stable Windows
        // backend and sidesteps that driver bug. Elsewhere keep the platform default
        // (Metal on macOS, Vulkan on Linux). `WGPU_BACKEND` (e.g. =vulkan) overrides.
        // Enumerate every backend that has a usable adapter on this machine (a
        // throwaway all-backends instance) — the options for the Backend dropdown.
        let enum_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let mut available_backends: Vec<wgpu::Backend> = enum_instance
            .enumerate_adapters(wgpu::Backends::all())
            .await
            .iter()
            .map(|a| a.get_info().backend)
            .filter(|b| *b != wgpu::Backend::Noop)
            .collect();
        available_backends.sort_by_key(|b| *b as u8);
        available_backends.dedup();
        drop(enum_instance);

        // Backend selection: a persisted UI preference (if its backend is available)
        // overrides the platform default; `WGPU_BACKEND` env (.with_env()) still wins.
        // On Windows the default is DX12 — the NVIDIA Vulkan driver (581.95) faults the
        // device the moment we present the surface, so the window only showed dropped
        // frames; DX12 is the native, stable Windows backend.
        let pref = read_gpu_pref().filter(|b| available_backends.contains(b));
        let backends = if let Some(b) = pref {
            wgpu::Backends::from(b)
        } else if cfg!(target_os = "windows") {
            wgpu::Backends::DX12
        } else {
            wgpu::Backends::PRIMARY
        };
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor {
                backends,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            }
            .with_env(),
        );

        // `window` is an Arc<Window>, which is 'static, so the surface is too.
        let surface = instance
            .create_surface(window.clone())
            .expect("create wgpu surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .expect("no suitable GPU adapter found");

        let active_backend = adapter.get_info().backend;
        log::info!("using adapter: {:?}", adapter.get_info());

        // Wireframe viewport mode needs line polygon mode; request it when the
        // adapter offers it (it's not a core WebGPU feature), else fall back to a
        // solid-but-flat wireframe view.
        let wireframe_supported = adapter
            .features()
            .contains(wgpu::Features::POLYGON_MODE_LINE);
        // The froxel volumetric writes HDR scatter into a 3D rgba16float STORAGE
        // texture from compute; that needs the adapter-specific-format feature
        // (rgba16float isn't storage-capable in core WebGPU). Confirmed present on
        // Apple Silicon — when absent we just keep the fragment raymarch.
        let froxel_supported = adapter
            .features()
            .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
        // Per-pass GPU timing for the performance overlay. TIMESTAMP_QUERY enables
        // RenderPass/ComputePass `timestamp_writes` + `resolve_query_set` — all we need
        // (NOT the INSIDE_ENCODERS/PASSES variants). Opt-in feature; present on Apple
        // Silicon but absent on some older drivers, so probe + degrade to a CPU-only HUD.
        let timestamps_supported = adapter
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY);
        let mut required_features = wgpu::Features::empty();
        if wireframe_supported {
            required_features |= wgpu::Features::POLYGON_MODE_LINE;
        }
        if froxel_supported {
            required_features |= wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        }
        if timestamps_supported {
            required_features |= wgpu::Features::TIMESTAMP_QUERY;
        }

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("previz-device"),
                required_features,
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .expect("request device");

        // Diagnostic: log the reason if the device is ever lost (e.g. a GPU hang /
        // VK_ERROR_DEVICE_LOST). After a loss every resource creation silently
        // returns an invalid handle — which is what surfaces downstream as
        // "Texture 'viewport-hdr' is invalid". This tells us the true cause.
        device.set_device_lost_callback(|reason, msg| {
            log::error!("WGPU DEVICE LOST ({reason:?}): {msg}");
        });

        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        // --- camera uniform (bind group 0 of the forward pipelines) ---
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-bind-group-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind-group"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let light_layout = pipeline::light_bind_group_layout(&device);

        let line_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("line-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_bgl)],
            immediate_size: 0,
        });
        let world_bgl = pipeline::world_bind_group_layout(&device);
        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&light_layout), Some(&world_bgl)],
            immediate_size: 0,
        });
        let sky_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&world_bgl)],
            immediate_size: 0,
        });

        let line_pipeline = pipeline::line_pipeline(&device, &line_layout);
        let mesh_pipeline = pipeline::mesh_pipeline(&device, &mesh_layout);
        let mesh_depth_prepass = pipeline::mesh_depth_prepass_pipeline(&device, &mesh_layout);
        let mesh_depth_equal = pipeline::mesh_depth_equal_pipeline(&device, &mesh_layout);
        let mesh_wire_pipeline =
            wireframe_supported.then(|| pipeline::mesh_wire_pipeline(&device, &mesh_layout));
        let lens_pipeline = pipeline::lens_pipeline(&device, &line_layout);
        // LED walls: camera (group 0) + a per-wall content texture (group 1).
        let wall_content_layout = pipeline::single_tex_bind_group_layout(&device);
        let wall_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wall-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&wall_content_layout)],
            immediate_size: 0,
        });
        let wall_pipeline = pipeline::wall_pipeline(&device, &wall_layout);
        let wall_alpha_pipeline = pipeline::wall_alpha_pipeline(&device, &wall_layout);
        // Selection-mask pipelines: camera-only layout (the mask shader reads only
        // group 0); the same instance buffers as the forward pass feed them.
        let sel_mask_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sel-mask-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_bgl)],
            immediate_size: 0,
        });
        let sel_mask_mesh_pipeline = pipeline::mesh_mask_pipeline(&device, &sel_mask_layout);
        let sel_mask_wall_pipeline = pipeline::wall_mask_pipeline(&device, &sel_mask_layout);
        let content_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("content-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // 1×1 white placeholder content (procedural walls ignore it in-shader).
        let wall_placeholder_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wall-placeholder"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &wall_placeholder_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255u8, 255, 255, 255],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let wall_placeholder_view = wall_placeholder_tex.create_view(&Default::default());
        let wall_placeholder_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wall-placeholder-bg"),
            layout: &wall_content_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&wall_placeholder_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&content_sampler) },
            ],
        });
        let sky_pipeline = pipeline::sky_pipeline(&device, &sky_layout);

        let world_tex = world::WorldTexture::placeholder(&device, &queue);
        let world_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world-bg"),
            layout: &world_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&world_tex.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&world_tex.sampler) },
            ],
        });

        // --- meshes ---
        let grid_mesh = GpuMesh::new(&device, "grid", &mesh::grid_and_axes(20.0, 1.0));
        let floor_mesh = GpuMesh::new(&device, "floor", &mesh::floor_plane(50.0));
        let cylinder_mesh = GpuMesh::new(
            &device,
            "par-cylinder",
            &mesh::cylinder(Fixture::BODY_LENGTH, Fixture::BODY_RADIUS, 28),
        );
        let cone_mesh = GpuMesh::new(
            &device,
            "fixture-cone",
            &mesh::cone(0.45, 0.45 * 12.0_f32.to_radians().tan(), 28),
        );
        let disc_mesh = GpuMesh::new(&device, "lens-disc", &mesh::disc(48));
        let quad_mesh = GpuMesh::new(&device, "led-wall-quad", &mesh::unit_quad(64, 24));

        let vertex = wgpu::BufferUsages::VERTEX;
        let inst = std::mem::size_of::<MeshInstance>() as u64;
        let floor_instances = GrowBuffer::new(&device, "floor-instances", vertex, inst);
        let fixture_instances = GrowBuffer::new(&device, "fixture-instances", vertex, inst * 64);
        let lens_instances = GrowBuffer::new(&device, "lens-instances", vertex, inst * 64);
        let wall_inst = std::mem::size_of::<WallInstance>() as u64;
        let wall_instances = GrowBuffer::new(&device, "wall-instances", vertex, wall_inst * 8);
        let dynamic_lines = GrowBuffer::new(&device, "dynamic-lines", vertex, 8192);
        let gdtf_instances = GrowBuffer::new(&device, "gdtf-instances", vertex, inst * 32);
        let scene_geom_instances =
            GrowBuffer::new(&device, "scene-geom-instances", vertex, inst * 64);
        let gdtf_placeholder_instances =
            GrowBuffer::new(&device, "gdtf-placeholder-instances", vertex, inst * 16);

        // --- volumetric raymarch ---
        let volumetric_layout = pipeline::volumetric_bind_group_layout(&device);
        let volumetric_pipeline = pipeline::volumetric_pipeline(&device, &volumetric_layout);
        let volumetric_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("volumetric-uniform"),
            size: std::mem::size_of::<VolumetricUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let vol_temporal_layout = pipeline::vol_temporal_layout(&device);
        let vol_temporal_pipeline = pipeline::vol_temporal_pipeline(&device, &vol_temporal_layout);
        let temporal_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal-uniform"),
            size: std::mem::size_of::<TemporalUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let vol_linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vol-linear-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let fixtures_storage = GrowBuffer::new(
            &device,
            "fixtures-gpu",
            wgpu::BufferUsages::STORAGE,
            std::mem::size_of::<FixtureGpu>() as u64 * 16,
        );
        let tile_offsets = GrowBuffer::new(&device, "tile-offsets", wgpu::BufferUsages::STORAGE, 4096);
        let tile_lights = GrowBuffer::new(&device, "tile-lights", wgpu::BufferUsages::STORAGE, 4096);
        let wheels_storage = GrowBuffer::new(
            &device,
            "wheels-gpu",
            wgpu::BufferUsages::STORAGE,
            std::mem::size_of::<WheelGpu>() as u64 * 32,
        );

        // Precomputed tiling 3D haze noise (sampled by the volumetric shader
        // instead of recomputing FBM per raymarch sample).
        let noise_size = 64u32;
        let noise_data = noise::generate_fbm_volume(noise_size as usize);
        let noise_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("haze-noise-3d"),
            size: wgpu::Extent3d {
                width: noise_size,
                height: noise_size,
                depth_or_array_layers: noise_size,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &noise_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &noise_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(noise_size),
                rows_per_image: Some(noise_size),
            },
            wgpu::Extent3d {
                width: noise_size,
                height: noise_size,
                depth_or_array_layers: noise_size,
            },
        );
        let noise_view = noise_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });
        let noise_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("noise-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // --- screen-space AO (Unlit mode) ---
        let ssao_layout = pipeline::ssao_bind_group_layout(&device);
        let ssao_pipeline = pipeline::ssao_pipeline(&device, &ssao_layout);
        let ao_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ao-uniform"),
            size: std::mem::size_of::<AoUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- post (bloom + tonemap) ---
        let single_tex_layout = pipeline::single_tex_bind_group_layout(&device);
        // Selection silhouette edge-detect composite (samples the R8 mask).
        let sel_outline_pipeline = pipeline::sel_outline_pipeline(&device, &single_tex_layout);
        let composite_upsample_layout = pipeline::composite_upsample_layout(&device);
        let composite_pipeline = pipeline::composite_pipeline(&device, &composite_upsample_layout);
        let tonemap_layout = pipeline::tonemap_bind_group_layout(&device);
        let (bloom_bright, bloom_blur_h, bloom_blur_v) =
            pipeline::bloom_pipelines(&device, &single_tex_layout);
        let tonemap_pipeline = pipeline::tonemap_pipeline(&device, &tonemap_layout);
        let post_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post-uniform"),
            size: std::mem::size_of::<PostUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // --- egui paint pass + offscreen viewport target ---
        let mut egui_renderer = egui_wgpu::Renderer::new(
            &device,
            surface_format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: true,
                predictable_texture_filtering: false,
            },
        );
        let viewport = Viewport::new(&device, &mut egui_renderer, (width, height));

        let gobo_atlas = atlas::GoboAtlas::new(&device, &queue);
        let shadow = shadow::ShadowMaps::new(&device);
        let froxel = froxel_supported.then(|| FroxelState::new(&device));
        let gpu_timers = timestamps_supported.then(|| gpu_timer::GpuTimers::new(&device));
        let mut last_timings = PassTimings::default();
        last_timings.gpu_valid = timestamps_supported;

        Self {
            surface,
            device,
            queue,
            config,
            anim_time: 0.0,
            camera_buffer,
            camera_bind_group,
            line_pipeline,
            mesh_pipeline,
            mesh_depth_prepass,
            mesh_depth_equal,
            mesh_wire_pipeline,
            lens_pipeline,
            wall_pipeline,
            wall_alpha_pipeline,
            sel_mask_mesh_pipeline,
            sel_mask_wall_pipeline,
            sel_outline_pipeline,
            wall_content_layout,
            wall_placeholder_bg,
            wall_placeholder_tex,
            content_sampler,
            screen_runtime: HashMap::new(),
            light_layout,
            grid_mesh,
            floor_mesh,
            cylinder_mesh,
            cone_mesh,
            disc_mesh,
            quad_mesh,
            floor_instances,
            fixture_instances,
            lens_instances,
            wall_instances,
            dynamic_lines,
            gdtf_cache: HashMap::new(),
            gdtf_instances,
            scene_geom_cache: HashMap::new(),
            scene_geom_bounds: HashMap::new(),
            scene_geom_instances,
            gdtf_placeholder_instances,
            sky_pipeline,
            world_bgl,
            world_tex,
            world_bind_group,
            world_key: 0,
            world_loaded: false,
            gobo_atlas,
            shadow,
            froxel,
            gpu_timers,
            last_timings,
            volumetric_pipeline,
            volumetric_layout,
            volumetric_uniform,
            vol_temporal_layout,
            vol_temporal_pipeline,
            temporal_uniform,
            vol_linear_sampler,
            prev_view_proj: glam::Mat4::IDENTITY,
            frame_index: 0,
            accum_frames: 0,
            prev_beam_sig: 0,
            ema_valid: false,
            prev_size: (0, 0),
            fixtures_storage,
            tile_offsets,
            tile_lights,
            wheels_storage,
            composite_pipeline,
            composite_upsample_layout,
            ssao_pipeline,
            ssao_layout,
            ao_uniform,
            noise_texture,
            noise_view,
            noise_sampler,
            bloom_bright,
            bloom_blur_h,
            bloom_blur_v,
            tonemap_pipeline,
            single_tex_layout,
            tonemap_layout,
            post_uniform,
            post_sampler,
            egui_renderer,
            viewport,
            render_view: None,
            active_backend,
            available_backends,
        }
    }

    /// The label of the GPU backend currently in use (e.g. "Metal").
    pub fn active_backend_label(&self) -> &'static str {
        backend_label(self.active_backend)
    }

    /// Labels of every available GPU backend (the Backend dropdown options).
    pub fn available_backend_labels(&self) -> Vec<String> {
        self.available_backends
            .iter()
            .map(|b| backend_label(*b).to_string())
            .collect()
    }

    /// Acquire the next swapchain texture for this frame, or `None` to skip it.
    ///
    /// During a window resize the swapchain is transiently out of date — the Vulkan
    /// backend reports this as `Outdated`, or (on Windows) as `Validation`. We MUST
    /// NOT try to recover by reconfiguring the surface and re-acquiring within the
    /// same frame: on NVIDIA Vulkan the first (failed) acquire can leave an acquire
    /// semaphore pending, and `configure()` then destroys the swapchain while that
    /// semaphore is in use → "SwapchainAcquireSemaphore still in use" → the driver
    /// faults the whole device (`VK_ERROR_DEVICE_LOST`). Instead we simply SKIP the
    /// frame; the next `WindowEvent::Resized` reconfigures the surface to the final
    /// size (see `resize_surface`) and acquisition self-heals. Skipped frames are
    /// invisible — the live loop re-arms the next redraw immediately.
    fn acquire_frame(&mut self) -> Option<wgpu::SurfaceTexture> {
        use wgpu::CurrentSurfaceTexture as St;
        match self.surface.get_current_texture() {
            St::Success(f) | St::Suboptimal(f) => Some(f),
            // Reconfigure for a stale swapchain, but DO NOT re-acquire this frame —
            // re-acquiring after a configure is what faulted the device. Skip; the
            // next frame (or the next `Resized`) acquires cleanly.
            St::Outdated | St::Lost => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            other => {
                log::debug!("surface not presentable ({other:?}); skipping frame");
                None
            }
        }
    }

    /// Reconfigure the swapchain after a window resize.
    pub fn resize_surface(&mut self, size: (u32, u32)) {
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        let max = self.device.limits().max_texture_dimension_2d;
        self.config.width = size.0.min(max);
        self.config.height = size.1.min(max);
        self.surface.configure(&self.device, &self.config);
    }

    /// Set the scene animation clock (seconds) used for the next render — fog
    /// drift + beam/wheel resolve. The caller advances it with the logical scene
    /// time for the live view, and FREEZES it (a fixed value) for a still render so
    /// the fog/beams don't drift across the accumulation frames.
    pub fn set_anim_time(&mut self, t: f32) {
        self.anim_time = t;
    }

    /// Resize the offscreen 3D target to match the viewport panel.
    pub fn resize_viewport(&mut self, size: (u32, u32)) {
        // The viewport panel always lives INSIDE the window, so its render target can
        // never legitimately exceed the surface size. egui can transiently report a
        // huge (or infinite → saturates to `u32::MAX`) `available_size` during a
        // layout pass around a resize/maximize; clamping to the surface here is a
        // no-op in normal use (render_scale ≤ 1) but prevents a multi-gigabyte
        // allocation that OOMs the GPU and yields an invalid texture (→ abort).
        let size = (
            size.0.min(self.config.width).max(1),
            size.1.min(self.config.height).max(1),
        );
        self.viewport
            .resize(&self.device, &mut self.egui_renderer, size);
    }

    fn mesh_for(&self, geometry: FixtureGeometry) -> &GpuMesh {
        match geometry {
            FixtureGeometry::Cylinder => &self.cylinder_mesh,
            FixtureGeometry::Cone => &self.cone_mesh,
        }
    }

    /// Load + cache a GDTF fixture's part meshes (GLBs) the first time it is
    /// seen. Keyed by the `Arc` pointer so all instances of a type share.
    fn ensure_gdtf_loaded(&mut self, key: usize, gdtf: &Arc<crate::gdtf::GdtfFixture>) {
        // The atlas allocates its own (key, wheel) blocks and is idempotent.
        self.gobo_atlas.ensure(&self.queue, key, gdtf);
        if self.gdtf_cache.contains_key(&key) {
            return;
        }
        let mut meshes = HashMap::new();
        for model in &gdtf.models {
            if let Some(glb) = &model.glb {
                let verts = fixture_model::load_glb(glb);
                if !verts.is_empty() {
                    meshes.insert(model.name.clone(), GpuMesh::new(&self.device, &model.name, &verts));
                }
            }
        }
        log::info!("loaded GDTF '{}' — {} mesh parts", gdtf.name, meshes.len());
        self.gdtf_cache.insert(key, meshes);
    }

    /// (Re)load the world HDRI texture when the scene's map changes (keyed by the
    /// bytes' `Arc` pointer), rebuilding the world bind group. Cheap no-op when
    /// the map is unchanged.
    fn ensure_world(&mut self, world: &crate::scene::World) {
        let key = world.hdri.as_ref().map(|a| Arc::as_ptr(a) as usize).unwrap_or(0);
        if key == self.world_key {
            return;
        }
        let tex = match &world.hdri {
            Some(bytes) => match world::WorldTexture::from_bytes(&self.device, &self.queue, bytes) {
                Some(t) => {
                    self.world_loaded = true;
                    t
                }
                None => {
                    log::warn!("world: could not decode environment map '{}'", world.hdri_name);
                    self.world_loaded = false;
                    world::WorldTexture::placeholder(&self.device, &self.queue)
                }
            },
            None => {
                self.world_loaded = false;
                world::WorldTexture::placeholder(&self.device, &self.queue)
            }
        };
        self.world_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world-bg"),
            layout: &self.world_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&tex.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&tex.sampler) },
            ],
        });
        self.world_tex = tex;
        self.world_key = key;
    }

    /// Bake an imported MVR static-geometry model (GLB or 3DS blob) into a cached
    /// mesh, keyed by the blob's `Arc` pointer so identical instances share and
    /// re-imports allocate fresh entries. The result (including a *failure*) is
    /// cached so a model is parsed — and warned about — at most once, not every
    /// frame. Returns the cache key, or `None` if the model had no drawable
    /// geometry.
    fn ensure_scene_geom_loaded(&mut self, model: &crate::mvr::GeometryModel) -> Option<usize> {
        let key = Arc::as_ptr(&model.glb) as usize;
        if !self.scene_geom_cache.contains_key(&key) {
            let verts = fixture_model::load_model(&model.file, &model.glb);
            let entry = if verts.is_empty() {
                log::warn!("mvr: model '{}' produced no geometry (unsupported/empty)", model.file);
                None
            } else {
                // Local-space AABB of the raw vertices (the up-flip + transforms are
                // applied at draw time), cached for shadow-caster frustum culling.
                let mut lo = Vec3::splat(f32::INFINITY);
                let mut hi = Vec3::splat(f32::NEG_INFINITY);
                for v in &verts {
                    let p = Vec3::from(v.position);
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
                self.scene_geom_bounds.insert(key, (lo.to_array(), hi.to_array()));
                Some(GpuMesh::new(&self.device, &model.file, &verts))
            };
            self.scene_geom_cache.insert(key, entry);
        }
        self.scene_geom_cache.get(&key).and_then(|m| m.as_ref()).map(|_| key)
    }

    /// Render one frame. Returns `true` if a frame was presented (a `false`
    /// return means the surface wasn't presentable; the caller should stop
    /// pumping redraws until the next event so we don't busy-loop).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        scene: &Scene,
        camera: &OrbitCamera,
        selection: &Selection,
        settings: &RenderSettings,
        paint_jobs: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) -> bool {
        // egui textures up front, before any early-out: egui hands each delta
        // once, so dropping it loses the font atlas forever.
        for (id, delta) in &textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        for id in &textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        let frame = match self.acquire_frame() {
            Some(f) => f,
            None => return false,
        };
        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        let user_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            paint_jobs,
            screen_descriptor,
        );

        // The 3D scene + volumetrics + post resolve into the LDR target.
        self.record_scene(&mut encoder, scene, camera, selection, settings);

        // egui (panels + the viewport image, which samples the LDR target) -> surface.
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.05,
                                g: 0.05,
                                b: 0.06,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                })
                .forget_lifetime();

            self.egui_renderer
                .render(&mut pass, paint_jobs, screen_descriptor);
        }

        self.queue
            .submit(user_buffers.into_iter().chain(std::iter::once(encoder.finish())));
        frame.present();

        // Pump the async per-pass GPU-timing readback (reads data 2 frames stale; never
        // blocks — the submit above services the pending map callbacks).
        let period = self.queue.get_timestamp_period();
        let bars = self.gpu_timers.as_mut().map(|t| {
            t.pump(period);
            t.bars
        });
        if let Some(b) = bars {
            self.last_timings.passes = b;
        }

        true
    }

    /// Render the 3D scene into the offscreen LDR target (no window/surface,
    /// no egui) and read it back as RGBA8 pixels. Used by the headless
    /// `--screenshot` path so the render can be verified without a visible
    /// window. Returns (width, height, rgba8 pixels).
    /// Render-only bench: record + submit the full offscreen 3D render and block
    /// until the GPU finishes — WITHOUT the capture readback (no buffer alloc, no
    /// GPU→CPU copy, no map). This is the honest per-frame render cost (the live
    /// app presents to screen and never reads back), so profiling with it isn't
    /// dominated by the ~16 MB readback `capture` pays.
    pub fn bench_render(&mut self, scene: &Scene, camera: &OrbitCamera, settings: &RenderSettings) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("bench-encoder") });
        self.record_scene(&mut encoder, scene, camera, &Selection::default(), settings);
        self.queue.submit(std::iter::once(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }

    pub fn capture(
        &mut self,
        scene: &Scene,
        camera: &OrbitCamera,
        settings: &RenderSettings,
    ) -> (u32, u32, Vec<u8>) {
        let (width, height) = self.viewport.size;

        // Warm up the temporal accumulation: a single headless frame would show the raw
        // jittered (dithered) raymarch, so render several static frames first to let the
        // EMA converge to the same smooth result the interactive viewport reaches.
        let warmup: u32 = std::env::var("PREVIZ_WARMUP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(28);
        for _ in 0..warmup {
            let mut warm = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("warm") });
            self.record_scene(&mut warm, scene, camera, &Selection::default(), settings);
            self.queue.submit(std::iter::once(warm.finish()));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("capture-encoder"),
            });
        self.record_scene(&mut encoder, scene, camera, &Selection::default(), settings);

        // copy_texture_to_buffer requires bytes_per_row aligned to 256.
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture-readback"),
            size: padded as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: self.viewport.ldr_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().expect("map channel").expect("map readback buffer");

        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity(unpadded as usize * height as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();

        (width, height, pixels)
    }

    // --- Offscreen still render (the dedicated F12 render target) --------------
    //
    // The deliberate render runs into `render_view`, a SEPARATE target from the live
    // `viewport`, so it never changes the interactive preview's size or content
    // (Blender keeps the 3D view and the render result wholly separate). The render
    // reuses the real pipeline: each frame the job swaps `render_view` in as the
    // scene target, records ONE frame (so the temporal volumetric EMA converges over
    // successive frames), and swaps it back — the live viewport is simply not
    // re-recorded while a render is in flight, so the EMA state is never thrashed
    // between two targets.

    /// Create or resize the offscreen render target to `size`, clamped only to the
    /// GPU's max dimension (NOT the window) so a render can exceed the on-screen
    /// viewport — e.g. a 4K still from a 1080p window.
    pub fn ensure_render_view(&mut self, size: (u32, u32)) {
        let max = self.device.limits().max_texture_dimension_2d;
        let size = (size.0.clamp(1, max), size.1.clamp(1, max));
        match self.render_view.as_mut() {
            Some(v) => v.resize(&self.device, &mut self.egui_renderer, size),
            None => {
                self.render_view = Some(Viewport::new(&self.device, &mut self.egui_renderer, size));
            }
        }
    }

    /// The egui texture id of the offscreen render target (for the Render tab),
    /// or `None` if no render target exists yet.
    pub fn render_view_texture(&self) -> Option<egui::TextureId> {
        self.render_view.as_ref().map(|v| v.texture_id)
    }

    /// Record ONE frame of the scene into the offscreen render target (NOT the live
    /// viewport). Swaps `render_view` into the scene-target slot, records + submits,
    /// blocks until the GPU finishes, then swaps back. Returns `false` if no render
    /// target exists. Each call advances the temporal accumulation by one frame.
    pub fn record_render_view(
        &mut self,
        scene: &Scene,
        camera: &OrbitCamera,
        settings: &RenderSettings,
    ) -> bool {
        // Take the render target OUT so `record_scene` (which needs `&mut self`) can
        // run with it installed as `self.viewport`, then restore the live viewport.
        let Some(mut rv) = self.render_view.take() else {
            return false;
        };
        std::mem::swap(&mut self.viewport, &mut rv); // self.viewport := render target; rv := live
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("render-view-encoder") });
        self.record_scene(&mut encoder, scene, camera, &Selection::default(), settings);
        self.queue.submit(std::iter::once(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        std::mem::swap(&mut self.viewport, &mut rv); // restore: self.viewport := live; rv := render target
        self.render_view = Some(rv);
        true
    }

    /// Read back the offscreen render target's LDR plate to CPU RGBA8 pixels.
    pub fn read_render_view_ldr(&self) -> Option<(u32, u32, Vec<u8>)> {
        self.render_view.as_ref().map(|v| self.read_ldr(v))
    }

    /// Drop the offscreen render target, freeing its GPU memory and unregistering
    /// its egui texture (after a render finishes/cancels). A subsequent render
    /// lazily recreates it. Safe to call right after a readback (the GPU is idle).
    pub fn free_render_view(&mut self) {
        if let Some(v) = self.render_view.take() {
            self.egui_renderer.free_texture(&v.texture_id);
        }
    }

    /// Present an egui frame to the surface WITHOUT re-recording the live 3D scene —
    /// used while a render is in flight, so the live viewport keeps its last frame
    /// (frozen, untouched) while the Render tab shows the converging render target.
    pub fn present_egui_only(
        &mut self,
        paint_jobs: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) -> bool {
        for (id, delta) in &textures_delta.set {
            self.egui_renderer.update_texture(&self.device, &self.queue, *id, delta);
        }
        for id in &textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        let frame = match self.acquire_frame() {
            Some(f) => f,
            None => return false,
        };
        let surface_view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("egui-only-encoder") });
        let user_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            paint_jobs,
            screen_descriptor,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui-only-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.05, g: 0.05, b: 0.06, a: 1.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, paint_jobs, screen_descriptor);
        }
        self.queue
            .submit(user_buffers.into_iter().chain(std::iter::once(encoder.finish())));
        frame.present();
        true
    }

    /// Read a viewport's LDR target (the tonemapped `Rgba8Unorm` plate) to CPU
    /// RGBA8 pixels — shared by the live + offscreen readbacks.
    fn read_ldr(&self, vp: &Viewport) -> (u32, u32, Vec<u8>) {
        let (width, height) = vp.size;
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ldr-readback"),
            size: padded as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ldr-readback-encoder") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: vp.ldr_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().expect("map channel").expect("map readback buffer");

        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity(unpadded as usize * height as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            pixels.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();
        (width, height, pixels)
    }

    /// Apply an egui textures delta (font atlas, user images) to the egui
    /// renderer — used by the headless UI capture to settle the atlas across
    /// frames before the final paint.
    pub fn apply_egui_textures(&mut self, delta: &egui::TexturesDelta) {
        for (id, d) in &delta.set {
            self.egui_renderer.update_texture(&self.device, &self.queue, *id, d);
        }
        for id in &delta.free {
            self.egui_renderer.free_texture(id);
        }
    }

    /// Render the **whole window** — the 3D viewport image + the egui chrome
    /// (panels, menus, dock) — to an offscreen texture and read it back as RGBA8.
    /// Used by the headless `PREVIZ_UI` path so the interface can be screenshotted
    /// without a visible window (and without Screen-Recording permission). The
    /// caller supplies a tessellated egui frame at size `(w, h)`.
    #[allow(clippy::too_many_arguments)]
    pub fn capture_ui(
        &mut self,
        size: (u32, u32),
        scene: &Scene,
        camera: &OrbitCamera,
        selection: &Selection,
        settings: &RenderSettings,
        paint_jobs: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) -> (u32, u32, Vec<u8>) {
        let (width, height) = size;
        for (id, delta) in &textures_delta.set {
            self.egui_renderer.update_texture(&self.device, &self.queue, *id, delta);
        }
        for id in &textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui-capture-target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ui-capture-encoder"),
        });
        let user_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            paint_jobs,
            screen_descriptor,
        );
        // 3D scene into the LDR target (the egui viewport image samples it).
        self.record_scene(&mut encoder, scene, camera, selection, settings);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ui-capture-egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.05, g: 0.05, b: 0.06, a: 1.0 }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, paint_jobs, screen_descriptor);
        }

        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ui-capture-readback"),
            size: padded as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue
            .submit(user_buffers.into_iter().chain(std::iter::once(encoder.finish())));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().expect("map channel").expect("map readback");
        let data = slice.get_mapped_range();
        let bgra = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut pixels = Vec::with_capacity(unpadded as usize * height as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            let line = &data[start..start + unpadded as usize];
            if bgra {
                for px in line.chunks_exact(4) {
                    pixels.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            } else {
                pixels.extend_from_slice(line);
            }
        }
        drop(data);
        readback.unmap();
        (width, height, pixels)
    }

    /// Record the full offscreen 3D frame into `encoder`: forward scene ->
    /// volumetric beams -> bloom -> tonemap into the LDR target. Shared by
    /// [`render`](Self::render) and [`capture`](Self::capture).
    /// Ensure screen `idx`'s content texture is current. Returns true if a real
    /// content texture is bound (image / live frame), false for procedural walls
    /// (solid / test pattern), which bind the placeholder instead.
    fn ensure_screen_content(&mut self, idx: usize, s: &LedScreen) -> bool {
        // A live frame (set by the app for pixel-map / NDI / CITP) wins; else
        // decode an `Image`'s bytes once (cached by the Arc pointer).
        if let Some(f) = &s.frame {
            let key = (Arc::as_ptr(f) as usize as u64)
                ^ f.generation.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            self.upload_screen_rgba(idx, key, f.width, f.height, &f.rgba);
            return self.screen_runtime.contains_key(&idx);
        }
        if let ScreenContent::Image { bytes, .. } = &s.content {
            let key = Arc::as_ptr(bytes) as usize as u64;
            if self.screen_runtime.get(&idx).map(|r| r.content_key) != Some(key) {
                match image::load_from_memory(bytes) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        self.upload_screen_rgba(idx, key, w, h, &rgba);
                    }
                    Err(e) => {
                        log::warn!("LED screen image decode failed: {e}");
                        self.screen_runtime.remove(&idx);
                    }
                }
            }
            return self.screen_runtime.contains_key(&idx);
        }
        // Procedural (solid / test pattern): no content texture.
        self.screen_runtime.remove(&idx);
        false
    }

    /// Create-or-reuse screen `idx`'s content texture and upload `rgba` (tightly
    /// packed RGBA8, `w*h*4` bytes) when the content key changes.
    fn upload_screen_rgba(&mut self, idx: usize, key: u64, w: u32, h: u32, rgba: &[u8]) {
        let w = w.max(1);
        let h = h.max(1);
        let expected = (w as usize) * (h as usize) * 4;
        if rgba.len() < expected {
            return; // malformed frame — keep whatever was there
        }
        let need_new = self.screen_runtime.get(&idx).map(|r| r.size != (w, h)).unwrap_or(true);
        if need_new {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("screen-content"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("screen-content-bg"),
                layout: &self.wall_content_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.content_sampler) },
                ],
            });
            self.screen_runtime.insert(
                idx,
                ScreenRuntime {
                    texture,
                    bind_group,
                    content_key: u64::MAX,
                    size: (w, h),
                    summary: Vec::new(),
                },
            );
        }
        let rt = self.screen_runtime.get_mut(&idx).unwrap();
        // Always upload when the texture was just (re)created, even if `key` happens
        // to collide with the `u64::MAX` force-upload sentinel.
        if need_new || rt.content_key != key {
            rt.summary = summarize_rgba(rgba, w, h);
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &rt.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba[..expected],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            rt.content_key = key;
        }
    }

    /// `timestamp_writes` for a render/compute pass timing query `pair`, or None when
    /// GPU timestamps are unavailable / the ring slot is busy (the pass just records
    /// untimed). Used to populate the perf overlay's per-pass bars.
    fn ts_rp(&self, pair: u32) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.gpu_timers.as_ref().and_then(|t| t.rp(pair))
    }
    fn ts_cp(&self, pair: u32) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        self.gpu_timers.as_ref().and_then(|t| t.cp(pair))
    }

    fn record_scene(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        camera: &OrbitCamera,
        selection: &Selection,
        settings: &RenderSettings,
    ) {
        let time = self.anim_time;
        let aspect = self.viewport.aspect();

        // --- world / HDRI environment (reloads the GPU map only when it changes) ---
        self.ensure_world(&scene.world);

        // --- camera uniform ---
        let mut camera_uniform = camera.uniform(aspect);
        camera_uniform.render_mode[0] = settings.mode.shader_code();
        camera_uniform.render_mode[1] = settings.gobo_sharpness.max(0.0); // floor-pool gobo sharpen
        // Tiled light culling (Gem 3): the mesh fragment reads its screen tile from
        // render_mode.zw to index the per-tile light list (the volumetric ray uses the
        // same full-res grid via VolumetricUniform.tile, so floor pool ↔ beam shaft stay
        // in lock-step). .z/.w were previously unused.
        let (vw, vh) = self.viewport.size;
        let tiles_x = vw.div_ceil(LIGHT_TILE_PX).max(1);
        let tiles_y = vh.div_ceil(LIGHT_TILE_PX).max(1);
        camera_uniform.render_mode[2] = tiles_x as f32;
        camera_uniform.render_mode[3] = LIGHT_TILE_PX as f32;
        {
            let w = &scene.world;
            let has = if self.world_loaded { 1.0 } else { 0.0 };
            camera_uniform.world = [w.brightness, w.rotation, w.ambient, has];
        }

        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera_uniform));

        // --- floor instance ---
        let floor_instance = [MeshInstance {
            model: Mat4::IDENTITY.to_cols_array_2d(),
            color: [0.16, 0.16, 0.19],
            intensity: 1.0,
            selected: 0.0,
        }];
        self.floor_instances
            .upload(&self.device, &self.queue, &floor_instance);

        // --- fixture instances, grouped by geometry ---
        let mut fixture_instances: Vec<MeshInstance> = Vec::with_capacity(scene.fixtures.len());
        let mut ranges: Vec<(FixtureGeometry, u32, u32)> = Vec::new();
        for geometry in [FixtureGeometry::Cylinder, FixtureGeometry::Cone] {
            let start = fixture_instances.len() as u32;
            for (i, fixture) in scene.fixtures.iter().enumerate() {
                if fixture.hidden || fixture.is_gdtf() || fixture.geometry != geometry {
                    continue;
                }
                fixture_instances.push(MeshInstance {
                    model: fixture.model_matrix().to_cols_array_2d(),
                    color: fixture.color,
                    intensity: fixture.intensity,
                    selected: if selection.contains_fixture(i) { 1.0 } else { 0.0 },
                });
            }
            let count = fixture_instances.len() as u32 - start;
            if count > 0 {
                ranges.push((geometry, start, count));
            }
        }
        self.fixture_instances
            .upload(&self.device, &self.queue, &fixture_instances);

        // --- GDTF fixtures: assemble parts (loading GLBs on first use) and the
        // articulated beam (origin/direction) per fixture ---
        let gdtf_to_world = Mat4::from_rotation_x(-FRAC_PI_2); // GDTF +Z up -> world +Y up
        let mut gdtf_parts: Vec<MeshInstance> = Vec::new();
        let mut gdtf_draws: Vec<(usize, String, u32)> = Vec::new();
        // (mesh key, part model, base instance, count) — one instanced forward draw per
        // unique fixture-model part (shared across all fixtures of that GDTF type).
        let mut gdtf_groups: Vec<(usize, String, u32, u32)> = Vec::new();
        // GDTF fixtures whose models didn't bake get a placeholder cone instead.
        let mut gdtf_placeholders: Vec<MeshInstance> = Vec::new();
        let mut beam_frames: Vec<Vec<fixture_model::BeamFrame>> =
            vec![Vec::new(); scene.fixtures.len()];
        for (i, fixture) in scene.fixtures.iter().enumerate() {
            let Some(gdtf) = fixture.gdtf.clone() else {
                continue;
            };
            if fixture.hidden {
                continue;
            }
            let key = Arc::as_ptr(&gdtf) as usize;
            self.ensure_gdtf_loaded(key, &gdtf);
            // Place the fixture: translate, then the MVR hang orientation (identity
            // for app-created fixtures), then GDTF +Z-up → world +Y-up. Pan/tilt
            // are articulated inside `assemble`.
            let root = Mat4::from_translation(fixture.position)
                * Mat4::from_quat(fixture.orientation)
                * gdtf_to_world;
            // BEAM + articulation always come from the fixture's OWN gdtf (its real
            // optics + mode). The BODY model may be BORROWED from another GDTF (the
            // Replace dialog's "mesh/model only"): assemble its parts under ITS own
            // cache key. With `model_src = None` (every normal fixture) this is the
            // same single assemble + `key` as before — byte-identical, no regression.
            let mut own = fixture_model::assemble(
                &gdtf,
                fixture.mode_index,
                root,
                fixture.pan_actual,
                fixture.tilt_actual,
                &fixture.cell_pan,
                &fixture.cell_tilt,
            );
            beam_frames[i] = std::mem::take(&mut own.beams);
            let (asm, key) = match &fixture.model_src {
                Some(m) => {
                    let mk = Arc::as_ptr(m) as usize;
                    self.ensure_gdtf_loaded(mk, m);
                    // The borrowed BODY model has no per-head data of its own — it's
                    // just geometry; its axes (if any) follow the fixture-wide angles.
                    (fixture_model::assemble(m, 0, root, fixture.pan_actual, fixture.tilt_actual, &[], &[]), mk)
                }
                None => (own, key),
            };
            let selected = if selection.contains_fixture(i) { 1.0 } else { 0.0 };
            let drawn_before = gdtf_draws.len();
            for part in &asm.parts {
                if self
                    .gdtf_cache
                    .get(&key)
                    .map(|m| m.contains_key(&part.model))
                    .unwrap_or(false)
                {
                    let idx = gdtf_parts.len() as u32;
                    gdtf_parts.push(MeshInstance {
                        model: part.world.to_cols_array_2d(),
                        color: [0.09, 0.09, 0.10],
                        intensity: 1.0,
                        selected,
                    });
                    gdtf_draws.push((key, part.model.clone(), idx));
                }
            }
            // No model parts baked for this fixture type — show a placeholder cone
            // at the fixture (placed + aimed by its model matrix) so it's visible.
            if gdtf_draws.len() == drawn_before {
                gdtf_placeholders.push(MeshInstance {
                    model: fixture.model_matrix().to_cols_array_2d(),
                    color: [0.16, 0.16, 0.19],
                    intensity: 1.0,
                    selected,
                });
            }
        }
        // Regroup parts by (mesh key, model) so all instances of a part across every
        // fixture of that type are contiguous → one instanced forward+shadow draw per
        // part. `gdtf_draws` keeps remapped per-part entries for the shadow loop.
        {
            let mut order: Vec<usize> = (0..gdtf_draws.len()).collect();
            order.sort_by(|&a, &b| {
                gdtf_draws[a].0.cmp(&gdtf_draws[b].0).then_with(|| gdtf_draws[a].1.cmp(&gdtf_draws[b].1))
            });
            let mut parts = Vec::with_capacity(gdtf_parts.len());
            let mut draws = Vec::with_capacity(gdtf_draws.len());
            for &old in &order {
                let new_idx = parts.len() as u32;
                parts.push(gdtf_parts[old]);
                let (key, model, _) = (gdtf_draws[old].0, gdtf_draws[old].1.clone(), 0u32);
                draws.push((key, model.clone(), new_idx));
                match gdtf_groups.last_mut() {
                    Some((k, m, _, count)) if *k == key && *m == model => *count += 1,
                    _ => gdtf_groups.push((key, model, new_idx, 1)),
                }
            }
            gdtf_parts = parts;
            gdtf_draws = draws;
        }
        self.gdtf_instances
            .upload(&self.device, &self.queue, &gdtf_parts);
        let gdtf_placeholder_count = self
            .gdtf_placeholder_instances
            .upload(&self.device, &self.queue, &gdtf_placeholders);

        // --- imported MVR static geometry (stage decks / truss / set pieces) ---
        // Each model is baked once and drawn as one lit instance; the +Y-up GLB
        // is flipped into the object's geometry frame before its world transform.
        let glb_flip = crate::mvr::glb_yup_to_zup();
        let mut scene_geom_instances: Vec<MeshInstance> = Vec::new();
        // (mesh key, instance index, world-space AABB) — the AABB frustum-culls the
        // draw out of shadow passes (and the camera-frustum forward pass).
        let mut scene_geom_draws: Vec<(usize, u32, Vec3, Vec3)> = Vec::new();
        // (mesh key, base instance, count) — one instanced forward draw per unique mesh.
        let mut scene_geom_groups: Vec<(usize, u32, u32)> = Vec::new();
        let mut total_models = 0usize;
        for (oi, obj) in scene.geometry.iter().enumerate() {
            if obj.hidden {
                total_models += obj.models.len();
                continue;
            }
            let selected = if selection.contains_geometry(oi) { 1.0 } else { 0.0 };
            for model in &obj.models {
                total_models += 1;
                if let Some(key) = self.ensure_scene_geom_loaded(model) {
                    // glTF is +Y-up and needs the flip into the +Z-up geometry
                    // frame; native-Z-up .3ds does not.
                    let flip = if fixture_model::model_needs_yup_flip(&model.file) {
                        glb_flip
                    } else {
                        Mat4::IDENTITY
                    };
                    let idx = scene_geom_instances.len() as u32;
                    let m = obj.transform * model.matrix * flip;
                    scene_geom_instances.push(MeshInstance {
                        // object placement · per-Geometry3D transform · up-flip.
                        model: m.to_cols_array_2d(),
                        color: [0.13, 0.13, 0.15],
                        intensity: 1.0,
                        selected,
                    });
                    // World AABB = local mesh bounds transformed by the full instance
                    // matrix (exact, so it accounts for model.matrix + flip too).
                    let (wlo, whi) = match self.scene_geom_bounds.get(&key) {
                        Some(&(lo, hi)) => transform_aabb(&m, Vec3::from(lo), Vec3::from(hi)),
                        None => (Vec3::splat(f32::NEG_INFINITY), Vec3::splat(f32::INFINITY)),
                    };
                    scene_geom_draws.push((key, idx, wlo, whi));
                }
            }
        }
        // Regroup instances by mesh key so identical static meshes are contiguous in
        // the buffer → ONE instanced forward draw per unique mesh instead of one per
        // instance (the dominant FOH draw-call cost). `scene_geom_draws` keeps a
        // per-instance entry (with REMAPPED idx) for the per-beam shadow LOD cull.
        {
            let mut order: Vec<usize> = (0..scene_geom_draws.len()).collect();
            order.sort_by_key(|&i| scene_geom_draws[i].0); // stable by mesh key
            let mut inst = Vec::with_capacity(scene_geom_instances.len());
            let mut draws = Vec::with_capacity(scene_geom_draws.len());
            for &old in &order {
                let new_idx = inst.len() as u32;
                inst.push(scene_geom_instances[old]);
                let (key, _, lo, hi) = scene_geom_draws[old];
                draws.push((key, new_idx, lo, hi));
                match scene_geom_groups.last_mut() {
                    Some((k, _, count)) if *k == key => *count += 1,
                    _ => scene_geom_groups.push((key, new_idx, 1)),
                }
            }
            scene_geom_instances = inst;
            scene_geom_draws = draws;
        }
        if std::env::var("PREVIZ_GEOM_STATS").is_ok() {
            let mut keys: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
            for (k, _, _, _) in &scene_geom_draws {
                *keys.entry(*k).or_default() += 1;
            }
            let mut counts: Vec<usize> = keys.values().copied().collect();
            counts.sort_unstable_by(|a, b| b.cmp(a));
            let mut diags: Vec<f32> =
                scene_geom_draws.iter().map(|(_, _, lo, hi)| (*hi - *lo).length()).collect();
            diags.sort_unstable_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            let n_ge = |t: f32| diags.iter().filter(|&&d| d >= t).count();
            log::info!(
                "GEOM: {} draws, {} meshes; diag max={:.1} | #≥0.5m={} #≥1m={} #≥1.5m={} #≥2m={} #≥3m={}",
                scene_geom_draws.len(),
                keys.len(),
                diags.first().copied().unwrap_or(0.0),
                n_ge(0.5),
                n_ge(1.0),
                n_ge(1.5),
                n_ge(2.0),
                n_ge(3.0),
            );
        }
        self.scene_geom_instances
            .upload(&self.device, &self.queue, &scene_geom_instances);
        // Drop baked meshes no longer referenced by the scene (e.g. after a new
        // MVR import replaces the geometry) so the cache can't grow unbounded.
        // Compare against the total model count (incl. cached failures) so the
        // steady state — failures and all — pays nothing.
        if self.scene_geom_cache.len() > total_models {
            let live: std::collections::HashSet<usize> = scene
                .geometry
                .iter()
                .flat_map(|o| o.models.iter().map(|m| Arc::as_ptr(&m.glb) as usize))
                .collect();
            self.scene_geom_cache.retain(|k, _| live.contains(k));
            self.scene_geom_bounds.retain(|k, _| live.contains(k));
        }

        // --- dynamic lines: fog-box wireframes + beam indicators ---
        let mut lines: Vec<LineVertex> = Vec::new();
        for (i, env) in scene.environments.iter().enumerate() {
            if env.hidden {
                continue; // outliner eye: a hidden fog box draws no wireframe
            }
            let color = if selection.environment == Some(i) {
                [0.6, 0.95, 1.0]
            } else {
                [0.30, 0.55, 0.72]
            };
            mesh::push_box_wireframe(&mut lines, env.min().to_array(), env.max().to_array(), color);
        }
        if settings.show_beam_wireframes {
            for (i, fixture) in scene.fixtures.iter().enumerate() {
                if fixture.hidden {
                    continue;
                }
                push_beam_indicator(&mut lines, &beam_spec(fixture, beam_frames[i].first().copied()));
            }
        }
        // Selection gizmo for every selected fixture (RGB axes + marker box) —
        // not for hidden ones (a hidden fixture draws nothing, gizmo included).
        for &sel in &selection.fixtures {
            if let Some(f) = scene.fixtures.get(sel).filter(|f| !f.hidden) {
                push_selection_gizmo(&mut lines, f.position);
            }
        }
        // Selection gizmo at the centre of every selected (visible) geometry object.
        for &sel in &selection.geometry {
            if let Some((lo, hi)) =
                scene.geometry.get(sel).filter(|g| !g.hidden).and_then(|g| g.world_bounds())
            {
                push_selection_gizmo(&mut lines, (lo + hi) * 0.5);
            }
        }
        // Infinite axis-constraint line (Blender style): when a modal transform has
        // an axis locked, draw a long coloured line through the pivot so the user can
        // see the constraint. The UI threads `(pivot, colour, dir)` via the
        // (runtime-only) `axis_hint` field on RenderSettings.
        if let Some((pivot, color, dir)) = settings.axis_hint {
            let dir = dir.normalize_or_zero();
            if dir != Vec3::ZERO {
                let half = camera.zfar * 0.9;
                lines.push(LineVertex { position: (pivot - dir * half).to_array(), color });
                lines.push(LineVertex { position: (pivot + dir * half).to_array(), color });
            }
        }
        let line_count = self.dynamic_lines.upload(&self.device, &self.queue, &lines);

        // --- volumetric uniforms + fixtures (use the first VISIBLE fog box, if any) ---
        // Outliner eye: skip hidden fog boxes so toggling a fog volume's visibility
        // actually removes its haze, not just the wireframe.
        let fog = scene.environments.iter().find(|e| !e.hidden);
        // PREVIZ_NOVOL disables ALL volumetric work (raymarch + froxel) — a bisection
        // kill-switch for the Windows/Vulkan device-loss hunt.
        let has_fog = fog.map(|f| f.density > 1e-4).unwrap_or(false)
            && std::env::var("PREVIZ_NOVOL").is_err();
        // Haze uniformity (1 smooth … 0 clustered). Drives the density-noise contrast
        // AND the temporal history cap below: at low uniformity the user WANTS to see the
        // moving smoke clusters, so we hold less history (let them live) instead of
        // smearing them into a smooth average.
        let fog_uniformity = fog.map(|f| f.uniformity).unwrap_or(1.0);
        // Hybrid froxel volumetric — opt-in (off by default; the per-pixel raymarch
        // is the default renderer). Enabled only when the adapter supports it and the
        // user turns it on (settings toggle), or PREVIZ_NOFROXEL is unset.
        let use_froxel = has_fog
            && self.froxel.is_some()
            && settings.froxel_volumetric
            && std::env::var("PREVIZ_NOFROXEL").is_err();

        // --- LED-wall surfaces: prepare each wall's content texture (image / live
        // frame), then build ONE emissive quad instance per visible screen (the
        // whole wall, never per-pixel). `wall_draws[j]` = the screen index for
        // instance j, so the forward pass can bind the right content texture. ---
        self.screen_runtime.retain(|&k, _| k < scene.screens.len());
        let mut wall_instances: Vec<WallInstance> = Vec::with_capacity(scene.screens.len());
        // (screen index, is_transparent) per drawn wall instance.
        let mut wall_draws: Vec<(usize, bool)> = Vec::new();
        for (i, s) in scene.screens.iter().enumerate() {
            if s.hidden {
                continue;
            }
            let res = s.resolution();
            let textured = self.ensure_screen_content(i, s);
            let (kind, tp, solid) = if textured {
                (2.0, 0.0, [0.0; 3]) // sample the content texture
            } else {
                match &s.content {
                    ScreenContent::SolidColor(c) => (0.0, 0.0, *c),
                    ScreenContent::TestPattern(p) => (1.0, p.code(), [0.0; 3]),
                    // Live/image content with no frame yet → a "no signal" grid.
                    _ => (1.0, 0.0, [0.0; 3]),
                }
            };
            // nits → HDR scale (1500 nits ≈ reference white): white content sits
            // near paper-white and only bright/over-driven walls bloom — a screen
            // displays its content tones, it is not a beam.
            let nits_scale = (s.nits / 1500.0).clamp(0.05, 6.0) * 1.25;
            let seam = if s.gap_mm > 0.0 { 0.06 } else { 0.015 };
            wall_instances.push(WallInstance {
                model: s.surface_matrix().to_cols_array_2d(),
                grid: [res[0] as f32, res[1] as f32, s.panels_wide as f32, s.panels_high as f32],
                color: [solid[0], solid[1], solid[2], nits_scale],
                look: [kind, tp, s.opacity, if selection.contains_screen(i) { 1.0 } else { 0.0 }],
                extra: [s.gamma, seam, s.curvature_deg.to_radians(), s.pixel_shape.code()],
            });
            wall_draws.push((i, s.opacity < 0.99));
        }
        self.wall_instances.upload(&self.device, &self.queue, &wall_instances);

        // Resolve each fixture's optics → its GPU beams (per lit emitter, per
        // prism facet; uniform LED arrays collapse to one aggregate). The
        // shaders loop `arrayLength(&fixtures)`, so the expansion is
        // transparent to them.
        let mut gpu_fixtures: Vec<FixtureGpu> = Vec::with_capacity(scene.fixtures.len());
        // Which scene fixture each GPU beam came from (shadow dedupe).
        let mut beam_fixture: Vec<usize> = Vec::with_capacity(scene.fixtures.len());
        let mut lens_instances: Vec<LensInstance> = Vec::with_capacity(scene.fixtures.len());
        // Per-fixture wheel chains, flattened into one buffer; each FixtureGpu
        // indexes its slice via cookie_r.w (offset) + cookie_u.w (count).
        let mut gpu_wheels: Vec<WheelGpu> = Vec::new();
        let beam_dump = std::env::var("PREVIZ_BEAM_DUMP").is_ok();
        for (i, f) in scene.fixtures.iter().enumerate() {
            if f.hidden {
                continue;
            }
            let key = f.gdtf.as_ref().map(|g| Arc::as_ptr(g) as usize).unwrap_or(0);
            let built = build_beam_gpus(f, &beam_frames[i], key, &self.gobo_atlas, time, &mut gpu_wheels);
            if beam_dump && !built.beams.is_empty() {
                let cmax = built
                    .beams
                    .iter()
                    .flat_map(|b| b.color[..3].iter().copied())
                    .fold(0.0_f32, f32::max);
                let b0 = &built.beams[0];
                log::info!(
                    "beams #{i} {}: n={} cmax={cmax:.3} tan_half={:.3} lens_r={:.3} n_ord={:.2} plain={} white={:.2} dir=[{:.2},{:.2},{:.2}]",
                    f.name,
                    built.beams.len(),
                    b0.dir_cos[3],
                    b0.color[3],
                    b0.shape[0],
                    b0.extra[2] < -0.5,
                    b0.extra[3],
                    b0.dir_cos[0], b0.dir_cos[1], b0.dir_cos[2],
                );
            }
            beam_fixture.resize(beam_fixture.len() + built.beams.len(), i);
            gpu_fixtures.extend(built.beams);
            lens_instances.extend(built.lenses);
        }
        if gpu_fixtures.is_empty() {
            gpu_fixtures.push(FixtureGpu::disabled());
            beam_fixture.push(usize::MAX);
        }

        // --- hero-beam shadow selection: the N sharpest lit beams get a shadow
        // map. Narrower cone (smaller tan_half) = sharper beam = most visible
        // shadow; at most one layer per fixture so a 19-cell array can't hog
        // the whole atlas. Each selected beam's light view-proj goes into the
        // dynamic-offset render buffer (for the depth pass) + the packed sample
        // buffer (for the lighting shaders), and its layer is stamped into
        // misc.w (-1 = unshadowed).
        let mut shadow_order: Vec<usize> = (0..gpu_fixtures.len())
            .filter(|&i| gpu_fixtures[i].dir_cos[3] > 1e-4)
            .collect();
        shadow_order.sort_by(|&a, &b| {
            gpu_fixtures[a].dir_cos[3]
                .partial_cmp(&gpu_fixtures[b].dir_cos[3])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // Shadows only matter in the lit Beauty view (unlit/wireframe skip lighting).
        let max_shadows = if settings.mode == ViewportMode::Beauty
            && std::env::var("PREVIZ_NOSHADOW").is_err()
        {
            // Capped by the user's `shadow_max` lever (each hero map is a depth pass,
            // ~2-3 ms at Retina) — can only reduce below the atlas-sized `shadow::MAX`.
            (settings.shadow_max as usize).min(shadow::MAX)
        } else {
            0
        };
        // sample_mats holds ALL atlas layers (heroes 0..n_shadows + the shared
        // occluder at shadow::SHARED); identity for the unused middle slots.
        let mut sample_mats: Vec<[[f32; 4]; 4]> =
            vec![Mat4::IDENTITY.to_cols_array_2d(); shadow::LAYERS];
        let mut shadowed_fixtures: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut n_shadows = 0usize;
        for fi in shadow_order {
            if n_shadows >= max_shadows {
                break;
            }
            if !shadowed_fixtures.insert(beam_fixture[fi]) {
                continue;
            }
            let layer = n_shadows;
            let f = &gpu_fixtures[fi];
            let origin = Vec3::new(f.pos_range[0], f.pos_range[1], f.pos_range[2]);
            let bdir = Vec3::new(f.dir_cos[0], f.dir_cos[1], f.dir_cos[2]);
            let up = Vec3::new(f.cookie_u[0], f.cookie_u[1], f.cookie_u[2]);
            let tan_half = f.dir_cos[3].max(1e-3);
            let range = f.pos_range[3].max(1.0);
            // Perspective from the lens, FOV = full cone angle (clamped so a wide
            // wash doesn't make a degenerate near-180° projection).
            let fov = (2.0 * tan_half.atan()).clamp(0.05, 2.4);
            // Push the near plane well off the lens. A near:far of 0.1:40 crams the
            // whole depth range into ndc.z ≈ 0.95–1.0, so the bias swamps the tiny
            // depth deltas and the beam LEAKS THROUGH occluders that aren't right at
            // the lens (worse the further out they are). A near ~3% of range (the
            // beam never hits anything that close to its own lens) restores precision
            // so partial / distant occluders block correctly.
            let near = (range * 0.03).clamp(0.4, 3.0);
            let vp = Mat4::perspective_rh(fov, 1.0, near, range) * Mat4::look_to_rh(origin, bdir, up);
            let cols = vp.to_cols_array_2d();
            self.queue.write_buffer(
                &self.shadow.render_matrices,
                layer as u64 * self.shadow.align,
                bytemuck::bytes_of(&cols),
            );
            sample_mats[layer] = cols;
            gpu_fixtures[fi].misc[3] = layer as f32;
            n_shadows += 1;
        }

        // SHARED occluder: ONE ortho depth pass fit to the union of every lit beam's
        // volume, used as the fallback occluder for every NON-hero beam. DISABLED by
        // default (opt-in via PREVIZ_SHARED): the single mean-direction projection stamped
        // a truss/set into the fog of EVERY non-hero beam — including beams whose path
        // never crosses it — producing phantom occluder outlines in the haze (occlusion is
        // per-light-to-sample, which one shared direction can't represent). Dropping it
        // means non-hero beams just don't self-occlude mid-air (hero beams keep correct
        // per-beam shadows); zero phantoms, and it reclaims the pass (~0.5 ms). A future
        // direction-binned version could restore correct occlusion for all beams.
        let mut shared_layer = -1i32;
        if max_shadows > 0 && std::env::var("PREVIZ_SHARED").is_ok() {
            let mut lo = Vec3::splat(f32::INFINITY);
            let mut hi = Vec3::splat(f32::NEG_INFINITY);
            let mut mean_dir = Vec3::ZERO;
            let mut any = false;
            for f in &gpu_fixtures {
                if f.dir_cos[3] <= 1e-4 {
                    continue;
                }
                let o = Vec3::new(f.pos_range[0], f.pos_range[1], f.pos_range[2]);
                let d = Vec3::new(f.dir_cos[0], f.dir_cos[1], f.dir_cos[2]);
                let r = f.pos_range[3].min(50.0);
                lo = lo.min(o).min(o + d * r);
                hi = hi.max(o).max(o + d * r);
                mean_dir += d;
                any = true;
            }
            if any {
                let dir = if mean_dir.length_squared() > 1e-6 {
                    mean_dir.normalize()
                } else {
                    Vec3::NEG_Y
                };
                let center = (lo + hi) * 0.5;
                let radius = ((hi - lo).length() * 0.5).max(1.0);
                let eye = center - dir * (radius + 5.0);
                let up = if dir.y.abs() > 0.95 { Vec3::Z } else { Vec3::Y };
                let vp = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.1, radius * 2.0 + 10.0)
                    * Mat4::look_to_rh(eye, dir, up);
                let cols = vp.to_cols_array_2d();
                self.queue.write_buffer(
                    &self.shadow.render_matrices,
                    shadow::SHARED as u64 * self.shadow.align,
                    bytemuck::bytes_of(&cols),
                );
                sample_mats[shadow::SHARED] = cols;
                shared_layer = shadow::SHARED as i32;
            }
        }
        if n_shadows > 0 || shared_layer >= 0 {
            self.queue
                .write_buffer(&self.shadow.sample_matrices, 0, bytemuck::cast_slice(&sample_mats));
        }

        // --- LED walls as cheap, blurred area lights. One wide, soft "beam" per
        // screen coloured by the wall's AVERAGE (summary) colour — the wall's
        // entire contribution to surface lighting + volumetric haze, never
        // per-pixel (docs/RESEARCH-led-ndi.md). Appended AFTER shadow selection so
        // a wall never consumes a hero shadow map; the wide cone + plain sentinel
        // keep it nearly free (the radial pre-cull rejects off-axis samples). ---
        for (si, s) in scene.screens.iter().enumerate() {
            if s.hidden || s.emit <= 0.0 {
                continue;
            }
            let nits_gain = (s.nits / 1500.0).clamp(0.05, 6.0);
            let total = s.emit * 0.45 * nits_gain;
            let normal = s.world_normal();
            if total <= 1e-4 || normal.length_squared() < 1e-6 {
                continue;
            }
            let right = s.transform.x_axis.truncate().normalize_or_zero();
            let up_axis = s.transform.y_axis.truncate().normalize_or_zero();
            // Aim the wash forward AND slightly down so it lights the floor + haze
            // IN FRONT of the wall (a flat normal alone never reaches the floor).
            let dir = (normal - up_axis * 0.35).normalize_or_zero();
            let up = right.cross(dir).normalize_or_zero();
            let [w, h] = s.size_m();
            let surf = s.surface_matrix();
            // Sample a small grid of emitters ACROSS the screen face, each coloured
            // by the content at that region — so a gradient/bars wall throws the
            // RIGHT COLOURS into the haze and reads as a broad AREA source, not one
            // point. More horizontal samples on wider walls. (`emit` scales it.)
            // More, tighter emitters so each region's colour stays LOCALISED in
            // front of it (wide overlapping cones would blend red+green+blue → white).
            let aspect = (w / h.max(1e-3)).max(0.2);
            let nx = ((aspect * 3.0).round() as i32).clamp(3, 10);
            let ny: i32 = 2;
            let per = total / (nx * ny) as f32;
            let lens_r = (0.5 * w / nx as f32).clamp(0.15, 0.7);
            // Narrow cone so adjacent colours don't all overlap into a white wash.
            let tan_half = (1.4 * w / nx as f32 / (h * 2.0)).clamp(0.18, 0.45);
            let range = (h * 2.0).max(4.0);
            let rt = self.screen_runtime.get(&si);
            for j in 0..ny {
                for i in 0..nx {
                    let u = (i as f32 + 0.5) / nx as f32;
                    let v = (j as f32 + 0.5) / ny as f32;
                    let c = screen_light_color(s, rt, u, v);
                    let p = surf.transform_point3(Vec3::new(u - 0.5, v - 0.5, 0.0));
                    gpu_fixtures.push(FixtureGpu {
                        pos_range: [p.x, p.y, p.z, range],
                        dir_cos: [dir.x, dir.y, dir.z, tan_half], // localized forward cone
                        color: [c[0] * per, c[1] * per, c[2] * per, lens_r],
                        cookie_r: [right.x, right.y, right.z, 0.0], // no wheel chain
                        cookie_u: [up.x, up.y, up.z, 0.0],
                        extra: [-1.0, 0.0, -1.0, 0.0], // no anim; plain; NO white wash
                        shape: [1.0, 0.0, 1.0, 0.0],   // n_order, focus, IRIS OPEN, frost
                        misc: [0.0, 0.0, 0.0, -1.0],   // no CA/laser/atlas; no shadow
                        cmyf: [0.0, 0.0, 0.0, 1.2],    // wash → blurred
                    });
                }
            }
        }

        self.fixtures_storage
            .upload(&self.device, &self.queue, &gpu_fixtures);

        // --- tiled light culling (Gem 3) ---
        // Bucket each beam's screen-projected cull cone into a 2D tile grid so the
        // forward light loop AND the volumetric raymarch each iterate only the beams
        // overlapping their pixel's tile, not all N. CPU-built (cheap at this fixture
        // count), CSR-packed (`tile_offsets`/`tile_lights`), uploaded like fixtures.
        // CONSERVATIVE by construction: a beam lands in a tile iff its cull cone — using
        // the EXACT shader cull radius (mesh.wgsl/volumetric.wgsl `cull`) — could light
        // any pixel there, and the per-light gates still run in-shader, so a tiled light
        // is only ever extra work, never a dropped contributor. Wide beams (covering most
        // of the screen) go in a global prefix every tile scans, keeping spot lists tight.
        // PREVIZ_NOCULL forces the all-lights fallback (the pixel-identity test harness).
        // `avg_tile_beams` (set in the block) = the beams an average ray actually marches;
        // the volumetric step budget divides by THIS, not the full count, so culling's freed
        // per-ray budget is spent on more march steps (the banding fix) — see the fog block.
        let mut avg_tile_beams = gpu_fixtures.len().max(1);
        {
            let n_tiles = (tiles_x * tiles_y) as usize;
            let view_proj = camera.view_proj(aspect);
            let eye = camera.eye();
            // The 8-point ring INSCRIBES the cull disc, reaching only cos(π/8)≈0.924·r
            // between vertices; scale it up so the octagon CIRCUMSCRIBES the disc (its
            // edges touch r) — otherwise the screen AABB under-covers the projected disc
            // and an edge fragment lands in an unscattered tile (a beam pop).
            const RING_CIRCUM: f32 = 1.082_392_3; // 1/cos(π/8)
            let build_tiles = gpu_fixtures.len() >= TILE_MIN_LIGHTS
                && std::env::var("PREVIZ_NOCULL").is_err();
            let mut wide: Vec<u32> = Vec::new();
            let mut per_tile: Vec<Vec<u32>> = vec![Vec::new(); n_tiles];
            let to_screen = |p: Vec3| -> Option<(f32, f32)> {
                let clip = view_proj * p.extend(1.0);
                if clip.w <= 1e-4 {
                    return None; // at/behind the near plane — unbounded screen extent
                }
                let nx = clip.x / clip.w;
                let ny = clip.y / clip.w;
                // NDC → framebuffer px (y flipped, matching the volumetric duv mapping).
                Some(((nx * 0.5 + 0.5) * vw as f32, (0.5 - ny * 0.5) * vh as f32))
            };
            for (i, fx) in gpu_fixtures.iter().enumerate() {
                let idx = i as u32;
                let range = fx.pos_range[3];
                let tan_half = fx.dir_cos[3];
                if !build_tiles || range <= 0.0 || tan_half <= 0.0 {
                    wide.push(idx); // disabled/degenerate or culling off → everywhere
                    continue;
                }
                let lpos = Vec3::new(fx.pos_range[0], fx.pos_range[1], fx.pos_range[2]);
                let bdir = Vec3::new(fx.dir_cos[0], fx.dir_cos[1], fx.dir_cos[2]);
                let right = Vec3::new(fx.cookie_r[0], fx.cookie_r[1], fx.cookie_r[2]);
                let up = Vec3::new(fx.cookie_u[0], fx.cookie_u[1], fx.cookie_u[2]);
                let lens_r = fx.color[3];
                let iris = fx.shape[2];
                let n_order = fx.shape[0];
                let ca = fx.misc[0];
                // The SHADER's conservative cull multiplier (mesh.wgsl / volumetric.wgsl).
                let k = 2.5 + ca.abs() + (2.0 - n_order.clamp(1.0, 2.0));
                // Camera inside/near this beam's cone → its projected silhouette wraps or
                // explodes under perspective and the sampled-ring AABB can't bound it
                // (the inter-slice bulge the reviewer flagged). Mark it wide — both correct
                // and cheap, since such a beam genuinely floods most of the view anyway.
                let rel_eye = eye - lpos;
                let along = rel_eye.dot(bdir).clamp(0.0, range);
                let perp = (rel_eye - bdir * along).length();
                if perp < (lens_r + along * tan_half) * iris * k * 2.5 {
                    wide.push(idx);
                    continue;
                }
                let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
                let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
                let mut cover_all = false;
                // 9 axial slices (vs the cone widening linearly) bound the inter-slice
                // perspective bulge; the circumscribed ring bounds each disc.
                'depths: for sd in 0..9 {
                    let d = (sd as f32) / 8.0 * range;
                    let center = lpos + bdir * d;
                    let r = (lens_r + d * tan_half) * iris * k * RING_CIRCUM; // cull radius at d
                    for s in 0..8 {
                        let a = std::f32::consts::TAU * (s as f32) / 8.0;
                        let p = center + (right * a.cos() + up * a.sin()) * r;
                        match to_screen(p) {
                            Some((sx, sy)) => {
                                min_x = min_x.min(sx);
                                min_y = min_y.min(sy);
                                max_x = max_x.max(sx);
                                max_y = max_y.max(sy);
                            }
                            None => {
                                cover_all = true;
                                break 'depths;
                            }
                        }
                    }
                }
                if cover_all || !min_x.is_finite() {
                    wide.push(idx);
                    continue;
                }
                let tpx = LIGHT_TILE_PX as f32;
                let txi = tiles_x as i32;
                let tyi = tiles_y as i32;
                let tx0 = ((min_x / tpx).floor() as i32 - 1).clamp(0, txi - 1);
                let tx1 = ((max_x / tpx).floor() as i32 + 1).clamp(0, txi - 1);
                let ty0 = ((min_y / tpx).floor() as i32 - 1).clamp(0, tyi - 1);
                let ty1 = ((max_y / tpx).floor() as i32 + 1).clamp(0, tyi - 1);
                // Spans most of the screen → cheaper as a wide light (one shared block
                // every tile scans) than scattered into ~every per-tile list.
                if ((tx1 - tx0 + 1) * 2 >= txi) && ((ty1 - ty0 + 1) * 2 >= tyi) {
                    wide.push(idx);
                    continue;
                }
                for ty in ty0..=ty1 {
                    let row = (ty as u32 * tiles_x) as usize;
                    for tx in tx0..=tx1 {
                        per_tile[row + tx as usize].push(idx);
                    }
                }
            }
            // Flatten to CSR: each tile's slice is [wide-prefix..., its narrow beams].
            let mut offsets: Vec<u32> = Vec::with_capacity(n_tiles + 1);
            let mut flat: Vec<u32> = Vec::with_capacity(n_tiles * wide.len() + 64);
            offsets.push(0);
            for tile in &per_tile {
                flat.extend_from_slice(&wide);
                flat.extend_from_slice(tile);
                offsets.push(flat.len() as u32);
            }
            if flat.is_empty() {
                flat.push(0); // storage buffers can't bind 0 bytes
            }
            self.tile_offsets.upload(&self.device, &self.queue, &offsets);
            self.tile_lights.upload(&self.device, &self.queue, &flat);
            // Beams an average ray marches (wide prefix + tile-local). Drives the
            // volumetric step budget below.
            avg_tile_beams = (flat.len() / n_tiles).max(1);
        }

        // Keep the storage binding non-empty (≥1 element) even with no wheels.
        if gpu_wheels.is_empty() {
            gpu_wheels.push(WheelGpu::zeroed());
        }
        self.wheels_storage
            .upload(&self.device, &self.queue, &gpu_wheels);
        let lens_count = self
            .lens_instances
            .upload(&self.device, &self.queue, &lens_instances);

        if let Some(fog) = fog {
            let inv_vp = camera.view_proj(aspect).inverse();
            let eye = camera.eye();
            // Constant-dt target for the raymarch: a full-diagonal ray spends the
            // whole `steps` budget; shorter clipped rays scale their step count down
            // to keep per-metre sampling (dt) roughly constant. See volumetric.wgsl.
            // Adaptive step budget. The raymarch is O(pixels·steps·beams), so spread
            // a fixed step×beam budget over however many beams there are: a FEW hero
            // beams (the shaft you scrutinise) get MANY steps — which is what makes
            // deterministic midpoint integration SMOOTH (enough samples to resolve the
            // haze density, so no dither and no banding) — while a busy many-beam rig
            // floors out for frame rate. `target_dt` is derived from the cap so the
            // extra steps actually apply (a full-box ray then takes `step_cap` samples).
            // In HYBRID mode the raymarch only marches the few hero beams (the froxel
            // carries the rest), so divide the step budget by the hero count, not all
            // beams → each hero gets MANY steps = crisp, smooth shafts.
            // Step budget divides by the beams a ray ACTUALLY marches. Pre-tiling that
            // was all N (so a 168-beam rig floored at 40 steps → bad longitudinal banding
            // in a big fog box, worst during camera motion when temporal can't accumulate).
            // Tiled culling means a ray now marches only its tile's beams (avg_tile_beams),
            // so the SAME per-ray beam cost buys far more steps — spend that on step count
            // (the ghost-free banding fix; raising temporal history instead would smear,
            // since the volumetric reprojects against scene depth, not the mid-air haze).
            // Isolated beams (sparse tiles → where banding reads most) get the most steps.
            let march_beams = if use_froxel {
                n_shadows.max(1)
            } else {
                avg_tile_beams
            };
            // Per-ray work budget in beam-samples (steps × beams-marched). Pre-tiling a ray
            // marched all N beams, so this budget floored the step count (40) and the big
            // fog box banded; now a ray marches only `avg_tile_beams`, so the same budget
            // buys ~N/avg× more steps — spent on killing the banding (and the motion flicker,
            // since a denser per-frame march bands far less when temporal can't accumulate).
            // Stays well under the pre-tiling per-ray cost. Scale with the `steps` quality knob.
            let beam_sample_budget = settings.steps.max(40) as f32 * 40.0;
            let step_cap = (beam_sample_budget / march_beams as f32).clamp(64.0, 176.0);
            let target_dt = (fog.max() - fog.min()).length() / step_cap;
            let vol = VolumetricUniform {
                inv_view_proj: inv_vp.to_cols_array_2d(),
                eye_time: [eye.x, eye.y, eye.z, time],
                fog_min_density: [fog.min().x, fog.min().y, fog.min().z, fog.density],
                fog_max_g: [fog.max().x, fog.max().y, fog.max().z, fog.anisotropy],
                albedo_beam: [
                    fog.color[0],
                    fog.color[1],
                    fog.color[2],
                    settings.beam_intensity,
                ],
                counts: [
                    // x: HYBRID sentinel -2 = raymarch heroes only (froxel does the
                    // masses); otherwise the shared-occluder atlas layer (-1 = none).
                    if use_froxel { -2.0 } else { shared_layer as f32 },
                    step_cap,
                    target_dt,
                    if std::env::var("PREVIZ_JITTER").is_ok() { 1.0 } else { 0.0 },
                ],
                // Same chroma read-up strength as the froxel pass (below) so the
                // hybrid masses/heroes seam lifts saturated colours identically.
                // y = frame index (mod 64): the volumetric uses it to ANIMATE its
                // interleaved-gradient-noise ray-start jitter (a fresh blue-noise pattern
                // each frame). Was a screen-coherent golden phase, which shifted the whole
                // step-band pattern rigidly each frame → read as flicker; per-pixel blue
                // grain instead resolves via the EMA when static and reads as faint grain
                // (not coherent bands) during motion.
                chroma: [
                    settings.chroma_haze,
                    (self.frame_index % 64) as f32,
                    fog.uniformity, // z = haze uniformity (1 smooth … 0 clustered)
                    fog.cluster_contrast, // w = cluster vs haze density contrast
                ],
                // Tiled light culling: same full-res grid the mesh pass uses (render_mode.zw),
                // so the ray scans exactly the beams its tile holds.
                tile: [tiles_x as f32, tiles_y as f32, LIGHT_TILE_PX as f32, 0.0],
            };
            self.queue
                .write_buffer(&self.volumetric_uniform, 0, bytemuck::bytes_of(&vol));

            if use_froxel {
                if let Some(fx) = &self.froxel {
                    let (lo, hi) = (fog.min(), fog.max());
                    let near = 0.3_f32;
                    // Far = distance to the farthest fog-box corner, so the grid
                    // spans the whole box along every ray.
                    let mut far = near + 1.0;
                    for cx in [lo.x, hi.x] {
                        for cy in [lo.y, hi.y] {
                            for cz in [lo.z, hi.z] {
                                far = far.max((Vec3::new(cx, cy, cz) - eye).length());
                            }
                        }
                    }
                    let fu = FroxelUniform {
                        inv_view_proj: inv_vp.to_cols_array_2d(),
                        eye_time: [eye.x, eye.y, eye.z, time],
                        fog_min_density: [lo.x, lo.y, lo.z, fog.density],
                        fog_max_g: [hi.x, hi.y, hi.z, fog.anisotropy],
                        albedo_beam: [fog.color[0], fog.color[1], fog.color[2], settings.beam_intensity],
                        dims: [
                            fx.dims.0 as f32,
                            fx.dims.1 as f32,
                            fx.dims.2 as f32,
                            shared_layer as f32,
                        ],
                        planes: [near, far, settings.chroma_haze, 0.0],
                    };
                    self.queue.write_buffer(&fx.uniform, 0, bytemuck::bytes_of(&fu));
                }
            }
        }

        let post = PostUniform {
            exposure: settings.exposure,
            bloom: settings.bloom,
            _pad: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.post_uniform, 0, bytemuck::bytes_of(&post));

        // --- bind groups for this frame ---
        // Bind only the *used* portion of the fixtures buffer, so `arrayLength`
        // in the shaders returns the real fixture count (the buffer may be
        // larger than what we wrote).
        let used_fixtures = (gpu_fixtures.len() * std::mem::size_of::<FixtureGpu>()) as u64;
        let fixtures_binding = || {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &self.fixtures_storage.buffer,
                offset: 0,
                size: std::num::NonZeroU64::new(used_fixtures),
            })
        };
        let used_wheels = (gpu_wheels.len() * std::mem::size_of::<WheelGpu>()) as u64;
        let wheels_binding = || {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &self.wheels_storage.buffer,
                offset: 0,
                size: std::num::NonZeroU64::new(used_wheels),
            })
        };

        let light_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("light-bg"),
            layout: &self.light_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: fixtures_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.gobo_atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.gobo_atlas.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.shadow.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.shadow.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.shadow.sample_matrices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wheels_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: self.tile_offsets.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: self.tile_lights.buffer.as_entire_binding(),
                },
            ],
        });

        let volumetric_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("volumetric-bg"),
            layout: &self.volumetric_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.volumetric_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: fixtures_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(self.viewport.depth_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.noise_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.noise_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&self.gobo_atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&self.gobo_atlas.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&self.shadow.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::Sampler(&self.shadow.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: self.shadow.sample_matrices.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wheels_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: self.tile_offsets.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: self.tile_lights.buffer.as_entire_binding(),
                },
            ],
        });
        // Froxel compute + composite bind groups (only when the froxel path runs).
        // inject writes inject_view + reads result_view (dummy); integrate writes
        // result_view + reads inject_view.
        let froxel_bgs = if use_froxel {
            self.froxel.as_ref().map(|fx| {
                let compute_bg = |out: &wgpu::TextureView, inp: &wgpu::TextureView| {
                    self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("froxel-compute-bg"),
                        layout: &fx.compute_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: fx.uniform.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: fixtures_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.noise_view) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.noise_sampler) },
                            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.gobo_atlas.view) },
                            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.gobo_atlas.sampler) },
                            wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&self.shadow.array_view) },
                            wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Sampler(&self.shadow.sampler) },
                            wgpu::BindGroupEntry { binding: 8, resource: self.shadow.sample_matrices.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 9, resource: wheels_binding() },
                            wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(out) },
                            wgpu::BindGroupEntry { binding: 11, resource: wgpu::BindingResource::TextureView(inp) },
                        ],
                    })
                };
                let inject_bg = compute_bg(&fx.inject_view, &fx.result_view);
                let integrate_bg = compute_bg(&fx.result_view, &fx.inject_view);
                let comp_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("froxel-composite-bg"),
                    layout: &fx.composite_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: fx.uniform.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&fx.result_view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&fx.sampler) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(self.viewport.depth_view()) },
                    ],
                });
                (inject_bg, integrate_bg, comp_bg)
            })
        } else {
            None
        };

        // --- temporal accumulation (EMA) state for the half-res volumetric ---
        // The raymarch jitters per-frame; this resolve reprojects the previous EMA and
        // blends it in, converging the jitter into a smooth beam. History weight ramps
        // up while STATIC and drops when the camera or any beam moves (so moving beams
        // don't ghost). Runs whenever the raymarch path renders fog.
        let temporal_on = has_fog && settings.mode == ViewportMode::Beauty;
        // The raymarch (and thus the temporal pass) actually runs unless we're in
        // froxel-only mode with no hero beams (mirrors the pass gate below).
        let raymarch_ran = temporal_on && (!use_froxel || n_shadows > 0);
        let cur_view_proj = camera.view_proj(aspect);
        let cur_ema = (self.frame_index & 1) as usize;
        let prev_ema = cur_ema ^ 1;
        if temporal_on {
            // Hash the FULL per-beam + per-wheel GPU state (position, direction, colour,
            // dimmer, gobo/prism/wheel rotation + scroll, CMY) so ANY in-beam motion —
            // not just pan/tilt — invalidates history and avoids ghosting a moving look.
            let mut sig: u64 = 0xcbf2_9ce4_8422_2325;
            for &w in bytemuck::cast_slice::<FixtureGpu, u32>(&gpu_fixtures) {
                sig = (sig ^ w as u64).wrapping_mul(0x0100_0000_01b3);
            }
            for &w in bytemuck::cast_slice::<WheelGpu, u32>(&gpu_wheels) {
                sig = (sig ^ w as u64).wrapping_mul(0x0100_0000_01b3);
            }
            // History is trustworthy only if last frame wrote the EMA at this size.
            let resized = self.viewport.size != self.prev_size;
            let history_valid = self.ema_valid
                && !resized
                && self.frame_index != 0
                && std::env::var("PREVIZ_NOTEMPORAL").is_err();
            let moving = cur_view_proj != self.prev_view_proj || sig != self.prev_beam_sig;
            if moving || !history_valid {
                self.accum_frames = 0;
            } else {
                self.accum_frames = (self.accum_frames + 1).min(64);
            }
            // History cap scales with uniformity: smooth fog accumulates hard (0.92) for
            // glass-smooth beams; clustered fog holds little (down to ~0.55) so the moving
            // smoke pockets stay crisp and alive instead of averaging into a smear.
            let hist_cap = 0.55 + 0.37 * fog_uniformity;
            let opacity = if !history_valid {
                0.0 // first frame / resize / froxel-skip → trust this frame fully
            } else if moving {
                (0.35_f32).min(hist_cap)
            } else {
                (self.accum_frames as f32 / (self.accum_frames as f32 + 1.0)).min(hist_cap)
            };
            let eye = camera.eye();
            let tu = TemporalUniform {
                cur_inv_view_proj: cur_view_proj.inverse().to_cols_array_2d(),
                prev_view_proj: self.prev_view_proj.to_cols_array_2d(),
                eye: [eye.x, eye.y, eye.z, 250.0],
                params: [opacity, 0.0, 0.0, 0.0],
            };
            self.queue.write_buffer(&self.temporal_uniform, 0, bytemuck::bytes_of(&tu));
            self.prev_beam_sig = sig;
        }
        let temporal_bg = if temporal_on {
            Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vol-temporal-bg"),
                layout: &self.vol_temporal_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.temporal_uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(self.viewport.vol_view()) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(self.viewport.vol_ema_view(prev_ema)) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.vol_linear_sampler) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(self.viewport.depth_view()) },
                ],
            }))
        } else {
            None
        };
        // The composite reads the temporally-resolved EMA when temporal ran, else the raw
        // vol — plus the scene depth, for the depth-aware (edge-preserving) upsample.
        let composite_src = if temporal_on {
            self.viewport.vol_ema_view(cur_ema)
        } else {
            self.viewport.vol_view()
        };
        let composite_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite-bg"),
            layout: &self.composite_upsample_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(composite_src) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.post_sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(self.viewport.depth_view()) },
            ],
        });
        let bright_bg = self.single_tex_bg(self.viewport.hdr_view());
        let blur_h_bg = self.single_tex_bg(self.viewport.bloom_view(0));
        let blur_v_bg = self.single_tex_bg(self.viewport.bloom_view(1));
        let tonemap_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tonemap-bg"),
            layout: &self.tonemap_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(self.viewport.hdr_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(self.viewport.bloom_view(0)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.post_uniform.as_entire_binding(),
                },
            ],
        });

        // Shadow-caster LOD (projected-size, NOT absolute size — so a 1.8 m
        // performer standing in a beam still casts its silhouette while the festival's
        // thousands of distant/tiny audience meshes, which would only cast sub-pixel
        // shadows, are skipped). Per hero beam: keep casters whose shadow-map
        // projection is at least SHADOW_MIN_PX, capped to the SHADOW_MAX_CASTERS
        // largest (bounds the worst case — a beam aimed at a dense mass). The forward
        // pass still draws + lights every object, so the crowd stays fully visible;
        // it just doesn't all CAST hero shadows.
        const SHADOW_MIN_PX: f32 = 3.0;
        const SHADOW_MAX_CASTERS: usize = 96;
        let mut casters: Vec<(usize, f32)> = Vec::new();

        // Pass 0: shadow maps — one depth-only pass per hero beam (layers 0..n)
        // plus the ONE shared occluder (layer shadow::SHARED), each rendering the
        // solid occluders (floor + MVR geometry + fixture models) from that layer's
        // viewpoint into its atlas layer.
        let mut render_layers: Vec<usize> = (0..n_shadows).collect();
        if shared_layer >= 0 {
            render_layers.push(shadow::SHARED);
        }
        for &layer in &render_layers {
            let mut spass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow.layer_views[layer],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.ts_rp(gpu_timer::P_SHADOW_BASE + layer as u32),
                ..Default::default()
            });
            spass.set_pipeline(&self.shadow.pipeline);
            spass.set_bind_group(0, &self.shadow.render_bg, &[(layer as u64 * self.shadow.align) as u32]);
            spass.set_vertex_buffer(0, self.floor_mesh.vertex_buffer.slice(..));
            spass.set_vertex_buffer(1, self.floor_instances.buffer.slice(..));
            spass.draw(0..self.floor_mesh.vertex_count, 0..1);
            if !scene_geom_draws.is_empty() {
                // Gather casters visible to this beam, sized by shadow-map projection;
                // drop the sub-pixel ones (frustum cull is implicit in clip_proj_px).
                let svp = Mat4::from_cols_array_2d(&sample_mats[layer]);
                casters.clear();
                for (di, (_, _, lo, hi)) in scene_geom_draws.iter().enumerate() {
                    if let Some(px) = clip_proj_px(&svp, *lo, *hi, shadow::RES as f32) {
                        if px >= SHADOW_MIN_PX {
                            casters.push((di, px));
                        }
                    }
                }
                // Cap to the biggest N (the visible silhouettes); bounds a beam aimed
                // at a dense mass without dropping the prominent occluders.
                if casters.len() > SHADOW_MAX_CASTERS {
                    casters.select_nth_unstable_by(SHADOW_MAX_CASTERS, |a, b| {
                        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    casters.truncate(SHADOW_MAX_CASTERS);
                }
                spass.set_vertex_buffer(1, self.scene_geom_instances.buffer.slice(..));
                for &(di, _) in &casters {
                    let (key, idx, _, _) = &scene_geom_draws[di];
                    if let Some(Some(mesh)) = self.scene_geom_cache.get(key) {
                        spass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        spass.draw(0..mesh.vertex_count, *idx..*idx + 1);
                    }
                }
            }
            if !gdtf_draws.is_empty() {
                spass.set_vertex_buffer(1, self.gdtf_instances.buffer.slice(..));
                for (key, model, idx) in &gdtf_draws {
                    if let Some(mesh) = self.gdtf_cache.get(key).and_then(|m| m.get(model)) {
                        spass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        spass.draw(0..mesh.vertex_count, *idx..*idx + 1);
                    }
                }
            }
        }

        // Pass 0.5: DEPTH PRE-PASS — write the opaque scene depth with NO fragment work
        // so the main forward pass runs its heavy per-fragment light loop EXACTLY once per
        // visible pixel (Equal test, no overdraw). The pre-pass adds draws/vertex work, so
        // it only pays off when that loop is expensive — gate it on the lit-fixture count
        // (a sparse scene keeps the plain single Less+write pass, no regression). Wireframe
        // (lines, not fill) always skips it. The opaque-mesh draws here MUST stay identical
        // to the forward pass's, or DEPTH_EQUAL would reject pixels — keep them in sync.
        const PREPASS_MIN_LIGHTS: usize = 16;
        // The pre-pass re-draws all opaque geometry, so it only pays off when those draws
        // are cheap — i.e. instancing collapsed the scene into few mesh groups. If geometry
        // failed to dedup (thousands of unique meshes), the extra draw calls cost more than
        // the light-loop overdraw they save (measured: a non-deduped 5932-group scene
        // regressed 61→56 fps; the same scene deduped to 236 groups gained 72→75). Both load
        // paths dedup — MVR import shares resource Arcs, `.archie` load re-interns them
        // (project::intern_geometry_resources) — so real scenes stay well under the cap.
        const PREPASS_MAX_GROUPS: usize = 1500;
        let geom_groups = ranges.len() + gdtf_groups.len() + scene_geom_groups.len();
        let use_prepass = settings.mode != ViewportMode::Wireframe
            && gpu_fixtures.len() >= PREPASS_MIN_LIGHTS
            && geom_groups <= PREPASS_MAX_GROUPS;
        if use_prepass {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("depth-prepass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: self.viewport.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.ts_rp(gpu_timer::P_DEPTH),
                ..Default::default()
            });
            pass.set_pipeline(&self.mesh_depth_prepass);
            // Shares the mesh layout (camera+light+world), so bind all three even though
            // only group 0 is read by the vertex stage.
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &light_bg, &[]);
            pass.set_bind_group(2, &self.world_bind_group, &[]);
            pass.set_vertex_buffer(0, self.floor_mesh.vertex_buffer.slice(..));
            pass.set_vertex_buffer(1, self.floor_instances.buffer.slice(..));
            pass.draw(0..self.floor_mesh.vertex_count, 0..1);
            pass.set_vertex_buffer(1, self.fixture_instances.buffer.slice(..));
            for (geometry, start, count) in &ranges {
                let m = self.mesh_for(*geometry);
                pass.set_vertex_buffer(0, m.vertex_buffer.slice(..));
                pass.draw(0..m.vertex_count, *start..*start + *count);
            }
            if !gdtf_groups.is_empty() {
                pass.set_vertex_buffer(1, self.gdtf_instances.buffer.slice(..));
                for (key, model, base, count) in &gdtf_groups {
                    if let Some(mesh) = self.gdtf_cache.get(key).and_then(|m| m.get(model)) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *base..*base + *count);
                    }
                }
            }
            if !scene_geom_groups.is_empty() {
                pass.set_vertex_buffer(1, self.scene_geom_instances.buffer.slice(..));
                for (key, base, count) in &scene_geom_groups {
                    if let Some(Some(mesh)) = self.scene_geom_cache.get(key) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *base..*base + *count);
                    }
                }
            }
            if gdtf_placeholder_count > 0 {
                pass.set_vertex_buffer(0, self.cone_mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.gdtf_placeholder_instances.buffer.slice(..));
                pass.draw(0..self.cone_mesh.vertex_count, 0..gdtf_placeholder_count);
            }
        }

        // Pass 1: forward opaque scene -> HDR target.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("forward-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.viewport.hdr_view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.002,
                            g: 0.0025,
                            b: 0.005,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: self.viewport.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        // Pre-pass already wrote depth → load it (mesh draws test Equal,
                        // no write). Without it, clear here (plain Less+write path).
                        load: if use_prepass {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(1.0)
                        },
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.ts_rp(gpu_timer::P_FORWARD),
                ..Default::default()
            });

            pass.set_bind_group(0, &self.camera_bind_group, &[]);

            // World HDRI background: a fullscreen sky behind everything (depth
            // Always, no write) — opaque geometry below overdraws it. Only in
            // Beauty mode, when a map is loaded and the background is enabled.
            if self.world_loaded
                && scene.world.show_background
                && settings.mode == ViewportMode::Beauty
            {
                pass.set_pipeline(&self.sky_pipeline);
                pass.set_bind_group(1, &self.world_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }

            pass.set_bind_group(1, &light_bg, &[]);
            pass.set_bind_group(2, &self.world_bind_group, &[]); // mesh IBL ambient

            // Wireframe → line pipeline (no pre-pass). Else if the pre-pass ran, test
            // Equal (once per visible pixel); otherwise the plain Less+write pipeline
            // against the depth cleared this pass.
            let mesh_pipe = if settings.mode == ViewportMode::Wireframe {
                self.mesh_wire_pipeline.as_ref().unwrap_or(&self.mesh_pipeline)
            } else if use_prepass {
                &self.mesh_depth_equal
            } else {
                &self.mesh_pipeline
            };
            pass.set_pipeline(mesh_pipe);
            pass.set_vertex_buffer(0, self.floor_mesh.vertex_buffer.slice(..));
            pass.set_vertex_buffer(1, self.floor_instances.buffer.slice(..));
            pass.draw(0..self.floor_mesh.vertex_count, 0..1);

            pass.set_vertex_buffer(1, self.fixture_instances.buffer.slice(..));
            for (geometry, start, count) in &ranges {
                let m = self.mesh_for(*geometry);
                pass.set_vertex_buffer(0, m.vertex_buffer.slice(..));
                pass.draw(0..m.vertex_count, *start..*start + *count);
            }

            // GDTF fixture model parts (each part is one instance).
            // GDTF fixture-model parts — ONE instanced draw per unique part (shared
            // across every fixture of that GDTF type) instead of one per part per fixture.
            if !gdtf_groups.is_empty() {
                pass.set_vertex_buffer(1, self.gdtf_instances.buffer.slice(..));
                for (key, model, base, count) in &gdtf_groups {
                    if let Some(mesh) = self.gdtf_cache.get(key).and_then(|m| m.get(model)) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *base..*base + *count);
                    }
                }
            }

            // Imported MVR static geometry — ONE instanced draw per unique mesh (truss
            // segments, deck tiles, set pieces are overwhelmingly repeats). Off-screen
            // instances are GPU-clipped (vertex-only); the per-instance camera-frustum
            // cull is dropped here since the draw-call collapse is the real win (a future
            // GPU compute cull can prune off-screen instances before the draw).
            if !scene_geom_groups.is_empty() {
                pass.set_vertex_buffer(1, self.scene_geom_instances.buffer.slice(..));
                for (key, base, count) in &scene_geom_groups {
                    if let Some(Some(mesh)) = self.scene_geom_cache.get(key) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *base..*base + *count);
                    }
                }
            }

            // Placeholder cones for GDTF fixtures with no baked model.
            if gdtf_placeholder_count > 0 {
                pass.set_vertex_buffer(0, self.cone_mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.gdtf_placeholder_instances.buffer.slice(..));
                pass.draw(0..self.cone_mesh.vertex_count, 0..gdtf_placeholder_count);
            }

            // LED video-wall surfaces: one emissive quad per screen, each binding
            // its own content texture (procedural walls bind the placeholder).
            // Opaque walls draw first (REPLACE, write depth) so beams behind them
            // are occluded; transparent / mesh walls draw after with premultiplied
            // alpha + NO depth write, so the scene shows through their gaps.
            if !wall_draws.is_empty() {
                pass.set_vertex_buffer(0, self.quad_mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.wall_instances.buffer.slice(..));
                // Pass A: opaque walls (REPLACE, write depth).
                pass.set_pipeline(&self.wall_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                for (j, &(si, transparent)) in wall_draws.iter().enumerate() {
                    if transparent {
                        continue;
                    }
                    let bg = self.screen_runtime.get(&si).map(|r| &r.bind_group).unwrap_or(&self.wall_placeholder_bg);
                    pass.set_bind_group(1, bg, &[]);
                    pass.draw(0..self.quad_mesh.vertex_count, j as u32..j as u32 + 1);
                }
                // Pass B: transparent / mesh walls (premultiplied alpha, no depth write).
                pass.set_pipeline(&self.wall_alpha_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                for (j, &(si, transparent)) in wall_draws.iter().enumerate() {
                    if !transparent {
                        continue;
                    }
                    let bg = self.screen_runtime.get(&si).map(|r| &r.bind_group).unwrap_or(&self.wall_placeholder_bg);
                    pass.set_bind_group(1, bg, &[]);
                    pass.draw(0..self.quad_mesh.vertex_count, j as u32..j as u32 + 1);
                }
            }

            // Glass/dust front lenses (one disc per fixture, camera-only pipeline).
            if lens_count > 0 {
                pass.set_pipeline(&self.lens_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                pass.set_vertex_buffer(0, self.disc_mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.lens_instances.buffer.slice(..));
                pass.draw(0..self.disc_mesh.vertex_count, 0..lens_count);
            }

            pass.set_pipeline(&self.line_pipeline);
            if settings.show_grid {
                pass.set_vertex_buffer(0, self.grid_mesh.vertex_buffer.slice(..));
                pass.draw(0..self.grid_mesh.vertex_count, 0..1);
            }
            // The fog-box border + gizmos (dynamic_lines) — drawn for the live view,
            // suppressed for a clean still render (settings.show_gizmos).
            if line_count > 0 && settings.show_gizmos {
                pass.set_vertex_buffer(0, self.dynamic_lines.buffer.slice(..));
                pass.draw(0..line_count, 0..1);
            }
        }

        // Pass 1.25: SELECTION MASK — mark every selected object's VISIBLE surface
        // into the R8 sel_mask (1 = selected, depth-tested against the scene depth so
        // an occluded part doesn't mark it), so the composite below can edge-detect a
        // true per-object silhouette outline. Re-issues the SAME opaque draw groups as
        // the forward pass (fixtures, GDTF parts, MVR geometry, placeholder cones, LED
        // walls); the mask shader discards non-selected fragments, so the draws stay
        // identical to the forward pass — only the marked subset survives. Skipped
        // entirely when nothing is selected (the common case → zero added cost).
        let any_selected = !selection.fixtures.is_empty()
            || !selection.geometry.is_empty()
            || !selection.screens.is_empty();
        if any_selected {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sel-mask-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.viewport.sel_mask_view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                // Depth-TEST (LessEqual) against the forward pass's depth, NO write.
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: self.viewport.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                ..Default::default()
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_pipeline(&self.sel_mask_mesh_pipeline);
            // Fixtures with a simple primitive body.
            pass.set_vertex_buffer(1, self.fixture_instances.buffer.slice(..));
            for (geometry, start, count) in &ranges {
                let m = self.mesh_for(*geometry);
                pass.set_vertex_buffer(0, m.vertex_buffer.slice(..));
                pass.draw(0..m.vertex_count, *start..*start + *count);
            }
            // GDTF fixture model parts.
            if !gdtf_groups.is_empty() {
                pass.set_vertex_buffer(1, self.gdtf_instances.buffer.slice(..));
                for (key, model, base, count) in &gdtf_groups {
                    if let Some(mesh) = self.gdtf_cache.get(key).and_then(|m| m.get(model)) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *base..*base + *count);
                    }
                }
            }
            // Imported MVR static geometry.
            if !scene_geom_groups.is_empty() {
                pass.set_vertex_buffer(1, self.scene_geom_instances.buffer.slice(..));
                for (key, base, count) in &scene_geom_groups {
                    if let Some(Some(mesh)) = self.scene_geom_cache.get(key) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *base..*base + *count);
                    }
                }
            }
            // Placeholder cones for GDTF fixtures with no baked model.
            if gdtf_placeholder_count > 0 {
                pass.set_vertex_buffer(0, self.cone_mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.gdtf_placeholder_instances.buffer.slice(..));
                pass.draw(0..self.cone_mesh.vertex_count, 0..gdtf_placeholder_count);
            }
            // LED screens (WallInstance layout; selected = look.w).
            if !wall_draws.is_empty() {
                pass.set_pipeline(&self.sel_mask_wall_pipeline);
                pass.set_vertex_buffer(0, self.quad_mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.wall_instances.buffer.slice(..));
                pass.draw(0..self.quad_mesh.vertex_count, 0..wall_draws.len() as u32);
            }
        }

        // Pass 1.5: SSAO (Unlit mode) — multiply a depth-based occlusion factor
        // onto the otherwise-flat HDR so geometry gains contact/crevice shading.
        if settings.mode == ViewportMode::Unlit {
            let focal_px = self.viewport.size.1 as f32 * 0.5 / (camera.fov_y * 0.5).tan();
            let ao = AoUniform {
                // near, far, world-radius (~0.6 m) in px at 1 m, intensity.
                params: [camera.znear, camera.zfar, 0.6 * focal_px, 2.1],
            };
            self.queue.write_buffer(&self.ao_uniform, 0, bytemuck::bytes_of(&ao));
            let ao_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ssao-bg"),
                layout: &self.ssao_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(self.viewport.depth_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.ao_uniform.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ssao-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.viewport.hdr_view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: self.ts_rp(gpu_timer::P_SSAO),
                ..Default::default()
            });
            pass.set_pipeline(&self.ssao_pipeline);
            pass.set_bind_group(0, &ao_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Pass 2a: volumetric raymarch -> half-res vol target (scatter, transmit).
        // Pass 2b: upsample + composite into the HDR scene.
        if has_fog && settings.mode == ViewportMode::Beauty {
            // HYBRID stage 1 — the froxel volume carries the wide/dim "masses"
            // (all non-hero beams) at a cost decoupled from beam count, with no
            // dither/banding and full mid-air occlusion. inject → integrate →
            // trilinear composite into HDR.
            if let (Some((inject_bg, integrate_bg, comp_bg)), Some(fx)) =
                (&froxel_bgs, self.froxel.as_ref())
            {
                let gx = fx.dims.0.div_ceil(8);
                let gy = fx.dims.1.div_ceil(8);
                {
                    let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("froxel-inject"),
                        timestamp_writes: None,
                    });
                    cpass.set_pipeline(&fx.inject_pipeline);
                    cpass.set_bind_group(0, inject_bg, &[]);
                    cpass.dispatch_workgroups(gx, gy, fx.dims.2);
                }
                {
                    let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("froxel-integrate"),
                        timestamp_writes: None,
                    });
                    cpass.set_pipeline(&fx.integrate_pipeline);
                    cpass.set_bind_group(0, integrate_bg, &[]);
                    cpass.dispatch_workgroups(gx, gy, 1);
                }
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("froxel-composite-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: self.viewport.hdr_view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });
                pass.set_pipeline(&fx.composite_pipeline);
                pass.set_bind_group(0, comp_bg, &[]);
                pass.draw(0..3, 0..1);
            }

            // HYBRID stage 2 — the per-pixel raymarch lays the SHARP hero shafts
            // over the froxel masses (in hybrid mode the shader skips non-heroes,
            // so it only marches the few sharpest beams = crisp gobo/CA/prism
            // detail at low cost). In raymarch-only mode this marches every beam.
            // Skipped entirely in hybrid when there are no hero beams.
            if !use_froxel || n_shadows > 0 {
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("volumetric-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: self.viewport.vol_view(),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: self.ts_rp(gpu_timer::P_VOL),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.volumetric_pipeline);
                    pass.set_bind_group(0, &volumetric_bg, &[]);
                    pass.draw(0..3, 0..1);
                }
                // Temporal resolve: blend the raw raymarch (vol) with the reprojected
                // previous EMA into vol_ema[cur]; the composite then reads that. This is
                // what converges the per-frame jitter into a smooth, uniform beam.
                if let Some(temporal_bg) = &temporal_bg {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("vol-temporal-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: self.viewport.vol_ema_view(cur_ema),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: self.ts_rp(gpu_timer::P_VOLTEMP),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.vol_temporal_pipeline);
                    pass.set_bind_group(0, temporal_bg, &[]);
                    pass.draw(0..3, 0..1);
                }
                // Composite (blend One/SrcAlpha) is configured on the pipeline.
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("composite-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: self.viewport.hdr_view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: self.ts_rp(gpu_timer::P_COMPOSITE),
                    ..Default::default()
                });
                pass.set_pipeline(&self.composite_pipeline);
                pass.set_bind_group(0, &composite_bg, &[]);
                pass.draw(0..3, 0..1);
            }
        }

        // Pass 2.9: SELECTION OUTLINE — edge-detect the sel_mask and ADD a bright
        // amber silhouette ring into the HDR (blend One/One), BEFORE bloom so the
        // outline glows slightly. Only when something selectable is selected.
        if any_selected {
            let outline_bg = self.single_tex_bg(self.viewport.sel_mask_view());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sel-outline-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.viewport.hdr_view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                ..Default::default()
            });
            pass.set_pipeline(&self.sel_outline_pipeline);
            pass.set_bind_group(0, &outline_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Pass 3: bloom bright-pass (HDR -> bloom[0]).
        self.fullscreen(encoder, "bloom-bright", &self.bloom_bright, self.viewport.bloom_view(0), &bright_bg, self.ts_rp(gpu_timer::P_BLOOM_BRIGHT));
        // Pass 4: separable blur (bloom[0] -> bloom[1] -> bloom[0]).
        self.fullscreen(encoder, "bloom-blur-h", &self.bloom_blur_h, self.viewport.bloom_view(1), &blur_h_bg, self.ts_rp(gpu_timer::P_BLOOM_H));
        self.fullscreen(encoder, "bloom-blur-v", &self.bloom_blur_v, self.viewport.bloom_view(0), &blur_v_bg, self.ts_rp(gpu_timer::P_BLOOM_V));
        // Pass 5: tonemap/resolve (HDR + bloom -> LDR, sRGB-encoded).
        self.fullscreen(encoder, "tonemap", &self.tonemap_pipeline, self.viewport.ldr_view(), &tonemap_bg, self.ts_rp(gpu_timer::P_TONEMAP));

        // Resolve this frame's per-pass GPU timestamps into the ring (read back async in
        // render()); skipped automatically if the ring slot is still being read.
        if let Some(t) = self.gpu_timers.as_ref() {
            t.resolve(encoder);
        }
        // "What's being rendered" counts for the perf overlay — populated every frame even
        // when GPU timestamps are unavailable (the bars come from the render() ring pump).
        self.last_timings.fixtures = scene.fixtures.len() as u32;
        self.last_timings.beams = gpu_fixtures.len() as u32;
        self.last_timings.shadow_maps = n_shadows as u32;
        self.last_timings.geom_draws =
            (ranges.len() + gdtf_groups.len() + scene_geom_groups.len()) as u32;
        self.last_timings.render_px = self.viewport.size;

        // Advance temporal-accumulation state for next frame. `ema_valid` reflects
        // whether the EMA was actually written this frame (so next frame knows whether
        // its `prev_ema` target holds real history or stale/zeroed content).
        self.prev_view_proj = cur_view_proj;
        self.prev_size = self.viewport.size;
        self.ema_valid = raymarch_ran;
        self.frame_index = self.frame_index.wrapping_add(1);
    }

    fn single_tex_bg(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("single-tex-bg"),
            layout: &self.single_tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                },
            ],
        })
    }

    /// Run a fullscreen-triangle pass writing `target`, clearing it first.
    fn fullscreen(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
        pipeline: &wgpu::RenderPipeline,
        target: &wgpu::TextureView,
        bind_group: &wgpu::BindGroup,
        ts: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: ts,
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Lightweight beam description for the wireframe indicator gizmo only.
struct BeamSpec {
    origin: Vec3,
    dir: Vec3,
    angle: f32,
    color: [f32; 3],
    intensity: f32,
}

fn beam_spec(fixture: &Fixture, frame: Option<fixture_model::BeamFrame>) -> BeamSpec {
    let (origin, dir) = match frame {
        Some(f) => (f.origin, f.dir),
        None => (fixture.lens_position(), fixture.beam_direction()),
    };
    // Show the indicator at the current (zoomed) angle for GDTF fixtures.
    let angle = match &fixture.gdtf {
        Some(g) => optics::map_attr(g, "Zoom", fixture.optics.zoom, (fixture.beam_angle, fixture.beam_angle)),
        None => fixture.beam_angle,
    };
    BeamSpec {
        origin,
        dir,
        angle,
        color: fixture.color,
        intensity: fixture.intensity,
    }
}

/// An orthonormal lens-plane basis `(right, up)` perpendicular to `dir`, kept
/// close to `hint_up` (falls back to a stable axis when `dir ‖ hint_up`).
fn ortho_basis(dir: Vec3, hint_up: Vec3) -> (Vec3, Vec3) {
    let mut right = hint_up.cross(dir);
    if right.length_squared() < 1e-6 {
        right = Vec3::X.cross(dir);
    }
    let right = right.normalize_or_zero();
    let up = dir.cross(right).normalize_or_zero();
    (right, up)
}

/// The GPU beams for a fixture plus its front-lens disc instances (one per
/// visible emitter — a Spiider contributes 19).
struct BeamBuild {
    beams: Vec<FixtureGpu>,
    lenses: Vec<LensInstance>,
}

/// Build a lens-disc instance facing `dir` at `origin`, radius `r`.
#[allow(clippy::too_many_arguments)]
fn lens_instance(
    origin: Vec3,
    dir: Vec3,
    right: Vec3,
    up: Vec3,
    r: f32,
    color: [f32; 3],
    level: f32,
    tan_half: f32,
    n_order: f32,
    candela: f32,
    shutter: [f32; 4],
) -> LensInstance {
    let model = Mat4::from_cols(
        (right * r).extend(0.0),
        (up * r).extend(0.0),
        (dir * r).extend(0.0),
        origin.extend(1.0),
    );
    LensInstance {
        model: model.to_cols_array_2d(),
        color: [color[0], color[1], color[2], level],
        params: [tan_half, n_order, candela, r],
        shutter,
    }
}

/// Build the GPU beam + lens for a laser engine: a thin, near-collimated streak.
/// `misc.y = 1` switches the shaders to no inverse-square falloff + razor edge +
/// Tyndall boost (visible only in haze). A physically razor-thin laser (mm core)
/// under-samples in the half-res raymarch and breaks into speckle, so the
/// rendered core is a few cm with a soft super-Gaussian edge — wide enough to
/// read as a continuous streak — while a tiny divergence keeps it collimated.
fn build_laser(
    f: &fixture_model::BeamFrame,
    fixture: &Fixture,
    atlas_layers: f32,
) -> BeamBuild {
    const LASER_RANGE: f32 = 80.0;
    const LASER_CORE: f32 = 0.11;
    let i = (fixture.intensity * fixture.optics.dimmer).max(0.0);
    let tan_half = (fixture.beam_angle * 0.5).to_radians().tan().clamp(2.5e-3, 0.02);
    let g = i * 1.2; // HDR gain; the no-falloff streak keeps it bright far out
    BeamBuild {
        beams: if i < 1e-4 {
            Vec::new()
        } else {
            vec![FixtureGpu {
                pos_range: [f.origin.x, f.origin.y, f.origin.z, LASER_RANGE],
                dir_cos: [f.dir.x, f.dir.y, f.dir.z, tan_half],
                color: [fixture.color[0] * g, fixture.color[1] * g, fixture.color[2] * g, LASER_CORE],
                cookie_r: [f.right.x, f.right.y, f.right.z, 0.0], // wheel offset 0
                cookie_u: [f.up.x, f.up.y, f.up.z, 0.0],          // wheel count 0 (no wheels)
                extra: [-1.0, 0.0, 0.0, 0.0], // anim layer = none
                shape: [4.0, 8.0, 1.0, 0.0], // soft-ish edge → survives downsampling
                misc: [0.0, 1.0, atlas_layers, -1.0], // misc.y = laser flag
                cmyf: [0.0, 0.0, 0.0, 0.0],
            }]
        },
        lenses: vec![lens_instance(
            f.origin + f.dir * 0.02,
            f.dir,
            f.right,
            f.up,
            LASER_CORE * 1.5,
            fixture.color,
            i,
            tan_half,
            6.0,
            8.0,
            [0.0; 4], // lasers have no mechanical shutter blade
        )],
    }
}

/// Resolve a fixture's optical chain into the GPU beam(s) the shaders consume
/// plus the front-lens discs. A single-emitter fixture yields one beam (or one
/// per facet when a prism is engaged); a multi-emitter fixture yields one beam
/// per lit cell — or ONE aggregated beam when the array is uniform (the common
/// wash case), keeping the raymarch loop off the per-pixel cliff. `key` is the
/// GDTF Arc pointer; `time` drives strobe.
fn build_beam_gpus(
    fixture: &Fixture,
    frames: &[fixture_model::BeamFrame],
    key: usize,
    atlas: &atlas::GoboAtlas,
    time: f32,
    wheels_out: &mut Vec<WheelGpu>,
) -> BeamBuild {
    let atlas_layers = atlas.layer_count() as f32;
    const RANGE: f32 = 40.0;

    let fallback_frame = || {
        let m = fixture.model_matrix();
        fixture_model::BeamFrame {
            origin: fixture.lens_position(),
            dir: fixture.beam_direction(),
            right: m.transform_vector3(Vec3::X).normalize_or_zero(),
            up: m.transform_vector3(Vec3::Z).normalize_or_zero(),
        }
    };

    // Laser engines render the same way whether or not they carry a GDTF model
    // (a `LampType="Laser"` GDTF still aims its <Beam> geometry) — handle them
    // first so the flag isn't dead for imported lasers.
    if fixture.is_laser {
        let f = frames.first().copied().unwrap_or_else(fallback_frame);
        return build_laser(&f, fixture, atlas_layers);
    }

    let Some(gdtf) = &fixture.gdtf else {
        let f = fallback_frame();
        let i = (fixture.intensity * fixture.optics.dimmer).max(0.0);
        // Placeholder (non-GDTF) fixture: a plain super-Gaussian cone, no cookie.
        let tan_half = (fixture.beam_angle * 0.5).to_radians().tan().max(1e-3);
        return BeamBuild {
            // A blacked-out fixture emits nothing, so it must not cost anything in the
            // per-step raymarch / floor-pool loops — emit no beam. (The lens instance
            // is still built; it's dark glass.) Big win for "patch the whole rig,
            // light a few" scenes, where most fixtures sit at intensity 0.
            beams: if i < 1e-4 {
                Vec::new()
            } else {
                vec![FixtureGpu {
                    pos_range: [f.origin.x, f.origin.y, f.origin.z, RANGE],
                    dir_cos: [f.dir.x, f.dir.y, f.dir.z, tan_half],
                    color: [fixture.color[0] * i, fixture.color[1] * i, fixture.color[2] * i, Fixture::BODY_RADIUS],
                    cookie_r: [f.right.x, f.right.y, f.right.z, 0.0], // wheel offset 0
                    cookie_u: [f.up.x, f.up.y, f.up.z, 0.0],          // wheel count 0 (no wheels)
                    extra: [-1.0, 0.0, 0.0, 0.0], // anim layer = none
                    shape: [6.0, 8.0, 1.0, 0.0],
                    misc: [0.0, 0.0, atlas_layers, -1.0],
                    cmyf: [0.0, 0.0, 0.0, 0.0],
                }]
            },
            lenses: vec![lens_instance(
                f.origin + f.dir * 0.02,
                f.dir,
                f.right,
                f.up,
                Fixture::BODY_RADIUS * 0.95,
                fixture.color,
                i,
                tan_half,
                6.0,
                1.0,
                [0.0; 4],
            )],
        };
    };

    let o = optics::resolve(gdtf, fixture.mode_index, &fixture.optics, &fixture.motion, time);
    let emitters = fixture.emitters();

    // Dynamic wheel chain: every engaged gobo/colour wheel becomes a WheelGpu in
    // the global buffer; this fixture's beams reference the contiguous slice
    // [wheel_off, wheel_off + wheel_count). Wheels not present in the atlas (e.g.
    // a prism, or one that didn't bake) are simply dropped.
    let mut my_wheels: Vec<WheelGpu> = Vec::with_capacity(o.wheels.len());
    for s in &o.wheels {
        if let Some(base) = atlas.base_layer(key, &s.wheel) {
            my_wheels.push(WheelGpu {
                d: [base as f32, s.position, s.n_slots, s.gap],
                m: [if s.is_color { 1.0 } else { 0.0 }, s.rot, 0.0, 0.0],
            });
        }
    }
    let wheel_off = wheels_out.len() as f32;
    let wheel_count = my_wheels.len() as f32;
    // A patterned cookie (gobo image or animation glass) makes lateral CA read as
    // wild colour fringing on every gobo edge; on an open/colour beam it's the
    // pleasing two-sided rim. So damp CA hard when a pattern is present, keep it
    // full otherwise. (Colour wheels are solid → no fine detail → not "patterned".)
    // CA damping keys on a REAL gobo (gobo_engaged), not "a gobo wheel is present"
    // — the disc is always emitted now, so an open beam must still get full CA.
    let has_pattern = o.anim.is_some() || o.gobo_engaged;
    let ca_strength = if has_pattern { o.ca_strength * 0.18 } else { o.ca_strength };
    wheels_out.extend(my_wheels);
    let cmyf = [o.cmy[0], o.cmy[1], o.cmy[2], 0.0];
    let (anim_layer, anim_scroll) = match &o.anim {
        // Slot 0 = open (white), slot 1 = the animation glass.
        Some((wheel, scroll)) => match atlas.base_layer(key, wheel) {
            Some(base) => ((base + 1) as f32, *scroll),
            None => (-1.0, 0.0),
        },
        None => (-1.0, 0.0),
    };

    // "The dimmer IS the shutter." On a blade fixture the mechanical blade does
    // the dim AND the strobe (one mechanism), so the blade close tracks dimmer ×
    // shutter and the uniform `level` carries only the master intensity — the blade
    // (in the cookie + on the lens) provides the actual attenuation. On a plain
    // fixture the dimmer/shutter is a uniform multiply and there's no blade.
    let master = fixture.intensity.max(0.0);
    let open = (fixture.optics.dimmer.max(0.0) * o.shutter_gain).clamp(0.0, 1.0);
    let effective = master * open; // true emitted brightness (used as the skip gate)
    let shutter_kind = fixture.shutter.code();
    let blade = shutter_kind > 0.5;
    let level = if blade { master } else { effective };
    let lens_level = level;
    let shutter_close = if blade { 1.0 - open } else { 0.0 };
    // Blade edge blur: heavy by default (a real dimmer-blade is far out of focus →
    // near-perfect smooth dimming), growing with focus error + frost. cmyf.w per beam.
    let focus_defocus = (fixture.optics.focus - 0.5).abs() * 2.0;

    // `sc`/`sk` = the blade close + kind for THIS beam. The single-emitter path
    // passes the real blade (its `level` is undimmed master, so the blade does the
    // dim); the multi-emitter path passes 0 (its colour is already pre-dimmed by
    // `effective`, so a blade would double-dim).
    let make = |frame: &fixture_model::BeamFrame,
                bdir: Vec3,
                r: Vec3,
                u: Vec3,
                col: [f32; 3],
                tan_half: f32,
                n_order: f32,
                lens_r: f32,
                sc: f32,
                sk: f32| FixtureGpu {
        pos_range: [frame.origin.x, frame.origin.y, frame.origin.z, RANGE],
        dir_cos: [bdir.x, bdir.y, bdir.z, tan_half],
        color: [col[0], col[1], col[2], lens_r],
        cookie_r: [r.x, r.y, r.z, wheel_off],
        cookie_u: [u.x, u.y, u.z, wheel_count],
        extra: [anim_layer, anim_scroll, sc, sk],
        shape: [n_order, o.focus_dist, o.iris, o.frost],
        misc: [ca_strength, 0.0, atlas_layers, -1.0], // misc.w = shadow layer (-1 = none)
        // cmyf.w = shutter-blade edge softness: crisp on a narrow beam (the gate
        // images sharply on a beam fixture), blurred out on a wide wash.
        cmyf: [cmyf[0], cmyf[1], cmyf[2], (0.45 + 0.5 * focus_defocus + 0.7 * o.frost + tan_half * 0.4).clamp(0.2, 1.3)],
    };

    // ----- single-emitter path (classic moving head; prism expansion) -----
    if emitters.len() <= 1 {
        let frame = frames.first().copied().unwrap_or_else(fallback_frame);
        let em_beam = emitters.first().map(|e| &e.beam).unwrap_or(&gdtf.beam);
        let flux_norm =
            (optics::FIXTURE_FLUX_CAP / optics::emitter_flux(em_beam, 1)).min(1.0);
        let cone = optics::emitter_cone(gdtf, em_beam, &fixture.optics, o.frost, 1, flux_norm);
        let lens_radius = em_beam.beam_radius.max(0.02);
        let cell = fixture.cells.first().copied().unwrap_or([1.0, 1.0, 1.0]);
        let tint = [
            o.tint[0] * cell[0] * fixture.color[0],
            o.tint[1] * cell[1] * fixture.color[1],
            o.tint[2] * cell[2] * fixture.color[2],
        ];
        // Per-fixture volumetric beam intensity scales the projected shaft + pool
        // (the beam colour), NOT the lens face (which still shows the source lit).
        let scale = level * cone.brightness * fixture.beam.max(0.0);
        let base_color = [tint[0] * scale, tint[1] * scale, tint[2] * scale];
        let cell_lit = cell.iter().fold(0.0_f32, |a, &b| a.max(b));

        let beams = if effective * cell_lit < 1e-4 || !cone.shaft {
            Vec::new()
        } else if o.prism.is_empty() {
            vec![make(&frame, frame.dir, frame.right, frame.up, base_color, cone.tan_half, cone.n_order, lens_radius, shutter_close, shutter_kind)]
        } else {
            // Each facet is a separated aerial beam: deflect the axis, rebuild
            // its basis, split energy. While the prism is sliding in (prism_insert
            // < 1) a straight passthrough of weight 1−insert keeps the main beam —
            // the bleed during the move.
            let mut out: Vec<FixtureGpu> = o
                .prism
                .iter()
                .map(|p| {
                    let d = (frame.dir + frame.right * p.offset[0] + frame.up * p.offset[1]).normalize_or_zero();
                    let (r2, u2) = ortho_basis(d, frame.up);
                    let c = [base_color[0] * p.weight, base_color[1] * p.weight, base_color[2] * p.weight];
                    make(&frame, d, r2, u2, c, cone.tan_half, cone.n_order, lens_radius, shutter_close, shutter_kind)
                })
                .collect();
            let straight = (1.0 - o.prism_insert).clamp(0.0, 1.0);
            if straight > 0.01 {
                let c = [base_color[0] * straight, base_color[1] * straight, base_color[2] * straight];
                out.push(make(&frame, frame.dir, frame.right, frame.up, c, cone.tan_half, cone.n_order, lens_radius, shutter_close, shutter_kind));
            }
            out
        };

        // Physical front lens at the beam exit, tinted by the colour chain. The
        // shutter blade shows on the lens even when open (a thin parked sliver) —
        // the mechanism lives at the gate; crisp here (it's right at the glass).
        let lens_shutter = if shutter_kind > 0.5 {
            // Lens-face blade: a touch sharper than the projected beam (it's right
            // at the glass) but still blurs with frost / focus error.
            [shutter_close, shutter_kind, (0.12 + 0.4 * focus_defocus + 0.5 * o.frost).clamp(0.08, 1.0), 0.0]
        } else {
            [0.0; 4]
        };
        // The beam applies the CMY flags spatially, so `tint` excludes them; the
        // lens face is a single small disc, so fold the average CMY transmittance
        // into its colour here (otherwise the glass would read un-tinted).
        let cmy_t = optics::color::cmy_transmittance(o.cmy);
        let lens_tint = [tint[0] * cmy_t[0], tint[1] * cmy_t[1], tint[2] * cmy_t[2]];
        let lenses = vec![lens_instance(
            frame.origin + frame.dir * 0.04,
            frame.dir,
            frame.right,
            frame.up,
            lens_radius * 1.25,
            lens_tint,
            lens_level * cell_lit.min(1.0),
            cone.tan_half,
            cone.n_order,
            cone.face_gain,
            lens_shutter,
        )];
        return BeamBuild { beams, lenses };
    }

    // ----- multi-emitter path (LED arrays / wash heads / pixel bars) -----
    let mut beams: Vec<FixtureGpu> = Vec::new();
    let mut lenses: Vec<LensInstance> = Vec::with_capacity(emitters.len());

    struct Cell {
        frame: fixture_model::BeamFrame,
        color: [f32; 3], // beam color, fully scaled
        tint: [f32; 3],  // lens face color (unscaled chain tint × cell)
        /// Brightest channel of the raw cell value (0 = cell commanded dark).
        cell_max: f32,
        /// Achromatic-white level = the cell's MIN raw channel (white = 1, any
        /// saturated colour = 0). Drives the HDR shaft whiten/boost so a bright
        /// WHITE cell punches a brighter, whiter shaft while a saturated blue cell
        /// stays blue. (Min, not max: a full blue [0,0,1] has max 1 but is NOT white.)
        white: f32,
        lit: f32,
        cone: optics::EmitterCone,
        lens_r: f32,
    }
    // Fixture-total flux cap: GDTF pixel files often duplicate group flux onto
    // every cell — normalise so the whole array sums to a plausible fixture.
    let total_flux: f32 = emitters
        .iter()
        .filter(|e| e.merged_into.is_none())
        .map(|e| optics::emitter_flux(&e.beam, emitters.len()))
        .sum();
    let flux_norm = (optics::FIXTURE_FLUX_CAP / total_flux.max(1.0)).min(1.0);

    let mut cells: Vec<Cell> = Vec::new();
    for (i, em) in emitters.iter().enumerate() {
        // An occluded emitter (fires through another's aperture) was HTP-merged
        // into its front cell by the decode; draw nothing for it.
        if em.merged_into.is_some() {
            continue;
        }
        let Some(frame) = frames.get(i).copied() else {
            continue;
        };
        let cell = fixture.cells.get(i).copied().unwrap_or([1.0, 1.0, 1.0]);
        let cone =
            optics::emitter_cone(gdtf, &em.beam, &fixture.optics, o.frost, emitters.len(), flux_norm);
        let tint = [
            o.tint[0] * cell[0] * fixture.color[0],
            o.tint[1] * cell[1] * fixture.color[1],
            o.tint[2] * cell[2] * fixture.color[2],
        ];
        // Multi-emitter cells carry no cookie blade, so they dim uniformly by the
        // effective level (not the full master that the single-emitter blade path
        // uses and then attenuates with the blade).
        let scale = effective * cone.brightness * fixture.beam.max(0.0);
        let cell_max = cell.iter().fold(0.0_f32, |a, &b| a.max(b));
        let cell_white = cell.iter().fold(f32::INFINITY, |a, &b| a.min(b)).max(0.0);
        cells.push(Cell {
            frame,
            color: [tint[0] * scale, tint[1] * scale, tint[2] * scale],
            tint,
            cell_max,
            white: cell_white,
            lit: cell_max * effective,
            cone,
            lens_r: em.beam.beam_radius.max(0.01),
        });
    }

    // Lens faces: every visible cell, lit or dark (dark = glass).
    for c in &cells {
        lenses.push(lens_instance(
            c.frame.origin + c.frame.dir * 0.006,
            c.frame.dir,
            c.frame.right,
            c.frame.up,
            c.lens_r,
            c.tint,
            (effective * c.cell_max).min(1.0), // no blade here → uniform dim
            c.cone.tan_half,
            c.cone.n_order,
            c.cone.face_gain,
            [0.0; 4], // multi-emitter washes: no framing blade on each cell
        ));
    }

    // Beams: skip dark cells, then cluster the rest by direction (a Spiider is
    // one parallel cluster; a multi-tube blinder is several). A uniform cluster
    // of ≥4 collapses to ONE wide disc beam (sum of cell outputs — exact in the
    // far field where the cell cones overlap; dense-array areas match near the
    // face). Non-uniform (pixel-mapped) clusters stay per-cell — until the
    // fixture would exceed its beam budget, where a lossy direction-cone LOD
    // bounds the volumetric cost (the raymarch is O(px·steps·beams); one 72-px
    // omnidirectional blinder must not cost 72 wide cones).
    let lit: Vec<&Cell> = cells.iter().filter(|c| c.lit > 1e-4 && c.cone.shaft).collect();
    if lit.is_empty() {
        return BeamBuild { beams, lenses };
    }
    let no_cookie = wheel_count < 0.5
        && anim_layer < 0.0
        && o.cmy.iter().all(|&v| v < 0.005)
        && o.prism.is_empty();
    // A "plain" cell beam carries no gobo/animation/CMY/shutter-blade, so the GPU
    // can skip the whole projected-cookie chain (the dominant cost for dense pixel
    // bars). Flagged on the GPU beam with extra.z = -1 (a sentinel that can't
    // collide with a real shutter_close ≥ 0). extra.w then carries the cell's peak
    // raw level so the shader whitens + boosts bright cells (accuracy: bright cells
    // punch distinct brighter/whiter shafts, dim coloured cells stay saturated).
    // Multi-emitter cells never apply the shutter BLADE (they pass sc/sk = 0 to
    // `make` and dim uniformly via `effective`), so the blade is moot here and
    // `plain` keys only on the cookie chain — a pixel bar with an electronic strobe
    // is still plain.
    let plain = no_cookie;
    // The cluster's whiteness = the whitest member (so a merged group containing a
    // bright white cell still punches a white shaft).
    let cluster_white = |cl: &[&Cell]| cl.iter().map(|c| c.white).fold(0.0_f32, f32::max);
    // Stamp the plain flag + whiteness, and zero CA (a wash cell has no lens
    // chromatic aberration — and dropping it collapses opt_radial_ca's 3 pow()s to
    // 1), on every multi-emitter beam before it goes to the GPU.
    let finish = |mut b: FixtureGpu, white: f32| -> FixtureGpu {
        if plain {
            b.extra[2] = -1.0;
            b.extra[3] = white.clamp(0.0, 1.0);
        }
        b.misc[0] = 0.0;
        b
    };
    let cluster_by = |lit: &[&'_ Cell], min_dot: f32| -> Vec<Vec<usize>> {
        let mut out: Vec<(Vec3, Vec<usize>)> = Vec::new();
        for (i, c) in lit.iter().enumerate() {
            match out.iter_mut().find(|(d, _)| d.dot(c.frame.dir) > min_dot) {
                Some((_, v)) => v.push(i),
                None => out.push((c.frame.dir, vec![i])),
            }
        }
        out.into_iter().map(|(_, v)| v).collect()
    };
    // One merged beam covering a cluster: centroid origin, mean direction
    // widened by the member spread, summed output.
    let aggregate = |cl: &[&Cell]| -> FixtureGpu {
        let mean_dir = cl
            .iter()
            .fold(Vec3::ZERO, |a, c| a + c.frame.dir)
            .normalize_or_zero();
        let centroid = cl.iter().fold(Vec3::ZERO, |a, c| a + c.frame.origin) / cl.len() as f32;
        let f0 = &cl[0].frame;
        let (right, up) = ortho_basis(mean_dir, f0.up);
        let spread_r = cl
            .iter()
            .map(|c| {
                let rel = c.frame.origin - centroid;
                (rel - mean_dir * rel.dot(mean_dir)).length() + c.lens_r
            })
            .fold(0.0_f32, f32::max);
        let spread_ang = cl
            .iter()
            .map(|c| c.frame.dir.dot(mean_dir).clamp(-1.0, 1.0).acos())
            .fold(0.0_f32, f32::max);
        let tan_eff = (cl[0].cone.tan_half.atan() + spread_ang).tan().clamp(
            cl[0].cone.tan_half,
            3.7, // ~150° full cone cap
        );
        let color = cl.iter().fold([0.0_f32; 3], |a, c| {
            [a[0] + c.color[0], a[1] + c.color[1], a[2] + c.color[2]]
        });
        let n_order = if cl.len() > 1 {
            cl[0].cone.n_order.min(2.0)
        } else {
            cl[0].cone.n_order
        };
        let agg_frame = fixture_model::BeamFrame { origin: centroid, dir: mean_dir, right, up };
        make(&agg_frame, mean_dir, right, up, color, tan_eff, n_order, spread_r.max(cl[0].lens_r), 0.0, 0.0)
    };

    // Per-cell SHAFT cone: a real LED cell images far tighter than its broad
    // scatter wash, so for the volumetric shaft (NOT the lens face, which keeps the
    // soft wide source disc) tighten the beam angle and force a crisp super-Gaussian
    // shoulder. This is what makes a pixel map read as DISTINCT coloured beams
    // (yellow vs blue, on vs off) instead of a merged grey blob — and, decisively,
    // the tighter/crisper cone lets the radial pre-cull actually reject the other
    // cells at each ray sample (the old soft 42°-field spill is exactly why every
    // sample fell inside every cell's cone → O(all cells) → the 4 fps wall).
    // Narrow the shaft to ~70% of the cell's beam angle (the user's bars read a
    // touch too wide) and floor the shoulder at a crisp n=3 (≥2 also zeroes the
    // cull-widening term, so this is where the per-cell cull speed-up comes from).
    const SHAFT_NARROW: f32 = 0.72;
    const SHAFT_N_ORDER: f32 = 3.0;
    let shaft_cone = |c: &Cell| -> (f32, f32) {
        (c.cone.tan_half * SHAFT_NARROW, c.cone.n_order.max(SHAFT_N_ORDER))
    };

    const MAX_FIXTURE_BEAMS: usize = 24;
    if lit.len() > MAX_FIXTURE_BEAMS {
        // Bounded-cost LOD for a huge array (e.g. a 72-cell blinder / LED wall):
        // coarse direction-cone merge so the raymarch can't pay for hundreds of
        // beams. Loses per-cell colour in the shaft, but the per-cell lens faces
        // above still carry the pixel-mapped detail on the source.
        let coarse = cluster_by(&lit, 0.906);
        for cl in &coarse {
            let cs: Vec<&Cell> = cl.iter().map(|&i| lit[i]).collect();
            beams.push(finish(aggregate(&cs), cluster_white(&cs)));
        }
    } else {
        // The common case (bars, washes, clusters ≤ 24 lit cells): every lit cell
        // is its OWN crisp shaft, so the pixel map is faithful and each beam culls
        // tightly. No merging — merging co-located cells into one wide cone was
        // both the blob look AND a perf trap (the wide cone never culls).
        for c in &lit {
            let (tan_half, n_order) = shaft_cone(c);
            let b = make(
                &c.frame,
                c.frame.dir,
                c.frame.right,
                c.frame.up,
                c.color,
                tan_half,
                n_order,
                c.lens_r,
                0.0,
                0.0,
            );
            beams.push(finish(b, c.white));
        }
    }
    BeamBuild { beams, lenses }
}

/// World-space AABB of a local AABB transformed by `m` (8-corner bound).
fn transform_aabb(m: &Mat4, lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    let mut wlo = Vec3::splat(f32::INFINITY);
    let mut whi = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8u32 {
        let c = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let w = m.transform_point3(c);
        wlo = wlo.min(w);
        whi = whi.max(w);
    }
    (wlo, whi)
}

/// True if the world AABB is fully outside `vp`'s clip volume (conservative;
/// wgpu clip z ∈ [0, w]) — i.e. the draw can be skipped for this view. Retained for
/// the planned GPU compute frustum-cull of instanced static geometry (the forward
/// per-instance CPU cull was removed in favour of one instanced draw per mesh).
#[allow(dead_code)]
fn aabb_outside_clip(vp: &Mat4, lo: Vec3, hi: Vec3) -> bool {
    let (mut nx, mut px, mut ny, mut py, mut nz, mut pz) = (true, true, true, true, true, true);
    for i in 0..8u32 {
        let c = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let p = *vp * c.extend(1.0);
        if p.x >= -p.w {
            nx = false;
        }
        if p.x <= p.w {
            px = false;
        }
        if p.y >= -p.w {
            ny = false;
        }
        if p.y <= p.w {
            py = false;
        }
        if p.z >= 0.0 {
            nz = false;
        }
        if p.z <= p.w {
            pz = false;
        }
    }
    nx || px || ny || py || nz || pz
}

/// Projected size (in pixels) of a world AABB under `vp` rendered to a `res`²
/// target, or `None` if fully outside the clip volume. Drives the shadow-caster
/// LOD: a big silhouette (a performer filling the beam) projects large and casts;
/// a distant / tiny audience mesh projects sub-pixel and is skipped (its shadow
/// would be invisible). An object spanning the near plane returns `res` (always
/// cast) so we never wrongly drop a close occluder.
fn clip_proj_px(vp: &Mat4, lo: Vec3, hi: Vec3, res: f32) -> Option<f32> {
    let (mut nx, mut px, mut ny, mut py, mut nz, mut pz) = (true, true, true, true, true, true);
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut spans_near = false;
    for i in 0..8u32 {
        let c = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let p = *vp * c.extend(1.0);
        if p.x >= -p.w {
            nx = false;
        }
        if p.x <= p.w {
            px = false;
        }
        if p.y >= -p.w {
            ny = false;
        }
        if p.y <= p.w {
            py = false;
        }
        if p.z >= 0.0 {
            nz = false;
        }
        if p.z <= p.w {
            pz = false;
        }
        if p.w <= 1e-4 {
            spans_near = true;
        } else {
            let (ndx, ndy) = (p.x / p.w, p.y / p.w);
            minx = minx.min(ndx);
            maxx = maxx.max(ndx);
            miny = miny.min(ndy);
            maxy = maxy.max(ndy);
        }
    }
    if nx || px || ny || py || nz || pz {
        return None;
    }
    if spans_near {
        return Some(res);
    }
    Some((maxx - minx).max(maxy - miny) * 0.5 * res)
}

/// Append a small RGB world-axes pivot marker at `p`, so the selected object's
/// position/orientation reads at a glance. The object's actual selection cue is
/// the screen-space SILHOUETTE OUTLINE (the sel_mask edge-detect composite) — this
/// is just a complementary pivot indicator, no longer a misleading fixed-size box.
fn push_selection_gizmo(out: &mut Vec<LineVertex>, p: Vec3) {
    let len = 0.4;
    for (dir, color) in [
        (Vec3::X, [0.95, 0.3, 0.3]),
        (Vec3::Y, [0.4, 0.9, 0.4]),
        (Vec3::Z, [0.4, 0.6, 1.0]),
    ] {
        out.push(LineVertex { position: p.to_array(), color });
        out.push(LineVertex { position: (p + dir * len).to_array(), color });
    }
}

/// Append a wireframe cone showing a beam (axis, end ring, a few generatrices)
/// in the fixture color — a placeholder gizmo alongside the volumetric beam.
fn push_beam_indicator(out: &mut Vec<LineVertex>, spec: &BeamSpec) {
    let dir = spec.dir;
    if dir == Vec3::ZERO {
        return;
    }
    let lens = spec.origin;
    let length = 6.0;
    let half_angle = (spec.angle * 0.5).to_radians();
    let radius = length * half_angle.tan();
    let end = lens + dir * length;

    let helper = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let right = dir.cross(helper).normalize_or_zero();
    let fwd = right.cross(dir).normalize_or_zero();

    let i = 0.2 + 0.5 * spec.intensity.clamp(0.0, 1.0);
    let color = [spec.color[0] * i, spec.color[1] * i, spec.color[2] * i];

    const SEGS: usize = 24;
    let ring: Vec<Vec3> = (0..SEGS)
        .map(|k| {
            let a = k as f32 / SEGS as f32 * TAU;
            end + (right * a.cos() + fwd * a.sin()) * radius
        })
        .collect();

    let mut line = |a: Vec3, b: Vec3| {
        out.push(LineVertex { position: a.to_array(), color });
        out.push(LineVertex { position: b.to_array(), color });
    };

    for k in 0..SEGS {
        line(ring[k], ring[(k + 1) % SEGS]);
    }
    for k in (0..SEGS).step_by(SEGS / 8) {
        line(lens, ring[k]);
    }
    line(lens, end);
}
