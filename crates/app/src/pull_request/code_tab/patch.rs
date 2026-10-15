//! A unified-patch parser producing the file model `@pierre/diffs` `parsePatchFiles` hands the
//! pull request views, plus the small pure helpers upstream keeps in `lib/diffRendering.ts` and
//! `reviewCommentContext.ts` (`getDiffLineStat`, `resolveFileDiffPath`, `resolveDiffReviewPosition`,
//! the whitespace filter). `@pierre/diffs` has no Rust equivalent, so the patch the host returned
//! is parsed here into [`FileDiffMetadata`] (the type the review guide tour already reads) and
//! rendered by `code_tab`'s own diff view.
//!
//! Not ported: pierre's split/unified line offsets (rows are laid out by the view), full-file
//! hydration (`loadDiffFiles`), word-level diffs (`lineDiffType: "none"` upstream).

use turin_contracts::PositiveInt;
use turin_contracts::pull_request::{PullRequestDiffSide, PullRequestReviewPosition};

use crate::pull_request::pull_request_diff_logic::DiffHunkRange;
use crate::pull_request::pull_request_file_order_logic::OrderableDiffFile;
use crate::state::helpers::review_guide_tour::{
    DiffRow, DiffRowKind, FileDiffMetadata, Hunk, HunkContent, file_diff_rows,
};

/// `FileDiffMetadata.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    Change,
    RenamePure,
    RenameChanged,
    New,
    Deleted,
}

/// What a hunk's `@@ -a,b +c,d @@ section` line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkHeader {
    pub deletion_start: i64,
    pub deletion_count: i64,
    pub addition_start: i64,
    pub addition_count: i64,
    /// The function / heading git prints after the second `@@`.
    pub section: String,
}

/// One file of a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFile {
    /// `name`: the path after the change.
    pub name: String,
    /// `prevName`: only set where the file moved.
    pub prev_name: Option<String>,
    pub change: FileChange,
    pub meta: FileDiffMetadata,
    pub headers: Vec<HunkHeader>,
    /// A binary file has no hunks to show.
    pub binary: bool,
    pub additions: u64,
    pub deletions: u64,
}

/// `getRenderablePatch`'s two answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedPatch {
    Files(Vec<PatchFile>),
    /// A patch that could not be structured, shown as text.
    Raw {
        reason: String,
        text: String,
    },
}

impl PatchFile {
    /// `resolveFileDiffPath`.
    pub fn path(&self) -> &str {
        if self.name.is_empty() {
            self.prev_name.as_deref().unwrap_or("")
        } else {
            &self.name
        }
    }

    /// `resolveFileDiffPreviousPath`.
    pub fn previous_path(&self) -> &str {
        self.prev_name.as_deref().unwrap_or(self.path())
    }

    /// `buildFileDiffRenderKey`: stable across re-renders of the same file, distinct for every
    /// block in a patch.
    pub fn key(&self) -> String {
        format!(
            "{}:{}",
            self.prev_name.as_deref().unwrap_or("none"),
            self.name
        )
    }

    pub fn rows(&self) -> Vec<DiffRow> {
        file_diff_rows(&self.meta)
    }

    /// The hunks as `isLineInFileDiff` reads them.
    pub fn hunk_ranges(&self) -> Vec<DiffHunkRange> {
        self.headers
            .iter()
            .map(|header| DiffHunkRange {
                deletion_start: header.deletion_start,
                deletion_count: header.deletion_count,
                addition_start: header.addition_start,
                addition_count: header.addition_count,
            })
            .collect()
    }

    /// Input for `order_diff_files`: the path with its added and removed lines.
    pub fn orderable(&self) -> OrderableDiffFile {
        let rows = self.rows();
        let text = |kind: DiffRowKind| -> Vec<String> {
            rows.iter()
                .filter(|row| row.kind == kind)
                .map(|row| row.text.trim_end_matches(['\n', '\r']).to_owned())
                .collect()
        };
        OrderableDiffFile {
            path: self.path().to_owned(),
            addition_lines: text(DiffRowKind::Addition),
            deletion_lines: text(DiffRowKind::Deletion),
        }
    }
}

