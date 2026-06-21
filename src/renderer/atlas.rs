//! The gobo / animation-glass texture atlas.
//!
//! A single persistent `texture_2d_array` (full mip chain) holds every
//! projectable wheel of every loaded GDTF fixture. Layer 0 is solid white
//! ("open / no gobo"). A **gobo / animation** wheel gets one image layer per
//! slot (a consecutive block). A **colour** wheel is packed into ONE layer as
//! vertical colour bands (one per slot) and sampled horizontally by slot — so a
//! 60-slot virtual colour wheel costs a single layer, not 60.
//!
//! Prism wheels (CPU beam-expansion) are *not* allocated here. Mips are built on
//! the CPU (box filter) so the focus/frost mip-LOD blur has something to read.

use std::collections::HashMap;
use std::sync::Arc;

use crate::gdtf::GdtfFixture;

/// Edge length of each atlas layer (square). 512 gives the projected cookie 2×
/// angular resolution before the GPU mip chain takes over — and a real signal
/// for the runtime CAS sharpener. (RGBA8 × LAYERS × full mips ≈ 192 MB @ 48.)
const RES: u32 = 512;
/// Total layers the array can hold (white + every gobo SLOT + one strip layer
/// per colour wheel, across all loaded fixture types). Colour wheels now cost a
/// single layer each (see `color_strip`), so gobos dominate; 128 covers large
/// multi-type rigs without exceeding the GPU's per-texture budget.
const LAYERS: u32 = 128;

pub struct GoboAtlas {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// Mip-filtering clamp sampler (gobo edges read black past `[0,1]`).
    pub sampler: wgpu::Sampler,
    mip_count: u32,
    next_layer: u32,
    /// `(gdtf Arc ptr, wheel name) -> base layer` for projectable wheels.
    base_of: HashMap<(usize, String), u32>,
}

impl GoboAtlas {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let mip_count = 32 - (RES.leading_zeros()) ; // floor(log2(RES)) + 1
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gobo-atlas"),
            size: wgpu::Extent3d {
                width: RES,
                height: RES,
                depth_or_array_layers: LAYERS,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("gobo-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let mut atlas = Self {
            texture,
            view,
            sampler,
            mip_count,
            next_layer: 0,
            base_of: HashMap::new(),
        };
        // Layer 0: solid white (open / no gobo).
        let white = vec![255u8; (RES * RES * 4) as usize];
        atlas.write_layer(queue, 0, &white);
        atlas.next_layer = 1;
        atlas
    }

    pub fn layer_count(&self) -> u32 {
        LAYERS
    }

    /// Base layer of a projectable wheel for a loaded fixture, if allocated.
    pub fn base_layer(&self, key: usize, wheel: &str) -> Option<u32> {
        self.base_of.get(&(key, wheel.to_string())).copied()
    }

