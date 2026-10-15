//! Port of upstream/apps/web/src/lib/diffRendering.ts (+ the cases of
//! diffRendering.test.ts that exercise ported logic).
//!
//! `DIFF_SURFACE_THEME_UNSAFE_CSS` (the CSS overrides routing the app's code
//! tokens into `@pierre/diffs`) is realised by [`super::theme`], which
//! resolves the same `color-mix()` formulas to concrete colors.

use super::pierre::hydrate_partial_diff::parse_diff_from_file;
use super::pierre::parse_patch_files::parse_patch_files;
use super::pierre::types::{ChangeType, FileContents, FileDiffMetadata, Hunk, HunkContent};

/// `DiffThemeName`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffThemeName {
    PierreLight,
    PierreDark,
}

/// `resolveDiffThemeName`.
pub fn resolve_diff_theme_name(dark: bool) -> DiffThemeName {
    if dark {
        DiffThemeName::PierreDark
    } else {
        DiffThemeName::PierreLight
    }
}

const FNV_OFFSET_BASIS_32: u32 = 0x811c_9dc5;
const FNV_PRIME_32: u32 = 0x0100_0193;
const SECONDARY_HASH_SEED: u32 = 0x9e37_79b9;
const SECONDARY_HASH_MULTIPLIER: u32 = 0x85eb_ca6b;

/// `fnv1a32`: hashes UTF-16 code units, as `charCodeAt` does.
pub fn fnv1a32(input: &str, seed: u32, multiplier: u32) -> u32 {
    let mut hash = seed;
    for unit in input.encode_utf16() {
        hash ^= unit as u32;
        hash = hash.wrapping_mul(multiplier);
    }
    hash
}

fn to_base36(mut value: u32) -> String {
    if value == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(char::from_digit(value % 36, 36).unwrap_or('0'));
        value /= 36;
    }
    digits.iter().rev().collect()
}

/// `buildPatchCacheKey`.
pub fn build_patch_cache_key(patch: &str, scope: &str) -> String {
    let normalized = patch.trim();
    let primary = to_base36(fnv1a32(normalized, FNV_OFFSET_BASIS_32, FNV_PRIME_32));
    let secondary = to_base36(fnv1a32(
        normalized,
        SECONDARY_HASH_SEED,
        SECONDARY_HASH_MULTIPLIER,
    ));
    format!(
        "{scope}:{}:{primary}:{secondary}",
        normalized.encode_utf16().count()
    )
}

