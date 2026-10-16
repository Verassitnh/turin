//! Port of upstream/apps/web/src/components/chat/threadFind.ts.
//!
//! `ThreadFindPositionReader` reads the live DOM in upstream, so here it is any
//! closure that returns the anchor for a query (`thread_find_position.rs` holds
//! the geometry rule it wraps).

use turin_contracts::base_schemas::{RunId, TrimmedNonEmptyString};
use turin_contracts::orchestration_v2::OrchestrationV2SearchThreadStart;

/// `NonNullable<OrchestrationV2SearchThreadInput["start"]>`.
pub type ThreadFindStart = OrchestrationV2SearchThreadStart;

/// Anchor for a query from the rows on screen. `None` when no row qualifies.
pub type ThreadFindPositionReader<'a> = dyn Fn(&str) -> Option<ThreadFindStart> + 'a;

/// One occurrence of the query inside a searchable timeline entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadFindMatch {
    pub entry_id: TrimmedNonEmptyString,
    /// The turn to expand when the matching entry is folded.
    pub run_id: Option<RunId>,
    /// Zero-based occurrence within this timeline entry.
    pub occurrence: u64,
}

/// Upstream clamps a stored index into `[0, total - 1]`; an empty result set is index 0.
fn clamp_thread_find_index(index: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    index.min(total - 1)
}

/// Moves `delta` steps from `index` and wraps around the result set.
pub fn step_thread_find_index(index: u64, total: u64, delta: i64) -> u64 {
    if total == 0 {
        return 0;
    }
    let clamped = i128::from(clamp_thread_find_index(index, total));
    let total = i128::from(total);
    (((clamped + i128::from(delta)) % total + total) % total) as u64
}

/// `"n/total"` with a 1-based position, or `"0/0"` when nothing matches.
pub fn format_thread_find_count(index: u64, total: u64) -> String {
    if total == 0 {
        return "0/0".to_owned();
    }
    format!("{}/{}", clamp_thread_find_index(index, total) + 1, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_wraps_and_formats_positions() {
        assert_eq!(step_thread_find_index(2, 3, 1), 0);
        assert_eq!(step_thread_find_index(0, 3, -1), 2);
        assert_eq!(format_thread_find_count(4, 2), "2/2");
        assert_eq!(format_thread_find_count(0, 0), "0/0");
    }

    #[test]
    fn out_of_range_index_clamps_before_stepping() {
        // Upstream clamps first, so index 10 in a 3-entry set steps from the last entry.
        assert_eq!(step_thread_find_index(10, 3, 1), 0);
        assert_eq!(step_thread_find_index(10, 3, -1), 1);
        assert_eq!(format_thread_find_count(10, 3), "3/3");
    }

    #[test]
    fn empty_set_stays_at_zero() {
        assert_eq!(step_thread_find_index(5, 0, 1), 0);
        assert_eq!(step_thread_find_index(5, 0, -1), 0);
    }
}
