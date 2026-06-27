use super::*;

impl Ui {
    /// Gather + serialise the whole project to `path` (bundling GDTF archive
    /// bytes; model + HDRI bytes ride along inside the serialised `Scene`).
    fn write_project(
        &self,
        path: &std::path::Path,
        scene: &Scene,
        camera: &OrbitCamera,
        dmx: &crate::dmx::DmxIo,
    ) -> Result<(), String> {
        let fixture_specs: Vec<Option<String>> = scene
            .fixtures
            .iter()
            .map(|f| f.gdtf.as_ref().map(|g| g.spec.clone()).filter(|s| !s.is_empty()))
            .collect();
        let mut gdtf_assets: HashMap<String, Vec<u8>> = HashMap::new();
        for f in &scene.fixtures {
            if let Some(g) = &f.gdtf {
                if !g.spec.is_empty() {
                    if let Some(raw) = &g.raw {
                        gdtf_assets.entry(g.spec.clone()).or_insert_with(|| raw.as_ref().clone());
                    }
                }
            }
        }
        let pr = project::ProjectRef {
            format: project::FORMAT,
            scene,
            fixture_specs,
            gdtf_assets,
            camera,
            settings: &self.settings,
            prefs: &self.prefs,
            groups: &self.groups,
            cues: &self.cues,
            scene_sort: self.scene_sort,
            patch: dmx.patch(),
            dmx_config: dmx.config(),
        };
        project::write(path, &pr)
    }

    /// Save to the current path, or fall back to Save As if untitled.
    pub(super) fn save_project(&mut self, scene: &Scene, camera: &OrbitCamera, dmx: &crate::dmx::DmxIo) {
        match self.current_path.clone() {
            Some(path) => self.save_to(&path, scene, camera, dmx),
            None => self.save_project_as(scene, camera, dmx),
        }
    }

