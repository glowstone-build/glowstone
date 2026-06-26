//! The online Fixture Library window — browse the GDTF Share catalogue and
//! download fixtures into the project Library (the stock you then place from the
//! Library panel). Downloading here NEVER instantiates into the scene/viewport;
//! it only stocks the Library.
//!
//! UX shape borrows from Blender's extensions/plugins browser: a searchable,
//! filterable catalogue of rows, each with a clear status (cloud / cached /
//! update) and a single primary action. Signed-out users still see the cached
//! catalogue and can add already-downloaded fixtures; signing in enables refresh
//! and downloads.

use egui::{Align, Button, Layout, RichText};

use crate::scene::Library;
use crate::share::{ListEntry, RowStatus, Share};

use super::theme;

const ROW_H: f32 = 48.0;

/// The online Fixture Library window. Call every frame from `Ui::show`.
pub fn fixture_library_window(
    ctx: &egui::Context,
    open: &mut bool,
    share: &mut Share,
    library: &mut Library,
) {
    if *open {
        share.ensure_started(ctx);
    }
    // Pull worker state + finish any background download (download → import to
    // library), even if the window was closed mid-download.
    share.sync();
    resolve_pending(share, library);

    if !*open {
        return;
    }
    share.ensure_view();

    let mut keep = true;
    let mut add_clicks: Vec<i64> = Vec::new();
    let title = format!("{}  Fixture Library", theme::icon::ONLINE);

    egui::Window::new(title)
        .open(&mut keep)
        .resizable(true)
        .default_size([780.0, 560.0])
        .min_width(460.0)
        .show(ctx, |ui| {
            let accent = ui.visuals().selection.stroke.color;
            // ---- account row ----
            ui.horizontal(|ui| {
                ui.label(RichText::new("GDTF Share").strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if share.logged_in {
                        if ui.button(format!("{}  Sign out", theme::icon::SIGN_OUT)).clicked() {
                            share.logout();
                        }
                        ui.label(RichText::new(format!("{}  signed in", theme::icon::USER)).small().color(theme::OK));
                    } else if !share.user.is_empty() {
                        ui.label(RichText::new("signed out").small().weak());
                    }
                });
            });
            ui.separator();

            let first_run = !share.logged_in && share.list.is_empty();
            if first_run {
                login_card(ui, share, accent);
            } else {
                if !share.logged_in {
                    signin_banner(ui, share);
                }
                toolbar(ui, share, accent);
                banners(ui, share);
                ui.add_space(4.0);
                catalogue(ui, share, &mut add_clicks, accent);
            }
        });

    *open = keep;

    // Apply row actions outside the catalogue's immutable borrow of `share`.
    for rid in add_clicks {
        if share.downloaded.contains(&rid) {
            import_to_library(share, rid, library);
        } else if share.logged_in {
            share.pending_add.insert(rid);
            share.download(rid);
        }
    }
}

// ---------------------------------------------------------------------------

fn login_card(ui: &mut egui::Ui, share: &mut Share, accent: egui::Color32) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(360.0);
        ui.label(RichText::new(format!("{}  Sign in to GDTF Share", theme::icon::ONLINE)).heading());
        ui.add_space(4.0);
        ui.label(
            RichText::new("Browse and download fixtures from the community GDTF library. A free account at gdtf-share.com is required.")
                .small()
                .weak(),
        );
        ui.add_space(14.0);

        egui::Grid::new("gdtf-login").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
            ui.label("User");
            ui.add(egui::TextEdit::singleline(&mut share.user).desired_width(220.0).hint_text("username or e-mail"));
            ui.end_row();
            ui.label("Password");
            let submit = ui
                .add(egui::TextEdit::singleline(&mut share.password).password(true).desired_width(220.0))
                .lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.end_row();
            if submit && can_submit(share) {
                share.login();
            }
        });

        ui.add_space(4.0);
        ui.checkbox(&mut share.remember, "Remember me on this computer");
        ui.add_space(12.0);

        let busy = share.is_busy();
        let signin = ui.add_enabled(
            !busy && can_submit(share),
            Button::new(RichText::new(if busy { "Signing in…" } else { "Sign in" }).color(egui::Color32::BLACK))
                .fill(accent)
                .min_size(egui::vec2(220.0, 30.0)),
        );
        if signin.clicked() {
            share.login();
        }
        if let Some(err) = &share.error {
            ui.add_space(8.0);
            ui.label(RichText::new(err).color(theme::CONFLICT).small());
        }
    });
    ui.add_space(24.0);
}

