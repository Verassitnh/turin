//! Port of upstream/packages/shared/src/orchestrationV2PendingBackgroundWork.ts
//!
//! Upstream takes turn items as a structural union (`type`, `status`, `title`, `input`, `prompt`,
//! `toolName`, ...). This takes [`PendingBackgroundTurnItem`]: the same fields, with `input` split
//! into the two shapes the rules read (`input_text` for a command string, `input_persistent` for a
//! dynamic tool whose input object has `persistent: true`). The orchestration layer maps a
//! `OrchestrationV2TurnItem` into it, so this module never depends on the item union.
//!
//! Upstream's `TERMINAL_RUN_STATUSES` is referenced only to keep it defined, so it is not ported.

use indexmap::IndexMap;
use turin_contracts::base_schemas::{ThreadId, TrimmedNonEmptyString, js_trim};
use turin_contracts::orchestration_v2::{
    OrchestrationV2PendingBackgroundTask as PendingBackgroundWorkTask, OrchestrationV2RunStatus,
    OrchestrationV2TurnItemStatus,
};
use turin_contracts::thread_pull_request::{ThreadPullRequestLink, ThreadPullRequestLinkSource};

use crate::thread_pull_requests::normalize_thread_pull_request_key;

/// The turn item `type` values the background rules distinguish. Everything else is `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnItemType {
    CommandExecution,
    DynamicTool,
    Subagent,
    Other,
}

/// The turn item fields the pending-work rules read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBackgroundTurnItem {
    pub id: String,
    pub item_type: TurnItemType,
    pub status: OrchestrationV2TurnItemStatus,
    pub title: Option<String>,
    /// `None` is a null or absent run id, which stays eligible.
    pub run_id: Option<String>,
    pub native_id: Option<String>,
    /// `input` when it is a string: the command text of a `command_execution`.
    pub input_text: Option<String>,
    /// `input` is an object with `persistent: true`: a Grok persistent monitor, excluded.
    pub input_persistent: bool,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub child_thread_id: Option<ThreadId>,
}

/// The run fields the pending-work rules read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBackgroundRun {
    pub id: String,
    pub ordinal: u64,
    pub status: OrchestrationV2RunStatus,
}

/// A provider thread's roster. `None` is an absent `pendingBackgroundTasks`.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingBackgroundProviderThread {
    pub id: String,
    pub pending_background_tasks: Option<Vec<PendingBackgroundWorkTask>>,
}

/// Inputs to [`derive_pending_background_work`]. Optional fields follow the upstream optional
/// parameters; `Default` gives the empty input.
#[derive(Debug, Clone, Copy, Default)]
pub struct PendingBackgroundWorkInput<'a> {
    pub latest_run: Option<&'a PendingBackgroundRun>,
    pub provider_threads: &'a [PendingBackgroundProviderThread],
    pub turn_items: &'a [PendingBackgroundTurnItem],
    pub active_provider_thread_id: Option<&'a str>,
    pub has_active_run: Option<bool>,
    pub runs: Option<&'a [PendingBackgroundRun]>,
    pub pull_requests: Option<&'a [ThreadPullRequestLink]>,
}

/// Run statuses that allow a pending-background roster to surface. Includes `waiting`: a
/// successful turn persists as waiting until checkpoint capture flips it to completed. Excludes
/// `rolled_back`: the roster is not run-tied, so a rolled-back latest run fails the gate entirely.
const SETTLED_FOR_BACKGROUND_WAIT_RUN_STATUSES: [OrchestrationV2RunStatus; 5] = [
    OrchestrationV2RunStatus::Cancelled,
    OrchestrationV2RunStatus::Completed,
    OrchestrationV2RunStatus::Failed,
    OrchestrationV2RunStatus::Interrupted,
    OrchestrationV2RunStatus::Waiting,
];

fn is_background_turn_item_type(item_type: TurnItemType) -> bool {
    matches!(
        item_type,
        TurnItemType::CommandExecution | TurnItemType::DynamicTool | TurnItemType::Subagent
    )
}

