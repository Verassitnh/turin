//! Upstream `ChatView.logic.test.ts` provider selection and locked-provider matrix.

use turin_contracts::model::ANTIGRAVITY_DEFAULT_MODEL;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

use super::composer::{
    ComposerInteractionMode, get_antigravity_send_block_reason, resolve_composer_interaction_mode,
    resolve_composer_provider_selection,
};
use super::provider_instances::NO_PROVIDER_INSTANCE_ID;
use super::test_fixtures::provider::{self, EntryOverrides};
use super::thread::{
    DeriveLockedProviderInput, ProviderDriverLookup, ThreadStartedView, derive_locked_provider,
};
use crate::state::models::{RunStatus, ThreadRuntimeSummary};

fn imported_thread(_instance_id: &ProviderInstanceId) -> ThreadStartedView<'_> {
    ThreadStartedView {
        latest_run: None,
        item_count: 1,
        runtime: None,
    }
}

fn driver_lookups(
    entries: &[super::provider_instances::ProviderInstanceEntry],
) -> Vec<ProviderDriverLookup<'_>> {
    entries
        .iter()
        .map(|entry| ProviderDriverLookup {
            instance_id: &entry.instance_id,
            driver: entry.driver_kind.clone(),
        })
        .collect()
}

#[test]
fn keeps_imported_claude_history_selectable_through_its_custom_instance() {
    matrix_imported_history("claudeAgent", "claude_work");
}

#[test]
fn keeps_imported_codex_history_selectable_through_its_custom_instance() {
    matrix_imported_history("codex", "codex_work");
}

#[test]
fn keeps_imported_ollama_history_selectable_through_its_custom_instance() {
    matrix_imported_history("ollama", "local_models");
}

fn matrix_imported_history(driver: &str, instance_id: &str) {
    let imported_entry = provider::provider_entry(driver, instance_id, EntryOverrides::default());
    let other_driver = if driver == "codex" {
        "claudeAgent"
    } else {
        "codex"
    };
    let entries = vec![
        provider::provider_entry(other_driver, other_driver, EntryOverrides::default()),
        imported_entry.clone(),
    ];
    let lookups = driver_lookups(&entries);
    let thread = imported_thread(&imported_entry.instance_id);
    let locked = derive_locked_provider(DeriveLockedProviderInput {
        thread: Some(thread),
        selected_provider: Some(&entries[0].instance_id),
        thread_provider: Some(&imported_entry.instance_id),
        providers: &lookups,
    });
    assert_eq!(locked.as_ref().map(|d| d.as_str()), Some(driver));
    let selection = resolve_composer_provider_selection(
        &entries,
        &[Some(imported_entry.instance_id.clone())],
        locked.as_ref(),
        Some(&imported_entry.instance_id),
    );
    assert_eq!(
        selection
            .selected_provider_entry
            .as_ref()
            .map(|e| &e.instance_id),
        Some(&imported_entry.instance_id)
    );
}

#[test]
fn keeps_the_session_driver_authoritative_over_instance_and_draft_selections() {
    let selected =
        provider::provider_entry("claudeAgent", "claude_work", EntryOverrides::default());
    let session_entry =
        provider::provider_entry("ollama", "local_models", EntryOverrides::default());
    let runtime = ThreadRuntimeSummary {
        status: RunStatus::Completed,
        active_run_id: None,
        activity_started_at: None,
        provider_instance_id: session_entry.instance_id.as_str().to_owned(),
        provider_name: Some(session_entry.driver_kind.as_str().to_owned()),
        last_error: None,
        last_error_class: None,
        updated_at: "2026-03-29T00:00:10.000Z".to_owned(),
    };
    let thread_view = ThreadStartedView {
        latest_run: None,
        item_count: 1,
        runtime: Some(&runtime),
    };
    let entry_pair = [selected.clone(), session_entry.clone()];
    let lookups = driver_lookups(&entry_pair);
    assert_eq!(
        derive_locked_provider(DeriveLockedProviderInput {
            thread: Some(thread_view),
            selected_provider: Some(&selected.instance_id),
            thread_provider: Some(&selected.instance_id),
            providers: &lookups,
        }),
        Some(session_entry.driver_kind.clone())
    );
}

