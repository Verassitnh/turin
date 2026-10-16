//! Port of upstream/apps/server/src/provider/ProviderRegistry.test.ts,
//! `ProviderRegistry.layer` block.
//!
//! Not ported:
//! - the ACP Registry and Antigravity cases (both providers are cut);
//! - `checkCodexProviderStatus` / `checkClaudeProviderStatus` (driver probes,
//!   owned by the Codex and Claude drivers, not the registry);
//! - the three cases that boot the real hydration layer with real Codex or
//!   Cursor probes ("propagates real Codex probe failures", "re-probes when
//!   settings change the codex binaryPath", "keeps Cursor disabled ..."); they
//!   need the Codex driver and settings-driven hydration. The rebuild path is
//!   covered by "deduplicates cwd probes and clears snapshots when an instance
//!   rebuilds".
//!
//! Upstream stubs `ProviderInstanceRegistry` with a record; here
//! [`TestSource`] plays that part. Time is a [`TestClock`] (upstream
//! `TestClock`), and `Deferred` is a one-shot gate.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use turin_contracts::{
    git::TextGenerationError,
    orchestration_v2::OrchestrationV2ProviderCapabilities,
    provider_instance::{
        ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
    },
    provider_usage_limits::ProviderUsageLimitsUpdate,
};
use turin_provider_core::{
    driver::{InvalidateCaches, ProviderContinuationIdentity, ProviderInstance, SnapshotForCwd},
    maintenance::ProviderMaintenanceCapabilities,
    maintenance_resolver::make_manual_only_provider_maintenance_capabilities,
    model_manifest::{ModelManifest, UpdateChecks, bundled_model_manifest},
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2Error,
        ProviderAdapterV2OpenSessionInput, ProviderSessionRuntime,
    },
    provider_compatibility::{
        apply_provider_compatibility, decode_provider_compatibility_policies,
    },
    provider_instance_registry::ProviderInstanceRegistry,
    provider_status_cache::{
        read_provider_status_cache, resolve_provider_status_cache_path, write_provider_status_cache,
    },
    rt::{BoxFuture, BoxStream},
    selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
    server_provider::{
        PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS, ProviderWorkspaceSnapshot, ServerProvider,
        ServerProviderCompatibilityStatus, ServerProviderState, ServerProviderUpdateState,
        ServerProviderWorkspaceSnapshot,
    },
    snapshot::ManagedServerProvider,
    text_generation::{
        BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
        CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
        ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
        ThreadTitleGenerationInput, ThreadTitleGenerationResult,
    },
};

use super::*;

const T3_CODE_VERSION: &str = "1.0.0";

/// `COMPACT_SLASH_COMMAND` from upstream `snapshotProbe.ts`.
fn compact_slash_command() -> Value {
    json!({
        "name": "compact",
        "description": "Summarize the conversation and reduce context usage",
    })
}

// ── Test helpers ────────────────────────────────────────────────────

fn driver_kind(value: &str) -> ProviderDriverKind {
    ProviderDriverKind::new(value).unwrap()
}

fn instance_id(value: &str) -> ProviderInstanceId {
    ProviderInstanceId::new(value).unwrap()
}

/// A ready, authenticated, enabled provider with no models, as JSON so each
/// case overrides only what upstream's `...spread` overrides.
fn base(instance: &str, driver: &str) -> Value {
    json!({
        "instanceId": instance,
        "driver": driver,
        "status": "ready",
        "enabled": true,
        "installed": true,
        "auth": { "status": "authenticated" },
        "checkedAt": "2026-04-14T00:00:00.000Z",
        "version": "1.0.0",
        "models": [],
        "slashCommands": [],
        "skills": [],
    })
}

fn with(base: &Value, overrides: Value) -> Value {
    let mut merged = base.clone();
    for (key, value) in overrides.as_object().unwrap() {
        merged[key] = value.clone();
    }
    merged
}

fn to_provider(value: &Value) -> ServerProvider {
    serde_json::from_value(value.clone()).unwrap()
}

fn model(slug: &str, name: &str, is_custom: bool) -> Value {
    json!({ "slug": slug, "name": name, "isCustom": is_custom, "capabilities": null })
}

fn sub_provider_model(slug: &str, name: &str, sub_provider: &str) -> Value {
    json!({
        "slug": slug, "name": name, "subProvider": sub_provider,
        "isCustom": false, "capabilities": null,
    })
}

fn select_descriptor(id: &str, label: &str, default_option: &str) -> Value {
    json!({
        "id": id,
        "label": label,
        "type": "select",
        "options": [{ "id": default_option, "label": "High", "isDefault": true }],
        "currentValue": default_option,
    })
}

fn boolean_descriptor(id: &str, label: &str) -> Value {
    json!({ "id": id, "label": label, "type": "boolean" })
}

fn capabilities(descriptors: Vec<Value>) -> Value {
    json!({ "optionDescriptors": descriptors })
}

fn bundled_policies()
-> Option<Vec<turin_provider_core::provider_compatibility::ProviderCompatibilityPolicy>> {
    bundled_model_manifest()
        .compatibility
        .as_deref()
        .map(decode_provider_compatibility_policies)
}

fn with_bundled_compatibility(snapshot: ServerProvider) -> ServerProvider {
    apply_provider_compatibility(
        snapshot,
        None,
        bundled_policies().as_deref(),
        T3_CODE_VERSION,
    )
}

fn scoped(provider: &Value, pending: Option<bool>) -> ProviderWorkspaceSnapshot {
    ProviderWorkspaceSnapshot {
        provider: to_provider(provider),
        slash_commands_pending: pending,
    }
}

fn workspace_snapshot(value: Value) -> ServerProviderWorkspaceSnapshot {
    serde_json::from_value(value).unwrap()
}

/// `Deferred<void>`: completes once, can be awaited any number of times.
#[derive(Clone)]
struct Deferred(Arc<watch::Sender<bool>>);

impl Deferred {
    fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    fn succeed(&self) {
        self.0.send_replace(true);
    }

    async fn wait(&self) {
        let mut receiver = self.0.subscribe();
        receiver.wait_for(|done| *done).await.unwrap();
    }
}

/// Upstream `TestClock`.
#[derive(Clone)]
struct TestClock(Arc<AtomicI64>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(AtomicI64::new(0)))
    }

    fn adjust(&self, millis: i64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }

    fn clock(&self) -> Clock {
        let now = Arc::clone(&self.0);
        Arc::new(move || now.load(Ordering::SeqCst))
    }
}

/// Spins on `condition`, yielding between checks. Bounded so a regression fails
/// instead of hanging.
async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition was not met");
}

struct StubAdapter {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
}

#[async_trait]
impl ProviderAdapterV2 for StubAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }

    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }

    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::Capabilities {
                driver: self.driver.clone(),
            },
        ))
    }

    async fn plan_selection_transition(
        &self,
        _input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::Capabilities {
                driver: self.driver.clone(),
            },
        ))
    }

    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::OpenSession {
                driver: self.driver.clone(),
                provider_session_id: input.provider_session_id,
            },
        ))
    }
}

struct StubText;

fn text_error() -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: "stub".to_owned(),
        detail: "stub text generation".to_owned(),
        cause: None,
    }
}

#[async_trait]
impl ProviderTextGeneration for StubText {
    async fn generate_commit_message(
        &self,
        _input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        Err(text_error())
    }
    async fn generate_pr_content(
        &self,
        _input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        Err(text_error())
    }
    async fn generate_branch_name(
        &self,
        _input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        Err(text_error())
    }
    async fn generate_thread_title(
        &self,
        _input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        Err(text_error())
    }
    async fn generate_review_guide(
        &self,
        _input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        Err(text_error())
    }
}

/// The `snapshot` record every upstream case builds inline.
struct TestSnapshot {
    driver: ProviderDriverKind,
    snapshot: Box<dyn Fn() -> ServerProvider + Send + Sync>,
    refresh: Box<dyn Fn() -> BoxFuture<'static, ServerProvider> + Send + Sync>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<ServerProvider>>>,
}

impl TestSnapshot {
    fn new(
        driver: ProviderDriverKind,
        snapshot: impl Fn() -> ServerProvider + Send + Sync + 'static,
        refresh: impl Fn() -> BoxFuture<'static, ServerProvider> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            driver,
            snapshot: Box::new(snapshot),
            refresh: Box::new(refresh),
            subscribers: Mutex::new(Vec::new()),
        })
    }

    /// `getSnapshot` and `refresh` both answer `provider`.
    fn fixed(provider: &ServerProvider) -> Arc<Self> {
        let current = provider.clone();
        let refreshed = provider.clone();
        Self::new(
            provider.driver.clone(),
            move || current.clone(),
            move || {
                let refreshed = refreshed.clone();
                Box::pin(async move { refreshed })
            },
        )
    }

    fn publish(&self, provider: &ServerProvider) {
        self.subscribers
            .lock()
            .retain(|subscriber| subscriber.send(provider.clone()).is_ok());
    }
}

