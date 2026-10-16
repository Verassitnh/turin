//! Port of upstream/apps/server/src/provider/providerMaintenanceRunner.test.ts.
//!
//! Not ported: "resolves npm to a .cmd shim and routes through the shell on
//! win32" (Windows shim resolution is not ported; Turin is macOS only). Added:
//! the real spawner cases at the end, which upstream covers through its mock
//! `ChildProcessSpawner`.
//!
//! Upstream mocks `ProviderRegistry`, `ModelManifest`, the `ChildProcessSpawner`
//! and the `HttpClient`; here [`FakeRegistry`], [`FakeManifest`],
//! [`MockSpawner`] and [`FakeVersions`] play those parts.

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::stream;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use turin_provider_core::{
    maintenance_resolver::{ProviderVersionAdvisoryInput, create_provider_version_advisory},
    package_managed_maintenance::{
        MaintenanceCapabilitiesInput, make_provider_maintenance_capabilities,
    },
    server_provider::ServerProviderUpdateStatus,
};

use super::*;

const T3_CODE_VERSION: &str = "1.0.0";

fn kind(value: &str) -> ProviderDriverKind {
    ProviderDriverKind::new(value).unwrap()
}

fn id(value: &str) -> ProviderInstanceId {
    ProviderInstanceId::new(value).unwrap()
}

/// `Deferred<void>`: completes once, can be awaited any number of times.
#[derive(Clone)]
struct Deferred(Arc<tokio::sync::watch::Sender<bool>>);

impl Deferred {
    fn new() -> Self {
        Self(Arc::new(tokio::sync::watch::channel(false).0))
    }

    fn succeed(&self) {
        self.0.send_replace(true);
    }

    async fn wait(&self) {
        let mut receiver = self.0.subscribe();
        receiver.wait_for(|done| *done).await.unwrap();
    }
}

/// Spins on `condition`, yielding between checks. Bounded by attempts, not by
/// a timer: a timer never fires while the paused clock has a task to run, so a
/// timeout would turn a regression into a hang.
async fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..100_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition was not met");
}

// ── Fixtures ───────────────────────────────────────────────────────

fn caps(
    provider: &ProviderDriverKind,
    package_name: Option<&str>,
    executable: Option<&str>,
    args: &[&str],
    lock_key: Option<&str>,
) -> ProviderMaintenanceCapabilities {
    make_provider_maintenance_capabilities(
        provider.clone(),
        MaintenanceCapabilitiesInput {
            provider: Some(provider.clone()),
            package_name: package_name.map(str::to_owned),
            update_executable: executable.map(str::to_owned),
            update_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            update_lock_key: lock_key.map(str::to_owned),
            ..MaintenanceCapabilitiesInput::default()
        },
    )
}

fn lifecycle_for(provider: &ProviderDriverKind) -> ProviderMaintenanceCapabilities {
    match provider.as_str() {
        "nativeCli" => caps(
            provider,
            None,
            Some("native-agent"),
            &["update"],
            Some("native-agent"),
        ),
        "opencode" => caps(
            provider,
            Some("opencode-ai"),
            Some("npm"),
            &["install", "-g", "opencode-ai@latest"],
            Some("npm-global"),
        ),
        _ => caps(
            provider,
            Some("@openai/codex"),
            Some("npm"),
            &["install", "-g", "@openai/codex@latest"],
            Some("npm-global"),
        ),
    }
}

