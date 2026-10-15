//! Port of `@pierre/diffs@1.5.2` `dist/utils/parsePatchFiles.js` (with
//! `parseQuotedDiffFileName.js` and `realignChangeContent.js`), upstream's
//! patch parser.
//!
//! Upstream's non-throwing mode is the only one ported (`throwOnError` is
//! always false in the app): invalid input is logged and skipped, exactly
//! where upstream calls `console.error`.

use std::sync::LazyLock;

use regex::Regex;

use super::types::{
    ChangeType, FileContents, FileDiffMetadata, Hunk, HunkContent, clean_last_newline,
    compose_cache_key, get_hunk_side_end_boundary, get_hunk_side_start_boundary,
    split_with_newlines,
};

static GIT_DIFF_HEADER_FILENAMES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^diff --git ("a/(?:[^"\\]|\\.)*"|a/.+?) ("b/(?:[^"\\]|\\.)*"|b/.+?)$"#)
        .expect("valid regex")
});
/// `FILENAME_HEADER_REGEX`.
static FILENAME_HEADER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(---|\+\+\+)\s+([^\t\r\n]+)").expect("valid regex"));
/// `INDEX_LINE_METADATA`.
static INDEX_LINE_METADATA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^index ([0-9a-f]+)\.\.([0-9a-f]+)(?: (\d+))?$").expect("valid regex")
});
/// `COMMIT_METADATA_SPLIT` (`/(?=^From [a-f0-9]+ .+$)/m`), as a line test.
static COMMIT_METADATA_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^From [a-f0-9]+ .+$").expect("valid regex"));

/// `ParsedPatch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedPatch {
    pub patch_metadata: Option<String>,
    pub files: Vec<FileDiffMetadata>,
}

/// Options of `processFile`.
#[derive(Default)]
pub struct ProcessFileOptions<'a> {
    pub cache_key: Option<String>,
    pub is_git_diff: Option<bool>,
    pub old_file: Option<&'a FileContents>,
    pub new_file: Option<&'a FileContents>,
}

/// `GIT_DIFF_FILE_BREAK_REGEX.test`: any line starts with `diff --git`.
fn has_git_diff_file_break(data: &str) -> bool {
    data.starts_with("diff --git") || data.contains("\ndiff --git")
}

/// `parsePatchFiles(data, cacheKeyPrefix)`.
pub fn parse_patch_files(data: &str, cache_key_prefix: Option<&str>) -> Vec<ParsedPatch> {
    let raw_patches = if has_commit_metadata_boundary(data) {
        split_commit_metadata(data)
    } else {
        vec![data]
    };
    let mut patches = Vec::new();
    for patch in raw_patches {
        let index = cache_key_prefix.map(|_| patches.len());
        patches.push(process_patch_inner(patch, cache_key_prefix, index));
    }
    patches
}

fn has_commit_metadata_boundary(data: &str) -> bool {
    data.starts_with("From ") || data.contains("\nFrom ")
}

/// `data.split(COMMIT_METADATA_SPLIT)`: split before each `From <sha> …` line.
fn split_commit_metadata(data: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut line_start = 0;
    for line in data.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if line_start > 0 && COMMIT_METADATA_LINE.is_match(trimmed) {
            parts.push(&data[start..line_start]);
            start = line_start;
        }
        line_start += line.len();
    }
    parts.push(&data[start..]);
    parts
}

/// `processPatch`.
pub fn process_patch(data: &str, cache_key_prefix: Option<&str>) -> ParsedPatch {
    process_patch_inner(data, cache_key_prefix, None)
}

