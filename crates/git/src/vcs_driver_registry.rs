//! Port of upstream/apps/server/src/vcs/VcsDriverRegistry.ts
//!
//! Detection cache: 2048 entries, 5 minute TTL. Misses and failures are not
//! stored (upstream TTL zero). Every hit is re-checked against the disk with
//! `exists` on `.git` between the cwd and the repository root, the same way
//! `detect` does, so `git init` or a removed worktree invalidates the entry
//! without another git process when nothing changed.
//!
//! The cache map is a `parking_lot::Mutex`. It is never held across `.await`.

#![allow(clippy::result_large_err)]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use turin_contracts::vcs::{
    VcsDriverKind, VcsError, VcsRepositoryIdentity, VcsUnsupportedOperationError,
};

use crate::git_vcs_driver::GitVcsDriver;
use crate::vcs_driver::VcsDriver;
use crate::vcs_project_config::{
    RequestedVcsKind, VcsProjectConfig, VcsProjectConfigResolveInput,
};

const DETECTION_CACHE_CAPACITY: usize = 2_048;
const DETECTION_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

pub struct VcsDriverResolveInput {
    pub cwd: String,
    pub requested_kind: Option<RequestedVcsKind>,
}

#[derive(Clone)]
pub struct VcsDriverHandle {
    pub kind: VcsDriverKind,
    pub repository: VcsRepositoryIdentity,
    pub driver: Arc<dyn VcsDriver>,
}

#[derive(Clone)]
struct CachedDetection {
    handle: VcsDriverHandle,
    git_entries: Vec<bool>,
    stored_at: Instant,
}

struct DiskState {
    git_entries: Vec<bool>,
    repository_on_disk: bool,
}

struct DetectionCache {
    map: HashMap<String, CachedDetection>,
    order: VecDeque<String>,
}

