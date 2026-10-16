//! `ProviderDriver` for Codex: port of `CodexDriver` and the status half of
//! `CodexProvider.ts` (`makePendingCodexProvider`, `checkCodexProviderStatus`,
//! `accountProbeStatus`).
//!
//! `create` materializes one instance from its decoded `CodexSettings`. The
//! snapshot starts pending; `refresh` connects a `codex app-server`, reads the
//! account and model list, and reports installed/auth/status. Usage limits,
//! skills, slash commands, and text generation are not ported yet (PORTING.md).

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{Value, json};
use turin_codex_app_server::errors::AppServerError;
use turin_contracts::UtcDateTime;
use turin_contracts::model::CustomModelSetting;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_usage_limits::{
    ProviderUsageLimitsUpdate, ServerProviderUsageLimits, ServerProviderUsageUnavailableReason,
};
use turin_contracts::settings::{CodexSettings, CodexSetupMode};
use turin_provider_core::driver::{
    ProviderContinuationIdentity, ProviderDriver, ProviderDriverCreateInput,
    ProviderDriverMetadata, ProviderInstance, SnapshotForCwd, driver_error,
};
use turin_provider_core::errors::ProviderDriverError;
use turin_provider_core::instance_identity::{InstanceIdentity, with_instance_identity};
use turin_provider_core::maintenance::ProviderMaintenanceCapabilities;
use turin_provider_core::maintenance_resolver::make_manual_only_provider_maintenance_capabilities;
use turin_provider_core::model_catalog::ModelCatalog;
use turin_provider_core::path_expansion::expand_home_path;
use turin_provider_core::rt::BoxStream;
use turin_provider_core::server_provider::{
    ProviderWorkspaceSnapshot, ServerProvider, ServerProviderAuth, ServerProviderAuthStatus,
    ServerProviderModel, ServerProviderSkill, ServerProviderSlashCommand,
    ServerProviderSlashCommandInput, ServerProviderState,
};
use turin_provider_core::snapshot::ManagedServerProvider;
use turin_provider_core::snapshot_probe::{
    BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
    build_server_provider,
};
use turin_provider_core::text_generation::ProviderTextGeneration;
use turin_provider_core::usage_limits::make_unavailable_usage_limits;
use turin_shared::cli_args::tokenize_cli_args;
use turin_text_generation::codex_text_generation::{
    AttachmentPathResolver, CodexModel, CodexModelSource, CodexTextGenerationOptions,
    codex_text_generation,
};

use crate::{
    codex_home_layout::{
        CodexHomeLayout, materialize_codex_shadow_home, resolve_codex_home_layout,
    },
    codex_launch_args::resolve_codex_launch_args,
    codex_managed_provider::make_managed_codex_provider,
    codex_managed_runtime::CodexManagedHost,
    codex_model::{
        append_custom_codex_models, apply_preferred_codex_default_model, parse_codex_model,
    },
    codex_model_catalog::apply_codex_model_catalog,
    codex_provider::{now_ms, probe},
    codex_skills::parse_codex_skills_list_response,
    codex_snapshot::SnapshotCell,
    codex_usage_limits::{
        CodexRateLimitSnapshot, CodexResetCreditsSummary, codex_rate_limits_to_limits,
        codex_usage_probe_failed_limits,
    },
    orchestration_adapter::CodexProviderAdapter,
    runtime::CodexRuntimeConfig,
};

/// `AUTH_PROBE_TIMEOUT_MS` in `packages/provider-core/src/server/snapshotProbe.ts`.
const AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

const NOT_CHECKED_MESSAGE: &str = "Codex provider status has not been checked in this session yet.";
const DISABLED_MESSAGE: &str = "Codex is disabled in T3 Code settings.";

pub struct CodexDriver {
    kind: ProviderDriverKind,
    metadata: ProviderDriverMetadata,
    /// What a `setupMode: "managed"` instance needs from the host. Without it
    /// those instances fail to create.
    managed_host: Option<CodexManagedHost>,
    /// The model manifest's Codex catalog (`ModelCatalog`). Without it models
    /// are not reclassified and no update-required models are named.
    model_catalog: Option<Arc<ModelCatalog>>,
    /// Maps an image attachment to its file for text generation. Without it
    /// image attachments are skipped.
    attachment_path: Option<AttachmentPathResolver>,
}

