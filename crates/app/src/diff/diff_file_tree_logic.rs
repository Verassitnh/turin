//! Port of upstream/apps/web/src/components/diffs/diffFileTree.logic.ts (+ test).
//!
//! The `preloadFileTree` reading-order test relied on `@pierre/trees`' row
//! renderer; here the equivalent is checked against [`flatten_visible_rows`].

use std::collections::{HashMap, HashSet};

use super::diff_rendering::resolve_file_diff_path;
use super::pierre::types::{ChangeType, FileDiffMetadata};

/// `GitStatus` of `@pierre/trees` (the four a diff can produce).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitStatus {
    Added,
    Deleted,
    Renamed,
    Modified,
}

/// One changed file as the tree shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFileTreeEntry {
    pub path: String,
    pub status: GitStatus,
}

fn to_git_status(file: &FileDiffMetadata) -> GitStatus {
    match file.r#type {
        ChangeType::New => GitStatus::Added,
        ChangeType::Deleted => GitStatus::Deleted,
        ChangeType::RenamePure | ChangeType::RenameChanged => GitStatus::Renamed,
        ChangeType::Change => GitStatus::Modified,
    }
}

/// `diffFileTreeEntries`: keeps the diff's own order; a path appears once and
/// a type change (deletion + addition of one path) reads as modified.
pub fn diff_file_tree_entries(files: &[FileDiffMetadata]) -> Vec<DiffFileTreeEntry> {
    let mut order: Vec<String> = Vec::new();
    let mut status_by_path: HashMap<String, GitStatus> = HashMap::new();
    for file in files {
        let path = resolve_file_diff_path(file);
        let status = to_git_status(file);
        match status_by_path.get(&path) {
            None => {
                order.push(path.clone());
                status_by_path.insert(path, status);
            }
            Some(previous) if *previous == status => {}
            Some(_) => {
                status_by_path.insert(path, GitStatus::Modified);
            }
        }
    }
    order
        .into_iter()
        .map(|path| {
            let status = status_by_path[&path];
            DiffFileTreeEntry { path, status }
        })
        .collect()
}

/// `collectDirectoryPaths`: every ancestor once, parents first, with the
/// trailing slash Pierre uses for directory ids.
pub fn collect_directory_paths(paths: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut directories = Vec::new();
    for path in paths {
        let segments: Vec<&str> = path.split('/').collect();
        let mut directory = String::new();
        for segment in &segments[..segments.len().saturating_sub(1)] {
            directory.push_str(segment);
            directory.push('/');
            if seen.insert(directory.clone()) {
                directories.push(directory.clone());
            }
        }
    }
    directories
}

/// `diffFileTreePositions`: a folder takes the position of its first file.
pub fn diff_file_tree_positions(paths: &[String]) -> HashMap<String, usize> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        positions.insert(path.clone(), index);
        let segments: Vec<&str> = path.split('/').collect();
        let mut directory = String::new();
        for segment in &segments[..segments.len().saturating_sub(1)] {
            directory.push_str(segment);
            directory.push('/');
            positions.entry(directory.clone()).or_insert(index);
        }
    }
    positions
}

/// A node as the tree sorts it.
pub struct SortNode<'a> {
    pub path: &'a str,
    pub depth: usize,
}

/// `compareDiffFileTreeEntries`.
pub fn compare_diff_file_tree_entries(
    positions: &HashMap<String, usize>,
    left: &SortNode<'_>,
    right: &SortNode<'_>,
) -> std::cmp::Ordering {
    let position = |path: &str| positions.get(path).copied().unwrap_or(usize::MAX);
    position(left.path)
        .cmp(&position(right.path))
        .then(left.depth.cmp(&right.depth))
        .then_with(|| left.path.cmp(right.path))
}

fn path_depth(path: &str) -> usize {
    path.split('/').filter(|s| !s.is_empty()).count()
}

/// `FileTreeBatchOperation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileTreeBatchOperation {
    Add { path: String },
    Remove { path: String, recursive: bool },
}

