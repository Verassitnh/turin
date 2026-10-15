//! Port of upstream/apps/server/src/pullRequest/pullRequestReviewGuideNormalize.ts

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString};
use turin_contracts::pull_request_review_guide::{
    PullRequestReviewGuideAttention, PullRequestReviewGuideExcerpt,
    PullRequestReviewGuideExcerptSide, PullRequestReviewGuideFile, PullRequestReviewGuideNote,
    PullRequestReviewGuideStop,
};
use turin_provider_core::text_generation::{ReviewGuideExcerptOutput, ReviewGuideGenerationResult};

use crate::shared::js::js_trim;

/// Longest excerpt kept. The reader wants the lines that carry the idea, not the whole hunk.
const MAX_EXCERPT_LINES: u64 = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: u64,
    pub end: u64,
}

/// The lines a change touched in one file, on each side of the diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileHunks {
    pub new: Vec<LineRange>,
    pub old: Vec<LineRange>,
}

/// What a guide is held to: the changed paths, and the lines each touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewGuideChange {
    /// Every changed path, in diff order, named as `git diff --name-only` names it.
    pub files: Vec<String>,
    pub hunks: HashMap<String, FileHunks>,
}

fn hunk_range(start: &str, count: Option<&str>) -> Option<LineRange> {
    let start: u64 = start.parse().ok()?;
    let lines: u64 = match count {
        None => 1,
        Some(count) => count.parse().ok()?,
    };
    (lines != 0).then(|| LineRange {
        start,
        end: start + lines - 1,
    })
}

/// Reads `git diff --name-only -z` and `git diff --unified=0` of the same range. Hunks are keyed
/// by the new path, or the old one for a deleted file, so both sides of a renamed file sit under
/// the name the model sees in `--stat`.
pub fn parse_review_guide_change(name_only: &str, patch: &str) -> ReviewGuideChange {
    static HUNK_HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^@@ -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? @@").unwrap()
    });
    let mut hunks: HashMap<String, FileHunks> = HashMap::new();
    let mut old_path: Option<String> = None;
    let mut current: Option<String> = None;
    // Inside a file's hunks a `--- ` or `+++ ` line is a removed or added source line, not a header.
    let mut in_hunks = false;
    for line in patch.split('\n') {
        if line.starts_with("diff --git ") {
            old_path = None;
            current = None;
            in_hunks = false;
        } else if !in_hunks && line.starts_with("--- ") {
            old_path = line.strip_prefix("--- a/").map(str::to_owned);
        } else if !in_hunks && line.starts_with("+++ ") {
            let Some(path) = line
                .strip_prefix("+++ b/")
                .map(str::to_owned)
                .or(old_path.clone())
            else {
                continue;
            };
            hunks.entry(path.clone()).or_default();
            current = Some(path);
        } else if let Some(path) = current.as_ref() {
            let Some(header) = HUNK_HEADER.captures(line) else {
                continue;
            };
            in_hunks = true;
            let removed = hunk_range(&header[1], header.get(2).map(|m| m.as_str()));
            let added = hunk_range(&header[3], header.get(4).map(|m| m.as_str()));
            let file = hunks.get_mut(path).expect("inserted with the path");
            if let Some(removed) = removed {
                file.old.push(removed);
            }
            if let Some(added) = added {
                file.new.push(added);
            }
        }
    }
    ReviewGuideChange {
        files: name_only
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect(),
        hunks,
    }
}

/// Narrows an excerpt to the changed lines it covers: from the first changed line it overlaps to
/// the last. `None` when it covers none, which means the model pointed somewhere the change is not.
/// The model's lines are signed and may be zero; a clamped range is inside a changed hunk, so it is
/// positive.
fn clamp_excerpt(
    excerpt: &ReviewGuideExcerptOutput,
    change: &ReviewGuideChange,
) -> Option<PullRequestReviewGuideExcerpt> {
    let start = excerpt.start.min(excerpt.end);
    let end = excerpt.start.max(excerpt.end);
    let ranges = change
        .hunks
        .get(excerpt.path.as_str())
        .map(|hunks| match excerpt.side {
            PullRequestReviewGuideExcerptSide::New => &hunks.new,
            PullRequestReviewGuideExcerptSide::Old => &hunks.old,
        });
    let overlapping: Vec<&LineRange> = ranges
        .into_iter()
        .flatten()
        .filter(|hunk| (hunk.start as i64) <= end && start <= hunk.end as i64)
        .collect();
    let (first, last) = (overlapping.first()?, overlapping.last()?);
    let start = start.max(first.start as i64);
    let end = end.min(last.end as i64);
    Some(PullRequestReviewGuideExcerpt {
        path: TrimmedNonEmptyString::new(excerpt.path.as_str()).ok()?,
        side: excerpt.side,
        start: PositiveInt::new(start as u64)?,
        end: PositiveInt::new(end as u64)?,
        caption: excerpt.caption.clone(),
    })
}