/// `isOrchestrationV2WorkActive` for a turn item's status.
fn is_turn_item_work_active(status: OrchestrationV2TurnItemStatus) -> bool {
    matches!(
        status,
        OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting
    )
}

/// Whether a turn-item update can end background work that a settled run is still waiting on: an
/// item of a background type that is no longer active. Streaming output on a running item, and
/// every other item type, cannot.
pub fn turn_item_update_can_end_background_work(
    item_type: TurnItemType,
    status: OrchestrationV2TurnItemStatus,
) -> bool {
    is_background_turn_item_type(item_type) && !is_turn_item_work_active(status)
}

/// Whether background work left behind by a completed root run holds back its completion alert.
/// Commands, such as dev servers and other long-lived shells, do not: the agent is done and may
/// leave them running for hours. Subagents, monitors, and unknown kinds do.
pub fn background_work_holds_completion(tasks: &[PendingBackgroundWorkTask]) -> bool {
    tasks
        .iter()
        .any(|task| !matches!(task, PendingBackgroundWorkTask::Command { .. }))
}

fn is_persistent_dynamic_tool(item: &PendingBackgroundTurnItem) -> bool {
    item.item_type == TurnItemType::DynamicTool && item.input_persistent
}

/// The turn items the pending-work list names, without its settled-run gate. Stop ends exactly
/// these, so the list and Stop cannot disagree.
pub fn pending_background_turn_items<'a>(
    turn_items: &'a [PendingBackgroundTurnItem],
    runs: Option<&[PendingBackgroundRun]>,
) -> Vec<&'a PendingBackgroundTurnItem> {
    let rolled_back_run_ids: Vec<&str> = runs
        .unwrap_or(&[])
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::RolledBack)
        .map(|run| run.id.as_str())
        .collect();
    turn_items
        .iter()
        .filter(|item| {
            is_background_turn_item_type(item.item_type)
                && is_turn_item_work_active(item.status)
                && !is_persistent_dynamic_tool(item)
                // Null/absent run id stays eligible; only known rolled_back runs drop.
                && item
                    .run_id
                    .as_deref()
                    .map_or(true, |run_id| !rolled_back_run_ids.contains(&run_id))
        })
        .collect()
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = js_trim(value);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn description_from_turn_item(item: &PendingBackgroundTurnItem) -> Option<String> {
    if let Some(title) = item.title.as_deref().and_then(non_empty_trimmed) {
        return Some(title);
    }
    match item.item_type {
        TurnItemType::CommandExecution => item.input_text.as_deref().and_then(non_empty_trimmed),
        TurnItemType::DynamicTool => item.tool_name.as_deref().and_then(non_empty_trimmed),
        TurnItemType::Subagent => item.prompt.as_deref().and_then(non_empty_trimmed),
        TurnItemType::Other => None,
    }
}

fn trimmed_name(value: String) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn pending_task_from_turn_item(
    task_id: TrimmedNonEmptyString,
    item: &PendingBackgroundTurnItem,
) -> PendingBackgroundWorkTask {
    let description = description_from_turn_item(item).and_then(trimmed_name);
    match item.item_type {
        TurnItemType::Subagent => PendingBackgroundWorkTask::Subagent {
            task_id,
            description,
            child_thread_id: item.child_thread_id.clone(),
        },
        TurnItemType::CommandExecution => PendingBackgroundWorkTask::Command {
            task_id,
            description,
        },
        TurnItemType::DynamicTool | TurnItemType::Other => {
            PendingBackgroundWorkTask::BackgroundTask {
                task_id,
                description,
            }
        }
    }
}

/// The native id when the adapter gives one, else the turn item id.
fn native_task_id_from_turn_item(item: &PendingBackgroundTurnItem) -> &str {
    match item.native_id.as_deref() {
        Some(native_id) if !native_id.is_empty() => native_id,
        _ => &item.id,
    }
}

