//! Port of the discovery half of upstream/apps/server/src/project/AgentSessionScanner.ts:
//! `scan` (candidate directories) and `recentThreads` (importable transcripts of
//! one workspace). The model types and the transcript parser live in
//! `agent_session_scanner.rs`. Upstream's `FileSystem`, `ServerConfig`,
//! `ServerSettings`, `ProjectStore` and `HostProcess` are the seams below.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{self, BoxStream, StreamExt};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;
use turin_contracts::agent_sessions::{
    AgentSessionImportSource, AgentSessionProjectCandidate, AgentSessionProjectGit,
    AgentSessionScanError, AgentSessionScanOperation, AgentSessionScanResult, AgentSessionSource,
};
use turin_contracts::base_schemas::{TrimmedNonEmptyString, UtcDateTime, js_trim};
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
};
use turin_contracts::settings::{ClaudeSettings, CodexSettings, resolve_provider_instance_enabled};
use turin_git::shared_git::{
    normalize_git_remote_url, parse_github_repository_name_with_owner_from_remote_url,
    parse_origin_url_from_git_config,
};

use crate::agent_session_json::{
    JsonSelectSchema, TranscriptJsonLimitError, TranscriptJsonReader,
    create_transcript_json_selector,
};
use crate::agent_session_scanner::{
    AgentSessionRecentThread, AgentSessionScanner, AgentSessionThread,
    AgentSessionTranscriptMetadata, MAX_IMPORT_RECORDS, TranscriptRecord, parse_records,
};
use crate::codex_home_layout::{lexical_resolve, resolve_codex_home_layout};
use crate::project_path::{expand_home_path, normalize_project_path_for_comparison};
use crate::project_ports::{PortError, ProjectStoreReads};

/// Chunk size for full transcript reads.
const TRANSCRIPT_PREFIX_BYTES: usize = 32 * 1024;
/// Small reads avoid wasting the metadata budget on long Codex instruction headers.
const METADATA_READ_BYTES: usize = 8 * 1024;
/// Prevent malformed transcripts from turning project discovery into a full file scan.
const MAX_TRANSCRIPT_SCAN_BYTES: u64 = 1024 * 1024;
/// Upper bound on transcripts inspected (first line read) per source.
const MAX_TRANSCRIPTS_PER_SOURCE: usize = 5000;
/// Upper bound on discovery filesystem operations per source (directory reads and stats).
const MAX_DISCOVERY_OPERATIONS_PER_SOURCE: usize = MAX_TRANSCRIPTS_PER_SOURCE * 4;
const MAX_METADATA_BYTES_PER_SOURCE: u64 = 64 * 1024 * 1024;
const MAX_METADATA_OPERATIONS_PER_SOURCE: usize = MAX_TRANSCRIPTS_PER_SOURCE * 4;
const MAX_METADATA_RECORDS_PER_SOURCE: usize = 100_000;
const MAX_METADATA_RECORDS_PER_TRANSCRIPT: usize = 1_000;
const RECENT_THREAD_WINDOW_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;
/// Large tool results can make an ordinary Codex transcript several GiB. Selected
/// history and raw I/O have separate budgets.
const MAX_IMPORTED_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_IMPORT_HISTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_IMPORT_TRANSCRIPTS: usize = 100;
/// JavaScript `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

static GITDIR_POINTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^gitdir:\s*(.+)$").expect("valid regex"));
static LINKED_WORKTREE_DIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\\/]worktrees[\\/][^\\/]+[\\/]?$").expect("valid regex"));

/// What `stat` reports. Numbers stay `f64` because they go straight into
/// `AgentSessionImportSource` (`device`, `inode`, the mtimes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileStat {
    pub is_file: bool,
    pub is_directory: bool,
    pub size: u64,
    pub mtime_ms: Option<f64>,
    pub birthtime_ms: Option<f64>,
    pub dev: f64,
    pub ino: Option<f64>,
}

/// Upstream `FileSystem` as discovery uses it. Every method reports failure as
/// `None` or an empty list, matching the `orElseSucceed` fallbacks upstream.
#[async_trait]
pub trait ScannerFileSystem: Send + Sync {
    async fn stat(&self, path: &str) -> Option<FileStat>;
    async fn real_path(&self, path: &str) -> Option<String>;
    async fn read_directory(&self, path: &str) -> Vec<String>;
    async fn read_file_string(&self, path: &str) -> Option<String>;
    async fn open(&self, path: &str) -> Option<Box<dyn ScannerFile>>;
}

/// An open file (upstream `FileSystem.File`).
#[async_trait]
pub trait ScannerFile: Send + Sync {
    async fn stat(&self) -> Option<FileStat>;
    /// Upstream `readAlloc(size)`: up to `size` bytes, `None` at end of file.
    async fn read_alloc(&mut self, size: usize) -> std::io::Result<Option<Vec<u8>>>;
}

/// The host filesystem through `tokio::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalScannerFileSystem;

#[async_trait]
impl ScannerFileSystem for LocalScannerFileSystem {
    async fn stat(&self, path: &str) -> Option<FileStat> {
        tokio::fs::metadata(path).await.ok().map(|m| file_stat(&m))
    }

    async fn real_path(&self, path: &str) -> Option<String> {
        tokio::fs::canonicalize(path)
            .await
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    async fn read_directory(&self, path: &str) -> Vec<String> {
        let Ok(mut entries) = tokio::fs::read_dir(path).await else {
            return Vec::new();
        };
        let mut names = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names
    }

    async fn read_file_string(&self, path: &str) -> Option<String> {
        tokio::fs::read_to_string(path).await.ok()
    }

    async fn open(&self, path: &str) -> Option<Box<dyn ScannerFile>> {
        tokio::fs::File::open(path)
            .await
            .ok()
            .map(|file| Box::new(LocalScannerFile(file)) as Box<dyn ScannerFile>)
    }
}

struct LocalScannerFile(tokio::fs::File);

#[async_trait]
impl ScannerFile for LocalScannerFile {
    async fn stat(&self) -> Option<FileStat> {
        self.0.metadata().await.ok().map(|m| file_stat(&m))
    }

    async fn read_alloc(&mut self, size: usize) -> std::io::Result<Option<Vec<u8>>> {
        let mut buffer = vec![0u8; size];
        let mut filled = 0;
        while filled < size {
            let read = self.0.read(&mut buffer[filled..]).await?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        if filled == 0 {
            return Ok(None);
        }
        buffer.truncate(filled);
        Ok(Some(buffer))
    }
}

fn file_stat(metadata: &std::fs::Metadata) -> FileStat {
    FileStat {
        is_file: metadata.is_file(),
        is_directory: metadata.is_dir(),
        size: metadata.len(),
        mtime_ms: metadata.modified().ok().map(system_time_ms),
        birthtime_ms: metadata.created().ok().map(system_time_ms),
        dev: device_of(metadata),
        ino: inode_of(metadata),
    }
}

#[cfg(unix)]
fn device_of(metadata: &std::fs::Metadata) -> f64 {
    std::os::unix::fs::MetadataExt::dev(metadata) as f64
}

#[cfg(unix)]
fn inode_of(metadata: &std::fs::Metadata) -> Option<f64> {
    Some(std::os::unix::fs::MetadataExt::ino(metadata) as f64)
}

#[cfg(not(unix))]
fn device_of(_metadata: &std::fs::Metadata) -> f64 {
    0.0
}

#[cfg(not(unix))]
fn inode_of(_metadata: &std::fs::Metadata) -> Option<f64> {
    None
}

/// Epoch milliseconds, truncated like JavaScript `Date#getTime`.
fn system_time_ms(time: SystemTime) -> f64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as f64,
        Err(error) => -(error.duration().as_millis() as f64),
    }
}

