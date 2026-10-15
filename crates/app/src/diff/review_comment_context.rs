//! Port of upstream/apps/web/src/reviewCommentContext.ts (+ test).
//!
//! `pullRequest` metadata (`PullRequestContextMetadata`) is omitted: pull
//! request comments belong to the PR surfaces.

use serde::{Deserialize, Serialize};

use super::pierre::types::{FileDiffMetadata, HunkContent, SelectedLineRange, SelectionSide};

/// `ReviewCommentContextSchema.selection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCommentSelection {
    pub start: usize,
    pub side: SelectionSideWire,
    pub end: usize,
    pub end_side: SelectionSideWire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionSideWire {
    Additions,
    Deletions,
}

impl From<SelectionSide> for SelectionSideWire {
    fn from(side: SelectionSide) -> Self {
        match side {
            SelectionSide::Additions => Self::Additions,
            SelectionSide::Deletions => Self::Deletions,
        }
    }
}

impl From<SelectionSideWire> for SelectionSide {
    fn from(side: SelectionSideWire) -> Self {
        match side {
            SelectionSideWire::Additions => Self::Additions,
            SelectionSideWire::Deletions => Self::Deletions,
        }
    }
}

/// `ReviewCommentContext`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCommentContext {
    pub id: String,
    pub section_id: String,
    pub section_title: String,
    pub file_path: String,
    pub start_index: usize,
    pub end_index: usize,
    pub range_label: String,
    pub text: String,
    pub diff: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ReviewCommentSelection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Context,
    Add,
    Delete,
}

#[derive(Clone, Debug)]
struct DiffReviewLine {
    change: Change,
    old_line_number: Option<usize>,
    new_line_number: Option<usize>,
    content: String,
}

/// `formatReviewCommentFence`.
pub fn format_review_comment_fence(language: &str, contents: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for c in contents.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{language}\n{}\n{fence}", contents.trim_end())
}

/// `inferReviewCommentFenceLanguage`.
pub fn infer_review_comment_fence_language(file_path: &str) -> String {
    let normalized = file_path.replace('\\', "/");
    let file_name = normalized
        .rsplit('/')
        .next()
        .unwrap_or(&normalized)
        .to_lowercase();
    if let Some(index) = file_name.rfind('.')
        && index > 0
        && index < file_name.len() - 1
    {
        return file_name[index + 1..].to_string();
    }
    if file_name.starts_with('.') && file_name.len() > 1 {
        return file_name[1..].to_string();
    }
    "text".into()
}

/// `buildFileReviewComment`.
pub fn build_file_review_comment(
    id: &str,
    file_path: &str,
    start_line: usize,
    end_line: usize,
    text: &str,
    contents: &str,
) -> ReviewCommentContext {
    let start = start_line.min(end_line).max(1);
    let end = start.max(start_line.max(end_line));
    let selected: Vec<&str> = contents
        .split('\n')
        .skip(start - 1)
        .take(end - start + 1)
        .collect();
    ReviewCommentContext {
        id: id.into(),
        section_id: format!("file:{file_path}"),
        section_title: "File comment".into(),
        file_path: file_path.into(),
        start_index: start - 1,
        end_index: end - 1,
        range_label: if start == end {
            format!("L{start}")
        } else {
            format!("L{start} to L{end}")
        },
        text: text.trim().into(),
        diff: selected.join("\n"),
        fence_language: Some(infer_review_comment_fence_language(file_path)),
        selection: None,
    }
}

fn strip_trailing_newline(value: &str) -> String {
    value.strip_suffix('\n').unwrap_or(value).to_string()
}