fn signin_banner(ui: &mut egui::Ui, share: &mut Share) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  Sign in to download fixtures", theme::icon::SIGN_IN)).small());
            ui.add(egui::TextEdit::singleline(&mut share.user).desired_width(130.0).hint_text("user"));
            ui.add(egui::TextEdit::singleline(&mut share.password).password(true).desired_width(120.0).hint_text("password"));
            let enabled = !share.is_busy() && can_submit(share);
            if ui.add_enabled(enabled, Button::new("Sign in")).clicked() {
                share.login();
            }
            if let Some(err) = &share.error {
                ui.label(RichText::new(err).color(theme::CONFLICT).small());
            }
        });
    });
    ui.add_space(4.0);
}

fn toolbar(ui: &mut egui::Ui, share: &mut Share, _accent: egui::Color32) {
    ui.horizontal(|ui| {
        let can_refresh = share.logged_in && !share.is_busy();
        if ui
            .add_enabled(can_refresh, Button::new(format!("{}  Refresh", theme::icon::RESET)))
            .on_hover_text("Re-fetch the full fixture list from GDTF Share")
            .clicked()
        {
            share.refresh();
        }
        match &share.busy {
            Some(label) => {
                ui.spinner();
                ui.label(RichText::new(label).small().weak());
            }
            None => {
                if !share.list.is_empty() {
                    ui.label(RichText::new(format!("{} fixtures", share.list.len())).small().weak());
                }
            }
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.toggle_value(&mut share.updates_only, "Updates").on_hover_text("Only fixtures with a newer revision than the one you have");
            ui.toggle_value(&mut share.downloaded_only, "Downloaded").on_hover_text("Only fixtures already in your cache");
            manufacturer_filter(ui, share);
            ui.add(
                egui::TextEdit::singleline(&mut share.search)
                    .desired_width(190.0)
                    .hint_text(format!("{}  search fixtures…", theme::icon::SEARCH)),
            );
        });
    });
}

fn manufacturer_filter(ui: &mut egui::Ui, share: &mut Share) {
    let selected = if share.manufacturer.is_empty() { "All makes".to_string() } else { share.manufacturer.clone() };
    egui::ComboBox::from_id_salt("gdtf-mfr").selected_text(selected).width(150.0).show_ui(ui, |ui| {
        ui.selectable_value(&mut share.manufacturer, String::new(), "All makes");
        // Clone so the &share borrow ends before we mutate share.manufacturer.
        let mfrs = share.manufacturers().to_vec();
        for m in mfrs {
            ui.selectable_value(&mut share.manufacturer, m.clone(), m);
        }
    });
}

fn banners(ui: &mut egui::Ui, share: &Share) {
    if let Some(err) = &share.error {
        ui.add_space(4.0);
        ui.label(RichText::new(format!("{}  {}", theme::icon::WARNING, err)).color(theme::CONFLICT).small());
    }
}

fn catalogue(ui: &mut egui::Ui, share: &Share, add_clicks: &mut Vec<i64>, accent: egui::Color32) {
    let filtered = share.filtered();
    if filtered.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            let msg = if share.list.is_empty() {
                "No fixtures yet — Refresh to fetch the library."
            } else {
                "No fixtures match the current filters."
            };
            ui.label(RichText::new(msg).weak());
        });
        return;
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, filtered.len(), |ui, range| {
        for di in range {
            let i = filtered[di];
            let Some(e) = share.list.get(i) else { continue };
            let status = share.status(i);
            let row = ui.horizontal(|ui| {
                ui.set_height(ROW_H - 6.0);
                // status glyph
                let (glyph, color) = match status {
                    RowStatus::Cached => (theme::icon::CACHED, theme::OK),
                    RowStatus::Update => (theme::icon::CLOUD, theme::WARN),
                    RowStatus::Downloading => (theme::icon::DOWNLOAD, accent),
                    RowStatus::Cloud => (theme::icon::CLOUD, ui.visuals().weak_text_color()),
                };
                ui.label(RichText::new(glyph).color(color));
                ui.add_space(2.0);

                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new(&e.fixture).strong());
                    let modes = e.modes.len();
                    let rev = if e.revision.is_empty() { "—".to_string() } else { e.revision.clone() };
                    ui.label(
                        RichText::new(format!(
                            "{}   ·   rev {}   ·   {} mode{}   ·   {}",
                            e.manufacturer,
                            rev,
                            modes,
                            if modes == 1 { "" } else { "s" },
                            human_size(e.filesize),
                        ))
                        .small()
                        .weak(),
                    );
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    row_action(ui, share, e.rid, status, add_clicks, accent);
                    if e.rating > 0.0 {
                        ui.label(RichText::new(format!("{} {:.1}", theme::icon::STAR, e.rating)).small().weak());
                    }
                });
            });
            row.response.on_hover_ui(|ui| detail_tooltip(ui, e));
            ui.separator();
        }
    });
}

