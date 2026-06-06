//! The content library: categorized fixture and environment *definitions* you
//! can instantiate into the scene.
//!
//! This is the precursor to GDTF (fixtures) and MVR (scenes). For now it is a
//! small hand-written catalog, but the shape — categories of profiles that
//! carry default geometry/params — is what those importers will populate.

/// How a fixture body is drawn. Maps to a mesh the renderer holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
}

/// What kind of environment volume this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// The whole catalog, grouped into built-in fixtures, environments, and
/// imported GDTF fixture definitions.
pub struct Library {
    pub fixtures: Vec<FixtureProfile>,
    pub environments: Vec<EnvironmentProfile>,
    /// GDTF fixtures imported at runtime.
    pub gdtf: Vec<std::sync::Arc<crate::gdtf::GdtfFixture>>,
}

impl Library {
    pub fn standard() -> Self {
        Self {
            fixtures: vec![FixtureProfile {
                name: "PAR Can",
                category: "Generic",
                geometry: FixtureGeometry::Cylinder,
                default_beam_angle: 24.0,
                default_color: [1.0, 0.95, 0.85],
            }],
            environments: vec![EnvironmentProfile {
                name: "Fog Box",
                category: "Environments",
                kind: EnvironmentKind::FogBox,
                default_size: [40.0, 20.0, 40.0],
                // Light theatrical haze (extinction per metre): visible beams
                // without fogging out the whole stage.
                default_density: 0.03,
            }],
            gdtf: Vec::new(),
        }
    }

    /// Import a GDTF file and add it to the library. Returns the new index.
    pub fn import_gdtf(&mut self, path: &std::path::Path) -> Result<usize, String> {
        let fixture = crate::gdtf::GdtfFixture::load_path(path)?;
        self.gdtf.push(std::sync::Arc::new(fixture));
        Ok(self.gdtf.len() - 1)
    }
}

impl Default for Library {
    fn default() -> Self {
        Self::standard()
    }
}