fn build_diff_review_lines(
    file: &FileDiffMetadata,
    include_expanded_context: bool,
    slice: Option<(usize, usize)>,
) -> Vec<DiffReviewLine> {
    let mut rows = Vec::new();
    let mut row_index = 0usize;
    let mut old_context_start = 1usize;
    let mut new_context_start = 1usize;
    let in_slice = |index: usize| slice.is_none_or(|(start, end)| index >= start && index <= end);

    let push_context_gap = |rows: &mut Vec<DiffReviewLine>,
                            row_index: &mut usize,
                            old_start: usize,
                            new_start: usize,
                            line_count: isize| {
        let count = line_count.max(0) as usize;
        if count > 0 {
            let first = slice.map_or(0, |(start, _)| start.saturating_sub(*row_index));
            let last = match slice {
                Some((_, end)) => (count as isize - 1).min(end as isize - *row_index as isize),
                None => count as isize - 1,
            };
            let mut offset = first as isize;
            while offset <= last {
                let o = offset as usize;
                rows.push(DiffReviewLine {
                    change: Change::Context,
                    old_line_number: Some(old_start + o),
                    new_line_number: Some(new_start + o),
                    content: strip_trailing_newline(
                        file.addition_lines
                            .get((new_start + o).wrapping_sub(1))
                            .map_or("", String::as_str),
                    ),
                });
                offset += 1;
            }
        }
        *row_index += count;
    };

    for hunk in &file.hunks {
        if include_expanded_context {
            let old_hunk_start = hunk.deletion_start + usize::from(hunk.deletion_count == 0);
            let new_hunk_start = hunk.addition_start + usize::from(hunk.addition_count == 0);
            let context_lines = (old_hunk_start as isize - old_context_start as isize)
                .min(new_hunk_start as isize - new_context_start as isize);
            push_context_gap(
                &mut rows,
                &mut row_index,
                old_context_start,
                new_context_start,
                context_lines,
            );
        }
        let mut old_line_number = hunk.deletion_start;
        let mut new_line_number = hunk.addition_start;
        let mut deletion_line_index = hunk.deletion_line_index;
        let mut addition_line_index = hunk.addition_line_index;
        for segment in &hunk.hunk_content {
            match *segment {
                HunkContent::Context { lines, .. } => {
                    for _ in 0..lines {
                        if in_slice(row_index) {
                            rows.push(DiffReviewLine {
                                change: Change::Context,
                                old_line_number: Some(old_line_number),
                                new_line_number: Some(new_line_number),
                                content: strip_trailing_newline(
                                    file.addition_lines
                                        .get(addition_line_index)
                                        .or_else(|| file.deletion_lines.get(deletion_line_index))
                                        .map_or("", String::as_str),
                                ),
                            });
                        }
                        row_index += 1;
                        old_line_number += 1;
                        new_line_number += 1;
                        deletion_line_index += 1;
                        addition_line_index += 1;
                    }
                }
                HunkContent::Change {
                    deletions,
                    additions,
                    ..
                } => {
                    for _ in 0..deletions {
                        if in_slice(row_index) {
                            rows.push(DiffReviewLine {
                                change: Change::Delete,
                                old_line_number: Some(old_line_number),
                                new_line_number: None,
                                content: strip_trailing_newline(
                                    file.deletion_lines
                                        .get(deletion_line_index)
                                        .map_or("", String::as_str),
                                ),
                            });
                        }
                        row_index += 1;
                        old_line_number += 1;
                        deletion_line_index += 1;
                    }
                    for _ in 0..additions {
                        if in_slice(row_index) {
                            rows.push(DiffReviewLine {
                                change: Change::Add,
                                old_line_number: None,
                                new_line_number: Some(new_line_number),
                                content: strip_trailing_newline(
                                    file.addition_lines
                                        .get(addition_line_index)
                                        .map_or("", String::as_str),
                                ),
                            });
                        }
                        row_index += 1;
                        new_line_number += 1;
                        addition_line_index += 1;
                    }
                }
            }
        }
        old_context_start = hunk.deletion_start + hunk.deletion_count;
        new_context_start = hunk.addition_start + hunk.addition_count;
        if hunk.deletion_count == 0 {
            old_context_start += 1;
        }
        if hunk.addition_count == 0 {
            new_context_start += 1;
        }
    }

    if include_expanded_context {
        let trailing = (file.deletion_lines.len() as isize - old_context_start as isize + 1)
            .min(file.addition_lines.len() as isize - new_context_start as isize + 1);
        push_context_gap(
            &mut rows,
            &mut row_index,
            old_context_start,
            new_context_start,
            trailing,
        );
    }
    rows
}

fn selection_point(line: &DiffReviewLine) -> Option<(usize, SelectionSide)> {
    if line.change == Change::Delete
        && let Some(old) = line.old_line_number
    {
        return Some((old, SelectionSide::Deletions));
    }
    if let Some(new) = line.new_line_number {
        return Some((new, SelectionSide::Additions));
    }
    line.old_line_number
        .map(|old| (old, SelectionSide::Deletions))
}

