//! The offscreen render targets for the 3D viewport.
//!
//! The scene + volumetrics render into an **HDR** color target (`Rgba16Float`)
//! so bright beams don't clip; a post chain (bloom + tonemap) resolves that into
//! an **LDR** `Rgba8Unorm` target, which is what egui samples and shows in the
//! Viewport panel (egui requires a non-sRGB `Rgba8Unorm` user texture, and
//! treats its texels as gamma-encoded — so the resolve writes sRGB).
//!
//! Everything is sized to the panel's pixel size and resized lazily.

use egui::TextureId;

/// One color texture + its default view.
struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    fn new(
        device: &wgpu::Device,
        label: &str,
        size: (u32, u32),
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

pub struct Viewport {
    hdr: Target,
    depth: Target,
    ldr: Target,
    bloom: [Target; 2],
    vol: Target,
    /// Ping-pong half-res targets holding the TEMPORALLY-ACCUMULATED (EMA) volumetric
    /// — the raymarch writes `vol` (raw, jittered), the temporal resolve blends it with
    /// the reprojected previous EMA into the current one, and the composite reads that.
    vol_ema: [Target; 2],
    pub size: (u32, u32),
    /// The egui handle for the LDR target, stable across resizes.
    pub texture_id: TextureId,
}

impl Viewport {
    /// Forward + volumetric render target (linear HDR).
    pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
    pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
    /// `register_native_texture` requires a non-sRGB `Rgba8Unorm` target.
    pub const LDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
    /// Half-res volumetric target: scatter.rgb + transmittance.a.
    pub const VOL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

    pub fn new(
        device: &wgpu::Device,
        egui_renderer: &mut egui_wgpu::Renderer,
        size: (u32, u32),
    ) -> Self {
        let size = clamp_size(size, device.limits().max_texture_dimension_2d);
        // Diagnostic: confirm whether the very first target allocation succeeds.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (hdr, depth, ldr, bloom, vol, vol_ema) = Self::create_targets(device, size);
        match pollster::block_on(scope.pop()) {
            Some(err) => log::error!("Viewport::new alloc FAILED at {size:?}: {err}"),
            None => log::info!("Viewport::new alloc OK at {size:?}"),
        }
        let texture_id =
            egui_renderer.register_native_texture(device, &ldr.view, wgpu::FilterMode::Linear);
        Self {
            hdr,
            depth,
            ldr,
            bloom,
            vol,
            vol_ema,
            size,
            texture_id,
        }
    }

    /// Recreate the targets at a new size, rebinding the same egui `TextureId`.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        egui_renderer: &mut egui_wgpu::Renderer,
        size: (u32, u32),
    ) {
        let size = clamp_size(size, device.limits().max_texture_dimension_2d);
        if size == self.size {
            return;
        }
        // Allocate inside an error scope so an allocation/validation failure (e.g.
        // the device already lost to an NVIDIA TDR reset) is CAPTURED and logged
        // with the offending size, instead of going to the default fatal handler
        // and aborting on the subsequent `create_view`. On failure we keep the old
        // (valid) targets and skip this resize — far better than crashing.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let targets = Self::create_targets(device, size);
        if let Some(err) = pollster::block_on(scope.pop()) {
            log::error!(
                "viewport target alloc failed at {size:?} (keeping {:?}): {err}",
                self.size
            );
            return;
        }
        log::debug!("viewport resized to {size:?}");
        let (hdr, depth, ldr, bloom, vol, vol_ema) = targets;
        self.hdr = hdr;
        self.depth = depth;
        self.ldr = ldr;
        self.bloom = bloom;
        self.vol = vol;
        self.vol_ema = vol_ema;
        self.size = size;
        egui_renderer.update_egui_texture_from_wgpu_texture(
            device,
            &self.ldr.view,
            wgpu::FilterMode::Linear,
            self.texture_id,
        );
    }

    pub fn hdr_view(&self) -> &wgpu::TextureView {
        &self.hdr.view
    }
    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth.view
    }
    pub fn ldr_view(&self) -> &wgpu::TextureView {
        &self.ldr.view
    }
    pub fn ldr_texture(&self) -> &wgpu::Texture {
        &self.ldr.texture
    }
    pub fn bloom_view(&self, i: usize) -> &wgpu::TextureView {
        &self.bloom[i].view
    }
    pub fn vol_view(&self) -> &wgpu::TextureView {
        &self.vol.view
    }
    /// One of the two ping-pong temporally-accumulated volumetric targets.
    pub fn vol_ema_view(&self, i: usize) -> &wgpu::TextureView {
        &self.vol_ema[i & 1].view
    }

    pub fn aspect(&self) -> f32 {
        self.size.0 as f32 / self.size.1.max(1) as f32
    }

    #[allow(clippy::type_complexity)]
    fn create_targets(
        device: &wgpu::Device,
        size: (u32, u32),
    ) -> (Target, Target, Target, [Target; 2], Target, [Target; 2]) {
        let attach_sample =
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;

        let hdr = Target::new(device, "viewport-hdr", size, Self::HDR_FORMAT, attach_sample);
        let depth = Target::new(
            device,
            "viewport-depth",
            size,
            Self::DEPTH_FORMAT,
            attach_sample,
        );
        let ldr = Target::new(
            device,
            "viewport-ldr",
            size,
            Self::LDR_FORMAT,
            attach_sample | wgpu::TextureUsages::COPY_SRC,
        );

        let half = ((size.0 / 2).max(1), (size.1 / 2).max(1));
        let bloom = [
            Target::new(device, "bloom-0", half, Self::HDR_FORMAT, attach_sample),
            Target::new(device, "bloom-1", half, Self::HDR_FORMAT, attach_sample),
        ];
        // Half-res volumetric (4x fewer rays). The shader samples the nearest
        // opaque depth over each ray's footprint so the beam still stops
        // cleanly at edges (no bleeding past the floor).
        let vol = Target::new(device, "viewport-vol", half, Self::VOL_FORMAT, attach_sample);
        let vol_ema = [
            Target::new(device, "viewport-vol-ema0", half, Self::VOL_FORMAT, attach_sample),
            Target::new(device, "viewport-vol-ema1", half, Self::VOL_FORMAT, attach_sample),
        ];

        (hdr, depth, ldr, bloom, vol, vol_ema)
    }
}

/// Clamp a requested target size to `[1, max]` per axis. The upper bound matters:
/// egui can momentarily report an unconstrained (huge or infinite) `available_size`
/// during a transient layout pass around a resize/maximize, and `INFINITY as u32`
/// saturates to `u32::MAX` — which would make `create_texture` produce an invalid
/// texture and abort. `max` is the device's `max_texture_dimension_2d`.
fn clamp_size(size: (u32, u32), max: u32) -> (u32, u32) {
    (size.0.clamp(1, max), size.1.clamp(1, max))
}