/// `fnv1a32`.
pub fn fnv1a32(value: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in value.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// `getDiffLineStat` over files.
pub fn diff_line_stat(files: &[PatchFile]) -> (u64, u64) {
    files.iter().fold((0, 0), |(additions, deletions), file| {
        (additions + file.additions, deletions + file.deletions)
    })
}

// ── Parsing ─────────────────────────────────────────────────────────────────

fn strip_prefix_path(value: &str) -> Option<String> {
    let value = value.trim_end_matches('\t').trim();
    if value == "/dev/null" {
        return None;
    }
    let unquoted = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    for prefix in ["a/", "b/"] {
        if let Some(rest) = unquoted.strip_prefix(prefix) {
            return Some(rest.to_owned());
        }
    }
    Some(unquoted.to_owned())
}

/// Names from `diff --git a/x b/y`.
fn header_names(line: &str) -> (Option<String>, Option<String>) {
    let rest = line.strip_prefix("diff --git ").unwrap_or(line);
    // `a/x b/x` with the same path on both sides splits exactly in the middle.
    if let Some(inner) = rest.strip_prefix("a/") {
        let bytes = inner.len();
        if bytes >= 3 && (bytes - 3) % 2 == 0 {
            let half = (bytes - 3) / 2;
            let (left, right) = (&inner[..half], &inner[half + 1..]);
            if inner.as_bytes().get(half) == Some(&b' ') && right.strip_prefix("b/") == Some(left) {
                return (Some(left.to_owned()), Some(left.to_owned()));
            }
        }
    }
    match rest.rfind(" b/") {
        Some(split) => (
            strip_prefix_path(&rest[..split]),
            strip_prefix_path(&rest[split + 1..]),
        ),
        None => (None, None),
    }
}

fn parse_range(spec: &str) -> Option<(i64, i64)> {
    let spec = spec.trim_start_matches(['-', '+']);
    match spec.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((spec.parse().ok()?, 1)),
    }
}

fn parse_hunk_header(line: &str) -> Option<HunkHeader> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, section) = rest.split_once(" @@").unwrap_or((rest, ""));
    let (old, new) = ranges.split_once(' ')?;
    let (deletion_start, deletion_count) = parse_range(old)?;
    let (addition_start, addition_count) = parse_range(new)?;
    Some(HunkHeader {
        deletion_start,
        deletion_count,
        addition_start,
        addition_count,
        section: section.trim().to_owned(),
    })
}

#[derive(Default)]
struct FileBuilder {
    header_old: Option<String>,
    header_new: Option<String>,
    old_name: Option<String>,
    new_name: Option<String>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    similarity: Option<u32>,
    new_file: bool,
    deleted_file: bool,
    binary: bool,
    headers: Vec<HunkHeader>,
    hunks: Vec<Hunk>,
    addition_lines: Vec<String>,
    deletion_lines: Vec<String>,
    additions: u64,
    deletions: u64,
}

/// A hunk's lines while they are being read.
struct HunkReader {
    header: HunkHeader,
    old_remaining: i64,
    new_remaining: i64,
    content: Vec<HunkContent>,
    /// The last line pushed, for `\ No newline at end of file`.
    last: Option<(DiffRowKind, usize)>,
}

impl FileBuilder {
    fn finish_hunk(&mut self, reader: HunkReader) {
        self.hunks.push(Hunk {
            deletion_start: reader.header.deletion_start,
            addition_start: reader.header.addition_start,
            hunk_content: reader.content,
        });
        self.headers.push(reader.header);
    }

