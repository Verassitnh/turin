//! Port of upstream/apps/web/src/components/chat/MessagesTimeline.logic.test.ts
//! and MessagesTimeline.active-tools.test.ts, split by upstream `describe`
//! block. Shared fixture builders live here (upstream inlines object
//! literals; Rust builds them with these helpers plus struct update syntax).
//!
//! `makeStreamingTimelineFixture` is a port of the helper in
//! upstream/apps/web/src/test-fixtures.ts.

#![allow(clippy::too_many_lines)]

use std::collections::HashSet;
use std::sync::Arc;

pub(super) use serde_json::{Value, json};

pub(super) use super::*;
pub(super) use crate::components::chat::timeline::session_logic::{
    TimelineEntriesInput, derive_timeline_entries_from_visible_turn_items,
};
pub(super) use crate::components::chat::timeline::turn_item::{
    CheckpointFileSummary, ProjectedTurnItem, ProjectedVisibility, ProviderFailure,
    ProviderFailureClass, ProviderRequestKind, ProviderRetry, ToolActivityIcon,
    ToolActivitySurface, TurnItem, TurnItemPayload, TurnItemStatus, TurnItemType,
};
pub(super) use crate::components::chat::timeline::types::{
    Actor, AttemptStatus, ChatMessage, CheckpointStatus, CreationSource, InputIntent, MessageRole,
    RunStatus, TimelineAttempt, TimelineEntry, TimelineEntryKind, TimelineLatestRun,
    ToolLifecycleStatus, TurnDiffSummary, WorkLogEntry, WorkTone,
};
pub(super) use MessagesTimelineRowContent as Row;

mod derive_rows;
mod derive_rows_compaction;
mod derive_rows_folds;
mod derive_rows_live;
mod derive_rows_messages;
mod derive_rows_metadata;
mod derive_rows_runless;
mod derive_rows_steers;
mod derive_rows_timing;
mod helpers;
mod stable_rows;

// ---------------------------------------------------------------------------
// Timeline entries
// ---------------------------------------------------------------------------

/// A `ChatMessage` with every optional field empty. Upstream literals that
/// omit `runId` / `inputIntent` leave them `undefined`, which is `None` here.
pub(super) fn chat_message(
    id: &str,
    role: MessageRole,
    text: &str,
    run_id: Option<&str>,
    created_at: &str,
    updated_at: &str,
    streaming: bool,
) -> ChatMessage {
    ChatMessage {
        id: id.to_string(),
        role,
        text: text.to_string(),
        attachments: Vec::new(),
        run_id: run_id.map(str::to_string),
        streaming,
        created_by: None,
        creation_source: None,
        scheduled_task_id: None,
        sender_thread_id: None,
        created_at: created_at.to_string(),
        updated_at: updated_at.to_string(),
        input_intent: None,
    }
}

pub(super) fn user_message(
    id: &str,
    text: &str,
    run_id: Option<&str>,
    created_at: &str,
) -> ChatMessage {
    chat_message(
        id,
        MessageRole::User,
        text,
        run_id,
        created_at,
        created_at,
        false,
    )
}

pub(super) fn assistant_message(
    id: &str,
    text: &str,
    run_id: Option<&str>,
    created_at: &str,
    updated_at: &str,
    streaming: bool,
) -> ChatMessage {
    chat_message(
        id,
        MessageRole::Assistant,
        text,
        run_id,
        created_at,
        updated_at,
        streaming,
    )
}

/// `{ id, kind: "message", createdAt, message }`.
pub(super) fn message_entry(id: &str, created_at: &str, message: ChatMessage) -> TimelineEntry {
    TimelineEntry {
        id: id.to_string(),
        created_at: created_at.to_string(),
        kind: TimelineEntryKind::Message {
            message,
            projected_item: None,
        },
        attempt: None,
    }
}

/// `{ id, kind: "work", createdAt, entry }`.
pub(super) fn work_entry(id: &str, created_at: &str, entry: WorkLogEntry) -> TimelineEntry {
    TimelineEntry {
        id: id.to_string(),
        created_at: created_at.to_string(),
        kind: TimelineEntryKind::Work { entry },
        attempt: None,
    }
}