#[async_trait]
impl ManagedServerProvider for TestSnapshot {
    async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        make_manual_only_provider_maintenance_capabilities(self.driver.clone())
    }

    async fn get_snapshot(&self) -> ServerProvider {
        (self.snapshot)()
    }

    async fn refresh(&self) -> ServerProvider {
        (self.refresh)().await
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.subscribers.lock().push(sender);
        Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(
            receiver,
        ))
    }

    async fn apply_usage_limits(&self, _update: ProviderUsageLimitsUpdate, _checked_at: &str) {}
}

fn make_instance(
    provider: &ServerProvider,
    snapshot: Arc<TestSnapshot>,
    snapshot_for_cwd: Option<SnapshotForCwd>,
    invalidate_caches: Option<InvalidateCaches>,
) -> Arc<ProviderInstance> {
    Arc::new(ProviderInstance {
        instance_id: provider.instance_id.clone(),
        driver_kind: provider.driver.clone(),
        continuation_identity: ProviderContinuationIdentity {
            driver_kind: provider.driver.clone(),
            continuation_key: format!("{}:instance:{}", provider.driver, provider.instance_id),
        },
        display_name: None,
        accent_color: None,
        enabled: true,
        snapshot,
        snapshot_for_cwd,
        refresh_models: None,
        invalidate_caches,
        consume_reset_credit: None,
        orchestration_adapter: Arc::new(StubAdapter {
            instance_id: provider.instance_id.clone(),
            driver: provider.driver.clone(),
        }),
        text_generation: Arc::new(StubText),
        auth: None,
    })
}

fn fixed_instance(provider: &ServerProvider) -> Arc<ProviderInstance> {
    make_instance(provider, TestSnapshot::fixed(provider), None, None)
}

/// Upstream's stubbed `ProviderInstanceRegistry` service.
struct TestSource {
    instances: Mutex<Vec<Arc<ProviderInstance>>>,
    unavailable: Mutex<Vec<ServerProvider>>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<()>>>,
    fail_next_list: AtomicBool,
}

impl TestSource {
    fn new(instances: Vec<Arc<ProviderInstance>>) -> Arc<Self> {
        Arc::new(Self {
            instances: Mutex::new(instances),
            unavailable: Mutex::new(Vec::new()),
            subscribers: Mutex::new(Vec::new()),
            fail_next_list: AtomicBool::new(false),
        })
    }

    fn set_instances(&self, instances: Vec<Arc<ProviderInstance>>) {
        *self.instances.lock() = instances;
    }

    fn publish_change(&self) {
        self.subscribers
            .lock()
            .retain(|subscriber| subscriber.send(()).is_ok());
    }
}

impl ProviderInstanceSource for TestSource {
    fn get_instance(&self, id: &ProviderInstanceId) -> Option<Arc<ProviderInstance>> {
        self.instances
            .lock()
            .iter()
            .find(|instance| &instance.instance_id == id)
            .cloned()
    }

    fn list_instances(&self) -> Vec<Arc<ProviderInstance>> {
        if self.fail_next_list.swap(false, Ordering::SeqCst) {
            panic!("simulated registry list failure");
        }
        self.instances.lock().clone()
    }

    fn list_unavailable(&self) -> Vec<ServerProvider> {
        self.unavailable.lock().clone()
    }

    fn subscribe_changes(&self) -> mpsc::UnboundedReceiver<()> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.subscribers.lock().push(sender);
        receiver
    }
}

/// `ModelManifest.layerTest`: the bundled manifest, no network.
fn test_manifest(dir: &std::path::Path) -> Arc<ModelManifest> {
    let checks: UpdateChecks = Arc::new(|| Box::pin(async { false }));
    Arc::new(ModelManifest::new(dir.join("manifest"), checks))
}

async fn start_registry_with(
    source: Arc<dyn ProviderInstanceSource>,
    dir: &std::path::Path,
    clock: Option<Clock>,
) -> Arc<ProviderRegistry> {
    Arc::new(
        ProviderRegistry::start(
            source,
            test_manifest(dir),
            ProviderRegistryOptions {
                provider_status_cache_dir: dir.join("provider-status-cache"),
                t3_code_version: T3_CODE_VERSION.to_owned(),
                clock,
            },
        )
        .await,
    )
}

async fn start_registry(
    source: Arc<dyn ProviderInstanceSource>,
    dir: &std::path::Path,
) -> Arc<ProviderRegistry> {
    start_registry_with(source, dir, None).await
}

fn cache_path(dir: &std::path::Path, id: &ProviderInstanceId) -> std::path::PathBuf {
    resolve_provider_status_cache_path(&dir.join("provider-status-cache"), id)
}

/// The registry writes the status cache and only then publishes the change, so
/// a subscriber that sees `checkedAt` on the stream knows the file is on disk.
/// Subscribe before the publish that triggers it.
fn await_persisted_provider(
    registry: &ProviderRegistry,
    checked_at: &str,
) -> tokio::task::JoinHandle<()> {
    let mut changes = registry.stream_changes();
    let checked_at = checked_at.to_owned();
    tokio::spawn(async move {
        while let Some(providers) = changes.next().await {
            if providers
                .iter()
                .any(|provider| provider.checked_at == checked_at)
            {
                return;
            }
        }
    })
}

// ── Pure merge cases ────────────────────────────────────────────────

#[test]
fn stores_workspace_skills_and_commands_without_changing_machine_metadata() {
    let machine = with(
        &base("codex", "codex"),
        json!({
            "checkedAt": "2026-03-25T00:00:00.000Z",
            "slashCommands": [{ "name": "global" }],
            "skills": [{ "name": "global", "path": "/global/SKILL.md", "enabled": true }],
        }),
    );
    let provider = to_provider(&machine);
    let scoped_json = with(
        &machine,
        json!({
            "checkedAt": "2026-03-25T00:01:00.000Z",
            "slashCommands": [{ "name": "project" }],
            "skills": [{ "name": "project", "path": "/project/SKILL.md", "enabled": true }],
        }),
    );
    let scoped_snapshot = scoped(&scoped_json, None);

    let result = upsert_provider_workspace_snapshot(&provider, "/project", &scoped_snapshot);

    assert_eq!(result.slash_commands, provider.slash_commands);
    assert_eq!(result.skills, provider.skills);
    assert_eq!(
        result.workspace_snapshots,
        Some(vec![workspace_snapshot(json!({
            "cwd": "/project",
            "checkedAt": scoped_snapshot.provider.checked_at,
            "slashCommands": scoped_json["slashCommands"],
            "skills": scoped_json["skills"],
        }))])
    );

    let pending_snapshot = scoped(
        &with(
            &scoped_json,
            json!({ "slashCommands": [compact_slash_command()] }),
        ),
        Some(true),
    );
    let partial = upsert_provider_workspace_snapshot(&result, "/project", &pending_snapshot);
    let partial_snapshots = partial.workspace_snapshots.as_ref().unwrap();
    assert_eq!(
        partial_snapshots[0].slash_commands,
        to_provider(&scoped_json).slash_commands
    );
    assert_eq!(partial_snapshots[0].slash_commands_pending, Some(true));
    let other_project =
        upsert_provider_workspace_snapshot(&partial, "/other-project", &pending_snapshot);
    assert_eq!(
        other_project.workspace_snapshots.as_ref().unwrap()[1].slash_commands,
        pending_snapshot.provider.slash_commands
    );
    let recovered = upsert_provider_workspace_snapshot(
        &partial,
        "/project",
        &scoped(
            &with(
                &scoped_json,
                json!({ "slashCommands": [compact_slash_command(), { "name": "replacement" }] }),
            ),
            None,
        ),
    );
    let recovered_snapshots = recovered.workspace_snapshots.as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(&recovered_snapshots[0].slash_commands).unwrap(),
        json!([compact_slash_command(), { "name": "replacement" }])
    );
    assert_eq!(recovered_snapshots[0].slash_commands_pending, None);
}

#[test]
fn caps_workspace_snapshots_per_provider_to_the_newest_sixteen() {
    // Added: upstream slices to the last MAX_WORKSPACE_SNAPSHOTS_PER_PROVIDER.
    let mut provider = to_provider(&base("codex", "codex"));
    for index in 0..20 {
        provider = upsert_provider_workspace_snapshot(
            &provider,
            &format!("/project-{index}"),
            &scoped(&base("codex", "codex"), None),
        );
    }
    let cwds: Vec<&str> = provider
        .workspace_snapshots
        .as_ref()
        .unwrap()
        .iter()
        .map(|snapshot| snapshot.cwd.as_str())
        .collect();
    assert_eq!(cwds.len(), 16);
    assert_eq!(cwds.first(), Some(&"/project-4"));
    assert_eq!(cwds.last(), Some(&"/project-19"));
}