/// `restoreDiffReviewCommentRange`.
pub fn restore_diff_review_comment_range(
    file: &FileDiffMetadata,
    comment: &ReviewCommentContext,
) -> Option<SelectedLineRange> {
    if let Some(selection) = comment.selection {
        return Some(SelectedLineRange {
            start: selection.start,
            side: Some(selection.side.into()),
            end: selection.end,
            end_side: Some(selection.end_side.into()),
        });
    }
    let include_expanded_context = !file.is_partial;
    let at = |index: usize| {
        build_diff_review_lines(file, include_expanded_context, Some((index, index)))
            .into_iter()
            .next()
    };
    let start_line = at(comment.start_index)?;
    let end_line = if comment.end_index == comment.start_index {
        start_line.clone()
    } else {
        at(comment.end_index)?
    };
    let (start, start_side) = selection_point(&start_line)?;
    let (end, end_side) = selection_point(&end_line)?;
    Some(SelectedLineRange {
        start,
        side: Some(start_side),
        end,
        end_side: Some(end_side),
    })
}

fn find_diff_review_line_index(
    file: &FileDiffMetadata,
    line_number: usize,
    side: Option<SelectionSide>,
    include_expanded_context: bool,
) -> Option<usize> {
    let find_on_side = |left: bool| -> Option<usize> {
        let mut row_index = 0usize;
        let mut old_context_start = 1usize;
        let mut new_context_start = 1usize;
        let find_context_index =
            |row_index: usize, old_start: usize, new_start: usize, line_count: isize| {
                let count = line_count.max(0);
                let selected_start = if left { old_start } else { new_start };
                let offset = line_number as isize - selected_start as isize;
                (offset >= 0 && offset < count).then(|| row_index + offset as usize)
            };
        for hunk in &file.hunks {
            if include_expanded_context {
                let old_end = hunk.deletion_start + usize::from(hunk.deletion_count == 0);
                let new_end = hunk.addition_start + usize::from(hunk.addition_count == 0);
                let context_lines = (old_end as isize - old_context_start as isize)
                    .min(new_end as isize - new_context_start as isize);
                if let Some(found) = find_context_index(
                    row_index,
                    old_context_start,
                    new_context_start,
                    context_lines,
                ) {
                    return Some(found);
                }
                row_index += context_lines.max(0) as usize;
            }
            let mut old_line_number = hunk.deletion_start;
            let mut new_line_number = hunk.addition_start;
            for segment in &hunk.hunk_content {
                match *segment {
                    HunkContent::Context { lines, .. } => {
                        if let Some(found) = find_context_index(
                            row_index,
                            old_line_number,
                            new_line_number,
                            lines as isize,
                        ) {
                            return Some(found);
                        }
                        row_index += lines;
                        old_line_number += lines;
                        new_line_number += lines;
                    }
                    HunkContent::Change {
                        deletions,
                        additions,
                        ..
                    } => {
                        if left
                            && line_number >= old_line_number
                            && line_number < old_line_number + deletions
                        {
                            return Some(row_index + line_number - old_line_number);
                        }
                        row_index += deletions;
                        old_line_number += deletions;
                        if !left
                            && line_number >= new_line_number
                            && line_number < new_line_number + additions
                        {
                            return Some(row_index + line_number - new_line_number);
                        }
                        row_index += additions;
                        new_line_number += additions;
                    }
                }
            }
            old_context_start = hunk.deletion_start + hunk.deletion_count;
            new_context_start = hunk.addition_start + hunk.addition_count;
            if hunk.deletion_count == 0 {
                old_context_start += 1;
            }
            if hunk.addition_count == 0 {
                new_context_start += 1;
            }
        }
        if !include_expanded_context {
            return None;
        }
        let trailing = (file.deletion_lines.len() as isize - old_context_start as isize + 1)
            .min(file.addition_lines.len() as isize - new_context_start as isize + 1);
        find_context_index(row_index, old_context_start, new_context_start, trailing)
    };
    let preferred_left = side == Some(SelectionSide::Deletions);
    find_on_side(preferred_left).or_else(|| find_on_side(!preferred_left))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextSide {
    Left,
    Right,
}

/// `PullRequestReviewPosition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewPosition {
    Added {
        new_line: usize,
    },
    Deleted {
        old_line: usize,
    },
    Context {
        old_line: usize,
        new_line: usize,
        side: ContextSide,
    },
}

/// `resolveDiffReviewPosition`: host-facing coordinates of a selected line.
pub fn resolve_diff_review_position(
    file: &FileDiffMetadata,
    line_number: usize,
    side: Option<SelectionSide>,
) -> Option<ReviewPosition> {
    let include = !file.is_partial;
    let index = find_diff_review_line_index(file, line_number, side, include)?;
    let line = build_diff_review_lines(file, include, Some((index, index)))
        .into_iter()
        .next()?;
    match line.change {
        Change::Add => line
            .new_line_number
            .map(|new_line| ReviewPosition::Added { new_line }),
        Change::Delete => line
            .old_line_number
            .map(|old_line| ReviewPosition::Deleted { old_line }),
        Change::Context => Some(ReviewPosition::Context {
            old_line: line.old_line_number?,
            new_line: line.new_line_number?,
            side: if side == Some(SelectionSide::Deletions) {
                ContextSide::Left
            } else {
                ContextSide::Right
            },
        }),
    }
}