    /// Allocate + upload the projectable wheels (gobo / animation) of a fixture
    /// the first time it is seen. Idempotent per `(key, wheel)`.
    pub fn ensure(&mut self, queue: &wgpu::Queue, key: usize, gdtf: &Arc<GdtfFixture>) {
        for wheel in &gdtf.wheels {
            let has_media = wheel.slots.iter().any(|s| s.media.is_some());
            let is_prism = wheel.slots.iter().any(|s| !s.facets.is_empty());
            let has_color = wheel.slots.iter().any(|s| s.color.is_some());
            // Bake: gobo/animation wheels (imagery) AND colour wheels (solid
            // dichroic-colour slots) — both are sampled as the physical wheel.
            // Prisms (facets) are CPU beam-expansion, not atlas slots.
            let bake = (has_media || has_color) && !is_prism;
            if !bake {
                continue;
            }
            let k = (key, wheel.name.clone());
            if self.base_of.contains_key(&k) {
                continue;
            }
            // A pure colour wheel (solid dichroic slots, no imagery) is packed
            // into ONE layer as vertical colour bands — sampled horizontally by
            // slot in the shader (`opt_color_strip`). A gobo/animation wheel still
            // gets one image layer per slot. This keeps virtual colour wheels
            // (often 60+ slots) from each eating 60+ of the 512² atlas layers.
            let is_color_wheel = !has_media;
            let count = wheel.slots.len() as u32;
            let layers_needed = if is_color_wheel { 1 } else { count };
            if self.next_layer + layers_needed > LAYERS {
                log::warn!("gobo atlas full; skipping wheel '{}'", wheel.name);
                continue;
            }
            let base = self.next_layer;
            if is_color_wheel {
                self.write_with_mips(queue, base, color_strip(&wheel.slots));
                self.next_layer += 1;
            } else {
                for (i, slot) in wheel.slots.iter().enumerate() {
                    let rgba = slot
                        .media
                        .as_deref()
                        .and_then(decode_gobo)
                        // A media wheel's slot with no image = open (white).
                        .unwrap_or_else(|| vec![255u8; (RES * RES * 4) as usize]);
                    self.write_with_mips(queue, base + i as u32, rgba);
                }
                self.next_layer += count;
            }
            self.base_of.insert(k, base);
            log::info!("atlas: wheel '{}' -> base {base} (+{layers_needed})", wheel.name);
        }
    }

    /// Write one mip-0 layer (RES×RES RGBA8).
    fn write_layer(&self, queue: &wgpu::Queue, layer: u32, rgba: &[u8]) {
        self.write_mip(queue, layer, 0, RES, rgba);
    }

    fn write_mip(&self, queue: &wgpu::Queue, layer: u32, mip: u32, dim: u32, rgba: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: mip,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(dim * 4),
                rows_per_image: Some(dim),
            },
            wgpu::Extent3d { width: dim, height: dim, depth_or_array_layers: 1 },
        );
    }

    /// Write a layer and its full box-filtered mip chain.
    fn write_with_mips(&self, queue: &wgpu::Queue, layer: u32, base_rgba: Vec<u8>) {
        let mut dim = RES;
        let mut cur = base_rgba;
        self.write_mip(queue, layer, 0, dim, &cur);
        for mip in 1..self.mip_count {
            let next_dim = (dim / 2).max(1);
            cur = box_downsample(&cur, dim, next_dim);
            dim = next_dim;
            self.write_mip(queue, layer, mip, dim, &cur);
        }
    }
}

/// A whole colour wheel packed into one RES×RES layer as `n` vertical bands,
/// one per slot, each the slot's dichroic **transmittance** triple (linear, raw
/// in the Unorm atlas so the shader multiplies it into the beam). The shader
/// samples band `slot` at horizontal `(slot+0.5)/n`. `None` slot = open/white.
/// Saturated dichroics pass less light (deep blue ≈ 9 %, amber ≈ 60 %, pale ≈
/// 100 %) per research-dichroic.md.
fn color_strip(slots: &[crate::gdtf::WheelSlot]) -> Vec<u8> {
    let n = slots.len().max(1);
    // Precompute each band's RGBA8 once.
    let bands: Vec<[u8; 4]> = slots
        .iter()
        .map(|s| {
            let t = crate::optics::color::dichroic_transmittance(s.color);
            [
                (t[0] * 255.0).round() as u8,
                (t[1] * 255.0).round() as u8,
                (t[2] * 255.0).round() as u8,
                255u8,
            ]
        })
        .collect();
    let mut out = vec![0u8; (RES * RES * 4) as usize];
    // One row of bands, then replicated down every row.
    let mut row = vec![0u8; (RES * 4) as usize];
    for x in 0..RES as usize {
        let slot = (x * n) / RES as usize;
        row[x * 4..x * 4 + 4].copy_from_slice(&bands[slot.min(n - 1)]);
    }
    for y in 0..RES as usize {
        out[y * RES as usize * 4..(y + 1) * RES as usize * 4].copy_from_slice(&row);
    }
    out
}