#[test]
fn does_not_move_imported_history_when_its_instance_is_missing_from_the_catalog() {
    missing_or_disabled_imported("missing");
}

#[test]
fn does_not_move_imported_history_when_its_instance_is_disabled() {
    missing_or_disabled_imported("disabled");
}

fn missing_or_disabled_imported(state: &str) {
    let imported = provider::provider_entry(
        "claudeAgent",
        "claude_work",
        EntryOverrides {
            enabled: false,
            ..Default::default()
        },
    );
    let other = provider::provider_entry("codex", "codex", EntryOverrides::default());
    let entries = if state == "missing" {
        vec![other.clone()]
    } else {
        vec![other.clone(), imported.clone()]
    };
    let lookups = driver_lookups(&entries);
    let thread = imported_thread(&imported.instance_id);
    let locked = derive_locked_provider(DeriveLockedProviderInput {
        thread: Some(thread),
        selected_provider: Some(&other.instance_id),
        thread_provider: Some(&imported.instance_id),
        providers: &lookups,
    });
    assert!(locked.is_some());
    let selection = resolve_composer_provider_selection(
        &entries,
        &[
            Some(other.instance_id.clone()),
            Some(imported.instance_id.clone()),
        ],
        locked.as_ref(),
        Some(&imported.instance_id),
    );
    assert!(selection.selected_provider_entry.is_none());
}

#[test]
fn leaves_a_new_draft_free_to_select_a_different_driver() {
    let original =
        provider::provider_entry("claudeAgent", "claude_work", EntryOverrides::default());
    let selected = provider::provider_entry("codex", "codex_work", EntryOverrides::default());
    let entry_pair = [original.clone(), selected.clone()];
    let lookups = driver_lookups(&entry_pair);
    let fresh = ThreadStartedView {
        latest_run: None,
        item_count: 0,
        runtime: None,
    };
    assert_eq!(
        derive_locked_provider(DeriveLockedProviderInput {
            thread: Some(fresh),
            selected_provider: Some(&selected.instance_id),
            thread_provider: Some(&original.instance_id),
            providers: &lookups,
        }),
        None
    );
}

#[test]
fn uses_the_custom_instance_plan_capability_instead_of_the_default_instance() {
    let default_entry = provider::provider_entry(
        "antigravity",
        "antigravity",
        EntryOverrides {
            show_interaction_mode_toggle: Some(true),
            ..Default::default()
        },
    );
    let custom_entry = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            show_interaction_mode_toggle: Some(false),
            ..Default::default()
        },
    );
    let selection = resolve_composer_provider_selection(
        &[default_entry, custom_entry.clone()],
        &[Some(custom_entry.instance_id.clone())],
        None,
        None,
    );
    assert_eq!(
        selection
            .selected_provider_entry
            .as_ref()
            .map(|e| &e.instance_id),
        Some(&custom_entry.instance_id)
    );
    assert_eq!(
        resolve_composer_interaction_mode(
            true,
            selection
                .selected_provider_entry
                .as_ref()
                .map(|entry| entry.snapshot.show_interaction_mode_toggle),
            turin_contracts::provider_policy::ProviderInteractionMode::Plan,
        ),
        ComposerInteractionMode {
            enabled: false,
            interaction_mode: turin_contracts::provider_policy::ProviderInteractionMode::Default,
        }
    );
}