impl CodexDriver {
    pub fn new() -> Self {
        Self {
            kind: ProviderDriverKind::new("codex").expect("codex driver slug"),
            metadata: ProviderDriverMetadata::new("Codex"),
            managed_host: None,
            model_catalog: None,
            attachment_path: None,
        }
    }

    /// A driver that can also create managed instances: the server config,
    /// secret store, environment id, and installation come from `host`.
    pub fn with_managed_host(host: CodexManagedHost) -> Self {
        Self {
            managed_host: Some(host),
            ..Self::new()
        }
    }

    /// `ModelCatalog` for the status check (`applyCodexModelCatalog`).
    pub fn with_model_catalog(mut self, catalog: Arc<ModelCatalog>) -> Self {
        self.model_catalog = Some(catalog);
        self
    }

    /// `resolveAttachmentPath` for text generation.
    pub fn with_attachment_resolver(mut self, resolver: AttachmentPathResolver) -> Self {
        self.attachment_path = Some(resolver);
        self
    }
}

impl Default for CodexDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderDriver for CodexDriver {
    fn driver_kind(&self) -> &ProviderDriverKind {
        &self.kind
    }

    fn metadata(&self) -> &ProviderDriverMetadata {
        &self.metadata
    }

    fn default_config(&self) -> Value {
        serde_json::from_value::<CodexSettings>(json!({}))
            .and_then(serde_json::to_value)
            .unwrap_or_else(|_| Value::Object(Default::default()))
    }

    fn decode_config(&self, raw: &Value) -> Result<Value, String> {
        let settings = if raw.is_null() {
            serde_json::from_value::<CodexSettings>(json!({}))
        } else {
            serde_json::from_value::<CodexSettings>(raw.clone())
        }
        .map_err(|error| error.to_string())?;
        serde_json::to_value(settings).map_err(|error| error.to_string())
    }

    async fn create(
        &self,
        input: ProviderDriverCreateInput,
    ) -> Result<ProviderInstance, ProviderDriverError> {
        let settings: CodexSettings = serde_json::from_value(input.config.clone())
            .map_err(|error| driver_error(&self.kind, &input.instance_id, error.to_string()))?;
        if settings.setup_mode == Some(CodexSetupMode::Managed) {
            let Some(host) = self.managed_host.clone() else {
                return Err(driver_error(
                    &self.kind,
                    &input.instance_id,
                    "Managed Codex needs the host installation and credential store.",
                ));
            };
            return make_managed_codex_provider(
                &self.kind,
                input,
                settings,
                host,
                self.attachment_path.clone(),
            )
            .await;
        }
        let enabled = settings.enabled && input.enabled;
        let maintenance = make_manual_only_provider_maintenance_capabilities(self.kind.clone());
        let custom_models = settings.custom_models.clone();
        let home = std::env::var("HOME").unwrap_or_default();
        let layout = resolve_codex_home_layout(
            settings.home_path.as_str(),
            settings.shadow_home_path.as_str(),
            &home,
        );
        materialize_codex_shadow_home(&layout)
            .map_err(|error| driver_error(&self.kind, &input.instance_id, error.to_string()))?;
        let config = runtime_config(&settings, &input, &layout);
        let identity = SnapshotIdentity {
            display_name: input
                .display_name
                .as_ref()
                .map(|name| name.as_str().to_owned()),
            accent_color: input
                .accent_color
                .as_ref()
                .map(|color| color.as_str().to_owned()),
            continuation_group_key: layout.continuation_key.clone(),
        };
        let catalog_now = match &self.model_catalog {
            Some(catalog) => catalog.current(&self.kind).await,
            None => None,
        };
        let mut pending = pending_provider(&self.kind, &input.instance_id, enabled, &custom_models);
        pending.draft = apply_codex_model_catalog(pending.draft, catalog_now.as_ref());
        let snapshot = Arc::new(CodexSnapshot {
            kind: self.kind.clone(),
            instance_id: input.instance_id.clone(),
            enabled,
            config: config.clone(),
            custom_models: custom_models.clone(),
            cell: SnapshotCell::new(identity.stamp(&self.kind, &input.instance_id, pending)),
            identity,
            maintenance,
            catalog: self.model_catalog.clone(),
        });
        let usage_target = Arc::clone(&snapshot);
        let mut adapter = CodexProviderAdapter::new(input.instance_id.clone(), config.clone())
            .with_usage_sink(Arc::new(move |update, checked_at| {
                usage_target.cell.apply_usage_limits(&update, &checked_at);
            }));
        if let Some(resolver) = &self.attachment_path {
            adapter = adapter.with_attachment_resolver(Arc::clone(resolver));
        }
        let text_generation = native_text_generation(
            &settings,
            &config,
            &layout,
            enabled,
            Arc::clone(&snapshot),
            self.attachment_path.clone(),
        );
        let for_cwd = Arc::clone(&snapshot);
        let snapshot_for_cwd: SnapshotForCwd = Arc::new(move |cwd: String| {
            let snapshot = Arc::clone(&for_cwd);
            Box::pin(async move { snapshot.workspace_snapshot(&cwd).await })
        });
        Ok(ProviderInstance {
            instance_id: input.instance_id.clone(),
            driver_kind: self.kind.clone(),
            continuation_identity: codex_continuation_identity(&self.kind, &layout),
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
            refresh_models: None,
            invalidate_caches: None,
            consume_reset_credit: None,
            orchestration_adapter: Arc::new(adapter),
            text_generation,
            auth: None,
        })
    }
}

