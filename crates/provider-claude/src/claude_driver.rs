//! Port of upstream/apps/server/src/provider/Drivers/ClaudeDriver.ts: the
//! `ProviderDriver` that builds one `ProviderInstance` per Claude instance.
//!
//! The driver owns the process-wide services (`ClaudeSharedServices`): the
//! remote model manifest, the latest-version cache, and the reset-credit
//! coordinator. Each instance reads the catalog from the manifest service at
//! create time and shares the rest. The attachments directory is the host's
//! attachment store, and the runtime instruction text comes from provider-core.
use crate::{
    claude_home::make_claude_continuation_group_key,
    claude_managed_provider::{ClaudeInstanceProbes, ClaudeManagedProvider, ClaudeSharedServices},
    claude_model_catalog::resolve_claude_model_catalog,
    claude_model_manifest::resolve_claude_provider_catalog,
    claude_provider_adapter::ClaudeProviderAdapter,
    claude_usage::claude_usage_reader,
    claude_usage_limits::{
        ClaudeScopedLimitNames, ClaudeUsageLimitHooks, SharedClaudeScopedLimitNames,
    },
};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use turin_contracts::provider_usage_limits::ProviderUsageLimitsUpdate;
use turin_contracts::{
    TrimmedString,
    git::TextGenerationError,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_usage_limits::ProviderConsumeResetCreditOutcome,
    settings::ClaudeSettings,
};
use turin_provider_core::{
    driver::{
        ConsumeResetCredit, InvalidateCaches, ProviderContinuationIdentity, ProviderDriver,
        ProviderDriverCreateInput, ProviderDriverMetadata, ProviderInstance, SnapshotForCwd,
        default_provider_continuation_identity, driver_error,
    },
    errors::ProviderDriverError,
    host_process::HostProcess,
    instance_environment::merge_provider_instance_environment,
    path_expansion::expand_home_path,
    provider_adapter::ProviderAdapterV2,
    provider_host::ProviderHost,
    rt::BoxFuture,
    runtime_instructions::{RuntimeInfo, build_runtime_instructions},
    server_provider::{ProviderWorkspaceSnapshot, ServerProvider},
    snapshot::ManagedServerProvider,
    text_generation::{
        BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
        CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
        ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
        ThreadTitleGenerationInput, ThreadTitleGenerationResult,
    },
    usage::ProviderUsageReader,
};

const DRIVER_NAME: &str = "claudeAgent";

pub struct ClaudeDriver {
    kind: ProviderDriverKind,
    metadata: ProviderDriverMetadata,
    services: Arc<ClaudeSharedServices>,
    /// The `<home>/projects` transcripts the usage page reads (`claudeUsage.ts`).
    usage: ProviderUsageReader,
}

impl ClaudeDriver {
    /// The driver with the bundled manifest's remote refresh, cached under the
    /// host's state directory.
    pub fn new(host: Arc<ProviderHost>) -> Self {
        Self::with_services(Arc::new(ClaudeSharedServices::new(host)))
    }

    pub fn with_services(services: Arc<ClaudeSharedServices>) -> Self {
        Self {
            kind: ProviderDriverKind::new(DRIVER_NAME).expect("valid Claude driver"),
            metadata: ProviderDriverMetadata::new("Claude"),
            services,
            usage: claude_usage_reader(),
        }
    }

    pub fn services(&self) -> &Arc<ClaudeSharedServices> {
        &self.services
    }
}

fn decode(raw: &Value) -> Result<ClaudeSettings, String> {
    serde_json::from_value(raw.clone()).map_err(|error| error.to_string())
}

/// Runtime instruction text for Claude's system prompt. Model and effort are
/// per turn, so they stay out of the instance-level text.
fn runtime_instructions() -> String {
    build_runtime_instructions(&RuntimeInfo {
        harness: "Turin",
        model: None,
        model_name: None,
        reasoning_effort: None,
    })
}

/// Placeholder until text generation lands (task N3 owns `textGeneration/*`).
/// Every call fails with an explicit "not implemented" error instead of
/// reaching the provider.
pub struct UnavailableClaudeTextGeneration;

fn not_implemented(operation: &str) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.to_owned(),
        detail: "Claude text generation is not implemented in this build.".to_owned(),
        cause: None,
    }
}