fn with_description(
    task: &PendingBackgroundWorkTask,
    description: Option<TrimmedNonEmptyString>,
) -> PendingBackgroundWorkTask {
    match task.clone() {
        PendingBackgroundWorkTask::Subagent {
            task_id,
            child_thread_id,
            ..
        } => PendingBackgroundWorkTask::Subagent {
            task_id,
            description,
            child_thread_id,
        },
        PendingBackgroundWorkTask::Command { task_id, .. } => PendingBackgroundWorkTask::Command {
            task_id,
            description,
        },
        PendingBackgroundWorkTask::Monitor { task_id, .. } => PendingBackgroundWorkTask::Monitor {
            task_id,
            description,
        },
        PendingBackgroundWorkTask::BackgroundTask { task_id, .. } => {
            PendingBackgroundWorkTask::BackgroundTask {
                task_id,
                description,
            }
        }
    }
}

/// A monitor per visible pull request watch. A watch wakes the agent, so the thread stays working
/// between wakes. Stack-dismissed links are tombstones and do not count.
fn pull_request_watch_tasks(
    pull_requests: Option<&[ThreadPullRequestLink]>,
) -> Vec<PendingBackgroundWorkTask> {
    pull_requests
        .unwrap_or(&[])
        .iter()
        .filter(|link| {
            link.watch.is_some() && link.source != ThreadPullRequestLinkSource::StackDismissed
        })
        .filter_map(|link| {
            let key = normalize_thread_pull_request_key(
                link.host.as_str(),
                link.repository.as_str(),
                link.number.get(),
            );
            Some(PendingBackgroundWorkTask::Monitor {
                task_id: trimmed_name(format!(
                    "pull-request-watch:{}/{}#{}",
                    key.host, key.repository, key.number
                ))?,
                description: trimmed_name(format!("Watching pull request #{}", link.number.get())),
            })
        })
        .collect()
}

