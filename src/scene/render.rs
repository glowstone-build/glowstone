//! Render-job configuration — the "Render Properties" the World root exposes in
//! the Inspector (mirroring Blender's Render + Output property tabs, and the
//! Redshift reference layout). Separate from [`RenderSettings`](super::RenderSettings),
//! which is the *live viewport* look: this is the recipe for a deliberate
//! still-image render (chosen resolution, sample budget, output file).
//!
//! It lives on [`Scene`](super::Scene) and is persisted with the show, so a saved
//! project keeps its render setup. (The genuinely machine/session-specific knobs —
//! `render_scale`, `shadow_max`, `froxel_volumetric` — are the `#[serde(skip)]` ones
//! that default on every load instead.)

use super::{RenderSettings, ViewportMode};

/// Output image file format for a saved render.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum RenderFormat {
    /// Lossless 8-bit RGBA — the default, universally viewable.
    #[default]
    Png,
    /// Lossy 8-bit RGB — small files for quick shares (no alpha).
    Jpeg,
    /// 32-bit float OpenEXR. NOTE: the readback is the post-tonemap LDR plate, so
    /// the floats are tonemapped/sRGB in `[0,1]` (an EXR container, not raw linear
    /// HDR) — convenient for a float-pipeline compositor, not for highlight recovery.
    Exr,
}

impl RenderFormat {
    pub const ALL: [RenderFormat; 3] = [Self::Png, Self::Jpeg, Self::Exr];

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Exr => "OpenEXR",
        }
    }

    /// File extension (no dot).
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Exr => "exr",
        }
    }
}

/// Where a finished render is shown (Blender's Output ▸ Display dropdown). We
/// route everything to the in-app Render tab; `NewWindow` is a greyed stub.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum RenderDisplay {
    /// Show the result in the dockable Render tab (auto-focused on render).
    #[default]
    RenderTab,
    /// Open a separate OS window (not implemented — greyed).
    NewWindow,
    /// Render into the buffer without switching focus.
    KeepUi,
}

impl RenderDisplay {
    pub const ALL: [RenderDisplay; 3] = [Self::RenderTab, Self::NewWindow, Self::KeepUi];

    pub fn label(self) -> &'static str {
        match self {
            Self::RenderTab => "Render Tab",
            Self::NewWindow => "New Window",
            Self::KeepUi => "Keep Interface",
        }
    }

    /// Whether this display mode is wired up (the others are greyed stubs).
    pub fn enabled(self) -> bool {
        !matches!(self, Self::NewWindow)
    }
}

/// Sampling quality preset — sets the progressive sample budget + volumetric
/// step count in one click (Blender's Render ▸ Sampling presets). `Custom` lets
/// the two numbers be dialled independently.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum QualityPreset {
    #[default]
    Draft,
    Medium,
    Final,
    Custom,
}

impl QualityPreset {
    pub const ALL: [QualityPreset; 4] = [Self::Draft, Self::Medium, Self::Final, Self::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Medium => "Medium",
            Self::Final => "Final",
            Self::Custom => "Custom",
        }
    }

    /// `(max_samples, volumetric_steps)` for a preset, or `None` for `Custom`.
    /// The sample count is how many static-camera accumulation passes the
    /// temporal volumetric EMA converges over — more = cleaner beams.
    pub fn params(self) -> Option<(u32, u32)> {
        match self {
            Self::Draft => Some((16, 64)),
            Self::Medium => Some((48, 96)),
            Self::Final => Some((96, 160)),
            Self::Custom => None,
        }
    }
}