/// `codexContinuationIdentity`: instances that share a Codex home share a continuation.
pub(crate) fn codex_continuation_identity(
    kind: &ProviderDriverKind,
    layout: &CodexHomeLayout,
) -> ProviderContinuationIdentity {
    ProviderContinuationIdentity {
        driver_kind: kind.clone(),
        continuation_key: layout.continuation_key.clone(),
    }
}

/// `binaryPath`, `homePath`, and `launchArgs` from settings, plus the instance
/// environment. `launchArgs` resolves like upstream's `resolveCodexLaunchArgs`
/// (instance env, then process env, then settings) and shell-splits the result.
fn runtime_config(
    settings: &CodexSettings,
    input: &ProviderDriverCreateInput,
    layout: &CodexHomeLayout,
) -> CodexRuntimeConfig {
    // `homePath: homeLayout.effectiveHomePath ?? ""`: a shadow home when there is one.
    let home_path = layout
        .effective_home_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let environment: BTreeMap<String, String> = input
        .environment
        .iter()
        .map(|variable| (variable.name.as_str().to_owned(), variable.value.clone()))
        .collect();
    let resolved = resolve_codex_launch_args(Some(settings.launch_args.as_str()), |name| {
        environment
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
    });
    let launch_args = tokenize_cli_args(Some(&resolved));
    CodexRuntimeConfig {
        binary: PathBuf::from(expand_home_path(
            settings.binary_path.as_str(),
            &std::env::var("HOME").unwrap_or_default(),
        )),
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        home_path,
        launch_args,
        environment,
        remove_environment: Vec::new(),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn presentation() -> ServerProviderPresentation {
    ServerProviderPresentation {
        display_name: "Codex".to_owned(),
        badge_label: None,
        show_interaction_mode_toggle: Some(true),
        reports_context_window: Some(true),
        supported_runtime_modes: None,
        requires_new_thread_for_model_change: None,
        supports_conversation_rollback: None,
    }
}

pub(crate) fn provider_with(
    kind: &ProviderDriverKind,
    instance_id: &ProviderInstanceId,
    enabled: bool,
    models: Vec<ServerProviderModel>,
    skills: Vec<ServerProviderSkill>,
    slash_commands: Vec<ServerProviderSlashCommand>,
    probe: ProviderProbeResult,
) -> ServerProvider {
    let mut draft = build_server_provider(BuildServerProviderInput {
        driver: Some(kind),
        presentation: presentation(),
        enabled,
        checked_at: UtcDateTime::now().to_string(),
        models,
        update_required_models: Vec::new(),
        slash_commands,
        skills,
        probe,
        maintenance: None,
    });
    draft.supports_text_generation = Some(false);
    ServerProvider {
        instance_id: instance_id.clone(),
        driver: kind.clone(),
        draft,
    }
}

/// The slash commands a successful probe reports: `/compact`
/// (`COMPACT_SLASH_COMMAND`), `/goal`, and `/feedback`.
pub(crate) fn codex_slash_commands() -> Vec<ServerProviderSlashCommand> {
    let command = |name: &str, description: &str, hint: Option<&str>| ServerProviderSlashCommand {
        name: tne(name).expect("slash command name"),
        description: tne(description),
        input: hint.map(|hint| ServerProviderSlashCommandInput {
            hint: tne(hint).expect("slash command hint"),
        }),
    };
    vec![
        command(
            "compact",
            "Summarize the conversation and reduce context usage",
            None,
        ),
        command(
            "goal",
            "Set a goal Codex keeps working toward until it is done",
            Some("Objective, or pause, resume, clear"),
        ),
        command(
            "feedback",
            "Send this thread and Codex logs to OpenAI",
            Some("Describe the issue (optional)"),
        ),
    ]
}

/// `makePendingCodexProvider`: the snapshot before the first probe. Custom
/// models show up before the first probe, as upstream's `emptyCodexModelsFromSettings` does.
pub(crate) fn pending_provider(
    kind: &ProviderDriverKind,
    instance_id: &ProviderInstanceId,
    enabled: bool,
    custom_models: &[CustomModelSetting],
) -> ServerProvider {
    let message = if enabled {
        NOT_CHECKED_MESSAGE
    } else {
        DISABLED_MESSAGE
    };
    provider_with(
        kind,
        instance_id,
        enabled,
        append_custom_codex_models(&[], custom_models),
        Vec::new(),
        Vec::new(),
        ProviderProbeResult {
            installed: false,
            version: None,
            status: ServerProviderState::Warning,
            auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
            message: Some(message.to_owned()),
            usage_limits: None,
        },
    )
}

struct CodexSnapshot {
    kind: ProviderDriverKind,
    instance_id: ProviderInstanceId,
    enabled: bool,
    config: CodexRuntimeConfig,
    custom_models: Vec<CustomModelSetting>,
    identity: SnapshotIdentity,
    cell: SnapshotCell,
    maintenance: ProviderMaintenanceCapabilities,
    catalog: Option<Arc<ModelCatalog>>,
}

/// What `withInstanceIdentity` stamps on every snapshot of one instance.
#[derive(Clone)]
pub(crate) struct SnapshotIdentity {
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub continuation_group_key: String,
}

impl SnapshotIdentity {
    pub(crate) fn stamp(
        &self,
        kind: &ProviderDriverKind,
        instance_id: &ProviderInstanceId,
        provider: ServerProvider,
    ) -> ServerProvider {
        with_instance_identity(
            &InstanceIdentity {
                instance_id: instance_id.clone(),
                driver_kind: kind.clone(),
                display_name: self.display_name.clone(),
                accent_color: self.accent_color.clone(),
                continuation_group_key: self.continuation_group_key.clone(),
            },
            provider.draft,
        )
    }
}

impl CodexSnapshot {
    /// `checkProvider` for a native instance: `checkCodexProviderStatus`, then
    /// `applyCodexModelCatalog`. The manifest refreshes in the background, so a
    /// slow fetch never delays the check; the in-memory catalog classifies.
    async fn check(&self) -> ServerProvider {
        if let Some(catalog) = &self.catalog {
            catalog.refresh_in_background();
        }
        let status = check_codex_provider_status(StatusInput {
            kind: &self.kind,
            instance_id: &self.instance_id,
            enabled: self.enabled,
            config: &self.config,
            custom_models: &self.custom_models,
            managed_auth: None,
        });
        let catalog = async {
            match &self.catalog {
                Some(catalog) => catalog.current(&self.kind).await,
                None => None,
            }
        };
        let (mut status, catalog) = tokio::join!(status, catalog);
        status.draft = apply_codex_model_catalog(status.draft, catalog.as_ref());
        self.identity.stamp(&self.kind, &self.instance_id, status)
    }

    /// `snapshotForCwd`: the machine snapshot with this workspace's skills.
    #[allow(clippy::result_large_err)]
    async fn workspace_snapshot(
        &self,
        cwd: &str,
    ) -> Result<ProviderWorkspaceSnapshot, ProviderDriverError> {
        let mut provider = self.cell.get();
        if self.enabled {
            provider.skills = tokio::time::timeout(
                SKILLS_PROBE_TIMEOUT,
                probe_skills_for_cwd(&self.config, cwd),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| {
                driver_error(
                    &self.kind,
                    &self.instance_id,
                    format!("Failed to probe Codex skills for '{cwd}'"),
                )
            })?;
        }
        Ok(ProviderWorkspaceSnapshot {
            provider,
            slash_commands_pending: None,
        })
    }
}

/// `Effect.timeout("20 seconds")` around `probeCodexSkillsForCwd`.
pub(crate) const SKILLS_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// `probeCodexSkillsForCwd`: one `skills/list` for `cwd` on a fresh app-server.
pub(crate) async fn probe_skills_for_cwd(
    config: &CodexRuntimeConfig,
    cwd: &str,
) -> Result<Vec<ServerProviderSkill>, AppServerError> {
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let session = config.connect(&home).await?;
    let response = session
        .client()
        .request("skills/list", Some(json!({"cwds": [cwd]})))
        .await;
    let _ = session.shutdown().await;
    let response = response?.ok_or_else(|| AppServerError::InvalidPayload {
        method: "skills/list".into(),
        detail: "missing response".into(),
    })?;
    Ok(parse_codex_skills_list_response(&response, cwd))
}

/// The live model list text generation resolves aliases against.
struct SnapshotModels(Arc<CodexSnapshot>);

#[async_trait]
impl CodexModelSource for SnapshotModels {
    async fn models(&self) -> Vec<CodexModel> {
        self.0
            .cell
            .get()
            .models
            .iter()
            .map(|model| CodexModel {
                slug: model.slug.as_str().to_owned(),
                is_custom: model.is_custom,
            })
            .collect()
    }
}

/// `makeCodexTextGeneration(effectiveConfig, processEnv, models)`: one
/// `codex exec` per request, from `turin-text-generation`.
fn native_text_generation(
    settings: &CodexSettings,
    config: &CodexRuntimeConfig,
    layout: &CodexHomeLayout,
    enabled: bool,
    snapshot: Arc<CodexSnapshot>,
    attachment_path: Option<AttachmentPathResolver>,
) -> Arc<dyn ProviderTextGeneration> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut effective = serde_json::to_value(settings).unwrap_or_default();
    effective["enabled"] = json!(enabled);
    effective["binaryPath"] = json!(config.binary.to_string_lossy());
    effective["homePath"] = json!(
        layout
            .effective_home_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    let environment: std::collections::BTreeMap<String, String> =
        std::env::vars().chain(config.environment.clone()).collect();
    Arc::new(codex_text_generation(CodexTextGenerationOptions {
        settings: serde_json::from_value(effective).unwrap_or_else(|_| settings.clone()),
        environment: Some(environment),
        models: Arc::new(SnapshotModels(snapshot)),
        managed_runtime: None,
        resolve_attachment_path: attachment_path.unwrap_or_else(|| Arc::new(|_| None)),
        home_dir: home,
    }))
}

/// `checkCodexProviderStatus`'s inputs. `managed_auth` is the account a managed
/// instance signed in with: it replaces the app-server's own account and usage.
pub(crate) struct StatusInput<'a> {
    pub kind: &'a ProviderDriverKind,
    pub instance_id: &'a ProviderInstanceId,
    pub enabled: bool,
    pub config: &'a CodexRuntimeConfig,
    pub custom_models: &'a [CustomModelSetting],
    pub managed_auth: Option<ServerProviderAuth>,
}

/// Port of `checkCodexProviderStatus`: probe a `codex app-server` and report
/// installed/auth/models/skills, or why it could not.
pub(crate) async fn check_codex_provider_status(input: StatusInput<'_>) -> ServerProvider {
    if !input.enabled {
        return pending_provider(input.kind, input.instance_id, false, input.custom_models);
    }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let checked = tokio::time::timeout(AUTH_PROBE_TIMEOUT, probe_provider(&input, &home)).await;
    match checked {
        Ok(provider) => provider,
        Err(_) => failure(
            &input,
            true,
            "Timed out while checking Codex app-server provider status.".to_owned(),
        ),
    }
}

async fn probe_provider(input: &StatusInput<'_>, home: &Path) -> ServerProvider {
    let session = match input.config.connect(home).await {
        Ok(session) => session,
        Err(error) => return spawn_or_probe_failure(input, &error),
    };
    let cwd = input.config.cwd.to_string_lossy().into_owned();
    // A managed account has no native usage to read.
    let skip_native_usage = input.managed_auth.is_some();
    let snapshot = probe(&session, &cwd, skip_native_usage, now_ms()).await;
    let version = session.version.clone();
    let _ = session.shutdown().await;
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => return spawn_or_probe_failure(input, &error),
    };
    let models = apply_preferred_codex_default_model(append_custom_codex_models(
        &snapshot
            .models
            .iter()
            .filter_map(parse_codex_model)
            .collect::<Vec<_>>(),
        input.custom_models,
    ));
    let skills = parse_codex_skills_list_response(&snapshot.skills, &cwd);
    let checked_at = UtcDateTime::now().to_string();
    let (status, auth, message, usage_limits) = match &input.managed_auth {
        Some(managed) => (ServerProviderState::Ready, managed.clone(), None, None),
        None => {
            let usage = usage_limits_for_probe(
                &snapshot.account,
                snapshot.rate_limits.as_ref(),
                &checked_at,
            );
            let account = account_probe(
                &snapshot.account,
                workspace_id(snapshot.rate_limits.as_ref()),
            );
            (account.status, account.auth, account.message, usage)
        }
    };
    provider_with(
        input.kind,
        input.instance_id,
        input.enabled,
        models,
        skills,
        codex_slash_commands(),
        ProviderProbeResult {
            installed: true,
            version: version.or(snapshot.version),
            status,
            auth,
            message,
            usage_limits,
        },
    )
}

