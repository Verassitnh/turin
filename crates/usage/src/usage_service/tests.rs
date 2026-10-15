//! Port of the Claude and Codex cases in upstream/apps/server/src/usage/UsageService.test.ts.
//!
//! The suite runs against fakes for everything the composition root owns: the
//! transcript readers (directory resolution per instance, env and config
//! precedence) and the LiteLLM rate endpoint (a local HTTP server that counts
//! fetches and can hold a response). Cursor, Pi, Grok, OpenCode and Antigravity
//! cases are not ported: their readers are not in this crate (see PORTING.md).

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::FutureExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use turin_contracts::usage::{UsageResolution, UsageSource, UsageSourceStatus};
use turin_provider_core::rt::boxed;
use turin_provider_core::usage::TranscriptUsageFormat;

use super::*;
use crate::test_formats::{claude_format, codex_format};

const BEFORE_NARROW_WINDOW: &str = "2026-08-01T10:00:00Z";

fn window() -> UsageSummaryInput {
    input("2026-07-31", "2026-08-02")
}

fn narrow_window() -> UsageSummaryInput {
    input("2026-09-01", "2026-09-02")
}

fn input(since: &str, until: &str) -> UsageSummaryInput {
    UsageSummaryInput {
        since_day: UsageDay::new(since).unwrap(),
        until_day: UsageDay::new(until).unwrap(),
        time_zone: TrimmedNonEmptyString::new("UTC").unwrap(),
        resolution: None,
        since_time: None,
        until_time: None,
        await_refresh: None,
    }
}

fn settings(value: Value) -> ServerSettings {
    serde_json::from_value(value).expect("settings decode")
}

fn env_var(name: &str, value: &Path) -> Value {
    json!({ "name": name, "value": value.to_string_lossy(), "sensitive": false })
}

/// Settings with the default Claude and Codex accounts, both at their home dirs.
fn default_settings(home: &Path) -> ServerSettings {
    settings(json!({
        "providerInstances": {
            "claudeAgent": { "driver": "claudeAgent", "config": { "homePath": home.join("claude") } },
            "codex": { "driver": "codex", "config": { "homePath": home.join("codex") } },
        },
    }))
}

fn total_output(summary: &UsageSummary) -> u64 {
    summary
        .buckets
        .iter()
        .map(|bucket| bucket.totals.output_tokens)
        .sum()
}

fn total_cost(summary: &UsageSummary) -> f64 {
    summary.buckets.iter().map(|bucket| bucket.cost_usd).sum()
}

fn ok_sources(summary: &UsageSummary) -> Vec<&UsageSource> {
    summary
        .sources
        .iter()
        .filter(|source| source.status == UsageSourceStatus::Ok)
        .collect()
}

fn records(summary: &UsageSummary) -> u64 {
    summary.buckets.iter().map(|bucket| bucket.records).sum()
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0),
        "{actual} != {expected}"
    );
}

fn claude_line(id: u32, output_tokens: u64) -> String {
    claude_line_with(id, output_tokens, "claude-fable-5")
}

fn claude_line_with(id: u32, output_tokens: u64, model: &str) -> String {
    format!(
        "{}\n",
        json!({
            "type": "assistant",
            "timestamp": "2026-08-01T10:00:00Z",
            "requestId": format!("req_{id}"),
            "sessionId": "session-1",
            "message": {
                "id": format!("msg_{id}"),
                "model": model,
                "usage": { "input_tokens": 10, "output_tokens": output_tokens },
            },
        })
    )
}

/// A Codex rollout of one session whose token events all carry the same timestamp.
fn codex_rollout(session: &str, outputs: &[u64]) -> String {
    let mut lines = vec![
        json!({ "type": "session_meta", "payload": { "id": session } }),
        json!({ "type": "turn_context", "payload": { "model": "gpt-5.6-sol" } }),
    ];
    lines.extend(outputs.iter().map(|output| {
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-01T10:00:00Z",
            "payload": {
                "type": "token_count",
                "info": { "last_token_usage": { "input_tokens": 10, "output_tokens": output } },
            },
        })
    }));
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// A Codex rollout on a service tier, with one token event.
fn codex_tiered_rollout(session: &str, model: &str, tier: &str, output: u64) -> String {
    [
        json!({ "type": "session_meta", "payload": { "id": session } }),
        json!({ "type": "turn_context", "payload": { "model": model } }),
        json!({
            "type": "event_msg",
            "payload": { "type": "thread_settings_applied", "thread_settings": { "service_tier": tier } },
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-01T10:00:00Z",
            "payload": {
                "type": "token_count",
                "info": { "last_token_usage": { "input_tokens": 0, "output_tokens": output } },
            },
        }),
    ]
    .iter()
    .map(|line| format!("{line}\n"))
    .collect()
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn append_file(path: &Path, contents: &str) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
}

/// Replaces a file by rename, so a scan holding the old one keeps reading it.
fn replace_file(path: &Path, contents: &str) {
    let next = PathBuf::from(format!("{}.next", path.display()));
    std::fs::write(&next, contents).unwrap();
    std::fs::rename(next, path).unwrap();
}

fn set_mtime(path: &Path, seconds: i64) {
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let times = [libc::timeval {
        tv_sec: seconds as _,
        tv_usec: 0,
    }; 2];
    assert_eq!(unsafe { libc::utimes(c_path.as_ptr(), times.as_ptr()) }, 0);
}

fn epoch_seconds(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text)
        .unwrap()
        .timestamp()
}

/// A FIFO named like a transcript. A scan's read of it waits in `open` until a
/// writer opens it, so a gate holds that scan's read in flight.
fn make_gate(path: &Path, last_write: Option<&str>) {
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
    if let Some(last_write) = last_write {
        set_mtime(path, epoch_seconds(last_write));
    }
}

