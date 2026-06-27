//! Cues — saved fixture "looks" with crossfade playback (depence's Scenes /
//! Repository, a console's cue list).
//!
//! A cue snapshots every fixture's controllable look (pan / tilt / colour /
//! intensity / beam / optics). Recalling a cue crossfades the *continuous* fields
//! (pan, tilt, colour, intensity, beam) over the cue's fade time; the discrete
//! optics (gobo / prism / wheel slot) snap at the start, since wheel slots don't
//! interpolate. The fade is ticked once per real frame from `app::render`.
//!
//! Note: this is the offline-glowstone look engine. The cue tick runs each frame
//! *after* live-DMX decode (see `app::render`), so while a fade is in progress the
//! cue WINS over a connected console — the recalled look is what you see. Outside
//! a fade nothing is written, so live DMX drives the rig normally. Cues are meant
//! for building and showing looks without a console; don't run both at once.

use egui::{Grid, RichText};

use crate::optics::OpticalControls;
use crate::scene::{Fixture, Scene};

use super::theme;

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Interpolate an angle in degrees along the SHORTEST path, so a pan/tilt fade
/// from +170° to −170° crosses 0 (20°) instead of spinning the long way (340°).
#[inline]
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    let mut d = (b - a) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    a + d * t
}

/// The controllable look of one fixture, captured into / restored from a cue.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FixtureLook {
    pub pan: f32,
    pub tilt: f32,
    pub color: [f32; 3],
    pub intensity: f32,
    pub beam_angle: f32,
    pub optics: OpticalControls,
}

impl FixtureLook {
    fn capture(f: &Fixture) -> Self {
        Self {
            pan: f.pan,
            tilt: f.tilt,
            color: f.color,
            intensity: f.intensity,
            beam_angle: f.beam_angle,
            optics: f.optics.clone(),
        }
    }

    /// Write `lerp(self → to, a)` of the continuous fields into the fixture.
    fn apply_lerp(&self, to: &FixtureLook, a: f32, f: &mut Fixture) {
        f.pan = lerp_angle(self.pan, to.pan, a);
        f.tilt = lerp_angle(self.tilt, to.tilt, a);
        f.intensity = lerp(self.intensity, to.intensity, a);
        // The dimmer is the fixture's level (continuous), so it crossfades — the
        // rest of the optics (wheel slots / prism) are discrete and snapped.
        f.optics.dimmer = lerp(self.optics.dimmer, to.optics.dimmer, a);
        f.beam_angle = lerp(self.beam_angle, to.beam_angle, a);
        for c in 0..3 {
            f.color[c] = lerp(self.color[c], to.color[c], a);
        }
        // The cue fade IS the head movement: pin the slewed follower to the faded
        // target (and kill velocity) so the motor slew doesn't compound a second
        // ease on top of the cue's fade time.
        f.pan_actual = f.pan;
        f.tilt_actual = f.tilt;
        f.pan_vel = 0.0;
        f.tilt_vel = 0.0;
    }
}

/// A saved look: one [`FixtureLook`] per fixture (aligned to fixture index at
/// capture time), plus a fade time.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cue {
    pub name: String,
    pub looks: Vec<FixtureLook>,
    pub fade: f32,
}

/// An in-progress crossfade toward a target cue.
struct Fade {
    from: Vec<FixtureLook>,
    to: usize,
    t: f32,
    dur: f32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct CueEngine {
    pub cues: Vec<Cue>,
    /// The last cue fully reached (for Go / highlight).
    pub current: Option<usize>,
    /// An in-progress fade is transient and need not persist; restored as `None`.
    #[serde(skip)]
    fade: Option<Fade>,
    name_buf: String,
    fade_buf: f32,
}

impl Default for CueEngine {
    fn default() -> Self {
        Self { cues: Vec::new(), current: None, fade: None, name_buf: String::new(), fade_buf: 2.0 }
    }
}

impl CueEngine {
    /// Capture the whole rig's current look as a new cue.
    fn record(&mut self, scene: &Scene) {
        let looks: Vec<FixtureLook> = scene.fixtures.iter().map(FixtureLook::capture).collect();
        let name = if self.name_buf.trim().is_empty() {
            format!("Cue {}", self.cues.len() + 1)
        } else {
            self.name_buf.trim().to_string()
        };
        self.cues.push(Cue { name, looks, fade: self.fade_buf.max(0.0) });
        self.name_buf.clear();
    }

