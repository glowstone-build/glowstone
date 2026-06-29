//! A reusable cursor-anchored radial PIE menu (Blender's `wmOperatorType` pie
//! menus, `interface/interface_region_menu_pie.cc`). egui ships no pie widget, so
//! this is our own: a ring of labelled + iconed sectors drawn over the whole UI,
//! anchored where the pie opened. You pick a sector either by CLICKING it, or by
//! dragging out from the centre in its direction and RELEASING (Blender's
//! "gesture" mode — fast muscle-memory picks). The dead zone at the centre and the
//! Esc key both CANCEL.
//!
//! Generic by design (seeds P5's pie infra): build one with [`Pie::new`] over any
//! list of [`PieItem`]s and call [`Pie::show`]; it returns `Some(index)` into that
//! list on a pick, `None` while still open, and closes itself (via `state.open`)
//! on pick / cancel. The caller owns a [`PieState`] across frames.

/// Map a pointer OFFSET from the pie centre (`dx`, `dy` in screen px — `dy` grows
/// downward) to the sector index it points at, for `n` sectors. Sector `i` is drawn
/// at direction `i/n·τ − π/2` (item 0 straight up, going clockwise), and its wedge
/// is CENTRED on that direction — so this offsets the raw angle by half a sector
/// before bucketing. Pulled out as a free fn so the angle math is unit-testable
/// without an egui context.
fn sector_at(dx: f32, dy: f32, n: usize) -> usize {
    let tau = std::f32::consts::TAU;
    let ang = dy.atan2(dx); // angle from +x, screen y down
    // Rotate so 0 == straight up, then add half a sector so each item's wedge is
    // centred on its drawn direction; normalise to [0, τ) and bucket.
    let from_top = ang + std::f32::consts::FRAC_PI_2 + tau / (2.0 * n as f32);
    let norm = from_top.rem_euclid(tau);
    (norm / tau * n as f32).floor() as usize % n
}

/// One labelled sector. `icon` is an optional Phosphor glyph drawn inline before
/// the label (see [`crate::ui::theme::icon`]); pass `""` for none.
pub struct PieItem {
    pub icon: &'static str,
    pub label: String,
}

impl PieItem {
    pub fn new(icon: &'static str, label: impl Into<String>) -> Self {
        Self {
            icon,
            label: label.into(),
        }
    }
}

/// Cross-frame state for an open pie. Held by the caller (one live at a time is
/// the usual case). Default is closed.
#[derive(Clone, Default)]
pub struct PieState {
    pub open: bool,
    /// Screen-space point the pie is anchored to (its centre).
    pub center: egui::Pos2,
}

impl PieState {
    /// Open the pie centred on `at` (typically the cursor when the key was hit).
    pub fn open_at(&mut self, at: egui::Pos2) {
        self.open = true;
        self.center = at;
    }
}

/// A pie menu about to be drawn. Borrows nothing but the item list it's built from.
pub struct Pie<'a> {
    items: &'a [PieItem],
    accent: egui::Color32,
}

impl<'a> Pie<'a> {
    pub fn new(items: &'a [PieItem]) -> Self {
        Self {
            items,
            accent: egui::Color32::from_rgb(90, 170, 255),
        }
    }

    /// Tint the highlighted-sector wedge + ring (defaults to a cyan-blue).
    pub fn accent(mut self, accent: egui::Color32) -> Self {
        self.accent = accent;
        self
    }

    /// Geometry constants (logical px). The ring radius is where the labels sit;
    /// the dead zone is the cancel disc at the centre.
    const RING: f32 = 96.0;
    const DEAD_ZONE: f32 = 26.0;

    /// Draw the pie if `state.open`. Returns the chosen index on a pick (and clears
    /// `state.open`); `None` while open or after a cancel (which also clears it).
    pub fn show(self, ctx: &egui::Context, state: &mut PieState) -> Option<usize> {
        if !state.open || self.items.is_empty() {
            return None;
        }

        // Esc cancels outright (read before drawing so it wins over any hover).
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            state.open = false;
            return None;
        }

        let center = state.center;
        let n = self.items.len();
        let mut chosen: Option<usize> = None;

