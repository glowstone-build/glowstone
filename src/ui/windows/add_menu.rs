//! The viewport **Add menu** (Shift+A) — a cursor-anchored popup, modelled on
//! Blender's `Add` menu, that drops a new entity into the scene at the place the
//! mouse was when it was summoned. It is split into three categories — Fixtures,
//! Screens, Environment — each backed by the project [`Library`]. The left column
//! picks a category; the right column lists that category's (filterable) entries.
//!
//! The menu is fully keyboard-drivable so it works whether opened by Shift+A or
//! by a click: Up/Down move the highlight, Left/Right switch category, Enter
//! commits the highlighted entry, Esc dismisses. It returns the chosen
//! [`AddAction`] (decoupled from the scene mutation, which the caller performs so
//! the new object can also be selected).

use crate::scene::library::Library;
use crate::ui::theme;

/// Which group of addable things the menu is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddCategory {
    Fixtures,
    Screens,
    Environment,
}

impl AddCategory {
    /// Stable left-column order.
    const ORDER: [AddCategory; 3] =
        [AddCategory::Fixtures, AddCategory::Screens, AddCategory::Environment];

    fn label(self) -> &'static str {
        match self {
            AddCategory::Fixtures => "Fixtures",
            AddCategory::Screens => "Screens",
            AddCategory::Environment => "Environment",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            AddCategory::Fixtures => theme::icon::FIXTURE,
            AddCategory::Screens => theme::icon::SCREEN,
            AddCategory::Environment => theme::icon::ENVIRONMENT,
        }
    }
}

/// The entity the user chose to add. Indices point into the corresponding
/// [`Library`] vectors (`AddGdtf` into `library.gdtf`).
pub enum AddAction {
    /// A built-in fixture profile (`library.fixtures[i]`).
    Fixture(usize),
    /// An imported GDTF definition (`library.gdtf[i]`).
    Gdtf(usize),
    /// An LED-wall component (`library.screens[i]`).
    Screen(usize),
    /// An environment volume (`library.environments[i]`).
    Environment(usize),
}

/// Transient state for an open Add menu. `anchor` is the cursor position captured
/// when the menu was summoned, so it pops up where the mouse is.
pub struct AddMenuState {
    pub open: bool,
    pub anchor: egui::Pos2,
    pub category: AddCategory,
    /// Highlighted row within the *current category's filtered list*.
    pub idx: usize,
    pub search: String,
}

impl Default for AddMenuState {
    fn default() -> Self {
        Self {
            open: false,
            anchor: egui::Pos2::ZERO,
            category: AddCategory::Fixtures,
            idx: 0,
            search: String::new(),
        }
    }
}

impl AddMenuState {
    /// Open the menu at `anchor`, resetting the highlight + filter.
    pub fn show_at(&mut self, anchor: egui::Pos2) {
        self.open = true;
        self.anchor = anchor;
        self.idx = 0;
        self.search.clear();
    }
}

/// One flattened, filtered, displayable entry: its label + the action it commits.
struct Entry {
    icon: &'static str,
    label: String,
    action: AddAction,
}

/// Build the (filtered) entry list for the active category. GDTF fixtures are
/// listed first (most specific), then the built-in profiles.
fn entries(library: &Library, state: &AddMenuState) -> Vec<Entry> {
    let q = state.search.trim().to_lowercase();
    let matches = |hay: &str| q.is_empty() || hay.to_lowercase().contains(&q);
    let mut out = Vec::new();
    match state.category {
        AddCategory::Fixtures => {
            for (gi, g) in library.gdtf.iter().enumerate() {
                let hay = format!("{} {}", g.manufacturer, g.name);
                if matches(&hay) {
                    out.push(Entry {
                        icon: theme::icon::FIXTURE,
                        label: format!("{} · {}", g.manufacturer, g.name),
                        action: AddAction::Gdtf(gi),
                    });
                }
            }
            for (pi, p) in library.fixtures.iter().enumerate() {
                let hay = format!("{} {}", p.category, p.name);
                if matches(&hay) {
                    let icon = if p.laser { theme::icon::COLOR } else { theme::icon::FIXTURE };
                    out.push(Entry {
                        icon,
                        label: format!("{} · {}", p.category, p.name),
                        action: AddAction::Fixture(pi),
                    });
                }
            }
        }
        AddCategory::Screens => {
            for (si, s) in library.screens.iter().enumerate() {
                let hay = format!("{} {}", s.category, s.name);
                if matches(&hay) {
                    out.push(Entry {
                        icon: theme::icon::SCREEN,
                        label: format!("{} · {}", s.category, s.name),
                        action: AddAction::Screen(si),
                    });
                }
            }
        }
        AddCategory::Environment => {
            for (ei, e) in library.environments.iter().enumerate() {
                let hay = format!("{} {}", e.category, e.name);
                if matches(&hay) {
                    out.push(Entry {
                        icon: theme::icon::ENVIRONMENT,
                        label: format!("{} · {}", e.category, e.name),
                        action: AddAction::Environment(ei),
                    });
                }
            }
        }
    }
    out
}