fn provider(instance: &str, driver: &str, overrides: Value) -> ServerProvider {
    let mut value = json!({
        "instanceId": instance,
        "driver": driver,
        "enabled": true,
        "installed": true,
        "version": "0.0.0",
        "status": "ready",
        "auth": { "status": "authenticated" },
        "checkedAt": "2026-04-10T00:00:00.000Z",
        "models": [],
        "slashCommands": [],
        "skills": [],
    });
    for (key, entry) in overrides.as_object().unwrap() {
        value[key] = entry.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn base_provider() -> ServerProvider {
    provider("codex", "codex", json!({}))
}

fn base_native_cli_provider() -> ServerProvider {
    provider("nativeCli", "nativeCli", json!({}))
}

fn base_open_code_provider() -> ServerProvider {
    provider("opencode", "opencode", json!({}))
}

type CapabilitiesFn = Box<
    dyn Fn(&ProviderInstanceId, &ProviderDriverKind, bool) -> ProviderMaintenanceCapabilities
        + Send
        + Sync,
>;
type RefreshFn =
    Box<dyn Fn(&FakeRegistry, &ProviderInstanceId) -> Vec<ServerProvider> + Send + Sync>;

/// Upstream `makeRegistry` plus its per-case overrides.
struct FakeRegistry {
    providers: Mutex<Vec<ServerProvider>>,
    update_states: Mutex<Vec<ServerProviderUpdateState>>,
    capabilities: CapabilitiesFn,
    refresh_instance: Option<RefreshFn>,
    /// Parks the `queued` state write until released.
    block_queued: Mutex<Option<(Deferred, Deferred)>>,
    capability_reads: Mutex<Vec<bool>>,
}

impl FakeRegistry {
    fn new(providers: Vec<ServerProvider>) -> Self {
        Self {
            providers: Mutex::new(providers),
            update_states: Mutex::new(Vec::new()),
            capabilities: Box::new(|_instance, provider, _fresh| lifecycle_for(provider)),
            refresh_instance: None,
            block_queued: Mutex::new(None),
            capability_reads: Mutex::new(Vec::new()),
        }
    }

    fn with_capabilities(
        mut self,
        capabilities: impl Fn(
            &ProviderInstanceId,
            &ProviderDriverKind,
            bool,
        ) -> ProviderMaintenanceCapabilities
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.capabilities = Box::new(capabilities);
        self
    }

    fn with_refresh_instance(
        mut self,
        refresh: impl Fn(&FakeRegistry, &ProviderInstanceId) -> Vec<ServerProvider>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.refresh_instance = Some(Box::new(refresh));
        self
    }

    fn states(&self) -> Vec<(ServerProviderUpdateStatus, String)> {
        self.update_states
            .lock()
            .iter()
            .map(|state| {
                (
                    state.status,
                    state
                        .message
                        .as_ref()
                        .map(|message| message.to_string())
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    fn running_messages(&self) -> Vec<String> {
        self.states()
            .into_iter()
            .filter(|(status, _)| *status == ServerProviderUpdateStatus::Running)
            .map(|(_, message)| message)
            .collect()
    }
}

#[async_trait]
impl MaintenanceRegistry for FakeRegistry {
    async fn get_providers(&self) -> Vec<ServerProvider> {
        self.providers.lock().clone()
    }

    async fn refresh_instance(&self, instance_id: &ProviderInstanceId) -> Vec<ServerProvider> {
        match &self.refresh_instance {
            Some(refresh) => refresh(self, instance_id),
            None => self.providers.lock().clone(),
        }
    }

    async fn get_provider_maintenance_capabilities_for_instance(
        &self,
        instance_id: &ProviderInstanceId,
        provider: &ProviderDriverKind,
        fresh: bool,
    ) -> ProviderMaintenanceCapabilities {
        self.capability_reads.lock().push(fresh);
        (self.capabilities)(instance_id, provider, fresh)
    }

    async fn set_provider_maintenance_action_state(
        &self,
        instance_id: &ProviderInstanceId,
        _action: ProviderMaintenanceActionKind,
        state: Option<ServerProviderUpdateState>,
    ) -> Vec<ServerProvider> {
        if let Some(state) = &state {
            self.update_states.lock().push(state.clone());
        }
        let providers = {
            let mut providers = self.providers.lock();
            for candidate in providers.iter_mut() {
                if &candidate.instance_id == instance_id {
                    candidate.update_state = state.clone();
                }
            }
            providers.clone()
        };
        let gate = self.block_queued.lock().clone();
        if let (Some((written, release)), Some(state)) = (gate, &state)
            && state.status == ServerProviderUpdateStatus::Queued
        {
            written.succeed();
            release.wait().await;
        }
        providers
    }
}

struct FakeManifest(ModelManifestData);

#[async_trait]
impl MaintenanceManifest for FakeManifest {
    async fn current(&self) -> ModelManifestData {
        self.0.clone()
    }
}

/// Generic updater fixtures use synthetic versions. Keep their compatibility
/// unknown so real harness minimums do not bypass the command under test.
fn default_manifest() -> ModelManifestData {
    manifest(json!([
        { "driver": "codex", "t3CodeRange": ">=0.0.42", "ranges": [] },
        { "driver": "opencode", "t3CodeRange": ">=0.0.42", "ranges": [] },
    ]))
}

fn manifest(compatibility: Value) -> ModelManifestData {
    serde_json::from_value(json!({
        "version": 1,
        "currentModels": {},
        "compatibility": compatibility,
    }))
    .unwrap()
}

/// Upstream `layerLatestVersionHttpClient(version)`: the npm registry answers
/// `version`; an installer-declared `latest_version` still wins.
struct FakeVersions {
    npm_latest: String,
}

#[async_trait]
impl MaintenanceVersions for FakeVersions {
    async fn resolve_latest(
        &self,
        capabilities: &ProviderMaintenanceCapabilities,
    ) -> Option<String> {
        if let Some(latest) = &capabilities.latest_version {
            return latest.clone();
        }
        capabilities
            .package_name
            .as_ref()
            .map(|_| self.npm_latest.clone())
    }

    async fn enrich(
        &self,
        mut snapshot: ServerProvider,
        capabilities: &ProviderMaintenanceCapabilities,
    ) -> ServerProvider {
        let should_resolve = snapshot.enabled && snapshot.installed && snapshot.version.is_some();
        let latest = if should_resolve {
            self.resolve_latest(capabilities).await
        } else {
            None
        };
        let current = snapshot.version.as_ref().map(|version| version.to_string());
        snapshot.version_advisory = Some(create_provider_version_advisory(
            ProviderVersionAdvisoryInput {
                driver: &snapshot.driver,
                current_version: current.as_deref(),
                latest_version: latest.as_deref(),
                checked_at: Some(&snapshot.checked_at),
                maintenance: Some(capabilities),
            },
        ));
        snapshot
    }
}

struct MockProcess {
    stdout: String,
    stderr: String,
    code: i32,
    exit: Option<Deferred>,
}

impl MockProcess {
    fn stdout(text: &str) -> Self {
        Self {
            stdout: text.to_owned(),
            stderr: String::new(),
            code: 0,
            exit: None,
        }
    }
}

type SpawnHandler = Box<dyn Fn(&str, &[String], Option<&ProcessEnv>) -> MockProcess + Send + Sync>;

/// Upstream `layerMockSpawner`.
struct MockSpawner {
    handler: SpawnHandler,
}

#[async_trait]
impl CommandSpawner for MockSpawner {
    async fn spawn(
        &self,
        command: &str,
        args: &[String],
        env: Option<&ProcessEnv>,
    ) -> Result<SpawnedCommand, String> {
        let process = (self.handler)(command, args, env);
        let code = process.code;
        let exit = process.exit;
        Ok(SpawnedCommand {
            stdout: Box::pin(stream::iter(vec![Ok(process.stdout.into_bytes())])),
            stderr: Box::pin(stream::iter(vec![Ok(process.stderr.into_bytes())])),
            exit_code: Box::pin(async move {
                if let Some(exit) = exit {
                    exit.wait().await;
                }
                Ok(code)
            }),
            guard: Box::new(()),
        })
    }
}

type Calls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

fn recording_spawner(calls: &Calls, stdout: &'static str) -> MockSpawner {
    let calls = Arc::clone(calls);
    MockSpawner {
        handler: Box::new(move |command, args, _env| {
            calls.lock().push((command.to_owned(), args.to_vec()));
            MockProcess::stdout(stdout)
        }),
    }
}

fn call(command: &str, args: &[&str]) -> (String, Vec<String>) {
    (
        command.to_owned(),
        args.iter().map(|arg| (*arg).to_owned()).collect(),
    )
}

fn runner_with(
    registry: Arc<FakeRegistry>,
    spawner: MockSpawner,
    npm_latest: &str,
    manifest: ModelManifestData,
) -> ProviderMaintenanceRunner {
    ProviderMaintenanceRunner::new(ProviderMaintenanceRunnerDeps {
        registry,
        manifest: Arc::new(FakeManifest(manifest)),
        spawner: Arc::new(spawner),
        versions: Arc::new(FakeVersions {
            npm_latest: npm_latest.to_owned(),
        }),
        t3_code_version: T3_CODE_VERSION.to_owned(),
    })
}

fn runner(
    registry: &Arc<FakeRegistry>,
    spawner: MockSpawner,
    npm_latest: &str,
) -> ProviderMaintenanceRunner {
    runner_with(
        Arc::clone(registry),
        spawner,
        npm_latest,
        default_manifest(),
    )
}

fn status_of(payload: &ServerProviderUpdatedPayload, index: usize) -> ServerProviderUpdateStatus {
    payload.providers[index]
        .update_state
        .as_ref()
        .expect("update state")
        .status
}

fn message_of(payload: &ServerProviderUpdatedPayload, index: usize) -> String {
    payload.providers[index]
        .update_state
        .as_ref()
        .and_then(|state| state.message.as_ref())
        .map(|message| message.to_string())
        .unwrap_or_default()
}

// ── Cases ──────────────────────────────────────────────────────────

#[tokio::test]
async fn runs_the_allowlisted_provider_update_command_and_records_success() {
    let calls: Calls = Arc::default();
    let registry = Arc::new(FakeRegistry::new(vec![base_native_cli_provider()]));
    let updater = runner(&registry, recording_spawner(&calls, "updated"), "0.0.0");

    let result = updater.update_provider(kind("nativeCli")).await.unwrap();

    assert_eq!(*calls.lock(), vec![call("native-agent", &["update"])]);
    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Succeeded);
    assert_eq!(
        registry.states(),
        vec![
            (
                ServerProviderUpdateStatus::Queued,
                "Waiting for another provider update to finish.".to_owned()
            ),
            (
                ServerProviderUpdateStatus::Running,
                "Checking for the latest version".to_owned()
            ),
            (
                ServerProviderUpdateStatus::Running,
                "Running native-agent update".to_owned()
            ),
            (
                ServerProviderUpdateStatus::Running,
                "Verifying the installed version".to_owned()
            ),
            (
                ServerProviderUpdateStatus::Succeeded,
                "Provider updated.".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn reports_unchanged_when_the_updater_exits_0_but_the_provider_is_gone() {
    // After the update, the refreshed snapshot no longer sees an install.
    let registry = Arc::new(
        FakeRegistry::new(vec![base_provider()]).with_refresh_instance(|registry, _id| {
            let mut providers = registry.providers.lock();
            for provider in providers.iter_mut() {
                provider.installed = false;
                provider.version = None;
            }
            providers.clone()
        }),
    );
    let updater = runner(
        &registry,
        MockSpawner {
            handler: Box::new(|_, _, _| MockProcess::stdout("updated")),
        },
        "0.0.0",
    );

    let result = updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Unchanged);
    assert!(message_of(&result, 0).contains("could not verify"));
}

#[tokio::test]
async fn keeps_a_successful_update_when_the_binary_is_present_but_its_version_is_unreadable() {
    // A native CLI version probe can fail immediately after a successful update.
    let registry = Arc::new(
        FakeRegistry::new(vec![base_native_cli_provider()]).with_refresh_instance(
            |registry, _id| {
                let mut providers = registry.providers.lock();
                for provider in providers.iter_mut() {
                    provider.installed = true;
                    provider.version = None;
                }
                providers.clone()
            },
        ),
    );
    let updater = runner(
        &registry,
        MockSpawner {
            handler: Box::new(|_, _, _| MockProcess::stdout("updated")),
        },
        "0.0.0",
    );

    let result = updater.update_provider(kind("nativeCli")).await.unwrap();

    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Succeeded);
}

#[tokio::test]
async fn spawns_the_updater_with_the_environment_its_capabilities_declare() {
    let seen: Arc<Mutex<Vec<Option<ProcessEnv>>>> = Arc::default();
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]).with_capabilities(
        |_id, provider, _fresh| {
            let mut capabilities = lifecycle_for(provider);
            capabilities.update = Some(ProviderMaintenanceCommandAction {
                command: "codex update".to_owned(),
                executable: "/work/codex-home/packages/standalone/bin/codex".to_owned(),
                args: vec!["update".to_owned()],
                lock_key: "codex-native".to_owned(),
                env: Some(HashMap::from([(
                    "CODEX_HOME".to_owned(),
                    "/work/codex-home".to_owned(),
                )])),
            });
            capabilities
        },
    ));
    let updater = {
        let seen = Arc::clone(&seen);
        runner(
            &registry,
            MockSpawner {
                handler: Box::new(move |_command, _args, env| {
                    seen.lock().push(env.cloned());
                    MockProcess::stdout("updated")
                }),
            },
            "0.0.0",
        )
    };

    updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(
        *seen.lock(),
        vec![Some(HashMap::from([(
            "CODEX_HOME".to_owned(),
            "/work/codex-home".to_owned()
        )]))]
    );
}

#[tokio::test]
async fn re_resolves_ownership_before_running_and_executes_the_fresh_command() {
    let calls: Calls = Arc::default();
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]).with_capabilities(
        |_id, provider, fresh| {
            caps(
                provider,
                Some("@openai/codex"),
                Some(if fresh {
                    "/opt/homebrew/bin/brew"
                } else {
                    "brew"
                }),
                &["upgrade", "--cask", "codex"],
                Some("homebrew"),
            )
        },
    ));
    let updater = runner(&registry, recording_spawner(&calls, "updated"), "0.0.0");

    updater.update_provider(kind("codex")).await.unwrap();

    // Cached read picks the lock; the two fresh reads bracket the command.
    assert_eq!(*registry.capability_reads.lock(), vec![false, true, true]);
    assert_eq!(
        *calls.lock(),
        vec![call(
            "/opt/homebrew/bin/brew",
            &["upgrade", "--cask", "codex"]
        )]
    );
}

#[tokio::test(start_paused = true)]
async fn reports_the_installers_latest_output_line_while_the_update_runs() {
    let exited = Deferred::new();
    let registry = Arc::new(FakeRegistry::new(vec![base_native_cli_provider()]));
    let updater = {
        let exited = exited.clone();
        Arc::new(runner(
            &registry,
            MockSpawner {
                handler: Box::new(move |_, _, _| MockProcess {
                    stdout:
                        "Checking for updates\nDownloading 2.1.293\n\u{1b}[32m 40%\u{1b}[0m\r 80%"
                            .to_owned(),
                    stderr: String::new(),
                    code: 0,
                    exit: Some(exited.clone()),
                }),
            },
            "0.0.0",
        ))
    };

    let task = {
        let updater = Arc::clone(&updater);
        tokio::spawn(async move { updater.update_provider(kind("nativeCli")).await })
    };
    wait_until(|| {
        registry
            .running_messages()
            .contains(&"Running native-agent update".to_owned())
    })
    .await;
    // Let the stdout reader record the newest line before the sampler wakes.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    wait_until(|| registry.running_messages().len() >= 3).await;
    assert_eq!(
        registry.running_messages(),
        vec![
            "Checking for the latest version".to_owned(),
            "Running native-agent update".to_owned(),
            "80%".to_owned(),
        ]
    );

    exited.succeed();
    let result = task.await.unwrap().unwrap();
    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Succeeded);
}