/// Returns once a scan has opened the gate.
async fn open_gate(path: &Path) {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _ = std::fs::OpenOptions::new().write(true).open(path);
    })
    .await
    .unwrap();
}

/// Reads the directory's entries in the order a walk sees them.
fn walk_order(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect()
}

/// Resolves a reader's root the way a provider does: explicit account home,
/// then the instance's environment (host env merged in), then the default home.
fn root_of(instance: &ProviderUsageInstance, env_key: &str, default_root: &Path) -> PathBuf {
    if let Some(home) = instance
        .config
        .as_ref()
        .and_then(|config| config.get("homePath"))
        .and_then(Value::as_str)
        .filter(|home| !home.is_empty())
    {
        return PathBuf::from(home);
    }
    if let Some(home) = instance
        .environment
        .get(env_key)
        .filter(|home| !home.is_empty())
    {
        return PathBuf::from(home);
    }
    default_root.to_path_buf()
}

fn transcript_reader(
    driver_kind: &str,
    provider: UsageProviderKind,
    format: TranscriptUsageFormat,
    env_key: &'static str,
    subdir: &'static str,
    default_root: PathBuf,
) -> UsageReader {
    UsageReader {
        driver_kind: driver_kind.to_owned(),
        reader: ProviderUsageReader::Transcripts {
            provider,
            format,
            directories: Arc::new(move |instance: ProviderUsageInstance| {
                let root = root_of(&instance, env_key, &default_root);
                boxed(async move {
                    vec![TranscriptDirectory {
                        dir: root.join(subdir).to_string_lossy().into_owned(),
                        file_name: None,
                    }]
                })
            }),
        },
    }
}

fn claude_reader(home: &Path) -> UsageReader {
    transcript_reader(
        "claudeAgent",
        UsageProviderKind::Claude,
        claude_format(),
        "CLAUDE_CONFIG_DIR",
        "projects",
        home.join("claude"),
    )
}

fn codex_reader(home: &Path) -> UsageReader {
    transcript_reader(
        "codex",
        UsageProviderKind::Codex,
        codex_format(),
        "CODEX_HOME",
        "sessions",
        home.join("codex"),
    )
}

fn dead_rates_url() -> String {
    "http://127.0.0.1:9/rates.json".to_owned()
}

fn service_with(
    state_dir: &Path,
    readers: Vec<UsageReader>,
    host_env: &[(&str, PathBuf)],
    rates_url: String,
    clock: Clock,
) -> UsageService {
    UsageService::new(UsageServiceConfig {
        state_dir: state_dir.to_path_buf(),
        readers,
        host_env: host_env
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string_lossy().into_owned()))
            .collect(),
        home_dir: state_dir.to_string_lossy().into_owned(),
        rates_url,
        clock,
    })
}

/// The usual service: Claude and Codex readers, no host environment, the
/// system clock, and a rate endpoint nothing listens on.
fn default_service(state_dir: &Path, home: &Path) -> UsageService {
    service_with(
        state_dir,
        vec![claude_reader(home), codex_reader(home)],
        &[],
        dead_rates_url(),
        system_clock(),
    )
}

/// Writes a fresh rate table, so a scan does not fetch.
fn seed_rates(state_dir: &Path, document: Value) {
    write_file(
        &state_dir.join(RATES_CACHE_FILE_NAME),
        json!({ "fetchedAtMs": now_ms(), "document": document }).to_string(),
    );
}

fn fable_rates() -> Value {
    json!({ "claude-fable-5": { "input_cost_per_token": 1e-5, "output_cost_per_token": 5e-5 } })
}

/// A rate endpoint on localhost that counts requests. A held endpoint keeps
/// every response until `release`, so a test can hold a scan in flight.
struct RatesServer {
    url: String,
    hits: watch::Receiver<usize>,
    release: watch::Sender<bool>,
    _accept: AbortOnDrop,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl RatesServer {
    async fn start(body: Value, held: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/rates.json", listener.local_addr().unwrap());
        let body = body.to_string();
        let (hits_tx, hits) = watch::channel(0usize);
        let hits_tx = Arc::new(hits_tx);
        let (release, release_rx) = watch::channel(!held);
        let accept = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.clone();
                let hits_tx = hits_tx.clone();
                let mut release = release_rx.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buf[..read]),
                        }
                    }
                    hits_tx.send_modify(|hits| *hits += 1);
                    let _ = release.wait_for(|open| *open).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            url,
            hits,
            release,
            _accept: AbortOnDrop(accept),
        }
    }

    fn hits(&self) -> usize {
        *self.hits.borrow()
    }

    async fn wait_for_hits(&self, count: usize) {
        let mut hits = self.hits.clone();
        hits.wait_for(|hits| *hits >= count).await.unwrap();
    }

    fn release(&self) {
        self.release.send_replace(true);
    }
}

/// A clock the test moves forward.
fn movable_clock(start_ms: f64) -> (Arc<AtomicU64>, Clock) {
    let now = Arc::new(AtomicU64::new(start_ms as u64));
    let reader = now.clone();
    (now, Arc::new(move || reader.load(Ordering::SeqCst) as f64))
}

// ── Basic reads (the cases the service unit tests started with) ─────────────