#[test]
fn preserves_previously_discovered_provider_models_when_a_refresh_returns_none() {
    let previous_json = with(
        &base("cursor", "cursor"),
        json!({
            "version": "2026.04.09-f2b0fcd",
            "models": [{
                "slug": "claude-opus-4-6",
                "name": "Opus 4.6",
                "isCustom": false,
                "capabilities": capabilities(vec![
                    select_descriptor("reasoning", "Reasoning", "high"),
                    boolean_descriptor("fastMode", "Fast Mode"),
                    boolean_descriptor("thinking", "Thinking"),
                ]),
            }],
            "slashCommands": [{ "name": "review", "description": "Review changes" }],
            "skills": [{
                "name": "typescript",
                "description": "TypeScript help",
                "path": "/skills/typescript/SKILL.md",
                "enabled": true,
            }],
        }),
    );
    let previous_provider = to_provider(&previous_json);
    let refreshed_provider = to_provider(&with(
        &previous_json,
        json!({
            "checkedAt": "2026-04-14T00:01:00.000Z",
            "models": [],
            "slashCommands": [],
            "skills": [],
        }),
    ));

    let merged = merge_provider_snapshot(Some(&previous_provider), &refreshed_provider);
    assert_eq!(merged.models, previous_provider.models);
    assert_eq!(merged.slash_commands, Vec::new());
    assert_eq!(merged.skills, Vec::new());
}

#[test]
fn drops_custom_models_the_refreshed_snapshot_no_longer_carries() {
    let previous_json = with(
        &base("claudeAgent", "claudeAgent"),
        json!({
            "version": "2.1.0",
            "models": [
                model("claude-sonnet-4-6", "Sonnet 4.6", false),
                model("removed-custom", "removed-custom", true),
            ],
        }),
    );
    let previous_provider = to_provider(&previous_json);
    let refreshed_provider = to_provider(&with(
        &previous_json,
        json!({
            "checkedAt": "2026-04-14T00:01:00.000Z",
            "models": [model("claude-sonnet-4-6", "Sonnet 4.6", false)],
        }),
    ));

    assert_eq!(
        merge_provider_snapshot(Some(&previous_provider), &refreshed_provider).models,
        refreshed_provider.models
    );
}

#[test]
fn does_not_bring_back_models_the_installed_cli_is_too_old_to_run() {
    // The pending snapshot lists the whole catalog before the version is known.
    let pending_json = with(
        &base("claudeAgent", "claudeAgent"),
        json!({
            "status": "warning",
            "installed": false,
            "auth": { "status": "unknown" },
            "version": null,
            "models": [
                model("claude-old", "Old", false),
                model("claude-next", "Next", false),
            ],
        }),
    );
    let pending_provider = to_provider(&pending_json);
    let probed_provider = to_provider(&with(
        &pending_json,
        json!({
            "status": "ready",
            "installed": true,
            "auth": { "status": "authenticated" },
            "version": "1.0.0",
            "models": [model("claude-old", "Old", false)],
            "updateRequiredModels": [
                { "slug": "claude-next", "name": "Next", "minVersion": "1.1.0" },
            ],
        }),
    ));

    assert_eq!(
        merge_provider_snapshot(Some(&pending_provider), &probed_provider).models,
        probed_provider.models
    );
}

fn open_code_previous(models: Vec<Value>) -> Value {
    with(
        &base("opencode", "opencode"),
        json!({ "checkedAt": "2026-07-17T00:00:00.000Z", "models": models }),
    )
}

#[test]
fn drops_stale_open_code_models_missing_from_a_successful_refresh() {
    let previous_json = open_code_previous(vec![
        sub_provider_model("github/gpt-5", "GPT-5", "GitHub"),
        sub_provider_model(
            "removed-plugin/model",
            "Removed Plugin Model",
            "Removed Plugin",
        ),
    ]);
    let previous_provider = to_provider(&previous_json);
    let refreshed_provider = to_provider(&with(
        &previous_json,
        json!({
            "checkedAt": "2026-07-17T00:01:00.000Z",
            "models": [sub_provider_model("github/gpt-5", "GPT-5", "GitHub")],
        }),
    ));

    assert_eq!(
        merge_provider_snapshot(Some(&previous_provider), &refreshed_provider).models,
        refreshed_provider.models
    );
}

fn open_code_with_metadata() -> Value {
    with(
        &open_code_previous(vec![sub_provider_model("github/gpt-5", "GPT-5", "GitHub")]),
        json!({
            "slashCommands": [{ "name": "review", "description": "Review changes" }],
            "skills": [{
                "name": "typescript",
                "description": "TypeScript help",
                "path": "/skills/typescript/SKILL.md",
                "enabled": true,
            }],
        }),
    )
}

#[test]
fn retains_stale_open_code_models_when_a_refresh_fails() {
    let previous_json = open_code_with_metadata();
    let previous_provider = to_provider(&previous_json);
    let refreshed_provider = to_provider(&with(
        &previous_json,
        json!({
            "status": "error",
            "auth": { "status": "unknown" },
            "checkedAt": "2026-07-17T00:01:00.000Z",
            "models": [],
            "message": "Failed to refresh OpenCode models.",
        }),
    ));

    let merged = merge_provider_snapshot(Some(&previous_provider), &refreshed_provider);
    assert_eq!(merged.models, previous_provider.models);
    assert_eq!(merged.slash_commands, previous_provider.slash_commands);
    assert_eq!(merged.skills, previous_provider.skills);
}

#[test]
fn classifies_pending_logout_uninstall_and_reconnect_open_code_inventories() {
    let previous_json = with(
        &open_code_previous(vec![
            sub_provider_model("github/gpt-5", "GPT-5", "GitHub"),
            sub_provider_model(
                "removed-plugin/model",
                "Removed Plugin Model",
                "Removed Plugin",
            ),
        ]),
        json!({
            "slashCommands": [{ "name": "review", "description": "Review changes" }],
            "skills": [{
                "name": "typescript",
                "description": "TypeScript help",
                "path": "/skills/typescript/SKILL.md",
                "enabled": true,
            }],
        }),
    );
    let previous_provider = to_provider(&previous_json);
    let pending_provider = to_provider(&with(
        &previous_json,
        json!({
            "status": "warning",
            "installed": false,
            "auth": { "status": "unknown" },
            "checkedAt": "2026-07-17T00:01:00.000Z",
            "version": null,
            "models": [],
            "message": "OpenCode provider status has not been checked in this session yet.",
        }),
    ));
    let logged_out_provider = to_provider(&with(
        &previous_json,
        json!({
            "status": "warning",
            "auth": { "status": "unknown" },
            "checkedAt": "2026-07-17T00:02:00.000Z",
            "models": [],
            "slashCommands": [],
            "skills": [],
            "message": "OpenCode is available, but it did not report any connected upstream providers.",
        }),
    ));
    let missing_provider = to_provider(&with(
        &previous_json,
        json!({
            "status": "error",
            "installed": false,
            "auth": { "status": "unknown" },
            "checkedAt": "2026-07-17T00:03:00.000Z",
            "version": null,
            "models": [],
            "message": "OpenCode CLI (`opencode`) is not installed or not on PATH.",
        }),
    ));
    let authoritative_json = with(
        &previous_json,
        json!({
            "checkedAt": "2026-07-17T00:04:00.000Z",
            "models": [previous_json["models"][0].clone()],
        }),
    );
    let authoritative_provider = to_provider(&authoritative_json);
    let failed_provider = to_provider(&with(
        &authoritative_json,
        json!({
            "status": "error",
            "auth": { "status": "unknown" },
            "checkedAt": "2026-07-17T00:05:00.000Z",
            "models": [],
            "message": "Failed to refresh OpenCode models.",
        }),
    ));

    assert_eq!(
        merge_provider_snapshot(Some(&previous_provider), &pending_provider).models,
        previous_provider.models
    );
    let logged_out = merge_provider_snapshot(Some(&previous_provider), &logged_out_provider);
    assert_eq!(logged_out.models, Vec::new());
    assert_eq!(logged_out.slash_commands, Vec::new());
    assert_eq!(logged_out.skills, Vec::new());
    assert_eq!(
        merge_provider_snapshot(Some(&previous_provider), &missing_provider).models,
        Vec::new()
    );

    let after_removal = merge_provider_snapshot(Some(&previous_provider), &authoritative_provider);
    let after_failure = merge_provider_snapshot(Some(&after_removal), &failed_provider);

    assert_eq!(after_failure.models, authoritative_provider.models);
}

