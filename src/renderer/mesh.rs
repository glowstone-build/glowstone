//! Vertex types, CPU geometry generation, and simple GPU mesh upload.
//!
//! Geometry lives on the CPU as plain `Vec`s; [`GpuMesh`] wraps an uploaded
//! vertex buffer and [`GrowBuffer`] wraps a per-frame buffer that grows on
//! demand. Nothing here knows about fixtures — per-object data arrives as
//! [`MeshInstance`] rows.

use std::f32::consts::TAU;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// A lit triangle-mesh vertex. `emissive` (0 or 1) marks self-illuminated
/// surfaces like a fixture lens, which glow in the instance color instead of
/// being shaded.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub emissive: f32,
}

impl MeshVertex {
    const ATTRS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRS,
        }
    }
}

/// A flat, per-vertex-colored line vertex (grid, axes, wireframes, beams).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LineVertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
}

impl LineVertex {
    const ATTRS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRS,
        }
    }
}

/// One per-object instance row consumed by `mesh.wgsl` as instance-step vertex
/// attributes (used for the floor and every fixture). The CPU rewrites these
/// each frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MeshInstance {
    pub model: [[f32; 4]; 4],
    pub color: [f32; 3],
    pub intensity: f32,
    pub selected: f32,
}

impl MeshInstance {
    const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        5 => Float32x4,
        6 => Float32x4,
        7 => Float32x4,
        8 => Float32x4,
        9 => Float32x3,
        10 => Float32,
        11 => Float32,
    ];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRS,
        }
    }
}

/// One emitter lens-face instance (`lens.wgsl`): placement + emission state.
/// The unit disc is scaled/oriented by the model matrix (column z = beam dir).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LensInstance {
    pub model: [[f32; 4]; 4],
    /// rgb = cell color (linear; optics-chain tint × cell × UI master),
    /// w = level `0..1` (0 = off → dark glass).
    pub color: [f32; 4],
    /// x = tan(half beam angle), y = super-Gaussian edge order, z = candela
    /// gain (zoom concentration → face luminance), w = lens radius (m).
    pub params: [f32; 4],
    /// Mechanical shutter on the lens face: x = close 0..1, y = kind (0 none /
    /// 1 blade / 2 sawtooth), z = edge softness, w = unused.
    pub shutter: [f32; 4],
}

impl LensInstance {
    const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        5 => Float32x4,
        6 => Float32x4,
        7 => Float32x4,
        8 => Float32x4,
        9 => Float32x4,
        10 => Float32x4,
        11 => Float32x4,
    ];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LensInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRS,
        }
    }
}

/// A non-indexed vertex buffer plus its vertex count.
pub struct GpuMesh {
    pub vertex_buffer: wgpu::Buffer,
    pub vertex_count: u32,
}

impl GpuMesh {
    pub fn new<V: Pod>(device: &wgpu::Device, label: &str, vertices: &[V]) -> Self {
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Self {
            vertex_buffer,
            vertex_count: vertices.len() as u32,
        }
    }
}

/// A vertex buffer that is rewritten every frame and grows when it needs to.
pub struct GrowBuffer {
    pub buffer: wgpu::Buffer,
    capacity_bytes: u64,
    usage: wgpu::BufferUsages,
    label: &'static str,
}

impl GrowBuffer {
    pub fn new(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        initial_bytes: u64,
    ) -> Self {
        let usage = usage | wgpu::BufferUsages::COPY_DST;
        let size = initial_bytes.max(256);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            capacity_bytes: size,
            usage,
            label,
        }
    }

    /// Upload `data`, reallocating larger if needed. Returns the element count.
    pub fn upload<T: Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &[T],
    ) -> u32 {
        let bytes = std::mem::size_of_val(data) as u64;
        if bytes > self.capacity_bytes {
            let new_cap = bytes.next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: new_cap,
                usage: self.usage,
                mapped_at_creation: false,
            });
            self.capacity_bytes = new_cap;
        }
        if !data.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(data));
        }
        data.len() as u32
    }
}

// ---------------------------------------------------------------------------
// Geometry generators (CPU)
// ---------------------------------------------------------------------------

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
    [v[0] / len, v[1] / len, v[2] / len]
}

