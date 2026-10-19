//! Port of the pure rules in upstream/apps/web/src/components/chat/useThreadFindNavigation.ts.
//!
//! Upstream materializes missing results by paging history, then scrolls only enough to
//! reveal the active text. The scroll itself is LegendList and DOM geometry; GPUI's list
//! and text layout do that work in the view. This module holds the numbers and
//! decisions the hook makes from them: the match key, when to page earlier history, how
//! far to scroll so the match sits in the readable band, and how a pinned paging
//! position follows an insert. The view calls these with the geometry it measured.
//!
//! Port of upstream/apps/web/src/components/chat/useThreadFindNavigation.ts (pure rules).

use std::collections::HashSet;

/// Upstream `FIND_MATCH_VIEW_MARGIN`: the band kept clear around a revealed match.
pub const FIND_MATCH_VIEW_MARGIN: f64 = 96.0;
/// Horizontal padding kept clear around a match inside a code block's scroller.
pub const FIND_CODE_HORIZONTAL_PADDING: f64 = 16.0;

/// Identifies one navigation step, so a stale positioning does not apply to a newer one.
pub fn find_match_key(navigation_id: u64, query: &str, entry_id: &str, occurrence: u64) -> String {
    format!("{navigation_id}:{query}:{entry_id}:{occurrence}")
}

/// The reveal margin, shrunk when the band above the composer is too short to hold it.
pub fn reveal_margin(viewport_height: f64, content_inset_end: f64, rect_height: f64) -> f64 {
    FIND_MATCH_VIEW_MARGIN
        .min(0.0_f64.max((viewport_height - content_inset_end - rect_height) / 2.0))
}

/// Vertical scroll that puts the range inside `[top + margin, bottom - margin - inset]`.
/// Zero when it already fits. Above the band wins over below it, as upstream orders them.
pub fn vertical_reveal_delta(
    rect_top: f64,
    rect_bottom: f64,
    viewport_top: f64,
    viewport_bottom: f64,
    margin: f64,
    content_inset_end: f64,
) -> f64 {
    let top = viewport_top + margin;
    let bottom = viewport_bottom - margin - content_inset_end;
    if rect_top < top {
        rect_top - top
    } else if rect_bottom > bottom {
        rect_bottom - bottom
    } else {
        0.0
    }
}

/// Horizontal scroll inside a code block so the match is visible with its padding.
pub fn horizontal_reveal_delta(
    rect_left: f64,
    rect_right: f64,
    viewport_left: f64,
    viewport_right: f64,
) -> f64 {
    if rect_left < viewport_left {
        rect_left - viewport_left - FIND_CODE_HORIZONTAL_PADDING
    } else if rect_right > viewport_right {
        rect_right - viewport_right + FIND_CODE_HORIZONTAL_PADDING
    } else {
        0.0
    }
}

/// A scroll smaller than a pixel is not worth a scroll.
pub fn is_visible_delta(delta: f64) -> bool {
    delta.abs() >= 1.0
}

/// A new list can expose DOM text before its row sizes settle. The hook waits for row
/// positioning before measuring an offscreen occurrence, unless this match was already
/// positioned for these entries or has settled.
pub fn must_position_rows_first(
    delta: f64,
    positioned_for_current_entries: bool,
    settled_for_current_match: bool,
) -> bool {
    is_visible_delta(delta) && !positioned_for_current_entries && !settled_for_current_match
}

/// The pinned paging position stays while the match is inside the viewport band. Once
/// it scrolls out of the band, the pin is released.
pub fn paging_position_still_visible(
    rect_top: f64,
    rect_bottom: f64,
    viewport_top: f64,
    viewport_bottom: f64,
) -> bool {
    !(rect_bottom < viewport_top || rect_top > viewport_bottom)
}

/// How far to scroll so a pinned match stays where it was after rows insert above it.
pub fn paging_restore_delta(current_top: f64, pinned_top: f64) -> Option<f64> {
    let delta = current_top - pinned_top;
    is_visible_delta(delta).then_some(delta)
}

/// Whether the find has to page earlier history for its match. Upstream does this only
/// when the list is ready, the match's row is not loaded, and the server reports more
/// history that is not already loading or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarlierHistoryControls {
    pub has_more_history: bool,
    pub loading: bool,
    pub error: bool,
}

pub fn needs_earlier_history(
    match_present: bool,
    list_ready: bool,
    row_loaded: bool,
    match_entry_loaded: bool,
    controls: Option<EarlierHistoryControls>,
) -> bool {
    if !match_present || !list_ready || row_loaded || match_entry_loaded {
        return false;
    }
    controls
        .is_some_and(|controls| controls.has_more_history && !controls.loading && !controls.error)
}

/// Dedupes history requests per navigation step. A page is requested once per cursor,
/// and a new step starts with none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EarlierHistoryRequests {
    key: Option<String>,
    pages: HashSet<String>,
}

impl EarlierHistoryRequests {
    /// Records a request for `cursor` under `match_key`. Returns whether the caller
    /// should ask for the page. `cursor` is the first loaded entry id, or `"first"`.
    pub fn request(&mut self, match_key: Option<&str>, cursor: &str) -> bool {
        if self.key.as_deref() != match_key {
            self.key = match_key.map(str::to_owned);
            self.pages.clear();
        }
        self.pages.insert(cursor.to_owned())
    }
}

