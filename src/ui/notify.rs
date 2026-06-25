//! Transient toasts + a persistent report log — the status/feedback domain we had
//! none of (`docs/RESEARCH-industry-patterns.md` §2.10, backlog #21/#22/#23).
//!
//! One [`report`](Notifier::report) call is BOTH a fading [`Toast`] (stacked
//! top-right, auto-expiring) AND a permanent [`LogEntry`] in the history — the
//! report/toast duality Blender's `BKE_report` + Unreal's `FNotificationInfo`
//! share. User-facing moments that used to `log::info!`/`log::error!` silently
//! (save / open / import / DMX connect / undo) now `report(..)` so the user sees
//! them. The status-bar message STACK (#21) and the modal-hint pill (#23) live in
//! `mod.rs`/`panels.rs`; this module owns the toast/log half.
//!
//! Lifecycle: `report` enqueues with a severity-scaled TTL; [`tick`](Notifier::tick)
//! ages every toast each frame and retires the expired ones; [`draw`](Notifier::draw)
//! paints the live stack. The log keeps the last [`LOG_CAP`] entries regardless of
//! toast expiry, so a closed toast is still recoverable (a future log window can
//! read [`Notifier::log`]).

use crate::ui::theme;

/// How long a toast lingers before fading out, scaled by severity — an error
/// stays put long enough to read; a routine success blinks past. Matches the
/// "errors persist, info is glanceable" convention both engines use.
const TTL_INFO: f32 = 3.0;
const TTL_SUCCESS: f32 = 3.5;
const TTL_WARN: f32 = 5.0;
const TTL_ERROR: f32 = 7.0;
/// Last second of a toast's life cross-fades its alpha to 0.
const FADE: f32 = 0.6;
/// Hard cap on retained log history (ring-buffer-ish; oldest dropped past this).
const LOG_CAP: usize = 200;
/// At most this many toasts are drawn at once (older live ones wait their turn by
/// simply being further down the stack; we cap the *drawn* count to keep the
/// corner uncluttered on a burst).
const MAX_VISIBLE: usize = 5;

/// Report severity — drives the toast accent, icon, and TTL, and tags the log
/// entry. Ordered least→most urgent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Info,
    Success,
    Warn,
    Error,
}

impl Severity {
    fn ttl(self) -> f32 {
        match self {
            Severity::Info => TTL_INFO,
            Severity::Success => TTL_SUCCESS,
            Severity::Warn => TTL_WARN,
            Severity::Error => TTL_ERROR,
        }
    }
    /// Leading glyph (Phosphor, no emoji — `theme::icon`).
    fn icon(self) -> &'static str {
        match self {
            Severity::Info => theme::icon::INFO,
            Severity::Success => theme::icon::CHECK,
            Severity::Warn => theme::icon::WARN,
            Severity::Error => theme::icon::ERROR,
        }
    }
    /// Accent colour for the icon + left rule, read from the live role palette so
    /// it tracks the theme (Success→ok, Warn→warn, Error→conflict, Info→accent).
    fn color(self, pal: &theme::Palette) -> egui::Color32 {
        match self {
            Severity::Info => pal.accent,
            Severity::Success => pal.ok,
            Severity::Warn => pal.warn,
            Severity::Error => pal.conflict,
        }
    }
}

/// A single fading toast. `age` counts up from 0; the toast retires once
/// `age >= ttl`. `text` is the user-facing line (kept short — detail goes to the
/// log). `action` is reserved for the provider-keyed actionable warnings (#56);
/// stored now so the call sites and log keep one shape.
pub struct Toast {
    pub severity: Severity,
    pub text: String,
    pub age: f32,
    pub ttl: f32,
}

impl Toast {
    /// Remaining-life alpha in [0,1]: full until the last [`FADE`] seconds, then
    /// a linear cross-fade to 0 so the toast dissolves rather than popping.
    fn alpha(&self) -> f32 {
        let remaining = self.ttl - self.age;
        (remaining / FADE).clamp(0.0, 1.0)
    }
    fn expired(&self) -> bool {
        self.age >= self.ttl
    }
}

/// A permanent log row — the report history that outlives the toast. `text` +
/// `severity` mirror the toast; a future log window reads these (the fields are
/// read by tests + the upcoming report-log window, not by the toast renderer).
#[allow(dead_code)] // consumed by the report-log window (later stage).
pub struct LogEntry {
    pub severity: Severity,
    pub text: String,
}

/// The notifier: the live toast queue + the persistent log. Held on `Ui`, ticked
/// once per frame, drawn over the central area.
#[derive(Default)]
pub struct Notifier {
    toasts: Vec<Toast>,
    log: Vec<LogEntry>,
}