/// What discovery reads from `ServerConfig` and `HostProcess`.
#[derive(Debug, Clone, Default)]
pub struct AgentSessionDiscoveryHost {
    /// `HostProcess.HomeDirectory`.
    pub home_dir: String,
    /// `NodeOS.tmpdir()`. Excluded as a project root.
    pub tmp_dir: String,
    /// `ServerConfig.baseDir`. Anything under it is T3's own data.
    pub base_dir: String,
    /// `ServerConfig.worktreesDir`. Anything under it is a T3 sandbox.
    pub worktrees_dir: String,
    /// Windows filesystems are case-insensitive, so prefix checks fold case there.
    pub case_fold: bool,
    /// `HostProcess.Environment`.
    pub environment: HashMap<String, String>,
}

/// Upstream `ServerSettings.providerInstances`.
#[async_trait]
pub trait AgentSessionDiscoverySettings: Send + Sync {
    async fn provider_instances(&self) -> Result<ProviderInstanceConfigMap, PortError>;
}

/// Upstream `AgentSessionScanner` service, with its dependencies injected.
pub struct AgentSessionDiscovery {
    host: AgentSessionDiscoveryHost,
    fs: Arc<dyn ScannerFileSystem>,
    settings: Arc<dyn AgentSessionDiscoverySettings>,
    projects: Arc<dyn ProjectStoreReads>,
    /// Upstream `importReadLock`: only one transcript holds the selected-history budget.
    import_read_lock: Semaphore,
    /// Upstream `cachedCandidates`, set by `scan` and reused by `recentThreads`.
    cached_candidates: Mutex<Option<Arc<Vec<RawCandidate>>>>,
    /// Upstream `DateTime.now`, epoch milliseconds.
    now_ms: Box<dyn Fn() -> f64 + Send + Sync>,
    excluded_roots: HashSet<String>,
    excluded_ancestors: Vec<String>,
}

#[derive(Debug, Clone)]
struct RawTranscript {
    file_path: String,
    mtime_ms: f64,
}

/// One directory's evidence from one source (upstream `RawCandidate`).
#[derive(Debug, Clone)]
struct RawCandidate {
    cwd: String,
    source: AgentSessionSource,
    provider_instance_id: ProviderInstanceId,
    thread_count: u64,
    last_active_at_ms: f64,
    transcripts: Vec<RawTranscript>,
}

#[derive(Debug, Clone)]
struct TranscriptCandidate {
    file_path: String,
    mtime_ms: f64,
    provider_instance_id: ProviderInstanceId,
    size: u64,
}

struct Discovered {
    transcripts: Vec<TranscriptCandidate>,
    truncated: bool,
}

/// Upstream `MetadataReadBudget`: shared across the homes of one source.
struct MetadataReadBudget {
    bytes_remaining: u64,
    operations_remaining: usize,
    records_remaining: usize,
    truncated: bool,
}

/// Upstream's per-discovery operation counter. Exhaustion marks the scan truncated.
struct DiscoveryBudget {
    remaining: usize,
    truncated: bool,
}

impl DiscoveryBudget {
    fn new(remaining: usize) -> Self {
        Self {
            remaining,
            truncated: false,
        }
    }

    /// Upstream `if (operationsRemaining <= 0) { truncated = true; break; }`.
    fn exhausted(&mut self) -> bool {
        if self.remaining == 0 {
            self.truncated = true;
            return true;
        }
        false
    }

    fn spend(&mut self) {
        self.remaining = self.remaining.saturating_sub(1);
    }
}

/// Upstream `TranscriptIdentity`: what makes a transcript file "the same file".
#[derive(Debug, Clone, PartialEq)]
struct TranscriptIdentity {
    file_path: String,
    size: u64,
    mtime_ms: Option<f64>,
    device: f64,
    inode: Option<f64>,
    birthtime_ms: Option<f64>,
}

fn transcript_identity(file_path: &str, stats: &FileStat) -> TranscriptIdentity {
    TranscriptIdentity {
        file_path: file_path.to_owned(),
        size: stats.size,
        mtime_ms: stats.mtime_ms,
        device: stats.dev,
        inode: stats.ino,
        birthtime_ms: stats.birthtime_ms,
    }
}

/// Upstream `sameTranscriptIdentity(source, identity)` for a completed import.
fn same_source_identity(source: &AgentSessionImportSource, identity: &TranscriptIdentity) -> bool {
    source.file_path.as_str() == identity.file_path
        && source.size == identity.size
        && source.mtime_ms == identity.mtime_ms
        && source.device == identity.device
        && source.inode == identity.inode
        && source.birthtime_ms == identity.birthtime_ms
}

/// Upstream `GitIdentity` result of `readGitIdentity`.
enum GitIdentity {
    Repository(AgentSessionProjectGit),
    Worktree,
    NotGit,
}

struct MergedCandidate {
    path: String,
    sources: Vec<AgentSessionSource>,
    thread_count: u64,
    last_active_at_ms: f64,
    git: Option<AgentSessionProjectGit>,
}

struct EligibleTranscript {
    source: AgentSessionSource,
    provider_instance_id: ProviderInstanceId,
    file_path: String,
    mtime_ms: f64,
}

/// State of one `recentThreads` stream (the upstream closure over its budgets).
struct RecentState {
    eligible: VecDeque<EligibleTranscript>,
    completed: HashMap<String, Vec<AgentSessionImportSource>>,
    imported_sessions: HashSet<String>,
    bytes_remaining: u64,
    transcripts_remaining: usize,
    records_remaining: usize,
    root_identity: String,
}

impl RecentState {
    fn empty() -> Self {
        Self {
            eligible: VecDeque::new(),
            completed: HashMap::new(),
            imported_sessions: HashSet::new(),
            bytes_remaining: 0,
            transcripts_remaining: 0,
            records_remaining: 0,
            root_identity: String::new(),
        }
    }
}

struct TranscriptSnapshot {
    records: Vec<TranscriptRecord>,
    record_count: usize,
}

/// Byte counters shared with the JSON reader's reserve callback.
#[derive(Default)]
struct HistoryBudget {
    record_bytes: AtomicUsize,
    history_bytes: AtomicUsize,
}

/// Upstream `readTranscript`'s decode loop, one record at a time.
struct TranscriptSelection {
    reader: TranscriptJsonReader<'static>,
    history: Arc<HistoryBudget>,
    pending: Vec<u8>,
    record_started: bool,
    record_count: usize,
    record_limit: usize,
    records: Vec<TranscriptRecord>,
    source: AgentSessionSource,
}

impl AgentSessionDiscovery {
    pub fn new(
        host: AgentSessionDiscoveryHost,
        fs: Arc<dyn ScannerFileSystem>,
        settings: Arc<dyn AgentSessionDiscoverySettings>,
        projects: Arc<dyn ProjectStoreReads>,
    ) -> Self {
        let excluded_roots = [
            host.home_dir.as_str(),
            host.tmp_dir.as_str(),
            "/tmp",
            "/private/tmp",
        ]
        .into_iter()
        .map(|directory| normalize_project_path_for_comparison(&lexical_resolve(directory)))
        .collect();
        let excluded_ancestors = [
            Path::new(&host.home_dir).join("Downloads"),
            Path::new(&host.home_dir).join("Documents").join("Codex"),
        ]
        .iter()
        .map(|ancestor| normalize_for_worktree_match(&ancestor.to_string_lossy(), host.case_fold))
        .collect();
        Self {
            host,
            fs,
            settings,
            projects,
            import_read_lock: Semaphore::new(1),
            cached_candidates: Mutex::new(None),
            now_ms: Box::new(|| Utc::now().timestamp_millis() as f64),
            excluded_roots,
            excluded_ancestors,
        }
    }

