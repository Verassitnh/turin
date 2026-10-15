//! Port of `@pierre/diffs@1.5.2` `dist/utils/hydratePartialDiff.js` and
//! `parseDiffFromFile.js`.
//!
//! `parseDiffFromFile` uses jsdiff's `createTwoFilesPatch`; Turin builds the
//! same unified patch text from `similar`'s Myers line diff and feeds it to
//! [`process_file`], exactly as upstream does.

use similar::{Algorithm, ChangeTag, TextDiff};

use super::parse_patch_files::{ProcessFileOptions, process_file};
use super::types::{
    ChangeType, FileContents, FileDiffMetadata, HunkContent, compose_cache_key,
    get_hunk_side_end_boundary, get_hunk_side_start_boundary, split_file_contents,
};

const MISSING_FILE_NAME: &str = "/dev/null";

/// `hydratePartialDiff(type = "clone", …)`. Errors mirror upstream's throws.
pub fn hydrate_partial_diff(
    file_diff: &FileDiffMetadata,
    old_file: Option<&FileContents>,
    new_file: Option<&FileContents>,
) -> Result<FileDiffMetadata, String> {
    if !file_diff.is_partial {
        return Err("hydratePartialDiff: fileDiff must be partial".into());
    }
    let mut target = file_diff.clone();
    match target.r#type {
        ChangeType::Change | ChangeType::RenameChanged => {
            let old = old_file.ok_or_else(|| {
                format!(
                    "hydratePartialDiff: {} diff for {} requires oldFile",
                    target.r#type.as_str(),
                    target.name
                )
            })?;
            let new = new_file.ok_or_else(|| {
                format!(
                    "hydratePartialDiff: {} diff for {} requires newFile",
                    target.r#type.as_str(),
                    target.name
                )
            })?;
            let deletion_lines = split_file_contents(&old.contents);
            let addition_lines = split_file_contents(&new.contents);
            let (hunks, split, unified) = hydrate_hunks(&target.hunks, addition_lines.len());
            target.hunks = hunks;
            target.split_line_count = split;
            target.unified_line_count = unified;
            target.is_partial = false;
            target.deletion_lines = deletion_lines;
            target.addition_lines = addition_lines;
            set_hydrated_cache_key(&mut target, Some(old), Some(new));
            Ok(target)
        }
        ChangeType::RenamePure => {
            let new = new_file.ok_or_else(|| {
                format!(
                    "hydratePartialDiff: rename-pure diff for {} requires newFile",
                    target.name
                )
            })?;
            let lines = split_file_contents(&new.contents);
            target.is_partial = false;
            target.deletion_lines = lines.clone();
            target.addition_lines = lines;
            set_hydrated_cache_key(&mut target, None, Some(new));
            Ok(target)
        }
        other => Err(format!(
            "hydratePartialDiff: {} diffs cannot be hydrated from loaded files",
            other.as_str()
        )),
    }
}

