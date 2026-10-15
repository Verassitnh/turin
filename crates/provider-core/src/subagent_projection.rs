//! Port of upstream/packages/provider-core/src/server/subagentProjection.ts.
//!
//! Upstream takes `Pick<OrchestrationV2ThreadProjection, …>` views; here each
//! function takes the slices it reads, with `From<&OrchestrationV2ThreadProjection>`
//! for the full projection.

use std::{cmp::Ordering, collections::HashSet, future::Future};

use serde_json::{Value, json};
use turin_contracts::{
    EventId, MessageId, NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId,
    TrimmedNonEmptyString, TurnItemId, UtcDateTime, js_trim,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2AppThreadLineage,
        OrchestrationV2ConversationMessage, OrchestrationV2ConversationMessageRole,
        OrchestrationV2CreationSource, OrchestrationV2DelegatedCompletionTaskDeliveryState,
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ForkSource,
        OrchestrationV2LineageRelationship, OrchestrationV2NotificationSource,
        OrchestrationV2ProviderRef, OrchestrationV2ProviderThread, OrchestrationV2Run,
        OrchestrationV2RunStatus, OrchestrationV2Subagent, OrchestrationV2SubagentOrigin,
        OrchestrationV2SubagentStatus, OrchestrationV2ThreadProjection, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus, is_orchestration_v2_work_active,
    },
    provider_instance::ProviderInstanceId,
};

use crate::{
    js::{take_utf16, utf16_len},
    turn_item::turn_item_common,
};

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(js_trim).filter(|text| !text.is_empty())
}

pub fn subagent_thread_title(
    parent_title: &str,
    title: Option<&str>,
    prompt: &str,
    ordinal: u64,
) -> String {
    let Some(detail) = trimmed(title).or_else(|| trimmed(Some(prompt))) else {
        return format!("{parent_title} subagent {ordinal}");
    };
    if utf16_len(detail) > 72 {
        format!("{}...", take_utf16(detail, 69))
    } else {
        detail.to_owned()
    }
}

pub struct SubagentChildThreadInput<'a> {
    pub parent_thread: &'a OrchestrationV2AppThread,
    pub child_thread_id: ThreadId,
    pub parent_node_id: NodeId,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub title: TrimmedNonEmptyString,
    pub now: UtcDateTime,
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
}

pub fn make_subagent_child_thread(input: SubagentChildThreadInput<'_>) -> OrchestrationV2AppThread {
    let mut child = input.parent_thread.clone();
    child.created_by = input.created_by;
    child.creation_source = input.creation_source;
    child.id = input.child_thread_id;
    child.title = input.title;
    child.linked_pull_request = Some(None);
    child.pull_requests = Some(Vec::new());
    child.history_origin = None;
    child.provider_instance_id = input.provider_instance_id;
    child.model_selection = input.model_selection;
    child.active_provider_thread_id = input.active_provider_thread_id;
    child.lineage = OrchestrationV2AppThreadLineage {
        parent_thread_id: Some(input.parent_thread.id.clone()),
        relationship_to_parent: Some(OrchestrationV2LineageRelationship::Subagent),
        root_thread_id: input.parent_thread.lineage.root_thread_id.clone(),
    };
    child.forked_from = Some(OrchestrationV2ForkSource::Node {
        node_id: input.parent_node_id,
    });
    child.created_at = input.now;
    child.updated_at = input.now;
    child.archived_at = None;
    child.settled_override = None;
    child.settled_at = None;
    child.snoozed_until = Some(None);
    child.snoozed_at = Some(None);
    child.last_visited_at = None;
    child.deleted_at = None;
    child
}

pub struct SubagentConversationArtifactsInput {
    pub message_id: MessageId,
    pub sender_thread_id: Option<ThreadId>,
    pub turn_item_id: TurnItemId,
    pub thread_id: ThreadId,
    pub root_node_id: NodeId,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<OrchestrationV2ProviderRef>,
    /// `"user" | "assistant"`; any other role is treated as assistant.
    pub role: OrchestrationV2ConversationMessageRole,
    pub text: String,
    pub ordinal: u64,
    pub now: UtcDateTime,
}

pub struct SubagentConversationArtifacts {
    pub message: OrchestrationV2ConversationMessage,
    pub turn_item: OrchestrationV2TurnItem,
}

pub fn make_subagent_conversation_artifacts(
    input: SubagentConversationArtifactsInput,
) -> SubagentConversationArtifacts {
    let is_user = input.role == OrchestrationV2ConversationMessageRole::User;
    let sender_thread_id = if is_user {
        input.sender_thread_id
    } else {
        None
    };
    let message = OrchestrationV2ConversationMessage {
        notification: None,
        created_by: OrchestrationV2Actor::Agent,
        creation_source: OrchestrationV2CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: sender_thread_id.clone(),
        id: input.message_id.clone(),
        thread_id: input.thread_id.clone(),
        run_id: None,
        node_id: Some(input.root_node_id.clone()),
        role: input.role,
        text: input.text.clone(),
        context: None,
        attachments: Vec::new(),
        streaming: false,
        created_at: input.now,
        updated_at: input.now,
        delegated_completion: None,
    };
    let mut item = json!({
        "id": input.turn_item_id, "threadId": input.thread_id, "runId": null,
        "nodeId": input.root_node_id, "providerThreadId": input.provider_thread_id,
        "providerTurnId": input.provider_turn_id, "nativeItemRef": input.native_item_ref,
        "parentItemId": null, "ordinal": input.ordinal, "status": "completed",
        "title": null, "startedAt": input.now, "completedAt": input.now,
        "updatedAt": input.now, "messageId": input.message_id, "text": input.text,
    });
    let fields = item.as_object_mut().expect("object literal");
    if is_user {
        fields.insert("createdBy".into(), json!("agent"));
        fields.insert("creationSource".into(), json!("provider"));
        fields.insert("type".into(), json!("user_message"));
        if let Some(sender) = sender_thread_id {
            fields.insert("senderThreadId".into(), json!(sender));
        }
        fields.insert("inputIntent".into(), json!("turn_start"));
        fields.insert("attachments".into(), json!([]));
    } else {
        fields.insert("type".into(), json!("assistant_message"));
        fields.insert("streaming".into(), json!(false));
    }
    let turn_item = serde_json::from_value(item).expect("valid subagent turn item");
    SubagentConversationArtifacts { message, turn_item }
}

