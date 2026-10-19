//! The OpenCode `ProviderDriver` the composition root fills the built-in
//! driver slot with (port of `provider-opencode/src/server/driver.ts`).
//!
//! `turin-provider-opencode` ships the session adapter (`OpenCodeAdapter`), the
//! version probe, the server owner and the snapshot builders, but no driver, so
//! the app assembles one:
//!
//! - config: `{enabled, binaryPath, serverUrl, serverPassword, customModels}`
//!   (upstream's `OpenCodeSettings`), decoded into the adapter's settings;
//! - orchestration adapter: the real `OpenCodeAdapter`, which launches `opencode
//!   serve` itself, sharing the instance's OpenCode 2 server;
//! - snapshot: a managed provider (`makeManagedServerProvider`). The first probe runs
//!   at creation and on every refresh: the version probe (`opencode --version`, or the
//!   configured server's version endpoints), then the provider/model/agent inventory
//!   (1.x) or the model catalog (2.x), plus the Go usage windows (see [`status`]);
//! - `snapshot_for_cwd`: the skills and slash commands of one workspace directory;
//! - text generation: the 1.x or 2.x runner, whichever the probe found.

mod status;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::git::TextGenerationError;
use turin_contracts::model::CustomModelSetting;
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_provider_core::driver::{
    ProviderDriver, ProviderDriverCreateInput, ProviderDriverMetadata, ProviderInstance,
    RefreshModels, SnapshotForCwd, default_provider_continuation_identity, driver_error,
};
use turin_provider_core::errors::ProviderDriverError;
use turin_provider_core::host_process::HostProcess;
use turin_provider_core::instance_environment::merge_provider_instance_environment;
use turin_provider_core::maintenance_resolver::make_manual_only_provider_maintenance_capabilities;
use turin_provider_core::managed_provider::{ManagedServerProvider, ManagedServerProviderInput};
use turin_provider_core::provider_host::ProviderHost;
use turin_provider_core::rt::BoxFuture;
use turin_provider_core::server_provider::ProviderWorkspaceSnapshot;
use turin_provider_core::snapshot::ManagedServerProvider as _;
use turin_provider_core::snapshot_settings::{
    ProviderSnapshotSettings, ProviderSnapshotSettingsSource,
    have_provider_snapshot_settings_changed,
};
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ThreadTitleGenerationInput, ThreadTitleGenerationResult,
};
use turin_provider_core::usage::ProviderUsageReader;
use turin_provider_opencode::{
    OpenCode2Config, OpenCode2Server, OpenCodeAdapter, OpenCodeServerOwner, OpenCodeSettings,
    OwnerConfig, ProbeSettings, SystemOpenCodeBinary, open_code_usage_reader,
    probe_open_code_runtime,
};
use turin_text_generation::codex_text_generation::AttachmentPathResolver;
use turin_text_generation::opencode_text_generation::{
    OpenCodeTextGenerationSettings, open_code_text_generation,
};
use turin_text_generation::opencode2_text_generation::open_code2_text_generation;

use status::{OpenCodeRuntime, ProbeFn};

/// Upstream's `Effect.timeout("20 seconds")` on a workspace listing.
const WORKSPACE_LIST_TIMEOUT: Duration = Duration::from_secs(20);

pub struct OpenCodeDriver {
    kind: ProviderDriverKind,
    metadata: ProviderDriverMetadata,
    /// The SQLite and legacy JSON history the usage page scans (`usage.ts`).
    usage: ProviderUsageReader,
    host: Arc<ProviderHost>,
}

impl OpenCodeDriver {
    pub fn new(host: Arc<ProviderHost>) -> Self {
        Self {
            kind: ProviderDriverKind::new("opencode").expect("opencode driver slug"),
            metadata: ProviderDriverMetadata::new("OpenCode"),
            usage: open_code_usage_reader(),
            host,
        }
    }
}