#[test]
fn splits_installer_output_into_clean_status_lines() {
    assert_eq!(
        split_output_lines("Down", "loading\r\n 5"),
        (vec!["Downloading".to_owned()], " 5".to_owned())
    );
    assert_eq!(
        to_progress_line("\u{1b}[1m==>\u{1b}[0m  Pouring"),
        Some("==> Pouring".to_owned())
    );
    assert_eq!(to_progress_line(" \t "), None);
    assert_eq!(
        split_output_lines(&"x".repeat(5_000), "y")
            .1
            .chars()
            .count(),
        4_096
    );
    assert_eq!(
        to_progress_line(&"x".repeat(300)).unwrap().chars().count(),
        200
    );
}

#[test]
fn a_multi_byte_character_split_across_chunks_decodes_whole() {
    // Added: upstream's `TextDecoder` runs with `stream: true`.
    let bytes = "50% ✓ done".as_bytes();
    let (first, second) = bytes.split_at(6); // inside the three-byte check mark
    let mut decoder = Utf8StreamDecoder::default();
    let mut text = decoder.push(first);
    text.push_str(&decoder.push(second));
    assert_eq!(text, "50% ✓ done");
}

#[tokio::test]
async fn aborts_without_running_when_the_installation_changed_since_the_advisory() {
    let calls: Calls = Arc::default();
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]).with_capabilities(
        |_id, provider, fresh| {
            if fresh {
                caps(provider, Some("@openai/codex"), None, &[], None)
            } else {
                lifecycle_for(provider)
            }
        },
    ));
    let updater = runner(&registry, recording_spawner(&calls, "updated"), "0.0.0");

    let result = updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(*calls.lock(), Vec::new());
    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Failed);
    assert_eq!(
        message_of(&result, 0),
        "Provider installation changed. Refresh and try again."
    );
    assert_eq!(
        registry
            .states()
            .into_iter()
            .map(|(status, _)| status)
            .collect::<Vec<_>>(),
        vec![
            ServerProviderUpdateStatus::Queued,
            ServerProviderUpdateStatus::Running,
            ServerProviderUpdateStatus::Failed
        ]
    );
}