/// First element of a stable descending sort: the earliest of the maxima.
fn first_max<'a, T>(
    items: impl IntoIterator<Item = &'a T>,
    mut compare: impl FnMut(&T, &T) -> Ordering,
) -> Option<&'a T> {
    items.into_iter().fold(None, |best, candidate| match best {
        Some(current) if compare(candidate, current) != Ordering::Greater => Some(current),
        _ => Some(candidate),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentResult {
    pub text: String,
    pub message_id: Option<MessageId>,
    pub turn_item_id: Option<TurnItemId>,
}

pub fn subagent_result_for_run(
    messages: &[OrchestrationV2ConversationMessage],
    turn_items: &[OrchestrationV2TurnItem],
    run: &OrchestrationV2Run,
) -> SubagentResult {
    let for_run = |run_id: Option<&RunId>| run_id == Some(&run.id);
    let message = first_max(
        messages.iter().filter(|candidate| {
            for_run(candidate.run_id.as_ref())
                && candidate.role == OrchestrationV2ConversationMessageRole::Assistant
                && !js_trim(&candidate.text).is_empty()
        }),
        |left, right| left.updated_at.cmp(&right.updated_at),
    );
    let ordinal = |left: &OrchestrationV2TurnItem, right: &OrchestrationV2TurnItem| {
        turn_item_common(left)
            .ordinal
            .cmp(&turn_item_common(right).ordinal)
    };
    let turn_item = first_max(
        turn_items.iter().filter(|candidate| {
            matches!(
                candidate,
                OrchestrationV2TurnItem::AssistantMessage { run_id, text, .. }
                    if for_run(run_id.as_ref()) && !js_trim(text).is_empty()
            )
        }),
        ordinal,
    );
    let failure = (run.status == OrchestrationV2RunStatus::Failed)
        .then(|| {
            first_max(
                turn_items.iter().filter(|item| {
                    matches!(item, OrchestrationV2TurnItem::Error { run_id, .. } if for_run(run_id.as_ref()))
                }),
                ordinal,
            )
        })
        .flatten();
    let failure_message = failure.and_then(|item| match item {
        OrchestrationV2TurnItem::Error { failure, .. } => Some(failure.message.as_str()),
        _ => None,
    });
    let turn_item_text = turn_item.and_then(|item| match item {
        OrchestrationV2TurnItem::AssistantMessage { text, .. } => Some(text.as_str()),
        _ => None,
    });
    let text = failure_message
        .or(message.map(|message| message.text.as_str()))
        .or(turn_item_text)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if run.status == OrchestrationV2RunStatus::Completed {
                "Child task completed without an assistant result.".into()
            } else {
                format!("Child task ended with status {}.", enum_text(&run.status))
            }
        });
    let turn_item_message_id = turn_item.and_then(|item| match item {
        OrchestrationV2TurnItem::AssistantMessage { message_id, .. } => Some(message_id.clone()),
        _ => None,
    });
    SubagentResult {
        text,
        message_id: match failure {
            None => message
                .map(|message| message.id.clone())
                .or(turn_item_message_id),
            Some(_) => None,
        },
        turn_item_id: failure
            .or(turn_item)
            .map(|item| turn_item_common(item).id.clone()),
    }
}

fn enum_text(value: &impl serde::Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        _ => String::new(),
    }
}

/// `runRanAfter` from upstream/packages/shared/src/orchestrationV2ThreadError.ts.
///
/// Whether started `run` ran after `other`. Ordinals follow submission, but a
/// run can start ahead of a held queue (a restart continuation, or a message
/// sent while the queue is held), so a queued run resumed later can have a
/// lower ordinal than one that already ended. An unfinished run is the latest.
pub fn run_ran_after(run: &OrchestrationV2Run, other: &OrchestrationV2Run) -> bool {
    let end = |candidate: &OrchestrationV2Run| {
        candidate
            .completed_at
            .map(|at| at.as_datetime().timestamp_millis())
    };
    let cmp_end = |left: Option<i64>, right: Option<i64>| match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => left.cmp(&right),
    };
    match cmp_end(end(run), end(other)) {
        Ordering::Equal => run.ordinal.get() > other.ordinal.get(),
        ordering => ordering == Ordering::Greater,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegatedTaskProgressState {
    Working,
    WaitingForChildren,
    ResultAvailable,
}

#[derive(Debug, Clone, Copy)]
pub struct DelegatedTaskProgress<'a> {
    pub state: DelegatedTaskProgressState,
    pub result_run: Option<&'a OrchestrationV2Run>,
}

#[derive(Debug, Clone, Copy)]
pub struct DelegatedTaskProgressInput<'a> {
    pub runs: &'a [OrchestrationV2Run],
    pub messages: &'a [OrchestrationV2ConversationMessage],
    pub subagents: &'a [OrchestrationV2Subagent],
    pub provider_threads: &'a [OrchestrationV2ProviderThread],
}

impl<'a> From<&'a OrchestrationV2ThreadProjection> for DelegatedTaskProgressInput<'a> {
    fn from(projection: &'a OrchestrationV2ThreadProjection) -> Self {
        Self {
            runs: &projection.runs,
            messages: &projection.messages,
            subagents: &projection.subagents,
            provider_threads: &projection.provider_threads,
        }
    }
}

fn subagent_work_active(status: OrchestrationV2SubagentStatus) -> bool {
    matches!(
        status,
        OrchestrationV2SubagentStatus::Pending
            | OrchestrationV2SubagentStatus::Running
            | OrchestrationV2SubagentStatus::Waiting
    )
}