    /// Prompt for a destination, then save (forcing the `.glow` extension).
    pub(super) fn save_project_as(&mut self, scene: &Scene, camera: &OrbitCamera, dmx: &crate::dmx::DmxIo) {
        let name = self
            .current_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("show.{}", project::EXT));
        if let Some(mut path) = rfd::FileDialog::new()
            .add_filter("glowstone project", &[project::EXT])
            .set_file_name(&name)
            .save_file()
        {
            if path.extension().and_then(|e| e.to_str()) != Some(project::EXT) {
                path.set_extension(project::EXT);
            }
            self.save_to(&path, scene, camera, dmx);
        }
    }

    fn save_to(
        &mut self,
        path: &std::path::Path,
        scene: &Scene,
        camera: &OrbitCamera,
        dmx: &crate::dmx::DmxIo,
    ) {
        match self.write_project(path, scene, camera, dmx) {
            Ok(()) => {
                self.current_path = Some(path.to_path_buf());
                self.saved_state_id = self.undo.state_id(); // mark the document clean
                project::push_recent(path);
                self.recent = project::load_recent();
                let name = path.file_name().map(|s| s.to_string_lossy().into_owned());
                self.notify.success(format!("Saved {}", name.as_deref().unwrap_or("project")));
            }
            Err(e) => self.notify.error(format!("Save failed: {e}")),
        }
    }

    /// Prompt for a project file (`.glow`), then open it.
    pub(super) fn open_project_dialog(
        &mut self,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("glowstone project", &[project::EXT])
            .pick_file()
        {
            self.open_project(&path, scene, camera, dmx);
        }
    }

    /// Open a project file, replacing the current scene + UI/DMX state.
    pub fn open_project(
        &mut self,
        path: &std::path::Path,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        match project::read(path) {
            Ok(p) => {
                self.apply_project(p, scene, camera, dmx);
                self.current_path = Some(path.to_path_buf());
                self.saved_state_id = self.undo.state_id(); // freshly opened ⇒ clean
                project::push_recent(path);
                self.recent = project::load_recent();
                self.show_splash = false;
                let name = path.file_name().map(|s| s.to_string_lossy().into_owned());
                self.notify.success(format!(
                    "Opened {} · {} fixtures",
                    name.as_deref().unwrap_or("project"),
                    scene.fixtures.len()
                ));
            }
            Err(e) => self.notify.error(format!("Open failed: {e}")),
        }
    }

    fn apply_project(
        &mut self,
        p: project::Project,
        scene: &mut Scene,
        camera: &mut OrbitCamera,
        dmx: &mut crate::dmx::DmxIo,
    ) {
        *scene = p.scene;
        // serde-skip zeroed every EntityId on load → reassign stable ids before
        // the outliner addresses any row (the Fixture.gdtf snapshot-trap class).
        scene.ensure_ids();
        // Re-link each fixture's GDTF by re-parsing the bundled archive (one parse
        // per unique spec, Arc-shared so the renderer's per-type model cache and
        // the GPU wheel atlas stay deduped).
        let mut cache: HashMap<String, Arc<crate::gdtf::GdtfFixture>> = HashMap::new();
        for (i, f) in scene.fixtures.iter_mut().enumerate() {
            let Some(spec) = p.fixture_specs.get(i).cloned().flatten() else { continue };
            let arc = if let Some(a) = cache.get(&spec) {
                a.clone()
            } else if let Some(bytes) = p.gdtf_assets.get(&spec) {
                match crate::gdtf::GdtfFixture::load_bytes(bytes) {
                    Ok(mut g) => {
                        g.spec = spec.clone();
                        g.raw = Some(Arc::new(bytes.clone()));
                        let a = Arc::new(g);
                        cache.insert(spec.clone(), a.clone());
                        a
                    }
                    Err(e) => {
                        self.notify.warn(format!("Could not re-link GDTF {spec}: {e}"));
                        continue;
                    }
                }
            } else {
                continue;
            };
            f.gdtf = Some(arc);
            f.sync_mode();
        }
        // Register the project's fixture types in the library (Replace / add picker).
        for a in cache.values() {
            if !self.library.gdtf.iter().any(|g| g.spec == a.spec) {
                self.library.gdtf.push(a.clone());
            }
        }
        *camera = p.camera;
        self.settings = p.settings;
        self.prefs = p.prefs;
        self.groups = p.groups;
        self.cues = p.cues;
        self.scene_sort = p.scene_sort;
        *dmx.patch_mut() = p.patch;
        *dmx.config_mut() = p.dmx_config;
        self.selection = Selection::default();
    }

    /// Start a fresh, empty project.
    pub(super) fn new_project(&mut self, scene: &mut Scene, camera: &mut OrbitCamera, dmx: &mut crate::dmx::DmxIo) {
        *scene = Scene::default();
        *dmx.patch_mut() = crate::dmx::PatchTable::default();
        self.groups.clear();
        self.cues = cues::CueEngine::default();
        self.selection = Selection::default();
        self.current_path = None;
        self.saved_state_id = self.undo.state_id(); // empty doc ⇒ clean
        camera.frame(Vec3::ZERO, 12.0);
        self.show_splash = false;
    }

    /// Periodic crash-recovery autosave — writes the whole project to the cache
    /// dir every ~20 s when there's content. Driven from `app::render` with `dt`.
    pub fn autosave_tick(
        &mut self,
        scene: &Scene,
        camera: &OrbitCamera,
        dmx: &crate::dmx::DmxIo,
        dt: f32,
    ) {
        self.autosave_timer += dt;
        if self.autosave_timer < 20.0 {
            return;
        }
        self.autosave_timer = 0.0;
        if scene.fixtures.is_empty() && scene.geometry.is_empty() {
            return;
        }
        if let Some(path) = project::autosave_path() {
            if let Err(e) = self.write_project(&path, scene, camera, dmx) {
                self.notify.warn(format!("Autosave failed: {e}"));
            }
        }
    }

    /// Import any `.gdtf` / `.mvr` files dropped onto the window.
    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context, scene: &mut Scene, camera: &mut OrbitCamera) {
        // The common case is nothing dropped — avoid the per-frame allocation.
        if ctx.input(|i| i.raw.dropped_files.is_empty()) {
            return;
        }
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
        });
        for path in dropped {
            match path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()) {
                Some(ext) if ext == "gdtf" => match self.library.import_gdtf(&path) {
                    Ok(idx) => {
                        let arc = self.library.gdtf[idx].clone();
                        let name = arc.name.clone();
                        let f = scene.add_gdtf(arc, Vec3::new(0.0, 4.0, 0.0));
                        self.selection = Selection::fixture(f);
                        self.notify.success(format!("Imported {name}"));
                    }
                    Err(e) => self.notify.error(format!("Import GDTF failed: {e}")),
                },
                Some(ext) if ext == "mvr" => match crate::mvr::MvrImport::load_path(&path) {
                    Ok(import) => {
                        let before = scene.fixtures.len();
                        scene.import_mvr(import);
                        if let Some((c, r)) = scene.scene_frame() {
                            camera.frame(c, r * 1.15);
                        }
                        self.selection = Selection::default();
                        self.notify
                            .success(format!("Imported MVR · {} fixtures", scene.fixtures.len() - before));
                    }
                    Err(e) => self.notify.error(format!("Import MVR failed: {e}")),
                },
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmx::DmxIo;
    use crate::scene::Scene;

    /// Extract one GDTF archive's bytes from the bundled Basic Festival MVR.
    fn gdtf_bytes(member: &str) -> Option<Vec<u8>> {
        let candidates = [
            format!("{}/.context/attachments/05W1Dh/Basic Festival.mvr", env!("CARGO_MANIFEST_DIR")),
            format!("{}/Downloads/Basic Festival/Basic Festival.mvr", std::env::var("HOME").unwrap_or_default()),
        ];
        for path in candidates {
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else { continue };
            let Ok(mut f) = zip.by_name(member) else { continue };
            let mut buf = Vec::new();
            if std::io::Read::read_to_end(&mut f, &mut buf).is_ok() {
                return Some(buf);
            }
        }
        None
    }

    /// Round-trip a real GDTF fixture through `.glow`: save bundles the archive
    /// bytes, open re-parses + re-links them, and per-fixture state (cells, beam,
    /// dimmer) plus the camera survive the trip.
    #[test]
    fn glow_save_load_relinks_gdtf_and_state() {
        let member = "Astera LED Technology@AX2-100 PixelBar.gdtf";
        let Some(bytes) = gdtf_bytes(member) else {
            eprintln!("skip glow_save_load: Basic Festival.mvr not found");
            return;
        };
        let mut g = crate::gdtf::GdtfFixture::load_bytes(&bytes).expect("parse gdtf");
        g.spec = member.to_string();
        g.raw = Some(Arc::new(bytes));

        let ui = Ui::new();
        let mut scene = Scene::default();
        let base = scene.fixtures.len();
        let idx = scene.add_gdtf(Arc::new(g), Vec3::new(1.0, 4.0, -2.0));
        scene.fixtures[idx].beam = 0.5;
        scene.fixtures[idx].optics.dimmer = 0.8;
        let cells = scene.fixtures[idx].cells.len();
        let mut camera = OrbitCamera::default();
        camera.distance = 21.0;
        let dmx = DmxIo::new();

        let path = std::env::temp_dir().join("glowstone-relink-test.glow");
        ui.write_project(&path, &scene, &camera, &dmx).expect("write project");

        // Open into a completely fresh app state.
        let project = project::read(&path).expect("read project");
        let _ = std::fs::remove_file(&path);
        let mut ui2 = Ui::new();
        let mut scene2 = Scene::default();
        let mut camera2 = OrbitCamera::default();
        let mut dmx2 = DmxIo::new();
        ui2.apply_project(project, &mut scene2, &mut camera2, &mut dmx2);

        assert_eq!(scene2.fixtures.len(), base + 1);
        let f = &scene2.fixtures[idx];
        let linked = f.gdtf.as_ref().expect("gdtf re-linked");
        assert_eq!(linked.spec, member);
        assert!(linked.raw.is_some(), "archive bytes restored for re-save");
        assert_eq!(f.cells.len(), cells, "per-cell colours preserved");
        assert!((f.beam - 0.5).abs() < 1e-6, "beam intensity round-trips");
        assert!((f.optics.dimmer - 0.8).abs() < 1e-6, "dimmer round-trips");
        assert!((camera2.distance - 21.0).abs() < 1e-6, "camera round-trips");
    }
}
