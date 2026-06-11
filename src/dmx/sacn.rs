//! Hand-rolled sACN / E1.31 parser (ANSI E1.31-2018).
//!
//! Byte-slice in, borrowed view out. We parse the Data packet (root vector
//! `0x4`) into [`E131`] and recognise the extended Synchronization packet (root
//! vector `0x8`) as [`SacnSync`] so the receive layer can see sync events even
//! though v1 does not yet implement the hold-and-promote state machine. INPUT-
//! ONLY: nothing here transmits or builds a discovery/universe-list reply.
//!
//! Layout (offsets): Root: preamble u16 BE @0 (=0x0010); ACN PID @4 (12 bytes);
//! root vector u32 BE @18; CID @22 (16). Framing (@38): vector u32 BE @40;
//! source name @44 (64); priority @108; sync addr u16 BE @109; sequence @111;
//! options @112; universe u16 BE @113. DMP (@115): vector u8 @117 (=0x02);
//! property-value-count u16 BE @123; start code @125; DMX slots @126.

use std::net::Ipv4Addr;

/// ACN packet identifier, "ASC-E1.17" + 3 NULs.
const ACN_PID: &[u8; 12] = b"ASC-E1.17\0\0\0";
const VECTOR_ROOT_DATA: u32 = 0x0000_0004;
const VECTOR_ROOT_EXTENDED: u32 = 0x0000_0008;
const VECTOR_DATA_PACKET: u32 = 0x0000_0002;
const VECTOR_EXTENDED_SYNC: u32 = 0x0000_0001;
const VECTOR_DMP_SET_PROPERTY: u8 = 0x02;

/// E1.31 framing options bits.
const OPT_PREVIEW: u8 = 0x80;
const OPT_STREAM_TERMINATED: u8 = 0x40;

/// A parsed sACN packet.
#[derive(Debug)]
#[allow(dead_code)] // Sync payload is recognised but not consumed in v1.
pub enum SacnPacket<'a> {
    /// A DMX data packet.
    Data(E131<'a>),
    /// A universe-synchronization packet (recognised; promote logic is TODO).
    Sync(SacnSync),
}

/// A parsed E1.31 **data** packet (borrows the DMX payload).
#[derive(Debug)]
pub struct E131<'a> {
    /// Sender's component identifier (UUID) — the stable source key.
    pub cid: [u8; 16],
    /// 1-based universe (E1.31 universes are natively 1-based).
    pub universe: u16,
    pub sequence: u8,
    /// Priority 0..200 (higher wins in a priority merge).
    pub priority: u8,
    pub options: u8,
    pub source_name: String,
    /// DMX channel levels with the start code already stripped (channel 1 =
    /// `slots[0]`). Only start-code-0 (dimmer) packets parse to `Some`.
    pub slots: &'a [u8],
}

impl E131<'_> {
    /// Whether the source is shutting this stream down (release immediately).
    pub fn stream_terminated(&self) -> bool {
        self.options & OPT_STREAM_TERMINATED != 0
    }
    /// Whether this is preview data (should not drive live output).
    pub fn preview(&self) -> bool {
        self.options & OPT_PREVIEW != 0
    }
}

/// A parsed E1.31 synchronization packet. Recognised so the receive layer can see
/// sync events; the hold-and-promote state machine is a v1 TODO, so the fields are
/// not consumed yet.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SacnSync {
    pub cid: [u8; 16],
    pub sequence: u8,
    pub sync_addr: u16,
}

/// The sACN multicast group for a 1-based `universe`: `239.255.{hi}.{lo}`.
pub fn multicast_addr(universe: u16) -> Ipv4Addr {
    Ipv4Addr::new(239, 255, (universe >> 8) as u8, (universe & 0xff) as u8)
}

/// Parse an sACN datagram, or `None` for any non-E1.31 / malformed packet.
pub fn parse_e131(buf: &[u8]) -> Option<SacnPacket<'_>> {
    if buf.len() < 38 {
        return None;
    }
    // Root layer.
    if u16::from_be_bytes([buf[0], buf[1]]) != 0x0010 || &buf[4..16] != ACN_PID {
        return None;
    }
    let root_vector = u32::from_be_bytes([buf[18], buf[19], buf[20], buf[21]]);
    let mut cid = [0u8; 16];
    cid.copy_from_slice(&buf[22..38]);

    match root_vector {
        VECTOR_ROOT_DATA => parse_data(buf, cid),
        VECTOR_ROOT_EXTENDED => parse_sync(buf, cid),
        _ => None,
    }
}

fn parse_data(buf: &[u8], cid: [u8; 16]) -> Option<SacnPacket<'_>> {
    // Need through the DMP start code at byte 125.
    if buf.len() < 126 {
        return None;
    }
    if u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) != VECTOR_DATA_PACKET {
        return None;
    }
    if buf[117] != VECTOR_DMP_SET_PROPERTY {
        return None;
    }
    // Property value count includes the 1-byte start code.
    let pvc = u16::from_be_bytes([buf[123], buf[124]]) as usize;
    if pvc == 0 || buf[125] != 0x00 {
        // Only start-code-0 (dimmer) data drives fixtures.
        return None;
    }
    let slots = buf.get(126..126 + (pvc - 1))?;
    if slots.len() > 512 {
        return None;
    }
    Some(SacnPacket::Data(E131 {
        cid,
        universe: u16::from_be_bytes([buf[113], buf[114]]),
        sequence: buf[111],
        priority: buf[108],
        options: buf[112],
        source_name: cstr_utf8(&buf[44..108]),
        slots,
    }))
}