mod codex_model_inventories {
    use pretty_assertions::assert_eq;

    use super::*;

    fn cached_json() -> Value {
        let models: Vec<Value> = [
            "vega-alpha",
            "joule-alpha",
            "kindle-alpha",
            "ultima-alpha",
            "solstice-alpha",
        ]
        .iter()
        .map(|slug| model(slug, slug, false))
        .collect();
        with(
            &base("codex-personal", "codex"),
            json!({
                "checkedAt": "2026-09-04T19:00:00.000Z",
                "version": "0.153.3",
                "models": models,
            }),
        )
    }

    fn custom_model() -> Value {
        model("custom-model", "Custom model", true)
    }

    fn refreshed_json() -> Value {
        with(
            &cached_json(),
            json!({
                "checkedAt": "2026-09-04T19:01:00.000Z",
                "models": [
                    model("gpt-6-astra", "GPT 6 Astra", false),
                    cached_json()["models"][0].clone(),
                    custom_model(),
                ],
            }),
        )
    }

    fn pending_json() -> Value {
        with(
            &cached_json(),
            json!({
                "status": "warning",
                "installed": false,
                "auth": { "status": "unknown" },
                "models": [custom_model()],
            }),
        )
    }

    fn failed_json() -> Value {
        with(
            &pending_json(),
            json!({
                "checkedAt": "2026-09-04T19:02:00.000Z",
                "status": "error",
                "installed": true,
            }),
        )
    }

    #[test]
    fn drops_retired_alpha_models_after_discovery_including_without_open_ai_authentication() {
        let cached_provider = to_provider(&cached_json());
        for auth_status in ["authenticated", "unknown"] {
            let refreshed = to_provider(&with(
                &refreshed_json(),
                json!({ "auth": { "status": auth_status } }),
            ));
            assert_eq!(
                merge_provider_snapshot(Some(&cached_provider), &refreshed).models,
                to_provider(&refreshed_json()).models
            );
        }
    }

    #[test]
    fn keeps_discovered_models_during_startup_and_failed_probes_without_restoring_removed_custom_models()
     {
        for provider_json in [pending_json(), failed_json()] {
            let mut previous_models = cached_json()["models"].as_array().unwrap().clone();
            previous_models.push(model("removed-custom", "Custom model", true));
            let previous = to_provider(&with(&cached_json(), json!({ "models": previous_models })));
            let mut expected = vec![custom_model()];
            expected.extend(cached_json()["models"].as_array().unwrap().clone());
            assert_eq!(
                merge_provider_snapshot(Some(&previous), &to_provider(&provider_json)).models,
                to_provider(&with(&cached_json(), json!({ "models": expected }))).models
            );
        }
    }

    #[test]
    fn clears_discovered_models_after_sign_out_disable_uninstall_or_empty_discovery() {
        let empty_json = with(&refreshed_json(), json!({ "models": [custom_model()] }));
        let cleared = [
            with(
                &empty_json,
                json!({ "status": "error", "auth": { "status": "unauthenticated" } }),
            ),
            with(
                &empty_json,
                json!({ "status": "disabled", "enabled": false }),
            ),
            with(
                &empty_json,
                json!({ "status": "error", "installed": false, "auth": { "status": "unknown" } }),
            ),
            empty_json.clone(),
            with(&empty_json, json!({ "models": [] })),
        ];
        let cached_provider = to_provider(&cached_json());

        for provider_json in cleared {
            let provider = to_provider(&provider_json);
            assert_eq!(
                merge_provider_snapshot(Some(&cached_provider), &provider).models,
                provider.models
            );
        }
    }

    #[tokio::test]
    async fn persists_removals_across_failed_refreshes_and_registry_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let cached_provider = to_provider(&cached_json());
        let file_path = cache_path(dir.path(), &cached_provider.instance_id);
        write_provider_status_cache(&file_path, &cached_provider)
            .await
            .unwrap();
        let next_provider = Arc::new(Mutex::new(to_provider(&refreshed_json())));
        let pending_provider = to_provider(&pending_json());
        let snapshot = {
            let next_provider = Arc::clone(&next_provider);
            TestSnapshot::new(
                cached_provider.driver.clone(),
                move || pending_provider.clone(),
                move || {
                    let next = next_provider.lock().clone();
                    Box::pin(async move { next })
                },
            )
        };
        let instance = make_instance(&cached_provider, snapshot, None, None);
        let source = TestSource::new(vec![Arc::clone(&instance)]);

        let refreshed_models = to_provider(&refreshed_json()).models.clone();
        let mut retained_models = vec![
            to_provider(&with(&cached_json(), json!({ "models": [custom_model()] })))
                .models
                .remove(0),
        ];
        retained_models.extend(
            refreshed_models
                .iter()
                .filter(|model| !model.is_custom)
                .cloned(),
        );

        for restarted in [false, true] {
            let registry = start_registry(source.clone(), dir.path()).await;
            let expected_models = if restarted {
                retained_models.clone()
            } else {
                let mut models = vec![retained_models[0].clone()];
                models.extend(cached_provider.models.clone());
                models
            };
            assert_eq!(registry.get_providers()[0].models, expected_models);

            registry.refresh_instance(&instance.instance_id).await;
            assert_eq!(
                read_provider_status_cache(&file_path).await.unwrap().models,
                if restarted {
                    retained_models.clone()
                } else {
                    refreshed_models.clone()
                }
            );

            *next_provider.lock() = to_provider(&failed_json());
            let after_failure = registry.refresh_instance(&instance.instance_id).await;
            assert_eq!(after_failure[0].models, retained_models);
            assert_eq!(
                read_provider_status_cache(&file_path).await.unwrap().models,
                retained_models
            );
        }
    }
}

#[test]
fn fills_missing_capabilities_from_the_previous_provider_snapshot() {
    let previous_json = with(
        &base("cursor", "cursor"),
        json!({
            "version": "2026.04.09-f2b0fcd",
            "models": [{
                "slug": "claude-opus-4-6",
                "name": "Opus 4.6",
                "isCustom": false,
                "capabilities": capabilities(vec![
                    select_descriptor("reasoning", "Reasoning", "high"),
                    boolean_descriptor("fastMode", "Fast Mode"),
                    boolean_descriptor("thinking", "Thinking"),
                ]),
            }],
        }),
    );
    let previous_provider = to_provider(&previous_json);
    let refreshed_provider = to_provider(&with(
        &previous_json,
        json!({
            "checkedAt": "2026-04-14T00:01:00.000Z",
            "models": [{
                "slug": "claude-opus-4-6",
                "name": "Opus 4.6",
                "isCustom": false,
                "capabilities": capabilities(vec![]),
            }],
        }),
    ));

    assert_eq!(
        merge_provider_snapshot(Some(&previous_provider), &refreshed_provider).models,
        previous_provider.models
    );
}

#[test]
fn persists_merged_provider_snapshots_for_the_providers_that_were_refreshed() {
    let cursor_json = with(
        &base("cursor", "cursor"),
        json!({
            "version": "2026.04.09-f2b0fcd",
            "models": [{
                "slug": "claude-opus-4-6",
                "name": "Opus 4.6",
                "isCustom": false,
                "capabilities": capabilities(vec![
                    select_descriptor("reasoning", "Reasoning", "high"),
                    boolean_descriptor("fastMode", "Fast Mode"),
                    boolean_descriptor("thinking", "Thinking"),
                ]),
            }],
        }),
    );
    let previous_providers = vec![
        to_provider(&cursor_json),
        to_provider(&base("codex", "codex")),
    ];
    let refreshed_cursor = to_provider(&with(
        &cursor_json,
        json!({ "checkedAt": "2026-04-14T00:01:00.000Z", "models": [] }),
    ));

    let merged_providers =
        merge_provider_snapshots(&previous_providers, std::slice::from_ref(&refreshed_cursor));
    let persisted_providers =
        select_providers_by_kind(&merged_providers, &HashSet::from([driver_kind("cursor")]));

    let mut expected = refreshed_cursor.clone();
    expected.models = previous_providers[0].models.clone();
    assert_eq!(persisted_providers, vec![expected]);
}

// ── Registry cases ──────────────────────────────────────────────────

