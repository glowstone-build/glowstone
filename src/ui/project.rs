//! The `.archie` project format — one self-contained binary file holding the
//! WHOLE show: the scene (fixtures, environments, world, imported geometry — with
//! the GDTF / model / HDRI bytes BUNDLED inline), the camera, render settings,
//! preferences, DMX patch + config, selection groups and cues.
//!
//! It's a `bincode` dump of the same serde types the app runs on, so it captures
//! the exact in-memory state — not a lossy interchange (that's what MVR is for).
//! A short magic + `FORMAT` version header guards against reading a stale/foreign
//! layout. GDTF definitions aren't serialised as parsed trees: their original
//! `.gdtf` archive bytes are bundled and re-parsed on open (so a saved show needs
//! no external fixture files), while model / HDRI bytes ride along inside the
//! serialised `Scene` (they're `Arc<Vec<u8>>`, serialised via serde's `rc`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::cues::CueEngine;
use super::panels::SceneSort;
use super::windows::Preferences;
use super::SelectionGroup;
use crate::dmx::{DmxConfig, PatchTable};
use crate::renderer::camera::OrbitCamera;
use crate::scene::{RenderSettings, Scene};

/// File magic — `ARCHIE` + a NUL + the byte form of the major version.
const MAGIC: &[u8] = b"ARCHIE\0";
/// On-disk format version. Bumped on an incompatible layout change.
/// v2: `mvr::GeometryModel` gained a per-`<Geometry3D>` `matrix` field.
/// v3: `Scene` gained `screens: Vec<LedScreen>` (LED walls).
/// v4: `LedScreen` gained content sources (Image/NDI/CITP/PixelMap) + `pixel_shape`.
/// v5: `Environment` gained `uniformity` (haze cluster control).
/// v6: `Environment` gained `cluster_contrast`.
/// v7: `Scene` gained `render: RenderConfig` (the persisted render-target setup).
/// v8: `Fixture` gained `source` (provenance chip). Field is LAST in serde
///     order with `#[serde(default)]`. NOTE: `read` rejects any version != FORMAT
///     up front, so pre-v8 files surface a clean "unsupported version" error
///     rather than mis-decoding — positional bincode never sees the missing
///     trailing field. The `#[serde(default)]` keeps the in-memory struct sound
///     and future-proofs any optional/skipped-field shifts above it.
/// v9: `Scene` gained `pyro: Vec<PyroDevice>` (CO2 cannons + cold-spark machines),
///     appended LAST in `Scene`'s serde order (after `render`). The per-device
///     `PyroPatch` (universe/address) persists with the device — pyro is NOT in
///     the fixture `PatchTable`. The transient particle sim + `armed`/`fire`/`id`
///     stay `#[serde(skip)]` (runtime-only, like `LedScreen::frame`).
/// v10: `PyroDevice` gained `dissipation` (CO2 smoke hang time), LAST in its serde
///     order with `#[serde(default)]` — show data for the new inspector slider, so
///     persisted with the device (NOT serde-skip).
/// v11: `PyroDevice` gained `speed` (CO2 jet exit velocity, decoupled from throw),
///     LAST after `dissipation`. `PyroDevice` decodes via a hand-written, version-
///     aware `Deserialize` (see `scene::pyro`) that reads each trailing field only
///     when the file's FORMAT is new enough — so v9/v10 files keep loading.
/// v12: `PyroDevice` gained `viewport_hq` (live-preview CO2 quality toggle), LAST
///     after `speed`. Same version-aware decode keeps v9–v11 files loading.
/// v13: `PyroDevice` gained `thickness` (CO2 visual density), LAST after `viewport_hq`.
///     Version-aware decode keeps v9–v12 files loading.
pub const FORMAT: u32 = 13;
/// Oldest on-disk FORMAT this build can still decode. Versions in
/// `MIN_READ_FORMAT..=FORMAT` load via the version-aware deserializers (which skip
/// trailing fields that postdate the file — see [`crate::scene::pyro::LOADING_FORMAT`]);
/// older files predate fields we don't down-migrate and are rejected cleanly.
pub const MIN_READ_FORMAT: u32 = 9;
/// The project file extension (no dot).
pub const EXT: &str = "archie";

