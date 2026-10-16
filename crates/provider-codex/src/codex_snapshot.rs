//! The published snapshot of one Codex instance: what `makeManagedServerProvider`
//! keeps for `getSnapshot`, `streamChanges` and `applyUsageLimits`.

use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use turin_contracts::provider_usage_limits::ProviderUsageLimitsUpdate;
use turin_provider_core::{
    rt::BoxStream, server_provider::ServerProvider, usage_limits::apply_usage_limits_update,
};

pub(crate) struct SnapshotCell {
    current: watch::Sender<ServerProvider>,
}

impl SnapshotCell {
    pub(crate) fn new(initial: ServerProvider) -> Self {
        Self {
            current: watch::channel(initial).0,
        }
    }

    pub(crate) fn get(&self) -> ServerProvider {
        self.current.borrow().clone()
    }

    /// Replaces the snapshot and tells every subscriber.
    pub(crate) fn publish(&self, next: ServerProvider) {
        self.current.send_replace(next);
    }

    /// `streamChanges`: the current snapshot, then every published one.
    pub(crate) fn stream(&self) -> BoxStream<'static, ServerProvider> {
        Box::pin(WatchStream::new(self.current.subscribe()))
    }

    /// `applyUsageLimits`: folds a runtime rate-limit push into the published
    /// snapshot. Codex sends one beside every token-usage tick, nearly always
    /// with unchanged numbers, so an unchanged result publishes nothing.
    pub(crate) fn apply_usage_limits(&self, update: &ProviderUsageLimitsUpdate, checked_at: &str) {
        let mut next = self.get();
        let Some(limits) =
            apply_usage_limits_update(next.draft.usage_limits.as_ref(), update, checked_at)
        else {
            return;
        };
        if next.draft.usage_limits.as_ref() == Some(&limits) {
            return;
        }
        next.draft.usage_limits = Some(limits);
        self.publish(next);
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use turin_contracts::{
        TrimmedNonEmptyString,
        provider_instance::{ProviderDriverKind, ProviderInstanceId},
        provider_usage_limits::{ServerProviderUsageWindow, ServerProviderUsageWindowKind},
    };
    use turin_provider_core::{
        server_provider::{ServerProviderAuth, ServerProviderAuthStatus, ServerProviderState},
        snapshot_probe::{
            BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
            build_server_provider,
        },
        usage_limits::make_usage_limits,
    };

    use super::*;

    fn window(id: &str, used: f64) -> ServerProviderUsageWindow {
        ServerProviderUsageWindow {
            id: TrimmedNonEmptyString::new(id).unwrap(),
            kind: ServerProviderUsageWindowKind::Session,
            label: TrimmedNonEmptyString::new("Session").unwrap(),
            used_percent: used,
            resets_at: Some("2026-01-01T05:00:00.000Z".to_owned()),
            window_duration_mins: Some(300),
        }
    }

    fn provider(windows: Vec<ServerProviderUsageWindow>) -> ServerProvider {
        let draft = build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: ServerProviderPresentation {
                display_name: "Codex".to_owned(),
                badge_label: None,
                show_interaction_mode_toggle: None,
                reports_context_window: None,
                supported_runtime_modes: None,
                requires_new_thread_for_model_change: None,
                supports_conversation_rollback: None,
            },
            enabled: true,
            checked_at: "2026-01-01T00:00:00.000Z".to_owned(),
            models: Vec::new(),
            update_required_models: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            probe: ProviderProbeResult {
                installed: true,
                version: None,
                status: ServerProviderState::Ready,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated),
                message: None,
                usage_limits: Some(make_usage_limits(
                    "2026-01-01T00:00:00.000Z".to_owned(),
                    windows,
                )),
            },
            maintenance: None,
        });
        ServerProvider {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
            driver: ProviderDriverKind::new("codex").unwrap(),
            draft,
        }
    }

    fn update(windows: Vec<ServerProviderUsageWindow>) -> ProviderUsageLimitsUpdate {
        ProviderUsageLimitsUpdate { windows }
    }

    #[tokio::test]
    async fn a_usage_push_updates_one_window_and_publishes_once() {
        let cell = SnapshotCell::new(provider(vec![window("primary", 10.0)]));
        let mut changes = cell.stream();
        assert_eq!(
            changes
                .next()
                .await
                .unwrap()
                .usage_limits
                .clone()
                .unwrap()
                .windows[0]
                .used_percent,
            10.0
        );
        cell.apply_usage_limits(
            &update(vec![window("primary", 55.0), window("secondary", 3.0)]),
            "2026-01-01T01:00:00.000Z",
        );
        let published = changes.next().await.unwrap();
        let limits = published.usage_limits.clone().unwrap();
        assert_eq!(limits.checked_at, "2026-01-01T01:00:00.000Z");
        let used: Vec<_> = limits
            .windows
            .iter()
            .map(|window| (window.id.as_str().to_owned(), window.used_percent))
            .collect();
        assert_eq!(
            used,
            [("primary".to_owned(), 55.0), ("secondary".to_owned(), 3.0)]
        );
        assert_eq!(cell.get().usage_limits.clone().unwrap().windows.len(), 2);
    }

    #[tokio::test]
    async fn an_unchanged_or_empty_push_publishes_nothing() {
        let cell = SnapshotCell::new(provider(vec![window("primary", 10.0)]));
        let mut changes = cell.stream();
        changes.next().await.unwrap();
        cell.apply_usage_limits(
            &update(vec![window("primary", 10.0)]),
            "2026-01-01T01:00:00.000Z",
        );
        cell.apply_usage_limits(&update(Vec::new()), "2026-01-01T01:00:00.000Z");
        let waited =
            tokio::time::timeout(std::time::Duration::from_millis(100), changes.next()).await;
        assert!(waited.is_err(), "nothing was published");
        assert_eq!(
            cell.get().usage_limits.clone().unwrap().checked_at,
            "2026-01-01T00:00:00.000Z"
        );
    }
}
