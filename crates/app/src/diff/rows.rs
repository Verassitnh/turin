//! The row model of one rendered file: what `DiffHunksRenderer.processDiffResult`
//! (`@pierre/diffs@1.5.2`, `dist/renderers/DiffHunksRenderer.js`) emits as
//! gutter/content rows, as plain data the GPUI list renders from.
//!
//! Unified diffs produce one row per line; split diffs pair a deletion cell
//! with an addition cell (an empty side is a hatched buffer). Hunk separators
//! (`createSeparator`) and `\ No newline at end of file` rows sit between.

use super::pierre::iterate_over_diff::{
    DEFAULT_COLLAPSED_CONTEXT_THRESHOLD, DEFAULT_EXPANSION_LINE_COUNT, DiffStyle, ExpandedHunks,
    LineEvent, LineEventKind, get_trailing_context_range_size, iterate_over_diff,
};
use super::pierre::types::{ChangeType, FileDiffMetadata, SelectionSide};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

/// One gutter + content cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub kind: LineKind,
    /// The line number shown in the gutter.
    pub number: usize,
    /// Index into `additionLines` (or `deletionLines` for deletions).
    pub line_index: usize,
    pub side: SelectionSide,
}

/// Which expand buttons a separator shows (`pushSeparator`'s `expandable`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expandable {
    pub up: bool,
    pub down: bool,
    pub chunked: bool,
}

/// `createSeparator` input for the `line-info` style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Separator {
    pub hunk_index: usize,
    /// `None`: the count is unknown until the file loads
    /// ("More unchanged context may be available").
    pub collapsed_lines: Option<usize>,
    pub range_size: usize,
    pub is_first: bool,
    pub is_last: bool,
    /// `data-expand-index` present: the separator is clickable.
    pub expandable: bool,
    pub chunked: bool,
}

impl Separator {
    /// The buttons `createSeparator` draws, in order (up, down) or one `both`.
    pub fn buttons(&self) -> Vec<ExpandButton> {
        if !self.expandable {
            return Vec::new();
        }
        if !self.chunked {
            return vec![if !self.is_first && !self.is_last {
                ExpandButton::Both
            } else if self.is_first {
                ExpandButton::Down
            } else {
                ExpandButton::Up
            }];
        }
        let mut buttons = Vec::new();
        if !self.is_first {
            buttons.push(ExpandButton::Up);
        }
        if !self.is_last {
            buttons.push(ExpandButton::Down);
        }
        buttons
    }

