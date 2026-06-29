//! A CITP/MSEX streaming client: a background thread discovers media servers on
//! the LAN (PINF/PLoc multicast), and per-source streaming threads open a TCP
//! connection, request a video stream, and decode incoming `StFr` frames into
//! [`ScreenFrame`]s that an LED wall displays.
//!
//! The protocol codec ([`super::proto`]) is unit-tested; this networking layer is
//! best-effort (it needs a live media server to exercise end-to-end, which isn't
//! available in CI). Everything degrades gracefully when no server is present.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::proto;
use crate::scene::screen::ScreenFrame;

/// A media server seen via PLoc multicast.
#[derive(Clone)]
struct Server {
    name: String,
    addr: SocketAddr,
    last_seen: Instant,
}

/// One active stream's shared state.
struct Stream {
    frame: Arc<Mutex<Option<Arc<ScreenFrame>>>>,
    stop: Arc<AtomicBool>,
}

struct Shared {
    servers: Mutex<Vec<Server>>,
}

/// The client handle held by the app. Cloneable internals; one discovery thread.
pub struct CitpClient {
    shared: Arc<Shared>,
    streams: HashMap<String, Stream>,
    discovery_stop: Arc<AtomicBool>,
}

impl CitpClient {
    /// Start the discovery thread (idempotent for the app to hold one).
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            servers: Mutex::new(Vec::new()),
        });
        let discovery_stop = Arc::new(AtomicBool::new(false));
        spawn_discovery(shared.clone(), discovery_stop.clone());
        Self {
            shared,
            streams: HashMap::new(),
            discovery_stop,
        }
    }

    /// Discovered media-server names (for the inspector source picker).
    pub fn server_names(&self) -> Vec<String> {
        let now = Instant::now();
        self.shared
            .servers
            .lock()
            .unwrap()
            .iter()
            .filter(|s| now.duration_since(s.last_seen) < Duration::from_secs(10))
            .map(|s| s.name.clone())
            .collect()
    }

    /// Ensure a stream is running for `source` (`"Server | layer"` or `"Server"`)
    /// and return its latest decoded frame, if any. Spawns the stream thread on
    /// first request for a source.
    pub fn frame_for(&mut self, source: &str) -> Option<Arc<ScreenFrame>> {
        if source.is_empty() {
            return None;
        }
        if !self.streams.contains_key(source) {
            let stream = spawn_stream(self.shared.clone(), source.to_string());
            self.streams.insert(source.to_string(), stream);
        }
        self.streams
            .get(source)
            .and_then(|s| s.frame.lock().unwrap().clone())
    }

    /// Stop any streams no longer referenced by `active` sources (called each
    /// frame with the set of CITP sources currently in the scene).
    pub fn retain(&mut self, active: &[String]) {
        self.streams.retain(|k, s| {
            let keep = active.iter().any(|a| a == k);
            if !keep {
                s.stop.store(true, Ordering::Relaxed);
            }
            keep
        });
    }
}

impl Drop for CitpClient {
    fn drop(&mut self) {
        self.discovery_stop.store(true, Ordering::Relaxed);
        for s in self.streams.values() {
            s.stop.store(true, Ordering::Relaxed);
        }
    }
}

impl Default for CitpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Bind a reusable UDP socket (so we can coexist with other CITP peers on 4809).
fn reuse_bind(addr: SocketAddr, timeout: Duration) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.bind(&addr.into())?;
    sock.set_read_timeout(Some(timeout))?;
    Ok(sock.into())
}

fn spawn_discovery(shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("citp-discovery".into())
        .spawn(move || {
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), proto::PORT);
            let sock = match reuse_bind(addr, Duration::from_millis(500)) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("CITP: discovery bind failed: {e}");
                    return;
                }
            };
            let iface = Ipv4Addr::UNSPECIFIED;
            for grp in [proto::MULTICAST_ADDR, proto::MULTICAST_ADDR_LEGACY] {
                let g = Ipv4Addr::from(grp);
                if let Err(e) = sock.join_multicast_v4(&g, &iface) {
                    log::debug!("CITP: join {g} failed: {e}");
                }
            }
            let mut buf = [0u8; 2048];
            while !stop.load(Ordering::Relaxed) {
                match sock.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        if let Some(p) = proto::parse_ploc(&buf[..n])
                            && p.kind == "MediaServer"
                            && p.listening_tcp_port != 0
                        {
                            let addr = SocketAddr::new(from.ip(), p.listening_tcp_port);
                            let mut servers = shared.servers.lock().unwrap();
                            let now = Instant::now();
                            if let Some(s) = servers.iter_mut().find(|s| s.name == p.name) {
                                s.addr = addr;
                                s.last_seen = now;
                            } else {
                                servers.push(Server {
                                    name: p.name,
                                    addr,
                                    last_seen: now,
                                });
                            }
                            // Prune long-stale entries.
                            servers.retain(|s| {
                                now.duration_since(s.last_seen) < Duration::from_secs(30)
                            });
                        }
                    }
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        log::debug!("CITP: discovery recv: {e}");
                    }
                }
            }
        })
        .ok();
}

/// Parse a `"Server | layer"` source string into (server name, source id).
fn parse_source(source: &str) -> (String, u16) {
    match source.split_once('|') {
        Some((server, layer)) => {
            let id = layer.trim().parse().unwrap_or(0);
            (server.trim().to_string(), id)
        }
        None => (source.trim().to_string(), 0),
    }
}