impl Notifier {
    /// Enqueue a report: a fading toast AND a permanent log entry. The single
    /// entry point replacing silent `log::*` at user-facing moments. Also mirrors
    /// to `log::*` so the console trace is unchanged for headless / CI runs.
    pub fn report(&mut self, severity: Severity, text: impl Into<String>) {
        let text = text.into();
        match severity {
            Severity::Error => log::error!("{text}"),
            Severity::Warn => log::warn!("{text}"),
            _ => log::info!("{text}"),
        }
        self.toasts.push(Toast { severity, text: text.clone(), age: 0.0, ttl: severity.ttl() });
        self.log.push(LogEntry { severity, text });
        if self.log.len() > LOG_CAP {
            // Drop the oldest overflow in one shot (rare; only on a long session).
            let drop = self.log.len() - LOG_CAP;
            self.log.drain(0..drop);
        }
    }

    // Convenience shorthands for the common severities — keep call sites terse.
    pub fn info(&mut self, text: impl Into<String>) {
        self.report(Severity::Info, text);
    }
    pub fn success(&mut self, text: impl Into<String>) {
        self.report(Severity::Success, text);
    }
    pub fn warn(&mut self, text: impl Into<String>) {
        self.report(Severity::Warn, text);
    }
    pub fn error(&mut self, text: impl Into<String>) {
        self.report(Severity::Error, text);
    }

    /// The persistent report history (oldest→newest). Read by a future log window.
    #[allow(dead_code)] // consumed by the report-log window (later stage).
    pub fn log(&self) -> &[LogEntry] {
        &self.log
    }

    /// Age every live toast by `dt` and retire the expired ones. Called once per
    /// frame from `Ui::show` (a running fade keeps requesting repaints via `draw`).
    pub fn tick(&mut self, dt: f32) {
        for t in &mut self.toasts {
            t.age += dt;
        }
        self.toasts.retain(|t| !t.expired());
    }

    /// Paint the live toast stack in the top-right of the central area. Newest on
    /// top; capped at [`MAX_VISIBLE`] drawn rows. Requests a repaint while any
    /// toast is alive so the fade animates even when the app is otherwise idle.
    pub fn draw(&self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        // Keep animating the fade even when nothing else is dirtying the frame.
        ctx.request_repaint();
        let pal = theme::Palette::get_ctx(ctx);
        // Anchor below the menu+the dock's top so we don't cover the header chrome;
        // egui's Area handles the actual screen rect.
        let area = egui::Area::new(egui::Id::new("notify-toasts"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 38.0))
            .interactable(false)
            .order(egui::Order::Foreground);
        area.show(ctx, |ui| {
            ui.set_max_width(340.0);
            // Newest first (top of the stack), capped.
            for t in self.toasts.iter().rev().take(MAX_VISIBLE) {
                toast_row(ui, t, &pal);
                ui.add_space(6.0);
            }
        });
    }
}

/// Draw one toast: a dark rounded chip with a severity-tinted left rule + icon
/// and the message, faded by the toast's remaining-life alpha. Matches the
/// `theme::overlay_label` visual language (dark chip, light ink).
fn toast_row(ui: &mut egui::Ui, t: &Toast, pal: &theme::Palette) {
    let a = t.alpha();
    let accent = t.severity.color(pal).gamma_multiply(a);
    let fg = egui::Color32::from_gray(238).gamma_multiply(a);
    let bg = egui::Color32::from_black_alpha((180.0 * a) as u8);

    egui::Frame::NONE
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(10, 7))
        .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.6)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t.severity.icon()).color(accent).size(14.0));
                ui.add_space(2.0);
                ui.label(egui::RichText::new(&t.text).color(fg).size(12.5));
            });
        });
}

// ===========================================================================
// Handle-based status-bar message stack (#21).
//
// Unreal's `PushStatusBarMessage`/`PopStatusBarMessage` model: a tool pushes a
// transient message and gets back an opaque [`StatusHandle`]; it pops by that
// handle when done, so two tools can't clobber each other's slot. The status bar
// shows the TOP (most-recent live) message. A separate grey `hint` slot holds a
// passive context hint (e.g. "Drag to orbit") that any frame can overwrite —
// it's not handle-owned because it's advisory, not owned by a long-lived gesture.
// ===========================================================================

/// Opaque handle to a pushed status message — returned by [`StatusStack::push`],
/// passed back to [`StatusStack::pop`]. A monotonic id, so a stale handle (whose
/// message was already popped) is a harmless no-op.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StatusHandle(u64);

