//! The OpenCode `ProviderDriver` the composition root fills the built-in
//! driver slot with.
//!
//! `turin-provider-opencode` ships the session adapter (`OpenCodeAdapter`) but
//! no `ProviderDriver` yet (T11's handoff names none), so without this the
//! `opencode` slot stayed empty and a configured OpenCode instance never
//! materialized. This driver is the minimum that makes an instance run:
//!
//! - config: `{enabled, binaryPath, serverUrl, serverPassword}` (upstream's
//!   `OpenCodeSettings`), decoded into the adapter's settings;
//! - orchestration adapter: the real `OpenCodeAdapter`, which probes the
//!   binary or server and launches `opencode serve` itself;
//! - snapshot: the pending draft (`ProbeOutcome::Pending`). The live probe and
//!   inventory fetch (`status.ts`) are not wired, so the settings and composer
//!   views see "checking" until that slice lands;
//! - text generation: not provided (same message shape as Cursor's).

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use serde_json::{Value, json};
use tokio::sync::watch;
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
use turin_provider_core::server_provider::ServerProvider;
use turin_provider_core::snapshot::ManagedServerProvider;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ThreadTitleGenerationInput, ThreadTitleGenerationResult,
};
use turin_provider_opencode::{
    OpenCodeAdapter, OpenCodeSettings, OpenCodeSnapshotInput, ProbeOutcome,
    build_open_code_snapshot,
};

pub struct OpenCodeDriver {
    kind: ProviderDriverKind,
    metadata: ProviderDriverMetadata,
}

impl OpenCodeDriver {
    pub fn new() -> Self {
        Self {
            kind: ProviderDriverKind::new("opencode").expect("opencode driver slug"),
            metadata: ProviderDriverMetadata::new("OpenCode"),
        }
    }
}

impl Default for OpenCodeDriver {
    fn default() -> Self {
        Self::new()
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

#[async_trait]
impl ProviderDriver for OpenCodeDriver {
    fn driver_kind(&self) -> &ProviderDriverKind {
        &self.kind
    }

    fn metadata(&self) -> &ProviderDriverMetadata {
        &self.metadata
    }

    fn default_config(&self) -> Value {
        json!({"enabled": false, "binaryPath": "opencode", "serverUrl": "", "serverPassword": ""})
    }

    fn decode_config(&self, raw: &Value) -> Result<Value, String> {
        let settings = settings_from(raw)?;
        Ok(json!({
            "enabled": settings.enabled,
            "binaryPath": settings.binary_path,
            "serverUrl": settings.server_url,
            "serverPassword": settings.server_password,
        }))
    }

    async fn create(
        &self,
        input: ProviderDriverCreateInput,
    ) -> Result<ProviderInstance, ProviderDriverError> {
        let settings = settings_from(&input.config)
            .map_err(|error| driver_error(&self.kind, &input.instance_id, error))?;
        let enabled = settings.enabled && input.enabled;
        let draft = build_open_code_snapshot(OpenCodeSnapshotInput {
            enabled,
            server_url: &settings.server_url,
            checked_at: UtcDateTime::now().to_string(),
            outcome: ProbeOutcome::Pending,
            usage_limits: None,
        });
        let (current, _) = watch::channel(ServerProvider {
            instance_id: input.instance_id.clone(),
            driver: self.kind.clone(),
            draft,
        });
        let snapshot = Arc::new(OpenCodeSnapshot {
            maintenance: make_manual_only_provider_maintenance_capabilities(self.kind.clone()),
            current,
        });
        let adapter = OpenCodeAdapter::new(
            input.instance_id.clone(),
            OpenCodeSettings {
                enabled,
                ..settings
            },
        );
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
            orchestration_adapter: Arc::new(adapter),
            text_generation: Arc::new(OpenCodeText),
            auth: None,
        })
    }
}

struct OpenCodeSnapshot {
    maintenance: ProviderMaintenanceCapabilities,
    current: watch::Sender<ServerProvider>,
}

#[async_trait]
impl ManagedServerProvider for OpenCodeSnapshot {
    async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        self.maintenance.clone()
    }

    async fn get_snapshot(&self) -> ServerProvider {
        self.current.borrow().clone()
    }

    async fn refresh(&self) -> ServerProvider {
        self.current.borrow().clone()
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        let receiver = self.current.subscribe();
        Box::pin(stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            let provider = receiver.borrow_and_update().clone();
            Some((provider, receiver))
        }))
    }

    async fn apply_usage_limits(&self, _update: ProviderUsageLimitsUpdate, _checked_at: &str) {}
}

struct OpenCodeText;

fn text_error() -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: "opencode".to_owned(),
        detail: "OpenCode text generation is not wired in Turin.".to_owned(),
        cause: None,
    }
}

#[async_trait]
impl ProviderTextGeneration for OpenCodeText {
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
    fn config_round_trips_with_upstream_defaults() {
        let driver = OpenCodeDriver::new();
        let decoded = driver
            .decode_config(&json!({"enabled": true, "binaryPath": " /opt/opencode "}))
            .expect("decodes");
        assert_eq!(decoded["enabled"], true);
        assert_eq!(decoded["binaryPath"], "/opt/opencode");
        assert_eq!(decoded["serverUrl"], "");
        assert_eq!(
            driver.decode_config(&Value::Null).expect("null")["binaryPath"],
            "opencode"
        );
        assert!(driver.decode_config(&json!({"binaryPath": 3})).is_err());
    }
}
