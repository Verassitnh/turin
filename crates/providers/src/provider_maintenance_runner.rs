//! Port of upstream/apps/server/src/provider/providerMaintenanceRunner.ts.
//!
//! Runs a provider's one-click update: queue behind the command lock, re-derive
//! ownership, check compatibility, run the installer, verify the refreshed
//! provider, and publish the progress as volatile update state on the registry.
//!
//! Upstream reads `ProviderRegistry`, `ModelManifest`, the child process
//! spawner and the HTTP client from Effect services. Here each is a trait the
//! runner takes as an `Arc`, with the live implementation for the real types:
//! [`MaintenanceRegistry`] (`ProviderRegistry`), [`MaintenanceManifest`]
//! (`ModelManifest`), [`CommandSpawner`] ([`TokioCommandSpawner`]), and
//! [`MaintenanceVersions`] ([`LiveMaintenanceVersions`]).
//!
//! Windows shim resolution (`resolveSpawnCommand`) is not ported: Turin is macOS
//! only, where it is the identity.

use std::{any::Any, io, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{FutureExt, StreamExt};
use parking_lot::Mutex;
use regex::Regex;
use turin_contracts::{
    TrimmedNonEmptyString, UtcDateTime,
    provider_instance::{ProviderDriverKind, ProviderInstanceId, default_instance_id_for_driver},
};
use turin_provider_core::{
    collect_stream_text::collect_uint8_stream_text,
    host_process::ProcessEnv,
    maintenance::{ProviderMaintenanceCapabilities, ProviderMaintenanceCommandAction},
    maintenance_resolver::make_targeted_provider_update_action,
    model_manifest::{ModelManifest, ModelManifestData, bundled_model_manifest},
    package_managed_maintenance::{
        enrich_provider_snapshot_with_version_advisory, resolve_latest_provider_version,
    },
    process_group::signal_process_group,
    provider_compatibility::{
        ProviderCompatibilityPolicy, decode_provider_compatibility_policies,
        resolve_provider_compatibility,
    },
    provider_latest_versions::ProviderLatestVersions,
    provider_maintenance_command_coordinator::ProviderMaintenanceCommandCoordinator,
    rt::{BoxFuture, BoxStream},
    server_provider::{
        ServerProvider, ServerProviderCompatibilityStatus, ServerProviderUpdateState,
        ServerProviderUpdateStatus, ServerProviderVersionAdvisoryStatus,
    },
};

use crate::provider_registry::{ProviderMaintenanceActionKind, ProviderRegistry};

const UPDATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UPDATE_OUTPUT_MAX_BYTES: usize = 10_000;
// Every progress publish resends the provider list to each client, so the
// installer's output is sampled rather than forwarded line by line.
const UPDATE_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
const UPDATE_PROGRESS_MAX_LENGTH: usize = 200;
// An installer may write for minutes without a line ending; only its tail matters.
const UPDATE_PARTIAL_LINE_MAX_LENGTH: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMaintenanceCommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// Upstream `ServerProviderUpdateError` (contracts/server.ts).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Provider update failed for {provider}: {reason}")]
pub struct ServerProviderUpdateError {
    pub provider: ProviderDriverKind,
    pub reason: String,
}

/// Upstream `ServerProviderUpdatedPayload`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerProviderUpdatedPayload {
    pub providers: Vec<ServerProvider>,
}

/// What to update: a driver's default instance, or one named instance, and
/// optionally a pinned version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUpdateTarget {
    pub provider: ProviderDriverKind,
    pub instance_id: Option<ProviderInstanceId>,
    pub target_version: Option<String>,
}

