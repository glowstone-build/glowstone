//! The scene: a flat owner of plain data (fixtures, environment volumes, and
//! later the stage rig, truss, imported MVR geometry). No ECS — just `Vec`s and
//! structs. The renderer reads from here every frame; the UI mutates it.

pub mod environment;
pub mod fixture;
pub mod library;

pub use environment::Environment;
pub use fixture::Fixture;
pub use library::{EnvironmentProfile, FixtureProfile, Library};

use glam::Vec3;

/// Global look/post-processing controls, edited in the UI and read by the
/// renderer each frame (exposure/bloom tonemapping + the volumetric beam look).
#[derive(Clone, Copy, Debug)]
pub struct RenderSettings {
    pub exposure: f32,
    pub bloom: f32,
    pub beam_intensity: f32,
    pub steps: u32,
    pub show_beam_wireframes: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            bloom: 0.7,
            beam_intensity: 320.0,
            steps: 80,
            show_beam_wireframes: false,
        }
    }
}

/// What the UI currently has selected. Drives the Inspector and the
/// highlight/wireframe emphasis in the viewport.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Selection {
    #[default]
    None,
    Fixture(usize),
    Environment(usize),
}

/// Everything the renderer draws and the UI edits.
pub struct Scene {
    pub fixtures: Vec<Fixture>,
    pub environments: Vec<Environment>,
}

impl Scene {
    /// The default test scene: one PAR can 4 m up aimed down at 45°, full
    /// intensity, inside a large fog box at the origin. All editable.
    pub fn demo() -> Self {
        let library = Library::standard();

        let par = &library.fixtures[0];
        let mut fixture = Fixture::from_profile(par, "PAR Can 1", Vec3::new(0.0, 4.0, 0.0));
        fixture.tilt = 45.0;
        fixture.intensity = 1.0;

        let fog = &library.environments[0];
        let environment = Environment::from_profile(fog, "Fog Box", Vec3::ZERO);

        Self {
            fixtures: vec![fixture],
            environments: vec![environment],
        }
    }

    /// Add a fixture from a library profile; returns its new index.
    pub fn add_fixture(&mut self, profile: &FixtureProfile) -> usize {
        let n = self.fixtures.iter().filter(|f| f.profile == profile.name).count() + 1;
        let name = format!("{} {}", profile.name, n);
        // Place new fixtures a few metres up, aimed down.
        let mut fixture = Fixture::from_profile(profile, name, Vec3::new(0.0, 4.0, 0.0));
        fixture.tilt = 30.0;
        self.fixtures.push(fixture);
        self.fixtures.len() - 1
    }

    /// Add an imported GDTF fixture; returns its new index.
    pub fn add_gdtf(
        &mut self,
        gdtf: std::sync::Arc<crate::gdtf::GdtfFixture>,
        position: Vec3,
    ) -> usize {
        let n = self.fixtures.iter().filter(|f| f.is_gdtf()).count() + 1;
        let name = format!("{} {}", gdtf.name, n);
        self.fixtures.push(Fixture::from_gdtf(gdtf, name, position));
        self.fixtures.len() - 1
    }

    /// Add an environment from a library profile; returns its new index.
    pub fn add_environment(&mut self, profile: &EnvironmentProfile) -> usize {
        let n = self.environments.len() + 1;
        let name = format!("{} {}", profile.name, n);
        self.environments
            .push(Environment::from_profile(profile, name, Vec3::ZERO));
        self.environments.len() - 1
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::demo()
    }
}
