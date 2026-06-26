//! The content library: categorized fixture and environment *definitions* you
//! can instantiate into the scene.
//!
//! This is the precursor to GDTF (fixtures) and MVR (scenes). For now it is a
//! small hand-written catalog, but the shape — categories of profiles that
//! carry default geometry/params — is what those importers will populate.

/// How a fixture body is drawn. Maps to a mesh the renderer holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum FixtureGeometry {
    /// A PAR-can-style cylinder with a glowing lens.
    Cylinder,
    /// A tapered cone (moving heads / beams).
    Cone,
}

/// A fixture definition in the library.
#[derive(Clone)]
pub struct FixtureProfile {
    pub name: &'static str,
    pub category: &'static str,
    pub geometry: FixtureGeometry,
    /// Full beam cone angle, in degrees.
    pub default_beam_angle: f32,
    /// Default emitted color, linear RGB.
    pub default_color: [f32; 3],
    /// A laser engine: rendered as a thin, near-collimated, haze-only streak
    /// (no inverse-square falloff, razor edge) rather than a lamp cone.
    pub laser: bool,
}

/// What kind of environment volume this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum EnvironmentKind {
    /// A box of participating media (haze/fog) the beams will scatter through.
    FogBox,
}

/// An environment definition in the library.
#[derive(Clone)]
pub struct EnvironmentProfile {
    pub name: &'static str,
    pub category: &'static str,
    pub kind: EnvironmentKind,
    /// Default box size in metres (width, height, depth).
    pub default_size: [f32; 3],
    /// Default extinction density (uniform for now).
    pub default_density: f32,
}

/// An LED-wall component definition: one cabinet/panel type with its native
/// resolution and photometry. A placed [`LedScreen`](crate::scene::LedScreen)
/// multiplies the cabinet into a `panels_wide × panels_high` array.
#[derive(Clone)]
pub struct ScreenProfile {
    pub name: &'static str,
    pub category: &'static str,
    /// One cabinet's face size (width, height) in millimetres.
    pub cabinet_mm: [f32; 2],
    /// Native pixels per cabinet (x, y). Pitch is `cabinet_mm / cabinet_px`.
    pub cabinet_px: [u32; 2],
    /// Inter-cabinet seam / bezel in millimetres (0 = seamless rental tile).
    pub gap_mm: f32,
    /// See-through / mesh LED (defaults to a low surface opacity).
    pub transparent: bool,
    /// Peak brightness in nits.
    pub default_nits: f32,
}

/// The whole catalog, grouped into built-in fixtures, environments, LED-wall
/// components, and imported GDTF fixture definitions.
pub struct Library {
    pub fixtures: Vec<FixtureProfile>,
    pub environments: Vec<EnvironmentProfile>,
    /// Built-in LED-wall component types (indoor / outdoor / transparent / …).
    pub screens: Vec<ScreenProfile>,
    /// GDTF fixtures imported at runtime.
    pub gdtf: Vec<std::sync::Arc<crate::gdtf::GdtfFixture>>,
}

