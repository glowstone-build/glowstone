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
pub mod mesh;
mod noise;
mod pipeline;
mod shadow;
pub mod viewport;

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use winit::window::Window;

use crate::optics;
use crate::scene::library::FixtureGeometry;
use crate::scene::{Fixture, RenderSettings, Scene, Selection, ViewportMode};
use camera::{CameraUniform, OrbitCamera};
use mesh::{GpuMesh, GrowBuffer, LensInstance, LineVertex, MeshInstance};
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
    cookie_r: [f32; 4],    // xyz = lens-plane right basis, w = gobo1 atlas layer (frac; <0 none)
    cookie_u: [f32; 4],    // xyz = lens-plane up basis,    w = gobo1 rotation (rad)
    extra: [f32; 4],       // x = gobo2 layer (<0 none), y = gobo2 rot, z = anim layer (<0 none), w = anim scroll
    shape: [f32; 4],       // x = super-Gaussian order, y = focus dist (m), z = iris frac, w = frost 0..1
    misc: [f32; 4],        // x = CA strength, y = unused, z = atlas layer count, w = unused
}

impl FixtureGpu {
    /// A disabled (zero-radiance) beam — used to keep the storage buffer's bound
    /// length ≥ 1 when the scene has no fixtures.
    fn disabled() -> Self {
        let mut f = Self::zeroed();
        f.cookie_r[3] = -1.0;
        f.extra[0] = -1.0;
        f.extra[2] = -1.0;
        f.misc[3] = -1.0;
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

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    start_time: Instant,

    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    line_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    /// Wireframe variant of the mesh pipeline (None if the GPU lacks line polygon mode).
    mesh_wire_pipeline: Option<wgpu::RenderPipeline>,
    lens_pipeline: wgpu::RenderPipeline,
    light_layout: wgpu::BindGroupLayout,

    grid_mesh: GpuMesh,
    floor_mesh: GpuMesh,
    cylinder_mesh: GpuMesh,
    cone_mesh: GpuMesh,
    disc_mesh: GpuMesh,

    floor_instances: GrowBuffer,
    fixture_instances: GrowBuffer,
    lens_instances: GrowBuffer,
    dynamic_lines: GrowBuffer,

    // Imported GDTF fixture models: per-fixture-type (Arc ptr) cache of part
    // meshes (keyed by model name), plus a per-frame instance buffer.
    gdtf_cache: HashMap<usize, HashMap<String, GpuMesh>>,
    gdtf_instances: GrowBuffer,

    // Imported MVR static geometry (stage/truss/set): cache of baked meshes
    // keyed by the model blob's Arc pointer, plus a per-frame instance buffer.
    scene_geom_cache: HashMap<usize, GpuMesh>,
    scene_geom_instances: GrowBuffer,

    // Placeholder cone bodies for GDTF fixtures whose 3D models didn't bake
    // (absent / unsupported model format) — so the fixture is still visible.
    gdtf_placeholder_instances: GrowBuffer,

    // Gobo/animation texture atlas (built from GDTF wheel media on first load).
    gobo_atlas: atlas::GoboAtlas,

    // Per-beam shadow maps for the hero (sharp moving-head) beams.
    shadow: shadow::ShadowMaps,

    // Volumetric raymarch (rendered at half resolution, then upsampled).
    volumetric_pipeline: wgpu::RenderPipeline,
    volumetric_layout: wgpu::BindGroupLayout,
    volumetric_uniform: wgpu::Buffer,
    fixtures_storage: GrowBuffer,
    composite_pipeline: wgpu::RenderPipeline,
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
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Self {
        let instance = wgpu::Instance::default();

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

        log::info!("using adapter: {:?}", adapter.get_info());

        // Wireframe viewport mode needs line polygon mode; request it when the
        // adapter offers it (it's not a core WebGPU feature), else fall back to a
        // solid-but-flat wireframe view.
        let wireframe_supported = adapter
            .features()
            .contains(wgpu::Features::POLYGON_MODE_LINE);
        let required_features = if wireframe_supported {
            wgpu::Features::POLYGON_MODE_LINE
        } else {
            wgpu::Features::empty()
        };

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("previz-device"),
                required_features,
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .expect("request device");

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
        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&light_layout)],
            immediate_size: 0,
        });

        let line_pipeline = pipeline::line_pipeline(&device, &line_layout);
        let mesh_pipeline = pipeline::mesh_pipeline(&device, &mesh_layout);
        let mesh_wire_pipeline =
            wireframe_supported.then(|| pipeline::mesh_wire_pipeline(&device, &mesh_layout));
        let lens_pipeline = pipeline::lens_pipeline(&device, &line_layout);

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

        let vertex = wgpu::BufferUsages::VERTEX;
        let inst = std::mem::size_of::<MeshInstance>() as u64;
        let floor_instances = GrowBuffer::new(&device, "floor-instances", vertex, inst);
        let fixture_instances = GrowBuffer::new(&device, "fixture-instances", vertex, inst * 64);
        let lens_instances = GrowBuffer::new(&device, "lens-instances", vertex, inst * 64);
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
        let fixtures_storage = GrowBuffer::new(
            &device,
            "fixtures-gpu",
            wgpu::BufferUsages::STORAGE,
            std::mem::size_of::<FixtureGpu>() as u64 * 16,
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
        let composite_pipeline = pipeline::composite_pipeline(&device, &single_tex_layout);
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

        Self {
            surface,
            device,
            queue,
            config,
            start_time: Instant::now(),
            camera_buffer,
            camera_bind_group,
            line_pipeline,
            mesh_pipeline,
            mesh_wire_pipeline,
            lens_pipeline,
            light_layout,
            grid_mesh,
            floor_mesh,
            cylinder_mesh,
            cone_mesh,
            disc_mesh,
            floor_instances,
            fixture_instances,
            lens_instances,
            dynamic_lines,
            gdtf_cache: HashMap::new(),
            gdtf_instances,
            scene_geom_cache: HashMap::new(),
            scene_geom_instances,
            gdtf_placeholder_instances,
            gobo_atlas,
            shadow,
            volumetric_pipeline,
            volumetric_layout,
            volumetric_uniform,
            fixtures_storage,
            composite_pipeline,
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
        }
    }

    /// Reconfigure the swapchain after a window resize.
    pub fn resize_surface(&mut self, size: (u32, u32)) {
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        self.config.width = size.0;
        self.config.height = size.1;
        self.surface.configure(&self.device, &self.config);
    }

    /// Resize the offscreen 3D target to match the viewport panel.
    pub fn resize_viewport(&mut self, size: (u32, u32)) {
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

    /// Bake an imported MVR static-geometry model (a GLB blob) into a cached
    /// mesh, keyed by the blob's `Arc` pointer so identical instances share and
    /// re-imports allocate fresh entries. Returns the cache key, or `None` if the
    /// GLB had no drawable geometry.
    fn ensure_scene_geom_loaded(&mut self, model: &crate::mvr::GeometryModel) -> Option<usize> {
        let key = Arc::as_ptr(&model.glb) as usize;
        if !self.scene_geom_cache.contains_key(&key) {
            let verts = fixture_model::load_glb(&model.glb);
            if verts.is_empty() {
                log::warn!("mvr: model '{}' baked to 0 triangles", model.file);
                return None;
            }
            let mesh = GpuMesh::new(&self.device, &model.file, &verts);
            self.scene_geom_cache.insert(key, mesh);
        }
        Some(key)
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

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => {
                log::debug!("surface not presentable; skipping frame");
                return false;
            }
            other => {
                log::warn!("dropping frame: surface status {other:?}");
                return false;
            }
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

        true
    }

    /// Render the 3D scene into the offscreen LDR target (no window/surface,
    /// no egui) and read it back as RGBA8 pixels. Used by the headless
    /// `--screenshot` path so the render can be verified without a visible
    /// window. Returns (width, height, rgba8 pixels).
    pub fn capture(
        &mut self,
        scene: &Scene,
        camera: &OrbitCamera,
        settings: &RenderSettings,
    ) -> (u32, u32, Vec<u8>) {
        let (width, height) = self.viewport.size;

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

    /// Record the full offscreen 3D frame into `encoder`: forward scene ->
    /// volumetric beams -> bloom -> tonemap into the LDR target. Shared by
    /// [`render`](Self::render) and [`capture`](Self::capture).
    fn record_scene(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        camera: &OrbitCamera,
        selection: &Selection,
        settings: &RenderSettings,
    ) {
        let time = self.start_time.elapsed().as_secs_f32();
        let aspect = self.viewport.aspect();

        // --- camera uniform ---
        let mut camera_uniform = camera.uniform(aspect);
        camera_uniform.render_mode[0] = settings.mode.shader_code();
        camera_uniform.render_mode[1] = settings.gobo_sharpness.max(0.0); // floor-pool gobo sharpen

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
                if fixture.is_gdtf() || fixture.geometry != geometry {
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
        // GDTF fixtures whose models didn't bake get a placeholder cone instead.
        let mut gdtf_placeholders: Vec<MeshInstance> = Vec::new();
        let mut beam_frames: Vec<Vec<fixture_model::BeamFrame>> =
            vec![Vec::new(); scene.fixtures.len()];
        for (i, fixture) in scene.fixtures.iter().enumerate() {
            let Some(gdtf) = fixture.gdtf.clone() else {
                continue;
            };
            let key = Arc::as_ptr(&gdtf) as usize;
            self.ensure_gdtf_loaded(key, &gdtf);
            // Place the fixture: translate, then the MVR hang orientation (identity
            // for app-created fixtures), then GDTF +Z-up → world +Y-up. Pan/tilt
            // are articulated inside `assemble`.
            let root = Mat4::from_translation(fixture.position)
                * Mat4::from_quat(fixture.orientation)
                * gdtf_to_world;
            let asm =
                fixture_model::assemble(&gdtf, fixture.mode_index, root, fixture.pan_actual, fixture.tilt_actual);
            beam_frames[i] = asm.beams;
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
        let mut scene_geom_draws: Vec<(usize, u32)> = Vec::new();
        for obj in &scene.geometry {
            for model in &obj.models {
                if let Some(key) = self.ensure_scene_geom_loaded(model) {
                    let idx = scene_geom_instances.len() as u32;
                    scene_geom_instances.push(MeshInstance {
                        model: (obj.transform * glb_flip).to_cols_array_2d(),
                        color: [0.13, 0.13, 0.15],
                        intensity: 1.0,
                        selected: 0.0,
                    });
                    scene_geom_draws.push((key, idx));
                }
            }
        }
        self.scene_geom_instances
            .upload(&self.device, &self.queue, &scene_geom_instances);
        // Drop baked meshes no longer referenced by the scene (e.g. after a new
        // MVR import replaces the geometry) so the cache can't grow unbounded.
        // Guarded so the steady state pays nothing.
        if self.scene_geom_cache.len() > scene_geom_draws.len() {
            let live: std::collections::HashSet<usize> = scene
                .geometry
                .iter()
                .flat_map(|o| o.models.iter().map(|m| Arc::as_ptr(&m.glb) as usize))
                .collect();
            self.scene_geom_cache.retain(|k, _| live.contains(k));
        }

        // --- dynamic lines: fog-box wireframes + beam indicators ---
        let mut lines: Vec<LineVertex> = Vec::new();
        for (i, env) in scene.environments.iter().enumerate() {
            let color = if selection.environment == Some(i) {
                [0.6, 0.95, 1.0]
            } else {
                [0.30, 0.55, 0.72]
            };
            mesh::push_box_wireframe(&mut lines, env.min().to_array(), env.max().to_array(), color);
        }
        if settings.show_beam_wireframes {
            for (i, fixture) in scene.fixtures.iter().enumerate() {
                push_beam_indicator(&mut lines, &beam_spec(fixture, beam_frames[i].first().copied()));
            }
        }
        // Selection gizmo for every selected fixture (RGB axes + marker box).
        for &sel in &selection.fixtures {
            if let Some(f) = scene.fixtures.get(sel) {
                push_selection_gizmo(&mut lines, f.position);
            }
        }
        let line_count = self.dynamic_lines.upload(&self.device, &self.queue, &lines);

        // --- volumetric uniforms + fixtures (use the first fog box, if any) ---
        let fog = scene.environments.first();
        let has_fog = fog.map(|f| f.density > 1e-4).unwrap_or(false);
        // Resolve each fixture's optics → its GPU beams (per lit emitter, per
        // prism facet; uniform LED arrays collapse to one aggregate). The
        // shaders loop `arrayLength(&fixtures)`, so the expansion is
        // transparent to them.
        let mut gpu_fixtures: Vec<FixtureGpu> = Vec::with_capacity(scene.fixtures.len());
        // Which scene fixture each GPU beam came from (shadow dedupe).
        let mut beam_fixture: Vec<usize> = Vec::with_capacity(scene.fixtures.len());
        let mut lens_instances: Vec<LensInstance> = Vec::with_capacity(scene.fixtures.len());
        let beam_dump = std::env::var("PREVIZ_BEAM_DUMP").is_ok();
        for (i, f) in scene.fixtures.iter().enumerate() {
            let key = f.gdtf.as_ref().map(|g| Arc::as_ptr(g) as usize).unwrap_or(0);
            let built = build_beam_gpus(f, &beam_frames[i], key, &self.gobo_atlas, time);
            if beam_dump && !built.beams.is_empty() {
                let cmax = built
                    .beams
                    .iter()
                    .flat_map(|b| b.color[..3].iter().copied())
                    .fold(0.0_f32, f32::max);
                let b0 = &built.beams[0];
                log::info!(
                    "beams #{i} {}: n={} cmax={cmax:.3} tan_half={:.3} lens_r={:.3} n_ord={:.2}",
                    f.name,
                    built.beams.len(),
                    b0.dir_cos[3],
                    b0.color[3],
                    b0.shape[0],
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
        let max_shadows = if settings.mode == ViewportMode::Beauty {
            shadow::MAX
        } else {
            0
        };
        let mut sample_mats: Vec<[[f32; 4]; 4]> = Vec::with_capacity(max_shadows);
        let mut shadowed_fixtures: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for fi in shadow_order {
            if sample_mats.len() >= max_shadows {
                break;
            }
            if !shadowed_fixtures.insert(beam_fixture[fi]) {
                continue;
            }
            let layer = sample_mats.len();
            let f = &gpu_fixtures[fi];
            let origin = Vec3::new(f.pos_range[0], f.pos_range[1], f.pos_range[2]);
            let bdir = Vec3::new(f.dir_cos[0], f.dir_cos[1], f.dir_cos[2]);
            let up = Vec3::new(f.cookie_u[0], f.cookie_u[1], f.cookie_u[2]);
            let tan_half = f.dir_cos[3].max(1e-3);
            let range = f.pos_range[3].max(1.0);
            // Perspective from the lens, FOV = full cone angle (clamped so a wide
            // wash doesn't make a degenerate near-180° projection).
            let fov = (2.0 * tan_half.atan()).clamp(0.05, 2.4);
            let vp = Mat4::perspective_rh(fov, 1.0, 0.1, range) * Mat4::look_to_rh(origin, bdir, up);
            let cols = vp.to_cols_array_2d();
            self.queue.write_buffer(
                &self.shadow.render_matrices,
                layer as u64 * self.shadow.align,
                bytemuck::bytes_of(&cols),
            );
            sample_mats.push(cols);
            gpu_fixtures[fi].misc[3] = layer as f32;
        }
        let n_shadows = sample_mats.len();
        if !sample_mats.is_empty() {
            self.queue
                .write_buffer(&self.shadow.sample_matrices, 0, bytemuck::cast_slice(&sample_mats));
        }

        self.fixtures_storage
            .upload(&self.device, &self.queue, &gpu_fixtures);
        let lens_count = self
            .lens_instances
            .upload(&self.device, &self.queue, &lens_instances);

        if let Some(fog) = fog {
            let inv_vp = camera.view_proj(aspect).inverse();
            let eye = camera.eye();
            // Constant-dt target for the raymarch: a full-diagonal ray spends the
            // whole `steps` budget; shorter clipped rays scale their step count down
            // to keep per-metre sampling (dt) roughly constant. See volumetric.wgsl.
            let steps = settings.steps.max(1);
            let target_dt = (fog.max() - fog.min()).length() / steps as f32;
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
                counts: [gpu_fixtures.len() as f32, steps as f32, target_dt, 0.0],
            };
            self.queue
                .write_buffer(&self.volumetric_uniform, 0, bytemuck::bytes_of(&vol));
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
            ],
        });
        let composite_bg = self.single_tex_bg(self.viewport.vol_view());
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

        // Pass 0: hero-beam shadow maps — one depth-only pass per selected beam,
        // rendering the solid occluders (floor + MVR geometry + fixture models)
        // from that beam's point of view into its atlas layer.
        for layer in 0..n_shadows {
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
                ..Default::default()
            });
            spass.set_pipeline(&self.shadow.pipeline);
            spass.set_bind_group(0, &self.shadow.render_bg, &[(layer as u64 * self.shadow.align) as u32]);
            spass.set_vertex_buffer(0, self.floor_mesh.vertex_buffer.slice(..));
            spass.set_vertex_buffer(1, self.floor_instances.buffer.slice(..));
            spass.draw(0..self.floor_mesh.vertex_count, 0..1);
            if !scene_geom_draws.is_empty() {
                spass.set_vertex_buffer(1, self.scene_geom_instances.buffer.slice(..));
                for (key, idx) in &scene_geom_draws {
                    if let Some(mesh) = self.scene_geom_cache.get(key) {
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
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });

            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &light_bg, &[]);

            let mesh_pipe = match (settings.mode, self.mesh_wire_pipeline.as_ref()) {
                (ViewportMode::Wireframe, Some(wire)) => wire,
                _ => &self.mesh_pipeline,
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
            if !gdtf_draws.is_empty() {
                pass.set_vertex_buffer(1, self.gdtf_instances.buffer.slice(..));
                for (key, model, idx) in &gdtf_draws {
                    if let Some(mesh) = self.gdtf_cache.get(key).and_then(|m| m.get(model)) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *idx..*idx + 1);
                    }
                }
            }

            // Imported MVR static geometry (each model is one instance).
            if !scene_geom_draws.is_empty() {
                pass.set_vertex_buffer(1, self.scene_geom_instances.buffer.slice(..));
                for (key, idx) in &scene_geom_draws {
                    if let Some(mesh) = self.scene_geom_cache.get(key) {
                        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                        pass.draw(0..mesh.vertex_count, *idx..*idx + 1);
                    }
                }
            }

            // Placeholder cones for GDTF fixtures with no baked model.
            if gdtf_placeholder_count > 0 {
                pass.set_vertex_buffer(0, self.cone_mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.gdtf_placeholder_instances.buffer.slice(..));
                pass.draw(0..self.cone_mesh.vertex_count, 0..gdtf_placeholder_count);
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
            // The fog-box border + gizmos (dynamic_lines) are always drawn.
            if line_count > 0 {
                pass.set_vertex_buffer(0, self.dynamic_lines.buffer.slice(..));
                pass.draw(0..line_count, 0..1);
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
                ..Default::default()
            });
            pass.set_pipeline(&self.ssao_pipeline);
            pass.set_bind_group(0, &ao_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Pass 2a: volumetric raymarch -> half-res vol target (scatter, transmit).
        // Pass 2b: upsample + composite into the HDR scene.
        if has_fog && settings.mode == ViewportMode::Beauty {
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("volumetric-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: self.viewport.vol_view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    ..Default::default()
                });
                pass.set_pipeline(&self.volumetric_pipeline);
                pass.set_bind_group(0, &volumetric_bg, &[]);
                pass.draw(0..3, 0..1);
            }
            // Composite (blend One/SrcAlpha) is configured on the pipeline.
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite-pass"),
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
                ..Default::default()
            });
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, &composite_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        // Pass 3: bloom bright-pass (HDR -> bloom[0]).
        self.fullscreen(encoder, "bloom-bright", &self.bloom_bright, self.viewport.bloom_view(0), &bright_bg);
        // Pass 4: separable blur (bloom[0] -> bloom[1] -> bloom[0]).
        self.fullscreen(encoder, "bloom-blur-h", &self.bloom_blur_h, self.viewport.bloom_view(1), &blur_h_bg);
        self.fullscreen(encoder, "bloom-blur-v", &self.bloom_blur_v, self.viewport.bloom_view(0), &blur_v_bg);
        // Pass 5: tonemap/resolve (HDR + bloom -> LDR, sRGB-encoded).
        self.fullscreen(encoder, "tonemap", &self.tonemap_pipeline, self.viewport.ldr_view(), &tonemap_bg);
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
                cookie_r: [f.right.x, f.right.y, f.right.z, -1.0],
                cookie_u: [f.up.x, f.up.y, f.up.z, 0.0],
                extra: [-1.0, 0.0, -1.0, 0.0],
                shape: [4.0, 8.0, 1.0, 0.0], // soft-ish edge → survives downsampling
                misc: [0.0, 1.0, atlas_layers, -1.0], // misc.y = laser flag
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
        let i = fixture.intensity.max(0.0);
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
                    cookie_r: [f.right.x, f.right.y, f.right.z, -1.0],
                    cookie_u: [f.up.x, f.up.y, f.up.z, 0.0],
                    extra: [-1.0, 0.0, -1.0, 0.0],
                    shape: [6.0, 8.0, 1.0, 0.0],
                    misc: [0.0, 0.0, atlas_layers, -1.0],
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
            )],
        };
    };

    let o = optics::resolve(gdtf, fixture.mode_index, &fixture.optics, &fixture.motion, time);
    let emitters = fixture.emitters();

    // Gobo/animation wheel selections → absolute fractional atlas layers.
    let layer_of = |sel: &Option<optics::WheelSel>| -> (f32, f32) {
        match sel {
            Some(s) => match atlas.base_layer(key, &s.wheel) {
                Some(base) => (base as f32 + s.slot_frac, s.rot),
                None => (-1.0, 0.0),
            },
            None => (-1.0, 0.0),
        }
    };
    let (g1_layer, g1_rot) = layer_of(&o.gobo1);
    let (g2_layer, g2_rot) = layer_of(&o.gobo2);
    let (anim_layer, anim_scroll) = match &o.anim {
        // Slot 0 = open (white), slot 1 = the animation glass.
        Some((wheel, scroll)) => match atlas.base_layer(key, wheel) {
            Some(base) => ((base + 1) as f32, *scroll),
            None => (-1.0, 0.0),
        },
        None => (-1.0, 0.0),
    };

    // Commanded master level: intensity × dimmer × shutter. ~0 = blacked out →
    // emit no beams so the raymarch / floor loop skips the fixture entirely.
    let level = fixture.intensity.max(0.0) * fixture.optics.dimmer.max(0.0) * o.shutter_gain;
    let lens_level = (fixture.intensity * fixture.optics.dimmer).max(0.0) * o.shutter_gain;

    let make = |frame: &fixture_model::BeamFrame,
                bdir: Vec3,
                r: Vec3,
                u: Vec3,
                col: [f32; 3],
                tan_half: f32,
                n_order: f32,
                lens_r: f32| FixtureGpu {
        pos_range: [frame.origin.x, frame.origin.y, frame.origin.z, RANGE],
        dir_cos: [bdir.x, bdir.y, bdir.z, tan_half],
        color: [col[0], col[1], col[2], lens_r],
        cookie_r: [r.x, r.y, r.z, g1_layer],
        cookie_u: [u.x, u.y, u.z, g1_rot],
        extra: [g2_layer, g2_rot, anim_layer, anim_scroll],
        shape: [n_order, o.focus_dist, o.iris, o.frost],
        misc: [o.ca_strength, 0.0, atlas_layers, -1.0], // misc.w = shadow layer (-1 = none)
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
        let scale = level * cone.brightness;
        let base_color = [tint[0] * scale, tint[1] * scale, tint[2] * scale];
        let cell_lit = cell.iter().fold(0.0_f32, |a, &b| a.max(b));

        let beams = if level * cell_lit < 1e-4 || !cone.shaft {
            Vec::new()
        } else if o.prism.is_empty() {
            vec![make(&frame, frame.dir, frame.right, frame.up, base_color, cone.tan_half, cone.n_order, lens_radius)]
        } else {
            // Each facet is a separated aerial beam: deflect the axis, rebuild
            // its basis, split energy.
            o.prism
                .iter()
                .map(|p| {
                    let d = (frame.dir + frame.right * p.offset[0] + frame.up * p.offset[1]).normalize_or_zero();
                    let (r2, u2) = ortho_basis(d, frame.up);
                    let c = [base_color[0] * p.weight, base_color[1] * p.weight, base_color[2] * p.weight];
                    make(&frame, d, r2, u2, c, cone.tan_half, cone.n_order, lens_radius)
                })
                .collect()
        };

        // Physical front lens at the beam exit, tinted by the colour chain.
        let lenses = vec![lens_instance(
            frame.origin + frame.dir * 0.04,
            frame.dir,
            frame.right,
            frame.up,
            lens_radius * 1.25,
            tint,
            lens_level * cell_lit.min(1.0),
            cone.tan_half,
            cone.n_order,
            cone.face_gain,
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
        let scale = level * cone.brightness;
        let cell_max = cell.iter().fold(0.0_f32, |a, &b| a.max(b));
        cells.push(Cell {
            frame,
            color: [tint[0] * scale, tint[1] * scale, tint[2] * scale],
            tint,
            cell_max,
            lit: cell_max * level,
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
            (lens_level * c.cell_max).min(1.0),
            c.cone.tan_half,
            c.cone.n_order,
            c.cone.face_gain,
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
    let no_cookie = g1_layer < 0.0 && g2_layer < 0.0 && anim_layer < 0.0 && o.prism.is_empty();
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
        make(&agg_frame, mean_dir, right, up, color, tan_eff, n_order, spread_r.max(cl[0].lens_r))
    };

    // Pass 1: exact direction clusters; uniform ones merge losslessly.
    let exact = cluster_by(&lit, 0.9999);
    let mut planned: Vec<Vec<&Cell>> = Vec::new(); // clusters to merge
    let mut single: Vec<&Cell> = Vec::new(); // cells to emit individually
    for cl in &exact {
        let cs: Vec<&Cell> = cl.iter().map(|&i| lit[i]).collect();
        let uniform = cs.iter().all(|c| {
            (0..3).all(|k| (c.color[k] - cs[0].color[k]).abs() < 0.02 * cs[0].color[k].max(0.05))
        });
        if cs.len() >= 4 && uniform && no_cookie {
            planned.push(cs);
        } else {
            single.extend(cs);
        }
    }
    const MAX_FIXTURE_BEAMS: usize = 16;
    if planned.len() + single.len() > MAX_FIXTURE_BEAMS {
        // Pass 2 (LOD): coarse 25° direction cones, merged unconditionally —
        // bounded cost, slightly soft/averaged but faithful in aggregate. The
        // per-cell lens faces above still carry the pixel-mapped detail.
        let coarse = cluster_by(&lit, 0.906);
        for cl in &coarse {
            beams.push(aggregate(&cl.iter().map(|&i| lit[i]).collect::<Vec<_>>()));
        }
    } else {
        for cs in &planned {
            beams.push(aggregate(cs));
        }
        for c in single {
            beams.push(make(
                &c.frame,
                c.frame.dir,
                c.frame.right,
                c.frame.up,
                c.color,
                c.cone.tan_half,
                c.cone.n_order,
                c.lens_r,
            ));
        }
    }
    BeamBuild { beams, lenses }
}

/// Append a selection gizmo at `p`: RGB world axes plus a small amber marker
/// box, so the selected fixture's position/orientation is clear in the view.
fn push_selection_gizmo(out: &mut Vec<LineVertex>, p: Vec3) {
    let len = 0.6;
    for (dir, color) in [
        (Vec3::X, [0.95, 0.3, 0.3]),
        (Vec3::Y, [0.4, 0.9, 0.4]),
        (Vec3::Z, [0.4, 0.6, 1.0]),
    ] {
        out.push(LineVertex { position: p.to_array(), color });
        out.push(LineVertex { position: (p + dir * len).to_array(), color });
    }
    let h = Vec3::splat(0.22);
    mesh::push_box_wireframe(out, (p - h).to_array(), (p + h).to_array(), [1.0, 0.75, 0.2]);
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
