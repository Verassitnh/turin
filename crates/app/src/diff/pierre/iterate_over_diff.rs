//! Port of `@pierre/diffs@1.5.2` `dist/utils/iterateOverDiff.js` and the
//! expansion helpers of `dist/utils/virtualDiffLayout.js`.
//!
//! Only the non-windowed walk is ported (`startingLine = 0`,
//! `totalLines = Infinity`): Turin virtualizes by building flat row lists and
//! letting the GPUI list render the visible rows, instead of re-walking the
//! diff per scroll window.

use std::collections::HashMap;

use super::types::{
    FileDiffMetadata, HunkContent, get_hunk_side_end_boundary, get_hunk_side_start_boundary,
};

/// `DEFAULT_COLLAPSED_CONTEXT_THRESHOLD`.
pub const DEFAULT_COLLAPSED_CONTEXT_THRESHOLD: usize = 1;
/// `expansionLineCount` default of `DiffHunksRenderer`.
pub const DEFAULT_EXPANSION_LINE_COUNT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffStyle {
    Unified,
    Split,
}

/// `HunkExpansionRegion`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpandedRegion {
    pub from_start: usize,
    pub from_end: usize,
}

/// `expandedHunks`: `true` (everything) or a per-hunk map. The trailing
/// region of a file is keyed by `hunks.len()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpandedHunks {
    pub all: bool,
    pub regions: HashMap<usize, ExpandedRegion>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpandDirection {
    Up,
    Down,
    Both,
}

impl ExpandedHunks {
    /// `DiffHunksRenderer.expandHunk`.
    pub fn expand(&mut self, index: usize, direction: ExpandDirection, line_count: usize) {
        let region = self.regions.entry(index).or_default();
        if matches!(direction, ExpandDirection::Up | ExpandDirection::Both) {
            region.from_start += line_count;
        }
        if matches!(direction, ExpandDirection::Down | ExpandDirection::Both) {
            region.from_end += line_count;
        }
    }
}

/// `getExpandedRegion` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpandedRegionInfo {
    pub from_start: usize,
    pub from_end: usize,
    pub range_size: usize,
    pub collapsed_lines: usize,
    pub render_all: bool,
}

/// `getExpandedRegion`.
pub fn get_expanded_region(
    is_partial: bool,
    range_size: usize,
    expanded: &ExpandedHunks,
    hunk_index: usize,
    threshold: usize,
) -> ExpandedRegionInfo {
    if range_size == 0 || is_partial {
        return ExpandedRegionInfo {
            from_start: 0,
            from_end: 0,
            range_size,
            collapsed_lines: range_size,
            render_all: false,
        };
    }
    if expanded.all || range_size <= threshold {
        return ExpandedRegionInfo {
            from_start: range_size,
            from_end: 0,
            range_size,
            collapsed_lines: 0,
            render_all: true,
        };
    }
    let region = expanded.regions.get(&hunk_index).copied().unwrap_or_default();
    let from_start = region.from_start.min(range_size);
    let from_end = region.from_end.min(range_size);
    let expanded_count = from_start + from_end;
    let render_all = expanded_count >= range_size;
    ExpandedRegionInfo {
        from_start: if render_all { range_size } else { from_start },
        from_end: if render_all { 0 } else { from_end },
        range_size,
        collapsed_lines: range_size.saturating_sub(expanded_count),
        render_all,
    }
}

/// `getTrailingContextRangeSize`; a mismatch (upstream throws) yields 0.
pub fn get_trailing_context_range_size(file: &FileDiffMetadata) -> usize {
    let Some(last) = file.hunks.last() else {
        return 0;
    };
    if file.is_partial || file.addition_lines.is_empty() || file.deletion_lines.is_empty() {
        return 0;
    }
    let addition_remaining = file.addition_lines.len() as isize
        - get_hunk_side_end_boundary(last.addition_start, last.addition_count) as isize;
    let deletion_remaining = file.deletion_lines.len() as isize
        - get_hunk_side_end_boundary(last.deletion_start, last.deletion_count) as isize;
    if addition_remaining <= 0 && deletion_remaining <= 0 {
        return 0;
    }
    if addition_remaining != deletion_remaining {
        tracing::error!(
            additions = addition_remaining,
            deletions = deletion_remaining,
            file = %file.name,
            "trailing context mismatch"
        );
        return 0;
    }
    addition_remaining.max(0) as usize
}