impl Library {
    pub fn standard() -> Self {
        Self {
            fixtures: vec![
                FixtureProfile {
                    name: "PAR Can",
                    category: "Generic",
                    geometry: FixtureGeometry::Cylinder,
                    default_beam_angle: 24.0,
                    default_color: [1.0, 0.95, 0.85],
                    laser: false,
                },
                // Laser engines: near-spectral, gamut-clamped chroma (638/520/445 nm),
                // razor-thin haze-only streaks. See .context/research-color-physics.md.
                FixtureProfile {
                    name: "Laser — Red",
                    category: "Laser",
                    geometry: FixtureGeometry::Cone,
                    default_beam_angle: 0.2,
                    default_color: [1.0, 0.02, 0.0],
                    laser: true,
                },
                FixtureProfile {
                    name: "Laser — Green",
                    category: "Laser",
                    geometry: FixtureGeometry::Cone,
                    default_beam_angle: 0.2,
                    default_color: [0.18, 1.0, 0.05],
                    laser: true,
                },
                FixtureProfile {
                    name: "Laser — Blue",
                    category: "Laser",
                    geometry: FixtureGeometry::Cone,
                    default_beam_angle: 0.2,
                    default_color: [0.18, 0.03, 1.0],
                    laser: true,
                },
            ],
            environments: vec![EnvironmentProfile {
                name: "Fog Box",
                category: "Environments",
                kind: EnvironmentKind::FogBox,
                default_size: [40.0, 20.0, 40.0],
                // Light theatrical haze (extinction per metre): visible beams
                // without fogging out the whole stage.
                default_density: 0.03,
            }],
            // Generic LED-wall components with realistic spec-sheet defaults
            // (pitch = cabinet_mm / cabinet_px). See docs/RESEARCH-led-ndi.md.
            screens: vec![
                ScreenProfile {
                    name: "Indoor 3.9mm",
                    category: "LED Wall",
                    cabinet_mm: [500.0, 500.0],
                    cabinet_px: [128, 128], // 500/128 = 3.906 mm
                    gap_mm: 0.0,
                    transparent: false,
                    default_nits: 1200.0,
                },
                ScreenProfile {
                    name: "Indoor 2.6mm",
                    category: "LED Wall",
                    cabinet_mm: [500.0, 500.0],
                    cabinet_px: [192, 192], // 500/192 = 2.604 mm
                    gap_mm: 0.0,
                    transparent: false,
                    default_nits: 1500.0,
                },
                ScreenProfile {
                    name: "Broadcast / XR 1.56mm",
                    category: "LED Wall",
                    cabinet_mm: [500.0, 500.0],
                    cabinet_px: [320, 320], // 500/320 = 1.5625 mm
                    gap_mm: 0.0,
                    transparent: false,
                    default_nits: 1000.0,
                },
                ScreenProfile {
                    name: "Outdoor 4.8mm",
                    category: "LED Wall",
                    cabinet_mm: [500.0, 1000.0],
                    cabinet_px: [104, 208], // 500/104 = 4.81 mm
                    gap_mm: 0.0,
                    transparent: false,
                    default_nits: 4500.0,
                },
                ScreenProfile {
                    name: "Outdoor 10mm",
                    category: "LED Wall",
                    cabinet_mm: [960.0, 960.0],
                    cabinet_px: [96, 96], // 960/96 = 10 mm
                    gap_mm: 0.0,
                    transparent: false,
                    default_nits: 6000.0,
                },
                ScreenProfile {
                    name: "Transparent 7.8mm",
                    category: "LED Wall",
                    cabinet_mm: [1000.0, 500.0],
                    cabinet_px: [128, 64], // 1000/128 = 7.81 mm
                    gap_mm: 0.0,
                    transparent: true,
                    default_nits: 4500.0,
                },
                ScreenProfile {
                    name: "Floor Tile 4.8mm",
                    category: "LED Wall",
                    cabinet_mm: [500.0, 500.0],
                    cabinet_px: [104, 104], // 500/104 = 4.81 mm
                    gap_mm: 0.0,
                    transparent: false,
                    default_nits: 1500.0,
                },
            ],
            gdtf: Vec::new(),
        }
    }

    /// Import a GDTF file and add it to the library. Returns the new index.
    pub fn import_gdtf(&mut self, path: &std::path::Path) -> Result<usize, String> {
        self.import_gdtf_with_source(path, crate::gdtf::FixtureSource::Import)
    }

    /// Import a GDTF file, tagging it with its provenance (`Import` for disk
    /// drops, `GdtfShare` for online downloads) BEFORE it is shared into an
    /// `Arc`, so every placed fixture inherits the right chip. Returns the new
    /// index.
    pub fn import_gdtf_with_source(
        &mut self,
        path: &std::path::Path,
        source: crate::gdtf::FixtureSource,
    ) -> Result<usize, String> {
        let mut fixture = crate::gdtf::GdtfFixture::load_path(path)?;
        fixture.source = source;
        self.gdtf.push(std::sync::Arc::new(fixture));
        Ok(self.gdtf.len() - 1)
    }
}

impl Default for Library {
    fn default() -> Self {
        Self::standard()
    }
}
