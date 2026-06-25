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

/// Session-stable per-entity identity. Assigned at create + reconstructed on
/// load (serde-skip → never serialized → NO .archie format bump, same trick as
/// `Fixture.gdtf`). The outliner addresses rows by this so expand-state + the
/// range anchor survive add/delete reordering (Blender's `TreeStoreElem.id` role).
pub type EntityId = u64;

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
    /// Session-stable identity (serde-skip → reassigned by [`Scene::ensure_ids`]
    /// on load). The outliner keys rows by this so reorder/delete is robust.
    #[serde(skip)]
    pub id: EntityId,
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
    /// Froxel volumetric: a compute fog grid for the wide "mass" beams. OFF BY
    /// DEFAULT — the per-pixel RAYMARCH is the default renderer because its beams
    /// are far cleaner (the froxel's coarse 160×90×64 grid produces blocky/
    /// duplicated "fog circle" artifacts near the camera, flicker, and mushy gobos
    /// in the masses). Kept as an opt-in toggle for the perf win on huge rigs only.
    /// `skip`-ped from (de)serialization: the .archie format is bincode (positional,
    /// NOT self-describing), so a newly-added serialized field misaligns every older
    /// save. Kept out of the byte stream entirely and defaulted on each load.
    #[serde(skip, default = "default_false")]
    pub froxel_volumetric: bool,
    /// Chroma read-up of saturated beams in haze (Helmholtz–Kohlrausch): lifts
    /// dim-saturated hues (blue/deep-red/magenta) so they read in fog without
    /// flattening to neon; white/pastel and bright-whitened cells are untouched.
    /// 0 = off (exact pre-feature look). `skip`-ped like [froxel_volumetric] —
    /// bincode .archie is positional, so a new serialized field corrupts old saves.
    #[serde(skip, default = "default_chroma_haze")]
    pub chroma_haze: f32,
    /// Floor-pool gobo edge sharpening amount (0 = off). Drives the contour
    /// steepening in mesh.wgsl via `camera.render_mode.y`.
    pub gobo_sharpness: f32,
    /// Internal render scale (0.5..=1.0): the viewport renders at this fraction of
    /// native and bilinearly upscales — the single biggest fps lever on a Retina
    /// display (everything per-pixel scales with it²). `skip`-ped: it's a machine/
    /// GPU-specific perf preference, not part of the SHOW, so it must NOT ride in the
    /// positional bincode .archie (which would also force a FORMAT bump that rejects
    /// every existing save). Defaults to 1.0 = native (unchanged behaviour).
    #[serde(skip, default = "default_render_scale")]
    pub render_scale: f32,
    /// Max hero (per-beam) shadow maps to render, capped to `shadow::MAX`. Lower =
    /// fewer shadow depth passes = faster (each is ~2-3 ms at Retina). `skip`-ped:
    /// machine-specific perf knob (see [render_scale]).
    #[serde(skip, default = "default_shadow_max")]
    pub shadow_max: u32,
    pub show_beam_wireframes: bool,
    /// Show the origin grid + world axes.
    pub show_grid: bool,
    /// How the viewport draws the scene (beauty / unlit / wireframe).
    pub mode: ViewportMode,
    /// Active modal-transform axis constraint, for the Blender-style infinite
    /// constraint line: `(pivot, axis colour, axis direction)`. Set by the UI each
    /// frame from the live `TransformOp` (None when no axis is locked). Purely a
    /// per-frame render hint — never persisted (bincode .archie is positional, so a
    /// new serialized field would corrupt older saves, like the fields above).
    #[serde(skip, default)]
    pub axis_hint: Option<(glam::Vec3, [f32; 3], glam::Vec3)>,
}

fn default_false() -> bool {
    false
}

fn default_chroma_haze() -> f32 {
    1.2
}

fn default_render_scale() -> f32 {
    1.0
}

