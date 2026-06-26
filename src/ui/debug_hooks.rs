use super::*;

impl Ui {
    /// Force the quick-select palette open (headless screenshot hook).
    pub fn debug_open_quick_select(&mut self) {
        self.quick_select = true;
    }

    /// Force the F3 operator-search palette open (headless screenshot hook).
    pub fn debug_open_op_search(&mut self) {
        self.op_search.show();
    }

    /// Open the `~` View pie at the screen centre (headless screenshot hook).
    pub fn debug_open_view_pie(&mut self) {
        self.view_pie.open_at(egui::Pos2::new(750.0, 475.0));
    }

    /// Open the `Z` Shading pie at the screen centre (headless screenshot hook).
    /// Wired into the PREVIZ_UI harness by the lead (in the off-limits app.rs);
    /// dead in the default build until then.
    #[allow(dead_code)]
    pub fn debug_open_shading_pie(&mut self) {
        self.shading_pie.open_at(egui::Pos2::new(750.0, 475.0));
    }

    /// Open the online Fixture Library window (headless hook). `demo` injects fake
    /// catalogue rows so the browse view renders without real credentials.
    pub fn debug_open_share(&mut self, demo: bool) {
        self.show_share = true;
        if demo {
            self.share.debug_demo();
        }
    }

    /// Select the first fixture and open the Replace dialog (headless hook).
    pub fn debug_open_replace(&mut self, scene: &Scene) {
        if !scene.fixtures.is_empty() {
            self.selection = Selection::fixture(0);
            self.replace = Some(ReplaceDialog::default());
        }
    }

    /// Open the profile editor for the first GDTF fixture (headless hook).
    pub fn debug_open_profile(&mut self, scene: &Scene) {
        if let Some(i) = scene.fixtures.iter().position(|f| f.is_gdtf()) {
            self.selection = Selection::fixture(i);
            self.profile = Some(ProfileEditor::new(i));
        }
    }

    /// Select the first GDTF fixture (headless hook for inspector screenshots).
    pub fn debug_select_first_gdtf(&mut self, scene: &Scene) {
        if let Some(i) = scene.fixtures.iter().position(|f| f.is_gdtf()) {
            self.selection = Selection::fixture(i);
        }
    }

    /// Select the World root so the Inspector shows the Render Properties.
    /// Headless hook (`PREVIZ_UI_WORLD`) for the render-inspector screenshot.
    pub fn debug_select_world(&mut self) {
        self.selection = Selection::world();
    }

    /// Select the first environment (fog volume). Headless hook (`PREVIZ_UI_ENV`).
    pub fn debug_select_environment(&mut self, scene: &Scene) {
        if !scene.environments.is_empty() {
            self.selection = Selection::environment(0);
        }
    }

    /// Multi-select up to `n` fixtures sharing the profile of the first fixture
    /// that has a wheel chain (so the bulk Wheels section is exercised); falls
    /// back to the first `n` GDTF fixtures. Headless hook for bulk screenshots.
    pub fn debug_select_n(&mut self, scene: &Scene, n: usize) {
        let with_wheels = scene.fixtures.iter().position(|f| {
            f.gdtf
                .as_ref()
                .and_then(|g| g.modes.get(f.mode_index))
                .is_some_and(|m| !m.components.is_empty())
        });
        let pick: Vec<usize> = match with_wheels {
            Some(seed) => {
                let prof = scene.fixtures[seed].profile.clone();
                scene
                    .fixtures
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.profile == prof)
                    .map(|(i, _)| i)
                    .take(n)
                    .collect()
            }
            None => scene.fixtures.iter().enumerate().filter(|(_, f)| f.is_gdtf()).map(|(i, _)| i).take(n).collect(),
        };
        if !pick.is_empty() {
            self.selection = Selection { fixtures: pick, geometry: Vec::new(), screens: Vec::new(), environment: None, world: false };
        }
    }
}