/// Borrowed view of everything to save — avoids cloning the (possibly large)
/// scene. Field order MUST match [`Project`] (bincode is positional).
#[derive(Serialize)]
pub struct ProjectRef<'a> {
    pub format: u32,
    pub scene: &'a Scene,
    /// Per-fixture GDTF spec key (aligned to `scene.fixtures`); `None` = plain.
    pub fixture_specs: Vec<Option<String>>,
    /// `spec` → original `.gdtf` archive bytes, deduped across fixtures.
    pub gdtf_assets: HashMap<String, Vec<u8>>,
    pub camera: &'a OrbitCamera,
    pub settings: &'a RenderSettings,
    pub prefs: &'a Preferences,
    pub groups: &'a [SelectionGroup],
    pub cues: &'a CueEngine,
    pub scene_sort: SceneSort,
    pub patch: &'a PatchTable,
    pub dmx_config: &'a DmxConfig,
}

/// Owned project read back from disk.
#[derive(Deserialize)]
pub struct Project {
    pub format: u32,
    pub scene: Scene,
    pub fixture_specs: Vec<Option<String>>,
    pub gdtf_assets: HashMap<String, Vec<u8>>,
    pub camera: OrbitCamera,
    pub settings: RenderSettings,
    pub prefs: Preferences,
    pub groups: Vec<SelectionGroup>,
    pub cues: CueEngine,
    pub scene_sort: SceneSort,
    pub patch: PatchTable,
    pub dmx_config: DmxConfig,
}

/// Serialise + write atomically (write a temp sibling, then rename) so a crash
/// mid-write can't corrupt an existing project.
pub fn write(path: &Path, project: &ProjectRef) -> Result<(), String> {
    let body = bincode::serialize(project).map_err(|e| format!("encode: {e}"))?;
    let mut bytes = Vec::with_capacity(body.len() + MAGIC.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&project.format.to_le_bytes());
    bytes.extend_from_slice(&body);
    let tmp = path.with_extension("archie.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Read + validate magic/version, then decode.
pub fn read(path: &Path) -> Result<Project, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let head = MAGIC.len() + 4;
    if bytes.len() < head || &bytes[..MAGIC.len()] != MAGIC {
        return Err("not an .archie project (bad magic)".into());
    }
    let ver = u32::from_le_bytes([
        bytes[MAGIC.len()],
        bytes[MAGIC.len() + 1],
        bytes[MAGIC.len() + 2],
        bytes[MAGIC.len() + 3],
    ]);
    if ver > FORMAT {
        return Err(format!(
            "project version {ver} is newer than this build supports (max {FORMAT}); update the app"
        ));
    }
    if ver < MIN_READ_FORMAT {
        return Err(format!(
            "project version {ver} is too old to open in this build (min {MIN_READ_FORMAT})"
        ));
    }
    // The decode is version-aware: structs skip trailing fields that postdate `ver`
    // (positional bincode has no field names — see `scene::pyro::LOADING_FORMAT`).
    // Reset the flag whatever the outcome so later (non-file) decodes read everything.
    crate::scene::pyro::LOADING_FORMAT.with(|v| v.set(ver));
    let decoded = bincode::deserialize::<Project>(&bytes[head..]);
    crate::scene::pyro::LOADING_FORMAT.with(|v| v.set(u32::MAX));
    let mut project = decoded.map_err(|e| format!("decode: {e}"))?;
    project.format = FORMAT; // migrated up to the current format in memory
    intern_geometry_resources(&mut project.scene);
    Ok(project)
}

/// Re-share identical geometry blobs after load.
///
/// bincode (via serde's `rc` feature) deserialises every `Arc<Vec<u8>>`
/// INDEPENDENTLY — so N imported objects that all referenced the same resource
/// file come back as N distinct `Arc`s holding identical bytes. The renderer
/// caches and instances geometry by `Arc::as_ptr`, so without re-sharing them an
/// N-copy set piece (truss, deck, chair) becomes N unique meshes = N draw calls
/// instead of one instanced draw — exactly the dedup the live import gets for
/// free. Re-intern by file name (the `.archie` resource key — save bundles one
/// blob per name, see `mvr::write`), restoring import-time sharing so static
/// instancing collapses the forward pass.
fn intern_geometry_resources(scene: &mut Scene) {
    use std::sync::Arc;
    let mut interned: HashMap<String, Arc<Vec<u8>>> = HashMap::new();
    for obj in &mut scene.geometry {
        for m in &mut obj.models {
            match interned.get(&m.file) {
                Some(shared) => m.glb = Arc::clone(shared),
                None => {
                    interned.insert(m.file.clone(), Arc::clone(&m.glb));
                }
            }
        }
    }
}

// --- recent projects + autosave locations (per-user dirs) -------------------

fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "Embedder", "previz")
}

