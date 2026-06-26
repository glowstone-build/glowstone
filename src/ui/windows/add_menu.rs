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

use std::collections::HashMap;
use std::sync::Arc;

use crate::scene::library::Library;
use crate::ui::lib_prefs::{self, LibItem, LibraryPrefs};
use crate::ui::panels::load_gdtf_textures;
use crate::ui::theme;
use crate::ui::GdtfTextures;

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

/// Map an [`AddAction`] to the [`LibItem`] used to key it in Recent/Favourites.
fn action_item(a: &AddAction) -> LibItem {
    match *a {
        AddAction::Fixture(i) => LibItem::Fixture(i),
        AddAction::Gdtf(i) => LibItem::Gdtf(i),
        AddAction::Screen(i) => LibItem::Screen(i),
        AddAction::Environment(i) => LibItem::Env(i),
    }
}

/// One flattened, filtered, displayable entry: its label + the action it commits,
/// the search-rank score, and the lib-prefs key (so the menu can star it / mark
/// it recent).
struct Entry {
    icon: &'static str,
    label: String,
    action: AddAction,
    /// Fuzzy-search score (higher = better; 0 when the query is empty).
    score: i32,
    /// Stable lib-prefs key, for the star toggle + Recent matching.
    key: String,
    /// Decoded GDTF thumbnail (imported fixtures only), drawn in place of the
    /// glyph when present (S2a).
    thumb: Option<egui::TextureId>,
}

/// Every addable entry in the active category, unfiltered, in catalog order.
/// (GDTF fixtures first — most specific — then the built-in profiles.)
fn category_entries(
    library: &Library,
    category: AddCategory,
    thumbs: &HashMap<usize, egui::TextureId>,
) -> Vec<Entry> {
    let mut out = Vec::new();
    let mk = |action: AddAction, icon: &'static str, label: String| {
        let key = lib_prefs::entry_key(library, action_item(&action)).unwrap_or_default();
        // Only GDTF imports carry a thumbnail (keyed by catalog index).
        let thumb = match action {
            AddAction::Gdtf(i) => thumbs.get(&i).copied(),
            _ => None,
        };
        Entry { icon, label, action, score: 0, key, thumb }
    };
    match category {
        AddCategory::Fixtures => {
            for (gi, g) in library.gdtf.iter().enumerate() {
                out.push(mk(
                    AddAction::Gdtf(gi),
                    theme::icon::FIXTURE,
                    format!("{} · {}", g.manufacturer, g.name),
                ));
            }
            for (pi, p) in library.fixtures.iter().enumerate() {
                let icon = if p.laser { theme::icon::COLOR } else { theme::icon::FIXTURE };
                out.push(mk(AddAction::Fixture(pi), icon, format!("{} · {}", p.category, p.name)));
            }
        }
        AddCategory::Screens => {
            for (si, s) in library.screens.iter().enumerate() {
                out.push(mk(
                    AddAction::Screen(si),
                    theme::icon::SCREEN,
                    format!("{} · {}", s.category, s.name),
                ));
            }
        }
        AddCategory::Environment => {
            for (ei, e) in library.environments.iter().enumerate() {
                out.push(mk(
                    AddAction::Environment(ei),
                    theme::icon::ENVIRONMENT,
                    format!("{} · {}", e.category, e.name),
                ));
            }
        }
    }
    out
}

/// Build the filtered + fuzzy-ranked entry list for the active category. An empty
/// query keeps catalog order; a non-empty query fuzzy-scores against the label and
/// drops non-matches, best-first.
fn entries(
    library: &Library,
    state: &AddMenuState,
    thumbs: &HashMap<usize, egui::TextureId>,
) -> Vec<Entry> {
    let q = state.search.trim().to_lowercase();
    let mut out = category_entries(library, state.category, thumbs);
    if q.is_empty() {
        return out;
    }
    out.retain_mut(|e| match lib_prefs::fuzzy_score(&q, &e.label.to_lowercase()) {
        Some(s) => {
            e.score = s;
            true
        }
        None => false,
    });
    // Stable sort by descending score keeps catalog order among ties.
    out.sort_by(|a, b| b.score.cmp(&a.score));
    out
}