fn spawn_or_probe_failure(input: &StatusInput<'_>, error: &AppServerError) -> ServerProvider {
    if let AppServerError::Spawn(_) = error {
        let binary = input.config.binary.to_string_lossy().into_owned();
        let hint = if binary == "codex" {
            " Installing ChatGPT or Codex desktop may not add codex to PATH."
        } else {
            " Make sure the configured executable exists and can be run."
        };
        return failure(
            input,
            false,
            format!(
                "Could not start Codex CLI (`{binary}`). Check Settings → Providers → Codex → Binary path on the server.{hint}"
            ),
        );
    }
    failure(
        input,
        true,
        format!("Codex app-server provider probe failed: {error}."),
    )
}

fn failure(input: &StatusInput<'_>, installed: bool, message: String) -> ServerProvider {
    provider_with(
        input.kind,
        input.instance_id,
        input.enabled,
        append_custom_codex_models(&[], input.custom_models),
        Vec::new(),
        Vec::new(),
        ProviderProbeResult {
            installed,
            version: None,
            status: ServerProviderState::Error,
            auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
            message: Some(message),
            usage_limits: None,
        },
    )
}

struct AccountStatus {
    status: ServerProviderState,
    auth: ServerProviderAuth,
    message: Option<String>,
}

/// `accountProbeStatus`: signed-in accounts are ready; a missing account that
/// requires OpenAI auth is an error; anything else stays ready with unknown auth.
fn account_probe(response: &Value, workspace_id: Option<&str>) -> AccountStatus {
    let account = response.get("account").filter(|value| !value.is_null());
    let Some(account) = account else {
        if response
            .get("requiresOpenaiAuth")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return AccountStatus {
                status: ServerProviderState::Error,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unauthenticated),
                message: Some(
                    "Codex CLI is not authenticated. Run `codex login` and try again.".to_owned(),
                ),
            };
        }
        return AccountStatus {
            status: ServerProviderState::Ready,
            auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
            message: None,
        };
    };
    let kind = account.get("type").and_then(Value::as_str);
    let mut auth = ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated);
    auth.type_ = kind.and_then(tne);
    auth.label = auth_label(account).and_then(tne);
    auth.email = (kind == Some("chatgpt"))
        .then(|| account.get("email").and_then(Value::as_str))
        .flatten()
        .and_then(tne);
    auth.workspace_id = workspace_id.and_then(|id| tne(id.trim()));
    AccountStatus {
        status: ServerProviderState::Ready,
        auth,
        message: None,
    }
}