    /// Replaces the wall clock. Used by the window cutoff and `scannedAt`.
    pub fn with_clock(mut self, now_ms: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        self.now_ms = Box::new(now_ms);
        self
    }

    /// Host filesystem and the given settings and project reads.
    pub fn with_local_filesystem(
        host: AgentSessionDiscoveryHost,
        settings: Arc<dyn AgentSessionDiscoverySettings>,
        projects: Arc<dyn ProjectStoreReads>,
    ) -> Self {
        Self::new(host, Arc::new(LocalScannerFileSystem), settings, projects)
    }

    /// Upstream `isExcludedProjectPath`.
    fn is_excluded_project_path(&self, candidate: &str) -> bool {
        let case_fold = self.host.case_fold;
        self.excluded_roots
            .contains(&normalize_project_path_for_comparison(candidate))
            || self.excluded_ancestors.iter().any(|ancestor| {
                normalize_for_worktree_match(candidate, case_fold).starts_with(ancestor)
            })
            || normalize_for_worktree_match(candidate, case_fold).starts_with(
                &normalize_for_worktree_match(&self.host.base_dir, case_fold),
            )
            || is_t3_managed_worktree(candidate, &self.host.worktrees_dir, case_fold)
    }

    /// Upstream `directoryIdentity`: inode when the filesystem has one, otherwise
    /// the realpath, so aliases merge without merging distinct case-sensitive dirs.
    async fn directory_identity(&self, target: &str, known: Option<FileStat>) -> String {
        let resolved = lexical_resolve(target);
        let stats = match known {
            Some(stats) => Some(stats),
            None => self.fs.stat(&resolved).await,
        };
        if let Some(stats) = stats
            && let Some(ino) = stats.ino
            && ino.fract() == 0.0
            && ino > 0.0
            && ino <= MAX_SAFE_INTEGER
        {
            return format!("inode:{}:{}", stats.dev, ino);
        }
        let real = self
            .fs
            .real_path(&resolved)
            .await
            .unwrap_or_else(|| resolved.clone());
        format!("path:{}", normalize_project_path_for_comparison(&real))
    }

    /// Upstream `readGitIdentity`. Reads `.git` directly instead of spawning git.
    async fn read_git_identity(&self, directory: &str) -> GitIdentity {
        let git_path = join(&[directory, ".git"]);
        let Some(git_stats) = self.fs.stat(&git_path).await else {
            return GitIdentity::NotGit;
        };
        let mut git_dir = git_path.clone();
        if !git_stats.is_directory {
            let pointer = self
                .fs
                .read_file_string(&git_path)
                .await
                .unwrap_or_default();
            let target = GITDIR_POINTER
                .captures(&pointer)
                .and_then(|captures| captures.get(1))
                .map(|target| js_trim(target.as_str()).to_owned())
                .filter(|target| !target.is_empty());
            let Some(target) = target else {
                return GitIdentity::NotGit;
            };
            git_dir = lexical_resolve(&join(&[directory, &target]));
            if LINKED_WORKTREE_DIR.is_match(&git_dir) {
                return GitIdentity::Worktree;
            }
        }
        let config_text = self
            .fs
            .read_file_string(&join(&[&git_dir, "config"]))
            .await
            .unwrap_or_default();
        let origin_url = parse_origin_url_from_git_config(&config_text);
        GitIdentity::Repository(AgentSessionProjectGit {
            remote_key: origin_url.as_deref().map(normalize_git_remote_url),
            repository: parse_github_repository_name_with_owner_from_remote_url(
                origin_url.as_deref(),
            ),
        })
    }

    /// Upstream `readCwd`: the first `cwd` in the transcript's leading records,
    /// read in bounded chunks under the shared metadata budget.
    async fn read_cwd(
        &self,
        transcript: &TranscriptCandidate,
        budget: &mut MetadataReadBudget,
    ) -> Option<String> {
        if transcript.size == 0 {
            return None;
        }
        if budget.bytes_remaining == 0
            || budget.operations_remaining < 2
            || budget.records_remaining == 0
        {
            budget.truncated = true;
            return None;
        }
        budget.operations_remaining -= 1;
        let mut file = self.fs.open(&transcript.file_path).await?;
        let max_bytes = MAX_TRANSCRIPT_SCAN_BYTES.min(transcript.size);
        let mut remaining: Vec<u8> = Vec::new();
        let mut bytes_read: u64 = 0;
        let mut records_read = 0usize;
        // Bytes of `remaining` already known to hold no newline, so a long record is scanned once.
        let mut scanned = 0usize;
        while bytes_read < max_bytes {
            if budget.bytes_remaining == 0 || budget.operations_remaining == 0 {
                budget.truncated = true;
                return None;
            }
            let read_size = (METADATA_READ_BYTES as u64)
                .min(max_bytes - bytes_read)
                .min(budget.bytes_remaining);
            budget.operations_remaining -= 1;
            budget.bytes_remaining -= read_size;
            let next = match file.read_alloc(read_size as usize).await {
                Ok(Some(next)) => next,
                Ok(None) => {
                    return last_record_cwd(&remaining, budget, &mut records_read);
                }
                Err(_) => return None,
            };
            bytes_read += next.len() as u64;
            remaining.extend_from_slice(&next);
            while let Some(newline) = remaining[scanned..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|offset| scanned + offset)
            {
                let line: Vec<u8> = remaining.drain(..=newline).collect();
                scanned = 0;
                let text = String::from_utf8_lossy(&line[..line.len() - 1]);
                if !reserve_metadata_record(budget, &mut records_read) {
                    return None;
                }
                if let Some(cwd) = extract_cwd(js_trim(&text)) {
                    return Some(cwd);
                }
            }
            scanned = remaining.len();
        }
        if bytes_read < transcript.size {
            budget.truncated = true;
            return None;
        }
        last_record_cwd(&remaining, budget, &mut records_read)
    }