/// Derive one normalized pending-background-work list for post-settlement UI.
///
/// Sources: the provider-thread roster (Claude SDK background tasks), active `command_execution`
/// / `dynamic_tool` / `subagent` turn items, and pull request watches as monitors.
///
/// Gated on latest root run settlement. Dedupes by native task id. Excludes the roster while any
/// interruptible foreground run remains active. Excludes Grok persistent monitors. Excludes turn
/// items whose run is `rolled_back`; items with a null run id stay eligible.
pub fn derive_pending_background_work(
    input: PendingBackgroundWorkInput<'_>,
) -> Vec<PendingBackgroundWorkTask> {
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

    let mut by_task_id: IndexMap<String, PendingBackgroundWorkTask> = IndexMap::new();

    let provider_threads = input.provider_threads.iter().filter(|thread| {
        input
            .active_provider_thread_id
            .map_or(true, |active| thread.id == active)
    });
    for provider_thread in provider_threads {
        for task in provider_thread.pending_background_tasks.iter().flatten() {
            let task_id = task.task_id().as_str().to_owned();
            if by_task_id.contains_key(&task_id) {
                continue;
            }
            let description = task
                .description()
                .and_then(|description| non_empty_trimmed(description.as_str()))
                .and_then(trimmed_name);
            by_task_id.insert(task_id, with_description(task, description));
        }
    }

    for item in pending_background_turn_items(input.turn_items, input.runs) {
        let task_id = native_task_id_from_turn_item(item).to_owned();
        if by_task_id.contains_key(&task_id) {
            continue;
        }
        let Some(name) = trimmed_name(task_id.clone()) else {
            continue;
        };
        by_task_id.insert(task_id, pending_task_from_turn_item(name, item));
    }

    // Replacing a value keeps its place in the list, as `Map.set` does.
    for task in pull_request_watch_tasks(input.pull_requests) {
        by_task_id.insert(task.task_id().as_str().to_owned(), task);
    }

    by_task_id.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use OrchestrationV2RunStatus::{Cancelled, Completed, RolledBack, Running, Waiting};
    use OrchestrationV2TurnItemStatus::{
        Completed as ItemCompleted, Failed, Idle, Running as ItemRunning,
    };
    use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString as Tne};
    use turin_contracts::thread_pull_request::ThreadPullRequestWatch;

    fn item(
        id: &str,
        item_type: TurnItemType,
        status: OrchestrationV2TurnItemStatus,
    ) -> PendingBackgroundTurnItem {
        PendingBackgroundTurnItem {
            id: id.to_owned(),
            item_type,
            status,
            title: None,
            run_id: None,
            native_id: None,
            input_text: None,
            input_persistent: false,
            prompt: None,
            tool_name: None,
            child_thread_id: None,
        }
    }

    fn run(id: &str, ordinal: u64, status: OrchestrationV2RunStatus) -> PendingBackgroundRun {
        PendingBackgroundRun {
            id: id.to_owned(),
            ordinal,
            status,
        }
    }

    fn thread(
        id: &str,
        tasks: Option<Vec<PendingBackgroundWorkTask>>,
    ) -> PendingBackgroundProviderThread {
        PendingBackgroundProviderThread {
            id: id.to_owned(),
            pending_background_tasks: tasks,
        }
    }

    fn roster_task(
        task_id: &str,
        kind: &str,
        description: Option<&str>,
    ) -> PendingBackgroundWorkTask {
        let task_id = Tne::new(task_id).unwrap();
        let description = description.map(|value| Tne::new(value).unwrap());
        match kind {
            "command" => PendingBackgroundWorkTask::Command {
                task_id,
                description,
            },
            "monitor" => PendingBackgroundWorkTask::Monitor {
                task_id,
                description,
            },
            "subagent" => PendingBackgroundWorkTask::Subagent {
                task_id,
                description,
                child_thread_id: None,
            },
            _ => PendingBackgroundWorkTask::BackgroundTask {
                task_id,
                description,
            },
        }
    }

    /// `(taskId, description, kind)` for compact assertions.
    fn summary(tasks: &[PendingBackgroundWorkTask]) -> Vec<(String, Option<String>, &'static str)> {
        tasks
            .iter()
            .map(|task| {
                let kind = match task {
                    PendingBackgroundWorkTask::Subagent { .. } => "subagent",
                    PendingBackgroundWorkTask::Command { .. } => "command",
                    PendingBackgroundWorkTask::Monitor { .. } => "monitor",
                    PendingBackgroundWorkTask::BackgroundTask { .. } => "background_task",
                };
                (
                    task.task_id().as_str().to_owned(),
                    task.description().map(|value| value.as_str().to_owned()),
                    kind,
                )
            })
            .collect()
    }

    fn expected(
        task_id: &str,
        description: Option<&str>,
        kind: &'static str,
    ) -> (String, Option<String>, &'static str) {
        (task_id.to_owned(), description.map(str::to_owned), kind)
    }

    #[test]
    fn turn_item_update_can_end_background_work_only_for_inactive_background_items() {
        let cases = [
            (
                "a finished subagent",
                true,
                TurnItemType::Subagent,
                ItemCompleted,
            ),
            (
                "a failed background command",
                true,
                TurnItemType::CommandExecution,
                Failed,
            ),
            (
                "an idle dynamic tool",
                true,
                TurnItemType::DynamicTool,
                Idle,
            ),
            (
                "a running command's output",
                false,
                TurnItemType::CommandExecution,
                ItemRunning,
            ),
            (
                "a waiting subagent",
                false,
                TurnItemType::Subagent,
                OrchestrationV2TurnItemStatus::Waiting,
            ),
            (
                "a finished assistant message",
                false,
                TurnItemType::Other,
                ItemCompleted,
            ),
        ];
        for (case, can_end, item_type, status) in cases {
            assert_eq!(
                turn_item_update_can_end_background_work(item_type, status),
                can_end,
                "{case}"
            );
        }
    }

    #[test]
    fn background_work_holds_completion_unless_only_commands_are_pending() {
        let cases: Vec<(&str, bool, Vec<PendingBackgroundWorkTask>)> = vec![
            ("nothing pending", false, vec![]),
            (
                "a dev server",
                false,
                vec![roster_task("dev", "command", None)],
            ),
            (
                "two long-lived shells",
                false,
                vec![
                    roster_task("web", "command", None),
                    roster_task("api", "command", None),
                ],
            ),
            (
                "a subagent",
                true,
                vec![roster_task("review", "subagent", None)],
            ),
            (
                "a monitor",
                true,
                vec![roster_task("watch", "monitor", None)],
            ),
            (
                "a command and a monitor",
                true,
                vec![
                    roster_task("dev", "command", None),
                    roster_task("watch", "monitor", None),
                ],
            ),
            // Also what an unknown or missing kind decodes to.
            (
                "work the provider cannot name",
                true,
                vec![roster_task("opaque", "background_task", None)],
            ),
        ];
        for (case, holds, tasks) in cases {
            assert_eq!(background_work_holds_completion(&tasks), holds, "{case}");
        }
    }

    #[test]
    fn returns_empty_while_the_latest_run_is_not_settled() {
        let latest = run("run-1", 1, Running);
        let providers = [thread(
            "pt-1",
            Some(vec![roster_task("bg-1", "command", Some("sleep 20"))]),
        )];
        let mut command = item("item-1", TurnItemType::CommandExecution, ItemRunning);
        command.title = Some("npm test".to_owned());
        command.input_text = Some("npm test".to_owned());
        let items = [command];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn does_not_keep_a_settled_parent_waiting_for_an_idle_child() {
        let latest = run("run-1", 1, Completed);
        let mut child = item("idle-child", TurnItemType::Subagent, Idle);
        child.title = Some("Review".to_owned());
        let items = [child];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            turn_items: &items,
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn returns_pending_work_when_the_latest_run_is_waiting_post_success_pre_checkpoint() {
        let latest = run("run-1", 1, Waiting);
        let providers = [thread("pt-1", None)];
        let mut command = item("item-1", TurnItemType::CommandExecution, ItemRunning);
        command.title = Some("npm test".to_owned());
        command.native_id = Some("cmd-1".to_owned());
        command.input_text = Some("npm test".to_owned());
        let items = [command];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("cmd-1", Some("npm test"), "command")]
        );
    }

    #[test]
    fn still_returns_empty_when_the_latest_run_is_running_even_with_background_items() {
        let latest = run("run-1", 1, Running);
        let providers = [thread(
            "pt-1",
            Some(vec![roster_task("bg-1", "command", Some("sleep 20"))]),
        )];
        let mut sub = item("item-1", TurnItemType::Subagent, ItemRunning);
        sub.title = Some("review".to_owned());
        sub.native_id = Some("sub-1".to_owned());
        sub.prompt = Some("review".to_owned());
        let items = [sub];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn excludes_rolled_back_items_when_the_latest_run_is_waiting() {
        let latest = run("run-2", 2, Waiting);
        let providers = [thread("pt-1", None)];
        let runs = [run("run-1", 1, RolledBack), run("run-2", 2, Waiting)];
        let mut old = item("item-old", TurnItemType::CommandExecution, ItemRunning);
        old.title = Some("from rolled-back run".to_owned());
        old.run_id = Some("run-1".to_owned());
        old.native_id = Some("cmd-old".to_owned());
        old.input_text = Some("sleep 99".to_owned());
        let mut new = item("item-new", TurnItemType::CommandExecution, ItemRunning);
        new.title = Some("still pending".to_owned());
        new.run_id = Some("run-2".to_owned());
        new.native_id = Some("cmd-new".to_owned());
        new.input_text = Some("npm test".to_owned());
        let items = [old, new];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            runs: Some(&runs),
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("cmd-new", Some("still pending"), "command")]
        );
    }

    #[test]
    fn returns_the_provider_thread_roster_after_settlement() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread(
            "pt-1",
            Some(vec![roster_task(
                "bg-1",
                "command",
                Some("Run Codex review"),
            )]),
        )];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            active_provider_thread_id: Some("pt-1"),
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("bg-1", Some("Run Codex review"), "command")]
        );
    }

    #[test]
    fn includes_nonterminal_turn_items_and_excludes_completed_ones() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread("pt-1", None)];
        let mut running = item("item-1", TurnItemType::CommandExecution, ItemRunning);
        running.title = Some("npm test".to_owned());
        running.native_id = Some("cmd-1".to_owned());
        running.input_text = Some("npm test".to_owned());
        let mut done = item("item-2", TurnItemType::CommandExecution, ItemCompleted);
        done.title = Some("done".to_owned());
        done.native_id = Some("cmd-2".to_owned());
        done.input_text = Some("echo done".to_owned());
        let items = [running, done];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("cmd-1", Some("npm test"), "command")]
        );
    }

    #[test]
    fn trims_normalized_background_work_descriptions() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread(
            "pt-1",
            Some(vec![PendingBackgroundWorkTask::BackgroundTask {
                task_id: Tne::new("native-task").unwrap(),
                description: Some(Tne::new("  native background work  ").unwrap()),
            }]),
        )];
        let mut command = item("item-command", TurnItemType::CommandExecution, ItemRunning);
        command.title = Some("  npm test  ".to_owned());
        let mut tool = item("item-tool", TurnItemType::DynamicTool, ItemRunning);
        tool.tool_name = Some("  browser.search  ".to_owned());
        let items = [command, tool];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![
                expected(
                    "native-task",
                    Some("native background work"),
                    "background_task"
                ),
                expected("item-command", Some("npm test"), "command"),
                expected("item-tool", Some("browser.search"), "background_task"),
            ]
        );
    }

    #[test]
    fn dedupes_roster_entries_against_turn_items_by_native_task_id() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread(
            "pt-1",
            Some(vec![roster_task(
                "task-9",
                "background_task",
                Some("Agent review"),
            )]),
        )];
        let mut sub = item("item-sub", TurnItemType::Subagent, ItemRunning);
        sub.title = Some("Agent review".to_owned());
        sub.native_id = Some("task-9".to_owned());
        sub.prompt = Some("review the plan".to_owned());
        let items = [sub];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("task-9", Some("Agent review"), "background_task")]
        );
    }

    #[test]
    fn excludes_grok_persistent_monitors() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread("pt-1", None)];
        let mut persistent = item("item-1", TurnItemType::DynamicTool, ItemRunning);
        persistent.title = Some("monitor logs".to_owned());
        persistent.native_id = Some("mon-1".to_owned());
        persistent.input_persistent = true;
        let mut finite = item("item-2", TurnItemType::DynamicTool, ItemRunning);
        finite.title = Some("finite monitor".to_owned());
        finite.native_id = Some("mon-2".to_owned());
        let items = [persistent, finite];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("mon-2", Some("finite monitor"), "background_task")]
        );
    }

    #[test]
    fn returns_multiple_tasks_with_stable_ordering_from_insertion() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread(
            "pt-1",
            Some(vec![
                roster_task("bg-1", "command", Some("first")),
                roster_task("bg-2", "command", Some("second")),
            ]),
        )];
        let mut third = item("item-1", TurnItemType::CommandExecution, ItemRunning);
        third.title = Some("third".to_owned());
        third.native_id = Some("cmd-3".to_owned());
        third.input_text = Some("third".to_owned());
        let items = [third];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        let ids: Vec<String> = summary(&tasks).into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(ids, vec!["bg-1", "bg-2", "cmd-3"]);
    }

    #[test]
    fn excludes_turn_items_owned_by_a_rolled_back_run() {
        let latest = run("run-1", 1, RolledBack);
        let providers = [thread("pt-1", None)];
        let runs = [run("run-1", 1, RolledBack)];
        let mut abandoned = item("item-1", TurnItemType::CommandExecution, ItemRunning);
        abandoned.title = Some("abandoned".to_owned());
        abandoned.run_id = Some("run-1".to_owned());
        abandoned.native_id = Some("cmd-1".to_owned());
        abandoned.input_text = Some("sleep 99".to_owned());
        let items = [abandoned];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            runs: Some(&runs),
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn returns_empty_when_the_latest_run_is_rolled_back_even_with_a_nonempty_roster() {
        let latest = run("run-1", 1, RolledBack);
        let providers = [thread(
            "pt-1",
            Some(vec![roster_task("bg-1", "command", Some("sleep 20"))]),
        )];
        let mut abandoned = item("item-1", TurnItemType::CommandExecution, ItemRunning);
        abandoned.title = Some("abandoned".to_owned());
        abandoned.run_id = Some("run-1".to_owned());
        abandoned.native_id = Some("cmd-1".to_owned());
        abandoned.input_text = Some("sleep 99".to_owned());
        let items = [abandoned];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn excludes_an_older_rolled_back_nonterminal_item_when_the_latest_run_is_completed() {
        let latest = run("run-2", 2, Completed);
        let providers = [thread("pt-1", None)];
        let runs = [run("run-1", 1, RolledBack), run("run-2", 2, Completed)];
        let mut old = item("item-old", TurnItemType::CommandExecution, ItemRunning);
        old.title = Some("from rolled-back run".to_owned());
        old.run_id = Some("run-1".to_owned());
        old.native_id = Some("cmd-old".to_owned());
        old.input_text = Some("sleep 99".to_owned());
        let mut new = item("item-new", TurnItemType::CommandExecution, ItemRunning);
        new.title = Some("still pending".to_owned());
        new.run_id = Some("run-2".to_owned());
        new.native_id = Some("cmd-new".to_owned());
        new.input_text = Some("npm test".to_owned());
        let items = [old, new];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            runs: Some(&runs),
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("cmd-new", Some("still pending"), "command")]
        );
    }

    #[test]
    fn does_not_reclassify_an_older_active_run_as_background_work() {
        let latest = run("run-2", 2, Cancelled);
        let providers = [thread(
            "pt-1",
            Some(vec![roster_task("provider-task", "background_task", None)]),
        )];
        let runs = [run("run-1", 1, Running), run("run-2", 2, Cancelled)];
        let mut active = item("item-active", TurnItemType::CommandExecution, ItemRunning);
        active.title = Some("foreground work".to_owned());
        active.run_id = Some("run-1".to_owned());
        active.native_id = Some("cmd-active".to_owned());
        active.input_text = Some("vp check".to_owned());
        let items = [active];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            runs: Some(&runs),
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn includes_items_with_a_null_run_id_even_when_runs_list_has_rolled_back_rows() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread("pt-1", None)];
        let runs = [run("run-1", 1, RolledBack)];
        let mut orphan = item("item-null-run", TurnItemType::CommandExecution, ItemRunning);
        orphan.title = Some("orphan item".to_owned());
        orphan.run_id = None;
        orphan.native_id = Some("cmd-null".to_owned());
        orphan.input_text = Some("echo orphan".to_owned());
        let items = [orphan];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            runs: Some(&runs),
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected("cmd-null", Some("orphan item"), "command")]
        );
    }

    #[test]
    fn keeps_the_rosters_kinds_and_names_turn_items_by_their_type() {
        let latest = run("run-1", 1, Completed);
        let providers = [thread(
            "pt-1",
            Some(vec![
                roster_task("bash", "command", None),
                roster_task("watch", "monitor", None),
                roster_task("workflow", "background_task", None),
            ]),
        )];
        let mut sub = item("item-sub", TurnItemType::Subagent, ItemRunning);
        sub.title = Some("Review src/math.ts".to_owned());
        sub.native_id = Some("sub".to_owned());
        sub.child_thread_id = Some(ThreadId::new("thread:child").unwrap());
        let mut command = item("item-cmd", TurnItemType::CommandExecution, ItemRunning);
        command.native_id = Some("cmd".to_owned());
        command.input_text = Some("npm test".to_owned());
        let items = [sub, command];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            provider_threads: &providers,
            turn_items: &items,
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![
                expected("bash", None, "command"),
                expected("watch", None, "monitor"),
                expected("workflow", None, "background_task"),
                expected("sub", Some("Review src/math.ts"), "subagent"),
                expected("cmd", Some("npm test"), "command"),
            ]
        );
        let subagent = tasks
            .iter()
            .find(|task| task.task_id().as_str() == "sub")
            .unwrap();
        assert!(matches!(
            subagent,
            PendingBackgroundWorkTask::Subagent { child_thread_id: Some(child), .. }
                if child.as_str() == "thread:child"
        ));
    }

    fn watch() -> ThreadPullRequestWatch {
        ThreadPullRequestWatch {
            started_at: "2026-10-05T00:00:00.000Z".to_owned(),
            head_sha: None,
            failed_checks: vec![],
            passed: false,
            passed_checks: vec![],
            remarks_through: "2026-10-05T00:00:00.000Z".to_owned(),
            remark_ids: vec![],
            conflicting: false,
            wakes: 0,
        }
    }

    fn pull_request(
        number: u64,
        source: ThreadPullRequestLinkSource,
        watched: bool,
    ) -> ThreadPullRequestLink {
        ThreadPullRequestLink {
            host: Tne::new("github.com").unwrap(),
            repository: Tne::new("acme/app").unwrap(),
            number: PositiveInt::new(number).unwrap(),
            url: Tne::new(format!("https://github.com/acme/app/pull/{number}")).unwrap(),
            source,
            linked_at: "2026-10-05T00:00:00.000Z".to_owned(),
            snapshot: None,
            stack: None,
            watch: watched.then(watch),
        }
    }

    #[test]
    fn keeps_a_settled_run_waiting_on_each_visible_watch_as_a_monitor() {
        let latest = run("run-1", 1, Completed);
        let pull_requests = [
            pull_request(1, ThreadPullRequestLinkSource::Agent, true),
            pull_request(2, ThreadPullRequestLinkSource::Agent, false),
            pull_request(3, ThreadPullRequestLinkSource::StackDismissed, true),
        ];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            pull_requests: Some(&pull_requests),
            ..Default::default()
        });
        assert_eq!(
            summary(&tasks),
            vec![expected(
                "pull-request-watch:github.com/acme/app#1",
                Some("Watching pull request #1"),
                "monitor",
            )]
        );
        assert!(background_work_holds_completion(&tasks));
    }

    #[test]
    fn keeps_a_thread_that_never_ran_waiting_on_its_watch() {
        let pull_requests = [pull_request(1, ThreadPullRequestLinkSource::Agent, true)];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: None,
            pull_requests: Some(&pull_requests),
            ..Default::default()
        });
        let ids: Vec<String> = summary(&tasks).into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(ids, vec!["pull-request-watch:github.com/acme/app#1"]);
    }

    #[test]
    fn lists_no_watch_while_a_run_is_active() {
        let latest = run("run-1", 1, Running);
        let pull_requests = [pull_request(1, ThreadPullRequestLinkSource::Agent, true)];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            pull_requests: Some(&pull_requests),
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }

    #[test]
    fn a_failed_turn_item_is_not_pending_work_for_a_completed_run() {
        let latest = run("run-1", 1, Completed);
        let mut failed = item("item-1", TurnItemType::CommandExecution, Failed);
        failed.native_id = Some("cmd-1".to_owned());
        let items = [failed];
        let tasks = derive_pending_background_work(PendingBackgroundWorkInput {
            latest_run: Some(&latest),
            turn_items: &items,
            ..Default::default()
        });
        assert!(tasks.is_empty());
    }
}
