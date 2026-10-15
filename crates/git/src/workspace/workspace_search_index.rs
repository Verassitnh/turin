//! Port of upstream/apps/server/src/workspace/WorkspaceSearchIndex.ts
//!
//! Seam: upstream dlopens `@ff-labs/fff-node` (`FileFinder`). The mapping,
//! whole-word filter, ancestor fill, page caps, and scan-timeout rules below
//! are that algorithm. [`FileFinderEngine`] is the native index; this crate
//! does not implement it. Idle eviction runs on the next lookup instead of an
//! Effect LayerMap timer.

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, LazyLock,
};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use thiserror::Error;
use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString};
use turin_contracts::project::{
    ProjectContentMatch, ProjectContentMatchRange, ProjectEntry, ProjectEntryKind,
    ProjectListEntriesResult, ProjectSearchContentsResult, ProjectSearchEntriesResult,
};

pub const WORKSPACE_INDEX_MAX_ENTRIES: u64 = 25_000;
pub const WORKSPACE_INDEX_PAGE_SIZE: u64 = WORKSPACE_INDEX_MAX_ENTRIES + 2;
const WORKSPACE_INDEX_SCAN_TIMEOUT: &str = "15 seconds";
const WORKSPACE_INDEX_SCAN_TIMEOUT_MS: u64 = 15_000;
const WORKSPACE_INDEX_IDLE_TTL: Duration = Duration::from_secs(15 * 60);
const CONTENT_SEARCH_TIME_BUDGET: Duration = Duration::from_millis(250);
const CONTENT_SEARCH_MAX_MATCHES_PER_FILE: u64 = 100;

/// Same extensions as upstream `PROJECT_FAVICON_EXTENSIONS`.
const PROJECT_FAVICON_EXTENSIONS: &[&str] = &[
    ".avif", ".gif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".webp", ".icns",
];

/// Unicode word characters, matching `/[\p{Letter}\p{Mark}\p{Number}_]/u`.
static WORD_CHARACTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{Letter}\p{Mark}\p{Number}_]").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSearchIndexVariant {
    Paths,
    Content,
}

impl WorkspaceSearchIndexVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paths => "paths",
            Self::Content => "content",
        }
    }
}

pub const WORKSPACE_SEARCH_INDEX_VARIANTS: [WorkspaceSearchIndexVariant; 2] = [
    WorkspaceSearchIndexVariant::Paths,
    WorkspaceSearchIndexVariant::Content,
];

/// `"\n"` cannot appear in a filesystem path, so it separates variant and cwd.
pub fn workspace_search_index_key(cwd: &str, variant: WorkspaceSearchIndexVariant) -> String {
    format!("{}\n{cwd}", variant.as_str())
}

fn parse_workspace_search_index_key(key: &str) -> (WorkspaceSearchIndexVariant, String) {
    match key.split_once('\n') {
        Some(("content", cwd)) => (WorkspaceSearchIndexVariant::Content, cwd.to_owned()),
        Some((_, cwd)) => (WorkspaceSearchIndexVariant::Paths, cwd.to_owned()),
        None => (WorkspaceSearchIndexVariant::Paths, key.to_owned()),
    }
}

#[derive(Debug, Clone, Error)]
#[error("Failed to create the workspace search index for '{cwd}'.")]
pub struct WorkspaceSearchIndexCreateFailed {
    pub cwd: String,
    pub reason: String,
    pub cause: Option<String>,
}

#[derive(Debug, Clone, Error)]
#[error("Workspace search index for '{cwd}' did not finish scanning within {timeout}")]
pub struct WorkspaceSearchIndexScanTimedOut {
    pub cwd: String,
    pub timeout: String,
}

#[derive(Debug, Clone, Error)]
#[error("Workspace search failed for '{cwd}'.")]
pub struct WorkspaceSearchIndexSearchFailed {
    pub cwd: String,
    pub query_length: u64,
    pub page_size: u64,
    pub reason: String,
    pub cause: Option<String>,
}

#[derive(Debug, Clone, Error)]
#[error("Failed to refresh the workspace search index for '{cwd}'.")]
pub struct WorkspaceSearchIndexRefreshFailed {
    pub cwd: String,
    pub reason: String,
    pub cause: Option<String>,
}

#[derive(Debug, Clone, Error)]
#[error("Failed to destroy the workspace search index for '{cwd}'.")]
pub struct WorkspaceSearchIndexDestroyFailed {
    pub cwd: String,
    pub cause: String,
}