/// The cursor a history request is keyed by: the first loaded entry id, or `"first"`.
pub fn earlier_history_cursor(first_entry_id: Option<&str>) -> &str {
    first_entry_id.unwrap_or("first")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_key_changes_with_every_navigation_input() {
        let base = find_match_key(1, "COD4", "entry", 0);
        assert_ne!(base, find_match_key(2, "COD4", "entry", 0));
        assert_ne!(base, find_match_key(1, "cod4", "entry", 0));
        assert_ne!(base, find_match_key(1, "COD4", "other", 0));
        assert_ne!(base, find_match_key(1, "COD4", "entry", 1));
    }

    #[test]
    fn margin_shrinks_only_when_the_band_is_short() {
        assert_eq!(reveal_margin(800.0, 100.0, 40.0), FIND_MATCH_VIEW_MARGIN);
        // (200 - 100 - 80) / 2 = 10
        assert_eq!(reveal_margin(200.0, 100.0, 80.0), 10.0);
        assert_eq!(reveal_margin(100.0, 100.0, 80.0), 0.0);
    }

    #[test]
    fn vertical_delta_brings_a_match_above_or_below_the_band_into_view() {
        // Band: 100 + 96 = 196 to 600 - 96 - 100 = 404.
        assert_eq!(
            vertical_reveal_delta(150.0, 170.0, 100.0, 600.0, 96.0, 100.0),
            150.0 - 196.0
        );
        assert_eq!(
            vertical_reveal_delta(500.0, 520.0, 100.0, 600.0, 96.0, 100.0),
            520.0 - 404.0
        );
        assert_eq!(
            vertical_reveal_delta(250.0, 270.0, 100.0, 600.0, 96.0, 100.0),
            0.0
        );
    }

    #[test]
    fn horizontal_delta_keeps_padding_on_both_sides() {
        assert_eq!(
            horizontal_reveal_delta(10.0, 30.0, 50.0, 400.0),
            10.0 - 50.0 - 16.0
        );
        assert_eq!(
            horizontal_reveal_delta(380.0, 420.0, 50.0, 400.0),
            420.0 - 400.0 + 16.0
        );
        assert_eq!(horizontal_reveal_delta(100.0, 120.0, 50.0, 400.0), 0.0);
    }

    #[test]
    fn sub_pixel_scrolls_are_skipped() {
        assert!(!is_visible_delta(0.5));
        assert!(is_visible_delta(-1.0));
        assert_eq!(paging_restore_delta(100.4, 100.0), None);
        assert_eq!(paging_restore_delta(120.0, 100.0), Some(20.0));
    }

    #[test]
    fn rows_are_positioned_before_measuring_an_offscreen_match() {
        assert!(must_position_rows_first(40.0, false, false));
        assert!(!must_position_rows_first(40.0, true, false));
        assert!(!must_position_rows_first(40.0, false, true));
        assert!(!must_position_rows_first(0.0, false, false));
    }

    #[test]
    fn paging_pin_is_released_only_once_it_leaves_the_band() {
        assert!(paging_position_still_visible(90.0, 110.0, 100.0, 600.0));
        assert!(!paging_position_still_visible(20.0, 80.0, 100.0, 600.0));
        assert!(!paging_position_still_visible(610.0, 640.0, 100.0, 600.0));
    }

    #[test]
    fn earlier_history_is_requested_only_for_an_unloaded_match() {
        let controls = EarlierHistoryControls {
            has_more_history: true,
            loading: false,
            error: false,
        };
        let controls_some = Some(controls);
        assert!(needs_earlier_history(
            true,
            true,
            false,
            false,
            controls_some
        ));
        assert!(!needs_earlier_history(
            false,
            true,
            false,
            false,
            controls_some
        ));
        assert!(!needs_earlier_history(
            true,
            false,
            false,
            false,
            controls_some
        ));
        assert!(!needs_earlier_history(
            true,
            true,
            true,
            false,
            controls_some
        ));
        assert!(!needs_earlier_history(
            true,
            true,
            false,
            true,
            controls_some
        ));
        assert!(!needs_earlier_history(true, true, false, false, None));
        for blocked in [
            EarlierHistoryControls {
                has_more_history: false,
                ..controls
            },
            EarlierHistoryControls {
                loading: true,
                ..controls
            },
            EarlierHistoryControls {
                error: true,
                ..controls
            },
        ] {
            assert!(!needs_earlier_history(
                true,
                true,
                false,
                false,
                Some(blocked)
            ));
        }
    }

    #[test]
    fn each_history_cursor_is_requested_once_per_navigation_step() {
        let mut requests = EarlierHistoryRequests::default();
        assert!(requests.request(Some("1:q:a:0"), "first-entry"));
        assert!(!requests.request(Some("1:q:a:0"), "first-entry"));
        assert!(requests.request(Some("1:q:a:0"), "older-entry"));
        // A new step forgets the pages it already asked for.
        assert!(requests.request(Some("2:q:a:0"), "first-entry"));
        assert_eq!(earlier_history_cursor(None), "first");
        assert_eq!(earlier_history_cursor(Some("e")), "e");
    }
}