    /// `getModifiedLinesString`.
    pub fn label(&self) -> String {
        match self.collapsed_lines {
            Some(1) => "1 unmodified line".into(),
            Some(count) => format!("{count} unmodified lines"),
            None => "More unchanged context may be available".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpandButton {
    Up,
    Down,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Separator(Separator),
    /// A unified-diff line.
    Line(Cell),
    /// A split-diff row: deletions on the left, additions on the right.
    Split {
        left: Option<Cell>,
        right: Option<Cell>,
    },
    /// `\ No newline at end of file` (`createNoNewlineElement`).
    NoNewline {
        left: bool,
        right: bool,
        kind: LineKind,
    },
}

/// Whether `loadDiffFiles` can hydrate this partial diff
/// (`canHydrateCollapsedContext`).
pub fn can_hydrate_collapsed_context(file: &FileDiffMetadata, has_file_loader: bool) -> bool {
    file.is_partial
        && has_file_loader
        && matches!(file.r#type, ChangeType::Change | ChangeType::RenameChanged)
}

/// `processDiffResult`'s row walk.
pub fn build_rows(
    file: &FileDiffMetadata,
    style: DiffStyle,
    expanded: &ExpandedHunks,
    has_file_loader: bool,
) -> Vec<DiffRow> {
    let can_hydrate = can_hydrate_collapsed_context(file, has_file_loader);
    let is_expandable = !file.is_partial || can_hydrate;
    let trailing_range_size = get_trailing_context_range_size(file);
    let events = iterate_over_diff(file, style, expanded, DEFAULT_COLLAPSED_CONTEXT_THRESHOLD);
    let final_hunk_index = file.hunks.len().wrapping_sub(1);
    let mut rows = Vec::new();
    for event in &events {
        push_event(
            &mut rows,
            file,
            event,
            style,
            is_expandable,
            can_hydrate,
            trailing_range_size,
            final_hunk_index,
        );
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn push_event(
    rows: &mut Vec<DiffRow>,
    file: &FileDiffMetadata,
    event: &LineEvent,
    style: DiffStyle,
    is_expandable: bool,
    can_hydrate: bool,
    trailing_range_size: usize,
    final_hunk_index: usize,
) {
    if event.collapsed_before > 0 {
        let hunk = file.hunks.get(event.hunk_index);
        let range_size = hunk.map_or(0, |h| h.collapsed_before);
        rows.push(DiffRow::Separator(Separator {
            hunk_index: event.hunk_index,
            collapsed_lines: Some(event.collapsed_before),
            range_size,
            is_first: event.hunk_index == 0,
            is_last: false,
            expandable: is_expandable,
            chunked: range_size > DEFAULT_EXPANSION_LINE_COUNT,
        }));
    }
    let deletion_cell = |kind| {
        event.deletion.map(|line| Cell {
            kind,
            number: line.line_number,
            line_index: line.line_index,
            side: SelectionSide::Deletions,
        })
    };
    let addition_cell = |kind| {
        event.addition.map(|line| Cell {
            kind,
            number: line.line_number,
            line_index: line.line_index,
            side: SelectionSide::Additions,
        })
    };
    let context_kind = LineKind::Context;
    let is_change = event.kind == LineEventKind::Change;
    match style {
        DiffStyle::Unified => {
            let cell = if is_change {
                addition_cell(LineKind::Addition).or_else(|| deletion_cell(LineKind::Deletion))
            } else {
                addition_cell(context_kind)
            };
            if let Some(cell) = cell {
                rows.push(DiffRow::Line(cell));
            }
            // Deletion marker row first, then addition (unified rows are
            // one-sided for changes, so each carries its own flag).
            let eof = if is_change {
                match (event.deletion, event.addition) {
                    (Some(line), None) => {
                        line.no_eofcr.then_some((true, false, LineKind::Deletion))
                    }
                    (None, Some(line)) => {
                        line.no_eofcr.then_some((false, true, LineKind::Addition))
                    }
                    _ => None,
                }
            } else {
                let deletion = event.deletion.is_some_and(|l| l.no_eofcr);
                let addition = event.addition.is_some_and(|l| l.no_eofcr);
                // Both sides' markers collapse into one row per side as in
                // the unified renderer; context shows deletion's first.
                match (deletion, addition) {
                    (false, false) => None,
                    (d, a) => Some((d, a, LineKind::Context)),
                }
            };
            if let Some((left, right, kind)) = eof {
                if !is_change && left && right {
                    rows.push(DiffRow::NoNewline {
                        left: true,
                        right: false,
                        kind,
                    });
                    rows.push(DiffRow::NoNewline {
                        left: false,
                        right: true,
                        kind,
                    });
                } else {
                    rows.push(DiffRow::NoNewline { left, right, kind });
                }
            }
        }
        DiffStyle::Split => {
            let (left, right) = if is_change {
                (
                    deletion_cell(LineKind::Deletion),
                    addition_cell(LineKind::Addition),
                )
            } else {
                (deletion_cell(context_kind), addition_cell(context_kind))
            };
            rows.push(DiffRow::Split { left, right });
            let deletion_eof = event.deletion.is_some_and(|l| l.no_eofcr);
            let addition_eof = event.addition.is_some_and(|l| l.no_eofcr);
            if deletion_eof || addition_eof {
                let kind = if is_change {
                    if addition_eof {
                        LineKind::Addition
                    } else {
                        LineKind::Deletion
                    }
                } else {
                    LineKind::Context
                };
                rows.push(DiffRow::NoNewline {
                    left: deletion_eof,
                    right: addition_eof,
                    kind,
                });
            }
        }
    }
    let is_final_hunk_row = event.hunk_index == final_hunk_index
        && event.has_hunk
        && match style {
            DiffStyle::Unified => event
                .addition
                .or(event.deletion)
                .zip(file.hunks.get(event.hunk_index))
                .is_some_and(|(line, hunk)| {
                    line.unified_line_index == hunk.unified_line_start + hunk.unified_line_count - 1
                }),
            DiffStyle::Split => event
                .addition
                .or(event.deletion)
                .zip(file.hunks.get(event.hunk_index))
                .is_some_and(|(line, hunk)| {
                    line.split_line_index == hunk.split_line_start + hunk.split_line_count - 1
                }),
        };
    if event.collapsed_after > 0 || (is_final_hunk_row && can_hydrate) {
        rows.push(DiffRow::Separator(Separator {
            hunk_index: if event.kind == LineEventKind::ContextExpanded {
                event.hunk_index
            } else {
                event.hunk_index + 1
            },
            collapsed_lines: if is_final_hunk_row && can_hydrate {
                None
            } else {
                Some(event.collapsed_after)
            },
            range_size: trailing_range_size,
            is_first: false,
            is_last: true,
            expandable: is_expandable,
            chunked: trailing_range_size > DEFAULT_EXPANSION_LINE_COUNT,
        }));
    }
}

/// `totalLines` of `createPreElement`: sizes the number column.
pub fn total_lines(file: &FileDiffMetadata) -> usize {
    let from_hunks = file.hunks.last().map_or(0, |hunk| {
        (hunk.addition_start + hunk.addition_count).max(hunk.deletion_start + hunk.deletion_count)
    });
    from_hunks
        .max(file.addition_lines.len())
        .max(file.deletion_lines.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::pierre::parse_patch_files::parse_patch_files;

    fn parse(patch: &str) -> FileDiffMetadata {
        parse_patch_files(patch, None).remove(0).files.remove(0)
    }

    const PATCH: &str = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10,3 +10,4 @@\n ctx\n-old\n+new\n+extra\n tail\n@@ -40 +41 @@\n-p\n+q\n";

    #[test]
    fn unified_rows_interleave_separators() {
        let rows = build_rows(
            &parse(PATCH),
            DiffStyle::Unified,
            &ExpandedHunks::default(),
            false,
        );
        let kinds: Vec<String> = rows
            .iter()
            .map(|row| match row {
                DiffRow::Separator(s) => format!("sep:{}", s.label()),
                DiffRow::Line(c) => format!("{:?}:{}", c.kind, c.number),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "sep:9 unmodified lines",
                "Context:10",
                "Deletion:11",
                "Addition:11",
                "Addition:12",
                "Context:13",
                "sep:27 unmodified lines",
                "Deletion:40",
                "Addition:41",
            ]
        );
    }

    #[test]
    fn partial_separators_are_not_expandable_without_a_loader() {
        let rows = build_rows(
            &parse(PATCH),
            DiffStyle::Unified,
            &ExpandedHunks::default(),
            false,
        );
        let DiffRow::Separator(first) = rows[0] else {
            panic!()
        };
        assert!(!first.expandable);
        assert!(first.buttons().is_empty());
    }

    #[test]
    fn a_loader_makes_separators_expandable_with_an_unknown_trailing_range() {
        let rows = build_rows(
            &parse(PATCH),
            DiffStyle::Unified,
            &ExpandedHunks::default(),
            true,
        );
        let DiffRow::Separator(first) = rows[0] else {
            panic!()
        };
        assert!(first.expandable);
        assert_eq!(first.buttons(), vec![ExpandButton::Down]);
        let DiffRow::Separator(last) = *rows.last().unwrap() else {
            panic!()
        };
        assert_eq!(last.collapsed_lines, None);
        assert!(last.is_last);
        assert_eq!(last.buttons(), vec![ExpandButton::Up]);
        assert_eq!(last.label(), "More unchanged context may be available");
    }

    #[test]
    fn split_pairs_changes_and_leaves_an_empty_side() {
        let rows = build_rows(
            &parse(PATCH),
            DiffStyle::Split,
            &ExpandedHunks::default(),
            false,
        );
        let pairs: Vec<(Option<usize>, Option<usize>)> = rows
            .iter()
            .filter_map(|row| match row {
                DiffRow::Split { left, right } => {
                    Some((left.map(|c| c.number), right.map(|c| c.number)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                (Some(10), Some(10)),
                (Some(11), Some(11)),
                (None, Some(12)),
                (Some(12), Some(13)),
                (Some(40), Some(41)),
            ]
        );
    }

    #[test]
    fn no_newline_markers_follow_their_line() {
        let file = parse(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
        );
        let rows = build_rows(&file, DiffStyle::Unified, &ExpandedHunks::default(), false);
        assert_eq!(rows.len(), 4);
        assert!(matches!(
            rows[1],
            DiffRow::NoNewline {
                left: true,
                right: false,
                kind: LineKind::Deletion
            }
        ));
        assert!(matches!(
            rows[3],
            DiffRow::NoNewline {
                left: false,
                right: true,
                kind: LineKind::Addition
            }
        ));
    }

    #[test]
    fn total_lines_sizes_the_number_column() {
        assert_eq!(total_lines(&parse(PATCH)), 42);
    }

    #[test]
    fn chunked_separators_show_both_directional_buttons() {
        let separator = Separator {
            hunk_index: 1,
            collapsed_lines: Some(300),
            range_size: 300,
            is_first: false,
            is_last: false,
            expandable: true,
            chunked: true,
        };
        assert_eq!(
            separator.buttons(),
            vec![ExpandButton::Up, ExpandButton::Down]
        );
    }
}