/// The **render-target** recipe, edited in the World ▸ Render Properties inspector.
///
/// This holds only what's SPECIFIC to a deliberate still render (resolution,
/// sampling budget, output, render-side quality). Following Blender, the *look*
/// (exposure / bloom / beam / gobo / chroma + the world + fog) is SHARED with the
/// live viewport — it lives on [`RenderSettings`] / the [`Scene`] and the render
/// inherits it, so the preview matches the render. Persisted in the `.glow`
/// project (serde-derived) so a saved show keeps its render setup.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RenderConfig {
    // --- Output ---
    /// Base render width / height, pixels (before the percentage scale).
    pub res_x: u32,
    pub res_y: u32,
    /// RENDER resolution scale percentage (Blender's `resolution_percentage`): the
    /// render runs at `res * percentage/100`. Default 100% (the viewport has its own,
    /// lower, scale — see [`RenderSettings::render_scale`]).
    pub resolution_percentage: u32,
    pub format: RenderFormat,
    /// Output file path (may be empty — Save-to-Disk then prompts).
    pub out_path: String,
    /// "Render to image file": write the result to `out_path` on completion.
    pub write_to_disk: bool,
    pub display: RenderDisplay,
    /// Film transparency (alpha=0 background). NOT yet wired through the tonemap
    /// pass — greyed in the UI; kept for parity + a future implementation.
    pub transparent: bool,

    // --- Sampling (render-side; the viewport has its own quality knobs) ---
    pub quality: QualityPreset,
    /// Progressive accumulation target (how many converging passes to render).
    pub max_samples: u32,
    /// Volumetric raymarch step budget for the render pass.
    pub volumetric_steps: u32,

    // --- Performance (render-side) ---
    /// Max hero (per-beam) shadow maps for the render.
    pub shadow_max: u32,
    /// Multi-emitter WASH beam detail for the RENDER (separate from the viewport's
    /// [`RenderSettings::wash_beam_lod`]): max volumetric shaft beams per wash/array
    /// fixture. A still render can afford more than the live preview.
    #[serde(default = "default_render_wash_beam_lod")]
    pub wash_beam_lod: u32,
    /// Use the froxel grid for wide/dim beams (perf on huge rigs).
    pub froxel_volumetric: bool,
    /// Draw the origin grid + world axes in the render (off for a clean plate).
    pub show_overlays: bool,

    // --- Not-yet-implemented (greyed stubs, kept for parity with Blender) ---
    pub motion_blur: bool,
    pub caustics: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            res_x: 1920,
            res_y: 1080,
            // The RENDER is full resolution by default; the viewport previews at a
            // lower scale (RenderSettings::render_scale defaults to 50%).
            resolution_percentage: 100,
            format: RenderFormat::Png,
            out_path: String::new(),
            write_to_disk: false,
            display: RenderDisplay::RenderTab,
            transparent: false,
            quality: QualityPreset::Medium,
            max_samples: 48,
            volumetric_steps: 96,
            shadow_max: 8,
            wash_beam_lod: default_render_wash_beam_lod(),
            froxel_volumetric: false,
            show_overlays: false,
            motion_blur: false,
            caustics: false,
        }
    }
}

/// A still render can spend more shaft beams per wash than the live preview (no
/// frame-rate pressure), so the render default is higher than the viewport's.
fn default_render_wash_beam_lod() -> u32 {
    32
}

impl RenderConfig {
    /// Final pixel resolution after the percentage scale, clamped to a sane GPU
    /// range (so a stray 0% or 9000px entry can't allocate an invalid target).
    pub fn output_size(&self) -> (u32, u32) {
        let s = (self.resolution_percentage.clamp(10, 400)) as f32 / 100.0;
        let w = ((self.res_x as f32 * s).round() as u32).clamp(16, 8192);
        let h = ((self.res_y as f32 * s).round() as u32).clamp(16, 8192);
        (w, h)
    }

    /// The [`RenderSettings`] the render pass uses: the SHARED viewport look (so the
    /// render matches the preview) with the render-side quality overridden — full
    /// render scale (the percentage already chose the target size), the render's
    /// step budget / shadow cap / froxel choice, and overlays/gizmos off for a clean
    /// plate. Always a Beauty render so the temporal EMA accumulates.
    pub fn render_settings(&self, viewport: &RenderSettings) -> RenderSettings {
        RenderSettings {
            // Shared look — inherited from the live viewport.
            exposure: viewport.exposure,
            bloom: viewport.bloom,
            beam_intensity: viewport.beam_intensity,
            gobo_sharpness: viewport.gobo_sharpness,
            chroma_haze: viewport.chroma_haze,
            // Render uses its OWN (typically higher) wash beam budget.
            wash_beam_lod: self.wash_beam_lod,
            // Render-side quality overrides.
            steps: self.volumetric_steps,
            froxel_volumetric: self.froxel_volumetric,
            shadow_max: self.shadow_max,
            // Render at native — the percentage scale already chose the target size,
            // so the perf-downscale lever must be 1.0 (NOT the viewport's scale), and
            // a still render never dynamically rescales.
            render_scale: 1.0,
            auto_resolution: false,
            fps_target: viewport.fps_target,
            show_beam_wireframes: false,
            show_grid: self.show_overlays,
            mode: ViewportMode::Beauty,
            axis_hint: None,
            // A still render is a clean plate: no fog-box border / gizmos / cursor
            // (unless the user explicitly asked for overlays).
            show_gizmos: self.show_overlays,
        }
    }

    /// Push the active preset's sample/step counts into the fields (no-op for
    /// `Custom`). Call after the user picks a preset.
    pub fn apply_quality(&mut self) {
        if let Some((samples, steps)) = self.quality.params() {
            self.max_samples = samples;
            self.volumetric_steps = steps;
        }
    }
}
