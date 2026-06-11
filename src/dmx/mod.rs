//! Live DMX **input**: receive Art-Net + sACN from a lighting console and decode
//! it into the scene's fixtures, so a real desk animates the 3D rig.
//!
//! INPUT-ONLY by construction: nothing in this module transmits. Sockets are used
//! only with `recv_from` + `join_multicast_v4`; Art-Net `ArtPoll` is parsed and
//! ignored (never answered); no sACN discovery is sent. The
//! `input_only_no_transmit` guard test (in [`net`]) asserts no transmit call
//! appears anywhere in this tree.
//!
//! Threading: one background thread ([`net::run_loop`]) owns the sockets and the
//! per-source merge state, and publishes an immutable [`UniverseSnapshot`] into
//! the shared handle. The render thread reads it with a pointer-clone in
//! [`DmxIo::poll`] and applies it with [`DmxIo::decode`] — never blocking on I/O.

pub mod artnet;
pub mod decode;
pub mod feed;
pub mod net;
pub mod patch;
pub mod sacn;
pub mod universe;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::scene::Scene;

pub use net::SourceStat;
pub use patch::{PatchSource, PatchTable};
pub use universe::UniverseSnapshot;

/// How multiple sources contributing to one universe are combined.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MergePolicy {
    /// Highest priority wins; HTP (per-channel max) among equal-priority sources.
    #[default]
    PriorityHtp,
    /// Highest-takes-precedence per channel across all sources (ignore priority).
    Htp,
    /// The most recently received source replaces the others.
    Latest,
}

impl MergePolicy {
    pub const ALL: [MergePolicy; 3] = [Self::PriorityHtp, Self::Htp, Self::Latest];
    pub fn label(self) -> &'static str {
        match self {
            Self::PriorityHtp => "Priority + HTP",
            Self::Htp => "HTP",
            Self::Latest => "Latest",
        }
    }
}

/// Connectivity configuration, edited in the panel and pushed to the worker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DmxConfig {
    pub artnet: bool,
    pub sacn: bool,
    /// Interface to bind / multicast-join on (`0.0.0.0` = all interfaces).
    pub bind_ip: IpAddr,
    /// Universes to receive (1-based). sACN joins one multicast group per entry;
    /// for Art-Net an empty list accepts all.
    pub universes: Vec<u16>,
    pub merge: MergePolicy,
    /// Priority assigned to Art-Net sources (the protocol carries none).
    pub artnet_priority: u8,
}

impl Default for DmxConfig {
    fn default() -> Self {
        Self {
            artnet: true,
            sacn: true,
            bind_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            universes: vec![1],
            merge: MergePolicy::default(),
            artnet_priority: 100,
        }
    }
}

/// Live receive status, published by the worker for the connectivity panel.
#[derive(Clone, Default, Debug)]
pub struct DmxStatus {
    pub sources: Vec<SourceStat>,
    pub bound_artnet: bool,
    pub bound_sacn: bool,
}

/// The cross-thread handle. The worker writes; the render thread reads.
pub struct DmxShared {
    /// The latest published universes (swapped wholesale — a hand-rolled arc-swap).
    pub snapshot: Mutex<Arc<UniverseSnapshot>>,
    pub status: Mutex<DmxStatus>,
    pub config: Mutex<DmxConfig>,
    pub stop: AtomicBool,
}

/// A UI-requested control action, deferred so start/stop (which joins the worker
/// thread) never runs inside the egui pass.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PendingNetCmd {
    #[default]
    None,
    Start,
    Stop,
    /// Stop then start, to re-bind after a protocol/interface change.
    Reapply,
}

/// The DMX input facade: owns the patch (a side table), the worker, and the
/// last-polled snapshot/status. Lives on `State` (not `Ui`), so a stop/join can
/// never happen inside the egui pass.
pub struct DmxIo {
    patch: PatchTable,
    shared: Arc<DmxShared>,
    worker: Option<JoinHandle<()>>,
    /// Last snapshot pointer-cloned from the worker (or an injected feed).
    snapshot: Arc<UniverseSnapshot>,
    status: DmxStatus,
    /// Per-fixture: driven by live DMX this frame.
    live_mask: Vec<bool>,
    /// UI-facing config; pushed to `shared.config` each frame.
    config: DmxConfig,
    /// Universe shown in the grid panel.
    selected_universe: u16,
    /// Edit buffers for the connectivity panel's text fields (parsed into
    /// `config` each frame, so partial/invalid typing doesn't fight the parse).
    bind_ip_text: String,
    universes_text: String,
    pending: PendingNetCmd,
    /// Headless synthetic feed (no socket), used by `PREVIZ_DMX_FEED/INJECT`.
    injected: Option<Arc<UniverseSnapshot>>,
    stale: Duration,
}

