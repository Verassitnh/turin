//! Port of upstream/packages/shared/src/orchestrationV2PendingBackgroundWork.ts
//!
//! Lives here until `turin-shared` (T05b) carries its contract-dependent files.

use std::collections::HashSet;

use indexmap::IndexMap;
use serde_json::Value;
use turin_contracts::orchestration_v2::{
    OrchestrationV2PendingBackgroundTask, OrchestrationV2RunStatus, OrchestrationV2TurnItem,
    OrchestrationV2TurnItemStatus,
};
use turin_contracts::thread_pull_request::{ThreadPullRequestLink, ThreadPullRequestLinkSource};
use turin_contracts::{ThreadId, TrimmedNonEmptyString, js_trim};

use super::thread_pull_requests::thread_pull_request_key_of;
use crate::projection::turn_item;

/// `BACKGROUND_TURN_ITEM_TYPES`.
const BACKGROUND_TURN_ITEM_TYPES: [&str; 3] = ["command_execution", "dynamic_tool", "subagent"];

fn is_background_type(type_tag: &str) -> bool {
    BACKGROUND_TURN_ITEM_TYPES.contains(&type_tag)
}

/// `isOrchestrationV2WorkActive`, applied to a turn item's status.
fn is_work_active(status: OrchestrationV2TurnItemStatus) -> bool {
    matches!(
        status,
        OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting
    )
}

/// Run statuses that allow a pending-background roster to surface.
/// Includes `waiting`: a successful turn persists as waiting until checkpoint
/// capture flips it to completed, and waiting is only set from completed.
/// Excludes `rolled_back`: the provider-thread roster is not run-tied, so a
/// rolled-back latest run must fail the gate entirely (not only item filter).
/// (Upstream also keeps the true-terminal set, including `rolled_back`, as an
/// unused constant; it is omitted here.)
fn is_settled_for_background_wait(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Cancelled
            | OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Interrupted
            | OrchestrationV2RunStatus::Waiting
    )
}

/// Whether a turn-item update can end background work that a settled run is
/// still waiting on: an item of a background type that is no longer active.
/// Streaming output on a running item, and every other item type, cannot.
pub fn turn_item_update_can_end_background_work(
    type_tag: &str,
    status: OrchestrationV2TurnItemStatus,
) -> bool {
    is_background_type(type_tag) && !is_work_active(status)
}

/// Whether background work left behind by a completed root run holds back its
/// completion alert (desktop notification). Commands, such as dev servers and
/// other long-lived shells, do not: the agent is done and may leave them
/// running for hours. Subagents and monitors do, because they wake the agent
/// and it continues (#13625). Work the adapter cannot name, including kinds
/// this build does not know, holds as the conservative choice.
pub fn background_work_holds_completion(tasks: &[OrchestrationV2PendingBackgroundTask]) -> bool {
    tasks
        .iter()
        .any(|task| !matches!(task, OrchestrationV2PendingBackgroundTask::Command { .. }))
}

/// `Pick<OrchestrationV2Run, "id" | "ordinal" | "status">`.
#[derive(Debug, Clone, Copy)]
pub struct PendingBackgroundWorkRun<'a> {
    pub id: &'a str,
    pub ordinal: u64,
    pub status: OrchestrationV2RunStatus,
}

/// `Pick<OrchestrationV2ProviderThread, "id" | "pendingBackgroundTasks">`.
#[derive(Debug, Clone, Copy)]
pub struct PendingBackgroundWorkProviderThread<'a> {
    pub id: &'a str,
    pub pending_background_tasks: &'a [OrchestrationV2PendingBackgroundTask],
}

/// A turn item's `input`: a command line, or a dynamic tool's JSON arguments.
#[derive(Debug, Clone, Copy)]
pub enum PendingBackgroundWorkInput<'a> {
    Text(&'a str),
    Json(&'a Value),
}

/// The slice of a turn item the pending-work list reads
/// (`PendingBackgroundWorkTurnItem`).
#[derive(Debug, Clone, Copy)]
pub struct PendingBackgroundWorkItem<'a> {
    pub id: &'a str,
    pub type_tag: &'a str,
    pub status: OrchestrationV2TurnItemStatus,
    pub title: Option<&'a str>,
    /// When present and the run is rolled_back, the item is abandoned, not pending.
    pub run_id: Option<&'a str>,
    pub native_id: Option<&'a str>,
    pub input: Option<PendingBackgroundWorkInput<'a>>,
    pub tool_name: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub child_thread_id: Option<&'a ThreadId>,
}

