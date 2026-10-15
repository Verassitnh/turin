//! Filesystem cases of upstream `AgentSessionScanner.test.ts` (`scan` and
//! `recentThreads`). Every fixture lives in a temp dir; the real `~/.claude`
//! and `~/.codex` are never read.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;
use turin_contracts::agent_sessions::{AgentSessionProjectGit, AgentSessionScanResult, AgentSessionSource};
use turin_contracts::base_schemas::{ProjectId, TrimmedNonEmptyString};
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceEnvironmentVariable,
    ProviderInstanceEnvironmentVariableName, ProviderInstanceId,
};

use super::*;
use crate::agent_session_scanner::{AgentSessionRecentThread, AgentSessionScanner};
use crate::project_ports::{PortError, ProjectStoreReads, ProjectRow};

/// `Date.parse` for the ISO strings the cases use.
fn ms(iso: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(iso)
        .expect("valid ISO timestamp")
        .timestamp_millis() as f64
}

fn temp_dir(prefix: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("temp dir")
}

/// Upstream `writeTranscript`: contents plus a fixed mtime, so ordering never depends on timing.
fn write_transcript(file_path: &Path, contents: &str, mtime_ms: f64) {
    fs::create_dir_all(file_path.parent().expect("parent")).expect("mkdir");
    fs::write(file_path, contents).expect("write transcript");
    let mtime = UNIX_EPOCH + Duration::from_millis(mtime_ms as u64);
    File::options()
        .write(true)
        .open(file_path)
        .expect("reopen")
        .set_modified(mtime)
        .expect("set mtime");
}

/// Claude session line: the first record carries the real `cwd`.
fn claude_session_line(cwd: &Path) -> String {
    format!(
        "{}\n{}\n",
        json!({"type": "user", "cwd": cwd, "sessionId": "s1"}),
        json!({"type": "assistant"})
    )
}

/// Codex rollout line: session metadata is nested under `payload`.
fn codex_rollout_line(cwd: &Path) -> String {
    format!(
        "{}\n",
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": "r1", "cwd": cwd},
        })
    )
}

fn source_name(source: AgentSessionSource) -> &'static str {
    driver_slug(source)
}

/// Comparable view of one candidate, in upstream's JSON shape.
#[derive(Debug, PartialEq)]
struct Summary {
    path: String,
    title: String,
    sources: Vec<&'static str>,
    thread_count: u64,
    last_active_at: Option<String>,
    already_imported: bool,
    git: Option<Option<AgentSessionProjectGit>>,
}

fn summarize(result: &AgentSessionScanResult) -> Vec<Summary> {
    result
        .candidates
        .iter()
        .map(|candidate| Summary {
            path: candidate.path.as_str().to_owned(),
            title: candidate.title.as_str().to_owned(),
            sources: candidate.sources.iter().copied().map(source_name).collect(),
            thread_count: candidate.thread_count,
            last_active_at: candidate.last_active_at.clone(),
            already_imported: candidate.already_imported,
            git: candidate.git.clone(),
        })
        .collect()
}