/// Draw the Add menu if open. Returns the chosen action (and auto-closes) on a
/// pick; returns `None` otherwise. Esc / clicking outside dismisses it.
pub fn add_menu_window(
    ctx: &egui::Context,
    library: &Library,
    state: &mut AddMenuState,
) -> Option<AddAction> {
    if !state.open {
        return None;
    }

    // --- keyboard navigation (read before the window so it works even if focus
    // hasn't landed on a widget yet) ---
    let mut commit = false;
    let mut next_cat: Option<AddCategory> = None;
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            state.open = false;
        }
        if i.key_pressed(egui::Key::Enter) {
            commit = true;
        }
        if i.key_pressed(egui::Key::ArrowDown) {
            state.idx = state.idx.wrapping_add(1);
        }
        if i.key_pressed(egui::Key::ArrowUp) {
            state.idx = state.idx.wrapping_sub(1);
        }
        let cur = AddCategory::ORDER.iter().position(|&c| c == state.category).unwrap_or(0);
        if i.key_pressed(egui::Key::ArrowRight) {
            next_cat = Some(AddCategory::ORDER[(cur + 1) % AddCategory::ORDER.len()]);
        }
        if i.key_pressed(egui::Key::ArrowLeft) {
            next_cat =
                Some(AddCategory::ORDER[(cur + AddCategory::ORDER.len() - 1) % AddCategory::ORDER.len()]);
        }
    });
    if !state.open {
        return None;
    }
    if let Some(c) = next_cat {
        state.category = c;
        state.idx = 0;
    }

    let list = entries(library, state);
    // Wrap/clamp the highlight against the live list length.
    if list.is_empty() {
        state.idx = 0;
    } else {
        state.idx %= list.len();
    }

    let mut picked: Option<usize> = None; // index into `list`
    let mut keep_open = true;

    let resp = egui::Window::new("add-menu")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .fixed_pos(state.anchor)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.horizontal_top(|ui| {
                // Left column — category switcher.
                ui.vertical(|ui| {
                    ui.set_min_width(108.0);
                    ui.label(egui::RichText::new("ADD").weak().small());
                    ui.add_space(2.0);
                    for c in AddCategory::ORDER {
                        let label = format!("{}  {}", c.icon(), c.label());
                        if ui.selectable_label(state.category == c, label).clicked() {
                            state.category = c;
                            state.idx = 0;
                        }
                    }
                });
                ui.separator();
                // Right column — filterable entry list.
                ui.vertical(|ui| {
                    ui.set_min_width(196.0);
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut state.search)
                            .hint_text(format!("{}  Filter…", theme::icon::SEARCH))
                            .desired_width(f32::INFINITY),
                    );
                    // Focus the filter on the frame the menu opens so typing
                    // narrows immediately (Blender-style).
                    if search.changed() {
                        state.idx = 0;
                    }
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(280.0).auto_shrink([false, false]).show(
                        ui,
                        |ui| {
                            if list.is_empty() {
                                ui.label(egui::RichText::new("no match").weak().small());
                            }
                            for (li, e) in list.iter().enumerate() {
                                let sel = li == state.idx;
                                let row = ui.selectable_label(
                                    sel,
                                    format!("{}  {}", e.icon, e.label),
                                );
                                if sel {
                                    row.scroll_to_me(None);
                                }
                                if row.clicked() {
                                    picked = Some(li);
                                }
                            }
                        },
                    );
                });
            });
        });

    // Click-outside dismiss: if the pointer pressed and the press wasn't on our
    // window, close (matches the transient-popup convention of the other dialogs).
    if let Some(r) = &resp {
        let pointer_down = ctx.input(|i| i.pointer.any_pressed());
        if pointer_down {
            let on_us = ctx
                .input(|i| i.pointer.interact_pos())
                .map(|p| r.response.rect.contains(p))
                .unwrap_or(false);
            if !on_us {
                keep_open = false;
            }
        }
    }

    // Resolve a keyboard Enter against the highlighted row.
    if commit && !list.is_empty() {
        picked = Some(state.idx.min(list.len() - 1));
    }

    if let Some(li) = picked {
        state.open = false;
        // Move the chosen entry out of the list (it owns the AddAction).
        let mut list = list;
        return Some(list.swap_remove(li).action);
    }
    if !keep_open {
        state.open = false;
    }
    None
}
