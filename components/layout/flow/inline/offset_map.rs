/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! DOM/glyph offset mapping from upstream Servo #46864, #47022, and #47025.
//! Kept independent of style/layout so its boundary arithmetic can be tested without a browser build.
use malloc_size_of_derive::MallocSizeOf;
use servo_base::text::Utf32CodeUnits;

#[derive(MallocSizeOf, Clone, Copy)]
struct OffsetMapKnownPosition {
    original_offset: Utf32CodeUnits,
    final_offset: Utf32CodeUnits,
}

#[derive(Default, MallocSizeOf)]
pub struct OffsetMap {
    /// Not including `IMPLICIT_KNOWN_POSITION_AT_START`
    known_positions: Vec<OffsetMapKnownPosition>,
    /// `Default` initializes to `false`
    last_range_maps_one_to_one: bool,
}

impl std::fmt::Debug for OffsetMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OffsetMap")
            .field("total_original_size", &self.total_original_size())
            .field("total_final_size", &self.total_final_size())
            .finish()
    }
}

static IMPLICIT_KNOWN_POSITION_AT_START: OffsetMapKnownPosition = OffsetMapKnownPosition {
    original_offset: Utf32CodeUnits(0),
    final_offset: Utf32CodeUnits(0),
};

impl OffsetMap {
    fn last_known_position(&self) -> &OffsetMapKnownPosition {
        self.known_positions
            .last()
            .unwrap_or(&IMPLICIT_KNOWN_POSITION_AT_START)
    }

    pub fn total_original_size(&self) -> Utf32CodeUnits {
        self.last_known_position().original_offset
    }

    pub fn total_final_size(&self) -> Utf32CodeUnits {
        self.last_known_position().final_offset
    }

    pub fn push_range(
        &mut self,
        additional_original_length: Utf32CodeUnits,
        additional_final_length: Utf32CodeUnits,
    ) {
        let this_range_maps_one_to_one = additional_original_length == additional_final_length;
        if this_range_maps_one_to_one
            && self.last_range_maps_one_to_one
            && let Some(last) = self.known_positions.last_mut()
        {
            last.original_offset += additional_original_length;
            last.final_offset += additional_final_length;
        } else {
            let last = self.last_known_position();
            self.known_positions.push(OffsetMapKnownPosition {
                original_offset: last.original_offset + additional_original_length,
                final_offset: last.final_offset + additional_final_length,
            });
        }
        self.last_range_maps_one_to_one = this_range_maps_one_to_one;
    }

    pub fn map(&self, target_original_offset: Utf32CodeUnits) -> Utf32CodeUnits {
        self.map_common(
            target_original_offset,
            |position| position.original_offset,
            |position| position.final_offset,
        )
    }

    /// Reverse mapping at a shared boundary can land after collapsed text in the next node.
    /// Clamp to the originating node before converting to its local offset (#47025/#47579).
    pub fn reverse_map_in_range(
        &self,
        offset: Utf32CodeUnits,
        node: std::ops::Range<Utf32CodeUnits>,
    ) -> Utf32CodeUnits {
        self.reverse_map(offset).clamp(node.start, node.end) - node.start
    }

    pub fn reverse_map(&self, target_final_offset: Utf32CodeUnits) -> Utf32CodeUnits {
        self.map_common(
            target_final_offset,
            |position| position.final_offset,
            |position| position.original_offset,
        )
    }