/// `{ id, kind: "event", createdAt, projectedItem }`.
pub(super) fn event_entry(
    id: &str,
    created_at: &str,
    projected_item: ProjectedTurnItem,
) -> TimelineEntry {
    TimelineEntry {
        id: id.to_string(),
        created_at: created_at.to_string(),
        kind: TimelineEntryKind::Event { projected_item },
        attempt: None,
    }
}

/// A `WorkLogEntry` with a run id (most upstream fixtures set one).
pub(super) fn work_log(
    id: &str,
    created_at: &str,
    run_id: Option<&str>,
    label: &str,
    tone: WorkTone,
) -> WorkLogEntry {
    WorkLogEntry {
        run_id: run_id.map(str::to_string),
        ..WorkLogEntry::new(id, created_at, label, tone)
    }
}

pub(super) fn attempt(
    id: &str,
    run_id: &str,
    status: AttemptStatus,
    completed_at: Option<&str>,
) -> TimelineAttempt {
    TimelineAttempt {
        id: id.to_string(),
        run_id: run_id.to_string(),
        attempt_ordinal: 1,
        root_node_id: format!("{id}-root"),
        status,
        completed_at: completed_at.map(str::to_string),
    }
}

pub(super) fn latest_run(
    run_id: &str,
    status: RunStatus,
    started_at: Option<&str>,
    completed_at: Option<&str>,
) -> TimelineLatestRun {
    TimelineLatestRun {
        run_id: run_id.to_string(),
        status,
        started_at: started_at.map(str::to_string),
        completed_at: completed_at.map(str::to_string),
    }
}

pub(super) fn set(values: &[&str]) -> HashSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

/// `{ timelineEntries, isWorking, activeTurnStartedAt: null, turnDiffSummaries: [],
/// supportsConversationRollback: false }` — fill the rest with struct update syntax.
pub(super) fn rows_input(
    timeline_entries: Vec<TimelineEntry>,
    is_working: bool,
) -> MessagesTimelineRowsInput {
    MessagesTimelineRowsInput {
        timeline_entries: Arc::new(timeline_entries),
        is_working,
        ..Default::default()
    }
}

pub(super) fn derive(input: &MessagesTimelineRowsInput) -> Vec<Arc<MessagesTimelineRow>> {
    derive_messages_timeline_rows(input)
}

// ---------------------------------------------------------------------------
// Turn items
// ---------------------------------------------------------------------------

/// A completed turn item in `thread-1` with no run, node, or timestamps
/// other than `updated_at`.
pub(super) fn turn_item(id: &str, updated_at: &str, payload: TurnItemPayload) -> TurnItem {
    TurnItem::new(
        id,
        "thread-1",
        TurnItemStatus::Completed,
        updated_at,
        payload,
    )
}

/// `{ position, visibility: "local", sourceThreadId: item.threadId, sourceItemId: item.id, item }`.
pub(super) fn projected(position: u64, item: TurnItem) -> ProjectedTurnItem {
    ProjectedTurnItem {
        position,
        visibility: ProjectedVisibility::Local,
        source_thread_id: item.thread_id.clone(),
        source_item_id: item.id.clone(),
        item,
    }
}

pub(super) fn user_message_payload(message_id: &str, text: &str) -> TurnItemPayload {
    TurnItemPayload::UserMessage {
        message_id: message_id.to_string(),
        text: text.to_string(),
        created_by: Actor::User,
        creation_source: CreationSource::Web,
        scheduled_task_id: None,
        sender_thread_id: None,
        input_intent: InputIntent::TurnStart,
        attachments: Vec::new(),
    }
}

pub(super) fn assistant_message_payload(
    message_id: &str,
    text: &str,
    streaming: bool,
) -> TurnItemPayload {
    TurnItemPayload::AssistantMessage {
        message_id: message_id.to_string(),
        text: text.to_string(),
        streaming,
        attachments: Vec::new(),
    }
}

pub(super) fn dynamic_tool_payload(
    tool_name: Option<&str>,
    input: Value,
    output: Option<Value>,
) -> TurnItemPayload {
    TurnItemPayload::DynamicTool {
        tool_name: tool_name.map(str::to_string),
        viewed_image_path: None,
        input,
        output,
        output_omitted: false,
    }
}

