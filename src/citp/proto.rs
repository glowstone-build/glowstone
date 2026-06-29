//! CITP (Controller Interface Transport Protocol) wire format — the subset a
//! pure-Rust **MSEX streaming client** needs to pull video frames off a media
//! server: the 20-byte base header, PINF/PLoc discovery, and the MSEX
//! `CInf` / `RqSt` / `StFr` messages. Everything is little-endian, 1-byte
//! packed. See `docs/RESEARCH-led-ndi.md` (CITP section) for the byte tables.

pub const CITP_COOKIE: &[u8; 4] = b"CITP";
pub const PINF: &[u8; 4] = b"PINF";
pub const MSEX: &[u8; 4] = b"MSEX";
pub const PLOC: &[u8; 4] = b"PLoc";
pub const CINF: &[u8; 4] = b"CInf";
pub const RQST: &[u8; 4] = b"RqSt";
pub const STFR: &[u8; 4] = b"StFr";

/// Current CITP multicast discovery group + port (since 2014).
pub const MULTICAST_ADDR: [u8; 4] = [239, 224, 0, 180];
/// Legacy pre-2014 group (clients should also listen here).
pub const MULTICAST_ADDR_LEGACY: [u8; 4] = [224, 0, 0, 180];
pub const PORT: u16 = 4809;

pub const HEADER_LEN: usize = 20;

// Image-format FourCCs.
pub const FMT_RGB8: &[u8; 4] = b"RGB8";
pub const FMT_JPEG: &[u8; 4] = b"JPEG";
pub const FMT_PNG: &[u8; 4] = b"PNG ";
pub const FMT_FJPG: &[u8; 4] = b"fJPG";
pub const FMT_FPNG: &[u8; 4] = b"fPNG";