    fn map_common(
        &self,
        target_offset: Utf32CodeUnits,
        get_input_offset: impl Copy + Fn(&OffsetMapKnownPosition) -> Utf32CodeUnits,
        get_output_offset: impl Fn(&OffsetMapKnownPosition) -> Utf32CodeUnits,
    ) -> Utf32CodeUnits {
        if target_offset.0 == 0 {
            // Implict known position
            return Utf32CodeUnits(0);
        }
        match self
            .known_positions
            .binary_search_by_key(&target_offset, get_input_offset)
        {
            Ok(index) => {
                // Exact known position
                get_output_offset(&self.known_positions[index])
            },
            Err(index) => {
                // `index` is where inserting a new position would keep the `Vec` sorted
                if let Some(position_after) = self.known_positions.get(index) {
                    let position_before = if index > 0 {
                        &self.known_positions[index - 1]
                    } else {
                        &IMPLICIT_KNOWN_POSITION_AT_START
                    };
                    debug_assert!(target_offset > get_input_offset(position_before));
                    debug_assert!(target_offset < get_input_offset(position_after));
                    let offset_within_range = target_offset - get_input_offset(position_before);
                    let candidate = get_output_offset(position_before) + offset_within_range;
                    // If the output range is shorter, to go beyond it
                    let upper_bound = get_output_offset(position_after);
                    upper_bound.min(candidate)
                } else {
                    // `target_offset` at or past the end of the text covered by this map
                    get_output_offset(self.last_known_position())
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_ranges(ranges: &[(usize, usize)]) -> OffsetMap {
        let mut map = OffsetMap::default();
        for &(before, after) in ranges {
            map.push_range(Utf32CodeUnits(before), Utf32CodeUnits(after));
        }
        map
    }

    #[test]
    fn collapsed_whitespace_retains_dom_offsets() {
        // "  aaa  b \nc" -> "aaa b\nc"
        let map = map_ranges(&[(2, 0), (3, 3), (2, 1), (1, 1), (2, 1), (1, 1)]);
        assert_eq!(
            (0..=11)
                .map(|i| map.map(Utf32CodeUnits(i)).0)
                .collect::<Vec<_>>(),
            [0, 0, 0, 1, 2, 3, 4, 4, 5, 6, 6, 7]
        );
        assert_eq!(map.reverse_map(Utf32CodeUnits(4)).0, 7);
        assert_eq!(map.map(Utf32CodeUnits(100)).0, 7);
    }

    #[test]
    fn expanded_case_mapping_round_trips_boundaries() {
        // aßΰb -> ASSΫ́B
        let map = map_ranges(&[(1, 1), (1, 2), (1, 3), (1, 1)]);
        for (dom, glyph) in [(0, 0), (1, 1), (2, 3), (3, 6), (4, 7)] {
            assert_eq!(map.map(Utf32CodeUnits(dom)).0, glyph);
            assert_eq!(map.reverse_map(Utf32CodeUnits(glyph)).0, dom);
        }
        assert_eq!(map.reverse_map(Utf32CodeUnits(100)).0, 4);
    }

    #[test]
    fn adjacent_nodes_use_their_own_original_start() {
        let map = map_ranges(&[(3, 3), (2, 1), (3, 3)]);
        let second_node_start = Utf32CodeUnits(5);
        assert_eq!(
            (map.reverse_map(Utf32CodeUnits(5)) - second_node_start).0,
            1
        );
        assert_eq!(
            (map.reverse_map(Utf32CodeUnits(7)) - second_node_start).0,
            3
        );
    }

    #[test]
    fn empty_map_has_only_the_zero_boundary() {
        let map = OffsetMap::default();
        assert_eq!(map.map(Utf32CodeUnits(5)).0, 0);
        assert_eq!(map.reverse_map(Utf32CodeUnits(5)).0, 0);
    }
}

#[test]
fn shared_boundary_does_not_escape_into_next_nodes_collapsed_text() {
    let mut map = OffsetMap::default();
    map.push_range(Utf32CodeUnits(3), Utf32CodeUnits(3));
    map.push_range(Utf32CodeUnits(4), Utf32CodeUnits(0));
    map.push_range(Utf32CodeUnits(2), Utf32CodeUnits(2));
    assert_eq!(
        map.reverse_map_in_range(Utf32CodeUnits(3), Utf32CodeUnits(0)..Utf32CodeUnits(3))
            .0,
        3
    );
    assert_eq!(
        map.reverse_map_in_range(Utf32CodeUnits(4), Utf32CodeUnits(7)..Utf32CodeUnits(9))
            .0,
        1
    );
}
