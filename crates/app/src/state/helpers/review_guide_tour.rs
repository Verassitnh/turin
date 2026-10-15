//! Port of upstream/packages/client-runtime/src/reviewGuideTour.ts

use indexmap::IndexMap;
use turin_contracts::pull_request_review_guide::{
    PullRequestReviewGuideExcerpt, PullRequestReviewGuideExcerptSide, PullRequestReviewGuideNote,
    PullRequestReviewGuideStop,
};

/// One step forward or back, held to the tour's first and last stops.
pub fn step_tour_position(current: i64, delta: i64, stop_count: i64) -> i64 {
    (current + delta).min(stop_count - 1).max(0)
}

/// The stop's files in reading order: the excerpted ones first, then the rest it owns.
pub fn stop_file_paths(stop: &PullRequestReviewGuideStop) -> Vec<&str> {
    let mut paths: Vec<&str> = Vec::new();
    let excerpted = stop.excerpts.iter().map(|excerpt| excerpt.path.as_str());
    let listed = stop.files.iter().map(|file| file.path.as_str());
    for path in excerpted.chain(listed) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// The rail's second line for a stop.
pub fn stop_subline(stop: &PullRequestReviewGuideStop) -> String {
    match stop_file_paths(stop).len() {
        0 => "Context, no code".to_owned(),
        1 => "1 file".to_owned(),
        count => format!("{count} files"),
    }
}

/// A note's marker: A, B, C... in the order the guide lists them, numbers past Z.
fn note_marker(index: usize) -> String {
    if index < 26 {
        char::from(b'A' + index as u8).to_string()
    } else {
        (index + 1).to_string()
    }
}

/// One run of lines in a hunk (`@pierre/diffs` `ContextContent` / `ChangeContent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkContent {
    Context {
        lines: usize,
        addition_line_index: usize,
        deletion_line_index: usize,
    },
    Change {
        deletions: usize,
        additions: usize,
        addition_line_index: usize,
        deletion_line_index: usize,
    },
}

/// The part of `@pierre/diffs` `Hunk` the tour reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub deletion_start: i64,
    pub addition_start: i64,
    pub hunk_content: Vec<HunkContent>,
}

/// The part of `@pierre/diffs` `FileDiffMetadata` the tour reads. `addition_lines` holds the
/// new side's lines (context included) and `deletion_lines` the old side's, each with its
/// trailing newline; content blocks index into them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiffMetadata {
    pub hunks: Vec<Hunk>,
    pub addition_lines: Vec<String>,
    pub deletion_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    Context,
    Addition,
    Deletion,
}

/// One rendered line of a file's diff, with where it sits on each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    /// Line number on the old side, `None` for an added line.
    pub old_line: Option<i64>,
    /// Line number on the new side, `None` for a removed line.
    pub new_line: Option<i64>,
    /// The next line number on each side when this row is reached, for hunk headers.
    pub old_at: i64,
    pub new_at: i64,
    pub text: String,
    /// Which hunk of the file the row came from; rows from two hunks are not adjacent.
    pub hunk: usize,
}

/// Every line of a parsed file diff as the unified view lists it.
pub fn file_diff_rows(file: &FileDiffMetadata) -> Vec<DiffRow> {
    let line_at = |lines: &[String], index: usize| lines.get(index).cloned().unwrap_or_default();
    let mut rows: Vec<DiffRow> = Vec::new();
    for (hunk_index, hunk) in file.hunks.iter().enumerate() {
        let mut old_at = hunk.deletion_start;
        let mut new_at = hunk.addition_start;
        for content in &hunk.hunk_content {
            match content {
                HunkContent::Context {
                    lines,
                    addition_line_index,
                    ..
                } => {
                    for offset in 0..*lines {
                        rows.push(DiffRow {
                            kind: DiffRowKind::Context,
                            old_line: Some(old_at),
                            new_line: Some(new_at),
                            old_at,
                            new_at,
                            text: line_at(&file.addition_lines, addition_line_index + offset),
                            hunk: hunk_index,
                        });
                        old_at += 1;
                        new_at += 1;
                    }
                }
                HunkContent::Change {
                    deletions,
                    additions,
                    addition_line_index,
                    deletion_line_index,
                } => {
                    for offset in 0..*deletions {
                        rows.push(DiffRow {
                            kind: DiffRowKind::Deletion,
                            old_line: Some(old_at),
                            new_line: None,
                            old_at,
                            new_at,
                            text: line_at(&file.deletion_lines, deletion_line_index + offset),
                            hunk: hunk_index,
                        });
                        old_at += 1;
                    }
                    for offset in 0..*additions {
                        rows.push(DiffRow {
                            kind: DiffRowKind::Addition,
                            old_line: None,
                            new_line: Some(new_at),
                            old_at,
                            new_at,
                            text: line_at(&file.addition_lines, addition_line_index + offset),
                            hunk: hunk_index,
                        });
                        new_at += 1;
                    }
                }
            }
        }
    }
    rows
}

