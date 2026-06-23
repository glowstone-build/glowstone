//! The scene: a flat owner of plain data (fixtures, environment volumes, and
//! later the stage rig, truss, imported MVR geometry). No ECS — just `Vec`s and
//! structs. The renderer reads from here every frame; the UI mutates it.

pub mod environment;
pub mod fixture;
pub mod library;
pub mod screen;

pub use environment::Environment;
pub use fixture::Fixture;
pub use library::{EnvironmentProfile, FixtureProfile, Library, ScreenProfile};
pub use screen::LedScreen;

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::mvr::{GeometryModel, MvrHeader, MvrImport, MvrObjectMeta};

/// A static, non-fixture object placed in the scene — a stage deck, truss,
/// set piece, or screen imported from MVR. Drawn as lit geometry that occludes
/// beams; not a light source.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SceneGeometry {
    pub name: String,
    /// World-space placement (Y-up, metres) of the object's frame. The renderer
    /// applies the glTF +Y-up → geometry +Z-up flip to each model on top.
    pub transform: Mat4,
    /// The placed 3D models (file name + glTF bytes). Usually one per object.
    pub models: Vec<GeometryModel>,
    /// MVR round-trip metadata (UUID, class, layer). `None` for app-created.
    pub mvr: Option<MvrObjectMeta>,
    /// Object-local AABB of all models (post yup-flip, pre `transform`), computed
    /// once at import. Drives viewport ray-picking + the framing bounds. `None`
    /// if no model parsed.
    pub bounds: Option<(Vec3, Vec3)>,
    /// Hidden in the viewport (the Scene outliner's eye toggle): not drawn, not
    /// pickable. Still listed in the outliner.
    pub hidden: bool,
}

impl SceneGeometry {
    /// World-space AABB (`transform` applied to the local [`bounds`]), if known.
    ///
    /// [`bounds`]: Self::bounds
    pub fn world_bounds(&self) -> Option<(Vec3, Vec3)> {
        let (lo, hi) = self.bounds?;
        let mut wlo = Vec3::splat(f32::INFINITY);
        let mut whi = Vec3::splat(f32::NEG_INFINITY);
        for cx in [lo.x, hi.x] {
            for cy in [lo.y, hi.y] {
                for cz in [lo.z, hi.z] {
                    let p = self.transform.transform_point3(Vec3::new(cx, cy, cz));
                    wlo = wlo.min(p);
                    whi = whi.max(p);
                }
            }
        }
        Some((wlo, whi))
    }
}

/// Object-local AABB of an imported object's models, in the same frame the
/// renderer draws them (the +Y-up → +Z-up flip is baked into glTF verts here so
/// it matches `obj.transform * flip`). `None` if nothing parsed.
fn model_local_bounds(models: &[GeometryModel]) -> Option<(Vec3, Vec3)> {
    let flip = crate::mvr::glb_yup_to_zup();
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let mut any = false;
    for m in models {
        let needs_flip = crate::renderer::fixture_model::model_needs_yup_flip(&m.file);
        for v in crate::renderer::fixture_model::load_model(&m.file, &m.glb) {
            let mut p = Vec3::from(v.position);
            if needs_flip {
                p = flip.transform_point3(p);
            }
            // Mirror the renderer's per-model placement (`world * matrix * flip`):
            // the per-Geometry3D matrix is part of the local frame, so bounds /
            // ray-picking / framing must include it.
            p = m.matrix.transform_point3(p);
            lo = lo.min(p);
            hi = hi.max(p);
            any = true;
        }
    }
    any.then_some((lo, hi))
}

/// Document-level MVR data retained from an import so the scene can be written
/// back out: the header (version/provider, layer/class/position tables) and
/// every original resource blob (the `.gdtf`/`.glb`/texture bytes), keyed by
/// archive file name.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MvrSceneData {
    pub header: MvrHeader,
    pub resources: HashMap<String, Arc<Vec<u8>>>,
}

/// How the 3D viewport draws the scene.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ViewportMode {
    /// Full render: lit surfaces + volumetric beams + bloom/tonemap.
    #[default]
    Beauty,
    /// Flat albedo, no fixture/beam lighting and no fog — see the raw set/rig.
    Unlit,
    /// Scene geometry as wireframe (no fog) — read structure and fixture layout.
    Wireframe,
}

impl ViewportMode {
    pub const ALL: [ViewportMode; 3] = [Self::Beauty, Self::Unlit, Self::Wireframe];