fn text(config: &Value, key: &str) -> Result<String, String> {
    match config.get(key) {
        None => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.trim().to_owned()),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn settings_from(config: &Value) -> Result<OpenCodeSettings, String> {
    if !(config.is_object() || config.is_null()) {
        return Err("OpenCode settings must be an object".to_owned());
    }
    let defaults = OpenCodeSettings::default();
    let binary_path = text(config, "binaryPath")?;
    Ok(OpenCodeSettings {
        enabled: match config.get("enabled") {
            None => defaults.enabled,
            Some(Value::Bool(enabled)) => *enabled,
            Some(_) => return Err("enabled must be a boolean".to_owned()),
        },
        binary_path: if binary_path.is_empty() {
            defaults.binary_path
        } else {
            binary_path
        },
        server_url: text(config, "serverUrl")?,
        server_password: text(config, "serverPassword")?,
    })
}

fn custom_models_from(config: &Value) -> Result<Vec<CustomModelSetting>, String> {
    match config.get("customModels") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(models) => serde_json::from_value(models.clone())
            .map_err(|error| format!("customModels is invalid: {error}")),
    }
}

/// What a settings change must re-probe on (upstream compares the whole decoded settings).
#[derive(Clone, PartialEq, Eq)]
struct InstanceSettingsKey {
    enabled: bool,
    binary_path: String,
    server_url: String,
    server_password: String,
}

#[async_trait]
impl ProviderDriver for OpenCodeDriver {
    fn driver_kind(&self) -> &ProviderDriverKind {
        &self.kind
    }

    fn metadata(&self) -> &ProviderDriverMetadata {
        &self.metadata
    }

    fn usage(&self) -> Option<&ProviderUsageReader> {
        Some(&self.usage)
    }

    fn default_config(&self) -> Value {
        json!({"enabled": false, "binaryPath": "opencode", "serverUrl": "", "serverPassword": ""})
    }

    fn decode_config(&self, raw: &Value) -> Result<Value, String> {
        let settings = settings_from(raw)?;
        let mut decoded = json!({
            "enabled": settings.enabled,
            "binaryPath": settings.binary_path,
            "serverUrl": settings.server_url,
            "serverPassword": settings.server_password,
        });
        if let Some(custom) = raw.get("customModels").filter(|models| !models.is_null()) {
            custom_models_from(raw)?;
            decoded["customModels"] = custom.clone();
        }
        Ok(decoded)
    }