/// The crash-recovery autosave path (`<cache>/last-session.archie`).
pub fn autosave_path() -> Option<PathBuf> {
    let d = dirs()?;
    let dir = d.cache_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("last-session.archie"))
}

fn recent_path() -> Option<PathBuf> {
    let d = dirs()?;
    let dir = d.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("recent.json"))
}

/// The recent-project list, most-recent first, pruned to existing files.
pub fn load_recent() -> Vec<PathBuf> {
    let Some(p) = recent_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&p) else { return Vec::new() };
    let list: Vec<PathBuf> = serde_json::from_str(&text).unwrap_or_default();
    list.into_iter().filter(|p| p.exists()).take(12).collect()
}

/// Push `path` to the front of the recent list (deduped, capped).
pub fn push_recent(path: &Path) {
    let Some(rp) = recent_path() else { return };
    let mut list = load_recent();
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(12);
    if let Ok(text) = serde_json::to_string_pretty(&list) {
        let _ = std::fs::write(&rp, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ref<'a>(
        scene: &'a Scene,
        camera: &'a OrbitCamera,
        settings: &'a RenderSettings,
        prefs: &'a Preferences,
        cues: &'a CueEngine,
        patch: &'a PatchTable,
        config: &'a DmxConfig,
    ) -> ProjectRef<'a> {
        ProjectRef {
            format: FORMAT,
            scene,
            fixture_specs: Vec::new(),
            gdtf_assets: HashMap::new(),
            camera,
            settings,
            prefs,
            groups: &[],
            cues,
            scene_sort: SceneSort::Name,
            patch,
            dmx_config: config,
        }
    }

    #[test]
    fn project_roundtrips_scene_camera_settings() {
        let mut scene = Scene::default();
        scene.world.brightness = 2.5;
        scene.world.rotation = 0.75;
        // Render config persists with the show (FORMAT 7) — set non-defaults.
        scene.render.res_x = 3840;
        scene.render.res_y = 2160;
        scene.render.resolution_percentage = 75;
        scene.render.max_samples = 128;
        scene.render.format = crate::scene::RenderFormat::Exr;
        scene.render.out_path = "/tmp/shot.exr".to_string();
        let fixtures = scene.fixtures.len();
        let mut camera = OrbitCamera::default();
        camera.distance = 33.0;
        let settings = RenderSettings::default();
        let prefs = Preferences::default();
        let cues = CueEngine::default();
        let patch = PatchTable::default();
        let config = DmxConfig::default();
        let pr = empty_ref(&scene, &camera, &settings, &prefs, &cues, &patch, &config);

        let path = std::env::temp_dir().join("previz-roundtrip-test.archie");
        write(&path, &pr).expect("write");
        let loaded = read(&path).expect("read");
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.scene.fixtures.len(), fixtures);
        assert!((loaded.scene.world.brightness - 2.5).abs() < 1e-6);
        assert!((loaded.scene.world.rotation - 0.75).abs() < 1e-6);
        assert!((loaded.camera.distance - 33.0).abs() < 1e-6);
        assert!(matches!(loaded.scene_sort, SceneSort::Name));
        // Render setup survived the round-trip (no more serde-skip).
        assert_eq!(loaded.scene.render.res_x, 3840);
        assert_eq!(loaded.scene.render.res_y, 2160);
        assert_eq!(loaded.scene.render.resolution_percentage, 75);
        assert_eq!(loaded.scene.render.max_samples, 128);
        assert!(matches!(loaded.scene.render.format, crate::scene::RenderFormat::Exr));
        assert_eq!(loaded.scene.render.out_path, "/tmp/shot.exr");
    }

    #[test]
    fn read_rejects_non_archie_file() {
        let path = std::env::temp_dir().join("previz-bad-magic-test.archie");
        std::fs::write(&path, b"this is not a project file at all").unwrap();
        let err = read(&path);
        let _ = std::fs::remove_file(&path);
        assert!(err.is_err());
    }
}
