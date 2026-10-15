//! Port of upstream/packages/provider-core/src/server/notification.ts.

use turin_contracts::{
    NodeId, ThreadId, TrimmedNonEmptyString, js_trim,
    orchestration_v2::{
        OrchestrationV2ConversationMessage, OrchestrationV2MessageDelegatedCompletion,
        OrchestrationV2Notification, OrchestrationV2NotificationOutcome as Outcome,
        OrchestrationV2NotificationSource as Source, OrchestrationV2Subagent,
        OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
    },
};

use crate::turn_item::turn_item_common;

/// A piece of background work an adapter can name, by kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundWork {
    /// Subagent title, command, or monitor description.
    pub label: Option<String>,
    pub kind: BackgroundWorkKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundWorkKind {
    Subagent { child_thread_id: Option<ThreadId> },
    Command { exit_code: Option<i64> },
    Monitor,
    BackgroundTask,
}

impl BackgroundWorkKind {
    fn tag(&self) -> &'static str {
        match self {
            Self::Subagent { .. } => "subagent",
            Self::Command { .. } => "command",
            Self::Monitor => "monitor",
            Self::BackgroundTask => "background_task",
        }
    }

    /// `KIND_NOUN`: `[singular, plural]`.
    fn noun(&self) -> (&'static str, &'static str) {
        match self {
            Self::Subagent { .. } => ("Subagent", "subagents"),
            Self::Command { .. } => ("Command", "commands"),
            Self::Monitor => ("Monitor", "monitors"),
            Self::BackgroundTask => ("Background task", "background tasks"),
        }
    }
}

/// Background work an adapter saw finish or report, as the user should read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundWorkReport {
    pub work: BackgroundWork,
    pub outcome: Outcome,
}

const LABEL_MAX_LENGTH: usize = 80;

/// UTF-16 length, as JS `string.length`.
fn js_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn js_slice_units(value: &str, end: usize) -> String {
    let units: Vec<u16> = value.encode_utf16().take(end).collect();
    String::from_utf16_lossy(&units)
}

fn report_label(label: Option<&str>) -> Option<String> {
    let first_line = js_trim(js_trim(label?).split('\n').next().unwrap_or(""));
    if first_line.is_empty() {
        return None;
    }
    Some(if js_len(first_line) > LABEL_MAX_LENGTH {
        format!("{}…", js_slice_units(first_line, LABEL_MAX_LENGTH - 1))
    } else {
        first_line.to_owned()
    })
}

fn outcome_verb(kind: &BackgroundWorkKind, outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Failed => "failed",
        Outcome::Cancelled => "was stopped",
        Outcome::Updated => {
            if matches!(kind, BackgroundWorkKind::Monitor) {
                "reported new output"
            } else {
                "updated"
            }
        }
        Outcome::Completed | Outcome::Unknown => "finished",
    }
}

fn combined_outcome(outcomes: &[Outcome]) -> Outcome {
    if outcomes.contains(&Outcome::Failed) {
        Outcome::Failed
    } else if outcomes.contains(&Outcome::Cancelled) {
        Outcome::Cancelled
    } else if !outcomes.is_empty()
        && outcomes
            .iter()
            .all(|outcome| *outcome == Outcome::Completed)
    {
        Outcome::Completed
    } else if outcomes.contains(&Outcome::Updated) {
        Outcome::Updated
    } else {
        Outcome::Unknown
    }
}

fn exit_suffix(report: &BackgroundWorkReport) -> String {
    match &report.work.kind {
        BackgroundWorkKind::Command {
            exit_code: Some(code),
        } => format!(" (exit {code})"),
        _ => String::new(),
    }
}