impl From<ProviderDriverKind> for ProviderUpdateTarget {
    fn from(provider: ProviderDriverKind) -> Self {
        Self {
            provider,
            instance_id: None,
            target_version: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct ProviderMaintenanceCommandError {
    message: String,
}

// ── Seams ──────────────────────────────────────────────────────────

/// The registry surface the runner uses.
#[async_trait]
pub trait MaintenanceRegistry: Send + Sync {
    async fn get_providers(&self) -> Vec<ServerProvider>;
    async fn refresh_instance(&self, instance_id: &ProviderInstanceId) -> Vec<ServerProvider>;
    async fn get_provider_maintenance_capabilities_for_instance(
        &self,
        instance_id: &ProviderInstanceId,
        provider: &ProviderDriverKind,
        fresh: bool,
    ) -> ProviderMaintenanceCapabilities;
    async fn set_provider_maintenance_action_state(
        &self,
        instance_id: &ProviderInstanceId,
        action: ProviderMaintenanceActionKind,
        state: Option<ServerProviderUpdateState>,
    ) -> Vec<ServerProvider>;
}

#[async_trait]
impl MaintenanceRegistry for ProviderRegistry {
    async fn get_providers(&self) -> Vec<ServerProvider> {
        ProviderRegistry::get_providers(self)
    }

    async fn refresh_instance(&self, instance_id: &ProviderInstanceId) -> Vec<ServerProvider> {
        ProviderRegistry::refresh_instance(self, instance_id).await
    }

    async fn get_provider_maintenance_capabilities_for_instance(
        &self,
        instance_id: &ProviderInstanceId,
        provider: &ProviderDriverKind,
        fresh: bool,
    ) -> ProviderMaintenanceCapabilities {
        ProviderRegistry::get_provider_maintenance_capabilities_for_instance(
            self,
            instance_id,
            provider,
            fresh,
        )
        .await
    }

    async fn set_provider_maintenance_action_state(
        &self,
        instance_id: &ProviderInstanceId,
        action: ProviderMaintenanceActionKind,
        state: Option<ServerProviderUpdateState>,
    ) -> Vec<ServerProvider> {
        ProviderRegistry::set_provider_maintenance_action_state(self, instance_id, action, state)
            .await
    }
}

/// `ModelManifest.current`.
#[async_trait]
pub trait MaintenanceManifest: Send + Sync {
    async fn current(&self) -> ModelManifestData;
}

#[async_trait]
impl MaintenanceManifest for ModelManifest {
    async fn current(&self) -> ModelManifestData {
        ModelManifest::current(self).await
    }
}

/// The version lookups the runner makes: upstream's `resolveLatestProviderVersion`
/// and `enrichProviderSnapshotWithVersionAdvisory`, which read the HTTP client
/// and the shared latest-version cache.
#[async_trait]
pub trait MaintenanceVersions: Send + Sync {
    async fn resolve_latest(
        &self,
        capabilities: &ProviderMaintenanceCapabilities,
    ) -> Option<String>;
    async fn enrich(
        &self,
        snapshot: ServerProvider,
        capabilities: &ProviderMaintenanceCapabilities,
    ) -> ServerProvider;
}

pub struct LiveMaintenanceVersions {
    latest_versions: Arc<ProviderLatestVersions>,
    client: reqwest::Client,
}

impl LiveMaintenanceVersions {
    pub fn new(latest_versions: Arc<ProviderLatestVersions>, client: reqwest::Client) -> Self {
        Self {
            latest_versions,
            client,
        }
    }
}

#[async_trait]
impl MaintenanceVersions for LiveMaintenanceVersions {
    async fn resolve_latest(
        &self,
        capabilities: &ProviderMaintenanceCapabilities,
    ) -> Option<String> {
        resolve_latest_provider_version(capabilities, &self.latest_versions, &self.client, false)
            .await
    }

    async fn enrich(
        &self,
        snapshot: ServerProvider,
        capabilities: &ProviderMaintenanceCapabilities,
    ) -> ServerProvider {
        enrich_provider_snapshot_with_version_advisory(
            snapshot,
            capabilities,
            None,
            &self.latest_versions,
            &self.client,
        )
        .await
    }
}

/// A spawned updater. Dropping it kills the process (upstream's scope
/// finalizer), so a timeout or a dropped caller never leaves an installer running.
pub struct SpawnedCommand {
    pub stdout: BoxStream<'static, io::Result<Vec<u8>>>,
    pub stderr: BoxStream<'static, io::Result<Vec<u8>>>,
    pub exit_code: BoxFuture<'static, io::Result<i32>>,
    pub guard: Box<dyn Any + Send>,
}

/// `ChildProcessSpawner`. `env` extends the host environment.
#[async_trait]
pub trait CommandSpawner: Send + Sync {
    async fn spawn(
        &self,
        command: &str,
        args: &[String],
        env: Option<&ProcessEnv>,
    ) -> Result<SpawnedCommand, String>;
}

#[cfg(unix)]
struct KillGroupOnDrop {
    pid: Option<u32>,
    exited: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        const SIGKILL: i32 = 9;
        if self.exited.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Some(pid) = self.pid {
            let _ = signal_process_group(i64::from(pid), SIGKILL);
        }
    }
}

/// Runs the updater as a child process in its own process group.
pub struct TokioCommandSpawner;

#[async_trait]
impl CommandSpawner for TokioCommandSpawner {
    #[cfg(unix)]
    async fn spawn(
        &self,
        command: &str,
        args: &[String],
        env: Option<&ProcessEnv>,
    ) -> Result<SpawnedCommand, String> {
        use std::process::Stdio;
        use std::sync::atomic::{AtomicBool, Ordering};

        use tokio_util::io::ReaderStream;

        let mut process = tokio::process::Command::new(command);
        process
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        if let Some(env) = env {
            process.envs(env);
        }
        let mut child = process.spawn().map_err(|error| error.to_string())?;
        let exited = Arc::new(AtomicBool::new(false));
        let guard = KillGroupOnDrop {
            pid: child.id(),
            exited: Arc::clone(&exited),
        };
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        fn to_chunks<R: tokio::io::AsyncRead + Send + 'static>(
            reader: R,
        ) -> BoxStream<'static, io::Result<Vec<u8>>> {
            Box::pin(ReaderStream::new(reader).map(|chunk| chunk.map(|bytes| bytes.to_vec())))
        }
        Ok(SpawnedCommand {
            stdout: to_chunks(stdout),
            stderr: to_chunks(stderr),
            exit_code: Box::pin(async move {
                use std::os::unix::process::ExitStatusExt;
                let status = child.wait().await;
                exited.store(true, Ordering::SeqCst);
                status.map(|status| {
                    status
                        .code()
                        .or_else(|| status.signal().map(|signal| 128 + signal))
                        .unwrap_or(-1)
                })
            }),
            guard: Box::new(guard),
        })
    }

