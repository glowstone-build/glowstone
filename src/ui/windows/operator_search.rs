//! The F3 operator-search palette — a keyboard-driven command finder (Blender's
//! `F3` "search menu"). Lists every palette-runnable command from
//! [`shortcuts::palette_commands()`](crate::ui::shortcuts::palette_commands) by
//! label + category + its currently-bound shortcut, filtered by a live text field;
//! arrows move the highlight, Enter runs the highlighted command, Esc dismisses.
//! The actual dispatch happens at the call site
//! ([`Ui::run_palette_command`](crate::ui::Ui::run_palette_command)) — a pick only
//! returns the chosen command `id`. Catalog ops whose `poll` fails are shown greyed
//! and can't be picked (mirrors the menu's `add_enabled`); pure-action commands
//! (view / nav / selection) are always enabled.

use egui::{RichText, Sense};

use crate::ui::shortcuts::{self, Command};
use crate::ui::theme;

/// State of the operator-search palette. `open == false` = closed.
#[derive(Default)]
pub struct OperatorSearchState {
    pub open: bool,
    /// Live filter text (matched case-insensitively against the op label).
    pub search: String,
    /// Index of the highlighted row within the filtered list.
    pub idx: usize,
}

impl OperatorSearchState {
    /// Open the palette fresh (clear the previous filter + highlight).
    pub fn show(&mut self) {
        self.open = true;
        self.search.clear();
        self.idx = 0;
    }
}

/// Render the operator-search palette. `runnable(id)` reports whether each command's
/// `poll` passes right now (so it renders enabled / pickable). Returns the chosen
/// command `id` exactly once on Enter / click; `None` while open, on cancel, or
/// on Esc. The window auto-closes on a pick or cancel.
pub fn operator_search_window(
    ctx: &egui::Context,
    state: &mut OperatorSearchState,
    runnable: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    if !state.open {
        return None;
    }

    // Fuzzy-filter the palette commands by the live query (subsequence match; contiguous
    // runs rank higher — like Blender's search), so "mvx" finds "Move on X". An empty
    // query keeps registry order. `shortcuts::palette_commands()` is the whole registry
    // minus the viewport-/modal-only actions (Transform grab + axis lock).
    let q = state.search.trim();
    let mut scored: Vec<(i32, &'static Command)> = shortcuts::palette_commands()
        .into_iter()
        .filter_map(|c| crate::ui::lib_prefs::fuzzy_score(q, c.label).map(|s| (s, c)))
        .collect();
    scored.sort_by_key(|a| std::cmp::Reverse(a.0)); // best match first; stable sort preserves ties' order
    let list: Vec<&'static Command> = scored.into_iter().map(|(_, c)| c).collect();

    // --- keyboard navigation (read before the window so it works before focus
    // lands on a widget) ---
    let mut commit = false;
    ctx.input_mut(|i| {
        if i.key_pressed(egui::Key::Escape) {
            state.open = false;
        }
        // CONSUME the Enter so it both commits the pick AND can't leak to another
        // reader the same frame (bug 8).
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            commit = true;
        }
        if i.key_pressed(egui::Key::ArrowDown) {
            state.idx = state.idx.wrapping_add(1);
        }
        if i.key_pressed(egui::Key::ArrowUp) {
            state.idx = state.idx.wrapping_sub(1);
        }
    });
    if !state.open {
        return None;
    }
    // Clamp / wrap the highlight against the live list length.
    if list.is_empty() {
        state.idx = 0;
    } else {
        state.idx %= list.len();
    }

    let mut picked: Option<&'static str> = None; // chosen command id

    egui::Window::new("operator-search")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -80.0])
        .show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}  Run Operator", theme::icon::SEARCH)).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Esc").small().weak());
                });
            });
            let search = ui.add(
                egui::TextEdit::singleline(&mut state.search)
                    .hint_text("Search operators…")
                    .desired_width(f32::INFINITY),
            );
            // Focus the filter the frame the palette opens so typing narrows it
            // immediately; reset the highlight when the query changes.
            if search.changed() {
                state.idx = 0;
            }
            search.request_focus();
            ui.add_space(4.0);
            ui.separator();

            if list.is_empty() {
                ui.label(RichText::new("No matching operators").weak());
                return;
            }
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    for (i, c) in list.iter().enumerate() {
                        let enabled = runnable(c.id);
                        let resp = op_row(ui, c, i == state.idx, enabled);
                        if resp.clicked() && enabled {
                            picked = Some(c.id);
                        }
                    }
                });
        });

    // Enter runs the highlighted row (if it's runnable).
    if commit
        && let Some(c) = list.get(state.idx)
        && runnable(c.id)
    {
        picked = Some(c.id);
    }

    if picked.is_some() {
        state.open = false;
    }
    picked
}

/// One command row: label on the left; the currently-bound shortcut (if any) then
/// the category tag on the right. Highlighted when it's the keyboard cursor and
/// greyed when its `poll` fails.
fn op_row(ui: &mut egui::Ui, c: &Command, highlighted: bool, enabled: bool) -> egui::Response {
    let ink = theme::ink(!ui.visuals().dark_mode);
    let h = 26.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let resp = resp.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::NotAllowed
    });
    let painter = ui.painter_at(rect);
    if highlighted || (enabled && resp.hovered()) {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let fg = if enabled { ink.primary } else { ink.muted };
    painter.text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        c.label,
        egui::FontId::proportional(13.0),
        fg,
    );
    // Category on the far right; the bound shortcut (if any) sits just left of it.
    let cat_anchor = rect.right_center() - egui::vec2(8.0, 0.0);
    let cat = painter.text(
        cat_anchor,
        egui::Align2::RIGHT_CENTER,
        c.category.title(),
        egui::FontId::monospace(10.0),
        ink.tertiary,
    );
    if let Some(sc) = shortcuts::shortcut_for(c.id, &shortcuts::active()) {
        painter.text(
            egui::pos2(cat.min.x - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            sc,
            egui::FontId::monospace(11.0),
            ink.secondary,
        );
    }
    resp
}
