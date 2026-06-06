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
    /// Show the origin grid + world axes.
    pub show_grid: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            bloom: 0.85,
            beam_intensity: 650.0,
            steps: 80,
            show_beam_wireframes: false,
            show_grid: true,
        }
    }
}

/// What the UI currently has selected. Drives the Inspector and the
/// highlight/wireframe emphasis in the viewport. Supports multi-select of
/// fixtures (for bulk editing); the environment selection is single.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Selection {
    /// Selected fixture indices; the first is the "primary" (drives single-edit).
    pub fixtures: Vec<usize>,
    /// Selected environment volume, if any.
    pub environment: Option<usize>,
}

impl Selection {
    /// Select a single fixture (clearing any other selection).
    pub fn fixture(i: usize) -> Self {
        Self { fixtures: vec![i], environment: None }
    }

    /// Select a single environment.
    pub fn environment(i: usize) -> Self {
        Self { fixtures: Vec::new(), environment: Some(i) }
    }

    pub fn contains_fixture(&self, i: usize) -> bool {
        self.fixtures.contains(&i)
    }

    /// The primary (first) selected fixture, if any.
    pub fn primary_fixture(&self) -> Option<usize> {
        self.fixtures.first().copied()
    }

    /// Toggle a fixture in/out of the selection (for ctrl/cmd-click multi-select).
    pub fn toggle_fixture(&mut self, i: usize) {
        self.environment = None;
        if let Some(p) = self.fixtures.iter().position(|&x| x == i) {
            self.fixtures.remove(p);
        } else {
            self.fixtures.push(i);
        }
    }
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

    /// Advance time-based wheel motion (gobo/color/animation/prism spin and
    /// scroll) by `dt` seconds. Call **once per real frame** from the update
    /// loop — never from the renderer (capture + render share `record_scene`
    /// and would double-advance the animation).
    pub fn advance(&mut self, dt: f32) {
        for f in &mut self.fixtures {
            let optics = f.optics;
            f.motion.advance(&optics, dt);
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

    /// Duplicate fixture `idx` into an array of `count` copies. Copy `i` (1..=N)
    /// is translated by `offset * i` and panned by `y_angle_deg * i` — so a Y
    /// angle with zero offset fans the beams, and an offset makes a row/stack.
    /// Returns the index of the first new copy.
    pub fn duplicate_fixture(
        &mut self,
        idx: usize,
        offset: Vec3,
        y_angle_deg: f32,
        count: u32,
    ) -> Option<usize> {
        let base = self.fixtures.get(idx)?.clone();
        let first = self.fixtures.len();
        for i in 1..=count {
            let mut f = base.clone();
            f.position = base.position + offset * i as f32;
            f.pan = base.pan + y_angle_deg * i as f32;
            f.name = format!("{} ({i})", base.name);
            self.fixtures.push(f);
        }
        (count > 0).then_some(first)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_fixture_arrays() {
        let mut scene = Scene::demo();
        let base = scene.fixtures[0].clone();
        let first = scene
            .duplicate_fixture(0, Vec3::new(1.0, 0.0, 0.0), 36.0, 9)
            .expect("first index");
        assert_eq!(scene.fixtures.len(), 10);
        assert_eq!(first, 1);
        // Copy i=3 is the third new fixture (index first+2).
        let c = &scene.fixtures[first + 2];
        assert!((c.position.x - (base.position.x + 3.0)).abs() < 1e-4);
        assert!((c.pan - (base.pan + 108.0)).abs() < 1e-3);
    }
}