impl DetectionCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get_fresh(&mut self, key: &str) -> Option<CachedDetection> {
        let expired = self
            .map
            .get(key)
            .is_some_and(|entry| entry.stored_at.elapsed() >= DETECTION_CACHE_TTL);
        if expired {
            self.remove(key);
            return None;
        }
        if !self.map.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: String, value: CachedDetection) {
        if self.map.contains_key(&key) {
            self.touch(&key);
            self.map.insert(key, value);
            return;
        }
        while self.map.len() >= DETECTION_CACHE_CAPACITY {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }

    fn remove(&mut self, key: &str) {
        self.map.remove(key);
        if let Some(position) = self.order.iter().position(|item| item == key) {
            self.order.remove(position);
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.order.iter().position(|item| item == key) {
            self.order.remove(position);
            self.order.push_back(key.to_owned());
        }
    }
}

pub struct VcsDriverRegistry {
    project_config: Arc<VcsProjectConfig>,
    git: Arc<dyn VcsDriver>,
    cache: parking_lot::Mutex<DetectionCache>,
}

impl VcsDriverRegistry {
    pub fn new(project_config: Arc<VcsProjectConfig>, git: Arc<GitVcsDriver>) -> Self {
        Self {
            project_config,
            git,
            cache: parking_lot::Mutex::new(DetectionCache::new()),
        }
    }

    pub async fn get(&self, kind: VcsDriverKind) -> Result<Arc<dyn VcsDriver>, VcsError> {
        if kind == VcsDriverKind::Git {
            return Ok(Arc::clone(&self.git));
        }
        Err(unsupported(
            "VcsDriverRegistry.get",
            kind,
            format!("No {kind} VCS driver is registered."),
        ))
    }

    pub async fn detect(
        &self,
        input: &VcsDriverResolveInput,
    ) -> Result<Option<VcsDriverHandle>, VcsError> {
        let requested = self.resolved_kind(input).await?;
        let key = detection_cache_key(requested, &input.cwd);
        let cached = self.cache_get(&key);
        if let Some(cached) = cached {
            if self.disk_matches(&input.cwd, &cached).await {
                return Ok(Some(cached.handle));
            }
            self.cache_invalidate(&key);
        }
        Ok(self
            .compute(&key, requested, &input.cwd)
            .await?
            .map(|cached| cached.handle))
    }

    pub async fn resolve(
        &self,
        input: &VcsDriverResolveInput,
    ) -> Result<VcsDriverHandle, VcsError> {
        if let Some(detected) = self.detect(input).await? {
            return Ok(detected);
        }
        let requested = input.requested_kind.unwrap_or(RequestedVcsKind::Auto);
        let (kind, detail) = match requested {
            RequestedVcsKind::Auto => (
                VcsDriverKind::Unknown,
                format!(
                    "No supported VCS repository was detected at {}.",
                    input.cwd
                ),
            ),
            RequestedVcsKind::Kind(kind) => (
                kind,
                format!("No {kind} repository was detected at {}.", input.cwd),
            ),
        };
        Err(unsupported("VcsDriverRegistry.resolve", kind, detail))
    }

    async fn resolved_kind(
        &self,
        input: &VcsDriverResolveInput,
    ) -> Result<RequestedVcsKind, VcsError> {
        self.project_config
            .resolve_kind(&VcsProjectConfigResolveInput {
                cwd: input.cwd.clone(),
                requested_kind: input.requested_kind,
            })
            .await
            .map_err(|error| {
                unsupported(
                    "VcsDriverRegistry.detect",
                    VcsDriverKind::Unknown,
                    error.to_string(),
                )
            })
    }

    async fn compute(
        &self,
        key: &str,
        requested: RequestedVcsKind,
        cwd: &str,
    ) -> Result<Option<CachedDetection>, VcsError> {
        if let Some(hit) = self.cache_get(key) {
            return Ok(Some(hit));
        }
        let Some(handle) = self.detect_resolved(requested, cwd).await? else {
            // TTL zero: a negative lookup is not stored.
            return Ok(None);
        };
        let git_entries = self
            .read_disk_state(cwd, &handle.repository)
            .await
            .map(|disk| disk.git_entries)
            .unwrap_or_default();
        let cached = CachedDetection {
            handle,
            git_entries,
            stored_at: Instant::now(),
        };
        self.cache_insert(key.to_owned(), cached.clone());
        Ok(Some(cached))
    }

    async fn detect_resolved(
        &self,
        requested: RequestedVcsKind,
        cwd: &str,
    ) -> Result<Option<VcsDriverHandle>, VcsError> {
        if let RequestedVcsKind::Kind(kind) = requested {
            if kind != VcsDriverKind::Unknown {
                let driver = self.get(kind).await?;
                return self.detect_with_driver(kind, driver, cwd).await;
            }
        }
        self.detect_with_driver(VcsDriverKind::Git, Arc::clone(&self.git), cwd)
            .await
    }

    async fn detect_with_driver(
        &self,
        kind: VcsDriverKind,
        driver: Arc<dyn VcsDriver>,
        cwd: &str,
    ) -> Result<Option<VcsDriverHandle>, VcsError> {
        let Some(repository) = driver.detect_repository(cwd).await? else {
            return Ok(None);
        };
        Ok(Some(VcsDriverHandle {
            kind,
            repository,
            driver,
        }))
    }

    fn cache_get(&self, key: &str) -> Option<CachedDetection> {
        self.cache.lock().get_fresh(key)
    }

    fn cache_insert(&self, key: String, value: CachedDetection) {
        self.cache.lock().insert(key, value);
    }

    fn cache_invalidate(&self, key: &str) {
        self.cache.lock().remove(key);
    }

    async fn disk_matches(&self, cwd: &str, cached: &CachedDetection) -> bool {
        let Some(disk) = self.read_disk_state(cwd, &cached.handle.repository).await else {
            return false;
        };
        disk.repository_on_disk
            && disk.git_entries.len() == cached.git_entries.len()
            && disk
                .git_entries
                .iter()
                .zip(&cached.git_entries)
                .all(|(live, cached)| live == cached)
    }

    async fn read_disk_state(
        &self,
        cwd: &str,
        repository: &VcsRepositoryIdentity,
    ) -> Option<DiskState> {
        let folders = folders_below_root(cwd, repository.root_path.as_str()).await;
        let mut git_entries = Vec::with_capacity(folders.len());
        for folder in &folders {
            git_entries.push(try_exists(&folder.join(".git")).await?);
        }
        let cwd_path = PathBuf::from(cwd);
        let targets = match repository.metadata_path.as_ref() {
            None => vec![cwd_path],
            Some(metadata) => {
                let metadata = lexical_absolute(&cwd_path.join(metadata.as_str()));
                vec![cwd_path, metadata]
            }
        };
        let mut repository_on_disk = true;
        for target in &targets {
            if !try_exists(target).await? {
                repository_on_disk = false;
            }
        }
        Some(DiskState {
            git_entries,
            repository_on_disk,
        })
    }
}

fn unsupported(operation: &str, kind: VcsDriverKind, detail: String) -> VcsError {
    VcsError::UnsupportedOperation(VcsUnsupportedOperationError::VcsUnsupportedOperationError {
        operation: operation.to_owned(),
        kind,
        detail,
    })
}

fn detection_cache_key(kind: RequestedVcsKind, cwd: &str) -> String {
    format!("{}\0{cwd}", kind.cache_label())
}

/// `exists` that turns an IO error into `None` so the caller treats the whole
/// disk snapshot as unknown and invalidates, matching `Effect.orElseSucceed`.
async fn try_exists(path: &Path) -> Option<bool> {
    match tokio::fs::try_exists(path).await {
        Ok(exists) => Some(exists),
        Err(_) => None,
    }
}

fn lexical_absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The cwd and each parent below the repository root. A new `.git` in any of
/// them means `git init` made a nested repository. Falls back to the cwd alone
/// when the root is not above it.
///
/// Both sides are canonicalized when the directories exist. Git's
/// `--show-toplevel` resolves symlinks (`/var` → `/private/var` on macOS)
/// while the cwd string often does not; a lexical compare would treat every
/// macOS temp repo as "outside" and miss a nested `.git`.
async fn folders_below_root(cwd: &str, root_path: &str) -> Vec<PathBuf> {
    let cwd_abs = canonical_or_lexical(Path::new(cwd)).await;
    let root_abs = canonical_or_lexical(Path::new(root_path)).await;
    if !cwd_abs.starts_with(&root_abs) {
        return vec![cwd_abs];
    }
    let relative = cwd_abs.strip_prefix(&root_abs).unwrap_or(Path::new(""));
    let levels = relative.components().count().max(1);
    let mut folders = Vec::with_capacity(levels);
    for levels_up in 0..levels {
        let mut path = cwd_abs.clone();
        for _ in 0..levels_up {
            path.pop();
        }
        folders.push(path);
    }
    folders
}

async fn canonical_or_lexical(path: &Path) -> PathBuf {
    match tokio::fs::canonicalize(path).await {
        Ok(canonical) => canonical,
        Err(_) => lexical_absolute(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessRunner;
    use crate::vcs_process::VcsProcess;
    use std::process::Command;

    fn registry() -> VcsDriverRegistry {
        VcsDriverRegistry::new(
            Arc::new(VcsProjectConfig::new()),
            Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
        )
    }

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} in {}", cwd.display());
    }

    fn git_init(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-b", "main"]);
    }

    #[tokio::test]
    async fn routes_directly_by_vcs_driver_kind() {
        let registry = registry();
        let driver = registry.get(VcsDriverKind::Git).await.unwrap();
        assert_eq!(driver.capabilities().kind, VcsDriverKind::Git);
        let error = match registry.get(VcsDriverKind::Jj).await {
            Err(error) => error,
            Ok(_) => panic!("expected jj to be unsupported"),
        };
        match error {
            VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation,
                    kind,
                    detail,
                },
            ) => {
                assert_eq!(operation, "VcsDriverRegistry.get");
                assert_eq!(kind, VcsDriverKind::Jj);
                assert_eq!(detail, "No jj VCS driver is registered.");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn caches_repository_detection_for_repeated_resolves() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let registry = registry();
        let cwd = dir.path().display().to_string();
        let input = VcsDriverResolveInput {
            cwd: cwd.clone(),
            requested_kind: Some(RequestedVcsKind::Kind(VcsDriverKind::Git)),
        };
        let first = registry.resolve(&input).await.unwrap();
        break_git_dir(dir.path());
        let second = registry.resolve(&input).await.unwrap();
        assert_eq!(first.repository.root_path, second.repository.root_path);
        assert_eq!(first.kind, VcsDriverKind::Git);
        // A live detect would now fail. The cached handle is only valid because
        // `.git` still exists and the recorded git-entry bits did not change.
        std::fs::remove_dir_all(dir.path().join(".git")).unwrap();
        assert!(registry.detect(&input).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn detects_again_when_a_cached_repository_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let registry = registry();
        let cwd = dir.path().display().to_string();
        let input = VcsDriverResolveInput {
            cwd,
            requested_kind: None,
        };
        assert!(registry.detect(&input).await.unwrap().is_some());
        std::fs::remove_dir_all(dir.path().join(".git")).unwrap();
        assert!(registry.detect(&input).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn detects_again_when_git_init_creates_a_repository_inside_a_cached_one() {
        let parent = tempfile::tempdir().unwrap();
        git_init(parent.path());
        let project = parent.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let registry = registry();
        let cwd = project.display().to_string();
        let input = VcsDriverResolveInput {
            cwd: cwd.clone(),
            requested_kind: None,
        };
        let first = registry.detect(&input).await.unwrap().unwrap();
        git_init(&project);
        let second = registry.detect(&input).await.unwrap().unwrap();
        assert_ne!(first.repository.root_path, second.repository.root_path);
        assert!(second.repository.root_path.as_str().ends_with("project"));
    }

    #[tokio::test]
    async fn detects_again_when_git_init_creates_a_repository_between_cwd_and_root() {
        let parent = tempfile::tempdir().unwrap();
        git_init(parent.path());
        let middle = parent.path().join("a");
        let cwd_path = middle.join("b");
        std::fs::create_dir_all(&cwd_path).unwrap();
        let registry = registry();
        let input = VcsDriverResolveInput {
            cwd: cwd_path.display().to_string(),
            requested_kind: None,
        };
        let first = registry.detect(&input).await.unwrap().unwrap();
        git_init(&middle);
        let second = registry.detect(&input).await.unwrap().unwrap();
        assert_ne!(first.repository.root_path, second.repository.root_path);
        assert!(second.repository.root_path.as_str().ends_with("/a") || second.repository.root_path.as_str().ends_with("\\a"));
    }

    #[tokio::test]
    async fn detects_a_repository_created_after_a_negative_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry();
        let input = VcsDriverResolveInput {
            cwd: dir.path().display().to_string(),
            requested_kind: None,
        };
        assert!(registry.detect(&input).await.unwrap().is_none());
        git_init(dir.path());
        let detected = registry.detect(&input).await.unwrap().unwrap();
        assert!(detected.repository.root_path.as_str().contains(dir.path().file_name().unwrap().to_str().unwrap()));
    }

    #[tokio::test]
    async fn resolve_reports_unknown_when_nothing_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry();
        let cwd = dir.path().display().to_string();
        let error = match registry
            .resolve(&VcsDriverResolveInput {
                cwd: cwd.clone(),
                requested_kind: None,
            })
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("expected resolve to fail"),
        };
        match error {
            VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation,
                    kind,
                    detail,
                },
            ) => {
                assert_eq!(operation, "VcsDriverRegistry.resolve");
                assert_eq!(kind, VcsDriverKind::Unknown);
                assert_eq!(
                    detail,
                    format!("No supported VCS repository was detected at {cwd}.")
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn detection_cache_evicts_the_oldest_entry_past_capacity() {
        let mut cache = DetectionCache::new();
        for index in 0..=DETECTION_CACHE_CAPACITY {
            cache.insert(format!("k{index}"), sample_cached());
        }
        assert!(cache.get_fresh("k0").is_none());
        assert!(cache.get_fresh("k1").is_some());
        cache.insert("fresh".to_owned(), sample_cached());
        assert!(cache.get_fresh("k1").is_some());
        assert!(cache.get_fresh("k2").is_none());
    }

    #[test]
    fn detection_cache_drops_entries_after_the_ttl() {
        let mut cache = DetectionCache::new();
        let mut cached = sample_cached();
        cached.stored_at = Instant::now() - (DETECTION_CACHE_TTL + Duration::from_secs(1));
        cache.insert("old".to_owned(), cached);
        assert!(cache.get_fresh("old").is_none());
    }

    fn sample_cached() -> CachedDetection {
        CachedDetection {
            handle: VcsDriverHandle {
                kind: VcsDriverKind::Git,
                repository: VcsRepositoryIdentity {
                    kind: VcsDriverKind::Git,
                    root_path: turin_contracts::base_schemas::TrimmedNonEmptyString::new("/repo")
                        .unwrap(),
                    metadata_path: None,
                    freshness: turin_contracts::vcs::VcsFreshness {
                        source: turin_contracts::vcs::VcsFreshnessSource::LiveLocal,
                        observed_at: turin_contracts::base_schemas::UtcDateTime::now(),
                        expires_at: turin_contracts::base_schemas::EffectOption::None,
                    },
                },
                driver: Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
            },
            git_entries: vec![true],
            stored_at: Instant::now(),
        }
    }

    fn break_git_dir(repo: &Path) {
        let git_dir = repo.join(".git");
        for name in ["HEAD", "config", "index"] {
            let _ = std::fs::remove_file(git_dir.join(name));
        }
        let _ = std::fs::remove_dir_all(git_dir.join("objects"));
        let _ = std::fs::remove_dir_all(git_dir.join("refs"));
        assert!(git_dir.is_dir());
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--is-inside-work-tree"])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(!status.success());
    }
}