#[derive(Debug, Error)]
pub enum WorkspaceSearchIndexOpenError {
    #[error(transparent)]
    Create(#[from] WorkspaceSearchIndexCreateFailed),
    #[error(transparent)]
    ScanTimedOut(#[from] WorkspaceSearchIndexScanTimedOut),
}

#[derive(Debug, Error)]
pub enum WorkspaceSearchIndexRefreshError {
    #[error(transparent)]
    Refresh(#[from] WorkspaceSearchIndexRefreshFailed),
    #[error(transparent)]
    ScanTimedOut(#[from] WorkspaceSearchIndexScanTimedOut),
}

/// `fff` `Result<T>`: either a value or an engine diagnostic. A thrown error
/// is the outer `Err`, and stays out of `cause` when the engine returns `ok:
/// false` instead of throwing.
#[derive(Debug, Clone)]
pub enum FinderResult<T> {
    Ok(T),
    Err(String),
}

#[derive(Debug, Clone)]
pub struct FileFinderCreateOptions {
    pub base_path: String,
    pub disable_mmap_cache: bool,
    pub disable_content_indexing: bool,
    pub ai_mode: bool,
    pub enable_fs_root_scanning: bool,
    pub enable_home_dir_scanning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileItem {
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirItem {
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MixedItem {
    File(FileItem),
    Directory(DirItem),
}

#[derive(Debug, Clone)]
pub struct FileSearchResult {
    pub items: Vec<FileItem>,
    pub total_matched: u64,
}

#[derive(Debug, Clone)]
pub struct DirSearchResult {
    pub items: Vec<DirItem>,
    pub total_matched: u64,
}

#[derive(Debug, Clone)]
pub struct MixedSearchResult {
    pub items: Vec<MixedItem>,
    pub total_matched: u64,
}

/// Opaque grep page token. Upstream brands an object; equality is the token
/// the engine handed back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepCursor(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepMode {
    Regex,
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepOptions {
    pub mode: GrepMode,
    pub smart_case: bool,
    pub max_matches_per_file: u64,
    pub page_size: u64,
    pub cursor: Option<GrepCursor>,
    pub time_budget_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepItem {
    pub relative_path: String,
    pub line_number: u64,
    pub line_content: String,
    /// Byte offsets into `line_content`, the way `fff` reports them.
    pub match_ranges: Vec<(u64, u64)>,
}

#[derive(Debug, Clone)]
pub struct GrepResult {
    pub items: Vec<GrepItem>,
    pub next_cursor: Option<GrepCursor>,
    pub regex_fallback_error: Option<String>,
}

pub trait FileFinderFactory: Send + Sync {
    fn create(
        &self,
        options: &FileFinderCreateOptions,
    ) -> Result<FinderResult<Arc<dyn FileFinderEngine>>, String>;
}

#[async_trait]
pub trait FileFinderEngine: Send + Sync {
    async fn wait_for_index_ready(&self, timeout_ms: u64) -> Result<FinderResult<bool>, String>;
    fn destroy(&self) -> Result<(), String>;
    fn is_scanning(&self) -> bool;
    fn scan_files(&self) -> Result<FinderResult<()>, String>;
    fn mixed_search(
        &self,
        query: &str,
        page_size: u64,
    ) -> Result<FinderResult<MixedSearchResult>, String>;
    fn file_search(
        &self,
        query: &str,
        page_size: u64,
    ) -> Result<FinderResult<FileSearchResult>, String>;
    fn directory_search(
        &self,
        query: &str,
        page_size: u64,
    ) -> Result<FinderResult<DirSearchResult>, String>;
    fn grep(&self, query: &str, options: &GrepOptions) -> Result<FinderResult<GrepResult>, String>;
}

/// Content search input: `ProjectSearchContentsInput` without `cwd`, because
/// the index is already bound to one workspace.
#[derive(Debug, Clone)]
pub struct ContentSearchInput {
    pub query: String,
    pub limit: u64,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

pub struct WorkspaceSearchIndex {
    cwd: String,
    finder: Arc<dyn FileFinderEngine>,
    initial_scan_timed_out: bool,
    destroyed: AtomicBool,
}

impl WorkspaceSearchIndex {
    pub async fn open(
        cwd: &str,
        variant: WorkspaceSearchIndexVariant,
        factory: &dyn FileFinderFactory,
    ) -> Result<Self, WorkspaceSearchIndexOpenError> {
        let options = FileFinderCreateOptions {
            base_path: cwd.to_owned(),
            disable_mmap_cache: true,
            // Content indexing costs scan CPU and memory, so only the
            // on-demand content-search index pays for it.
            disable_content_indexing: variant != WorkspaceSearchIndexVariant::Content,
            ai_mode: false,
            enable_fs_root_scanning: true,
            enable_home_dir_scanning: true,
        };
        let finder = match factory.create(&options) {
            Err(cause) => {
                return Err(WorkspaceSearchIndexCreateFailed {
                    cwd: cwd.to_owned(),
                    reason: "FileFinder.create threw unexpectedly.".to_owned(),
                    cause: Some(cause),
                }
                .into());
            }
            Ok(FinderResult::Err(reason)) => {
                return Err(WorkspaceSearchIndexCreateFailed {
                    cwd: cwd.to_owned(),
                    reason,
                    cause: None,
                }
                .into());
            }
            Ok(FinderResult::Ok(finder)) => finder,
        };

        let waited = wait_for_index_ready(cwd, finder.as_ref(), |reason, cause| {
            WorkspaceSearchIndexCreateFailed {
                cwd: cwd.to_owned(),
                reason,
                cause,
            }
        })
        .await;
        let mut initial_scan_timed_out = false;
        match waited {
            Ok(()) => {}
            Err(WaitError::TimedOut(_error)) if variant == WorkspaceSearchIndexVariant::Paths => {
                initial_scan_timed_out = true;
            }
            Err(error) => {
                if let Err(cause) = finder.destroy() {
                    tracing::error!(
                        cwd,
                        %cause,
                        "Failed to destroy the workspace search index"
                    );
                }
                return Err(match error {
                    WaitError::Failure(error) => error.into(),
                    WaitError::TimedOut(error) => error.into(),
                });
            }
        }

        Ok(Self {
            cwd: cwd.to_owned(),
            finder,
            initial_scan_timed_out,
            destroyed: AtomicBool::new(false),
        })
    }

    pub fn close(&self) -> Result<(), WorkspaceSearchIndexDestroyFailed> {
        match self.destroy_once() {
            None | Some(Ok(())) => Ok(()),
            Some(Err(cause)) => Err(WorkspaceSearchIndexDestroyFailed {
                cwd: self.cwd.clone(),
                cause,
            }),
        }
    }

    fn destroy_once(&self) -> Option<Result<(), String>> {
        if self.destroyed.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(self.finder.destroy())
    }

    fn has_incomplete_initial_scan(&self) -> bool {
        self.initial_scan_timed_out && self.finder.is_scanning()
    }

    pub async fn list(&self) -> Result<ProjectListEntriesResult, WorkspaceSearchIndexSearchFailed> {
        let incomplete_before_query = self.has_incomplete_initial_scan();
        let result = self.run_search("", WORKSPACE_INDEX_PAGE_SIZE, "mixedSearch", || {
            self.finder
                .mixed_search("", WORKSPACE_INDEX_PAGE_SIZE)
        })?;
        let mapped = map_mixed_search_result(&result, WORKSPACE_INDEX_MAX_ENTRIES);
        let mut sorted = with_directory_ancestors(&mapped.entries);
        sorted.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
        let entries: Vec<ProjectEntry> = sorted
            .iter()
            .take(WORKSPACE_INDEX_MAX_ENTRIES as usize)
            .cloned()
            .collect();
        Ok(ProjectListEntriesResult {
            truncated: incomplete_before_query
                || mapped.truncated
                || entries.len() < sorted.len(),
            entries,
        })
    }

    pub async fn search(
        &self,
        query: &str,
        limit: u64,
        kind: Option<ProjectEntryKind>,
        image_only: bool,
    ) -> Result<ProjectSearchEntriesResult, WorkspaceSearchIndexSearchFailed> {
        let page_size = if image_only {
            WORKSPACE_INDEX_PAGE_SIZE
        } else {
            1.max(limit.saturating_add(1))
        };
        let incomplete_before_query = self.has_incomplete_initial_scan();
        let mapped = if kind == Some(ProjectEntryKind::File) || image_only {
            let result = self.run_search(query, page_size, "fileSearch", || {
                self.finder.file_search(query, page_size)
            })?;
            map_file_search_result(&result, limit, image_only)
        } else if kind == Some(ProjectEntryKind::Directory) {
            let result = self.run_search(query, page_size, "directorySearch", || {
                self.finder.directory_search(query, page_size)
            })?;
            map_directory_search_result(&result, limit)
        } else {
            let result = self.run_search(query, page_size, "mixedSearch", || {
                self.finder.mixed_search(query, page_size)
            })?;
            map_mixed_search_result(&result, limit)
        };
        Ok(ProjectSearchEntriesResult {
            entries: mapped.entries,
            truncated: incomplete_before_query || mapped.truncated,
        })
    }

    pub async fn search_contents(
        &self,
        input: &ContentSearchInput,
    ) -> Result<ProjectSearchContentsResult, WorkspaceSearchIndexSearchFailed> {
        let built = build_content_search_query(input);
        let deadline = Instant::now() + CONTENT_SEARCH_TIME_BUDGET;
        // Grep cursors advance by file, so whole-word post-filtering needs
        // enough raw candidates from the current file before the next one.
        let raw_page_size = if input.whole_word {
            input.limit.max(CONTENT_SEARCH_MAX_MATCHES_PER_FILE)
        } else {
            input.limit
        };
        let mut matches = Vec::new();
        let mut next_cursor = None;
        let mut regex_fallback_error = None;
        loop {
            let remaining_time_budget_ms = remaining_time_budget_ms(deadline);
            let options = GrepOptions {
                mode: if built.regex_mode {
                    GrepMode::Regex
                } else {
                    GrepMode::Plain
                },
                smart_case: !input.case_sensitive && !built.regex_mode,
                max_matches_per_file: CONTENT_SEARCH_MAX_MATCHES_PER_FILE.min(raw_page_size),
                page_size: raw_page_size,
                cursor: next_cursor.clone(),
                time_budget_ms: remaining_time_budget_ms,
            };
            let result = self.run_search(&input.query, input.limit, "grep", || {
                self.finder.grep(&built.search_query, &options)
            })?;
            for item in result.items {
                let match_ranges = map_content_match_ranges(&item.line_content, &item.match_ranges)
                    .into_iter()
                    .filter(|range| !input.whole_word || is_whole_word_range(&item.line_content, range))
                    .collect::<Vec<_>>();
                if match_ranges.is_empty() {
                    continue;
                }
                matches.push(ProjectContentMatch {
                    path: TrimmedNonEmptyString::new(to_posix_path(&item.relative_path))
                        .expect("grep relative path is non-empty"),
                    line_number: PositiveInt::new(item.line_number)
                        .expect("grep line numbers start at 1"),
                    line_content: item.line_content,
                    match_ranges: match_ranges
                        .into_iter()
                        .map(|range| ProjectContentMatchRange {
                            start: range.start,
                            end: range.end,
                        })
                        .collect(),
                });
            }
            next_cursor = result.next_cursor;
            if regex_fallback_error.is_none() {
                regex_fallback_error = result.regex_fallback_error;
            }
            if matches.len() as u64 >= input.limit
                || next_cursor.is_none()
                || Instant::now() >= deadline
            {
                break;
            }
        }
        let truncated = matches.len() as u64 > input.limit || next_cursor.is_some();
        matches.truncate(input.limit as usize);
        Ok(ProjectSearchContentsResult {
            matches,
            truncated,
            regex_fallback_error,
        })
    }

    pub async fn refresh(&self) -> Result<(), WorkspaceSearchIndexRefreshError> {
        match self.finder.scan_files() {
            Err(cause) => {
                return Err(WorkspaceSearchIndexRefreshFailed {
                    cwd: self.cwd.clone(),
                    reason: "FileFinder.scanFiles threw unexpectedly.".to_owned(),
                    cause: Some(cause),
                }
                .into());
            }
            Ok(FinderResult::Err(reason)) => {
                return Err(WorkspaceSearchIndexRefreshFailed {
                    cwd: self.cwd.clone(),
                    reason,
                    cause: None,
                }
                .into());
            }
            Ok(FinderResult::Ok(())) => {}
        }
        wait_for_index_ready(&self.cwd, self.finder.as_ref(), |reason, cause| {
            WorkspaceSearchIndexRefreshFailed {
                cwd: self.cwd.clone(),
                reason,
                cause,
            }
        })
        .await
        .map_err(|error| match error {
            WaitError::Failure(error) => error.into(),
            WaitError::TimedOut(error) => error.into(),
        })
    }

    fn run_search<T>(
        &self,
        query: &str,
        page_size: u64,
        operation: &str,
        execute: impl FnOnce() -> Result<FinderResult<T>, String>,
    ) -> Result<T, WorkspaceSearchIndexSearchFailed> {
        let failed = |reason: String, cause: Option<String>| WorkspaceSearchIndexSearchFailed {
            cwd: self.cwd.clone(),
            query_length: js_string_length(query),
            page_size,
            reason,
            cause,
        };
        match execute() {
            Err(cause) => Err(failed(
                format!("FileFinder.{operation} threw unexpectedly."),
                Some(cause),
            )),
            Ok(FinderResult::Err(reason)) => Err(failed(reason, None)),
            Ok(FinderResult::Ok(value)) => Ok(value),
        }
    }
}

impl Drop for WorkspaceSearchIndex {
    fn drop(&mut self) {
        // Effect `acquireRelease` fails the scope if `destroy` throws. `Drop`
        // cannot return that error; [`Self::close`] does, and a forgotten
        // index logs it here.
        if let Some(Err(cause)) = self.destroy_once() {
            tracing::error!(
                cwd = %self.cwd,
                %cause,
                "Failed to destroy the workspace search index"
            );
        }
    }
}

enum WaitError<E> {
    Failure(E),
    TimedOut(WorkspaceSearchIndexScanTimedOut),
}

async fn wait_for_index_ready<E>(
    cwd: &str,
    finder: &dyn FileFinderEngine,
    on_failure: impl Fn(String, Option<String>) -> E,
) -> Result<(), WaitError<E>> {
    let result = match finder
        .wait_for_index_ready(WORKSPACE_INDEX_SCAN_TIMEOUT_MS)
        .await
    {
        Err(cause) => {
            return Err(WaitError::Failure(on_failure(
                "FileFinder.waitForIndexReady rejected unexpectedly.".to_owned(),
                Some(cause),
            )));
        }
        Ok(result) => result,
    };
    match result {
        FinderResult::Err(reason) => Err(WaitError::Failure(on_failure(reason, None))),
        FinderResult::Ok(true) => Ok(()),
        FinderResult::Ok(false) => Err(WaitError::TimedOut(WorkspaceSearchIndexScanTimedOut {
            cwd: cwd.to_owned(),
            timeout: WORKSPACE_INDEX_SCAN_TIMEOUT.to_owned(),
        })),
    }
}

struct IndexSlot {
    index: Arc<WorkspaceSearchIndex>,
    last_used: Instant,
}

/// Memoizes one path index and one content index per workspace root.
pub struct WorkspaceSearchIndexMap {
    slots: tokio::sync::Mutex<HashMap<String, IndexSlot>>,
    factory: Arc<dyn FileFinderFactory>,
}

impl WorkspaceSearchIndexMap {
    pub fn new(factory: Arc<dyn FileFinderFactory>) -> Self {
        Self {
            slots: tokio::sync::Mutex::new(HashMap::new()),
            factory,
        }
    }

    pub async fn contains(&self, key: &str) -> bool {
        let mut slots = self.slots.lock().await;
        evict_idle(&mut slots);
        slots.contains_key(key)
    }

    pub async fn get(
        &self,
        key: &str,
    ) -> Result<Arc<WorkspaceSearchIndex>, WorkspaceSearchIndexOpenError> {
        if let Some(existing) = self.fresh(key).await {
            return Ok(existing);
        }
        let (variant, cwd) = parse_workspace_search_index_key(key);
        let created = Arc::new(WorkspaceSearchIndex::open(&cwd, variant, self.factory.as_ref()).await?);
        let mut slots = self.slots.lock().await;
        evict_idle(&mut slots);
        if let Some(existing) = slots.get_mut(key) {
            existing.last_used = Instant::now();
            return Ok(Arc::clone(&existing.index));
        }
        slots.insert(
            key.to_owned(),
            IndexSlot {
                index: Arc::clone(&created),
                last_used: Instant::now(),
            },
        );
        Ok(created)
    }

    pub async fn invalidate(&self, key: &str) {
        self.slots.lock().await.remove(key);
    }

    async fn fresh(&self, key: &str) -> Option<Arc<WorkspaceSearchIndex>> {
        let mut slots = self.slots.lock().await;
        evict_idle(&mut slots);
        let slot = slots.get_mut(key)?;
        slot.last_used = Instant::now();
        Some(Arc::clone(&slot.index))
    }
}

fn evict_idle(slots: &mut HashMap<String, IndexSlot>) {
    let now = Instant::now();
    slots.retain(|_, slot| now.saturating_duration_since(slot.last_used) < WORKSPACE_INDEX_IDLE_TTL);
}

fn to_posix_path(input: &str) -> String {
    input.replace('\\', "/")
}

fn trim_directory_separator(input: &str) -> &str {
    input.strip_suffix('/').unwrap_or(input)
}

fn parent_path_of(input: &str) -> Option<&str> {
    let separator = input.rfind('/')?;
    Some(&input[..separator])
}

fn project_entry(path: &str, kind: ProjectEntryKind) -> Option<ProjectEntry> {
    let posix = to_posix_path(path);
    let normalized = trim_directory_separator(&posix);
    if normalized.is_empty() {
        return None;
    }
    Some(ProjectEntry {
        path: TrimmedNonEmptyString::new(normalized).ok()?,
        kind,
        ignored: None,
    })
}

fn to_project_entry(item: &MixedItem) -> Option<ProjectEntry> {
    match item {
        MixedItem::File(file) => project_entry(&file.relative_path, ProjectEntryKind::File),
        MixedItem::Directory(directory) => {
            project_entry(&directory.relative_path, ProjectEntryKind::Directory)
        }
    }
}

fn to_file_entry(item: &FileItem) -> Option<ProjectEntry> {
    project_entry(&item.relative_path, ProjectEntryKind::File)
}

fn to_directory_entry(item: &DirItem) -> Option<ProjectEntry> {
    project_entry(&item.relative_path, ProjectEntryKind::Directory)
}

fn is_project_favicon_path(path: &str) -> bool {
    let literal = path.to_lowercase();
    PROJECT_FAVICON_EXTENSIONS
        .iter()
        .any(|extension| literal.ends_with(extension))
}

struct MappedEntries {
    entries: Vec<ProjectEntry>,
    truncated: bool,
}

fn map_file_search_result(
    result: &FileSearchResult,
    limit: u64,
    image_only: bool,
) -> MappedEntries {
    let entries: Vec<ProjectEntry> = result
        .items
        .iter()
        .filter_map(|item| {
            let entry = to_file_entry(item)?;
            if image_only && !is_project_favicon_path(entry.path.as_str()) {
                return None;
            }
            Some(entry)
        })
        .collect();
    let truncated =
        entries.len() as u64 > limit || result.total_matched > result.items.len() as u64;
    MappedEntries {
        entries: entries.into_iter().take(limit as usize).collect(),
        truncated,
    }
}

fn map_directory_search_result(result: &DirSearchResult, limit: u64) -> MappedEntries {
    let entries: Vec<ProjectEntry> = result.items.iter().filter_map(to_directory_entry).collect();
    let root_directory_count = u64::from(
        result
            .items
            .iter()
            .any(|item| item.relative_path.is_empty()),
    );
    MappedEntries {
        truncated: result.total_matched.saturating_sub(root_directory_count) > limit,
        entries: entries.into_iter().take(limit as usize).collect(),
    }
}

fn map_mixed_search_result(result: &MixedSearchResult, limit: u64) -> MappedEntries {
    let mut entries = Vec::new();
    for item in &result.items {
        if let Some(entry) = to_project_entry(item) {
            entries.push(entry);
        }
        if entries.len() as u64 >= limit {
            break;
        }
    }
    let root_directory_count = u64::from(result.items.iter().any(|item| {
        matches!(item, MixedItem::Directory(directory) if directory.relative_path.is_empty())
    }));
    MappedEntries {
        entries,
        truncated: result.total_matched.saturating_sub(root_directory_count) > limit,
    }
}

fn with_directory_ancestors(entries: &[ProjectEntry]) -> Vec<ProjectEntry> {
    let mut by_path = HashMap::<String, ProjectEntry>::new();
    for entry in entries {
        by_path.insert(entry.path.as_str().to_owned(), entry.clone());
    }
    for entry in entries {
        let mut parent = parent_path_of(entry.path.as_str());
        while let Some(parent_path) = parent {
            if !by_path.contains_key(parent_path) {
                if let Ok(path) = TrimmedNonEmptyString::new(parent_path) {
                    by_path.insert(
                        parent_path.to_owned(),
                        ProjectEntry {
                            path,
                            kind: ProjectEntryKind::Directory,
                            ignored: None,
                        },
                    );
                }
            }
            parent = parent_path_of(parent_path);
        }
    }
    by_path.into_values().collect()
}

struct BuiltContentQuery {
    search_query: String,
    regex_mode: bool,
}

fn build_content_search_query(input: &ContentSearchInput) -> BuiltContentQuery {
    if input.case_sensitive {
        return BuiltContentQuery {
            search_query: input.query.clone(),
            regex_mode: input.use_regex,
        };
    }
    // Plain mode relies on smart case: an all-lowercase needle matches
    // case-insensitively. Regex mode needs an explicit inline flag instead.
    // Rust lowercase matches JS `toLowerCase` for ASCII; a few Unicode case
    // folds (U+0130, ß) differ.
    if input.use_regex {
        BuiltContentQuery {
            search_query: format!("(?i){}", input.query),
            regex_mode: true,
        }
    } else {
        BuiltContentQuery {
            search_query: input.query.to_lowercase(),
            regex_mode: false,
        }
    }
}

struct JsRange {
    start: u64,
    end: u64,
}

fn map_content_match_ranges(line: &str, byte_ranges: &[(u64, u64)]) -> Vec<JsRange> {
    byte_ranges
        .iter()
        .map(|(start_byte, end_byte)| JsRange {
            start: byte_offset_to_utf16_index(line, *start_byte),
            end: byte_offset_to_utf16_index(line, *end_byte),
        })
        .collect()
}

/// JS `Buffer.from(line).subarray(0, byteOffset).toString().length`: a UTF-16
/// index, with an incomplete UTF-8 sequence replaced by U+FFFD.
fn byte_offset_to_utf16_index(line: &str, byte_offset: u64) -> u64 {
    let bytes = line.as_bytes();
    let end = (byte_offset as usize).min(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
        .encode_utf16()
        .count() as u64
}

fn code_point_at(units: &[u16], index: usize) -> Option<char> {
    if index >= units.len() {
        return None;
    }
    char::decode_utf16(units[index..].iter().copied())
        .next()
        .and_then(Result::ok)
}

fn code_point_before(units: &[u16], index: usize) -> Option<char> {
    if index == 0 {
        return None;
    }
    let previous = units[index - 1];
    let previous_index = if (0xDC00..=0xDFFF).contains(&previous) {
        index.checked_sub(2)?
    } else {
        index - 1
    };
    code_point_at(units, previous_index)
}

/// A match edge is a word boundary when it touches the line edge, the
/// neighbour is not a word character, or the match's own edge character is
/// not a word character. Filtering after grep avoids the ranges that a
/// consuming `(?:^|\W)` boundary would widen.
fn is_whole_word_range(line: &str, range: &JsRange) -> bool {
    let units: Vec<u16> = line.encode_utf16().collect();
    let start = range.start as usize;
    let end = range.end as usize;
    if end <= start {
        return false;
    }
    let is_word = |character: Option<char>| {
        character.is_some_and(|character| WORD_CHARACTER.is_match(&character.to_string()))
    };
    let left_is_boundary = start == 0
        || !is_word(code_point_before(&units, start))
        || !is_word(code_point_at(&units, start));
    let right_is_boundary = end >= units.len()
        || !is_word(code_point_at(&units, end))
        || !is_word(code_point_before(&units, end));
    left_is_boundary && right_is_boundary
}

fn remaining_time_budget_ms(deadline: Instant) -> u64 {
    let now = Instant::now();
    if now >= deadline {
        return 1;
    }
    let millis = (deadline - now).as_secs_f64() * 1000.0;
    (millis.ceil() as u64).max(1)
}

fn js_string_length(value: &str) -> u64 {
    value.encode_utf16().count() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    fn range(start: u64, end: u64) -> JsRange {
        JsRange { start, end }
    }

    #[test]
    fn whole_word_edges_match_the_upstream_cases() {
        assert!(is_whole_word_range("note notes denote", &range(0, 4)));
        assert!(!is_whole_word_range("note notes denote", &range(5, 9)));
        let astral = "𐐀foo foo foo𐐀";
        assert!(!is_whole_word_range(astral, &range(2, 5)));
        assert!(is_whole_word_range(astral, &range(6, 9)));
        assert!(!is_whole_word_range(astral, &range(10, 13)));
        let punctuation = "-foo- -foo- -foo-";
        assert!(is_whole_word_range(punctuation, &range(0, 5)));
        assert!(is_whole_word_range(punctuation, &range(6, 11)));
        assert!(is_whole_word_range(punctuation, &range(12, 17)));
        assert!(!is_whole_word_range("afoo-b", &range(1, 5)));
    }

    #[test]
    fn maps_multi_byte_lines_to_utf16_indexes() {
        let line = "const label = \"héllo wörld\";";
        let needle = "wörld";
        let start = line.find(needle).unwrap() as u64;
        let end = start + needle.len() as u64;
        let ranges = map_content_match_ranges(line, &[(start, end)]);
        let units: Vec<u16> = line.encode_utf16().collect();
        let sliced = String::from_utf16(
            &units[ranges[0].start as usize..ranges[0].end as usize],
        )
        .unwrap();
        assert_eq!(sliced, "wörld");
    }

    #[test]
    fn astral_prefix_uses_utf16_indexes_not_scalar_indexes() {
        let line = "𐐀wörld";
        let start = "𐐀".len() as u64;
        let ranges = map_content_match_ranges(line, &[(start, start + "wörld".len() as u64)]);
        assert_eq!(ranges[0].start, 2);
        assert_eq!(ranges[0].end, 7);
    }

    #[test]
    fn builds_content_queries_the_way_fff_expects() {
        let sensitive = build_content_search_query(&ContentSearchInput {
            query: "Square".into(),
            limit: 1,
            case_sensitive: true,
            whole_word: false,
            use_regex: true,
        });
        assert_eq!(sensitive.search_query, "Square");
        assert!(sensitive.regex_mode);
        let insensitive = build_content_search_query(&ContentSearchInput {
            query: "\\SQUARE".into(),
            limit: 1,
            case_sensitive: false,
            whole_word: false,
            use_regex: true,
        });
        assert_eq!(insensitive.search_query, "(?i)\\SQUARE");
        assert!(insensitive.regex_mode);
        let plain = build_content_search_query(&ContentSearchInput {
            query: "Square".into(),
            limit: 1,
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
        });
        assert_eq!(plain.search_query, "square");
        assert!(!plain.regex_mode);
    }

    #[test]
    fn fills_directory_ancestors_and_drops_the_root_entry() {
        let file = project_entry("src/components/Composer.tsx", ProjectEntryKind::File).unwrap();
        let mut paths: Vec<_> = with_directory_ancestors(&[file])
            .into_iter()
            .map(|entry| entry.path.as_str().to_owned())
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            ["src", "src/components", "src/components/Composer.tsx"]
        );
        let mixed = MixedSearchResult {
            items: vec![
                MixedItem::Directory(DirItem {
                    relative_path: String::new(),
                }),
                MixedItem::File(FileItem {
                    relative_path: "a.ts".into(),
                }),
            ],
            total_matched: 12,
        };
        let mapped = map_mixed_search_result(&mixed, 10);
        assert_eq!(mapped.entries.len(), 1);
        assert!(mapped.truncated);
    }

    struct MockFinder {
        wait: Mutex<Result<FinderResult<bool>, String>>,
        wait_timeouts: Mutex<Vec<u64>>,
        scanning: Arc<AtomicBool>,
        destroy_error: Mutex<Option<String>>,
        destroy_count: AtomicUsize,
        scan_files: Mutex<Option<Box<dyn FnMut() -> Result<FinderResult<()>, String> + Send>>>,
        mixed_search: Mutex<
            Option<
                Box<
                    dyn FnMut(&str, u64) -> Result<FinderResult<MixedSearchResult>, String> + Send,
                >,
            >,
        >,
        file_search: Mutex<
            Option<
                Box<dyn FnMut(&str, u64) -> Result<FinderResult<FileSearchResult>, String> + Send>,
            >,
        >,
        file_calls: Mutex<Vec<(String, u64)>>,
        grep: Mutex<
            Option<
                Box<
                    dyn FnMut(&str, &GrepOptions) -> Result<FinderResult<GrepResult>, String>
                        + Send,
                >,
            >,
        >,
        grep_calls: Mutex<Vec<GrepOptions>>,
    }

    impl MockFinder {
        fn ready() -> Self {
            Self {
                wait: Mutex::new(Ok(FinderResult::Ok(true))),
                wait_timeouts: Mutex::new(Vec::new()),
                scanning: Arc::new(AtomicBool::new(false)),
                destroy_error: Mutex::new(None),
                destroy_count: AtomicUsize::new(0),
                scan_files: Mutex::new(None),
                mixed_search: Mutex::new(None),
                file_search: Mutex::new(None),
                file_calls: Mutex::new(Vec::new()),
                grep: Mutex::new(None),
                grep_calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl FileFinderEngine for MockFinder {
        async fn wait_for_index_ready(
            &self,
            timeout_ms: u64,
        ) -> Result<FinderResult<bool>, String> {
            self.wait_timeouts.lock().expect("lock").push(timeout_ms);
            self.wait.lock().expect("lock").clone()
        }

        fn destroy(&self) -> Result<(), String> {
            self.destroy_count.fetch_add(1, Ordering::SeqCst);
            match self.destroy_error.lock().expect("lock").clone() {
                Some(cause) => Err(cause),
                None => Ok(()),
            }
        }

        fn is_scanning(&self) -> bool {
            self.scanning.load(Ordering::SeqCst)
        }

        fn scan_files(&self) -> Result<FinderResult<()>, String> {
            let mut callback = self.scan_files.lock().expect("lock");
            if let Some(callback) = callback.as_mut() {
                return callback();
            }
            Ok(FinderResult::Ok(()))
        }

        fn mixed_search(
            &self,
            query: &str,
            page_size: u64,
        ) -> Result<FinderResult<MixedSearchResult>, String> {
            let mut callback = self.mixed_search.lock().expect("lock");
            if let Some(callback) = callback.as_mut() {
                return callback(query, page_size);
            }
            Ok(FinderResult::Ok(MixedSearchResult {
                items: Vec::new(),
                total_matched: 0,
            }))
        }

        fn file_search(
            &self,
            query: &str,
            page_size: u64,
        ) -> Result<FinderResult<FileSearchResult>, String> {
            self.file_calls
                .lock()
                .expect("lock")
                .push((query.to_owned(), page_size));
            let mut callback = self.file_search.lock().expect("lock");
            if let Some(callback) = callback.as_mut() {
                return callback(query, page_size);
            }
            Ok(FinderResult::Ok(FileSearchResult {
                items: Vec::new(),
                total_matched: 0,
            }))
        }

        fn directory_search(
            &self,
            _query: &str,
            _page_size: u64,
        ) -> Result<FinderResult<DirSearchResult>, String> {
            Ok(FinderResult::Ok(DirSearchResult {
                items: Vec::new(),
                total_matched: 0,
            }))
        }

        fn grep(
            &self,
            query: &str,
            options: &GrepOptions,
        ) -> Result<FinderResult<GrepResult>, String> {
            self.grep_calls.lock().expect("lock").push(options.clone());
            let mut callback = self.grep.lock().expect("lock");
            if let Some(callback) = callback.as_mut() {
                return callback(query, options);
            }
            Ok(FinderResult::Ok(GrepResult {
                items: Vec::new(),
                next_cursor: None,
                regex_fallback_error: None,
            }))
        }
    }

    struct ScriptFactory {
        next: Mutex<Option<Result<FinderResult<Arc<dyn FileFinderEngine>>, String>>>,
    }

    impl ScriptFactory {
        fn new(next: Result<FinderResult<Arc<dyn FileFinderEngine>>, String>) -> Self {
            Self {
                next: Mutex::new(Some(next)),
            }
        }
    }

    impl FileFinderFactory for ScriptFactory {
        fn create(
            &self,
            _options: &FileFinderCreateOptions,
        ) -> Result<FinderResult<Arc<dyn FileFinderEngine>>, String> {
            self.next
                .lock()
                .expect("lock")
                .take()
                .expect("create called once")
        }
    }

    fn open_error(
        result: Result<WorkspaceSearchIndex, WorkspaceSearchIndexOpenError>,
    ) -> WorkspaceSearchIndexOpenError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("expected workspace search index open to fail"),
        }
    }

    fn file_item(relative_path: &str) -> FileItem {
        FileItem {
            relative_path: relative_path.to_owned(),
        }
    }

    async fn open_with(
        variant: WorkspaceSearchIndexVariant,
        finder: Arc<MockFinder>,
    ) -> Result<WorkspaceSearchIndex, WorkspaceSearchIndexOpenError> {
        let factory = ScriptFactory::new(Ok(FinderResult::Ok(
            finder as Arc<dyn FileFinderEngine>,
        )));
        WorkspaceSearchIndex::open("/workspace/project", variant, &factory).await
    }

    #[tokio::test]
    async fn filters_image_searches_before_applying_the_result_limit() {
        let mut items = (0..200)
            .map(|index| file_item(&format!("src/file-{index}.ts")))
            .collect::<Vec<_>>();
        items.push(file_item("public/icon.svg"));
        let total = items.len() as u64;
        let finder = Arc::new(MockFinder::ready());
        *finder.file_search.lock().unwrap() = Some(Box::new(move |_query, _page| {
            Ok(FinderResult::Ok(FileSearchResult {
                items: items.clone(),
                total_matched: total,
            }))
        }));
        let index = open_with(WorkspaceSearchIndexVariant::Paths, Arc::clone(&finder))
            .await
            .unwrap();
        let without_kind = index.search("", 200, None, true).await.unwrap();
        let with_directory = index
            .search("", 200, Some(ProjectEntryKind::Directory), true)
            .await
            .unwrap();
        assert_eq!(without_kind.entries.len(), 1);
        assert_eq!(without_kind.entries[0].path.as_str(), "public/icon.svg");
        assert_eq!(without_kind.entries[0].kind, ProjectEntryKind::File);
        assert_eq!(with_directory.entries[0].path.as_str(), "public/icon.svg");
        let calls = finder.file_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (String::new(), 25_002));
        assert_eq!(calls[1].1, 25_002);
    }

    #[tokio::test]
    async fn preserves_unexpected_file_finder_creation_failures() {
        let factory = ScriptFactory::new(Err("native initialization failed".into()));
        let error = WorkspaceSearchIndex::open(
            "/workspace/project",
            WorkspaceSearchIndexVariant::Paths,
            &factory,
        )
        .await;
        let error = open_error(error);
        let WorkspaceSearchIndexOpenError::Create(error) = error else {
            panic!("expected create failure");
        };
        assert_eq!(error.cwd, "/workspace/project");
        assert_eq!(error.reason, "FileFinder.create threw unexpectedly.");
        assert_eq!(error.cause.as_deref(), Some("native initialization failed"));
    }

    #[tokio::test]
    async fn keeps_returned_file_finder_creation_diagnostics_out_of_the_cause() {
        let factory = ScriptFactory::new(Ok(FinderResult::Err(
            "native index rejected the directory".into(),
        )));
        let error = WorkspaceSearchIndex::open(
            "/workspace/project",
            WorkspaceSearchIndexVariant::Paths,
            &factory,
        )
        .await;
        let error = open_error(error);
        let WorkspaceSearchIndexOpenError::Create(error) = error else {
            panic!("expected create failure");
        };
        assert_eq!(error.reason, "native index rejected the directory");
        assert!(error.cause.is_none());
    }

    #[tokio::test]
    async fn waits_for_the_full_content_index_warmup_before_returning() {
        let finder = Arc::new(MockFinder::ready());
        let index = open_with(WorkspaceSearchIndexVariant::Content, Arc::clone(&finder))
            .await
            .unwrap();
        assert_eq!(finder.wait_timeouts.lock().unwrap().as_slice(), &[15_000]);
        index.close().unwrap();
    }

    #[tokio::test]
    async fn preserves_a_full_index_warmup_timeout_as_a_structured_error() {
        let finder = Arc::new(MockFinder::ready());
        *finder.wait.lock().unwrap() = Ok(FinderResult::Ok(false));
        let error = open_error(
            open_with(WorkspaceSearchIndexVariant::Content, Arc::clone(&finder)).await,
        );
        let WorkspaceSearchIndexOpenError::ScanTimedOut(error) = error else {
            panic!("expected scan timeout");
        };
        assert_eq!(error.cwd, "/workspace/project");
        assert_eq!(error.timeout, "15 seconds");
        assert_eq!(finder.destroy_count.load(Ordering::SeqCst), 1);
    }

    fn two_items() -> MixedSearchResult {
        MixedSearchResult {
            items: vec![
                MixedItem::Directory(DirItem {
                    relative_path: "src".into(),
                }),
                MixedItem::File(file_item("src/index.ts")),
            ],
            total_matched: 2,
        }
    }

    #[tokio::test]
    async fn returns_partial_path_results_when_the_initial_scan_times_out() {
        let finder = Arc::new(MockFinder::ready());
        *finder.wait.lock().unwrap() = Ok(FinderResult::Ok(false));
        finder.scanning.store(true, Ordering::SeqCst);
        let calls = Arc::new(AtomicUsize::new(0));
        let scanning = Arc::clone(&finder.scanning);
        let calls_for_search = Arc::clone(&calls);
        *finder.mixed_search.lock().unwrap() = Some(Box::new(move |_query, _page| {
            let n = calls_for_search.fetch_add(1, Ordering::SeqCst);
            if n == 2 {
                scanning.store(false, Ordering::SeqCst);
            }
            Ok(FinderResult::Ok(two_items()))
        }));
        let index = open_with(WorkspaceSearchIndexVariant::Paths, Arc::clone(&finder))
            .await
            .unwrap();
        let list = index.list().await.unwrap();
        let search = index.search("src", 10, None, false).await.unwrap();
        let completing = index.search("src", 10, None, false).await.unwrap();
        finder.scanning.store(false, Ordering::SeqCst);
        let completed = index.search("src", 10, None, false).await.unwrap();
        let mut paths: Vec<_> = list
            .entries
            .iter()
            .map(|entry| (entry.kind, entry.path.as_str().to_owned()))
            .collect();
        paths.sort_by(|left, right| left.1.cmp(&right.1));
        assert_eq!(
            paths,
            vec![
                (ProjectEntryKind::Directory, "src".into()),
                (ProjectEntryKind::File, "src/index.ts".into()),
            ]
        );
        assert!(list.truncated);
        assert!(search.truncated);
        assert!(completing.truncated);
        assert!(!completed.truncated);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn close_preserves_file_finder_destroy_failures() {
        let finder = Arc::new(MockFinder::ready());
        *finder.destroy_error.lock().unwrap() = Some("native destroy failed".into());
        let index = open_with(WorkspaceSearchIndexVariant::Paths, Arc::clone(&finder))
            .await
            .unwrap();
        let error = index.close().unwrap_err();
        assert_eq!(error.cwd, "/workspace/project");
        assert_eq!(error.cause, "native destroy failed");
        assert_eq!(finder.destroy_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn preserves_search_and_refresh_failures_with_operation_context() {
        let finder = Arc::new(MockFinder::ready());
        *finder.mixed_search.lock().unwrap() = Some(Box::new(|_, _| Err("native search failed".into())));
        *finder.grep.lock().unwrap() = Some(Box::new(|_, _| Err("native grep failed".into())));
        *finder.scan_files.lock().unwrap() = Some(Box::new(|| Err("native scan failed".into())));
        let index = open_with(WorkspaceSearchIndexVariant::Paths, finder)
            .await
            .unwrap();
        let query = "authorization: Bearer secret-token";
        let search_error = index.search(query, 3, None, false).await.unwrap_err();
        let content_error = index
            .search_contents(&ContentSearchInput {
                query: query.to_owned(),
                limit: 3,
                case_sensitive: false,
                whole_word: false,
                use_regex: false,
            })
            .await
            .unwrap_err();
        let refresh_error = index.refresh().await.unwrap_err();
        assert_eq!(search_error.cwd, "/workspace/project");
        assert_eq!(search_error.query_length, js_string_length(query));
        assert_eq!(search_error.page_size, 4);
        assert_eq!(search_error.reason, "FileFinder.mixedSearch threw unexpectedly.");
        assert_eq!(search_error.cause.as_deref(), Some("native search failed"));
        assert!(!search_error.to_string().contains("Bearer"));
        assert!(!format!("{search_error:?}").contains("secret-token"));
        assert_eq!(content_error.query_length, js_string_length(query));
        assert_eq!(content_error.page_size, 3);
        assert_eq!(content_error.reason, "FileFinder.grep threw unexpectedly.");
        assert_eq!(content_error.cause.as_deref(), Some("native grep failed"));
        assert!(!content_error.to_string().contains("secret-token"));
        let WorkspaceSearchIndexRefreshError::Refresh(refresh_error) = refresh_error else {
            panic!("expected refresh failure");
        };
        assert_eq!(refresh_error.cwd, "/workspace/project");
        assert_eq!(refresh_error.reason, "FileFinder.scanFiles threw unexpectedly.");
        assert_eq!(refresh_error.cause.as_deref(), Some("native scan failed"));
    }

    #[tokio::test]
    async fn keeps_returned_search_diagnostics_out_of_the_cause_chain() {
        let finder = Arc::new(MockFinder::ready());
        *finder.mixed_search.lock().unwrap() =
            Some(Box::new(|_, _| Ok(FinderResult::Err("native query rejected".into()))));
        *finder.scan_files.lock().unwrap() =
            Some(Box::new(|| Ok(FinderResult::Err("native refresh rejected".into()))));
        let index = open_with(WorkspaceSearchIndexVariant::Paths, finder)
            .await
            .unwrap();
        let query = "authorization: Bearer secret-token";
        let search_error = index.search(query, 3, None, false).await.unwrap_err();
        let refresh_error = index.refresh().await.unwrap_err();
        assert_eq!(search_error.reason, "native query rejected");
        assert!(search_error.cause.is_none());
        assert_eq!(search_error.page_size, 4);
        assert!(!search_error.to_string().contains("Bearer"));
        let WorkspaceSearchIndexRefreshError::Refresh(refresh_error) = refresh_error else {
            panic!("expected refresh failure");
        };
        assert_eq!(refresh_error.reason, "native refresh rejected");
        assert!(refresh_error.cause.is_none());
    }

    #[tokio::test]
    async fn continues_whole_word_searches_after_a_filtered_grep_page() {
        let cursor = GrepCursor(1);
        let finder = Arc::new(MockFinder::ready());
        let cursor_for_page = cursor.clone();
        *finder.grep.lock().unwrap() = Some(Box::new(move |_query, options| {
            let (line, next) = if options.cursor.is_some() {
                ("needle", None)
            } else {
                ("needleSuffix", Some(cursor_for_page.clone()))
            };
            Ok(FinderResult::Ok(GrepResult {
                items: vec![GrepItem {
                    relative_path: "src/words.ts".into(),
                    line_number: 1,
                    line_content: line.to_owned(),
                    match_ranges: vec![(0, 6)],
                }],
                next_cursor: next,
                regex_fallback_error: None,
            }))
        }));
        let index = open_with(WorkspaceSearchIndexVariant::Content, Arc::clone(&finder))
            .await
            .unwrap();
        let result = index
            .search_contents(&ContentSearchInput {
                query: "needle".into(),
                limit: 1,
                case_sensitive: true,
                whole_word: true,
                use_regex: false,
            })
            .await
            .unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path.as_str(), "src/words.ts");
        assert_eq!(result.matches[0].line_number.get(), 1);
        assert_eq!(result.matches[0].line_content, "needle");
        assert_eq!(result.matches[0].match_ranges.len(), 1);
        assert_eq!(result.matches[0].match_ranges[0].start, 0);
        assert_eq!(result.matches[0].match_ranges[0].end, 6);
        assert!(!result.truncated);
        let calls = finder.grep_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].cursor, Some(cursor));
    }
}