    fn push_line(&mut self, reader: &mut HunkReader, kind: DiffRowKind, text: &str) {
        let text = format!("{text}\n");
        match kind {
            DiffRowKind::Context => {
                let addition_index = self.addition_lines.len();
                let deletion_index = self.deletion_lines.len();
                self.addition_lines.push(text.clone());
                self.deletion_lines.push(text);
                match reader.content.last_mut() {
                    Some(HunkContent::Context { lines, .. }) => *lines += 1,
                    _ => reader.content.push(HunkContent::Context {
                        lines: 1,
                        addition_line_index: addition_index,
                        deletion_line_index: deletion_index,
                    }),
                }
                reader.last = Some((kind, addition_index));
                reader.old_remaining -= 1;
                reader.new_remaining -= 1;
            }
            DiffRowKind::Deletion => {
                let index = self.deletion_lines.len();
                self.deletion_lines.push(text);
                self.deletions += 1;
                match reader.content.last_mut() {
                    // A deletion after an addition starts a block of its own, so rows keep the
                    // order the patch gave them.
                    Some(HunkContent::Change {
                        deletions,
                        additions: 0,
                        ..
                    }) => *deletions += 1,
                    _ => reader.content.push(HunkContent::Change {
                        deletions: 1,
                        additions: 0,
                        addition_line_index: self.addition_lines.len(),
                        deletion_line_index: index,
                    }),
                }
                reader.last = Some((kind, index));
                reader.old_remaining -= 1;
            }
            DiffRowKind::Addition => {
                let index = self.addition_lines.len();
                self.addition_lines.push(text);
                self.additions += 1;
                match reader.content.last_mut() {
                    Some(HunkContent::Change { additions, .. }) => *additions += 1,
                    _ => reader.content.push(HunkContent::Change {
                        deletions: 0,
                        additions: 1,
                        addition_line_index: index,
                        deletion_line_index: self.deletion_lines.len(),
                    }),
                }
                reader.last = Some((kind, index));
                reader.new_remaining -= 1;
            }
        }
    }

    fn build(self) -> Option<PatchFile> {
        let moved = self.rename_from.is_some() || self.rename_to.is_some();
        let new_name = self
            .rename_to
            .clone()
            .or(self.new_name.clone())
            .or(self.old_name.clone().filter(|_| self.deleted_file))
            .or(self.header_new.clone())?;
        let old_name = self
            .rename_from
            .clone()
            .or(self.old_name.clone())
            .or(self.header_old.clone());
        let change = if self.new_file {
            FileChange::New
        } else if self.deleted_file {
            FileChange::Deleted
        } else if moved && old_name.as_deref() != Some(new_name.as_str()) {
            if self.hunks.is_empty() && self.similarity.is_none_or(|value| value == 100) {
                FileChange::RenamePure
            } else {
                FileChange::RenameChanged
            }
        } else {
            FileChange::Change
        };
        let prev_name = old_name.filter(|old| old != &new_name && change != FileChange::New);
        Some(PatchFile {
            name: new_name,
            prev_name,
            change,
            meta: FileDiffMetadata {
                hunks: self.hunks,
                addition_lines: self.addition_lines,
                deletion_lines: self.deletion_lines,
            },
            headers: self.headers,
            binary: self.binary,
            additions: self.additions,
            deletions: self.deletions,
        })
    }
}