#[test]
fn uses_the_fallback_provider_plan_capability_after_the_draft_instance_is_disabled() {
    let disabled_entry = provider::provider_entry(
        "antigravity",
        "antigravity",
        EntryOverrides {
            enabled: false,
            show_interaction_mode_toggle: Some(false),
            ..Default::default()
        },
    );
    let fallback_entry = provider::provider_entry("codex", "codex", EntryOverrides::default());
    let selection = resolve_composer_provider_selection(
        &[disabled_entry.clone(), fallback_entry.clone()],
        &[Some(disabled_entry.instance_id.clone())],
        None,
        None,
    );
    assert_eq!(
        selection
            .selected_provider_entry
            .as_ref()
            .map(|e| &e.instance_id),
        Some(&fallback_entry.instance_id)
    );
    assert_eq!(
        resolve_composer_interaction_mode(
            true,
            selection
                .selected_provider_entry
                .as_ref()
                .map(|entry| entry.snapshot.show_interaction_mode_toggle),
            turin_contracts::provider_policy::ProviderInteractionMode::Plan,
        ),
        ComposerInteractionMode {
            enabled: true,
            interaction_mode: turin_contracts::provider_policy::ProviderInteractionMode::Plan,
        }
    );
}

#[test]
fn keeps_a_signed_out_selection_instead_of_silently_switching_providers() {
    let signed_out_entry = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            status: turin_provider_core::server_provider::ServerProviderState::Error,
            auth: turin_provider_core::server_provider::ServerProviderAuth::with_status(
                turin_provider_core::server_provider::ServerProviderAuthStatus::Unauthenticated,
            ),
            models: provider::catalog_models(),
            ..Default::default()
        },
    );
    let selection = resolve_composer_provider_selection(
        &[
            provider::provider_entry("codex", "codex", EntryOverrides::default()),
            signed_out_entry.clone(),
        ],
        &[Some(signed_out_entry.instance_id.clone())],
        None,
        None,
    );
    assert_eq!(
        selection
            .selected_provider_entry
            .as_ref()
            .map(|e| &e.instance_id),
        Some(&signed_out_entry.instance_id)
    );
    assert_eq!(
        get_antigravity_send_block_reason(
            selection
                .selected_provider_entry
                .as_ref()
                .map(|e| &e.snapshot),
            "gemini-pro"
        )
        .as_deref(),
        Some("Sign in to Antigravity in provider settings before sending.")
    );
}

#[test]
fn blocks_sends_until_the_selected_antigravity_profile_is_installed() {
    let provider = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            installed: false,
            models: provider::catalog_models(),
            ..Default::default()
        },
    );
    assert_eq!(
        get_antigravity_send_block_reason(Some(&provider.snapshot), "gemini-pro").as_deref(),
        Some("Install Antigravity in provider settings before sending.")
    );
}

#[test]
fn lets_antigravity_check_saved_credentials_when_resuming_after_a_restart() {
    let provider = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            status: turin_provider_core::server_provider::ServerProviderState::Warning,
            auth: turin_provider_core::server_provider::ServerProviderAuth::with_status(
                turin_provider_core::server_provider::ServerProviderAuthStatus::Unknown,
            ),
            models: Vec::new(),
            ..Default::default()
        },
    );
    assert_eq!(
        get_antigravity_send_block_reason(Some(&provider.snapshot), "gemini-pro"),
        None
    );
    assert_eq!(
        get_antigravity_send_block_reason(Some(&provider.snapshot), ANTIGRAVITY_DEFAULT_MODEL),
        None
    );
    let mut with_models = provider.clone();
    with_models.snapshot.models = provider::catalog_models();
    assert_eq!(
        get_antigravity_send_block_reason(Some(&with_models.snapshot), "gemini-pro"),
        None
    );
    assert_eq!(
        get_antigravity_send_block_reason(Some(&provider.snapshot), "").as_deref(),
        Some("Choose an Antigravity model before sending.")
    );
}

#[test]
fn blocks_saved_model_sends_until_antigravity_loads_its_account_catalog() {
    assert_eq!(
        get_antigravity_send_block_reason(
            Some(
                &provider::provider_entry("antigravity", "antigravity", EntryOverrides::default())
                    .snapshot
            ),
            "gemini-pro"
        )
        .as_deref(),
        Some("Refresh Antigravity models in provider settings before sending.")
    );
}

