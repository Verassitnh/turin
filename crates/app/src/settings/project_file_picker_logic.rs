//! Port of upstream/apps/web/src/components/files/ProjectFilePicker.logic.ts
//! and upstream/apps/web/src/components/files/ProjectFilePicker.logic.test.ts.
//!
//! Maps server search results to picker rows. Server ordering is preserved
//! (ranking already happened there); this pass only filters to files and
//! computes highlight positions with the same query normalization the server
//! applied. Lives beside its first caller, the project favicon picker.

use std::sync::LazyLock;

use turin_shared::search_ranking::{NormalizeSearchQueryOptions, normalize_search_query};

pub const PROJECT_FILE_PICKER_RESULT_LIMIT: usize = 200;

/// `ProjectEntry["kind"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectEntryKind {
    File,
    Directory,
}

/// `ProjectEntry`: the fields the picker reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    pub kind: ProjectEntryKind,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFilePickerMatch {
    pub name: String,
    pub name_match_indices: Vec<usize>,
    pub path: String,
    pub path_match_indices: Vec<usize>,
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// First ordered subsequence of `query` inside `value`, as highlight indices.
/// Returns `None` when `value` does not contain the subsequence at all: the
/// entry still renders (the server matched it), just without highlights.
/// Indices count UTF-16 units, like the string indices they come from.
fn find_match_indices(value: &str, query: &str) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let query: Vec<char> = query.chars().collect();
    let mut indices = Vec::new();
    let mut query_index = 0;
    let mut unit_index = 0;
    for c in value.to_lowercase().chars() {
        if c == query[query_index] {
            indices.push(unit_index);
            query_index += 1;
            if query_index == query.len() {
                return Some(indices);
            }
        }
        unit_index += c.len_utf16();
    }
    None
}

static LEADING_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[@./]+").expect("leading pattern"));

pub fn get_project_file_picker_matches(
    entries: &[ProjectEntry],
    raw_query: &str,
    limit: usize,
) -> Vec<ProjectFilePickerMatch> {
    if limit == 0 {
        return Vec::new();
    }
    let query: String = normalize_search_query(
        raw_query,
        Some(&NormalizeSearchQueryOptions {
            trim_leading_pattern: Some(&LEADING_PATTERN),
        }),
    )
    .chars()
    .filter(|c| !c.is_whitespace())
    .collect();
    let mut matches = Vec::new();
    for entry in entries {
        if entry.kind != ProjectEntryKind::File {
            continue;
        }
        let name = file_name(&entry.path);
        matches.push(ProjectFilePickerMatch {
            name: name.to_owned(),
            name_match_indices: find_match_indices(name, &query).unwrap_or_default(),
            path: entry.path.clone(),
            path_match_indices: find_match_indices(&entry.path, &query).unwrap_or_default(),
        });
        if matches.len() >= limit {
            break;
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: ProjectEntryKind, path: &str) -> ProjectEntry {
        ProjectEntry {
            kind,
            path: path.to_owned(),
        }
    }

    fn file(path: &str) -> ProjectEntry {
        entry(ProjectEntryKind::File, path)
    }

    fn entries() -> Vec<ProjectEntry> {
        vec![
            entry(ProjectEntryKind::Directory, "apps/web/src"),
            file("apps/web/src/index.ts"),
            file("packages/shared/src/index.ts"),
            file("README.md"),
            file(".gitignore"),
        ]
    }

    fn paths_for_query(entries: &[ProjectEntry], query: &str) -> Vec<(String, String)> {
        get_project_file_picker_matches(entries, query, PROJECT_FILE_PICKER_RESULT_LIMIT)
            .into_iter()
            .map(|m| (m.name, m.path))
            .collect()
    }

    fn pair(name: &str, path: &str) -> (String, String) {
        (name.to_owned(), path.to_owned())
    }

    #[test]
    fn returns_files_only_and_preserves_index_order_for_an_empty_query() {
        assert_eq!(
            paths_for_query(&entries(), ""),
            vec![
                pair("index.ts", "apps/web/src/index.ts"),
                pair("index.ts", "packages/shared/src/index.ts"),
                pair("README.md", "README.md"),
                pair(".gitignore", ".gitignore"),
            ]
        );
    }

    #[test]
    fn preserves_the_server_result_order() {
        assert_eq!(
            paths_for_query(&entries(), "index"),
            vec![
                pair("index.ts", "apps/web/src/index.ts"),
                pair("index.ts", "packages/shared/src/index.ts"),
                pair("README.md", "README.md"),
                pair(".gitignore", ".gitignore"),
            ]
        );
    }

    #[test]
    fn supports_space_separated_path_tokens_and_a_result_limit() {
        let matches = get_project_file_picker_matches(&entries(), "src index", 1);
        assert_eq!(
            matches
                .into_iter()
                .map(|m| (m.name, m.path))
                .collect::<Vec<_>>(),
            vec![pair("index.ts", "apps/web/src/index.ts")]
        );
    }

    #[test]
    fn matches_ordered_characters_while_allowing_skipped_characters() {
        let fuzzy = vec![
            file("src/TestFlags.tsx"),
            file("src/SubtestFlow.tsx"),
            file("src/useSubtestFlags.ts"),
            file("src/useSubtestFlags/useTabActivity.ts"),
        ];
        assert_eq!(
            paths_for_query(&fuzzy, "testf")
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            vec![
                "TestFlags.tsx",
                "SubtestFlow.tsx",
                "useSubtestFlags.ts",
                "useTabActivity.ts"
            ]
        );
        assert_eq!(
            get_project_file_picker_matches(&fuzzy, "tsfl", 200)[0],
            ProjectFilePickerMatch {
                name: "TestFlags.tsx".to_owned(),
                name_match_indices: vec![0, 2, 4, 5],
                path: "src/TestFlags.tsx".to_owned(),
                path_match_indices: vec![4, 6, 8, 9],
            }
        );
    }

    #[test]
    fn uses_the_first_ordered_subsequence_for_highlighting() {
        assert_eq!(
            get_project_file_picker_matches(&[file("aabba")], "aba", 200)[0].name_match_indices,
            vec![0, 2, 4]
        );
    }

    #[test]
    fn normalizes_path_prefixes_consistently_with_server_search() {
        assert_eq!(
            get_project_file_picker_matches(&[file("src/index.ts")], "@/src", 200)[0]
                .path_match_indices,
            vec![0, 1, 2]
        );
    }
}