fn parse_sync(buf: &[u8], cid: [u8; 16]) -> Option<SacnPacket<'_>> {
    // Extended framing: vector @40, sequence @44, sync address @45.
    if buf.len() < 47 {
        return None;
    }
    if u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) != VECTOR_EXTENDED_SYNC {
        return None;
    }
    Some(SacnPacket::Sync(SacnSync {
        cid,
        sequence: buf[44],
        sync_addr: u16::from_be_bytes([buf[45], buf[46]]),
    }))
}

/// Read a NUL-terminated UTF-8 string from a fixed field (lossy, trimmed at NUL).
fn cstr_utf8(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_packet(universe: u16, priority: u8, seq: u8, name: &str, slots: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 126];
        p[0..2].copy_from_slice(&0x0010u16.to_be_bytes());
        p[4..16].copy_from_slice(ACN_PID);
        p[18..22].copy_from_slice(&VECTOR_ROOT_DATA.to_be_bytes());
        for i in 0..16 {
            p[22 + i] = (i as u8) + 1; // CID = 1..=16
        }
        p[40..44].copy_from_slice(&VECTOR_DATA_PACKET.to_be_bytes());
        let nb = name.as_bytes();
        let n = nb.len().min(63);
        p[44..44 + n].copy_from_slice(&nb[..n]);
        p[108] = priority;
        p[111] = seq;
        p[113..115].copy_from_slice(&universe.to_be_bytes());
        p[117] = VECTOR_DMP_SET_PROPERTY;
        p[118] = 0xA1;
        p[121..123].copy_from_slice(&0x0001u16.to_be_bytes());
        p[123..125].copy_from_slice(&((slots.len() + 1) as u16).to_be_bytes());
        p[125] = 0x00; // start code
        p.extend_from_slice(slots);
        p
    }

    #[test]
    fn parses_valid_data_packet() {
        let pkt = data_packet(42, 150, 9, "Console A", &[5, 10, 255]);
        let SacnPacket::Data(d) = parse_e131(&pkt).expect("valid") else {
            panic!("expected data");
        };
        assert_eq!(d.universe, 42);
        assert_eq!(d.priority, 150);
        assert_eq!(d.sequence, 9);
        assert_eq!(d.source_name, "Console A");
        assert_eq!(d.cid, std::array::from_fn::<u8, 16, _>(|i| i as u8 + 1));
        assert_eq!(d.slots, &[5, 10, 255], "start code stripped");
        assert!(!d.preview() && !d.stream_terminated());
    }

    #[test]
    fn rejects_bad_pid_vector_and_start_code() {
        let mut bad_pid = data_packet(1, 100, 0, "x", &[1]);
        bad_pid[4] = b'Z';
        assert!(parse_e131(&bad_pid).is_none());

        let mut bad_root = data_packet(1, 100, 0, "x", &[1]);
        bad_root[18..22].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        assert!(parse_e131(&bad_root).is_none());

        let mut bad_framing = data_packet(1, 100, 0, "x", &[1]);
        bad_framing[40..44].copy_from_slice(&0u32.to_be_bytes());
        assert!(parse_e131(&bad_framing).is_none());

        let mut nonzero_sc = data_packet(1, 100, 0, "x", &[1]);
        nonzero_sc[125] = 0xDD; // a non-dimmer start code (e.g. RDM/per-channel priority)
        assert!(parse_e131(&nonzero_sc).is_none());

        assert!(parse_e131(&[0u8; 10]).is_none());
    }

    #[test]
    fn recognises_sync_packet() {
        let mut p = vec![0u8; 49];
        p[0..2].copy_from_slice(&0x0010u16.to_be_bytes());
        p[4..16].copy_from_slice(ACN_PID);
        p[18..22].copy_from_slice(&VECTOR_ROOT_EXTENDED.to_be_bytes());
        p[40..44].copy_from_slice(&VECTOR_EXTENDED_SYNC.to_be_bytes());
        p[44] = 7; // sequence
        p[45..47].copy_from_slice(&99u16.to_be_bytes()); // sync addr
        let SacnPacket::Sync(s) = parse_e131(&p).expect("valid") else {
            panic!("expected sync");
        };
        assert_eq!(s.sequence, 7);
        assert_eq!(s.sync_addr, 99);
    }

    #[test]
    fn multicast_groups() {
        assert_eq!(multicast_addr(1), Ipv4Addr::new(239, 255, 0, 1));
        assert_eq!(multicast_addr(0x0102), Ipv4Addr::new(239, 255, 1, 2));
        assert_eq!(multicast_addr(63999), Ipv4Addr::new(239, 255, 249, 255));
    }
}
