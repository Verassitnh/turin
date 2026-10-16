//! Live chat column wiring: projection → subagent bar, overlay banners, composer notices.

use std::rc::Rc;

use gpui_kit::{App, Entity, SharedString, Window};
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::ProviderFailureClass;

use super::chat_overlay_banners::{ProviderStatusBanner, ThreadErrorBanner};
use super::usage_limit_recovery_banner::{
    RecoveryActionsState, UsageLimitRecoveryProps, usage_limit_recovery_banner_item,
};
use crate::app_runtime::write::bus::CommandBus;
use crate::composer::live_catalog::provider_entries;
use crate::composer::model_display::get_trigger_display_model_name;
use crate::composer::view_model::BannerItem;
use crate::components::chat_view::SubagentBarData;
use crate::state::client_state::ClientState;
use crate::state::models::scoped_thread_key;
use crate::state::store::ClientStore;
use crate::state::thread_execution::{
    derive_provider_subagent_status, derive_thread_runtime,
};

use super::provider_status_banner::{
    get_provider_status_banner_key, should_show_provider_status_banner,
};
use super::thread_error_banner::{
    dismiss_thread_error_banner_for_session, get_thread_error_banner_key,
    is_thread_error_banner_dismissed_for_session, should_show_thread_error_banner,
};

/// Provider-native subagent threads replace the composer with [`ProviderSubagentBar`].
pub fn subagent_bar_from_state(
    state: &ClientState,
    bus: &CommandBus,
    thread_id: &ThreadId,
) -> Option<SubagentBarData> {
    let entry = state.thread(thread_id)?;
    let projection = entry.state.data.as_ref()?;
    if !crate::state::contracts::is_provider_native_subagent_thread(&projection.thread) {
        return None;
    }
    let providers = provider_entries(&bus.providers().borrow());
    let instance_id = projection.thread.model_selection.instance_id.as_str();
    let model_slug = projection.thread.model_selection.model.as_str();
    let provider = providers
        .iter()
        .find(|entry| entry.instance_id == instance_id)
        .cloned();
    let model_label = provider
        .as_ref()
        .and_then(|entry| {
            entry
                .models
                .iter()
                .find(|model| model.slug == model_slug)
                .map(get_trigger_display_model_name)
        })
        .unwrap_or_else(|| model_slug.to_owned());
    let parent_thread_id = projection
        .thread
        .lineage
        .parent_thread_id
        .as_ref()
        .map(|id| id.as_str().to_owned());
    Some(SubagentBarData {
        provider,
        model_label: SharedString::from(model_label),
        effort_label: None,
        status: derive_provider_subagent_status(projection),
        parent_thread_id,
    })
}

pub fn usage_limit_recovery_props(
    state: &ClientState,
    thread_id: &ThreadId,
) -> Option<UsageLimitRecoveryProps> {
    let entry = state.thread(thread_id)?;
    let projection = entry.state.data.as_ref()?;
    let runtime = derive_thread_runtime(projection)?;
    if runtime.status != crate::state::models::RunStatus::Failed {
        return None;
    }
    if runtime.last_error_class != Some(ProviderFailureClass::UsageLimit) {
        return None;
    }
    let shell = state.shell_view.as_ref()?.thread(thread_id)?;
    let latest_run = shell.latest_run.as_ref()?;
    Some(UsageLimitRecoveryProps {
        run_id: latest_run.run_id.to_string(),
        reset_at: runtime.usage_limit_reset_at.clone(),
        stopped_at: latest_run
            .completed_at
            .clone()
            .unwrap_or_else(|| shell.updated_at.clone()),
        snoozed_until: shell.snoozed_until.clone(),
        recovery: shell.limit_recovery.clone().flatten(),
    })
}

pub fn usage_limit_recovery_banner(state: &ClientState, thread_id: &ThreadId) -> Option<BannerItem> {
    let props = usage_limit_recovery_props(state, thread_id)?;
    Some(usage_limit_recovery_banner_item(
        &props,
        &RecoveryActionsState {
            now_ms: chrono::Utc::now().timestamp_millis(),
            ..Default::default()
        },
    ))
}

/// Overlay banners above the timeline (provider advisory, thread runtime error).
pub fn live_overlay_banners(
    client: &Entity<ClientStore>,
    bus: &CommandBus,
    thread_id: &ThreadId,
    window: &mut Window,
    cx: &mut App,
) -> Vec<gpui_kit::AnyElement> {
    let state = client.read(cx).state().clone();
    let Some(entry) = state.thread(thread_id) else {
        return Vec::new();
    };
    let environment_id = crate::state::local_environment_id();
    let thread_key = scoped_thread_key(&environment_id, thread_id);
    let mut banners = Vec::new();

    if let Some(projection) = entry.state.data.as_ref() {
        let providers = provider_entries(&bus.providers().borrow());
        let instance_id = projection.thread.model_selection.instance_id.as_str();
        if let Some(provider) = providers.iter().find(|p| p.instance_id == instance_id) {
            let dismissed = window.use_keyed_state(
                format!("provider-status-dismissed-{thread_key}"),
                cx,
                |_, _| None::<String>,
            );
            if should_show_provider_status_banner(provider, dismissed.read(cx).as_deref()) {
                let key = get_provider_status_banner_key(provider);
                let provider = provider.clone();
                banners.push(
                    ProviderStatusBanner::new(provider)
                        .on_dismiss(Rc::new(move |_, cx| {
                            dismissed.update(cx, |dismissed, cx| {
                                *dismissed = key.clone();
                                cx.notify();
                            });
                        }))
                        .into_any_element(),
                );
            }
        }

        if let Some(runtime) = derive_thread_runtime(projection) {
            let error = runtime.last_error.as_deref();
            let usage_limit = runtime.last_error_class == Some(ProviderFailureClass::UsageLimit);
            let tick = window.use_keyed_state(
                format!("thread-error-dismissed-{thread_key}"),
                cx,
                |_, _| 0_u32,
            );
            let _ = tick.read(cx);
            let key = get_thread_error_banner_key(&thread_key, error);
            let dismissed = is_thread_error_banner_dismissed_for_session(key.as_deref());
            if should_show_thread_error_banner(&thread_key, error, dismissed) {
                let message = error.unwrap_or("Something went wrong");
                banners.push(
                    ThreadErrorBanner::new(message, usage_limit)
                        .on_dismiss(Rc::new(move |_, cx| {
                            dismiss_thread_error_banner_for_session(key.as_deref());
                            tick.update(cx, |tick, cx| {
                                *tick += 1;
                                cx.notify();
                            });
                        }))
                        .into_any_element(),
                );
            }
        }
    }

    banners
}