/// A large flat ground plane on `y = 0`, normal up, for diffuse shading.
pub fn floor_plane(half_size: f32) -> Vec<MeshVertex> {
    let h = half_size;
    let up = [0.0, 1.0, 0.0];
    let p = |x: f32, z: f32| MeshVertex {
        position: [x, 0.0, z],
        normal: up,
        emissive: 0.0,
    };
    vec![
        p(-h, -h),
        p(-h, h),
        p(h, h),
        p(-h, -h),
        p(h, h),
        p(h, -h),
    ]
}

/// A capped cylinder for a PAR-can body: back cap at the local origin, lens at
/// `y = -length`. The lens cap is marked `emissive` (it is the light source);
/// the beam points down `-Y` at rest. Non-indexed; `cull_mode: None`.
pub fn cylinder(length: f32, radius: f32, segments: u32) -> Vec<MeshVertex> {
    let seg = segments.max(3);
    let mut verts = Vec::with_capacity(seg as usize * 12);

    let y_back = 0.0;
    let y_front = -length;
    let ring = |a: f32, y: f32| [radius * a.cos(), y, radius * a.sin()];
    let mut push = |position, normal, emissive| {
        verts.push(MeshVertex {
            position,
            normal,
            emissive,
        })
    };

    for i in 0..seg {
        let a0 = i as f32 / seg as f32 * TAU;
        let a1 = (i + 1) as f32 / seg as f32 * TAU;
        let n0 = normalize3([a0.cos(), 0.0, a0.sin()]);
        let n1 = normalize3([a1.cos(), 0.0, a1.sin()]);

        let b0 = ring(a0, y_back);
        let b1 = ring(a1, y_back);
        let f0 = ring(a0, y_front);
        let f1 = ring(a1, y_front);

        // Side wall (two triangles).
        push(b0, n0, 0.0);
        push(f0, n0, 0.0);
        push(f1, n1, 0.0);
        push(b0, n0, 0.0);
        push(f1, n1, 0.0);
        push(b1, n1, 0.0);

        // Back cap (normal +Y).
        push([0.0, y_back, 0.0], [0.0, 1.0, 0.0], 0.0);
        push(b1, [0.0, 1.0, 0.0], 0.0);
        push(b0, [0.0, 1.0, 0.0], 0.0);

        // Front cap / lens (normal -Y, emissive).
        push([0.0, y_front, 0.0], [0.0, -1.0, 0.0], 1.0);
        push(f0, [0.0, -1.0, 0.0], 1.0);
        push(f1, [0.0, -1.0, 0.0], 1.0);
    }
    verts
}

/// A cone whose apex is at the local origin and whose base circle of radius
/// `radius` sits at `y = -length` (beam points down `-Y`). Kept for non-PAR
/// fixture geometry. Smooth side normals, flat base.
pub fn cone(length: f32, radius: f32, segments: u32) -> Vec<MeshVertex> {
    let seg = segments.max(3);
    let mut verts = Vec::with_capacity(seg as usize * 6);

    let base_y = -length;
    let wall_normal = |a: f32| normalize3([length * a.cos(), radius, length * a.sin()]);
    let ring = |a: f32| [radius * a.cos(), base_y, radius * a.sin()];
    let apex = [0.0, 0.0, 0.0];

    for i in 0..seg {
        let a0 = i as f32 / seg as f32 * TAU;
        let a1 = (i + 1) as f32 / seg as f32 * TAU;
        let amid = 0.5 * (a0 + a1);

        verts.push(MeshVertex {
            position: apex,
            normal: wall_normal(amid),
            emissive: 0.0,
        });
        verts.push(MeshVertex {
            position: ring(a0),
            normal: wall_normal(a0),
            emissive: 0.0,
        });
        verts.push(MeshVertex {
            position: ring(a1),
            normal: wall_normal(a1),
            emissive: 0.0,
        });

        let down = [0.0, -1.0, 0.0];
        verts.push(MeshVertex {
            position: [0.0, base_y, 0.0],
            normal: down,
            emissive: 0.0,
        });
        verts.push(MeshVertex {
            position: ring(a1),
            normal: down,
            emissive: 0.0,
        });
        verts.push(MeshVertex {
            position: ring(a0),
            normal: down,
            emissive: 0.0,
        });
    }
    verts
}