fn default_shadow_max() -> u32 {
    8
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
            froxel_volumetric: false,
            chroma_haze: 1.2,
            gobo_sharpness: 0.6,
            render_scale: 1.0,
            shadow_max: 8,
            show_beam_wireframes: false,
            show_grid: true,
            mode: ViewportMode::Beauty,
            axis_hint: None,
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
    /// Whether the top-level World node (HDRI sky + ambient) is selected. World
    /// is mutually exclusive with the entity selections above — selecting World
    /// clears them, and selecting any entity clears World.
    pub world: bool,
}

impl Selection {
    /// Select a single fixture (clearing any other selection).
    pub fn fixture(i: usize) -> Self {
        Self { fixtures: vec![i], geometry: Vec::new(), screens: Vec::new(), environment: None, world: false }
    }

    /// Select a single static-geometry object (clearing any other selection).
    pub fn geometry(i: usize) -> Self {
        Self { fixtures: Vec::new(), geometry: vec![i], screens: Vec::new(), environment: None, world: false }
    }

    /// Select a single LED screen (clearing any other selection).
    pub fn screen(i: usize) -> Self {
        Self { fixtures: Vec::new(), geometry: Vec::new(), screens: vec![i], environment: None, world: false }
    }

    /// Select a single environment.
    pub fn environment(i: usize) -> Self {
        Self { fixtures: Vec::new(), geometry: Vec::new(), screens: Vec::new(), environment: Some(i), world: false }
    }

    /// Select the top-level World node (clearing any other selection).
    pub fn world() -> Self {
        Self { fixtures: Vec::new(), geometry: Vec::new(), screens: Vec::new(), environment: None, world: true }
    }

    /// Toggle the World node selection on/off (clearing everything else when on).
    /// Kept for selection-API symmetry with the other `toggle_*` entities; wired to
    /// ⌘/Ctrl-click on the World node in a later pass.
    #[allow(dead_code)]
    pub fn toggle_world(&mut self) {
        *self = if self.world { Self::default() } else { Self::world() };
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
    /// Click selection now flows through the pure [`apply_select`] truth table
    /// (#24); kept for selection-API symmetry with `toggle_geometry`/`_screen`
    /// (still used by the outliner) and any direct caller.
    #[allow(dead_code)]
    pub fn toggle_fixture(&mut self, i: usize) {
        self.world = false;
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
        self.world = false;
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
        self.world = false;
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
        self.world = false;
        self.environment = None;
        self.geometry.clear();
        self.screens.clear();
        self.fixtures = (a.min(b)..=a.max(b)).collect();
    }
}

/// One entity the viewport can pick / marquee, addressed by its current kind +
/// index. The unified currency of the [`SelectOp`] model: click yields zero or
/// one of these; a box-select yields many. `Environment`/`World` are single-only
/// kinds (no multi-select), so the apply rules below collapse them to a replace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelItem {
    Fixture(usize),
    Geometry(usize),
    Screen(usize),
    Environment(usize),
    /// The World node — a single-only selection (the viewport doesn't pick it
    /// yet; the outliner's ⌘-click path will yield it). Carried in the enum now so
    /// `apply_select` already handles it, mirroring `Selection::toggle_world`.
    #[allow(dead_code)]
    World,
}

/// How a set of freshly-picked [`SelItem`]s combines with the current
/// [`Selection`] — Blender's `eSelectOp` indirection, so click / box / (later)
/// lasso share ONE truth table instead of duplicating per-Hit modifier arms.
/// The UI maps keyboard modifiers to one of these (UE/CAD convention):
/// plain = [`Replace`], Shift = [`Add`], ⌘/Ctrl = [`Toggle`] (click) or
/// [`Subtract`] (box).
///
/// [`Replace`]: SelectOp::Replace
/// [`Add`]: SelectOp::Add
/// [`Toggle`]: SelectOp::Toggle
/// [`Subtract`]: SelectOp::Subtract
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectOp {
    /// Discard the old selection; the hits become the whole selection. An empty
    /// hit set clears (click on empty space).
    Replace,
    /// Union the hits into the current selection (Shift).
    Add,
    /// Remove the hits from the current selection (Ctrl in a box drag).
    Subtract,
    /// Flip each hit's membership (⌘/Ctrl click).
    Toggle,
}