#[tokio::test]
async fn reads_and_prices_a_transcript_directory() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line(1, 5) + &claude_line(2, 7),
    );
    seed_rates(&state, fable_rates());

    let summary = default_service(&state, &home)
        .read_summary(&window(), &default_settings(&home))
        .await
        .unwrap();

    assert_eq!(summary.buckets.len(), 1);
    let bucket = &summary.buckets[0];
    assert_eq!(bucket.records, 2);
    assert_eq!(bucket.totals.output_tokens, 12);
    // Two records of 10 input and 5 or 7 output tokens: 2 * 10 * 1e-5 + 12 * 5e-5.
    assert_eq!(
        bucket.cost_source,
        turin_contracts::usage::UsageCostSource::ModelPriced
    );
    close(bucket.cost_usd, 8e-4);
    assert_eq!(summary.sources.len(), 2);
    let claude = summary
        .sources
        .iter()
        .find(|source| source.fingerprint.provider == UsageProviderKind::Claude)
        .unwrap();
    assert_eq!(claude.status, UsageSourceStatus::Ok);
    assert_eq!(claude.scanned_files, 1);
    assert_eq!(claude.distinct_sessions, 1);
    assert_eq!(summary.pricing.status, UsagePricingStatus::Cached);
    assert_eq!(summary.pricing.known_models, 1);
}

#[tokio::test]
async fn a_warm_scan_sees_only_appended_lines_of_a_grown_file() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let file = home.join("claude/projects/proj/session.jsonl");
    write_file(&file, claude_line(1, 5));
    seed_rates(&state, fable_rates());
    let service = default_service(&state, &home);
    let settings = default_settings(&home);

    let cold = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(records(&cold), 1);
    append_file(&file, &claude_line(2, 9));
    let warm = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(records(&warm), 2);
    assert_eq!(total_output(&warm), 14);
}

#[tokio::test]
async fn drops_a_message_copied_into_another_transcript() {
    // Resumed and forked sessions copy a message's records forward; the
    // global dedupe pass keeps one.
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/a.jsonl"),
        claude_line(1, 5),
    );
    write_file(
        &home.join("claude/projects/proj/b.jsonl"),
        claude_line(1, 5),
    );
    seed_rates(&state, fable_rates());

    let summary = default_service(&state, &home)
        .read_summary(&window(), &default_settings(&home))
        .await
        .unwrap();
    assert_eq!(records(&summary), 1);
    assert_eq!(total_output(&summary), 5);
}

#[tokio::test]
async fn persists_the_scan_cache_for_the_next_start() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line(1, 5),
    );
    seed_rates(&state, fable_rates());
    let settings = default_settings(&home);

    let first = default_service(&state, &home);
    let cold = first.read_summary(&window(), &settings).await.unwrap();
    first.await_persisted().await;
    assert!(state.join(SCAN_CACHE_FILE_NAME).exists());

    let restarted = default_service(&state, &home)
        .read_summary(&window(), &settings)
        .await
        .unwrap();
    assert_eq!(restarted.buckets, cold.buckets);
}

#[tokio::test]
async fn reports_a_missing_directory_as_a_missing_source() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    seed_rates(&state, fable_rates());
    let summary = default_service(&state, &home)
        .read_summary(&window(), &default_settings(&home))
        .await
        .unwrap();

    assert!(summary.buckets.is_empty());
    assert!(
        summary
            .sources
            .iter()
            .all(|source| source.status == UsageSourceStatus::Missing)
    );
    assert_eq!(
        summary.sources[0]
            .message
            .as_ref()
            .map(|message| message.as_str()),
        Some("No transcript directory on this environment.")
    );
}

#[tokio::test]
async fn rejects_an_inverted_window() {
    let root = tempfile::tempdir().unwrap();
    let error = default_service(&root.path().join("state"), root.path())
        .read_summary(
            &input("2026-08-31", "2026-08-01"),
            &default_settings(root.path()),
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&error), UsageReadErrorReason::InvalidWindow);
}

#[tokio::test]
async fn rejects_an_hourly_window_without_instants_or_longer_than_a_day() {
    let root = tempfile::tempdir().unwrap();
    let service = default_service(&root.path().join("state"), root.path());
    let settings = default_settings(root.path());

    let mut hourly = input("2026-08-01", "2026-08-01");
    hourly.resolution = Some(UsageResolution::Hour);
    let error = service.read_summary(&hourly, &settings).await.unwrap_err();
    assert_eq!(reason(&error), UsageReadErrorReason::InvalidWindow);

    hourly.since_time = Some(TrimmedNonEmptyString::new("2026-08-01T00:00:00Z").unwrap());
    hourly.until_time = Some(TrimmedNonEmptyString::new("2026-08-03T00:00:00Z").unwrap());
    let error = service.read_summary(&hourly, &settings).await.unwrap_err();
    assert_eq!(reason(&error), UsageReadErrorReason::InvalidWindow);
}

fn reason(error: &UsageReadError) -> UsageReadErrorReason {
    match error {
        UsageReadError::ReadError { reason, .. } => *reason,
    }
}

// ── Accounts, directories, and settings ─────────────────────────────────────

#[tokio::test]
async fn reads_a_directory_shared_by_two_instances_once() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let sessions = home.join("shared-codex/sessions");
    write_file(
        &sessions.join("codex-rollout.jsonl"),
        codex_rollout("codex", &[17]),
    );
    write_file(
        &sessions.join("codex-personal-rollout.jsonl"),
        codex_rollout("codex-personal", &[23]),
    );
    seed_rates(&state, fable_rates());

    let settings = settings(json!({
        "providerInstances": {
            "codex": { "driver": "codex", "config": { "homePath": home.join("shared-codex") } },
            "codex-personal": {
                "driver": "codex",
                "enabled": false,
                "config": {
                    "homePath": home.join("shared-codex"),
                    "shadowHomePath": home.join("personal-shadow"),
                },
                "environment": [env_var("CODEX_HOME", &home.join("ignored-environment"))],
            },
        },
    }));
    let summary = service_with(
        &state,
        vec![codex_reader(&home)],
        &[],
        dead_rates_url(),
        system_clock(),
    )
    .read_summary(&window(), &settings)
    .await
    .unwrap();

    assert_eq!(total_output(&summary), 40);
    assert_eq!(ok_sources(&summary).len(), 1);
}

