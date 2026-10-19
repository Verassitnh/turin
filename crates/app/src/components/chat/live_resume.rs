//! What the live thread view derives for Resume and per-turn revert, from the
//! thread projection and the provider catalogue (upstream `ChatView`'s
//! `resumableRunId`, `hasHeldQueuedRuns` and `supportsConversationRollback`).

use turin_provider_core::server_provider::ServerProvider;

use crate::composer::view_model::{BannerItem, BannerPriority, BannerVariant};
use crate::state::thread_execution::present_provider_goal;
use crate::ui::IconName;
use crate::state::contracts::{
    OrchestrationV2ProviderFailureClass, OrchestrationV2RunStatus, RunId,
};
use crate::components::chat::timeline::live_projection::active_run;
use crate::state::models::ProviderFailureClass;
use turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole;
use crate::state::support::thread_error::{latest_executed_run, latest_root_provider_failure};
use crate::state::thread_execution::derive_thread_runtime;
use crate::state::thread_projection::ThreadProjection;

/// What the composer's Resume does for the thread right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePlan {
    /// The interrupted (or usage-limited) run a manual continuation picks up.
    pub resumable_run_id: Option<RunId>,
    /// Queued runs the Stop held back; `queue.resume` releases them.
    pub has_held_queued_runs: bool,
}

impl ResumePlan {
    pub fn can_resume(&self) -> bool {
        self.resumable_run_id.is_some() || self.has_held_queued_runs
    }
}

pub fn resume_plan(projection: &ThreadProjection) -> ResumePlan {
    let has_held_queued_runs = projection
        .runs
        .iter()
        .any(|run| run.status == OrchestrationV2RunStatus::Queued && run.queue_held == Some(true));
    let resumable_run_id = latest_executed_run(&projection.runs).and_then(|run| match run.status {
        OrchestrationV2RunStatus::Interrupted => Some(run.id.clone()),
        OrchestrationV2RunStatus::Failed => {
            let runtime_limited = derive_thread_runtime(projection)
                .is_some_and(|runtime| runtime.last_error_class == Some(ProviderFailureClass::UsageLimit));
            let run_limited = latest_root_provider_failure(Some(run), &projection.turn_items)
                .is_some_and(|failure| {
                    failure.class == OrchestrationV2ProviderFailureClass::UsageLimit
                });
            (runtime_limited && run_limited).then(|| run.id.clone())
        }
        _ => None,
    });
    ResumePlan {
        resumable_run_id,
        has_held_queued_runs,
    }
}

/// Whether the thread's provider can rewind conversation history. The
/// provider must be known, and must not say it cannot (Cursor does).
pub fn supports_conversation_rollback(
    providers: &[ServerProvider],
    projection: &ThreadProjection,
) -> bool {
    let thread = &projection.thread;
    let status = providers
        .iter()
        .find(|provider| provider.instance_id == thread.provider_instance_id)
        .or_else(|| {
            providers
                .iter()
                .find(|provider| provider.instance_id == thread.model_selection.instance_id)
        });
    status.is_some_and(|provider| provider.supports_conversation_rollback != Some(false))
}

/// Why "Edit from here" does nothing, or only reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertRefusal {
    /// Only a user message can be rewound to; anything else is ignored.
    NotAUserMessage,
    Blocked(&'static str),
}

/// Checks a revert request (`onRevertToTurnCount`'s guards) and returns the
/// user message's text, which goes back to the composer.
pub fn revert_request(
    projection: &ThreadProjection,
    message_id: &str,
    supports_rollback: bool,
    busy: bool,
) -> Result<String, RevertRefusal> {
    let message = projection
        .messages
        .iter()
        .find(|message| message.id.as_str() == message_id)
        .filter(|message| message.role == OrchestrationV2ConversationMessageRole::User)
        .ok_or(RevertRefusal::NotAUserMessage)?;
    if !supports_rollback {
        return Err(RevertRefusal::Blocked(
            "This provider does not support reverting conversation history. Start a new thread instead.",
        ));
    }
    if busy || active_run(&projection.runs).is_some() {
        return Err(RevertRefusal::Blocked(
            "Interrupt the current turn before reverting checkpoints.",
        ));
    }
    Ok(message.text.clone())
}

/// Id prefix of the native `/goal` banner.
pub const GOAL_BANNER_PREFIX: &str = "goal:";
pub const GOAL_RESUME_ACTION: &str = "resume";
pub const GOAL_CLEAR_ACTION: &str = "clear";