impl DmxIo {
    pub fn new() -> Self {
        let config = DmxConfig::default();
        Self {
            patch: PatchTable::new(),
            shared: Arc::new(DmxShared {
                snapshot: Mutex::new(Arc::new(UniverseSnapshot::default())),
                status: Mutex::new(DmxStatus::default()),
                config: Mutex::new(config.clone()),
                stop: AtomicBool::new(false),
            }),
            worker: None,
            snapshot: Arc::new(UniverseSnapshot::default()),
            status: DmxStatus::default(),
            live_mask: Vec::new(),
            selected_universe: 1,
            bind_ip_text: "0.0.0.0".to_string(),
            universes_text: "1".to_string(),
            config,
            // Listen by default: the first interactive frame's `apply_pending`
            // starts the receiver, so a console drives the rig with no setup.
            // (Headless capture paths exit before the render loop, so they never
            // bind a socket from this.)
            pending: PendingNetCmd::Start,
            injected: None,
            stale: Duration::from_millis(2500),
        }
    }

    /// Spawn the receive thread (no-op if already running).
    pub fn start(&mut self) {
        if self.worker.is_some() {
            return;
        }
        *self.shared.config.lock().unwrap() = self.config.clone();
        self.shared.stop.store(false, Ordering::Relaxed);
        let shared = self.shared.clone();
        self.worker = Some(std::thread::spawn(move || net::run_loop(shared)));
        log::info!("DMX: receiver started ({:?})", self.config.universes);
    }

    /// Signal the worker to stop and join it; clears the live snapshot.
    pub fn stop(&mut self) {
        if let Some(handle) = self.worker.take() {
            self.shared.stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
            log::info!("DMX: receiver stopped");
        }
        self.snapshot = Arc::new(UniverseSnapshot::default());
        self.status = DmxStatus::default();
        *self.shared.status.lock().unwrap() = DmxStatus::default();
    }

    /// Apply any deferred UI command and push config edits to the worker. Call
    /// once per frame from `State::render`, BEFORE building the egui UI.
    pub fn apply_pending(&mut self) {
        if self.worker.is_some() {
            *self.shared.config.lock().unwrap() = self.config.clone();
        }
        match std::mem::take(&mut self.pending) {
            PendingNetCmd::Start => self.start(),
            PendingNetCmd::Stop => self.stop(),
            PendingNetCmd::Reapply => {
                self.stop();
                self.start();
            }
            PendingNetCmd::None => {}
        }
    }

    /// Pointer-clone the latest snapshot + status (cheap; under a brief lock).
    pub fn poll(&mut self) {
        if let Some(inj) = &self.injected {
            self.snapshot = inj.clone();
            return;
        }
        if self.worker.is_none() {
            return;
        }
        self.snapshot = self.shared.snapshot.lock().unwrap().clone();
        self.status = self.shared.status.lock().unwrap().clone();
    }

    /// Sync the patch to the scene and decode the latest snapshot into fixtures.
    pub fn decode(&mut self, scene: &mut Scene) {
        self.patch.sync(scene);
        decode::apply(
            &mut scene.fixtures,
            &self.patch,
            &self.snapshot,
            &mut self.live_mask,
            self.stale,
        );
    }

    /// Auto-assign addresses to every unpatched fixture (the "Auto-patch" action).
    pub fn auto_patch(&mut self, scene: &Scene) {
        self.patch.auto_assign(scene, 1, 1);
    }

    /// Inject a synthetic snapshot (headless feed; no socket).
    pub fn inject(&mut self, snap: UniverseSnapshot) {
        self.injected = Some(Arc::new(snap));
    }

    pub fn patch(&self) -> &PatchTable {
        &self.patch
    }

    pub fn live_mask(&self) -> &[bool] {
        &self.live_mask
    }

