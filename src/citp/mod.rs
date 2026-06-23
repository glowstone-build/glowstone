//! CITP (Controller Interface Transport Protocol) / MSEX support: receive live
//! video streams from media servers (Resolume, Capture, grandMA, …) onto LED
//! walls. Pure-Rust, no external runtime — discovery + the MSEX streaming
//! handshake are implemented here ([`proto`] is the wire codec, [`client`] the
//! discovery + per-source streaming threads). See `docs/RESEARCH-led-ndi.md`.

mod client;
pub mod proto;

pub use client::CitpClient;