impl<'a> PendingBackgroundWorkItem<'a> {
    pub fn from_turn_item(item: &'a OrchestrationV2TurnItem) -> Self {
        let common = turn_item::common(item);
        let mut view = Self {
            id: common.id.as_str(),
            type_tag: turn_item::type_tag(item),
            status: common.status,
            title: common.title,
            run_id: common.run_id.map(|id| id.as_str()),
            native_id: common
                .native_item_ref
                .and_then(|reference| reference.native_id.as_ref())
                .map(TrimmedNonEmptyString::as_str),
            input: None,
            tool_name: None,
            prompt: None,
            child_thread_id: None,
        };
        match item {
            OrchestrationV2TurnItem::CommandExecution { input, .. } => {
                view.input = Some(PendingBackgroundWorkInput::Text(input));
            }
            OrchestrationV2TurnItem::DynamicTool {
                input, tool_name, ..
            } => {
                view.input = Some(PendingBackgroundWorkInput::Json(input));
                view.tool_name = tool_name.as_ref().map(TrimmedNonEmptyString::as_str);
            }
            OrchestrationV2TurnItem::Subagent {
                prompt,
                child_thread_id,
                ..
            } => {
                view.prompt = Some(prompt);
                view.child_thread_id = child_thread_id.as_ref();
            }
            _ => {}
        }
        view
    }
}

fn is_persistent_dynamic_tool_input(input: Option<PendingBackgroundWorkInput<'_>>) -> bool {
    match input {
        Some(PendingBackgroundWorkInput::Json(Value::Object(map))) => {
            map.get("persistent") == Some(&Value::Bool(true))
        }
        _ => false,
    }
}

fn non_blank(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn description_from_turn_item(item: &PendingBackgroundWorkItem<'_>) -> Option<String> {
    if let Some(title) = item.title
        && !js_trim(title).is_empty()
    {
        return Some(js_trim(title).to_owned());
    }
    if item.type_tag == "command_execution"
        && let Some(PendingBackgroundWorkInput::Text(command)) = item.input
    {
        let command = js_trim(command);
        return (!command.is_empty()).then(|| command.to_owned());
    }
    if item.type_tag == "dynamic_tool"
        && let Some(tool_name) = item.tool_name
        && !js_trim(tool_name).is_empty()
    {
        return Some(js_trim(tool_name).to_owned());
    }
    if item.type_tag == "subagent"
        && let Some(prompt) = item.prompt
    {
        let prompt = js_trim(prompt);
        return (!prompt.is_empty()).then(|| prompt.to_owned());
    }
    None
}

fn pending_task_from_turn_item(
    task_id: &str,
    item: &PendingBackgroundWorkItem<'_>,
) -> Option<OrchestrationV2PendingBackgroundTask> {
    let task_id = non_blank(task_id)?;
    let description = description_from_turn_item(item).and_then(|text| non_blank(&text));
    Some(match item.type_tag {
        "subagent" => OrchestrationV2PendingBackgroundTask::Subagent {
            task_id,
            description,
            child_thread_id: item.child_thread_id.cloned(),
        },
        "command_execution" => OrchestrationV2PendingBackgroundTask::Command {
            task_id,
            description,
        },
        _ => OrchestrationV2PendingBackgroundTask::BackgroundTask {
            task_id,
            description,
        },
    })
}

fn native_task_id_from_turn_item(item: &PendingBackgroundWorkItem<'_>) -> String {
    match item.native_id {
        Some(native_id) if !native_id.is_empty() => native_id.to_owned(),
        _ => item.id.to_owned(),
    }
}

/// The turn items the pending-work list names, without its settled-run gate.
/// Stop ends exactly these, so the list and Stop cannot disagree.
pub fn pending_background_turn_items<'i, 'a>(
    turn_items: &'i [PendingBackgroundWorkItem<'a>],
    runs: Option<&[PendingBackgroundWorkRun<'_>]>,
) -> Vec<&'i PendingBackgroundWorkItem<'a>> {
    let rolled_back_run_ids = runs
        .unwrap_or_default()
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::RolledBack)
        .map(|run| run.id)
        .collect::<HashSet<_>>();
    turn_items
        .iter()
        .filter(|item| {
            is_background_type(item.type_tag)
                && is_work_active(item.status)
                && !(item.type_tag == "dynamic_tool" && is_persistent_dynamic_tool_input(item.input))
                // Null/absent run id stays eligible; only known rolled_back runs drop.
                && item
                    .run_id
                    .is_none_or(|run_id| !rolled_back_run_ids.contains(run_id))
        })
        .collect()
}