#[async_trait]
impl ProviderTextGeneration for UnavailableClaudeTextGeneration {
    async fn generate_commit_message(
        &self,
        _input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        Err(not_implemented("generateCommitMessage"))
    }
    async fn generate_pr_content(
        &self,
        _input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        Err(not_implemented("generatePrContent"))
    }
    async fn generate_branch_name(
        &self,
        _input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        Err(not_implemented("generateBranchName"))
    }
    async fn generate_thread_title(
        &self,
        _input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        Err(not_implemented("generateThreadTitle"))
    }
    async fn generate_review_guide(
        &self,
        _input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        Err(not_implemented("generateReviewGuide"))
    }
}

/// `ManagedServerProvider` for the instance. The inherent methods do the work.
#[async_trait]
impl ManagedServerProvider for ClaudeManagedProvider {
    async fn resolve_maintenance(
        &self,
        fresh: bool,
    ) -> turin_provider_core::maintenance::ProviderMaintenanceCapabilities {
        ClaudeManagedProvider::resolve_maintenance(self, fresh).await
    }

    async fn get_snapshot(&self) -> ServerProvider {
        ClaudeManagedProvider::get_snapshot(self)
    }

    async fn refresh(&self) -> ServerProvider {
        ClaudeManagedProvider::refresh(self).await
    }

    fn stream_changes(&self) -> turin_provider_core::rt::BoxStream<'static, ServerProvider> {
        ClaudeManagedProvider::stream_changes(self)
    }

    async fn apply_usage_limits(&self, update: ProviderUsageLimitsUpdate, checked_at: &str) {
        ClaudeManagedProvider::apply_usage_limits_sync(self, &update, checked_at);
    }
}

#[async_trait]
impl ProviderDriver for ClaudeDriver {
    fn driver_kind(&self) -> &ProviderDriverKind {
        &self.kind
    }

    fn metadata(&self) -> &ProviderDriverMetadata {
        &self.metadata
    }

    fn default_config(&self) -> Value {
        // `DEFAULT_CLAUDE_SETTINGS` is the schema decode of `{}`.
        serde_json::to_value(decode(&json!({})).expect("empty Claude config decodes"))
            .expect("Claude settings serialize")
    }

    fn decode_config(&self, raw: &Value) -> Result<Value, String> {
        decode(raw)
            .and_then(|settings| serde_json::to_value(settings).map_err(|error| error.to_string()))
    }

    fn usage(&self) -> Option<&ProviderUsageReader> {
        Some(&self.usage)
    }

