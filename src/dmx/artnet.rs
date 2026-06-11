//! Hand-rolled Art-Net `ArtDmx` parser (the only Art-Net packet we consume).
//!
//! Byte-slice in, borrowed view out — same house style as `gdtf.rs` / `mvr.rs`.
//! Other opcodes (`ArtPoll`, etc.) parse to `None` and are ignored; this module
//! never builds or sends a reply (INPUT-ONLY).
//!
//! Reference: Art-Net 4 protocol, ArtDmx (OpCode 0x5000). Header layout:
//! `Art-Net\0` (8) · OpCode u16 LE (2) · ProtVer u16 BE (2) · Sequence (1) ·
//! Physical (1) · SubUni (1) · Net (1) · Length u16 BE (2) · DMX data.

/// `Art-Net\0` packet identifier.
const ARTNET_ID: &[u8; 8] = b"Art-Net\0";
/// OpCode for an ArtDmx (DMX data) packet.
pub const OP_DMX: u16 = 0x5000;
/// OpCode for an ArtPoll — recognised so it can be explicitly ignored (never
/// answered: this is an input-only receiver).
#[allow(dead_code)]
pub const OP_POLL: u16 = 0x2000;

/// A parsed ArtDmx packet (borrows the datagram's DMX payload).
#[derive(Debug)]
pub struct ArtDmx<'a> {
    /// 1-based universe id: the Art-Net 15-bit Port-Address (0-based) **+ 1**, so
    /// an Art-Net "universe 0" lines up with sACN universe 1 in the patch.
    pub universe: u16,
    pub sequence: u8,
    /// DMX channel levels (channel 1 = `data[0]`). 1..=512 bytes.
    pub data: &'a [u8],
}

/// Parse an ArtDmx packet, or `None` for any non-ArtDmx / malformed datagram.
pub fn parse_artdmx(buf: &[u8]) -> Option<ArtDmx<'_>> {
    // 18-byte header before the DMX data.
    if buf.len() < 18 || &buf[0..8] != ARTNET_ID {
        return None;
    }
    // OpCode is little-endian on the wire.
    if u16::from_le_bytes([buf[8], buf[9]]) != OP_DMX {
        return None;
    }
    // Protocol version (big-endian); ArtDmx requires >= 14.
    if u16::from_be_bytes([buf[10], buf[11]]) < 14 {
        return None;
    }
    let sequence = buf[12];
    // buf[13] is the Physical port (informational; unused).
    // Port-Address = Net(7 bits) : SubNet(4) : Universe(4) = (Net << 8) | SubUni.
    let port = ((buf[15] as u16 & 0x7f) << 8) | buf[14] as u16;
    let length = u16::from_be_bytes([buf[16], buf[17]]) as usize;
    let data = buf.get(18..18 + length)?;
    if data.len() > 512 {
        return None;
    }
    Some(ArtDmx {
        universe: port + 1,
        sequence,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an ArtDmx datagram with the given 0-based port address.
    fn artdmx(port: u16, seq: u8, data: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(ARTNET_ID);
        p.extend_from_slice(&OP_DMX.to_le_bytes());
        p.extend_from_slice(&14u16.to_be_bytes());
        p.push(seq);
        p.push(0); // physical
        p.push((port & 0xff) as u8); // SubUni
        p.push(((port >> 8) & 0x7f) as u8); // Net
        p.extend_from_slice(&(data.len() as u16).to_be_bytes());
        p.extend_from_slice(data);
        p
    }

    #[test]
    fn parses_valid_artdmx() {
        let pkt = artdmx(0, 7, &[10, 20, 30]);
        let d = parse_artdmx(&pkt).expect("valid");
        assert_eq!(d.universe, 1, "port 0 -> universe 1");
        assert_eq!(d.sequence, 7);
        assert_eq!(d.data, &[10, 20, 30]);
    }

    #[test]
    fn assembles_net_and_subuni() {
        // Port 0x0102 = Net 1, SubUni 2.
        let pkt = artdmx(0x0102, 0, &[1, 2, 3, 4]);
        let d = parse_artdmx(&pkt).expect("valid");
        assert_eq!(d.universe, 0x0102 + 1);
    }

    #[test]
    fn rejects_bad_magic_opcode_and_truncation() {
        let mut bad_magic = artdmx(0, 0, &[1]);
        bad_magic[0] = b'X';
        assert!(parse_artdmx(&bad_magic).is_none());

        let mut poll = artdmx(0, 0, &[1]);
        poll[8..10].copy_from_slice(&OP_POLL.to_le_bytes());
        assert!(parse_artdmx(&poll).is_none(), "ArtPoll is not ArtDmx");

        assert!(parse_artdmx(&[]).is_none());
        assert!(parse_artdmx(&[0u8; 10]).is_none());

        // Length claims more data than present.
        let mut short = artdmx(0, 0, &[1, 2, 3]);
        short[16..18].copy_from_slice(&100u16.to_be_bytes());
        assert!(parse_artdmx(&short).is_none());
    }
}