fn process_patch_inner(
    data: &str,
    cache_key_prefix: Option<&str>,
    patch_index: Option<usize>,
) -> ParsedPatch {
    let is_git_diff = is_git_diff_patch(data);
    let raw_files = if is_git_diff {
        split_at_line_prefix(data, "diff --git")
    } else {
        split_unified_diff_files(data)
    };
    let mut patch_metadata = None;
    let mut files: Vec<FileDiffMetadata> = Vec::new();
    for blob in raw_files {
        let is_file = if is_git_diff {
            has_git_diff_file_break(blob)
        } else {
            is_unified_diff_file_header_at(blob, 0)
        };
        if !is_file {
            if patch_metadata.is_none() {
                patch_metadata = Some(blob.to_string());
            } else {
                tracing::error!(blob, "parsePatchContent: unknown file blob");
            }
            continue;
        }
        let cache_key = cache_key_prefix.map(|prefix| match patch_index {
            None => compose_cache_key("patch-file", &[prefix, &files.len().to_string()]),
            Some(index) => compose_cache_key(
                "patch-file",
                &[prefix, &index.to_string(), &files.len().to_string()],
            ),
        });
        if let Some(file) = process_file(
            blob,
            ProcessFileOptions {
                cache_key,
                is_git_diff: Some(is_git_diff),
                ..Default::default()
            },
        ) {
            files.push(file);
        }
    }
    ParsedPatch {
        patch_metadata,
        files,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineType {
    Addition,
    Deletion,
    Context,
    Metadata,
}

/// `processFile`.
pub fn process_file(file_diff: &str, options: ProcessFileOptions<'_>) -> Option<FileDiffMetadata> {
    let is_git_diff = options
        .is_git_diff
        .unwrap_or_else(|| has_git_diff_file_break(file_diff));
    let old_file = options.old_file;
    let new_file = options.new_file;
    let is_partial = old_file.is_none() || new_file.is_none();
    let mut last_hunk_end: isize = 0;
    let mut current: Option<FileDiffMetadata> = None;
    let mut deletion_line_index: usize = 0;
    let mut addition_line_index: usize = 0;

    for hunk_text in split_at_line_prefix(file_diff, "@@ ") {
        let mut lines = split_with_newlines(hunk_text);
        let first_line = lines[0];
        let header = parse_hunk_header(first_line);
        let (Some(header), Some(file)) = (header, current.as_mut()) else {
            if current.is_some() {
                tracing::error!(hunk = hunk_text, "parsePatchContent: Invalid hunk");
                continue;
            }
            let mut file = FileDiffMetadata::empty(is_partial);
            file.cache_key = options.cache_key.clone();
            if let (false, Some(old), Some(new)) = (is_partial, old_file, new_file) {
                file.addition_lines = split_with_newlines(&new.contents)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                file.deletion_lines = split_with_newlines(&old.contents)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
            }
            if file.addition_lines.len() == 1 && new_file.is_some_and(|f| f.contents.is_empty()) {
                file.addition_lines.clear();
            }
            if file.deletion_lines.len() == 1 && old_file.is_some_and(|f| f.contents.is_empty()) {
                file.deletion_lines.clear();
            }
            parse_file_header_lines(&mut file, &lines, is_git_diff);
            current = Some(file);
            continue;
        };

        while lines
            .last()
            .is_some_and(|line| matches!(*line, "\n" | "\r" | "\r\n" | ""))
        {
            lines.pop();
        }
        let addition_start = header.addition_start;
        let deletion_start = header.deletion_start;
        if !is_partial {
            deletion_line_index = deletion_start.saturating_sub(1);
            addition_line_index = addition_start.saturating_sub(1);
        }
        let mut hunk = Hunk {
            addition_count: header.addition_count,
            addition_start,
            deletion_count: header.deletion_count,
            deletion_start,
            deletion_line_index,
            addition_line_index,
            hunk_context: header.hunk_context,
            hunk_specs: Some(first_line.to_string()),
            ..Default::default()
        };
        let mut addition_lines = 0;
        let mut deletion_lines = 0;
        let mut parsed_additions = 0;
        let mut parsed_deletions = 0;
        let mut current_content: Option<usize> = None;
        let mut last_line_type: Option<LineType> = None;
        for raw_line in lines.iter().skip(1).copied() {
            if parsed_additions >= hunk.addition_count
                && parsed_deletions >= hunk.deletion_count
                && !raw_line.starts_with('\\')
                && !(is_hunk_body_line(raw_line) && !is_format_patch_version_separator(raw_line))
            {
                break;
            }
            let line_type = match raw_line.as_bytes().first() {
                Some(b'+') => LineType::Addition,
                Some(b'-') => LineType::Deletion,
                Some(b' ') => LineType::Context,
                Some(b'\\') => LineType::Metadata,
                _ => {
                    tracing::error!(raw_line, "processFile: invalid rawLine");
                    continue;
                }
            };
            let content_is = |content: Option<usize>, hunk: &Hunk, context: bool| {
                content.is_some_and(|index| {
                    matches!(hunk.hunk_content[index], HunkContent::Context { .. }) == context
                })
            };
            match line_type {
                LineType::Addition | LineType::Deletion => {
                    let line = parsed_line_content(raw_line);
                    if !content_is(current_content, &hunk, false) {
                        hunk.hunk_content.push(HunkContent::Change {
                            deletions: 0,
                            deletion_line_index,
                            additions: 0,
                            addition_line_index,
                        });
                        current_content = Some(hunk.hunk_content.len() - 1);
                    }
                    let Some(HunkContent::Change {
                        additions,
                        deletions,
                        ..
                    }) = current_content.map(|i| &mut hunk.hunk_content[i])
                    else {
                        unreachable!()
                    };
                    if line_type == LineType::Addition {
                        addition_line_index += 1;
                        parsed_additions += 1;
                        if is_partial {
                            file.addition_lines.push(line);
                        }
                        *additions += 1;
                        addition_lines += 1;
                    } else {
                        deletion_line_index += 1;
                        parsed_deletions += 1;
                        if is_partial {
                            file.deletion_lines.push(line);
                        }
                        *deletions += 1;
                        deletion_lines += 1;
                    }
                    last_line_type = Some(line_type);
                }
                LineType::Context => {
                    let line = parsed_line_content(raw_line);
                    if !content_is(current_content, &hunk, true) {
                        hunk.hunk_content.push(HunkContent::Context {
                            lines: 0,
                            addition_line_index,
                            deletion_line_index,
                        });
                        current_content = Some(hunk.hunk_content.len() - 1);
                    }
                    addition_line_index += 1;
                    deletion_line_index += 1;
                    parsed_additions += 1;
                    parsed_deletions += 1;
                    if is_partial {
                        file.deletion_lines.push(line.clone());
                        file.addition_lines.push(line);
                    }
                    if let Some(HunkContent::Context { lines, .. }) =
                        current_content.map(|i| &mut hunk.hunk_content[i])
                    {
                        *lines += 1;
                    }
                    last_line_type = Some(LineType::Context);
                }
                LineType::Metadata => {
                    let Some(index) = current_content else {
                        continue;
                    };
                    if matches!(hunk.hunk_content[index], HunkContent::Context { .. }) {
                        hunk.no_eofcr_additions = true;
                        hunk.no_eofcr_deletions = true;
                    } else if last_line_type == Some(LineType::Deletion) {
                        hunk.no_eofcr_deletions = true;
                    } else if last_line_type == Some(LineType::Addition) {
                        hunk.no_eofcr_additions = true;
                    }
                    if is_partial
                        && matches!(
                            last_line_type,
                            Some(LineType::Addition | LineType::Context)
                        )
                        && let Some(last) = file.addition_lines.last_mut()
                    {
                        *last = clean_last_newline(last).to_string();
                    }
                    if is_partial
                        && matches!(
                            last_line_type,
                            Some(LineType::Deletion | LineType::Context)
                        )
                        && let Some(last) = file.deletion_lines.last_mut()
                    {
                        *last = clean_last_newline(last).to_string();
                    }
                }
            }
        }

        if parsed_additions != hunk.addition_count || parsed_deletions != hunk.deletion_count {
            tracing::error!(
                header = first_line.trim_end(),
                declared_old = hunk.deletion_count,
                declared_new = hunk.addition_count,
                parsed_old = parsed_deletions,
                parsed_new = parsed_additions,
                "parsePatchContent: hunk line count mismatch"
            );
            let repaired_addition_start =
                get_hunk_side_start_boundary(hunk.addition_start, hunk.addition_count)
                    + usize::from(parsed_additions != 0);
            let repaired_deletion_start =
                get_hunk_side_start_boundary(hunk.deletion_start, hunk.deletion_count)
                    + usize::from(parsed_deletions != 0);
            if !is_partial {
                let addition_delta = repaired_addition_start as isize - hunk.addition_start as isize;
                let deletion_delta = repaired_deletion_start as isize - hunk.deletion_start as isize;
                let shift = |value: usize, delta: isize| (value as isize + delta).max(0) as usize;
                hunk.addition_line_index = shift(hunk.addition_line_index, addition_delta);
                hunk.deletion_line_index = shift(hunk.deletion_line_index, deletion_delta);
                for content in &mut hunk.hunk_content {
                    let addition = shift(content.addition_line_index(), addition_delta);
                    let deletion = shift(content.deletion_line_index(), deletion_delta);
                    content.set_line_indices(addition, deletion);
                }
            }
            hunk.addition_start = repaired_addition_start;
            hunk.deletion_start = repaired_deletion_start;
            hunk.addition_count = parsed_additions;
            hunk.deletion_count = parsed_deletions;
        }
        hunk.addition_lines = addition_lines;
        hunk.deletion_lines = deletion_lines;
        hunk.collapsed_before =
            (get_hunk_side_start_boundary(hunk.addition_start, hunk.addition_count) as isize
                - last_hunk_end)
                .max(0) as usize;
        last_hunk_end = get_hunk_side_end_boundary(hunk.addition_start, hunk.addition_count) as isize;
        for content in &hunk.hunk_content {
            match *content {
                HunkContent::Context { lines, .. } => {
                    hunk.split_line_count += lines;
                    hunk.unified_line_count += lines;
                }
                HunkContent::Change {
                    additions,
                    deletions,
                    ..
                } => {
                    hunk.split_line_count += additions.max(deletions);
                    hunk.unified_line_count += additions + deletions;
                }
            }
        }
        hunk.split_line_start = file.split_line_count + hunk.collapsed_before;
        hunk.unified_line_start = file.unified_line_count + hunk.collapsed_before;
        file.split_line_count += hunk.collapsed_before + hunk.split_line_count;
        file.unified_line_count += hunk.collapsed_before + hunk.unified_line_count;
        file.hunks.push(hunk);
    }

    let mut file = current?;
    if !is_partial
        && !file.addition_lines.is_empty()
        && !file.deletion_lines.is_empty()
        && let Some(last) = file.hunks.last()
    {
        let last_end = get_hunk_side_end_boundary(last.addition_start, last.addition_count);
        let collapsed_after = file.addition_lines.len().saturating_sub(last_end);
        file.split_line_count += collapsed_after;
        file.unified_line_count += collapsed_after;
    }
    if !is_git_diff {
        if file
            .prev_name
            .as_ref()
            .is_some_and(|prev| *prev != file.name)
        {
            file.r#type = if file.hunks.is_empty() {
                ChangeType::RenamePure
            } else {
                ChangeType::RenameChanged
            };
        } else if old_file.is_none_or(|f| f.contents.is_empty())
            && new_file.is_some_and(|f| !f.contents.is_empty())
        {
            file.r#type = ChangeType::New;
        } else if old_file.is_some_and(|f| !f.contents.is_empty())
            && new_file.is_none_or(|f| f.contents.is_empty())
        {
            file.r#type = ChangeType::Deleted;
        }
    }
    if !matches!(
        file.r#type,
        ChangeType::RenamePure | ChangeType::RenameChanged
    ) {
        file.prev_name = None;
    }
    realign_change_content_by_similarity(&mut file);
    Some(file)
}

/// The header half of `_processFile`'s first chunk.
fn parse_file_header_lines(file: &mut FileDiffMetadata, lines: &[&str], is_git_diff: bool) {
    for line in lines {
        if line.starts_with("diff --git") {
            let Some(captures) = GIT_DIFF_HEADER_FILENAMES.captures(line.trim()) else {
                tracing::error!(line, "parsePatchContent: invalid git diff header");
                continue;
            };
            let prev_name = decode_diff_file_name(&captures[1], true);
            let name = decode_diff_file_name(&captures[2], true);
            if prev_name != name {
                file.prev_name = Some(prev_name);
            }
            file.name = name;
            continue;
        }
        let filename = if line.starts_with("---") || line.starts_with("+++") {
            FILENAME_HEADER_REGEX.captures(line)
        } else {
            None
        };
        if let Some(captures) = filename {
            let file_name = decode_diff_file_name(&captures[2], is_git_diff);
            if &captures[1] == "---" && file_name != "/dev/null" {
                file.prev_name = Some(file_name.clone());
                file.name = file_name;
            } else if &captures[1] == "+++" && file_name != "/dev/null" {
                file.name = file_name;
            }
        } else if is_git_diff {
            if let Some(rest) = line.strip_prefix("new mode ") {
                file.mode = Some(rest.trim().to_string());
            }
            if let Some(rest) = line.strip_prefix("old mode ") {
                file.prev_mode = Some(rest.trim().to_string());
            }
            if let Some(rest) = line.strip_prefix("new file mode") {
                file.r#type = ChangeType::New;
                file.mode = Some(rest.trim().to_string());
            }
            if let Some(rest) = line.strip_prefix("deleted file mode") {
                file.r#type = ChangeType::Deleted;
                file.mode = Some(rest.trim().to_string());
            }
            if line.starts_with("similarity index") {
                file.r#type = if line.starts_with("similarity index 100%") {
                    ChangeType::RenamePure
                } else {
                    ChangeType::RenameChanged
                };
            }
            if line.starts_with("index ")
                && let Some(captures) = INDEX_LINE_METADATA.captures(line.trim())
            {
                file.prev_object_id = Some(captures[1].to_string());
                file.new_object_id = Some(captures[2].to_string());
                if let Some(mode) = captures.get(3) {
                    file.mode = Some(mode.as_str().to_string());
                }
            }
            if let Some(rest) = line
                .strip_prefix("rename from ")
                .or_else(|| line.strip_prefix("copy from "))
            {
                file.prev_name = Some(decode_diff_file_name(rest, false));
            }
            if let Some(rest) = line
                .strip_prefix("rename to ")
                .or_else(|| line.strip_prefix("copy to "))
            {
                file.name = decode_diff_file_name(rest, false);
            }
        }
    }
}