        // Full-screen overlay layer so the pie floats above the dock and eats the
        // pointer. We paint manually rather than allocating widgets — sectors are
        // hit-tested by the pointer angle, the Blender way.
        let area = egui::Area::new(egui::Id::new("view-pie"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .interactable(true);

        area.show(ctx, |ui| {
            #[allow(deprecated)] // egui 0.34 screen_rect — content_rect migration later
            let screen = ui.ctx().screen_rect();
            // Claim the whole screen so clicks anywhere land on the pie (outside a
            // sector = cancel).
            let resp = ui.allocate_rect(screen, egui::Sense::click());
            let painter = ui.painter();

            // Dim backdrop.
            painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(60));

            let pointer = ui
                .ctx()
                .input(|i| i.pointer.interact_pos())
                .unwrap_or(center);
            let offset = pointer - center;
            let dist = offset.length();
            let in_dead_zone = dist < Self::DEAD_ZONE;

            // Which sector is the pointer pointing at? Sectors are centred on evenly
            // spaced directions starting at straight up (−Y) and going clockwise, so
            // the layout matches a clock face the user can predict.
            let highlight = if in_dead_zone {
                None
            } else {
                Some(sector_at(offset.x, offset.y, n))
            };

            // Paint each sector's label/icon around the ring, highlighting the hovered.
            for (i, item) in self.items.iter().enumerate() {
                let dir_ang =
                    (i as f32 / n as f32) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                let dir = egui::vec2(dir_ang.cos(), dir_ang.sin());
                let pos = center + dir * Self::RING;
                let active = highlight == Some(i);

                // Pill behind the label so it reads over the 3D viewport. Icon and
                // label sit on ONE line ("⊞ Beauty"), separated by a couple of
                // spaces — `layout_no_wrap` keeps it single-line, so the pill sizes
                // itself to that line's width/height (shorter + wider than stacked).
                let text = if item.icon.is_empty() {
                    item.label.clone()
                } else {
                    format!("{}  {}", item.icon, item.label)
                };
                let galley = painter.layout_no_wrap(
                    text,
                    egui::FontId::proportional(13.0),
                    if active {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_gray(220)
                    },
                );
                let pad = egui::vec2(10.0, 6.0);
                let pill = egui::Rect::from_center_size(pos, galley.size() + pad * 2.0);
                let (bg, stroke) = if active {
                    (self.accent, egui::Stroke::new(1.5, egui::Color32::WHITE))
                } else {
                    (egui::Color32::from_rgb(38, 40, 46), egui::Stroke::NONE)
                };
                painter.rect_filled(pill, 6.0, bg);
                if stroke.width > 0.0 {
                    painter.rect_stroke(pill, 6.0, stroke, egui::StrokeKind::Inside);
                }
                painter.galley(pill.min + pad, galley, egui::Color32::WHITE);
            }

            // Centre dot (the cancel target).
            let dz_col = if in_dead_zone {
                egui::Color32::from_gray(150)
            } else {
                egui::Color32::from_gray(90)
            };
            painter.circle_filled(center, 4.0, dz_col);
            // Line from centre toward the pointer (the gesture cue), unless dead.
            if !in_dead_zone {
                let edge = center + offset.normalized() * Self::DEAD_ZONE;
                painter.line_segment(
                    [edge, pointer],
                    egui::Stroke::new(1.5, self.accent.gamma_multiply(0.7)),
                );
            }

            // Resolve a pick: a click anywhere, OR releasing a drag that started on
            // the pie. Both reduce to "the pointer was released" — Blender opens the
            // pie on key-press and lets a press-drag-release gesture choose, while a
            // plain click also works. Either way: a release inside a sector picks it,
            // a release in the dead zone (or no sector) cancels.
            let released = ui.ctx().input(|i| i.pointer.any_released());
            if resp.clicked() || released {
                if let Some(i) = highlight {
                    chosen = Some(i);
                }
                state.open = false;
            }
        });

        chosen
    }
}

/// One labelled choice for [`choose`]: a Phosphor `icon` (or `""`), a `label`, and
/// the `value` returned when the sector is picked. The natural shape for an
/// enum/toggle pie — see the `~` View pie and the `Z` Shading pie.
pub struct Choice<T> {
    pub icon: &'static str,
    pub label: String,
    pub value: T,
}

impl<T> Choice<T> {
    pub fn new(icon: &'static str, label: impl Into<String>, value: T) -> Self {
        Self {
            icon,
            label: label.into(),
            value,
        }
    }
}

/// Build + show a pie over a list of [`Choice<T>`] and return the chosen `value`
/// (consumed by index) on a pick, `None` while open / on cancel. This is the
/// ergonomic wrapper over [`Pie::new`] + [`Pie::show`] for the common
/// "pick one of N values" case: the caller hands over labelled values and gets
/// the value back, with no parallel-array bookkeeping. `accent` tints the
/// highlighted wedge. The `choices` `Vec` is consumed so the matched value can be
/// moved out (no `Clone` bound on `T`).
pub fn choose<T>(
    ctx: &egui::Context,
    state: &mut PieState,
    accent: egui::Color32,
    mut choices: Vec<Choice<T>>,
) -> Option<T> {
    let items: Vec<PieItem> = choices
        .iter()
        .map(|c| PieItem::new(c.icon, c.label.clone()))
        .collect();
    Pie::new(&items)
        .accent(accent)
        .show(ctx, state)
        .map(|i| choices.swap_remove(i).value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With 8 sectors, the four cardinal directions land on items 0/2/4/6 (up,
    /// right, down, left) — the layout the View pie relies on. `dy` grows downward.
    #[test]
    fn sector_at_maps_cardinals() {
        let n = 8;
        assert_eq!(sector_at(0.0, -100.0, n), 0); // straight up   → Top
        assert_eq!(sector_at(100.0, 0.0, n), 2); // right          → Right
        assert_eq!(sector_at(0.0, 100.0, n), 4); // straight down  → Bottom
        assert_eq!(sector_at(-100.0, 0.0, n), 6); // left          → Left
    }

    /// A pointer slightly off a sector centre still resolves to that sector (wedges
    /// are centred on their item direction, so small wobble doesn't cross a bound).
    #[test]
    fn sector_at_tolerates_wobble_within_wedge() {
        let n = 8;
        // Nearly straight up, nudged a few px sideways either way → still Top.
        assert_eq!(sector_at(8.0, -100.0, n), 0);
        assert_eq!(sector_at(-8.0, -100.0, n), 0);
    }

    /// The diagonal between Top (0) and Right (2) lands on the in-between item 1.
    #[test]
    fn sector_at_diagonal_picks_between() {
        let n = 8;
        assert_eq!(sector_at(70.0, -70.0, n), 1); // up-right → item 1
    }

    /// Every angle around the circle maps to a valid in-range sector index.
    #[test]
    fn sector_at_always_in_range() {
        let n = 6;
        for deg in 0..360 {
            let a = (deg as f32).to_radians();
            let s = sector_at(a.cos(), a.sin(), n);
            assert!(s < n, "deg {deg} -> {s}");
        }
    }
}
