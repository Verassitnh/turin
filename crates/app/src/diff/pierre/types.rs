//! Port of the diff data model in `@pierre/diffs@1.5.2` (`dist/types.d.ts`):
//! `FileDiffMetadata`, `Hunk`, `ContextContent` / `ChangeContent`,
//! `ChangeTypes`, `SelectedLineRange`, `DiffLineAnnotation`.
//!
//! Field names and semantics follow upstream exactly. Lines keep their
//! trailing newline, as pierre's `splitWithNewlines` does.

/// `ChangeTypes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeType {
    Change,
    RenamePure,
    RenameChanged,
    New,
    Deleted,
}

impl ChangeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::RenamePure => "rename-pure",
            Self::RenameChanged => "rename-changed",
            Self::New => "new",
            Self::Deleted => "deleted",
        }
    }
}

/// `ContextContent` / `ChangeContent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkContent {
    Context {
        /// Number of unchanged lines in this block.
        lines: usize,
        /// Zero-based index into `additionLines` where the block starts.
        addition_line_index: usize,
        /// Zero-based index into `deletionLines` where the block starts.
        deletion_line_index: usize,
    },
    Change {
        deletions: usize,
        deletion_line_index: usize,
        additions: usize,
        addition_line_index: usize,
    },
}

impl HunkContent {
    pub fn addition_line_index(&self) -> usize {
        match *self {
            Self::Context {
                addition_line_index,
                ..
            }
            | Self::Change {
                addition_line_index,
                ..
            } => addition_line_index,
        }
    }

    pub fn deletion_line_index(&self) -> usize {
        match *self {
            Self::Context {
                deletion_line_index,
                ..
            }
            | Self::Change {
                deletion_line_index,
                ..
            } => deletion_line_index,
        }
    }

    pub fn set_line_indices(&mut self, addition: usize, deletion: usize) {
        match self {
            Self::Context {
                addition_line_index,
                deletion_line_index,
                ..
            }
            | Self::Change {
                addition_line_index,
                deletion_line_index,
                ..
            } => {
                *addition_line_index = addition;
                *deletion_line_index = deletion;
            }
        }
    }
}

/// `Hunk`: one `@@ … @@` block.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Hunk {
    /// Unchanged lines between the previous hunk (or file start) and this one.
    pub collapsed_before: usize,
    pub addition_start: usize,
    pub addition_count: usize,
    /// Lines prefixed with `+`.
    pub addition_lines: usize,
    pub addition_line_index: usize,
    pub deletion_start: usize,
    pub deletion_count: usize,
    /// Lines prefixed with `-`.
    pub deletion_lines: usize,
    pub deletion_line_index: usize,
    pub hunk_content: Vec<HunkContent>,
    /// Function/method name after the `@@` markers.
    pub hunk_context: Option<String>,
    /// Raw hunk header (`@@ -1,5 +1,7 @@`).
    pub hunk_specs: Option<String>,
    pub split_line_start: usize,
    pub split_line_count: usize,
    pub unified_line_start: usize,
    pub unified_line_count: usize,
    pub no_eofcr_deletions: bool,
    pub no_eofcr_additions: bool,
}

/// `FileDiffMetadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiffMetadata {
    pub name: String,
    pub prev_name: Option<String>,
    pub lang: Option<String>,
    pub new_object_id: Option<String>,
    pub prev_object_id: Option<String>,
    pub mode: Option<String>,
    pub prev_mode: Option<String>,
    pub r#type: ChangeType,
    pub hunks: Vec<Hunk>,
    pub split_line_count: usize,
    pub unified_line_count: usize,
    /// Parsed from a patch (true): the line arrays hold only patch lines and
    /// hunk expansion is unavailable. False: full file contents.
    pub is_partial: bool,
    pub deletion_lines: Vec<String>,
    pub addition_lines: Vec<String>,
    pub cache_key: Option<String>,
}

impl FileDiffMetadata {
    /// An empty `change` file, the parser's starting point.
    pub fn empty(is_partial: bool) -> Self {
        Self {
            name: String::new(),
            prev_name: None,
            lang: None,
            new_object_id: None,
            prev_object_id: None,
            mode: None,
            prev_mode: None,
            r#type: ChangeType::Change,
            hunks: Vec::new(),
            split_line_count: 0,
            unified_line_count: 0,
            is_partial,
            deletion_lines: Vec::new(),
            addition_lines: Vec::new(),
            cache_key: None,
        }
    }
}

/// `SelectionSide` / `AnnotationSide`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SelectionSide {
    Deletions,
    Additions,
}

impl SelectionSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deletions => "deletions",
            Self::Additions => "additions",
        }
    }
}

/// `SelectedLineRange`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedLineRange {
    pub start: usize,
    pub side: Option<SelectionSide>,
    pub end: usize,
    pub end_side: Option<SelectionSide>,
}

/// `DiffLineAnnotation<T>`.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffLineAnnotation<T> {
    pub side: SelectionSide,
    pub line_number: usize,
    pub metadata: T,
}

/// `FileContents` (the subset the loaders use).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileContents {
    pub name: String,
    pub contents: String,
    pub cache_key: Option<String>,
}

/// `getHunkSideStartBoundary`: a unified hunk side's start/count as the
/// consumed-file range start.
pub fn get_hunk_side_start_boundary(start: usize, count: usize) -> usize {
    start.saturating_sub(if count == 0 { 0 } else { 1 })
}

/// `getHunkSideEndBoundary`.
pub fn get_hunk_side_end_boundary(start: usize, count: usize) -> usize {
    get_hunk_side_start_boundary(start, count) + count
}

/// `cleanLastNewline`.
pub fn clean_last_newline(contents: &str) -> &str {
    let contents = contents.strip_suffix('\n').map_or(contents, |rest| {
        rest.strip_suffix('\r').unwrap_or(rest)
    });
    contents
}

/// `splitWithNewlines`: lines keep their `\n`; empty input is `[""]`.
pub fn split_with_newlines(contents: &str) -> Vec<&str> {
    if contents.is_empty() {
        return vec![""];
    }
    contents.split_inclusive('\n').collect()
}

/// `splitFileContents` (utils/splitFileContents.ts): `split(/(?<=\n)/)`, empty → `[]`.
pub fn split_file_contents(contents: &str) -> Vec<String> {
    if contents.is_empty() {
        return Vec::new();
    }
    contents.split_inclusive('\n').map(str::to_string).collect()
}

/// `composeCacheKey`: `ck1:` + JSON array of the scope and segments.
pub fn compose_cache_key(scope: &str, segments: &[&str]) -> String {
    let mut all = vec![scope];
    all.extend_from_slice(segments);
    format!(
        "ck1:{}",
        serde_json::to_string(&all).unwrap_or_else(|_| "[]".into())
    )
}