    #[cfg(not(unix))]
    async fn spawn(
        &self,
        _command: &str,
        _args: &[String],
        _env: Option<&ProcessEnv>,
    ) -> Result<SpawnedCommand, String> {
        Err("Provider updates are only supported on unix hosts.".to_owned())
    }
}

// ── Output handling ────────────────────────────────────────────────

/// Split a chunk of installer output into complete lines. `\r` ends a line too,
/// so a redrawn progress bar reports its latest frame. The partial line keeps
/// its last `UPDATE_PARTIAL_LINE_MAX_LENGTH` characters (upstream counts UTF-16
/// units; the two agree outside the astral planes).
pub fn split_output_lines(partial_line: &str, text: &str) -> (Vec<String>, String) {
    let joined = format!("{partial_line}{text}");
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = joined.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                parts.push(std::mem::take(&mut current));
            }
            '\n' => parts.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    let tail: Vec<char> = current.chars().collect();
    let skip = tail.len().saturating_sub(UPDATE_PARTIAL_LINE_MAX_LENGTH);
    (parts, tail[skip..].iter().collect())
}

fn vt_control_regex() -> &'static Regex {
    // Node's `util.stripVTControlCharacters` pattern.
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            "[\u{1b}\u{9b}][\\[\\]()#;?]*",
            "(?:(?:(?:(?:;[-a-zA-Z0-9/#&.:=?%@~_]+)*",
            "|[a-zA-Z0-9]+(?:;[-a-zA-Z0-9/#&.:=?%@~_]*)*)?",
            "(?:\u{7}|\u{1b}\\\\|\u{9c}))",
            "|(?:(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-PR-TZcf-ntqry=><~]))"
        ))
        .expect("valid VT control pattern")
    })
}