fn summary(
    path: &str,
    title: &str,
    sources: &[&'static str],
    thread_count: u64,
    last_active_at: Option<&str>,
) -> Summary {
    Summary {
        path: path.to_owned(),
        title: title.to_owned(),
        sources: sources.to_vec(),
        thread_count,
        last_active_at: last_active_at.map(str::to_owned),
        already_imported: false,
        git: Some(None),
    }
}

fn base_name(path: &Path) -> String {
    path.file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned()
}

struct Settings(ProviderInstanceConfigMap);

#[async_trait]
impl AgentSessionDiscoverySettings for Settings {
    async fn provider_instances(&self) -> Result<ProviderInstanceConfigMap, PortError> {
        Ok(self.0.clone())
    }
}

/// Only `list_shells` matters to discovery; the other reads are unused here.
struct Shells(Vec<OrchestrationProjectShell>);

#[async_trait]
impl ProjectStoreReads for Shells {
    async fn get(&self, _: &ProjectId, _: bool) -> Result<Option<ProjectRow>, PortError> {
        Ok(None)
    }
    async fn find_active_by_workspace_root(&self, _: &str) -> Result<Option<ProjectRow>, PortError> {
        Ok(None)
    }
    async fn list(&self, _: bool) -> Result<Vec<ProjectRow>, PortError> {
        Ok(Vec::new())
    }
    async fn get_shell(
        &self,
        _: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, PortError> {
        Ok(None)
    }
    async fn list_shells(
        &self,
        _: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, PortError> {
        Ok(self.0.clone())
    }
}

fn project_shell(workspace_root: &Path) -> OrchestrationProjectShell {
    OrchestrationProjectShell {
        id: ProjectId::new("project-1").expect("id"),
        title: TrimmedNonEmptyString::new("Imported").expect("title"),
        workspace_root: TrimmedNonEmptyString::new(workspace_root.to_string_lossy())
            .expect("root"),
        repository_identity: None,
        default_model_selection: None,
        default_thread_env_mode: None,
        auto_pull: None,
        favicon_path: None,
        project_icon: None,
        scripts: Vec::new(),
        created_at: "2026-01-01T00:00:00.000Z".to_owned(),
        updated_at: "2026-01-01T00:00:00.000Z".to_owned(),
    }
}

fn instance(driver: &str, config: Value) -> ProviderInstanceConfig {
    ProviderInstanceConfig {
        driver: ProviderDriverKind::new(driver).expect("driver"),
        display_name: None,
        accent_color: None,
        environment: None,
        enabled: None,
        config: Some(config),
    }
}

/// The fixture homes: default slots point at them; a case may replace a slot.
struct Fixture {
    _root: TempDir,
    claude_home: PathBuf,
    codex_home: PathBuf,
    base_dir: PathBuf,
    home_dir: PathBuf,
    instances: BTreeMap<String, ProviderInstanceConfig>,
    imported_roots: Vec<PathBuf>,
    host_environment: HashMap<String, String>,
    /// Pins `now` (epoch ms) for the window and `scannedAt`. `None` is the wall clock.
    now_ms: Option<f64>,
}

impl Fixture {
    fn new() -> Self {
        let root = temp_dir("t3code-scanner-");
        // Canonical, as the app passes its config and home dirs: the realpath
        // exclusion check compares against them (see the symlink case).
        let canonical = fs::canonicalize(root.path()).expect("canonical root");
        let claude_home = canonical.join("claude-home");
        let codex_home = canonical.join("codex-home");
        let base_dir = canonical.join("base");
        let home_dir = canonical.join("home");
        for dir in [&claude_home, &codex_home, &base_dir, &home_dir] {
            fs::create_dir_all(dir).expect("fixture dir");
        }
        let mut fixture = Self {
            _root: root,
            claude_home,
            codex_home,
            base_dir,
            home_dir,
            instances: BTreeMap::new(),
            imported_roots: Vec::new(),
            host_environment: HashMap::new(),
            now_ms: None,
        };
        fixture.instances.insert(
            "claudeAgent".to_owned(),
            instance("claudeAgent", json!({"homePath": fixture.claude_home})),
        );
        fixture.instances.insert(
            "codex".to_owned(),
            instance("codex", json!({"homePath": fixture.codex_home})),
        );
        fixture
    }

    /// Temp dir for a workspace. Children of the temp root are never excluded.
    fn workspace(&self, prefix: &str) -> TempDir {
        temp_dir(prefix)
    }

    fn configure(&mut self, id: &str, config: ProviderInstanceConfig) -> &mut Self {
        self.instances.insert(id.to_owned(), config);
        self
    }

    fn host(&self) -> AgentSessionDiscoveryHost {
        let tmp_dir = std::env::temp_dir().to_string_lossy().into_owned();
        AgentSessionDiscoveryHost {
            home_dir: self.home_dir.to_string_lossy().into_owned(),
            tmp_dir,
            base_dir: self.base_dir.to_string_lossy().into_owned(),
            worktrees_dir: self
                .base_dir
                .join("worktrees")
                .to_string_lossy()
                .into_owned(),
            case_fold: false,
            environment: self.host_environment.clone(),
        }
    }

    fn scanner_with(&self, fs: Arc<dyn ScannerFileSystem>) -> AgentSessionDiscovery {
        let settings = self
            .instances
            .iter()
            .map(|(id, config)| {
                (
                    ProviderInstanceId::new(id).expect("instance id"),
                    config.clone(),
                )
            })
            .collect::<ProviderInstanceConfigMap>();
        let discovery = AgentSessionDiscovery::new(
            self.host(),
            fs,
            Arc::new(Settings(settings)),
            Arc::new(Shells(
                self.imported_roots
                    .iter()
                    .map(|root| project_shell(root))
                    .collect(),
            )),
        );
        match self.now_ms {
            Some(now) => discovery.with_clock(move || now),
            None => discovery,
        }
    }

    fn scanner(&self) -> AgentSessionDiscovery {
        self.scanner_with(Arc::new(LocalScannerFileSystem))
    }

    async fn scan(&self) -> AgentSessionScanResult {
        self.scanner().scan().await.expect("scan")
    }

    async fn outcomes(&self, workspace_root: &Path) -> Vec<AgentSessionRecentThread> {
        let root = workspace_root.to_string_lossy().into_owned();
        self.scanner()
            .recent_threads(&root, &[])
            .map(|outcome| outcome.expect("recent thread"))
            .collect()
            .await
    }

    async fn threads(&self, workspace_root: &Path) -> Vec<AgentSessionThread> {
        self.outcomes(workspace_root)
            .await
            .into_iter()
            .filter_map(|outcome| match outcome {
                AgentSessionRecentThread::Importable { thread, .. } => Some(thread),
                _ => None,
            })
            .collect()
    }

    fn claude_transcript(&self, slug: &str, name: &str) -> PathBuf {
        self.claude_home.join("projects").join(slug).join(name)
    }

    fn codex_rollout(&self, year: &str, month: &str, day: &str, name: &str) -> PathBuf {
        self.codex_home
            .join("sessions")
            .join(year)
            .join(month)
            .join(day)
            .join(name)
    }
}

#[tokio::test]
async fn reads_claude_project_cwds_from_transcripts_newest_first() {
    let fixture = Fixture::new();
    let older = fixture.workspace("t3code-workspace-older-");
    let newer = fixture.workspace("t3code-workspace-newer-");
    // Slugs are intentionally lossy; the scanner must not decode them.
    write_transcript(
        &fixture.claude_transcript("-slug-older", "a.jsonl"),
        &claude_session_line(older.path()),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &fixture.claude_transcript("-slug-older", "b.jsonl"),
        &claude_session_line(older.path()),
        ms("2026-01-02T00:00:00.000Z"),
    );
    write_transcript(
        &fixture.claude_transcript("-slug-newer", "c.jsonl"),
        &claude_session_line(newer.path()),
        ms("2026-03-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(
        summarize(&result),
        vec![
            summary(
                &newer.path().to_string_lossy(),
                &base_name(newer.path()),
                &["claudeAgent"],
                1,
                Some("2026-03-01T00:00:00.000Z"),
            ),
            summary(
                &older.path().to_string_lossy(),
                &base_name(older.path()),
                &["claudeAgent"],
                2,
                Some("2026-01-02T00:00:00.000Z"),
            ),
        ]
    );
}

#[tokio::test]
async fn groups_codex_rollouts_by_cwd_across_date_directories() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let other = fixture.workspace("t3code-workspace-other-");
    write_transcript(
        &fixture.codex_rollout("2026", "01", "05", "rollout-2026-01-05T10-00-00-aaa.jsonl"),
        &codex_rollout_line(workspace.path()),
        ms("2026-01-05T10:00:00.000Z"),
    );
    write_transcript(
        &fixture.codex_rollout("2026", "02", "09", "rollout-2026-02-09T10-00-00-bbb.jsonl"),
        &codex_rollout_line(workspace.path()),
        ms("2026-02-09T10:00:00.000Z"),
    );
    write_transcript(
        &fixture.codex_rollout("2026", "02", "09", "rollout-2026-02-09T11-00-00-ccc.jsonl"),
        &codex_rollout_line(other.path()),
        ms("2026-02-09T11:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(
        summarize(&result),
        vec![
            summary(
                &other.path().to_string_lossy(),
                &base_name(other.path()),
                &["codex"],
                1,
                Some("2026-02-09T11:00:00.000Z"),
            ),
            summary(
                &workspace.path().to_string_lossy(),
                &base_name(workspace.path()),
                &["codex"],
                2,
                Some("2026-02-09T10:00:00.000Z"),
            ),
        ]
    );
}

#[tokio::test]
async fn does_not_open_a_non_file_transcript() {
    for source in [AgentSessionSource::ClaudeAgent, AgentSessionSource::Codex] {
        let fixture = Fixture::new();
        let transcript_path = match source {
            AgentSessionSource::ClaudeAgent => fixture.claude_transcript("-slug", "session.jsonl"),
            AgentSessionSource::Codex => {
                fixture.codex_rollout("2026", "08", "24", "rollout-session.jsonl")
            }
        };
        // A directory named like a transcript must be skipped before any open.
        fs::create_dir_all(&transcript_path).expect("directory transcript");
        let recording = Arc::new(FsDouble::default());

        let result = fixture
            .scanner_with(recording.clone())
            .scan()
            .await
            .expect("scan");

        assert_eq!(summarize(&result), Vec::new(), "{source:?}");
        assert_eq!(recording.opened(&transcript_path), 0, "{source:?}");
    }
}

/// Test double over the host filesystem. Each hook mirrors one upstream
/// instrumented `FileSystem` override: scripted listings, path aliasing, open
/// counts, and capped reads.
#[derive(Default)]
struct FsDouble {
    inner: LocalScannerFileSystem,
    /// `stat(path)` answers for `path` as if it were this other path.
    stat_redirects: HashMap<String, String>,
    /// Maps every stat and open path (virtual files onto a template).
    path_redirect: Option<Box<dyn Fn(&str) -> Option<String> + Send + Sync>>,
    /// Scripted `readDirectory` answers, counted in `directory_reads`.
    listings: HashMap<String, Vec<String>>,
    /// Every directory whose parent is this path reads as empty and is counted.
    empty_children_of: Option<String>,
    directory_reads: Mutex<usize>,
    opens: Mutex<Vec<String>>,
    /// Size requested by each `read_alloc`, in call order.
    read_requests: Arc<Mutex<Vec<usize>>>,
    /// Caps every `read_alloc` request, forcing short reads.
    max_read: Option<usize>,
}

impl FsDouble {
    fn opened(&self, path: &Path) -> usize {
        let target = path.to_string_lossy();
        self.opens.lock().iter().filter(|open| **open == target).count()
    }

    fn resolve(&self, path: &str) -> String {
        self.path_redirect
            .as_ref()
            .and_then(|redirect| redirect(path))
            .unwrap_or_else(|| path.to_owned())
    }
}

#[async_trait]
impl ScannerFileSystem for FsDouble {
    async fn stat(&self, path: &str) -> Option<FileStat> {
        let resolved = self.resolve(path);
        let target = self
            .stat_redirects
            .get(&resolved)
            .map_or(resolved.as_str(), String::as_str);
        self.inner.stat(target).await
    }
    async fn real_path(&self, path: &str) -> Option<String> {
        self.inner.real_path(path).await
    }
    async fn read_directory(&self, path: &str) -> Vec<String> {
        if let Some(names) = self.listings.get(path) {
            *self.directory_reads.lock() += 1;
            return names.clone();
        }
        if self.empty_children_of.as_deref() == Path::new(path).parent().and_then(Path::to_str) {
            *self.directory_reads.lock() += 1;
            return Vec::new();
        }
        self.inner.read_directory(path).await
    }
    async fn read_file_string(&self, path: &str) -> Option<String> {
        self.inner.read_file_string(path).await
    }
    async fn open(&self, path: &str) -> Option<Box<dyn ScannerFile>> {
        self.opens.lock().push(path.to_owned());
        let file = self.inner.open(&self.resolve(path)).await?;
        Some(Box::new(CountingFile {
            inner: file,
            requests: Arc::clone(&self.read_requests),
            max_read: self.max_read,
        }))
    }
}

struct CountingFile {
    inner: Box<dyn ScannerFile>,
    requests: Arc<Mutex<Vec<usize>>>,
    max_read: Option<usize>,
}

#[async_trait]
impl ScannerFile for CountingFile {
    async fn stat(&self) -> Option<FileStat> {
        self.inner.stat().await
    }
    async fn read_alloc(&mut self, size: usize) -> std::io::Result<Option<Vec<u8>>> {
        self.requests.lock().push(size);
        let size = self.max_read.map_or(size, |cap| size.min(cap));
        self.inner.read_alloc(size).await
    }
}

fn fs_double(configure: impl FnOnce(&mut FsDouble)) -> Arc<FsDouble> {
    let mut double = FsDouble::default();
    configure(&mut double);
    Arc::new(double)
}

#[tokio::test]
async fn stops_directory_reads_at_the_discovery_operation_budget() {
    for source in [AgentSessionSource::ClaudeAgent, AgentSessionSource::Codex] {
        let fixture = Fixture::new();
        let discovery_root = match source {
            AgentSessionSource::ClaudeAgent => fixture.claude_home.join("projects"),
            AgentSessionSource::Codex => fixture.codex_home.join("sessions"),
        };
        let names: Vec<String> = (0..20_001).map(|index| format!("empty-{index:05}")).collect();
        let double = fs_double(|double| {
            double
                .listings
                .insert(discovery_root.to_string_lossy().into_owned(), names);
            double.empty_children_of = Some(discovery_root.to_string_lossy().into_owned());
        });

        let result = fixture
            .scanner_with(double.clone())
            .scan()
            .await
            .expect("scan");

        assert_eq!(summarize(&result), Vec::new(), "{source:?}");
        assert_eq!(*double.directory_reads.lock(), 20_000, "{source:?}");
    }
}

#[tokio::test]
async fn merges_the_same_cwd_seen_by_both_agents_and_flags_imported_projects() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(workspace.path()),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &fixture.codex_rollout("2026", "04", "01", "rollout-2026-04-01T09-00-00-aaa.jsonl"),
        &codex_rollout_line(workspace.path()),
        ms("2026-04-01T09:00:00.000Z"),
    );
    fixture.imported_roots.push(workspace.path().to_path_buf());

    let result = fixture.scan().await;

    assert_eq!(
        summarize(&result),
        vec![Summary {
            already_imported: true,
            ..summary(
                &workspace.path().to_string_lossy(),
                &base_name(workspace.path()),
                &["claudeAgent", "codex"],
                2,
                Some("2026-04-01T09:00:00.000Z"),
            )
        }]
    );
    assert_eq!(
        result.candidates[0].project_id,
        Some(ProjectId::new("project-1").expect("id"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn returns_the_imported_project_id_through_a_realpath_alias() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let links = fixture.workspace("t3code-scanner-links-");
    let alias = links.path().join("workspace-alias");
    std::os::unix::fs::symlink(workspace.path(), &alias).expect("symlink");
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(&alias),
        ms("2026-01-01T00:00:00.000Z"),
    );
    fixture.imported_roots.push(workspace.path().to_path_buf());

    let result = fixture.scan().await;

    let candidate = &result.candidates[0];
    assert_eq!(candidate.path.as_str(), workspace.path().to_string_lossy());
    assert_eq!(candidate.project_id, Some(ProjectId::new("project-1").expect("id")));
    assert!(candidate.already_imported);
    assert_eq!(candidate.git, Some(None));
}

#[cfg(unix)]
#[tokio::test]
async fn matches_a_persisted_project_alias_to_a_transcript_realpath() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let links = fixture.workspace("t3code-scanner-links-");
    let alias = links.path().join("workspace-alias");
    std::os::unix::fs::symlink(workspace.path(), &alias).expect("symlink");
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(workspace.path()),
        ms("2026-01-01T00:00:00.000Z"),
    );
    fixture.imported_roots.push(alias.clone());

    let result = fixture.scan().await;

    let candidate = &result.candidates[0];
    assert_eq!(candidate.path.as_str(), alias.to_string_lossy());
    assert_eq!(candidate.project_id, Some(ProjectId::new("project-1").expect("id")));
    assert!(candidate.already_imported);
    assert_eq!(candidate.git, Some(None));
}

#[tokio::test]
async fn merges_case_aliases_and_preserves_the_persisted_project_path() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let alias_name = base_name(workspace.path()).to_uppercase();
    let alias = workspace.path().parent().expect("parent").join(alias_name);
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(&alias),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &fixture.codex_rollout("2026", "01", "02", "rollout-b.jsonl"),
        &codex_rollout_line(workspace.path()),
        ms("2026-01-02T00:00:00.000Z"),
    );
    fixture.imported_roots.push(workspace.path().to_path_buf());
    // The alias spelling stats as the real workspace, as on a case-insensitive volume.
    let double = fs_double(|double| {
        double.stat_redirects.insert(
            alias.to_string_lossy().into_owned(),
            workspace.path().to_string_lossy().into_owned(),
        );
    });

    let result = fixture
        .scanner_with(double)
        .scan()
        .await
        .expect("scan");

    assert_eq!(
        summarize(&result),
        vec![Summary {
            already_imported: true,
            ..summary(
                &workspace.path().to_string_lossy(),
                &base_name(workspace.path()),
                &["claudeAgent", "codex"],
                2,
                Some("2026-01-02T00:00:00.000Z"),
            )
        }]
    );
}