#[tokio::test]
async fn uses_the_resolved_provider_capabilities_when_choosing_the_update_executable() {
    let calls: Calls = Arc::default();
    let registry = Arc::new(
        FakeRegistry::new(vec![provider(
            "codex",
            "codex",
            json!({
                "versionAdvisory": {
                    "status": "behind_latest",
                    "currentVersion": "2.0.14",
                    "latestVersion": "2.1.123",
                    "updateCommand": "bun i -g @anthropic-ai/claude-code@latest",
                    "canUpdate": true,
                    "checkedAt": "2026-04-30T12:00:00.000Z",
                    "message": "Update available.",
                },
            }),
        )])
        .with_capabilities(|_id, _provider, _fresh| {
            caps(
                &kind("codex"),
                Some("@openai/codex"),
                Some("bun"),
                &["i", "-g", "@openai/codex@latest"],
                Some("bun-global"),
            )
        }),
    );
    let updater = runner(&registry, recording_spawner(&calls, "updated"), "0.0.0");

    updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(
        *calls.lock(),
        vec![call("bun", &["i", "-g", "@openai/codex@latest"])]
    );
}

#[tokio::test]
async fn runs_update_commands_through_the_spawner_when_no_test_runner_is_injected() {
    let calls: Calls = Arc::default();
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]));
    let updater = runner(&registry, recording_spawner(&calls, "updated"), "0.0.0");

    let result = updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(
        *calls.lock(),
        vec![call("npm", &["install", "-g", "@openai/codex@latest"])]
    );
    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Succeeded);
}

