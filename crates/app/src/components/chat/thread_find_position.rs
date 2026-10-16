//! Port of the geometry rule in upstream/apps/web/src/components/chat/threadFindPosition.ts
//! (`readThreadFindPosition`).
//!
//! Upstream reads `getBoundingClientRect` from DOM rows and ranges. Here the caller
//! passes the same numbers as plain values, so the rule can be tested without a DOM.
//! The caller collects a row's match bottoms only for rows it will read (upstream
//! does the same, after the row has an entry id).

use turin_contracts::base_schemas::TrimmedNonEmptyString;

use super::thread_find::ThreadFindStart;

/// A vertical band (top and bottom edges) in the scroll container's coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadFindBand {
    pub top: f64,
    pub bottom: f64,
}

/// One timeline row as the position reader sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadFindRowGeometry {
    /// `data-timeline-row-id`. Rows without one are skipped.
    pub row_id: Option<String>,
    pub top: f64,
    pub bottom: f64,
    /// Bottom edge of each occurrence in the row, in occurrence order.
    pub match_bottoms: Vec<f64>,
}

/// Anchor to the first row being read, skipping occurrences above its viewport.
pub fn read_thread_find_position(
    viewport: ThreadFindBand,
    rows: &[ThreadFindRowGeometry],
    resolve_entry: impl Fn(&str) -> Option<TrimmedNonEmptyString>,
    content_inset_end_adjustment: f64,
) -> Option<ThreadFindStart> {
    for row in rows {
        if row.bottom <= viewport.top || row.top >= viewport.bottom - content_inset_end_adjustment {
            continue;
        }
        let Some(entry_id) = row.row_id.as_deref().and_then(&resolve_entry) else {
            continue;
        };
        // The first occurrence whose bottom edge is still below the viewport top.
        let occurrence = row
            .match_bottoms
            .iter()
            .position(|bottom| *bottom > viewport.top)
            .unwrap_or(row.match_bottoms.len());
        return Some(ThreadFindStart {
            entry_id,
            occurrence: occurrence as u64,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(id).expect("non-blank entry id")
    }

    /// The upstream fixture: container (0,100)-(500,500), an "above" row at 0-80,
    /// and the "reading" row at 50-350. The composer covers the bottom 100 units.
    fn viewport_rows(reading_text_matches: Vec<f64>) -> Vec<ThreadFindRowGeometry> {
        vec![
            ThreadFindRowGeometry {
                row_id: Some("above".to_owned()),
                top: 0.0,
                bottom: 80.0,
                match_bottoms: vec![80.0],
            },
            ThreadFindRowGeometry {
                row_id: Some("reading".to_owned()),
                top: 50.0,
                bottom: 350.0,
                match_bottoms: reading_text_matches,
            },
        ]
    }

    const VIEWPORT: ThreadFindBand = ThreadFindBand {
        top: 100.0,
        bottom: 500.0,
    };

    #[test]
    fn skips_earlier_rows_and_occurrences_above_the_viewport_within_the_current_message() {
        // First match ends at 80 (above the viewport top at 100); second ends at 180.
        let rows = viewport_rows(vec![80.0, 180.0]);
        assert_eq!(
            read_thread_find_position(VIEWPORT, &rows, |id| Some(entry(id)), 100.0),
            Some(ThreadFindStart {
                entry_id: entry("reading"),
                occurrence: 1,
            })
        );
    }

    #[test]
    fn starts_after_the_current_message_when_all_its_occurrences_are_above_the_viewport() {
        let rows = viewport_rows(vec![80.0, 80.0]);
        assert_eq!(
            read_thread_find_position(VIEWPORT, &rows, |id| Some(entry(id)), 100.0),
            Some(ThreadFindStart {
                entry_id: entry("reading"),
                occurrence: 2,
            })
        );
    }

    #[test]
    fn anchors_unmatched_or_folded_content_and_excludes_rows_hidden_behind_the_composer() {
        let mut rows = viewport_rows(Vec::new());
        assert_eq!(
            read_thread_find_position(VIEWPORT, &rows, |_| Some(entry("folded-message")), 100.0),
            Some(ThreadFindStart {
                entry_id: entry("folded-message"),
                occurrence: 0,
            })
        );
        // Moved behind the composer (top 420 is past 500 - 100).
        rows[1].top = 420.0;
        rows[1].bottom = 720.0;
        assert_eq!(
            read_thread_find_position(VIEWPORT, &rows, |id| Some(entry(id)), 100.0),
            None
        );
    }

    #[test]
    fn rows_without_a_row_id_or_entry_are_skipped() {
        let mut rows = viewport_rows(vec![180.0]);
        rows[1].row_id = None;
        assert_eq!(
            read_thread_find_position(VIEWPORT, &rows, |id| Some(entry(id)), 100.0),
            None
        );
        let rows = viewport_rows(vec![180.0]);
        assert_eq!(
            read_thread_find_position(
                VIEWPORT,
                &rows,
                |id| (id != "reading").then(|| entry(id)),
                100.0
            ),
            None
        );
    }
}