    async fn create(
        &self,
        input: ProviderDriverCreateInput,
    ) -> Result<ProviderInstance, ProviderDriverError> {
        let instance_id: ProviderInstanceId = input.instance_id.clone();
        let fail = |detail: String| driver_error(&self.kind, &instance_id, detail);
        let mut settings = decode(&input.config).map_err(|error| {
            fail(format!(
                "Failed to create Claude Agent SDK adapter. {error}"
            ))
        })?;
        settings.enabled = input.enabled;

        let host = HostProcess::current();
        let environment = merge_provider_instance_environment(
            Some(&input.environment),
            &host.environment,
            &host.home_directory,
        );
        settings.binary_path = TrimmedString::new(expand_home_path(
            settings.binary_path.as_str(),
            &host.home_directory,
        ));

        let manifest = self.services.model_manifest.current().await;
        let catalog = Arc::new(resolve_claude_model_catalog(
            resolve_claude_provider_catalog(&manifest).as_ref(),
        ));
        let continuation_key = make_claude_continuation_group_key(
            settings.home_path.as_str(),
            Some(&environment),
            &host.home_directory,
        );
        let adapter_environment: BTreeMap<String, String> = environment
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // The store the message intake writes to, so turns can read the bytes.
        let attachments = PathBuf::from(&self.services.host.paths.attachments_dir);

        let scoped_names: SharedClaudeScopedLimitNames =
            Arc::new(Mutex::new(ClaudeScopedLimitNames::default()));
        let probes = Arc::new(ClaudeInstanceProbes::new(
            instance_id.clone(),
            settings.clone(),
            environment,
            host.home_directory.clone(),
            scoped_names.clone(),
            self.services.clone(),
        ));
        let managed = Arc::new(ClaudeManagedProvider::new(
            probes.clone(),
            self.kind.clone(),
            &catalog,
        ));

        // Rate-limit frames fold into the published snapshot on the adapter's event path.
        let sink_target = managed.clone();
        let hooks = ClaudeUsageLimitHooks {
            scoped_names,
            sink: Arc::new(move |update, checked_at| {
                sink_target.apply_usage_limits_sync(&update, &checked_at);
            }),
        };
        let adapter = ClaudeProviderAdapter::new(
            instance_id.clone(),
            settings,
            (*catalog).clone(),
            Some(adapter_environment),
            Some(attachments),
            runtime_instructions(),
            host.home_directory.clone(),
        )
        .with_usage_limit_hooks(hooks);

        // Start the first probe without delaying readiness. The task is aborted when the instance closes.
        let warm = managed.clone();
        let warm_handle = tokio::spawn(async move {
            warm.refresh().await;
        });
        input.scope.on_close(async move {
            warm_handle.abort();
        });

        let snapshot_for_cwd: SnapshotForCwd = {
            let managed = managed.clone();
            Arc::new(move |cwd: String| {
                let managed = managed.clone();
                Box::pin(async move {
                    Ok::<ProviderWorkspaceSnapshot, ProviderDriverError>(
                        managed.snapshot_for_cwd(&cwd).await,
                    )
                })
                    as BoxFuture<'static, Result<ProviderWorkspaceSnapshot, ProviderDriverError>>
            })
        };
        let invalidate_caches: InvalidateCaches = {
            let probes = probes.clone();
            Arc::new(move || {
                let probes = probes.clone();
                Box::pin(async move { probes.invalidate_capabilities().await })
                    as BoxFuture<'static, ()>
            })
        };
        let consume_reset_credit: ConsumeResetCredit = {
            let managed = managed.clone();
            Arc::new(move || {
                let managed = managed.clone();
                Box::pin(async move { managed.consume_reset_credit().await })
                    as BoxFuture<
                        'static,
                        Result<ProviderConsumeResetCreditOutcome, ProviderDriverError>,
                    >
            })
        };

        let continuation_identity = ProviderContinuationIdentity {
            continuation_key,
            ..default_provider_continuation_identity(&self.kind, &instance_id)
        };
        Ok(ProviderInstance {
            instance_id,
            driver_kind: self.kind.clone(),
            continuation_identity,
            display_name: input.display_name.map(|name| name.as_str().to_owned()),
            accent_color: input.accent_color.map(|color| color.as_str().to_owned()),
            enabled: input.enabled,
            snapshot: managed,
            snapshot_for_cwd: Some(snapshot_for_cwd),
            refresh_models: None,
            invalidate_caches: Some(invalidate_caches),
            consume_reset_credit: Some(consume_reset_credit),
            orchestration_adapter: Arc::new(adapter) as Arc<dyn ProviderAdapterV2>,
            text_generation: Arc::new(UnavailableClaudeTextGeneration)
                as Arc<dyn ProviderTextGeneration>,
            auth: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_managed_provider::provider_update_checks;
    use std::net::SocketAddr;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use turin_provider_core::{
        driver::ProviderInstanceScope, model_manifest::ModelManifest,
        provider_host_live::DesktopProviderHost,
    };

    /// The bundled manifest with one extra current Claude model and a newer
    /// `updatedAt`, so a fetch of it replaces the bundle.
    fn remote_manifest_json() -> String {
        let mut manifest: Value =
            serde_json::from_str(include_str!("../../provider-core/model-manifest.json"))
                .expect("bundled manifest is JSON");
        manifest["updatedAt"] = json!("2026-10-15T00:00:00Z");
        manifest["providers"]["claudeAgent"]["models"]
            .as_array_mut()
            .expect("claude models")
            .push(
                json!({"slug": "claude-remote-test", "name": "Remote Test", "status": "current"}),
            );
        manifest.to_string()
    }

    /// Serves `body` to every request. Returns the base URL.
    async fn serve_manifest(body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address: SocketAddr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    let _ = socket.read(&mut buffer).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        format!("http://{address}/model-manifest.json")
    }

    fn host(dir: &std::path::Path, settings: Value) -> Arc<ProviderHost> {
        let settings: turin_contracts::settings::ServerSettings =
            serde_json::from_value(settings).expect("server settings decode");
        let should_run: turin_provider_core::provider_host_live::ShouldRunBackgroundWork =
            Arc::new(|_scope| Box::pin(async { true }));
        let desktop =
            DesktopProviderHost::new(dir, dir, settings, should_run).expect("desktop host opens");
        Arc::new(desktop.host)
    }

    fn create_input(scope: Arc<ProviderInstanceScope>, config: Value) -> ProviderDriverCreateInput {
        ProviderDriverCreateInput {
            instance_id: ProviderInstanceId::new("claudeAgent").expect("instance id"),
            display_name: None,
            accent_color: None,
            environment: Default::default(),
            enabled: true,
            config,
            scope,
        }
    }

    #[tokio::test]
    async fn exposes_the_transcript_usage_reader_for_the_usage_page() {
        let dir = tempfile::tempdir().expect("tempdir");
        let driver = ClaudeDriver::new(host(dir.path(), json!({})));
        assert!(matches!(
            driver.usage(),
            Some(ProviderUsageReader::Transcripts {
                provider: turin_contracts::usage::UsageProviderKind::Claude,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn builds_an_instance_from_settings_and_host_env_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let driver = ClaudeDriver::new(host(dir.path(), json!({})));
        assert_eq!(driver.driver_kind().as_str(), "claudeAgent");
        let config = driver
            .decode_config(&json!({"binaryPath": "/nonexistent/claude-for-test", "homePath": ""}))
            .expect("config decodes");
        let scope = ProviderInstanceScope::new();
        let instance = driver
            .create(create_input(scope.clone(), config))
            .await
            .expect("instance creates");
        assert_eq!(instance.driver_kind.as_str(), "claudeAgent");
        assert!(instance.snapshot_for_cwd.is_some());
        assert!(instance.consume_reset_credit.is_some());
        scope.close().await;
    }

    #[tokio::test]
    async fn a_refresh_publishes_the_remote_manifest_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let url = serve_manifest(remote_manifest_json()).await;
        let host = host(dir.path(), json!({"enableProviderUpdateChecks": true}));
        let manifest =
            ModelManifest::with_url(dir.path(), provider_update_checks(host.clone()), url);
        let services = Arc::new(ClaudeSharedServices::with_model_manifest(host, manifest));
        let driver = ClaudeDriver::with_services(services.clone());
        let config = driver
            .decode_config(&json!({"binaryPath": "/nonexistent/claude-for-test"}))
            .expect("config decodes");
        let scope = ProviderInstanceScope::new();
        let instance = driver
            .create(create_input(scope.clone(), config))
            .await
            .expect("instance creates");

        // The TTL-gated refresh is what the status check starts; force it here so the test does not race the fetch.
        let fetched = services.model_manifest.force_refresh().await;
        assert!(
            fetched
                .providers
                .as_ref()
                .and_then(|providers| providers.get("claudeAgent"))
                .is_some_and(|catalog| {
                    catalog
                        .models
                        .iter()
                        .any(|m| m.slug.as_str() == "claude-remote-test")
                })
        );

        let snapshot = instance.snapshot.refresh().await;
        let slugs: Vec<&str> = snapshot
            .draft
            .models
            .iter()
            .map(|m| m.slug.as_str())
            .collect();
        assert!(slugs.contains(&"claude-remote-test"), "{slugs:?}");
        scope.close().await;
    }

    #[tokio::test]
    async fn an_unreachable_manifest_keeps_the_bundled_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let host = host(dir.path(), json!({"enableProviderUpdateChecks": true}));
        // Port 9 has no listener here, so the fetch fails and the bundle stays.
        let manifest = ModelManifest::with_url(
            dir.path(),
            provider_update_checks(host.clone()),
            "http://127.0.0.1:9/model-manifest.json",
        );
        let services = Arc::new(ClaudeSharedServices::with_model_manifest(host, manifest));
        let driver = ClaudeDriver::with_services(services.clone());
        let config = driver
            .decode_config(&json!({"binaryPath": "/nonexistent/claude-for-test"}))
            .expect("config decodes");
        let scope = ProviderInstanceScope::new();
        let instance = driver
            .create(create_input(scope.clone(), config))
            .await
            .expect("instance creates");
        services.model_manifest.force_refresh().await;
        let snapshot = instance.snapshot.refresh().await;
        let slugs: Vec<&str> = snapshot
            .draft
            .models
            .iter()
            .map(|m| m.slug.as_str())
            .collect();
        assert!(slugs.contains(&"claude-fable-5-1"), "{slugs:?}");
        assert!(!slugs.contains(&"claude-remote-test"));
        scope.close().await;
    }
}