#[tokio::test]
async fn keeps_case_variants_distinct_when_the_filesystem_identities_differ() {
    let fixture = Fixture::new();
    let backing_upper = fixture.workspace("t3code-backing-upper-");
    let backing_lower = fixture.workspace("t3code-backing-lower-");
    let alias_parent = fixture.workspace("t3code-case-aliases-");
    let upper = alias_parent.path().join("Repo");
    let lower = alias_parent.path().join("repo");
    write_transcript(
        &fixture.claude_transcript("-upper", "a.jsonl"),
        &claude_session_line(&upper),
        ms("2026-01-02T00:00:00.000Z"),
    );
    write_transcript(
        &fixture.claude_transcript("-lower", "b.jsonl"),
        &claude_session_line(&lower),
        ms("2026-01-01T00:00:00.000Z"),
    );
    let double = fs_double(|double| {
        double.stat_redirects.insert(
            upper.to_string_lossy().into_owned(),
            backing_upper.path().to_string_lossy().into_owned(),
        );
        double.stat_redirects.insert(
            lower.to_string_lossy().into_owned(),
            backing_lower.path().to_string_lossy().into_owned(),
        );
    });

    let result = fixture
        .scanner_with(double)
        .scan()
        .await
        .expect("scan");

    let paths: Vec<String> = result
        .candidates
        .iter()
        .map(|candidate| candidate.path.as_str().to_owned())
        .collect();
    assert_eq!(
        paths,
        vec![
            upper.to_string_lossy().into_owned(),
            lower.to_string_lossy().into_owned(),
        ]
    );
}