    /// Upstream `readTranscript`: project history fields while reading, before
    /// allocating whole JSON records. Identity is checked on both sides of the read.
    async fn read_transcript(
        &self,
        file_path: &str,
        expected: &TranscriptIdentity,
        record_limit: usize,
        source: AgentSessionSource,
    ) -> Option<TranscriptSnapshot> {
        if expected.size > MAX_IMPORTED_TRANSCRIPT_BYTES {
            return None;
        }
        let mut file = self.fs.open(file_path).await?;
        let opened = file.stat().await?;
        if transcript_identity(file_path, &opened) != *expected {
            return None;
        }
        let history = Arc::new(HistoryBudget::default());
        let mut selection = TranscriptSelection::new(history, record_limit, source);
        let mut bytes_read: u64 = 0;
        while bytes_read < expected.size {
            let want = (TRANSCRIPT_PREFIX_BYTES as u64).min(expected.size - bytes_read) as usize;
            let chunk = match file.read_alloc(want).await {
                Ok(Some(chunk)) => chunk,
                _ => return None,
            };
            bytes_read += chunk.len() as u64;
            let mut start = 0;
            while start < chunk.len() {
                let end = chunk[start..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(chunk.len(), |offset| start + offset);
                selection.write_segment(&chunk[start..end]).ok()?;
                if end == chunk.len() {
                    break;
                }
                if !selection.finish_record().ok()? {
                    return None;
                }
                start = end + 1;
            }
        }
        if selection.record_started && !selection.finish_record().ok()? {
            return None;
        }
        let closing = file.stat().await?;
        if transcript_identity(file_path, &closing) != *expected {
            return None;
        }
        Some(TranscriptSnapshot {
            record_count: selection.record_count,
            records: selection.records,
        })
    }

    /// Upstream `discoverClaudeTranscripts`: `projects/<dir>/*.jsonl`.
    async fn discover_claude_transcripts(
        &self,
        home_path: &str,
        provider_instance_id: &ProviderInstanceId,
        operation_budget: usize,
    ) -> Discovered {
        let projects_dir = join(&[home_path, "projects"]);
        let mut budget = DiscoveryBudget::new(operation_budget);
        let mut transcripts = Vec::new();
        let project_directories = self
            .read_budgeted_directory(&mut budget, &projects_dir)
            .await;
        for project_directory in project_directories {
            if budget.exhausted() {
                break;
            }
            let directory = join(&[&projects_dir, &project_directory]);
            let entries = self.read_budgeted_directory(&mut budget, &directory).await;
            for entry in entries.iter().filter(|entry| entry.ends_with(".jsonl")) {
                if budget.exhausted() {
                    break;
                }
                let file_path = join(&[&directory, entry]);
                budget.spend();
                if let Some(candidate) =
                    self.stat_transcript(&file_path, provider_instance_id).await
                {
                    transcripts.push(candidate);
                }
            }
        }
        Discovered {
            transcripts,
            truncated: budget.truncated,
        }
    }

    /// Upstream `discoverCodexTranscripts`: `sessions/YYYY/MM/DD/rollout-*.jsonl`,
    /// walked newest date first so the budget keeps recent sessions.
    async fn discover_codex_transcripts(
        &self,
        home_path: &str,
        provider_instance_id: &ProviderInstanceId,
        operation_budget: usize,
    ) -> Discovered {
        let sessions_dir = join(&[home_path, "sessions"]);
        let mut budget = DiscoveryBudget::new(operation_budget);
        let mut transcripts = Vec::new();
        for year in self
            .read_budgeted_directory_newest_first(&mut budget, &sessions_dir)
            .await
        {
            if budget.exhausted() {
                break;
            }
            let year_dir = join(&[&sessions_dir, &year]);
            for month in self
                .read_budgeted_directory_newest_first(&mut budget, &year_dir)
                .await
            {
                if budget.exhausted() {
                    break;
                }
                let month_dir = join(&[&year_dir, &month]);
                for day in self
                    .read_budgeted_directory_newest_first(&mut budget, &month_dir)
                    .await
                {
                    if budget.exhausted() {
                        break;
                    }
                    let directory = join(&[&month_dir, &day]);
                    let entries = self
                        .read_budgeted_directory_newest_first(&mut budget, &directory)
                        .await;
                    for entry in entries {
                        if !entry.starts_with("rollout-") || !entry.ends_with(".jsonl") {
                            continue;
                        }
                        if budget.exhausted() {
                            break;
                        }
                        let file_path = join(&[&directory, &entry]);
                        budget.spend();
                        if let Some(candidate) =
                            self.stat_transcript(&file_path, provider_instance_id).await
                        {
                            transcripts.push(candidate);
                        }
                    }
                }
            }
        }
        Discovered {
            transcripts,
            truncated: budget.truncated,
        }
    }

    async fn read_budgeted_directory(
        &self,
        budget: &mut DiscoveryBudget,
        directory: &str,
    ) -> Vec<String> {
        if budget.exhausted() {
            return Vec::new();
        }
        budget.spend();
        self.fs.read_directory(directory).await
    }

    async fn read_budgeted_directory_newest_first(
        &self,
        budget: &mut DiscoveryBudget,
        directory: &str,
    ) -> Vec<String> {
        let mut entries = self.read_budgeted_directory(budget, directory).await;
        entries.sort();
        entries.reverse();
        entries
    }

    async fn stat_transcript(
        &self,
        file_path: &str,
        provider_instance_id: &ProviderInstanceId,
    ) -> Option<TranscriptCandidate> {
        let stats = self.fs.stat(file_path).await?;
        if !stats.is_file {
            return None;
        }
        Some(TranscriptCandidate {
            file_path: file_path.to_owned(),
            mtime_ms: stats.mtime_ms?,
            provider_instance_id: provider_instance_id.clone(),
            size: stats.size,
        })
    }

    /// Upstream `groupTranscriptsByCwd`: one candidate per (instance, cwd).
    async fn group_transcripts_by_cwd(
        &self,
        source: AgentSessionSource,
        transcripts: &[TranscriptCandidate],
        budget: &mut MetadataReadBudget,
    ) -> Vec<RawCandidate> {
        let mut groups: IndexMap<String, RawCandidate> = IndexMap::new();
        for transcript in transcripts {
            let Some(cwd) = self.read_cwd(transcript, budget).await else {
                continue;
            };
            let raw_transcript = RawTranscript {
                file_path: transcript.file_path.clone(),
                mtime_ms: transcript.mtime_ms,
            };
            let key = format!("{}\0{}", transcript.provider_instance_id.as_str(), cwd);
            match groups.get_mut(&key) {
                Some(existing) => {
                    existing.last_active_at_ms =
                        existing.last_active_at_ms.max(transcript.mtime_ms);
                    existing.thread_count += 1;
                    existing.transcripts.push(raw_transcript);
                }
                None => {
                    groups.insert(
                        key,
                        RawCandidate {
                            cwd,
                            source,
                            provider_instance_id: transcript.provider_instance_id.clone(),
                            thread_count: 1,
                            last_active_at_ms: transcript.mtime_ms,
                            transcripts: vec![raw_transcript],
                        },
                    );
                }
            }
        }
        groups.into_values().collect()
    }

    /// Upstream `collectCandidates`: every enabled Claude and Codex home, scanned
    /// newest-first within the per-source budgets. Returns the candidates and
    /// whether any budget ran out.
    async fn collect_candidates(&self) -> Result<(Vec<RawCandidate>, bool), AgentSessionScanError> {
        let settings = self
            .settings
            .provider_instances()
            .await
            .map_err(|cause| scan_error(AgentSessionScanOperation::ReadSettings, cause))?;

        let mut raw = Vec::new();
        let mut truncated = false;
        for source in [AgentSessionSource::ClaudeAgent, AgentSessionSource::Codex] {
            let driver = driver_slug(source);
            let mut instances: Vec<(ProviderInstanceId, ProviderInstanceConfig)> = settings
                .iter()
                .filter(|(_, instance)| {
                    instance.driver.as_str() == driver
                        && resolve_provider_instance_enabled(instance)
                })
                .map(|(id, config)| (id.clone(), config.clone()))
                .collect();
            // The built-in instance runs with default config when settings have no entry for it.
            if let Ok(default_id) = ProviderInstanceId::new(driver)
                && !settings.contains_key(&default_id)
            {
                let default_config = default_instance_config(driver);
                if resolve_provider_instance_enabled(&default_config) {
                    instances.push((default_id, default_config));
                }
            }
            // A shared home holds one copy of each session. The built-in instance owns it.
            instances.sort_by_key(|(id, _)| usize::from(id.as_str() != driver));

            let home_variable = match source {
                AgentSessionSource::ClaudeAgent => "CLAUDE_CONFIG_DIR",
                AgentSessionSource::Codex => "CODEX_HOME",
            };
            let mut homes: Vec<(String, ProviderInstanceId)> = Vec::new();
            let mut seen_homes: HashSet<String> = HashSet::new();
            for (instance_id, config) in &instances {
                let environment_home = config
                    .environment
                    .as_ref()
                    .and_then(|environment| {
                        environment
                            .iter()
                            .rev()
                            .find(|variable| variable.name.as_str() == home_variable)
                    })
                    .map(|variable| variable.value.clone())
                    .or_else(|| self.host.environment.get(home_variable).cloned());

                let home_path = match source {
                    AgentSessionSource::ClaudeAgent => {
                        let Some(settings) = decode_config::<ClaudeSettings>(config) else {
                            continue;
                        };
                        resolve_claude_config_dir(
                            settings.home_path.as_str(),
                            environment_home.as_deref(),
                            &self.host.home_dir,
                        )
                    }
                    AgentSessionSource::Codex => {
                        let Some(settings) = decode_config::<CodexSettings>(config) else {
                            continue;
                        };
                        let home_setting = settings.home_path.as_str();
                        let shadow_setting = settings.shadow_home_path.as_str();
                        // An env var fills in only when neither home setting is present.
                        let effective_home =
                            match environment_home.filter(|value| !js_trim(value).is_empty()) {
                                Some(environment_home)
                                    if js_trim(home_setting).is_empty()
                                        && js_trim(shadow_setting).is_empty() =>
                                {
                                    environment_home
                                }
                                _ => home_setting.to_owned(),
                            };
                        resolve_codex_home_layout(
                            &self.host.home_dir,
                            &effective_home,
                            shadow_setting,
                        )
                        .shared_home_path
                    }
                };

                let home_key = format!(
                    "{}\0{}",
                    driver,
                    self.directory_identity(&home_path, None).await
                );
                if !seen_homes.insert(home_key) {
                    continue;
                }
                homes.push((home_path, instance_id.clone()));
            }

            let mut transcript_candidates: Vec<TranscriptCandidate> = Vec::new();
            let homes_count = homes.len().max(1);
            let base_operation_budget = MAX_DISCOVERY_OPERATIONS_PER_SOURCE / homes_count;
            let extra_operation_budgets = MAX_DISCOVERY_OPERATIONS_PER_SOURCE % homes_count;
            for (index, (home_path, instance_id)) in homes.iter().enumerate() {
                let operation_budget =
                    base_operation_budget + usize::from(index < extra_operation_budgets);
                if operation_budget == 0 {
                    truncated = true;
                    continue;
                }
                let discovered = match source {
                    AgentSessionSource::ClaudeAgent => {
                        self.discover_claude_transcripts(home_path, instance_id, operation_budget)
                            .await
                    }
                    AgentSessionSource::Codex => {
                        self.discover_codex_transcripts(home_path, instance_id, operation_budget)
                            .await
                    }
                };
                truncated |= discovered.truncated;
                transcript_candidates.extend(discovered.transcripts);
            }

            transcript_candidates.sort_by(|left, right| {
                right
                    .mtime_ms
                    .partial_cmp(&left.mtime_ms)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| left.file_path.cmp(&right.file_path))
            });
            if transcript_candidates.len() > MAX_TRANSCRIPTS_PER_SOURCE {
                truncated = true;
            }
            // Give each account a turn before taking another file from the same home.
            let selected = select_metadata_transcripts(&transcript_candidates);
            let mut metadata_budget = MetadataReadBudget {
                bytes_remaining: MAX_METADATA_BYTES_PER_SOURCE,
                operations_remaining: MAX_METADATA_OPERATIONS_PER_SOURCE,
                records_remaining: MAX_METADATA_RECORDS_PER_SOURCE,
                truncated: false,
            };
            raw.extend(
                self.group_transcripts_by_cwd(source, &selected, &mut metadata_budget)
                    .await,
            );
            truncated |= metadata_budget.truncated;
        }
        Ok((raw, truncated))
    }