/// `decodeDiffFileName`.
fn decode_diff_file_name(value: &str, strip_git_prefix: bool) -> String {
    let raw = value.trim();
    let name = if raw.starts_with('"') {
        parse_quoted_diff_file_name(raw).unwrap_or_else(|| raw.to_string())
    } else {
        raw.to_string()
    };
    if strip_git_prefix && (name.starts_with("a/") || name.starts_with("b/")) {
        name[2..].to_string()
    } else {
        name
    }
}

/// `parseQuotedDiffFileName`: git's C-style quoting, octal bytes decoded as UTF-8.
pub fn parse_quoted_diff_file_name(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out: Vec<u8> = Vec::new();
    let mut index = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'"' {
            return Some(String::from_utf8_lossy(&out).into_owned());
        }
        if byte != b'\\' {
            out.push(byte);
            index += 1;
            continue;
        }
        let named = match bytes.get(index + 1) {
            Some(b'"') => Some(b'"'),
            Some(b'\\') => Some(b'\\'),
            Some(b'a') => Some(0x07),
            Some(b'b') => Some(0x08),
            Some(b't') => Some(b'\t'),
            Some(b'n') => Some(b'\n'),
            Some(b'v') => Some(0x0b),
            Some(b'f') => Some(0x0c),
            Some(b'r') => Some(b'\r'),
            _ => None,
        };
        if let Some(named) = named {
            out.push(named);
            index += 2;
            continue;
        }
        loop {
            out.push(read_octal_byte(bytes, index + 1)?);
            index += 4;
            if !(bytes.get(index) == Some(&b'\\')
                && bytes.get(index + 1).is_some_and(|b| (b'0'..=b'7').contains(b)))
            {
                break;
            }
        }
    }
    None
}