#[test]
fn blocks_an_empty_antigravity_selection_after_the_catalog_has_loaded() {
    let provider = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            models: provider::catalog_models(),
            ..Default::default()
        },
    );
    assert_eq!(
        get_antigravity_send_block_reason(Some(&provider.snapshot), "").as_deref(),
        Some("Choose an Antigravity model before sending.")
    );
}

#[test]
fn blocks_a_saved_model_that_a_ready_catalog_no_longer_lists() {
    let provider = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            status: turin_provider_core::server_provider::ServerProviderState::Ready,
            models: provider::catalog_models(),
            ..Default::default()
        },
    );
    assert_eq!(
        get_antigravity_send_block_reason(
            Some(&provider.snapshot),
            "saved-model-not-in-current-catalog"
        )
        .as_deref(),
        Some("That Antigravity model is no longer available. Choose another model.")
    );
    assert_eq!(
        get_antigravity_send_block_reason(Some(&provider.snapshot), "gemini-pro"),
        None
    );
}

#[test]
fn allows_a_saved_native_model_to_retry_after_a_provider_error_without_changing_it() {
    let provider = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            status: turin_provider_core::server_provider::ServerProviderState::Error,
            models: provider::catalog_models(),
            ..Default::default()
        },
    );
    assert_eq!(
        get_antigravity_send_block_reason(
            Some(&provider.snapshot),
            "saved-model-not-in-current-catalog"
        ),
        None
    );
}

#[test]
fn keeps_existing_send_behavior_for_other_providers() {
    let provider = provider::provider_entry(
        "codex",
        "codex",
        EntryOverrides {
            installed: false,
            auth: turin_provider_core::server_provider::ServerProviderAuth::with_status(
                turin_provider_core::server_provider::ServerProviderAuthStatus::Unknown,
            ),
            models: Vec::new(),
            ..Default::default()
        },
    );
    assert_eq!(
        get_antigravity_send_block_reason(Some(&provider.snapshot), "gpt-model"),
        None
    );
}

#[test]
fn does_not_continue_an_existing_antigravity_thread_in_another_profile_after_deletion() {
    let missing_instance_id = ProviderInstanceId::new("google_work").unwrap();
    let selection = resolve_composer_provider_selection(
        &[provider::provider_entry(
            "antigravity",
            "antigravity",
            EntryOverrides::default(),
        )],
        &[Some(missing_instance_id.clone())],
        Some(&ProviderDriverKind::new("antigravity").unwrap()),
        Some(&missing_instance_id),
    );
    assert!(selection.selected_provider_entry.is_none());
    assert_eq!(
        selection.unavailable_provider_instance_id,
        Some(missing_instance_id)
    );
}

#[test]
fn does_not_treat_the_empty_draft_placeholder_as_a_provider_setup_target() {
    let no_provider = ProviderInstanceId::new(NO_PROVIDER_INSTANCE_ID).unwrap();
    let selection = resolve_composer_provider_selection(
        &[provider::provider_entry(
            "antigravity",
            "antigravity",
            EntryOverrides {
                enabled: false,
                ..Default::default()
            },
        )],
        &[Some(no_provider)],
        None,
        None,
    );
    assert!(selection.selected_provider_entry.is_none());
    assert!(selection.unavailable_provider_instance_id.is_none());
}

#[test]
fn keeps_the_session_continuation_group_when_another_instance_was_selected() {
    let session_entry = provider::provider_entry(
        "antigravity",
        "google_work",
        EntryOverrides {
            enabled: false,
            continuation_group_key: Some("work-profile"),
            ..Default::default()
        },
    );
    let another_entry = provider::provider_entry(
        "antigravity",
        "google_personal",
        EntryOverrides {
            continuation_group_key: Some("personal-profile"),
            ..Default::default()
        },
    );
    let selection = resolve_composer_provider_selection(
        &[session_entry.clone(), another_entry.clone()],
        &[
            Some(another_entry.instance_id.clone()),
            Some(session_entry.instance_id.clone()),
        ],
        Some(&ProviderDriverKind::new("antigravity").unwrap()),
        Some(&session_entry.instance_id),
    );
    assert!(selection.selected_provider_entry.is_none());
}