/// `Pick<PullRequestReviewGuideExcerpt, "side" | "start" | "end">`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcerptRange {
    pub side: PullRequestReviewGuideExcerptSide,
    pub start: i64,
    pub end: i64,
}

impl From<&PullRequestReviewGuideExcerpt> for ExcerptRange {
    fn from(excerpt: &PullRequestReviewGuideExcerpt) -> Self {
        Self {
            side: excerpt.side,
            start: excerpt.start.get() as i64,
            end: excerpt.end.get() as i64,
        }
    }
}

/// The rows an excerpt covers. Its range is in one side's line numbers, so the other side's half
/// of a replacement at either edge comes along: an excerpt starting on an added line also shows
/// the lines it replaced, which is what makes it read as a change rather than as new code.
pub fn excerpt_rows(rows: &[DiffRow], excerpt: &ExcerptRange) -> Vec<DiffRow> {
    let line_of = |row: &DiffRow| match excerpt.side {
        PullRequestReviewGuideExcerptSide::New => row.new_line,
        PullRequestReviewGuideExcerptSide::Old => row.old_line,
    };
    let in_range = |row: &DiffRow| {
        line_of(row).is_some_and(|line| line >= excerpt.start && line <= excerpt.end)
    };
    let Some(mut first) = rows.iter().position(in_range) else {
        return Vec::new();
    };
    let mut last = rows.iter().rposition(in_range).unwrap_or(first);
    let same_hunk = |index: usize, anchor: usize| rows[index].hunk == rows[anchor].hunk;
    if excerpt.side == PullRequestReviewGuideExcerptSide::New
        && rows[first].kind == DiffRowKind::Addition
    {
        while first > 0
            && rows[first - 1].kind == DiffRowKind::Deletion
            && same_hunk(first - 1, first)
        {
            first -= 1;
        }
    }
    if excerpt.side == PullRequestReviewGuideExcerptSide::Old
        && rows[last].kind == DiffRowKind::Deletion
    {
        while last + 1 < rows.len()
            && rows[last + 1].kind == DiffRowKind::Addition
            && same_hunk(last + 1, last)
        {
            last += 1;
        }
    }
    rows[first..=last].to_vec()
}

fn row_prefix(kind: DiffRowKind) -> char {
    match kind {
        DiffRowKind::Context => ' ',
        DiffRowKind::Addition => '+',
        DiffRowKind::Deletion => '-',
    }
}