/// The native `/goal` row above the composer: it keeps the agent working
/// across turns, and once the thread is idle offers `/goal resume` (a stopped
/// goal) and `/goal clear`.
pub fn goal_banner(projection: &ThreadProjection, working: bool) -> Option<BannerItem> {
    let active = projection.thread.active_provider_thread_id.as_ref()?;
    let goal = projection
        .provider_threads
        .iter()
        .find(|provider_thread| &provider_thread.id == active)?
        .goal
        .clone()?;
    let presentation = present_provider_goal(&goal, working);
    let mut actions = Vec::new();
    if !working {
        if presentation.can_resume {
            actions.push(("Resume".to_owned(), GOAL_RESUME_ACTION.to_owned()));
        }
        actions.push(("Clear".to_owned(), GOAL_CLEAR_ACTION.to_owned()));
    }
    Some(BannerItem {
        id: format!("{GOAL_BANNER_PREFIX}{}", projection.thread.id),
        variant: BannerVariant::Default,
        priority: Some(BannerPriority::Activity),
        icon: IconName::CircleDot,
        // Usage stays in the title so a long objective cannot clip it.
        title: match &presentation.usage {
            Some(usage) => format!("{} · {usage}", presentation.title),
            None => presentation.title.clone(),
        },
        description: Some(presentation.objective),
        dismissible: false,
        dismiss_label: None,
        actions,
        actions_ghost: true,
        disabled_actions: Vec::new(),
        error: None,
    })
}