/// A unit disc in the local XY plane (radius 1, normal +Z), as a triangle fan of
/// non-indexed triangles. `position.xy` doubles as a `-1..1` radial coordinate
/// the lens shader uses for the glass/dust look. Oriented by its instance matrix.
pub fn disc(segments: u32) -> Vec<MeshVertex> {
    let seg = segments.max(8);
    let mut verts = Vec::with_capacity(seg as usize * 3);
    let n = [0.0, 0.0, 1.0];
    for i in 0..seg {
        let a0 = i as f32 / seg as f32 * TAU;
        let a1 = (i + 1) as f32 / seg as f32 * TAU;
        verts.push(MeshVertex { position: [0.0, 0.0, 0.0], normal: n, emissive: 1.0 });
        verts.push(MeshVertex { position: [a0.cos(), a0.sin(), 0.0], normal: n, emissive: 1.0 });
        verts.push(MeshVertex { position: [a1.cos(), a1.sin(), 0.0], normal: n, emissive: 1.0 });
    }
    verts
}

/// Generate the ground grid (on `y = 0`) plus the three world axes as a
/// LineList. Grid lines are dim grey; axes are RGB.
pub fn grid_and_axes(half_extent: f32, step: f32) -> Vec<LineVertex> {
    let mut verts = Vec::new();
    let grid_color = [0.28, 0.30, 0.34];
    let major_color = [0.40, 0.43, 0.48];

    let count = (half_extent / step).floor() as i32;
    for i in -count..=count {
        let p = i as f32 * step;
        let color = if i == 0 { major_color } else { grid_color };
        // Lifted a hair above the floor plane to avoid z-fighting.
        verts.push(LineVertex { position: [p, 0.002, -half_extent], color });
        verts.push(LineVertex { position: [p, 0.002, half_extent], color });
        verts.push(LineVertex { position: [-half_extent, 0.002, p], color });
        verts.push(LineVertex { position: [half_extent, 0.002, p], color });
    }

    let axis_len = (half_extent * 0.25).max(1.5);
    let y = 0.003;
    let x_axis = [0.90, 0.25, 0.25];
    let y_axis = [0.35, 0.85, 0.35];
    let z_axis = [0.30, 0.55, 0.95];
    verts.push(LineVertex { position: [0.0, y, 0.0], color: x_axis });
    verts.push(LineVertex { position: [axis_len, y, 0.0], color: x_axis });
    verts.push(LineVertex { position: [0.0, y, 0.0], color: y_axis });
    verts.push(LineVertex { position: [0.0, axis_len, 0.0], color: y_axis });
    verts.push(LineVertex { position: [0.0, y, 0.0], color: z_axis });
    verts.push(LineVertex { position: [0.0, y, axis_len], color: z_axis });

    verts
}

/// Append the 12 edges of an axis-aligned box (`min`..`max`) to `out` as lines.
pub fn push_box_wireframe(out: &mut Vec<LineVertex>, min: [f32; 3], max: [f32; 3], color: [f32; 3]) {
    let corner = |xi: usize, yi: usize, zi: usize| LineVertex {
        position: [
            if xi == 0 { min[0] } else { max[0] },
            if yi == 0 { min[1] } else { max[1] },
            if zi == 0 { min[2] } else { max[2] },
        ],
        color,
    };
    // Edges along X, Y, Z for each of the 4 spanning positions.
    for &(y, z) in &[(0, 0), (1, 0), (0, 1), (1, 1)] {
        out.push(corner(0, y, z));
        out.push(corner(1, y, z));
    }
    for &(x, z) in &[(0, 0), (1, 0), (0, 1), (1, 1)] {
        out.push(corner(x, 0, z));
        out.push(corner(x, 1, z));
    }
    for &(x, y) in &[(0, 0), (1, 0), (0, 1), (1, 1)] {
        out.push(corner(x, y, 0));
        out.push(corner(x, y, 1));
    }
}