fn positive(value: u64, fallback: PositiveInt) -> PositiveInt {
    PositiveInt::new(value).unwrap_or(fallback)
}

/// Joins a stop's excerpts that overlap or touch in one file, then caps each one's length.
fn merge_excerpts(
    excerpts: Vec<PullRequestReviewGuideExcerpt>,
) -> Vec<PullRequestReviewGuideExcerpt> {
    let mut merged: Vec<PullRequestReviewGuideExcerpt> = Vec::new();
    for excerpt in excerpts {
        let index = merged.iter().position(|other| {
            other.path == excerpt.path
                && other.side == excerpt.side
                && other.start.get() <= excerpt.end.get() + 1
                && excerpt.start.get() <= other.end.get() + 1
        });
        let Some(index) = index else {
            merged.push(excerpt);
            continue;
        };
        let other = &merged[index];
        let caption = if other.caption.is_empty() {
            excerpt.caption.clone()
        } else {
            other.caption.clone()
        };
        merged[index] = PullRequestReviewGuideExcerpt {
            start: other.start.min(excerpt.start),
            end: other.end.max(excerpt.end),
            caption,
            ..other.clone()
        };
    }
    merged
        .into_iter()
        .map(|excerpt| PullRequestReviewGuideExcerpt {
            end: positive(
                excerpt
                    .end
                    .get()
                    .min(excerpt.start.get() + MAX_EXCERPT_LINES - 1),
                excerpt.end,
            ),
            ..excerpt
        })
        .collect()
}

fn non_empty(value: &str) -> bool {
    !js_trim(value).is_empty()
}