/// Turn one raw output line into a short status message, or `None` if it has no text.
pub fn to_progress_line(line: &str) -> Option<String> {
    static WHITESPACE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let whitespace = WHITESPACE.get_or_init(|| Regex::new(r"\s+").expect("valid regex"));
    let stripped = vt_control_regex().replace_all(line, "");
    let text = whitespace.replace_all(&stripped, " ");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let characters: Vec<char> = text.chars().collect();
    if characters.len() <= UPDATE_PROGRESS_MAX_LENGTH {
        return Some(text.to_owned());
    }
    let mut shortened: String = characters[..UPDATE_PROGRESS_MAX_LENGTH - 1]
        .iter()
        .collect();
    shortened.push('…');
    Some(shortened)
}

/// `TextDecoder.decode(chunk, { stream: true })`: a multi-byte character split
/// across chunks is held until its tail arrives.
#[derive(Default)]
struct Utf8StreamDecoder {
    held: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn push(&mut self, chunk: &[u8]) -> String {
        self.held.extend_from_slice(chunk);
        let mut output = String::new();
        loop {
            match std::str::from_utf8(&self.held) {
                Ok(valid) => {
                    output.push_str(valid);
                    self.held.clear();
                    return output;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    output.push_str(
                        std::str::from_utf8(&self.held[..valid_up_to]).expect("valid prefix"),
                    );
                    match error.error_len() {
                        // An incomplete sequence at the end: wait for more bytes.
                        None => {
                            self.held.drain(..valid_up_to);
                            return output;
                        }
                        Some(length) => {
                            output.push('\u{FFFD}');
                            self.held.drain(..valid_up_to + length);
                        }
                    }
                }
            }
        }
    }
}