fn hydrate_hunks(
    hunks: &[super::types::Hunk],
    total_addition_lines: usize,
) -> (Vec<super::types::Hunk>, usize, usize) {
    let mut split_line_count = 0;
    let mut unified_line_count = 0;
    let mut last_hunk_addition_end = 0usize;
    let mut hydrated = Vec::new();
    for hunk in hunks {
        let addition_line_index = hunk.addition_start.saturating_sub(1);
        let deletion_line_index = hunk.deletion_start.saturating_sub(1);
        let mut content_addition = addition_line_index;
        let mut content_deletion = deletion_line_index;
        let (mut addition_lines, mut deletion_lines) = (0, 0);
        let (mut hunk_split, mut hunk_unified) = (0, 0);
        let mut hunk_content = Vec::new();
        for content in &hunk.hunk_content {
            match *content {
                HunkContent::Context { lines, .. } => {
                    hunk_content.push(HunkContent::Context {
                        lines,
                        addition_line_index: content_addition,
                        deletion_line_index: content_deletion,
                    });
                    content_addition += lines;
                    content_deletion += lines;
                    hunk_split += lines;
                    hunk_unified += lines;
                }
                HunkContent::Change {
                    deletions,
                    additions,
                    ..
                } => {
                    hunk_content.push(HunkContent::Change {
                        deletions,
                        deletion_line_index: content_deletion,
                        additions,
                        addition_line_index: content_addition,
                    });
                    content_addition += additions;
                    content_deletion += deletions;
                    addition_lines += additions;
                    deletion_lines += deletions;
                    hunk_split += additions.max(deletions);
                    hunk_unified += additions + deletions;
                }
            }
        }
        let collapsed_before = get_hunk_side_start_boundary(hunk.addition_start, hunk.addition_count)
            .saturating_sub(last_hunk_addition_end);
        let mut next = hunk.clone();
        next.collapsed_before = collapsed_before;
        next.addition_line_index = addition_line_index;
        next.deletion_line_index = deletion_line_index;
        next.addition_lines = addition_lines;
        next.deletion_lines = deletion_lines;
        next.hunk_content = hunk_content;
        next.split_line_start = split_line_count + collapsed_before;
        next.unified_line_start = unified_line_count + collapsed_before;
        next.split_line_count = hunk_split;
        next.unified_line_count = hunk_unified;
        hydrated.push(next);
        split_line_count += collapsed_before + hunk_split;
        unified_line_count += collapsed_before + hunk_unified;
        last_hunk_addition_end = get_hunk_side_end_boundary(hunk.addition_start, hunk.addition_count);
    }
    if let Some(last) = hydrated.last() {
        let last_end = get_hunk_side_end_boundary(last.addition_start, last.addition_count);
        let collapsed_after = total_addition_lines.saturating_sub(last_end);
        split_line_count += collapsed_after;
        unified_line_count += collapsed_after;
    }
    (hydrated, split_line_count, unified_line_count)
}

fn set_hydrated_cache_key(
    file: &mut FileDiffMetadata,
    old: Option<&FileContents>,
    new: Option<&FileContents>,
) {
    file.cache_key = match &file.cache_key {
        Some(key) => Some(format!("{key}:hydrated")),
        None => match (old, new) {
            (Some(old), Some(new)) => match (&old.cache_key, &new.cache_key) {
                (Some(a), Some(b)) => Some(compose_cache_key("hydrated-files", &[a, b])),
                _ => None,
            },
            (old, new) => old
                .and_then(|f| f.cache_key.clone())
                .or_else(|| new.and_then(|f| f.cache_key.clone())),
        },
    };
}

/// `parseDiffFromFile(oldFile, newFile, { context })`. `context: None` is
/// jsdiff's `Infinity`. `None` for both files is upstream's thrown error.
pub fn parse_diff_from_file(
    old_file: Option<&FileContents>,
    new_file: Option<&FileContents>,
    context: Option<usize>,
) -> Option<FileDiffMetadata> {
    if old_file.is_none() && new_file.is_none() {
        return None;
    }
    let missing = FileContents {
        name: MISSING_FILE_NAME.into(),
        contents: String::new(),
        cache_key: None,
    };
    let resolved_old = old_file.unwrap_or(&missing);
    let resolved_new = new_file.unwrap_or(&missing);
    let patch = create_two_files_patch(resolved_old, resolved_new, context);
    let cache_key = match (
        old_file.and_then(|f| f.cache_key.as_deref()),
        new_file.and_then(|f| f.cache_key.as_deref()),
    ) {
        (Some(a), Some(b)) => Some(compose_cache_key("diff", &[a, b])),
        _ => None,
    };
    let mut file = process_file(
        &patch,
        ProcessFileOptions {
            cache_key,
            old_file: Some(resolved_old),
            new_file: Some(resolved_new),
            ..Default::default()
        },
    )?;
    if old_file.is_none() {
        file.r#type = ChangeType::New;
        file.prev_name = None;
    } else if new_file.is_none() {
        file.r#type = ChangeType::Deleted;
        file.prev_name = None;
    }
    Some(file)
}