    /// Upstream `scan`: merge candidates by filesystem identity, drop T3's own and
    /// vanished directories, and mark projects that already exist.
    async fn scan_candidates(&self) -> Result<AgentSessionScanResult, AgentSessionScanError> {
        let (raw, truncated) = self.collect_candidates().await?;
        let raw = Arc::new(raw);
        *self.cached_candidates.lock() = Some(Arc::clone(&raw));

        let home = self.host.home_dir.as_str();
        let mut merged: IndexMap<String, MergedCandidate> = IndexMap::new();
        let mut directory_keys: HashMap<String, String> = HashMap::new();
        let mut git_identities: HashMap<String, Option<AgentSessionProjectGit>> = HashMap::new();

        for candidate in raw.iter() {
            let expanded = expand_home_path(js_trim(&candidate.cwd), home);
            if !Path::new(&expanded).is_absolute() {
                continue;
            }
            let resolved = lexical_resolve(&expanded);
            if self.is_excluded_project_path(&resolved) {
                continue;
            }
            let key = match directory_keys.get(&resolved) {
                Some(key) => key.clone(),
                None => {
                    let key = match self.fs.stat(&resolved).await {
                        Some(stats) if stats.is_directory => {
                            let real = self
                                .fs
                                .real_path(&resolved)
                                .await
                                .unwrap_or_else(|| resolved.clone());
                            // A symlink can reach the worktrees dir even when its own spelling doesn't.
                            if self.is_excluded_project_path(&real) {
                                String::new()
                            } else {
                                match self.read_git_identity(&resolved).await {
                                    GitIdentity::Worktree => String::new(),
                                    identity => {
                                        let key =
                                            self.directory_identity(&resolved, Some(stats)).await;
                                        let git = match identity {
                                            GitIdentity::Repository(git) => Some(git),
                                            _ => None,
                                        };
                                        git_identities.insert(key.clone(), git);
                                        key
                                    }
                                }
                            }
                        }
                        // Directories that no longer exist can't be imported.
                        _ => String::new(),
                    };
                    directory_keys.insert(resolved.clone(), key.clone());
                    key
                }
            };
            if key.is_empty() {
                continue;
            }

            match merged.get_mut(&key) {
                None => {
                    merged.insert(
                        key.clone(),
                        MergedCandidate {
                            path: resolved,
                            sources: vec![candidate.source],
                            thread_count: candidate.thread_count,
                            last_active_at_ms: candidate.last_active_at_ms,
                            git: git_identities.get(&key).cloned().flatten(),
                        },
                    );
                }
                Some(existing) => {
                    if !existing.sources.contains(&candidate.source) {
                        existing.sources.push(candidate.source);
                    }
                    existing.thread_count += candidate.thread_count;
                    existing.last_active_at_ms =
                        existing.last_active_at_ms.max(candidate.last_active_at_ms);
                }
            }
        }

        // Resolve persisted roots too. A project and a transcript can name different
        // symlinks to the same directory.
        let shells = self
            .projects
            .list_shells(None)
            .await
            .map_err(|cause| scan_error(AgentSessionScanOperation::ReadProjects, cause))?;
        let mut imported_by_root: HashMap<String, OrchestrationProjectShell> = HashMap::new();
        for shell in shells {
            let project_root =
                lexical_resolve(&expand_home_path(shell.workspace_root.as_str(), home));
            imported_by_root.insert(
                normalize_project_path_for_comparison(&project_root),
                shell.clone(),
            );
            imported_by_root.insert(self.directory_identity(&project_root, None).await, shell);
        }

        let mut candidates: Vec<AgentSessionProjectCandidate> = Vec::new();
        for (key, entry) in merged.iter() {
            let imported = imported_by_root
                .get(&normalize_project_path_for_comparison(&entry.path))
                .or_else(|| imported_by_root.get(key));
            // The persisted root wins: it is the path the project was created with.
            let candidate_path = imported
                .map(|shell| shell.workspace_root.as_str().to_owned())
                .unwrap_or_else(|| entry.path.clone());
            let Ok(path) = TrimmedNonEmptyString::new(&candidate_path) else {
                continue;
            };
            let title = Path::new(&candidate_path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| candidate_path.clone());
            let Ok(title) = TrimmedNonEmptyString::new(&title) else {
                continue;
            };
            candidates.push(AgentSessionProjectCandidate {
                path,
                title,
                project_id: imported.map(|shell| shell.id.clone()),
                sources: entry.sources.clone(),
                thread_count: entry.thread_count,
                last_active_at: iso_from_millis(entry.last_active_at_ms),
                already_imported: imported.is_some(),
                git: Some(entry.git.clone()),
            });
        }

        // Newest first, undated candidates last. Ties fall back to path order.
        candidates.sort_by(
            |left, right| match (&left.last_active_at, &right.last_active_at) {
                (left_at, right_at) if left_at == right_at => {
                    left.path.as_str().cmp(right.path.as_str())
                }
                (None, _) => Ordering::Greater,
                (_, None) => Ordering::Less,
                (Some(left_at), Some(right_at)) => right_at.cmp(left_at),
            },
        );

        Ok(AgentSessionScanResult {
            candidates,
            scanned_at: iso_from_millis((self.now_ms)())
                .unwrap_or_else(|| UtcDateTime::now().to_string()),
            truncated: truncated.then_some(true),
        })
    }