fn read_octal_byte(bytes: &[u8], index: usize) -> Option<u8> {
    let digit = |offset: usize| bytes.get(index + offset).map(|b| b.wrapping_sub(b'0'));
    let (first, second, third) = (digit(0)?, digit(1)?, digit(2)?);
    (first <= 3 && second <= 7 && third <= 7).then(|| first * 64 + second * 8 + third)
}

struct HunkHeader {
    addition_count: usize,
    addition_start: usize,
    deletion_count: usize,
    deletion_start: usize,
    hunk_context: Option<String>,
}

fn read_positive_integer(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut index = start;
    let mut value: usize = 0;
    while let Some(b) = bytes.get(index).filter(|b| b.is_ascii_digit()) {
        value = value.saturating_mul(10).saturating_add((b - b'0') as usize);
        index += 1;
    }
    (index != start).then_some((value, index))
}

/// `parseHunkHeader`.
fn parse_hunk_header(line: &str) -> Option<HunkHeader> {
    if !line.starts_with("@@ -") {
        return None;
    }
    let bytes = line.as_bytes();
    let (deletion_start, mut index) = read_positive_integer(bytes, 4)?;
    let mut deletion_count = 1;
    if bytes.get(index) == Some(&b',') {
        (deletion_count, index) = read_positive_integer(bytes, index + 1)?;
    }
    if bytes.get(index) != Some(&b' ') || bytes.get(index + 1) != Some(&b'+') {
        return None;
    }
    let (addition_start, next) = read_positive_integer(bytes, index + 2)?;
    index = next;
    let mut addition_count = 1;
    if bytes.get(index) == Some(&b',') {
        (addition_count, index) = read_positive_integer(bytes, index + 1)?;
    }
    if bytes.get(index) != Some(&b' ')
        || bytes.get(index + 1) != Some(&b'@')
        || bytes.get(index + 2) != Some(&b'@')
    {
        return None;
    }
    let context_start = index + 3;
    let hunk_context = (bytes.get(context_start) == Some(&b' '))
        .then(|| trim_line_end(&line[context_start + 1..]).to_string());
    Some(HunkHeader {
        addition_count,
        addition_start,
        deletion_count,
        deletion_start,
        hunk_context,
    })
}