/// Pure selection algebra: fold `hits` into `current` under `op`, returning the
/// NEW selection (no mutation, no undo — selection changes are undo-free). This
/// is the single source the viewport click + box-select both call.
///
/// Heterogeneous rule: fixtures, geometry and screens are the three multi-select
/// kinds and stay mutually exclusive (the existing `toggle_*` invariant — a
/// scene has at most one *kind* of selection at a time so the Inspector/gizmo
/// have one unambiguous target). For [`Add`]/[`Toggle`]/[`Subtract`] we operate
/// within the kind already selected when there is one (so Shift+box keeps
/// growing a fixture set and ignores stray geometry hits); otherwise the hits'
/// own kind wins. [`Environment`]/[`World`] are single-only → always a replace.
///
/// [`Add`]: SelectOp::Add
/// [`Toggle`]: SelectOp::Toggle
/// [`Subtract`]: SelectOp::Subtract
/// [`Environment`]: SelItem::Environment
/// [`World`]: SelItem::World
pub fn apply_select(current: &Selection, hits: &[SelItem], op: SelectOp) -> Selection {
    // The kind we operate within: the active multi-select kind for additive ops
    // (so Shift/Ctrl extend the existing set), else the dominant kind among the
    // hits (fixtures > geometry > screens), else None.
    let active_kind = if !current.fixtures.is_empty() {
        Some(0u8)
    } else if !current.geometry.is_empty() {
        Some(1)
    } else if !current.screens.is_empty() {
        Some(2)
    } else {
        None
    };
    let hit_kind = |k: u8| -> Vec<usize> {
        hits.iter()
            .filter_map(|h| match (k, h) {
                (0, SelItem::Fixture(i)) | (1, SelItem::Geometry(i)) | (2, SelItem::Screen(i)) => {
                    Some(*i)
                }
                _ => None,
            })
            .collect()
    };

    // A single Environment/World hit is a single-only kind: any op replaces.
    if hits.len() == 1 {
        match hits[0] {
            SelItem::Environment(i) => return Selection::environment(i),
            SelItem::World => return Selection::world(),
            _ => {}
        }
    }

    match op {
        SelectOp::Replace => {
            // Hits define the whole new selection (empty → cleared). Multi-kind
            // hit sets collapse to the dominant kind (fixtures > geometry > screens).
            for k in [0u8, 1, 2] {
                let v = hit_kind(k);
                if !v.is_empty() {
                    let mut s = Selection::default();
                    match k {
                        0 => s.fixtures = v,
                        1 => s.geometry = v,
                        _ => s.screens = v,
                    }
                    return s;
                }
            }
            Selection::default()
        }
        SelectOp::Add | SelectOp::Subtract | SelectOp::Toggle => {
            // A single Ctrl-click (Toggle) on an item of a DIFFERENT kind than the
            // current selection SWITCHES kinds — matching the old toggle_fixture/
            // toggle_geometry/toggle_screen, which cleared the other kind. (Box-select
            // Add/Subtract deliberately stay within the active kind; conflating the two
            // was the regression.) Same-kind toggles fall through to the normal path.
            if matches!(op, SelectOp::Toggle) && hits.len() == 1 {
                let hk = match hits[0] {
                    SelItem::Fixture(_) => Some(0u8),
                    SelItem::Geometry(_) => Some(1),
                    SelItem::Screen(_) => Some(2),
                    _ => None,
                };
                if hk.is_some() && hk != active_kind {
                    return apply_select(current, hits, SelectOp::Replace);
                }
            }
            // Operate within the active kind (extend the existing set) or, if
            // nothing is selected yet, the hits' dominant kind.
            let kind = active_kind.or_else(|| {
                [0u8, 1, 2].into_iter().find(|&k| !hit_kind(k).is_empty())
            });
            let Some(kind) = kind else { return current.clone() };
            let mut set: Vec<usize> = match kind {
                0 => current.fixtures.clone(),
                1 => current.geometry.clone(),
                _ => current.screens.clone(),
            };
            for i in hit_kind(kind) {
                let pos = set.iter().position(|&x| x == i);
                match (op, pos) {
                    (SelectOp::Add, None) => set.push(i),
                    (SelectOp::Add, Some(_)) => {}
                    (SelectOp::Subtract, Some(p)) => {
                        set.remove(p);
                    }
                    (SelectOp::Subtract, None) => {}
                    (SelectOp::Toggle, Some(p)) => {
                        set.remove(p);
                    }
                    (SelectOp::Toggle, None) => set.push(i),
                    (SelectOp::Replace, _) => unreachable!(),
                }
            }
            let mut s = Selection::default();
            match kind {
                0 => s.fixtures = set,
                1 => s.geometry = set,
                _ => s.screens = set,
            }
            s
        }
    }
}