#[tokio::test]
async fn reads_configured_and_disabled_accounts_once_across_shared_and_aliased_homes() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let codex_account = home.join("codex-account");
    let codex_alias = home.join("codex-alias");
    let claude_account = home.join("claude-account");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line(1, 5),
    );
    write_file(
        &claude_account.join("projects/session.jsonl"),
        claude_line(2, 7),
    );
    write_file(
        &codex_account.join("sessions/rollout.jsonl"),
        // A-B-A at one timestamp must preserve both equal A events.
        codex_rollout("codex-account-session", &[11, 12, 11]),
    );
    std::os::unix::fs::symlink(&codex_account, &codex_alias).unwrap();
    seed_rates(&state, fable_rates());

    let settings = settings(json!({
        "providerInstances": {
            "claudeAgent": { "driver": "claudeAgent", "config": { "homePath": home.join("claude") } },
            "claude-work": {
                "driver": "claudeAgent",
                "enabled": false,
                "environment": [env_var("CLAUDE_CONFIG_DIR", &claude_account)],
            },
            "codex-work": {
                "driver": "codex",
                "environment": [env_var("CODEX_HOME", &codex_account)],
            },
            "codex-alias": { "driver": "codex", "config": { "homePath": codex_alias } },
            "codex-shadow": {
                "driver": "codex",
                "config": { "homePath": codex_account, "shadowHomePath": home.join("shadow") },
                "environment": [env_var("CODEX_HOME", &home.join("ignored"))],
            },
        },
    }));
    let service = service_with(
        &state,
        vec![claude_reader(&home), codex_reader(&home)],
        &[],
        dead_rates_url(),
        system_clock(),
    );
    let summary = service.read_summary(&window(), &settings).await.unwrap();

    assert_eq!(total_output(&summary), 5 + 7 + 11 + 12 + 11);
    let ok = ok_sources(&summary);
    assert_eq!(ok.len(), 3);
    assert_eq!(ok.iter().map(|source| source.scanned_files).sum::<u64>(), 3);
    assert_eq!(
        ok.iter()
            .filter(|source| source.fingerprint.provider == UsageProviderKind::Codex)
            .count(),
        1
    );

    // A moved rollout is the same usage, not a second copy.
    std::fs::rename(
        codex_account.join("sessions/rollout.jsonl"),
        codex_account.join("sessions/moved.jsonl"),
    )
    .unwrap();
    let moved = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(moved.buckets, summary.buckets);
    std::fs::remove_dir_all(codex_account.join("sessions")).unwrap();
    let removed = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(removed.buckets, summary.buckets);
}

#[tokio::test]
async fn uses_explicit_account_settings_before_environment_and_default_homes_then_refreshes_them() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let configured = home.join("configured");
    let environment_home = home.join("environment");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line(1, 100),
    );
    write_file(
        &configured.join("projects/session.jsonl"),
        claude_line(2, 7),
    );
    write_file(
        &environment_home.join("projects/session.jsonl"),
        claude_line(3, 8),
    );
    write_file(
        &configured.join(".claude/projects/wrong.jsonl"),
        claude_line(4, 1000),
    );
    seed_rates(&state, fable_rates());

    let service = service_with(
        &state,
        vec![claude_reader(&home)],
        &[("CLAUDE_CONFIG_DIR", home.join("host-ignored"))],
        dead_rates_url(),
        system_clock(),
    );
    let explicit = settings(json!({
        "providerInstances": {
            "claudeAgent": {
                "driver": "claudeAgent",
                "config": { "homePath": configured },
                "environment": [env_var("CLAUDE_CONFIG_DIR", &environment_home)],
            },
        },
    }));
    let first = service.read_summary(&window(), &explicit).await.unwrap();
    assert_eq!(total_output(&first), 7);
    let configured_projects = std::fs::canonicalize(configured.join("projects")).unwrap();
    assert!(first.sources.iter().any(|source| Path::new(
        source.fingerprint.resolved_home_path.as_str()
    ) == configured_projects));

    let environment_only = settings(json!({
        "providerInstances": {
            "claudeAgent": {
                "driver": "claudeAgent",
                "config": { "homePath": "" },
                "environment": [env_var("CLAUDE_CONFIG_DIR", &environment_home)],
            },
        },
    }));
    let second = service
        .read_summary(&window(), &environment_only)
        .await
        .unwrap();
    assert_eq!(total_output(&second), 8);
    let environment_projects = std::fs::canonicalize(environment_home.join("projects")).unwrap();
    assert!(second.sources.iter().any(|source| Path::new(
        source.fingerprint.resolved_home_path.as_str()
    ) == environment_projects));
}

#[tokio::test]
async fn uses_inherited_home_variables_when_explicit_default_accounts_have_no_home_settings() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line(1, 5),
    );
    seed_rates(&state, fable_rates());

    let service = service_with(
        &state,
        vec![claude_reader(&home), codex_reader(&home)],
        &[
            ("CODEX_HOME", home.join("inherited-codex")),
            ("CLAUDE_CONFIG_DIR", home.join("claude")),
        ],
        dead_rates_url(),
        system_clock(),
    );
    let settings = settings(json!({
        "providerInstances": {
            "codex": { "driver": "codex", "config": {} },
            "claudeAgent": { "driver": "claudeAgent", "config": {} },
        },
    }));
    let summary = service.read_summary(&window(), &settings).await.unwrap();

    assert_eq!(total_output(&summary), 5);
    let codex = summary
        .sources
        .iter()
        .find(|source| source.fingerprint.provider == UsageProviderKind::Codex)
        .unwrap();
    assert_eq!(
        codex.fingerprint.resolved_home_path.as_str(),
        home.join("inherited-codex/sessions").to_string_lossy()
    );
}