    /// Upstream `prepareRecentThreads`: the eligible transcripts of one workspace,
    /// newest first, with the import budgets. Their bodies are read lazily.
    async fn prepare_recent_threads(
        &self,
        workspace_root: &str,
        completed_sources: &[AgentSessionImportSource],
    ) -> Result<RecentState, AgentSessionScanError> {
        let home = self.host.home_dir.as_str();
        let root = lexical_resolve(&expand_home_path(workspace_root, home));
        let real_root = self
            .fs
            .real_path(&root)
            .await
            .unwrap_or_else(|| root.clone());
        if self.is_excluded_project_path(&root) || self.is_excluded_project_path(&real_root) {
            return Ok(RecentState::empty());
        }
        let root_identity = self.directory_identity(&root, None).await;
        let now_ms = (self.now_ms)();
        let cutoff_ms = now_ms - RECENT_THREAD_WINDOW_MS;

        let candidates = self.recent_candidates().await?;

        let mut eligible_transcripts: Vec<EligibleTranscript> = Vec::new();
        for candidate in candidates.iter() {
            let expanded = expand_home_path(js_trim(&candidate.cwd), home);
            if !Path::new(&expanded).is_absolute() {
                continue;
            }
            let resolved = lexical_resolve(&expanded);
            if self.directory_identity(&resolved, None).await != root_identity {
                continue;
            }
            for transcript in &candidate.transcripts {
                if transcript.mtime_ms < cutoff_ms || transcript.mtime_ms > now_ms {
                    continue;
                }
                eligible_transcripts.push(EligibleTranscript {
                    source: candidate.source,
                    provider_instance_id: candidate.provider_instance_id.clone(),
                    file_path: transcript.file_path.clone(),
                    mtime_ms: transcript.mtime_ms,
                });
            }
        }
        eligible_transcripts.sort_by(|left, right| {
            right
                .mtime_ms
                .partial_cmp(&left.mtime_ms)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.file_path.cmp(&right.file_path))
        });

        let mut completed: HashMap<String, Vec<AgentSessionImportSource>> = HashMap::new();
        for source in completed_sources {
            completed
                .entry(format!(
                    "{}\0{}",
                    source.provider_instance_id.as_str(),
                    source.file_path.as_str()
                ))
                .or_default()
                .push(source.clone());
        }

        Ok(RecentState {
            eligible: eligible_transcripts.into(),
            completed,
            imported_sessions: HashSet::new(),
            bytes_remaining: MAX_IMPORT_BYTES,
            transcripts_remaining: MAX_IMPORT_TRANSCRIPTS,
            records_remaining: MAX_IMPORT_RECORDS,
            root_identity,
        })
    }

    /// The cached candidates from `scan`, or a fresh collection.
    async fn recent_candidates(&self) -> Result<Arc<Vec<RawCandidate>>, AgentSessionScanError> {
        let cached = self.cached_candidates.lock().clone();
        if let Some(candidates) = cached {
            return Ok(candidates);
        }
        let (raw, _) = self.collect_candidates().await?;
        let raw = Arc::new(raw);
        *self.cached_candidates.lock() = Some(Arc::clone(&raw));
        Ok(raw)
    }

    /// One eligible transcript, under the import lock. `None` means the item is
    /// dropped without being reported (an already-imported session seen twice).
    async fn step_recent_thread(
        &self,
        state: &mut RecentState,
        item: EligibleTranscript,
    ) -> Option<AgentSessionRecentThread> {
        let _permit = self
            .import_read_lock
            .acquire()
            .await
            .expect("import read lock is never closed");
        let completed_key = format!("{}\0{}", item.provider_instance_id.as_str(), item.file_path);
        let has_completed = state.completed.contains_key(&completed_key);
        if !has_completed
            && (state.transcripts_remaining == 0
                || state.bytes_remaining == 0
                || state.records_remaining == 0)
        {
            return Some(AgentSessionRecentThread::Skipped);
        }
        let Some(stats) = self
            .fs
            .stat(&item.file_path)
            .await
            .filter(|stats| stats.is_file)
        else {
            return Some(AgentSessionRecentThread::Skipped);
        };
        let identity = transcript_identity(&item.file_path, &stats);
        let completed_source = state
            .completed
            .get(&completed_key)
            .and_then(|sources| {
                sources.iter().find(|source| {
                    source.provider == item.source && same_source_identity(source, &identity)
                })
            })
            .cloned();
        if let Some(source) = completed_source {
            let session_key = format!(
                "{}\0{}",
                source.provider_instance_id.as_str(),
                source.provider_session_id.as_str()
            );
            if !state.imported_sessions.insert(session_key) {
                return None;
            }
            return Some(AgentSessionRecentThread::AlreadyImported { source });
        }
        if state.transcripts_remaining == 0
            || state.records_remaining == 0
            || identity.size > MAX_IMPORTED_TRANSCRIPT_BYTES
            || identity.size > state.bytes_remaining
        {
            return Some(AgentSessionRecentThread::Skipped);
        }
        // Reserve the whole file even if its read or parse fails.
        state.transcripts_remaining -= 1;
        state.bytes_remaining -= identity.size;
        let Some(snapshot) = self
            .read_transcript(
                &item.file_path,
                &identity,
                state.records_remaining,
                item.source,
            )
            .await
        else {
            return Some(AgentSessionRecentThread::Skipped);
        };
        state.records_remaining -= snapshot.record_count;

        // A stable replacement file can belong to a different project than the cached candidate.
        let Some(snapshot_cwd) = snapshot.records.iter().find_map(extract_decoded_cwd) else {
            return Some(AgentSessionRecentThread::Skipped);
        };
        let expanded_cwd = expand_home_path(&snapshot_cwd, &self.host.home_dir);
        if !Path::new(&expanded_cwd).is_absolute()
            || self
                .directory_identity(&lexical_resolve(&expanded_cwd), None)
                .await
                != state.root_identity
        {
            return Some(AgentSessionRecentThread::Skipped);
        }

        let fallback_session_id = Path::new(&item.file_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fallback_session_id = fallback_session_id
            .strip_suffix(".jsonl")
            .unwrap_or(&fallback_session_id)
            .to_owned();
        let metadata = AgentSessionTranscriptMetadata {
            source: item.source,
            provider_instance_id: item.provider_instance_id.clone(),
            fallback_session_id: &fallback_session_id,
            last_active_at_ms: item.mtime_ms as i64,
        };
        let Some(parsed) = parse_records(&metadata, &snapshot.records) else {
            return Some(AgentSessionRecentThread::Skipped);
        };
        let source = match import_source(&identity, &parsed) {
            Some(source) => source,
            None => return Some(AgentSessionRecentThread::Skipped),
        };
        let session_key = format!(
            "{}\0{}",
            parsed.provider_instance_id.as_str(),
            parsed.provider_session_id
        );
        if !state.imported_sessions.insert(session_key) {
            return Some(AgentSessionRecentThread::Duplicate { source });
        }
        Some(AgentSessionRecentThread::Importable {
            thread: parsed,
            source,
        })
    }

    fn stream_recent_threads(
        &self,
        state: RecentState,
    ) -> BoxStream<'_, Result<AgentSessionRecentThread, AgentSessionScanError>> {
        stream::unfold(state, move |mut state| async move {
            loop {
                let item = state.eligible.pop_front()?;
                if let Some(outcome) = self.step_recent_thread(&mut state, item).await {
                    return Some((Ok(outcome), state));
                }
            }
        })
        .boxed()
    }
}