// ---------------------------------------------------------------------------
// A minimal little-endian cursor reader.
// ---------------------------------------------------------------------------

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.remaining() >= n {
            let s = &self.b[self.pos..self.pos + n];
            self.pos += n;
            Some(s)
        } else {
            None
        }
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn fourcc(&mut self) -> Option<[u8; 4]> {
        self.take(4).map(|s| [s[0], s[1], s[2], s[3]])
    }
    /// A null-terminated ASCII string.
    fn cstring(&mut self) -> Option<String> {
        let start = self.pos;
        while self.pos < self.b.len() && self.b[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.b.len() {
            return None;
        }
        let s = String::from_utf8_lossy(&self.b[start..self.pos]).into_owned();
        self.pos += 1; // skip the nul
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// Base header
// ---------------------------------------------------------------------------

/// The fields of a CITP base header a client needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub content_type: [u8; 4],
    pub message_size: u32,
}

/// Parse the 20-byte base header. Returns the header + the payload offset (= 20).
pub fn parse_header(b: &[u8]) -> Option<Header> {
    let mut r = Reader::new(b);
    if r.take(4)? != CITP_COOKIE {
        return None;
    }
    let _vmaj = r.u8()?;
    let _vmin = r.u8()?;
    let _request_index = r.u16()?;
    let message_size = r.u32()?;
    let _part_count = r.u16()?;
    let _part = r.u16()?;
    let content_type = r.fourcc()?;
    Some(Header {
        content_type,
        message_size,
    })
}

fn write_header(out: &mut Vec<u8>, content_type: &[u8; 4], total_size: u32) {
    out.extend_from_slice(CITP_COOKIE);
    out.push(1); // version major
    out.push(0); // version minor
    out.extend_from_slice(&0u16.to_le_bytes()); // request/response index
    out.extend_from_slice(&total_size.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // part count (1 over TCP/UDP)
    out.extend_from_slice(&0u16.to_le_bytes()); // part index
    out.extend_from_slice(content_type);
}

/// Wrap a fully-built MSEX body (`[vmaj, vmin, msex_cookie, payload…]`) in a base
/// header with `ContentType = "MSEX"`.
fn finish_msex(body: Vec<u8>) -> Vec<u8> {
    let total = (HEADER_LEN + body.len()) as u32;
    let mut out = Vec::with_capacity(total as usize);
    write_header(&mut out, MSEX, total);
    out.extend_from_slice(&body);
    out
}

// ---------------------------------------------------------------------------
// PINF / PLoc — discovery
// ---------------------------------------------------------------------------

/// A discovered CITP peer (from a PLoc multicast announce).
#[derive(Clone, Debug, PartialEq)]
pub struct PeerLocation {
    pub listening_tcp_port: u16,
    pub kind: String, // "MediaServer" / "LightingConsole" / "Visualiser"
    pub name: String,
    pub state: String,
}

/// Parse a PINF/PLoc message (the whole datagram, including the base header).
pub fn parse_ploc(b: &[u8]) -> Option<PeerLocation> {
    let h = parse_header(b)?;
    if &h.content_type != PINF {
        return None;
    }
    let mut r = Reader::new(b);
    let _ = r.take(HEADER_LEN)?;
    // The PINF message cookie follows the base header.
    let cookie = r.fourcc()?;
    if &cookie != PLOC {
        return None;
    }
    let listening_tcp_port = r.u16()?;
    let kind = r.cstring()?;
    let name = r.cstring()?;
    let state = r.cstring().unwrap_or_default();
    Some(PeerLocation {
        listening_tcp_port,
        kind,
        name,
        state,
    })
}

/// Build a PLoc announce for THIS peer (so servers see us as a Visualiser).
/// (Available for active announce; discovery works passively without it.)
#[allow(dead_code)]
pub fn build_ploc(listening_tcp_port: u16, name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(PLOC);
    body.extend_from_slice(&listening_tcp_port.to_le_bytes());
    body.extend_from_slice(b"Visualiser\0");
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body.extend_from_slice(b"Idle\0");
    let total = (HEADER_LEN + body.len()) as u32;
    let mut out = Vec::with_capacity(total as usize);
    write_header(&mut out, PINF, total);
    out.extend_from_slice(&body);
    out
}

// ---------------------------------------------------------------------------
// MSEX CInf / RqSt (client → server)
// ---------------------------------------------------------------------------

/// Build a Client Information message advertising MSEX 1.0/1.1/1.2 support.
/// Sent immediately after the TCP connect.
pub fn build_cinf() -> Vec<u8> {
    let mut body = Vec::new();
    body.push(1); // MSEX version major (sub-header)
    body.push(2); // MSEX version minor (advertise our highest)
    body.extend_from_slice(CINF);
    // Supported versions: count, then (major, minor) byte pairs.
    let versions: [(u8, u8); 3] = [(1, 0), (1, 1), (1, 2)];
    body.push(versions.len() as u8);
    for (maj, min) in versions {
        body.push(maj);
        body.push(min);
    }
    finish_msex(body)
}

/// Build a Request Stream message asking `source_id` for `format` frames at the
/// given size/fps. `timeout_s` is how long the request stays live (re-send
/// before it expires); 0 = a single frame.
pub fn build_rqst(
    source_id: u16,
    format: &[u8; 4],
    width: u16,
    height: u16,
    fps: u8,
    timeout_s: u8,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(1); // MSEX version major
    body.push(0); // MSEX version minor (RqSt is unchanged across 1.0–1.2)
    body.extend_from_slice(RQST);
    body.extend_from_slice(&source_id.to_le_bytes());
    body.extend_from_slice(format);
    body.extend_from_slice(&width.to_le_bytes());
    body.extend_from_slice(&height.to_le_bytes());
    body.push(fps);
    body.push(timeout_s);
    finish_msex(body)
}

// ---------------------------------------------------------------------------
// MSEX StFr (server → client) — the frame carrier
// ---------------------------------------------------------------------------

/// One fragment preamble (present only for `fJPG`/`fPNG`).
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub frame_index: u32,
    pub count: u16,
    pub index: u16,
    pub byte_offset: u32,
}

/// A parsed Stream Frame: the source/format/size plus the raw (or fragment)
/// image buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamFrame {
    pub source_id: u16,
    pub format: [u8; 4],
    pub width: u16,
    pub height: u16,
    /// The MSEX minor version this frame was sent at (0 = RGB8 is BGR; 1+ = RGB).
    pub msex_minor: u8,
    pub uuid: Option<String>,
    pub fragment: Option<Fragment>,
    pub buffer: Vec<u8>,
}

