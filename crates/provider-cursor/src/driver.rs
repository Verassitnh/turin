//! `ProviderDriver` for Cursor.
//!
//! `create` materializes one instance. Each `open_session` then owns its own
//! `cursor-agent acp` process. Text generation is not on this transport.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use serde_json::Value;
use turin_contracts::UtcDateTime;
use turin_contracts::git::TextGenerationError;
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::provider_usage_limits::ProviderUsageLimitsUpdate;
use turin_provider_core::driver::{
    ProviderDriver, ProviderDriverCreateInput, ProviderDriverMetadata, ProviderInstance,
    default_provider_continuation_identity, driver_error,
};
use turin_provider_core::errors::ProviderDriverError;
use turin_provider_core::maintenance::ProviderMaintenanceCapabilities;
use turin_provider_core::maintenance_resolver::make_manual_only_provider_maintenance_capabilities;
use turin_provider_core::rt::BoxStream;
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuth, ServerProviderAuthStatus, ServerProviderState,
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

use crate::session::CursorAdapter;
use crate::settings::CursorSettings;

pub struct CursorDriver {
    kind: ProviderDriverKind,
    metadata: ProviderDriverMetadata,
}

impl CursorDriver {
    pub fn new() -> Self {
        Self {
            kind: ProviderDriverKind::new("cursor").expect("cursor driver slug"),
            metadata: ProviderDriverMetadata::new("Cursor"),
        }
    }
}

impl Default for CursorDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderDriver for CursorDriver {
    fn driver_kind(&self) -> &ProviderDriverKind {
        &self.kind
    }

    fn metadata(&self) -> &ProviderDriverMetadata {
        &self.metadata
    }

    fn default_config(&self) -> Value {
        serde_json::to_value(CursorSettings::default())
            .unwrap_or_else(|_| Value::Object(Default::default()))
    }

    fn decode_config(&self, raw: &Value) -> Result<Value, String> {
        let settings = if raw.is_null() {
            CursorSettings::default()
        } else {
            CursorSettings::from_value(raw.clone()).map_err(|error| error.to_string())?
        };
        serde_json::to_value(settings).map_err(|error| error.to_string())
    }

    async fn create(
        &self,
        input: ProviderDriverCreateInput,
    ) -> Result<ProviderInstance, ProviderDriverError> {
        let settings = CursorSettings::from_value(input.config.clone()).map_err(|error| {
            driver_error(&self.kind, &input.instance_id, error.to_string())
        })?;
        let adapter = CursorAdapter::from_settings(input.instance_id.clone(), &settings);
        let snapshot = CursorSnapshot {
            provider: snapshot_provider(&input, &self.kind, settings.enabled && input.enabled),
            maintenance: make_manual_only_provider_maintenance_capabilities(self.kind.clone()),
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
            snapshot: Arc::new(snapshot),
            snapshot_for_cwd: None,
            refresh_models: None,
            invalidate_caches: None,
            consume_reset_credit: None,
            orchestration_adapter: Arc::new(adapter),
            text_generation: Arc::new(CursorText),
            auth: None,
        })
    }
}

struct CursorSnapshot {
    provider: ServerProvider,
    maintenance: ProviderMaintenanceCapabilities,
}

#[async_trait]
impl ManagedServerProvider for CursorSnapshot {
    async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        self.maintenance.clone()
    }

    async fn get_snapshot(&self) -> ServerProvider {
        self.provider.clone()
    }

    async fn refresh(&self) -> ServerProvider {
        self.provider.clone()
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        Box::pin(stream::empty())
    }

    async fn apply_usage_limits(&self, _update: ProviderUsageLimitsUpdate, _checked_at: &str) {}
}

struct CursorText;

fn text_error() -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: "cursor".to_owned(),
        detail: "Cursor ACP does not generate commit messages or titles.".to_owned(),
        cause: None,
    }
}

#[async_trait]
impl ProviderTextGeneration for CursorText {
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

fn snapshot_provider(
    input: &ProviderDriverCreateInput,
    kind: &ProviderDriverKind,
    enabled: bool,
) -> ServerProvider {
    let mut draft = build_server_provider(BuildServerProviderInput {
        driver: Some(kind),
        presentation: ServerProviderPresentation {
            display_name: "Cursor".to_owned(),
            badge_label: None,
            show_interaction_mode_toggle: Some(false),
            reports_context_window: Some(false),
            supported_runtime_modes: None,
            requires_new_thread_for_model_change: Some(false),
            supports_conversation_rollback: Some(false),
        },
        enabled,
        checked_at: UtcDateTime::now().to_string(),
        models: Vec::new(),
        update_required_models: Vec::new(),
        slash_commands: Vec::new(),
        skills: Vec::new(),
        probe: ProviderProbeResult {
            installed: false,
            version: None,
            status: ServerProviderState::Ready,
            auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
            message: None,
            usage_limits: None,
        },
        maintenance: None,
    });
    draft.supports_text_generation = Some(false);
    ServerProvider {
        instance_id: input.instance_id.clone(),
        driver: kind.clone(),
        draft,
    }
}