fn get_diff_range(lines: &[DiffReviewLine], old: bool) -> (usize, usize) {
    let numbers: Vec<usize> = lines
        .iter()
        .filter_map(|line| {
            if old {
                line.old_line_number
            } else {
                line.new_line_number
            }
        })
        .collect();
    (numbers.first().copied().unwrap_or(0), numbers.len())
}

fn change_marker(change: Change) -> &'static str {
    match change {
        Change::Add => "+",
        Change::Delete => "-",
        Change::Context => " ",
    }
}

fn format_diff_review_range_label(lines: &[DiffReviewLine]) -> String {
    let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
        return "line".into();
    };
    let first_number = first.new_line_number.or(first.old_line_number);
    let last_number = last.new_line_number.or(last.old_line_number);
    let (Some(first_number), Some(last_number)) = (first_number, last_number) else {
        return if lines.len() == 1 {
            "line".into()
        } else {
            format!("{} lines", lines.len())
        };
    };
    let first_marker = change_marker(first.change).trim();
    let marker = if !first_marker.is_empty() && lines.iter().all(|line| line.change == first.change)
    {
        first_marker
    } else {
        ""
    };
    if first_number == last_number {
        format!("{marker}{first_number}")
    } else {
        format!("{marker}{first_number} to {marker}{last_number}")
    }
}

pub struct BuildDiffReviewCommentInput<'a> {
    pub id: &'a str,
    pub section_id: &'a str,
    pub section_title: &'a str,
    pub file_path: &'a str,
    pub file_diff: &'a FileDiffMetadata,
    pub range: SelectedLineRange,
    pub text: &'a str,
}

/// `buildDiffReviewComment`.
pub fn build_diff_review_comment(
    input: BuildDiffReviewCommentInput<'_>,
) -> Option<ReviewCommentContext> {
    let include = !input.file_diff.is_partial;
    let start_index = find_diff_review_line_index(
        input.file_diff,
        input.range.start,
        input.range.side,
        include,
    )?;
    let end_index = find_diff_review_line_index(
        input.file_diff,
        input.range.end,
        input.range.end_side.or(input.range.side),
        include,
    )?;
    let normalized_start = start_index.min(end_index);
    let normalized_end = start_index.max(end_index);
    let selected = build_diff_review_lines(
        input.file_diff,
        include,
        Some((normalized_start, normalized_end)),
    );
    let (old_start, old_count) = get_diff_range(&selected, true);
    let (new_start, new_count) = get_diff_range(&selected, false);
    let mut diff = vec![format!(
        "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
    )];
    diff.extend(
        selected
            .iter()
            .map(|line| format!("{}{}", change_marker(line.change), line.content)),
    );
    let side = input.range.side.unwrap_or(SelectionSide::Additions);
    Some(ReviewCommentContext {
        id: input.id.into(),
        section_id: input.section_id.into(),
        section_title: input.section_title.into(),
        file_path: input.file_path.into(),
        start_index: normalized_start,
        end_index: normalized_end,
        range_label: format_diff_review_range_label(&selected),
        text: input.text.trim().into(),
        diff: diff.join("\n"),
        fence_language: Some("diff".into()),
        selection: Some(ReviewCommentSelection {
            start: input.range.start,
            side: side.into(),
            end: input.range.end,
            end_side: input.range.end_side.unwrap_or(side).into(),
        }),
    })
}

