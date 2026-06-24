//! An LED video wall: a parametric grid of cabinets driven by ONE content
//! source, sampled crisply onto a single emissive surface.
//!
//! Design (see `docs/RESEARCH-led-ndi.md`): the wall surface is **one texture /
//! one emissive quad** in the renderer — emphatically NOT the multi-emitter
//! [`Fixture`](super::fixture::Fixture) `cells` / per-cell DMX path (recomposing
//! a 1080p/4K wall per-pixel every frame is the "-69fps" trap). Up close the
//! shader reveals the individual LED pixels via a screen-space-derivative dot
//! mask, so zooming in looks like a real panel. The wall still **emits a little
//! light and feeds the volumetric haze**, but that contribution is a
//! deliberately cheap, heavily-blurred summary (an average colour driving a soft
//! area light) computed in the renderer — never per-pixel.

use glam::{Mat4, Vec3};

use super::library::ScreenProfile;

/// How an individual LED pixel is drawn up close. Industry layouts: an **SMD**
/// (surface-mount) package combines R/G/B in one point (round or square lens),
/// while a **discrete / DIP** wall has three separate R, G, B emitters visible as
/// vertical sub-pixel stripes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PixelShape {
    /// SMD, round lens — a single combined dot (the default).
    SmdRound,
    /// SMD, square lens — a single combined square.
    SmdSquare,
    /// Discrete R/G/B sub-pixels as vertical stripes (DIP / real-pixel look).
    DiscreteRgb,
}

impl PixelShape {
    pub const ALL: [PixelShape; 3] = [Self::SmdRound, Self::SmdSquare, Self::DiscreteRgb];

    pub fn label(self) -> &'static str {
        match self {
            Self::SmdRound => "SMD round",
            Self::SmdSquare => "SMD square",
            Self::DiscreteRgb => "Discrete RGB",
        }
    }

    /// Shader code read by `wall.wgsl`.
    pub fn code(self) -> f32 {
        match self {
            Self::SmdRound => 0.0,
            Self::SmdSquare => 1.0,
            Self::DiscreteRgb => 2.0,
        }
    }
}

/// A procedural test pattern shown on a wall that has no real content yet — what
/// a real LED processor boots to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum TestPattern {
    /// Per-pixel alignment grid (crosshatch + cabinet ticks).
    Grid,
    /// SMPTE-style vertical colour bars.
    Bars,
    /// Smooth horizontal hue/luma sweep.
    Gradient,
}

impl TestPattern {
    pub const ALL: [TestPattern; 3] = [Self::Grid, Self::Bars, Self::Gradient];

    pub fn label(self) -> &'static str {
        match self {
            Self::Grid => "Grid",
            Self::Bars => "Colour Bars",
            Self::Gradient => "Gradient",
        }
    }

    /// Shader code read by `wall.wgsl`.
    pub fn code(self) -> f32 {
        match self {
            Self::Grid => 0.0,
            Self::Bars => 1.0,
            Self::Gradient => 2.0,
        }
    }
}

/// A deliberately tiny DMX control grid for a low-res creative wall (the
/// SECONDARY content path — full video comes from NDI/CITP/media). `cols * rows`
/// RGB cells are read from Art-Net/sACN starting at `universe`/`start_address`;
/// this is patched INLINE (not through `PatchTable`, not the per-cell `cells`
/// composite). See `docs/RESEARCH-led-ndi.md`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PixelMap {
    pub cols: u32,
    pub rows: u32,
    pub universe: u16,
    pub start_address: u16,
}

impl Default for PixelMap {
    fn default() -> Self {
        Self { cols: 16, rows: 9, universe: 1, start_address: 1 }
    }
}

