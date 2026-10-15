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
    ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
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
}

impl Fixture {
    fn new() -> Self {
        let root = temp_dir("t3code-scanner-");
        let claude_home = root.path().join("claude-home");
        let codex_home = root.path().join("codex-home");
        let base_dir = root.path().join("base");
        let home_dir = root.path().join("home");
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
        AgentSessionDiscovery::new(
            self.host(),
            fs,
            Arc::new(Settings(settings)),
            Arc::new(Shells(
                self.imported_roots
                    .iter()
                    .map(|root| project_shell(root))
                    .collect(),
            )),
        )
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
/// instrumented `FileSystem` override: scripted listings, stat aliasing,
/// open counts, and short reads.
#[derive(Default)]
struct FsDouble {
    inner: LocalScannerFileSystem,
    /// `stat(path)` answers for `path` as if it were this other path.
    stat_redirects: HashMap<String, String>,
    /// Scripted `readDirectory` answers, counted in `directory_reads`.
    listings: HashMap<String, Vec<String>>,
    /// Every directory whose parent is this path reads as empty and is counted.
    empty_children_of: Option<String>,
    directory_reads: Mutex<usize>,
    opens: Mutex<Vec<String>>,
    read_calls: Arc<Mutex<usize>>,
    /// Caps every `read_alloc` request, forcing short reads.
    max_read: Option<usize>,
}

impl FsDouble {
    fn opened(&self, path: &Path) -> usize {
        let target = path.to_string_lossy();
        self.opens.lock().iter().filter(|open| **open == target).count()
    }
}

#[async_trait]
impl ScannerFileSystem for FsDouble {
    async fn stat(&self, path: &str) -> Option<FileStat> {
        let target = self.stat_redirects.get(path).map_or(path, String::as_str);
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
        let file = self.inner.open(path).await?;
        Some(Box::new(CountingFile {
            inner: file,
            reads: Arc::clone(&self.read_calls),
            max_read: self.max_read,
        }))
    }
}

struct CountingFile {
    inner: Box<dyn ScannerFile>,
    reads: Arc<Mutex<usize>>,
    max_read: Option<usize>,
}

#[async_trait]
impl ScannerFile for CountingFile {
    async fn stat(&self) -> Option<FileStat> {
        self.inner.stat().await
    }
    async fn read_alloc(&mut self, size: usize) -> std::io::Result<Option<Vec<u8>>> {
        *self.reads.lock() += 1;
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
