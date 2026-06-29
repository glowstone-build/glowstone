//! The background DMX receive thread — the ONLY code that touches a socket.
//!
//! It binds UDP sockets (Art-Net 6454, sACN 5568 multicast), parses incoming
//! packets, tracks per-source liveness/FPS/sequence loss, merges multiple sources
//! per universe, and PUBLISHES an immutable [`UniverseSnapshot`] into
//! [`DmxShared`] for the render thread to pointer-clone. It never transmits:
//! sockets are used only with `recv_from` + `join_multicast_v4`, and the
//! `input_only_no_transmit` test asserts no transmit call exists in this tree.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::artnet;
use super::sacn::{self, SacnPacket};
use super::universe::{UniverseFrame, UniverseSnapshot};
use super::{DmxConfig, DmxShared, MergePolicy};

/// Standard Art-Net UDP port.
const ARTNET_PORT: u16 = 6454;
/// Standard sACN / E1.31 UDP port.
const SACN_PORT: u16 = 5568;
/// A source is dropped (and its merge contribution released) after this idle gap.
const STALE: Duration = Duration::from_millis(2500);
/// Socket read timeout — bounds worst-case service latency to ~2× this.
const READ_TIMEOUT: Duration = Duration::from_millis(10);
/// Snapshots are rebuilt at most this often (coalesce bursts; ~200 Hz cap).
const PUBLISH_INTERVAL: Duration = Duration::from_millis(5);
/// Max packets drained from one socket per loop, so a flood can't starve the other.
const DRAIN_CAP: usize = 256;

/// Which protocol a source speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Proto {
    ArtNet,
    Sacn,
}

impl Proto {
    pub fn label(self) -> &'static str {
        match self {
            Proto::ArtNet => "Art-Net",
            Proto::Sacn => "sACN",
        }
    }
}

/// A live source's telemetry, cloned into [`DmxStatus`](super::DmxStatus) for the UI.
#[derive(Clone, Debug)]
pub struct SourceStat {
    pub proto: Proto,
    /// Stable key label: `ip:port` (Art-Net) or CID hex prefix (sACN).
    pub label: String,
    /// sACN source name; empty for Art-Net.
    pub name: String,
    /// Universes this source has sent, sorted.
    pub universes: Vec<u16>,
    pub priority: u8,
    pub last_seen: Instant,
    /// Smoothed packets-per-second.
    pub fps: f32,
    pub packets: u64,
    pub seq_errors: u64,
}

impl SourceStat {
    fn new(proto: Proto, label: String, name: String, now: Instant) -> Self {
        Self {
            proto,
            label,
            name,
            universes: Vec::new(),
            priority: 0,
            last_seen: now,
            fps: 0.0,
            packets: 0,
            seq_errors: 0,
        }
    }

    /// Age since the last packet (for the UI "last seen" column).
    pub fn age(&self) -> Duration {
        self.last_seen.elapsed()
    }
}

/// A source key: Art-Net by socket address, sACN by CID. The inner values are
/// the hash/eq identity (they distinguish sources), not read directly.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
enum SourceKey {
    ArtNet(SocketAddr),
    Sacn([u8; 16]),
}

/// One source's contribution to a single universe.
struct SourceBuf {
    data: [u8; 512],
    last_seen: Instant,
    priority: u8,
    last_seq: u8,
}

/// All sources contributing to one universe, plus the merged result.
struct UniverseMerge {
    per_source: HashMap<SourceKey, SourceBuf>,
    merged: [u8; 512],
    last_update: Instant,
}

impl UniverseMerge {
    fn new(now: Instant) -> Self {
        Self {
            per_source: HashMap::new(),
            merged: [0; 512],
            last_update: now,
        }
    }

