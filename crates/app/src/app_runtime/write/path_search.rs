//! `@` path completion: N3's `WorkspaceEntries` over a small file finder.
//!
//! Upstream links the native `fff` finder; Turin has none, so
//! [`ListedFinder`] indexes a workspace once (git's file list when the folder
//! is a repository, a bounded walk otherwise) and ranks by subsequence match.
//! Content search (grep) is not supported and says so.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use turin_git::process::ProcessRunner;
use turin_git::vcs_process::VcsProcess;
use turin_git::workspace::workspace_entries::WorkspaceEntries;
use turin_git::workspace::workspace_paths::WorkspacePaths;
use turin_git::workspace::workspace_search_index::{
    DirItem, DirSearchResult, FileFinderCreateOptions, FileFinderEngine, FileFinderFactory,
    FileItem, FileSearchResult, FinderResult, GrepOptions, GrepResult, MixedItem,
    MixedSearchResult, WorkspaceSearchIndexMap,
};

/// A workspace is indexed up to this many files.
const MAX_FILES: usize = 100_000;
/// Directories never worth completing into.
const SKIPPED: &[&str] = &[".git", "node_modules", "target", ".DS_Store"];

pub fn workspace_entries() -> Arc<WorkspaceEntries> {
    let home = std::env::var("HOME").unwrap_or_default();
    let paths = Arc::new(WorkspacePaths::new(home.clone()));
    let indexes = Arc::new(WorkspaceSearchIndexMap::new(Arc::new(ListedFinderFactory)));
    Arc::new(WorkspaceEntries::new(
        paths,
        indexes,
        Arc::new(VcsProcess::new(ProcessRunner)),
        home,
    ))
}

struct ListedFinderFactory;

impl FileFinderFactory for ListedFinderFactory {
    fn create(
        &self,
        options: &FileFinderCreateOptions,
    ) -> Result<FinderResult<Arc<dyn FileFinderEngine>>, String> {
        Ok(FinderResult::Ok(Arc::new(ListedFinder::new(
            PathBuf::from(&options.base_path),
        ))))
    }
}

/// A listing is re-read once it is this old and a search arrives, so files
/// created by the agent or outside the app turn up while the user types.
const LISTING_STALE_AFTER: Duration = Duration::from_secs(2);

struct Listing {
    files: Vec<String>,
    directories: Vec<String>,
    listed_at: Instant,
}

impl Listing {
    fn scan(root: &Path) -> Self {
        let files = list_files(root);
        let mut directories: Vec<String> = files
            .iter()
            .flat_map(|file| {
                let mut parents = Vec::new();
                let mut current = Path::new(file).parent();
                while let Some(parent) = current.filter(|p| !p.as_os_str().is_empty()) {
                    parents.push(parent.to_string_lossy().into_owned());
                    current = parent.parent();
                }
                parents
            })
            .collect();
        directories.sort();
        directories.dedup();
        Self {
            files,
            directories,
            listed_at: Instant::now(),
        }
    }
}

fn list_files(root: &Path) -> Vec<String> {
    let listed = std::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success());
    let mut files: Vec<String> = match listed {
        Some(output) => output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .filter_map(|path| String::from_utf8(path.to_vec()).ok())
            .collect(),
        None => {
            let mut files = Vec::new();
            walk(root, root, &mut files);
            files
        }
    };
    files.truncate(MAX_FILES);
    files
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if files.len() >= MAX_FILES {
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if SKIPPED.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(root, &path, files),
            Ok(kind) if kind.is_file() => {
                if let Ok(relative) = path.strip_prefix(root) {
                    files.push(relative.to_string_lossy().into_owned());
                }
            }
            _ => {}
        }
    }
}

struct ListedFinder {
    root: PathBuf,
    listing: Mutex<Arc<Listing>>,
    stale_after: Duration,
}

impl ListedFinder {
    fn new(root: PathBuf) -> Self {
        Self::with_stale_after(root, LISTING_STALE_AFTER)
    }

    fn with_stale_after(root: PathBuf, stale_after: Duration) -> Self {
        let listing = Mutex::new(Arc::new(Listing::scan(&root)));
        Self {
            root,
            listing,
            stale_after,
        }
    }

    /// The listing to search, re-read first when it has gone stale.
    fn current(&self) -> Arc<Listing> {
        let mut listing = self.listing.lock().expect("listing lock");
        if listing.listed_at.elapsed() >= self.stale_after {
            *listing = Arc::new(Listing::scan(&self.root));
        }
        listing.clone()
    }
}

/// Lower is better; `None` when the query is not a subsequence of the path.
fn rank(path: &str, query: &str) -> Option<(u8, usize)> {
    if query.is_empty() {
        return Some((2, path.len()));
    }
    let path_lower = path.to_lowercase();
    let query = query.to_lowercase();
    let name = path_lower.rsplit('/').next().unwrap_or(&path_lower);
    if name.starts_with(&query) {
        return Some((0, path.len()));
    }
    if path_lower.contains(&query) {
        return Some((1, path.len()));
    }
    let mut remaining = query.chars();
    let mut wanted = remaining.next();
    for character in path_lower.chars() {
        if Some(character) == wanted {
            wanted = remaining.next();
        }
    }
    wanted.is_none().then_some((3, path.len()))
}

fn ranked<'a>(items: &'a [String], query: &str, limit: usize) -> (Vec<&'a String>, u64) {
    let mut matched: Vec<_> = items
        .iter()
        .filter_map(|item| rank(item, query).map(|rank| (rank, item)))
        .collect();
    let total = matched.len() as u64;
    matched.sort();
    (
        matched
            .into_iter()
            .take(limit)
            .map(|(_, item)| item)
            .collect(),
        total,
    )
}

