//! Per-beam shadow maps for the "hero" (sharp moving-head) beams.
//!
//! A fixed-layer `Depth32Float` texture array — a software shadow atlas. Each lit
//! hero beam gets one layer, filled by a depth-only pass from the beam's point of
//! view (`shadow.wgsl`). Both the surface lighting (`mesh.wgsl`) and the beam
//! raymarch (`volumetric.wgsl`) sample it, so set pieces cast shadows on the floor
//! AND occlude the beam shaft mid-air. The dozens of moving heads are animated
//! (un-cacheable), so they correctly warrant dedicated per-beam maps; the static
//! wash/pixel masses are a separate (cached/aggregate) problem — see
//! `docs/RESEARCH-shadows.md`.

use super::mesh::{MeshInstance, MeshVertex};

/// Maximum simultaneously-shadowed beams (= atlas layers). The N sharpest lit
/// beams are chosen each frame; the rest go unshadowed.
pub const MAX: usize = 8;
/// Per-map resolution (square).
pub const RES: u32 = 1024;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

pub struct ShadowMaps {
    /// `texture_depth_2d_array` view for sampling in the lighting shaders.
    pub array_view: wgpu::TextureView,
    /// One single-layer `D2` view per atlas layer, used as the depth attachment.
    pub layer_views: Vec<wgpu::TextureView>,
    /// Comparison sampler (hardware 2x2 PCF).
    pub sampler: wgpu::Sampler,
    /// Light view-proj per layer for the depth passes (dynamic-offset uniform).
    pub render_matrices: wgpu::Buffer,
    /// Light view-proj per layer, tightly packed, for sampling (`array<mat4>`).
    pub sample_matrices: wgpu::Buffer,
    pub render_bg: wgpu::BindGroup,
    pub pipeline: wgpu::RenderPipeline,
    /// Dynamic-offset stride (>= one mat4, rounded to the uniform alignment).
    pub align: u64,
    _atlas: wgpu::Texture,
}

impl ShadowMaps {
    pub fn new(device: &wgpu::Device) -> Self {
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow-atlas"),
            size: wgpu::Extent3d {
                width: RES,
                height: RES,
                depth_or_array_layers: MAX as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let array_view = atlas.create_view(&wgpu::TextureViewDescriptor {
            label: Some("shadow-atlas-array"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layer_views = (0..MAX as u32)
            .map(|i| {
                atlas.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("shadow-layer"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: i,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        let align = (device.limits().min_uniform_buffer_offset_alignment as u64).max(64);
        let render_matrices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow-render-matrices"),
            size: align * MAX as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sample_matrices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow-sample-matrices"),
            size: 64 * MAX as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let render_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow-render-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: std::num::NonZeroU64::new(64),
                },
                count: None,
            }],
        });
        let render_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow-render-bg"),
            layout: &render_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &render_matrices,
                    offset: 0,
                    size: std::num::NonZeroU64::new(64),
                }),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow-pipeline-layout"),
            bind_group_layouts: &[Some(&render_bgl)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/shadow.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[MeshVertex::layout(), MeshInstance::layout()],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                // Slope-scaled + constant bias to keep the receiver off its own
                // occluder depth (kills shadow acne); the shader adds a tiny bias too.
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });

        Self {
            array_view,
            layer_views,
            sampler,
            render_matrices,
            sample_matrices,
            render_bg,
            pipeline,
            align,
            _atlas: atlas,
        }
    }
}