/// `parsePatchFiles` + `getRenderablePatch`'s fallback: files, or the text when none parse.
pub fn parse_patch(patch: &str) -> Option<ParsedPatch> {
    let patch = patch.trim();
    if patch.is_empty() {
        return None;
    }
    let mut files: Vec<PatchFile> = Vec::new();
    let mut current: Option<FileBuilder> = None;
    let mut hunk: Option<HunkReader> = None;
    let finish_file = |current: &mut Option<FileBuilder>, files: &mut Vec<PatchFile>| {
        if let Some(builder) = current.take()
            && let Some(file) = builder.build()
        {
            files.push(file);
        }
    };
    for line in patch.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // Inside a hunk, the header counts decide where it ends, so a removed `-- x` line is
        // never mistaken for a file header.
        if let (Some(reader), Some(builder)) = (hunk.as_mut(), current.as_mut()) {
            if reader.old_remaining > 0 || reader.new_remaining > 0 {
                let kind_and_text = match line.chars().next() {
                    Some(' ') => Some((DiffRowKind::Context, &line[1..])),
                    Some('+') if reader.new_remaining > 0 => {
                        Some((DiffRowKind::Addition, &line[1..]))
                    }
                    Some('-') if reader.old_remaining > 0 => {
                        Some((DiffRowKind::Deletion, &line[1..]))
                    }
                    None => Some((DiffRowKind::Context, "")),
                    _ => None,
                };
                if let Some((kind, text)) = kind_and_text {
                    builder.push_line(reader, kind, text);
                    continue;
                }
            }
            if line.starts_with('\\') {
                // `\ No newline at end of file` belongs to the line just before it.
                if let Some((kind, index)) = reader.last {
                    let strip = |lines: &mut Vec<String>, index: usize| {
                        if let Some(text) = lines.get_mut(index)
                            && text.ends_with('\n')
                        {
                            text.pop();
                        }
                    };
                    match kind {
                        DiffRowKind::Addition => strip(&mut builder.addition_lines, index),
                        DiffRowKind::Deletion => strip(&mut builder.deletion_lines, index),
                        DiffRowKind::Context => {
                            strip(&mut builder.addition_lines, index);
                            let deletion_index = builder.deletion_lines.len().saturating_sub(1);
                            strip(&mut builder.deletion_lines, deletion_index);
                        }
                    }
                }
                continue;
            }
        }
        if line.starts_with("@@ ") {
            if let (Some(reader), Some(builder)) = (hunk.take(), current.as_mut()) {
                builder.finish_hunk(reader);
            }
            if current.is_some()
                && let Some(header) = parse_hunk_header(line)
            {
                hunk = Some(HunkReader {
                    old_remaining: header.deletion_count,
                    new_remaining: header.addition_count,
                    header,
                    content: Vec::new(),
                    last: None,
                });
            }
            continue;
        }
        if line.starts_with("diff --git ") {
            if let (Some(reader), Some(builder)) = (hunk.take(), current.as_mut()) {
                builder.finish_hunk(reader);
            }
            finish_file(&mut current, &mut files);
            let (old, new) = header_names(line);
            current = Some(FileBuilder {
                header_old: old,
                header_new: new,
                ..FileBuilder::default()
            });
            continue;
        }
        let Some(builder) = current.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("--- ") {
            builder.old_name = strip_prefix_path(rest);
            if rest.trim() == "/dev/null" {
                builder.new_file = true;
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            builder.new_name = strip_prefix_path(rest);
            if rest.trim() == "/dev/null" {
                builder.deleted_file = true;
            }
        } else if line.starts_with("new file mode") {
            builder.new_file = true;
        } else if line.starts_with("deleted file mode") {
            builder.deleted_file = true;
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            builder.rename_from = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            builder.rename_to = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("similarity index ") {
            builder.similarity = rest.trim().trim_end_matches('%').parse().ok();
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            builder.binary = true;
        }
    }
    if let (Some(reader), Some(builder)) = (hunk.take(), current.as_mut()) {
        builder.finish_hunk(reader);
    }
    finish_file(&mut current, &mut files);
    if files.is_empty() {
        return Some(ParsedPatch::Raw {
            reason: "Unable to parse patch".to_owned(),
            text: patch.to_owned(),
        });
    }
    Some(ParsedPatch::Files(files))
}

// ── Review positions ────────────────────────────────────────────────────────

/// `resolveDiffReviewPosition`: the host-facing coordinates of a line picked in the viewer.
/// `side` is the side the reader's selection ended on; `None` reads the new side.
pub fn resolve_diff_review_position(
    file: &PatchFile,
    line: i64,
    side: PullRequestDiffSide,
) -> Option<PullRequestReviewPosition> {
    let rows = file.rows();
    let row = rows.iter().find(|row| match side {
        PullRequestDiffSide::Left => row.old_line == Some(line),
        PullRequestDiffSide::Right => row.new_line == Some(line),
    })?;
    let positive = |value: i64| PositiveInt::new(u64::try_from(value).ok()?);
    match row.kind {
        DiffRowKind::Addition => Some(PullRequestReviewPosition::Added {
            new_line: positive(row.new_line?)?,
        }),
        DiffRowKind::Deletion => Some(PullRequestReviewPosition::Deleted {
            old_line: positive(row.old_line?)?,
        }),
        DiffRowKind::Context => Some(PullRequestReviewPosition::Context {
            old_line: positive(row.old_line?)?,
            new_line: positive(row.new_line?)?,
            side,
        }),
    }
}

// ── Whitespace filter ───────────────────────────────────────────────────────

fn squash(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// One step of the alignment between a hunk's old and new side.
#[derive(Clone, Copy)]
enum Step {
    /// Old line `.0` and new line `.1` are the same text bar whitespace: context.
    Match(usize, usize),
    Delete(usize),
    Add(usize),
}

/// The longest-common-subsequence alignment of two lists of squashed lines.
fn align(old_key: &[String], new_key: &[String]) -> Vec<Step> {
    let (n, m) = (old_key.len(), new_key.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old_key[i] == new_key[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut steps = Vec::with_capacity(n.max(m));
    while i < n || j < m {
        if i < n && j < m && old_key[i] == new_key[j] {
            steps.push(Step::Match(i, j));
            i += 1;
            j += 1;
        } else if j >= m || (i < n && table[i + 1][j] >= table[i][j + 1]) {
            steps.push(Step::Delete(i));
            i += 1;
        } else {
            steps.push(Step::Add(j));
            j += 1;
        }
    }
    steps
}

/// The line arrays and counts of the file being rebuilt.
#[derive(Default)]
struct Rebuilt {
    addition_lines: Vec<String>,
    deletion_lines: Vec<String>,
    additions: u64,
    deletions: u64,
}

fn flush_change(
    state: &mut Rebuilt,
    content: &mut Vec<HunkContent>,
    pending_del: &mut Vec<&str>,
    pending_add: &mut Vec<&str>,
) {
    if pending_del.is_empty() && pending_add.is_empty() {
        return;
    }
    content.push(HunkContent::Change {
        deletions: pending_del.len(),
        additions: pending_add.len(),
        addition_line_index: state.addition_lines.len(),
        deletion_line_index: state.deletion_lines.len(),
    });
    state
        .deletion_lines
        .extend(pending_del.iter().map(|text| (*text).to_owned()));
    state
        .addition_lines
        .extend(pending_add.iter().map(|text| (*text).to_owned()));
    state.deletions += pending_del.len() as u64;
    state.additions += pending_add.len() as u64;
    pending_del.clear();
    pending_add.clear();
}

/// `hideWhitespaceChanges`: each hunk is diffed again with all whitespace removed, so a line
/// whose only change is whitespace becomes context.
pub fn hide_whitespace_changes(file: &PatchFile) -> PatchFile {
    let mut hunks = Vec::with_capacity(file.meta.hunks.len());
    let mut state = Rebuilt::default();
    for hunk in &file.meta.hunks {
        // A hunk is the unit: old = context + deletions, new = context + additions, in order.
        let mut old_side: Vec<&str> = Vec::new();
        let mut new_side: Vec<&str> = Vec::new();
        let mut original: Vec<Step> = Vec::new();
        for content in &hunk.hunk_content {
            match content {
                HunkContent::Context {
                    lines,
                    addition_line_index,
                    ..
                } => {
                    for offset in 0..*lines {
                        original.push(Step::Match(old_side.len(), new_side.len()));
                        let text = file.meta.addition_lines[addition_line_index + offset].as_str();
                        old_side.push(text);
                        new_side.push(text);
                    }
                }
                HunkContent::Change {
                    deletions,
                    additions,
                    addition_line_index,
                    deletion_line_index,
                } => {
                    for offset in 0..*deletions {
                        original.push(Step::Delete(old_side.len()));
                        old_side
                            .push(file.meta.deletion_lines[deletion_line_index + offset].as_str());
                    }
                    for offset in 0..*additions {
                        original.push(Step::Add(new_side.len()));
                        new_side
                            .push(file.meta.addition_lines[addition_line_index + offset].as_str());
                    }
                }
            }
        }
        let old_key: Vec<String> = old_side.iter().map(|text| squash(text)).collect();
        let new_key: Vec<String> = new_side.iter().map(|text| squash(text)).collect();
        // Quadratic in a hunk, which is bounded by the host's own context windows; past the
        // bound the patch's own alignment stands.
        let steps = if old_key.len().saturating_mul(new_key.len()) > 4_000_000 {
            original
        } else {
            align(&old_key, &new_key)
        };
        let mut content: Vec<HunkContent> = Vec::new();
        let mut pending_del: Vec<&str> = Vec::new();
        let mut pending_add: Vec<&str> = Vec::new();
        for step in steps {
            match step {
                Step::Match(_, new_index) => {
                    flush_change(&mut state, &mut content, &mut pending_del, &mut pending_add);
                    let addition_index = state.addition_lines.len();
                    let deletion_index = state.deletion_lines.len();
                    state.addition_lines.push(new_side[new_index].to_owned());
                    state.deletion_lines.push(new_side[new_index].to_owned());
                    match content.last_mut() {
                        Some(HunkContent::Context { lines, .. }) => *lines += 1,
                        _ => content.push(HunkContent::Context {
                            lines: 1,
                            addition_line_index: addition_index,
                            deletion_line_index: deletion_index,
                        }),
                    }
                }
                Step::Delete(old_index) => pending_del.push(old_side[old_index]),
                Step::Add(new_index) => pending_add.push(new_side[new_index]),
            }
        }
        flush_change(&mut state, &mut content, &mut pending_del, &mut pending_add);
        hunks.push(Hunk {
            deletion_start: hunk.deletion_start,
            addition_start: hunk.addition_start,
            hunk_content: content,
        });
    }
    let mut next = file.clone();
    next.meta = FileDiffMetadata {
        hunks,
        addition_lines: state.addition_lines,
        deletion_lines: state.deletion_lines,
    };
    next.additions = state.additions;
    next.deletions = state.deletions;
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SIMPLE: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@ fn main
 keep
-old
+new
+extra
 tail
 end
diff --git a/README.md b/README.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/README.md
@@ -0,0 +1,2 @@
+# Title
+text
";

    fn files(patch: &str) -> Vec<PatchFile> {
        match parse_patch(patch) {
            Some(ParsedPatch::Files(files)) => files,
            other => panic!("expected files, got {other:?}"),
        }
    }

    #[test]
    fn parses_files_hunks_and_counts() {
        let files = files(SIMPLE);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "src/lib.rs");
        assert_eq!(files[0].change, FileChange::Change);
        assert_eq!((files[0].additions, files[0].deletions), (2, 1));
        assert_eq!(files[0].headers[0].section, "fn main");
        assert_eq!(files[1].change, FileChange::New);
        assert_eq!(files[1].prev_name, None);
        assert_eq!((files[1].additions, files[1].deletions), (2, 0));
    }

    #[test]
    fn rows_carry_line_numbers_from_both_sides() {
        let files = files(SIMPLE);
        let rows = files[0].rows();
        let summary: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.kind,
                    row.old_line,
                    row.new_line,
                    row.text.trim_end().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (DiffRowKind::Context, Some(1), Some(1), "keep".into()),
                (DiffRowKind::Deletion, Some(2), None, "old".into()),
                (DiffRowKind::Addition, None, Some(2), "new".into()),
                (DiffRowKind::Addition, None, Some(3), "extra".into()),
                (DiffRowKind::Context, Some(3), Some(4), "tail".into()),
                (DiffRowKind::Context, Some(4), Some(5), "end".into()),
            ]
        );
    }

    #[test]
    fn deletion_after_addition_starts_a_new_block() {
        let patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-a\n+b\n-c\n+d\n";
        let files = files(patch);
        let kinds: Vec<_> = files[0].rows().iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffRowKind::Deletion,
                DiffRowKind::Addition,
                DiffRowKind::Deletion,
                DiffRowKind::Addition
            ]
        );
    }

    #[test]
    fn a_removed_line_that_looks_like_a_header_stays_in_its_hunk() {
        let patch =
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,1 @@\n--- not a header\n keep\n";
        let files = files(patch);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[0].rows()[0].text, "-- not a header\n");
    }

    #[test]
    fn renames_and_deletions_are_typed() {
        let patch = "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs\ndiff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let files = files(patch);
        assert_eq!(files[0].change, FileChange::RenamePure);
        assert_eq!(files[0].prev_name.as_deref(), Some("old.rs"));
        assert_eq!(files[0].path(), "new.rs");
        assert_eq!(files[1].change, FileChange::Deleted);
        assert_eq!(files[1].path(), "gone.rs");
        assert_eq!(files[1].deletions, 1);
    }

    #[test]
    fn binary_files_have_no_hunks() {
        let patch = "diff --git a/img.png b/img.png\nindex 1..2 100644\nBinary files a/img.png and b/img.png differ\n";
        let files = files(patch);
        assert!(files[0].binary);
        assert!(files[0].meta.hunks.is_empty());
    }

    #[test]
    fn no_newline_marker_strips_the_trailing_newline() {
        let patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n";
        let files = files(patch);
        assert_eq!(files[0].meta.deletion_lines, vec!["a".to_owned()]);
        assert_eq!(files[0].meta.addition_lines, vec!["b\n".to_owned()]);
    }

    #[test]
    fn text_without_a_diff_header_is_raw() {
        assert!(matches!(
            parse_patch("just some words"),
            Some(ParsedPatch::Raw { .. })
        ));
        assert_eq!(parse_patch("   \n"), None);
    }

    #[test]
    fn positions_resolve_per_side() {
        let files = files(SIMPLE);
        let file = &files[0];
        assert_eq!(
            resolve_diff_review_position(file, 2, PullRequestDiffSide::Right),
            Some(PullRequestReviewPosition::Added {
                new_line: PositiveInt::new(2).unwrap()
            })
        );
        assert_eq!(
            resolve_diff_review_position(file, 2, PullRequestDiffSide::Left),
            Some(PullRequestReviewPosition::Deleted {
                old_line: PositiveInt::new(2).unwrap()
            })
        );
        assert_eq!(
            resolve_diff_review_position(file, 4, PullRequestDiffSide::Right),
            Some(PullRequestReviewPosition::Context {
                old_line: PositiveInt::new(3).unwrap(),
                new_line: PositiveInt::new(4).unwrap(),
                side: PullRequestDiffSide::Right,
            })
        );
        assert_eq!(
            resolve_diff_review_position(file, 99, PullRequestDiffSide::Right),
            None
        );
    }

    #[test]
    fn hunk_ranges_feed_line_placement() {
        let files = files(SIMPLE);
        let ranges = files[0].hunk_ranges();
        assert!(
            crate::pull_request::pull_request_diff_logic::is_line_in_file_diff(
                &ranges,
                PullRequestDiffSide::Right,
                5
            )
        );
        assert!(
            !crate::pull_request::pull_request_diff_logic::is_line_in_file_diff(
                &ranges,
                PullRequestDiffSide::Right,
                6
            )
        );
    }

    #[test]
    fn hiding_whitespace_turns_reindented_lines_into_context() {
        let patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n keep\n-  indented\n+    indented\n-real\n+changed\n";
        let files = files(patch);
        let hidden = hide_whitespace_changes(&files[0]);
        assert_eq!((hidden.additions, hidden.deletions), (1, 1));
        let kinds: Vec<_> = hidden.rows().iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffRowKind::Context,
                DiffRowKind::Context,
                DiffRowKind::Deletion,
                DiffRowKind::Addition
            ]
        );
    }

    #[test]
    fn fixture_patch_parses() {
        let result = crate::pull_request::fixtures::diff_result();
        let files = files(&result.patch);
        assert!(!files.is_empty());
        assert!(files.iter().all(|file| !file.name.is_empty()));
    }

    #[test]
    fn fnv_matches_the_reference_vector() {
        assert_eq!(fnv1a32(""), 0x811c_9dc5);
        assert_eq!(fnv1a32("a"), 0xe40c_292c);
    }
}