    /// All disjoint borrows the UI panels need, in one call (so the panels can
    /// hold several `&mut` views of `DmxIo` at once).
    pub fn view(&mut self) -> DmxView<'_> {
        DmxView {
            running: self.worker.is_some(),
            patch: &mut self.patch,
            snapshot: &self.snapshot,
            status: &self.status,
            config: &mut self.config,
            live_mask: &self.live_mask,
            selected_universe: &mut self.selected_universe,
            bind_ip_text: &mut self.bind_ip_text,
            universes_text: &mut self.universes_text,
            pending: &mut self.pending,
        }
    }
}

impl Default for DmxIo {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DmxIo {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A bundle of disjoint borrows of [`DmxIo`] for the UI panels.
pub struct DmxView<'a> {
    pub running: bool,
    pub patch: &'a mut PatchTable,
    pub snapshot: &'a UniverseSnapshot,
    pub status: &'a DmxStatus,
    pub config: &'a mut DmxConfig,
    pub live_mask: &'a [bool],
    pub selected_universe: &'a mut u16,
    pub bind_ip_text: &'a mut String,
    pub universes_text: &'a mut String,
    pub pending: &'a mut PendingNetCmd,
}

/// Apply `PREVIZ_DMX_*` dev knobs in `resumed()`, mirroring the other `PREVIZ_*`
/// harness entry points. Lets the whole pipeline be exercised headlessly:
///
/// - `PREVIZ_DMX=artnet|sacn|both` (+ `PREVIZ_DMX_BIND`, `PREVIZ_DMX_UNIVERSES`)
///   configures and starts the REAL receiver (point a console at the binary).
/// - `PREVIZ_DMX_AUTOPATCH=1` auto-assigns addresses to the rig.
/// - `PREVIZ_DMX_FEED=look|full|ramp` injects a deterministic synthetic universe
///   set (no socket) through the real decode path — composes with
///   `PREVIZ_SCREENSHOT`/`PREVIZ_SHEET`.
/// - `PREVIZ_DMX_INJECT="u,ch,val; …"` injects explicit channel values.
/// - `PREVIZ_DMX_DUMP=1` logs each fixture's decoded state (a non-graphical oracle).
pub fn apply_env_knobs(dmx: &mut DmxIo, scene: &mut Scene) {
    let env = std::env::var;

    // Headless receive diagnostic: start the real receiver, dump what arrives for
    // N seconds, then exit. Verifies live Art-Net/sACN without the GUI.
    if let Ok(secs) = env("PREVIZ_DMX_LISTEN") {
        listen_and_exit(dmx, scene, secs.parse().unwrap_or(5));
    }

    if env("PREVIZ_DMX_AUTOPATCH").is_ok() {
        dmx.auto_patch(scene);
    }

    if let Ok(mode) = env("PREVIZ_DMX") {
        match mode.to_lowercase().as_str() {
            "artnet" | "art-net" => {
                dmx.config.artnet = true;
                dmx.config.sacn = false;
            }
            "sacn" => {
                dmx.config.artnet = false;
                dmx.config.sacn = true;
            }
            _ => {
                dmx.config.artnet = true;
                dmx.config.sacn = true;
            }
        }
        if let Ok(ip) = env("PREVIZ_DMX_BIND")
            && let Ok(ip) = ip.parse::<IpAddr>()
        {
            dmx.config.bind_ip = ip;
        }
        if let Ok(list) = env("PREVIZ_DMX_UNIVERSES") {
            dmx.config.universes = parse_universe_list(&list);
        }
        dmx.start();
    }

    if let Ok(kind) = env("PREVIZ_DMX_FEED") {
        let snap = feed::look(scene, dmx.patch(), &kind);
        dmx.inject(snap);
    }
    if let Ok(spec) = env("PREVIZ_DMX_INJECT") {
        dmx.inject(feed::inject_spec(&spec));
    }

    // For headless capture paths (which render without the live loop), decode the
    // injected feed once now so the screenshot reflects it.
    if dmx.injected.is_some() {
        dmx.poll();
        dmx.decode(scene);
    }

    if env("PREVIZ_DMX_DUMP").is_ok() {
        dmx.poll();
        dmx.decode(scene);
        for (i, f) in scene.fixtures.iter().enumerate() {
            let p = dmx.patch().get(i);
            log::info!(
                "DMX dump #{i} {}: patch {:?} pan {:.1} tilt {:.1} dim {:.2} color {:?} live {}",
                f.name,
                p.map(|p| (p.universe, p.address, p.enabled)),
                f.pan,
                f.tilt,
                f.intensity,
                f.color,
                dmx.live_mask().get(i).copied().unwrap_or(false),
            );
        }
    }
}

/// `PREVIZ_DMX_LISTEN=secs`: start the real receiver, print every universe that
/// arrives (and the sources) for `secs` seconds, then exit. A non-GUI smoke test
/// for live reception. Honours `PREVIZ_DMX_BIND` / `PREVIZ_DMX_UNIVERSES`.
fn listen_and_exit(dmx: &mut DmxIo, scene: &mut Scene, secs: u64) -> ! {
    if let Some(ip) = std::env::var("PREVIZ_DMX_BIND").ok().and_then(|s| s.parse::<IpAddr>().ok()) {
        dmx.config.bind_ip = ip;
    }
    if let Ok(list) = std::env::var("PREVIZ_DMX_UNIVERSES") {
        dmx.config.universes = parse_universe_list(&list);
    }
    // Optional: PREVIZ_DMX_PATCH="universe,address" patches fixture 0 so we can
    // confirm the live feed decodes into a moving fixture.
    let patch_fixture0 = std::env::var("PREVIZ_DMX_PATCH").ok().and_then(|s| {
        let p: Vec<&str> = s.split(',').map(str::trim).collect();
        match p.as_slice() {
            [u, a] => Some((u.parse::<u16>().ok()?, a.parse::<u16>().ok()?)),
            _ => None,
        }
    });
    log::info!(
        "DMX LISTEN: starting receiver for {secs}s (artnet={} sacn={} bind={} sacn_universes={:?})",
        dmx.config.artnet,
        dmx.config.sacn,
        dmx.config.bind_ip,
        dmx.config.universes,
    );
    dmx.start();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs.max(1)) {
        std::thread::sleep(Duration::from_millis(250));
        dmx.poll();
    }
    dmx.poll();
    log::info!(
        "DMX LISTEN: bound artnet={} sacn={}",
        dmx.status.bound_artnet,
        dmx.status.bound_sacn
    );
    let mut us: Vec<u16> = dmx.snapshot.frames.keys().copied().collect();
    us.sort_unstable();
    if us.is_empty() {
        log::warn!("DMX LISTEN: no universes received");
    }
    for u in us {
        let f = &dmx.snapshot.frames[&u];
        log::info!("  universe {u}: {} src, ch1..16 = {:?}", f.sources, &f.levels[..16]);
    }
    for s in &dmx.status.sources {
        let who = if s.name.is_empty() { &s.label } else { &s.name };
        log::info!(
            "  source: {} {} universes={:?} fps={:.0} packets={}",
            s.proto.label(),
            who,
            s.universes,
            s.fps,
            s.packets
        );
    }