    /// Start recalling cue `idx`: snap the discrete optics now, crossfade the rest.
    fn recall(&mut self, idx: usize, scene: &mut Scene) {
        let Some(cue) = self.cues.get(idx) else { return };
        // Capture the CURRENT look first — the crossfade's starting point — BEFORE
        // we snap the discrete optics, so the dimmer fades from where it actually
        // is (snapping would otherwise overwrite the starting dimmer).
        let from: Vec<FixtureLook> = scene.fixtures.iter().map(FixtureLook::capture).collect();
        // Snap the discrete optics immediately (wheel slots don't interpolate); the
        // continuous dimmer is re-faded by `apply_lerp` below / in `tick`.
        for (i, f) in scene.fixtures.iter_mut().enumerate() {
            if let Some(look) = cue.looks.get(i) {
                f.optics = look.optics.clone();
            }
        }
        if cue.fade <= 0.0 {
            // Instant: apply the final look and we're done.
            for (i, f) in scene.fixtures.iter_mut().enumerate() {
                if let Some(look) = cue.looks.get(i) {
                    look.apply_lerp(look, 1.0, f);
                }
            }
            self.current = Some(idx);
            self.fade = None;
            return;
        }
        // Seed the crossfade's first frame (t=0) NOW so the click frame already
        // shows the `from` look — the snap above set the dimmer to the *target*,
        // and the fade tick only runs next frame, so without this the rig flashes
        // to the target dimmer for one frame before fading.
        for (i, f) in scene.fixtures.iter_mut().enumerate() {
            if let (Some(a), Some(b)) = (from.get(i), cue.looks.get(i)) {
                a.apply_lerp(b, 0.0, f);
            }
        }
        self.fade = Some(Fade { from, to: idx, t: 0.0, dur: cue.fade });
    }

    /// Advance to the next cue in the list (Go). Stops at the last cue.
    fn go(&mut self, scene: &mut Scene) {
        let next = match self.current {
            Some(c) => c + 1,
            None => 0,
        };
        if next < self.cues.len() {
            self.recall(next, scene);
        }
    }

    fn prev(&mut self, scene: &mut Scene) {
        // Symmetric with go(): only steps when there's a current cue to step back
        // from (Prev with no active cue does nothing, rather than jumping to 0).
        if let Some(c) = self.current
            && c > 0
        {
            self.recall(c - 1, scene);
        }
    }

    /// A fixture was deleted from the scene at index `i` — drop its slot from every
    /// cue's look list so the lists stay aligned to `scene.fixtures`, and abort any
    /// running fade (its snapshot is now misaligned). Call in descending index
    /// order, in lock-step with `scene.fixtures.remove(i)`.
    pub fn remove_fixture(&mut self, i: usize) {
        for cue in &mut self.cues {
            if i < cue.looks.len() {
                cue.looks.remove(i);
            }
        }
        self.fade = None;
    }

    /// A cue was deleted at `removed` — keep `current` and any running fade pointing
    /// at the right cue (or clear them).
    fn on_cue_removed(&mut self, removed: usize) {
        let fix = |idx: usize| -> Option<usize> {
            if idx == removed {
                None
            } else if idx > removed {
                Some(idx - 1)
            } else {
                Some(idx)
            }
        };
        self.current = self.current.and_then(fix);
        // If the in-flight fade targeted the removed cue (or shifts), stop it.
        match self.fade.as_mut() {
            Some(f) => match fix(f.to) {
                Some(t) => f.to = t,
                None => self.fade = None,
            },
            None => {}
        }
    }

    /// Tick an in-progress crossfade. Call once per real frame with `dt` seconds.
    pub fn tick(&mut self, scene: &mut Scene, dt: f32) {
        let Some(fade) = self.fade.as_mut() else { return };
        fade.t += dt;
        let a = (fade.t / fade.dur.max(1e-3)).clamp(0.0, 1.0);
        if let Some(cue) = self.cues.get(fade.to) {
            for (i, f) in scene.fixtures.iter_mut().enumerate() {
                if let (Some(from), Some(to)) = (fade.from.get(i), cue.looks.get(i)) {
                    from.apply_lerp(to, a, f);
                }
            }
        }
        if a >= 1.0 {
            self.current = Some(fade.to);
            self.fade = None;
        }
    }