/// The standalone command a goal action sends, and the toast title on failure.
pub fn goal_action_command(action_id: &str) -> Option<(&'static str, &'static str)> {
    match action_id {
        GOAL_RESUME_ACTION => Some(("/goal resume", "Failed to resume goal")),
        GOAL_CLEAR_ACTION => Some(("/goal clear", "Failed to clear goal")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{from_json, v2_projection, v2_run};
    use serde_json::json;

    fn projection_with(runs: Vec<serde_json::Value>) -> ThreadProjection {
        let mut projection = v2_projection();
        projection.runs = runs.into_iter().map(from_json).collect();
        projection.into()
    }

    fn run(id: &str, ordinal: u64, status: &str, queue_held: bool) -> serde_json::Value {
        let mut value = serde_json::to_value(v2_run(id, ordinal, status)).expect("run json");
        value["queueHeld"] = json!(queue_held);
        value["startedAt"] = json!("2026-06-20T00:00:00.000Z");
        if status != "queued" {
            value["completedAt"] = json!(format!("2026-06-20T00:00:0{ordinal}.000Z"));
        }
        value
    }

    #[test]
    fn an_interrupted_latest_run_is_resumable() {
        let plan = resume_plan(&projection_with(vec![
            run("run-1", 1, "completed", false),
            run("run-2", 2, "interrupted", false),
        ]));
        assert_eq!(plan.resumable_run_id.as_ref().map(RunId::as_str), Some("run-2"));
        assert!(!plan.has_held_queued_runs);
        assert!(plan.can_resume());
    }

    #[test]
    fn a_completed_latest_run_has_nothing_to_resume() {
        let plan = resume_plan(&projection_with(vec![run("run-1", 1, "completed", false)]));
        assert_eq!(plan, ResumePlan::default());
        assert!(!plan.can_resume());
    }

    #[test]
    fn a_failed_run_without_a_usage_limit_is_not_resumable() {
        let plan = resume_plan(&projection_with(vec![run("run-1", 1, "failed", false)]));
        assert!(!plan.can_resume());
    }

    #[test]
    fn held_queued_runs_alone_can_resume() {
        let plan = resume_plan(&projection_with(vec![
            run("run-1", 1, "completed", false),
            run("run-2", 2, "queued", true),
        ]));
        assert_eq!(plan.resumable_run_id, None);
        assert!(plan.has_held_queued_runs);
        assert!(plan.can_resume());
    }

    #[test]
    fn queued_runs_that_are_not_held_do_not_offer_resume() {
        let plan = resume_plan(&projection_with(vec![
            run("run-1", 1, "running", false),
            run("run-2", 2, "queued", false),
        ]));
        assert!(!plan.can_resume());
    }

    fn message_projection(runs: Vec<serde_json::Value>) -> ThreadProjection {
        let mut projection = v2_projection();
        projection.runs = runs.into_iter().map(from_json).collect();
        projection.messages = vec![
            crate::state::test_fixtures::v2_message("user-1", "Fix the bug"),
            from_json(json!({
                "createdBy": "user", "creationSource": "web", "id": "assistant-1",
                "threadId": "thread-v2", "runId": null, "nodeId": null, "role": "assistant",
                "text": "Done", "attachments": [], "streaming": false,
                "createdAt": "2026-06-20T00:00:00.000Z", "updatedAt": "2026-06-20T00:00:00.000Z",
            })),
        ];
        projection.into()
    }

    #[test]
    fn a_revert_returns_the_user_messages_text() {
        let projection = message_projection(vec![run("run-1", 1, "completed", false)]);
        assert_eq!(
            revert_request(&projection, "user-1", true, false),
            Ok("Fix the bug".to_owned())
        );
    }

    #[test]
    fn a_revert_is_refused_for_other_messages_providers_and_running_turns() {
        let idle = message_projection(vec![run("run-1", 1, "completed", false)]);
        assert_eq!(
            revert_request(&idle, "assistant-1", true, false),
            Err(RevertRefusal::NotAUserMessage)
        );
        assert_eq!(
            revert_request(&idle, "missing", true, false),
            Err(RevertRefusal::NotAUserMessage)
        );
        assert!(matches!(
            revert_request(&idle, "user-1", false, false),
            Err(RevertRefusal::Blocked(message)) if message.contains("does not support reverting")
        ));
        assert!(matches!(
            revert_request(&idle, "user-1", true, true),
            Err(RevertRefusal::Blocked(message)) if message.contains("Interrupt the current turn")
        ));
        let running = message_projection(vec![run("run-1", 1, "running", false)]);
        assert!(matches!(
            revert_request(&running, "user-1", true, false),
            Err(RevertRefusal::Blocked(_))
        ));
    }

    fn provider_thread_json() -> serde_json::Value {
        json!({
            "id": "provider-thread-1",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": null,
            "appThreadId": "thread-v2",
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "active",
            "firstRunOrdinal": null,
            "lastRunOrdinal": null,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": "2026-06-20T00:00:00.000Z",
            "updatedAt": "2026-06-20T00:00:00.000Z",
        })
    }

    fn goal_projection(status: &str) -> ThreadProjection {
        let mut projection = v2_projection();
        let mut provider_thread: crate::state::contracts::OrchestrationV2ProviderThread =
            from_json(provider_thread_json());
        provider_thread.goal = Some(from_json(json!({
            "objective": "Ship it",
            "status": status,
        })));
        projection.thread.active_provider_thread_id = Some(provider_thread.id.clone());
        projection.provider_threads = vec![provider_thread];
        projection.into()
    }

    #[test]
    fn a_paused_goal_offers_resume_and_clear_when_idle() {
        let banner = goal_banner(&goal_projection("paused"), false).expect("banner");
        let labels: Vec<_> = banner.actions.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["Resume", "Clear"]);
        assert_eq!(banner.description.as_deref(), Some("Ship it"));
        assert_eq!(goal_action_command("resume"), Some(("/goal resume", "Failed to resume goal")));
        assert_eq!(goal_action_command("clear"), Some(("/goal clear", "Failed to clear goal")));
    }

    #[test]
    fn an_active_or_working_goal_cannot_resume() {
        let idle = goal_banner(&goal_projection("active"), false).expect("banner");
        let labels: Vec<_> = idle.actions.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["Clear"]);
        let working = goal_banner(&goal_projection("paused"), true).expect("banner");
        assert!(working.actions.is_empty());
        assert!(goal_banner(&v2_projection().into(), false).is_none());
    }

    fn provider(instance_id: &str, rollback: Option<bool>) -> ServerProvider {
        let mut provider: ServerProvider = from_json(json!({
            "instanceId": instance_id,
            "driver": instance_id,
            "enabled": true,
            "installed": true,
            "version": null,
            "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": "2026-06-20T00:00:00.000Z",
            "models": [],
            "slashCommands": [],
            "skills": [],
        }));
        provider.supports_conversation_rollback = rollback;
        provider
    }

    #[test]
    fn rollback_follows_the_threads_provider_capability() {
        let projection: ThreadProjection = v2_projection().into();
        let instance = projection.thread.provider_instance_id.to_string();
        assert!(supports_conversation_rollback(
            &[provider(&instance, None)],
            &projection
        ));
        assert!(supports_conversation_rollback(
            &[provider(&instance, Some(true))],
            &projection
        ));
        assert!(!supports_conversation_rollback(
            &[provider(&instance, Some(false))],
            &projection
        ));
        // An unknown provider cannot be rolled back.
        assert!(!supports_conversation_rollback(&[], &projection));
        assert!(!supports_conversation_rollback(
            &[provider("someone-else", None)],
            &projection
        ));
    }
}