#[tokio::test]
async fn updates_a_single_provider_instance_without_touching_sibling_instances() {
    let calls: Calls = Arc::default();
    let personal = id("codex_personal");
    let work = id("codex_work");
    let refreshed: Arc<Mutex<Vec<ProviderInstanceId>>> = Arc::default();
    let registry = {
        let personal = personal.clone();
        let refreshed = Arc::clone(&refreshed);
        Arc::new(
            FakeRegistry::new(vec![
                provider(
                    "codex_personal",
                    "codex",
                    json!({ "version": "0.124.0-alpha.3" }),
                ),
                provider(
                    "codex_work",
                    "codex",
                    json!({ "version": "0.124.0-alpha.3" }),
                ),
            ])
            .with_capabilities(move |instance_id, provider, _fresh| {
                assert_eq!(instance_id, &personal);
                caps(
                    provider,
                    Some("@openai/codex-instance-test"),
                    Some("vp"),
                    &["i", "-g", "@openai/codex"],
                    Some("vite-plus-global"),
                )
            })
            .with_refresh_instance(move |registry, instance_id| {
                refreshed.lock().push(instance_id.clone());
                registry.providers.lock().clone()
            }),
        )
    };
    let updater = runner(
        &registry,
        recording_spawner(&calls, "updated"),
        "0.124.0-alpha.3",
    );

    let result = updater
        .update_provider(ProviderUpdateTarget {
            provider: kind("codex"),
            instance_id: Some(personal.clone()),
            target_version: None,
        })
        .await
        .unwrap();

    assert_eq!(
        *calls.lock(),
        vec![call("vp", &["i", "-g", "@openai/codex"])]
    );
    assert_eq!(*refreshed.lock(), vec![personal.clone()]);
    assert_eq!(result.providers[0].instance_id, personal);
    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Succeeded);
    assert_eq!(result.providers[1].instance_id, work);
    assert_eq!(result.providers[1].update_state, None);
}

