//! The `.glow` project format — one self-contained binary file holding the
//! WHOLE show: the scene (fixtures, environments, world, imported geometry — with
//! the GDTF / model / HDRI bytes BUNDLED inline), the camera, render settings,
//! preferences, DMX patch + config, selection groups and cues. Legacy `.archie`
//! files (the previous extension/magic) are still read.
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
use super::outliner::SceneSort;
use super::windows::Preferences;
use super::SelectionGroup;
use crate::dmx::{DmxConfig, PatchTable};
use crate::renderer::camera::OrbitCamera;
use crate::scene::{RenderSettings, Scene};

/// File magic for a glowstone project (`.glow`).
const MAGIC: &[u8] = b"GLOW\0";
/// Legacy magic — files saved as `.archie` by older builds. Still readable.
const LEGACY_MAGIC: &[u8] = b"ARCHIE\0";

/// On-disk format version of the **core** (the positional `bincode` body). Bumped
/// only on an incompatible core-layout change; evolving show data that needs to stay
/// version-tolerant (currently the pyro devices) lives in a SELF-DESCRIBING trailer
/// instead, so it never forces a core bump.
///
/// History (core layout):
/// v2–v7: incremental `Scene`/`Environment`/`LedScreen` field additions.
/// v8: `Fixture` gained `source` (the last `.archie` core layout — pre-pyro).
/// v9: the `.glow` container — IDENTICAL core layout to v8 (pyro was moved OUT of the
///     positional core into a JSON trailer; see below), plus that trailer. So a v8
///     `.archie` and a v9 `.glow` share the exact same bincode body; only `.glow`
///     carries the trailing pyro section.
///
/// Pyro persistence (the trailer): after the bincode core we append
/// `serde_json(scene.pyro)` — a self-describing section. `PyroDevice` is a plain
/// `#[derive(Serialize, Deserialize)]` with `#[serde(default)]` on its evolving
/// fields, so adding a field needs NO version bump and NO hand-written decode: an
/// older save simply lacks the key and it defaults. `read` skips the trailer silently
/// if it's absent (legacy `.archie`) or fails to decode.
pub const FORMAT: u32 = 9;
/// Oldest core layout this build can decode. Pre-v8 cores have different field
/// layouts we don't down-migrate, so they're rejected cleanly (the loader reports
/// and skips them rather than mis-decoding).
pub const MIN_READ_FORMAT: u32 = 8;
/// The project file extension (no dot).
pub const EXT: &str = "glow";
/// Legacy extension still accepted by the open dialog.
pub const LEGACY_EXT: &str = "archie";

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
    // The positional bincode CORE (pyro is `#[serde(skip)]` on Scene, so it is NOT in
    // here) ...
    let body = bincode::serialize(project).map_err(|e| format!("encode: {e}"))?;
    // ... then the SELF-DESCRIBING pyro trailer (JSON). `read` reads the core with
    // `deserialize_from` (which stops exactly at the core's end), so the trailer needs
    // no length prefix — it's simply the rest of the file.
    let pyro_json = serde_json::to_vec(&project.scene.pyro).map_err(|e| format!("encode pyro: {e}"))?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + body.len() + pyro_json.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&project.format.to_le_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&pyro_json);
    let tmp = path.with_extension("glow.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Read + validate magic/version, decode the bincode core, then the (optional)
/// self-describing pyro trailer. Tolerant by design — see [`FORMAT`]:
/// * a version newer than this build → clean error (update the app);
/// * a core layout older than [`MIN_READ_FORMAT`] → clean error (skipped, not crashed);
/// * a missing or unreadable pyro trailer → silently skipped (the rest still loads).
pub fn read(path: &Path) -> Result<Project, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    // Accept either the current `.glow` magic or the legacy `.archie` magic.
    let magic_len = if bytes.starts_with(MAGIC) {
        MAGIC.len()
    } else if bytes.starts_with(LEGACY_MAGIC) {
        LEGACY_MAGIC.len()
    } else {
        return Err("not a glowstone project (bad magic)".into());
    };
    let head = magic_len + 4;
    if bytes.len() < head {
        return Err("truncated project file".into());
    }
    let ver = u32::from_le_bytes([bytes[magic_len], bytes[magic_len + 1], bytes[magic_len + 2], bytes[magic_len + 3]]);
    if ver > FORMAT {
        return Err(format!(
            "project version {ver} is newer than this build supports (max {FORMAT}); update the app"
        ));
    }
    if ver < MIN_READ_FORMAT {
        return Err(format!(
            "project version {ver} predates this build's readable formats (min {MIN_READ_FORMAT}) — skipping"
        ));
    }
    // Decode the bincode CORE from a cursor so it stops exactly at the core's end,
    // leaving any trailing pyro section for the next step.
    let mut cur = std::io::Cursor::new(&bytes[head..]);
    let mut project: Project =
        bincode::deserialize_from(&mut cur).map_err(|e| format!("decode: {e}"))?;
    // The self-describing pyro trailer (JSON), if present. Absent in legacy `.archie`;
    // skipped (with a warning) rather than failing the whole load if it can't decode.
    let consumed = cur.position() as usize;
    let rest = &bytes[head + consumed..];
    if !rest.is_empty() {
        match serde_json::from_slice::<Vec<crate::scene::PyroDevice>>(rest) {
            Ok(pyro) => project.scene.pyro = pyro,
            Err(e) => log::warn!("skipping unreadable pyro section in {}: {e}", path.display()),
        }
    }
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
    directories::ProjectDirs::from("dev", "Embedder", "glowstone")
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

    /// A legacy `.archie` file (old magic, v8 core, NO pyro trailer) still loads: the
    /// core layout is unchanged (pyro moved out of the positional stream), pyro is empty.
    #[test]
    fn reads_legacy_archie_without_trailer() {
        let mut scene = Scene::default();
        scene.world.brightness = 1.7;
        let camera = OrbitCamera::default();
        let settings = RenderSettings::default();
        let prefs = Preferences::default();
        let cues = CueEngine::default();
        let patch = PatchTable::default();
        let config = DmxConfig::default();
        // Build a legacy-shaped file by hand: ARCHIE magic + v8 + bincode core, no trailer.
        let mut pr = empty_ref(&scene, &camera, &settings, &prefs, &cues, &patch, &config);
        pr.format = 8;
        let body = bincode::serialize(&pr).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(LEGACY_MAGIC);
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&body);
        let path = std::env::temp_dir().join("glowstone-legacy-archie.archie");
        std::fs::write(&path, &bytes).unwrap();
        let loaded = read(&path).expect("legacy .archie loads");
        let _ = std::fs::remove_file(&path);
        assert!((loaded.scene.world.brightness - 1.7).abs() < 1e-6);
        assert!(loaded.scene.pyro.is_empty());
        assert_eq!(loaded.format, FORMAT); // migrated up in memory
    }

    /// Pyro persists via the self-describing JSON trailer (NOT the positional core),
    /// so devices survive the round-trip and the core stays untouched.
    #[test]
    fn pyro_devices_roundtrip_via_trailer() {
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
