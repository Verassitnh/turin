//! `ProviderDriver` for Cursor.
//!
//! `create` materializes one instance. Each `open_session` then owns its own
//! `cursor-agent acp` process. Text generation is not on this transport.
//!
//! The snapshot is probed on refresh, not at creation: a probe spawns
//! `cursor-agent`, so it waits for the first refresh the host asks for.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use serde_json::Value;
use tokio::sync::watch;
use turin_contracts::UtcDateTime;
use turin_contracts::git::TextGenerationError;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_usage_limits::ProviderUsageLimitsUpdate;
use turin_provider_core::driver::{
    ProviderDriver, ProviderDriverCreateInput, ProviderDriverMetadata, ProviderInstance,
    RefreshModels, default_provider_continuation_identity, driver_error,
};
use turin_provider_core::errors::ProviderDriverError;
use turin_provider_core::maintenance::ProviderMaintenanceCapabilities;
use turin_provider_core::maintenance_resolver::make_manual_only_provider_maintenance_capabilities;
use turin_provider_core::rt::{BoxFuture, BoxStream};
use turin_provider_core::server_provider::{ServerProvider, ServerProviderAuthStatus};
use turin_provider_core::snapshot::ManagedServerProvider;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ThreadTitleGenerationInput, ThreadTitleGenerationResult,
};

use crate::probe::probe_status;
use crate::session::CursorAdapter;
use crate::settings::CursorSettings;
use crate::sign_in;
use crate::status;

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
        let settings = CursorSettings::from_value(input.config.clone())
            .map_err(|error| driver_error(&self.kind, &input.instance_id, error.to_string()))?;
        let adapter = CursorAdapter::from_settings(input.instance_id.clone(), &settings);
        let enabled = settings.enabled && input.enabled;
        let snapshot = Arc::new(CursorSnapshot::new(
            self.kind.clone(),
            input.instance_id.clone(),
            settings.clone(),
            enabled,
        ));

        let refresh_snapshot = Arc::clone(&snapshot);
        let refresh_models: RefreshModels = Arc::new(move || {
            let snapshot = Arc::clone(&refresh_snapshot);
            Box::pin(async move {
                snapshot.refresh().await;
                Ok(())
            }) as BoxFuture<'static, Result<(), ProviderDriverError>>
        });
        let changed_snapshot = Arc::clone(&snapshot);
        let auth = sign_in::controller(
            input.instance_id.clone(),
            settings.agent_program().to_owned(),
            enabled,
            Arc::new(move || {
                let snapshot = Arc::clone(&changed_snapshot);
                Box::pin(async move {
                    snapshot.refresh().await;
                }) as BoxFuture<'static, ()>
            }),
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
            refresh_models: Some(refresh_models),
            invalidate_caches: None,
            consume_reset_credit: None,
            orchestration_adapter: Arc::new(adapter),
            text_generation: Arc::new(CursorText),
            auth: Some(Arc::new(auth)),
        })
    }
}

/// The published Cursor snapshot for one instance. Refresh probes the agent
/// and publishes the result to `get_snapshot` and `stream_changes`.
struct CursorSnapshot {
    driver: ProviderDriverKind,
    instance_id: ProviderInstanceId,
    settings: CursorSettings,
    enabled: bool,
    program: String,
    maintenance: ProviderMaintenanceCapabilities,
    current: watch::Sender<ServerProvider>,
}

impl CursorSnapshot {
    fn new(
        driver: ProviderDriverKind,
        instance_id: ProviderInstanceId,
        settings: CursorSettings,
        enabled: bool,
    ) -> Self {
        let initial = ServerProvider {
            instance_id: instance_id.clone(),
            driver: driver.clone(),
            draft: status::initial_draft(
                &driver,
                &settings,
                enabled,
                UtcDateTime::now().to_string(),
            ),
        };
        let (current, _) = watch::channel(initial);
        Self {
            program: settings.agent_program().to_owned(),
            maintenance: make_manual_only_provider_maintenance_capabilities(driver.clone()),
            driver,
            instance_id,
            settings,
            enabled,
            current,
        }
    }

    async fn probe(&self) -> ServerProvider {
        let checked_at = UtcDateTime::now().to_string();
        let draft = if self.enabled {
            let probe = probe_status(&self.program).await;
            // Upstream warns when the catalog comes back empty for a signed-in account.
            let warning = (probe.models.is_empty()
                && probe.message.is_none()
                && probe.auth.status == ServerProviderAuthStatus::Authenticated)
                .then(|| "Cursor model discovery returned no built-in models.".to_owned());
            status::draft_from_probe(
                &self.driver,
                self.enabled,
                &self.settings,
                checked_at,
                probe,
                warning,
            )
        } else {
            status::initial_draft(&self.driver, &self.settings, false, checked_at)
        };
        let provider = ServerProvider {
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
            draft,
        };
        self.current.send_replace(provider.clone());
        provider
    }
}

