//! The `.glow` project format — one self-contained binary file holding the
//! WHOLE show: the scene (fixtures, environments, world, imported geometry — with
//! the GDTF / model / HDRI bytes BUNDLED inline), the camera, render settings,
//! preferences, DMX patch + config, selection groups and cues.
//!
//! It's a single MessagePack dump of the same serde types the app runs on, so it
//! captures the exact in-memory state — not a lossy interchange (that's what MVR is
//! for). The encoding is **self-describing**: structs are written as field-name maps
//! ([`rmp_serde::to_vec_named`]), so a save is matched by NAME on load, not by byte
//! position. That's what makes the format **backwards compatible by construction**:
//!
//! * an OLDER save opens in a NEWER build — fields the old file never had simply take
//!   their [`serde` defaults](https://serde.rs/field-attrs.html#default) (every
//!   persisted struct carries `#[serde(default)]`, so this needs no per-version code);
//! * a NEWER save opens in an OLDER build best-effort — fields it doesn't recognise
//!   are ignored.
//!
//! So you can always load your old shows after updating. The only changes that AREN'T
//! free are renaming a field (use `#[serde(alias)]`), changing a field's type, or
//! removing/reordering an enum variant — those still need a deliberate migration.
//!
//! A short magic + `FORMAT` header rides in front for diagnostics (and as an escape
//! hatch if we ever make a truly breaking change); it does NOT gate loading. GDTF
//! definitions aren't serialised as parsed trees: their original `.gdtf` archive bytes
//! are bundled and re-parsed on open (so a saved show needs no external fixture files),
//! while model / HDRI bytes ride along inside the serialised `Scene` (they're
//! `Arc<Vec<u8>>`, serialised via serde's `rc`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::cues::CueEngine;
use super::outliner::SceneSort;
use super::windows::Preferences;
use super::SelectionGroup;
use crate::dmx::{DmxConfig, PatchTable};
use crate::renderer::camera::OrbitCamera;
use crate::scene::{RenderSettings, Scene};

/// File magic for a glowstone project (`.glow`).
const MAGIC: &[u8] = b"GLOW\0";

/// On-disk format version — a diagnostic stamp, NOT a load gate. Because the body is
/// self-describing (see the module docs), adding or removing fields stays compatible
/// across versions without bumping this. Bump it only on a genuinely breaking change
/// (a field rename without an alias, a type change, an enum-variant removal); the
/// loader records it and can warn, but never refuses a file on version alone.
pub const FORMAT: u32 = 1;
/// The project file extension (no dot).
pub const EXT: &str = "glow";

/// Borrowed view of everything to save — avoids cloning the (possibly large)
/// scene. Field NAMES must match [`Project`] (the body is decoded by field name).
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
#[derive(Deserialize, Default)]
#[serde(default)]
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
    // `to_vec_named` writes structs as field-name maps → the body is self-describing,
    // which is what lets older saves load in newer builds (see the module docs).
    let body = rmp_serde::to_vec_named(project).map_err(|e| format!("encode: {e}"))?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + body.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&project.format.to_le_bytes());
    bytes.extend_from_slice(&body);
    let tmp = path.with_extension("glow.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Read a project. The magic is checked (so a foreign file fails cleanly), then the
/// self-describing body is decoded by field NAME — so a save from any version loads:
/// missing fields take their `#[serde(default)]`, unknown fields are ignored. The
/// version stamp only sharpens the error message for a file from a newer build.
pub fn read(path: &Path) -> Result<Project, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    if !bytes.starts_with(MAGIC) {
        return Err("not a glowstone project (bad magic)".into());
    }
    let head = MAGIC.len() + 4;
    if bytes.len() < head {
        return Err("truncated project file".into());
    }
    let ver = u32::from_le_bytes([
        bytes[MAGIC.len()],
        bytes[MAGIC.len() + 1],
        bytes[MAGIC.len() + 2],
        bytes[MAGIC.len() + 3],
    ]);
    let mut project: Project = rmp_serde::from_slice(&bytes[head..]).map_err(|e| {
        if ver > FORMAT {
            format!("this show was saved by a newer glowstone (v{ver} > v{FORMAT}); some of it may not load — {e}")
        } else {
            format!("decode: {e}")
        }
    })?;
    project.format = FORMAT; // stamp the in-memory copy as current
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
/// free. Re-intern by file name (the `.glow` resource key — save bundles one
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
    directories::ProjectDirs::from("build", "glowstone", "glowstone")
}