#[tokio::test]
async fn does_not_run_provider_probes_during_construction() {
    let initial_provider = to_provider(&with(
        &base("codex", "codex"),
        json!({
            "status": "warning",
            "installed": false,
            "auth": { "status": "unknown" },
            "checkedAt": "2026-06-10T00:00:00.000Z",
            "version": null,
            "message": "Checking Codex provider status.",
        }),
    ));
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let snapshot = {
        let initial = initial_provider.clone();
        let refresh_calls = Arc::clone(&refresh_calls);
        TestSnapshot::new(
            initial_provider.driver.clone(),
            move || initial.clone(),
            move || {
                refresh_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(futures::future::pending())
            },
        )
    };
    let instance = make_instance(&initial_provider, snapshot, None, None);
    let dir = tempfile::tempdir().unwrap();

    let registry = start_registry(TestSource::new(vec![instance]), dir.path()).await;

    assert_eq!(registry.get_providers(), vec![initial_provider]);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deduplicates_cwd_probes_and_clears_snapshots_when_an_instance_rebuilds() {
    let machine_json = with(
        &base("codex", "codex"),
        json!({
            "checkedAt": "2026-06-10T00:00:00.000Z",
            "slashCommands": [{ "name": "global" }],
            "skills": [{ "name": "global", "path": "/global/SKILL.md", "enabled": true }],
        }),
    );
    let machine_provider = to_provider(&machine_json);
    let scoped_json = with(
        &machine_json,
        json!({
            "checkedAt": "2026-06-10T00:01:00.000Z",
            "slashCommands": [{ "name": "project" }],
            "skills": [{ "name": "project", "path": "/workspace/SKILL.md", "enabled": true }],
        }),
    );
    let scoped_provider = to_provider(&scoped_json);
    let pending_scoped_provider = to_provider(&with(
        &scoped_json,
        json!({ "status": "error", "installed": false, "slashCommands": [] }),
    ));
    let snapshot_calls = Arc::new(AtomicUsize::new(0));
    let scoped_result = Arc::new(Mutex::new(scoped(
        &with(
            &scoped_json,
            json!({ "status": "error", "slashCommands": [compact_slash_command()] }),
        ),
        Some(true),
    )));
    let cache_invalidations = Arc::new(AtomicUsize::new(0));
    // `scanGate`: the next scan to read its result parks here once.
    let scan_gate: Arc<Mutex<Option<(Deferred, Deferred)>>> = Arc::new(Mutex::new(None));
    let return_pending_snapshot = Arc::new(AtomicBool::new(true));
    let probe_started = Deferred::new();
    let release_probe = Deferred::new();

    let invalidate_caches: InvalidateCaches = {
        let cache_invalidations = Arc::clone(&cache_invalidations);
        Arc::new(move || {
            cache_invalidations.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        })
    };
    let first_snapshot_for_cwd: SnapshotForCwd = {
        let snapshot_calls = Arc::clone(&snapshot_calls);
        let return_pending_snapshot = Arc::clone(&return_pending_snapshot);
        let probe_started = probe_started.clone();
        let release_probe = release_probe.clone();
        let scoped_result = Arc::clone(&scoped_result);
        let scan_gate = Arc::clone(&scan_gate);
        let pending = scoped(
            &serde_json::to_value(&pending_scoped_provider).unwrap(),
            None,
        );
        Arc::new(move |_cwd| {
            snapshot_calls.fetch_add(1, Ordering::SeqCst);
            let return_pending = return_pending_snapshot.load(Ordering::SeqCst);
            let probe_started = probe_started.clone();
            let release_probe = release_probe.clone();
            let scoped_result = Arc::clone(&scoped_result);
            let scan_gate = Arc::clone(&scan_gate);
            let pending = pending.clone();
            Box::pin(async move {
                if return_pending {
                    return Ok(pending);
                }
                probe_started.succeed();
                release_probe.wait().await;
                let result = scoped_result.lock().clone();
                let gate = scan_gate.lock().take();
                if let Some((started, release)) = gate {
                    started.succeed();
                    release.wait().await;
                }
                Ok(result)
            })
        })
    };
    let first_instance = make_instance(
        &machine_provider,
        TestSnapshot::fixed(&machine_provider),
        Some(first_snapshot_for_cwd),
        Some(Arc::clone(&invalidate_caches)),
    );
    let rebuilt_json = with(
        &machine_json,
        json!({
            "checkedAt": "2026-06-10T00:02:00.000Z",
            "status": "warning",
            "installed": false,
            "auth": { "status": "unknown" },
        }),
    );
    let rebuilt_provider = to_provider(&rebuilt_json);
    let rebuilt_snapshot_for_cwd: SnapshotForCwd = {
        let snapshot_calls = Arc::clone(&snapshot_calls);
        let scoped_provider = scoped_provider.clone();
        Arc::new(move |_cwd| {
            snapshot_calls.fetch_add(1, Ordering::SeqCst);
            let scoped_provider = scoped_provider.clone();
            Box::pin(async move {
                Ok(ProviderWorkspaceSnapshot {
                    provider: scoped_provider,
                    slash_commands_pending: None,
                })
            })
        })
    };
    let rebuilt_instance = make_instance(
        &rebuilt_provider,
        TestSnapshot::fixed(&rebuilt_provider),
        Some(rebuilt_snapshot_for_cwd),
        Some(invalidate_caches),
    );
    let source = TestSource::new(vec![first_instance]);
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let registry = start_registry_with(source.clone(), dir.path(), Some(clock.clock())).await;
    let id = instance_id("codex");
    let refresh = |registry: &Arc<ProviderRegistry>, fresh: bool| {
        let registry = Arc::clone(registry);
        let id = id.clone();
        async move {
            registry
                .refresh_workspace_snapshot(RefreshWorkspaceSnapshotInput {
                    instance_id: id,
                    cwd: "/workspace".to_owned(),
                    fresh,
                })
                .await
        }
    };

    refresh(&registry, false).await;
    assert_eq!(registry.get_providers()[0].workspace_snapshots, None);
    return_pending_snapshot.store(false, Ordering::SeqCst);
    let mut workspace_update = registry.stream_changes();
    let first_refresh = tokio::spawn(refresh(&registry, false));
    probe_started.wait().await;
    let duplicate_refresh = tokio::spawn(refresh(&registry, false));
    // The duplicate joins the running scan and returns without probing.
    duplicate_refresh.await.unwrap();
    assert_eq!(snapshot_calls.load(Ordering::SeqCst), 2);
    release_probe.succeed();
    first_refresh.await.unwrap();
    assert!(workspace_update.next().await.is_some());
    let providers = registry.get_providers();
    assert_eq!(providers[0].skills, machine_provider.skills);
    let snapshots = providers[0].workspace_snapshots.as_ref().unwrap();
    assert_eq!(snapshots[0].skills, scoped_provider.skills);
    assert_eq!(
        serde_json::to_value(&snapshots[0].slash_commands).unwrap(),
        json!([compact_slash_command()])
    );
    assert_eq!(snapshots[0].slash_commands_pending, Some(true));
    *scoped_result.lock() = scoped(
        &with(&scoped_json, json!({ "status": "error" })),
        Some(false),
    );
    refresh(&registry, false).await;
    assert_eq!(snapshot_calls.load(Ordering::SeqCst), 3);
    let providers = registry.get_providers();
    let snapshots = providers[0].workspace_snapshots.as_ref().unwrap();
    assert_eq!(snapshots[0].slash_commands, scoped_provider.slash_commands);
    assert_eq!(snapshots[0].slash_commands_pending, None);
    refresh(&registry, false).await;
    assert_eq!(snapshot_calls.load(Ordering::SeqCst), 3);
    // Nothing watches skill directories, so an expired scan is redone on use.
    clock.adjust(PROVIDER_WORKSPACE_SNAPSHOT_TTL_MS - 1);
    refresh(&registry, false).await;
    assert_eq!(snapshot_calls.load(Ordering::SeqCst), 3);
    clock.adjust(1);
    refresh(&registry, false).await;
    assert_eq!(snapshot_calls.load(Ordering::SeqCst), 4);
    let mut new_skills = scoped_json["skills"].as_array().unwrap().clone();
    new_skills
        .push(json!({ "name": "added", "path": "/workspace/added/SKILL.md", "enabled": true }));
    *scoped_result.lock() = scoped(&with(&scoped_json, json!({ "skills": new_skills })), None);
    refresh(&registry, true).await;
    assert_eq!(snapshot_calls.load(Ordering::SeqCst), 5);
    assert_eq!(cache_invalidations.load(Ordering::SeqCst), 1);
    let skills_of = |registry: &ProviderRegistry| -> Vec<Value> {
        registry.get_providers()[0]
            .workspace_snapshots
            .as_ref()
            .unwrap()
            .iter()
            .map(|snapshot| serde_json::to_value(&snapshot.skills).unwrap())
            .collect()
    };
    assert_eq!(skills_of(&registry), vec![json!(new_skills)]);

    // A slow fresh scan that read older files must not overwrite a newer scan
    // that finished first.
    let slow_started = Deferred::new();
    let release_slow = Deferred::new();
    *scan_gate.lock() = Some((slow_started.clone(), release_slow.clone()));
    *scoped_result.lock() = scoped(&scoped_json, None);
    let slow_scan = tokio::spawn(refresh(&registry, true));
    slow_started.wait().await;
    let mut latest_skills = new_skills.clone();
    latest_skills
        .push(json!({ "name": "latest", "path": "/workspace/latest/SKILL.md", "enabled": true }));
    *scoped_result.lock() = scoped(
        &with(&scoped_json, json!({ "skills": latest_skills })),
        None,
    );
    refresh(&registry, true).await;
    release_slow.succeed();
    slow_scan.await.unwrap();
    assert_eq!(skills_of(&registry), vec![json!(latest_skills)]);

    source.set_instances(vec![rebuilt_instance]);
    source.publish_change();
    wait_until(|| registry.get_providers()[0].checked_at == rebuilt_provider.checked_at).await;
    assert_eq!(
        registry.get_providers()[0].checked_at,
        rebuilt_provider.checked_at
    );
    assert_eq!(registry.get_providers()[0].workspace_snapshots, None);
}

#[tokio::test]
async fn refreshes_open_code_catalogs_and_preserves_other_providers() {
    let codex_provider = to_provider(&with(
        &base("codex", "codex"),
        json!({ "checkedAt": "2026-06-10T00:00:00.000Z" }),
    ));
    let failed_open_code_json = with(
        &base("opencode", "opencode"),
        json!({
            "status": "error",
            "auth": { "status": "unknown" },
            "checkedAt": "2026-06-10T00:00:00.000Z",
            "message": "Failed to refresh OpenCode models.",
        }),
    );
    let failed_open_code_provider = to_provider(&failed_open_code_json);
    let recovered_open_code_provider = to_provider(&with(
        &failed_open_code_json,
        json!({
            "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": "2026-06-10T00:01:00.000Z",
            "message": "One upstream provider connected through OpenCode.",
            "models": [sub_provider_model("github/gpt-5", "GPT-5", "GitHub")],
        }),
    ));
    let changed_catalog_provider = to_provider(&with(
        &serde_json::to_value(&recovered_open_code_provider).unwrap(),
        json!({
            "checkedAt": "2026-06-10T00:02:00.000Z",
            "models": [sub_provider_model("anthropic/claude-sonnet-4", "Claude Sonnet 4", "Anthropic")],
        }),
    ));
    let codex_refresh_calls = Arc::new(AtomicUsize::new(0));
    let open_code_refresh_calls = Arc::new(AtomicUsize::new(0));
    let catalog_snapshot = Arc::new(Mutex::new(recovered_open_code_provider.clone()));
    let codex_instance = {
        let current = codex_provider.clone();
        let refreshed = codex_provider.clone();
        let calls = Arc::clone(&codex_refresh_calls);
        make_instance(
            &codex_provider,
            TestSnapshot::new(
                codex_provider.driver.clone(),
                move || current.clone(),
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let refreshed = refreshed.clone();
                    Box::pin(async move { refreshed })
                },
            ),
            None,
            None,
        )
    };
    let open_code_instance = {
        let current = failed_open_code_provider.clone();
        let calls = Arc::clone(&open_code_refresh_calls);
        let catalog_snapshot = Arc::clone(&catalog_snapshot);
        make_instance(
            &failed_open_code_provider,
            TestSnapshot::new(
                failed_open_code_provider.driver.clone(),
                move || current.clone(),
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let next = catalog_snapshot.lock().clone();
                    Box::pin(async move { next })
                },
            ),
            None,
            None,
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(
        TestSource::new(vec![codex_instance, open_code_instance]),
        dir.path(),
    )
    .await;
    let open_code_id = instance_id("opencode");
    let codex_id = instance_id("codex");
    let find = |providers: &[ServerProvider], id: &ProviderInstanceId| {
        providers
            .iter()
            .find(|provider| &provider.instance_id == id)
            .cloned()
    };

    let initial_providers = registry.get_providers();
    assert_eq!(
        find(&initial_providers, &open_code_id).unwrap().status,
        turin_provider_core::server_provider::ServerProviderState::Error
    );

    let recovered_providers = registry.refresh(None).await;
    assert_eq!(
        find(&recovered_providers, &open_code_id).unwrap().models,
        recovered_open_code_provider.models
    );
    assert_eq!(
        find(&recovered_providers, &codex_id),
        Some(with_bundled_compatibility(codex_provider.clone()))
    );

    *catalog_snapshot.lock() = changed_catalog_provider.clone();
    let changed_providers = registry.refresh(None).await;
    assert_eq!(
        find(&changed_providers, &open_code_id).unwrap().models,
        changed_catalog_provider.models
    );
    assert_eq!(
        find(&changed_providers, &codex_id),
        Some(with_bundled_compatibility(codex_provider))
    );

    drop(registry);
    assert_eq!(codex_refresh_calls.load(Ordering::SeqCst), 2);
    assert_eq!(open_code_refresh_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn shares_one_in_flight_pass_between_concurrent_untargeted_refreshes() {
    let provider = to_provider(&with(
        &base("codex", "codex"),
        json!({ "checkedAt": "2026-06-10T00:00:00.000Z" }),
    ));
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let probe_started = Deferred::new();
    let release_probe = Deferred::new();
    let instance = {
        let current = provider.clone();
        let refreshed = provider.clone();
        let refresh_calls = Arc::clone(&refresh_calls);
        let probe_started = probe_started.clone();
        let release_probe = release_probe.clone();
        make_instance(
            &provider,
            TestSnapshot::new(
                provider.driver.clone(),
                move || current.clone(),
                move || {
                    refresh_calls.fetch_add(1, Ordering::SeqCst);
                    probe_started.succeed();
                    let release_probe = release_probe.clone();
                    let refreshed = refreshed.clone();
                    Box::pin(async move {
                        release_probe.wait().await;
                        refreshed
                    })
                },
            ),
            None,
            None,
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(TestSource::new(vec![instance]), dir.path()).await;

    let first = {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { registry.refresh(None).await })
    };
    probe_started.wait().await;
    let second = {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { registry.refresh(None).await })
    };
    // Let the second caller reach the shared pass before the probe is released.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    release_probe.succeed();
    let expected = vec![with_bundled_compatibility(provider.clone())];
    assert_eq!(first.await.unwrap(), expected);
    assert_eq!(second.await.unwrap(), expected);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);

    registry.refresh(None).await;
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persists_the_merged_snapshot_when_a_live_update_has_empty_models() {
    let initial_json = with(
        &base("cursor", "cursor"),
        json!({
            "version": "2026.04.09-f2b0fcd",
            "models": [{
                "slug": "claude-opus-4-6",
                "name": "Opus 4.6",
                "isCustom": false,
                "capabilities": capabilities(vec![select_descriptor("reasoning", "Reasoning", "high")]),
            }],
        }),
    );
    let initial_provider = to_provider(&initial_json);
    let refreshed_provider = to_provider(&with(
        &initial_json,
        json!({ "checkedAt": "2026-04-14T00:01:00.000Z", "models": [] }),
    ));
    let snapshot = {
        let current = initial_provider.clone();
        let refreshed = refreshed_provider.clone();
        TestSnapshot::new(
            initial_provider.driver.clone(),
            move || current.clone(),
            move || {
                let refreshed = refreshed.clone();
                Box::pin(async move { refreshed })
            },
        )
    };
    let instance = make_instance(&initial_provider, Arc::clone(&snapshot), None, None);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(TestSource::new(vec![instance]), dir.path()).await;
    let file_path = cache_path(dir.path(), &initial_provider.instance_id);

    assert_eq!(registry.get_providers()[0].models, initial_provider.models);
    let persisted = await_persisted_provider(&registry, &refreshed_provider.checked_at);
    snapshot.publish(&refreshed_provider);
    persisted.await.unwrap();
    let cached_provider = read_provider_status_cache(&file_path).await;

    let mut expected = refreshed_provider.clone();
    expected.models = initial_provider.models.clone();
    assert_eq!(cached_provider, Some(with_bundled_compatibility(expected)));
}

#[tokio::test]
async fn persists_authoritative_open_code_removals_without_resurrecting_them_on_a_failed_live_refresh()
 {
    let initial_json = open_code_previous(vec![
        sub_provider_model("github/gpt-5", "GPT-5", "GitHub"),
        sub_provider_model(
            "removed-plugin/model",
            "Removed Plugin Model",
            "Removed Plugin",
        ),
    ]);
    let initial_provider = to_provider(&initial_json);
    let authoritative_json = with(
        &initial_json,
        json!({
            "checkedAt": "2026-07-17T00:01:00.000Z",
            "models": [initial_json["models"][0].clone()],
        }),
    );
    let authoritative_provider = to_provider(&authoritative_json);
    let failed_provider = to_provider(&with(
        &authoritative_json,
        json!({
            "status": "error",
            "auth": { "status": "unknown" },
            "checkedAt": "2026-07-17T00:02:00.000Z",
            "models": [],
            "message": "Failed to refresh OpenCode models.",
        }),
    ));
    let snapshot = {
        let current = initial_provider.clone();
        let refreshed = authoritative_provider.clone();
        TestSnapshot::new(
            initial_provider.driver.clone(),
            move || current.clone(),
            move || {
                let refreshed = refreshed.clone();
                Box::pin(async move { refreshed })
            },
        )
    };
    let instance = make_instance(&initial_provider, Arc::clone(&snapshot), None, None);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(TestSource::new(vec![instance]), dir.path()).await;
    let file_path = cache_path(dir.path(), &initial_provider.instance_id);

    let authoritative_persisted =
        await_persisted_provider(&registry, &authoritative_provider.checked_at);
    snapshot.publish(&authoritative_provider);
    authoritative_persisted.await.unwrap();
    let cached_provider = read_provider_status_cache(&file_path).await.unwrap();

    assert_eq!(
        cached_provider.models,
        vec![authoritative_provider.models[0].clone()]
    );

    let failed_persisted = await_persisted_provider(&registry, &failed_provider.checked_at);
    snapshot.publish(&failed_provider);
    failed_persisted.await.unwrap();
    let cached_provider = read_provider_status_cache(&file_path).await.unwrap();

    assert_eq!(
        cached_provider.models,
        vec![authoritative_provider.models[0].clone()]
    );
    assert_eq!(
        registry.get_providers()[0].models,
        vec![authoritative_provider.models[0].clone()]
    );
}

#[tokio::test]
async fn returns_the_cached_provider_list_when_a_manual_refresh_fails() {
    let cached_provider = to_provider(&with(
        &base("codex", "codex"),
        json!({ "checkedAt": "2026-04-29T10:00:00.000Z" }),
    ));
    let snapshot = {
        let current = cached_provider.clone();
        TestSnapshot::new(
            cached_provider.driver.clone(),
            move || current.clone(),
            || panic!("simulated refresh failure"),
        )
    };
    let instance = make_instance(&cached_provider, snapshot, None, None);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(TestSource::new(vec![instance]), dir.path()).await;
    let expected = vec![with_bundled_compatibility(cached_provider.clone())];

    assert_eq!(registry.get_providers(), expected);
    assert_eq!(
        registry.refresh(Some(&driver_kind("codex"))).await,
        expected
    );
    assert_eq!(
        registry.refresh_instance(&instance_id("codex")).await,
        expected
    );
}

#[tokio::test]
async fn keeps_consuming_registry_changes_after_one_sync_fails() {
    let codex_provider = to_provider(&with(
        &base("codex", "codex"),
        json!({ "checkedAt": "2026-04-29T10:00:00.000Z" }),
    ));
    let claude_provider = to_provider(&with(
        &base("claudeAgent", "claudeAgent"),
        json!({ "checkedAt": "2026-04-29T10:01:00.000Z" }),
    ));
    let codex_instance = fixed_instance(&codex_provider);
    let claude_instance = fixed_instance(&claude_provider);
    let source = TestSource::new(vec![Arc::clone(&codex_instance)]);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(source.clone(), dir.path()).await;
    assert_eq!(
        registry.get_providers(),
        vec![with_bundled_compatibility(codex_provider.clone())]
    );

    source.fail_next_list.store(true, Ordering::SeqCst);
    source.publish_change();

    source.set_instances(vec![codex_instance, claude_instance]);
    source.publish_change();

    wait_until(|| {
        registry
            .get_providers()
            .iter()
            .any(|provider| provider.instance_id == claude_provider.instance_id)
    })
    .await;

    let mut ids: Vec<String> = registry
        .get_providers()
        .iter()
        .map(|provider| provider.instance_id.to_string())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["claudeAgent".to_owned(), "codex".to_owned()]);
}

#[tokio::test]
async fn includes_unavailable_instance_snapshots_in_get_providers() {
    // Upstream builds this through the settings hydration layer. Here the real
    // instance registry gets a config for a driver this build does not ship,
    // which is the same shadow-snapshot path.
    let config = ProviderInstanceConfigMap::from_iter([(
        instance_id("ghost_main"),
        ProviderInstanceConfig {
            driver: driver_kind("ghostDriver"),
            display_name: Some(
                turin_contracts::TrimmedNonEmptyString::new("A fork-only driver we don't ship")
                    .unwrap(),
            ),
            accent_color: None,
            environment: None,
            enabled: Some(false),
            config: Some(json!({ "arbitrary": "payload" })),
        },
    )]);
    let instance_registry = ProviderInstanceRegistry::new(Vec::new(), config).await;
    let dir = tempfile::tempdir().unwrap();

    let registry = start_registry(instance_registry, dir.path()).await;

    let providers = registry.get_providers();
    let ghost = providers
        .iter()
        .find(|provider| provider.instance_id.as_str() == "ghost_main")
        .expect("the aggregator knows ghost_main");
    assert_eq!(ghost.driver.as_str(), "ghostDriver");
    assert_eq!(
        ghost.availability,
        Some(turin_provider_core::server_provider::ServerProviderAvailability::Unavailable)
    );
    assert!(
        ghost
            .unavailable_reason
            .as_ref()
            .is_some_and(|reason| reason.as_str().contains("ghostDriver")),
        "unavailable reason names the driver: {:?}",
        ghost.unavailable_reason
    );
}

// ── Added: maintenance projection (no upstream case in this file) ──

#[tokio::test]
async fn projects_update_state_onto_the_provider_and_clears_it_when_idle() {
    let provider = to_provider(&base("codex", "codex"));
    let source = TestSource::new(vec![fixed_instance(&provider)]);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(source, dir.path()).await;
    let running: ServerProviderUpdateState = serde_json::from_value(json!({
        "status": "running",
        "startedAt": "2026-04-14T00:00:00.000Z",
    }))
    .unwrap();
    let idle: ServerProviderUpdateState =
        serde_json::from_value(json!({ "status": "idle" })).unwrap();
    let id = instance_id("codex");

    let mut changes = registry.stream_changes();
    let providers = registry
        .set_provider_maintenance_action_state(
            &id,
            ProviderMaintenanceActionKind::Update,
            Some(running.clone()),
        )
        .await;
    assert_eq!(providers[0].update_state, Some(running.clone()));
    assert_eq!(changes.next().await.unwrap()[0].update_state, Some(running));
    // The volatile state never reaches the disk cache.
    assert_eq!(
        read_provider_status_cache(&cache_path(dir.path(), &id))
            .await
            .and_then(|p| p.update_state.clone()),
        None
    );

    let providers = registry
        .set_provider_maintenance_action_state(
            &id,
            ProviderMaintenanceActionKind::Update,
            Some(idle),
        )
        .await;
    assert_eq!(providers[0].update_state, None);
}

#[tokio::test]
async fn maintenance_capabilities_fall_back_to_manual_when_the_instance_is_not_live() {
    let provider = to_provider(&base("codex", "codex"));
    let source = TestSource::new(vec![fixed_instance(&provider)]);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(source, dir.path()).await;
    let manual = make_manual_only_provider_maintenance_capabilities(driver_kind("codex"));

    // Live and matching driver: the instance answers.
    assert_eq!(
        registry
            .get_provider_maintenance_capabilities_for_instance(
                &instance_id("codex"),
                &driver_kind("codex"),
                false,
            )
            .await,
        manual
    );
    // Unknown instance, or a driver that does not match: manual-only for the asked driver.
    let other = make_manual_only_provider_maintenance_capabilities(driver_kind("opencode"));
    assert_eq!(
        registry
            .get_provider_maintenance_capabilities_for_instance(
                &instance_id("codex"),
                &driver_kind("opencode"),
                true,
            )
            .await,
        other
    );
    assert_eq!(
        registry
            .get_provider_maintenance_capabilities_for_instance(
                &instance_id("missing"),
                &driver_kind("opencode"),
                false,
            )
            .await,
        other
    );
}

// ── Port of providerCompatibility.test.ts, registry-stream case ─────

/// A local manifest server that parks the first request until `release`, so the
/// test controls when the remote policy refresh lands (upstream replaces the
/// `ModelManifest` service with a gated `refresh`).
async fn serve_gated_manifest(body: String, started: Deferred, release: Deferred) -> String {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/model-manifest.json",
        listener.local_addr().unwrap()
    );
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
        started.succeed();
        release.wait().await;
        let reply = format!(
            "HTTP/1.1 200 Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(reply.as_bytes()).await.unwrap();
        socket.shutdown().await.ok();
    });
    url
}