/// `claude update` reads better in a status line than the full executable path.
fn describe_command(command: &ProviderMaintenanceCommandAction) -> String {
    let executable = command
        .executable
        .rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(&command.executable);
    std::iter::once(executable.to_owned())
        .chain(command.args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn trim_nullable(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn command_output(result: &ProviderMaintenanceCommandResult) -> Option<String> {
    let joined = [result.stderr.as_str(), result.stdout.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let output = trim_nullable(&joined)?;
    Some(output.chars().take(UPDATE_OUTPUT_MAX_BYTES).collect())
}

fn failure_message(result: &ProviderMaintenanceCommandResult) -> String {
    if result.timed_out {
        return "Update timed out.".to_owned();
    }
    match result.exit_code {
        Some(code) if code != 0 => format!("Update command exited with code {code}."),
        _ => "Update command failed.".to_owned(),
    }
}

fn is_outdated_provider(provider: &ServerProvider) -> bool {
    provider.version_advisory.as_ref().is_some_and(|advisory| {
        advisory.status == ServerProviderVersionAdvisoryStatus::BehindLatest
    })
}

fn make_update_state(
    status: ServerProviderUpdateStatus,
    started_at: Option<String>,
    finished_at: Option<String>,
    message: Option<String>,
    output: Option<String>,
) -> ServerProviderUpdateState {
    ServerProviderUpdateState {
        status,
        started_at,
        finished_at,
        message: message.and_then(|text| TrimmedNonEmptyString::new(text).ok()),
        output,
    }
}

fn now_iso() -> String {
    UtcDateTime::now().to_string()
}

type ProgressSink = Arc<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>;

/// Aborts the sampler when the command ends, however it ends.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Upstream `runProviderMaintenanceCommandWithSpawner`.
async fn run_provider_maintenance_command(
    spawner: &dyn CommandSpawner,
    command: &ProviderMaintenanceCommandAction,
    on_progress: ProgressSink,
    timeout: Duration,
    progress_interval: Duration,
) -> Result<ProviderMaintenanceCommandResult, ProviderMaintenanceCommandError> {
    let collect = async {
        let spawned = spawner
            .spawn(&command.executable, &command.args, command.env.as_ref())
            .await
            .map_err(|cause| ProviderMaintenanceCommandError {
                message: format!(
                    "Failed to run update command {}: {cause}",
                    command.executable
                ),
            })?;
        let SpawnedCommand {
            stdout,
            stderr,
            exit_code,
            guard: _guard,
        } = spawned;

        // Holds the newest output line not yet reported; the sampler takes it.
        let pending_progress: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let _sampler = AbortOnDrop(tokio::spawn({
            let pending_progress = Arc::clone(&pending_progress);
            let on_progress = Arc::clone(&on_progress);
            async move {
                loop {
                    let line = pending_progress.lock().take();
                    if let Some(line) = line {
                        on_progress(line).await;
                    }
                    tokio::time::sleep(progress_interval).await;
                }
            }
        }));

        let track_progress = |stream: BoxStream<'static, io::Result<Vec<u8>>>| {
            let pending_progress = Arc::clone(&pending_progress);
            let mut decoder = Utf8StreamDecoder::default();
            let mut partial_line = String::new();
            let tracked: BoxStream<'static, io::Result<Vec<u8>>> =
                Box::pin(stream.inspect(move |chunk| {
                    let Ok(chunk) = chunk else {
                        return;
                    };
                    let (lines, next_partial) =
                        split_output_lines(&partial_line, &decoder.push(chunk));
                    partial_line = next_partial;
                    // A progress bar that redraws with a leading `\r` keeps its newest
                    // frame unterminated, so that frame is the latest status.
                    let line = to_progress_line(&partial_line)
                        .or_else(|| lines.iter().rev().find_map(|line| to_progress_line(line)));
                    if let Some(line) = line {
                        *pending_progress.lock() = Some(line);
                    }
                }));
            tracked
        };

        let (stdout, stderr, exit_code) = tokio::join!(
            collect_uint8_stream_text(track_progress(stdout), Some(UPDATE_OUTPUT_MAX_BYTES), None),
            collect_uint8_stream_text(track_progress(stderr), Some(UPDATE_OUTPUT_MAX_BYTES), None),
            exit_code,
        );
        let to_error = |cause: io::Error| ProviderMaintenanceCommandError {
            message: cause.to_string(),
        };
        let stdout = stdout.map_err(to_error)?;
        let stderr = stderr.map_err(to_error)?;
        let exit_code = exit_code.map_err(to_error)?;

        Ok(ProviderMaintenanceCommandResult {
            stdout: stdout.text,
            stderr: stderr.text,
            exit_code: Some(exit_code),
            timed_out: false,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        })
    };

    match tokio::time::timeout(timeout, collect).await {
        Ok(result) => result,
        Err(_) => Ok(ProviderMaintenanceCommandResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: true,
            stdout_truncated: false,
            stderr_truncated: false,
        }),
    }
}

// ── Runner ─────────────────────────────────────────────────────────

struct VerifiedProviderRefresh {
    verified_providers: Vec<ServerProvider>,
}

pub struct ProviderMaintenanceRunnerDeps {
    pub registry: Arc<dyn MaintenanceRegistry>,
    pub manifest: Arc<dyn MaintenanceManifest>,
    pub spawner: Arc<dyn CommandSpawner>,
    pub versions: Arc<dyn MaintenanceVersions>,
    /// The running app's version, matched against compatibility policies.
    pub t3_code_version: String,
}

pub struct ProviderMaintenanceRunner {
    deps: ProviderMaintenanceRunnerDeps,
    coordinator: ProviderMaintenanceCommandCoordinator<ServerProviderUpdateError>,
    command_timeout: Duration,
    progress_interval: Duration,
}

fn decoded_policies(manifest: &ModelManifestData) -> Option<Vec<ProviderCompatibilityPolicy>> {
    manifest
        .compatibility
        .as_deref()
        .map(decode_provider_compatibility_policies)
}

impl ProviderMaintenanceRunner {
    /// Upstream `make`.
    pub fn new(deps: ProviderMaintenanceRunnerDeps) -> Self {
        Self {
            deps,
            coordinator: ProviderMaintenanceCommandCoordinator::new(|_target| {
                ServerProviderUpdateError {
                    provider: ProviderDriverKind::new("unknown").expect("valid driver slug"),
                    reason: "An update is already running for this provider.".to_owned(),
                }
            }),
            command_timeout: UPDATE_TIMEOUT,
            progress_interval: UPDATE_PROGRESS_INTERVAL,
        }
    }

    /// Overrides the five-minute command timeout and the one-second progress
    /// sampling interval.
    pub fn with_timing(mut self, command_timeout: Duration, progress_interval: Duration) -> Self {
        self.command_timeout = command_timeout;
        self.progress_interval = progress_interval;
        self
    }

    async fn verify_refreshed_provider(
        &self,
        provider: &ProviderDriverKind,
        maintenance_capabilities: &ProviderMaintenanceCapabilities,
        instance_id: &ProviderInstanceId,
    ) -> VerifiedProviderRefresh {
        let registry = &self.deps.registry;
        let matches = |candidate: &ServerProvider| {
            &candidate.driver == provider && &candidate.instance_id == instance_id
        };
        let known = registry.get_providers().await.iter().any(matches);
        let providers = if known {
            registry.refresh_instance(instance_id).await;
            registry.get_providers().await
        } else {
            registry.refresh_instance(instance_id).await
        };
        let refreshed_providers: Vec<ServerProvider> = providers
            .into_iter()
            .filter(|candidate| matches(candidate))
            .collect();
        if refreshed_providers.is_empty() {
            return VerifiedProviderRefresh {
                verified_providers: Vec::new(),
            };
        }
        let enriched = AssertUnwindSafe(futures::future::join_all(refreshed_providers.iter().map(
            |refreshed_provider| {
                self.deps
                    .versions
                    .enrich(refreshed_provider.clone(), maintenance_capabilities)
            },
        )))
        .catch_unwind()
        .await;
        match enriched {
            Ok(verified_providers) => VerifiedProviderRefresh { verified_providers },
            Err(_) => {
                tracing::warn!(%provider, "Provider post-update version verification failed");
                VerifiedProviderRefresh {
                    verified_providers: refreshed_providers,
                }
            }
        }
    }

    pub async fn update_provider(
        &self,
        target: impl Into<ProviderUpdateTarget>,
    ) -> Result<ServerProviderUpdatedPayload, ServerProviderUpdateError> {
        let target = target.into();
        let provider = target.provider;
        let instance_id = target
            .instance_id
            .unwrap_or_else(|| default_instance_id_for_driver(&provider));
        let target_version = target.target_version;
        let target_key = format!("instance:{instance_id}");
        let capabilities = self
            .deps
            .registry
            .get_provider_maintenance_capabilities_for_instance(&instance_id, &provider, false)
            .await;
        let Some(update) = capabilities.update else {
            return Err(ServerProviderUpdateError {
                provider,
                reason: "This provider does not support one-click updates.".to_owned(),
            });
        };

        let queued = async {
            self.set_update_state(
                &instance_id,
                Some(make_update_state(
                    ServerProviderUpdateStatus::Queued,
                    None,
                    None,
                    Some("Waiting for another provider update to finish.".to_owned()),
                    None,
                )),
            )
            .await;
            Ok(())
        };

        self.coordinator
            .with_command_lock(
                &target_key,
                &update.lock_key,
                Some(Box::pin(queued)),
                self.run_provider_update(
                    &provider,
                    &instance_id,
                    &update,
                    target_version.as_deref(),
                ),
            )
            .await
            .map_err(|error| ServerProviderUpdateError {
                provider: provider.clone(),
                reason: error.reason,
            })
    }

    async fn set_update_state(
        &self,
        instance_id: &ProviderInstanceId,
        state: Option<ServerProviderUpdateState>,
    ) -> Vec<ServerProvider> {
        self.deps
            .registry
            .set_provider_maintenance_action_state(
                instance_id,
                ProviderMaintenanceActionKind::Update,
                state,
            )
            .await
    }

    async fn finish(
        &self,
        instance_id: &ProviderInstanceId,
        state: ServerProviderUpdateState,
    ) -> ServerProviderUpdatedPayload {
        ServerProviderUpdatedPayload {
            providers: self.set_update_state(instance_id, Some(state)).await,
        }
    }

    async fn run_provider_update(
        &self,
        provider: &ProviderDriverKind,
        instance_id: &ProviderInstanceId,
        update: &ProviderMaintenanceCommandAction,
        target_version: Option<&str>,
    ) -> Result<ServerProviderUpdatedPayload, ServerProviderUpdateError> {
        let started_at: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let outcome = AssertUnwindSafe(self.run_command_and_verify(
            provider,
            instance_id,
            update,
            target_version,
            &started_at,
        ))
        .catch_unwind()
        .await;
        // Upstream `recordFailedUpdate`: any failure, including a defect, ends as a failed state.
        let message = match outcome {
            Ok(Ok(payload)) => return Ok(payload),
            Ok(Err(error)) => error.message,
            Err(panic) => {
                panic_message(panic.as_ref()).unwrap_or_else(|| "Update command failed.".to_owned())
            }
        };
        let started_at = started_at.lock().clone();
        Ok(self
            .finish(
                instance_id,
                make_update_state(
                    ServerProviderUpdateStatus::Failed,
                    started_at,
                    Some(now_iso()),
                    Some(message),
                    None,
                ),
            )
            .await)
    }

    async fn run_command_and_verify(
        &self,
        provider: &ProviderDriverKind,
        instance_id: &ProviderInstanceId,
        update: &ProviderMaintenanceCommandAction,
        target_version: Option<&str>,
        started_at_cell: &Arc<Mutex<Option<String>>>,
    ) -> Result<ServerProviderUpdatedPayload, ProviderMaintenanceCommandError> {
        let registry = &self.deps.registry;
        let started_at = now_iso();
        *started_at_cell.lock() = Some(started_at.clone());
        let running = |message: String| {
            make_update_state(
                ServerProviderUpdateStatus::Running,
                Some(started_at.clone()),
                None,
                Some(message),
                None,
            )
        };
        self.set_update_state(
            instance_id,
            Some(running("Checking for the latest version".to_owned())),
        )
        .await;

        // The cached capabilities chose the lock; re-derive ownership now so
        // the command that runs matches the executable as it is at click time,
        // not as it was at the last health refresh.
        let fresh = registry
            .get_provider_maintenance_capabilities_for_instance(instance_id, provider, true)
            .await;
        if fresh
            .update
            .as_ref()
            .is_none_or(|fresh_update| fresh_update.lock_key != update.lock_key)
        {
            return Ok(self
                .finish(
                    instance_id,
                    make_update_state(
                        ServerProviderUpdateStatus::Failed,
                        Some(started_at.clone()),
                        Some(now_iso()),
                        Some("Provider installation changed. Refresh and try again.".to_owned()),
                        None,
                    ),
                )
                .await);
        }

        let manifest = self.deps.manifest.current().await;
        let candidate_version = match target_version {
            Some(version) => Some(version.to_owned()),
            None => self.deps.versions.resolve_latest(&fresh).await,
        };
        let t3_code_version = &self.deps.t3_code_version;
        let advisory = resolve_provider_compatibility(
            decoded_policies(&manifest).as_deref(),
            provider,
            candidate_version.as_deref(),
            t3_code_version,
        )
        .or_else(|| {
            resolve_provider_compatibility(
                decoded_policies(&bundled_model_manifest()).as_deref(),
                provider,
                candidate_version.as_deref(),
                t3_code_version,
            )
        });
        let command = match target_version {
            Some(version) => make_targeted_provider_update_action(&fresh, version),
            None => fresh.update.clone(),
        };
        let rejected = match target_version {
            Some(version) => {
                command.is_none()
                    || advisory
                        .as_ref()
                        .and_then(|advisory| advisory.recommended_version.as_ref())
                        .map(|recommended| recommended.as_str())
                        != Some(version)
                    || advisory.as_ref().map(|advisory| advisory.status)
                        != Some(ServerProviderCompatibilityStatus::Supported)
            }
            None => advisory.as_ref().is_some_and(|advisory| {
                matches!(
                    advisory.status,
                    ServerProviderCompatibilityStatus::Broken
                        | ServerProviderCompatibilityStatus::Unsupported
                )
            }),
        };
        let command = match command {
            Some(command) if !rejected => command,
            _ => {
                return Ok(self
                    .finish(
                        instance_id,
                        make_update_state(
                            ServerProviderUpdateStatus::Failed,
                            Some(started_at.clone()),
                            Some(now_iso()),
                            Some(if target_version.is_some() {
                                "This version is no longer recommended or this installer cannot install a specific version. Refresh provider settings.".to_owned()
                            } else {
                                "The latest provider version is incompatible with this T3 Code release. Review provider settings.".to_owned()
                            }),
                            None,
                        ),
                    )
                    .await);
            }
        };
        self.set_update_state(
            instance_id,
            Some(running(format!("Running {}", describe_command(&command)))),
        )
        .await;
        let on_progress: ProgressSink = {
            let registry = Arc::clone(&self.deps.registry);
            let instance_id = instance_id.clone();
            let started_at = started_at.clone();
            Arc::new(move |line: String| {
                let registry = Arc::clone(&registry);
                let instance_id = instance_id.clone();
                let started_at = started_at.clone();
                Box::pin(async move {
                    registry
                        .set_provider_maintenance_action_state(
                            &instance_id,
                            ProviderMaintenanceActionKind::Update,
                            Some(make_update_state(
                                ServerProviderUpdateStatus::Running,
                                Some(started_at),
                                None,
                                Some(line),
                                None,
                            )),
                        )
                        .await;
                })
            })
        };
        let result = run_provider_maintenance_command(
            self.deps.spawner.as_ref(),
            &command,
            on_progress,
            self.command_timeout,
            self.progress_interval,
        )
        .await?;
        let finished_at = now_iso();
        if result.timed_out || result.exit_code != Some(0) {
            return Ok(self
                .finish(
                    instance_id,
                    make_update_state(
                        ServerProviderUpdateStatus::Failed,
                        Some(started_at.clone()),
                        Some(finished_at),
                        Some(failure_message(&result)),
                        command_output(&result),
                    ),
                )
                .await);
        }

        self.set_update_state(
            instance_id,
            Some(running("Verifying the installed version".to_owned())),
        )
        .await;
        // Homebrew's "latest" moves once the upgrade lands; read it again.
        let verified = registry
            .get_provider_maintenance_capabilities_for_instance(instance_id, provider, true)
            .await;
        let VerifiedProviderRefresh { verified_providers } = self
            .verify_refreshed_provider(provider, &verified, instance_id)
            .await;
        // "Succeeded" needs the provider to still be installed: an installer
        // that exits 0 and leaves the binary missing is not a success. A
        // missing version alone is not held against it, since Cursor's `about`
        // probe can fail transiently on a healthy binary.
        let could_not_verify = verified_providers.is_empty()
            || verified_providers.iter().any(|verified_provider| {
                !verified_provider.installed
                    || target_version.is_some_and(|target| {
                        verified_provider.version.as_ref().map(|version| {
                            let version = version.as_str();
                            version.strip_prefix('v').unwrap_or(version)
                        }) != Some(target)
                    })
            });
        let still_outdated =
            target_version.is_none() && verified_providers.iter().any(is_outdated_provider);
        let (status, message) = if could_not_verify {
            (
                ServerProviderUpdateStatus::Unchanged,
                "Update command completed, but T3 Code could not verify the provider version.",
            )
        } else if still_outdated {
            (
                ServerProviderUpdateStatus::Unchanged,
                "Update command completed, but T3 Code still detects an outdated provider version.",
            )
        } else {
            (ServerProviderUpdateStatus::Succeeded, "Provider updated.")
        };
        Ok(self
            .finish(
                instance_id,
                make_update_state(
                    status,
                    Some(started_at.clone()),
                    Some(finished_at),
                    Some(message.to_owned()),
                    command_output(&result),
                ),
            )
            .await)
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> Option<String> {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
}

#[cfg(test)]
mod tests;
