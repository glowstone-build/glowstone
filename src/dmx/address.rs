//! Shared DMX universe/address math.
//!
//! DMX addresses are user-facing 1-based `(universe, address)` pairs, with 512
//! channels per universe. Internally, span math is easier and less error-prone in
//! an absolute 0-based channel space.

pub const CHANNELS_PER_UNIVERSE: u32 = 512;

/// Convert a 1-based universe/address pair into an absolute 0-based DMX slot.
pub fn slot_abs(universe: u16, address: u16) -> u32 {
    let u0 = universe.max(1) as u32 - 1;
    let a0 = address.clamp(1, CHANNELS_PER_UNIVERSE as u16) as u32 - 1;
    u0.saturating_mul(CHANNELS_PER_UNIVERSE).saturating_add(a0)
}

/// Convert an absolute 0-based DMX slot into a 1-based universe/address pair.
pub fn abs_to_slot(abs: u32) -> (u16, u16) {
    let universe = (abs / CHANNELS_PER_UNIVERSE + 1).min(u16::MAX as u32) as u16;
    let address = (abs % CHANNELS_PER_UNIVERSE + 1) as u16;
    (universe, address)
}

pub fn push_unique_universe(universes: &mut Vec<u16>, universe: u16) {
    if !universes.contains(&universe) {
        universes.push(universe);
    }
}

/// Push every universe touched by a patched span.
pub fn push_span_universes(universes: &mut Vec<u16>, universe: u16, address: u16, footprint: u16) {
    let start = slot_abs(universe, address);
    let end = start.saturating_add(footprint.max(1) as u32 - 1);
    let first = abs_to_slot(start).0;
    let last = abs_to_slot(end).0;
    for u in first..=last {
        push_unique_universe(universes, u);
    }
}

/// Whether a patched span overlaps any channel in `selected_universe`.
pub fn span_intersects_universe(
    universe: u16,
    address: u16,
    footprint: u16,
    selected_universe: u16,
) -> bool {
    let start = slot_abs(universe, address);
    let end = start.saturating_add(footprint.max(1) as u32);
    let universe_start = slot_abs(selected_universe, 1);
    let universe_end = universe_start.saturating_add(CHANNELS_PER_UNIVERSE);
    start < universe_end && end > universe_start
}

/// 0-based channel cells within `selected_universe` occupied by a sub-span.
pub fn span_slots_in_universe(
    base_universe: u16,
    base_address: u16,
    offset: u32,
    width: u16,
    selected_universe: u16,
) -> Vec<usize> {
    let start = slot_abs(base_universe, base_address).saturating_add(offset);
    let end = start.saturating_add(width.max(1) as u32);
    let universe_start = slot_abs(selected_universe, 1);
    let universe_end = universe_start.saturating_add(CHANNELS_PER_UNIVERSE);
    (start.max(universe_start)..end.min(universe_end))
        .map(|abs| (abs - universe_start) as usize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_slot_round_trip() {
        for (universe, address) in [(1, 1), (1, 512), (2, 1), (7, 42)] {
            assert_eq!(
                abs_to_slot(slot_abs(universe, address)),
                (universe, address)
            );
        }
    }

    #[test]
    fn span_crossing_boundary_touches_both_universes() {
        let mut universes = Vec::new();
        push_span_universes(&mut universes, 1, 511, 4);
        assert_eq!(universes, vec![1, 2]);
        assert!(span_intersects_universe(1, 511, 4, 1));
        assert!(span_intersects_universe(1, 511, 4, 2));
        assert!(!span_intersects_universe(1, 511, 4, 3));
    }

    #[test]
    fn sub_span_slots_are_relative_to_selected_universe() {
        assert_eq!(span_slots_in_universe(1, 511, 1, 3, 2), vec![0, 1]);
        assert_eq!(span_slots_in_universe(1, 511, 1, 3, 1), vec![511]);
    }
}