fn named_report(report: &BackgroundWorkReport, capitalize: bool) -> String {
    let noun = report.work.kind.noun().0;
    let label = report_label(report.work.label.as_deref());
    let noun = if capitalize {
        noun.to_owned()
    } else {
        noun.to_lowercase()
    };
    format!(
        "{noun}{}{}",
        label.map_or_else(String::new, |label| format!(" \"{label}\"")),
        exit_suffix(report)
    )
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

fn reports_summary(reports: &[BackgroundWorkReport], outcome: Outcome) -> String {
    let first = &reports[0];
    if reports.len() == 1 {
        let label = report_label(first.work.label.as_deref());
        let noun = first.work.kind.noun().0;
        return format!(
            "{noun}{} {}{}",
            label.map_or_else(String::new, |label| format!(" \"{label}\"")),
            outcome_verb(&first.work.kind, first.outcome),
            exit_suffix(first)
        );
    }
    let verb = match outcome {
        Outcome::Failed => "failed",
        Outcome::Cancelled => "were stopped",
        Outcome::Updated => "updated",
        _ => "finished",
    };
    if reports.len() > 3
        || reports
            .iter()
            .all(|report| report_label(report.work.label.as_deref()).is_none())
    {
        let same_kind = reports
            .iter()
            .all(|report| report.work.kind.tag() == first.work.kind.tag());
        let noun = if same_kind {
            first.work.kind.noun().1
        } else {
            "background tasks"
        };
        return format!("{} {noun} {verb}", reports.len());
    }
    let names: Vec<String> = reports
        .iter()
        .enumerate()
        .map(|(index, report)| named_report(report, index == 0))
        .collect();
    format!("{} {verb}", join_names(&names))
}

/// Reports of one kind share it; mixed kinds are generic background work.
fn reports_source(reports: &[BackgroundWorkReport]) -> Source {
    let first = &reports[0];
    if reports[1..]
        .iter()
        .any(|report| report.work.kind.tag() != first.work.kind.tag())
    {
        return Source::BackgroundTask;
    }
    match &first.work.kind {
        // One subagent is the thing the row opens; several open nothing.
        BackgroundWorkKind::Subagent { child_thread_id } => Source::Subagent {
            child_thread_id: if reports.len() == 1 {
                child_thread_id.clone()
            } else {
                None
            },
        },
        BackgroundWorkKind::Command { .. } => Source::Command,
        BackgroundWorkKind::Monitor => Source::Monitor,
        BackgroundWorkKind::BackgroundTask => Source::BackgroundTask,
    }
}

fn summary(text: String) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(text).expect("notification summaries are non-blank")
}

/// A notification that says which background work a provider reported. `None`
/// when nothing is known, so the caller keeps its generic notification.
pub fn background_work_notification(
    reports: &[BackgroundWorkReport],
) -> Option<OrchestrationV2Notification> {
    if reports.is_empty() {
        return None;
    }
    let outcomes: Vec<Outcome> = reports.iter().map(|report| report.outcome).collect();
    let outcome = combined_outcome(&outcomes);
    Some(OrchestrationV2Notification {
        source: reports_source(reports),
        outcome,
        summary: summary(reports_summary(reports, outcome)),
        detail: None,
    })
}

struct DelegatedTaskReport {
    label: Option<String>,
    outcome: Outcome,
    child_thread_id: Option<ThreadId>,
}

fn delegated_task_report(task: Option<&OrchestrationV2Subagent>) -> DelegatedTaskReport {
    let outcome = match task.map(|task| task.status) {
        Some(OrchestrationV2SubagentStatus::Failed) => Outcome::Failed,
        Some(
            OrchestrationV2SubagentStatus::Cancelled | OrchestrationV2SubagentStatus::Interrupted,
        ) => Outcome::Cancelled,
        Some(OrchestrationV2SubagentStatus::Completed) => Outcome::Completed,
        _ => Outcome::Unknown,
    };
    // Delegated tasks are titled by their prompt unless the caller named them.
    let label = task.and_then(|task| {
        let title = task
            .title
            .as_deref()
            .map(js_trim)
            .filter(|title| !title.is_empty());
        title
            .map(str::to_owned)
            .or_else(|| Some(task.prompt.clone()))
    });
    DelegatedTaskReport {
        label,
        outcome,
        child_thread_id: task.and_then(|task| task.child_thread_id.clone()),
    }
}