/// Resolve a fixture click into a selection update given the keyboard modifiers
/// and a shift-range `anchor`. Shared by the scene outliner and the 3D viewport
/// so list-click and viewport-click behave identically: plain = replace,
/// ⌘/Ctrl = toggle, Shift = range from the anchor.
///
/// The plain/toggle paths defer to the unified [`apply_select`] truth table so
/// there is one selection algebra; Shift = inclusive index range stays special
/// (it needs the `anchor`, which the pure fn has no notion of).
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
    } else {
        let op = if toggle { SelectOp::Toggle } else { SelectOp::Replace };
        *selection = apply_select(selection, &[SelItem::Fixture(i)], op);
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
    /// Monotonic [`EntityId`] allocator. serde-skip → reset to 0 on load;
    /// [`ensure_ids`](Self::ensure_ids) reseeds it past the max live id after
    /// every load/import/undo-restore.
    #[serde(skip)]
    next_id: EntityId,
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

        let mut scene = Self {
            fixtures: vec![fixture],
            environments: vec![environment],
            world: World::default(),
            geometry: Vec::new(),
            screens: Vec::new(),
            mvr: None,
            next_id: 0,
        };
        scene.ensure_ids(); // hand the demo entities their stable ids
        scene
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

    /// Add a fixture from a library profile at the legacy default pose (a few
    /// metres up, aimed down); returns its new index. Prefer [`add_fixture_at`]
    /// for the place-at-cursor path (#19). Kept as the convenience default-pose
    /// entry point (the demo + tests use it; the UI now always supplies a point).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_fixture(&mut self, profile: &FixtureProfile) -> usize {
        self.add_fixture_at(profile, Vec3::new(0.0, 4.0, 0.0))
    }

    /// Add a fixture from a library profile at `position`; returns its new index.
    /// The place-at-cursor entry point — `position` is the viewport cursor's
    /// ground/ray hit (see `panels::placement_point`).
    pub fn add_fixture_at(&mut self, profile: &FixtureProfile, position: Vec3) -> usize {
        let n = self.fixtures.iter().filter(|f| f.profile == profile.name).count() + 1;
        let name = format!("{} {}", profile.name, n);
        let mut fixture = Fixture::from_profile(profile, name, position);
        fixture.tilt = 30.0; // aimed down
        fixture.snap_movement(); // appear at the placed pose, not slewing from 0
        fixture.id = self.alloc_id();
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
            f.id = self.alloc_id(); // a clone shares base.id → give each a fresh one
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
                id: 0, // assigned by ensure_ids() at the end of import_mvr
            });
        }
        for s in import.screens {
            self.screens.push(s);
        }
        self.mvr = Some(MvrSceneData {
            header: import.header,
            resources: import.resources,
        });
        // The imported entities were pushed with id == 0; hand them stable ids.
        self.ensure_ids();
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
        let mut fixture = Fixture::from_gdtf(gdtf, name, position);
        fixture.id = self.alloc_id();
        self.fixtures.push(fixture);
        self.fixtures.len() - 1
    }

    /// Add an LED screen from a library component at the legacy default pose
    /// (upstage, facing the audience); returns its new index. Prefer
    /// [`add_screen_at`] for the place-at-cursor path (#19).
    pub fn add_screen(&mut self, profile: &ScreenProfile) -> usize {
        self.add_screen_at(profile, Vec3::new(0.0, 0.0, -4.0))
    }

    /// Add an LED screen from a library component standing upright over the
    /// `ground` point (its base rests on the floor), facing +Z; returns its new
    /// index. `ground.y` is ignored — the wall always sits on the floor.
    pub fn add_screen_at(&mut self, profile: &ScreenProfile, ground: Vec3) -> usize {
        let n = self.screens.len() + 1;
        let name = format!("LED Wall {n}");
        // A default 4×2 array; lift it so the bottom edge rests near the floor and
        // stand it over the cursor's ground point.
        let proto = LedScreen::from_profile(profile, name.clone(), Mat4::IDENTITY);
        let [_, h] = proto.size_m();
        let transform = Mat4::from_translation(Vec3::new(ground.x, h * 0.5 + 0.2, ground.z));
        let mut screen = LedScreen::from_profile(profile, name, transform);
        screen.id = self.alloc_id();
        self.screens.push(screen);
        self.screens.len() - 1
    }

    /// Add an environment from a library profile at the origin; returns its new
    /// index. Prefer [`add_environment_at`] for the place-at-cursor path (#19).
    /// Kept as the convenience origin entry point (demo + tests use it).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_environment(&mut self, profile: &EnvironmentProfile) -> usize {
        self.add_environment_at(profile, Vec3::ZERO)
    }

    /// Add an environment box from a library profile resting on the floor over the
    /// `ground` point; returns its new index. `ground.y` is ignored — the box
    /// always rests on the floor so it doesn't sink below ground.
    pub fn add_environment_at(&mut self, profile: &EnvironmentProfile, ground: Vec3) -> usize {
        let n = self.environments.len() + 1;
        let name = format!("{} {}", profile.name, n);
        // Rest the box on the floor (see Scene::demo) so it doesn't sink below ground.
        let on_floor = Vec3::new(ground.x, profile.default_size[1] * 0.5, ground.z);
        let idx = {
            self.environments
                .push(Environment::from_profile(profile, name, on_floor));
            self.environments.len() - 1
        };
        self.environments[idx].id = self.alloc_id();
        idx
    }

    /// Hand out a fresh, never-reused [`EntityId`].
    pub fn alloc_id(&mut self) -> EntityId {
        self.next_id += 1;
        self.next_id
    }

    /// Assign ids to any entity with `id == 0` and reseed `next_id` past the max
    /// live id. MUST run after every load / MVR import / undo-restore: serde-skip
    /// zeroes ids on a bincode round-trip, so without this every entity shares id
    /// 0 → `NodeKey` collisions → selection/expand cross-talk. This is Blender's
    /// treestore reconstruction on file read.
    pub fn ensure_ids(&mut self) {
        // Seed the counter past the highest id that survived (in-memory adds), so
        // freshly-assigned ids never collide with live ones.
        let mut n = self.next_id;
        for f in &self.fixtures {
            n = n.max(f.id);
        }
        for g in &self.geometry {
            n = n.max(g.id);
        }
        for s in &self.screens {
            n = n.max(s.id);
        }
        for e in &self.environments {
            n = n.max(e.id);
        }
        // Fill every zeroed id (post-load) with a fresh value.
        for f in &mut self.fixtures {
            if f.id == 0 {
                n += 1;
                f.id = n;
            }
        }
        for g in &mut self.geometry {
            if g.id == 0 {
                n += 1;
                g.id = n;
            }
        }
        for s in &mut self.screens {
            if s.id == 0 {
                n += 1;
                s.id = n;
            }
        }
        for e in &mut self.environments {
            if e.id == 0 {
                n += 1;
                e.id = n;
            }
        }
        self.next_id = n;
    }

    /// Resolve an [`EntityId`] to its current `fixtures` index (`None` if stale
    /// after a delete). The outliner converts id→index at the click moment so
    /// `Selection`'s `Vec<usize>` can stay index-based.
    // Consumed by the S2 custom tree widget (id↔index seam).
    pub fn fixture_index_of(&self, id: EntityId) -> Option<usize> {
        self.fixtures.iter().position(|e| e.id == id)
    }
    /// Resolve an [`EntityId`] to its current `geometry` index.
    pub fn geometry_index_of(&self, id: EntityId) -> Option<usize> {
        self.geometry.iter().position(|e| e.id == id)
    }
    /// Resolve an [`EntityId`] to its current `screens` index.
    pub fn screen_index_of(&self, id: EntityId) -> Option<usize> {
        self.screens.iter().position(|e| e.id == id)
    }
    /// Resolve an [`EntityId`] to its current `environments` index.
    pub fn environment_index_of(&self, id: EntityId) -> Option<usize> {
        self.environments.iter().position(|e| e.id == id)
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

    #[test]
    fn entity_ids_unique_and_stable_across_delete() {
        let library = Library::standard();
        let mut scene = Scene::demo(); // one fixture, one environment

        // Build a multi-fixture rig (demo's 1 + 4 added = 5).
        for _ in 0..4 {
            scene.add_fixture(&library.fixtures[0]);
        }
        assert_eq!(scene.fixtures.len(), 5);

        // 1) Every id is unique and non-zero.
        let ids: Vec<EntityId> = scene.fixtures.iter().map(|f| f.id).collect();
        assert!(ids.iter().all(|&id| id != 0), "no entity keeps id 0");
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "fixture ids are unique");

        // 2) index_of resolves each id to its current slot.
        for (i, f) in scene.fixtures.iter().enumerate() {
            assert_eq!(scene.fixture_index_of(f.id), Some(i));
        }

        // 3) Delete a middle fixture → indices shift, but index_of(id) stays
        //    correct for the survivors and the deleted id resolves to None.
        let removed_id = scene.fixtures[1].id;
        let third_id = scene.fixtures[2].id; // was at index 2, should become 1
        scene.fixtures.remove(1);
        assert_eq!(scene.fixture_index_of(removed_id), None, "stale id → None");
        assert_eq!(scene.fixture_index_of(third_id), Some(1), "survivor re-indexed");
        for (i, f) in scene.fixtures.iter().enumerate() {
            assert_eq!(scene.fixture_index_of(f.id), Some(i));
        }

        // 4) A subsequent add never reuses the deleted id.
        let new_idx = scene.add_fixture(&library.fixtures[0]);
        let new_id = scene.fixtures[new_idx].id;
        assert!(!ids.contains(&new_id), "fresh id is never a previously-used id");
        assert_ne!(new_id, removed_id);
    }

    // --- SelectOp truth table (#24) ---------------------------------------

    fn fsel(idx: &[usize]) -> Selection {
        let mut s = Selection::default();
        s.fixtures = idx.to_vec();
        s
    }

    #[test]
    fn select_replace_from_empty_and_nonempty() {
        // Replace with one hit: whole selection becomes that hit.
        let s = apply_select(&Selection::default(), &[SelItem::Fixture(3)], SelectOp::Replace);
        assert_eq!(s, fsel(&[3]));
        // Replace over an existing multi-selection discards the old set.
        let s = apply_select(&fsel(&[0, 1, 2]), &[SelItem::Fixture(7)], SelectOp::Replace);
        assert_eq!(s, fsel(&[7]));
        // Replace with NO hits clears (empty-space click).
        let s = apply_select(&fsel(&[0, 1]), &[], SelectOp::Replace);
        assert_eq!(s, Selection::default());
        // Replace with a multi-hit box keeps every hit.
        let s = apply_select(
            &Selection::default(),
            &[SelItem::Fixture(1), SelItem::Fixture(4)],
            SelectOp::Replace,
        );
        assert_eq!(s, fsel(&[1, 4]));
    }

    #[test]
    fn select_add_subtract_toggle_within_kind() {
        let cur = fsel(&[0, 1]);
        // Add: union, dedup (1 already present).
        let s = apply_select(&cur, &[SelItem::Fixture(1), SelItem::Fixture(2)], SelectOp::Add);
        assert_eq!(s, fsel(&[0, 1, 2]));
        // Subtract: remove the hit, keep the rest.
        let s = apply_select(&cur, &[SelItem::Fixture(1)], SelectOp::Subtract);
        assert_eq!(s, fsel(&[0]));
        // Subtract a non-member is a no-op.
        let s = apply_select(&cur, &[SelItem::Fixture(9)], SelectOp::Subtract);
        assert_eq!(s, cur);
        // Toggle flips membership per hit (1 off, 5 on).
        let s = apply_select(&cur, &[SelItem::Fixture(1), SelItem::Fixture(5)], SelectOp::Toggle);
        assert_eq!(s, fsel(&[0, 5]));
        // Add from empty seeds the hits' own kind.
        let s = apply_select(&Selection::default(), &[SelItem::Fixture(2)], SelectOp::Add);
        assert_eq!(s, fsel(&[2]));
    }

    #[test]
    fn select_additive_ops_stay_within_active_kind() {
        // A fixture selection + a box that hit a geometry object: Add ignores the
        // off-kind hit (keeps the one-kind-at-a-time invariant).
        let s = apply_select(&fsel(&[0]), &[SelItem::Geometry(3)], SelectOp::Add);
        assert_eq!(s, fsel(&[0]));
        // But Replace switches kind to the (dominant) hit kind.
        let s = apply_select(&fsel(&[0]), &[SelItem::Geometry(3)], SelectOp::Replace);
        let mut want = Selection::default();
        want.geometry = vec![3];
        assert_eq!(s, want);
    }

    #[test]
    fn select_toggle_offkind_single_switches_kind() {
        // A single Ctrl-click (Toggle, ONE hit) on an OFF-kind item switches the
        // selection to it (matching the old toggle_geometry/screen clear-and-switch),
        // instead of no-op'ing because the active kind was fixtures (the regression).
        let s = apply_select(&fsel(&[0, 1]), &[SelItem::Geometry(3)], SelectOp::Toggle);
        let mut want = Selection::default();
        want.geometry = vec![3];
        assert_eq!(s, want);
        // A SAME-kind single Toggle still toggles within the set (deselects one).
        let s = apply_select(&fsel(&[0, 1]), &[SelItem::Fixture(1)], SelectOp::Toggle);
        assert_eq!(s, fsel(&[0]));
    }

    #[test]
    fn select_single_only_kinds_always_replace() {
        // Environment + World are single-only: even a Toggle/Add resolves to a
        // clean replace of that node.
        let s = apply_select(&fsel(&[0, 1]), &[SelItem::Environment(2)], SelectOp::Toggle);
        assert_eq!(s, Selection::environment(2));
        let s = apply_select(&fsel(&[0]), &[SelItem::World], SelectOp::Add);
        assert_eq!(s, Selection::world());
    }

    #[test]
    fn ensure_ids_reassigns_after_serde_roundtrip() {
        // serde-skip zeroes ids; ensure_ids must reassign unique ones (the load /
        // undo-restore path). Simulate the round-trip by zeroing ids.
        let library = Library::standard();
        let mut scene = Scene::demo();
        for _ in 0..3 {
            scene.add_fixture(&library.fixtures[0]);
        }
        scene.add_environment(&library.environments[0]);

        // Round-trip wipe (what bincode deserialize of serde-skip fields does).
        scene.next_id = 0;
        for f in &mut scene.fixtures {
            f.id = 0;
        }
        for e in &mut scene.environments {
            e.id = 0;
        }

        scene.ensure_ids();

        let mut all: Vec<EntityId> = scene.fixtures.iter().map(|f| f.id).collect();
        all.extend(scene.environments.iter().map(|e| e.id));
        assert!(all.iter().all(|&id| id != 0), "all reassigned");
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "ids unique across entity kinds");
    }
}