/// Decode a gobo PNG to RES×RES RGBA8: keep its color, set alpha to the
/// transmittance mask (luminance × source alpha) so dark/holder areas occlude.
/// Lanczos3 resize (sharper low-pass than the old tent filter) plus a mild
/// linear-space unsharp on the transmittance channel recover detail lost to the
/// low-res source; the GPU's runtime CAS finishes the in-focus edge.
fn decode_gobo(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = img
        .resize_exact(RES, RES, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let mut out = img.into_raw();
    for px in out.chunks_exact_mut(4) {
        let (r, g, b, a) = (px[0] as f32, px[1] as f32, px[2] as f32, px[3] as f32);
        let lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;
        px[3] = (lum * (a / 255.0) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    // Mild unsharp on the (linear) transmittance alpha — undo resize softness.
    unsharp_alpha(&mut out, RES, 0.5, 0.9, 2);
    Some(out)
}

/// In-place unsharp mask on the alpha (transmittance) channel of an RES×RES
/// RGBA8 buffer. Transmittance is linear data (it multiplies light), so sharpen
/// it directly. `amount` ~0.5, `sigma` ~0.9 px, `thr_u8` skips flat areas to
/// avoid amplifying decode noise. Conservative — the runtime CAS does the rest.
fn unsharp_alpha(buf: &mut [u8], dim: u32, amount: f32, sigma: f32, thr_u8: u8) {
    let n = (dim * dim) as usize;
    let a: Vec<f32> = (0..n).map(|i| buf[i * 4 + 3] as f32 / 255.0).collect();
    let blur = gaussian_blur(&a, dim, sigma);
    let thr = thr_u8 as f32 / 255.0;
    for i in 0..n {
        let c = a[i];
        let hi = c - blur[i];
        let s = if hi.abs() < thr { c } else { c + amount * hi };
        buf[i * 4 + 3] = (s.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

/// Separable Gaussian blur of a single-channel `dim`×`dim` image (clamp edges).
fn gaussian_blur(src: &[f32], dim: u32, sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil().max(1.0) as i32;
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut ksum = 0.0;
    for k in -radius..=radius {
        let w = (-(k * k) as f32 / (2.0 * sigma * sigma)).exp();
        kernel.push(w);
        ksum += w;
    }
    for w in &mut kernel {
        *w /= ksum;
    }
    let d = dim as i32;
    let at = |x: i32, y: i32, v: &[f32]| v[(y.clamp(0, d - 1) * d + x.clamp(0, d - 1)) as usize];
    // Horizontal then vertical pass.
    let mut tmp = vec![0.0f32; src.len()];
    for y in 0..d {
        for x in 0..d {
            let mut acc = 0.0;
            for (ki, w) in kernel.iter().enumerate() {
                acc += w * at(x + ki as i32 - radius, y, src);
            }
            tmp[(y * d + x) as usize] = acc;
        }
    }
    let mut out = vec![0.0f32; src.len()];
    for y in 0..d {
        for x in 0..d {
            let mut acc = 0.0;
            for (ki, w) in kernel.iter().enumerate() {
                acc += w * at(x, y + ki as i32 - radius, &tmp);
            }
            out[(y * d + x) as usize] = acc;
        }
    }
    out
}

/// Box-downsample an RGBA8 image from `src_dim`² to `dst_dim`² (dst = src/2).
fn box_downsample(src: &[u8], src_dim: u32, dst_dim: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dst_dim * dst_dim * 4) as usize];
    let scale = src_dim / dst_dim.max(1);
    for y in 0..dst_dim {
        for x in 0..dst_dim {
            for ch in 0..4 {
                let mut sum = 0u32;
                let mut n = 0u32;
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = (x * scale + sx).min(src_dim - 1);
                        let py = (y * scale + sy).min(src_dim - 1);
                        sum += src[((py * src_dim + px) * 4 + ch) as usize] as u32;
                        n += 1;
                    }
                }
                out[((y * dst_dim + x) * 4 + ch) as usize] = (sum / n.max(1)) as u8;
            }
        }
    }
    out
}