/// The ChatGPT workspace the usage read names, when it names one.
fn workspace_id(rate_limits: Option<&Result<Value, String>>) -> Option<&str> {
    rate_limits?
        .as_ref()
        .ok()?
        .get("accountId")
        .and_then(Value::as_str)
}

/// `checkCodexProviderStatus` usage: API-key accounts have no subscription
/// windows; a skipped or failed read reports why.
fn usage_limits_for_probe(
    account: &Value,
    rate_limits: Option<&Result<Value, String>>,
    checked_at: &str,
) -> Option<ServerProviderUsageLimits> {
    if account.pointer("/account/type").and_then(Value::as_str) == Some("apiKey") {
        return Some(make_unavailable_usage_limits(
            checked_at.to_owned(),
            ServerProviderUsageUnavailableReason::Unsupported,
            None,
        ));
    }
    let response = match rate_limits {
        None => return Some(codex_usage_probe_failed_limits(checked_at, None)),
        Some(Err(message)) => {
            return Some(codex_usage_probe_failed_limits(checked_at, Some(message)));
        }
        Some(Ok(response)) => response,
    };
    let Some(snapshot) = response
        .get("rateLimits")
        .and_then(|value| serde_json::from_value::<CodexRateLimitSnapshot>(value.clone()).ok())
    else {
        return Some(codex_usage_probe_failed_limits(
            checked_at,
            Some("Codex did not answer the usage request."),
        ));
    };
    let by_limit_id: Option<HashMap<String, CodexRateLimitSnapshot>> = response
        .get("rateLimitsByLimitId")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let reset_credits: Option<CodexResetCreditsSummary> = response
        .get("rateLimitResetCredits")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    Some(codex_rate_limits_to_limits(
        &snapshot,
        by_limit_id.as_ref(),
        reset_credits.as_ref(),
        checked_at,
    ))
}