/// `buildDiffFileTreeUpdates`.
pub fn build_diff_file_tree_updates(
    previous_paths: &[String],
    next_paths: &[String],
) -> Vec<FileTreeBatchOperation> {
    let previous_directories: HashSet<String> = collect_directory_paths(previous_paths)
        .into_iter()
        .collect();
    let next_directories: HashSet<String> =
        collect_directory_paths(next_paths).into_iter().collect();
    let previous: HashSet<&String> = previous_paths.iter().collect();
    let next: HashSet<&String> = next_paths.iter().collect();
    let mut updates = Vec::new();

    for path in previous_paths {
        if !next.contains(path) {
            updates.push(FileTreeBatchOperation::Remove {
                path: path.clone(),
                recursive: false,
            });
        }
    }
    let mut removed: Vec<String> = collect_directory_paths(previous_paths)
        .into_iter()
        .filter(|directory| !next_directories.contains(directory))
        .collect();
    removed.sort_by_key(|directory| std::cmp::Reverse(path_depth(directory)));
    for directory in removed {
        updates.push(FileTreeBatchOperation::Remove {
            path: directory,
            recursive: true,
        });
    }
    let mut added: Vec<String> = collect_directory_paths(next_paths)
        .into_iter()
        .filter(|directory| !previous_directories.contains(directory))
        .collect();
    added.sort_by_key(|directory| path_depth(directory));
    for directory in added {
        updates.push(FileTreeBatchOperation::Add { path: directory });
    }
    for path in next_paths {
        if !previous.contains(path) {
            updates.push(FileTreeBatchOperation::Add { path: path.clone() });
        }
    }
    updates
}

/// A row of the flattened, fully expanded tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// Directory ids carry a trailing slash.
    pub path: String,
    pub depth: usize,
    pub is_directory: bool,
    /// The label: with `flattenEmptyDirectories`, single-child directory
    /// chains read `a/b/c`.
    pub label: String,
}