/// Recent + Favourites pinned entries, resolved from `prefs` against the live
/// library across ALL categories (so a starred screen shows even while the
/// Fixtures tab is active). Recent keeps prefs order (most-recent first);
/// Favourites is rendered in catalog order. Only shown when the search box is
/// empty (a query searches the full catalog, Blender-style).
fn pinned_entries(
    library: &Library,
    prefs: &LibraryPrefs,
    thumbs: &HashMap<usize, egui::TextureId>,
) -> (Vec<Entry>, Vec<Entry>) {
    // Flatten every category once, indexed by key, to resolve prefs keys → Entry.
    let mut all: Vec<Entry> = Vec::new();
    for c in AddCategory::ORDER {
        all.extend(category_entries(library, c, thumbs));
    }
    let find = |key: &str| all.iter().find(|e| e.key == key).map(clone_entry);
    let recent: Vec<Entry> = prefs.recent.iter().filter_map(|k| find(k)).collect();
    let favourites: Vec<Entry> =
        all.iter().filter(|e| prefs.is_favourite(&e.key)).map(clone_entry).collect();
    (recent, favourites)
}

/// Shallow clone of an Entry (AddAction isn't Clone — it's a plain index enum, so
/// reconstruct it from the variant).
fn clone_entry(e: &Entry) -> Entry {
    let action = match e.action {
        AddAction::Fixture(i) => AddAction::Fixture(i),
        AddAction::Gdtf(i) => AddAction::Gdtf(i),
        AddAction::Screen(i) => AddAction::Screen(i),
        AddAction::Environment(i) => AddAction::Environment(i),
    };
    Entry { icon: e.icon, label: e.label.clone(), action, score: e.score, key: e.key.clone(), thumb: e.thumb }
}

