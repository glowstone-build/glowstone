//! The world / environment HDRI texture.
//!
//! An equirectangular environment map decoded to `Rgba16Float` (filterable) with
//! a CPU-built box-filter mip chain. Mip 0 is sampled sharply for the sky
//! background; a high (blurred) mip approximates diffuse irradiance for the
//! mesh image-based ambient. The U axis wraps (azimuth), V clamps (poles).

use half::f16;

/// Largest equirect width we keep on the GPU (height = width/2). Caps memory; the
/// blurred IBL mip is tiny anyway and the sky is low-frequency.
const MAX_W: u32 = 1024;

pub struct WorldTexture {
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

impl WorldTexture {
    /// A 1×1 white placeholder — bound when no HDRI is loaded (the shaders gate on
    /// the has-HDRI flag, so it is never actually shown).
    pub fn placeholder(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::build(device, queue, 1, 1, vec![[1.0f32, 1.0, 1.0, 1.0]])
    }

    /// Decode equirectangular image bytes (`.hdr` / `.png` / `.jpg`) into the GPU
    /// texture. Returns `None` if the image can't be decoded.
    pub fn from_bytes(device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> Option<Self> {
        let mut img = image::load_from_memory(bytes).ok()?;
        if img.width() > MAX_W {
            let h = (MAX_W / 2).max(1);
            img = img.resize_exact(MAX_W, h, image::imageops::FilterType::Triangle);
        }
        let rgba = img.to_rgba32f();
        let (w, h) = rgba.dimensions();
        let px: Vec<[f32; 4]> = rgba.pixels().map(|p| p.0).collect();
        Some(Self::build(device, queue, w, h, px))
    }

    /// Build the texture + full mip chain from mip-0 RGBA f32 pixels.
    fn build(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, mip0: Vec<[f32; 4]>) -> Self {
        let mip_count = 32 - (w.max(h)).leading_zeros(); // floor(log2(max))+1
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("world-hdri"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let mut cur = mip0;
        let (mut cw, mut ch) = (w, h);
        for mip in 0..mip_count {
            let half: Vec<[f16; 4]> = cur
                .iter()
                .map(|p| [f16::from_f32(p[0]), f16::from_f32(p[1]), f16::from_f32(p[2]), f16::from_f32(p[3])])
                .collect();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&half),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(cw * 8), // 4 × f16
                    rows_per_image: Some(ch),
                },
                wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
            );
            if mip + 1 < mip_count {
                let (nw, nh) = ((cw / 2).max(1), (ch / 2).max(1));
                cur = box_downsample(&cur, cw, ch, nw, nh);
                cw = nw;
                ch = nh;
            }
        }

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,      // azimuth wraps
            address_mode_v: wgpu::AddressMode::ClampToEdge, // poles clamp
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        Self { view, sampler }
    }
}

/// 2×2 box downsample of an RGBA f32 image to `(nw, nh)`.
fn box_downsample(src: &[[f32; 4]], w: u32, h: u32, nw: u32, nh: u32) -> Vec<[f32; 4]> {
    let mut out = vec![[0.0f32; 4]; (nw * nh) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0.0f32; 4];
            let mut n = 0.0;
            for dy in 0..2 {
                for dx in 0..2 {
                    let sx = (x * 2 + dx).min(w - 1);
                    let sy = (y * 2 + dy).min(h - 1);
                    let s = src[(sy * w + sx) as usize];
                    for c in 0..4 {
                        acc[c] += s[c];
                    }
                    n += 1.0;
                }
            }
            out[(y * nw + x) as usize] = [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n];
        }
    }
    out
}