/// jsdiff `createTwoFilesPatch` (without the `Index:` banner, which the
/// parser ignores).
fn create_two_files_patch(old: &FileContents, new: &FileContents, context: Option<usize>) -> String {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_lines(old.contents.as_str(), new.contents.as_str());
    let mut out = format!("--- {}\n+++ {}\n", old.name, new.name);
    let groups = match context {
        Some(context) => diff.grouped_ops(context),
        None => {
            let ops = diff.ops().to_vec();
            if ops.iter().all(|op| matches!(op.tag(), similar::DiffTag::Equal)) {
                Vec::new()
            } else {
                vec![ops]
            }
        }
    };
    for group in groups {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_start = first.old_range().start;
        let new_start = first.new_range().start;
        let old_len = last.old_range().end - old_start;
        let new_len = last.new_range().end - new_start;
        let header_start = |start: usize, len: usize| if len == 0 { start } else { start + 1 };
        out.push_str(&format!(
            "@@ -{}{} +{}{} @@\n",
            header_start(old_start, old_len),
            if old_len == 1 { String::new() } else { format!(",{old_len}") },
            header_start(new_start, new_len),
            if new_len == 1 { String::new() } else { format!(",{new_len}") },
        ));
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                };
                out.push(sign);
                let value = change.value();
                out.push_str(value);
                if !value.ends_with('\n') {
                    out.push_str("\n\\ No newline at end of file\n");
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents(name: &str, contents: &str) -> FileContents {
        FileContents {
            name: name.into(),
            contents: contents.into(),
            cache_key: None,
        }
    }

    #[test]
    fn diffs_two_files_with_full_context() {
        let old = contents("a.ts", "one\ntwo\nthree\n");
        let new = contents("a.ts", "one\nTWO\nthree\nfour\n");
        let file = parse_diff_from_file(Some(&old), Some(&new), None).unwrap();
        assert!(!file.is_partial);
        assert_eq!(file.hunks.len(), 1);
        assert_eq!(file.hunks[0].addition_lines, 2);
        assert_eq!(file.hunks[0].deletion_lines, 1);
        assert_eq!(file.addition_lines.len(), 4);
        assert_eq!(file.r#type, ChangeType::Change);
    }

    #[test]
    fn missing_old_file_is_new() {
        let new = contents("a.ts", "x\n");
        let file = parse_diff_from_file(None, Some(&new), None).unwrap();
        assert_eq!(file.r#type, ChangeType::New);
        assert_eq!(file.hunks[0].addition_lines, 1);
    }

    #[test]
    fn hydrating_a_partial_diff_recomputes_layout() {
        let patch = "diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -3,3 +3,3 @@\n c\n-d\n+D\n e\n";
        let partial = crate::diff::pierre::parse_patch_files::parse_patch_files(patch, None)
            .remove(0)
            .files
            .remove(0);
        assert_eq!(partial.hunks[0].collapsed_before, 2);
        let old = contents("a.ts", "a\nb\nc\nd\ne\nf\ng\n");
        let new = contents("a.ts", "a\nb\nc\nD\ne\nf\ng\n");
        let hydrated = hydrate_partial_diff(&partial, Some(&old), Some(&new)).unwrap();
        assert!(!hydrated.is_partial);
        assert_eq!(hydrated.addition_lines.len(), 7);
        assert_eq!(hydrated.hunks[0].collapsed_before, 2);
        // 2 before + 3 hunk rows (unified: c, -d, +D, e = 4) + 2 trailing.
        assert_eq!(hydrated.unified_line_count, 2 + 4 + 2);
        assert!(hydrate_partial_diff(&hydrated, Some(&old), Some(&new)).is_err());
    }
}