/// What the wall surface displays. Only stable, serializable descriptors live
/// here — decoded frames / GPU textures / the blurred lighting summary are
/// runtime-only ([`LedScreen::frame`] + the renderer cache, keyed by screen
/// index). New variants go on the END so existing `.archie` files stay loadable.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScreenContent {
    /// A procedural test pattern (the default for a freshly placed wall).
    TestPattern(TestPattern),
    /// A flat linear-RGB colour across the whole surface.
    SolidColor([f32; 3]),
    /// A still image (bundled bytes, like `World.hdri`), decoded by the renderer.
    Image { name: String, bytes: std::sync::Arc<Vec<u8>> },
    /// A live NDI source — only the stable source NAME is serialized; frames
    /// arrive at runtime via [`LedScreen::frame`].
    Ndi { source: String },
    /// A live CITP/MSEX media-server stream — `"server/layer"` identifier; frames
    /// arrive at runtime via [`LedScreen::frame`].
    Citp { source: String },
    /// A low-res grid driven by Art-Net/sACN (secondary path; not real video).
    PixelMapDmx(PixelMap),
}

impl ScreenContent {
    /// A short label for the source picker / outliner subtitle.
    pub fn label(&self) -> &'static str {
        match self {
            Self::TestPattern(_) => "Test Pattern",
            Self::SolidColor(_) => "Solid Colour",
            Self::Image { .. } => "Image",
            Self::Ndi { .. } => "NDI",
            Self::Citp { .. } => "CITP",
            Self::PixelMapDmx(_) => "Pixel-map DMX",
        }
    }

}

/// A runtime content frame (tightly-packed RGBA8, top-down rows) handed to the
/// renderer for GPU upload. Produced by the app for live sources (pixel-map DMX,
/// NDI, CITP); NEVER serialized. `generation` bumps when the pixels change so the
/// renderer re-uploads only on change.
#[derive(Clone)]
pub struct ScreenFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub generation: u64,
}

/// An LED video wall placed in the scene: a parametric grid of cabinets plus the
/// look/photometry and the content source. Drawn as one emissive surface; not a
/// fixture and not an occluding mesh.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LedScreen {
    pub name: String,
    /// Library component this was built from (display / info only).
    pub panel_type: String,
    /// World placement (Y-up, metres). The surface spans the local XY plane,
    /// facing +Z, centred on the transform origin; its physical width/height come
    /// from the cabinet grid ([`size_m`](Self::size_m)). Mirrors
    /// [`SceneGeometry::transform`](super::SceneGeometry::transform).
    pub transform: Mat4,

    // --- physical cabinet grid ---
    /// One cabinet's face size (width, height) in millimetres.
    pub cabinet_mm: [f32; 2],
    /// Native pixels per cabinet (x, y), e.g. `[128, 128]`.
    pub cabinet_px: [u32; 2],
    pub panels_wide: u32,
    pub panels_high: u32,
    /// Inter-cabinet seam / bezel in millimetres (0 = seamless rental tile).
    pub gap_mm: f32,
    /// Horizontal arc bowed across the whole wall, degrees (0 = flat). Stored
    /// now; honoured by the renderer in a later phase.
    pub curvature_deg: f32,

    // --- look / photometry ---
    /// Peak brightness (nits). Scales the emissive surface and the light summary.
    pub nits: f32,
    /// Display gamma applied to content (default 2.2).
    pub gamma: f32,
    /// Surface opacity `0..1` (1 = opaque; `<1` = see-through / mesh LED).
    pub opacity: f32,
    /// Per-screen multiplier on the cheap area-light + volumetric contribution
    /// (cf. [`Fixture::beam`](super::fixture::Fixture::beam)). 0 = lights nothing.
    pub emit: f32,
    /// How an individual LED pixel is drawn up close (SMD round/square or
    /// discrete RGB sub-pixels).
    pub pixel_shape: PixelShape,
    /// Hidden in the viewport (outliner eye toggle): not drawn, not pickable.
    pub hidden: bool,
    /// Session-stable identity (serde-skip → reassigned by `Scene::ensure_ids`
    /// on load). The Scene outliner keys rows by this; never serialized.
    #[serde(skip)]
    pub id: super::EntityId,

    /// What the surface displays.
    pub content: ScreenContent,

    /// Runtime content frame for live sources (pixel-map DMX / NDI / CITP),
    /// produced by the app each frame and uploaded by the renderer. Never
    /// serialized; `None` for procedural / image content.
    #[serde(skip)]
    pub frame: Option<std::sync::Arc<ScreenFrame>>,
}