#[async_trait]
impl FileFinderEngine for ListedFinder {
    async fn wait_for_index_ready(&self, _timeout_ms: u64) -> Result<FinderResult<bool>, String> {
        Ok(FinderResult::Ok(true))
    }

    fn destroy(&self) -> Result<(), String> {
        Ok(())
    }

    fn is_scanning(&self) -> bool {
        false
    }

    fn scan_files(&self) -> Result<FinderResult<()>, String> {
        *self.listing.lock().expect("listing lock") = Arc::new(Listing::scan(&self.root));
        Ok(FinderResult::Ok(()))
    }

    fn mixed_search(
        &self,
        query: &str,
        page_size: u64,
    ) -> Result<FinderResult<MixedSearchResult>, String> {
        let limit = page_size as usize;
        let listing = self.current();
        let (files, file_total) = ranked(&listing.files, query, limit);
        let (directories, directory_total) = ranked(&listing.directories, query, limit);
        let mut items: Vec<(u8, usize, MixedItem)> = files
            .into_iter()
            .map(|path| {
                let r = rank(path, query).unwrap_or((3, path.len()));
                (
                    r.0,
                    r.1,
                    MixedItem::File(FileItem {
                        relative_path: path.clone(),
                    }),
                )
            })
            .chain(directories.into_iter().map(|path| {
                let r = rank(path, query).unwrap_or((3, path.len()));
                (
                    r.0,
                    r.1,
                    MixedItem::Directory(DirItem {
                        relative_path: path.clone(),
                    }),
                )
            }))
            .collect();
        items.sort_by_key(|(tier, length, _)| (*tier, *length));
        items.truncate(limit);
        Ok(FinderResult::Ok(MixedSearchResult {
            items: items.into_iter().map(|(_, _, item)| item).collect(),
            total_matched: file_total + directory_total,
        }))
    }

    fn file_search(
        &self,
        query: &str,
        page_size: u64,
    ) -> Result<FinderResult<FileSearchResult>, String> {
        let listing = self.current();
        let (files, total_matched) = ranked(&listing.files, query, page_size as usize);
        Ok(FinderResult::Ok(FileSearchResult {
            items: files
                .into_iter()
                .map(|path| FileItem {
                    relative_path: path.clone(),
                })
                .collect(),
            total_matched,
        }))
    }

    fn directory_search(
        &self,
        query: &str,
        page_size: u64,
    ) -> Result<FinderResult<DirSearchResult>, String> {
        let listing = self.current();
        let (directories, total_matched) = ranked(&listing.directories, query, page_size as usize);
        Ok(FinderResult::Ok(DirSearchResult {
            items: directories
                .into_iter()
                .map(|path| DirItem {
                    relative_path: path.clone(),
                })
                .collect(),
            total_matched,
        }))
    }

    fn grep(
        &self,
        _query: &str,
        _options: &GrepOptions,
    ) -> Result<FinderResult<GrepResult>, String> {
        Ok(FinderResult::Err(
            "Content search is not available in this build.".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_name_prefix_before_path_substring_before_subsequence() {
        assert!(rank("src/main.rs", "main") < rank("src/domain/x.rs", "main"));
        assert!(rank("src/domain/x.rs", "main") < rank("src/m/a/i/n.rs", "main"));
        assert!(rank("README.md", "xyz").is_none());
    }

    fn searched_paths(finder: &ListedFinder, query: &str) -> Vec<String> {
        let Ok(FinderResult::Ok(result)) = finder.mixed_search(query, 50) else {
            panic!("search failed");
        };
        result
            .items
            .into_iter()
            .map(|item| match item {
                MixedItem::File(file) => file.relative_path,
                MixedItem::Directory(dir) => dir.relative_path,
            })
            .collect()
    }

    #[test]
    fn files_created_after_the_first_search_are_found() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("old.rs"), "").expect("file");
        // Every search sees a stale listing, as one after the 2s window.
        let finder = ListedFinder::with_stale_after(dir.path().to_path_buf(), Duration::ZERO);
        assert_eq!(searched_paths(&finder, "new"), Vec::<String>::new());
        std::fs::write(dir.path().join("newfile.rs"), "").expect("file");
        assert_eq!(searched_paths(&finder, "newfile"), vec!["newfile.rs"]);
    }

    #[test]
    fn a_fresh_listing_is_not_re_read_until_asked_to_scan() {
        let dir = tempfile::tempdir().expect("dir");
        let finder =
            ListedFinder::with_stale_after(dir.path().to_path_buf(), Duration::from_secs(3600));
        std::fs::write(dir.path().join("late.rs"), "").expect("file");
        assert!(searched_paths(&finder, "late").is_empty());
        finder.scan_files().expect("scan");
        assert_eq!(searched_paths(&finder, "late"), vec!["late.rs"]);
    }

    #[test]
    fn indexes_a_plain_folder_without_hidden_or_build_dirs() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::create_dir_all(dir.path().join("src")).expect("src");
        std::fs::create_dir_all(dir.path().join("target")).expect("target");
        std::fs::write(dir.path().join("src/lib.rs"), "").expect("file");
        std::fs::write(dir.path().join("target/out.o"), "").expect("file");
        std::fs::write(dir.path().join(".env"), "").expect("file");
        let files = list_files(dir.path());
        assert_eq!(files, vec!["src/lib.rs".to_owned()]);
    }
}