#[tokio::test]
async fn records_command_failure_output_in_provider_update_state() {
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]));
    let updater = runner(
        &registry,
        MockSpawner {
            handler: Box::new(|_, _, _| MockProcess {
                stdout: String::new(),
                stderr: "permission denied".to_owned(),
                code: 1,
                exit: None,
            }),
        },
        "0.0.0",
    );

    let result = updater.update_provider(kind("codex")).await.unwrap();
    let update_state = result.providers[0].update_state.as_ref().unwrap();

    assert_eq!(update_state.status, ServerProviderUpdateStatus::Failed);
    assert_eq!(
        update_state
            .message
            .as_ref()
            .map(|message| message.as_str()),
        Some("Update command exited with code 1.")
    );
    assert!(
        update_state
            .output
            .as_deref()
            .unwrap_or_default()
            .contains("permission denied")
    );
}

#[tokio::test]
async fn marks_successful_commands_as_unchanged_when_the_refreshed_provider_is_still_outdated() {
    let registry = Arc::new(FakeRegistry::new(vec![provider(
        "codex",
        "codex",
        json!({ "installed": true, "version": "0.1.0" }),
    )]));
    let updater = runner(
        &registry,
        MockSpawner {
            handler: Box::new(|_, _, _| MockProcess::stdout("updated")),
        },
        "9.9.9",
    );

    let result = updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Unchanged);
    assert!(message_of(&result, 0).contains("still detects"));
}

#[tokio::test]
async fn prevents_concurrent_updates_for_the_same_provider() {
    let started = Deferred::new();
    let release = Deferred::new();
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]));
    let updater = {
        let started = started.clone();
        let release = release.clone();
        Arc::new(runner(
            &registry,
            MockSpawner {
                handler: Box::new(move |_, _, _| {
                    started.succeed();
                    MockProcess {
                        stdout: "updated".to_owned(),
                        stderr: String::new(),
                        code: 0,
                        exit: Some(release.clone()),
                    }
                }),
            },
            "0.0.0",
        ))
    };

    let first = {
        let updater = Arc::clone(&updater);
        tokio::spawn(async move { updater.update_provider(kind("codex")).await })
    };
    started.wait().await;

    let second = updater.update_provider(kind("codex")).await;
    let error = second.unwrap_err();
    assert!(error.reason.contains("already running"));
    assert_eq!(error.provider, kind("codex"));

    release.succeed();
    first.await.unwrap().unwrap();
}

#[tokio::test]
async fn serializes_different_providers_that_share_the_same_update_lock_key() {
    let first_started = Deferred::new();
    let release_first = Deferred::new();
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let registry = Arc::new(FakeRegistry::new(vec![
        base_provider(),
        base_open_code_provider(),
    ]));
    let updater = {
        let first_started = first_started.clone();
        let release_first = release_first.clone();
        let calls = Arc::clone(&calls);
        Arc::new(runner(
            &registry,
            MockSpawner {
                handler: Box::new(move |_command, args, _env| {
                    let mut calls = calls.lock();
                    calls.push(args.join(" "));
                    if calls.len() == 1 {
                        first_started.succeed();
                        return MockProcess {
                            stdout: "updated".to_owned(),
                            stderr: String::new(),
                            code: 0,
                            exit: Some(release_first.clone()),
                        };
                    }
                    MockProcess::stdout("updated")
                }),
            },
            "2.0.0",
        ))
    };

    let first = {
        let updater = Arc::clone(&updater);
        tokio::spawn(async move { updater.update_provider(kind("codex")).await })
    };
    first_started.wait().await;

    let second = {
        let updater = Arc::clone(&updater);
        tokio::spawn(async move { updater.update_provider(kind("opencode")).await })
    };
    let queued_status = || {
        registry
            .providers
            .lock()
            .iter()
            .find(|provider| provider.instance_id == id("opencode"))
            .and_then(|provider| provider.update_state.as_ref().map(|state| state.status))
    };
    wait_until(|| queued_status() == Some(ServerProviderUpdateStatus::Queued)).await;
    assert_eq!(
        *calls.lock(),
        vec!["install -g @openai/codex@latest".to_owned()]
    );
    assert_eq!(queued_status(), Some(ServerProviderUpdateStatus::Queued));

    release_first.succeed();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(
        *calls.lock(),
        vec![
            "install -g @openai/codex@latest".to_owned(),
            "install -g opencode-ai@latest".to_owned(),
        ]
    );
}

#[tokio::test]
async fn accepts_arbitrary_driver_provided_update_lock_keys() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]).with_capabilities(
        |_id, provider, _fresh| {
            caps(
                provider,
                Some("@openai/codex"),
                Some("npm"),
                &["install", "-g", "@openai/codex@latest"],
                Some("unknown-lock-key"),
            )
        },
    ));
    let updater = {
        let calls = Arc::clone(&calls);
        runner(
            &registry,
            MockSpawner {
                handler: Box::new(move |_command, args, _env| {
                    calls.lock().push(args.join(" "));
                    MockProcess::stdout("updated")
                }),
            },
            "0.0.0",
        )
    };

    let result = updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Succeeded);
    assert_eq!(
        *calls.lock(),
        vec!["install -g @openai/codex@latest".to_owned()]
    );
}

