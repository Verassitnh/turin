//! Port of upstream/packages/client-runtime/src/work-log/scrollAnchor.ts (+ test).
//!
//! The save side of a tool group's scroll anchoring. The restore side is
//! `messages_timeline_logic::resolve_work_group_scroll_index`.

use super::messages_timeline_logic::WorkGroupScrollAnchor;

/// What `resolveWorkGroupScrollAnchor` returns.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedWorkGroupScrollAnchor {
    pub row_id: String,
    pub offset_within_row: f64,
    pub scroll_offset: f64,
}

impl ResolvedWorkGroupScrollAnchor {
    /// The anchor shape the restore side reads: the row's id and how far into
    /// the row the scroll offset sits.
    pub fn into_anchor(self) -> WorkGroupScrollAnchor {
        WorkGroupScrollAnchor {
            entry_id: self.row_id,
            offset: self.offset_within_row,
        }
    }
}

/// Save the row at the current offset; a virtualizer's cached visible range can lag a fling.
///
/// `row_ids` is `state.data` (each row's `id`), `scroll` the current offset and
/// `position_at_index` the measured top of a row, `None` while it is unmeasured.
pub fn resolve_work_group_scroll_anchor<S: AsRef<str>>(
    row_ids: &[S],
    scroll: f64,
    position_at_index: impl Fn(usize) -> Option<f64>,
) -> Option<ResolvedWorkGroupScrollAnchor> {
    if row_ids.is_empty() || !scroll.is_finite() {
        return None;
    }
    let scroll_offset = scroll.max(0.0);
    let mut low: isize = 0;
    let mut high = row_ids.len() as isize - 1;
    let mut index = 0usize;
    let mut row_top = position_at_index(0).filter(|top| top.is_finite())?;

    while low <= high {
        let middle = (low + high) / 2;
        let top = position_at_index(middle as usize).filter(|top| top.is_finite())?;
        if top <= scroll_offset {
            index = middle as usize;
            row_top = top;
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }

    Some(ResolvedWorkGroupScrollAnchor {
        row_id: row_ids[index].as_ref().to_owned(),
        offset_within_row: (scroll_offset - row_top).max(0.0),
        scroll_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<String> {
        (0..18).map(|index| format!("call-{index}")).collect()
    }

    fn position(index: usize) -> Option<f64> {
        Some(index as f64 * 33.0)
    }

    fn resolved(
        row_id: &str,
        offset_within_row: f64,
        scroll_offset: f64,
    ) -> ResolvedWorkGroupScrollAnchor {
        ResolvedWorkGroupScrollAnchor {
            row_id: row_id.to_owned(),
            offset_within_row,
            scroll_offset,
        }
    }

    #[test]
    fn returns_to_the_top_after_a_fast_flick_even_when_the_cached_start_is_five_rows_behind() {
        // The measured positions decide, not the virtualizer's cached `start`.
        assert_eq!(
            resolve_work_group_scroll_anchor(&data(), 0.0, position),
            Some(resolved("call-0", 0.0, 0.0))
        );
    }

    #[test]
    fn uses_the_measured_position_rather_than_the_cached_row() {
        for (scroll, row_id, offset_within_row) in [
            (302.0, "call-9", 5.0),
            (40.0, "call-1", 7.0),
            (165.0, "call-5", 0.0),
        ] {
            assert_eq!(
                resolve_work_group_scroll_anchor(&data(), scroll, position),
                Some(resolved(row_id, offset_within_row, scroll)),
                "scroll {scroll}"
            );
        }
    }

    #[test]
    fn preserves_an_offset_within_expanded_output() {
        let positions = [0.0, 33.0, 600.0];
        assert_eq!(
            resolve_work_group_scroll_anchor(&["first", "output", "last"], 153.0, |index| {
                positions.get(index).copied()
            }),
            Some(resolved("output", 120.0, 153.0))
        );
    }

    #[test]
    fn does_not_preserve_overscroll_before_the_first_row() {
        assert_eq!(
            resolve_work_group_scroll_anchor(&data(), -20.0, position),
            Some(resolved("call-0", 0.0, 0.0))
        );
    }

    #[test]
    fn skips_incomplete_layout_snapshots_instead_of_saving_an_invalid_anchor() {
        assert_eq!(
            resolve_work_group_scroll_anchor::<String>(&[], 0.0, position),
            None
        );
        assert_eq!(
            resolve_work_group_scroll_anchor(&data(), f64::NAN, position),
            None
        );
        assert_eq!(
            resolve_work_group_scroll_anchor(&data(), 165.0, |_| None),
            None
        );
        assert_eq!(
            resolve_work_group_scroll_anchor(&data(), 165.0, |index| {
                if index == 0 {
                    Some(0.0)
                } else {
                    Some(f64::NAN)
                }
            }),
            None
        );
    }

    #[test]
    fn the_saved_anchor_round_trips_through_the_restore_side() {
        use super::super::messages_timeline_logic::resolve_work_group_scroll_index;
        let ids = data();
        let anchor = resolve_work_group_scroll_anchor(&ids, 302.0, position)
            .expect("anchor")
            .into_anchor();
        let restored = resolve_work_group_scroll_index(&ids, Some(&anchor)).expect("restored");
        assert_eq!(restored.index, 9);
        assert_eq!(restored.view_offset, -5.0);
    }
}