fn trim_line_end(value: &str) -> &str {
    value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value)
}

fn is_git_diff_patch(data: &str) -> bool {
    data.starts_with("diff --git") || data.contains("\ndiff --git")
}

/// `splitAtLinePrefix`.
fn split_at_line_prefix<'a>(contents: &'a str, prefix: &str) -> Vec<&'a str> {
    if contents.is_empty() {
        return vec![""];
    }
    let newline_prefix = format!("\n{prefix}");
    let find = |from: usize| {
        contents
            .get(from..)
            .and_then(|rest| rest.find(&newline_prefix))
            .map(|i| from + i + 1)
    };
    let first = if contents.starts_with(prefix) {
        Some(0)
    } else {
        find(0)
    };
    let Some(first) = first else {
        return vec![contents];
    };
    let mut parts = Vec::new();
    if first > 0 {
        parts.push(&contents[..first]);
    }
    let mut start = first;
    while let Some(next) = find(start + 1) {
        parts.push(&contents[start..next]);
        start = next;
    }
    parts.push(&contents[start..]);
    parts
}

fn next_line_start(contents: &str, line_start: usize) -> usize {
    contents[line_start..]
        .find('\n')
        .map_or(contents.len(), |i| line_start + i + 1)
}

/// `splitUnifiedDiffFiles`.
fn split_unified_diff_files(contents: &str) -> Vec<&str> {
    if contents.is_empty() {
        return vec![""];
    }
    let mut parts = Vec::new();
    let mut part_start = 0;
    let mut line_start = 0;
    let mut remaining_deletions: usize = 0;
    let mut remaining_additions: usize = 0;
    let mut opened = false;
    while line_start < contents.len() {
        let next = next_line_start(contents, line_start);
        if remaining_deletions == 0 && remaining_additions == 0 {
            if is_unified_diff_file_header_at(contents, line_start) {
                if line_start > part_start {
                    parts.push(&contents[part_start..line_start]);
                }
                part_start = line_start;
                opened = true;
                line_start = next_line_start(contents, next);
                continue;
            }
            if opened
                && contents[line_start..].starts_with("@@ -")
                && let Some(header) = parse_hunk_header(&contents[line_start..next])
            {
                remaining_deletions = header.deletion_count;
                remaining_additions = header.addition_count;
            }
            line_start = next;
            continue;
        }
        match contents.as_bytes()[line_start] {
            b'\\' => {}
            b' ' => {
                remaining_deletions = remaining_deletions.saturating_sub(1);
                remaining_additions = remaining_additions.saturating_sub(1);
            }
            b'-' => remaining_deletions = remaining_deletions.saturating_sub(1),
            b'+' => remaining_additions = remaining_additions.saturating_sub(1),
            _ => {}
        }
        line_start = next;
    }
    parts.push(&contents[part_start..]);
    parts
}