/// Summarizes the delegated tasks one completion delivery reports, out of all
/// its parent run delegated.
fn delegated_completion_notification(
    completion: &OrchestrationV2MessageDelegatedCompletion,
    tasks: &[OrchestrationV2Subagent],
) -> OrchestrationV2Notification {
    let task_ids: &[NodeId] = &completion.task_ids;
    let reports: Vec<DelegatedTaskReport> = task_ids
        .iter()
        .map(|task_id| delegated_task_report(tasks.iter().find(|task| &task.id == task_id)))
        .collect();
    let delegated_by_run = tasks
        .iter()
        .filter(|task| {
            task.origin == OrchestrationV2SubagentOrigin::AppOwned
                && task.run_id.as_ref() == Some(&completion.parent_run_id)
        })
        .count();
    let outcomes: Vec<Outcome> = reports.iter().map(|report| report.outcome).collect();
    let outcome = combined_outcome(&outcomes);
    let verb = match outcome {
        Outcome::Failed => "failed",
        Outcome::Cancelled => "stopped",
        _ => "finished",
    };
    if let [only] = reports.as_slice() {
        let label = report_label(only.label.as_deref());
        return OrchestrationV2Notification {
            source: Source::DelegatedTask {
                task_ids: task_ids.to_vec(),
                child_thread_id: only.child_thread_id.clone(),
            },
            outcome,
            summary: summary(format!(
                "Delegated task{} {verb}",
                label.map_or_else(String::new, |label| format!(" \"{label}\""))
            )),
            detail: None,
        };
    }
    let labels: Vec<String> = reports
        .iter()
        .filter_map(|report| report_label(report.label.as_deref()))
        .collect();
    let count = if delegated_by_run > task_ids.len() {
        format!("{} of {delegated_by_run}", task_ids.len())
    } else {
        task_ids.len().to_string()
    };
    OrchestrationV2Notification {
        source: Source::DelegatedTask {
            task_ids: task_ids.to_vec(),
            child_thread_id: None,
        },
        outcome,
        summary: summary(format!(
            "{count} delegated tasks {verb}{}",
            if labels.is_empty() {
                String::new()
            } else {
                format!(": {}", labels.join(", "))
            }
        )),
        detail: None,
    }
}

/// The two message fields [`notification_turn_item`] reads (upstream
/// `Pick<OrchestrationV2ConversationMessage, "notification" | "delegatedCompletion">`).
#[derive(Debug, Clone, Copy, Default)]
pub struct NotificationMessage<'a> {
    pub notification: Option<&'a OrchestrationV2Notification>,
    pub delegated_completion: Option<&'a OrchestrationV2MessageDelegatedCompletion>,
}

impl<'a> From<&'a OrchestrationV2ConversationMessage> for NotificationMessage<'a> {
    fn from(message: &'a OrchestrationV2ConversationMessage) -> Self {
        Self {
            notification: message.notification.as_ref(),
            delegated_completion: message.delegated_completion.as_ref(),
        }
    }
}