    /// Drop idle sources and recompute the merged buffer per `policy`. Returns
    /// whether the set of contributing sources shrank (a release event).
    fn recompute(&mut self, policy: MergePolicy, now: Instant) -> bool {
        let before = self.per_source.len();
        self.per_source
            .retain(|_, b| now.duration_since(b.last_seen) < STALE);
        let mut merged = [0u8; 512];
        match policy {
            MergePolicy::Latest => {
                if let Some(b) = self.per_source.values().max_by_key(|b| b.last_seen) {
                    merged = b.data;
                }
            }
            MergePolicy::Htp => {
                for b in self.per_source.values() {
                    for (i, value) in merged.iter_mut().enumerate() {
                        *value = (*value).max(b.data[i]);
                    }
                }
            }
            MergePolicy::PriorityHtp => {
                let top = self
                    .per_source
                    .values()
                    .map(|b| b.priority)
                    .max()
                    .unwrap_or(0);
                for b in self.per_source.values().filter(|b| b.priority == top) {
                    for (i, value) in merged.iter_mut().enumerate() {
                        *value = (*value).max(b.data[i]);
                    }
                }
            }
        }
        self.merged = merged;
        self.per_source.len() != before
    }
}

/// Receive loop. Runs until `shared.stop` is set; never returns a value.
pub fn run_loop(shared: Arc<DmxShared>) {
    let mut cfg = shared.config.lock().unwrap().clone();
    let (mut art, mut sacn) = bind_sockets(&cfg);
    set_bound(&shared, art.is_some(), sacn.is_some());
    let mut joined: Vec<u16> = Vec::new();
    if let Some(s) = &sacn {
        joined = sync_multicast(s, &cfg, &joined);
    }

    let mut universes: HashMap<u16, UniverseMerge> = HashMap::new();
    let mut sources: HashMap<SourceKey, SourceStat> = HashMap::new();
    let mut buf = [0u8; 1500];
    let mut last_publish = Instant::now();
    let mut generation = 0_u64;
    let mut dirty = false;

    loop {
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }

        // Pick up live config edits (universe subscriptions, protocol/bind changes).
        let new_cfg = shared.config.lock().unwrap().clone();
        if new_cfg != cfg {
            let rebind = new_cfg.bind_ip != cfg.bind_ip
                || new_cfg.artnet != cfg.artnet
                || new_cfg.sacn != cfg.sacn;
            if rebind {
                (art, sacn) = bind_sockets(&new_cfg);
                set_bound(&shared, art.is_some(), sacn.is_some());
                joined.clear();
            }
            if let Some(s) = &sacn {
                joined = sync_multicast(s, &new_cfg, &joined);
            }
            cfg = new_cfg;
        }

        let now = Instant::now();

        // Drain Art-Net.
        if let Some(s) = &art {
            for _ in 0..DRAIN_CAP {
                match s.recv_from(&mut buf) {
                    Ok((n, addr)) => {
                        if handle_artnet(&buf[..n], addr, &cfg, &mut universes, &mut sources, now) {
                            dirty = true;
                        }
                    }
                    Err(_) => break, // WouldBlock / timeout
                }
            }
        }
        // Drain sACN.
        if let Some(s) = &sacn {
            for _ in 0..DRAIN_CAP {
                match s.recv_from(&mut buf) {
                    Ok((n, _addr)) => {
                        if handle_sacn(&buf[..n], &cfg, &mut universes, &mut sources, now) {
                            dirty = true;
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        // Release idle sources so a powered-off console drops its contribution.
        sources.retain(|_, s| now.duration_since(s.last_seen) < STALE);
        for um in universes.values_mut() {
            if um.recompute(cfg.merge, now) {
                dirty = true;
            }
        }

        if dirty && now.duration_since(last_publish) >= PUBLISH_INTERVAL {
            generation = generation.wrapping_add(1);
            publish(&shared, &universes, &sources, generation);
            last_publish = now;
            dirty = false;
        }

        // If neither socket is bound there is nothing to wait on — idle politely.
        if art.is_none() && sacn.is_none() {
            std::thread::sleep(READ_TIMEOUT);
        }
    }
}

fn handle_artnet(
    pkt: &[u8],
    addr: SocketAddr,
    cfg: &DmxConfig,
    universes: &mut HashMap<u16, UniverseMerge>,
    sources: &mut HashMap<SourceKey, SourceStat>,
    now: Instant,
) -> bool {
    let Some(d) = artnet::parse_artdmx(pkt) else {
        return false;
    };
    // Art-Net is broadcast — there is no per-universe subscription, so accept
    // every universe and let the grid/patch surface what's relevant. (sACN, which
    // is multicast, is still filtered by the joined groups.)
    let key = SourceKey::ArtNet(addr);
    let stat = sources
        .entry(key)
        .or_insert_with(|| SourceStat::new(Proto::ArtNet, addr.to_string(), String::new(), now));
    merge(
        universes,
        stat,
        key,
        d.universe,
        d.data,
        cfg.artnet_priority,
        d.sequence,
        cfg.merge,
        now,
    );
    true
}

fn handle_sacn(
    pkt: &[u8],
    cfg: &DmxConfig,
    universes: &mut HashMap<u16, UniverseMerge>,
    sources: &mut HashMap<SourceKey, SourceStat>,
    now: Instant,
) -> bool {
    let Some(SacnPacket::Data(d)) = sacn::parse_e131(pkt) else {
        // Sync packets and malformed datagrams are ignored in v1.
        return false;
    };
    if d.preview() || !subscribed(cfg, d.universe) {
        return false;
    }
    let key = SourceKey::Sacn(d.cid);
    if d.stream_terminated() {
        // Source is shutting this stream down — release it immediately.
        if let Some(um) = universes.get_mut(&d.universe) {
            um.per_source.remove(&key);
            um.recompute(cfg.merge, now);
        }
        return true;
    }
    let stat = sources.entry(key).or_insert_with(|| {
        SourceStat::new(Proto::Sacn, cid_label(&d.cid), d.source_name.clone(), now)
    });
    stat.name = d.source_name.clone();
    merge(
        universes, stat, key, d.universe, d.slots, d.priority, d.sequence, cfg.merge, now,
    );
    true
}

/// Update one source's stat + buffer for a universe and re-merge it.
#[allow(clippy::too_many_arguments)]
fn merge(
    universes: &mut HashMap<u16, UniverseMerge>,
    stat: &mut SourceStat,
    key: SourceKey,
    universe: u16,
    data: &[u8],
    priority: u8,
    seq: u8,
    policy: MergePolicy,
    now: Instant,
) {
    // FPS (EMA from inter-arrival).
    let dt = now.duration_since(stat.last_seen).as_secs_f32();
    if dt > 0.0 && dt < 5.0 {
        let inst = 1.0 / dt;
        stat.fps = if stat.fps == 0.0 {
            inst
        } else {
            stat.fps * 0.9 + inst * 0.1
        };
    }
    stat.last_seen = now;
    stat.priority = priority;
    stat.packets += 1;
    if let Err(pos) = stat.universes.binary_search(&universe) {
        stat.universes.insert(pos, universe);
    }

    let um = universes
        .entry(universe)
        .or_insert_with(|| UniverseMerge::new(now));
    let existed = um.per_source.contains_key(&key);
    let buf = um.per_source.entry(key).or_insert(SourceBuf {
        data: [0; 512],
        last_seen: now,
        priority,
        last_seq: seq,
    });
    // Sequence-gap loss (wrap-aware): a forward gap of 2..=20 means missed
    // packets; the 255->0 wrap is gap 1 (not a loss), and big/zero gaps are
    // reorders we ignore. Art-Net seq 0 means "disabled".
    if existed && seq != 0 {
        let gap = seq.wrapping_sub(buf.last_seq);
        if (2..=20).contains(&gap) {
            stat.seq_errors += (gap - 1) as u64;
        }
    }
    let n = data.len().min(512);
    buf.data = [0; 512];
    buf.data[..n].copy_from_slice(&data[..n]);
    buf.last_seen = now;
    buf.priority = priority;
    buf.last_seq = seq;
    um.last_update = now;
    um.recompute(policy, now);
}

/// Build a fresh snapshot + status and swap them into the shared handle.
fn publish(
    shared: &Arc<DmxShared>,
    universes: &HashMap<u16, UniverseMerge>,
    sources: &HashMap<SourceKey, SourceStat>,
    generation: u64,
) {
    let mut frames = HashMap::new();
    for (&u, um) in universes {
        if um.per_source.is_empty() {
            continue;
        }
        frames.insert(
            u,
            UniverseFrame {
                levels: um.merged,
                sources: um.per_source.len().min(255) as u8,
                last_update: um.last_update,
            },
        );
    }
    *shared.snapshot.lock().unwrap() = Arc::new(UniverseSnapshot { generation, frames });

    let mut stats: Vec<SourceStat> = sources.values().cloned().collect();
    stats.sort_by(|a, b| a.label.cmp(&b.label));
    shared.status.lock().unwrap().sources = stats;
}

/// Whether `universe` is in the subscription list (an empty list accepts all,
/// which matters for Art-Net; sACN still needs explicit multicast joins).
fn subscribed(cfg: &DmxConfig, universe: u16) -> bool {
    cfg.universes.is_empty() || cfg.universes.contains(&universe)
}

fn set_bound(shared: &Arc<DmxShared>, art: bool, sacn: bool) {
    let mut st = shared.status.lock().unwrap();
    st.bound_artnet = art;
    st.bound_sacn = sacn;
}

/// CID as a short hex label (first 6 bytes).
fn cid_label(cid: &[u8; 16]) -> String {
    cid[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

/// Bind the receive sockets per config. Isolated here so the std-vs-socket2
/// escape hatch (e.g. `SO_REUSEADDR`) stays a one-function change. Bind failures
/// are logged and yield `None` (the panel surfaces the unbound state).
fn bind_sockets(cfg: &DmxConfig) -> (Option<UdpSocket>, Option<UdpSocket>) {
    let art = cfg
        .artnet
        .then(|| bind_artnet(cfg))
        .and_then(report("Art-Net"));
    let sacn = cfg.sacn.then(bind_sacn).and_then(report("sACN"));
    (art, sacn)
}

fn report(what: &'static str) -> impl Fn(std::io::Result<UdpSocket>) -> Option<UdpSocket> {
    move |r| match r {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("DMX: {what} bind failed: {e}");
            None
        }
    }
}

fn bind_artnet(cfg: &DmxConfig) -> std::io::Result<UdpSocket> {
    reuse_bind(SocketAddr::new(cfg.bind_ip, ARTNET_PORT))
}

fn bind_sacn() -> std::io::Result<UdpSocket> {
    // Bind INADDR_ANY for reliable multicast receive across NICs.
    reuse_bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        SACN_PORT,
    ))
}

/// Bind a UDP receive socket with `SO_REUSEADDR` + `SO_REUSEPORT` set *before*
/// bind, so it can coexist with a console/node already holding the port on this
/// host (the std `UdpSocket::bind` can't set these pre-bind). Receive-only.
fn reuse_bind(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.bind(&addr.into())?;
    sock.set_read_timeout(Some(READ_TIMEOUT))?;
    Ok(sock.into())
}

/// Join/leave sACN multicast groups to match the subscription list. Returns the
/// new joined set.
fn sync_multicast(sock: &UdpSocket, cfg: &DmxConfig, already: &[u16]) -> Vec<u16> {
    let iface = match cfg.bind_ip {
        IpAddr::V4(v) => v,
        _ => Ipv4Addr::UNSPECIFIED,
    };
    for &u in already {
        if !cfg.universes.contains(&u) {
            let _ = sock.leave_multicast_v4(&sacn::multicast_addr(u), &iface);
        }
    }
    for &u in &cfg.universes {
        if !already.contains(&u)
            && let Err(e) = sock.join_multicast_v4(&sacn::multicast_addr(u), &iface)
        {
            log::warn!("DMX: sACN join universe {u} failed: {e}");
        }
    }
    cfg.universes.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_with(value: u8, priority: u8, now: Instant) -> SourceBuf {
        SourceBuf {
            data: [value; 512],
            last_seen: now,
            priority,
            last_seq: 0,
        }
    }

    #[test]
    fn htp_takes_per_channel_max() {
        let now = Instant::now();
        let mut um = UniverseMerge::new(now);
        um.per_source
            .insert(SourceKey::Sacn([1; 16]), buf_with(100, 100, now));
        um.per_source
            .insert(SourceKey::Sacn([2; 16]), buf_with(200, 100, now));
        um.recompute(MergePolicy::Htp, now);
        assert_eq!(um.merged[0], 200);
    }

    #[test]
    fn priority_htp_drops_lower_priority() {
        let now = Instant::now();
        let mut um = UniverseMerge::new(now);
        um.per_source
            .insert(SourceKey::Sacn([1; 16]), buf_with(255, 50, now)); // low prio
        um.per_source
            .insert(SourceKey::Sacn([2; 16]), buf_with(80, 200, now)); // high prio
        um.recompute(MergePolicy::PriorityHtp, now);
        assert_eq!(
            um.merged[0], 80,
            "only the highest-priority source contributes"
        );
    }

    #[test]
    fn latest_picks_most_recent() {
        let now = Instant::now();
        let older = now.checked_sub(Duration::from_millis(50)).unwrap();
        let mut um = UniverseMerge::new(now);
        um.per_source
            .insert(SourceKey::Sacn([1; 16]), buf_with(10, 100, older));
        um.per_source
            .insert(SourceKey::Sacn([2; 16]), buf_with(20, 100, now));
        um.recompute(MergePolicy::Latest, now);
        assert_eq!(um.merged[0], 20);
    }

    #[test]
    fn stale_source_is_evicted_and_releases_its_contribution() {
        let now = Instant::now();
        let old = now.checked_sub(Duration::from_secs(10)).unwrap();
        let mut um = UniverseMerge::new(now);
        um.per_source
            .insert(SourceKey::Sacn([1; 16]), buf_with(150, 100, old));
        let shrank = um.recompute(MergePolicy::Htp, now);
        assert!(shrank, "the stale source was evicted");
        assert_eq!(um.merged[0], 0, "its HTP contribution is released");
        assert!(um.per_source.is_empty());
    }

    #[test]
    fn seq_loss_counts_gaps_not_wrap() {
        let now = Instant::now();
        let mut universes = HashMap::new();
        let mut stat = SourceStat::new(Proto::Sacn, "x".into(), "x".into(), now);
        let key = SourceKey::Sacn([9; 16]);
        let data = [0u8; 512];
        // First packet seq=254 (no prior -> no loss).
        merge(
            &mut universes,
            &mut stat,
            key,
            1,
            &data,
            100,
            254,
            MergePolicy::Htp,
            now,
        );
        // 254 -> 255 (gap 1, ok).
        merge(
            &mut universes,
            &mut stat,
            key,
            1,
            &data,
            100,
            255,
            MergePolicy::Htp,
            now,
        );
        // 255 -> 0 (wrap, gap 1, NOT a loss).
        merge(
            &mut universes,
            &mut stat,
            key,
            1,
            &data,
            100,
            0,
            MergePolicy::Htp,
            now,
        );
        assert_eq!(stat.seq_errors, 0, "consecutive + wrap are not losses");
        // 0 -> 4 (gap 4 -> 3 missed).
        merge(
            &mut universes,
            &mut stat,
            key,
            1,
            &data,
            100,
            4,
            MergePolicy::Htp,
            now,
        );
        assert_eq!(stat.seq_errors, 3);
    }

    /// INPUT-ONLY guard: no transmit call may appear anywhere in `src/dmx`.
    #[test]
    fn input_only_no_transmit() {
        // Build the forbidden tokens without writing them literally, so this
        // guard never trips over its own source.
        let send = concat!(".", "send", "(");
        let send_to = concat!(".", "send", "_to(");
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/dmx");
        for entry in std::fs::read_dir(dir).expect("read src/dmx") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            for line in src.lines() {
                let l = line.trim_start();
                if l.starts_with("//") {
                    continue; // skip comments/docs
                }
                assert!(
                    !l.contains(send) && !l.contains(send_to),
                    "transmit call found in {path:?}: {line}"
                );
            }
        }
    }
}