// ── Partial reads, pricing and appends ──────────────────────────────────────

#[tokio::test]
async fn reports_unreadable_transcripts_as_partial_and_recovers_once_they_can_be_read() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        // Root reads mode-0 files, so there is nothing to test.
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let transcript = home.join("claude/projects/proj/session.jsonl");
    let unreadable = transcript.with_file_name("other.jsonl");
    write_file(&transcript, claude_line(1, 5));
    write_file(&unreadable, claude_line(2, 7));
    std::fs::set_permissions(
        &unreadable,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    seed_rates(&state, fable_rates());

    let service = default_service(&state, &home);
    let settings = default_settings(&home);
    let partial = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(total_output(&partial), 5);
    let claude = |summary: &UsageSummary| {
        summary
            .sources
            .iter()
            .find(|source| source.fingerprint.provider == UsageProviderKind::Claude)
            .cloned()
            .unwrap()
    };
    assert_eq!(claude(&partial).status, UsageSourceStatus::Partial);

    std::fs::set_permissions(
        &unreadable,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let healthy = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(total_output(&healthy), 12);
    assert_eq!(claude(&healthy).status, UsageSourceStatus::Ok);
    assert!(claude(&healthy).message.is_none());
}

#[tokio::test]
async fn reprices_unchanged_transcripts_when_custom_prices_are_added_edited_or_removed() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line_with(1, 5, "example-model"),
    );
    let service = default_service(&state, &home);
    let base = default_settings(&home);
    let with_prices = |prices: Value| {
        let mut value = serde_json::to_value(&base).unwrap();
        value["usagePriceOverrides"] = prices;
        settings(value)
    };

    let original = service.read_summary(&window(), &base).await.unwrap();
    assert_eq!(original.buckets[0].cost_usd, 0.0);
    assert_eq!(original.buckets[0].unpriced_records, 1);

    let overridden = service
        .read_summary(
            &window(),
            &with_prices(json!({ "example-model": { "inputCostPerMillionTokens": 2, "outputCostPerMillionTokens": 8 } })),
        )
        .await
        .unwrap();
    close(overridden.buckets[0].cost_usd, 0.00006);
    assert_eq!(
        overridden.buckets[0].cost_source,
        turin_contracts::usage::UsageCostSource::ModelPriced
    );
    assert_eq!(overridden.buckets[0].unpriced_records, 0);
    assert_eq!(overridden.buckets[0].totals, original.buckets[0].totals);

    let edited = service
        .read_summary(
            &window(),
            &with_prices(json!({ "example-model": { "inputCostPerMillionTokens": 4, "outputCostPerMillionTokens": 16 } })),
        )
        .await
        .unwrap();
    close(edited.buckets[0].cost_usd, 0.00012);

    let restored = service.read_summary(&window(), &base).await.unwrap();
    assert_eq!(restored.buckets, original.buckets);
}

#[tokio::test]
async fn counts_appended_usage_on_a_rescan_of_a_grown_transcript() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let transcript = home.join("claude/projects/proj/session.jsonl");
    write_file(&transcript, claude_line(1, 5));
    let service = default_service(&state, &home);
    let settings = default_settings(&home);

    let first = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(total_output(&first), 5);
    append_file(&transcript, &claude_line(2, 7));
    let second = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(total_output(&second), 12);
}

#[tokio::test]
async fn keeps_large_record_totals_and_costs_exact_through_append_dedupe_restart_and_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let transcript = home.join("claude/projects/proj/session.jsonl");
    let large = claude_line(1, 9900).replace(
        "\"message\":",
        &format!(
            "\"padding\":{},\"message\":",
            json!("x".repeat(9 * 1024 * 1024))
        ),
    );
    write_file(&transcript, &large);
    seed_rates(&state, fable_rates());
    let settings = default_settings(&home);

    let service = default_service(&state, &home);
    let first = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(total_output(&first), 9900);
    close(total_cost(&first), 0.4951);
    let warm = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(warm.buckets, first.buckets);

    // The repeated content block has the same message identity, so it dedupes.
    append_file(&transcript, &(large.clone() + &claude_line(2, 100)));
    let appended = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(total_output(&appended), 10000);
    assert_eq!(
        appended
            .buckets
            .iter()
            .map(|bucket| bucket.totals.uncached_input_tokens)
            .sum::<u64>(),
        20
    );

    service.await_persisted().await;
    let restarted = default_service(&state, &home);
    assert_eq!(
        restarted
            .read_summary(&window(), &settings)
            .await
            .unwrap()
            .buckets,
        appended.buckets
    );
    restarted.await_persisted().await;
    std::fs::remove_file(&transcript).unwrap();
    let after_cleanup = default_service(&state, &home)
        .read_summary(&window(), &settings)
        .await
        .unwrap();
    assert_eq!(after_cleanup.buckets, appended.buckets);
}