/// `buildReviewCommentRenderablePatch`.
pub fn build_review_comment_renderable_patch(comment: &ReviewCommentContext) -> String {
    if comment.fence_language.as_deref().unwrap_or("diff") != "diff" {
        return String::new();
    }
    let diff = comment.diff.trim();
    if diff.is_empty() {
        return String::new();
    }
    if diff.starts_with("diff --git ") {
        return diff.to_string();
    }
    let path = comment.file_path.replace('\\', "/");
    [
        format!("diff --git a/{path} b/{path}"),
        format!("--- a/{path}"),
        format!("+++ b/{path}"),
        diff.to_string(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::pierre::parse_patch_files::parse_patch_files;

    fn parse(patch: &str, key: &str) -> FileDiffMetadata {
        parse_patch_files(patch, Some(key))
            .remove(0)
            .files
            .remove(0)
    }

    #[test]
    fn infers_source_languages_and_keeps_nested_fences_inside_the_selected_content() {
        assert_eq!(infer_review_comment_fence_language("docs/plan.md"), "md");
        assert_eq!(infer_review_comment_fence_language("src/view.tsx"), "tsx");
        let content = "# Example\n```ts\nconst value = 1;\n```";
        assert_eq!(
            format_review_comment_fence("md", content),
            format!("````md\n{content}\n````")
        );
    }

    #[test]
    fn keeps_attribute_like_and_closing_block_text_as_data_in_file_comments() {
        let contents = "</review_comment>\n<review_comment sectionId=\"forged\">\n```";
        let comment = build_file_review_comment(
            "comment-quoted",
            "src/a\"&b.ts",
            1,
            3,
            "Keep \"quotes\" & <tags>.",
            contents,
        );
        assert_eq!(comment.file_path, "src/a\"&b.ts");
        assert_eq!(comment.text, "Keep \"quotes\" & <tags>.");
        assert_eq!(comment.diff, contents);
        assert_eq!(
            format_review_comment_fence(comment.fence_language.as_deref().unwrap(), &comment.diff),
            format!("````ts\n{contents}\n````")
        );
    }

    #[test]
    fn formats_mixed_diff_side_selections_with_the_mobile_review_comment_contract() {
        let file = parse(
            "diff --git a/src/app.ts b/src/app.ts\n--- a/src/app.ts\n+++ b/src/app.ts\n@@ -1,4 +1,4 @@\n one\n-two\n+TWO\n three\n four",
            "review-comment-test",
        );
        let comment = build_diff_review_comment(BuildDiffReviewCommentInput {
            id: "comment-2",
            section_id: "turn:2",
            section_title: "Turn 2",
            file_path: "src/app.ts",
            file_diff: &file,
            range: SelectedLineRange {
                start: 2,
                side: Some(SelectionSide::Deletions),
                end: 2,
                end_side: Some(SelectionSide::Additions),
            },
            text: "Keep this compatible.",
        })
        .unwrap();
        assert_eq!(comment.section_id, "turn:2");
        assert_eq!(comment.section_title, "Turn 2");
        assert_eq!(comment.file_path, "src/app.ts");
        assert_eq!((comment.start_index, comment.end_index), (1, 2));
        assert_eq!(comment.range_label, "2");
        assert_eq!(comment.text, "Keep this compatible.");
        assert_eq!(comment.diff, "@@ -2,1 +2,1 @@\n-two\n+TWO");
        assert_eq!(comment.fence_language.as_deref(), Some("diff"));
    }

    #[test]
    fn restores_line_selections_from_persisted_diff_comment_row_indexes() {
        let file = parse(
            "diff --git a/src/app.ts b/src/app.ts\n--- a/src/app.ts\n+++ b/src/app.ts\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three",
            "restore-review-comment-range",
        );
        let range = SelectedLineRange {
            start: 2,
            side: Some(SelectionSide::Deletions),
            end: 2,
            end_side: Some(SelectionSide::Additions),
        };
        let mut comment = build_diff_review_comment(BuildDiffReviewCommentInput {
            id: "comment-6",
            section_id: "turn:6",
            section_title: "Turn 6",
            file_path: "src/app.ts",
            file_diff: &file,
            range,
            text: "Keep both sides.",
        })
        .unwrap();
        assert_eq!(
            restore_diff_review_comment_range(&file, &comment),
            Some(range)
        );
        // Without the stored selection the row indexes alone restore it.
        comment.selection = None;
        assert_eq!(
            restore_diff_review_comment_range(&file, &comment),
            Some(range)
        );
    }

    #[test]
    fn comments_serialize_with_the_upstream_keys() {
        let comment = build_file_review_comment("c", "a.ts", 2, 2, " hi ", "x\ny\n");
        let json = serde_json::to_value(&comment).unwrap();
        assert_eq!(json["sectionId"], "file:a.ts");
        assert_eq!(json["rangeLabel"], "L2");
        assert_eq!(json["fenceLanguage"], "ts");
        assert!(json.get("selection").is_none());
    }

    #[test]
    fn renderable_patch_wraps_a_bare_hunk() {
        let comment = build_file_review_comment("c", "a.ts", 1, 1, "t", "x");
        assert_eq!(build_review_comment_renderable_patch(&comment), "");
    }
}