/// Keep the delivery message intact while projecting its trigger as an activity.
pub fn notification_turn_item(
    item: &OrchestrationV2TurnItem,
    message: NotificationMessage<'_>,
    tasks: &[OrchestrationV2Subagent],
) -> OrchestrationV2TurnItem {
    let OrchestrationV2TurnItem::UserMessage {
        tool_non_execution_kind,
        ..
    } = item
    else {
        return item.clone();
    };
    let notification = match message.delegated_completion {
        Some(completion) => delegated_completion_notification(completion, tasks),
        None => match message.notification {
            Some(notification) => notification.clone(),
            None => return item.clone(),
        },
    };
    let base = turn_item_common(item);
    OrchestrationV2TurnItem::Notification {
        tool_non_execution_kind: tool_non_execution_kind.clone(),
        id: base.id.clone(),
        thread_id: base.thread_id.clone(),
        run_id: base.run_id.cloned(),
        node_id: base.node_id.cloned(),
        provider_thread_id: base.provider_thread_id.cloned(),
        provider_turn_id: base.provider_turn_id.cloned(),
        native_item_ref: base.native_item_ref.cloned(),
        parent_item_id: base.parent_item_id.cloned(),
        ordinal: base.ordinal,
        status: base.status,
        title: base.title.map(str::to_owned),
        started_at: base.started_at,
        completed_at: base.completed_at,
        updated_at: base.updated_at,
        source: notification.source,
        outcome: notification.outcome,
        summary: notification.summary,
        detail: notification.detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::{
        MessageId, RunId, TurnItemId, UtcDateTime,
        orchestration_v2::OrchestrationV2TurnItemStatus,
        orchestration_v2::OrchestrationV2UserMessageInputIntent,
        orchestration_v2::{OrchestrationV2Actor, OrchestrationV2CreationSource},
    };

    fn child_thread_id() -> ThreadId {
        ThreadId::new("thread:child").unwrap()
    }

    fn report(kind: BackgroundWorkKind, label: &str, outcome: Outcome) -> BackgroundWorkReport {
        BackgroundWorkReport {
            work: BackgroundWork {
                label: Some(label.to_owned()),
                kind,
            },
            outcome,
        }
    }

    #[test]
    fn keeps_the_generic_notification_when_nothing_is_known() {
        assert_eq!(background_work_notification(&[]), None);
    }

    #[test]
    fn names_one_piece_of_work_and_the_thread_of_a_subagent() {
        let notification = background_work_notification(&[report(
            BackgroundWorkKind::Subagent {
                child_thread_id: Some(child_thread_id()),
            },
            "Review src/math.ts",
            Outcome::Completed,
        )])
        .unwrap();
        assert_eq!(
            notification,
            OrchestrationV2Notification {
                source: Source::Subagent {
                    child_thread_id: Some(child_thread_id())
                },
                outcome: Outcome::Completed,
                summary: summary("Subagent \"Review src/math.ts\" finished".into()),
                detail: None,
            }
        );
        assert_eq!(
            background_work_notification(&[report(
                BackgroundWorkKind::Command { exit_code: Some(1) },
                "npm test\nnpm run lint",
                Outcome::Failed
            )])
            .unwrap()
            .summary
            .as_str(),
            "Command \"npm test\" failed (exit 1)"
        );
        assert_eq!(
            background_work_notification(&[report(
                BackgroundWorkKind::Monitor,
                "Three ticks",
                Outcome::Updated
            )])
            .unwrap()
            .summary
            .as_str(),
            "Monitor \"Three ticks\" reported new output"
        );
    }

    #[test]
    fn names_every_piece_of_work_that_ended_together_without_opening_one() {
        let notification = background_work_notification(&[
            report(
                BackgroundWorkKind::Subagent {
                    child_thread_id: Some(child_thread_id()),
                },
                "Agent B",
                Outcome::Cancelled,
            ),
            report(
                BackgroundWorkKind::Command { exit_code: None },
                "Sleep 60 seconds",
                Outcome::Cancelled,
            ),
        ])
        .unwrap();
        assert_eq!(notification.source, Source::BackgroundTask);
        assert_eq!(notification.outcome, Outcome::Cancelled);
        assert_eq!(
            notification.summary.as_str(),
            "Subagent \"Agent B\" and command \"Sleep 60 seconds\" were stopped"
        );
    }

    #[test]
    fn counts_work_it_cannot_list() {
        let reports: Vec<_> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|label| {
                report(
                    BackgroundWorkKind::Subagent {
                        child_thread_id: None,
                    },
                    label,
                    Outcome::Completed,
                )
            })
            .collect();
        let notification = background_work_notification(&reports).unwrap();
        assert_eq!(
            notification.source,
            Source::Subagent {
                child_thread_id: None
            }
        );
        assert_eq!(notification.summary.as_str(), "4 subagents finished");
    }

    #[test]
    fn long_labels_are_cut_at_eighty_units() {
        let long = "x".repeat(120);
        let label = report_label(Some(&long)).unwrap();
        assert_eq!(js_len(&label), 80);
        assert!(label.ends_with('…'));
    }

    fn user_message() -> OrchestrationV2TurnItem {
        let now = UtcDateTime::now();
        OrchestrationV2TurnItem::UserMessage {
            tool_non_execution_kind: None,
            id: TurnItemId::new("item:delivery").unwrap(),
            thread_id: ThreadId::new("thread:parent").unwrap(),
            run_id: Some(RunId::new("run:delivery").unwrap()),
            node_id: None,
            provider_thread_id: None,
            provider_turn_id: None,
            native_item_ref: None,
            parent_item_id: None,
            ordinal: 1,
            status: OrchestrationV2TurnItemStatus::Completed,
            title: None,
            started_at: Some(now),
            completed_at: Some(now),
            updated_at: now,
            created_by: OrchestrationV2Actor::Agent,
            creation_source: OrchestrationV2CreationSource::Server,
            message_id: MessageId::new("message:delivery").unwrap(),
            scheduled_task_id: None,
            sender_thread_id: None,
            input_intent: OrchestrationV2UserMessageInputIntent::TurnStart,
            text: "Delegated tasks reached terminal states.".into(),
            context: None,
            attachments: vec![],
        }
    }

    fn task(
        id: &str,
        title: &str,
        status: OrchestrationV2SubagentStatus,
    ) -> OrchestrationV2Subagent {
        let json = serde_json::json!({
            "id": id,
            "threadId": "thread:parent",
            "runId": "run:parent",
            "parentNodeId": "node:parent",
            "origin": "app_owned",
            "createdBy": "agent",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerThreadId": null,
            "childThreadId": format!("thread:{id}"),
            "nativeTaskRef": null,
            "prompt": format!("{title} prompt"),
            "title": title,
            "model": null,
            "status": serde_json::to_value(status).unwrap(),
            "result": null,
            "startedAt": null,
            "completedAt": null,
            "updatedAt": "2026-09-27T00:00:00.000Z",
        });
        serde_json::from_value(json).unwrap()
    }

    fn delivery(task_ids: &[&str]) -> OrchestrationV2MessageDelegatedCompletion {
        OrchestrationV2MessageDelegatedCompletion {
            parent_run_id: RunId::new("run:parent").unwrap(),
            generation: turin_contracts::PositiveInt::new(1).unwrap(),
            task_ids: task_ids.iter().map(|id| NodeId::new(id).unwrap()).collect(),
        }
    }

    fn tasks() -> Vec<OrchestrationV2Subagent> {
        vec![
            task(
                "a",
                "Review src/math.ts",
                OrchestrationV2SubagentStatus::Completed,
            ),
            task("b", "Write tests", OrchestrationV2SubagentStatus::Completed),
            task("c", "Update docs", OrchestrationV2SubagentStatus::Running),
        ]
    }

    #[test]
    fn says_how_many_of_a_runs_delegated_tasks_a_delivery_reports() {
        let item = notification_turn_item(
            &user_message(),
            NotificationMessage {
                delegated_completion: Some(&delivery(&["a", "b"])),
                ..NotificationMessage::default()
            },
            &tasks(),
        );
        let OrchestrationV2TurnItem::Notification {
            summary, source, ..
        } = item
        else {
            panic!("expected a notification item");
        };
        assert_eq!(
            summary.as_str(),
            "2 of 3 delegated tasks finished: Review src/math.ts, Write tests"
        );
        assert_eq!(
            source,
            Source::DelegatedTask {
                task_ids: vec![NodeId::new("a").unwrap(), NodeId::new("b").unwrap()],
                child_thread_id: None
            }
        );
    }

    #[test]
    fn opens_the_child_thread_of_a_single_delegated_task() {
        let item = notification_turn_item(
            &user_message(),
            NotificationMessage {
                delegated_completion: Some(&delivery(&["a"])),
                ..NotificationMessage::default()
            },
            &tasks(),
        );
        let OrchestrationV2TurnItem::Notification {
            summary, source, ..
        } = item
        else {
            panic!("expected a notification item");
        };
        assert_eq!(
            summary.as_str(),
            "Delegated task \"Review src/math.ts\" finished"
        );
        assert_eq!(
            source,
            Source::DelegatedTask {
                task_ids: vec![NodeId::new("a").unwrap()],
                child_thread_id: Some(ThreadId::new("thread:a").unwrap())
            }
        );
    }
}