/// A unified patch of just these rows, so the shared diff renderer can draw a slice of a file
/// with its real line numbers instead of a second diff view being written for excerpts.
pub fn rows_to_patch(path: &str, previous_path: &str, rows: &[DiffRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        format!("diff --git a/{previous_path} b/{path}"),
        format!("--- a/{previous_path}"),
        format!("+++ b/{path}"),
    ];
    let mut start = 0;
    while start < rows.len() {
        let mut end = start;
        while end + 1 < rows.len() && rows[end + 1].hunk == rows[start].hunk {
            end += 1;
        }
        let group = &rows[start..=end];
        let head = &group[0];
        let old_count = group.iter().filter(|row| row.old_line.is_some()).count();
        let new_count = group.iter().filter(|row| row.new_line.is_some()).count();
        // Git names the line before an empty side, so a pure insertion after line 9 is `-9,0`.
        let old_start = if old_count == 0 {
            (head.old_at - 1).max(0)
        } else {
            head.old_at
        };
        let new_start = if new_count == 0 {
            (head.new_at - 1).max(0)
        } else {
            head.new_at
        };
        lines.push(format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
        ));
        for row in group {
            // `row.text.replace(/\r?\n$/, "")`
            let text = row
                .text
                .strip_suffix('\n')
                .map_or(row.text.as_str(), |text| {
                    text.strip_suffix('\r').unwrap_or(text)
                });
            lines.push(format!("{}{text}", row_prefix(row.kind)));
        }
        start = end + 1;
    }
    format!("{}\n", lines.join("\n"))
}

/// `Array.prototype.slice` on a slice: negative indexes count from the end.
fn js_slice<T>(items: &[T], start: i64, end: Option<i64>) -> &[T] {
    let length = items.len() as i64;
    let normalize = |index: i64| {
        if index < 0 {
            (length + index).max(0)
        } else {
            index.min(length)
        }
    };
    let start = normalize(start);
    let end = end.map_or(length, normalize);
    if start >= end {
        &[]
    } else {
        &items[start as usize..end as usize]
    }
}

/// The old side an excerpt expands against: the new file with only the excerpt's lines put back
/// as they were. The renderer fills the code around a hunk from both files and needs them to
/// agree outside it, which the real old file does not wherever another hunk changed lines. Only
/// the lines the reader can expand into are read, and those are the new file's, so the code
/// around the excerpt shows as it reads after the change. `None` when the excerpt spans hunks.
pub fn excerpt_expansion_old_contents(new_contents: &str, rows: &[DiffRow]) -> Option<String> {
    let head = rows.first()?;
    if rows.iter().any(|row| row.hunk != head.hunk) {
        return None;
    }
    // `newContents.match(/[^\n]*\n|[^\n]+$/g)`
    let new_lines: Vec<&str> = new_contents.split_inclusive('\n').collect();
    let before = head.new_at - 1;
    let new_count = rows.iter().filter(|row| row.new_line.is_some()).count() as i64;
    // The old side keeps its own numbering, so its prefix is as long as the old file's: trimmed
    // from the front, or padded there with lines no expansion reaches.
    let old_before = head.old_at - 1;
    let prefix = js_slice(&new_lines, 0, Some(before));
    let old_prefix: Vec<&str> = if old_before >= prefix.len() as i64 {
        let padding = (old_before - prefix.len() as i64) as usize;
        std::iter::repeat_n("\n", padding)
            .chain(prefix.iter().copied())
            .collect()
    } else {
        js_slice(prefix, prefix.len() as i64 - old_before, None).to_vec()
    };
    let mut contents: String = old_prefix.concat();
    for row in rows.iter().filter(|row| row.old_line.is_some()) {
        contents.push_str(&row.text);
        if !row.text.ends_with('\n') {
            contents.push('\n');
        }
    }
    contents.push_str(&js_slice(&new_lines, before + new_count, None).concat());
    Some(contents)
}

/// A stop's notes lettered in guide order, with the excerpt lines each one marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetteredNote<Note> {
    pub marker: String,
    pub note: Note,
}

/// A note's marker and title, as shown on the excerpt line it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteLineMarker {
    pub marker: String,
    pub title: String,
}

/// The markers on one excerpt line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcerptLineMarkers {
    pub line: u64,
    pub markers: Vec<NoteLineMarker>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetteredStopNotes<'a> {
    pub lettered: Vec<LetteredNote<&'a PullRequestReviewGuideNote>>,
    pub by_excerpt: Vec<Vec<ExcerptLineMarkers>>,
}

