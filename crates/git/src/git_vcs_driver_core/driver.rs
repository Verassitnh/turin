//! Git command methods for [`super::GitVcsDriverCore`].
//!
//! Arguments, `LC_ALL=C`, timeouts, and error details follow
//! `upstream/apps/server/src/vcs/GitVcsDriverCore.ts`. Stderr stays out of
//! `GitCommandError.detail`.

#![allow(clippy::result_large_err, clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use turin_contracts::base_schemas::{TrimmedNonEmptyString, UtcDateTime};
use turin_contracts::environment::WorktreeSubmodules;
use turin_contracts::git::{
    GitCommandError, GitFilePath, VcsCreateRefInput, VcsCreateRefResult, VcsCreateWorktreeInput,
    VcsCreateWorktreeResult, VcsInitInput, VcsListRefsInput, VcsListRefsRefKind, VcsListRefsResult,
    VcsPullResult, VcsPullStatus, VcsRef, VcsRemoveWorktreeInput, VcsStatusBranchChanges,
    VcsStatusInput, VcsStatusResult, VcsStatusWorkingTree, VcsStatusWorkingTreeFile,
    VcsSwitchRefInput, VcsSwitchRefResult, VcsWorktree,
};
use turin_contracts::review::{
    ReviewDiffFileChangeType, ReviewDiffFileContentsInput, ReviewDiffFileContentsResult,
    ReviewDiffFileStat, ReviewDiffPreviewInput, ReviewDiffPreviewResult, ReviewDiffPreviewSource,
    ReviewDiffPreviewSourceKind,
};

use super::{
    CheckoutProgress, ExecuteGitInput, ExecuteGitResult, GitLineCallback, GitVcsDriverCore,
    OUTPUT_TRUNCATED_MARKER, classify_git_failure, fetch_failure_detail,
    filter_branches_for_list_query, git_error, is_missing_worktree_stderr,
    is_non_repository_git_stderr, is_ref_lock_failure, is_unborn_head_stderr, paginate_branches,
    parse_branch_ab, parse_default_branch_from_remote_head_ref, parse_git_checkout_progress_line,
    parse_numstat_entries, parse_porcelain_path, parse_remote_fetch_urls, parse_review_numstat,
    parse_tracking_branch_by_upstream_ref, parse_upstream_ref_by_first_separator,
    parse_upstream_ref_with_remote_names, parse_worktree_branch_paths, sanitize_remote_name,
    split_null_separated_paths,
};
use crate::remote_refs::{
    parse_remote_names, parse_remote_names_in_git_order, parse_remote_ref_with_remote_names,
};
use crate::shared_git::{dedupe_remote_branches_with_local_matches, normalize_git_remote_url};

pub const PATCH_RENDER_PREFIX_ARGS: [&str; 2] = ["--src-prefix=a/", "--dst-prefix=b/"];

const REVIEW_DIFF_PATCH_MAX: usize = 120_000;
const REVIEW_METADATA_MAX: usize = 16 * 1024 * 1024;
const REVIEW_UNTRACKED_MAX_FILES: usize = 5_000;
const REVIEW_DIFF_FILE_MAX: usize = 1024 * 1024;
const PREPARED_COMMIT_PATCH_MAX: usize = 49_000;
const RANGE_COMMIT_SUMMARY_MAX: usize = 19_000;
const RANGE_DIFF_SUMMARY_MAX: usize = 19_000;
const RANGE_DIFF_PATCH_MAX: usize = 59_000;
const PATHS_CAPACITY: usize = 2_048;
const REFS_CAPACITY: usize = 64;
const STATUS_REFRESH_CAPACITY: usize = 2_048;