#[tokio::test]
async fn upgrades_a_v4_cache_by_repricing_live_codex_tiers_and_keeping_deleted_rollouts() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let sessions = home.join("codex/sessions");
    write_file(
        &sessions.join("live.jsonl"),
        codex_tiered_rollout("live", "gpt-6-astra", "ultrafast", 10),
    );
    write_file(
        &sessions.join("deleted.jsonl"),
        codex_tiered_rollout("deleted", "gpt-6-astra", "ultrafast", 20),
    );
    seed_rates(
        &state,
        json!({
            "gpt-6-astra": {
                "input_cost_per_token": 0,
                "output_cost_per_token": 1,
                "input_cost_per_token_ultrafast": 0,
                "output_cost_per_token_ultrafast": 6,
            },
        }),
    );
    let settings = default_settings(&home);

    let first = default_service(&state, &home);
    first.read_summary(&window(), &settings).await.unwrap();
    first.await_persisted().await;

    // Rewrite the cache as a v4 server left it: every Codex record at speed 0
    // (standard), and no tier in the reducer state.
    let cache_path = state.join(SCAN_CACHE_FILE_NAME);
    let legacy_path = state.join(LEGACY_SCAN_CACHE_FILE_NAME);
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&cache_path).unwrap()).unwrap();
    for file in document["files"].as_object_mut().unwrap().values_mut() {
        for row in file["r"].as_array_mut().unwrap() {
            let row = row.as_array_mut().unwrap();
            row.truncate(10);
            row.push(json!(0));
        }
        if let Some(state) = file["cs"].as_object_mut() {
            state.remove("speed");
        }
    }
    document["version"] = json!(4);
    let legacy = document.to_string();
    write_file(&legacy_path, &legacy);
    std::fs::remove_file(&cache_path).unwrap();
    std::fs::remove_file(sessions.join("deleted.jsonl")).unwrap();

    let summary = default_service(&state, &home)
        .read_summary(&window(), &settings)
        .await
        .unwrap();
    // The live rollout re-parses at the ultrafast rate (10 x 6); the deleted one
    // keeps its saved v4 usage at the standard rate (20 x 1).
    assert_eq!(total_output(&summary), 30);
    close(total_cost(&summary), 80.0);
    // A v4 server sharing this state directory still finds its own cache.
    assert_eq!(std::fs::read_to_string(&legacy_path).unwrap(), legacy);
}

#[tokio::test]
async fn preserves_saved_tokens_costs_and_sessions_after_transcript_cleanup_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let alias = home.join("claude-alias");
    let transcript = home.join("claude/projects/proj/session.jsonl");
    std::fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(home.join("claude"), &alias).unwrap();
    let content = claude_line(1, 5);
    write_file(&transcript, &content);
    seed_rates(&state, fable_rates());
    let settings = settings(json!({
        "providerInstances": {
            "claudeAgent": { "driver": "claudeAgent", "config": { "homePath": alias } },
        },
    }));
    let reader = |service_state: &Path| {
        service_with(
            service_state,
            vec![claude_reader(&home)],
            &[],
            dead_rates_url(),
            system_clock(),
        )
    };

    let service = reader(&state);
    let first = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(total_output(&first), 5);
    assert!(first.buckets[0].cost_usd > 0.0);

    std::fs::remove_file(&transcript).unwrap();
    let deleted = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(deleted.buckets, first.buckets);
    assert_eq!(deleted.sources, first.sources);

    service.await_persisted().await;
    let restarted = reader(&state);
    let restored = restarted.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(restored.buckets, first.buckets);
    assert_eq!(restored.sources, first.sources);

    // A moved transcript must not count the saved usage twice.
    write_file(&transcript.with_file_name("session.jsonl.jsonl"), &content);
    let moved = restarted.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(moved.buckets, first.buckets);
    assert_eq!(moved.sources[0].distinct_sessions, 1);
    restarted.await_persisted().await;

    let replacement = home.join("replacement-projects");
    std::fs::create_dir_all(&replacement).unwrap();
    std::fs::remove_dir_all(home.join("claude/projects")).unwrap();
    let after_root_cleanup = reader(&state);
    let missing_root = after_root_cleanup
        .read_summary(&window(), &settings)
        .await
        .unwrap();
    assert_eq!(missing_root.buckets, first.buckets);
    assert_eq!(missing_root.sources[0].distinct_sessions, 1);
    assert_eq!(missing_root.sources[0].status, UsageSourceStatus::Ok);
    assert_eq!(
        missing_root.sources[0].fingerprint,
        first.sources[0].fingerprint
    );

    std::fs::rename(&replacement, home.join("claude/projects")).unwrap();
    write_file(&home.join("claude/projects/new.jsonl"), claude_line(2, 7));
    let recreated = after_root_cleanup
        .read_summary(&window(), &settings)
        .await
        .unwrap();
    assert_eq!(total_output(&recreated), 12);
    assert_eq!(
        recreated.sources[0].fingerprint,
        first.sources[0].fingerprint
    );

    let outside_window = restarted
        .read_summary(&input("2026-08-02", "2026-08-02"), &settings)
        .await
        .unwrap();
    assert!(outside_window.buckets.is_empty());
    assert_eq!(outside_window.sources[0].distinct_sessions, 0);
}

#[tokio::test]
async fn credits_the_same_copy_of_a_duplicate_after_its_transcripts_are_deleted() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let dir = home.join("claude/projects/proj");
    write_file(&dir.join("a.jsonl"), "");
    write_file(&dir.join("b.jsonl"), "");
    // The walk-first file is the original, padded so it finishes parsing after
    // the small fork copy that repeats its record under a new session.
    let [first, second] = <[PathBuf; 2]>::try_from(walk_order(&dir)).unwrap();
    let forked = |line: String| line.replace("\"session-1\"", "\"session-2\"");
    write_file(
        &first,
        claude_line(1, 5).replace(
            "\"message\":",
            &format!(
                "\"padding\":{},\"message\":",
                json!("x".repeat(9 * 1024 * 1024))
            ),
        ),
    );
    write_file(
        &second,
        forked(claude_line(1, 5)) + &forked(claude_line(2, 7)),
    );
    seed_rates(&state, fable_rates());
    let settings = default_settings(&home);

    let service = default_service(&state, &home);
    let live = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(live.buckets[0].sessions, 2);

    std::fs::remove_file(&first).unwrap();
    std::fs::remove_file(&second).unwrap();
    let saved = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(saved.buckets, live.buckets);
    assert_eq!(saved.sources, live.sources);
    service.await_persisted().await;
    let restored = default_service(&state, &home)
        .read_summary(&window(), &settings)
        .await
        .unwrap();
    assert_eq!(restored.buckets, live.buckets);
}