#[tokio::test]
async fn releases_the_running_provider_marker_when_interrupted_after_queuing_but_before_the_lock_run_starts()
 {
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]));
    let queued_state_written = Deferred::new();
    let release_queued_state = Deferred::new();
    *registry.block_queued.lock() =
        Some((queued_state_written.clone(), release_queued_state.clone()));
    let updater = Arc::new(runner(
        &registry,
        MockSpawner {
            handler: Box::new(|_, _, _| MockProcess::stdout("updated")),
        },
        "0.0.0",
    ));

    let first = {
        let updater = Arc::clone(&updater);
        tokio::spawn(async move { updater.update_provider(kind("codex")).await })
    };
    queued_state_written.wait().await;
    *registry.block_queued.lock() = None;

    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    release_queued_state.succeed();

    let second = updater.update_provider(kind("codex")).await.unwrap();
    assert_eq!(status_of(&second, 0), ServerProviderUpdateStatus::Succeeded);
}

#[tokio::test]
async fn refuses_incompatible_latest_versions_and_unapproved_or_unpinnable_targets() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let compatibility = manifest(json!([{
        "driver": "codex",
        "t3CodeRange": ">=0.0.42",
        "recommendedVersion": "2.0.0",
        "ranges": [
            { "range": "=2.0.0", "status": "supported" },
            { "range": ">2.0.0", "status": "broken" },
        ],
    }]));
    let pinned_capabilities = caps(
        &kind("codex"),
        Some("@openai/codex"),
        Some("npm"),
        &["install", "-g", "@openai/codex@latest"],
        Some("npm-global:/fixture"),
    );
    let registry = {
        let pinned_capabilities = pinned_capabilities.clone();
        Arc::new(
            FakeRegistry::new(vec![base_provider()])
                .with_capabilities(move |_id, _provider, _fresh| pinned_capabilities.clone()),
        )
    };
    let spawner = || {
        let calls = Arc::clone(&calls);
        MockSpawner {
            handler: Box::new(move |_command, args, _env| {
                calls.lock().push(args.join(" "));
                MockProcess::stdout("installed")
            }),
        }
    };
    let updater = runner_with(
        Arc::clone(&registry),
        spawner(),
        "3.0.0",
        compatibility.clone(),
    );
    for target_version in [None, Some(""), Some("1.0.0"), Some("2.0.0; echo unsafe")] {
        let result = updater
            .update_provider(ProviderUpdateTarget {
                provider: kind("codex"),
                instance_id: None,
                target_version: target_version.map(str::to_owned),
            })
            .await
            .unwrap();
        assert_eq!(
            status_of(&result, 0),
            ServerProviderUpdateStatus::Failed,
            "target {target_version:?}"
        );
    }
    assert_eq!(*calls.lock(), Vec::<String>::new());
    for entry in registry.providers.lock().iter_mut() {
        entry.version = Some(TrimmedNonEmptyString::new("2.0.0").unwrap());
    }
    let installed = updater
        .update_provider(ProviderUpdateTarget {
            provider: kind("codex"),
            instance_id: None,
            target_version: Some("2.0.0".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(
        *calls.lock(),
        vec!["install -g @openai/codex@2.0.0".to_owned()]
    );
    assert_eq!(
        status_of(&installed, 0),
        ServerProviderUpdateStatus::Succeeded
    );
    for entry in registry.providers.lock().iter_mut() {
        entry.version = Some(TrimmedNonEmptyString::new("1.0.0").unwrap());
    }
    let unchanged = updater
        .update_provider(ProviderUpdateTarget {
            provider: kind("codex"),
            instance_id: None,
            target_version: Some("2.0.0".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(
        status_of(&unchanged, 0),
        ServerProviderUpdateStatus::Unchanged
    );

    let native_registry = {
        let mut native_capabilities = pinned_capabilities.clone();
        native_capabilities.update.as_mut().unwrap().lock_key = "codex-native".to_owned();
        Arc::new(
            FakeRegistry::new(registry.providers.lock().clone())
                .with_capabilities(move |_id, _provider, _fresh| native_capabilities.clone()),
        )
    };
    let native_updater = runner_with(native_registry, spawner(), "3.0.0", compatibility);
    let refused = native_updater
        .update_provider(ProviderUpdateTarget {
            provider: kind("codex"),
            instance_id: None,
            target_version: Some("2.0.0".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(status_of(&refused, 0), ServerProviderUpdateStatus::Failed);
    assert_eq!(calls.lock().len(), 2);
}

// ── Added: edges upstream covers through its mock spawner ──────────

#[tokio::test]
async fn a_provider_without_a_one_click_update_is_refused() {
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]).with_capabilities(
        |_id, provider, _fresh| caps(provider, Some("@openai/codex"), None, &[], None),
    ));
    let updater = runner(
        &registry,
        MockSpawner {
            handler: Box::new(|_, _, _| MockProcess::stdout("updated")),
        },
        "0.0.0",
    );

    let error = updater.update_provider(kind("codex")).await.unwrap_err();

    assert_eq!(error.provider, kind("codex"));
    assert_eq!(
        error.reason,
        "This provider does not support one-click updates."
    );
    assert_eq!(
        error.to_string(),
        "Provider update failed for codex: This provider does not support one-click updates."
    );
    assert_eq!(registry.states(), Vec::new());
}

#[tokio::test]
async fn a_spawn_failure_ends_as_a_failed_state() {
    struct FailingSpawner;

    #[async_trait]
    impl CommandSpawner for FailingSpawner {
        async fn spawn(
            &self,
            _command: &str,
            _args: &[String],
            _env: Option<&ProcessEnv>,
        ) -> Result<SpawnedCommand, String> {
            Err("spawn npm ENOENT".to_owned())
        }
    }

    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]));
    let updater = ProviderMaintenanceRunner::new(ProviderMaintenanceRunnerDeps {
        registry: Arc::clone(&registry) as Arc<dyn MaintenanceRegistry>,
        manifest: Arc::new(FakeManifest(default_manifest())),
        spawner: Arc::new(FailingSpawner),
        versions: Arc::new(FakeVersions {
            npm_latest: "0.0.0".to_owned(),
        }),
        t3_code_version: T3_CODE_VERSION.to_owned(),
    });

    let result = updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Failed);
    assert_eq!(
        message_of(&result, 0),
        "Failed to run update command npm: spawn npm ENOENT"
    );
}

#[tokio::test]
async fn a_command_that_outlives_the_timeout_ends_as_timed_out() {
    let hang = Deferred::new();
    let registry = Arc::new(FakeRegistry::new(vec![base_provider()]));
    let updater = {
        let hang = hang.clone();
        runner(
            &registry,
            MockSpawner {
                handler: Box::new(move |_, _, _| MockProcess {
                    stdout: String::new(),
                    stderr: String::new(),
                    code: 0,
                    exit: Some(hang.clone()),
                }),
            },
            "0.0.0",
        )
        .with_timing(Duration::from_millis(50), Duration::from_secs(1))
    };

    let result = updater.update_provider(kind("codex")).await.unwrap();

    assert_eq!(status_of(&result, 0), ServerProviderUpdateStatus::Failed);
    assert_eq!(message_of(&result, 0), "Update timed out.");
}

// ── Added: the real spawner (unix) ─────────────────────────────────

#[cfg(unix)]
mod tokio_spawner {
    use pretty_assertions::assert_eq;

    use super::*;

    async fn run(
        command: &str,
        args: &[&str],
        env: Option<&ProcessEnv>,
    ) -> ProviderMaintenanceCommandResult {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let action = ProviderMaintenanceCommandAction {
            command: command.to_owned(),
            executable: command.to_owned(),
            args,
            lock_key: "test".to_owned(),
            env: env.cloned(),
        };
        run_provider_maintenance_command(
            &TokioCommandSpawner,
            &action,
            Arc::new(|_line| Box::pin(async {})),
            Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn collects_output_and_the_exit_code() {
        let result = run("/bin/sh", &["-c", "echo out; echo err >&2; exit 3"], None).await;
        assert_eq!(result.stdout, "out\n");
        assert_eq!(result.stderr, "err\n");
        assert_eq!(result.exit_code, Some(3));
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn extends_the_host_environment_with_the_declared_one() {
        let env = HashMap::from([(
            "TURIN_RUNNER_TEST".to_owned(),
            "from-capabilities".to_owned(),
        )]);
        let result = run(
            "/bin/sh",
            &[
                "-c",
                "printf '%s:%s' \"$TURIN_RUNNER_TEST\" \"${PATH:+path}\"",
            ],
            Some(&env),
        )
        .await;
        assert_eq!(result.stdout, "from-capabilities:path");
    }

    #[tokio::test]
    async fn caps_collected_output_and_reports_truncation() {
        let result = run(
            "/bin/sh",
            &["-c", "head -c 20000 /dev/zero | tr '\\0' 'x'"],
            None,
        )
        .await;
        assert_eq!(result.stdout.len(), UPDATE_OUTPUT_MAX_BYTES);
        assert!(result.stdout_truncated);
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn a_timeout_kills_the_updater_and_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("sleep 600 & echo $! > {}; wait", pid_file.display());
        let action = ProviderMaintenanceCommandAction {
            command: "sh".to_owned(),
            executable: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script],
            lock_key: "test".to_owned(),
            env: None,
        };
        let result = run_provider_maintenance_command(
            &TokioCommandSpawner,
            &action,
            Arc::new(|_line| Box::pin(async {})),
            Duration::from_millis(500),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None);

        let child_pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("the script recorded its child's pid")
            .trim()
            .parse()
            .unwrap();
        let alive = || {
            std::process::Command::new("kill")
                .args(["-0", &child_pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        // A killed orphan is reaped by init shortly after its group is signalled.
        tokio::time::timeout(Duration::from_secs(5), async {
            while alive() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the background child of the updater survived the timeout");
    }

    #[tokio::test]
    async fn a_missing_executable_is_a_spawn_failure() {
        let action = ProviderMaintenanceCommandAction {
            command: "t3code-no-such-updater".to_owned(),
            executable: "t3code-no-such-updater".to_owned(),
            args: Vec::new(),
            lock_key: "test".to_owned(),
            env: None,
        };
        let error = run_provider_maintenance_command(
            &TokioCommandSpawner,
            &action,
            Arc::new(|_line| Box::pin(async {})),
            Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .message
                .starts_with("Failed to run update command t3code-no-such-updater:"),
            "{}",
            error.message
        );
    }
}