/// Parse a complete MSEX message that is a StreamFrame (`b` includes the base
/// header). The UUID prefix + fragment preamble depend on the MSEX minor version
/// carried in the message's own sub-header.
pub fn parse_stfr(b: &[u8]) -> Option<StreamFrame> {
    let h = parse_header(b)?;
    if &h.content_type != MSEX {
        return None;
    }
    let mut r = Reader::new(b);
    let _ = r.take(HEADER_LEN)?;
    let _vmaj = r.u8()?;
    let vmin = r.u8()?;
    let cookie = r.fourcc()?;
    if &cookie != STFR {
        return None;
    }
    let uuid = if vmin >= 2 {
        let raw = r.take(36)?;
        Some(String::from_utf8_lossy(raw).into_owned())
    } else {
        None
    };
    let source_id = r.u16()?;
    let format = r.fourcc()?;
    let width = r.u16()?;
    let height = r.u16()?;
    let buffer_size = r.u16()? as usize;
    let fragmented = vmin >= 2 && (&format == FMT_FJPG || &format == FMT_FPNG);
    let fragment = if fragmented {
        Some(Fragment {
            frame_index: r.u32()?,
            count: r.u16()?,
            index: r.u16()?,
            byte_offset: r.u32()?,
        })
    } else {
        None
    };
    // For fragmented formats `buffer_size` includes the 12-byte preamble.
    let img_len = if fragmented {
        buffer_size.saturating_sub(12)
    } else {
        buffer_size
    };
    let buffer = r.take(img_len.min(r.remaining()))?.to_vec();
    Some(StreamFrame {
        source_id,
        format,
        width,
        height,
        msex_minor: vmin,
        uuid,
        fragment,
        buffer,
    })
}