fn is_unified_diff_file_header_at(contents: &str, line_start: usize) -> bool {
    let next = next_line_start(contents, line_start);
    is_unified_diff_header_line_at(contents, line_start, "---")
        && is_unified_diff_header_line_at(contents, next, "+++")
}

fn is_unified_diff_header_line_at(contents: &str, line_start: usize, prefix: &str) -> bool {
    let Some(rest) = contents.get(line_start..) else {
        return false;
    };
    let Some(after) = rest.strip_prefix(prefix) else {
        return false;
    };
    let mut chars = after.chars();
    if !matches!(chars.next(), Some(' ' | '\t')) {
        return false;
    }
    for c in chars {
        if c == '\n' || c == '\r' {
            break;
        }
        if c != ' ' && c != '\t' {
            return true;
        }
    }
    false
}

fn is_hunk_body_line(line: &str) -> bool {
    matches!(line.as_bytes().first(), Some(b'+' | b'-' | b' '))
}

fn is_format_patch_version_separator(line: &str) -> bool {
    line.strip_prefix("--")
        .is_some_and(|rest| rest.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r')))
}

/// `getParsedLineContent`.
fn parsed_line_content(raw_line: &str) -> String {
    let processed = &raw_line[1..];
    if processed.is_empty() {
        "\n".to_string()
    } else {
        processed.to_string()
    }
}

// ── realignChangeContent.ts ─────────────────────────────────────────────────

const MAX_ALIGNMENT_COMPARISONS: usize = 4096;
const MIN_IMPROVEMENT_PER_PAIR: f64 = 0.5;

/// `realignChangeContentBySimilarity`: re-split count-mismatched change
/// blocks so paired lines are chosen by content similarity.
pub fn realign_change_content_by_similarity(diff: &mut FileDiffMetadata) {
    for hunk_index in 0..diff.hunks.len() {
        let mut index = 0;
        while index < diff.hunks[hunk_index].hunk_content.len() {
            let content = diff.hunks[hunk_index].hunk_content[index];
            if let Some(replacement) = realign_change_block(diff, content) {
                let len = replacement.len();
                diff.hunks[hunk_index]
                    .hunk_content
                    .splice(index..=index, replacement);
                index += len;
            } else {
                index += 1;
            }
        }
    }
}

fn realign_change_block(diff: &FileDiffMetadata, content: HunkContent) -> Option<Vec<HunkContent>> {
    let HunkContent::Change {
        deletions,
        deletion_line_index,
        additions,
        addition_line_index,
    } = content
    else {
        return None;
    };
    let pair_count = deletions.min(additions);
    let surplus = additions.abs_diff(deletions);
    if pair_count == 0 || surplus == 0 || pair_count * (surplus + 1) > MAX_ALIGNMENT_COMPARISONS {
        return None;
    }
    let stripped = |lines: &[String], start: usize, count: usize| -> Vec<Vec<u16>> {
        (0..count)
            .map(|i| {
                lines
                    .get(start + i)
                    .map(|line| strip_whitespace(line))
                    .unwrap_or_default()
            })
            .collect()
    };
    let stripped_deletions = stripped(&diff.deletion_lines, deletion_line_index, deletions);
    let stripped_additions = stripped(&diff.addition_lines, addition_line_index, additions);
    let additions_longer = additions > deletions;
    let mut best_offset = 0;
    let mut best_score = -1.;
    for offset in 0..=surplus {
        let mut score = 0.;
        for pair in 0..pair_count {
            let deletion = &stripped_deletions[pair + if additions_longer { 0 } else { offset }];
            let addition = &stripped_additions[pair + if additions_longer { offset } else { 0 }];
            score += line_similarity(deletion, addition);
        }
        if offset == 0 {
            best_score = score + pair_count as f64 * MIN_IMPROVEMENT_PER_PAIR;
        } else if score > best_score {
            best_score = score;
            best_offset = offset;
        }
    }
    if best_offset == 0 {
        return None;
    }
    let mut blocks = Vec::new();
    let mut push = |d: usize, a: usize, di: usize, ai: usize| {
        if d > 0 || a > 0 {
            blocks.push(HunkContent::Change {
                deletions: d,
                deletion_line_index: di,
                additions: a,
                addition_line_index: ai,
            });
        }
    };
    if additions_longer {
        push(0, best_offset, deletion_line_index, addition_line_index);
        push(
            pair_count,
            pair_count,
            deletion_line_index,
            addition_line_index + best_offset,
        );
        push(
            0,
            additions - pair_count - best_offset,
            deletion_line_index + pair_count,
            addition_line_index + best_offset + pair_count,
        );
    } else {
        push(best_offset, 0, deletion_line_index, addition_line_index);
        push(
            pair_count,
            pair_count,
            deletion_line_index + best_offset,
            addition_line_index,
        );
        push(
            deletions - pair_count - best_offset,
            0,
            deletion_line_index + best_offset + pair_count,
            addition_line_index + pair_count,
        );
    }
    Some(blocks)
}

/// `line.replace(/\s+/g, "")`, as UTF-16 units so lengths match JS.
fn strip_whitespace(line: &str) -> Vec<u16> {
    line.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .encode_utf16()
        .collect()
}

/// `lineSimilarity`: shared prefix + suffix over the longer length.
fn line_similarity(a: &[u16], b: &[u16]) -> f64 {
    if a == b {
        return 1.;
    }
    let max = a.len().max(b.len());
    let min = a.len().min(b.len());
    if min == 0 {
        return 0.;
    }
    let mut prefix = 0;
    while prefix < min && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < min - prefix && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix] {
        suffix += 1;
    }
    (prefix + suffix) as f64 / max as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(patch: &str) -> Vec<FileDiffMetadata> {
        parse_patch_files(patch, Some("test"))
            .into_iter()
            .flat_map(|p| p.files)
            .collect()
    }

    #[test]
    fn parses_a_git_patch_into_hunks() {
        let files = parse(
            "diff --git a/src/app.ts b/src/app.ts\nindex 1111111..2222222 100644\n--- a/src/app.ts\n+++ b/src/app.ts\n@@ -1,4 +1,4 @@ function main() {\n one\n-two\n+TWO\n three\n four\n",
        );
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!(file.name, "src/app.ts");
        assert_eq!(file.prev_name, None);
        assert_eq!(file.r#type, ChangeType::Change);
        assert_eq!(file.prev_object_id.as_deref(), Some("1111111"));
        assert_eq!(file.mode.as_deref(), Some("100644"));
        assert!(file.is_partial);
        let hunk = &file.hunks[0];
        assert_eq!(hunk.hunk_context.as_deref(), Some("function main() {"));
        assert_eq!(hunk.addition_lines, 1);
        assert_eq!(hunk.deletion_lines, 1);
        assert_eq!(hunk.unified_line_count, 5);
        assert_eq!(hunk.split_line_count, 4);
        assert_eq!(hunk.collapsed_before, 0);
        assert_eq!(
            hunk.hunk_content,
            vec![
                HunkContent::Context {
                    lines: 1,
                    addition_line_index: 0,
                    deletion_line_index: 0
                },
                HunkContent::Change {
                    deletions: 1,
                    deletion_line_index: 1,
                    additions: 1,
                    addition_line_index: 1
                },
                HunkContent::Context {
                    lines: 2,
                    addition_line_index: 2,
                    deletion_line_index: 2
                },
            ]
        );
        assert_eq!(file.addition_lines, vec!["one\n", "TWO\n", "three\n", "four\n"]);
        assert!(file.cache_key.as_deref().unwrap().starts_with("ck1:"));
    }

    #[test]
    fn detects_new_deleted_and_renamed_files() {
        let files = parse(concat!(
            "diff --git a/new.ts b/new.ts\nnew file mode 100644\n--- /dev/null\n+++ b/new.ts\n@@ -0,0 +1 @@\n+hello\n",
            "diff --git a/gone.ts b/gone.ts\ndeleted file mode 100644\n--- a/gone.ts\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
            "diff --git a/old.ts b/moved.ts\nsimilarity index 100%\nrename from old.ts\nrename to moved.ts\n",
        ));
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].r#type, ChangeType::New);
        assert_eq!(files[1].r#type, ChangeType::Deleted);
        assert_eq!(files[1].name, "gone.ts");
        assert_eq!(files[2].r#type, ChangeType::RenamePure);
        assert_eq!(files[2].name, "moved.ts");
        assert_eq!(files[2].prev_name.as_deref(), Some("old.ts"));
    }

    #[test]
    fn no_newline_marker_trims_the_last_line() {
        let files = parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
        );
        let hunk = &files[0].hunks[0];
        assert!(hunk.no_eofcr_deletions);
        assert!(hunk.no_eofcr_additions);
        assert_eq!(files[0].deletion_lines, vec!["old"]);
        assert_eq!(files[0].addition_lines, vec!["new"]);
    }

    #[test]
    fn collapsed_before_counts_unchanged_lines_between_hunks() {
        let files = parse(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10,2 +10,2 @@\n-x\n+y\n z\n@@ -40 +40 @@\n-p\n+q\n",
        );
        let hunks = &files[0].hunks;
        assert_eq!(hunks[0].collapsed_before, 9);
        assert_eq!(hunks[1].collapsed_before, 40 - 1 - 11);
        assert_eq!(hunks[0].unified_line_start, 9);
    }

    #[test]
    fn decodes_quoted_names() {
        assert_eq!(
            parse_quoted_diff_file_name(r#""caf\303\251.ts""#).as_deref(),
            Some("café.ts")
        );
        assert_eq!(
            parse_quoted_diff_file_name(r#""tab\tname""#).as_deref(),
            Some("tab\tname")
        );
    }

    #[test]
    fn realigns_insertions_by_similarity() {
        let files = parse(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,1 +1,2 @@\n-const value = compute(a);\n+// added comment\n+const value = compute(b);\n",
        );
        assert_eq!(
            files[0].hunks[0].hunk_content,
            vec![
                HunkContent::Change {
                    deletions: 0,
                    deletion_line_index: 0,
                    additions: 1,
                    addition_line_index: 0
                },
                HunkContent::Change {
                    deletions: 1,
                    deletion_line_index: 0,
                    additions: 1,
                    addition_line_index: 1
                },
            ]
        );
    }

    #[test]
    fn plain_unified_diffs_infer_renames() {
        let files = parse("--- a/old.txt\n+++ b/new.txt\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].r#type, ChangeType::RenameChanged);
        assert_eq!(files[0].name, "b/new.txt");
    }
}