#[tokio::test]
async fn a_remote_policy_refresh_preserves_a_newer_health_result_on_the_registry_stream() {
    let policy = json!({
        "driver": "codex",
        "t3CodeRange": ">=0.0.42 <0.1.0",
        "recommendedVersion": "2.0.0",
        "recommendedRange": ">=2.0.0 <3.0.0",
        "ranges": [
            { "range": "<1.0.0", "status": "broken" },
            { "range": ">=1.0.0 <1.5.0", "status": "unsupported" },
            { "range": ">=1.5.0 <2.0.0", "status": "graceful" },
            { "range": ">=2.0.0 <3.0.0", "status": "supported" },
        ],
    });
    let provider_json = json!({
        "driver": "codex",
        "instanceId": "codex-work",
        "enabled": true,
        "installed": true,
        "version": "0.9.0",
        "status": "error",
        "message": "Authentication failed",
        "checkedAt": "2026-09-22T00:00:00Z",
        "auth": { "status": "unauthenticated" },
        "models": [],
        "skills": [],
        "slashCommands": [],
    });
    let provider = to_provider(&provider_json);
    let health = Arc::new(Mutex::new(to_provider(&with(
        &provider_json,
        json!({ "status": "ready", "message": "Healthy" }),
    ))));
    let started = Deferred::new();
    let release = Deferred::new();
    // The manifest the registry holds at start is a cached copy carrying the
    // old policy; the gated remote then serves the relaxed one. The cache is
    // old enough for the first sync's refresh to go to the network.
    let dir = tempfile::tempdir().unwrap();
    let manifest_dir = dir.path().join("manifest");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("model-manifest.json"),
        json!({
            "fetchedAtMs": 0,
            "manifest": {
                "version": 1,
                "updatedAt": "2099-01-01T00:00:00.000Z",
                "currentModels": {},
                "compatibility": [policy],
            },
        })
        .to_string(),
    )
    .unwrap();
    let relaxed_policy = with(
        &policy,
        json!({ "ranges": [{ "range": ">=0.0.0", "status": "supported" }] }),
    );
    let url = serve_gated_manifest(
        json!({
            "version": 1,
            "updatedAt": "2099-06-01T00:00:00.000Z",
            "currentModels": {},
            "compatibility": [relaxed_policy],
        })
        .to_string(),
        started.clone(),
        release.clone(),
    )
    .await;
    let manifest = Arc::new(ModelManifest::with_url(
        &manifest_dir,
        Arc::new(|| Box::pin(async { true })),
        url,
    ));
    let instance = {
        let current = Arc::clone(&health);
        let refreshed = Arc::clone(&health);
        make_instance(
            &provider,
            TestSnapshot::new(
                provider.driver.clone(),
                move || current.lock().clone(),
                move || {
                    let next = refreshed.lock().clone();
                    Box::pin(async move { next })
                },
            ),
            None,
            None,
        )
    };

    let registry = Arc::new(
        ProviderRegistry::start(
            TestSource::new(vec![instance]),
            manifest,
            ProviderRegistryOptions {
                provider_status_cache_dir: dir.path().join("provider-status-cache"),
                // The first release that runs the V2 orchestrator.
                t3_code_version: "0.0.46".to_owned(),
                clock: None,
            },
        )
        .await,
    );
    started.wait().await;
    assert_eq!(
        registry.get_providers()[0]
            .compatibility_advisory
            .as_ref()
            .map(|advisory| advisory.status),
        Some(ServerProviderCompatibilityStatus::Broken)
    );
    *health.lock() = provider.clone();
    let latest_health = registry.refresh_instance(&provider.instance_id).await;
    assert_eq!(
        latest_health[0]
            .message
            .as_ref()
            .map(|message| message.as_str()),
        Some("Authentication failed")
    );
    let mut changes = registry.stream_changes();
    release.succeed();
    let updated = loop {
        let snapshots = changes.next().await.expect("registry stream ended");
        if snapshots[0]
            .compatibility_advisory
            .as_ref()
            .is_some_and(|advisory| advisory.status == ServerProviderCompatibilityStatus::Supported)
        {
            break snapshots.into_iter().next().unwrap();
        }
    };
    assert_eq!(updated.status, ServerProviderState::Error);
    assert_eq!(
        updated.message.as_ref().map(|message| message.as_str()),
        Some("Authentication failed")
    );
}