    async fn create(
        &self,
        input: ProviderDriverCreateInput,
    ) -> Result<ProviderInstance, ProviderDriverError> {
        let failed = |detail: String| driver_error(&self.kind, &input.instance_id, detail);
        let settings = settings_from(&input.config).map_err(&failed)?;
        let custom_models = custom_models_from(&input.config).map_err(&failed)?;
        // Upstream: `effectiveConfig = { ...config, enabled }` (envelope flag wins).
        let settings = OpenCodeSettings {
            enabled: input.enabled,
            ..settings
        };
        let process = HostProcess::current();
        let environment: BTreeMap<String, String> = merge_provider_instance_environment(
            Some(&input.environment),
            &process.environment,
            &process.home_directory,
        )
        .into_iter()
        .collect();
        let cwd = self.host.paths.cwd.clone();

        let probe_settings = ProbeSettings {
            binary_path: settings.binary_path.clone(),
            server_url: settings.server_url.clone(),
            server_password: settings.server_password.clone(),
        };
        let http = reqwest::Client::new();
        let probe_environment = environment.clone();
        let probe: ProbeFn = Box::new(move || {
            let http = http.clone();
            let settings = probe_settings.clone();
            let environment = probe_environment.clone();
            Box::pin(async move {
                probe_open_code_runtime(&http, &SystemOpenCodeBinary, &settings, Some(&environment))
                    .await
            })
        });
        let owner = OpenCodeServerOwner::new(OwnerConfig {
            binary_path: settings.binary_path.clone(),
            directory: cwd.clone(),
            server_password: Some(settings.server_password.clone()).filter(|p| !p.is_empty()),
            environment: Some(environment.clone()),
            idle: None,
            shutdown_grace: None,
            verify: None,
            ledger: None,
        });
        let v2 = Arc::new(
            OpenCode2Server::start(OpenCode2Config {
                binary_path: settings.binary_path.clone(),
                server_url: settings.server_url.clone(),
                server_password: settings.server_password.clone(),
                directory: cwd.clone(),
                environment: environment.clone(),
                idle: None,
                shutdown_grace: None,
                ledger: None,
                protocol: None,
            })
            .map_err(|error| failed(error.detail))?,
        );
        let runtime = Arc::new(OpenCodeRuntime::new(
            self.kind.clone(),
            input.instance_id.clone(),
            settings.clone(),
            custom_models,
            environment,
            cwd,
            probe,
            owner.clone(),
            Arc::clone(&v2),
        ));
        {
            // Upstream releases both servers when the registry scope closes.
            let owner = owner.clone();
            let v2 = Arc::clone(&v2);
            input.scope.on_close(async move {
                owner.close().await;
                v2.close().await;
            });
        }

        let snapshot = Arc::new(
            self.managed_snapshot(&runtime, &settings)
                .await
                .map_err(|error| failed(format!("Failed to build OpenCode snapshot: {error}")))?,
        );
        let refreshing = Arc::clone(&snapshot);
        let refresh_models: RefreshModels = Arc::new(move || {
            let snapshot = Arc::clone(&refreshing);
            Box::pin(async move {
                snapshot.refresh().await;
                Ok(())
            }) as BoxFuture<'static, Result<(), ProviderDriverError>>
        });
        let snapshot_for_cwd: SnapshotForCwd = {
            let runtime = Arc::clone(&runtime);
            let snapshot = Arc::clone(&snapshot);
            Arc::new(move |cwd| {
                let runtime = Arc::clone(&runtime);
                let snapshot = Arc::clone(&snapshot);
                Box::pin(async move { workspace_snapshot(&runtime, &*snapshot, cwd).await })
            })
        };

        let adapter = OpenCodeAdapter::new(input.instance_id.clone(), settings.clone())
            .with_v2_server(Arc::clone(&v2));
        let resolver = attachment_resolver(&self.host);
        let text_generation = SelectedTextGeneration {
            runtime: Arc::clone(&runtime),
            v1: open_code_text_generation(
                OpenCodeTextGenerationSettings {
                    binary_path: settings.binary_path.clone(),
                    server_url: settings.server_url.clone(),
                    server_password: Some(settings.server_password.clone())
                        .filter(|password| !password.is_empty()),
                },
                owner,
                resolver.clone(),
            ),
            v2: open_code2_text_generation(Arc::clone(&v2), resolver),
        };
        Ok(ProviderInstance {
            instance_id: input.instance_id.clone(),
            driver_kind: self.kind.clone(),
            continuation_identity: default_provider_continuation_identity(
                &self.kind,
                &input.instance_id,
            ),
            display_name: input
                .display_name
                .as_ref()
                .map(|name| name.as_str().to_owned()),
            accent_color: input
                .accent_color
                .as_ref()
                .map(|color| color.as_str().to_owned()),
            enabled: input.enabled,
            snapshot,
            snapshot_for_cwd: Some(snapshot_for_cwd),
            refresh_models: Some(refresh_models),
            invalidate_caches: None,
            consume_reset_credit: None,
            orchestration_adapter: Arc::new(adapter),
            text_generation: Arc::new(text_generation),
            auth: None,
        })
    }
}

type OpenCodeManaged = ManagedServerProvider<ProviderSnapshotSettings<InstanceSettingsKey>>;

impl OpenCodeDriver {
    /// `makeManagedServerProvider`: the first probe runs in the background right away;
    /// `refresh` (the Settings button, the registry's refresh) runs it again.
    async fn managed_snapshot(
        &self,
        runtime: &Arc<OpenCodeRuntime>,
        settings: &OpenCodeSettings,
    ) -> Result<OpenCodeManaged, turin_contracts::settings::ServerSettingsError> {
        let key = InstanceSettingsKey {
            enabled: settings.enabled,
            binary_path: settings.binary_path.clone(),
            server_url: settings.server_url.clone(),
            server_password: settings.server_password.clone(),
        };
        let maintenance = make_manual_only_provider_maintenance_capabilities(self.kind.clone());
        let source = ProviderSnapshotSettingsSource::new(key.clone(), &self.host);
        let host = Arc::clone(&self.host);
        let get_settings_key = key;
        let initial = Arc::clone(runtime);
        let check = Arc::clone(runtime);
        let stream = source.stream_settings();
        ManagedServerProvider::start(
            ManagedServerProviderInput {
                resolve_maintenance: Arc::new(move |_fresh| {
                    let maintenance = maintenance.clone();
                    Box::pin(async move { maintenance })
                }),
                get_settings: Arc::new({
                    let host = Arc::clone(&host);
                    move || {
                        let host = Arc::clone(&host);
                        let key = get_settings_key.clone();
                        Box::pin(async move {
                            ProviderSnapshotSettingsSource::new(key, &host)
                                .get_settings()
                                .await
                        })
                    }
                }),
                have_settings_changed: Arc::new(|previous, next| {
                    have_provider_snapshot_settings_changed(previous, next)
                }),
                initial_snapshot: Arc::new(move |_settings| {
                    let runtime = Arc::clone(&initial);
                    Box::pin(async move { runtime.pending_provider() })
                }),
                check_provider: Arc::new(move || {
                    let runtime = Arc::clone(&check);
                    Box::pin(async move { Ok(runtime.check_provider().await) })
                }),
                enrich_snapshot: None,
                refresh_interval: None,
                refresh_on_interval: false,
                // The instance is rebuilt when its own settings change.
                check_provider_on_settings_change: Some(Arc::new(|_, _| false)),
            },
            stream,
            host,
        )
        .await
    }
}