/// `RenderablePatch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderablePatch {
    Files {
        files: Vec<FileDiffMetadata>,
        source_files: Vec<FileDiffMetadata>,
    },
    Raw {
        text: String,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffLineStat {
    pub additions: usize,
    pub deletions: usize,
}

/// `getDiffLineStat`.
pub fn get_diff_line_stat<'a>(
    files: impl IntoIterator<Item = &'a FileDiffMetadata>,
) -> DiffLineStat {
    let mut total = DiffLineStat::default();
    for file in files {
        for hunk in &file.hunks {
            total.additions += hunk.addition_lines;
            total.deletions += hunk.deletion_lines;
        }
    }
    total
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RenderablePatchOptions {
    pub ignore_whitespace: bool,
    /// Pierre's partial-patch parser keeps hunk render starts in source-file
    /// coordinates; review diffs need compact render starts while retaining
    /// `collapsedBefore` for the "N unmodified lines" separator.
    pub compact_partial_hunk_offsets: bool,
}

fn strip_ws(line: &str) -> String {
    line.chars().filter(|c| !c.is_whitespace()).collect()
}

fn hide_whitespace_changes(file: &FileDiffMetadata) -> FileDiffMetadata {
    let mut split_delta: isize = 0;
    let mut unified_delta: isize = 0;
    let slice = |lines: &[String], start: usize, count: usize| -> String {
        lines
            .iter()
            .skip(start)
            .take(count)
            .map(|line| format!("{}\n", strip_ws(line)))
            .collect()
    };
    let hunks: Vec<Hunk> = file
        .hunks
        .iter()
        .map(|hunk| {
            let old_contents = slice(
                &file.deletion_lines,
                hunk.deletion_line_index,
                hunk.deletion_count,
            );
            let new_contents = slice(
                &file.addition_lines,
                hunk.addition_line_index,
                hunk.addition_count,
            );
            let old = FileContents {
                name: file.name.clone(),
                contents: old_contents,
                cache_key: None,
            };
            let new = FileContents {
                name: file.name.clone(),
                contents: new_contents,
                cache_key: None,
            };
            let filtered = parse_diff_from_file(Some(&old), Some(&new), None)
                .and_then(|diff| diff.hunks.into_iter().next());
            let mut next = hunk.clone();
            next.addition_lines = filtered.as_ref().map_or(0, |f| f.addition_lines);
            next.deletion_lines = filtered.as_ref().map_or(0, |f| f.deletion_lines);
            next.hunk_content = match &filtered {
                Some(filtered) => filtered
                    .hunk_content
                    .iter()
                    .map(|content| {
                        let mut content = *content;
                        content.set_line_indices(
                            content.addition_line_index() + hunk.addition_line_index,
                            content.deletion_line_index() + hunk.deletion_line_index,
                        );
                        content
                    })
                    .collect(),
                None => vec![HunkContent::Context {
                    lines: hunk.addition_count,
                    addition_line_index: hunk.addition_line_index,
                    deletion_line_index: hunk.deletion_line_index,
                }],
            };
            next.split_line_start = (hunk.split_line_start as isize + split_delta).max(0) as usize;
            next.unified_line_start =
                (hunk.unified_line_start as isize + unified_delta).max(0) as usize;
            next.split_line_count = filtered
                .as_ref()
                .map_or(hunk.addition_count, |f| f.split_line_count);
            next.unified_line_count = filtered
                .as_ref()
                .map_or(hunk.addition_count, |f| f.unified_line_count);
            split_delta += next.split_line_count as isize - hunk.split_line_count as isize;
            unified_delta += next.unified_line_count as isize - hunk.unified_line_count as isize;
            next
        })
        .collect();
    let mut next = file.clone();
    next.hunks = hunks;
    next.split_line_count = (file.split_line_count as isize + split_delta).max(0) as usize;
    next.unified_line_count = (file.unified_line_count as isize + unified_delta).max(0) as usize;
    if let Some(key) = &file.cache_key {
        next.cache_key = Some(format!("{key}:ignore-whitespace"));
    }
    next
}

fn compact_partial_hunk_offsets(file: &FileDiffMetadata) -> FileDiffMetadata {
    if !file.is_partial {
        return file.clone();
    }
    let mut split_line_start = 0;
    let mut unified_line_start = 0;
    let mut next = file.clone();
    for hunk in &mut next.hunks {
        hunk.split_line_start = split_line_start;
        hunk.unified_line_start = unified_line_start;
        split_line_start += hunk.split_line_count;
        unified_line_start += hunk.unified_line_count;
    }
    next.split_line_count = split_line_start;
    next.unified_line_count = unified_line_start;
    if let Some(key) = &file.cache_key {
        next.cache_key = Some(format!("{key}:compact-partial"));
    }
    next
}

/// `getRenderablePatch`.
pub fn get_renderable_patch(
    patch: Option<&str>,
    cache_scope: &str,
    options: RenderablePatchOptions,
) -> Option<RenderablePatch> {
    let normalized = patch?.trim();
    if normalized.is_empty() {
        return None;
    }
    let parsed = parse_patch_files(
        normalized,
        Some(&build_patch_cache_key(normalized, cache_scope)),
    );
    let source_files: Vec<FileDiffMetadata> = parsed.into_iter().flat_map(|p| p.files).collect();
    let files: Vec<FileDiffMetadata> = source_files
        .iter()
        .map(|file| {
            let filtered = if options.ignore_whitespace {
                hide_whitespace_changes(file)
            } else {
                file.clone()
            };
            if options.compact_partial_hunk_offsets {
                compact_partial_hunk_offsets(&filtered)
            } else {
                filtered
            }
        })
        .collect();
    if !files.is_empty() {
        return Some(RenderablePatch::Files {
            files,
            source_files,
        });
    }
    Some(RenderablePatch::Raw {
        text: normalized.to_string(),
        reason: "Unsupported diff format. Showing raw patch.".into(),
    })
}

/// `resolveFileDiffPath`: the file's own name (the parser decodes git's
/// quoting).
pub fn resolve_file_diff_path(file: &FileDiffMetadata) -> String {
    if file.name.is_empty() {
        file.prev_name.clone().unwrap_or_default()
    } else {
        file.name.clone()
    }
}

/// `resolveFileDiffPreviousPath`.
pub fn resolve_file_diff_previous_path(file: &FileDiffMetadata) -> String {
    file.prev_name.clone().unwrap_or_else(|| file.name.clone())
}

/// `buildFileDiffIdentityKey`: stable across re-renders of the same file,
/// distinct for every block in a patch (a type change is a deletion plus an
/// addition of one path).
pub fn build_file_diff_identity_key(file: &FileDiffMetadata) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        resolve_file_diff_previous_path(file),
        resolve_file_diff_path(file),
        file.r#type.as_str()
    )
}