struct StatusMsg {
    handle: StatusHandle,
    text: String,
}

/// The status-bar message stack + the passive grey hint slot.
#[derive(Default)]
pub struct StatusStack {
    stack: Vec<StatusMsg>,
    next_id: u64,
    /// Passive context hint (grey), overwritten freely each frame. Cleared by
    /// [`clear_hint`](Self::clear_hint).
    hint: Option<String>,
}

impl StatusStack {
    /// Push a transient message; returns a handle to [`pop`](Self::pop) it later.
    /// The top of the stack is what the status bar shows.
    #[allow(dead_code)] // consumed by long-lived tool gestures (later stage).
    pub fn push(&mut self, text: impl Into<String>) -> StatusHandle {
        let handle = StatusHandle(self.next_id);
        self.next_id += 1;
        self.stack.push(StatusMsg { handle, text: text.into() });
        handle
    }

    /// Pop the message with this handle (anywhere in the stack — not just the top,
    /// so out-of-order release is safe). A stale handle is a no-op.
    #[allow(dead_code)] // paired with push() by tool gestures (later stage).
    pub fn pop(&mut self, handle: StatusHandle) {
        self.stack.retain(|m| m.handle != handle);
    }

    /// The top (most-recent live) message, if any — what the status bar renders.
    pub fn top(&self) -> Option<&str> {
        self.stack.last().map(|m| m.text.as_str())
    }

    /// Set the passive grey hint slot (advisory; freely overwritten per frame).
    pub fn set_hint(&mut self, text: impl Into<String>) {
        self.hint = Some(text.into());
    }

    /// Clear the passive hint (call when no context hint applies this frame).
    pub fn clear_hint(&mut self) {
        self.hint = None;
    }

    /// The current passive hint, if any.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_enqueues_toast_and_log() {
        let mut n = Notifier::default();
        n.success("saved");
        assert_eq!(n.toasts.len(), 1);
        assert_eq!(n.log.len(), 1);
        assert_eq!(n.toasts[0].severity, Severity::Success);
        assert_eq!(n.log[0].text, "saved");
    }

    #[test]
    fn tick_retires_expired_but_keeps_log() {
        let mut n = Notifier::default();
        n.info("hi");
        // Age past the longest possible TTL: the toast retires, the log persists.
        n.tick(TTL_ERROR + 1.0);
        assert!(n.toasts.is_empty(), "expired toast retired");
        assert_eq!(n.log.len(), 1, "log entry persists past toast expiry");
    }

    #[test]
    fn errors_outlive_info() {
        // Severity-scaled TTL: an error toast must persist longer than an info one.
        assert!(Severity::Error.ttl() > Severity::Info.ttl());
    }

    #[test]
    fn alpha_fades_in_last_window_only() {
        let mut t = Toast { severity: Severity::Info, text: String::new(), age: 0.0, ttl: 3.0 };
        assert_eq!(t.alpha(), 1.0, "full alpha well before expiry");
        t.age = t.ttl - FADE * 0.5; // halfway through the fade window
        assert!(t.alpha() > 0.0 && t.alpha() < 1.0, "fading near the end");
        t.age = t.ttl; // expired
        assert_eq!(t.alpha(), 0.0);
    }

    #[test]
    fn status_stack_top_and_handle_pop() {
        let mut s = StatusStack::default();
        let a = s.push("first");
        let b = s.push("second");
        assert_eq!(s.top(), Some("second"), "top is most-recent push");
        // Pop the OLDER handle out-of-order: the newer message stays on top.
        s.pop(a);
        assert_eq!(s.top(), Some("second"));
        s.pop(b);
        assert_eq!(s.top(), None, "stack empty after both popped");
        // A stale handle is a harmless no-op.
        s.pop(a);
        assert_eq!(s.top(), None);
    }

    #[test]
    fn status_hint_slot() {
        let mut s = StatusStack::default();
        assert_eq!(s.hint(), None);
        s.set_hint("Drag to orbit");
        assert_eq!(s.hint(), Some("Drag to orbit"));
        s.clear_hint();
        assert_eq!(s.hint(), None);
    }

    #[test]
    fn log_capped() {
        let mut n = Notifier::default();
        for i in 0..(LOG_CAP + 25) {
            n.info(format!("msg {i}"));
        }
        assert_eq!(n.log.len(), LOG_CAP, "log ring-capped");
        // The oldest were dropped; the newest survives.
        assert_eq!(n.log.last().unwrap().text, format!("msg {}", LOG_CAP + 24));
    }
}