impl LedScreen {
    /// Build a wall from a library component at `transform` (a default 4×2 array).
    pub fn from_profile(
        profile: &ScreenProfile,
        name: impl Into<String>,
        transform: Mat4,
    ) -> Self {
        Self {
            name: name.into(),
            panel_type: profile.name.to_string(),
            transform,
            cabinet_mm: profile.cabinet_mm,
            cabinet_px: profile.cabinet_px,
            panels_wide: 4,
            panels_high: 2,
            gap_mm: profile.gap_mm,
            curvature_deg: 0.0,
            nits: profile.default_nits,
            gamma: 2.2,
            opacity: if profile.transparent { 0.35 } else { 1.0 },
            emit: 1.0,
            pixel_shape: PixelShape::SmdRound,
            hidden: false,
            id: 0, // assigned by Scene::add_screen / ensure_ids
            content: ScreenContent::TestPattern(TestPattern::Grid),
            frame: None,
        }
    }

    /// Total canvas resolution (px) = panels × native px/cabinet.
    pub fn resolution(&self) -> [u32; 2] {
        [
            self.panels_wide * self.cabinet_px[0],
            self.panels_high * self.cabinet_px[1],
        ]
    }

    /// Physical wall size (width, height) in metres, including inter-cabinet gaps.
    pub fn size_m(&self) -> [f32; 2] {
        let g = self.gap_mm;
        let w = self.panels_wide as f32 * self.cabinet_mm[0]
            + self.panels_wide.saturating_sub(1) as f32 * g;
        let h = self.panels_high as f32 * self.cabinet_mm[1]
            + self.panels_high.saturating_sub(1) as f32 * g;
        [w / 1000.0, h / 1000.0]
    }

    /// Pixel pitch (mm) implied by the cabinet size / native px (source of truth
    /// is `cabinet_px`).
    pub fn pitch_mm(&self) -> f32 {
        self.cabinet_mm[0] / self.cabinet_px[0].max(1) as f32
    }

    /// World matrix for the surface quad: `transform` scaled to the physical
    /// size, so a unit quad spanning local XY `[-0.5, 0.5]²` lands as the wall.
    /// The local Z is scaled by the width too, so the shader's curvature bow
    /// (in width-fraction units) comes out proportional to the wall size.
    pub fn surface_matrix(&self) -> Mat4 {
        let [w, h] = self.size_m();
        let w = w.max(1e-3);
        self.transform * Mat4::from_scale(Vec3::new(w, h.max(1e-3), w))
    }

    /// World-space centre of the surface.
    pub fn world_center(&self) -> Vec3 {
        self.transform.transform_point3(Vec3::ZERO)
    }

    /// World-space surface normal (the emitting face, local +Z).
    pub fn world_normal(&self) -> Vec3 {
        self.transform.transform_vector3(Vec3::Z).normalize_or_zero()
    }

