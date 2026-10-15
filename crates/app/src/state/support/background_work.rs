//! Port of upstream/packages/shared/src/orchestrationV2PendingBackgroundWork.ts

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::thread_pull_requests::thread_pull_request_key_of;
use super::turn_item::{TurnItemExt, TurnItemType, is_turn_item_status_active};
use crate::state::contracts::{
    OrchestrationV2PendingBackgroundTask, OrchestrationV2ProviderThread, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    ProviderThreadId, RunId, ThreadPullRequestLink, ThreadPullRequestLinkSource,
    TrimmedNonEmptyString, js_trim,
};

/// Whether a turn-item update can end background work that a settled run is
/// still waiting on: an item of a background type that is no longer active.
/// Streaming output on a running item, and every other item type, cannot.
pub fn turn_item_update_can_end_background_work(
    item_type: TurnItemType,
    status: OrchestrationV2TurnItemStatus,
) -> bool {
    matches!(
        item_type,
        TurnItemType::CommandExecution | TurnItemType::DynamicTool | TurnItemType::Subagent
    ) && !is_turn_item_status_active(status)
}

/// Whether background work left behind by a completed root run holds back its
/// completion alert (desktop/web notification and the mobile push). Commands,
/// such as dev servers and other long-lived shells, do not: the agent is done
/// and may leave them running for hours. Subagents and monitors do, because
/// they wake the agent and it continues (#13625). Work the adapter cannot name,
/// including kinds this build does not know, holds as the conservative choice.
pub fn background_work_holds_completion(tasks: &[OrchestrationV2PendingBackgroundTask]) -> bool {
    tasks.iter().any(background_work_kind_holds_completion)
}

fn background_work_kind_holds_completion(task: &OrchestrationV2PendingBackgroundTask) -> bool {
    match task {
        OrchestrationV2PendingBackgroundTask::Command { .. } => false,
        OrchestrationV2PendingBackgroundTask::Subagent { .. }
        | OrchestrationV2PendingBackgroundTask::Monitor { .. }
        | OrchestrationV2PendingBackgroundTask::BackgroundTask { .. } => true,
    }
}

const SETTLED_FOR_BACKGROUND_WAIT_RUN_STATUSES: [OrchestrationV2RunStatus; 5] = [
    OrchestrationV2RunStatus::Cancelled,
    OrchestrationV2RunStatus::Completed,
    OrchestrationV2RunStatus::Failed,
    OrchestrationV2RunStatus::Interrupted,
    // A successful turn persists as waiting until checkpoint capture flips it to
    // completed, and waiting is only set from completed. `rolled_back` is
    // excluded: the provider-thread roster is not run-tied, so a rolled-back
    // latest run must fail the gate entirely (not only the item filter).
    OrchestrationV2RunStatus::Waiting,
];

fn is_background_turn_item_type(item_type: TurnItemType) -> bool {
    matches!(
        item_type,
        TurnItemType::CommandExecution | TurnItemType::DynamicTool | TurnItemType::Subagent
    )
}

fn is_persistent_dynamic_tool_input(input: &serde_json::Value) -> bool {
    input
        .as_object()
        .is_some_and(|object| object.get("persistent") == Some(&serde_json::Value::Bool(true)))
}

/// The turn items the pending-work list names, without its settled-run gate.
/// Stop ends exactly these, so the list and Stop cannot disagree.
pub fn pending_background_turn_items<'a>(
    turn_items: &'a [Arc<OrchestrationV2TurnItem>],
    runs: Option<&[Arc<OrchestrationV2Run>]>,
) -> Vec<&'a Arc<OrchestrationV2TurnItem>> {
    let rolled_back_run_ids: HashSet<&RunId> = runs
        .unwrap_or(&[])
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::RolledBack)
        .map(|run| &run.id)
        .collect();
    turn_items
        .iter()
        .filter(|item| {
            is_background_turn_item_type(item.item_type())
                && is_turn_item_status_active(item.status())
                && !matches!(&***item, OrchestrationV2TurnItem::DynamicTool { input, .. }
                    if is_persistent_dynamic_tool_input(input))
                // Null/absent run id stays eligible; only known rolled_back runs drop.
                && item
                    .run_id()
                    .is_none_or(|run_id| !rolled_back_run_ids.contains(run_id))
        })
        .collect()
}