// ── Added: retired instances (QC wf_461dfbcb-a40, finding 1) ───────
//
// Upstream ends an instance's subscription when its scope closes. Here the
// subscription task is owned by the registry's `LiveSub` and holds only the
// stream receiver, so a rebuilt or removed instance is released and whatever it
// still emits is dropped.

fn codex_provider(checked_at: &str, status: &str) -> ServerProvider {
    to_provider(&with(
        &base("codex", "codex"),
        json!({ "checkedAt": checked_at, "status": status }),
    ))
}

#[tokio::test]
async fn a_rebuilt_instance_releases_its_old_managed_provider() {
    let old_provider = codex_provider("2026-06-10T00:00:00.000Z", "ready");
    let rebuilt_provider = codex_provider("2026-06-10T00:02:00.000Z", "warning");
    let old_snapshot = TestSnapshot::fixed(&old_provider);
    let weak_old_snapshot = Arc::downgrade(&old_snapshot);
    let source = TestSource::new(vec![make_instance(&old_provider, old_snapshot, None, None)]);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(source.clone(), dir.path()).await;
    assert!(weak_old_snapshot.upgrade().is_some());

    source.set_instances(vec![fixed_instance(&rebuilt_provider)]);
    source.publish_change();
    wait_until(|| registry.get_providers()[0].checked_at == rebuilt_provider.checked_at).await;
    wait_until(|| weak_old_snapshot.upgrade().is_none()).await;
}