    pub fn label(self) -> &'static str {
        match self {
            Self::Beauty => "Beauty",
            Self::Unlit => "Unlit",
            Self::Wireframe => "Wireframe",
        }
    }

    /// Shader code read from `CameraUniform.render_mode.x` (mesh.wgsl branches on it).
    pub fn shader_code(self) -> f32 {
        match self {
            Self::Beauty => 0.0,
            Self::Unlit => 1.0,
            Self::Wireframe => 2.0,
        }
    }
}

/// Global look/post-processing controls, edited in the UI and read by the
/// renderer each frame (exposure/bloom tonemapping + the volumetric beam look).
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct RenderSettings {
    pub exposure: f32,
    pub bloom: f32,
    pub beam_intensity: f32,
    pub steps: u32,
    /// Floor-pool gobo edge sharpening amount (0 = off). Drives the contour
    /// steepening in mesh.wgsl via `camera.render_mode.y`.
    pub gobo_sharpness: f32,
    pub show_beam_wireframes: bool,
    /// Show the origin grid + world axes.
    pub show_grid: bool,
    /// How the viewport draws the scene (beauty / unlit / wireframe).
    pub mode: ViewportMode,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            bloom: 0.85,
            beam_intensity: 650.0,
            // Max marching steps for a full-fog-box ray; the constant-dt cap scales
            // it down for shorter rays at the SAME per-metre density. Kept at the
            // pre-optimisation value because aerial gobo cross-sections alias into
            // longitudinal stripes below ~64 samples — the speed-up comes from the
            // lossless per-fixture pre-cull, not from marching fewer steps.
            steps: 80,
            gobo_sharpness: 0.6,
            show_beam_wireframes: false,
            show_grid: true,
            mode: ViewportMode::Beauty,
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
    /// Selected static-geometry (Objects) indices; the first is the "primary".
    /// A scene has at most one *kind* of selection active at a time (selecting
    /// geometry clears fixtures/environment and vice-versa) so the Inspector and
    /// transform tools have a single, unambiguous target.
    pub geometry: Vec<usize>,
    /// Selected LED-screen (Screens) indices; the first is the "primary".
    pub screens: Vec<usize>,
    /// Selected environment volume, if any.
    pub environment: Option<usize>,
}

impl Selection {
    /// Select a single fixture (clearing any other selection).
    pub fn fixture(i: usize) -> Self {
        Self { fixtures: vec![i], geometry: Vec::new(), screens: Vec::new(), environment: None }
    }

    /// Select a single static-geometry object (clearing any other selection).
    pub fn geometry(i: usize) -> Self {
        Self { fixtures: Vec::new(), geometry: vec![i], screens: Vec::new(), environment: None }
    }

    /// Select a single LED screen (clearing any other selection).
    pub fn screen(i: usize) -> Self {
        Self { fixtures: Vec::new(), geometry: Vec::new(), screens: vec![i], environment: None }
    }

    /// Select a single environment.
    pub fn environment(i: usize) -> Self {
        Self { fixtures: Vec::new(), geometry: Vec::new(), screens: Vec::new(), environment: Some(i) }
    }

    pub fn contains_fixture(&self, i: usize) -> bool {
        self.fixtures.contains(&i)
    }

    pub fn contains_geometry(&self, i: usize) -> bool {
        self.geometry.contains(&i)
    }

    pub fn contains_screen(&self, i: usize) -> bool {
        self.screens.contains(&i)
    }

    /// The primary (first) selected fixture, if any.
    pub fn primary_fixture(&self) -> Option<usize> {
        self.fixtures.first().copied()
    }

    /// The primary (first) selected geometry object, if any.
    pub fn primary_geometry(&self) -> Option<usize> {
        self.geometry.first().copied()
    }

    /// The primary (first) selected LED screen, if any.
    pub fn primary_screen(&self) -> Option<usize> {
        self.screens.first().copied()
    }

    /// Toggle a fixture in/out of the selection (for ctrl/cmd-click multi-select).
    pub fn toggle_fixture(&mut self, i: usize) {
        self.environment = None;
        self.geometry.clear();
        self.screens.clear();
        if let Some(p) = self.fixtures.iter().position(|&x| x == i) {
            self.fixtures.remove(p);
        } else {
            self.fixtures.push(i);
        }
    }

    /// Toggle a geometry object in/out of the selection (ctrl/cmd-click).
    pub fn toggle_geometry(&mut self, i: usize) {
        self.environment = None;
        self.fixtures.clear();
        self.screens.clear();
        if let Some(p) = self.geometry.iter().position(|&x| x == i) {
            self.geometry.remove(p);
        } else {
            self.geometry.push(i);
        }
    }

    /// Toggle an LED screen in/out of the selection (ctrl/cmd-click).
    pub fn toggle_screen(&mut self, i: usize) {
        self.environment = None;
        self.fixtures.clear();
        self.geometry.clear();
        if let Some(p) = self.screens.iter().position(|&x| x == i) {
            self.screens.remove(p);
        } else {
            self.screens.push(i);
        }
    }