fn description_from_turn_item(item: &OrchestrationV2TurnItem) -> Option<String> {
    let title = match item {
        OrchestrationV2TurnItem::Notification { title, .. }
        | OrchestrationV2TurnItem::UserMessage { title, .. }
        | OrchestrationV2TurnItem::AssistantMessage { title, .. }
        | OrchestrationV2TurnItem::Reasoning { title, .. }
        | OrchestrationV2TurnItem::ProposedPlan { title, .. }
        | OrchestrationV2TurnItem::TodoList { title, .. }
        | OrchestrationV2TurnItem::UserInputRequest { title, .. }
        | OrchestrationV2TurnItem::FileChange { title, .. }
        | OrchestrationV2TurnItem::CommandExecution { title, .. }
        | OrchestrationV2TurnItem::FileSearch { title, .. }
        | OrchestrationV2TurnItem::WebSearch { title, .. }
        | OrchestrationV2TurnItem::ApprovalRequest { title, .. }
        | OrchestrationV2TurnItem::Checkpoint { title, .. }
        | OrchestrationV2TurnItem::RunInterruptRequest { title, .. }
        | OrchestrationV2TurnItem::RunInterruptResult { title, .. }
        | OrchestrationV2TurnItem::SystemNotice { title, .. }
        | OrchestrationV2TurnItem::Error { title, .. }
        | OrchestrationV2TurnItem::Compaction { title, .. }
        | OrchestrationV2TurnItem::Handoff { title, .. }
        | OrchestrationV2TurnItem::Fork { title, .. }
        | OrchestrationV2TurnItem::ThreadCreated { title, .. }
        | OrchestrationV2TurnItem::SecretRequest { title, .. }
        | OrchestrationV2TurnItem::Subagent { title, .. }
        | OrchestrationV2TurnItem::DynamicTool { title, .. } => title,
    };
    if let Some(title) = title {
        let trimmed = js_trim(title);
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    match item {
        OrchestrationV2TurnItem::CommandExecution { input, .. } => {
            let command = js_trim(input);
            (!command.is_empty()).then(|| command.to_owned())
        }
        OrchestrationV2TurnItem::DynamicTool {
            tool_name: Some(tool_name),
            ..
        } => {
            let trimmed = js_trim(tool_name.as_str());
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        OrchestrationV2TurnItem::Subagent { prompt, .. } => {
            let prompt = js_trim(prompt);
            (!prompt.is_empty()).then(|| prompt.to_owned())
        }
        _ => None,
    }
}

fn pending_task_from_turn_item(
    task_id: &str,
    item: &OrchestrationV2TurnItem,
) -> Option<OrchestrationV2PendingBackgroundTask> {
    let task_id = TrimmedNonEmptyString::new(task_id).ok()?;
    let description = description_from_turn_item(item)
        .and_then(|description| TrimmedNonEmptyString::new(description).ok());
    Some(match item {
        OrchestrationV2TurnItem::Subagent {
            child_thread_id, ..
        } => OrchestrationV2PendingBackgroundTask::Subagent {
            task_id,
            description,
            child_thread_id: child_thread_id.clone(),
        },
        OrchestrationV2TurnItem::CommandExecution { .. } => {
            OrchestrationV2PendingBackgroundTask::Command {
                task_id,
                description,
            }
        }
        _ => OrchestrationV2PendingBackgroundTask::BackgroundTask {
            task_id,
            description,
        },
    })
}

fn native_task_id_from_turn_item(item: &OrchestrationV2TurnItem) -> String {
    let native_item_ref = match item {
        OrchestrationV2TurnItem::Notification {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::UserMessage {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::AssistantMessage {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::Reasoning {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::ProposedPlan {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::TodoList {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::UserInputRequest {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::FileChange {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::CommandExecution {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::FileSearch {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::WebSearch {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::ApprovalRequest {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::Checkpoint {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::RunInterruptRequest {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::RunInterruptResult {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::SystemNotice {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::Error {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::Compaction {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::Handoff {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::Fork {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::ThreadCreated {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::SecretRequest {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::Subagent {
            native_item_ref, ..
        }
        | OrchestrationV2TurnItem::DynamicTool {
            native_item_ref, ..
        } => native_item_ref,
    };
    match native_item_ref
        .as_ref()
        .and_then(|native_item_ref| native_item_ref.native_id.as_ref())
    {
        Some(native_id) if !native_id.as_str().is_empty() => native_id.as_str().to_owned(),
        _ => item.id().as_str().to_owned(),
    }
}

pub struct DerivePendingBackgroundWorkInput<'a> {
    pub latest_run: Option<&'a OrchestrationV2Run>,
    pub provider_threads: &'a [Arc<OrchestrationV2ProviderThread>],
    pub turn_items: &'a [Arc<OrchestrationV2TurnItem>],
    pub active_provider_thread_id: Option<&'a ProviderThreadId>,
    pub has_active_run: Option<bool>,
    /// Run rows used to exclude items owned by rolled_back runs. Optional for
    /// callers that already filtered (SQL shell path); in-memory callers should
    /// pass projection runs so policy cannot drift.
    pub runs: Option<&'a [Arc<OrchestrationV2Run>]>,
    pub pull_requests: Option<&'a [ThreadPullRequestLink]>,
}

/// Derive one normalized pending-background-work list for post-settlement UI.
///
/// Sources:
/// - Provider-thread roster (Claude SDK background tasks)
/// - Active command_execution / dynamic_tool / subagent turn items
/// - Pull request watches, as monitors: a watch wakes the agent, so the thread
///   stays working between wakes instead of returning to the inbox. Callers
///   that pick a run to interrupt leave `pullRequests` out; Stop ends watches
///   on its own.
///
/// Gated on latest root run settlement. Dedupes by native task ID. Excludes
/// the roster while any interruptible foreground run remains active. Excludes
/// Grok persistent monitors (`dynamic_tool` input with `persistent: true`).
/// Excludes turn items whose run resolves to `rolled_back` (abandoned work);
/// items with a null or absent run id stay eligible (matches SQL shell path).
/// Does not consult subagent entities (those double-count turn items).
pub fn derive_pending_background_work(
    input: &DerivePendingBackgroundWorkInput<'_>,
) -> Vec<OrchestrationV2PendingBackgroundTask> {
    let has_active_run = input.has_active_run.unwrap_or_else(|| {
        input.runs.is_some_and(|runs| {
            runs.iter().any(|run| {
                matches!(
                    run.status,
                    OrchestrationV2RunStatus::Preparing
                        | OrchestrationV2RunStatus::Starting
                        | OrchestrationV2RunStatus::Running
                )
            })
        })
    });
    if has_active_run {
        return Vec::new();
    }
    // A thread that never ran waits on nothing else, but a watch started on it still wakes it.
    let Some(latest_run) = input.latest_run else {
        return pull_request_watch_tasks(input.pull_requests);
    };
    if !SETTLED_FOR_BACKGROUND_WAIT_RUN_STATUSES.contains(&latest_run.status) {
        return Vec::new();
    }

    let mut by_task_id: IndexMap<String, OrchestrationV2PendingBackgroundTask> = IndexMap::new();

    for provider_thread in input.provider_threads.iter().filter(|thread| {
        input
            .active_provider_thread_id
            .is_none_or(|active| thread.id == *active)
    }) {
        for task in &provider_thread.pending_background_tasks {
            let task_id = task.task_id().as_str();
            if task_id.is_empty() || by_task_id.contains_key(task_id) {
                continue;
            }
            by_task_id.insert(task_id.to_owned(), task.clone());
        }
    }

    for item in pending_background_turn_items(input.turn_items, input.runs) {
        let task_id = native_task_id_from_turn_item(item);
        if by_task_id.contains_key(&task_id) {
            continue;
        }
        if let Some(task) = pending_task_from_turn_item(&task_id, item) {
            by_task_id.insert(task_id, task);
        }
    }

    for task in pull_request_watch_tasks(input.pull_requests) {
        by_task_id.insert(task.task_id().as_str().to_owned(), task);
    }

    by_task_id.into_values().collect()
}

fn pull_request_watch_tasks(
    pull_requests: Option<&[ThreadPullRequestLink]>,
) -> Vec<OrchestrationV2PendingBackgroundTask> {
    pull_requests
        .unwrap_or(&[])
        .iter()
        .filter(|link| {
            link.watch.is_some() && link.source != ThreadPullRequestLinkSource::StackDismissed
        })
        .filter_map(|link| {
            Some(OrchestrationV2PendingBackgroundTask::Monitor {
                task_id: TrimmedNonEmptyString::new(format!(
                    "pull-request-watch:{}",
                    thread_pull_request_key_of(&link.into())
                ))
                .ok()?,
                description: TrimmedNonEmptyString::new(format!(
                    "Watching pull request #{}",
                    link.number
                ))
                .ok(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    #[test]
    fn commands_do_not_hold_completion_but_the_rest_do() {
        let command = OrchestrationV2PendingBackgroundTask::Command {
            task_id: id("a"),
            description: None,
        };
        let monitor = OrchestrationV2PendingBackgroundTask::Monitor {
            task_id: id("b"),
            description: None,
        };
        assert!(!background_work_holds_completion(&[]));
        assert!(!background_work_holds_completion(std::slice::from_ref(
            &command
        )));
        assert!(background_work_holds_completion(&[command, monitor]));
    }

    #[test]
    fn only_settled_background_items_end_background_work() {
        assert!(turn_item_update_can_end_background_work(
            TurnItemType::Subagent,
            OrchestrationV2TurnItemStatus::Completed
        ));
        assert!(!turn_item_update_can_end_background_work(
            TurnItemType::CommandExecution,
            OrchestrationV2TurnItemStatus::Running
        ));
        assert!(!turn_item_update_can_end_background_work(
            TurnItemType::AssistantMessage,
            OrchestrationV2TurnItemStatus::Completed
        ));
    }

    mod derive {
        use super::super::*;
        use crate::state::test_fixtures::{from_json, v2_run};
        use serde_json::{Value, json};

        fn item(kind: &str, id: &str, status: &str, extra: Value) -> Arc<OrchestrationV2TurnItem> {
            let mut value = json!({
                "id": id, "threadId": "thread-v2", "runId": null, "nodeId": null,
                "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
                "parentItemId": null, "ordinal": 1, "status": status, "title": null,
                "startedAt": null, "completedAt": null, "updatedAt": "2026-06-20T00:00:00.000Z",
                "type": kind,
            });
            match kind {
                "command_execution" => value["input"] = json!(""),
                "subagent" => {
                    value["subagentId"] = json!("sub-node");
                    value["origin"] = json!("provider_native");
                    value["driver"] = json!("codex");
                    value["providerInstanceId"] = json!("codex");
                    value["childThreadId"] = json!(null);
                    value["prompt"] = json!("");
                }
                "dynamic_tool" => value["input"] = json!(null),
                _ => {}
            }
            for (key, field) in extra.as_object().unwrap() {
                value[key] = field.clone();
            }
            Arc::new(from_json(value))
        }

        fn native(id: &str) -> Value {
            json!({ "nativeItemRef": { "driver": "codex", "nativeId": id, "strength": "strong" } })
        }

        fn provider_thread(tasks: Value) -> Arc<OrchestrationV2ProviderThread> {
            Arc::new(from_json(json!({
                "id": "pt-1", "driver": "codex", "providerInstanceId": "codex",
                "providerSessionId": null, "appThreadId": "thread-v2", "ownerNodeId": null,
                "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "idle",
                "firstRunOrdinal": null, "lastRunOrdinal": null, "handoffIds": [], "forkedFrom": null,
                "pendingBackgroundTasks": tasks,
                "createdAt": "2026-06-20T00:00:00.000Z", "updatedAt": "2026-06-20T00:00:00.000Z",
            })))
        }

        fn derive(
            status: &str,
            threads: &[Arc<OrchestrationV2ProviderThread>],
            items: &[Arc<OrchestrationV2TurnItem>],
        ) -> Vec<(String, Option<String>, &'static str)> {
            let run = v2_run("run-1", 1, status);
            describe(&derive_pending_background_work(
                &DerivePendingBackgroundWorkInput {
                    latest_run: Some(&run),
                    provider_threads: threads,
                    turn_items: items,
                    active_provider_thread_id: None,
                    has_active_run: None,
                    runs: None,
                    pull_requests: None,
                },
            ))
        }

        fn describe(
            tasks: &[OrchestrationV2PendingBackgroundTask],
        ) -> Vec<(String, Option<String>, &'static str)> {
            tasks
                .iter()
                .map(|task| {
                    (
                        task.task_id().as_str().to_owned(),
                        task.description().map(|d| d.as_str().to_owned()),
                        match task {
                            OrchestrationV2PendingBackgroundTask::Subagent { .. } => "subagent",
                            OrchestrationV2PendingBackgroundTask::Command { .. } => "command",
                            OrchestrationV2PendingBackgroundTask::Monitor { .. } => "monitor",
                            OrchestrationV2PendingBackgroundTask::BackgroundTask { .. } => {
                                "background_task"
                            }
                        },
                    )
                })
                .collect()
        }

        fn row(
            id: &str,
            description: Option<&str>,
            kind: &'static str,
        ) -> (String, Option<String>, &'static str) {
            (id.to_owned(), description.map(str::to_owned), kind)
        }

        #[test]
        fn returns_empty_while_the_latest_run_is_not_settled() {
            let command = item(
                "command_execution",
                "item-1",
                "running",
                json!({ "title": "npm test", "input": "npm test" }),
            );
            assert_eq!(
                derive(
                    "running",
                    &[provider_thread(
                        json!([{ "taskId": "bg-1", "description": "sleep 20", "kind": "command" }])
                    )],
                    &[command]
                ),
                vec![]
            );
        }

        #[test]
        fn does_not_keep_a_settled_parent_waiting_for_an_idle_child() {
            let idle = item(
                "subagent",
                "idle-child",
                "idle",
                json!({ "title": "Review" }),
            );
            assert_eq!(derive("completed", &[], &[idle]), vec![]);
        }

        #[test]
        fn returns_pending_work_when_the_latest_run_is_waiting() {
            let command = item(
                "command_execution",
                "item-1",
                "running",
                json!({ "title": "npm test", "input": "npm test", "nativeItemRef": native("cmd-1")["nativeItemRef"] }),
            );
            assert_eq!(
                derive("waiting", &[provider_thread(json!([]))], &[command]),
                vec![row("cmd-1", Some("npm test"), "command")]
            );
        }

        #[test]
        fn returns_the_provider_thread_roster_after_settlement() {
            let thread = provider_thread(
                json!([{ "taskId": "bg-1", "description": "Run Codex review", "kind": "command" }]),
            );
            assert_eq!(
                derive("completed", &[thread], &[]),
                vec![row("bg-1", Some("Run Codex review"), "command")]
            );
        }

        #[test]
        fn includes_nonterminal_turn_items_and_excludes_completed_ones() {
            let running = item(
                "command_execution",
                "item-1",
                "running",
                json!({ "title": "npm test", "input": "npm test", "nativeItemRef": native("cmd-1")["nativeItemRef"] }),
            );
            let done = item(
                "command_execution",
                "item-2",
                "completed",
                json!({ "title": "done", "input": "echo done", "nativeItemRef": native("cmd-2")["nativeItemRef"] }),
            );
            assert_eq!(
                derive("completed", &[provider_thread(json!([]))], &[running, done]),
                vec![row("cmd-1", Some("npm test"), "command")]
            );
        }

        #[test]
        fn trims_normalized_background_work_descriptions() {
            let thread = provider_thread(
                json!([{ "taskId": "native-task", "description": "native background work", "kind": "background_task" }]),
            );
            let command = item(
                "command_execution",
                "item-command",
                "running",
                json!({ "title": "  npm test  " }),
            );
            let tool = item(
                "dynamic_tool",
                "item-tool",
                "running",
                json!({ "toolName": "browser.search" }),
            );
            assert_eq!(
                derive("completed", &[thread], &[command, tool]),
                vec![
                    row(
                        "native-task",
                        Some("native background work"),
                        "background_task"
                    ),
                    row("item-command", Some("npm test"), "command"),
                    row("item-tool", Some("browser.search"), "background_task"),
                ]
            );
        }

        #[test]
        fn dedupes_roster_entries_against_turn_items_by_native_task_id() {
            let thread = provider_thread(
                json!([{ "taskId": "task-9", "description": "Agent review", "kind": "background_task" }]),
            );
            let sub = item(
                "subagent",
                "item-sub",
                "running",
                json!({ "title": "Agent review", "prompt": "review the plan", "nativeItemRef": native("task-9")["nativeItemRef"] }),
            );
            assert_eq!(
                derive("completed", &[thread], &[sub]),
                vec![row("task-9", Some("Agent review"), "background_task")]
            );
        }

        #[test]
        fn excludes_persistent_monitors() {
            let persistent = item(
                "dynamic_tool",
                "item-1",
                "running",
                json!({ "title": "monitor logs", "input": { "persistent": true, "command": "tail -f" }, "nativeItemRef": native("mon-1")["nativeItemRef"] }),
            );
            let finite = item(
                "dynamic_tool",
                "item-2",
                "running",
                json!({ "title": "finite monitor", "input": { "persistent": false, "command": "sleep 5" }, "nativeItemRef": native("mon-2")["nativeItemRef"] }),
            );
            assert_eq!(
                derive(
                    "completed",
                    &[provider_thread(json!([]))],
                    &[persistent, finite]
                ),
                vec![row("mon-2", Some("finite monitor"), "background_task")]
            );
        }

        #[test]
        fn keeps_stable_ordering_from_insertion() {
            let thread = provider_thread(json!([
                { "taskId": "bg-1", "description": "first", "kind": "command" },
                { "taskId": "bg-2", "description": "second", "kind": "command" },
            ]));
            let third = item(
                "command_execution",
                "item-1",
                "running",
                json!({ "title": "third", "input": "third", "nativeItemRef": native("cmd-3")["nativeItemRef"] }),
            );
            let ids: Vec<String> = derive("completed", &[thread], &[third])
                .into_iter()
                .map(|(id, _, _)| id)
                .collect();
            assert_eq!(ids, vec!["bg-1", "bg-2", "cmd-3"]);
        }

        #[test]
        fn rolled_back_runs_own_no_pending_work() {
            let abandoned = item(
                "command_execution",
                "item-1",
                "running",
                json!({ "title": "abandoned", "runId": "run-1", "input": "sleep 99", "nativeItemRef": native("cmd-1")["nativeItemRef"] }),
            );
            let run = v2_run("run-1", 1, "rolled_back");
            let runs = vec![Arc::new(run.clone())];
            let tasks = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: Some(&run),
                provider_threads: &[provider_thread(json!([]))],
                turn_items: &[abandoned],
                active_provider_thread_id: None,
                has_active_run: None,
                runs: Some(&runs),
                pull_requests: None,
            });
            assert_eq!(tasks, vec![]);
        }

        #[test]
        fn excludes_an_older_rolled_back_item_when_the_latest_run_is_completed() {
            let old = item(
                "command_execution",
                "item-old",
                "running",
                json!({ "title": "from rolled-back run", "runId": "run-1", "input": "sleep 99", "nativeItemRef": native("cmd-old")["nativeItemRef"] }),
            );
            let new = item(
                "command_execution",
                "item-new",
                "running",
                json!({ "title": "still pending", "runId": "run-2", "input": "npm test", "nativeItemRef": native("cmd-new")["nativeItemRef"] }),
            );
            let runs = vec![
                Arc::new(v2_run("run-1", 1, "rolled_back")),
                Arc::new(v2_run("run-2", 2, "completed")),
            ];
            let latest = (*runs[1]).clone();
            let tasks = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: Some(&latest),
                provider_threads: &[provider_thread(json!([]))],
                turn_items: &[old, new],
                active_provider_thread_id: None,
                has_active_run: None,
                runs: Some(&runs),
                pull_requests: None,
            });
            assert_eq!(
                describe(&tasks),
                vec![row("cmd-new", Some("still pending"), "command")]
            );
        }

        #[test]
        fn does_not_reclassify_an_older_active_run_as_background_work() {
            let active = item(
                "command_execution",
                "item-active",
                "running",
                json!({ "title": "foreground work", "runId": "run-1", "input": "vp check", "nativeItemRef": native("cmd-active")["nativeItemRef"] }),
            );
            let runs = vec![
                Arc::new(v2_run("run-1", 1, "running")),
                Arc::new(v2_run("run-2", 2, "cancelled")),
            ];
            let latest = (*runs[1]).clone();
            let tasks = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: Some(&latest),
                provider_threads: &[provider_thread(
                    json!([{ "taskId": "provider-task", "kind": "background_task" }]),
                )],
                turn_items: &[active],
                active_provider_thread_id: None,
                has_active_run: None,
                runs: Some(&runs),
                pull_requests: None,
            });
            assert_eq!(tasks, vec![]);
        }

        #[test]
        fn includes_items_with_a_null_run_id_even_when_runs_list_has_rolled_back_rows() {
            let orphan = item(
                "command_execution",
                "item-null-run",
                "running",
                json!({ "title": "orphan item", "input": "echo orphan", "nativeItemRef": native("cmd-null")["nativeItemRef"] }),
            );
            let runs = vec![Arc::new(v2_run("run-1", 1, "rolled_back"))];
            let latest = v2_run("run-1", 1, "completed");
            let tasks = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: Some(&latest),
                provider_threads: &[provider_thread(json!([]))],
                turn_items: &[orphan],
                active_provider_thread_id: None,
                has_active_run: None,
                runs: Some(&runs),
                pull_requests: None,
            });
            assert_eq!(
                describe(&tasks),
                vec![row("cmd-null", Some("orphan item"), "command")]
            );
        }

        #[test]
        fn keeps_the_rosters_kinds_and_names_turn_items_by_their_type() {
            let thread = provider_thread(json!([
                { "taskId": "bash", "kind": "command" },
                { "taskId": "watch", "kind": "monitor" },
                { "taskId": "workflow", "kind": "background_task" },
            ]));
            let sub = item(
                "subagent",
                "item-sub",
                "running",
                json!({ "title": "Review src/math.ts", "childThreadId": "thread:child", "nativeItemRef": native("sub")["nativeItemRef"] }),
            );
            let cmd = item(
                "command_execution",
                "item-cmd",
                "running",
                json!({ "input": "npm test", "nativeItemRef": native("cmd")["nativeItemRef"] }),
            );
            let run = v2_run("run-1", 1, "completed");
            let tasks = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: Some(&run),
                provider_threads: &[thread],
                turn_items: &[sub, cmd],
                active_provider_thread_id: None,
                has_active_run: None,
                runs: None,
                pull_requests: None,
            });
            assert_eq!(
                describe(&tasks),
                vec![
                    row("bash", None, "command"),
                    row("watch", None, "monitor"),
                    row("workflow", None, "background_task"),
                    row("sub", Some("Review src/math.ts"), "subagent"),
                    row("cmd", Some("npm test"), "command"),
                ]
            );
            assert!(matches!(
                &tasks[3],
                OrchestrationV2PendingBackgroundTask::Subagent { child_thread_id: Some(id), .. } if id.as_str() == "thread:child"
            ));
        }

        fn link(number: u64, source: &str, watched: bool) -> ThreadPullRequestLink {
            let mut value = json!({
                "host": "github.com", "repository": "acme/app", "number": number,
                "url": format!("https://github.com/acme/app/pull/{number}"),
                "source": source, "linkedAt": "2026-10-05T00:00:00.000Z", "snapshot": null, "stack": null,
            });
            if watched {
                value["watch"] = json!({
                    "startedAt": "2026-10-05T00:00:00.000Z", "headSha": null, "failedChecks": [],
                    "passed": false, "passedChecks": [], "remarksThrough": "2026-10-05T00:00:00.000Z",
                    "remarkIds": [], "conflicting": false, "wakes": 0,
                });
            }
            from_json(value)
        }

        #[test]
        fn pull_request_watches_hold_a_settled_run_as_monitors() {
            let pull_requests = vec![
                link(1, "agent", true),
                link(2, "agent", false),
                link(3, "stack-dismissed", true),
            ];
            let run = v2_run("run-1", 1, "completed");
            let tasks = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: Some(&run),
                provider_threads: &[],
                turn_items: &[],
                active_provider_thread_id: None,
                has_active_run: None,
                runs: None,
                pull_requests: Some(&pull_requests),
            });
            assert_eq!(
                describe(&tasks),
                vec![row(
                    "pull-request-watch:github.com/acme/app#1",
                    Some("Watching pull request #1"),
                    "monitor"
                )]
            );
            assert!(background_work_holds_completion(&tasks));

            // A thread that never ran waits on its watch too.
            let never_ran = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: None,
                provider_threads: &[],
                turn_items: &[],
                active_provider_thread_id: None,
                has_active_run: None,
                runs: None,
                pull_requests: Some(&pull_requests),
            });
            assert_eq!(never_ran.len(), 1);

            // No watch is listed while a run is active.
            let running = v2_run("run-1", 1, "running");
            let active = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                latest_run: Some(&running),
                provider_threads: &[],
                turn_items: &[],
                active_provider_thread_id: None,
                has_active_run: Some(true),
                runs: None,
                pull_requests: Some(&pull_requests),
            });
            assert_eq!(active, vec![]);
        }
    }
}