#[async_trait]
impl AgentSessionScanner for AgentSessionDiscovery {
    async fn scan(&self) -> Result<AgentSessionScanResult, AgentSessionScanError> {
        self.scan_candidates().await
    }

    fn recent_threads<'a>(
        &'a self,
        workspace_root: &'a str,
        completed_sources: &'a [AgentSessionImportSource],
    ) -> BoxStream<'a, Result<AgentSessionRecentThread, AgentSessionScanError>> {
        stream::once(async move {
            self.prepare_recent_threads(workspace_root, completed_sources)
                .await
        })
        .flat_map(move |prepared| match prepared {
            Ok(state) => self.stream_recent_threads(state),
            Err(error) => stream::once(async move { Err(error) }).boxed(),
        })
        .boxed()
    }
}

impl TranscriptSelection {
    fn new(history: Arc<HistoryBudget>, record_limit: usize, source: AgentSessionSource) -> Self {
        Self {
            reader: new_record_reader(Arc::clone(&history)),
            history,
            pending: Vec::new(),
            record_started: false,
            record_count: 0,
            record_limit,
            records: Vec::new(),
            source,
        }
    }

    /// Feed one line's bytes (no newline). A UTF-8 sequence split across chunks is
    /// held until its rest arrives.
    fn write_segment(&mut self, bytes: &[u8]) -> Result<(), TranscriptJsonLimitError> {
        self.record_started = true;
        let text = decode_utf8_prefix(&mut self.pending, bytes);
        self.reader.write(&text)
    }

    /// Upstream `finishRecord`. `Ok(false)` once the record limit is exceeded.
    fn finish_record(&mut self) -> Result<bool, TranscriptJsonLimitError> {
        let tail = flush_utf8(&mut self.pending);
        self.reader.write(&tail)?;
        self.record_count += 1;
        if self.record_count > self.record_limit {
            return Ok(false);
        }
        let reader = std::mem::replace(
            &mut self.reader,
            new_record_reader(Arc::clone(&self.history)),
        );
        let decoded = reader
            .finish()?
            .and_then(|value| serde_json::from_value::<TranscriptRecord>(value).ok());
        let record_bytes = self.history.record_bytes.swap(0, AtomicOrdering::Relaxed);
        if let Some(record) = decoded
            && should_retain_decoded_record(self.source, &record)
        {
            self.history
                .history_bytes
                .fetch_add(record_bytes, AtomicOrdering::Relaxed);
            self.records.push(record);
        }
        self.record_started = false;
        Ok(true)
    }
}

fn new_record_reader(history: Arc<HistoryBudget>) -> TranscriptJsonReader<'static> {
    let reserve = move |bytes: usize| -> Result<(), TranscriptJsonLimitError> {
        let record_bytes = history
            .record_bytes
            .fetch_add(bytes, AtomicOrdering::Relaxed)
            + bytes;
        if history.history_bytes.load(AtomicOrdering::Relaxed) + record_bytes
            > MAX_IMPORT_HISTORY_BYTES
        {
            return Err(TranscriptJsonLimitError(
                "Transcript selected history exceeds the 32 MiB memory budget".to_owned(),
            ));
        }
        Ok(())
    };
    TranscriptJsonReader::new(
        reserve,
        create_transcript_json_selector(transcript_record_schema()),
    )
}

/// The encoded shape of `TranscriptRecord`: the paths a projection keeps.
fn transcript_record_schema() -> JsonSelectSchema {
    use JsonSelectSchema::{Any, Array, Object, Scalar, Union};
    let block = || Object(vec![("type", Scalar), ("text", Scalar)]);
    let message = Object(vec![
        ("role", Scalar),
        ("content", Union(vec![Scalar, Array(Box::new(block()))])),
        ("model", Scalar),
    ]);
    let payload = Object(vec![
        ("id", Scalar),
        ("session_id", Scalar),
        ("type", Scalar),
        ("role", Scalar),
        ("message", Scalar),
        ("model", Scalar),
        ("cwd", Scalar),
        ("content", Array(Box::new(block()))),
        ("internal_chat_message_metadata_passthrough", Any),
    ]);
    Object(vec![
        ("type", Scalar),
        ("timestamp", Scalar),
        ("cwd", Scalar),
        ("sessionId", Scalar),
        ("aiTitle", Scalar),
        ("isSidechain", Scalar),
        ("isMeta", Scalar),
        ("isCompactSummary", Scalar),
        ("message", message),
        ("payload", payload),
    ])
}

fn should_retain_decoded_record(source: AgentSessionSource, record: &TranscriptRecord) -> bool {
    if extract_decoded_cwd(record).is_some() {
        return true;
    }
    let kind = record.kind.as_deref();
    let payload_kind = record
        .payload
        .as_ref()
        .and_then(|payload| payload.kind.as_deref());
    match source {
        AgentSessionSource::ClaudeAgent => {
            matches!(kind, Some("user" | "assistant"))
                || record.session_id.is_some()
                || record.ai_title.is_some()
                || record
                    .message
                    .as_ref()
                    .is_some_and(|message| message.model.is_some())
        }
        AgentSessionSource::Codex => {
            kind == Some("session_meta")
                || kind == Some("turn_context")
                || (kind == Some("event_msg") && payload_kind == Some("user_message"))
                || (kind == Some("response_item")
                    && payload_kind == Some("message")
                    && matches!(
                        record.payload.as_ref().and_then(|p| p.role.as_deref()),
                        Some("user" | "assistant")
                    ))
        }
    }
}