/// Holds a generated guide to the change it describes. A model's line numbers and ids are
/// suggestions: excerpts are clamped to changed lines, notes must sit inside what is left, and any
/// changed file the tour skipped lands in a closing skim stop. Returns the stops the guide keeps.
pub fn normalize_review_guide(
    guide: ReviewGuideGenerationResult,
    change: &ReviewGuideChange,
) -> Vec<PullRequestReviewGuideStop> {
    let changed: HashSet<&str> = change.files.iter().map(String::as_str).collect();
    let in_change = |path: &str| changed.contains(path);

    let shaped: Vec<PullRequestReviewGuideStop> = guide
        .stops
        .into_iter()
        .filter_map(|stop| {
            if !non_empty(&stop.title) {
                return None;
            }
            let excerpts = merge_excerpts(
                stop.excerpts
                    .iter()
                    .filter(|excerpt| in_change(&excerpt.path))
                    .filter_map(|excerpt| clamp_excerpt(excerpt, change))
                    .collect(),
            );
            let attention = stop.attention.and_then(|attention| {
                if !non_empty(&attention.note) {
                    return None;
                }
                Some(PullRequestReviewGuideAttention {
                    kind: attention.kind,
                    note: TrimmedNonEmptyString::new(attention.note.as_str()).ok()?,
                })
            });
            let files = stop
                .files
                .into_iter()
                .filter(|file| in_change(&file.path))
                .filter_map(|file| {
                    Some(PullRequestReviewGuideFile {
                        path: TrimmedNonEmptyString::new(file.path.as_str()).ok()?,
                        note: Some(file.note).filter(|note| !js_trim(note).is_empty()),
                    })
                })
                .collect();
            let notes = stop
                .notes
                .into_iter()
                .filter_map(|note| {
                    let line = u64::try_from(note.line).ok().and_then(PositiveInt::new)?;
                    let inside = non_empty(&note.title)
                        && excerpts.iter().any(|excerpt| {
                            excerpt.path.as_str() == note.path
                                && excerpt.start.get() <= line.get()
                                && line.get() <= excerpt.end.get()
                        });
                    if !inside {
                        return None;
                    }
                    Some(PullRequestReviewGuideNote {
                        path: TrimmedNonEmptyString::new(note.path.as_str()).ok()?,
                        line,
                        title: TrimmedNonEmptyString::new(note.title.as_str()).ok()?,
                        body: note.body,
                    })
                })
                .collect();
            Some(PullRequestReviewGuideStop {
                title: TrimmedNonEmptyString::new(stop.title.as_str()).ok()?,
                summary: stop.summary,
                attention,
                excerpts,
                files,
                notes,
                questions: stop
                    .questions
                    .into_iter()
                    .filter(|question| non_empty(question))
                    .filter_map(|question| TrimmedNonEmptyString::new(question.as_str()).ok())
                    .collect(),
            })
        })
        .collect();

    // A file can be excerpted in several stops, its shape in one and its implementation in another.
    // A files entry is for a file the tour never shows, so it is listed once and only then.
    let mut placed: HashSet<String> = shaped
        .iter()
        .flat_map(|stop| stop.excerpts.iter().map(|excerpt| excerpt.path.to_string()))
        .collect();
    let mut stops: Vec<PullRequestReviewGuideStop> = shaped
        .into_iter()
        .map(|stop| {
            let files: Vec<PullRequestReviewGuideFile> = stop
                .files
                .iter()
                .filter(|file| !placed.contains(file.path.as_str()))
                .cloned()
                .collect();
            for file in &files {
                placed.insert(file.path.to_string());
            }
            PullRequestReviewGuideStop { files, ..stop }
        })
        .collect();
    let unplaced: Vec<&String> = change
        .files
        .iter()
        .filter(|path| !placed.contains(*path))
        .collect();
    if !unplaced.is_empty() {
        stops.push(PullRequestReviewGuideStop {
            title: TrimmedNonEmptyString::new("Other files").expect("non-blank literal"),
            summary: "Changed files the tour does not otherwise cover.".to_owned(),
            attention: None,
            excerpts: vec![],
            files: unplaced
                .into_iter()
                .filter_map(|path| TrimmedNonEmptyString::new(path).ok())
                .map(|path| PullRequestReviewGuideFile { path, note: None })
                .collect(),
            notes: vec![],
            questions: vec![],
        });
    }

    stops
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_provider_core::text_generation::{
        ReviewGuideFileOutput, ReviewGuideNoteOutput, ReviewGuideStopOutput,
    };

    // `git diff --unified=0` of: src/a.ts with lines 10-14 and 40-59 added and an old line 30
    // removed; src/gone.ts deleted (old lines 1-20); docs/readme.md with line 3 added.
    fn patch() -> String {
        [
            "diff --git a/src/a.ts b/src/a.ts",
            "index 1111111..2222222 100644",
            "--- a/src/a.ts",
            "+++ b/src/a.ts",
            "@@ -9,0 +10,5 @@ export const x = 1;",
            "+added",
            "@@ -30 +34,0 @@ function y() {",
            "-removed",
            "@@ -35,0 +40,20 @@",
            "+added",
            "diff --git a/src/gone.ts b/src/gone.ts",
            "deleted file mode 100644",
            "--- a/src/gone.ts",
            "+++ /dev/null",
            "@@ -1,20 +0,0 @@",
            "-removed",
            "diff --git a/docs/readme.md b/docs/readme.md",
            "--- a/docs/readme.md",
            "+++ b/docs/readme.md",
            "@@ -2,0 +3 @@",
            "+added",
            "",
        ]
        .join("\n")
    }

    fn change() -> ReviewGuideChange {
        parse_review_guide_change("src/a.ts\0src/gone.ts\0docs/readme.md\0", &patch())
    }

    fn t(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    // The model's stops, as provider-core hands them over. Lines are signed and titles may be empty.

    /// A raw stop titled `Stop`, shaped by `edit`.
    fn stop(edit: impl FnOnce(&mut ReviewGuideStopOutput)) -> ReviewGuideStopOutput {
        let mut stop = ReviewGuideStopOutput {
            title: "Stop".to_owned(),
            summary: String::new(),
            attention: None,
            excerpts: vec![],
            files: vec![],
            notes: vec![],
            questions: vec![],
        };
        edit(&mut stop);
        stop
    }

    fn guide(stops: Vec<ReviewGuideStopOutput>) -> ReviewGuideGenerationResult {
        ReviewGuideGenerationResult { stops }
    }

    fn raw_excerpt(path: &str, start: i64, end: i64) -> ReviewGuideExcerptOutput {
        raw_excerpt_on(path, start, end, PullRequestReviewGuideExcerptSide::New)
    }

    fn raw_excerpt_on(
        path: &str,
        start: i64,
        end: i64,
        side: PullRequestReviewGuideExcerptSide,
    ) -> ReviewGuideExcerptOutput {
        ReviewGuideExcerptOutput {
            path: path.to_owned(),
            side,
            start,
            end,
            caption: String::new(),
        }
    }

    fn raw_file(path: &str, note: Option<&str>) -> ReviewGuideFileOutput {
        ReviewGuideFileOutput {
            path: path.to_owned(),
            note: note.unwrap_or_default().to_owned(),
        }
    }

    fn raw_note(path: &str, line: i64, title: &str) -> ReviewGuideNoteOutput {
        ReviewGuideNoteOutput {
            path: path.to_owned(),
            line,
            title: title.to_owned(),
            body: String::new(),
        }
    }

    // The stops the normalizer keeps, as the contract stores them.

    fn excerpt(path: &str, start: u64, end: u64) -> PullRequestReviewGuideExcerpt {
        excerpt_on(path, start, end, PullRequestReviewGuideExcerptSide::New)
    }

    fn excerpt_on(
        path: &str,
        start: u64,
        end: u64,
        side: PullRequestReviewGuideExcerptSide,
    ) -> PullRequestReviewGuideExcerpt {
        PullRequestReviewGuideExcerpt {
            path: t(path),
            side,
            start: PositiveInt::new(start).unwrap(),
            end: PositiveInt::new(end).unwrap(),
            caption: String::new(),
        }
    }

    fn file(path: &str, note: Option<&str>) -> PullRequestReviewGuideFile {
        PullRequestReviewGuideFile {
            path: t(path),
            note: note.map(str::to_owned),
        }
    }

    fn range(start: u64, end: u64) -> LineRange {
        LineRange { start, end }
    }

    #[test]
    fn reads_a_removed_or_added_line_that_looks_like_a_file_header_as_a_source_line() {
        let tricky = [
            "diff --git a/notes.md b/notes.md",
            "--- a/notes.md",
            "+++ b/notes.md",
            "@@ -3,1 +3,2 @@",
            "--- a/old-heading",
            "+++ b/other.ts",
            "+tail",
            "@@ -9,0 +10,1 @@",
            "+later",
            "",
        ]
        .join("\n");
        let parsed = parse_review_guide_change("notes.md\0", &tricky);
        assert_eq!(parsed.hunks.keys().collect::<Vec<_>>(), vec!["notes.md"]);
        assert_eq!(
            parsed.hunks["notes.md"],
            FileHunks {
                new: vec![range(3, 4), range(10, 10)],
                old: vec![range(3, 3)]
            }
        );
    }

    #[test]
    fn reads_each_sides_hunks_keying_a_deleted_file_by_its_old_path() {
        let change = change();
        assert_eq!(
            change.files,
            vec!["src/a.ts", "src/gone.ts", "docs/readme.md"]
        );
        assert_eq!(
            change.hunks["src/a.ts"],
            FileHunks {
                new: vec![range(10, 14), range(40, 59)],
                old: vec![range(30, 30)]
            }
        );
        assert_eq!(
            change.hunks["src/gone.ts"],
            FileHunks {
                new: vec![],
                old: vec![range(1, 20)]
            }
        );
        assert_eq!(
            change.hunks["docs/readme.md"],
            FileHunks {
                new: vec![range(3, 3)],
                old: vec![]
            }
        );
    }

    #[test]
    fn clamps_excerpts_to_changed_lines_merges_touching_ones_and_caps_their_length() {
        let result = normalize_review_guide(
            guide(vec![stop(|s| {
                s.excerpts = vec![
                    // Starts on context before the hunk: clamped to the hunk.
                    raw_excerpt("src/a.ts", 5, 12),
                    // Touches the previous one once clamped: merged.
                    raw_excerpt("src/a.ts", 13, 14),
                    // Spans the whole second hunk and beyond: clamped to it.
                    raw_excerpt("src/a.ts", 38, 80),
                    // On no changed line: dropped.
                    raw_excerpt("src/a.ts", 20, 25),
                    // Removed code shown from the old side.
                    raw_excerpt_on("src/gone.ts", 2, 8, PullRequestReviewGuideExcerptSide::Old),
                ];
                s.files = vec![raw_file("docs/readme.md", Some(""))];
            })]),
            &change(),
        );
        assert_eq!(
            result[0].excerpts,
            vec![
                excerpt("src/a.ts", 10, 14),
                excerpt("src/a.ts", 40, 59),
                excerpt_on("src/gone.ts", 2, 8, PullRequestReviewGuideExcerptSide::Old),
            ]
        );
        assert_eq!(result[0].files, vec![file("docs/readme.md", None)]);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn keeps_a_long_excerpt_to_its_first_25_lines() {
        let long = parse_review_guide_change(
            "big.ts\0",
            "--- a/big.ts\n+++ b/big.ts\n@@ -0,0 +1,100 @@\n",
        );
        let result = normalize_review_guide(
            guide(vec![stop(|s| {
                s.excerpts = vec![raw_excerpt("big.ts", 1, 100)]
            })]),
            &long,
        );
        assert_eq!(result[0].excerpts, vec![excerpt("big.ts", 1, 25)]);
    }

    #[test]
    fn drops_notes_outside_the_stops_remaining_excerpts() {
        let result = normalize_review_guide(
            guide(vec![stop(|s| {
                s.excerpts = vec![raw_excerpt("src/a.ts", 5, 12)];
                s.notes = vec![
                    raw_note("src/a.ts", 11, "Inside"),
                    raw_note("src/a.ts", 6, "Clamped away"),
                    raw_note("src/other.ts", 11, "Other file"),
                ];
            })]),
            &change(),
        );
        let titles: Vec<&str> = result[0].notes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, vec!["Inside"]);
    }

    #[test]
    fn drops_a_note_on_a_line_the_model_wrote_as_zero_or_negative() {
        let result = normalize_review_guide(
            guide(vec![stop(|s| {
                s.excerpts = vec![raw_excerpt("src/a.ts", 10, 14)];
                s.notes = vec![
                    raw_note("src/a.ts", 0, "Zero"),
                    raw_note("src/a.ts", -3, "Negative"),
                    raw_note("src/a.ts", 12, "Kept"),
                ];
            })]),
            &change(),
        );
        let titles: Vec<&str> = result[0].notes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, vec!["Kept"]);
    }

    #[test]
    fn drops_a_stop_whose_title_is_blank_and_keeps_the_rest_of_the_guide() {
        let result = normalize_review_guide(
            guide(vec![
                stop(|s| {
                    s.title = "   ".to_owned();
                    s.excerpts = vec![raw_excerpt("src/a.ts", 10, 12)];
                }),
                stop(|s| {
                    s.title = "Kept".to_owned();
                    s.excerpts = vec![raw_excerpt("src/a.ts", 40, 41)];
                }),
            ]),
            &change(),
        );
        let titles: Vec<&str> = result.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Kept", "Other files"]);
    }

    #[test]
    fn drops_excerpts_and_files_outside_the_change() {
        let result = normalize_review_guide(
            guide(vec![stop(|s| {
                s.excerpts = vec![raw_excerpt("src/unchanged.ts", 1, 5)];
                s.files = vec![
                    raw_file("src/a.ts", Some("edited")),
                    raw_file("src/unchanged.ts", None),
                ];
            })]),
            &change(),
        );
        assert_eq!(result[0].excerpts, vec![]);
        assert_eq!(result[0].files, vec![file("src/a.ts", Some("edited"))]);
    }

    #[test]
    fn puts_changed_files_the_tour_left_out_into_a_closing_stop() {
        let result = normalize_review_guide(
            guide(vec![stop(|s| {
                s.excerpts = vec![raw_excerpt("src/a.ts", 10, 12)];
            })]),
            &change(),
        );
        let last = result.last().unwrap();
        assert_eq!(last.title.as_str(), "Other files");
        assert_eq!(
            last.files,
            vec![file("src/gone.ts", None), file("docs/readme.md", None)]
        );
    }

    #[test]
    fn lets_stops_show_different_parts_of_one_file_and_lists_an_unshown_file_once() {
        let result = normalize_review_guide(
            guide(vec![
                stop(|s| {
                    s.title = "Shape".to_owned();
                    s.excerpts = vec![raw_excerpt("src/a.ts", 10, 14)];
                    s.files = vec![raw_file("docs/readme.md", None)];
                }),
                stop(|s| {
                    s.title = "Implementation".to_owned();
                    s.excerpts = vec![raw_excerpt("src/a.ts", 40, 50)];
                    s.files = vec![
                        raw_file("src/a.ts", None),
                        raw_file("docs/readme.md", None),
                        raw_file("src/gone.ts", None),
                    ];
                }),
            ]),
            &change(),
        );
        let shape: Vec<_> = result
            .iter()
            .map(|s| {
                (
                    s.title.to_string(),
                    s.excerpts.iter().map(|e| e.start.get()).collect::<Vec<_>>(),
                    s.files.clone(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    "Shape".to_owned(),
                    vec![10],
                    vec![file("docs/readme.md", None)]
                ),
                (
                    "Implementation".to_owned(),
                    vec![40],
                    vec![file("src/gone.ts", None)]
                ),
            ]
        );
    }
}