    // Live decode check: patch fixture 0 and show what the console drove it to.
    if let Some((u, a)) = patch_fixture0
        && !scene.fixtures.is_empty()
    {
        dmx.patch.sync(scene);
        if let Some(p) = dmx.patch.get_mut(0) {
            p.universe = u;
            p.address = a;
            p.enabled = true;
            p.source = patch::PatchSource::Manual;
        }
        dmx.decode(scene);
        let f = &scene.fixtures[0];
        log::info!(
            "DMX LISTEN decode: fixture 0 '{}' patched {u}.{a} -> pan {:.1} tilt {:.1} dim {:.2} cmy {:?} live {}",
            f.name,
            f.pan,
            f.tilt,
            f.intensity,
            f.optics.cmy,
            dmx.live_mask().first().copied().unwrap_or(false),
        );
    }

    dmx.stop();
    std::process::exit(0);
}

/// Parse a `"1,2,5-8"` universe list into a sorted, de-duplicated vec.
pub fn parse_universe_list(s: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in s.split([',', ' ']).filter(|t| !t.trim().is_empty()) {
        let tok = tok.trim();
        if let Some((a, b)) = tok.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
                out.extend(a.min(b)..=a.max(b));
            }
        } else if let Ok(u) = tok.parse::<u16>() {
            out.push(u);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn universe_list_parses_ranges_and_dedups() {
        assert_eq!(parse_universe_list("1, 3, 5-7, 3"), vec![1, 3, 5, 6, 7]);
        assert_eq!(parse_universe_list(""), Vec::<u16>::new());
        assert_eq!(parse_universe_list("2"), vec![2]);
    }
}