/// `derivePendingBackgroundWork` input.
#[derive(Debug, Clone, Default)]
pub struct DerivePendingBackgroundWorkInput<'a> {
    pub latest_run: Option<PendingBackgroundWorkRun<'a>>,
    pub provider_threads: Vec<PendingBackgroundWorkProviderThread<'a>>,
    pub turn_items: Vec<PendingBackgroundWorkItem<'a>>,
    pub active_provider_thread_id: Option<&'a str>,
    pub has_active_run: Option<bool>,
    /// Run rows used to exclude items owned by rolled_back runs. Optional for
    /// callers that already filtered (SQL shell path); in-memory callers should
    /// pass projection runs so policy cannot drift.
    pub runs: Option<Vec<PendingBackgroundWorkRun<'a>>>,
    pub pull_requests: Option<&'a [ThreadPullRequestLink]>,
}

/// Derive one normalized pending-background-work list for post-settlement UI.
///
/// Sources:
/// - Provider-thread roster (Claude SDK background tasks)
/// - Active command_execution / dynamic_tool / subagent turn items
/// - Pull request watches, as monitors: a watch wakes the agent, so the thread
///   stays working between wakes instead of returning to the inbox. Callers
///   that pick a run to interrupt leave `pull_requests` out; Stop ends watches
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
        input.runs.as_ref().is_some_and(|runs| {
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
    if !is_settled_for_background_wait(latest_run.status) {
        return Vec::new();
    }

    let mut by_task_id: IndexMap<String, OrchestrationV2PendingBackgroundTask> = IndexMap::new();

    for provider_thread in &input.provider_threads {
        if let Some(active) = input.active_provider_thread_id
            && provider_thread.id != active
        {
            continue;
        }
        for task in provider_thread.pending_background_tasks {
            let task_id = task.task_id().as_str();
            if task_id.is_empty() || by_task_id.contains_key(task_id) {
                continue;
            }
            // `description?.trim()`, dropped when blank.
            let trimmed = trimmed_description(task);
            by_task_id.insert(task_id.to_owned(), with_description(task, trimmed));
        }
    }

    for item in pending_background_turn_items(&input.turn_items, input.runs.as_deref()) {
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

fn trimmed_description(
    task: &OrchestrationV2PendingBackgroundTask,
) -> Option<TrimmedNonEmptyString> {
    task.description().and_then(|text| non_blank(text.as_str()))
}

fn with_description(
    task: &OrchestrationV2PendingBackgroundTask,
    description: Option<TrimmedNonEmptyString>,
) -> OrchestrationV2PendingBackgroundTask {
    match task.clone() {
        OrchestrationV2PendingBackgroundTask::Subagent {
            task_id,
            child_thread_id,
            ..
        } => OrchestrationV2PendingBackgroundTask::Subagent {
            task_id,
            description,
            child_thread_id,
        },
        OrchestrationV2PendingBackgroundTask::Command { task_id, .. } => {
            OrchestrationV2PendingBackgroundTask::Command {
                task_id,
                description,
            }
        }
        OrchestrationV2PendingBackgroundTask::Monitor { task_id, .. } => {
            OrchestrationV2PendingBackgroundTask::Monitor {
                task_id,
                description,
            }
        }
        OrchestrationV2PendingBackgroundTask::BackgroundTask { task_id, .. } => {
            OrchestrationV2PendingBackgroundTask::BackgroundTask {
                task_id,
                description,
            }
        }
    }
}

fn pull_request_watch_tasks(
    pull_requests: Option<&[ThreadPullRequestLink]>,
) -> Vec<OrchestrationV2PendingBackgroundTask> {
    pull_requests
        .unwrap_or_default()
        .iter()
        .filter(|link| {
            link.watch.is_some() && link.source != ThreadPullRequestLinkSource::StackDismissed
        })
        .filter_map(|link| {
            Some(OrchestrationV2PendingBackgroundTask::Monitor {
                task_id: non_blank(&format!(
                    "pull-request-watch:{}",
                    thread_pull_request_key_of(link)
                ))?,
                description: non_blank(&format!("Watching pull request #{}", link.number)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(value: Value) -> OrchestrationV2PendingBackgroundTask {
        serde_json::from_value(value).unwrap()
    }

    fn run(
        id: &'static str,
        ordinal: u64,
        status: OrchestrationV2RunStatus,
    ) -> PendingBackgroundWorkRun<'static> {
        PendingBackgroundWorkRun {
            id,
            ordinal,
            status,
        }
    }

    fn item(
        id: &'static str,
        type_tag: &'static str,
        status: OrchestrationV2TurnItemStatus,
    ) -> PendingBackgroundWorkItem<'static> {
        PendingBackgroundWorkItem {
            id,
            type_tag,
            status,
            title: None,
            run_id: None,
            native_id: None,
            input: None,
            tool_name: None,
            prompt: None,
            child_thread_id: None,
        }
    }

    use OrchestrationV2RunStatus as Run;
    use OrchestrationV2TurnItemStatus as Status;

    fn command(
        id: &'static str,
        title: Option<&'static str>,
        native: Option<&'static str>,
        input: &'static str,
    ) -> PendingBackgroundWorkItem<'static> {
        PendingBackgroundWorkItem {
            title,
            native_id: native,
            input: Some(PendingBackgroundWorkInput::Text(input)),
            ..item(id, "command_execution", Status::Running)
        }
    }

    fn derive(
        input: DerivePendingBackgroundWorkInput<'_>,
    ) -> Vec<OrchestrationV2PendingBackgroundTask> {
        derive_pending_background_work(&input)
    }

    fn json_tasks(tasks: &[OrchestrationV2PendingBackgroundTask]) -> Value {
        serde_json::to_value(tasks).unwrap()
    }

    #[test]
    fn turn_item_update_can_end_background_work_cases() {
        for (case, can_end, type_tag, status) in [
            ("a finished subagent", true, "subagent", Status::Completed),
            (
                "a failed background command",
                true,
                "command_execution",
                Status::Failed,
            ),
            ("an idle dynamic tool", true, "dynamic_tool", Status::Idle),
            (
                "a running command's output",
                false,
                "command_execution",
                Status::Running,
            ),
            ("a waiting subagent", false, "subagent", Status::Waiting),
            (
                "a finished assistant message",
                false,
                "assistant_message",
                Status::Completed,
            ),
        ] {
            assert_eq!(
                turn_item_update_can_end_background_work(type_tag, status),
                can_end,
                "{case}"
            );
        }
    }

    #[test]
    fn background_work_holds_completion_cases() {
        let t = |id: &str, kind: &str| task(json!({"taskId": id, "kind": kind}));
        for (case, holds, tasks) in [
            ("nothing pending", false, vec![]),
            ("a dev server", false, vec![t("dev", "command")]),
            (
                "two long-lived shells",
                false,
                vec![t("web", "command"), t("api", "command")],
            ),
            ("a subagent", true, vec![t("review", "subagent")]),
            ("a monitor", true, vec![t("watch", "monitor")]),
            (
                "a command and a monitor",
                true,
                vec![t("dev", "command"), t("watch", "monitor")],
            ),
            (
                "a command and a subagent",
                true,
                vec![t("dev", "command"), t("review", "subagent")],
            ),
            (
                "work the provider cannot name",
                true,
                vec![t("opaque", "background_task")],
            ),
        ] {
            assert_eq!(background_work_holds_completion(&tasks), holds, "{case}");
        }
    }

    #[test]
    fn returns_empty_while_the_latest_run_is_not_settled() {
        let roster = [task(
            json!({"taskId": "bg-1", "description": "sleep 20", "kind": "command"}),
        )];
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Running)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            turn_items: vec![command("item-1", Some("npm test"), None, "npm test")],
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn does_not_keep_a_settled_parent_waiting_for_an_idle_child() {
        let mut idle = item("idle-child", "subagent", Status::Idle);
        idle.title = Some("Review");
        assert!(
            derive(DerivePendingBackgroundWorkInput {
                latest_run: Some(run("run-1", 1, Run::Completed)),
                turn_items: vec![idle],
                ..Default::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn returns_pending_work_when_the_latest_run_is_waiting() {
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Waiting)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &[],
            }],
            turn_items: vec![command(
                "item-1",
                Some("npm test"),
                Some("cmd-1"),
                "npm test",
            )],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "cmd-1", "description": "npm test", "kind": "command"}])
        );
    }

    #[test]
    fn still_returns_empty_when_the_latest_run_is_running_even_with_background_items() {
        let roster = [task(
            json!({"taskId": "bg-1", "description": "sleep 20", "kind": "command"}),
        )];
        let mut subagent = item("item-1", "subagent", Status::Running);
        subagent.title = Some("review");
        subagent.native_id = Some("sub-1");
        subagent.prompt = Some("review");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Running)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            turn_items: vec![subagent],
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn excludes_rolled_back_items_when_the_latest_run_is_waiting() {
        let mut old = command(
            "item-old",
            Some("from rolled-back run"),
            Some("cmd-old"),
            "sleep 99",
        );
        old.run_id = Some("run-1");
        let mut new = command(
            "item-new",
            Some("still pending"),
            Some("cmd-new"),
            "npm test",
        );
        new.run_id = Some("run-2");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-2", 2, Run::Waiting)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &[],
            }],
            runs: Some(vec![
                run("run-1", 1, Run::RolledBack),
                run("run-2", 2, Run::Waiting),
            ]),
            turn_items: vec![old, new],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "cmd-new", "description": "still pending", "kind": "command"}])
        );
    }

    #[test]
    fn returns_the_provider_thread_roster_after_settlement() {
        let roster = [task(
            json!({"taskId": "bg-1", "description": "Run Codex review", "kind": "command"}),
        )];
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            active_provider_thread_id: Some("pt-1"),
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "bg-1", "description": "Run Codex review", "kind": "command"}])
        );
    }

    #[test]
    fn roster_of_a_non_active_provider_thread_is_ignored() {
        let roster = [task(json!({"taskId": "bg-1", "kind": "command"}))];
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-old",
                pending_background_tasks: &roster,
            }],
            active_provider_thread_id: Some("pt-1"),
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn includes_nonterminal_turn_items_and_excludes_completed_ones() {
        let mut done = command("item-2", Some("done"), Some("cmd-2"), "echo done");
        done.status = Status::Completed;
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &[],
            }],
            turn_items: vec![
                command("item-1", Some("npm test"), Some("cmd-1"), "npm test"),
                done,
            ],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "cmd-1", "description": "npm test", "kind": "command"}])
        );
    }

    #[test]
    fn trims_normalized_background_work_descriptions() {
        let roster = [task(
            json!({"taskId": "native-task", "description": "  native background work  ", "kind": "background_task"}),
        )];
        let mut tool = item("item-tool", "dynamic_tool", Status::Running);
        tool.tool_name = Some("  browser.search  ");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            turn_items: vec![
                PendingBackgroundWorkItem {
                    title: Some("  npm test  "),
                    ..item("item-command", "command_execution", Status::Running)
                },
                tool,
            ],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([
                {"taskId": "native-task", "description": "native background work", "kind": "background_task"},
                {"taskId": "item-command", "description": "npm test", "kind": "command"},
                {"taskId": "item-tool", "description": "browser.search", "kind": "background_task"},
            ])
        );
    }

    #[test]
    fn dedupes_roster_entries_against_turn_items_by_native_task_id() {
        let roster = [task(
            json!({"taskId": "task-9", "description": "Agent review", "kind": "background_task"}),
        )];
        let mut sub = item("item-sub", "subagent", Status::Running);
        sub.title = Some("Agent review");
        sub.native_id = Some("task-9");
        sub.prompt = Some("review the plan");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            turn_items: vec![sub],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "task-9", "description": "Agent review", "kind": "background_task"}])
        );
    }

    #[test]
    fn excludes_grok_persistent_monitors() {
        let persistent = json!({"persistent": true, "command": "tail -f"});
        let finite = json!({"persistent": false, "command": "sleep 5"});
        let mut first = item("item-1", "dynamic_tool", Status::Running);
        first.title = Some("monitor logs");
        first.native_id = Some("mon-1");
        first.input = Some(PendingBackgroundWorkInput::Json(&persistent));
        let mut second = item("item-2", "dynamic_tool", Status::Running);
        second.title = Some("finite monitor");
        second.native_id = Some("mon-2");
        second.input = Some(PendingBackgroundWorkInput::Json(&finite));
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &[],
            }],
            turn_items: vec![first, second],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "mon-2", "description": "finite monitor", "kind": "background_task"}])
        );
    }

    #[test]
    fn returns_multiple_tasks_with_stable_ordering_from_insertion() {
        let roster = [
            task(json!({"taskId": "bg-1", "description": "first", "kind": "command"})),
            task(json!({"taskId": "bg-2", "description": "second", "kind": "command"})),
        ];
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            turn_items: vec![command("item-1", Some("third"), Some("cmd-3"), "third")],
            ..Default::default()
        });
        let ids = tasks
            .iter()
            .map(|task| task.task_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["bg-1", "bg-2", "cmd-3"]);
    }

    #[test]
    fn excludes_turn_items_owned_by_a_rolled_back_run() {
        let mut abandoned = command("item-1", Some("abandoned"), Some("cmd-1"), "sleep 99");
        abandoned.run_id = Some("run-1");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::RolledBack)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &[],
            }],
            runs: Some(vec![run("run-1", 1, Run::RolledBack)]),
            turn_items: vec![abandoned],
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn returns_empty_when_the_latest_run_is_rolled_back_even_with_a_nonempty_roster() {
        let roster = [task(
            json!({"taskId": "bg-1", "description": "sleep 20", "kind": "command"}),
        )];
        let mut abandoned = command("item-1", Some("abandoned"), Some("cmd-1"), "sleep 99");
        abandoned.run_id = Some("run-1");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::RolledBack)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            turn_items: vec![abandoned],
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn excludes_an_older_rolled_back_nonterminal_item_when_the_latest_run_is_completed() {
        let mut old = command(
            "item-old",
            Some("from rolled-back run"),
            Some("cmd-old"),
            "sleep 99",
        );
        old.run_id = Some("run-1");
        let mut new = command(
            "item-new",
            Some("still pending"),
            Some("cmd-new"),
            "npm test",
        );
        new.run_id = Some("run-2");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-2", 2, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &[],
            }],
            runs: Some(vec![
                run("run-1", 1, Run::RolledBack),
                run("run-2", 2, Run::Completed),
            ]),
            turn_items: vec![old, new],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "cmd-new", "description": "still pending", "kind": "command"}])
        );
    }

    #[test]
    fn does_not_reclassify_an_older_active_run_as_background_work() {
        let roster = [task(
            json!({"taskId": "provider-task", "kind": "background_task"}),
        )];
        let mut active = command(
            "item-active",
            Some("foreground work"),
            Some("cmd-active"),
            "vp check",
        );
        active.run_id = Some("run-1");
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-2", 2, Run::Cancelled)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            runs: Some(vec![
                run("run-1", 1, Run::Running),
                run("run-2", 2, Run::Cancelled),
            ]),
            turn_items: vec![active],
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn includes_items_with_a_null_run_id_even_when_runs_list_has_rolled_back_rows() {
        let orphan = command(
            "item-null-run",
            Some("orphan item"),
            Some("cmd-null"),
            "echo orphan",
        );
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &[],
            }],
            runs: Some(vec![run("run-1", 1, Run::RolledBack)]),
            turn_items: vec![orphan],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{"taskId": "cmd-null", "description": "orphan item", "kind": "command"}])
        );
    }

    #[test]
    fn keeps_the_rosters_kinds_and_names_turn_items_by_their_type() {
        let roster = [
            task(json!({"taskId": "bash", "kind": "command"})),
            task(json!({"taskId": "watch", "kind": "monitor"})),
            task(json!({"taskId": "workflow", "kind": "background_task"})),
        ];
        let child = ThreadId::new("thread:child").unwrap();
        let mut sub = item("item-sub", "subagent", Status::Running);
        sub.title = Some("Review src/math.ts");
        sub.native_id = Some("sub");
        sub.child_thread_id = Some(&child);
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            provider_threads: vec![PendingBackgroundWorkProviderThread {
                id: "pt-1",
                pending_background_tasks: &roster,
            }],
            turn_items: vec![sub, command("item-cmd", None, Some("cmd"), "npm test")],
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([
                {"taskId": "bash", "kind": "command"},
                {"taskId": "watch", "kind": "monitor"},
                {"taskId": "workflow", "kind": "background_task"},
                {"taskId": "sub", "description": "Review src/math.ts", "kind": "subagent", "childThreadId": "thread:child"},
                {"taskId": "cmd", "description": "npm test", "kind": "command"},
            ])
        );
    }

    fn pull_requests() -> Vec<ThreadPullRequestLink> {
        let watch = json!({
            "startedAt": "2026-10-05T00:00:00.000Z", "headSha": null, "failedChecks": [],
            "passed": false, "passedChecks": [], "remarksThrough": "2026-10-05T00:00:00.000Z",
            "remarkIds": [], "conflicting": false, "wakes": 0
        });
        let link = |number: u64, source: &str, watched: bool| -> ThreadPullRequestLink {
            let mut value = json!({
                "host": "github.com", "repository": "acme/app", "number": number,
                "url": format!("https://github.com/acme/app/pull/{number}"),
                "source": source, "linkedAt": "2026-10-05T00:00:00.000Z",
                "snapshot": null, "stack": null
            });
            if watched {
                value["watch"] = watch.clone();
            }
            serde_json::from_value(value).unwrap()
        };
        vec![
            link(1, "agent", true),
            link(2, "agent", false),
            link(3, "stack-dismissed", true),
        ]
    }

    #[test]
    fn keeps_a_settled_run_waiting_on_each_visible_watch_as_a_monitor() {
        let links = pull_requests();
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Completed)),
            pull_requests: Some(&links),
            ..Default::default()
        });
        assert_eq!(
            json_tasks(&tasks),
            json!([{
                "taskId": "pull-request-watch:github.com/acme/app#1",
                "description": "Watching pull request #1",
                "kind": "monitor"
            }])
        );
        assert!(background_work_holds_completion(&tasks));
    }

    #[test]
    fn keeps_a_thread_that_never_ran_waiting_on_its_watch() {
        let links = pull_requests();
        let tasks = derive(DerivePendingBackgroundWorkInput {
            pull_requests: Some(&links),
            ..Default::default()
        });
        let ids = tasks
            .iter()
            .map(|task| task.task_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["pull-request-watch:github.com/acme/app#1"]);
    }

    #[test]
    fn lists_no_watch_while_a_run_is_active() {
        let links = pull_requests();
        let tasks = derive(DerivePendingBackgroundWorkInput {
            latest_run: Some(run("run-1", 1, Run::Running)),
            pull_requests: Some(&links),
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn builds_a_view_from_a_typed_turn_item() {
        let value: OrchestrationV2TurnItem = serde_json::from_value(json!({
            "id": "item-1", "threadId": "thread-1", "runId": "run-1", "nodeId": null,
            "providerThreadId": null, "providerTurnId": null,
            "nativeItemRef": {"driver": "codex", "nativeId": "cmd-1", "strength": "strong"},
            "parentItemId": null, "ordinal": 1, "status": "running", "title": null,
            "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:00.000Z",
            "type": "command_execution", "input": "npm test"
        }))
        .unwrap();
        let view = PendingBackgroundWorkItem::from_turn_item(&value);
        assert_eq!(view.native_id, Some("cmd-1"));
        assert_eq!(view.run_id, Some("run-1"));
        let items = [view];
        assert_eq!(pending_background_turn_items(&items, None).len(), 1);
    }
}