pub(super) fn command_payload(
    input: &str,
    output: Option<&str>,
    exit_code: Option<i64>,
) -> TurnItemPayload {
    TurnItemPayload::CommandExecution {
        input: input.to_string(),
        output: output.map(str::to_string),
        output_omitted: false,
        output_indicates_failure: None,
        exit_code,
    }
}

pub(super) fn subagent_payload(
    subagent_id: &str,
    origin: &str,
    child_thread_id: Option<&str>,
) -> TurnItemPayload {
    TurnItemPayload::Subagent {
        subagent_id: subagent_id.to_string(),
        origin: origin.to_string(),
        child_thread_id: child_thread_id.map(str::to_string),
        prompt: "Investigate".to_string(),
        progress: None,
        result: None,
    }
}

pub(super) fn error_payload(
    class: ProviderFailureClass,
    message: &str,
    retry: Option<ProviderRetry>,
) -> TurnItemPayload {
    TurnItemPayload::Error {
        failure: ProviderFailure {
            class,
            message: message.to_string(),
            code: None,
            retryable: None,
            reset_at: None,
        },
        retry,
    }
}

/// A structured payload for entry-level tests that only set `type` and a few
/// fields (upstream `{ type: "dynamic_tool", toolName, input } as never`).
pub(super) fn dynamic_tool_item(tool_name: &str, input: Value) -> TurnItem {
    turn_item(
        "tool-item",
        "2026-01-01T00:00:00.000Z",
        dynamic_tool_payload(Some(tool_name), input, None),
    )
}

// ---------------------------------------------------------------------------
// makeStreamingTimelineFixture (upstream/apps/web/src/test-fixtures.ts)
// ---------------------------------------------------------------------------

pub(super) struct StreamingTimelineFixture {
    pub thread_id: String,
    pub run_id: String,
    pub history_run_id: String,
    pub visible_turn_items: Vec<ProjectedTurnItem>,
}

impl StreamingTimelineFixture {
    /// `new Date(Date.UTC(2026, 8, 4, 0, 0, second)).toISOString()`.
    pub fn time(&self, second: u32) -> String {
        streaming_fixture_time(second)
    }

    pub fn entries_input(&self) -> TimelineEntriesInput {
        TimelineEntriesInput {
            visible_turn_items: self.visible_turn_items.clone(),
            ..Default::default()
        }
    }
}

pub(super) fn streaming_fixture_time(second: u32) -> String {
    format!("2026-09-04T00:{:02}:{:02}.000Z", second / 60, second % 60)
}

pub(super) fn make_streaming_timeline_fixture(text: &str) -> StreamingTimelineFixture {
    let thread_id = "streaming-thread";
    let run_id = "live-run";
    let history_run_id = "history-run";
    let base = |id: &str, run: &str, second: u32, payload: TurnItemPayload| TurnItem {
        run_id: Some(run.to_string()),
        ordinal: u64::from(second),
        started_at: Some(streaming_fixture_time(second)),
        completed_at: Some(streaming_fixture_time(second)),
        ..TurnItem::new(
            id,
            thread_id,
            TurnItemStatus::Completed,
            streaming_fixture_time(second),
            payload,
        )
    };
    let items = vec![
        base(
            "history-user-item",
            history_run_id,
            0,
            user_message_payload("history-user", "Inspect"),
        ),
        TurnItem {
            status: TurnItemStatus::Failed,
            ..base(
                "history-work",
                history_run_id,
                1,
                command_payload("vp test", Some("Test failed"), Some(1)),
            )
        },
        TurnItem {
            updated_at: streaming_fixture_time(4),
            ..base(
                "history-assistant-item",
                history_run_id,
                3,
                assistant_message_payload("history-assistant", "Done", false),
            )
        },
        base(
            "live-user-item",
            run_id,
            5,
            user_message_payload("live-user", "Continue"),
        ),
        base(
            "live-work",
            run_id,
            6,
            dynamic_tool_payload(
                Some("read_file"),
                json!({ "path": "/repo/src/index.ts" }),
                Some(json!("Contents")),
            ),
        ),
        TurnItem {
            status: TurnItemStatus::Running,
            completed_at: None,
            ..base(
                "live-assistant-item",
                run_id,
                7,
                assistant_message_payload("live-assistant", text, true),
            )
        },
    ];
    let visible_turn_items = items
        .into_iter()
        .enumerate()
        .map(|(position, item)| ProjectedTurnItem {
            position: position as u64,
            visibility: ProjectedVisibility::Local,
            source_thread_id: thread_id.to_string(),
            source_item_id: item.id.clone(),
            item,
        })
        .collect();
    StreamingTimelineFixture {
        thread_id: thread_id.to_string(),
        run_id: run_id.to_string(),
        history_run_id: history_run_id.to_string(),
        visible_turn_items,
    }
}

