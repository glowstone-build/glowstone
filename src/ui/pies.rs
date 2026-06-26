use super::*;

/// One effect the `Z` Shading pie can apply: switch the viewport display
/// [`ViewportMode`], or flip one of the quick overlay toggles. Returned by
/// [`shading_pie_choices`] and applied by `Ui::apply_shading_choice`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShadingChoice {
    Mode(ViewportMode),
    ToggleGrid,
    ToggleStats,
}

/// The `Z` Shading-pie sectors, clockwise from straight up. The three display
/// modes sit at the cardinal-ish spokes (Beauty up, then Unlit / Wireframe); the
/// Grid + Stats toggles round out the ring. Returned as labelled [`pie::Choice`]
/// values so `pie::choose` maps sector→effect with no parallel-array bookkeeping.
/// A free fn (no `&self`) so the sector→effect layout is unit-testable.
fn shading_pie_choices() -> Vec<pie::Choice<ShadingChoice>> {
    use theme::icon;
    vec![
        pie::Choice::new(icon::VIEWPORT, "Beauty", ShadingChoice::Mode(ViewportMode::Beauty)),
        pie::Choice::new(icon::PATCH, "Grid", ShadingChoice::ToggleGrid),
        pie::Choice::new(icon::FIXTURE, "Unlit", ShadingChoice::Mode(ViewportMode::Unlit)),
        pie::Choice::new(icon::PERF, "Stats", ShadingChoice::ToggleStats),
        pie::Choice::new(icon::GEOMETRY, "Wireframe", ShadingChoice::Mode(ViewportMode::Wireframe)),
    ]
}

impl Ui {
    /// Draw + resolve the `~` View pie. Sectors are laid out clock-face from the
    /// top so the cardinal axis views land where the user expects (Top up, Bottom
    /// down, Left/Right to the sides); the diagonals carry Front/Back, the
    /// Persp/Ortho toggle, and Frame Selected. A pick applies to the camera.
    pub(super) fn view_pie(&mut self, ctx: &egui::Context, camera: &mut OrbitCamera, scene: &Scene) {
        use theme::icon;

        // One sector per entry; index == sector. Order = clockwise from straight up.
        #[derive(Clone, Copy)]
        enum Choice {
            View(CameraView),
            ToggleOrtho,
            FrameSelected,
        }
        let items = [
            (icon::VIEWPORT, "Top", Choice::View(CameraView::Top)),
            (icon::VIEWPORT, "Right", Choice::View(CameraView::Right)),
            (icon::CAMERA, "Persp/Ortho", Choice::ToggleOrtho),
            (icon::VIEWPORT, "Back", Choice::View(CameraView::Back)),
            (icon::VIEWPORT, "Bottom", Choice::View(CameraView::Bottom)),
            (icon::FRAME, "Frame Sel.", Choice::FrameSelected),
            (icon::VIEWPORT, "Front", Choice::View(CameraView::Front)),
            (icon::VIEWPORT, "Left", Choice::View(CameraView::Left)),
        ];
        let sectors: Vec<pie::PieItem> =
            items.iter().map(|(ic, lbl, _)| pie::PieItem::new(ic, *lbl)).collect();
        let accent = theme::accent(&self.prefs);
        if let Some(i) = pie::Pie::new(&sectors).accent(accent).show(ctx, &mut self.view_pie) {
            match items[i].2 {
                Choice::View(v) => camera.set_view(v),
                Choice::ToggleOrtho => camera.toggle_ortho(),
                Choice::FrameSelected => {
                    if let Some((lo, hi)) = self.frame_bounds(scene, true) {
                        camera.frame_aabb(lo, hi);
                    }
                }
            }
        }
    }

    /// Draw + resolve the `Z` Shading pie (Blender's Z pie). Sectors pick the
    /// viewport display Mode (Beauty / Unlit / Wireframe) plus two quick overlay
    /// toggles (Grid, Stats); a pick applies immediately to `settings` / `prefs`.
    /// Built via the generic [`pie::choose`] helper so the enum/toggle mapping is a
    /// one-liner over labelled values.
    pub(super) fn shading_pie(&mut self, ctx: &egui::Context) {
        let accent = theme::accent(&self.prefs);
        if let Some(choice) = pie::choose(ctx, &mut self.shading_pie, accent, shading_pie_choices()) {
            self.apply_shading_choice(choice);
        }
    }

    /// Apply one resolved [`ShadingChoice`] from the Z pie. Split out (and given a
    /// pure `&mut self`) so the sector→effect mapping is unit-testable without an
    /// egui context.
    fn apply_shading_choice(&mut self, choice: ShadingChoice) {
        match choice {
            ShadingChoice::Mode(m) => self.settings.mode = m,
            ShadingChoice::ToggleGrid => self.settings.show_grid = !self.settings.show_grid,
            ShadingChoice::ToggleStats => self.prefs.show_stats = !self.prefs.show_stats,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Z Shading pie lays out exactly the three display modes + the Grid /
    /// Stats toggles, and each sector's value applies the effect it advertises.
    #[test]
    fn shading_pie_sectors_map_to_modes_and_toggles() {
        let choices = shading_pie_choices();
        // Every display mode is offered exactly once, in ViewportMode::ALL order.
        let modes: Vec<ViewportMode> = choices
            .iter()
            .filter_map(|c| match c.value {
                ShadingChoice::Mode(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(modes, ViewportMode::ALL.to_vec(), "all three modes, once each");
        // Both quick toggles are present.
        assert!(choices.iter().any(|c| c.value == ShadingChoice::ToggleGrid), "Grid toggle present");
        assert!(choices.iter().any(|c| c.value == ShadingChoice::ToggleStats), "Stats toggle present");
        // No sector is unlabelled.
        assert!(choices.iter().all(|c| !c.label.is_empty()), "every sector is labelled");

        // Applying a Mode choice sets settings.mode; the toggles flip their flag.
        let mut ui = Ui::new();
        ui.settings.mode = ViewportMode::Beauty;
        ui.apply_shading_choice(ShadingChoice::Mode(ViewportMode::Wireframe));
        assert_eq!(ui.settings.mode, ViewportMode::Wireframe, "Mode sector switches display mode");

        let grid0 = ui.settings.show_grid;
        ui.apply_shading_choice(ShadingChoice::ToggleGrid);
        assert_ne!(ui.settings.show_grid, grid0, "Grid sector flips the grid flag");

        let stats0 = ui.prefs.show_stats;
        ui.apply_shading_choice(ShadingChoice::ToggleStats);
        assert_ne!(ui.prefs.show_stats, stats0, "Stats sector flips the stats overlay flag");
    }

    /// The generic `pie::choose` helper returns the picked sector's VALUE (not its
    /// index), moved out by index — the seam the Z pie relies on. (Drives the
    /// mapping logic without an egui frame by checking the choice list is non-empty
    /// and each value is what the layout fn declared; the angle→index resolution is
    /// covered by pie.rs's `sector_at` tests.)
    #[test]
    fn shading_pie_choices_are_stable_order() {
        let choices = shading_pie_choices();
        assert_eq!(choices.len(), 5, "3 modes + Grid + Stats");
        assert_eq!(choices[0].value, ShadingChoice::Mode(ViewportMode::Beauty));
    }
}