pub fn review_diff_args() -> Vec<String> {
    [
        "diff",
        "--find-renames",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--minimal",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(PATCH_RENDER_PREFIX_ARGS.into_iter().map(str::to_owned))
    .collect()
}

#[derive(Debug, Clone)]
pub struct GitStatusDetails {
    pub is_repo: bool,
    pub has_origin_remote: bool,
    pub is_default_branch: bool,
    pub branch: Option<String>,
    pub upstream_ref: Option<String>,
    pub has_working_tree_changes: bool,
    pub working_tree: VcsStatusWorkingTree,
    pub branch_changes: Option<VcsStatusBranchChanges>,
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    pub ahead_of_default_count: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GitLocalStatusOptions {
    /// `None` includes divergence. `Some(false)` skips revision walks.
    pub include_divergence: Option<bool>,
    pub include_branch_changes: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct GitRemoteStatusDetails {
    pub is_repo: bool,
    pub default_branch: Option<String>,
    pub is_default_branch: bool,
    pub branch: Option<String>,
    pub upstream_ref: Option<String>,
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    pub ahead_of_default_count: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GitRemoteStatusOptions {
    /// `None` refreshes. `Some(false)` keeps the last fetched refs.
    pub refresh_upstream: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPreparedCommitContext {
    pub staged_summary: String,
    pub staged_patch: String,
}

#[derive(Clone, Default)]
pub struct GitCommitOptions {
    pub timeout: Option<Duration>,
    /// `Some` stages before commit. `Some(empty)` stages everything.
    pub stage: Option<Vec<String>>,
    pub on_output_line: Option<GitLineCallback>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommitResult {
    pub commit_sha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPushStatus {
    Pushed,
    SkippedUpToDate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPushResult {
    pub status: GitPushStatus,
    pub branch: String,
    pub upstream_branch: Option<String>,
    pub set_upstream: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct GitPushOptions {
    pub remote_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRangeContext {
    pub commit_summary: String,
    pub diff_summary: String,
    pub diff_patch: String,
}

pub type WorktreeClaimedCallback = Arc<dyn Fn(&str) + Send + Sync>;
pub type SubmodulesDisabledCallback = Arc<dyn Fn(&str) + Send + Sync>;
pub type SubmodulesFinishedCallback = Arc<dyn Fn(bool, Option<String>) + Send + Sync>;

#[derive(Clone, Default)]
pub struct CreateWorktreeOptions {
    pub worktrees_directory: Option<String>,
    pub submodules: Option<WorktreeSubmodules>,
    pub on_checkout_progress: Option<Arc<dyn Fn(CheckoutProgress) + Send + Sync>>,
    pub on_worktree_claimed: Option<WorktreeClaimedCallback>,
    pub on_submodules_started: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_submodules_disabled: Option<SubmodulesDisabledCallback>,
    pub on_submodule_line: Option<GitLineCallback>,
    pub on_submodules_finished: Option<SubmodulesFinishedCallback>,
}

#[derive(Debug, Clone)]
pub struct GitFetchPullRequestBranchInput {
    pub cwd: String,
    pub pr_number: u64,
    pub branch: String,
}

#[derive(Debug, Clone)]
pub struct GitFetchPullRequestHeadCommitInput {
    pub cwd: String,
    pub pr_number: u64,
}

#[derive(Debug, Clone)]
pub struct GitResolveCommitInput {
    pub cwd: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitResolveCommitResult {
    pub commit_sha: String,
}

#[derive(Debug, Clone)]
pub struct GitRefreshCheckedOutBranchInput {
    pub cwd: String,
    pub target_commit: String,
    pub reset_when_head_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRefreshCheckedOutBranchResult {
    pub head_commit: String,
    pub moved: bool,
    pub on_target: bool,
}

#[derive(Debug, Clone)]
pub struct GitEnsureRemoteInput {
    pub cwd: String,
    pub preferred_name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct GitFetchRemoteBranchInput {
    pub cwd: String,
    pub remote_name: String,
    pub remote_branch: String,
    pub local_branch: String,
}

#[derive(Debug, Clone)]
pub struct GitFetchRemoteTrackingBranchInput {
    pub cwd: String,
    pub remote_name: String,
    pub remote_branch: String,
}

#[derive(Debug, Clone)]
pub struct GitFetchRemoteInput {
    pub cwd: String,
    pub remote_name: String,
    pub ref_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitRemoteExistsInput {
    pub cwd: String,
    pub remote_name: String,
}

#[derive(Debug, Clone)]
pub struct GitRemoteBranchExistsInput {
    pub cwd: String,
    pub remote_name: String,
    pub ref_name: String,
}

#[derive(Debug, Clone)]
pub struct GitResolveRemoteTrackingCommitInput {
    pub cwd: String,
    pub ref_name: String,
    pub fallback_remote_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitResolveRemoteTrackingCommitResult {
    pub commit_sha: String,
    pub remote_ref_name: String,
}

#[derive(Debug, Clone)]
pub struct GitSetBranchUpstreamInput {
    pub cwd: String,
    pub branch: String,
    pub remote_name: String,
    pub remote_branch: String,
}

#[derive(Debug, Clone)]
pub struct GitDeleteLocalBranchInput {
    pub cwd: String,
    pub ref_name: String,
    pub force: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct GitRenameBranchInput {
    pub exact_name: bool,
    pub cwd: String,
    pub old_branch: String,
    pub new_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRenameBranchResult {
    pub branch: String,
}

#[derive(Clone)]
struct RepoPaths {
    git_common_dir: String,
    worktree_root: Option<String>,
    current_branch: Option<String>,
}

#[derive(Clone)]
struct RefsSnapshot {
    local_branches: Vec<VcsRef>,
    remote_branches: Vec<VcsRef>,
    has_primary_remote: bool,
}

struct Timed<T> {
    value: T,
    expires: Instant,
}

struct TtlCache<T> {
    capacity: usize,
    entries: HashMap<String, Timed<T>>,
    order: VecDeque<String>,
}

impl<T: Clone> TtlCache<T> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<T> {
        let expired = self
            .entries
            .get(key)
            .is_some_and(|entry| Instant::now() >= entry.expires);
        if expired {
            self.invalidate(key);
            return None;
        }
        self.entries.get(key).map(|entry| entry.value.clone())
    }

    fn insert(&mut self, key: String, value: T, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }
        if !self.entries.contains_key(&key) {
            while self.entries.len() >= self.capacity {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                self.entries.remove(&oldest);
            }
        }
        self.order.retain(|item| item != &key);
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            Timed {
                value,
                expires: Instant::now() + ttl,
            },
        );
    }

    fn invalidate(&mut self, key: &str) {
        self.entries.remove(key);
        self.order.retain(|item| item != key);
    }
}

struct StatusRefresh {
    entries: TtlCache<()>,
    failures: VecDeque<(String, u32)>,
}

impl StatusRefresh {
    fn new() -> Self {
        Self {
            entries: TtlCache::new(STATUS_REFRESH_CAPACITY),
            failures: VecDeque::new(),
        }
    }

    fn fresh(&mut self, key: &str) -> bool {
        self.entries.get(key).is_some()
    }

    fn succeed(&mut self, key: &str) {
        self.failures.retain(|(name, _)| name != key);
        self.entries
            .insert(key.to_owned(), (), Duration::from_secs(15));
    }

    fn fail(&mut self, key: &str) {
        let count = self
            .failures
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, count)| *count)
            .unwrap_or(0)
            .saturating_add(1);
        self.failures.retain(|(name, _)| name != key);
        self.failures.push_back((key.to_owned(), count));
        while self.failures.len() > STATUS_REFRESH_CAPACITY {
            self.failures.pop_front();
        }
        let exponent = count.saturating_sub(1).min(16);
        let millis = 30_000_u64.saturating_mul(1_u64 << exponent);
        let ttl = Duration::from_millis(millis.min(15 * 60 * 1000));
        self.entries.insert(key.to_owned(), (), ttl);
    }
}

struct RefsCache {
    epochs: VecDeque<(String, u64)>,
    generations: VecDeque<(String, u64)>,
    epoch_seq: u64,
    generation_seq: u64,
    snapshots: TtlCache<RefsSnapshot>,
    refresh: TtlCache<RefsSnapshot>,
}

impl RefsCache {
    fn new() -> Self {
        Self {
            epochs: VecDeque::new(),
            generations: VecDeque::new(),
            epoch_seq: 0,
            generation_seq: 0,
            snapshots: TtlCache::new(REFS_CAPACITY),
            refresh: TtlCache::new(REFS_CAPACITY),
        }
    }

    fn remember(order: &mut VecDeque<(String, u64)>, key: &str, value: u64) {
        order.retain(|(name, _)| name != key);
        order.push_back((key.to_owned(), value));
        while order.len() > REFS_CAPACITY {
            order.pop_front();
        }
    }

    fn generation(&mut self, common: &str) -> u64 {
        if let Some((_, value)) = self.generations.iter().find(|(name, _)| name == common) {
            let value = *value;
            Self::remember(&mut self.generations, common, value);
            return value;
        }
        self.generation_seq += 1;
        let value = self.generation_seq;
        Self::remember(&mut self.generations, common, value);
        value
    }

    fn bump_generation(&mut self, common: &str) -> u64 {
        self.generation_seq += 1;
        let value = self.generation_seq;
        Self::remember(&mut self.generations, common, value);
        value
    }

    fn epoch(&self, common: &str) -> Option<u64> {
        self.epochs
            .iter()
            .find(|(name, _)| name == common)
            .map(|(_, value)| *value)
    }

    fn bump_epoch(&mut self, common: &str) -> u64 {
        self.epoch_seq += 1;
        let value = self.epoch_seq;
        Self::remember(&mut self.epochs, common, value);
        value
    }
}

pub(super) struct DriverState {
    paths: Mutex<TtlCache<Option<RepoPaths>>>,
    paths_refresh: Mutex<TtlCache<Option<RepoPaths>>>,
    default_branch: Mutex<TtlCache<Option<String>>>,
    origin_exists: Mutex<TtlCache<bool>>,
    status_refresh: Mutex<StatusRefresh>,
    refs: Mutex<RefsCache>,
}

impl DriverState {
    pub(super) fn new() -> Self {
        Self {
            paths: Mutex::new(TtlCache::new(PATHS_CAPACITY)),
            paths_refresh: Mutex::new(TtlCache::new(PATHS_CAPACITY)),
            default_branch: Mutex::new(TtlCache::new(PATHS_CAPACITY)),
            origin_exists: Mutex::new(TtlCache::new(PATHS_CAPACITY)),
            status_refresh: Mutex::new(StatusRefresh::new()),
            refs: Mutex::new(RefsCache::new()),
        }
    }
}

struct GitCall {
    operation: &'static str,
    args: Vec<String>,
    allow_non_zero: bool,
    timeout: Option<Option<Duration>>,
    max_output: Option<usize>,
    marker: bool,
    stable: bool,
    env: HashMap<String, String>,
    stdin: Option<String>,
    fallback: Option<&'static str>,
    on_stdout_line: Option<GitLineCallback>,
    on_stderr_line: Option<GitLineCallback>,
}

impl GitCall {
    fn new(operation: &'static str, args: Vec<String>) -> Self {
        Self {
            operation,
            args,
            allow_non_zero: false,
            timeout: None,
            max_output: None,
            marker: false,
            stable: false,
            env: HashMap::new(),
            stdin: None,
            fallback: None,
            on_stdout_line: None,
            on_stderr_line: None,
        }
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().copied().map(str::to_owned).collect()
}

fn non_repository_status() -> GitStatusDetails {
    GitStatusDetails {
        is_repo: false,
        has_origin_remote: false,
        is_default_branch: false,
        branch: None,
        upstream_ref: None,
        has_working_tree_changes: false,
        working_tree: VcsStatusWorkingTree {
            files: Vec::new(),
            insertions: 0,
            deletions: 0,
        },
        branch_changes: None,
        has_upstream: false,
        ahead_count: 0,
        behind_count: 0,
        ahead_of_default_count: 0,
    }
}

fn non_repository_remote() -> GitRemoteStatusDetails {
    GitRemoteStatusDetails {
        is_repo: false,
        default_branch: None,
        is_default_branch: false,
        branch: None,
        upstream_ref: None,
        has_upstream: false,
        ahead_count: 0,
        behind_count: 0,
        ahead_of_default_count: 0,
    }
}

fn failed(
    operation: &str,
    cwd: &str,
    args: &[String],
    result: &ExecuteGitResult,
    detail: &str,
) -> GitCommandError {
    GitCommandError::GitCommandError {
        operation: operation.to_owned(),
        command: "git".to_owned(),
        cwd: cwd.to_owned(),
        argument_count: Some(args.len() as f64),
        exit_code: result.exit_code.map(|code| code as f64),
        stdout_length: Some(result.stdout.chars().count() as f64),
        stderr_length: Some(result.stderr.chars().count() as f64),
        output_length: None,
        reason: classify_git_failure(&result.stderr),
        detail: detail.to_owned(),
        cause: None,
    }
}

fn plain_error(operation: &str, cwd: &str, args: &[String], detail: &str) -> GitCommandError {
    git_error(
        &ExecuteGitInput::new(operation, cwd, args.to_vec()),
        detail,
        None,
        None,
        None,
        None,
        None,
    )
}

fn is_missing_cwd(error: &GitCommandError) -> bool {
    let GitCommandError::GitCommandError { cwd, detail, .. } = error;
    detail == "Failed to spawn Git process." && !Path::new(cwd).is_dir()
}

fn normalize_logical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn resolve_path(base: &str, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        normalize_logical(path)
    } else {
        normalize_logical(&Path::new(base).join(path))
    }
}

fn cache_key(cwd: &str) -> String {
    let path = Path::new(cwd);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalize_logical(&absolute).to_string_lossy().into_owned()
}

fn fetch_cwd_for(git_common_dir: &str) -> String {
    let path = Path::new(git_common_dir);
    if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
        path.parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_else(|| git_common_dir.to_owned())
    } else {
        git_common_dir.to_owned()
    }
}

fn real_or_normalized(path: &Path) -> String {
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| normalize_logical(path).to_string_lossy().into_owned())
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn trimmed_name(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn file_path(value: &str) -> Option<GitFilePath> {
    GitFilePath::new(value).ok()
}

fn parse_count(value: &str) -> u64 {
    let trimmed = value.trim();
    let digits: String = trimmed
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-' || *ch == '+')
        .collect();
    digits.parse::<i64>().unwrap_or(0).max(0) as u64
}

fn status_refresh_env() -> HashMap<String, String> {
    HashMap::from([
        ("GCM_INTERACTIVE".to_owned(), "never".to_owned()),
        ("GIT_ASKPASS".to_owned(), String::new()),
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
        ("SSH_ASKPASS".to_owned(), String::new()),
        ("SSH_ASKPASS_REQUIRE".to_owned(), "never".to_owned()),
    ])
}

fn split_index_config() -> Vec<String> {
    argv(&[
        "-c",
        "core.splitIndex=false",
        "-c",
        "splitIndex.sharedIndexExpire=never",
    ])
}

fn stdout_text(result: &ExecuteGitResult) -> String {
    if result.stdout_truncated && !result.stdout.ends_with(OUTPUT_TRUNCATED_MARKER) {
        format!("{}{OUTPUT_TRUNCATED_MARKER}", result.stdout)
    } else {
        result.stdout.clone()
    }
}

fn paths_ttl(value: &Option<RepoPaths>) -> Duration {
    if value.is_none() {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(10 * 60)
    }
}

fn default_worktrees_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    // Upstream uses `{t3BaseDir}/worktrees`. Turin keeps managed worktrees under `~/.turin`.
    PathBuf::from(home).join(".turin").join("worktrees")
}

fn expand_home(value: &str) -> String {
    if value == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| value.to_owned());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return format!("{}/{}", std::env::var("HOME").unwrap_or_default(), rest);
    }
    value.to_owned()
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none_or(|parent| parent == path)
}

fn resolve_worktrees_directory(setting: &str) -> Option<PathBuf> {
    if setting.is_empty() {
        return Some(default_worktrees_dir());
    }
    let expanded = expand_home(setting);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        return None;
    }
    let resolved = normalize_logical(path);
    if is_filesystem_root(&resolved) {
        None
    } else {
        Some(resolved)
    }
}

fn set_index_times(path: &Path, secs: u64) {
    #[cfg(unix)]
    {
        let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
        if let Ok(c_path) = std::ffi::CString::new(bytes) {
            let stamp = libc::timeval {
                tv_sec: secs as libc::time_t,
                tv_usec: 0,
            };
            let times = [stamp, stamp];
            unsafe {
                libc::utimes(c_path.as_ptr(), times.as_ptr());
            }
            return;
        }
    }
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_modified(UNIX_EPOCH + Duration::from_secs(secs));
    }
}

fn index_mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        if self.0.is_dir() {
            let _ = std::fs::remove_dir_all(&self.0);
        } else {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn js_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        serde_json::to_string(&value).unwrap_or_else(|_| "0".to_owned())
    }
}

fn review_hash(diff: &str, files: &[ReviewDiffFileStat]) -> String {
    let mut files_json = String::from("[");
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            files_json.push(',');
        }
        let previous = file
            .previous_path
            .as_ref()
            .map(|path| serde_json::to_string(path).unwrap_or_else(|_| "null".to_owned()))
            .unwrap_or_else(|| "null".to_owned());
        files_json.push_str(&format!(
            "{{\"path\":{},\"previousPath\":{previous},\"additions\":{},\"deletions\":{}}}",
            serde_json::to_string(&file.path).unwrap_or_else(|_| "\"\"".to_owned()),
            js_number(file.additions),
            js_number(file.deletions),
        ));
    }
    files_json.push(']');
    let payload = format!(
        "[{},{files_json}]",
        serde_json::to_string(diff).unwrap_or_else(|_| "\"\"".to_owned())
    );
    sha256_hex(payload.as_bytes())
}

fn within_root(root: &Path, candidate: &Path) -> bool {
    let Ok(relative) = candidate.strip_prefix(root) else {
        return candidate == root;
    };
    !relative.starts_with("..")
}

impl GitVcsDriverCore {
    async fn git(&self, cwd: &str, call: GitCall) -> Result<ExecuteGitResult, GitCommandError> {
        let mut env = call.env;
        if call.stable {
            env.insert("LC_ALL".to_owned(), "C".to_owned());
        }
        let mut input = ExecuteGitInput::new(call.operation, cwd, call.args.clone());
        input.stdin = call.stdin;
        input.env = if env.is_empty() { None } else { Some(env) };
        input.allow_non_zero_exit = true;
        input.timeout = call.timeout;
        input.max_output_bytes = call.max_output;
        input.append_truncation_marker = call.marker;
        input.on_stdout_line = call.on_stdout_line;
        input.on_stderr_line = call.on_stderr_line;
        let result = self.execute(input).await?;
        if call.allow_non_zero || result.exit_code == Some(0) {
            return Ok(result);
        }
        Err(failed(
            call.operation,
            cwd,
            &call.args,
            &result,
            call.fallback
                .unwrap_or("Git command exited with a non-zero status."),
        ))
    }

    async fn git_out(
        &self,
        operation: &'static str,
        cwd: &str,
        args: Vec<String>,
        allow_non_zero: bool,
    ) -> Result<String, GitCommandError> {
        let mut call = GitCall::new(operation, args);
        call.allow_non_zero = allow_non_zero;
        Ok(self.git(cwd, call).await?.stdout)
    }

    async fn git_void(
        &self,
        operation: &'static str,
        cwd: &str,
        args: Vec<String>,
        timeout: Option<Option<Duration>>,
    ) -> Result<(), GitCommandError> {
        let mut call = GitCall::new(operation, args);
        call.timeout = timeout;
        self.git(cwd, call).await?;
        Ok(())
    }

    async fn invalidate_refs_and_status(&self, cwd: &str) {
        let _ = self.invalidate_list_refs(cwd).await;
        let _ = self.invalidate_status_static(cwd).await;
    }

    pub async fn status(&self, input: &VcsStatusInput) -> Result<VcsStatusResult, GitCommandError> {
        let details = self.status_details(input.cwd.as_str()).await?;
        Ok(VcsStatusResult {
            is_repo: details.is_repo,
            source_control_provider: None,
            has_primary_remote: details.has_origin_remote,
            is_default_ref: details.is_default_branch,
            ref_name: details.branch.as_deref().and_then(trimmed_name),
            has_working_tree_changes: details.has_working_tree_changes,
            working_tree: details.working_tree,
            branch_changes: None,
            has_upstream: details.has_upstream,
            ahead_count: details.ahead_count,
            behind_count: details.behind_count,
            ahead_of_default_count: Some(details.ahead_of_default_count),
            pr: None,
        })
    }

    pub async fn status_details(&self, cwd: &str) -> Result<GitStatusDetails, GitCommandError> {
        if let Err(error) = self.refresh_status_upstream_if_stale(cwd).await
            && !is_missing_cwd(&error)
        {
            tracing::warn!("background status refresh failed");
        }
        self.read_status_details_local(cwd, GitLocalStatusOptions::default())
            .await
    }

    pub async fn status_details_local(
        &self,
        cwd: &str,
        options: GitLocalStatusOptions,
    ) -> Result<GitStatusDetails, GitCommandError> {
        self.read_status_details_local(cwd, options).await
    }

    pub async fn status_details_remote(
        &self,
        cwd: &str,
        options: GitRemoteStatusOptions,
    ) -> Result<GitRemoteStatusDetails, GitCommandError> {
        if options.refresh_upstream != Some(false)
            && let Err(error) = self.refresh_status_upstream_if_stale(cwd).await
            && !is_missing_cwd(&error)
        {
            tracing::warn!("background status refresh failed");
        }
        self.read_status_details_remote(cwd).await
    }

    pub async fn prepare_commit_context(
        &self,
        cwd: &str,
        file_paths: Option<&[String]>,
    ) -> Result<Option<GitPreparedCommitContext>, GitCommandError> {
        let index_value = self
            .git_out(
                "GitVcsDriver.prepareCommitContext.indexPath",
                cwd,
                argv(&["rev-parse", "--git-path", "index"]),
                false,
            )
            .await?;
        let index_path = resolve_path(cwd, index_value.trim());
        let directory =
            std::env::temp_dir().join(format!("t3code-commit-index-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).map_err(|error| {
            plain_error(
                "GitVcsDriver.prepareCommitContext",
                cwd,
                &[],
                "Failed to prepare the temporary commit index.",
            )
            .with_cause(error.to_string())
        })?;
        let _guard = TempPath(directory.clone());
        let temp_index = directory.join("index");
        let mut env = HashMap::new();
        env.insert(
            "GIT_INDEX_FILE".to_owned(),
            temp_index.to_string_lossy().into_owned(),
        );
        let config = split_index_config();
        if index_path.exists() {
            std::fs::copy(&index_path, &temp_index).map_err(|error| {
                plain_error(
                    "GitVcsDriver.prepareCommitContext",
                    cwd,
                    &[],
                    "Failed to prepare the temporary commit index.",
                )
                .with_cause(error.to_string())
            })?;
            set_index_times(&temp_index, index_mtime_secs(&index_path));
            let mut args = config.clone();
            args.extend(argv(&["update-index", "--no-split-index"]));
            let mut call = GitCall::new("GitVcsDriver.prepareCommitContext.expandSplitIndex", args);
            call.env = env.clone();
            self.git(cwd, call).await?;
        } else {
            let mut args = config.clone();
            args.extend(argv(&["read-tree", "--empty"]));
            let mut call = GitCall::new("GitVcsDriver.prepareCommitContext.emptyIndex", args);
            call.env = env.clone();
            self.git(cwd, call).await?;
        }
        self.stage_commit_changes(cwd, file_paths, Some(env.clone()))
            .await?;
        let mut summary_call = GitCall::new(
            "GitVcsDriver.prepareCommitContext.stagedSummary",
            argv(&["diff", "--cached", "--name-status"]),
        );
        summary_call.env = env.clone();
        let staged_summary = stdout_text(&self.git(cwd, summary_call).await?)
            .trim()
            .to_owned();
        if staged_summary.is_empty() {
            return Ok(None);
        }
        let mut patch_call = GitCall::new(
            "GitVcsDriver.prepareCommitContext.stagedPatch",
            argv(&["diff", "--no-ext-diff", "--cached", "--patch", "--minimal"]),
        );
        patch_call.env = env;
        patch_call.max_output = Some(PREPARED_COMMIT_PATCH_MAX);
        patch_call.marker = true;
        let staged_patch = stdout_text(&self.git(cwd, patch_call).await?);
        Ok(Some(GitPreparedCommitContext {
            staged_summary,
            staged_patch,
        }))
    }

    pub async fn commit(
        &self,
        cwd: &str,
        subject: &str,
        body: &str,
        options: GitCommitOptions,
    ) -> Result<GitCommitResult, GitCommandError> {
        let result = self.commit_inner(cwd, subject, body, options).await;
        self.invalidate_refs_and_status(cwd).await;
        result
    }

    pub async fn push_current_branch(
        &self,
        cwd: &str,
        fallback_branch: Option<&str>,
        options: GitPushOptions,
    ) -> Result<GitPushResult, GitCommandError> {
        let result = self
            .push_current_branch_inner(cwd, fallback_branch, options)
            .await;
        self.invalidate_refs_and_status(cwd).await;
        result
    }

    pub async fn pull_current_branch(&self, cwd: &str) -> Result<VcsPullResult, GitCommandError> {
        let result = self.pull_current_branch_inner(cwd).await;
        self.invalidate_refs_and_status(cwd).await;
        result
    }

    pub async fn read_range_context(
        &self,
        cwd: &str,
        base_ref: &str,
    ) -> Result<GitRangeContext, GitCommandError> {
        let commit_range = format!("{base_ref}..HEAD");
        let diff_range = format!("{base_ref}...HEAD");
        let (commit_summary, diff_summary, diff_patch) = tokio::join!(
            self.limited_stdout(
                "GitVcsDriver.readRangeContext.log",
                cwd,
                argv(&["log", "--oneline", &commit_range]),
                RANGE_COMMIT_SUMMARY_MAX,
            ),
            self.limited_stdout(
                "GitVcsDriver.readRangeContext.diffStat",
                cwd,
                argv(&["diff", "--stat", &diff_range]),
                RANGE_DIFF_SUMMARY_MAX,
            ),
            self.limited_stdout(
                "GitVcsDriver.readRangeContext.diffPatch",
                cwd,
                argv(&["diff", "--no-ext-diff", "--patch", "--minimal", &diff_range]),
                RANGE_DIFF_PATCH_MAX,
            ),
        );
        Ok(GitRangeContext {
            commit_summary: commit_summary?,
            diff_summary: diff_summary?,
            diff_patch: diff_patch?,
        })
    }

    pub async fn get_review_diff_preview(
        &self,
        input: &ReviewDiffPreviewInput,
    ) -> Result<ReviewDiffPreviewResult, GitCommandError> {
        self.review_diff_preview(input).await
    }

    pub async fn get_review_diff_file_contents(
        &self,
        input: &ReviewDiffFileContentsInput,
    ) -> Result<ReviewDiffFileContentsResult, GitCommandError> {
        self.review_diff_file_contents(input).await
    }

    pub async fn read_config_value(
        &self,
        cwd: &str,
        key: &str,
    ) -> Result<Option<String>, GitCommandError> {
        let stdout = self
            .git_out(
                "GitVcsDriver.readConfigValue",
                cwd,
                argv(&["config", "--get", key]),
                true,
            )
            .await?;
        Ok(nonempty(stdout.trim()))
    }

    pub async fn resolve_branch_name(
        &self,
        cwd: &str,
        ref_name: &str,
    ) -> Result<Option<String>, GitCommandError> {
        let mut call = GitCall::new(
            "GitVcsDriver.resolveBranchName.resolveRef",
            argv(&[
                "rev-parse",
                "--symbolic-full-name",
                "--verify",
                "--end-of-options",
                ref_name,
            ]),
        );
        call.allow_non_zero = true;
        let result = self.git(cwd, call).await?;
        if result.exit_code != Some(0) {
            let remote_name = self.resolve_primary_remote_name(cwd).await.ok();
            if let Some(remote_name) = remote_name
                && self
                    .remote_branch_exists(&GitRemoteBranchExistsInput {
                        cwd: cwd.to_owned(),
                        remote_name,
                        ref_name: ref_name.to_owned(),
                    })
                    .await?
            {
                return Ok(Some(ref_name.to_owned()));
            }
            return Ok(None);
        }
        let reference = result.stdout.trim();
        if let Some(name) = reference.strip_prefix("refs/heads/") {
            return Ok(Some(name.to_owned()));
        }
        if let Some(remote_ref) = reference.strip_prefix("refs/remotes/") {
            let mut names = self.list_remote_names(cwd).await?;
            names.sort_by_key(|name| std::cmp::Reverse(name.len()));
            return Ok(parse_remote_ref_with_remote_names(remote_ref, &names)
                .map(|parsed| parsed.branch_name));
        }
        Ok(None)
    }

    pub async fn list_refs(
        &self,
        input: &VcsListRefsInput,
    ) -> Result<VcsListRefsResult, GitCommandError> {
        self.list_refs_inner(input).await
    }

    pub async fn create_worktree(
        &self,
        input: &VcsCreateWorktreeInput,
        options: CreateWorktreeOptions,
    ) -> Result<VcsCreateWorktreeResult, GitCommandError> {
        let result = self.create_worktree_inner(input, options).await;
        self.invalidate_refs_and_status(input.cwd.as_str()).await;
        result
    }

    pub async fn fetch_pull_request_branch(
        &self,
        input: &GitFetchPullRequestBranchInput,
    ) -> Result<(), GitCommandError> {
        let result = self.fetch_pull_request_branch_inner(input).await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn fetch_pull_request_head_commit(
        &self,
        input: &GitFetchPullRequestHeadCommitInput,
    ) -> Result<GitResolveCommitResult, GitCommandError> {
        let remote_name = self.resolve_primary_remote_name(&input.cwd).await?;
        let mut call = GitCall::new(
            "GitVcsDriver.fetchPullRequestHeadCommit",
            argv(&[
                "fetch",
                "--quiet",
                "--no-tags",
                &remote_name,
                &format!("refs/pull/{}/head", input.pr_number),
            ]),
        );
        call.fallback = Some("git fetch pull request head failed");
        self.git(&input.cwd, call).await?;
        self.resolve_commit(&GitResolveCommitInput {
            cwd: input.cwd.clone(),
            revision: "FETCH_HEAD".to_owned(),
        })
        .await
    }

    pub async fn resolve_commit(
        &self,
        input: &GitResolveCommitInput,
    ) -> Result<GitResolveCommitResult, GitCommandError> {
        let stdout = self
            .git_out(
                "GitVcsDriver.resolveCommit",
                &input.cwd,
                argv(&[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", input.revision),
                ]),
                false,
            )
            .await?;
        Ok(GitResolveCommitResult {
            commit_sha: stdout.trim().to_owned(),
        })
    }

    pub async fn refresh_checked_out_branch(
        &self,
        input: &GitRefreshCheckedOutBranchInput,
    ) -> Result<GitRefreshCheckedOutBranchResult, GitCommandError> {
        let result = self.refresh_checked_out_branch_inner(input).await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn ensure_remote(
        &self,
        input: &GitEnsureRemoteInput,
    ) -> Result<String, GitCommandError> {
        let result = self.ensure_remote_inner(input).await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn resolve_primary_remote_name(&self, cwd: &str) -> Result<String, GitCommandError> {
        if self
            .remote_exists(&GitRemoteExistsInput {
                cwd: cwd.to_owned(),
                remote_name: "origin".to_owned(),
            })
            .await?
        {
            return Ok("origin".to_owned());
        }
        let remotes = self.list_remote_names(cwd).await?;
        if let Some(first) = remotes.first() {
            return Ok(first.clone());
        }
        Err(plain_error(
            "GitVcsDriver.resolvePrimaryRemoteName",
            cwd,
            &argv(&["remote"]),
            "No git remote is configured for this repository.",
        ))
    }

    pub async fn resolve_default_branch_name(
        &self,
        cwd: &str,
        remote_name: &str,
    ) -> Result<Option<String>, GitCommandError> {
        let mut call = GitCall::new(
            "GitVcsDriver.resolveDefaultBranchName",
            argv(&["symbolic-ref", &format!("refs/remotes/{remote_name}/HEAD")]),
        );
        call.allow_non_zero = true;
        let result = self.git(cwd, call).await?;
        if result.exit_code != Some(0) {
            return Ok(None);
        }
        Ok(parse_default_branch_from_remote_head_ref(
            &result.stdout,
            remote_name,
        ))
    }

    pub async fn fetch_remote(&self, input: &GitFetchRemoteInput) -> Result<(), GitCommandError> {
        let result = self.fetch_remote_inner(input).await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn remote_exists(
        &self,
        input: &GitRemoteExistsInput,
    ) -> Result<bool, GitCommandError> {
        let mut call = GitCall::new(
            "GitVcsDriver.remoteExists",
            argv(&["remote", "get-url", &input.remote_name]),
        );
        call.allow_non_zero = true;
        Ok(self.git(&input.cwd, call).await?.exit_code == Some(0))
    }

    pub async fn remote_branch_exists(
        &self,
        input: &GitRemoteBranchExistsInput,
    ) -> Result<bool, GitCommandError> {
        let mut call = GitCall::new(
            "GitVcsDriver.remoteBranchExists",
            argv(&[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{}/{}", input.remote_name, input.ref_name),
            ]),
        );
        call.allow_non_zero = true;
        Ok(self.git(&input.cwd, call).await?.exit_code == Some(0))
    }

    pub async fn resolve_remote_tracking_commit(
        &self,
        input: &GitResolveRemoteTrackingCommitInput,
    ) -> Result<GitResolveRemoteTrackingCommitResult, GitCommandError> {
        let mut remote_names = self.list_remote_names(&input.cwd).await?;
        remote_names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        let remote_ref_name = parse_remote_ref_with_remote_names(&input.ref_name, &remote_names)
            .map(|parsed| parsed.remote_ref)
            .unwrap_or_else(|| format!("{}/{}", input.fallback_remote_name, input.ref_name));
        let stdout = self
            .git_out(
                "GitVcsDriver.resolveRemoteTrackingCommit",
                &input.cwd,
                argv(&[
                    "rev-parse",
                    "--verify",
                    &format!("refs/remotes/{remote_ref_name}^{{commit}}"),
                ]),
                false,
            )
            .await?;
        Ok(GitResolveRemoteTrackingCommitResult {
            commit_sha: stdout.trim().to_owned(),
            remote_ref_name,
        })
    }

    pub async fn fetch_remote_branch(
        &self,
        input: &GitFetchRemoteBranchInput,
    ) -> Result<(), GitCommandError> {
        let result = self.fetch_remote_branch_inner(input).await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn fetch_remote_tracking_branch(
        &self,
        input: &GitFetchRemoteTrackingBranchInput,
    ) -> Result<(), GitCommandError> {
        let result = self
            .git_void(
                "GitVcsDriver.fetchRemoteTrackingBranch",
                &input.cwd,
                argv(&[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    &input.remote_name,
                    &format!(
                        "+refs/heads/{0}:refs/remotes/{1}/{0}",
                        input.remote_branch, input.remote_name
                    ),
                ]),
                None,
            )
            .await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn set_branch_upstream(
        &self,
        input: &GitSetBranchUpstreamInput,
    ) -> Result<(), GitCommandError> {
        let result = self
            .git_void(
                "GitVcsDriver.setBranchUpstream",
                &input.cwd,
                argv(&[
                    "branch",
                    "--set-upstream-to",
                    &format!("{}/{}", input.remote_name, input.remote_branch),
                    &input.branch,
                ]),
                None,
            )
            .await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn remove_worktree(
        &self,
        input: &VcsRemoveWorktreeInput,
    ) -> Result<(), GitCommandError> {
        let result = self.remove_worktree_inner(input).await;
        self.invalidate_refs_and_status(input.cwd.as_str()).await;
        result
    }

    pub async fn prune_worktrees(&self, cwd: &str) -> Result<(), GitCommandError> {
        let mut call = GitCall::new("GitVcsDriver.pruneWorktrees", argv(&["worktree", "prune"]));
        call.timeout = Some(Some(Duration::from_secs(15)));
        call.fallback = Some("git worktree prune failed");
        let result = self.git(cwd, call).await.map(|_| ());
        self.invalidate_refs_and_status(cwd).await;
        result
    }

    pub async fn delete_local_branch(
        &self,
        input: &GitDeleteLocalBranchInput,
    ) -> Result<(), GitCommandError> {
        let flag = if input.force == Some(true) {
            "-D"
        } else {
            "-d"
        };
        let mut call = GitCall::new(
            "GitVcsDriver.deleteLocalBranch",
            argv(&["branch", flag, "--", &input.ref_name]),
        );
        call.timeout = Some(Some(Duration::from_secs(10)));
        call.fallback = Some("git branch delete failed");
        let result = self.git(&input.cwd, call).await.map(|_| ());
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn rename_branch(
        &self,
        input: &GitRenameBranchInput,
    ) -> Result<GitRenameBranchResult, GitCommandError> {
        let result = self.rename_branch_inner(input).await;
        self.invalidate_refs_and_status(&input.cwd).await;
        result
    }

    pub async fn create_ref(
        &self,
        input: &VcsCreateRefInput,
    ) -> Result<VcsCreateRefResult, GitCommandError> {
        let result = self.create_ref_inner(input).await;
        self.invalidate_refs_and_status(input.cwd.as_str()).await;
        result
    }

    pub async fn switch_ref(
        &self,
        input: &VcsSwitchRefInput,
    ) -> Result<VcsSwitchRefResult, GitCommandError> {
        let result = self.switch_ref_inner(input).await;
        self.invalidate_refs_and_status(input.cwd.as_str()).await;
        result
    }

    pub async fn init_repo(&self, input: &VcsInitInput) -> Result<(), GitCommandError> {
        let mut call = GitCall::new("GitVcsDriver.initRepo", argv(&["init"]));
        call.timeout = Some(Some(Duration::from_secs(10)));
        call.fallback = Some("git init failed");
        let result = self.git(input.cwd.as_str(), call).await.map(|_| ());
        let key = cache_key(input.cwd.as_str());
        self.state.paths_refresh.lock().invalidate(&key);
        self.state.paths.lock().invalidate(&key);
        let _ = self.invalidate_list_refs(input.cwd.as_str()).await;
        result
    }

    pub async fn list_local_branch_names(&self, cwd: &str) -> Result<Vec<String>, GitCommandError> {
        let stdout = self
            .git_out(
                "GitVcsDriver.listLocalBranchNames",
                cwd,
                argv(&[
                    "branch",
                    "--list",
                    "--no-column",
                    "--format=%(refname:short)",
                ]),
                false,
            )
            .await?;
        Ok(stdout
            .split('\n')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub async fn list_worktree_paths(&self, cwd: &str) -> Result<Vec<String>, GitCommandError> {
        let result = self
            .worktree_list("GitVcsDriver.listWorktreePaths", cwd, Vec::new(), None)
            .await?;
        let mut paths = Vec::new();
        for record in result.stdout.split("\0\0") {
            let fields: Vec<&str> = record.split('\0').collect();
            let Some(worktree) = fields.iter().find(|field| field.starts_with("worktree ")) else {
                continue;
            };
            if fields.iter().any(|field| field.starts_with("prunable")) {
                continue;
            }
            let relative = &worktree["worktree ".len()..];
            paths.push(resolve_path(cwd, relative).to_string_lossy().into_owned());
        }
        Ok(paths)
    }
}

trait WithCause {
    fn with_cause(self, message: String) -> Self;
}

impl WithCause for GitCommandError {
    fn with_cause(self, message: String) -> Self {
        match self {
            Self::GitCommandError {
                operation,
                command,
                cwd,
                argument_count,
                exit_code,
                stdout_length,
                stderr_length,
                output_length,
                reason,
                detail,
                ..
            } => Self::GitCommandError {
                operation,
                command,
                cwd,
                argument_count,
                exit_code,
                stdout_length,
                stderr_length,
                output_length,
                reason,
                detail,
                cause: Some(serde_json::json!({ "message": message })),
            },
        }
    }
}

include!("driver_body.rs");
include!("driver_rest.rs");