/// Decode a *complete* (non-fragmented, or already-reassembled) frame image into
/// tightly-packed RGBA8. `msex_minor` selects the RGB8 channel order (BGR in 1.0,
/// RGB from 1.1+). Returns `(width, height, rgba)`.
pub fn image_to_rgba(
    format: &[u8; 4],
    width: u16,
    height: u16,
    buffer: &[u8],
    msex_minor: u8,
) -> Option<(u32, u32, Vec<u8>)> {
    if format == FMT_RGB8 {
        let (w, h) = (width as usize, height as usize);
        let need = w * h * 3;
        if buffer.len() < need || w == 0 || h == 0 {
            return None;
        }
        let bgr = msex_minor == 0; // 1.0 = BGR, 1.1+ = RGB
        let mut rgba = vec![0u8; w * h * 4];
        for i in 0..(w * h) {
            let s = i * 3;
            let d = i * 4;
            let (r, g, b) = if bgr {
                (buffer[s + 2], buffer[s + 1], buffer[s])
            } else {
                (buffer[s], buffer[s + 1], buffer[s + 2])
            };
            rgba[d] = r;
            rgba[d + 1] = g;
            rgba[d + 2] = b;
            rgba[d + 3] = 255;
        }
        return Some((w as u32, h as u32, rgba));
    }
    // JPEG / PNG / (reassembled) fJPG / fPNG — decode with the image crate.
    if format == FMT_JPEG || format == FMT_PNG || format == FMT_FJPG || format == FMT_FPNG {
        let img = image::load_from_memory(buffer).ok()?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        return Some((w, h, rgba.into_raw()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrips() {
        let mut out = Vec::new();
        write_header(&mut out, MSEX, 20);
        assert_eq!(out.len(), HEADER_LEN);
        let h = parse_header(&out).expect("parse");
        assert_eq!(&h.content_type, MSEX);
        assert_eq!(h.message_size, 20);
        // Wrong cookie → None.
        let mut bad = out.clone();
        bad[0] = b'X';
        assert!(parse_header(&bad).is_none());
    }

    #[test]
    fn ploc_parses_server_and_port() {
        // Build a PLoc for a MediaServer, then parse it.
        let mut body = Vec::new();
        body.extend_from_slice(PLOC);
        body.extend_from_slice(&6436u16.to_le_bytes()); // listening TCP port
        body.extend_from_slice(b"MediaServer\0");
        body.extend_from_slice(b"Resolume Arena\0");
        body.extend_from_slice(b"Running\0");
        let total = (HEADER_LEN + body.len()) as u32;
        let mut msg = Vec::new();
        write_header(&mut msg, PINF, total);
        msg.extend_from_slice(&body);

        let p = parse_ploc(&msg).expect("ploc");
        assert_eq!(p.listening_tcp_port, 6436);
        assert_eq!(p.kind, "MediaServer");
        assert_eq!(p.name, "Resolume Arena");
        assert_eq!(p.state, "Running");
    }

    #[test]
    fn rqst_layout() {
        let m = build_rqst(3, FMT_RGB8, 1920, 1080, 30, 5);
        let h = parse_header(&m).expect("hdr");
        assert_eq!(&h.content_type, MSEX);
        assert_eq!(h.message_size as usize, m.len());
        // sub-header: vmaj, vmin, "RqSt"
        assert_eq!(&m[22..26], RQST);
        // payload at 26: source_id(2) format(4) w(2) h(2) fps(1) timeout(1)
        assert_eq!(u16::from_le_bytes([m[26], m[27]]), 3);
        assert_eq!(&m[28..32], FMT_RGB8);
        assert_eq!(u16::from_le_bytes([m[32], m[33]]), 1920);
        assert_eq!(u16::from_le_bytes([m[34], m[35]]), 1080);
        assert_eq!(m[36], 30);
        assert_eq!(m[37], 5);
        assert_eq!(m.len(), 38);
    }

    fn build_stfr(
        minor: u8,
        format: &[u8; 4],
        w: u16,
        h: u16,
        uuid: Option<&str>,
        buf: &[u8],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(1);
        body.push(minor);
        body.extend_from_slice(STFR);
        if minor >= 2 {
            let u = uuid.unwrap_or("");
            let mut padded = [b' '; 36];
            let bytes = u.as_bytes();
            padded[..bytes.len().min(36)].copy_from_slice(&bytes[..bytes.len().min(36)]);
            body.extend_from_slice(&padded);
        }
        body.extend_from_slice(&7u16.to_le_bytes()); // source id
        body.extend_from_slice(format);
        body.extend_from_slice(&w.to_le_bytes());
        body.extend_from_slice(&h.to_le_bytes());
        body.extend_from_slice(&(buf.len() as u16).to_le_bytes());
        body.extend_from_slice(buf);
        finish_msex(body)
    }

    #[test]
    fn stfr_rgb8_v11_parses_and_decodes_rgb() {
        // 2×1 RGB8 image: pixel0 = (10,20,30), pixel1 = (40,50,60).
        let img = [10u8, 20, 30, 40, 50, 60];
        let msg = build_stfr(1, FMT_RGB8, 2, 1, None, &img);
        let sf = parse_stfr(&msg).expect("stfr");
        assert_eq!(sf.source_id, 7);
        assert_eq!(&sf.format, FMT_RGB8);
        assert_eq!((sf.width, sf.height), (2, 1));
        assert!(sf.uuid.is_none());
        assert!(sf.fragment.is_none());
        assert_eq!(sf.msex_minor, 1, "version carried on the frame");
        let (w, h, rgba) =
            image_to_rgba(&sf.format, sf.width, sf.height, &sf.buffer, sf.msex_minor)
                .expect("rgba");
        assert_eq!((w, h), (2, 1));
        // 1.1 = RGB order, so pixels pass straight through, alpha forced 255.
        assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
        assert_eq!(&rgba[4..8], &[40, 50, 60, 255]);
    }

    #[test]
    fn stfr_rgb8_v10_is_bgr() {
        let img = [10u8, 20, 30]; // one pixel, stored BGR in 1.0
        let msg = build_stfr(0, FMT_RGB8, 1, 1, None, &img);
        let sf = parse_stfr(&msg).expect("stfr");
        assert_eq!(sf.msex_minor, 0, "v1.0 carried on the frame");
        // Decode through the frame's own version (the live path) — must be BGR.
        let (_, _, rgba) =
            image_to_rgba(&sf.format, sf.width, sf.height, &sf.buffer, sf.msex_minor)
                .expect("rgba");
        // BGR (10,20,30) → RGB (30,20,10).
        assert_eq!(&rgba[0..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn stfr_v12_has_uuid_prefix() {
        let img = [1u8, 2, 3];
        let msg = build_stfr(2, FMT_RGB8, 1, 1, Some("abc-uuid"), &img);
        let sf = parse_stfr(&msg).expect("stfr");
        assert!(sf.uuid.is_some());
        assert_eq!(sf.uuid.as_deref().unwrap().trim(), "abc-uuid");
        assert_eq!((sf.width, sf.height), (1, 1));
        assert_eq!(sf.buffer, img);
    }
}