// ── Shared-session dedupe ───────────────────────────────────────────────────

#[tokio::test]
async fn keeps_repeated_events_of_a_shared_session_and_drops_a_copy_in_another_file() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let sessions = home.join("codex/sessions");
    // Three token events in one rollout, the first and last equal; a second
    // rollout copies one of them. Occurrence counting keeps both equal events
    // and dedupes the copy, so the total is 10 + 11 + 10.
    write_file(
        &sessions.join("a.jsonl"),
        codex_rollout("shared", &[10, 11, 10]),
    );
    write_file(&sessions.join("b.jsonl"), codex_rollout("shared", &[10]));
    seed_rates(&state, fable_rates());

    let summary = default_service(&state, &home)
        .read_summary(&window(), &default_settings(&home))
        .await
        .unwrap();
    assert_eq!(total_output(&summary), 31);
}

// ── Scans that overlap ──────────────────────────────────────────────────────

#[tokio::test]
async fn keeps_a_newer_cached_read_when_a_slower_scan_of_another_window_finishes_later() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let dir = home.join("claude/projects/proj");
    let transcript = dir.join("session.jsonl");
    let probe = dir.join("probe.jsonl");
    let hold = dir.join("hold.jsonl");
    write_file(&transcript, claude_line(1, 5));
    make_gate(&probe, Some(BEFORE_NARROW_WINDOW));
    make_gate(&hold, Some(BEFORE_NARROW_WINDOW));
    seed_rates(&state, fable_rates());
    let settings = default_settings(&home);
    let service = default_service(&state, &home);

    let wide = {
        let (service, settings) = (service.clone(), settings.clone());
        tokio::spawn(async move { service.read_summary(&window(), &settings).await })
    };
    open_gate(&probe).await;
    replace_file(&transcript, &(claude_line(1, 5) + &claude_line(2, 7)));
    // The narrow scan caches the newer read while the wide one waits.
    service
        .read_summary(&narrow_window(), &settings)
        .await
        .unwrap();
    open_gate(&hold).await;
    wide.await.unwrap().unwrap();

    for path in [&transcript, &probe, &hold] {
        std::fs::remove_file(path).unwrap();
    }
    assert_eq!(
        total_output(&service.read_summary(&window(), &settings).await.unwrap()),
        12
    );
}

#[tokio::test]
async fn keeps_the_later_read_when_a_scan_that_read_earlier_finishes_first() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let dir = home.join("claude/projects/proj");
    let transcript = dir.join("session.jsonl");
    let wide_probe = dir.join("wide-probe.jsonl");
    let wide_hold = dir.join("wide-hold.jsonl");
    let narrow_probe = dir.join("narrow-probe.jsonl");
    let narrow_hold = dir.join("narrow-hold.jsonl");
    write_file(&transcript, claude_line(1, 5));
    make_gate(&wide_probe, Some(BEFORE_NARROW_WINDOW));
    make_gate(&wide_hold, Some(BEFORE_NARROW_WINDOW));
    seed_rates(&state, fable_rates());
    let settings = default_settings(&home);
    let service = default_service(&state, &home);

    let wide = {
        let (service, settings) = (service.clone(), settings.clone());
        tokio::spawn(async move { service.read_summary(&window(), &settings).await })
    };
    open_gate(&wide_probe).await;
    replace_file(&transcript, &(claude_line(1, 5) + &claude_line(2, 7)));
    // Made after the wide scan's walk, so only the narrow scan waits on them.
    make_gate(&narrow_probe, None);
    make_gate(&narrow_hold, None);
    let narrow = {
        let (service, settings) = (service.clone(), settings.clone());
        tokio::spawn(async move { service.read_summary(&narrow_window(), &settings).await })
    };
    open_gate(&narrow_probe).await;
    // Both scans started from an empty cache entry; the earlier read lands first.
    open_gate(&wide_hold).await;
    wide.await.unwrap().unwrap();
    open_gate(&narrow_hold).await;
    narrow.await.unwrap().unwrap();

    for path in [
        &transcript,
        &wide_probe,
        &wide_hold,
        &narrow_probe,
        &narrow_hold,
    ] {
        std::fs::remove_file(path).unwrap();
    }
    assert_eq!(
        total_output(&service.read_summary(&window(), &settings).await.unwrap()),
        12
    );
}

#[tokio::test]
async fn reports_saved_usage_of_a_removed_directory_only_for_windows_it_reaches() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let transcript = home.join("claude/projects/proj/session.jsonl");
    write_file(&transcript, claude_line(1, 5));
    set_mtime(&transcript, epoch_seconds(BEFORE_NARROW_WINDOW));
    seed_rates(&state, fable_rates());
    let settings = default_settings(&home);
    let service = default_service(&state, &home);

    let first = service.read_summary(&window(), &settings).await.unwrap();
    std::fs::remove_dir_all(home.join("claude/projects")).unwrap();

    let reached = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(reached.buckets, first.buckets);
    let claude = |summary: &UsageSummary| {
        summary
            .sources
            .iter()
            .find(|source| source.fingerprint.provider == UsageProviderKind::Claude)
            .cloned()
            .unwrap()
    };
    assert_eq!(claude(&reached).status, UsageSourceStatus::Ok);

    // A missing source cannot claim this directory from another environment that
    // still reads it, so it adds nothing to a window after its last write.
    let later = service
        .read_summary(&input("2026-08-10", "2026-08-12"), &settings)
        .await
        .unwrap();
    assert!(later.buckets.is_empty());
    assert_eq!(claude(&later).status, UsageSourceStatus::Missing);
    assert_eq!(claude(&later).scanned_files, 0);
}

