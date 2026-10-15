//! `ProviderDriver` for Codex: port of `CodexDriver` and the status half of
//! `CodexProvider.ts` (`makePendingCodexProvider`, `checkCodexProviderStatus`,
//! `accountProbeStatus`).
//!
//! `create` materializes one instance from its decoded `CodexSettings`. The
//! snapshot starts pending; `refresh` connects a `codex app-server`, reads the
//! account and model list, and reports installed/auth/status. Usage limits,
//! skills, slash commands, and text generation are not ported yet (PORTING.md).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::RwLock;
use serde_json::{Value, json};
use turin_codex_app_server::errors::AppServerError;
use turin_contracts::UtcDateTime;
use turin_contracts::git::TextGenerationError;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_usage_limits::ProviderUsageLimitsUpdate;
use turin_contracts::settings::CodexSettings;
use turin_provider_core::driver::{
    ProviderDriver, ProviderDriverCreateInput, ProviderDriverMetadata, ProviderInstance,
    default_provider_continuation_identity, driver_error,
};
use turin_provider_core::errors::ProviderDriverError;
use turin_provider_core::maintenance::ProviderMaintenanceCapabilities;
use turin_provider_core::maintenance_resolver::make_manual_only_provider_maintenance_capabilities;
use turin_provider_core::rt::BoxStream;
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuth, ServerProviderAuthStatus, ServerProviderModel,
    ServerProviderState,
};
use turin_provider_core::snapshot::ManagedServerProvider;
use turin_provider_core::snapshot_probe::{
    BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
    build_server_provider,
};
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ThreadTitleGenerationInput, ThreadTitleGenerationResult,
};

use crate::{
    codex_provider::{now_ms, probe},
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
}

impl CodexDriver {
    pub fn new() -> Self {
        Self {
            kind: ProviderDriverKind::new("codex").expect("codex driver slug"),
            metadata: ProviderDriverMetadata::new("Codex"),
        }
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
        let config = runtime_config(&settings, &input);
        let enabled = settings.enabled && input.enabled;
        let maintenance = make_manual_only_provider_maintenance_capabilities(self.kind.clone());
        let snapshot = Arc::new(CodexSnapshot {
            kind: self.kind.clone(),
            instance_id: input.instance_id.clone(),
            enabled,
            config: config.clone(),
            current: RwLock::new(pending_provider(&self.kind, &input.instance_id, enabled)),
            maintenance,
        });
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
            snapshot_for_cwd: None,
            refresh_models: None,
            invalidate_caches: None,
            consume_reset_credit: None,
            orchestration_adapter: Arc::new(CodexProviderAdapter::new(
                input.instance_id.clone(),
                config,
            )),
            text_generation: Arc::new(CodexText),
            auth: None,
        })
    }
}

