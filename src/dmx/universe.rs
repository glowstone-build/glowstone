//! Live DMX universe buffers and the immutable snapshot the network thread
//! publishes to the render thread.
//!
//! A [`UniverseSnapshot`] is plain `Clone` data: the receive thread builds a
//! fresh one when its merged buffers change and swaps it behind an `Arc` (a
//! hand-rolled arc-swap), and the render thread reads it with a pointer-clone —
//! so high packet rates never contend with the frame loop. Channel index 0 maps
//! to DMX channel 1.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// One received DMX universe: 512 channel levels plus liveness bookkeeping.
#[derive(Clone, Debug)]
pub struct UniverseFrame {
    /// Channel levels — `levels[0]` is channel 1, `levels[511]` is channel 512.
    pub levels: [u8; 512],
    /// Number of distinct sources currently contributing (after merge).
    pub sources: u8,
    /// When this universe last received a packet.
    pub last_update: Instant,
}

impl UniverseFrame {
    pub fn new() -> Self {
        Self {
            levels: [0; 512],
            sources: 0,
            last_update: Instant::now(),
        }
    }
}

impl Default for UniverseFrame {
    fn default() -> Self {
        Self::new()
    }
}

/// An immutable, point-in-time copy of every active universe, published by the
/// network thread and read (pointer-cloned) by the render thread each frame.
#[derive(Clone, Debug, Default)]
pub struct UniverseSnapshot {
    /// Active universes keyed by 1-based universe id.
    pub frames: HashMap<u16, UniverseFrame>,
}

impl UniverseSnapshot {
    /// Level of `channel` (1-based, `1..=512`) in `universe`, if present.
    pub fn level(&self, universe: u16, channel: u16) -> Option<u8> {
        if channel == 0 || channel > 512 {
            return None;
        }
        self.frames
            .get(&universe)
            .map(|f| f.levels[(channel - 1) as usize])
    }

    /// The raw 512-byte buffer for `universe`, if present.
    pub fn get(&self, universe: u16) -> Option<&[u8; 512]> {
        self.frames.get(&universe).map(|f| &f.levels)
    }

    /// Whether `universe` is present and was refreshed within `stale`.
    pub fn is_live(&self, universe: u16, stale: Duration) -> bool {
        self.frames
            .get(&universe)
            .is_some_and(|f| f.last_update.elapsed() < stale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a frame with a single channel set, aged `age` into the past.
    fn frame_with(channel: u16, value: u8, age: Duration) -> UniverseFrame {
        let mut f = UniverseFrame::new();
        f.levels[(channel - 1) as usize] = value;
        f.last_update = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        f
    }

    #[test]
    fn level_lookup_is_one_based_and_bounded() {
        let mut snap = UniverseSnapshot::default();
        snap.frames.insert(1, frame_with(1, 200, Duration::ZERO));
        assert_eq!(snap.level(1, 1), Some(200));
        assert_eq!(snap.level(1, 2), Some(0));
        // Out-of-range channels and absent universes are None, not a panic.
        assert_eq!(snap.level(1, 0), None);
        assert_eq!(snap.level(1, 513), None);
        assert_eq!(snap.level(2, 1), None);
    }

    #[test]
    fn liveness_respects_staleness() {
        let mut snap = UniverseSnapshot::default();
        snap.frames.insert(1, frame_with(1, 1, Duration::ZERO));
        snap.frames.insert(2, frame_with(1, 1, Duration::from_secs(10)));
        let stale = Duration::from_secs(3);
        assert!(snap.is_live(1, stale), "fresh universe is live");
        assert!(!snap.is_live(2, stale), "10s-old universe is stale");
        assert!(!snap.is_live(99, stale), "absent universe is not live");
    }
}