    fn fading_progress(&self) -> Option<(usize, f32)> {
        self.fade.as_ref().map(|f| (f.to, (f.t / f.dur.max(1e-3)).clamp(0.0, 1.0)))
    }
}

/// The Cues panel: record the current look, fire cues, run the list with Go.
pub fn cue_panel(ui: &mut egui::Ui, engine: &mut CueEngine, scene: &mut Scene) {
    let accent = ui.visuals().selection.stroke.color;

    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{}", engine.cues.len())).small().weak());
        });
    });
    ui.separator();

    // --- Record a new cue from the current rig look ---
    ui.horizontal(|ui| {
        let hint = format!("Cue {}", engine.cues.len() + 1);
        ui.add(egui::TextEdit::singleline(&mut engine.name_buf).desired_width(120.0).hint_text(&hint));
        ui.add(
            egui::DragValue::new(&mut engine.fade_buf)
                .range(0.0..=60.0)
                .speed(0.1)
                .suffix(" s"),
        )
        .on_hover_text("Fade time for the new cue");
        if ui
            .add_enabled(!scene.fixtures.is_empty(), egui::Button::new(format!("{}  Record", theme::icon::ADD)))
            .on_hover_text("Capture the current look of every fixture as a cue")
            .clicked()
        {
            engine.record(scene);
        }
    });

    // --- Transport ---
    ui.horizontal(|ui| {
        if ui.add_enabled(!engine.cues.is_empty(), egui::Button::new(format!("{}  Prev", theme::icon::PREV))).clicked() {
            engine.prev(scene);
        }
        let can_go = match engine.current {
            Some(c) => c + 1 < engine.cues.len(),
            None => !engine.cues.is_empty(),
        };
        if ui
            .add_enabled(can_go, egui::Button::new(RichText::new(format!("Go  {}", theme::icon::NEXT)).strong().color(accent)))
            .on_hover_text("Fire the next cue in the list")
            .clicked()
        {
            engine.go(scene);
        }
        match engine.current.and_then(|c| engine.cues.get(c)) {
            Some(cue) => ui.label(RichText::new(format!("{}  {}", theme::icon::PLAY, cue.name)).small().color(accent)),
            None => ui.label(RichText::new("—").small().weak()),
        };
    });
    if let Some((to, p)) = engine.fading_progress() {
        let name = engine.cues.get(to).map(|c| c.name.as_str()).unwrap_or("");
        ui.add(egui::ProgressBar::new(p).desired_height(6.0).text(RichText::new(format!("{}  {name}", theme::icon::ARROW_RIGHT)).small()));
    }
    ui.separator();

    if engine.cues.is_empty() {
        ui.label(RichText::new("none — set a look (Inspector / DMX), then Record").weak().small());
        return;
    }

    // --- The cue list ---
    let mut recall: Option<usize> = None;
    let mut remove: Option<usize> = None;
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        Grid::new("cue-list").num_columns(4).spacing([10.0, 6.0]).striped(true).show(ui, |ui| {
            for i in 0..engine.cues.len() {
                let is_current = engine.current == Some(i);
                let label = format!("{}.  {}", i + 1, engine.cues[i].name);
                if ui
                    .selectable_label(is_current, RichText::new(label).color(if is_current { accent } else { ui.visuals().text_color() }))
                    .on_hover_text("Recall this cue")
                    .clicked()
                {
                    recall = Some(i);
                }
                ui.label(RichText::new(format!("{} fx", engine.cues[i].looks.len())).small().weak());
                ui.add(
                    egui::DragValue::new(&mut engine.cues[i].fade)
                        .range(0.0..=60.0)
                        .speed(0.1)
                        .suffix(" s"),
                )
                .on_hover_text("Fade time");
                if ui.small_button(theme::icon::TRASH).on_hover_text("Delete cue").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    });

    if let Some(i) = recall {
        engine.recall(i, scene);
    }
    if let Some(i) = remove {
        engine.cues.remove(i);
        // Keep `current` + any running fade pointing at the right cue (or clear).
        engine.on_cue_removed(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_angle_takes_shortest_path() {
        // +170 -> -170 is 20 deg the short way (through 180), not 340 the long way.
        assert!((lerp_angle(170.0, -170.0, 0.5) - 180.0).abs() < 1e-3);
        assert!((lerp_angle(170.0, -170.0, 1.0) - 190.0).abs() < 1e-3); // 190 ≡ -170
        // Plain interpolation within a half-turn.
        assert!((lerp_angle(0.0, 90.0, 0.5) - 45.0).abs() < 1e-3);
    }

    fn cue(n: usize, looks: usize) -> Cue {
        Cue {
            name: format!("Cue {n}"),
            looks: (0..looks)
                .map(|_| FixtureLook { pan: 0.0, tilt: 0.0, color: [0.0; 3], intensity: 0.0, beam_angle: 0.0, optics: OpticalControls::default() })
                .collect(),
            fade: 1.0,
        }
    }

    #[test]
    fn remove_fixture_shrinks_every_cue_look_list() {
        let mut e = CueEngine { cues: vec![cue(1, 3), cue(2, 3)], ..Default::default() };
        e.remove_fixture(1);
        assert_eq!(e.cues[0].looks.len(), 2);
        assert_eq!(e.cues[1].looks.len(), 2);
    }

    #[test]
    fn on_cue_removed_fixes_current_index() {
        let mut e = CueEngine { cues: vec![cue(1, 0), cue(2, 0), cue(3, 0)], current: Some(2), ..Default::default() };
        e.on_cue_removed(1); // delete the middle cue
        assert_eq!(e.current, Some(1)); // index 2 shifted down to 1
        e.on_cue_removed(1); // delete the cue current now points at
        assert_eq!(e.current, None);
    }
}