#[async_trait]
impl ManagedServerProvider for CursorSnapshot {
    async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        self.maintenance.clone()
    }

    async fn get_snapshot(&self) -> ServerProvider {
        self.current.borrow().clone()
    }

    async fn refresh(&self) -> ServerProvider {
        self.probe().await
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        let receiver = self.current.subscribe();
        Box::pin(stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            let provider = receiver.borrow_and_update().clone();
            Some((provider, receiver))
        }))
    }

    /// Cursor's usage limits come from the dashboard, which is not ported.
    /// See PORTING.md Deviations.
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;
    use turin_contracts::provider_instance::{
        ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
    };
    use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
    use turin_provider_core::server_provider::ServerProviderState;

    fn entry(config: Value) -> ProviderInstanceConfig {
        ProviderInstanceConfig {
            driver: ProviderDriverKind::new("cursor").unwrap(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled: Some(true),
            config: Some(config),
        }
    }

    #[tokio::test]
    async fn registry_builds_cursor_instances_and_shadows_bad_config() {
        let good = ProviderInstanceId::new("cursor").unwrap();
        let bad = ProviderInstanceId::new("cursor-bad").unwrap();
        let mut map = ProviderInstanceConfigMap::default();
        map.insert(good.clone(), entry(json!({"enabled": true})));
        map.insert(bad.clone(), entry(json!({"binaryPath": null})));
        let registry = ProviderInstanceRegistry::new(
            vec![Arc::new(CursorDriver::new()) as Arc<dyn ProviderDriver>],
            map,
        )
        .await;

        let instance = registry.get_instance(&good).unwrap();
        assert_eq!(instance.driver_kind.as_str(), "cursor");
        let adapter = &instance.orchestration_adapter;
        assert_eq!(adapter.instance_id(), &good);
        let capabilities = adapter.get_capabilities().await.unwrap();
        assert!(!capabilities.threads.can_fork_thread);
        assert!(!capabilities.turns.supports_active_steering);
        assert_eq!(
            instance
                .snapshot
                .get_snapshot()
                .await
                .draft
                .supports_text_generation,
            Some(false)
        );

        assert!(registry.get_instance(&bad).is_none());
        let shadows = registry.list_unavailable();
        assert_eq!(shadows.len(), 1);
        assert_eq!(shadows[0].instance_id, bad);
    }

    #[tokio::test]
    async fn refresh_probes_a_missing_agent_and_publishes_it() {
        let id = ProviderInstanceId::new("cursor").unwrap();
        let mut map = ProviderInstanceConfigMap::default();
        map.insert(
            id.clone(),
            entry(json!({
                "enabled": true,
                "binaryPath": "/nonexistent/turin-cursor-agent-for-tests"
            })),
        );
        let registry = ProviderInstanceRegistry::new(
            vec![Arc::new(CursorDriver::new()) as Arc<dyn ProviderDriver>],
            map,
        )
        .await;
        let instance = registry.get_instance(&id).unwrap();
        let before = instance.snapshot.get_snapshot().await;
        assert!(before.draft.installed);
        assert_eq!(before.draft.status, ServerProviderState::Warning);

        let mut changes = instance.snapshot.stream_changes();
        let refreshed = instance.snapshot.refresh().await;
        assert!(!refreshed.draft.installed);
        assert_eq!(refreshed.draft.status, ServerProviderState::Error);
        assert!(
            refreshed
                .draft
                .message
                .as_ref()
                .is_some_and(|message| message
                    .as_str()
                    .starts_with("Cursor agent could not start"))
        );
        let published = changes.next().await.expect("refresh is published");
        assert_eq!(published.draft.status, ServerProviderState::Error);
        assert_eq!(
            instance.snapshot.get_snapshot().await.draft.status,
            ServerProviderState::Error
        );
    }

    #[tokio::test]
    async fn disabled_instances_never_probe_and_cannot_sign_in() {
        let id = ProviderInstanceId::new("cursor").unwrap();
        let mut map = ProviderInstanceConfigMap::default();
        map.insert(id.clone(), entry(json!({"enabled": false})));
        let registry = ProviderInstanceRegistry::new(
            vec![Arc::new(CursorDriver::new()) as Arc<dyn ProviderDriver>],
            map,
        )
        .await;
        let instance = registry.get_instance(&id).unwrap();
        let refreshed = instance.snapshot.refresh().await;
        assert!(!refreshed.draft.installed);
        assert_eq!(refreshed.draft.status, ServerProviderState::Disabled);
        let auth = instance
            .auth
            .as_ref()
            .expect("cursor has a sign-in controller");
        let error = (auth.start)("t3".to_owned(), None, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Enable Cursor before signing in.");
    }
}
