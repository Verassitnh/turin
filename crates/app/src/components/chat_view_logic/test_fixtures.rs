//! Shared helpers for ChatView.logic upstream test ports.

#[cfg(test)]
pub mod provider {
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
    use turin_provider_core::server_provider::{
        ServerProvider, ServerProviderAuth, ServerProviderAuthStatus, ServerProviderModel,
        ServerProviderState,
    };

    use crate::components::chat_view_logic::provider_instances::ProviderInstanceEntry;

    pub struct EntryOverrides {
        pub enabled: bool,
        pub installed: bool,
        pub status: ServerProviderState,
        pub auth: ServerProviderAuth,
        pub models: Vec<ServerProviderModel>,
        pub show_interaction_mode_toggle: Option<bool>,
        pub continuation_group_key: Option<&'static str>,
    }

    impl Default for EntryOverrides {
        fn default() -> Self {
            Self {
                enabled: true,
                installed: true,
                status: ServerProviderState::Ready,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated),
                models: Vec::new(),
                show_interaction_mode_toggle: None,
                continuation_group_key: None,
            }
        }
    }

    pub fn catalog_models() -> Vec<ServerProviderModel> {
        vec![ServerProviderModel {
            slug: TrimmedNonEmptyString::new("gemini-pro").unwrap(),
            name: TrimmedNonEmptyString::new("Gemini Pro").unwrap(),
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: false,
            is_default: None,
            is_legacy: None,
            capabilities: None,
        }]
    }

    pub fn provider_entry(
        driver: &str,
        instance_id: &str,
        overrides: EntryOverrides,
    ) -> ProviderInstanceEntry {
        let driver_kind = ProviderDriverKind::new(driver).unwrap();
        let instance_id = ProviderInstanceId::new(instance_id).unwrap();
        let models_json: Vec<serde_json::Value> = overrides
            .models
            .iter()
            .map(|model| {
                serde_json::json!({
                    "slug": model.slug.as_str(),
                    "name": model.name.as_str(),
                    "isCustom": model.is_custom,
                    "capabilities": model.capabilities,
                })
            })
            .collect();
        let mut snapshot: ServerProvider = serde_json::from_value(serde_json::json!({
            "instanceId": instance_id.as_str(),
            "driver": driver,
            "enabled": overrides.enabled,
            "installed": overrides.installed,
            "status": match overrides.status {
                ServerProviderState::Ready => "ready",
                ServerProviderState::Warning => "warning",
                ServerProviderState::Error => "error",
                ServerProviderState::Disabled => "disabled",
            },
            "auth": { "status": match overrides.auth.status {
                ServerProviderAuthStatus::Authenticated => "authenticated",
                ServerProviderAuthStatus::Unauthenticated => "unauthenticated",
                ServerProviderAuthStatus::Unknown => "unknown",
            }},
            "models": models_json,
            "showInteractionModeToggle": overrides.show_interaction_mode_toggle,
            "continuation": overrides.continuation_group_key.map(|group_key| serde_json::json!({ "groupKey": group_key })),
            "checkedAt": "2026-03-29T00:00:00.000Z",
            "version": null,
            "slashCommands": [],
            "skills": [],
        }))
        .expect("provider snapshot json");
        snapshot.models = overrides.models;
        ProviderInstanceEntry {
            instance_id: instance_id.clone(),
            driver_kind,
            continuation_group_key: overrides.continuation_group_key.map(str::to_owned),
            enabled: overrides.enabled,
            is_available: true,
            status: overrides.status,
            snapshot,
        }
    }
}

#[cfg(test)]
pub mod dispatch {
    use turin_contracts::{MessageId, RunId};

    use crate::components::chat_view_logic::dispatch::{
        HasServerAcknowledgedLocalDispatchInput, LocalDispatchSnapshot, SessionPhase,
        create_local_dispatch_snapshot, has_server_acknowledged_local_dispatch,
    };
    use crate::components::chat_view_logic::draft::ComposerSubmissionIntent;
    use crate::state::models::{RunStatus, ThreadRunSummary, ThreadRuntimeSummary};

    pub fn completed_turn() -> ThreadRunSummary {
        ThreadRunSummary {
            run_id: RunId::new("turn-1").unwrap(),
            status: RunStatus::Completed,
            requested_at: Some("2026-03-29T00:00:00.000Z".to_owned()),
            started_at: Some("2026-03-29T00:00:01.000Z".to_owned()),
            completed_at: Some("2026-03-29T00:00:10.000Z".to_owned()),
        }
    }

    pub fn ready_session() -> ThreadRuntimeSummary {
        ThreadRuntimeSummary {
            status: RunStatus::Completed,
            active_run_id: None,
            activity_started_at: None,
            provider_instance_id: "codex".to_owned(),
            provider_name: Some("codex".to_owned()),
            last_error: None,
            last_error_class: None,
            updated_at: "2026-03-29T00:00:10.000Z".to_owned(),
        }
    }

    pub fn snapshot_from_thread(
        latest_run: Option<ThreadRunSummary>,
        runtime: Option<ThreadRuntimeSummary>,
        preparing_worktree: bool,
        latest_user_message_id: Option<MessageId>,
        submission_intent: ComposerSubmissionIntent,
    ) -> LocalDispatchSnapshot {
        create_local_dispatch_snapshot(
            latest_run.as_ref(),
            runtime.as_ref(),
            preparing_worktree,
            latest_user_message_id,
            submission_intent,
        )
    }

    pub fn ack_input<'a>(
        local_dispatch: &'a LocalDispatchSnapshot,
        phase: SessionPhase,
        latest_run: Option<&'a ThreadRunSummary>,
        latest_user_message_id: Option<&'a MessageId>,
        runtime: Option<&'a ThreadRuntimeSummary>,
        pending: (bool, bool),
        thread_error: Option<&'a str>,
    ) -> HasServerAcknowledgedLocalDispatchInput<'a> {
        let (has_pending_approval, has_pending_user_input) = pending;
        HasServerAcknowledgedLocalDispatchInput {
            local_dispatch: Some(local_dispatch),
            phase,
            latest_run,
            latest_user_message_id,
            runtime,
            has_pending_approval,
            has_pending_user_input,
            thread_error,
        }
    }
}