#[tokio::test]
async fn a_rebuilt_instances_late_emission_does_not_overwrite_the_live_snapshot() {
    let old_provider = codex_provider("2026-06-10T00:00:00.000Z", "ready");
    let rebuilt_provider = codex_provider("2026-06-10T00:02:00.000Z", "warning");
    let old_snapshot = TestSnapshot::fixed(&old_provider);
    let source = TestSource::new(vec![make_instance(
        &old_provider,
        Arc::clone(&old_snapshot),
        None,
        None,
    )]);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(source.clone(), dir.path()).await;

    source.set_instances(vec![fixed_instance(&rebuilt_provider)]);
    source.publish_change();
    wait_until(|| registry.get_providers()[0].checked_at == rebuilt_provider.checked_at).await;

    // The retired provider still holds its subscriber list and emits again.
    let mut changes = registry.stream_changes();
    old_snapshot.publish(&codex_provider("2099-01-01T00:00:00.000Z", "error"));
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }

    let providers = registry.get_providers();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].checked_at, rebuilt_provider.checked_at);
    // Updates from the rebuild itself may still be queued; none may carry the stale snapshot.
    while let std::task::Poll::Ready(Some(update)) = futures::poll!(changes.next()) {
        assert!(
            update
                .iter()
                .all(|provider| provider.checked_at != "2099-01-01T00:00:00.000Z"),
            "the stale emission must not publish"
        );
    }
}

#[tokio::test]
async fn a_removed_instance_is_released_and_its_late_emission_does_not_bring_it_back() {
    let old_provider = codex_provider("2026-06-10T00:00:00.000Z", "ready");
    let old_snapshot = TestSnapshot::fixed(&old_provider);
    let weak_old_snapshot = Arc::downgrade(&old_snapshot);
    let source = TestSource::new(vec![make_instance(
        &old_provider,
        Arc::clone(&old_snapshot),
        None,
        None,
    )]);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(source.clone(), dir.path()).await;

    source.set_instances(Vec::new());
    source.publish_change();
    wait_until(|| registry.get_providers().is_empty()).await;

    old_snapshot.publish(&codex_provider("2099-01-01T00:00:00.000Z", "error"));
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(registry.get_providers(), Vec::new(), "no ghost entry");

    // Only the test's own handle is left.
    drop(old_snapshot);
    wait_until(|| weak_old_snapshot.upgrade().is_none()).await;
}

#[tokio::test]
async fn an_upsert_from_a_retired_source_is_dropped() {
    let provider = codex_provider("2026-06-10T00:00:00.000Z", "ready");
    let source = TestSource::new(vec![fixed_instance(&provider)]);
    let dir = tempfile::tempdir().unwrap();
    let registry = start_registry(source, dir.path()).await;
    let before = registry.get_providers();
    let mut changes = registry.stream_changes();
    let retired = Arc::new(AtomicBool::new(false));

    let returned = registry
        .inner
        .upsert_providers(
            vec![codex_provider("2099-01-01T00:00:00.000Z", "error")],
            UpsertOptions::from_source(&retired),
        )
        .await;

    assert_eq!(returned, before);
    assert_eq!(registry.get_providers(), before);
    assert!(futures::poll!(changes.next()).is_pending());
}