    /// Select an inclusive contiguous fixture range (shift-range select).
    pub fn set_fixture_range(&mut self, a: usize, b: usize) {
        self.environment = None;
        self.geometry.clear();
        self.screens.clear();
        self.fixtures = (a.min(b)..=a.max(b)).collect();
    }
}

/// Resolve a fixture click into a selection update given the keyboard modifiers
/// and a shift-range `anchor`. Shared by the scene outliner and the 3D viewport
/// so list-click and viewport-click behave identically: plain = replace,
/// ⌘/Ctrl = toggle, Shift = range from the anchor.
pub fn apply_fixture_click(
    selection: &mut Selection,
    anchor: &mut Option<usize>,
    i: usize,
    shift: bool,
    toggle: bool,
    count: usize,
) {
    // Drop a stale anchor (e.g. fixtures deleted since it was set) so a
    // shift-range can't span past the end of the scene.
    if anchor.is_some_and(|a| a >= count) {
        *anchor = None;
    }
    if shift {
        let a = anchor.unwrap_or(i);
        selection.set_fixture_range(a, i);
        // keep the anchor so chained shift-clicks grow/shrink from it
    } else if toggle {
        selection.toggle_fixture(i);
        *anchor = Some(i);
    } else {
        *selection = Selection::fixture(i);
        *anchor = Some(i);
    }
}

/// Everything the renderer draws and the UI edits.
/// The world environment: an equirectangular HDRI that both renders behind the
/// scene (a sky) and lights the geometry (image-based ambient), with overall
/// brightness, a yaw rotation, and an ambient-fill strength. When no map is
/// loaded the renderer keeps the dark void + a faint flat fill.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct World {
    /// Equirectangular map file bytes (`.hdr` / `.png` / `.jpg`), if loaded.
    pub hdri: Option<std::sync::Arc<Vec<u8>>>,
    /// Display name of the loaded map (for the UI / round-trip).
    pub hdri_name: String,
    /// Overall world brightness multiplier (drives both sky + ambient).
    pub brightness: f32,
    /// Environment yaw about +Y, radians (turns sky + ambient together).
    pub rotation: f32,
    /// Image-based ambient fill strength on geometry (0 = none).
    pub ambient: f32,
    /// Draw the map as the viewport background (off = keep the dark void).
    pub show_background: bool,
}

