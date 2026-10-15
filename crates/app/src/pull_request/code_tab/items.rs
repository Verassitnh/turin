//! The rows of the Code tab's list, derived from the parsed files: what pierre's `CodeView`
//! virtualizes. A file is a header, then (unless folded) its hunks as separators and lines, each
//! line followed by whatever is pinned under it. Pure, so the layout is tested without a window.

use std::collections::HashSet;

use turin_contracts::pull_request::PullRequestDiffSide;

use super::patch::PatchFile;
use crate::state::helpers::review_guide_tour::{DiffRow, DiffRowKind};

/// One parsed file with the rows the views draw.
#[derive(Debug, Clone)]
pub struct FileView {
    pub file: PatchFile,
    pub rows: Vec<DiffRow>,
    /// `buildFileDiffRenderKey`.
    pub key: String,
    pub path: String,
    /// What the header counts: the hunks' own, or the host's for a file whose hunks it withheld.
    pub additions: u64,
    pub deletions: u64,
}

impl FileView {
    pub fn new(file: PatchFile) -> Self {
        let rows = file.rows();
        Self {
            key: file.key(),
            path: file.path().to_owned(),
            additions: file.additions,
            deletions: file.deletions,
            rows,
            file,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    FileHeader {
        file: usize,
    },
    /// A binary file has no hunks to show.
    Binary {
        file: usize,
    },
    /// "N unmodified lines" between hunks.
    Separator {
        file: usize,
        lines: usize,
    },
    Line {
        file: usize,
        row: usize,
    },
    /// One row of the split layout: the old side and the new side.
    Split {
        file: usize,
        left: Option<usize>,
        right: Option<usize>,
    },
    /// What is pinned under a line.
    Annotation {
        file: usize,
        side: PullRequestDiffSide,
        line: i64,
    },
    /// The sentinel under the last file while slices are still owed.
    Footer,
}

pub type AnnotationKey = (usize, PullRequestDiffSide, i64);

pub struct BuildInput<'a> {
    pub files: &'a [FileView],
    pub collapsed: &'a dyn Fn(&str) -> bool,
    pub split: bool,
    pub annotated: &'a HashSet<AnnotationKey>,
    pub footer: bool,
}

/// The lines of unchanged code a separator stands for.
fn unmodified_before(file: &PatchFile, hunk: usize) -> usize {
    let header = &file.headers[hunk];
    let end_of_previous = if hunk == 0 {
        1
    } else {
        let previous = &file.headers[hunk - 1];
        previous.addition_start + previous.addition_count
    };
    usize::try_from(header.addition_start - end_of_previous).unwrap_or(0)
}

pub fn build_items(input: &BuildInput<'_>) -> Vec<Item> {
    let mut items = Vec::new();
    for (index, view) in input.files.iter().enumerate() {
        items.push(Item::FileHeader { file: index });
        if (input.collapsed)(&view.key) {
            continue;
        }
        if view.file.binary && view.rows.is_empty() {
            items.push(Item::Binary { file: index });
            continue;
        }
        let annotations_after = |items: &mut Vec<Item>, old: Option<i64>, new: Option<i64>| {
            if let Some(line) = old
                && input
                    .annotated
                    .contains(&(index, PullRequestDiffSide::Left, line))
            {
                items.push(Item::Annotation {
                    file: index,
                    side: PullRequestDiffSide::Left,
                    line,
                });
            }
            if let Some(line) = new
                && input
                    .annotated
                    .contains(&(index, PullRequestDiffSide::Right, line))
            {
                items.push(Item::Annotation {
                    file: index,
                    side: PullRequestDiffSide::Right,
                    line,
                });
            }
        };
        let mut cursor = 0;
        let mut hunk = usize::MAX;
        while cursor < view.rows.len() {
            let row = &view.rows[cursor];
            if row.hunk != hunk {
                hunk = row.hunk;
                let lines = unmodified_before(&view.file, hunk);
                if lines > 0 {
                    items.push(Item::Separator { file: index, lines });
                }
            }
            if !input.split {
                items.push(Item::Line {
                    file: index,
                    row: cursor,
                });
                annotations_after(&mut items, row.old_line, row.new_line);
                cursor += 1;
                continue;
            }
            match row.kind {
                DiffRowKind::Context => {
                    items.push(Item::Split {
                        file: index,
                        left: Some(cursor),
                        right: Some(cursor),
                    });
                    annotations_after(&mut items, row.old_line, row.new_line);
                    cursor += 1;
                }
                DiffRowKind::Deletion | DiffRowKind::Addition => {
                    // A run of removals and the additions that follow it sit side by side.
                    let mut deletions = Vec::new();
                    let mut additions = Vec::new();
                    while cursor < view.rows.len()
                        && view.rows[cursor].hunk == hunk
                        && view.rows[cursor].kind == DiffRowKind::Deletion
                    {
                        deletions.push(cursor);
                        cursor += 1;
                    }
                    while cursor < view.rows.len()
                        && view.rows[cursor].hunk == hunk
                        && view.rows[cursor].kind == DiffRowKind::Addition
                    {
                        additions.push(cursor);
                        cursor += 1;
                    }
                    for pair in 0..deletions.len().max(additions.len()) {
                        let left = deletions.get(pair).copied();
                        let right = additions.get(pair).copied();
                        items.push(Item::Split {
                            file: index,
                            left,
                            right,
                        });
                        annotations_after(
                            &mut items,
                            left.and_then(|ix| view.rows[ix].old_line),
                            right.and_then(|ix| view.rows[ix].new_line),
                        );
                    }
                }
            }
        }
    }
    if input.footer {
        items.push(Item::Footer);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_request::code_tab::patch::{ParsedPatch, parse_patch};
    use pretty_assertions::assert_eq;

    const PATCH: &str = "diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -3,4 +3,5 @@
 keep
-old1
-old2
+new1
+new2
+new3
 tail
@@ -20,1 +21,1 @@
-x
+y
";

    fn views() -> Vec<FileView> {
        match parse_patch(PATCH) {
            Some(ParsedPatch::Files(files)) => files.into_iter().map(FileView::new).collect(),
            other => panic!("{other:?}"),
        }
    }

    fn build(
        split: bool,
        folded: bool,
        annotated: HashSet<AnnotationKey>,
        footer: bool,
    ) -> Vec<Item> {
        let views = views();
        build_items(&BuildInput {
            files: &views,
            collapsed: &|_| folded,
            split,
            annotated: &annotated,
            footer,
        })
    }

    #[test]
    fn unified_lists_a_header_separators_and_every_row() {
        let items = build(false, false, HashSet::new(), false);
        assert_eq!(items[0], Item::FileHeader { file: 0 });
        assert_eq!(items[1], Item::Separator { file: 0, lines: 2 });
        // keep, old1, old2, new1, new2, new3, tail
        assert_eq!(
            items[2..9]
                .iter()
                .filter(|item| matches!(item, Item::Line { .. }))
                .count(),
            7
        );
        // Between hunks: lines 8..=20 of the new file are not shown.
        assert!(items.contains(&Item::Separator { file: 0, lines: 13 }));
        assert_eq!(items.len(), 1 + 1 + 7 + 1 + 2);
    }

    #[test]
    fn a_folded_file_is_only_its_header() {
        assert_eq!(
            build(false, true, HashSet::new(), false),
            vec![Item::FileHeader { file: 0 }]
        );
    }

    #[test]
    fn annotations_follow_the_line_they_hang_under() {
        let mut annotated = HashSet::new();
        annotated.insert((0, PullRequestDiffSide::Right, 5));
        annotated.insert((0, PullRequestDiffSide::Left, 4));
        let items = build(false, false, annotated, false);
        let position = |wanted: &Item| items.iter().position(|item| item == wanted).unwrap();
        let old2 = position(&Item::Annotation {
            file: 0,
            side: PullRequestDiffSide::Left,
            line: 4,
        });
        // Row order: keep(3/3) old1(4) old2(5) new1(4) new2(5)...; left 4 hangs under old1.
        assert!(matches!(items[old2 - 1], Item::Line { row: 1, .. }));
        let new2 = position(&Item::Annotation {
            file: 0,
            side: PullRequestDiffSide::Right,
            line: 5,
        });
        assert!(matches!(items[new2 - 1], Item::Line { row: 4, .. }));
    }

    #[test]
    fn split_pairs_removals_with_additions() {
        let items = build(true, false, HashSet::new(), false);
        let splits: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                Item::Split { left, right, .. } => Some((*left, *right)),
                _ => None,
            })
            .collect();
        assert_eq!(
            splits,
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(3)),
                (Some(2), Some(4)),
                (None, Some(5)),
                (Some(6), Some(6)),
                (Some(7), Some(8)),
            ]
        );
    }

    #[test]
    fn a_footer_closes_a_diff_that_is_still_paging() {
        assert_eq!(
            build(false, true, HashSet::new(), true).last(),
            Some(&Item::Footer)
        );
    }
}