/// `snapshotForCwd`: the machine snapshot with this directory's skills and slash commands.
async fn workspace_snapshot(
    runtime: &OpenCodeRuntime,
    snapshot: &OpenCodeManaged,
    cwd: String,
) -> Result<ProviderWorkspaceSnapshot, ProviderDriverError> {
    let machine = snapshot.get_snapshot().await;
    if !runtime.settings.enabled {
        return Ok(ProviderWorkspaceSnapshot {
            provider: machine,
            slash_commands_pending: None,
        });
    }
    let failed = |cause: String| {
        driver_error(
            &runtime.kind,
            &runtime.instance_id,
            format!("Failed to list OpenCode commands and skills for '{cwd}': {cause}"),
        )
    };
    let (skills, slash_commands) =
        match tokio::time::timeout(WORKSPACE_LIST_TIMEOUT, runtime.list_workspace(&cwd)).await {
            Ok(Ok(lists)) => lists,
            Ok(Err(error)) => return Err(failed(error.detail)),
            Err(_elapsed) => return Err(failed("timed out".to_owned())),
        };
    let mut provider = machine;
    provider.draft.skills = skills;
    provider.draft.slash_commands = slash_commands;
    Ok(ProviderWorkspaceSnapshot {
        provider,
        slash_commands_pending: None,
    })
}

/// Image attachments of a text generation resolve through the host, like every driver's.
fn attachment_resolver(host: &Arc<ProviderHost>) -> AttachmentPathResolver {
    let host = Arc::clone(host);
    Arc::new(move |attachment: &ChatAttachment| {
        host.resolve_attachment_path(attachment).map(PathBuf::from)
    })
}

/// `selectOpenCodeRuntimeTextGeneration`: text generation runs on the server the instance's
/// probe detected, each in its own protocol.
struct SelectedTextGeneration<V1, V2> {
    runtime: Arc<OpenCodeRuntime>,
    v1: V1,
    v2: V2,
}

#[async_trait]
impl<V1, V2> ProviderTextGeneration for SelectedTextGeneration<V1, V2>
where
    V1: ProviderTextGeneration,
    V2: ProviderTextGeneration,
{
    async fn generate_commit_message(
        &self,
        input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        if self.runtime.is_v2().await {
            self.v2.generate_commit_message(input).await
        } else {
            self.v1.generate_commit_message(input).await
        }
    }

    async fn generate_pr_content(
        &self,
        input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        if self.runtime.is_v2().await {
            self.v2.generate_pr_content(input).await
        } else {
            self.v1.generate_pr_content(input).await
        }
    }

    async fn generate_branch_name(
        &self,
        input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        if self.runtime.is_v2().await {
            self.v2.generate_branch_name(input).await
        } else {
            self.v1.generate_branch_name(input).await
        }
    }

    async fn generate_thread_title(
        &self,
        input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        if self.runtime.is_v2().await {
            self.v2.generate_thread_title(input).await
        } else {
            self.v1.generate_thread_title(input).await
        }
    }

    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        if self.runtime.is_v2().await {
            self.v2.generate_review_guide(input).await
        } else {
            self.v1.generate_review_guide(input).await
        }
    }
}

#[cfg(test)]
mod tests;