/// The visible rows of an everything-open tree in diff reading order, with
/// `flattenEmptyDirectories` (a directory whose only child is a directory
/// merges into it).
pub fn flatten_visible_rows(paths: &[String]) -> Vec<TreeRow> {
    #[derive(Default)]
    struct Node {
        children: Vec<(String, Node)>,
    }
    fn insert(node: &mut Node, segments: &[&str]) {
        let Some((first, rest)) = segments.split_first() else {
            return;
        };
        let index = match node.children.iter().position(|(name, _)| name == first) {
            Some(index) => index,
            None => {
                node.children.push(((*first).to_string(), Node::default()));
                node.children.len() - 1
            }
        };
        insert(&mut node.children[index].1, rest);
    }
    let positions = diff_file_tree_positions(paths);
    let mut root = Node::default();
    for path in paths {
        let segments: Vec<&str> = path.split('/').collect();
        insert(&mut root, &segments);
    }
    fn walk(
        node: &Node,
        prefix: &str,
        depth: usize,
        positions: &HashMap<String, usize>,
        rows: &mut Vec<TreeRow>,
    ) {
        let mut children: Vec<(&String, &Node, String, bool)> = node
            .children
            .iter()
            .map(|(name, child)| {
                let is_directory = !child.children.is_empty();
                let id = if is_directory {
                    format!("{prefix}{name}/")
                } else {
                    format!("{prefix}{name}")
                };
                (name, child, id, is_directory)
            })
            .collect();
        children.sort_by(|a, b| {
            compare_diff_file_tree_entries(
                positions,
                &SortNode { path: &a.2, depth },
                &SortNode { path: &b.2, depth },
            )
        });
        for (name, child, id, is_directory) in children {
            if !is_directory {
                rows.push(TreeRow {
                    path: id,
                    depth,
                    is_directory: false,
                    label: name.clone(),
                });
                continue;
            }
            let mut label = name.clone();
            let mut id = id;
            let mut current = child;
            while current.children.len() == 1 && !current.children[0].1.children.is_empty() {
                let (next_name, next) = &current.children[0];
                label = format!("{label}/{next_name}");
                id = format!("{id}{next_name}/");
                current = next;
            }
            rows.push(TreeRow {
                path: id.clone(),
                depth,
                is_directory: true,
                label,
            });
            walk(current, &id, depth + 1, positions, rows);
        }
    }
    let mut rows = Vec::new();
    walk(&root, "", 0, &positions, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(kind: ChangeType, name: &str, prev_name: Option<&str>) -> FileDiffMetadata {
        let mut file = FileDiffMetadata::empty(true);
        file.r#type = kind;
        file.name = name.into();
        file.prev_name = Some(prev_name.unwrap_or(name).into());
        file
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    fn entry(path: &str, status: GitStatus) -> DiffFileTreeEntry {
        DiffFileTreeEntry {
            path: path.into(),
            status,
        }
    }

    #[test]
    fn maps_each_change_type_to_its_git_status_under_the_files_current_path() {
        assert_eq!(
            diff_file_tree_entries(&[
                file(ChangeType::New, "a/src/a.ts", None),
                file(ChangeType::Deleted, "src/b.ts", None),
                file(ChangeType::RenamePure, "src/c.ts", Some("src/old-c.ts")),
                file(ChangeType::RenameChanged, "src/d.ts", Some("src/old-d.ts")),
                file(ChangeType::Change, "README.md", None),
            ]),
            vec![
                entry("a/src/a.ts", GitStatus::Added),
                entry("src/b.ts", GitStatus::Deleted),
                entry("src/c.ts", GitStatus::Renamed),
                entry("src/d.ts", GitStatus::Renamed),
                entry("README.md", GitStatus::Modified),
            ]
        );
    }

    #[test]
    fn folds_a_file_to_symlink_type_change_into_one_modified_entry() {
        assert_eq!(
            diff_file_tree_entries(&[
                file(ChangeType::Change, "CLAUDE.md", None),
                file(ChangeType::Deleted, "AGENTS.md", None),
                file(ChangeType::New, "AGENTS.md", None),
                file(ChangeType::New, "docs/new.md", None),
            ]),
            vec![
                entry("CLAUDE.md", GitStatus::Modified),
                entry("AGENTS.md", GitStatus::Modified),
                entry("docs/new.md", GitStatus::Added),
            ]
        );
    }

    #[test]
    fn lists_every_ancestor_once_parents_first_with_pierres_trailing_slash() {
        assert_eq!(
            collect_directory_paths(&strings(&[
                "apps/web/src/a.ts",
                "apps/web/b.ts",
                "README.md"
            ])),
            strings(&["apps/", "apps/web/", "apps/web/src/"])
        );
    }

    #[test]
    fn places_folders_and_files_where_their_first_diff_appears() {
        let paths = strings(&[
            "apps/mobile/src/state/shell.ts",
            "apps/mobile/src/features/threads/route.ts",
            "apps/mobile/src/features/threads/screen.tsx",
        ]);
        let rows = flatten_visible_rows(&paths);
        let ids: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "apps/mobile/src/",
                "apps/mobile/src/state/",
                "apps/mobile/src/state/shell.ts",
                "apps/mobile/src/features/threads/",
                "apps/mobile/src/features/threads/route.ts",
                "apps/mobile/src/features/threads/screen.tsx",
            ]
        );
        assert_eq!(rows[0].label, "apps/mobile/src");
        assert_eq!(rows[3].label, "features/threads");
    }

    #[test]
    fn adds_a_new_files_directories_before_the_file() {
        assert_eq!(
            build_diff_file_tree_updates(
                &strings(&["README.md"]),
                &strings(&["README.md", "src/lib/a.ts"])
            ),
            vec![
                FileTreeBatchOperation::Add {
                    path: "src/".into()
                },
                FileTreeBatchOperation::Add {
                    path: "src/lib/".into()
                },
                FileTreeBatchOperation::Add {
                    path: "src/lib/a.ts".into()
                },
            ]
        );
    }

    #[test]
    fn removes_files_before_their_now_empty_directories_deepest_first() {
        assert_eq!(
            build_diff_file_tree_updates(
                &strings(&["src/lib/a.ts", "src/b.ts"]),
                &strings(&["src/b.ts"])
            ),
            vec![
                FileTreeBatchOperation::Remove {
                    path: "src/lib/a.ts".into(),
                    recursive: false
                },
                FileTreeBatchOperation::Remove {
                    path: "src/lib/".into(),
                    recursive: true
                },
            ]
        );
    }

    #[test]
    fn keeps_a_directory_that_still_holds_a_file() {
        assert_eq!(
            build_diff_file_tree_updates(
                &strings(&["src/a.ts", "src/b.ts"]),
                &strings(&["src/b.ts"])
            ),
            vec![FileTreeBatchOperation::Remove {
                path: "src/a.ts".into(),
                recursive: false
            }]
        );
    }

    #[test]
    fn produces_nothing_when_the_paths_are_unchanged() {
        assert!(
            build_diff_file_tree_updates(&strings(&["src/a.ts"]), &strings(&["src/a.ts"]))
                .is_empty()
        );
    }
}
