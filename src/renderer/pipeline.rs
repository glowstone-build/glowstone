//! WGSL loading and render-pipeline construction.
//!
//! The forward pipelines (grid lines, lit meshes) render into the offscreen
//! **HDR** [`Viewport`] target. The post pipelines (volumetric raymarch, bloom,
//! tonemap) are fullscreen passes that consume/produce those targets.

use super::mesh::{LensInstance, LineVertex, MeshInstance, MeshVertex, WallInstance};
use super::viewport::Viewport;

fn load(device: &wgpu::Device, label: &str, source: &'static str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

/// Shared beam-optics helpers (`optics.wgsl`) prepended to a shader source. WGSL
/// has no `#include`, so the beam (`volumetric.wgsl`) and floor (`mesh.wgsl`)
/// shaders that share these helpers are built from the concatenated source.
const OPTICS_WGSL: &str = include_str!("../shaders/optics.wgsl");

fn load_with_optics(device: &wgpu::Device, label: &str, body: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(format!("{OPTICS_WGSL}\n{body}").into()),
    })
}

fn depth_stencil() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: Viewport::DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn hdr_target() -> Option<wgpu::ColorTargetState> {
    Some(wgpu::ColorTargetState {
        format: Viewport::HDR_FORMAT,
        blend: Some(wgpu::BlendState::REPLACE),
        write_mask: wgpu::ColorWrites::ALL,
    })
}

// ---------------------------------------------------------------------------
// Forward pipelines
// ---------------------------------------------------------------------------

/// Pipeline for the ground grid + world axes + wireframes/beams (a `LineList`).
pub fn line_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
    let shader = load(device, "grid.wgsl", include_str!("../shaders/grid.wgsl"));
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("line-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[LineVertex::layout()],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            ..Default::default()
        },
        depth_stencil: Some(depth_stencil()),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[hdr_target()],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Pipeline for the emitter lens faces (instanced `TriangleList`, camera-only
/// bind group, no backface cull so the lens reads from either side).
pub fn lens_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
    let shader = load(device, "lens.wgsl", include_str!("../shaders/lens.wgsl"));
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("lens-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[MeshVertex::layout(), LensInstance::layout()],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(depth_stencil()),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[hdr_target()],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Pipeline for LED-wall surfaces: an instanced unit quad (camera-only bind
/// group, like the lens pipeline) that writes emissive HDR colour with a
/// distance-aware LED pixel mask. No backface cull (read from either side).
pub fn wall_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
    let shader = load(device, "wall.wgsl", include_str!("../shaders/wall.wgsl"));
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wall-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[MeshVertex::layout(), WallInstance::layout()],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(depth_stencil()),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[hdr_target()],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Transparent / see-through LED walls: same as [`wall_pipeline`] but with
/// PREMULTIPLIED-alpha blending (`One` / `OneMinusSrcAlpha`) and **no depth
/// write** (depth-test still on), drawn after the opaque scene so it shows
/// through the gaps between lit LEDs.
pub fn wall_alpha_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
    let shader = load(device, "wall.wgsl", include_str!("../shaders/wall.wgsl"));
    let blend = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wall-alpha-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[MeshVertex::layout(), WallInstance::layout()],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: Viewport::DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: Viewport::HDR_FORMAT,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Pipeline for the instanced, lit meshes — floor + fixtures (a `TriangleList`).
pub fn mesh_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
    mesh_pipeline_mode(device, layout, wgpu::PolygonMode::Fill, "mesh-pipeline")
}

/// Same mesh pipeline drawn as wireframe (line polygon mode) for the Wireframe
/// viewport mode. Requires the `POLYGON_MODE_LINE` device feature.
pub fn mesh_wire_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
    mesh_pipeline_mode(device, layout, wgpu::PolygonMode::Line, "mesh-wire-pipeline")
}

fn mesh_pipeline_mode(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    polygon_mode: wgpu::PolygonMode,
    label: &str,
) -> wgpu::RenderPipeline {
    let shader = load_with_optics(device, "mesh.wgsl", include_str!("../shaders/mesh.wgsl"));
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[MeshVertex::layout(), MeshInstance::layout()],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            polygon_mode,
            ..Default::default()
        },
        depth_stencil: Some(depth_stencil()),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[hdr_target()],
        }),
        multiview_mask: None,
        cache: None,
    })
}

// ---------------------------------------------------------------------------
// Bind-group layouts for the post passes
// ---------------------------------------------------------------------------

pub fn volumetric_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("volumetric-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // 5: gobo atlas (texture_2d_array), 6: its filtering sampler.
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // 7: hero-beam shadow atlas, 8: comparison sampler, 9: shadow matrices.
            shadow_atlas_entry(7),
            shadow_sampler_entry(8),
            shadow_matrices_entry(9),
            // 10: per-fixture wheel chain (dynamic count).
            storage_entry(10),
        ],
    })
}