fn spawn_stream(shared: Arc<Shared>, source: String) -> Stream {
    let frame = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let (frame_t, stop_t) = (frame.clone(), stop.clone());
    let (server_name, source_id) = parse_source(&source);
    std::thread::Builder::new()
        .name("citp-stream".into())
        .spawn(move || {
            let mut generation = 0u64;
            while !stop_t.load(Ordering::Relaxed) {
                // Resolve the server address from discovery (wait until seen).
                let addr = shared
                    .servers
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|s| s.name == server_name)
                    .map(|s| s.addr);
                let Some(addr) = addr else {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                };
                if let Err(e) = stream_session(addr, source_id, &frame_t, &stop_t, &mut generation)
                {
                    log::debug!("CITP: stream {server_name} ended: {e}");
                }
                std::thread::sleep(Duration::from_millis(800)); // reconnect backoff
            }
        })
        .ok();
    Stream { frame, stop }
}

/// One TCP session: connect, handshake (CInf), request the stream, and decode
/// frames until disconnect or stop.
fn stream_session(
    addr: SocketAddr,
    source_id: u16,
    frame: &Arc<Mutex<Option<Arc<ScreenFrame>>>>,
    stop: &Arc<AtomicBool>,
    generation: &mut u64,
) -> std::io::Result<()> {
    let mut tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    tcp.set_read_timeout(Some(Duration::from_millis(500)))?;
    tcp.set_nodelay(true).ok();
    tcp.write_all(&proto::build_cinf())?;
    let rqst = proto::build_rqst(source_id, proto::FMT_RGB8, 0, 0, 30, 5);
    tcp.write_all(&rqst)?;
    let mut last_request = Instant::now();

    // Streaming read loop with a resync-capable framing buffer.
    let mut acc: Vec<u8> = Vec::with_capacity(1 << 16);
    let mut rbuf = [0u8; 1 << 15];
    // Fragment reassembly: frame_index -> (per-index seen flags, partial buffer).
    let mut frags: HashMap<u32, (Vec<bool>, Vec<u8>)> = HashMap::new();

    while !stop.load(Ordering::Relaxed) {
        // Re-send RqSt before the 5s timeout lapses.
        if last_request.elapsed() > Duration::from_secs(3) {
            tcp.write_all(&rqst)?;
            last_request = Instant::now();
        }
        match tcp.read(&mut rbuf) {
            Ok(0) => return Ok(()), // peer closed
            Ok(n) => acc.extend_from_slice(&rbuf[..n]),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => return Err(e),
        }
        // Drain whole CITP messages out of `acc`.
        loop {
            if acc.len() < proto::HEADER_LEN {
                break;
            }
            // Resync to the next "CITP" cookie if we're misaligned.
            if &acc[0..4] != proto::CITP_COOKIE {
                if let Some(pos) = acc.windows(4).position(|w| w == proto::CITP_COOKIE) {
                    acc.drain(0..pos);
                    if acc.len() < proto::HEADER_LEN {
                        break;
                    }
                } else {
                    acc.clear();
                    break;
                }
            }
            let Some(h) = proto::parse_header(&acc) else {
                break;
            };
            let msg_size = h.message_size as usize;
            if !(proto::HEADER_LEN..=(8 << 20)).contains(&msg_size) {
                // Corrupt size — drop one byte and resync.
                acc.drain(0..1);
                continue;
            }
            if acc.len() < msg_size {
                break; // wait for the rest
            }
            let decoded = if &h.content_type == proto::MSEX {
                let msg = &acc[..msg_size];
                proto::parse_stfr(msg).and_then(|sf| {
                    if sf.source_id != source_id {
                        return None;
                    }
                    // Reassemble fragments if needed, else decode directly.
                    if let Some(fr) = &sf.fragment {
                        let count = fr.count.max(1) as usize;
                        let idx = fr.index as usize;
                        let end = fr.byte_offset as usize + sf.buffer.len();
                        // Reject implausible offsets/indices to avoid OOM from crafted
                        // fragments (byte_offset is unvalidated off the wire).
                        let cap = ((sf.width as usize) * (sf.height as usize) * 4).max(16 << 20);
                        if idx >= count || end > cap {
                            None
                        } else {
                            // Bound the number of in-flight partial frames (drop orphans
                            // from incomplete/abandoned frames so the map can't grow).
                            if frags.len() > 8 && !frags.contains_key(&fr.frame_index) {
                                frags.clear();
                            }
                            let entry = frags
                                .entry(fr.frame_index)
                                .or_insert_with(|| (vec![false; count], Vec::new()));
                            if entry.1.len() < end {
                                entry.1.resize(end, 0);
                            }
                            entry.1[fr.byte_offset as usize..end].copy_from_slice(&sf.buffer);
                            // Count distinct fragments only (ignore duplicates/retransmits).
                            let mut complete = false;
                            if !entry.0.get(idx).copied().unwrap_or(true) {
                                entry.0[idx] = true;
                                complete = entry.0.iter().all(|&b| b);
                            }
                            if complete {
                                let (_, full) = frags.remove(&fr.frame_index).unwrap();
                                proto::image_to_rgba(
                                    &sf.format,
                                    sf.width,
                                    sf.height,
                                    &full,
                                    sf.msex_minor,
                                )
                            } else {
                                None
                            }
                        }
                    } else {
                        proto::image_to_rgba(
                            &sf.format,
                            sf.width,
                            sf.height,
                            &sf.buffer,
                            sf.msex_minor,
                        )
                    }
                })
            } else {
                None
            };
            acc.drain(0..msg_size);
            if let Some((w, hgt, rgba)) = decoded {
                *generation = generation.wrapping_add(1);
                *frame.lock().unwrap() = Some(Arc::new(ScreenFrame {
                    width: w,
                    height: hgt,
                    rgba,
                    generation: *generation,
                }));
            }
        }
    }
    Ok(())
}