/// Draw the Add menu if open. Returns the chosen action (and auto-closes) on a
/// pick; returns `None` otherwise. Esc / clicking outside dismisses it.
pub fn add_menu_window(
    ctx: &egui::Context,
    library: &Library,
    state: &mut AddMenuState,
    prefs: &mut LibraryPrefs,
    // Shared GDTF texture cache (keyed by Arc pointer, same as the inspector +
    // library browser) so imported fixtures show their thumbnail here too (S2a).
    gdtf_textures: &mut HashMap<usize, GdtfTextures>,
) -> Option<AddAction> {
    if !state.open {
        return None;
    }

    // Decode each imported GDTF's thumbnail once into the shared cache, then build
    // a catalog-index → TextureId lookup the entry rows can read.
    let mut thumbs: HashMap<usize, egui::TextureId> = HashMap::new();
    for (gi, g) in library.gdtf.iter().enumerate() {
        let key = Arc::as_ptr(g) as usize;
        let tex = gdtf_textures.entry(key).or_insert_with(|| load_gdtf_textures(ctx, g));
        if let Some(t) = &tex.thumbnail {
            thumbs.insert(gi, t.id());
        }
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

    let list = entries(library, state, &thumbs);
    // Wrap/clamp the highlight against the live list length.
    if list.is_empty() {
        state.idx = 0;
    } else {
        state.idx %= list.len();
    }
    // Pinned Recent + Favourites sections show only when the search box is empty
    // (a query searches the full catalog). Resolved across all categories.
    let show_pinned = state.search.trim().is_empty();
    let (recent, favourites) =
        if show_pinned { pinned_entries(library, prefs, &thumbs) } else { (Vec::new(), Vec::new()) };

    let mut picked: Option<usize> = None; // index into `list`
    let mut picked_action: Option<AddAction> = None; // a pinned-section click
    let mut toggle_fav: Option<String> = None; // a star click → key to toggle
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
                            // Pinned pseudo-categories (#20): Recent + Favourites,
                            // each a small caption header + its own clickable rows
                            // with a star toggle. Empty sections are skipped.
                            for (title, items) in [("RECENT", &recent), ("FAVOURITES", &favourites)] {
                                if items.is_empty() {
                                    continue;
                                }
                                ui.add_space(2.0);
                                ui.label(egui::RichText::new(title).weak().small());
                                for e in items {
                                    if let Some(a) = pinned_row(ui, e, prefs, &mut toggle_fav) {
                                        picked_action = Some(a);
                                    }
                                }
                                ui.add_space(2.0);
                                ui.separator();
                            }

                            if list.is_empty() {
                                ui.label(egui::RichText::new("no match").weak().small());
                            }
                            for (li, e) in list.iter().enumerate() {
                                let sel = li == state.idx;
                                if let Some(a) =
                                    catalog_row(ui, e, sel, prefs, &mut toggle_fav)
                                {
                                    picked_action = Some(a);
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

    // Apply a star toggle (mutates + persists prefs); does NOT commit/close.
    if let Some(key) = toggle_fav {
        prefs.toggle_favourite(&key);
    }

    // Resolve a keyboard Enter against the highlighted catalog row.
    if commit && picked_action.is_none() && !list.is_empty() {
        picked = Some(state.idx.min(list.len() - 1));
    }
    if picked_action.is_none()
        && let Some(li) = picked
    {
        let mut list = list;
        picked_action = Some(list.swap_remove(li).action);
    }

    if let Some(a) = picked_action {
        state.open = false;
        return Some(a);
    }
    if !keep_open {
        state.open = false;
    }
    None
}

/// Draw one pinned (Recent/Favourites) row: icon + label + a trailing star
/// toggle. Returns the action when the row body is clicked. A star click sets
/// `toggle_fav` and does NOT commit.
fn pinned_row(
    ui: &mut egui::Ui,
    e: &Entry,
    prefs: &LibraryPrefs,
    toggle_fav: &mut Option<String>,
) -> Option<AddAction> {
    row_with_star(ui, e, false, prefs, toggle_fav)
}

/// Draw one catalog row (keyboard-highlightable) with a trailing star toggle.
fn catalog_row(
    ui: &mut egui::Ui,
    e: &Entry,
    selected: bool,
    prefs: &LibraryPrefs,
    toggle_fav: &mut Option<String>,
) -> Option<AddAction> {
    row_with_star(ui, e, selected, prefs, toggle_fav)
}

/// Shared row: a full-width selectable label + a right-aligned star button.
/// Returns `Some(action)` when the label is clicked; a star click only flips
/// `toggle_fav`. The starred state is read live from `prefs`.
fn row_with_star(
    ui: &mut egui::Ui,
    e: &Entry,
    selected: bool,
    prefs: &LibraryPrefs,
    toggle_fav: &mut Option<String>,
) -> Option<AddAction> {
    let starred = !e.key.is_empty() && prefs.is_favourite(&e.key);
    let mut out = None;
    ui.horizontal(|ui| {
        // Star toggle — accent-tinted when favourited, dim otherwise. (The
        // Regular Phosphor set has no separate filled glyph, so colour carries
        // the state.)
        let star = theme::icon::STAR;
        let tint = if starred {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().weak_text_color()
        };
        if ui
            .add(egui::Button::new(egui::RichText::new(star).color(tint)).frame(false))
            .on_hover_text(if starred { "Unstar" } else { "Add to Favourites" })
            .clicked()
            && !e.key.is_empty()
        {
            *toggle_fav = Some(e.key.clone());
        }
        // A small thumbnail (imported GDTF) sits before the label; the glyph icon
        // is folded into the label text only when there's no thumbnail.
        let label = if e.thumb.is_some() {
            if let Some(id) = e.thumb {
                ui.add(egui::Image::new((id, egui::vec2(18.0, 18.0))));
            }
            e.label.clone()
        } else {
            format!("{}  {}", e.icon, e.label)
        };
        let row = ui.selectable_label(selected, label);
        if selected {
            row.scroll_to_me(None);
        }
        if row.clicked() {
            out = action_of(e);
        }
    });
    out
}

/// Reconstruct the [`AddAction`] of an entry (the enum is plain index variants).
fn action_of(e: &Entry) -> Option<AddAction> {
    Some(match e.action {
        AddAction::Fixture(i) => AddAction::Fixture(i),
        AddAction::Gdtf(i) => AddAction::Gdtf(i),
        AddAction::Screen(i) => AddAction::Screen(i),
        AddAction::Environment(i) => AddAction::Environment(i),
    })
}