/// `binaryPath`, `homePath`, and `launchArgs` from settings, plus the instance
/// environment. `launchArgs` splits on whitespace; upstream's `codexLaunchArgs.ts`
/// shell-splits it (PORTING.md).
fn runtime_config(
    settings: &CodexSettings,
    input: &ProviderDriverCreateInput,
) -> CodexRuntimeConfig {
    let home_path = Some(settings.home_path.as_str().to_owned()).filter(|home| !home.is_empty());
    let launch_args = settings
        .launch_args
        .as_str()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let environment: BTreeMap<String, String> = input
        .environment
        .iter()
        .map(|variable| (variable.name.as_str().to_owned(), variable.value.clone()))
        .collect();
    CodexRuntimeConfig {
        binary: PathBuf::from(settings.binary_path.as_str()),
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        home_path,
        launch_args,
        environment,
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

fn provider_with(
    kind: &ProviderDriverKind,
    instance_id: &ProviderInstanceId,
    enabled: bool,
    models: Vec<ServerProviderModel>,
    probe: ProviderProbeResult,
) -> ServerProvider {
    let mut draft = build_server_provider(BuildServerProviderInput {
        driver: Some(kind),
        presentation: presentation(),
        enabled,
        checked_at: UtcDateTime::now().to_string(),
        models,
        update_required_models: Vec::new(),
        slash_commands: Vec::new(),
        skills: Vec::new(),
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

/// `makePendingCodexProvider`: the snapshot before the first probe.
fn pending_provider(
    kind: &ProviderDriverKind,
    instance_id: &ProviderInstanceId,
    enabled: bool,
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
    current: RwLock<ServerProvider>,
    maintenance: ProviderMaintenanceCapabilities,
}

impl CodexSnapshot {
    /// `checkCodexProviderStatus`, minus usage limits and skills (not ported).
    async fn check(&self) -> ServerProvider {
        if !self.enabled {
            return pending_provider(&self.kind, &self.instance_id, false);
        }
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let checked = tokio::time::timeout(AUTH_PROBE_TIMEOUT, self.probe(&home)).await;
        match checked {
            Ok(provider) => provider,
            Err(_) => self.failure(
                true,
                "Timed out while checking Codex app-server provider status.".to_owned(),
            ),
        }
    }

    async fn probe(&self, home: &Path) -> ServerProvider {
        let session = match self.config.connect(home).await {
            Ok(session) => session,
            Err(error) => return self.spawn_or_probe_failure(&error),
        };
        let cwd = self.config.cwd.to_string_lossy().into_owned();
        // Usage is not ported yet, so skip the native rate-limit read.
        let snapshot = probe(&session, &cwd, true, now_ms()).await;
        let version = session.version.clone();
        let _ = session.shutdown().await;
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => return self.spawn_or_probe_failure(&error),
        };
        let models = snapshot
            .models
            .iter()
            .filter_map(model_from_probe)
            .collect::<Vec<_>>();
        let account = account_probe(&snapshot.account);
        provider_with(
            &self.kind,
            &self.instance_id,
            self.enabled,
            models,
            ProviderProbeResult {
                installed: true,
                version: version.or(snapshot.version),
                status: account.status,
                auth: account.auth,
                message: account.message,
                usage_limits: None,
            },
        )
    }

    fn spawn_or_probe_failure(&self, error: &AppServerError) -> ServerProvider {
        if let AppServerError::Spawn(_) = error {
            let binary = self.config.binary.to_string_lossy().into_owned();
            let hint = if binary == "codex" {
                " Installing ChatGPT or Codex desktop may not add codex to PATH."
            } else {
                " Make sure the configured executable exists and can be run."
            };
            return self.failure(
                false,
                format!(
                    "Could not start Codex CLI (`{binary}`). Check Settings → Providers → Codex → Binary path on the server.{hint}"
                ),
            );
        }
        self.failure(
            true,
            format!("Codex app-server provider probe failed: {error}."),
        )
    }

    fn failure(&self, installed: bool, message: String) -> ServerProvider {
        provider_with(
            &self.kind,
            &self.instance_id,
            self.enabled,
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
}

struct AccountStatus {
    status: ServerProviderState,
    auth: ServerProviderAuth,
    message: Option<String>,
}

/// `accountProbeStatus`: signed-in accounts are ready; a missing account that
/// requires OpenAI auth is an error; anything else stays ready with unknown auth.
fn account_probe(response: &Value) -> AccountStatus {
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
    AccountStatus {
        status: ServerProviderState::Ready,
        auth,
        message: None,
    }
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

/// Maps one raw `model/list` entry. Capabilities and custom models are not
/// ported yet (PORTING.md).
fn model_from_probe(model: &Value) -> Option<ServerProviderModel> {
    let slug = model
        .get("model")
        .or_else(|| model.get("id"))
        .and_then(Value::as_str)?;
    let name = model
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or(slug);
    Some(ServerProviderModel {
        slug: tne(slug)?,
        name: tne(name)?,
        short_name: None,
        sub_provider: None,
        aliases: None,
        badge: None,
        is_custom: false,
        is_default: model.get("isDefault").and_then(Value::as_bool),
        is_legacy: None,
        capabilities: None,
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
        self.current.read().clone()
    }

    async fn refresh(&self) -> ServerProvider {
        let next = self.check().await;
        *self.current.write() = next.clone();
        next
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        Box::pin(stream::empty())
    }

    async fn apply_usage_limits(&self, _update: ProviderUsageLimitsUpdate, _checked_at: &str) {}
}

/// Text generation runs a one-shot Codex turn upstream. That needs a paid turn
/// and is not ported yet, so it reports the gap instead of inventing output.
struct CodexText;

fn text_error() -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: "codex".to_owned(),
        detail: "Codex text generation is not ported to Turin yet.".to_owned(),
        cause: None,
    }
}

#[async_trait]
impl ProviderTextGeneration for CodexText {
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
        let signed_in = account_probe(&json!({
            "account": {"type": "chatgpt", "email": "a@b.c", "planType": "pro"},
            "requiresOpenaiAuth": true
        }));
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

        let missing = account_probe(&json!({"account": null, "requiresOpenaiAuth": true}));
        assert_eq!(missing.status, ServerProviderState::Error);
        assert_eq!(
            missing.auth.status,
            ServerProviderAuthStatus::Unauthenticated
        );

        let open = account_probe(&json!({"account": null, "requiresOpenaiAuth": false}));
        assert_eq!(open.status, ServerProviderState::Ready);
        assert_eq!(open.auth.status, ServerProviderAuthStatus::Unknown);
    }

    #[test]
    fn model_entries_map_slug_name_and_default() {
        let model = model_from_probe(&json!({
            "model": "gpt-x", "displayName": "GPT X", "isDefault": true
        }))
        .unwrap();
        assert_eq!(model.slug.as_str(), "gpt-x");
        assert_eq!(model.name.as_str(), "GPT X");
        assert_eq!(model.is_default, Some(true));
        assert!(model_from_probe(&json!({"displayName": "no slug"})).is_none());
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
}