fn auth_label(account: &Value) -> Option<&'static str> {
    match account.get("type").and_then(Value::as_str)? {
        "apiKey" => Some("OpenAI API Key"),
        "amazonBedrock" => Some("Amazon Bedrock"),
        "chatgpt" => plan_label(account.get("planType").and_then(Value::as_str)),
        _ => None,
    }
}

/// `codexPlanLabel`.
fn plan_label(plan: Option<&str>) -> Option<&'static str> {
    Some(match plan? {
        "free" => "ChatGPT Free Subscription",
        "go" => "ChatGPT Go Subscription",
        "plus" => "ChatGPT Plus Subscription",
        "pro" => "ChatGPT Pro 20x Subscription",
        "prolite" => "ChatGPT Pro 5x Subscription",
        "promax" => "ChatGPT Pro Max Subscription",
        "team" => "ChatGPT Team Subscription",
        "self_serve_business_prolite" | "self_serve_business_usage_based" | "business" => {
            "ChatGPT Business Subscription"
        }
        "ent26" | "enterprise_cbp_automation" | "enterprise_cbp_usage_based" | "enterprise" => {
            "ChatGPT Enterprise Subscription"
        }
        "edu" | "edu_plus" | "edu_pro" => "ChatGPT Edu Subscription",
        "unknown" => "ChatGPT Subscription",
        _ => return None,
    })
}