#[tokio::test]
async fn does_not_share_an_in_flight_scan_after_custom_prices_change() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line_with(1, 5, "example-model"),
    );
    // The rate endpoint holds the first scan in flight while the prices change.
    let rates = RatesServer::start(json!({}), true).await;
    let service = service_with(
        &state,
        vec![claude_reader(&home)],
        &[],
        rates.url.clone(),
        system_clock(),
    );
    let base = default_settings(&home);
    let mut priced = serde_json::to_value(&base).unwrap();
    priced["usagePriceOverrides"] = json!({ "example-model": { "inputCostPerMillionTokens": 2, "outputCostPerMillionTokens": 8 } });
    let priced = settings(priced);

    let first = tokio::spawn({
        let (service, base) = (service.clone(), base.clone());
        async move { service.read_summary(&window(), &base).await }
    });
    rates.wait_for_hits(1).await;
    let second = tokio::spawn({
        let (service, priced) = (service.clone(), priced.clone());
        async move { service.read_summary(&window(), &priced).await }
    });
    rates.release();

    let original = first.await.unwrap().unwrap();
    let updated = second.await.unwrap().unwrap();
    assert_eq!(original.buckets[0].cost_usd, 0.0);
    close(updated.buckets[0].cost_usd, 0.00006);
}

#[tokio::test]
async fn shares_one_scan_between_concurrent_identical_requests() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line(1, 5),
    );
    // An unparsable document, so every scan retries the fetch.
    let rates = RatesServer::start(json!({}), false).await;
    let service = service_with(
        &state,
        vec![claude_reader(&home)],
        &[],
        rates.url.clone(),
        system_clock(),
    );
    let settings = default_settings(&home);

    let range = window();
    let (first, second) = tokio::join!(
        service.read_summary(&range, &settings),
        service.read_summary(&range, &settings),
    );
    assert_eq!(first.unwrap(), second.unwrap());
    assert_eq!(rates.hits(), 1);

    // A later request is fresh work again, not a stale cached answer.
    service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(rates.hits(), 2);
}

#[tokio::test]
async fn refetches_a_rate_table_inside_its_ttl_only_when_the_client_asks() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    write_file(
        &home.join("claude/projects/proj/session.jsonl"),
        claude_line(1, 5),
    );
    let rates = RatesServer::start(fable_rates(), false).await;
    let (clock, now) = movable_clock(now_ms());
    let service = service_with(
        &state,
        vec![claude_reader(&home)],
        &[],
        rates.url.clone(),
        now,
    );
    let settings = default_settings(&home);

    let first = service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(rates.hits(), 1);
    assert_eq!(first.pricing.status, UsagePricingStatus::Fresh);

    // Inside the daily TTL a plain rescan keeps the cached table.
    clock.fetch_add(
        Duration::from_secs(120).as_millis() as u64,
        Ordering::SeqCst,
    );
    service.read_summary(&window(), &settings).await.unwrap();
    assert_eq!(rates.hits(), 1);

    // An explicit refresh fetches again so a newly listed model gets priced. A
    // burst of refreshes shares that one fetch.
    let (refreshed, _) = tokio::join!(service.refresh_rates(), service.refresh_rates());
    assert_eq!(rates.hits(), 2);
    assert_eq!(refreshed.status, UsagePricingStatus::Fresh);
    assert_eq!(refreshed.known_models, 1);
}

#[tokio::test]
async fn does_not_orphan_an_in_flight_scan_when_its_first_caller_is_dropped() {
    let root = tempfile::tempdir().unwrap();
    let service = default_service(&root.path().join("state"), &root.path().join("home"));
    let settings = default_settings(root.path());
    // An inverted window fails before any read, so the scan is quick once it runs.
    let inverted = input("2026-09-01", "2026-08-01");

    // Poll the first caller once, so its scan starts, then drop it.
    let first = service.read_summary(&inverted, &settings).now_or_never();
    assert!(first.is_none());

    let second = tokio::time::timeout(
        Duration::from_secs(5),
        service.read_summary(&inverted, &settings),
    )
    .await
    .expect("the matching request must not wait on a dropped scan");
    assert_eq!(
        reason(&second.unwrap_err()),
        UsageReadErrorReason::InvalidWindow
    );
}

// ── Gated scans that cover the warm path ───────────────────────────────────

#[tokio::test]
async fn a_grown_codex_rollout_keeps_its_reducer_state_across_a_restart() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    let rollout = home.join("codex/sessions/rollout.jsonl");
    write_file(&rollout, codex_rollout("resume-me", &[4]));
    seed_rates(&state, fable_rates());
    let settings = default_settings(&home);

    let first = default_service(&state, &home);
    first.read_summary(&window(), &settings).await.unwrap();
    first.await_persisted().await;

    // The appended event has no session or model of its own.
    append_file(
        &rollout,
        &format!(
            "{}\n",
            json!({
                "type": "event_msg",
                "timestamp": "2026-08-01T10:00:00Z",
                "payload": {
                    "type": "token_count",
                    "info": { "last_token_usage": { "input_tokens": 1, "output_tokens": 9 } },
                },
            })
        ),
    );
    let grown = default_service(&state, &home)
        .read_summary(&window(), &settings)
        .await
        .unwrap();
    assert_eq!(total_output(&grown), 13);
    assert_eq!(grown.buckets[0].model.as_str(), "gpt-5.6-sol");
    assert_eq!(grown.buckets[0].sessions, 1);
    assert_eq!(grown.buckets[0].totals.output_tokens, 13);
}