/// `getTrailingExpandedRegion`.
pub fn get_trailing_expanded_region(
    file: &FileDiffMetadata,
    expanded: &ExpandedHunks,
    threshold: usize,
) -> Option<ExpandedRegionInfo> {
    let size = get_trailing_context_range_size(file);
    if size == 0 {
        return None;
    }
    if expanded.all || size <= threshold {
        return Some(ExpandedRegionInfo {
            from_start: size,
            from_end: 0,
            range_size: size,
            collapsed_lines: 0,
            render_all: true,
        });
    }
    let region = expanded
        .regions
        .get(&file.hunks.len())
        .copied()
        .unwrap_or_default();
    let from_start = region.from_start.min(size);
    Some(ExpandedRegionInfo {
        from_start,
        from_end: 0,
        range_size: size,
        collapsed_lines: size - from_start,
        render_all: from_start >= size,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEventKind {
    Context,
    ContextExpanded,
    Change,
}

/// `deletionLine` / `additionLine` of an iteration event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SideLine {
    pub line_number: usize,
    pub line_index: usize,
    pub no_eofcr: bool,
    pub unified_line_index: usize,
    pub split_line_index: usize,
}

/// One callback of `iterateOverDiff`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineEvent {
    /// `diff.hunks.length` for lines of the trailing expanded region.
    pub hunk_index: usize,
    pub has_hunk: bool,
    pub collapsed_before: usize,
    pub collapsed_after: usize,
    pub kind: LineEventKind,
    pub deletion: Option<SideLine>,
    pub addition: Option<SideLine>,
}

/// `iterateOverDiff` over the whole diff.
pub fn iterate_over_diff(
    diff: &FileDiffMetadata,
    style: DiffStyle,
    expanded: &ExpandedHunks,
    threshold: usize,
) -> Vec<LineEvent> {
    let mut events = Vec::new();
    let final_hunk_index = diff.hunks.len().wrapping_sub(1);
    for (hunk_index, hunk) in diff.hunks.iter().enumerate() {
        let deletion_boundary = get_hunk_side_start_boundary(hunk.deletion_start, hunk.deletion_count);
        let addition_boundary = get_hunk_side_start_boundary(hunk.addition_start, hunk.addition_count);
        let deletion_start_index = if !diff.is_partial && hunk.deletion_count == 0 {
            deletion_boundary
        } else {
            hunk.deletion_line_index
        };
        let addition_start_index = if !diff.is_partial && hunk.addition_count == 0 {
            addition_boundary
        } else {
            hunk.addition_line_index
        };
        let leading = get_expanded_region(
            diff.is_partial,
            hunk.collapsed_before,
            expanded,
            hunk_index,
            threshold,
        );
        let trailing = if hunk_index == final_hunk_index {
            get_trailing_expanded_region(diff, expanded, threshold)
        } else {
            None
        };
        let trailing_collapsed_after = |unified_index: usize, split_index: usize| -> usize {
            let Some(trailing) = trailing else {
                return 0;
            };
            if trailing.collapsed_lines == 0 || trailing.from_start + trailing.from_end > 0 {
                return 0;
            }
            let last_row = match style {
                DiffStyle::Unified => {
                    unified_index == hunk.unified_line_start + hunk.unified_line_count - 1
                }
                DiffStyle::Split => split_index == hunk.split_line_start + hunk.split_line_count - 1,
            };
            if last_row { trailing.collapsed_lines } else { 0 }
        };
        let mut consumed_collapsed = leading.collapsed_lines == 0;
        let mut consume = |consumed: &mut bool| -> usize {
            if *consumed {
                0
            } else {
                *consumed = true;
                leading.collapsed_lines
            }
        };

        // Leading expanded context: the lines just after the previous hunk
        // (`fromStart`) and the lines just before this one (`fromEnd`).
        let expanded_context = |from_unified: usize,
                                from_split: usize,
                                from_deletion_index: usize,
                                from_addition_index: usize,
                                from_deletion_number: usize,
                                from_addition_number: usize,
                                count: usize,
                                events: &mut Vec<LineEvent>,
                                consumed: &mut bool,
                                first_consumes: bool| {
            for index in 0..count {
                let collapsed_before = if first_consumes {
                    consume(consumed)
                } else {
                    0
                };
                events.push(LineEvent {
                    hunk_index,
                    has_hunk: true,
                    collapsed_before,
                    collapsed_after: 0,
                    kind: LineEventKind::ContextExpanded,
                    deletion: Some(SideLine {
                        line_number: from_deletion_number + index,
                        line_index: from_deletion_index + index,
                        no_eofcr: false,
                        unified_line_index: from_unified + index,
                        split_line_index: from_split + index,
                    }),
                    addition: Some(SideLine {
                        line_number: from_addition_number + index,
                        line_index: from_addition_index + index,
                        no_eofcr: false,
                        unified_line_index: from_unified + index,
                        split_line_index: from_split + index,
                    }),
                });
            }
        };
        let range = leading.range_size;
        expanded_context(
            hunk.unified_line_start.saturating_sub(range),
            hunk.split_line_start.saturating_sub(range),
            deletion_start_index.saturating_sub(range),
            addition_start_index.saturating_sub(range),
            (deletion_boundary + 1).saturating_sub(range),
            (addition_boundary + 1).saturating_sub(range),
            leading.from_start,
            &mut events,
            &mut consumed_collapsed,
            false,
        );
        expanded_context(
            hunk.unified_line_start.saturating_sub(leading.from_end),
            hunk.split_line_start.saturating_sub(leading.from_end),
            deletion_start_index.saturating_sub(leading.from_end),
            addition_start_index.saturating_sub(leading.from_end),
            (deletion_boundary + 1).saturating_sub(leading.from_end),
            (addition_boundary + 1).saturating_sub(leading.from_end),
            leading.from_end,
            &mut events,
            &mut consumed_collapsed,
            true,
        );

        let mut unified_index = hunk.unified_line_start;
        let mut split_index = hunk.split_line_start;
        let mut deletion_index = deletion_start_index;
        let mut addition_index = addition_start_index;
        let mut deletion_number = deletion_boundary + 1;
        let mut addition_number = addition_boundary + 1;
        let last_content = hunk.hunk_content.len().wrapping_sub(1);
        for (content_index, content) in hunk.hunk_content.iter().enumerate() {
            let is_last_content = content_index == last_content;
            match *content {
                HunkContent::Context { lines, .. } => {
                    for index in 0..lines {
                        let is_last_line = is_last_content && index == lines - 1;
                        let unified_row = unified_index + index;
                        let split_row = split_index + index;
                        events.push(LineEvent {
                            hunk_index,
                            has_hunk: true,
                            collapsed_before: consume(&mut consumed_collapsed),
                            collapsed_after: trailing_collapsed_after(unified_row, split_row),
                            kind: LineEventKind::Context,
                            deletion: Some(SideLine {
                                line_number: deletion_number + index,
                                line_index: deletion_index + index,
                                no_eofcr: is_last_line && hunk.no_eofcr_deletions,
                                unified_line_index: unified_row,
                                split_line_index: split_row,
                            }),
                            addition: Some(SideLine {
                                line_number: addition_number + index,
                                line_index: addition_index + index,
                                no_eofcr: is_last_line && hunk.no_eofcr_additions,
                                unified_line_index: unified_row,
                                split_line_index: split_row,
                            }),
                        });
                    }
                    unified_index += lines;
                    split_index += lines;
                    deletion_index += lines;
                    addition_index += lines;
                    deletion_number += lines;
                    addition_number += lines;
                }
                HunkContent::Change {
                    deletions,
                    additions,
                    ..
                } => {
                    let split_count = deletions.max(additions);
                    let unified_count = deletions + additions;
                    let rows = match style {
                        DiffStyle::Unified => unified_count,
                        DiffStyle::Split => split_count,
                    };
                    for index in 0..rows {
                        let unified_deletion = (index < deletions).then_some(unified_index + index);
                        let (unified_addition, addition_offset) = match style {
                            DiffStyle::Unified => (
                                (index >= deletions).then_some(unified_index + index),
                                index.checked_sub(deletions),
                            ),
                            DiffStyle::Split => (
                                (index < additions).then_some(unified_index + deletions + index),
                                (index < additions).then_some(index),
                            ),
                        };
                        let resolved_split = match style {
                            DiffStyle::Unified => {
                                split_index
                                    + if index < deletions {
                                        index
                                    } else {
                                        index - deletions
                                    }
                            }
                            DiffStyle::Split => split_index + index,
                        };
                        let no_eofcr_deletion = match style {
                            DiffStyle::Unified => {
                                is_last_content
                                    && deletions > 0
                                    && index == deletions - 1
                                    && hunk.no_eofcr_deletions
                            }
                            DiffStyle::Split => {
                                is_last_content && index == split_count - 1 && hunk.no_eofcr_deletions
                            }
                        };
                        let no_eofcr_addition = match style {
                            DiffStyle::Unified => {
                                is_last_content
                                    && index == unified_count - 1
                                    && hunk.no_eofcr_additions
                            }
                            DiffStyle::Split => {
                                is_last_content && index == split_count - 1 && hunk.no_eofcr_additions
                            }
                        };
                        let deletion = unified_deletion.map(|unified| SideLine {
                            line_number: deletion_number + index,
                            line_index: deletion_index + index,
                            no_eofcr: no_eofcr_deletion,
                            unified_line_index: unified,
                            split_line_index: resolved_split,
                        });
                        let addition = unified_addition.zip(addition_offset).map(|(unified, offset)| {
                            SideLine {
                                line_number: addition_number + offset,
                                line_index: addition_index + offset,
                                no_eofcr: no_eofcr_addition,
                                unified_line_index: unified,
                                split_line_index: resolved_split,
                            }
                        });
                        events.push(LineEvent {
                            hunk_index,
                            has_hunk: true,
                            collapsed_before: consume(&mut consumed_collapsed),
                            collapsed_after: trailing_collapsed_after(
                                unified_index + index,
                                match style {
                                    DiffStyle::Unified => resolved_split,
                                    DiffStyle::Split => split_index + index,
                                },
                            ),
                            kind: LineEventKind::Change,
                            deletion,
                            addition,
                        });
                    }
                    consume(&mut consumed_collapsed);
                    unified_index += unified_count;
                    split_index += split_count;
                    deletion_index += deletions;
                    addition_index += additions;
                    deletion_number += deletions;
                    addition_number += additions;
                }
            }
        }

        if let Some(trailing) = trailing {
            let len = trailing.from_start + trailing.from_end;
            for index in 0..len {
                let is_last = index == len - 1;
                events.push(LineEvent {
                    hunk_index: diff.hunks.len(),
                    has_hunk: false,
                    collapsed_before: 0,
                    collapsed_after: if is_last { trailing.collapsed_lines } else { 0 },
                    kind: LineEventKind::ContextExpanded,
                    deletion: Some(SideLine {
                        line_number: deletion_number + index,
                        line_index: deletion_index + index,
                        no_eofcr: false,
                        unified_line_index: unified_index + index,
                        split_line_index: split_index + index,
                    }),
                    addition: Some(SideLine {
                        line_number: addition_number + index,
                        line_index: addition_index + index,
                        no_eofcr: false,
                        unified_line_index: unified_index + index,
                        split_line_index: split_index + index,
                    }),
                });
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::pierre::parse_patch_files::parse_patch_files;

    fn file(patch: &str) -> FileDiffMetadata {
        parse_patch_files(patch, None)
            .remove(0)
            .files
            .remove(0)
    }

    const TWO_HUNKS: &str = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10,2 +10,2 @@\n-x\n+y\n z\n@@ -40 +40 @@\n-p\n+q\n";

    #[test]
    fn partial_diffs_keep_context_collapsed() {
        let diff = file(TWO_HUNKS);
        let events = iterate_over_diff(
            &diff,
            DiffStyle::Unified,
            &ExpandedHunks::default(),
            DEFAULT_COLLAPSED_CONTEXT_THRESHOLD,
        );
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].collapsed_before, 9);
        assert_eq!(events[0].deletion.unwrap().line_number, 10);
        assert_eq!(events[0].addition, None);
        assert_eq!(events[1].addition.unwrap().line_number, 10);
        assert_eq!(events[2].kind, LineEventKind::Context);
        assert_eq!(events[3].collapsed_before, 28);
    }

    #[test]
    fn split_pairs_deletions_with_additions() {
        let diff = file(TWO_HUNKS);
        let events = iterate_over_diff(
            &diff,
            DiffStyle::Split,
            &ExpandedHunks::default(),
            DEFAULT_COLLAPSED_CONTEXT_THRESHOLD,
        );
        assert_eq!(events.len(), 3);
        let first = &events[0];
        assert_eq!(first.deletion.unwrap().line_number, 10);
        assert_eq!(first.addition.unwrap().line_number, 10);
    }

    #[test]
    fn expansion_regions() {
        let mut expanded = ExpandedHunks::default();
        let none = get_expanded_region(false, 300, &expanded, 1, 1);
        assert_eq!(none.collapsed_lines, 300);
        expanded.expand(1, ExpandDirection::Up, 100);
        expanded.expand(1, ExpandDirection::Down, 100);
        let region = get_expanded_region(false, 300, &expanded, 1, 1);
        assert_eq!((region.from_start, region.from_end, region.collapsed_lines), (100, 100, 100));
        expanded.expand(1, ExpandDirection::Both, 100);
        assert!(get_expanded_region(false, 300, &expanded, 1, 1).render_all);
        assert!(get_expanded_region(false, 1, &ExpandedHunks::default(), 0, 1).render_all);
    }
}