fn tne(value: impl AsRef<str>) -> Option<turin_contracts::TrimmedNonEmptyString> {
    turin_contracts::TrimmedNonEmptyString::new(value).ok()
}

#[async_trait]
impl ManagedServerProvider for CodexSnapshot {
    async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        self.maintenance.clone()
    }

    async fn get_snapshot(&self) -> ServerProvider {
        self.cell.get()
    }

    async fn refresh(&self) -> ServerProvider {
        let next = self.check().await;
        self.cell.publish(next.clone());
        next
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        self.cell.stream()
    }

    async fn apply_usage_limits(&self, update: ProviderUsageLimitsUpdate, checked_at: &str) {
        self.cell.apply_usage_limits(&update, checked_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_labels_follow_upstream_switch() {
        assert_eq!(plan_label(Some("plus")), Some("ChatGPT Plus Subscription"));
        assert_eq!(
            plan_label(Some("business")),
            Some("ChatGPT Business Subscription")
        );
        assert_eq!(plan_label(Some("mystery")), None);
        assert_eq!(plan_label(None), None);
    }

    #[test]
    fn account_status_matches_probe_rules() {
        let signed_in = account_probe(
            &json!({
                "account": {"type": "chatgpt", "email": "a@b.c", "planType": "pro"},
                "requiresOpenaiAuth": true
            }),
            Some(" ws-1 "),
        );
        assert_eq!(
            signed_in.auth.workspace_id.as_ref().map(|id| id.as_str()),
            Some("ws-1")
        );
        assert_eq!(signed_in.status, ServerProviderState::Ready);
        assert_eq!(
            signed_in.auth.status,
            ServerProviderAuthStatus::Authenticated
        );
        assert_eq!(
            signed_in.auth.label.as_ref().map(|label| label.as_str()),
            Some("ChatGPT Pro 20x Subscription")
        );
        assert_eq!(
            signed_in.auth.email.as_ref().map(|email| email.as_str()),
            Some("a@b.c")
        );

        let missing = account_probe(&json!({"account": null, "requiresOpenaiAuth": true}), None);
        assert_eq!(missing.status, ServerProviderState::Error);
        assert_eq!(
            missing.auth.status,
            ServerProviderAuthStatus::Unauthenticated
        );

        let open = account_probe(&json!({"account": null, "requiresOpenaiAuth": false}), None);
        assert_eq!(open.status, ServerProviderState::Ready);
        assert_eq!(open.auth.status, ServerProviderAuthStatus::Unknown);
    }

    #[test]
    fn decode_config_defaults_binary_and_rejects_bad_types() {
        let driver = CodexDriver::new();
        let decoded = driver.decode_config(&json!({})).unwrap();
        assert_eq!(decoded["binaryPath"], "codex");
        assert!(driver.decode_config(&json!({"binaryPath": 7})).is_err());
    }

    #[tokio::test]
    async fn missing_binary_reports_not_installed() {
        let driver = CodexDriver::new();
        let instance_id = ProviderInstanceId::new("codex").unwrap();
        let config = driver
            .decode_config(&json!({"binaryPath": "/nonexistent/turin-codex-binary"}))
            .unwrap();
        let instance = driver
            .create(ProviderDriverCreateInput {
                instance_id: instance_id.clone(),
                display_name: None,
                accent_color: None,
                environment: Vec::new(),
                enabled: true,
                config,
                scope: turin_provider_core::driver::ProviderInstanceScope::new(),
            })
            .await
            .unwrap();
        let before = instance.snapshot.get_snapshot().await;
        assert!(!before.draft.installed);
        let after = instance.snapshot.refresh().await;
        assert_eq!(after.instance_id, instance_id);
        assert!(!after.draft.installed);
        assert_eq!(after.draft.status, ServerProviderState::Error);
    }

    #[tokio::test]
    async fn a_shadow_home_is_materialized_and_keys_the_continuation_to_the_shared_home() {
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        let shadow = temp.path().join("shadow");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("config.toml"), "# shared\n").unwrap();
        let driver = CodexDriver::new();
        let config = driver
            .decode_config(&json!({
                "binaryPath": "/nonexistent/turin-codex-binary",
                "homePath": shared.to_string_lossy(),
                "shadowHomePath": shadow.to_string_lossy(),
            }))
            .unwrap();
        let instance = driver
            .create(ProviderDriverCreateInput {
                instance_id: ProviderInstanceId::new("codex-work").unwrap(),
                display_name: turin_contracts::TrimmedNonEmptyString::new("Work").ok(),
                accent_color: None,
                environment: Vec::new(),
                enabled: true,
                config,
                scope: turin_provider_core::driver::ProviderInstanceScope::new(),
            })
            .await
            .unwrap();
        assert_eq!(
            instance.continuation_identity.continuation_key,
            format!("codex:home:{}", shared.display())
        );
        assert_eq!(
            std::fs::read_link(shadow.join("config.toml")).unwrap(),
            shared.join("config.toml")
        );
        let snapshot = instance.snapshot.get_snapshot().await;
        assert_eq!(snapshot.display_name.as_ref().unwrap().as_str(), "Work");
        assert_eq!(
            snapshot.continuation.as_ref().unwrap().group_key.as_str(),
            format!("codex:home:{}", shared.display())
        );
    }

    #[test]
    fn a_successful_probe_offers_compact_goal_and_feedback() {
        let names: Vec<_> = codex_slash_commands()
            .into_iter()
            .map(|command| command.name.as_str().to_owned())
            .collect();
        assert_eq!(names, ["compact", "goal", "feedback"]);
    }
}