/// Rich hover detail for a catalogue row (version, author, dates, mode list).
fn detail_tooltip(ui: &mut egui::Ui, e: &ListEntry) {
    ui.set_max_width(280.0);
    ui.label(RichText::new(&e.fixture).strong());
    let ver = if e.version.is_empty() { "?" } else { e.version.as_str() };
    ui.label(RichText::new(format!("{} · GDTF spec {}", e.manufacturer, ver)).small());
    let by: Vec<&str> = [e.uploader.as_str(), e.creator.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
    if !by.is_empty() {
        ui.label(RichText::new(format!("by {}", by.join(" · "))).small().weak());
    }
    ui.label(
        RichText::new(format!("added {}   ·   updated {}", unix_to_date(e.creation_date), unix_to_date(e.last_modified)))
            .small()
            .weak(),
    );
    if !e.modes.is_empty() {
        ui.separator();
        for m in &e.modes {
            ui.label(RichText::new(format!("• {} — {} ch", m.name, m.dmxfootprint)).small());
        }
    }
}

/// `unix` seconds → `YYYY-MM-DD` (civil date, no external dep).
fn unix_to_date(ts: i64) -> String {
    if ts <= 0 {
        return "—".into();
    }
    let z = ts.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn row_action(
    ui: &mut egui::Ui,
    share: &Share,
    rid: i64,
    status: RowStatus,
    add_clicks: &mut Vec<i64>,
    accent: egui::Color32,
) {
    // Once a fixture has been imported into this project's Library, the row shows a
    // green, disabled "Added" — no further click (and no duplicate import) possible.
    if share.added.contains(&rid) {
        ui.add_enabled(
            false,
            Button::new(RichText::new(format!("{}  Added", theme::icon::CHECK)).color(theme::OK)),
        )
        .on_hover_text("Already in this project's Library — place it from the Library panel.");
        return;
    }
    match status {
        RowStatus::Downloading => {
            ui.add_enabled(false, Button::new("Downloading…"));
            ui.spinner();
        }
        RowStatus::Cached => {
            if ui
                .add(Button::new(RichText::new(format!("{}  Add to project", theme::icon::ADD)).color(egui::Color32::BLACK)).fill(accent))
                .on_hover_text("Add this fixture to the project library (already downloaded). Place it from the Library panel.")
                .clicked()
            {
                add_clicks.push(rid);
            }
        }
        RowStatus::Update => {
            let enabled = share.logged_in;
            if ui
                .add_enabled(enabled, Button::new(RichText::new(format!("{}  Update", theme::icon::CLOUD)).color(theme::WARN)))
                .on_hover_text(if enabled { "Download the newer revision into the project Library" } else { "Sign in to download" })
                .clicked()
            {
                add_clicks.push(rid);
            }
        }
        RowStatus::Cloud => {
            let enabled = share.logged_in;
            if ui
                .add_enabled(enabled, Button::new(format!("{}  Download", theme::icon::CLOUD)))
                .on_hover_text(if enabled { "Download this fixture into the project library, then place it from the Library panel." } else { "Sign in to download" })
                .clicked()
            {
                add_clicks.push(rid);
            }
        }
    }
}

// ---------------------------------------------------------------------------

/// Finish any background download: once the file is downloaded, import it into
/// the project library (NOT the scene). Drops entries whose download failed.
fn resolve_pending(share: &mut Share, library: &mut Library) {
    if share.pending_add.is_empty() {
        return;
    }
    let ready: Vec<i64> = share.pending_add.iter().copied().filter(|rid| share.downloaded.contains(rid)).collect();
    let failed: Vec<i64> = share
        .pending_add
        .iter()
        .copied()
        .filter(|rid| !share.downloaded.contains(rid) && !share.downloading.contains(rid))
        .collect();
    for rid in ready {
        share.pending_add.remove(&rid);
        import_to_library(share, rid, library);
    }
    for rid in failed {
        share.pending_add.remove(&rid); // the worker already surfaced the error
    }
}

/// Import a downloaded `.gdtf` into the project Library (dedup by filename),
/// tagged with the `GdtfShare` provenance. Does NOT touch the scene/viewport —
/// the user places it from the Library panel.
fn import_to_library(share: &mut Share, rid: i64, library: &mut Library) {
    let path = share.gdtf_path(rid);
    let fname = format!("{rid}.gdtf");
    let already = library.gdtf.iter().any(|g| g.spec == fname);
    if already {
        share.added.insert(rid); // already in this project → it reads as "Added"
        return;
    }
    match library.import_gdtf_with_source(&path, crate::gdtf::FixtureSource::GdtfShare) {
        Ok(_) => {
            share.added.insert(rid);
        }
        Err(e) => share.error = Some(format!("could not load downloaded fixture {rid}: {e}")),
    }
}

fn can_submit(share: &Share) -> bool {
    !share.user.trim().is_empty() && !share.password.is_empty()
}

fn human_size(bytes: i64) -> String {
    if bytes <= 0 {
        return "—".into();
    }
    let b = bytes as f64;
    if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