/// The crash-recovery autosave path (`<cache>/last-session.glow`).
pub fn autosave_path() -> Option<PathBuf> {
    let d = dirs()?;
    let dir = d.cache_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("last-session.glow"))
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
        // Render config persists with the show — set non-defaults.
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

        let path = std::env::temp_dir().join("glowstone-roundtrip-test.glow");
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
    fn read_rejects_non_project_file() {
        let path = std::env::temp_dir().join("glowstone-bad-magic-test.glow");
        std::fs::write(&path, b"this is not a project file at all").unwrap();
        let err = read(&path);
        let _ = std::fs::remove_file(&path);
        assert!(err.is_err());
    }

    /// The version stamp does NOT gate loading: a file stamped by a "newer" build still
    /// opens (a self-describing body just ignores fields it doesn't recognise).
    #[test]
    fn newer_version_stamp_still_loads() {
        let scene = Scene::default();
        let camera = OrbitCamera::default();
        let settings = RenderSettings::default();
        let prefs = Preferences::default();
        let cues = CueEngine::default();
        let patch = PatchTable::default();
        let config = DmxConfig::default();
        let mut pr = empty_ref(&scene, &camera, &settings, &prefs, &cues, &patch, &config);
        pr.format = FORMAT + 9; // pretend a future build wrote this
        let path = std::env::temp_dir().join("glowstone-newer-stamp.glow");
        write(&path, &pr).expect("write");
        let loaded = read(&path).expect("a newer-stamped file still loads");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.format, FORMAT); // re-stamped to current in memory
    }

    /// THE backwards-compat guarantee: a save written by an OLDER build (fewer fields)
    /// loads in this one — known fields keep their values, and fields the old save never
    /// had fall back to their REAL defaults (not zeroed). Shown on `World`; the same
    /// holds for every persisted struct (they all carry `#[serde(default)]`).
    #[test]
    fn older_save_missing_fields_loads_with_defaults() {
        // The bytes an older glowstone (which only knew two `World` fields) would write —
        // the encoding matches by field NAME, so this IS that older save's on-disk shape.
        #[derive(Serialize)]
        struct OldWorld {
            brightness: f32,
            rotation: f32,
        }
        let bytes = rmp_serde::to_vec_named(&OldWorld { brightness: 4.0, rotation: 0.25 }).unwrap();
        let w: crate::scene::World = rmp_serde::from_slice(&bytes).expect("older World loads");
        assert_eq!(w.brightness, 4.0); // a field the old save HAD is preserved
        assert_eq!(w.rotation, 0.25);
        // Fields the old save lacked take `World::default()` — note `true` / `1.0`, NOT
        // type-zero, proving the real default is used.
        assert!(w.show_background);
        assert_eq!(w.ambient, 1.0);
    }

    /// Pyro devices ride the bincode core, so they survive a save/load round-trip.
    #[test]
    fn pyro_devices_roundtrip() {
        use crate::scene::pyro::{PyroDevice, PyroKind};
        let mut scene = Scene::default();
        let lib = crate::scene::Library::standard();
        let prof = lib.pyro.iter().find(|p| p.kind == PyroKind::Co2Jet).unwrap();
        let mut dev = PyroDevice::from_profile(prof, "Cannon 1", glam::Mat4::IDENTITY);
        dev.throw_m = 14.0;
        dev.thickness = 3.3;
        scene.pyro.push(dev);
        let camera = OrbitCamera::default();
        let settings = RenderSettings::default();
        let prefs = Preferences::default();
        let cues = CueEngine::default();
        let patch = PatchTable::default();
        let config = DmxConfig::default();
        let pr = empty_ref(&scene, &camera, &settings, &prefs, &cues, &patch, &config);

        let path = std::env::temp_dir().join("glowstone-pyro-roundtrip.glow");
        write(&path, &pr).expect("write");
        let loaded = read(&path).expect("read");
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.scene.pyro.len(), 1);
        assert_eq!(loaded.scene.pyro[0].name, "Cannon 1");
        assert!((loaded.scene.pyro[0].throw_m - 14.0).abs() < 1e-6);
        assert!((loaded.scene.pyro[0].thickness - 3.3).abs() < 1e-6);
    }
}