pub(super) fn entries_from(items: Vec<ProjectedTurnItem>) -> Vec<TimelineEntry> {
    derive_timeline_entries_from_visible_turn_items(&TimelineEntriesInput {
        visible_turn_items: items,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Row queries (upstream `rows.find((row) => row.kind === ...)` and friends)
// ---------------------------------------------------------------------------

pub(super) fn ids(rows: &[Arc<MessagesTimelineRow>]) -> Vec<&str> {
    rows.iter().map(|row| row.id()).collect()
}

pub(super) fn kinds(rows: &[Arc<MessagesTimelineRow>]) -> Vec<&'static str> {
    rows.iter().map(|row| row.kind().as_str()).collect()
}

pub(super) fn find_kind(
    rows: &[Arc<MessagesTimelineRow>],
    kind: MessagesTimelineRowKind,
) -> Option<&MessagesTimelineRowContent> {
    rows.iter()
        .find(|row| row.kind() == kind)
        .map(|row| &row.content)
}

pub(super) fn filter_kind(
    rows: &[Arc<MessagesTimelineRow>],
    kind: MessagesTimelineRowKind,
) -> Vec<&MessagesTimelineRowContent> {
    rows.iter()
        .filter(|row| row.kind() == kind)
        .map(|row| &row.content)
        .collect()
}

pub(super) fn find_id<'a>(
    rows: &'a [Arc<MessagesTimelineRow>],
    id: &str,
) -> Option<&'a MessagesTimelineRowContent> {
    rows.iter()
        .find(|row| row.id() == id)
        .map(|row| &row.content)
}

pub(super) fn has_kind(rows: &[Arc<MessagesTimelineRow>], kind: MessagesTimelineRowKind) -> bool {
    rows.iter().any(|row| row.kind() == kind)
}

/// The `work-live` rows, as `(entry, grouped_entries, active, row)`.
pub(super) fn live_rows(rows: &[Arc<MessagesTimelineRow>]) -> Vec<&MessagesTimelineRowContent> {
    filter_kind(rows, MessagesTimelineRowKind::WorkLive)
}

/// `entry.id` of a `work-live` row.
pub(super) fn live_entry_id(row: &MessagesTimelineRowContent) -> Option<&str> {
    match row {
        Row::WorkLive { entry, .. } => Some(&entry.id),
        _ => None,
    }
}

/// `groupedEntries` of a `work`, `work-live` row.
pub(super) fn grouped_entries(row: &MessagesTimelineRowContent) -> &[WorkLogEntry] {
    match row {
        Row::Work {
            grouped_entries, ..
        }
        | Row::WorkLive {
            grouped_entries, ..
        } => grouped_entries,
        _ => &[],
    }
}

pub(super) fn grouped_ids(row: &MessagesTimelineRowContent) -> Vec<&str> {
    grouped_entries(row)
        .iter()
        .map(|entry| entry.id.as_str())
        .collect()
}

/// The message carried by a `message` / `assistant-meta` row.
pub(super) fn row_message(row: &MessagesTimelineRowContent) -> Option<&ChatMessage> {
    match row {
        Row::Message { message, .. } | Row::AssistantMeta { message, .. } => Some(message),
        _ => None,
    }
}

#[test]
fn fixtures_build_the_streaming_timeline() {
    let fixture = make_streaming_timeline_fixture("Hello");
    let entries = derive_timeline_entries_from_visible_turn_items(&fixture.entries_input());
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "history-user",
            "history-work",
            "history-assistant",
            "live-user",
            "live-work",
            "live-assistant",
        ]
    );
    assert_eq!(fixture.time(65), "2026-09-04T00:01:05.000Z");
}