fn env_var(name: &str, value: &Path) -> ProviderInstanceEnvironmentVariable {
    ProviderInstanceEnvironmentVariable {
        name: ProviderInstanceEnvironmentVariableName::new(name).expect("name"),
        value: value.to_string_lossy().into_owned(),
        sensitive: false,
        value_redacted: None,
    }
}

fn paths(result: &AgentSessionScanResult) -> Vec<String> {
    result
        .candidates
        .iter()
        .map(|candidate| candidate.path.as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn uses_explicit_default_slot_homes_instead_of_the_fixture_homes() {
    let mut fixture = Fixture::new();
    let legacy_workspace = fixture.workspace("t3code-workspace-legacy-");
    let claude_workspace = fixture.workspace("t3code-workspace-claude-");
    let codex_workspace = fixture.workspace("t3code-workspace-codex-");
    let claude_instance_home = temp_dir("t3code-claude-instance-");
    let codex_instance_home = temp_dir("t3code-codex-instance-");
    // The fixture homes still hold a transcript, but the slots now point elsewhere.
    write_transcript(
        &fixture.claude_transcript("-legacy", "session.jsonl"),
        &claude_session_line(legacy_workspace.path()),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &claude_instance_home.path().join("projects").join("-actual").join("session.jsonl"),
        &claude_session_line(claude_workspace.path()),
        ms("2026-02-01T00:00:00.000Z"),
    );
    write_transcript(
        &codex_instance_home
            .path()
            .join("sessions")
            .join("2026")
            .join("03")
            .join("01")
            .join("rollout-instance.jsonl"),
        &codex_rollout_line(codex_workspace.path()),
        ms("2026-03-01T00:00:00.000Z"),
    );
    fixture.configure(
        "claudeAgent",
        instance("claudeAgent", json!({"homePath": claude_instance_home.path()})),
    );
    fixture.configure(
        "codex",
        instance("codex", json!({"homePath": codex_instance_home.path()})),
    );

    let result = fixture.scan().await;

    assert_eq!(
        paths(&result),
        vec![
            codex_workspace.path().to_string_lossy().into_owned(),
            claude_workspace.path().to_string_lossy().into_owned(),
        ]
    );
}

#[tokio::test]
async fn scans_each_distinct_home_across_multiple_instances_once() {
    let mut fixture = Fixture::new();
    let other_codex_home = temp_dir("t3code-codex-other-");
    let workspace = fixture.workspace("t3code-workspace-");
    let other_workspace = fixture.workspace("t3code-workspace-other-");
    for (home, cwd) in [
        (fixture.codex_home.clone(), workspace.path().to_path_buf()),
        (other_codex_home.path().to_path_buf(), other_workspace.path().to_path_buf()),
    ] {
        write_transcript(
            &home.join("sessions").join("2026").join("01").join("01").join("rollout-session.jsonl"),
            &codex_rollout_line(&cwd),
            ms("2026-01-01T00:00:00.000Z"),
        );
    }
    fixture.configure(
        "codex-personal",
        instance("codex", json!({"homePath": fixture.codex_home})),
    );
    fixture.configure(
        "codex-work",
        instance("codex", json!({"homePath": other_codex_home.path()})),
    );

    let result = fixture.scan().await;

    assert_eq!(result.candidates.len(), 2);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.thread_count)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    let mut found = paths(&result);
    found.sort();
    let mut expected = vec![
        workspace.path().to_string_lossy().into_owned(),
        other_workspace.path().to_string_lossy().into_owned(),
    ];
    expected.sort();
    assert_eq!(found, expected);
}

#[tokio::test]
async fn honors_provider_instance_home_directory_environment_variables() {
    let mut fixture = Fixture::new();
    let claude_environment_home = temp_dir("t3code-claude-env-");
    let codex_environment_home = temp_dir("t3code-codex-env-");
    let claude_workspace = fixture.workspace("t3code-workspace-claude-");
    let codex_workspace = fixture.workspace("t3code-workspace-codex-");
    write_transcript(
        &claude_environment_home
            .path()
            .join("projects")
            .join("-actual")
            .join("session.jsonl"),
        &claude_session_line(claude_workspace.path()),
        ms("2026-01-01T00:00:00.000Z"),
    );
    write_transcript(
        &codex_environment_home
            .path()
            .join("sessions")
            .join("2026")
            .join("01")
            .join("01")
            .join("rollout-session.jsonl"),
        &codex_rollout_line(codex_workspace.path()),
        ms("2026-01-02T00:00:00.000Z"),
    );
    fixture.configure(
        "claudeAgent",
        ProviderInstanceConfig {
            environment: Some(vec![env_var("CLAUDE_CONFIG_DIR", claude_environment_home.path())]),
            ..instance("claudeAgent", json!({}))
        },
    );
    fixture.configure(
        "codex",
        ProviderInstanceConfig {
            environment: Some(vec![env_var("CODEX_HOME", codex_environment_home.path())]),
            ..instance("codex", json!({}))
        },
    );

    let result = fixture.scan().await;

    assert_eq!(
        paths(&result),
        vec![
            codex_workspace.path().to_string_lossy().into_owned(),
            claude_workspace.path().to_string_lossy().into_owned(),
        ]
    );
}

#[tokio::test]
async fn ignores_invalid_provider_instances_while_scanning_the_remaining_providers() {
    let mut fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    write_transcript(
        &fixture.claude_transcript("-actual", "session.jsonl"),
        &claude_session_line(workspace.path()),
        ms("2026-01-01T00:00:00.000Z"),
    );
    fixture.configure("codex", instance("codex", json!({"homePath": 123})));

    let result = fixture.scan().await;

    assert_eq!(paths(&result), vec![workspace.path().to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn does_not_scan_provider_instances_disabled_by_the_envelope_or_config() {
    let mut fixture = Fixture::new();
    let envelope_home = temp_dir("t3code-codex-disabled-envelope-");
    let config_home = temp_dir("t3code-codex-disabled-config-");
    let envelope_workspace = fixture.workspace("t3code-workspace-disabled-envelope-");
    let config_workspace = fixture.workspace("t3code-workspace-disabled-config-");
    for (home, workspace, session) in [
        (envelope_home.path(), envelope_workspace.path(), "envelope-disabled"),
        (config_home.path(), config_workspace.path(), "config-disabled"),
    ] {
        write_transcript(
            &home
                .join("sessions")
                .join("2026")
                .join("08")
                .join("24")
                .join(format!("rollout-{session}.jsonl")),
            &codex_rollout_line(workspace),
            ms("2026-08-24T12:00:00.000Z"),
        );
    }
    fixture.configure(
        "codex-envelope-disabled",
        ProviderInstanceConfig {
            enabled: Some(false),
            ..instance("codex", json!({"homePath": envelope_home.path()}))
        },
    );
    fixture.configure(
        "codex-config-disabled",
        instance("codex", json!({"enabled": false, "homePath": config_home.path()})),
    );

    let result = fixture.scan().await;

    assert_eq!(summarize(&result), Vec::new());
}

#[tokio::test]
async fn ignores_relative_working_directories_from_malformed_transcripts() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let relative = pathdiff_relative(workspace.path());
    write_transcript(
        &fixture.claude_transcript("-relative", "session.jsonl"),
        &claude_session_line(Path::new(&relative)),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(summarize(&result), Vec::new());
}

/// A cwd spelled relative to the process cwd, as a malformed transcript might record it.
fn pathdiff_relative(target: &Path) -> String {
    let cwd = std::env::current_dir().expect("cwd");
    let mut relative = PathBuf::new();
    let mut common = 0;
    let target_parts: Vec<_> = target.components().collect();
    let cwd_parts: Vec<_> = cwd.components().collect();
    while common < target_parts.len().min(cwd_parts.len()) && target_parts[common] == cwd_parts[common] {
        common += 1;
    }
    for _ in common..cwd_parts.len() {
        relative.push("..");
    }
    for part in &target_parts[common..] {
        relative.push(part.as_os_str());
    }
    relative.to_string_lossy().into_owned()
}

#[tokio::test]
async fn drops_candidates_whose_directory_no_longer_exists() {
    let fixture = Fixture::new();
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(&fixture.claude_home.join("does-not-exist")),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(summarize(&result), Vec::new());
}

#[tokio::test]
async fn excludes_the_home_directory_temporary_root_and_t3_data_directory() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let cwds = [
        fixture.home_dir.clone(),
        std::env::temp_dir(),
        fixture.base_dir.clone(),
        workspace.path().to_path_buf(),
    ];
    for (index, cwd) in cwds.iter().enumerate() {
        write_transcript(
            &fixture.claude_transcript(&format!("-slug-{index}"), "session.jsonl"),
            &claude_session_line(cwd),
            ms("2026-01-01T00:00:00.000Z") + index as f64,
        );
    }

    let result = fixture.scan().await;

    assert_eq!(paths(&result), vec![workspace.path().to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn excludes_t3_managed_worktree_sandboxes() {
    let fixture = Fixture::new();
    let worktree_cwd = fixture
        .claude_home
        .join(".t3")
        .join("worktrees")
        .join("t3code")
        .join("wt-1");
    fs::create_dir_all(&worktree_cwd).expect("worktree");
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(&worktree_cwd),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(summarize(&result), Vec::new());
}

#[tokio::test]
async fn excludes_codex_scratch_directories_and_downloads() {
    let fixture = Fixture::new();
    let scratch = fixture
        .home_dir
        .join("Documents")
        .join("Codex")
        .join("2026-09-01")
        .join("some-conversation");
    let downloads = fixture.home_dir.join("Downloads").join("unpacked");
    let keep = fixture.workspace("t3code-workspace-keep-");
    fs::create_dir_all(&scratch).expect("scratch");
    fs::create_dir_all(&downloads).expect("downloads");
    let cwds = [scratch, downloads, keep.path().to_path_buf()];
    for (index, cwd) in cwds.iter().enumerate() {
        write_transcript(
            &fixture.codex_rollout("2026", "09", "01", &format!("rollout-{index}.jsonl")),
            &codex_rollout_line(cwd),
            ms("2026-09-01T00:00:00.000Z"),
        );
    }

    let result = fixture.scan().await;

    assert_eq!(paths(&result), vec![keep.path().to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn skips_linked_git_worktrees_and_reports_the_origin_of_real_checkouts() {
    let fixture = Fixture::new();
    let repo = fixture.workspace("t3code-workspace-repo-");
    let worktree = fixture.workspace("t3code-workspace-worktree-");
    let plain = fixture.workspace("t3code-workspace-plain-");
    let no_remote = fixture.workspace("t3code-workspace-noremote-");
    let submodule = fixture.workspace("t3code-workspace-submodule-");
    let repo_git = repo.path().join(".git");
    fs::create_dir_all(&repo_git).expect("repo git");
    fs::write(
        repo_git.join("config"),
        "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:pingdotgg/t3code.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
    )
    .expect("repo config");
    fs::write(
        worktree.path().join(".git"),
        format!("gitdir: {}\n", repo_git.join("worktrees").join("wt").display()),
    )
    .expect("worktree pointer");
    fs::create_dir_all(no_remote.path().join(".git")).expect("no-remote git");
    fs::write(no_remote.path().join(".git").join("config"), "[core]\n").expect("config");
    // Submodules also use a gitdir pointer, but into `modules/`, not `worktrees/`.
    let submodule_git = repo_git.join("modules").join("vendor");
    fs::create_dir_all(&submodule_git).expect("submodule git");
    fs::write(
        submodule_git.join("config"),
        "[remote \"origin\"]\n\turl = ssh://github.com/pingdotgg/vendor.git\n",
    )
    .expect("submodule config");
    fs::write(
        submodule.path().join(".git"),
        format!("gitdir: {}\n", submodule_git.display()),
    )
    .expect("submodule pointer");
    let cwds = [
        repo.path(),
        worktree.path(),
        plain.path(),
        no_remote.path(),
        submodule.path(),
    ];
    for (index, cwd) in cwds.iter().enumerate() {
        write_transcript(
            &fixture.claude_transcript(&format!("-slug-{index}"), "a.jsonl"),
            &claude_session_line(cwd),
            ms(&format!("2026-01-0{}T00:00:00.000Z", index + 1)),
        );
    }

    let result = fixture.scan().await;

    let found: Vec<(String, Option<Option<AgentSessionProjectGit>>)> = result
        .candidates
        .iter()
        .map(|candidate| (candidate.path.as_str().to_owned(), candidate.git.clone()))
        .collect();
    let git = |remote_key: Option<&str>, repository: Option<&str>| {
        Some(Some(AgentSessionProjectGit {
            remote_key: remote_key.map(str::to_owned),
            repository: repository.map(str::to_owned),
        }))
    };
    assert_eq!(
        found,
        vec![
            (
                submodule.path().to_string_lossy().into_owned(),
                git(Some("github.com/pingdotgg/vendor"), Some("pingdotgg/vendor")),
            ),
            (no_remote.path().to_string_lossy().into_owned(), git(None, None)),
            (plain.path().to_string_lossy().into_owned(), Some(None)),
            (
                repo.path().to_string_lossy().into_owned(),
                git(Some("github.com/pingdotgg/t3code"), Some("pingdotgg/t3code")),
            ),
        ]
    );
}

#[tokio::test]
async fn excludes_sandboxes_under_the_configured_worktrees_dir_without_t3_in_the_path() {
    let fixture = Fixture::new();
    // Only the configured prefix can match here: the base dir has no `.t3` segment.
    let worktree_cwd = fixture.base_dir.join("worktrees").join("t3code").join("wt-2");
    fs::create_dir_all(&worktree_cwd).expect("worktree");
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(&worktree_cwd),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(summarize(&result), Vec::new());
}

#[cfg(unix)]
#[tokio::test]
async fn excludes_sandboxes_reached_through_a_symlink_into_the_worktrees_dir() {
    let fixture = Fixture::new();
    let worktree_cwd = fixture.base_dir.join("worktrees").join("t3code").join("wt-3");
    fs::create_dir_all(&worktree_cwd).expect("worktree");
    // The recorded cwd's own spelling looks harmless; only its realpath is managed.
    let links = fixture.workspace("t3code-scanner-links-");
    let symlink_cwd = links.path().join("innocent-project");
    std::os::unix::fs::symlink(&worktree_cwd, &symlink_cwd).expect("symlink");
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &claude_session_line(&symlink_cwd),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(summarize(&result), Vec::new());
}

/// A JSON record padded with leading spaces to exactly `total` bytes (no newline).
fn padded_record(cwd: &Path, total: usize) -> String {
    let record = json!({ "cwd": cwd }).to_string();
    format!("{}{}", " ".repeat(total - record.len()), record)
}

fn opens_in(double: &FsDouble, dir: &Path) -> usize {
    double
        .opens
        .lock()
        .iter()
        .filter(|open| Path::new(open.as_str()).parent() == Some(dir))
        .count()
}

#[tokio::test]
async fn finds_the_cwd_on_a_later_line_when_the_first_records_carry_none() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    // Claude transcripts often open with records that have no cwd.
    let contents = format!(
        "{}\n{}\n{}",
        json!({"type": "file-history-snapshot", "messageId": "m1"}),
        json!({"type": "queue-operation", "operation": "enqueue"}),
        claude_session_line(workspace.path()),
    );
    write_transcript(
        &fixture.claude_transcript("-slug", "a.jsonl"),
        &contents,
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(paths(&result), vec![workspace.path().to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn reads_a_complete_transcript_record_at_the_exact_chunk_boundary() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let record = claude_session_line(workspace.path())
        .split('\n')
        .next()
        .expect("first record")
        .to_owned();
    let prefix = r#"{"padding":""#;
    let suffix = format!(r#"",{}"#, &record[1..]);
    let contents = format!(
        "{prefix}{}{suffix}",
        "x".repeat(32 * 1024 - prefix.len() - suffix.len())
    );
    assert_eq!(contents.len(), 32 * 1024);
    write_transcript(
        &fixture.claude_transcript("-exact", "session.jsonl"),
        &contents,
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(paths(&result), vec![workspace.path().to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn finds_session_metadata_after_a_first_record_larger_than_one_chunk() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    let history = format!(
        "{{\"type\":\"file-history-snapshot\",\"data\":\"{}\"}}\n",
        "x".repeat(32 * 1024)
    );
    write_transcript(
        &fixture.claude_transcript("-large", "session.jsonl"),
        &format!("{history}{}", claude_session_line(workspace.path())),
        ms("2026-01-01T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(paths(&result), vec![workspace.path().to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn shares_metadata_bytes_across_homes_for_64_and_65_one_mib_files() {
    for count in [64usize, 65] {
        let mut fixture = Fixture::new();
        let second_home = temp_dir("t3code-metadata-second-");
        let first_workspace = fixture.workspace("t3code-metadata-first-project-");
        let second_workspace = fixture.workspace("t3code-metadata-second-project-");
        fixture.configure(
            "claude-work",
            instance("claudeAgent", json!({"homePath": second_home.path()})),
        );
        let directories = [
            fixture.claude_home.join("projects").join("p"),
            second_home.path().join("projects").join("p"),
        ];
        let templates = [
            directories[0].join("template.jsonl"),
            directories[1].join("template.jsonl"),
        ];
        let one_mib = 1024 * 1024;
        for (index, (template, workspace)) in templates
            .iter()
            .zip([first_workspace.path(), second_workspace.path()])
            .enumerate()
        {
            write_transcript(
                template,
                &padded_record(workspace, one_mib),
                ms("2026-01-01T00:00:00.000Z") - (index as f64) * 1_000.0,
            );
        }
        let names = |range: std::ops::Range<usize>| -> Vec<String> {
            range.map(|item| format!("session-{item}.jsonl")).collect()
        };
        let mut listings = HashMap::new();
        listings.insert(
            directories[0].to_string_lossy().into_owned(),
            names(0..32),
        );
        listings.insert(
            directories[1].to_string_lossy().into_owned(),
            names(0..count - 32),
        );
        let redirect_dirs: Vec<(String, String)> = directories
            .iter()
            .zip(templates.iter())
            .map(|(dir, template)| {
                (
                    dir.to_string_lossy().into_owned(),
                    template.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let double = fs_double(|double| {
            double.listings = listings;
            double.path_redirect = Some(Box::new(move |path| {
                let parent = Path::new(path).parent()?.to_str()?;
                redirect_dirs
                    .iter()
                    .find(|(dir, _)| dir == parent)
                    .map(|(_, template)| template.clone())
            }));
        });

        let result = fixture
            .scanner_with(double.clone())
            .scan()
            .await
            .expect("scan");

        assert_eq!(
            paths(&result),
            vec![
                first_workspace.path().to_string_lossy().into_owned(),
                second_workspace.path().to_string_lossy().into_owned(),
            ],
            "{count}"
        );
        assert_eq!(
            result.candidates.iter().map(|c| c.thread_count).collect::<Vec<_>>(),
            vec![32, 32],
            "{count}"
        );
        assert_eq!(result.truncated, (count == 65).then_some(true), "{count}");
        assert_eq!(double.opens.lock().len(), 64, "{count}");
        let requests = double.read_requests.lock();
        assert_eq!(requests.iter().sum::<usize>(), 64 * one_mib, "{count}");
        assert_eq!(requests.first(), Some(&(8 * 1024)), "{count}");
        assert_eq!(requests.iter().max(), Some(&(8 * 1024)), "{count}");
    }
}

#[tokio::test]
async fn bounds_metadata_open_and_read_calls_for_short_read_files() {
    for count in [50usize, 51] {
        let fixture = Fixture::new();
        let workspace = fixture.workspace("t3code-short-metadata-project-");
        let directory = fixture.claude_home.join("projects").join("p");
        let template = directory.join("template.jsonl");
        write_transcript(
            &template,
            &padded_record(workspace.path(), 399),
            ms("2026-01-01T00:00:00.000Z"),
        );
        let names: Vec<String> = (0..count).map(|index| format!("session-{index}.jsonl")).collect();
        let template_path = template.to_string_lossy().into_owned();
        let directory_path = directory.clone();
        let double = fs_double(|double| {
            double
                .listings
                .insert(directory.to_string_lossy().into_owned(), names);
            double.path_redirect = Some(Box::new(move |path| {
                (Path::new(path).parent() == Some(directory_path.as_path()))
                    .then(|| template_path.clone())
            }));
            // Every read returns one byte, so each record costs a read per byte.
            double.max_read = Some(1);
        });

        let result = fixture
            .scanner_with(double.clone())
            .scan()
            .await
            .expect("scan");

        let operations = double.opens.lock().len() + double.read_requests.lock().len();
        assert_eq!(operations, 20_000, "{count}");
        assert_eq!(result.candidates[0].thread_count, 50, "{count}");
        assert_eq!(result.truncated, (count == 51).then_some(true), "{count}");
    }
}

#[tokio::test]
async fn bounds_malformed_metadata_records_without_excluding_another_account() {
    let mut fixture = Fixture::new();
    let second_home = temp_dir("t3code-record-metadata-second-");
    let workspace = fixture.workspace("t3code-record-metadata-project-");
    let directory = fixture.claude_home.join("projects").join("p");
    let template = directory.join("template.jsonl");
    write_transcript(&template, &"x\n".repeat(1_001), ms("2026-01-02T00:00:00.000Z"));
    write_transcript(
        &second_home.path().join("projects").join("p").join("session.jsonl"),
        &json!({ "cwd": workspace.path() }).to_string(),
        ms("2026-01-01T00:00:00.000Z"),
    );
    fixture.configure(
        "claude-work",
        instance("claudeAgent", json!({"homePath": second_home.path()})),
    );
    let names: Vec<String> = (0..102).map(|index| format!("session-{index}.jsonl")).collect();
    let template_path = template.to_string_lossy().into_owned();
    let directory_path = directory.clone();
    let double = fs_double(|double| {
        double
            .listings
            .insert(directory.to_string_lossy().into_owned(), names);
        double.path_redirect = Some(Box::new(move |path| {
            (Path::new(path).parent() == Some(directory_path.as_path()))
                .then(|| template_path.clone())
        }));
    });

    let result = fixture
        .scanner_with(double.clone())
        .scan()
        .await
        .expect("scan");

    assert_eq!(paths(&result), vec![workspace.path().to_string_lossy().into_owned()]);
    assert_eq!(opens_in(&double, &directory), 100);
    assert_eq!(result.truncated, Some(true));
}

#[tokio::test]
async fn reports_unfinished_directory_work_for_19_999_and_20_000_project_directories() {
    for count in [19_999usize, 20_000] {
        let fixture = Fixture::new();
        let projects_dir = fixture.claude_home.join("projects");
        let names: Vec<String> = (0..count).map(|index| format!("project-{index}")).collect();
        let double = fs_double(|double| {
            double
                .listings
                .insert(projects_dir.to_string_lossy().into_owned(), names);
            double.empty_children_of = Some(projects_dir.to_string_lossy().into_owned());
        });

        let result = fixture
            .scanner_with(double.clone())
            .scan()
            .await
            .expect("scan");

        assert_eq!(*double.directory_reads.lock(), 20_000, "{count}");
        assert_eq!(summarize(&result), Vec::new(), "{count}");
        assert_eq!(result.truncated, (count == 20_000).then_some(true), "{count}");
    }
}

#[tokio::test]
async fn skips_malformed_transcripts_without_failing_the_scan() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("t3code-workspace-");
    write_transcript(
        &fixture.claude_transcript("-broken", "a.jsonl"),
        "not json at all\n",
        ms("2026-05-01T00:00:00.000Z"),
    );
    // Valid JSON, but no cwd anywhere in the record.
    write_transcript(
        &fixture.claude_transcript("-no-cwd", "a.jsonl"),
        "{\"type\":\"summary\"}\n",
        ms("2026-05-02T00:00:00.000Z"),
    );
    write_transcript(
        &fixture.claude_transcript("-good", "a.jsonl"),
        &claude_session_line(workspace.path()),
        ms("2026-05-03T00:00:00.000Z"),
    );

    let result = fixture.scan().await;

    assert_eq!(
        summarize(&result),
        vec![summary(
            &workspace.path().to_string_lossy(),
            &base_name(workspace.path()),
            &["claudeAgent"],
            1,
            Some("2026-05-03T00:00:00.000Z"),
        )]
    );
}

#[tokio::test]
async fn returns_an_empty_result_when_neither_home_directory_exists() {
    let mut fixture = Fixture::new();
    fixture.claude_home = fixture.home_dir.join("no-claude");
    fixture.codex_home = fixture.home_dir.join("no-codex");
    fixture.configure(
        "claudeAgent",
        instance("claudeAgent", json!({"homePath": fixture.claude_home})),
    );
    fixture.configure(
        "codex",
        instance("codex", json!({"homePath": fixture.codex_home})),
    );

    let result = fixture.scan().await;

    assert_eq!(summarize(&result), Vec::new());
    assert!(
        result.scanned_at.len() > 10 && result.scanned_at.as_bytes()[4] == b'-',
        "scannedAt {}",
        result.scanned_at
    );
}

#[tokio::test]
async fn counts_terminal_newlines_correctly_with_record_overflow() {
    for overflow in [false, true] {
        let mut fixture = Fixture::new();
        let now = ms("2026-08-24T12:00:00.000Z");
        fixture.now_ms = Some(now);
        let workspace = fixture.workspace("t3code-record-limit-project-");
        let directory = fixture.codex_home.join("sessions").join("2026").join("08").join("24");
        write_transcript(
            &directory.join("rollout-records.jsonl"),
            &make_record_limit_transcript(workspace.path(), overflow),
            now,
        );
        write_transcript(
            &directory.join("rollout-older.jsonl"),
            &[
                json!({
                    "type": "session_meta",
                    "payload": {"id": "older-session", "cwd": workspace.path()},
                })
                .to_string(),
                json!({
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": "Older prompt"},
                })
                .to_string(),
            ]
            .join("\n"),
            now - 1_000.0,
        );

        let outcomes = fixture
            .outcomes(workspace.path())
            .await;

        let tags: Vec<&str> = outcomes.iter().map(outcome_tag).collect();
        assert_eq!(
            tags,
            if overflow {
                vec!["Skipped", "Importable"]
            } else {
                vec!["Importable", "Skipped"]
            },
            "overflow={overflow}"
        );
        let messages: Vec<String> = outcomes
            .iter()
            .flat_map(|outcome| match outcome {
                AgentSessionRecentThread::Importable { thread, .. } => thread
                    .messages
                    .iter()
                    .map(|message| message.text.clone())
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(
            messages,
            vec![if overflow { "Older prompt" } else { "First prompt" }.to_owned()],
            "overflow={overflow}"
        );
    }
}

fn outcome_tag(outcome: &AgentSessionRecentThread) -> &'static str {
    match outcome {
        AgentSessionRecentThread::Importable { .. } => "Importable",
        AgentSessionRecentThread::AlreadyImported { .. } => "AlreadyImported",
        AgentSessionRecentThread::Duplicate { .. } => "Duplicate",
        AgentSessionRecentThread::Skipped => "Skipped",
    }
}

/// `MAX_IMPORT_RECORDS` newline-terminated records, with one more when `overflow`.
fn make_record_limit_transcript(cwd: &Path, overflow: bool) -> String {
    let head = [
        json!({"type": "session_meta", "payload": {"id": "record-limit-session", "cwd": cwd}})
            .to_string(),
        json!({"type": "event_msg", "payload": {"type": "user_message", "message": "First prompt"}})
            .to_string(),
    ]
    .join("\n");
    let mut transcript = format!("{head}\n{}", "{}\n".repeat(99_998));
    if overflow {
        transcript.push_str(&format!(
            "\n{}\n",
            json!({"type": "event_msg", "payload": {"type": "user_message", "message": "Overflow prompt"}})
        ));
    }
    transcript
}