fn turn_item_work_active(status: OrchestrationV2TurnItemStatus) -> bool {
    matches!(
        status,
        OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting
    )
}

/// A finished turn can still own live children or queued completion follow-ups.
pub fn delegated_task_progress(input: DelegatedTaskProgressInput<'_>) -> DelegatedTaskProgress<'_> {
    let terminal = |status: OrchestrationV2RunStatus| {
        matches!(
            status,
            OrchestrationV2RunStatus::Completed
                | OrchestrationV2RunStatus::Failed
                | OrchestrationV2RunStatus::Cancelled
                | OrchestrationV2RunStatus::Interrupted
                | OrchestrationV2RunStatus::RolledBack
        )
    };
    let monitor_runs: HashSet<Option<&RunId>> = input
        .messages
        .iter()
        .filter(|message| {
            message.notification.as_ref().is_some_and(|notification| {
                matches!(
                    notification.source,
                    OrchestrationV2NotificationSource::Monitor
                )
            })
        })
        .map(|message| message.run_id.as_ref())
        .collect();
    let work_runs: Vec<&OrchestrationV2Run> = input
        .runs
        .iter()
        .filter(|run| {
            !monitor_runs.contains(&Some(&run.id))
                && run.status != OrchestrationV2RunStatus::RolledBack
        })
        .collect();
    // A held queue waits for the user to resume it (after Stop, a restart, or a
    // provider failure), so its runs are not work the task still owes.
    let active = work_runs.iter().any(|run| {
        !terminal(run.status)
            && !(run.status == OrchestrationV2RunStatus::Queued && run.queue_held == Some(true))
    });
    let children = input.subagents.iter().any(|task| {
        subagent_work_active(task.status)
            // Publishing a child's result precedes scheduling its parent's wake.
            // The parent still owes that follow-up even between those transactions.
            || task.completion_delivery.as_ref().is_some_and(|delivery| {
                matches!(
                    delivery.state,
                    OrchestrationV2DelegatedCompletionTaskDeliveryState::Pending
                        | OrchestrationV2DelegatedCompletionTaskDeliveryState::Claimed
                )
            })
    }) || input
        .provider_threads
        .iter()
        .any(|thread| !thread.pending_background_tasks.is_empty());
    let result_run = first_max(
        work_runs.into_iter().filter(|run| {
            terminal(run.status) && (run.started_at.is_some() || run.ordinal.get() == 1)
        }),
        |left, right| {
            if run_ran_after(left, right) {
                Ordering::Greater
            } else if run_ran_after(right, left) {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        },
    );
    let state = if active || result_run.is_none() {
        DelegatedTaskProgressState::Working
    } else if children {
        DelegatedTaskProgressState::WaitingForChildren
    } else {
        DelegatedTaskProgressState::ResultAvailable
    };
    DelegatedTaskProgress { state, result_run }
}

/// `"completed" | "failed" | "interrupted" | "cancelled"`: how recovery ends work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedWorkStatus {
    Completed,
    Failed,
    Interrupted,
    Cancelled,
}

impl EndedWorkStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }

    fn subagent(self) -> OrchestrationV2SubagentStatus {
        match self {
            Self::Completed => OrchestrationV2SubagentStatus::Completed,
            Self::Failed => OrchestrationV2SubagentStatus::Failed,
            Self::Interrupted => OrchestrationV2SubagentStatus::Interrupted,
            Self::Cancelled => OrchestrationV2SubagentStatus::Cancelled,
        }
    }

    fn node(self) -> OrchestrationV2ExecutionNodeStatus {
        match self {
            Self::Completed => OrchestrationV2ExecutionNodeStatus::Completed,
            Self::Failed => OrchestrationV2ExecutionNodeStatus::Failed,
            Self::Interrupted => OrchestrationV2ExecutionNodeStatus::Interrupted,
            Self::Cancelled => OrchestrationV2ExecutionNodeStatus::Cancelled,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WrittenType {
    Subagent,
    Node,
    TurnItem,
}

/// Ids that `emitted` already wrote with an event of `type`.
fn written_ids(emitted: &[OrchestrationV2DomainEvent], kind: WrittenType) -> HashSet<String> {
    emitted
        .iter()
        .filter_map(|event| match (kind, event) {
            (
                WrittenType::Subagent,
                OrchestrationV2DomainEvent::SubagentUpdated { payload, .. },
            ) => Some(payload.id.as_str().to_owned()),
            (WrittenType::Node, OrchestrationV2DomainEvent::NodeUpdated { payload, .. }) => {
                Some(payload.id.as_str().to_owned())
            }
            (
                WrittenType::TurnItem,
                OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. },
            ) => Some(turn_item_common(payload).id.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// `{ ...item, ...patch }` on a turn item, which shares one base shape across members.
fn patch_turn_item(
    item: &OrchestrationV2TurnItem,
    patch: impl IntoIterator<Item = (&'static str, Value)>,
) -> OrchestrationV2TurnItem {
    let mut value = serde_json::to_value(item).expect("turn item encodes");
    let fields = value.as_object_mut().expect("turn item is an object");
    for (key, field) in patch {
        fields.insert(key.to_owned(), field);
    }
    serde_json::from_value(value).expect("patched turn item decodes")
}

#[derive(Debug, Clone)]
pub struct EndOrphanedNativeSubagentInput<'a> {
    pub nodes: &'a [OrchestrationV2ExecutionNode],
    pub subagents: &'a [OrchestrationV2Subagent],
    pub turn_items: &'a [OrchestrationV2TurnItem],
    pub subagent_id: &'a NodeId,
    pub status: EndedWorkStatus,
    pub completed_at: Option<UtcDateTime>,
    /// `undefined` (keep the recorded result) when `None`; `Some(None)` writes `null`.
    pub result: Option<Option<String>>,
    pub now: UtcDateTime,
    pub emitted: &'a [OrchestrationV2DomainEvent],
}

/// Ends a provider-native subagent, its node and its turn item once the provider
/// process that ran it is gone, since no terminal event will come from it. Each
/// record is skipped if already settled or already ended by `emitted`.
pub async fn end_orphaned_native_subagent<E, Fut>(
    input: EndOrphanedNativeSubagentInput<'_>,
    mut allocate_event_id: impl FnMut() -> Fut,
) -> Result<Vec<OrchestrationV2DomainEvent>, E>
where
    Fut: Future<Output = Result<EventId, E>>,
{
    let EndOrphanedNativeSubagentInput {
        now,
        status,
        subagent_id,
        ..
    } = input;
    let completed_at = input.completed_at.unwrap_or(now);
    let Some(subagent) = input
        .subagents
        .iter()
        .find(|entry| &entry.id == subagent_id)
        .filter(|entry| entry.origin == OrchestrationV2SubagentOrigin::ProviderNative)
    else {
        return Ok(Vec::new());
    };
    let node = input.nodes.iter().find(|entry| &entry.id == subagent_id);
    let item = input.turn_items.iter().find(|entry| {
        matches!(entry, OrchestrationV2TurnItem::Subagent { subagent_id: id, .. } if id == subagent_id)
    });
    let mut events = Vec::new();
    if subagent_work_active(subagent.status)
        && !written_ids(input.emitted, WrittenType::Subagent).contains(subagent_id.as_str())
    {
        let mut payload = subagent.clone();
        payload.status = status.subagent();
        payload.completed_at = Some(completed_at);
        payload.updated_at = now;
        if let Some(result) = &input.result {
            payload.result = result.clone();
        }
        events.push(OrchestrationV2DomainEvent::SubagentUpdated {
            id: allocate_event_id().await?,
            thread_id: subagent.thread_id.clone(),
            run_id: subagent.run_id.clone(),
            node_id: Some(subagent_id.clone()),
            driver: Some(subagent.driver.clone()),
            provider_instance_id: Some(subagent.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload,
        });
    }
    if let Some(node) = node
        && is_orchestration_v2_work_active(node.status)
        && !written_ids(input.emitted, WrittenType::Node).contains(subagent_id.as_str())
    {
        let mut payload = node.clone();
        payload.status = status.node();
        payload.completed_at = Some(completed_at);
        events.push(OrchestrationV2DomainEvent::NodeUpdated {
            id: allocate_event_id().await?,
            thread_id: subagent.thread_id.clone(),
            run_id: subagent.run_id.clone(),
            node_id: Some(subagent_id.clone()),
            driver: None,
            provider_instance_id: Some(subagent.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload,
        });
    }
    if let Some(item) = item {
        let common = turn_item_common(item);
        if turn_item_work_active(common.status)
            && !written_ids(input.emitted, WrittenType::TurnItem).contains(common.id.as_str())
        {
            let mut patch = vec![
                ("status", json!(status.as_str())),
                ("completedAt", json!(completed_at)),
                ("updatedAt", json!(now)),
            ];
            if let Some(result) = &input.result {
                patch.push(("result", json!(result)));
            }
            events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
                id: allocate_event_id().await?,
                thread_id: subagent.thread_id.clone(),
                run_id: subagent.run_id.clone(),
                node_id: Some(subagent_id.clone()),
                driver: None,
                provider_instance_id: Some(subagent.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: patch_turn_item(item, patch),
            });
        }
    }
    Ok(events)
}

#[derive(Debug, Clone, Copy)]
pub struct EndRunlessRootTurnsInput<'a> {
    pub thread_id: &'a ThreadId,
    pub provider_instance_id: &'a ProviderInstanceId,
    pub nodes: &'a [OrchestrationV2ExecutionNode],
    pub turn_items: &'a [OrchestrationV2TurnItem],
    pub status: EndedWorkStatus,
    pub completed_at: Option<UtcDateTime>,
    pub now: UtcDateTime,
    pub emitted: &'a [OrchestrationV2DomainEvent],
}

/// Ends a provider-native subagent thread's work, a runless root turn plus the
/// items under it (Claude's live progress), which only the provider process that
/// ran it could settle. Skips records `emitted` already ended.
pub async fn end_runless_root_turns<E, Fut>(
    input: EndRunlessRootTurnsInput<'_>,
    mut allocate_event_id: impl FnMut() -> Fut,
) -> Result<Vec<OrchestrationV2DomainEvent>, E>
where
    Fut: Future<Output = Result<EventId, E>>,
{
    let EndRunlessRootTurnsInput { now, status, .. } = input;
    let completed_at = input.completed_at.unwrap_or(now);
    let ended_node_ids = written_ids(input.emitted, WrittenType::Node);
    let ended_item_ids = written_ids(input.emitted, WrittenType::TurnItem);
    let mut events = Vec::new();
    for node in input.nodes {
        if node.kind != OrchestrationV2ExecutionNodeKind::RootTurn
            || node.run_id.is_some()
            || !is_orchestration_v2_work_active(node.status)
        {
            continue;
        }
        // Recovery may have ended this node through one of its items; its other
        // items still need ending.
        if !ended_node_ids.contains(node.id.as_str()) {
            let mut payload = node.clone();
            payload.status = status.node();
            payload.completed_at = Some(completed_at);
            events.push(OrchestrationV2DomainEvent::NodeUpdated {
                id: allocate_event_id().await?,
                thread_id: input.thread_id.clone(),
                run_id: None,
                node_id: Some(node.id.clone()),
                driver: None,
                provider_instance_id: Some(input.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
        }
        for item in input.turn_items {
            let common = turn_item_common(item);
            if common.node_id != Some(&node.id)
                || common.run_id.is_some()
                || !turn_item_work_active(common.status)
                || ended_item_ids.contains(common.id.as_str())
            {
                continue;
            }
            let item_status = if status == EndedWorkStatus::Completed
                && matches!(
                    item,
                    OrchestrationV2TurnItem::CommandExecution { .. }
                        | OrchestrationV2TurnItem::DynamicTool { .. }
                ) {
                EndedWorkStatus::Failed
            } else {
                status
            };
            let mut patch = vec![
                ("status", json!(item_status.as_str())),
                ("completedAt", json!(completed_at)),
                ("updatedAt", json!(now)),
            ];
            if matches!(
                item,
                OrchestrationV2TurnItem::Reasoning { .. }
                    | OrchestrationV2TurnItem::AssistantMessage { .. }
            ) {
                patch.push(("streaming", json!(false)));
            }
            events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
                id: allocate_event_id().await?,
                thread_id: input.thread_id.clone(),
                run_id: None,
                node_id: Some(node.id.clone()),
                driver: None,
                provider_instance_id: Some(input.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: patch_turn_item(item, patch),
            });
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::convert::Infallible;

    const PARENT_THREAD_ID: &str = "thread:subagent-snoozed-parent";
    const CHILD_THREAD_ID: &str = "thread:subagent-awake-child";
    const PARENT_CREATED_AT: &str = "2026-07-24T09:00:00.000Z";
    const SNOOZED_AT: &str = "2026-07-24T09:05:00.000Z";
    const SNOOZED_UNTIL: &str = "2026-07-25T09:00:00.000Z";
    const CHILD_CREATED_AT: &str = "2026-07-24T09:10:00.000Z";

    fn at(value: &str) -> UtcDateTime {
        serde_json::from_value(json!(value)).unwrap()
    }

    fn decode<T: serde::de::DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    fn parent_model_selection() -> Value {
        json!({ "instanceId": "codex", "model": "gpt-5.4" })
    }

    fn make_parent_thread() -> OrchestrationV2AppThread {
        decode(json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": PARENT_THREAD_ID,
            "projectId": "project:subagent-snooze",
            "title": "Snoozed parent",
            "providerInstanceId": "codex",
            "modelSelection": parent_model_selection(),
            "runtimeMode": "full-access",
            "interactionMode": "plan",
            "branch": "feature/source",
            "worktreePath": "/tmp/source-worktree",
            "activeProviderThreadId": "provider-thread:subagent-snoozed-parent",
            "lineage": {
                "parentThreadId": null,
                "relationshipToParent": null,
                "rootThreadId": PARENT_THREAD_ID,
            },
            "forkedFrom": null,
            "createdAt": PARENT_CREATED_AT,
            "updatedAt": SNOOZED_AT,
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "lastVisitedAt": null,
            "snoozedUntil": SNOOZED_UNTIL,
            "snoozedAt": SNOOZED_AT,
            "deletedAt": null,
            "historyOrigin": "v1_import",
        }))
    }

    #[test]
    fn keeps_a_subagent_child_awake_when_its_parent_thread_is_snoozed() {
        let parent_thread = make_parent_thread();
        let child_model_selection: ModelSelection =
            decode(json!({ "instanceId": "claude", "model": "claude-opus-4-1" }));
        let child = make_subagent_child_thread(SubagentChildThreadInput {
            parent_thread: &parent_thread,
            child_thread_id: ThreadId::new(CHILD_THREAD_ID).unwrap(),
            parent_node_id: NodeId::new("node:subagent-parent").unwrap(),
            active_provider_thread_id: Some(
                ProviderThreadId::new("provider-thread:subagent-awake-child").unwrap(),
            ),
            provider_instance_id: ProviderInstanceId::new("claude").unwrap(),
            model_selection: child_model_selection.clone(),
            title: TrimmedNonEmptyString::new("Awake child").unwrap(),
            now: at(CHILD_CREATED_AT),
            created_by: OrchestrationV2Actor::Agent,
            creation_source: OrchestrationV2CreationSource::Provider,
        });
        let encoded = serde_json::to_value(&child).unwrap();
        assert_eq!(encoded["snoozedUntil"], Value::Null);
        assert_eq!(encoded["snoozedAt"], Value::Null);
        assert_eq!(child.project_id, parent_thread.project_id);
        assert_eq!(encoded["runtimeMode"], json!("full-access"));
        assert_eq!(encoded["interactionMode"], json!("plan"));
        assert_eq!(encoded["branch"], json!("feature/source"));
        assert_eq!(encoded["worktreePath"], json!("/tmp/source-worktree"));
        assert_eq!(child.provider_instance_id.as_str(), "claude");
        assert_eq!(child.model_selection, child_model_selection);
        assert_eq!(
            encoded["activeProviderThreadId"],
            json!("provider-thread:subagent-awake-child")
        );
        assert!(encoded.get("historyOrigin").is_none());
        assert_eq!(
            encoded["lineage"],
            json!({
                "parentThreadId": PARENT_THREAD_ID,
                "relationshipToParent": "subagent",
                "rootThreadId": PARENT_THREAD_ID,
            })
        );
        assert_eq!(
            encoded["forkedFrom"],
            json!({ "type": "node", "nodeId": "node:subagent-parent" })
        );
    }

    fn artifacts(
        message_id: &str,
        turn_item_id: &str,
        thread_id: &str,
        sender: Option<&str>,
        role: OrchestrationV2ConversationMessageRole,
        text: &str,
        ordinal: u64,
    ) -> SubagentConversationArtifacts {
        make_subagent_conversation_artifacts(SubagentConversationArtifactsInput {
            message_id: MessageId::new(message_id).unwrap(),
            sender_thread_id: sender.map(|id| ThreadId::new(id).unwrap()),
            turn_item_id: TurnItemId::new(turn_item_id).unwrap(),
            thread_id: ThreadId::new(thread_id).unwrap(),
            root_node_id: NodeId::new("root").unwrap(),
            provider_thread_id: None,
            provider_turn_id: None,
            native_item_ref: None,
            role,
            text: text.into(),
            ordinal,
            now: at(CHILD_CREATED_AT),
        })
    }

    #[test]
    fn attributes_native_subagent_prompts_to_their_parent_thread() {
        for role in [
            OrchestrationV2ConversationMessageRole::User,
            OrchestrationV2ConversationMessageRole::Assistant,
        ] {
            let is_user = role == OrchestrationV2ConversationMessageRole::User;
            let artifacts = artifacts(
                "native",
                "native",
                CHILD_THREAD_ID,
                Some(PARENT_THREAD_ID),
                role,
                if is_user {
                    "Review the changes"
                } else {
                    "Review complete"
                },
                100,
            );
            assert_eq!(artifacts.message.thread_id.as_str(), CHILD_THREAD_ID);
            assert_eq!(
                artifacts.message.sender_thread_id,
                is_user.then(|| ThreadId::new(PARENT_THREAD_ID).unwrap())
            );
            let item = serde_json::to_value(&artifacts.turn_item).unwrap();
            if is_user {
                assert_eq!(item["type"], json!("user_message"));
                assert_eq!(item["senderThreadId"], json!(PARENT_THREAD_ID));
            } else {
                assert_eq!(item["type"], json!("assistant_message"));
            }
        }
    }

    fn run(id: &str, ordinal: u64, status: &str) -> Value {
        json!({
            "id": id,
            "threadId": PARENT_THREAD_ID,
            "ordinal": ordinal,
            "providerInstanceId": "codex",
            "modelSelection": parent_model_selection(),
            "providerThreadId": null,
            "userMessageId": "prompt",
            "rootNodeId": "root",
            "activeAttemptId": "attempt",
            "status": status,
            "requestedAt": PARENT_CREATED_AT,
            "startedAt": PARENT_CREATED_AT,
            "completedAt": CHILD_CREATED_AT,
            "checkpointId": null,
            "contextHandoffId": null,
        })
    }

    fn with(mut value: Value, patch: Value) -> Value {
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        value
    }

    fn subagent(patch: Value) -> OrchestrationV2Subagent {
        decode(with(
            json!({
                "id": "node:subagent",
                "threadId": PARENT_THREAD_ID,
                "runId": null,
                "parentNodeId": "root",
                "origin": "provider_native",
                "createdBy": "agent",
                "driver": "codex",
                "providerInstanceId": "codex",
                "providerThreadId": null,
                "childThreadId": null,
                "nativeTaskRef": null,
                "prompt": "Audit",
                "title": null,
                "model": null,
                "status": "running",
                "result": null,
                "startedAt": null,
                "completedAt": null,
                "updatedAt": PARENT_CREATED_AT,
            }),
            patch,
        ))
    }

    fn provider_thread(pending_background_tasks: Value) -> OrchestrationV2ProviderThread {
        decode(json!({
            "id": "provider-thread:parent",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": null,
            "appThreadId": PARENT_THREAD_ID,
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "active",
            "firstRunOrdinal": null,
            "lastRunOrdinal": null,
            "handoffIds": [],
            "forkedFrom": null,
            "pendingBackgroundTasks": pending_background_tasks,
            "createdAt": PARENT_CREATED_AT,
            "updatedAt": PARENT_CREATED_AT,
        }))
    }

    fn progress<'a>(
        runs: &'a [OrchestrationV2Run],
        messages: &'a [OrchestrationV2ConversationMessage],
        subagents: &'a [OrchestrationV2Subagent],
        provider_threads: &'a [OrchestrationV2ProviderThread],
    ) -> DelegatedTaskProgress<'a> {
        delegated_task_progress(DelegatedTaskProgressInput {
            runs,
            messages,
            subagents,
            provider_threads,
        })
    }

    #[test]
    fn reports_the_run_that_ended_last_not_the_highest_ordinal() {
        // A restart continuation (ordinal 4) ran ahead of held queued runs 2 and 3.
        let ended = |ordinal: u64, completed_at: &str| -> OrchestrationV2Run {
            decode(with(
                run(&format!("run:{ordinal}"), ordinal, "completed"),
                json!({ "completedAt": completed_at }),
            ))
        };
        let runs = [
            decode(run("initial", 1, "cancelled")),
            ended(4, "2026-07-24T10:00:00.000Z"),
            ended(2, "2026-07-24T10:05:00.000Z"),
            ended(3, "2026-07-24T10:10:00.000Z"),
        ];
        let result = progress(&runs, &[], &[], &[]);
        assert_eq!(result.state, DelegatedTaskProgressState::ResultAvailable);
        assert_eq!(result.result_run.map(|run| run.ordinal.get()), Some(3));
    }

    #[test]
    fn waits_for_nested_work_and_retains_the_report_across_monitor_acknowledgements() {
        let initial: OrchestrationV2Run = decode(run("initial", 1, "completed"));
        let runs = [initial.clone()];
        let state =
            |subagents: &[OrchestrationV2Subagent]| progress(&runs, &[], subagents, &[]).state;
        assert_eq!(
            state(&[subagent(json!({ "status": "running" }))]),
            DelegatedTaskProgressState::WaitingForChildren
        );
        assert_eq!(
            state(&[subagent(json!({ "status": "completed" }))]),
            DelegatedTaskProgressState::ResultAvailable
        );
        assert_eq!(
            state(&[subagent(json!({ "status": "idle" }))]),
            DelegatedTaskProgressState::ResultAvailable
        );
        for delivery in ["pending", "claimed"] {
            assert_eq!(
                state(&[subagent(json!({
                    "status": "completed",
                    "completionDelivery": { "state": delivery, "observedByRunId": null },
                }))]),
                DelegatedTaskProgressState::WaitingForChildren
            );
        }
        for delivery in ["acknowledged", "delivered", "disposed"] {
            assert_eq!(
                state(&[subagent(json!({
                    "status": "completed",
                    "completionDelivery": { "state": delivery, "observedByRunId": null },
                }))]),
                DelegatedTaskProgressState::ResultAvailable
            );
        }
        assert_eq!(
            progress(
                &runs,
                &[],
                &[],
                &[provider_thread(json!([
                    { "taskId": "background-audit", "kind": "command" }
                ]))]
            )
            .state,
            DelegatedTaskProgressState::WaitingForChildren
        );

        let pending = with(
            run("followup", 2, "queued"),
            json!({ "startedAt": null, "completedAt": CHILD_CREATED_AT }),
        );
        let queued = [initial.clone(), decode(pending.clone())];
        assert_eq!(
            progress(&queued, &[], &[], &[]).state,
            DelegatedTaskProgressState::Working
        );
        // Stop or a restart holds queued wakes for the user; they are not work the task owes.
        let cancelled: OrchestrationV2Run = decode(run("initial", 1, "cancelled"));
        let held_runs = [
            cancelled.clone(),
            decode(with(pending.clone(), json!({ "queueHeld": true }))),
        ];
        let held = progress(&held_runs, &[], &[], &[]);
        assert_eq!(held.state, DelegatedTaskProgressState::ResultAvailable);
        assert_eq!(held.result_run.map(|run| &run.id), Some(&cancelled.id));

        let report = with(
            pending,
            json!({ "status": "completed", "startedAt": PARENT_CREATED_AT }),
        );
        let monitor = with(report.clone(), json!({ "id": "monitor", "ordinal": 3 }));
        let mut message = serde_json::to_value(
            artifacts(
                "monitor-message",
                "monitor-item",
                PARENT_THREAD_ID,
                None,
                OrchestrationV2ConversationMessageRole::User,
                "Monitor update",
                3,
            )
            .message,
        )
        .unwrap();
        message["runId"] = json!("monitor");
        message["notification"] = json!({
            "source": { "kind": "monitor" },
            "outcome": "updated",
            "summary": "Monitor update",
        });
        let runs = [initial, decode(report), decode(monitor)];
        let messages = [decode(message)];
        let result = progress(&runs, &messages, &[], &[]);
        assert_eq!(result.state, DelegatedTaskProgressState::ResultAvailable);
        assert_eq!(
            result.result_run.map(|run| run.id.as_str()),
            Some("followup")
        );
    }

    #[test]
    fn exposes_the_provider_failure_rather_than_a_progress_message_from_the_failed_run() {
        let run_value = run("initial", 1, "completed");
        let failed_run: OrchestrationV2Run = decode(with(run_value, json!({ "status": "failed" })));
        let artifacts = artifacts(
            "progress",
            "error",
            PARENT_THREAD_ID,
            None,
            OrchestrationV2ConversationMessageRole::Assistant,
            "Starting the audit.",
            1,
        );
        let failure = json!({
            "class": "unknown",
            "message": "You've hit your usage limit. Try again Sep 14 at 7:20 PM.",
            "code": null,
            "retryable": false,
        });
        let mut message = serde_json::to_value(&artifacts.message).unwrap();
        message["runId"] = json!("initial");
        let mut item = serde_json::to_value(&artifacts.turn_item).unwrap();
        let fields = item.as_object_mut().unwrap();
        fields.remove("messageId");
        fields.remove("text");
        fields.remove("streaming");
        fields.insert("type".into(), json!("error"));
        fields.insert("runId".into(), json!("initial"));
        fields.insert("failure".into(), failure.clone());
        let result = subagent_result_for_run(&[decode(message)], &[decode(item)], &failed_run);
        assert_eq!(result.text, failure["message"].as_str().unwrap());
        assert_eq!(result.turn_item_id, Some(TurnItemId::new("error").unwrap()));
        assert_eq!(result.message_id, None);
    }

    #[test]
    fn prefers_the_earliest_of_equally_recent_assistant_messages() {
        let completed: OrchestrationV2Run = decode(run("initial", 1, "completed"));
        let message = |id: &str| {
            let mut value = serde_json::to_value(
                artifacts(
                    id,
                    id,
                    PARENT_THREAD_ID,
                    None,
                    OrchestrationV2ConversationMessageRole::Assistant,
                    id,
                    1,
                )
                .message,
            )
            .unwrap();
            value["runId"] = json!("initial");
            decode::<OrchestrationV2ConversationMessage>(value)
        };
        let result =
            subagent_result_for_run(&[message("first"), message("second")], &[], &completed);
        assert_eq!(result.text, "first");
        assert_eq!(result.message_id, Some(MessageId::new("first").unwrap()));
        assert_eq!(
            subagent_result_for_run(&[], &[], &completed).text,
            "Child task completed without an assistant result."
        );
        let interrupted: OrchestrationV2Run = decode(run("initial", 1, "interrupted"));
        assert_eq!(
            subagent_result_for_run(&[], &[], &interrupted).text,
            "Child task ended with status interrupted."
        );
    }

    #[test]
    fn subagent_thread_titles_clip_like_js() {
        assert_eq!(
            subagent_thread_title("Parent", None, "  ", 2),
            "Parent subagent 2"
        );
        assert_eq!(
            subagent_thread_title("Parent", Some(" Title "), "prompt", 1),
            "Title"
        );
        let long = "a".repeat(73);
        assert_eq!(
            subagent_thread_title("Parent", None, &long, 1),
            format!("{}...", "a".repeat(69))
        );
        assert_eq!(
            subagent_thread_title("Parent", None, &"a".repeat(72), 1).len(),
            72
        );
    }

    fn node(id: &str, kind: &str) -> OrchestrationV2ExecutionNode {
        decode(json!({
            "id": id,
            "threadId": CHILD_THREAD_ID,
            "runId": null,
            "parentNodeId": null,
            "rootNodeId": id,
            "kind": kind,
            "status": "running",
            "countsForRun": false,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "runtimeRequestId": null,
            "checkpointScopeId": null,
            "startedAt": null,
            "completedAt": null,
        }))
    }

    fn running_item(id: &str, node_id: &str, kind: &str) -> OrchestrationV2TurnItem {
        let mut value = json!({
            "id": id,
            "threadId": CHILD_THREAD_ID,
            "runId": null,
            "nodeId": node_id,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": 1,
            "status": "running",
            "title": null,
            "startedAt": null,
            "completedAt": null,
            "updatedAt": PARENT_CREATED_AT,
            "type": kind,
        });
        let extra = match kind {
            "reasoning" => json!({ "text": "thinking", "streaming": true }),
            "command_execution" => json!({ "input": "ls", "output": "" }),
            _ => json!({}),
        };
        value = with(value, extra);
        decode(value)
    }

    fn allocate() -> impl FnMut() -> std::future::Ready<Result<EventId, Infallible>> {
        let mut next = 0;
        move || {
            next += 1;
            std::future::ready(Ok(EventId::new(format!("event:{next}")).unwrap()))
        }
    }

    fn event_summary(event: &OrchestrationV2DomainEvent) -> (String, String) {
        let value = serde_json::to_value(event).unwrap();
        (
            value["type"].as_str().unwrap().to_owned(),
            format!(
                "{} {}",
                value["payload"]["id"].as_str().unwrap(),
                value["payload"]["status"].as_str().unwrap()
            ),
        )
    }

    #[tokio::test]
    async fn ends_an_orphaned_native_subagents_node_when_only_its_record_already_ended() {
        let id = NodeId::new("node:orphaned-native-subagent").unwrap();
        let record = subagent(json!({ "id": id }));
        let mut ended = record.clone();
        ended.status = OrchestrationV2SubagentStatus::Cancelled;
        let emitted = [decode(json!({
            "type": "subagent.updated",
            "id": "event:previous",
            "threadId": PARENT_THREAD_ID,
            "occurredAt": CHILD_CREATED_AT,
            "payload": ended,
        }))];
        let nodes = [node(id.as_str(), "subagent")];
        let events = end_orphaned_native_subagent(
            EndOrphanedNativeSubagentInput {
                nodes: &nodes,
                subagents: &[record],
                turn_items: &[],
                subagent_id: &id,
                status: EndedWorkStatus::Cancelled,
                completed_at: None,
                result: None,
                now: at(CHILD_CREATED_AT),
                emitted: &emitted,
            },
            allocate(),
        )
        .await
        .unwrap();
        assert_eq!(
            events.iter().map(event_summary).collect::<Vec<_>>(),
            [(
                "node.updated".to_owned(),
                "node:orphaned-native-subagent cancelled".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn ignores_app_owned_subagents() {
        let id = NodeId::new("node:app-owned").unwrap();
        let events = end_orphaned_native_subagent(
            EndOrphanedNativeSubagentInput {
                nodes: &[node(id.as_str(), "subagent")],
                subagents: &[subagent(json!({ "id": id, "origin": "app_owned" }))],
                turn_items: &[],
                subagent_id: &id,
                status: EndedWorkStatus::Cancelled,
                completed_at: None,
                result: None,
                now: at(CHILD_CREATED_AT),
                emitted: &[],
            },
            allocate(),
        )
        .await
        .unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn ends_a_runless_root_turns_items_when_its_node_already_ended() {
        let root = node("node:runless-root", "root_turn");
        let command = running_item("turn-item:command", root.id.as_str(), "command_execution");
        let reasoning = running_item("turn-item:reasoning", root.id.as_str(), "reasoning");
        let ended_command = patch_turn_item(&command, [("status", json!("cancelled"))]);
        let mut ended_root = root.clone();
        ended_root.status = OrchestrationV2ExecutionNodeStatus::Cancelled;
        let emitted: Vec<OrchestrationV2DomainEvent> = vec![
            decode(json!({
                "type": "turn-item.updated",
                "id": "event:a",
                "threadId": CHILD_THREAD_ID,
                "occurredAt": CHILD_CREATED_AT,
                "payload": ended_command,
            })),
            decode(json!({
                "type": "node.updated",
                "id": "event:b",
                "threadId": CHILD_THREAD_ID,
                "occurredAt": CHILD_CREATED_AT,
                "payload": ended_root,
            })),
        ];
        let events = end_runless_root_turns(
            EndRunlessRootTurnsInput {
                thread_id: &ThreadId::new(CHILD_THREAD_ID).unwrap(),
                provider_instance_id: &ProviderInstanceId::new("codex").unwrap(),
                nodes: &[root],
                turn_items: &[command, reasoning],
                status: EndedWorkStatus::Cancelled,
                completed_at: None,
                now: at(CHILD_CREATED_AT),
                emitted: &emitted,
            },
            allocate(),
        )
        .await
        .unwrap();
        assert_eq!(
            events.iter().map(event_summary).collect::<Vec<_>>(),
            [(
                "turn-item.updated".to_owned(),
                "turn-item:reasoning cancelled".to_owned()
            )]
        );
        let payload = serde_json::to_value(&events[0]).unwrap()["payload"].clone();
        assert_eq!(payload["streaming"], json!(false));
    }

    #[tokio::test]
    async fn completed_runless_tool_items_end_as_failed() {
        let root = node("node:runless-root", "root_turn");
        let command = running_item("turn-item:command", root.id.as_str(), "command_execution");
        let events = end_runless_root_turns(
            EndRunlessRootTurnsInput {
                thread_id: &ThreadId::new(CHILD_THREAD_ID).unwrap(),
                provider_instance_id: &ProviderInstanceId::new("codex").unwrap(),
                nodes: &[root],
                turn_items: &[command],
                status: EndedWorkStatus::Completed,
                completed_at: None,
                now: at(CHILD_CREATED_AT),
                emitted: &[],
            },
            allocate(),
        )
        .await
        .unwrap();
        assert_eq!(
            events.iter().map(event_summary).collect::<Vec<_>>(),
            [
                (
                    "node.updated".to_owned(),
                    "node:runless-root completed".to_owned()
                ),
                (
                    "turn-item.updated".to_owned(),
                    "turn-item:command failed".to_owned()
                ),
            ]
        );
    }
}