/// Letters a stop's notes and groups them by the excerpt line they sit on. A note outside every
/// excerpt keeps its letter in the list; it just has no line to mark.
pub fn letter_stop_notes(stop: &PullRequestReviewGuideStop) -> LetteredStopNotes<'_> {
    let lettered: Vec<LetteredNote<&PullRequestReviewGuideNote>> = stop
        .notes
        .iter()
        .enumerate()
        .map(|(index, note)| LetteredNote {
            marker: note_marker(index),
            note,
        })
        .collect();
    let by_excerpt = stop
        .excerpts
        .iter()
        .map(|excerpt| {
            let mut lines: IndexMap<u64, Vec<NoteLineMarker>> = IndexMap::new();
            for LetteredNote { marker, note } in &lettered {
                if note.path != excerpt.path
                    || note.line.get() < excerpt.start.get()
                    || note.line.get() > excerpt.end.get()
                {
                    continue;
                }
                lines
                    .entry(note.line.get())
                    .or_default()
                    .push(NoteLineMarker {
                        marker: marker.clone(),
                        title: note.title.as_str().to_owned(),
                    });
            }
            lines
                .into_iter()
                .map(|(line, markers)| ExcerptLineMarkers { line, markers })
                .collect()
        })
        .collect();
    LetteredStopNotes {
        lettered,
        by_excerpt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::LazyLock;

    const PATCH: &str = "diff --git a/src/tray.ts b/src/tray.ts
--- a/src/tray.ts
+++ b/src/tray.ts
@@ -10,5 +10,6 @@ function tray()
 const a = 1;
-const size = pick();
+const size = \"xs\";
+const variant = \"tray\";
 const b = 2;
 const c = 3;
 const d = 4;
@@ -40,3 +41,2 @@ function footer()
 keep();
-dropped();
 tail();
";

    /// A stand-in for `parsePatchFiles(patch)[0].files[0]` that understands just what the tests
    /// feed it: one file, `@@` headers, and ` `/`-`/`+` lines.
    fn parse(patch: &str) -> FileDiffMetadata {
        let header =
            regex::Regex::new(r"^@@ -([0-9]+)(?:,[0-9]+)? \+([0-9]+)(?:,[0-9]+)? @@").unwrap();
        let mut file = FileDiffMetadata {
            hunks: Vec::new(),
            addition_lines: Vec::new(),
            deletion_lines: Vec::new(),
        };
        for line in patch.lines() {
            if let Some(captures) = header.captures(line) {
                file.hunks.push(Hunk {
                    deletion_start: captures[1].parse().unwrap(),
                    addition_start: captures[2].parse().unwrap(),
                    hunk_content: Vec::new(),
                });
                continue;
            }
            let Some(hunk) = file.hunks.last_mut() else {
                continue;
            };
            let text = format!("{}\n", &line[1..]);
            let (addition_line_index, deletion_line_index) =
                (file.addition_lines.len(), file.deletion_lines.len());
            match line.as_bytes().first() {
                Some(b' ') => {
                    file.addition_lines.push(text.clone());
                    file.deletion_lines.push(text);
                    match hunk.hunk_content.last_mut() {
                        Some(HunkContent::Context { lines, .. }) => *lines += 1,
                        _ => hunk.hunk_content.push(HunkContent::Context {
                            lines: 1,
                            addition_line_index,
                            deletion_line_index,
                        }),
                    }
                }
                Some(&sign @ (b'-' | b'+')) => {
                    if !matches!(hunk.hunk_content.last(), Some(HunkContent::Change { .. })) {
                        hunk.hunk_content.push(HunkContent::Change {
                            deletions: 0,
                            additions: 0,
                            addition_line_index,
                            deletion_line_index,
                        });
                    }
                    let Some(HunkContent::Change {
                        deletions,
                        additions,
                        ..
                    }) = hunk.hunk_content.last_mut()
                    else {
                        unreachable!()
                    };
                    if sign == b'-' {
                        *deletions += 1;
                        file.deletion_lines.push(text);
                    } else {
                        *additions += 1;
                        file.addition_lines.push(text);
                    }
                }
                _ => {}
            }
        }
        file
    }

    static ROWS: LazyLock<Vec<DiffRow>> = LazyLock::new(|| file_diff_rows(&parse(PATCH)));

    fn new_range(start: i64, end: i64) -> ExcerptRange {
        ExcerptRange {
            side: PullRequestReviewGuideExcerptSide::New,
            start,
            end,
        }
    }

    fn old_range(start: i64, end: i64) -> ExcerptRange {
        ExcerptRange {
            side: PullRequestReviewGuideExcerptSide::Old,
            start,
            end,
        }
    }

    fn lines(rows: &[DiffRow]) -> Vec<(DiffRowKind, Option<i64>, Option<i64>)> {
        rows.iter()
            .map(|row| (row.kind, row.old_line, row.new_line))
            .collect()
    }

    fn stop(overrides: serde_json::Value) -> PullRequestReviewGuideStop {
        let mut value = json!({
            "title": "Stop",
            "summary": "",
            "attention": null,
            "excerpts": [],
            "files": [],
            "notes": [],
            "questions": [],
        });
        for (key, override_value) in overrides.as_object().unwrap() {
            value[key] = override_value.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn step_tour_position_holds_the_tour_to_its_first_and_last_stops() {
        assert_eq!(step_tour_position(0, -1, 3), 0);
        assert_eq!(step_tour_position(0, 1, 3), 1);
        assert_eq!(step_tour_position(2, 1, 3), 2);
    }

    #[test]
    fn file_diff_rows_numbers_both_sides_through_context_deletions_and_additions() {
        use DiffRowKind::{Addition, Context, Deletion};
        assert_eq!(
            lines(&ROWS),
            vec![
                (Context, Some(10), Some(10)),
                (Deletion, Some(11), None),
                (Addition, None, Some(11)),
                (Addition, None, Some(12)),
                (Context, Some(12), Some(13)),
                (Context, Some(13), Some(14)),
                (Context, Some(14), Some(15)),
                (Context, Some(40), Some(41)),
                (Deletion, Some(41), None),
                (Context, Some(42), Some(42)),
            ]
        );
    }

    #[test]
    fn excerpt_rows_brings_the_replaced_lines_along_with_an_excerpt_that_starts_on_an_addition() {
        let shown = excerpt_rows(&ROWS, &new_range(11, 12));
        let kinds: Vec<DiffRowKind> = shown.iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            [
                DiffRowKind::Deletion,
                DiffRowKind::Addition,
                DiffRowKind::Addition
            ]
        );
    }

    #[test]
    fn excerpt_rows_selects_old_side_lines_for_a_removal_and_stops_at_the_hunk_boundary() {
        let shown = excerpt_rows(&ROWS, &old_range(41, 41));
        assert_eq!(lines(&shown), vec![(DiffRowKind::Deletion, Some(41), None)]);
    }

    #[test]
    fn excerpt_rows_returns_nothing_for_a_range_outside_the_diff() {
        assert_eq!(excerpt_rows(&ROWS, &new_range(100, 120)), Vec::new());
    }

    #[test]
    fn rows_to_patch_cuts_a_slice_that_parses_back_to_the_same_lines_and_numbers() {
        let shown = excerpt_rows(&ROWS, &new_range(10, 13));
        let slice = parse(&rows_to_patch("src/tray.ts", "src/tray.ts", &shown));
        assert_eq!(lines(&file_diff_rows(&slice)), lines(&shown));
    }

    #[test]
    fn rows_to_patch_starts_a_hunk_per_source_hunk_so_separate_hunks_stay_separate() {
        let shown = excerpt_rows(&ROWS, &old_range(14, 42));
        let patch = rows_to_patch("src/tray.ts", "src/tray.ts", &shown);
        assert_eq!(
            patch.lines().filter(|line| line.starts_with("@@ ")).count(),
            2
        );
        assert!(patch.contains("@@ -14,1 +15,1 @@"));
        assert!(patch.contains("@@ -40,3 +41,2 @@"));
    }

    // The new file behind PATCH: lines 1-9, the hunk at 10-15, then lines up to 41-42 around the
    // second hunk, which removed a line.
    fn new_contents() -> String {
        let mut new_file: Vec<String> = (1..=50).map(|index| format!("line {index}\n")).collect();
        new_file.splice(
            9..15,
            [
                "const a = 1;\n",
                "const size = \"xs\";\n",
                "const variant = \"tray\";\n",
                "const b = 2;\n",
                "const c = 3;\n",
                "const d = 4;\n",
            ]
            .map(str::to_owned),
        );
        new_file.splice(40..42, ["keep();\n", "tail();\n"].map(str::to_owned));
        new_file.concat()
    }

    #[test]
    fn excerpt_expansion_puts_back_only_the_excerpts_lines_and_keeps_both_sides_numbered_as_in_the_file()
     {
        let new_contents = new_contents();
        let shown = excerpt_rows(&ROWS, &new_range(11, 12));
        let old = excerpt_expansion_old_contents(&new_contents, &shown).expect("expected contents");
        let old_lines: Vec<&str> = old.split('\n').collect();
        // The excerpt's old side sits at its real old line.
        assert_eq!(old_lines[10], "const size = pick();");
        // Around it, the new file's lines, so the two sides agree outside the excerpt.
        assert_eq!(old_lines[9], "const a = 1;");
        assert_eq!(old_lines[11], "const b = 2;");
        // One line replaced by two: the old side is a line shorter, everything after lines up.
        assert_eq!(old_lines.len(), new_contents.split('\n').count() - 1);
    }

    #[test]
    fn excerpt_expansion_does_not_try_to_expand_an_excerpt_that_spans_hunks() {
        let shown = excerpt_rows(&ROWS, &old_range(14, 42));
        assert_eq!(
            excerpt_expansion_old_contents(&new_contents(), &shown),
            None
        );
    }

    #[test]
    fn letter_stop_notes_letters_notes_in_guide_order_and_pins_each_to_the_excerpt_that_holds_its_line()
     {
        let stop = stop(json!({
            "excerpts": [
                { "path": "a.ts", "side": "new", "start": 1, "end": 5, "caption": "" },
                { "path": "b.ts", "side": "new", "start": 10, "end": 12, "caption": "" },
            ],
            "notes": [
                { "path": "b.ts", "line": 11, "title": "First", "body": "" },
                { "path": "a.ts", "line": 2, "title": "Second", "body": "" },
                { "path": "a.ts", "line": 2, "title": "Third", "body": "" },
                { "path": "c.ts", "line": 1, "title": "Unanchored", "body": "" },
            ],
        }));
        let LetteredStopNotes {
            lettered,
            by_excerpt,
        } = letter_stop_notes(&stop);
        let markers: Vec<&str> = lettered.iter().map(|entry| entry.marker.as_str()).collect();
        assert_eq!(markers, ["A", "B", "C", "D"]);
        let marker = |marker: &str, title: &str| NoteLineMarker {
            marker: marker.to_owned(),
            title: title.to_owned(),
        };
        assert_eq!(
            by_excerpt,
            vec![
                vec![ExcerptLineMarkers {
                    line: 2,
                    markers: vec![marker("B", "Second"), marker("C", "Third")],
                }],
                vec![ExcerptLineMarkers {
                    line: 11,
                    markers: vec![marker("A", "First")]
                }],
            ]
        );
    }

    #[test]
    fn stop_subline_counts_the_files_a_stop_owns() {
        assert_eq!(stop_subline(&stop(json!({}))), "Context, no code");
        let one = stop(json!({
            "excerpts": [{ "path": "a.ts", "side": "new", "start": 1, "end": 2, "caption": "" }],
            "files": [{ "path": "a.ts" }],
        }));
        assert_eq!(stop_subline(&one), "1 file");
        let two = stop(json!({
            "excerpts": [{ "path": "a.ts", "side": "new", "start": 1, "end": 2, "caption": "" }],
            "files": [{ "path": "b.ts" }],
        }));
        assert_eq!(stop_file_paths(&two), ["a.ts", "b.ts"]);
        assert_eq!(stop_subline(&two), "2 files");
    }
}
