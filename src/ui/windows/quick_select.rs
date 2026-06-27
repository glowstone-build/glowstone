//! The `s` quick-select palette — a small, keyboard-driven menu for batch
//! selection (All / same profile / same maker / invert / none). Operates on the
//! ACTIVE selection kind (`Selection::active_kind`), so it works for fixtures,
//! objects, screens and pyro devices alike — the type/maker filters only apply to
//! fixtures (which have a profile + GDTF maker), the rest stay disabled there.

use egui::{RichText, Sense};

use crate::scene::{Scene, SelKind, Selection};
use crate::ui::theme;

/// The `s` quick-select palette. Each option has a one-key shortcut; Esc dismisses.
pub fn quick_select_window(
    ctx: &egui::Context,
    scene: &Scene,
    selection: &mut Selection,
    open: &mut bool,
) {
    if !*open {
        return;
    }
    // Whichever kind currently holds the selection drives the whole palette
    // (defaults to Fixtures when nothing is selected — same as the keyboard path).
    let kind = selection.active_kind();
    let n = match kind {
        SelKind::Fixtures => scene.fixtures.len(),
        SelKind::Objects => scene.geometry.len(),
        SelKind::Screens => scene.screens.len(),
        SelKind::Pyro => scene.pyro.len(),
    };
    if n == 0 {
        *open = false;
        return;
    }

    // Type / maker filters are fixture-only (profile + GDTF manufacturer); for the
    // other kinds the T/M rows render disabled (kept in the layout so the key→id
    // mapping is stable).
    let fx = kind == SelKind::Fixtures;
    let primary = if fx { selection.primary_fixture().filter(|&i| i < n) } else { None };
    let prof = primary.map(|i| scene.fixtures[i].profile.clone());
    let maker = primary
        .and_then(|i| scene.fixtures[i].gdtf.as_ref())
        .map(|g| g.manufacturer.clone())
        .filter(|m| !m.is_empty());
    let type_n = prof.as_ref().map(|p| scene.fixtures.iter().filter(|f| &f.profile == p).count());
    let maker_n = maker.as_ref().map(|m| {
        scene.fixtures.iter().filter(|f| f.gdtf.as_ref().map(|g| &g.manufacturer) == Some(m)).count()
    });

    // The active kind's current selection (owned, so it survives the mutation below).
    let cur: Vec<usize> = match kind {
        SelKind::Fixtures => selection.fixtures.clone(),
        SelKind::Objects => selection.geometry.clone(),
        SelKind::Screens => selection.screens.clone(),
        SelKind::Pyro => selection.pyro.clone(),
    };
    let inv_n = n - cur.iter().filter(|&&i| i < n).count();

    let (icon, noun) = match kind {
        SelKind::Fixtures => (theme::icon::FIXTURE, "fixtures"),
        SelKind::Objects => (theme::icon::GEOMETRY, "objects"),
        SelKind::Screens => (theme::icon::SCREEN, "screens"),
        SelKind::Pyro => (theme::icon::PYRO, "pyro"),
    };
    let all_label = format!("All {noun}");

    // Resolve a chosen action id into a new selection.
    let mut action: Option<u8> = None;
    ctx.input(|i| {
        use egui::Key;
        if i.key_pressed(Key::A) {
            action = Some(0);
        }
        if i.key_pressed(Key::T) && fx && prof.is_some() {
            action = Some(1);
        }
        if i.key_pressed(Key::M) && fx && maker.is_some() {
            action = Some(2);
        }
        if i.key_pressed(Key::I) {
            action = Some(3);
        }
        if i.key_pressed(Key::N) {
            action = Some(4);
        }
        if i.key_pressed(Key::Escape) {
            action = Some(255);
        }
    });

    egui::Window::new("quick-select")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
        .show(ctx, |ui| {
            ui.set_min_width(260.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{icon}  Select")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Esc").small().weak());
                });
            });
            ui.separator();
            let rows: [(char, &str, Option<usize>, u8, bool); 5] = [
                ('A', all_label.as_str(), Some(n), 0, true),
                ('T', "All of this type", type_n, 1, fx && prof.is_some()),
                ('M', "All by this maker", maker_n, 2, fx && maker.is_some()),
                ('I', "Invert selection", Some(inv_n), 3, true),
                ('N', "Select none", None, 4, true),
            ];
            for (key, label, count, id, enabled) in rows {
                let resp = quick_row(ui, key, label, count, enabled);
                if resp.clicked() && enabled {
                    action = Some(id);
                }
            }
        });

    if let Some(a) = action {
        let new: Option<Vec<usize>> = match a {
            0 => Some((0..n).collect()),
            1 => prof.map(|p| (0..n).filter(|&i| scene.fixtures[i].profile == p).collect()),
            2 => maker.map(|m| {
                (0..n)
                    .filter(|&i| scene.fixtures[i].gdtf.as_ref().map(|g| &g.manufacturer) == Some(&m))
                    .collect()
            }),
            3 => {
                let set: std::collections::HashSet<usize> = cur.iter().copied().collect();
                Some((0..n).filter(|i| !set.contains(i)).collect())
            }
            4 => Some(Vec::new()),
            _ => None,
        };
        if let Some(v) = new {
            *selection = Selection::default();
            match kind {
                SelKind::Fixtures => selection.fixtures = v,
                SelKind::Objects => selection.geometry = v,
                SelKind::Screens => selection.screens = v,
                SelKind::Pyro => selection.pyro = v,
            }
        }
        *open = false;
    }
}

/// One row of the quick-select palette: label on the left, count + key badge on
/// the right, full-width clickable.
fn quick_row(ui: &mut egui::Ui, key: char, label: &str, count: Option<usize>, enabled: bool) -> egui::Response {
    let ink = theme::ink(!ui.visuals().dark_mode);
    let h = 26.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter_at(rect);
    if enabled && resp.hovered() {
        painter.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let fg = if enabled { ink.primary } else { ink.muted };
    painter.text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        fg,
    );
    let mut x = rect.right() - 8.0;
    // key badge
    let badge = egui::Rect::from_center_size(egui::pos2(x - 8.0, rect.center().y), egui::vec2(18.0, 16.0));
    painter.rect_filled(badge, 3.0, ui.visuals().extreme_bg_color);
    painter.text(badge.center(), egui::Align2::CENTER_CENTER, key, egui::FontId::monospace(11.0), ink.secondary);
    x -= 26.0;
    if let Some(c) = count {
        painter.text(
            egui::pos2(x, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{c}"),
            egui::FontId::monospace(11.0),
            ink.tertiary,
        );
    }
    resp
}