    /// World-space AABB of the four surface corners — for outliner framing and a
    /// coarse pick fallback.
    pub fn world_bounds(&self) -> (Vec3, Vec3) {
        let [w, h] = self.size_m();
        let (hw, hh) = (w * 0.5, h * 0.5);
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for sx in [-hw, hw] {
            for sy in [-hh, hh] {
                let p = self.transform.transform_point3(Vec3::new(sx, sy, 0.0));
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        (lo, hi)
    }

    /// Nearest positive ray–surface intersection distance (oriented quad), if the
    /// ray crosses the wall rectangle. The parameter `t` is in world units, so it
    /// is comparable with the other pick distances.
    pub fn ray_hit(&self, ro: Vec3, rd: Vec3) -> Option<f32> {
        let inv = self.surface_matrix().inverse();
        let lo = inv.transform_point3(ro);
        let ld = inv.transform_vector3(rd);
        if ld.z.abs() < 1e-9 {
            return None;
        }
        let t = -lo.z / ld.z;
        if t <= 0.0 {
            return None;
        }
        let h = lo + ld * t;
        (h.x.abs() <= 0.5 && h.y.abs() <= 0.5).then_some(t)
    }

    /// The blurred "summary" colour that drives the cheap area-light /
    /// volumetric contribution — a single average of the content, never
    /// per-pixel. Solid colour returns itself; a test pattern returns a fixed
    /// representative mid-grey (the renderer can refine this per phase).
    /// Linear-RGB content colour at surface UV `(u, v)` (`v = 0` bottom, `1` top)
    /// for the PROCEDURAL sources (solid + test patterns) — used to colour the
    /// per-region area-light samples so a gradient/bars wall throws the right
    /// colours into the haze. Image / live sources are sampled from their decoded
    /// frame in the renderer instead.
    pub fn sample_content(&self, u: f32, v: f32) -> [f32; 3] {
        match &self.content {
            ScreenContent::SolidColor(c) => *c,
            ScreenContent::TestPattern(p) => match p {
                // Grid is mostly a dark field — a dim blue wash.
                TestPattern::Grid => [0.05, 0.10, 0.20],
                TestPattern::Bars => bar_color(u),
                TestPattern::Gradient => {
                    let rgb = hsv_to_rgb(u, 0.9, 1.0);
                    let b = 0.12 + 0.88 * v.clamp(0.0, 1.0);
                    [rgb[0] * b, rgb[1] * b, rgb[2] * b]
                }
            },
            // Image / NDI / CITP / pixel-map: the renderer uses the decoded frame.
            _ => [0.4, 0.4, 0.4],
        }
    }
}

/// SMPTE-ish 7-bar colour at horizontal UV `u` (matches `wall.wgsl::color_bars`).
fn bar_color(u: f32) -> [f32; 3] {
    let i = (u.clamp(0.0, 0.999) * 7.0) as i32;
    let l = 0.75;
    match i {
        0 => [l, l, l],
        1 => [l, l, 0.0],
        2 => [0.0, l, l],
        3 => [0.0, l, 0.0],
        4 => [l, 0.0, l],
        5 => [l, 0.0, 0.0],
        _ => [0.0, 0.0, l],
    }
}

/// HSV→RGB with `h` in `0..1` (matches `wall.wgsl::hsv2rgb`).
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    let k = [5.0f32, 3.0, 1.0];
    for c in 0..3 {
        let p = ((h + k[c] / 6.0).fract() * 6.0 - 3.0).abs();
        out[c] = v * (1.0 - s) + v * s * (p - 1.0).clamp(0.0, 1.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::library::Library;

    fn wall() -> LedScreen {
        let lib = Library::standard();
        // "Indoor 3.9mm": 500×500 mm cabinet, 128×128 px.
        let p = lib.screens.iter().find(|p| p.name == "Indoor 3.9mm").unwrap();
        LedScreen::from_profile(p, "W", Mat4::IDENTITY)
    }

    #[test]
    fn derived_resolution_and_size() {
        let mut s = wall();
        s.panels_wide = 12;
        s.panels_high = 6;
        // resolution = panels × px/cabinet
        assert_eq!(s.resolution(), [12 * 128, 6 * 128]);
        // physical size = panels × cabinet_mm, in metres (no gap)
        let [w, h] = s.size_m();
        assert!((w - 6.0).abs() < 1e-4); // 12 × 0.5 m
        assert!((h - 3.0).abs() < 1e-4); // 6 × 0.5 m
        // pitch is derived from cabinet size / native px
        assert!((s.pitch_mm() - 500.0 / 128.0).abs() < 1e-3);
    }

    #[test]
    fn ray_hits_front_face_and_misses_outside() {
        // A 4×2 default wall (2.0 × 1.0 m) at the origin, facing +Z.
        let s = wall();
        // Ray from +Z straight at the centre → hits at z-distance 5.
        let t = s.ray_hit(Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, -1.0)).expect("centre hit");
        assert!((t - 5.0).abs() < 1e-3);
        // Ray well outside the rectangle (x = 10 m) → miss.
        assert!(s.ray_hit(Vec3::new(10.0, 0.0, 5.0), Vec3::new(0.0, 0.0, -1.0)).is_none());
        // Ray pointing away from the wall → miss.
        assert!(s.ray_hit(Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, 1.0)).is_none());
    }
}