/// Read-only storage buffer bind-group-layout entry (fragment-visible).
fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Fixtures-as-spotlights storage buffer + the gobo atlas for surface lighting
/// (mesh group 1): binding 0 = fixtures, 1 = atlas texture, 2 = atlas sampler,
/// 3 = hero-beam shadow atlas, 4 = its comparison sampler, 5 = shadow matrices.
pub fn light_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("light-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            shadow_atlas_entry(3),
            shadow_sampler_entry(4),
            shadow_matrices_entry(5),
            // 6: per-fixture wheel chain (dynamic count).
            storage_entry(6),
        ],
    })
}

/// World HDRI environment: equirectangular texture (with mips) + filtering
/// sampler. Bound as mesh group 2 (IBL ambient) and sky group 1 (background).
pub fn world_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("world-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// The sky / world-background pipeline: a fullscreen triangle (sky.wgsl) that
/// samples the HDRI by reconstructed camera ray. Runs inside the depth-attached
/// forward pass with depth-compare Always + no depth write, so opaque geometry
/// drawn afterwards overdraws it.
pub fn sky_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sky.wgsl"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/sky.wgsl").into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sky-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: Viewport::DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[hdr_target()],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Shadow atlas (`texture_depth_2d_array`) bind-group-layout entry.
fn shadow_atlas_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

/// Shadow comparison sampler (`sampler_comparison`) bind-group-layout entry.
fn shadow_sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
        count: None,
    }
}

/// Shadow light view-proj matrices (`array<mat4>` storage) bind-group-layout entry.
fn shadow_matrices_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Depth texture + a small params uniform for the SSAO pass (group 0).
pub fn ssao_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ssao-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

/// SSAO: reads the depth buffer, MULTIPLY-blends an occlusion factor onto the HDR
/// target (`out = ao * hdr`) so flat Unlit geometry gains contact/crevice shading.
pub fn ssao_pipeline(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
    let shader = load(device, "ssao.wgsl", include_str!("../shaders/ssao.wgsl"));
    let blend = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent::REPLACE,
    };
    fullscreen_pipeline(
        device, "ssao-pipeline", layout, &shader, "fs_ssao", Viewport::HDR_FORMAT, Some(blend),
    )
}

/// One sampled texture + a filtering sampler (bloom bright/blur).
pub fn single_tex_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("single-tex-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Two sampled textures + sampler + a small uniform (tonemap/resolve).
pub fn tonemap_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let tex = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tonemap-bgl"),
        entries: &[
            tex(0),
            tex(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// Post pipelines (fullscreen-triangle fragment passes)
// ---------------------------------------------------------------------------

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    bind_group_layout: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    fs_entry: &str,
    target: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_fullscreen"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Volumetric raymarch — writes (scatter.rgb, transmittance.a) into the
/// half-res volumetric target (no blend; the composite pass upsamples it).
pub fn volumetric_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = load_with_optics(device, "volumetric.wgsl", include_str!("../shaders/volumetric.wgsl"));
    fullscreen_pipeline(
        device,
        "volumetric-pipeline",
        layout,
        &shader,
        "fs_volumetric",
        Viewport::VOL_FORMAT,
        None,
    )
}

/// Composite the (upsampled) half-res volumetric target into the HDR scene:
/// `out = scatter + scene · transmittance` (blend `One`, `SrcAlpha`).
pub fn composite_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = load(device, "post.wgsl", include_str!("../shaders/post.wgsl"));
    let blend = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::SrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        },
    };
    fullscreen_pipeline(
        device,
        "composite-pipeline",
        layout,
        &shader,
        "fs_composite",
        Viewport::HDR_FORMAT,
        Some(blend),
    )
}

/// Bloom bright-pass + the two separable blur pipelines (share the layout).
pub fn bloom_pipelines(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> (wgpu::RenderPipeline, wgpu::RenderPipeline, wgpu::RenderPipeline) {
    let shader = load(device, "post.wgsl", include_str!("../shaders/post.wgsl"));
    let bright = fullscreen_pipeline(
        device, "bloom-bright", layout, &shader, "fs_bright", Viewport::HDR_FORMAT, None,
    );
    let blur_h = fullscreen_pipeline(
        device, "bloom-blur-h", layout, &shader, "fs_blur_h", Viewport::HDR_FORMAT, None,
    );
    let blur_v = fullscreen_pipeline(
        device, "bloom-blur-v", layout, &shader, "fs_blur_v", Viewport::HDR_FORMAT, None,
    );
    (bright, blur_h, blur_v)
}

/// Tonemap/resolve HDR scene + bloom into the LDR (sRGB-encoded) target.
pub fn tonemap_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = load(device, "post.wgsl", include_str!("../shaders/post.wgsl"));
    fullscreen_pipeline(
        device, "tonemap-pipeline", layout, &shader, "fs_tonemap", Viewport::LDR_FORMAT, None,
    )
}