/// Upstream `extractDecodedCwd`: the record's `cwd`, else the payload's, trimmed.
fn extract_decoded_cwd(record: &TranscriptRecord) -> Option<String> {
    record
        .cwd
        .as_deref()
        .map(js_trim)
        .filter(|cwd| !cwd.is_empty())
        .or_else(|| {
            record
                .payload
                .as_ref()
                .and_then(|payload| payload.cwd.as_deref())
                .map(js_trim)
                .filter(|cwd| !cwd.is_empty())
        })
        .map(str::to_owned)
}

/// Upstream `extractCwd`: `cwd` on a metadata line, tolerating the shape each CLI writes.
fn extract_cwd(line: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    let record = parsed.as_object()?;
    if let Some(cwd) = record
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !js_trim(cwd).is_empty())
    {
        return Some(cwd.to_owned());
    }
    // Codex nests session metadata under `payload`.
    let nested = record.get("payload")?.as_object()?.get("cwd")?.as_str()?;
    (!js_trim(nested).is_empty()).then(|| nested.to_owned())
}

/// Upstream `readCwd`'s end-of-file branch: the unterminated last line.
fn last_record_cwd(
    remaining: &[u8],
    budget: &mut MetadataReadBudget,
    records_read: &mut usize,
) -> Option<String> {
    let record = String::from_utf8_lossy(remaining);
    if record.is_empty() || !reserve_metadata_record(budget, records_read) {
        return None;
    }
    extract_cwd(js_trim(&record))
}

fn reserve_metadata_record(budget: &mut MetadataReadBudget, records_read: &mut usize) -> bool {
    if *records_read == MAX_METADATA_RECORDS_PER_TRANSCRIPT || budget.records_remaining == 0 {
        budget.truncated = true;
        return false;
    }
    *records_read += 1;
    budget.records_remaining -= 1;
    true
}

/// Upstream `selectMetadataTranscripts`: round-robin across instances so one
/// account with many files cannot starve another.
fn select_metadata_transcripts(transcripts: &[TranscriptCandidate]) -> Vec<TranscriptCandidate> {
    let mut groups: IndexMap<&str, Vec<&TranscriptCandidate>> = IndexMap::new();
    for transcript in transcripts {
        groups
            .entry(transcript.provider_instance_id.as_str())
            .or_default()
            .push(transcript);
    }
    let mut pending: Vec<std::vec::IntoIter<&TranscriptCandidate>> = groups
        .into_values()
        .map(|entries| entries.into_iter())
        .collect();
    let mut selected: Vec<TranscriptCandidate> = Vec::new();
    while !pending.is_empty() && selected.len() < MAX_TRANSCRIPTS_PER_SOURCE {
        let mut next_round = Vec::new();
        for mut iterator in pending {
            if selected.len() == MAX_TRANSCRIPTS_PER_SOURCE {
                break;
            }
            if let Some(next) = iterator.next() {
                selected.push(next.clone());
                next_round.push(iterator);
            }
        }
        pending = next_round;
    }
    selected
}

/// Upstream `isT3ManagedWorktree`: this server's worktrees dir, or the conventional
/// `.t3/worktrees` layout (catches sandboxes from other T3 homes too).
fn is_t3_managed_worktree(candidate: &str, worktrees_dir: &str, case_fold: bool) -> bool {
    let normalized = normalize_for_worktree_match(candidate, case_fold);
    normalized.starts_with(&normalize_for_worktree_match(worktrees_dir, case_fold))
        || normalized.contains("/.t3/worktrees/")
}

/// Upstream `normalizeForWorktreeMatch`: separators unified, trailing slash added
/// so prefix checks match whole path segments.
fn normalize_for_worktree_match(value: &str, case_fold: bool) -> String {
    let normalized = format!("{}/", value.replace('\\', "/"));
    if case_fold {
        normalized.to_lowercase()
    } else {
        normalized
    }
}

/// Upstream `resolveClaudeConfigDir`: the instance's `homePath`, then a
/// `CLAUDE_CONFIG_DIR` already in the environment, then `~/.claude`.
fn resolve_claude_config_dir(
    home_path: &str,
    environment_home: Option<&str>,
    home: &str,
) -> String {
    let configured = js_trim(home_path);
    if !configured.is_empty() {
        return lexical_resolve(&expand_home_path(configured, home));
    }
    let from_environment = environment_home.map(js_trim).unwrap_or("");
    if !from_environment.is_empty() {
        return lexical_resolve(&expand_home_path(from_environment, home));
    }
    join(&[home, ".claude"])
}

fn driver_slug(source: AgentSessionSource) -> &'static str {
    match source {
        AgentSessionSource::ClaudeAgent => "claudeAgent",
        AgentSessionSource::Codex => "codex",
    }
}

/// The built-in instance when settings have no entry for it: `{ driver }`, no config.
fn default_instance_config(driver: &str) -> ProviderInstanceConfig {
    ProviderInstanceConfig {
        driver: ProviderDriverKind::new(driver).expect("built-in driver slug is valid"),
        display_name: None,
        accent_color: None,
        environment: None,
        enabled: None,
        config: None,
    }
}

/// Decode an instance's `config` payload. A missing or `null` config is `{}`, as upstream.
fn decode_config<T: DeserializeOwned>(instance: &ProviderInstanceConfig) -> Option<T> {
    let value = match &instance.config {
        None | Some(Value::Null) => json!({}),
        Some(value) => value.clone(),
    };
    serde_json::from_value(value).ok()
}

fn import_source(
    identity: &TranscriptIdentity,
    thread: &AgentSessionThread,
) -> Option<AgentSessionImportSource> {
    Some(AgentSessionImportSource {
        provider: thread.source,
        provider_instance_id: thread.provider_instance_id.clone(),
        provider_session_id: TrimmedNonEmptyString::new(&thread.provider_session_id).ok()?,
        file_path: TrimmedNonEmptyString::new(&identity.file_path).ok()?,
        size: identity.size,
        mtime_ms: identity.mtime_ms,
        device: identity.device,
        inode: identity.inode,
        birthtime_ms: identity.birthtime_ms,
    })
}

/// Decode UTF-8 for one line segment. Incomplete trailing bytes wait in `pending`.
fn decode_utf8_prefix(pending: &mut Vec<u8>, incoming: &[u8]) -> String {
    pending.extend_from_slice(incoming);
    let mut out = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                out.push_str(text);
                pending.clear();
                return out;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                out.push_str(std::str::from_utf8(&pending[..valid]).unwrap_or_default());
                match error.error_len() {
                    Some(invalid) => {
                        out.push('\u{FFFD}');
                        pending.drain(..valid + invalid);
                    }
                    None => {
                        pending.drain(..valid);
                        return out;
                    }
                }
            }
        }
    }
}

fn flush_utf8(pending: &mut Vec<u8>) -> String {
    let text = String::from_utf8_lossy(pending).into_owned();
    pending.clear();
    text
}

fn iso_from_millis(millis: f64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
        .map(|value| UtcDateTime::from_datetime(value).to_string())
}

fn scan_error(
    operation: AgentSessionScanOperation,
    cause: impl std::fmt::Display,
) -> AgentSessionScanError {
    AgentSessionScanError::ScanError {
        operation,
        cause: Value::String(cause.to_string()),
    }
}

/// Node `path.join` for the path shapes discovery builds.
fn join(parts: &[&str]) -> String {
    parts
        .iter()
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