impl Default for World {
    fn default() -> Self {
        Self {
            hdri: None,
            hdri_name: String::new(),
            brightness: 1.0,
            rotation: 0.0,
            ambient: 1.0,
            show_background: true,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Scene {
    pub fixtures: Vec<Fixture>,
    pub environments: Vec<Environment>,
    /// The world / environment (HDRI sky + ambient lighting).
    pub world: World,
    /// Static imported geometry (stage, truss, set) — drawn but not a light.
    pub geometry: Vec<SceneGeometry>,
    /// LED video walls / screens — drawn as emissive surfaces (a single content
    /// texture each), contributing a cheap, blurred light to the scene/haze.
    pub screens: Vec<LedScreen>,
    /// Retained MVR document data, present when the scene came from an MVR
    /// import, so it can be exported back out faithfully.
    pub mvr: Option<MvrSceneData>,
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
        fixture.snap_movement(); // start at the commanded pose, don't slew on launch

        let fog = &library.environments[0];
        // Sit the fog box ON the floor (y=0), not centred at the origin — otherwise
        // its lower half is buried below the ground plane, where the raymarch wastes
        // steps and, from a low camera, beams visibly render under the floor.
        let on_floor = Vec3::new(0.0, fog.default_size[1] * 0.5, 0.0);
        let environment = Environment::from_profile(fog, "Fog Box", on_floor);

        Self {
            fixtures: vec![fixture],
            environments: vec![environment],
            world: World::default(),
            geometry: Vec::new(),
            screens: Vec::new(),
            mvr: None,
        }
    }

    /// Advance time-based wheel motion (gobo/color/animation/prism spin and
    /// scroll) by `dt` seconds. Call **once per real frame** from the update
    /// loop — never from the renderer (capture + render share `record_scene`
    /// and would double-advance the animation).
    pub fn advance(&mut self, dt: f32) {
        for f in &mut self.fixtures {
            let components = match &f.gdtf {
                Some(g) => g
                    .modes
                    .get(f.mode_index)
                    .map(|m| m.components.as_slice())
                    .unwrap_or(&[]),
                None => &[],
            };
            f.motion.advance(&f.optics, components, dt);
            // Slew the head toward its commanded pan/tilt at motor speed.
            f.advance_movement(dt);
        }
    }

    /// Settle every fixture's slewed pan/tilt to its commanded target (no
    /// motion lag). Headless capture paths render without the per-frame
    /// [`advance`](Self::advance) integrator, so they call this after posing.
    pub fn snap_movement(&mut self) {
        for f in &mut self.fixtures {
            f.snap_movement();
        }
    }

    /// Add a fixture from a library profile; returns its new index.
    pub fn add_fixture(&mut self, profile: &FixtureProfile) -> usize {
        let n = self.fixtures.iter().filter(|f| f.profile == profile.name).count() + 1;
        let name = format!("{} {}", profile.name, n);
        // Place new fixtures a few metres up, aimed down.
        let mut fixture = Fixture::from_profile(profile, name, Vec3::new(0.0, 4.0, 0.0));
        fixture.tilt = 30.0;
        fixture.snap_movement(); // appear at the placed pose, not slewing from 0
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
            f.snap_movement(); // each copy starts at its fanned pose
            self.fixtures.push(f);
        }
        (count > 0).then_some(first)
    }

    /// Replace the scene's fixtures + static geometry with an imported MVR
    /// scene. The environment volumes are kept (so the volumetric haze still
    /// reads), and the document data is retained for round-trip export.
    pub fn import_mvr(&mut self, import: MvrImport) {
        self.fixtures.clear();
        self.geometry.clear();
        self.screens.clear();
        for f in import.fixtures {
            self.fixtures.push(Fixture::from_mvr(f));
        }
        for o in import.objects {
            // Each model's own `<Geometry3D>` matrix (e.g. an inch→metre scale)
            // is honoured at import — see [`GeometryModel::matrix`]. A previous
            // build instead post-hoc downscaled any object whose world AABB
            // exceeded ~120 m, but that band-aid scaled about the bbox centre,
            // which corrupted placement (objects drifted / detached) and inflated
            // origins; honouring the source transform is the correct fix, so the
            // heuristic is gone.
            let bounds = model_local_bounds(&o.models);
            self.geometry.push(SceneGeometry {
                name: o.name,
                transform: o.world,
                models: o.models,
                mvr: Some(o.meta),
                bounds,
                hidden: false,
            });
        }
        for s in import.screens {
            self.screens.push(s);
        }
        self.mvr = Some(MvrSceneData {
            header: import.header,
            resources: import.resources,
        });
        log::info!(
            "scene: imported MVR — {} fixtures, {} static objects",
            self.fixtures.len(),
            self.geometry.len()
        );
    }

    /// Bounding sphere `(center, radius)` of all fixtures and static-geometry
    /// origins, for framing the camera after an import. `None` if the scene is
    /// empty.
    pub fn scene_frame(&self) -> Option<(Vec3, f32)> {
        let mut pts = self.fixtures.iter().map(|f| f.position).collect::<Vec<_>>();
        pts.extend(self.geometry.iter().map(|g| g.transform.w_axis.truncate()));
        pts.extend(self.screens.iter().map(|s| s.world_center()));
        let first = *pts.first()?;
        let (mut lo, mut hi) = (first, first);
        for p in &pts {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let center = (lo + hi) * 0.5;
        let radius = ((hi - lo).length() * 0.5).max(3.0);
        Some((center, radius))
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

    /// Add an LED screen from a library component; returns its new index. The
    /// wall stands upright at the back of the stage, facing +Z toward the
    /// audience, lifted so its base sits on the floor.
    pub fn add_screen(&mut self, profile: &ScreenProfile) -> usize {
        let n = self.screens.len() + 1;
        let name = format!("LED Wall {n}");
        // A default 4×2 array; lift it so the bottom edge rests near the floor and
        // push it a few metres upstage (−Z) of the origin.
        let proto = LedScreen::from_profile(profile, name.clone(), Mat4::IDENTITY);
        let [_, h] = proto.size_m();
        let transform = Mat4::from_translation(Vec3::new(0.0, h * 0.5 + 0.2, -4.0));
        let screen = LedScreen::from_profile(profile, name, transform);
        self.screens.push(screen);
        self.screens.len() - 1
    }

    /// Add an environment from a library profile; returns its new index.
    pub fn add_environment(&mut self, profile: &EnvironmentProfile) -> usize {
        let n = self.environments.len() + 1;
        let name = format!("{} {}", profile.name, n);
        // Rest the box on the floor (see Scene::demo) so it doesn't sink below ground.
        let on_floor = Vec3::new(0.0, profile.default_size[1] * 0.5, 0.0);
        self.environments
            .push(Environment::from_profile(profile, name, on_floor));
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