/// `buildFileDiffRenderKey`.
pub fn build_file_diff_render_key(file: &FileDiffMetadata) -> String {
    match &file.cache_key {
        None => format!(
            "{}:{}",
            file.prev_name.as_deref().unwrap_or("none"),
            file.name
        ),
        Some(key) => key.strip_suffix(":hydrated").unwrap_or(key).to_string(),
    }
}

/// `buildFileDiffContentVersion`: content-only, so one changed file does not
/// repaint every file.
pub fn build_file_diff_content_version(file: &FileDiffMetadata) -> u32 {
    let mut hash = FNV_OFFSET_BASIS_32;
    let mut append = |kind: &str, value: Option<String>| {
        let serialized = value.unwrap_or_else(|| "undefined".into());
        let with_length = fnv1a32(
            &format!("{kind}:{}:", serialized.encode_utf16().count()),
            hash,
            FNV_PRIME_32,
        );
        hash = fnv1a32(&serialized, with_length, FNV_PRIME_32);
    };
    let string = |value: &Option<String>| ("string", value.clone());
    let (kind, value) = ("string", Some(file.name.clone()));
    append(kind, value);
    let (kind, value) = string(&file.prev_name);
    append(if value.is_some() { kind } else { "undefined" }, value);
    for value in [
        &file.lang,
        &file.new_object_id,
        &file.prev_object_id,
        &file.mode,
        &file.prev_mode,
    ] {
        let (kind, value) = string(value);
        append(if value.is_some() { kind } else { "undefined" }, value);
    }
    append("string", Some(file.r#type.as_str().into()));
    append("boolean", Some(file.is_partial.to_string()));
    append("number", Some(file.split_line_count.to_string()));
    append("number", Some(file.unified_line_count.to_string()));
    for line in &file.addition_lines {
        append("string", Some(line.clone()));
    }
    for line in &file.deletion_lines {
        append("string", Some(line.clone()));
    }
    for hunk in &file.hunks {
        for number in [
            hunk.collapsed_before,
            hunk.addition_start,
            hunk.addition_count,
            hunk.addition_lines,
            hunk.addition_line_index,
            hunk.deletion_start,
            hunk.deletion_count,
            hunk.deletion_lines,
            hunk.deletion_line_index,
        ] {
            append("number", Some(number.to_string()));
        }
        match &hunk.hunk_context {
            Some(context) => append("string", Some(context.clone())),
            None => append("undefined", None),
        }
        match &hunk.hunk_specs {
            Some(specs) => append("string", Some(specs.clone())),
            None => append("undefined", None),
        }
        for number in [
            hunk.split_line_start,
            hunk.split_line_count,
            hunk.unified_line_start,
            hunk.unified_line_count,
        ] {
            append("number", Some(number.to_string()));
        }
        append("boolean", Some(hunk.no_eofcr_additions.to_string()));
        append("boolean", Some(hunk.no_eofcr_deletions.to_string()));
        for content in &hunk.hunk_content {
            match *content {
                HunkContent::Context {
                    lines,
                    addition_line_index,
                    deletion_line_index,
                } => {
                    append("string", Some("context".into()));
                    append("number", Some(addition_line_index.to_string()));
                    append("number", Some(deletion_line_index.to_string()));
                    append("number", Some(lines.to_string()));
                    append("undefined", None);
                }
                HunkContent::Change {
                    deletions,
                    deletion_line_index,
                    additions,
                    addition_line_index,
                } => {
                    append("string", Some("change".into()));
                    append("number", Some(addition_line_index.to_string()));
                    append("number", Some(deletion_line_index.to_string()));
                    append("number", Some(additions.to_string()));
                    append("number", Some(deletions.to_string()));
                }
            }
        }
    }
    hash
}

/// `getDiffCollapseIconClassName`: which token the collapse chevron takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffCollapseIconTone {
    Addition,
    Deletion,
    Modified,
    Muted,
}

pub fn get_diff_collapse_icon_tone(file: &FileDiffMetadata) -> DiffCollapseIconTone {
    match file.r#type {
        ChangeType::New => DiffCollapseIconTone::Addition,
        ChangeType::Deleted => DiffCollapseIconTone::Deletion,
        ChangeType::Change | ChangeType::RenamePure | ChangeType::RenameChanged => {
            DiffCollapseIconTone::Modified
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::pierre::hydrate_partial_diff::hydrate_partial_diff;
    use crate::diff::pierre::types::SelectionSide;
    use crate::diff::review_comment_context::{ReviewPosition, resolve_diff_review_position};

    fn files(patch: &str, scope: &str, options: RenderablePatchOptions) -> Vec<FileDiffMetadata> {
        match get_renderable_patch(Some(patch), scope, options) {
            Some(RenderablePatch::Files { files, .. }) => files,
            other => panic!("expected files, got {other:?}"),
        }
    }

    fn lines(items: &[&str]) -> String {
        items.join("\n")
    }

    #[test]
    fn patch_cache_key_normalizes_outer_whitespace_before_hashing() {
        let patch = "diff --git a/a.ts b/a.ts\n+console.log('hello')";
        assert_eq!(
            build_patch_cache_key(&format!("\n{patch}\n"), "diff-panel"),
            build_patch_cache_key(patch, "diff-panel")
        );
    }

    #[test]
    fn patch_cache_key_changes_with_content_and_scope() {
        let before = "diff --git a/a.ts b/a.ts\n+console.log('hello')";
        let after = "diff --git a/a.ts b/a.ts\n+console.log('hello world')";
        assert_ne!(
            build_patch_cache_key(before, "diff-panel"),
            build_patch_cache_key(after, "diff-panel")
        );
        assert_ne!(
            build_patch_cache_key(before, "diff-panel:light"),
            build_patch_cache_key(before, "diff-panel:dark")
        );
    }

    #[test]
    fn fnv_matches_known_vectors() {
        assert_eq!(fnv1a32("", FNV_OFFSET_BASIS_32, FNV_PRIME_32), 0x811c_9dc5);
        assert_eq!(fnv1a32("a", FNV_OFFSET_BASIS_32, FNV_PRIME_32), 0xe40c_292c);
        assert_eq!(to_base36(35), "z");
    }

    #[test]
    fn hides_indentation_changes_around_inserted_jsx_without_moving_review_lines() {
        let patch = lines(&[
            "diff --git a/item.tsx b/item.tsx",
            "--- a/item.tsx",
            "+++ b/item.tsx",
            "@@ -40,5 +40,7 @@",
            " <ItemContent className=\"min-w-0\">",
            "-  <ItemTitle>",
            "-    <h4 className=\"wrap-break-word\">{name}</h4>",
            "-  </ItemTitle>",
            "+  {showName && (",
            "+    <ItemTitle>",
            "+      <h4 className=\"wrap-break-word\">{name}</h4>",
            "+    </ItemTitle>",
            "+  )}",
            " </ItemContent>",
            "@@ -80 +82 @@",
            "-const value = 1;",
            "+const value = 2;",
        ]);
        let compact = RenderablePatchOptions {
            compact_partial_hunk_offsets: true,
            ..Default::default()
        };
        let hidden_options = RenderablePatchOptions {
            ignore_whitespace: true,
            compact_partial_hunk_offsets: true,
        };
        let shown = get_renderable_patch(Some(&patch), "pr", compact).unwrap();
        let hidden = get_renderable_patch(Some(&patch), "pr", hidden_options).unwrap();
        let (
            RenderablePatch::Files {
                files: shown_files, ..
            },
            RenderablePatch::Files {
                files: hidden_files,
                source_files,
            },
        ) = (shown, hidden)
        else {
            panic!("expected files");
        };
        assert_eq!(
            get_diff_line_stat(&shown_files),
            DiffLineStat {
                additions: 6,
                deletions: 4
            }
        );
        assert_eq!(
            get_diff_line_stat(&hidden_files),
            DiffLineStat {
                additions: 3,
                deletions: 1
            }
        );
        let file = &hidden_files[0];
        assert_eq!(file.addition_lines, shown_files[0].addition_lines);
        assert_eq!(file.deletion_lines, shown_files[0].deletion_lines);
        assert_ne!(file.cache_key, shown_files[0].cache_key);
        assert_eq!(
            resolve_diff_review_position(&source_files[0], 43, Some(SelectionSide::Additions)),
            Some(ReviewPosition::Added { new_line: 43 })
        );
        assert_eq!(
            resolve_diff_review_position(&source_files[0], 42, Some(SelectionSide::Deletions)),
            Some(ReviewPosition::Deleted { old_line: 42 })
        );
        assert!(file.hunks[0].hunk_content.contains(&HunkContent::Context {
            lines: 3,
            addition_line_index: 2,
            deletion_line_index: 1
        }));
        assert_eq!(
            resolve_diff_review_position(file, 41, Some(SelectionSide::Additions)),
            Some(ReviewPosition::Added { new_line: 41 })
        );
        assert_eq!(
            resolve_diff_review_position(file, 43, Some(SelectionSide::Additions)),
            Some(ReviewPosition::Context {
                old_line: 42,
                new_line: 43,
                side: super::super::review_comment_context::ContextSide::Right
            })
        );
        assert_eq!(
            resolve_diff_review_position(file, 42, Some(SelectionSide::Deletions)),
            Some(ReviewPosition::Context {
                old_line: 42,
                new_line: 43,
                side: super::super::review_comment_context::ContextSide::Left
            })
        );
        let second = &file.hunks[1];
        assert_eq!(
            (
                second.addition_start,
                second.deletion_start,
                second.split_line_start,
                second.unified_line_start
            ),
            (82, 80, 7, 7)
        );
        let prefix = "unchanged\n".repeat(39);
        let gap = "unchanged\n".repeat(35);
        let join = |lines: &[String]| lines.concat();
        let old = FileContents {
            name: file.name.clone(),
            contents: format!(
                "{prefix}{}{gap}const value = 1;\n",
                join(&file.deletion_lines[..5])
            ),
            cache_key: None,
        };
        let new = FileContents {
            name: file.name.clone(),
            contents: format!(
                "{prefix}{}{gap}const value = 2;\n",
                join(&file.addition_lines[..7])
            ),
            cache_key: None,
        };
        let hydrated = hydrate_partial_diff(file, Some(&old), Some(&new)).unwrap();
        assert_eq!(
            get_diff_line_stat([&hydrated]),
            DiffLineStat {
                additions: 3,
                deletions: 1
            }
        );
        assert_eq!(
            resolve_diff_review_position(&hydrated, 43, Some(SelectionSide::Additions)),
            resolve_diff_review_position(file, 43, Some(SelectionSide::Additions))
        );
    }

    #[test]
    fn filters_whitespace_to_the_expected_line_counts() {
        for (before, after, additions, deletions) in [
            ("  const x = 1;\t", "\tconst x=1;", 0, 0),
            ("const x = 1;", "const x = 2;", 1, 1),
            ("const x = 1;", "const x = 1;\n", 1, 0),
        ] {
            let after_lines: Vec<&str> = after.split('\n').collect();
            let mut patch = vec![
                "diff --git a/example.ts b/example.ts".to_string(),
                "--- a/example.ts".into(),
                "+++ b/example.ts".into(),
                format!("@@ -1 +1,{} @@", after_lines.len()),
                format!("-{before}"),
            ];
            patch.extend(after_lines.iter().map(|line| format!("+{line}")));
            patch.push(String::new());
            let filtered = files(
                &patch.join("\n"),
                "pr",
                RenderablePatchOptions {
                    ignore_whitespace: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                get_diff_line_stat(&filtered),
                DiffLineStat {
                    additions,
                    deletions
                },
                "{before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn keeps_blank_lines_without_a_final_newline() {
        for sign in ["+", "-"] {
            let patch = lines(&[
                "diff --git a/blank.txt b/blank.txt",
                "--- a/blank.txt",
                "+++ b/blank.txt",
                if sign == "+" {
                    "@@ -0,0 +1 @@"
                } else {
                    "@@ -1 +0,0 @@"
                },
                &format!("{sign}  "),
                "\\ No newline at end of file",
            ]);
            let parsed = files(
                &patch,
                "pr",
                RenderablePatchOptions {
                    ignore_whitespace: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                get_diff_line_stat(&parsed),
                DiffLineStat {
                    additions: usize::from(sign == "+"),
                    deletions: usize::from(sign == "-")
                },
                "{sign}"
            );
        }
    }

    #[test]
    fn preserves_repository_paths() {
        for (previous, path, kind) in [
            ("a/example.ts", "a/example.ts", "change"),
            ("b/example.ts", "b/example.ts", "change"),
            ("a/before.ts", "b/after.ts", "rename-changed"),
        ] {
            let mut patch = vec![format!("diff --git a/{previous} b/{path}")];
            if previous != path {
                patch.extend([
                    "similarity index 50%".to_string(),
                    format!("rename from {previous}"),
                    format!("rename to {path}"),
                ]);
            }
            patch.extend([
                format!("--- a/{previous}"),
                format!("+++ b/{path}"),
                "@@ -1 +1 @@".into(),
                "-before".into(),
                "+after".into(),
            ]);
            let parsed = files(&patch.join("\n"), "diff-panel", Default::default());
            let file = &parsed[0];
            assert_eq!(resolve_file_diff_path(file), path);
            assert_eq!(resolve_file_diff_previous_path(file), previous);
            assert_eq!(
                build_file_diff_identity_key(file),
                format!("{previous}\0{path}\0{kind}")
            );
        }
    }

    const REVIEW_PATCH: &str = "diff --git a/example.ts b/example.ts\nindex 1111111..2222222 100644\n--- a/example.ts\n+++ b/example.ts\n@@ -48,4 +48,4 @@\n context\n-before\n+after\n context\n context\n@@ -80,3 +80,4 @@\n context\n+added\n context\n context";

    #[test]
    fn compacts_partial_hunk_render_offsets_for_virtualized_review_diffs() {
        let parsed = files(
            REVIEW_PATCH,
            "review",
            RenderablePatchOptions {
                compact_partial_hunk_offsets: true,
                ..Default::default()
            },
        );
        let file = &parsed[0];
        assert_eq!(file.hunks[0].collapsed_before, 47);
        assert_eq!(file.hunks[0].unified_line_start, 0);
        assert!(file.hunks[1].collapsed_before > 0);
        assert_eq!(
            file.hunks[1].unified_line_start,
            file.hunks[0].unified_line_count
        );
        assert_eq!(
            file.unified_line_count,
            file.hunks
                .iter()
                .map(|h| h.unified_line_count)
                .sum::<usize>()
        );
    }

    #[test]
    fn retains_source_file_offsets_for_checkpoint_diffs() {
        let patch = lines(&[
            "diff --git a/example.ts b/example.ts",
            "--- a/example.ts",
            "+++ b/example.ts",
            "@@ -48,1 +48,1 @@",
            "-before",
            "+after",
        ]);
        let parsed = files(&patch, "checkpoint", Default::default());
        assert_eq!(parsed[0].hunks[0].unified_line_start, 47);
    }

    #[test]
    fn keeps_the_render_key_stable_when_a_partial_diff_hydrates() {
        let patch = "diff --git a/example.ts b/example.ts\n--- a/example.ts\n+++ b/example.ts\n@@ -1 +1 @@\n-before\n+after";
        let mut parsed = files(patch, "hydrated-key", Default::default());
        let file = &mut parsed[0];
        let key = build_file_diff_render_key(file);
        file.cache_key = Some(format!("{}:hydrated", file.cache_key.clone().unwrap()));
        assert_eq!(build_file_diff_render_key(file), key);
    }

    #[test]
    fn gives_a_type_change_its_own_identity_per_block() {
        let patch = lines(&[
            "diff --git a/AGENTS.md b/AGENTS.md",
            "deleted file mode 100644",
            "--- a/AGENTS.md",
            "+++ /dev/null",
            "@@ -1 +0,0 @@",
            "-duplicated instructions",
            "diff --git a/AGENTS.md b/AGENTS.md",
            "new file mode 120000",
            "--- /dev/null",
            "+++ b/AGENTS.md",
            "@@ -0,0 +1 @@",
            "+CLAUDE.md",
        ]);
        let parsed = files(&patch, "type-change", Default::default());
        assert_eq!(parsed[0].r#type, ChangeType::Deleted);
        assert_eq!(parsed[1].r#type, ChangeType::New);
        assert_ne!(
            build_file_diff_identity_key(&parsed[0]),
            build_file_diff_identity_key(&parsed[1])
        );
    }

    #[test]
    fn keeps_identities_stable_and_versions_local_to_the_changed_file() {
        let patch = |second: &str| {
            lines(&[
                "diff --git a/unchanged.ts b/unchanged.ts",
                "--- a/unchanged.ts",
                "+++ b/unchanged.ts",
                "@@ -1 +1 @@",
                "-before",
                "+after",
                "diff --git a/changed.ts b/changed.ts",
                "--- a/changed.ts",
                "+++ b/changed.ts",
                "@@ -1 +1 @@",
                "-old",
                &format!("+{second}"),
            ])
        };
        let before = files(&patch("new"), "before", Default::default());
        let after = files(&patch("newer"), "after", Default::default());
        assert_eq!(
            build_file_diff_identity_key(&after[0]),
            build_file_diff_identity_key(&before[0])
        );
        assert_eq!(
            build_file_diff_identity_key(&after[1]),
            build_file_diff_identity_key(&before[1])
        );
        assert_eq!(
            build_file_diff_content_version(&after[0]),
            build_file_diff_content_version(&before[0])
        );
        assert_ne!(
            build_file_diff_content_version(&after[1]),
            build_file_diff_content_version(&before[1])
        );
    }

    #[test]
    fn totals_additions_and_deletions_across_every_file_and_hunk() {
        let patch = lines(&[
            "diff --git a/example.ts b/example.ts",
            "--- a/example.ts",
            "+++ b/example.ts",
            "@@ -1,2 +1,3 @@",
            "-before",
            "+after",
            "+added",
            " context",
            "@@ -10,2 +11,1 @@",
            "-removed",
            " context",
            "diff --git a/README.md b/README.md",
            "--- a/README.md",
            "+++ b/README.md",
            "@@ -1 +1,2 @@",
            " title",
            "+description",
        ]);
        let parsed = files(&patch, "diff-panel", Default::default());
        assert_eq!(
            get_diff_line_stat(&parsed),
            DiffLineStat {
                additions: 3,
                deletions: 2
            }
        );
    }

    fn quoted_patch(written: &str) -> String {
        lines(&[
            &format!("diff --git \"a/{written}\" \"b/{written}\""),
            "index 1111111..2222222 100644",
            &format!("--- \"a/{written}\""),
            &format!("+++ \"b/{written}\""),
            "@@ -1 +1 @@",
            "-before",
            "+after",
            "",
        ])
    }

    fn path_of(patch: &str) -> String {
        resolve_file_diff_path(&files(patch, "review", Default::default())[0])
    }

    #[test]
    fn a_file_name_a_patch_header_cannot_carry_plainly() {
        assert_eq!(path_of(&quoted_patch("tab\\tfile.txt")), "tab\tfile.txt");
        assert_eq!(path_of(&quoted_patch("line\\nfile.txt")), "line\nfile.txt");
        assert_eq!(
            path_of(&quoted_patch("back\\\\slash.txt")),
            "back\\slash.txt"
        );
        assert_eq!(
            path_of(&quoted_patch("caf\\303\\251/r\\303\\251sum\\303\\251.ts")),
            "café/résumé.ts"
        );
    }

    #[test]
    fn reads_both_sides_of_a_rename_under_the_names_they_really_have() {
        let patch = lines(&[
            "diff --git \"a/old\\tname.ts\" \"b/new\\tname.ts\"",
            "similarity index 90%",
            "rename from \"old\\tname.ts\"",
            "rename to \"new\\tname.ts\"",
            "",
        ]);
        let parsed = files(&patch, "review", Default::default());
        assert_eq!(resolve_file_diff_path(&parsed[0]), "new\tname.ts");
        assert_eq!(resolve_file_diff_previous_path(&parsed[0]), "old\tname.ts");
    }
}
