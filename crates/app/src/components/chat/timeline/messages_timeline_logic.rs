//! Port of upstream/apps/web/src/components/chat/MessagesTimeline.logic.ts
//!
//! Cut (PORTING cut list): the minimap helpers (`resolveTimelineMinimap*`),
//! html-render and mcp-app rows, and the worktree-setup row (thread launch
//! into worktrees is replaced by hooks).
//!
//! Identity: upstream compares entries, messages and projected items by
//! object identity (`===`) to decide when a row can be reused. Turin's
//! timeline values are plain owned data, so those checks compare by value.
//! Rows are `Arc`-shared: [`compute_stable_messages_timeline_rows`] and
//! [`derive_messages_timeline_rows_with_state`] hand back the previous `Arc`
//! for an unchanged row, so the view can detect it with `Arc::ptr_eq`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde_json::{Value, json};

use super::assistant_citations::{assistant_citation_label, collect_assistant_citations};
use super::codex_markdown_directives::render_codex_directives_for_copy;
use super::command_label::{command_display_text, command_program_name};
use super::file_path_display::{format_workspace_relative_path, is_windows_absolute_path};
use super::js_text::{capitalize_first, collapse_whitespace, js_trim, utf16_len};
use super::orchestration_timing::format_duration;
use super::session_logic::{
    derive_revert_turn_count_by_user_message_id, is_streaming_message_text_update,
    is_streaming_turn_item_text_update, timeline_entry_is_persistent_resource_card,
    work_entry_indicates_tool_neutral_status, work_entry_indicates_tool_success,
    work_log_entry_is_tool_like,
};
use super::subagent_runtime::is_live_subagent_turn_item;
use super::t3_mcp_tool_presentation::{
    T3McpToolIcon, T3McpToolPresentation, T3McpToolSummaryAction, resolve_t3_mcp_tool_definition,
};
use super::tool_activity::{
    collect_tool_file_paths, dynamic_tool_title, format_read_tool_label, format_search_tool_label,
};
use super::tool_output::compact_dynamic_tool_output;
use super::turn_item::{
    ProjectedTurnItem, ToolActivityIcon, ToolActivitySurface, TurnItem, TurnItemPayload,
    TurnItemStatus, TurnItemType,
};
use super::types::{
    AttemptStatus, ChatMessage, InputIntent, MessageRole, ProposedPlan, RunStatus, TimelineEntry,
    TimelineEntryKind, TimelineLatestRun, ToolLifecycleStatus, TurnDiffSummary, WorkLogEntry,
    WorkTone,
};
use super::work_log_presentation::{
    ToolGroupAction, ToolGroupSummaryKind, live_activity_tool_status,
    resolve_work_entry_tool_presentation, resolve_work_entry_tool_presentation_with_status,
    summarize_tool_group, tool_group_action, tool_group_summary_kind,
    work_entry_display_indicates_tool_failure,
};

// Upstream re-exports these; kept for callers of this module.
#[allow(unused_imports)]
pub use super::work_log_presentation::normalize_compact_tool_label;

/// `entry.detail?.trim().replace(/\s+/g, " ")`, empty when absent.
fn collapsed_detail(entry: &WorkLogEntry) -> String {
    entry
        .detail
        .as_deref()
        .map(|detail| collapse_whitespace(js_trim(detail)))
        .unwrap_or_default()
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn timeline_entry_run_id(entry: &TimelineEntry) -> Option<&str> {
    match &entry.kind {
        TimelineEntryKind::Message { message, .. } => {
            if message.role == MessageRole::Assistant {
                message.run_id.as_deref()
            } else {
                None
            }
        }
        TimelineEntryKind::ProposedPlan { proposed_plan } => proposed_plan.run_id.as_deref(),
        TimelineEntryKind::Work { entry } => entry.run_id.as_deref(),
        TimelineEntryKind::Event { .. } => None,
    }
}

fn work_entry(entry: &TimelineEntry) -> Option<&WorkLogEntry> {
    match &entry.kind {
        TimelineEntryKind::Work { entry } => Some(entry),
        _ => None,
    }
}

fn message_of(entry: &TimelineEntry) -> Option<&ChatMessage> {
    match &entry.kind {
        TimelineEntryKind::Message { message, .. } => Some(message),
        _ => None,
    }
}

fn event_item(entry: &TimelineEntry) -> Option<&TurnItem> {
    match &entry.kind {
        TimelineEntryKind::Event { projected_item } => Some(&projected_item.item),
        _ => None,
    }
}

fn is_user_message(entry: &TimelineEntry) -> bool {
    message_of(entry).is_some_and(|message| message.role == MessageRole::User)
}

fn work_item_type_is(entry: &TimelineEntry, item_type: TurnItemType) -> bool {
    work_entry(entry).is_some_and(|work| work.item_type == Some(item_type))
}

fn source_activity_kind_is(entry: &WorkLogEntry, kind: &str) -> bool {
    entry.source_activity_kind.as_deref() == Some(kind)
}

/// Whether the entry still represents live activity, not a settled result.
fn work_entry_is_active_turn_activity(entry: &WorkLogEntry) -> bool {
    entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
        || (entry.tool_lifecycle_status.is_none()
            && (source_activity_kind_is(entry, "task.progress")
                || work_log_entry_is_tool_like(entry)))
}

/// `dynamicToolTitle(item.toolName, item.input, item.title)` for a dynamic tool payload.
fn structured_dynamic_tool_title(entry: &WorkLogEntry) -> Option<String> {
    let item = entry.structured_payload.as_ref()?;
    match &item.payload {
        TurnItemPayload::DynamicTool {
            tool_name, input, ..
        } => dynamic_tool_title(tool_name.as_deref(), input, item.title.as_deref()),
        _ => None,
    }
    .filter(|title| !title.is_empty())
}

fn single_tool_call_label(entry: &WorkLogEntry) -> String {
    if entry.item_type == Some(TurnItemType::Reasoning) {
        let thought = collapsed_detail(entry);
        return if thought.is_empty() {
            "Thought".to_string()
        } else {
            thought
        };
    }
    if let Some(tool_presentation) =
        resolve_work_entry_tool_presentation(entry, Some(ToolLifecycleStatus::Completed))
    {
        return tool_presentation.display_name;
    }
    if let Some(title) = structured_dynamic_tool_title(entry) {
        return title;
    }
    // A lone web search keeps its heading; the query stays in its detail.
    if entry.item_type == Some(TurnItemType::WebSearch) {
        return entry
            .tool_title
            .clone()
            .unwrap_or_else(|| "Web search".to_string());
    }
    work_entry_display_label(entry, None)
}

fn work_entry_tool_data_record(entry: &WorkLogEntry) -> Option<&Value> {
    entry.tool_data.as_ref().filter(|data| data.is_object())
}

fn work_entry_read_raw_paths(entry: &WorkLogEntry) -> Vec<String> {
    if let Some(changed_files) = entry
        .changed_files
        .as_ref()
        .filter(|files| !files.is_empty())
    {
        return changed_files.clone();
    }
    if let Some(TurnItemPayload::DynamicTool { input, .. }) =
        entry.structured_payload.as_ref().map(|item| &item.payload)
    {
        let structured_paths = collect_tool_file_paths(Some(&json!({ "input": input })));
        if !structured_paths.is_empty() {
            return structured_paths;
        }
    }
    collect_tool_file_paths(work_entry_tool_data_record(entry))
}

fn work_entry_read_paths(entry: &WorkLogEntry, workspace_root: Option<&str>) -> Vec<String> {
    work_entry_read_raw_paths(entry)
        .iter()
        .map(|file_path| format_workspace_relative_path(file_path, workspace_root))
        .collect()
}

pub fn work_entry_display_label(entry: &WorkLogEntry, workspace_root: Option<&str>) -> String {
    if entry.item_type == Some(TurnItemType::SystemNotice) {
        return entry.label.clone();
    }
    if entry.item_type == Some(TurnItemType::Reasoning) || entry.tone == WorkTone::Thinking {
        let thought = collapsed_detail(entry);
        return if thought.is_empty() {
            entry.label.clone()
        } else {
            thought
        };
    }
    if let Some(tool_presentation) = resolve_work_entry_tool_presentation(entry, None) {
        return tool_presentation.display_name;
    }
    if let Some(command) = entry
        .command
        .as_deref()
        .filter(|command| !js_trim(command).is_empty())
    {
        return command_display_text(command);
    }
    let action = tool_group_action(entry);
    let is_search = matches!(
        action,
        ToolGroupAction::CodeSearch | ToolGroupAction::Search
    );
    if is_search {
        // Adapters title file searches with their target; the item keeps only the pattern.
        let search_label = if entry.item_type == Some(TurnItemType::FileSearch) {
            Some(entry.label.clone())
        } else {
            format_search_tool_label(work_entry_tool_data_record(entry))
        };
        if let Some(search_label) = search_label.filter(|label| !label.is_empty()) {
            return search_label;
        }
    }
    let read_paths = if action == ToolGroupAction::Read {
        work_entry_read_paths(entry, workspace_root)
    } else {
        Vec::new()
    };
    if action == ToolGroupAction::Read
        && let Some(first) = read_paths.first().filter(|path| !path.is_empty())
    {
        return format_read_tool_label(first, read_paths.len() - 1);
    }
    // Retrying providers keep their progress label; other diagnostics expose
    // the retained message instead of a generic error heading. File bodies
    // are never a compact read label.
    let provider_retry = entry.projected_item.as_ref().is_some_and(|projected| {
        matches!(
            &projected.item.payload,
            TurnItemPayload::Error { retry: Some(_), .. }
        )
    });
    if let Some(title) = structured_dynamic_tool_title(entry) {
        return title;
    }
    let compact_detail = entry.detail.as_deref().map(js_trim);
    let detail_is_search_output = is_search && compact_detail.unwrap_or("").contains(['\r', '\n']);
    if let Some(compact_detail) = non_empty(compact_detail)
        && !provider_retry
        && action != ToolGroupAction::Read
        && !detail_is_search_output
    {
        return compact_detail.to_string();
    }
    if let Some(changed_files) = entry.changed_files.as_ref()
        && let Some(first_path) = changed_files.first().filter(|path| !path.is_empty())
    {
        let path = format_workspace_relative_path(first_path, workspace_root);
        return if changed_files.len() == 1 {
            path
        } else {
            format!("{path} +{} more", changed_files.len() - 1)
        };
    }
    if action == ToolGroupAction::Read && entry.viewed_image_path.is_none() {
        return "Read file".to_string();
    }
    let heading = normalize_compact_tool_label(
        non_empty(entry.tool_title.as_deref()).unwrap_or(&entry.label),
    );
    capitalize_first(&heading)
}

/// The trimmed thread id a `t3_thread_read` call targets, or None for any other entry.
pub fn thread_read_target_id(entry: &WorkLogEntry) -> Option<String> {
    let item = entry.structured_payload.as_ref()?;
    let TurnItemPayload::DynamicTool {
        tool_name, input, ..
    } = &item.payload
    else {
        return None;
    };
    if resolve_t3_mcp_tool_definition(tool_name.as_deref())
        .map(|definition| definition.summary_action)
        != Some(T3McpToolSummaryAction::ThreadRead)
    {
        return None;
    }
    let thread_id = input.as_object()?.get("threadId")?.as_str()?;
    let trimmed = js_trim(thread_id);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

const THREAD_READ_OBJECT: &str = " a T3 thread";

/// `Pick<ThreadShell, "title" | "archivedAt" | "deletedAt">`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadReadTargetShell {
    pub title: String,
    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
}

pub fn thread_read_target_title(shell: Option<&ThreadReadTargetShell>) -> Option<String> {
    let shell = shell?;
    if shell.archived_at.is_some() || shell.deleted_at.is_some() {
        return None;
    }
    let title = js_trim(&shell.title);
    (!title.is_empty()).then(|| title.to_string())
}

/// Names the read thread in place of the generic object ("Read a T3 thread" becomes
/// `Read thread “Title”`), keeping the label's tense. None keeps the generic label.
pub fn thread_read_label_prefix(label: &str) -> Option<String> {
    label
        .strip_suffix(THREAD_READ_OBJECT)
        .map(|prefix| format!("{prefix} thread"))
}

/// Inspectable read-file output is the path when we have one, otherwise nothing.
pub fn work_entry_read_output(
    entry: &WorkLogEntry,
    workspace_root: Option<&str>,
) -> Option<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for path in work_entry_read_raw_paths(entry) {
        let trimmed = js_trim(&path).replace('\\', "/");
        let resolved = match non_empty(workspace_root) {
            Some(workspace_root)
                if !trimmed.starts_with('/') && !is_windows_absolute_path(&trimmed) =>
            {
                let root = workspace_root.replace('\\', "/");
                let root = root.trim_end_matches('/');
                let relative = trimmed.strip_prefix("./").unwrap_or(&trimmed);
                let relative = relative.trim_start_matches('/');
                format!("{root}/{relative}")
            }
            _ => trimmed,
        };
        if !resolved.is_empty() && seen.insert(resolved.clone()) {
            paths.push(resolved);
        }
    }
    (!paths.is_empty()).then(|| paths.join("\n"))
}

pub fn live_work_entry_label(
    entry: &WorkLogEntry,
    workspace_root: Option<&str>,
    active: bool,
) -> String {
    let status = live_activity_tool_status(entry.tool_lifecycle_status, active);
    if entry.item_type == Some(TurnItemType::Reasoning) {
        let thought = collapsed_detail(entry);
        if !thought.is_empty() {
            return thought;
        }
        return if status == ToolLifecycleStatus::InProgress {
            "Thinking".to_string()
        } else {
            "Thought".to_string()
        };
    }
    if let Some(tool_presentation) =
        resolve_work_entry_tool_presentation_with_status(entry, Some(status))
    {
        return tool_presentation.display_name;
    }
    if let Some(command) = entry
        .command
        .as_deref()
        .map(js_trim)
        .filter(|command| !command.is_empty())
    {
        let verb = match status {
            ToolLifecycleStatus::InProgress => "Running",
            ToolLifecycleStatus::Failed => "Failed",
            ToolLifecycleStatus::Declined => "Declined",
            ToolLifecycleStatus::Stopped => "Stopped",
            _ => "Ran",
        };
        let program = command_program_name(command).unwrap_or_else(|| "command".to_string());
        return format!("{verb} {program}");
    }
    work_entry_display_label(entry, workspace_root)
}

pub fn work_entry_is_visible_in_group(
    entry: &WorkLogEntry,
    expanded_tool_group_entry: bool,
) -> bool {
    if entry.item_type == Some(TurnItemType::Reasoning) {
        return entry
            .detail
            .as_deref()
            .is_some_and(|detail| !js_trim(detail).is_empty());
    }
    (expanded_tool_group_entry
        && (entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
            || source_activity_kind_is(entry, "task.progress")))
        // A stopped call is an outcome ("Stopped sleep"), not an empty row.
        || entry.tool_lifecycle_status == Some(ToolLifecycleStatus::Stopped)
        || !work_entry_indicates_tool_neutral_status(entry)
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkGroupScrollAnchor {
    pub entry_id: String,
    pub offset: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkGroupScrollIndex {
    pub index: usize,
    pub view_offset: f64,
}

/// Restore a visible tool, including a position partway through its expanded output.
pub fn resolve_work_group_scroll_index<S: AsRef<str>>(
    entry_ids: &[S],
    anchor: Option<&WorkGroupScrollAnchor>,
) -> Option<WorkGroupScrollIndex> {
    let anchor = anchor?;
    let index = entry_ids
        .iter()
        .position(|id| id.as_ref() == anchor.entry_id)?;
    Some(WorkGroupScrollIndex {
        index,
        view_offset: -anchor.offset,
    })
}

/// Only newly appended calls may follow the end, never status or output updates.
pub fn should_follow_work_group_append<S: AsRef<str>>(
    previous_ids: &[S],
    entry_ids: &[S],
    distance_from_end: f64,
) -> bool {
    !previous_ids.is_empty()
        && entry_ids.len() > previous_ids.len()
        && distance_from_end <= 1.0
        && previous_ids
            .iter()
            .zip(entry_ids)
            .all(|(previous, next)| previous.as_ref() == next.as_ref())
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimelineEndState {
    pub is_at_end: Option<bool>,
    pub is_near_end: Option<bool>,
    pub content_length: Option<f64>,
    pub scroll: Option<f64>,
    pub scroll_length: Option<f64>,
}

/// The follow re-arm band (#5566): strict isAtEnd flickers false for a frame
/// while streaming content grows under the viewport, so follow re-arms within
/// this distance of the real content bottom instead.
pub const TIMELINE_FOLLOW_REARM_THRESHOLD_PX: f64 = 40.0;

pub fn resolve_timeline_is_at_end(state: Option<&TimelineEndState>) -> Option<bool> {
    let state = state?;
    let (Some(content_length), Some(scroll), Some(scroll_length)) =
        (state.content_length, state.scroll, state.scroll_length)
    else {
        return state.is_at_end;
    };
    // contentLength includes the composer inset spacer, but the composer hides
    // the same amount of viewport, so the inset cancels: plain
    // contentLength - scroll - scrollLength is the gap between the last real row
    // and the visible edge above the composer. LegendList's own isAtEnd subtracts
    // the inset and is true anywhere in the bottom composer-height band, so it is
    // only a fallback here, never a short-circuit.
    Some(content_length - scroll - scroll_length <= TIMELINE_FOLLOW_REARM_THRESHOLD_PX)
}

/// JS `Date.parse` in milliseconds; None for NaN.
fn parse_iso_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

fn compute_elapsed_ms(start_iso: &str, end_iso: &str) -> Option<f64> {
    let start = parse_iso_ms(start_iso)?;
    let end = parse_iso_ms(end_iso)?;
    Some((end - start).max(0) as f64)
}

fn max_iso_timestamp<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    let Some(a) = a else { return b };
    let Some(b) = b else { return Some(a) };
    let Some(a_ms) = parse_iso_ms(a) else {
        return Some(b);
    };
    let Some(b_ms) = parse_iso_ms(b) else {
        return Some(a);
    };
    Some(if b_ms > a_ms { b } else { a })
}

/// `isLessThan` on two ISO instants; false when either does not parse.
fn iso_is_less_than(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a < b,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineDurationMessage<'a> {
    pub id: &'a str,
    pub role: MessageRole,
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub streaming: bool,
}

impl<'a> From<&'a ChatMessage> for TimelineDurationMessage<'a> {
    fn from(message: &'a ChatMessage) -> Self {
        Self {
            id: &message.id,
            role: message.role,
            created_at: &message.created_at,
            updated_at: &message.updated_at,
            streaming: message.streaming,
        }
    }
}

const LIVE_ACTIVITY_ROW_ID: &str = "live-activity-row";
const WORKING_INDICATOR_ROW_ID: &str = "working-indicator-row";

/// `MessagesTimelineRow`: the row content plus `continuesWorkLog`.
#[derive(Clone, Debug, PartialEq)]
pub struct MessagesTimelineRow {
    pub content: MessagesTimelineRowContent,
    /// Adjacent work stays one visual list even when virtualization splits its groups.
    pub continues_work_log: bool,
}

impl From<MessagesTimelineRowContent> for MessagesTimelineRow {
    fn from(content: MessagesTimelineRowContent) -> Self {
        Self {
            content,
            continues_work_log: false,
        }
    }
}

impl MessagesTimelineRow {
    pub fn id(&self) -> &str {
        self.content.id()
    }

    pub fn kind(&self) -> MessagesTimelineRowKind {
        self.content.kind()
    }

    pub fn created_at(&self) -> Option<&str> {
        self.content.created_at()
    }
}

/// The `kind` discriminant of [`MessagesTimelineRowContent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessagesTimelineRowKind {
    Work,
    WorkLive,
    Working,
    Thinking,
    WorkToggle,
    TurnFold,
    AttemptFold,
    ContextCompaction,
    Message,
    AssistantMeta,
    Event,
    ProposedPlan,
}

impl MessagesTimelineRowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::WorkLive => "work-live",
            Self::Working => "working",
            Self::Thinking => "thinking",
            Self::WorkToggle => "work-toggle",
            Self::TurnFold => "turn-fold",
            Self::AttemptFold => "attempt-fold",
            Self::ContextCompaction => "context-compaction",
            Self::Message => "message",
            Self::AssistantMeta => "assistant-meta",
            Self::Event => "event",
            Self::ProposedPlan => "proposed-plan",
        }
    }
}

/// `MessagesTimelineRowContent`. Rows are `Arc`-shared, so variant size
/// differences do not cost copies.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum MessagesTimelineRowContent {
    Work {
        id: String,
        created_at: String,
        grouped_entries: Vec<WorkLogEntry>,
        is_expanded_tool_group: bool,
        display_label: Option<String>,
    },
    WorkLive {
        id: String,
        created_at: String,
        entry: WorkLogEntry,
        grouped_entries: Vec<WorkLogEntry>,
        group_id: String,
        expanded: bool,
        active: bool,
        /// Latest reasoning in the live group, shown above the status line.
        thought: Option<WorkLogEntry>,
    },
    Working {
        id: String,
        created_at: Option<String>,
    },
    Thinking {
        id: String,
        created_at: Option<String>,
        /// Tool calls this row stands in for after the latest one failed.
        group_id: Option<String>,
        expanded: Option<bool>,
    },
    WorkToggle {
        id: String,
        created_at: String,
        run_id: Option<String>,
        group_id: String,
        hidden_count: usize,
        expanded: bool,
        summary: String,
        summary_kind: ToolGroupSummaryKind,
        tool_surface: Option<ToolActivitySurface>,
        tool_icon: Option<ToolActivityIcon>,
        summary_tool_icon: Option<T3McpToolIcon>,
        has_failure: bool,
    },
    TurnFold {
        id: String,
        created_at: String,
        run_id: String,
        label: String,
        expanded: bool,
    },
    AttemptFold {
        id: String,
        created_at: String,
        run_id: String,
        attempt_id: String,
        label: String,
        expanded: bool,
    },
    ContextCompaction {
        id: String,
        created_at: String,
        label: String,
        active: bool,
    },
    Message {
        id: String,
        created_at: String,
        message: ChatMessage,
        projected_item: Option<ProjectedTurnItem>,
        duration_start: String,
        show_assistant_meta: bool,
        show_assistant_copy_button: bool,
        assistant_copy_streaming: bool,
        assistant_turn_diff_summary: Option<TurnDiffSummary>,
        revert_turn_count: Option<u64>,
    },
    AssistantMeta {
        projected_item: Option<ProjectedTurnItem>,
        id: String,
        created_at: String,
        message: ChatMessage,
        show_assistant_copy_button: bool,
        assistant_copy_streaming: bool,
    },
    Event {
        id: String,
        created_at: String,
        projected_item: ProjectedTurnItem,
        subagents: Option<Vec<ProjectedTurnItem>>,
        resource_summary: bool,
    },
    ProposedPlan {
        id: String,
        created_at: String,
        proposed_plan: ProposedPlan,
    },
}

impl MessagesTimelineRowContent {
    pub fn id(&self) -> &str {
        match self {
            Self::Work { id, .. }
            | Self::WorkLive { id, .. }
            | Self::Working { id, .. }
            | Self::Thinking { id, .. }
            | Self::WorkToggle { id, .. }
            | Self::TurnFold { id, .. }
            | Self::AttemptFold { id, .. }
            | Self::ContextCompaction { id, .. }
            | Self::Message { id, .. }
            | Self::AssistantMeta { id, .. }
            | Self::Event { id, .. }
            | Self::ProposedPlan { id, .. } => id,
        }
    }

    pub fn kind(&self) -> MessagesTimelineRowKind {
        match self {
            Self::Work { .. } => MessagesTimelineRowKind::Work,
            Self::WorkLive { .. } => MessagesTimelineRowKind::WorkLive,
            Self::Working { .. } => MessagesTimelineRowKind::Working,
            Self::Thinking { .. } => MessagesTimelineRowKind::Thinking,
            Self::WorkToggle { .. } => MessagesTimelineRowKind::WorkToggle,
            Self::TurnFold { .. } => MessagesTimelineRowKind::TurnFold,
            Self::AttemptFold { .. } => MessagesTimelineRowKind::AttemptFold,
            Self::ContextCompaction { .. } => MessagesTimelineRowKind::ContextCompaction,
            Self::Message { .. } => MessagesTimelineRowKind::Message,
            Self::AssistantMeta { .. } => MessagesTimelineRowKind::AssistantMeta,
            Self::Event { .. } => MessagesTimelineRowKind::Event,
            Self::ProposedPlan { .. } => MessagesTimelineRowKind::ProposedPlan,
        }
    }

    pub fn created_at(&self) -> Option<&str> {
        match self {
            Self::Working { created_at, .. } | Self::Thinking { created_at, .. } => {
                created_at.as_deref()
            }
            Self::Work { created_at, .. }
            | Self::WorkLive { created_at, .. }
            | Self::WorkToggle { created_at, .. }
            | Self::TurnFold { created_at, .. }
            | Self::AttemptFold { created_at, .. }
            | Self::ContextCompaction { created_at, .. }
            | Self::Message { created_at, .. }
            | Self::AssistantMeta { created_at, .. }
            | Self::Event { created_at, .. }
            | Self::ProposedPlan { created_at, .. } => Some(created_at),
        }
    }
}

/// `StableMessagesTimelineRowsState`. Both halves are `Arc`s so returning the
/// previous state is free and the view can compare `result` with `Arc::ptr_eq`.
#[derive(Clone, Debug, Default)]
pub struct StableMessagesTimelineRowsState {
    pub by_id: Arc<HashMap<String, Arc<MessagesTimelineRow>>>,
    pub result: Arc<Vec<Arc<MessagesTimelineRow>>>,
}

pub fn compute_message_duration_start<'a>(
    messages: impl IntoIterator<Item = TimelineDurationMessage<'a>>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let mut last_boundary: Option<&str> = None;

    for message in messages {
        if message.role == MessageRole::User {
            last_boundary = Some(message.created_at);
        }
        result.insert(
            message.id.to_string(),
            last_boundary.unwrap_or(message.created_at).to_string(),
        );
        if message.role == MessageRole::Assistant && !message.streaming {
            last_boundary = Some(message.updated_at);
        }
    }

    result
}

fn work_group_id(timeline_entry_id: &str) -> String {
    format!("work-group:{timeline_entry_id}")
}

pub type TimelineToolPresentation = T3McpToolPresentation;
// Upstream re-exports these; kept for callers of this module.
#[allow(unused_imports)]
pub use super::t3_mcp_tool_presentation::resolve_t3_mcp_tool_presentation as resolve_timeline_tool_presentation;

fn expanded_work_group_row(
    group_id: &str,
    created_at: &str,
    grouped_entries: Vec<WorkLogEntry>,
) -> MessagesTimelineRowContent {
    MessagesTimelineRowContent::Work {
        id: format!("{group_id}:details"),
        created_at: created_at.to_string(),
        grouped_entries,
        is_expanded_tool_group: true,
        display_label: None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssistantMessageCopyState {
    pub text: Option<String>,
    pub visible: bool,
}

pub fn resolve_assistant_message_copy_state(
    text: Option<&str>,
    show_copy_button: bool,
    streaming: bool,
) -> AssistantMessageCopyState {
    let text = text.filter(|text| !js_trim(text).is_empty());
    let visible = show_copy_button && text.is_some() && !streaming;
    AssistantMessageCopyState {
        text: text.map(|text| {
            if visible {
                render_codex_directives_for_copy(text)
            } else {
                text.to_string()
            }
        }),
        visible,
    }
}

fn derive_terminal_assistant_message_ids(timeline_entries: &[TimelineEntry]) -> HashSet<String> {
    let mut last_assistant_message_id_by_response_key: HashMap<String, &str> = HashMap::new();
    let mut null_turn_response_index = 0usize;

    for timeline_entry in timeline_entries {
        let Some(message) = message_of(timeline_entry) else {
            continue;
        };
        if message.role == MessageRole::User {
            null_turn_response_index += 1;
            continue;
        }
        if message.role != MessageRole::Assistant {
            continue;
        }

        let response_key = match non_empty(message.run_id.as_deref()) {
            Some(run_id) => format!("turn:{run_id}"),
            None => format!("unkeyed:{null_turn_response_index}"),
        };
        last_assistant_message_id_by_response_key.insert(response_key, &message.id);
    }

    last_assistant_message_id_by_response_key
        .into_values()
        .map(str::to_string)
        .collect()
}

struct TurnFold {
    run_id: String,
    created_at: String,
    hidden_entry_ids: HashSet<String>,
    label: String,
}

struct SupersededAttemptFold {
    run_id: String,
    attempt_id: String,
    created_at: String,
    hidden_entry_ids: HashSet<String>,
}

/// Final replies of superseded attempts that finished before the steer replaced
/// them. A steer stamps the attempt's completedAt when it is dispatched, and the
/// provider closes a reply it cut off only after that, so a reply whose item
/// completed earlier was whole. Item status alone cannot tell: an interrupted
/// stream also closes as `completed`. The reply must be the attempt's last
/// output; trailing tool or reasoning work means the attempt was still going.
fn complete_superseded_reply_entry_ids(timeline_entries: &[TimelineEntry]) -> HashSet<String> {
    let mut last_output_by_attempt_id: HashMap<&str, &TimelineEntry> = HashMap::new();
    for entry in timeline_entries {
        let Some(attempt) = entry
            .attempt
            .as_ref()
            .filter(|attempt| attempt.status == AttemptStatus::Superseded)
        else {
            continue;
        };
        if is_user_message(entry)
            || work_item_type_is(entry, TurnItemType::SystemNotice)
            || work_item_type_is(entry, TurnItemType::Notification)
            || !matches!(
                entry.kind,
                TimelineEntryKind::Message { .. } | TimelineEntryKind::Work { .. }
            )
        {
            continue;
        }
        last_output_by_attempt_id.insert(&attempt.id, entry);
    }
    let mut reply_entry_ids = HashSet::new();
    for entry in last_output_by_attempt_id.into_values() {
        let superseded_at = entry
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.completed_at.as_deref());
        let TimelineEntryKind::Message {
            message,
            projected_item,
        } = &entry.kind
        else {
            continue;
        };
        let Some(item) = projected_item.as_ref().map(|projected| &projected.item) else {
            continue;
        };
        if !message.streaming
            && item.item_type() == TurnItemType::AssistantMessage
            && item.status == TurnItemStatus::Completed
            && let Some(completed_at) = item.completed_at.as_deref()
            && let Some(superseded_at) = superseded_at
            && iso_is_less_than(completed_at, superseded_at)
        {
            reply_entry_ids.insert(entry.id.clone());
        }
    }
    reply_entry_ids
}

/// Groups only provider output owned by an explicitly superseded V2 attempt
/// that the steer cut off. An attempt that had already finished its reply is
/// left in the timeline like any other turn output.
/// User messages remain visible because they are inputs to the logical run,
/// including the steer message that started the replacement attempt.
fn derive_superseded_attempt_folds(
    timeline_entries: &[TimelineEntry],
    unfolded_run_ids: &HashSet<String>,
    live_subagent_entry_ids: &HashSet<String>,
    complete_reply_entry_ids: &HashSet<String>,
) -> HashMap<String, SupersededAttemptFold> {
    let complete_attempt_ids: HashSet<&str> = timeline_entries
        .iter()
        .filter(|entry| complete_reply_entry_ids.contains(&entry.id))
        .filter_map(|entry| entry.attempt.as_ref().map(|attempt| attempt.id.as_str()))
        .collect();
    let mut attempt_order: Vec<&str> = Vec::new();
    let mut entries_by_attempt_id: HashMap<&str, Vec<&TimelineEntry>> = HashMap::new();
    for entry in timeline_entries {
        let Some(attempt) = entry
            .attempt
            .as_ref()
            .filter(|attempt| attempt.status == AttemptStatus::Superseded)
        else {
            continue;
        };
        if complete_attempt_ids.contains(attempt.id.as_str())
            || unfolded_run_ids.contains(&attempt.run_id)
            || is_user_message(entry)
            // A published page stays visible, as it does when its turn folds.
            || timeline_entry_is_persistent_resource_card(entry)
            // A steer supersedes the attempt but leaves its children running.
            || live_subagent_entry_ids.contains(&entry.id)
            || work_item_type_is(entry, TurnItemType::SystemNotice)
        {
            continue;
        }
        let entries = entries_by_attempt_id.entry(&attempt.id).or_insert_with(|| {
            attempt_order.push(&attempt.id);
            Vec::new()
        });
        entries.push(entry);
    }

    let mut folds_by_anchor_entry_id = HashMap::new();
    for attempt_id in attempt_order {
        let Some(entries) = entries_by_attempt_id.get(attempt_id) else {
            continue;
        };
        let Some(first_entry) = entries.first() else {
            continue;
        };
        let Some(attempt) = first_entry.attempt.as_ref() else {
            continue;
        };
        folds_by_anchor_entry_id.insert(
            first_entry.id.clone(),
            SupersededAttemptFold {
                run_id: attempt.run_id.clone(),
                attempt_id: attempt.id.clone(),
                created_at: first_entry.created_at.clone(),
                hidden_entry_ids: entries.iter().map(|entry| entry.id.clone()).collect(),
            },
        );
    }
    folds_by_anchor_entry_id
}

/// The latest turn counts as unsettled while it is still running (or has not
/// recorded a completion). This is deliberately keyed on the turn's own
/// lifecycle rather than transient working state: right after the user sends
/// a message, the previous turn is still the "active" one until the server
/// creates the new turn, and folding must not flicker through that window.
fn derive_unsettled_run_id(
    latest_run: Option<&TimelineLatestRun>,
    running_run_id: Option<&str>,
) -> Option<String> {
    if let Some(running_run_id) = running_run_id {
        return Some(running_run_id.to_string());
    }
    let latest_run = latest_run?;
    let is_settled = latest_run.completed_at.is_some()
        && !matches!(
            latest_run.status,
            RunStatus::Running | RunStatus::Starting | RunStatus::Waiting
        );
    (!is_settled).then(|| latest_run.run_id.clone())
}

/// Subagent cards that stay out of their turn's folds. A child can keep working
/// after its launching turn settles (or its attempt is superseded), and the
/// waiting footer counts it, so its card stays visible until the child ends.
/// Adjacent cards from one provider turn render as a single grouped row; the
/// whole group stays visible while any member is live, so a launch batch never
/// shows half of its children.
fn live_subagent_card_entry_ids(entries: &[TimelineEntry]) -> HashSet<String> {
    let mut visible = HashSet::new();
    let mut batch: Vec<(&TimelineEntry, &TurnItem)> = Vec::new();
    let flush = |batch: &mut Vec<(&TimelineEntry, &TurnItem)>, visible: &mut HashSet<String>| {
        if batch
            .iter()
            .any(|(_, item)| is_live_subagent_turn_item(item))
        {
            for (entry, _) in batch.iter() {
                visible.insert(entry.id.clone());
            }
        }
        batch.clear();
    };
    for entry in entries {
        let Some(item) =
            event_item(entry).filter(|item| item.item_type() == TurnItemType::Subagent)
        else {
            flush(&mut batch, &mut visible);
            continue;
        };
        if let Some((_, previous)) = batch.last()
            && (previous.run_id != item.run_id
                || previous.provider_turn_id != item.provider_turn_id)
        {
            flush(&mut batch, &mut visible);
        }
        batch.push((entry, item));
    }
    flush(&mut batch, &mut visible);
    visible
}

/// `runless_key` stands in for the run of entries that have none.
fn timeline_entry_fold_run_id(entry: &TimelineEntry, runless_key: Option<&str>) -> Option<String> {
    if work_item_type_is(entry, TurnItemType::SystemNotice) {
        return None;
    }
    match &entry.kind {
        TimelineEntryKind::Message { message, .. } if message.role == MessageRole::Assistant => {
            message
                .run_id
                .as_deref()
                .or(runless_key)
                .map(str::to_string)
        }
        TimelineEntryKind::Work { entry } => {
            entry.run_id.as_deref().or(runless_key).map(str::to_string)
        }
        TimelineEntryKind::Event { projected_item }
            if timeline_entry_is_persistent_resource_card(entry)
                || projected_item.item.item_type() == TurnItemType::Subagent =>
        {
            projected_item
                .item
                .run_id
                .as_deref()
                .or(runless_key)
                .map(str::to_string)
        }
        _ => None,
    }
}

/// A steer adds input to its existing turn, without creating a new header.
fn timeline_entry_starts_response(entry: &TimelineEntry) -> bool {
    match &entry.kind {
        TimelineEntryKind::Message { message, .. } => {
            message.role == MessageRole::User
                && !matches!(
                    message.input_intent,
                    Some(InputIntent::Steer | InputIntent::PromotedQueuedToSteer)
                )
        }
        TimelineEntryKind::Work { entry } => entry.item_type == Some(TurnItemType::Notification),
        _ => false,
    }
}

fn response_boundary_run_id(entry: &TimelineEntry) -> Option<&str> {
    match &entry.kind {
        TimelineEntryKind::Message { message, .. } => message.run_id.as_deref(),
        TimelineEntryKind::Work { entry } => entry.run_id.as_deref(),
        _ => None,
    }
}

/// A promptless provider restart replaces the native turn without adding a
/// user message. Keep every provider turn since the initiating prompt in one
/// visual response until the replacement turn settles. Steers keep that
/// boundary; an automatic wake starts a new response with its notification.
fn last_response_boundary_index(timeline_entries: &[TimelineEntry]) -> Option<usize> {
    timeline_entries
        .iter()
        .rposition(timeline_entry_starts_response)
}

fn derive_active_visual_response_run_ids(
    timeline_entries: &[TimelineEntry],
    unsettled_run_id: Option<&str>,
    is_working: bool,
) -> HashSet<String> {
    let mut run_ids = HashSet::new();
    let Some(unsettled_run_id) = unsettled_run_id else {
        return run_ids;
    };

    run_ids.insert(unsettled_run_id.to_string());
    if !is_working {
        return run_ids;
    }

    let start = last_response_boundary_index(timeline_entries).map_or(0, |index| index + 1);
    for entry in timeline_entries.iter().skip(start) {
        if let Some(run_id) = timeline_entry_run_id(entry) {
            run_ids.insert(run_id.to_string());
        }
    }
    run_ids
}

fn timeline_entry_failed_item(entry: &TimelineEntry) -> Option<&TurnItem> {
    let item = match &entry.kind {
        TimelineEntryKind::Event { projected_item } => Some(&projected_item.item),
        TimelineEntryKind::Work { entry } => entry
            .projected_item
            .as_ref()
            .map(|projected| &projected.item),
        _ => None,
    }?;
    (item.item_type() == TurnItemType::Error
        && item.status == TurnItemStatus::Failed
        && item.parent_item_id.is_none())
    .then_some(item)
}

fn failed_timeline_run_ids(
    entries: &[TimelineEntry],
    latest_run: Option<&TimelineLatestRun>,
) -> HashSet<String> {
    let mut failed = HashSet::new();
    if let Some(latest_run) = latest_run.filter(|run| run.status == RunStatus::Failed) {
        failed.insert(latest_run.run_id.clone());
    }
    for entry in entries {
        if let Some(run_id) =
            timeline_entry_failed_item(entry).and_then(|item| non_empty(item.run_id.as_deref()))
        {
            failed.insert(run_id.to_string());
        }
    }
    failed
}

struct DeriveTurnFoldsInput<'a> {
    timeline_entries: &'a [TimelineEntry],
    terminal_assistant_message_ids: &'a HashSet<String>,
    latest_run: Option<&'a TimelineLatestRun>,
    unfolded_run_ids: &'a HashSet<String>,
    /// Keeps the latest runless response open; V2 work must not reopen imported turns.
    runless_work_active: bool,
    live_subagent_entry_ids: &'a HashSet<String>,
    /// Finished replies a steer superseded; they answer their prompt, so they stay out.
    complete_superseded_reply_entry_ids: &'a HashSet<String>,
}

/// Settled turns fold activity before their terminal assistant message behind
/// a "Worked for ..." row. Ordinary trailing work joins the fold, while failures
/// and work still in progress stay visible. A prompt without a run (a
/// provider-native subagent, or a turn imported from V1) folds its response
/// the same way.
fn derive_turn_folds(input: DeriveTurnFoldsInput<'_>) -> HashMap<String, TurnFold> {
    let mut interrupted_run_ids: HashSet<&str> = HashSet::new();
    let mut prompt_index_by_run_id: HashMap<&str, usize> = HashMap::new();
    for (index, entry) in input.timeline_entries.iter().enumerate() {
        if timeline_entry_starts_response(entry)
            && let Some(run_id) = response_boundary_run_id(entry)
        {
            prompt_index_by_run_id.entry(run_id).or_insert(index);
        }
        if let Some(item) = event_item(entry)
            && let Some(run_id) = item.run_id.as_deref()
            && matches!(
                item.item_type(),
                TurnItemType::RunInterruptRequest | TurnItemType::RunInterruptResult
            )
        {
            interrupted_run_ids.insert(run_id);
        }
    }

    struct TurnGroup<'e> {
        entries: Vec<&'e TimelineEntry>,
        terminal_entry: Option<&'e TimelineEntry>,
        has_streaming_message: bool,
        /// The user message or notification that kicked the turn off. Entry timestamps alone
        /// undercount the duration (the first entry appears only once the
        /// provider starts producing output), and a turn cut short by a steer may
        /// hold a single instantaneous commentary message.
        start_boundary: Option<String>,
        anchor_entry_id: String,
    }
    let mut group_order: Vec<String> = Vec::new();
    let mut groups_by_run_id: HashMap<String, TurnGroup<'_>> = HashMap::new();
    let mut runless_failed_keys: HashSet<String> = HashSet::new();

    // Fold state is keyed by run, so each runless prompt lends its response a
    // stable key of its own. Decide per prompt, not per thread: a V1 thread's
    // first V2 run must not unfold every imported turn above it.
    let mut runless_key: Option<String> = None;
    let mut response_key: Option<String> = None;
    let mut pending_boundary: Option<(String, String)> = None;
    for (index, entry) in input.timeline_entries.iter().enumerate() {
        if timeline_entry_starts_response(entry) {
            pending_boundary = input
                .timeline_entries
                .get(index + 1)
                .map(|next_entry| (entry.created_at.clone(), next_entry.id.clone()));
            let boundary_run_id = response_boundary_run_id(entry);
            runless_key = match boundary_run_id {
                None => Some(format!("runless:{}", entry.id)),
                Some(_) => None,
            };
            response_key = boundary_run_id.map(str::to_string).or(runless_key.clone());
            continue;
        }
        let Some(own_run_id) = timeline_entry_fold_run_id(entry, runless_key.as_deref())
            .filter(|run_id| !run_id.is_empty())
        else {
            continue;
        };
        // Resuming a subagent moves its item to the resuming run, but the item
        // keeps its place in the turn that started it. It folds with that turn,
        // so the later run's fold still opens under its own prompt.
        let run_id = match &response_key {
            Some(response_key)
                if prompt_index_by_run_id
                    .get(own_run_id.as_str())
                    .is_some_and(|prompt_index| *prompt_index > index) =>
            {
                response_key.clone()
            }
            _ => own_run_id,
        };
        if runless_key.as_deref() == Some(run_id.as_str())
            && timeline_entry_failed_item(entry).is_some()
        {
            runless_failed_keys.insert(run_id.clone());
        }
        let group = groups_by_run_id.entry(run_id.clone()).or_insert_with(|| {
            group_order.push(run_id.clone());
            // Each user boundary starts at most one turn; a second turn after the
            // same user message (e.g. a steer-superseded continuation) falls back
            // to its own first entry.
            let (start_boundary, anchor_entry_id) = match pending_boundary.take() {
                Some((created_at, anchor_entry_id)) => (Some(created_at), anchor_entry_id),
                None => (None, entry.id.clone()),
            };
            TurnGroup {
                entries: Vec::new(),
                terminal_entry: None,
                has_streaming_message: false,
                start_boundary,
                anchor_entry_id,
            }
        });
        group.entries.push(entry);
        if let Some(message) = message_of(entry) {
            if input.terminal_assistant_message_ids.contains(&message.id) {
                group.terminal_entry = Some(entry);
            }
            if message.streaming {
                group.has_streaming_message = true;
            }
        }
    }

    let mut folds_by_anchor_entry_id = HashMap::new();
    for run_id in group_order {
        let Some(group) = groups_by_run_id.get(&run_id) else {
            continue;
        };
        if input.unfolded_run_ids.contains(&run_id)
            || interrupted_run_ids.contains(run_id.as_str())
            || runless_failed_keys.contains(&run_id)
            || (input.runless_work_active && runless_key.as_deref() == Some(run_id.as_str()))
        {
            continue;
        }
        if group.has_streaming_message {
            continue;
        }
        let mut hidden_entry_ids = HashSet::new();
        let terminal_entry_id = group.terminal_entry.map(|entry| entry.id.as_str());
        let terminal_entry_index = match terminal_entry_id {
            Some(terminal_id) => group
                .entries
                .iter()
                .position(|entry| entry.id == terminal_id)
                .unwrap_or(group.entries.len()),
            None => group.entries.len(),
        };
        for (index, entry) in group.entries.iter().enumerate() {
            if Some(entry.id.as_str()) == terminal_entry_id {
                continue;
            }
            let work = work_entry(entry);
            let is_compaction =
                work.is_some_and(|work| source_activity_kind_is(work, "context-compaction"));
            let is_foldable_trailing_activity = work.is_some_and(|work| {
                work.tool_lifecycle_status != Some(ToolLifecycleStatus::InProgress)
                    && !work_entry_display_indicates_tool_failure(work)
            });
            if !is_compaction && index > terminal_entry_index && !is_foldable_trailing_activity {
                continue;
            }
            // Linked resources and still-working children can outlive their
            // launching run and stay visible after the surrounding work folds.
            if timeline_entry_is_persistent_resource_card(entry)
                || input.live_subagent_entry_ids.contains(&entry.id)
                || input
                    .complete_superseded_reply_entry_ids
                    .contains(&entry.id)
            {
                continue;
            }
            if work.is_some_and(|work| work.item_type == Some(TurnItemType::Notification)) {
                continue;
            }
            hidden_entry_ids.insert(entry.id.clone());
        }
        if hidden_entry_ids.is_empty() {
            continue;
        }
        // A lone compaction row stays visible on its own; it only folds away as
        // part of a turn that already folds other work.
        let hides_non_compaction_work = group.entries.iter().any(|entry| {
            hidden_entry_ids.contains(&entry.id)
                && !work_entry(entry)
                    .is_some_and(|work| source_activity_kind_is(work, "context-compaction"))
        });
        if !hides_non_compaction_work {
            continue;
        }

        let (Some(first_entry), Some(last_entry)) = (group.entries.first(), group.entries.last())
        else {
            continue;
        };

        let latest_run = input.latest_run.filter(|run| run.run_id == run_id);
        let is_latest_interrupted_turn =
            latest_run.is_some_and(|run| run.status == RunStatus::Interrupted);
        // A turn cut short by a steer leaves trailing work entries behind its
        // terminal message — take whichever ended last.
        let last_entry_end = match message_of(last_entry) {
            Some(message) => message.updated_at.as_str(),
            None => last_entry.created_at.as_str(),
        };
        let latest_run_timing = latest_run.and_then(|run| {
            Some((
                non_empty(run.started_at.as_deref())?,
                non_empty(run.completed_at.as_deref())?,
            ))
        });
        let elapsed_ms = match latest_run_timing {
            Some((started_at, completed_at)) => compute_elapsed_ms(started_at, completed_at),
            None => compute_elapsed_ms(
                group
                    .start_boundary
                    .as_deref()
                    .unwrap_or(&first_entry.created_at),
                max_iso_timestamp(
                    group
                        .terminal_entry
                        .and_then(message_of)
                        .map(|message| message.updated_at.as_str()),
                    Some(last_entry_end),
                )
                .unwrap_or(last_entry_end),
            ),
        };
        let duration = elapsed_ms
            .map(format_duration)
            .filter(|duration| !duration.is_empty());
        let label = match (is_latest_interrupted_turn, duration) {
            (true, Some(duration)) => format!("You stopped after {duration}"),
            (true, None) => "You stopped this response".to_string(),
            (false, Some(duration)) => format!("Worked for {duration}"),
            (false, None) => "Worked".to_string(),
        };

        folds_by_anchor_entry_id.insert(
            group.anchor_entry_id.clone(),
            TurnFold {
                run_id: run_id.clone(),
                created_at: group
                    .start_boundary
                    .clone()
                    .unwrap_or_else(|| first_entry.created_at.clone()),
                hidden_entry_ids,
                label,
            },
        );
    }
    folds_by_anchor_entry_id
}

/// When a settled turn ends with tool calls after its terminal text, treat the
/// text and tools as one visual response. The message metadata becomes the
/// footer for the whole block instead of separating the prose from the tools.
fn attach_trailing_tool_groups_to_assistant(
    rows: Vec<MessagesTimelineRowContent>,
) -> Vec<MessagesTimelineRowContent> {
    let mut message_rows_without_meta: HashSet<String> = HashSet::new();
    let mut meta_rows_after_index: HashMap<usize, MessagesTimelineRowContent> = HashMap::new();

    for (message_index, row) in rows.iter().enumerate() {
        let MessagesTimelineRowContent::Message {
            id: row_id,
            message,
            projected_item,
            show_assistant_meta,
            show_assistant_copy_button,
            assistant_copy_streaming,
            ..
        } = row
        else {
            continue;
        };
        let Some(run_id) = message.run_id.as_deref() else {
            continue;
        };
        if message.role != MessageRole::Assistant || !show_assistant_meta {
            continue;
        }

        let mut last_trailing_work_index: Option<usize> = None;
        let mut has_trailing_tool_group = false;
        for (index, candidate) in rows.iter().enumerate().skip(message_index + 1) {
            match candidate {
                MessagesTimelineRowContent::Message { .. } => break,
                MessagesTimelineRowContent::WorkToggle {
                    run_id: candidate_run_id,
                    ..
                } if candidate_run_id.as_deref() == Some(run_id) => {
                    has_trailing_tool_group = true;
                    last_trailing_work_index = Some(index);
                }
                MessagesTimelineRowContent::Event {
                    resource_summary: true,
                    projected_item,
                    ..
                } if projected_item.item.run_id.as_deref() == Some(run_id) => {
                    has_trailing_tool_group = true;
                    last_trailing_work_index = Some(index);
                }
                MessagesTimelineRowContent::Work {
                    grouped_entries,
                    is_expanded_tool_group,
                    ..
                } if grouped_entries
                    .iter()
                    .any(|entry| entry.run_id.as_deref() == Some(run_id)) =>
                {
                    if !is_expanded_tool_group
                        && grouped_entries.iter().any(|entry| {
                            work_log_entry_is_tool_like(entry)
                                || entry.projected_item.as_ref().is_some_and(|projected| {
                                    projected.item.item_type() == TurnItemType::Error
                                        && projected.item.status == TurnItemStatus::Failed
                                })
                        })
                    {
                        has_trailing_tool_group = true;
                    }
                    if has_trailing_tool_group {
                        last_trailing_work_index = Some(index);
                    }
                }
                _ => {}
            }
        }

        let Some(last_trailing_work_index) = last_trailing_work_index else {
            continue;
        };

        message_rows_without_meta.insert(row_id.clone());
        meta_rows_after_index.insert(
            last_trailing_work_index,
            MessagesTimelineRowContent::AssistantMeta {
                id: format!("assistant-meta:{}", message.id),
                projected_item: projected_item.clone(),
                created_at: rows
                    .get(last_trailing_work_index)
                    .and_then(MessagesTimelineRowContent::created_at)
                    .unwrap_or(&message.updated_at)
                    .to_string(),
                message: message.clone(),
                show_assistant_copy_button: *show_assistant_copy_button,
                assistant_copy_streaming: *assistant_copy_streaming,
            },
        );
    }

    let mut result = Vec::with_capacity(rows.len() + meta_rows_after_index.len());
    for (index, mut row) in rows.into_iter().enumerate() {
        if let MessagesTimelineRowContent::Message {
            id,
            show_assistant_meta,
            show_assistant_copy_button,
            ..
        } = &mut row
            && message_rows_without_meta.contains(id.as_str())
        {
            *show_assistant_meta = false;
            *show_assistant_copy_button = false;
        }
        result.push(row);
        if let Some(meta_row) = meta_rows_after_index.remove(&index) {
            result.push(meta_row);
        }
    }
    result
}

// Delegation already has a durable child card. Remove its tool row only after
// the returned task ID identifies that child; pending calls can share a prompt.
fn without_subagent_delegation_rows(entries: Vec<TimelineEntry>) -> Vec<TimelineEntry> {
    let mut children_by_run: HashMap<String, HashSet<String>> = HashMap::new();
    for entry in &entries {
        let Some(item) = event_item(entry) else {
            continue;
        };
        let TurnItemPayload::Subagent {
            subagent_id,
            origin,
            ..
        } = &item.payload
        else {
            continue;
        };
        let Some(run_id) = item.run_id.as_ref() else {
            continue;
        };
        if origin != "app_owned" {
            continue;
        }
        children_by_run
            .entry(run_id.clone())
            .or_default()
            .insert(subagent_id.clone());
    }
    entries
        .into_iter()
        .filter(|entry| {
            let Some(work) = work_entry(entry) else {
                return true;
            };
            if work_entry_display_indicates_tool_failure(work) {
                return true;
            }
            let Some(item) = work
                .projected_item
                .as_ref()
                .map(|projected| &projected.item)
                .or(work.structured_payload.as_ref())
            else {
                return true;
            };
            let TurnItemPayload::DynamicTool {
                tool_name, output, ..
            } = &item.payload
            else {
                return true;
            };
            let Some(run_id) = item.run_id.as_ref() else {
                return true;
            };
            if !matches!(
                item.status,
                TurnItemStatus::Running | TurnItemStatus::Completed
            ) || resolve_t3_mcp_tool_definition(tool_name.as_deref())
                .map(|definition| definition.summary_action)
                != Some(T3McpToolSummaryAction::Delegate)
            {
                return true;
            }
            let output = compact_dynamic_tool_output(output.as_ref());
            if output.as_ref().is_some_and(|output| output.is_error) {
                return true;
            }
            if let Some(task_id) = output.as_ref().and_then(|output| output.task_id.as_ref()) {
                return !children_by_run
                    .get(run_id)
                    .is_some_and(|children| children.contains(task_id));
            }
            true
        })
        .collect()
}

/// A steer or subsequent activity ends thinking even if the provider omits its completion.
fn settle_superseded_reasoning(entries: &[TimelineEntry]) -> Vec<TimelineEntry> {
    let last_index = entries.len().saturating_sub(1);
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut entry = entry.clone();
            if index != last_index
                && let TimelineEntryKind::Work { entry: work } = &mut entry.kind
                && work.item_type == Some(TurnItemType::Reasoning)
                && work.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
            {
                work.tool_lifecycle_status = Some(ToolLifecycleStatus::Completed);
            }
            entry
        })
        .collect()
}

fn timeline_row_entries(entries: &[TimelineEntry]) -> Vec<TimelineEntry> {
    without_subagent_delegation_rows(settle_superseded_reasoning(entries))
}

/// The input of [`derive_messages_timeline_rows`] (upstream's
/// `MessagesTimelineRowsInput`). Absent optional sets are empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessagesTimelineRowsInput {
    pub timeline_entries: Vec<TimelineEntry>,
    pub latest_run: Option<TimelineLatestRun>,
    pub running_run_id: Option<String>,
    pub expanded_run_ids: HashSet<String>,
    pub expanded_attempt_ids: HashSet<String>,
    pub expanded_work_group_ids: HashSet<String>,
    pub is_working: bool,
    /// The live work has no app run (a provider-native subagent thread), so
    /// runless entries are the current response instead of settled history.
    pub runless_work_active: bool,
    pub active_turn_started_at: Option<String>,
    pub turn_diff_summaries: Vec<TurnDiffSummary>,
    pub supports_conversation_rollback: bool,
    /// Task ids of subagents still working, used by the active tool indicator.
    pub live_agent_task_ids: Option<HashSet<String>>,
}

/// The turn folds the timeline would draw, before applying expansion state.
fn derive_timeline_turn_folds(input: &MessagesTimelineRowsInput) -> HashMap<String, TurnFold> {
    let timeline_entries = timeline_row_entries(&input.timeline_entries);
    let unsettled_run_id =
        derive_unsettled_run_id(input.latest_run.as_ref(), input.running_run_id.as_deref());
    let failed_run_ids = failed_timeline_run_ids(&timeline_entries, input.latest_run.as_ref());
    let active_visual_response_run_ids = derive_active_visual_response_run_ids(
        &timeline_entries,
        unsettled_run_id.as_deref(),
        input.is_working,
    );
    let unfolded_run_ids: HashSet<String> = active_visual_response_run_ids
        .into_iter()
        .chain(failed_run_ids)
        .collect();
    let terminal_assistant_message_ids = derive_terminal_assistant_message_ids(&timeline_entries);
    let live_subagent_entry_ids = live_subagent_card_entry_ids(&timeline_entries);
    let complete_superseded_reply_entry_ids =
        complete_superseded_reply_entry_ids(&timeline_entries);
    derive_turn_folds(DeriveTurnFoldsInput {
        timeline_entries: &timeline_entries,
        terminal_assistant_message_ids: &terminal_assistant_message_ids,
        latest_run: input.latest_run.as_ref(),
        unfolded_run_ids: &unfolded_run_ids,
        runless_work_active: input.is_working && input.runless_work_active,
        live_subagent_entry_ids: &live_subagent_entry_ids,
        complete_superseded_reply_entry_ids: &complete_superseded_reply_entry_ids,
    })
}

pub fn derive_messages_timeline_rows(
    input: &MessagesTimelineRowsInput,
) -> Vec<Arc<MessagesTimelineRow>> {
    let timeline_entries = timeline_row_entries(&input.timeline_entries);
    let mut turn_diff_summary_by_assistant_message_id: HashMap<&str, &TurnDiffSummary> =
        HashMap::new();
    for summary in &input.turn_diff_summaries {
        if let Some(assistant_message_id) = non_empty(summary.assistant_message_id.as_deref()) {
            turn_diff_summary_by_assistant_message_id.insert(assistant_message_id, summary);
        }
    }
    let revert_turn_count_by_user_message_id = if input.supports_conversation_rollback {
        derive_revert_turn_count_by_user_message_id(&timeline_entries, &input.turn_diff_summaries)
    } else {
        HashMap::new()
    };
    let mut next_rows: Vec<MessagesTimelineRowContent> = Vec::new();
    let duration_start_by_message_id = compute_message_duration_start(
        timeline_entries
            .iter()
            .filter_map(message_of)
            .map(TimelineDurationMessage::from),
    );
    let terminal_assistant_message_ids = derive_terminal_assistant_message_ids(&timeline_entries);
    let unsettled_run_id =
        derive_unsettled_run_id(input.latest_run.as_ref(), input.running_run_id.as_deref());
    let failed_run_ids = failed_timeline_run_ids(&timeline_entries, input.latest_run.as_ref());
    let live_subagent_entry_ids = live_subagent_card_entry_ids(&timeline_entries);
    let complete_superseded_reply_ids = complete_superseded_reply_entry_ids(&timeline_entries);
    let superseded_folds_by_anchor_entry_id = derive_superseded_attempt_folds(
        &timeline_entries,
        &failed_run_ids,
        &live_subagent_entry_ids,
        &complete_superseded_reply_ids,
    );
    let active_visual_response_run_ids = derive_active_visual_response_run_ids(
        &timeline_entries,
        unsettled_run_id.as_deref(),
        input.is_working,
    );
    let runless_work_active = input.is_working && input.runless_work_active;
    let unfolded_run_ids: HashSet<String> = active_visual_response_run_ids
        .iter()
        .chain(&failed_run_ids)
        .cloned()
        .collect();
    let folds_by_anchor_entry_id = derive_turn_folds(DeriveTurnFoldsInput {
        timeline_entries: &timeline_entries,
        terminal_assistant_message_ids: &terminal_assistant_message_ids,
        latest_run: input.latest_run.as_ref(),
        unfolded_run_ids: &unfolded_run_ids,
        runless_work_active,
        live_subagent_entry_ids: &live_subagent_entry_ids,
        complete_superseded_reply_entry_ids: &complete_superseded_reply_ids,
    });
    let mut collapsed_entry_ids: HashSet<&str> = HashSet::new();
    for fold in folds_by_anchor_entry_id.values() {
        if !input.expanded_run_ids.contains(&fold.run_id) {
            collapsed_entry_ids.extend(fold.hidden_entry_ids.iter().map(String::as_str));
        }
    }
    let mut collapsed_superseded_entry_ids: HashSet<&str> = HashSet::new();
    for fold in superseded_folds_by_anchor_entry_id.values() {
        if !input.expanded_attempt_ids.contains(&fold.attempt_id) {
            collapsed_superseded_entry_ids.extend(fold.hidden_entry_ids.iter().map(String::as_str));
        }
    }
    let run_id_is_active_response = |run_id: Option<&str>| match run_id {
        None => runless_work_active,
        Some(run_id) => active_visual_response_run_ids.contains(run_id),
    };
    let work_entry_is_in_active_run = |entry: &WorkLogEntry| {
        input.is_working
            && entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
            && match entry.run_id.as_deref() {
                None => runless_work_active,
                Some(run_id) => unsettled_run_id.as_deref() == Some(run_id),
            }
    };

    // A steer continues the current turn. Keep its elapsed-time header below
    // the initiating prompt (or automatic wake), rather than moving it down.
    let active_turn_header_index = if input.is_working {
        last_response_boundary_index(&timeline_entries).map_or(0, |index| index + 1)
    } else {
        timeline_entries.len()
    };

    // Contiguous trailing work entries of the active run collapse into one live
    // row that survives between actions: while a tool runs it shows that tool,
    // and once everything settles it keeps the latest tool in past tense
    // instead of vanishing (#8984).
    let mut active_tool_entries: Vec<(&TimelineEntry, &WorkLogEntry)> = Vec::new();
    if input.is_working && (unsettled_run_id.is_some() || runless_work_active) {
        let mut tail_attempt_id: Option<Option<&str>> = None;
        for entry in timeline_entries.iter().skip(active_turn_header_index).rev() {
            let Some(work) = work_entry(entry) else {
                break;
            };
            if work.tone == WorkTone::Error
                || source_activity_kind_is(work, "runtime.error")
                || work.item_type == Some(TurnItemType::SystemNotice)
                || work.item_type == Some(TurnItemType::Notification)
                || !run_id_is_active_response(work.run_id.as_deref())
                || source_activity_kind_is(work, "context-compaction")
                || collapsed_entry_ids.contains(entry.id.as_str())
                || collapsed_superseded_entry_ids.contains(entry.id.as_str())
                || folds_by_anchor_entry_id.contains_key(&entry.id)
                || superseded_folds_by_anchor_entry_id.contains_key(&entry.id)
            {
                break;
            }
            let attempt_id = entry.attempt.as_ref().map(|attempt| attempt.id.as_str());
            match tail_attempt_id {
                None => tail_attempt_id = Some(attempt_id),
                Some(tail) if tail != attempt_id => break,
                Some(_) => {}
            }
            active_tool_entries.push((entry, work));
        }
    }
    active_tool_entries.reverse();
    let visible_active_tool_entries: Vec<(&TimelineEntry, &WorkLogEntry)> = active_tool_entries
        .iter()
        .copied()
        .filter(|(_, work)| work_entry_is_visible_in_group(work, true))
        .collect();
    let active_work_anchor = active_tool_entries.first().map(|(entry, _)| *entry);
    let latest_visible_tool_entry = visible_active_tool_entries.last().copied();
    let latest_running_tool_entry = visible_active_tool_entries
        .iter()
        .rev()
        .copied()
        .find(|(_, work)| work_entry_is_active_turn_activity(work));
    let latest_tool_keeps_activity_live = latest_running_tool_entry.is_some()
        || latest_visible_tool_entry
            .is_some_and(|(_, work)| work_entry_indicates_tool_success(work));
    let latest_tool_failed = latest_running_tool_entry.is_none()
        && latest_visible_tool_entry.is_some_and(|(_, work)| {
            work.tool_lifecycle_status != Some(ToolLifecycleStatus::Declined)
                && work_entry_display_indicates_tool_failure(work)
        });

    let latest_thought_entry =
        visible_active_tool_entries
            .iter()
            .rev()
            .copied()
            .find(|(_, work)| {
                work.item_type == Some(TurnItemType::Reasoning)
                    && work
                        .detail
                        .as_deref()
                        .is_some_and(|detail| !js_trim(detail).is_empty())
            });
    let active_work_placement_entry_id =
        latest_visible_tool_entry.map(|(entry, _)| entry.id.as_str());
    struct ActiveWorkRow {
        row: MessagesTimelineRowContent,
        group_id: String,
        created_at: String,
        grouped_entries: Vec<WorkLogEntry>,
        expanded: bool,
        active: bool,
    }
    let active_work_row = match (active_work_anchor, latest_visible_tool_entry) {
        (Some(anchor), Some((_, latest_visible))) if !latest_tool_failed => {
            let group_id = work_group_id(&anchor.id);
            let expanded = input.expanded_work_group_ids.contains(&group_id);
            let grouped_entries: Vec<WorkLogEntry> = visible_active_tool_entries
                .iter()
                .map(|(_, work)| (*work).clone())
                .collect();
            let row = MessagesTimelineRowContent::WorkLive {
                id: if latest_tool_keeps_activity_live {
                    LIVE_ACTIVITY_ROW_ID.to_string()
                } else {
                    format!("work-live:{}", anchor.id)
                },
                created_at: anchor.created_at.clone(),
                entry: latest_running_tool_entry
                    .map_or(latest_visible, |(_, work)| work)
                    .clone(),
                grouped_entries: grouped_entries.clone(),
                group_id: group_id.clone(),
                expanded,
                active: latest_tool_keeps_activity_live,
                thought: latest_thought_entry.map(|(_, work)| work.clone()),
            };
            Some(ActiveWorkRow {
                row,
                group_id,
                created_at: anchor.created_at.clone(),
                grouped_entries,
                expanded,
                active: latest_tool_keeps_activity_live,
            })
        }
        _ => None,
    };
    let active_work_entry_ids: HashSet<&str> = if active_work_row.is_some() || latest_tool_failed {
        active_tool_entries
            .iter()
            .map(|(entry, _)| entry.id.as_str())
            .collect()
    } else {
        HashSet::new()
    };
    let working_row = || MessagesTimelineRowContent::Working {
        id: WORKING_INDICATOR_ROW_ID.to_string(),
        created_at: input.active_turn_started_at.clone(),
    };
    let mut has_activity_row = false;
    let mut has_active_compaction = false;

    let mut index = 0;
    while index < timeline_entries.len() {
        let timeline_entry = &timeline_entries[index];

        if input.is_working && index == active_turn_header_index {
            next_rows.push(working_row());
        }

        if Some(timeline_entry.id.as_str()) == active_work_placement_entry_id
            && let Some(active_work_row) = &active_work_row
        {
            next_rows.push(active_work_row.row.clone());
            has_activity_row |= active_work_row.active;
            if active_work_row.expanded {
                next_rows.push(expanded_work_group_row(
                    &active_work_row.group_id,
                    &active_work_row.created_at,
                    active_work_row.grouped_entries.clone(),
                ));
            }
        }

        // The terminal interrupt result is the useful timeline marker. The
        // preceding request is transient bookkeeping and duplicates that marker.
        if event_item(timeline_entry)
            .is_some_and(|item| item.item_type() == TurnItemType::RunInterruptRequest)
        {
            index += 1;
            continue;
        }

        if let Some(turn_fold) = folds_by_anchor_entry_id.get(&timeline_entry.id) {
            next_rows.push(MessagesTimelineRowContent::TurnFold {
                id: format!("turn-fold:{}", turn_fold.run_id),
                created_at: turn_fold.created_at.clone(),
                run_id: turn_fold.run_id.clone(),
                label: turn_fold.label.clone(),
                expanded: input.expanded_run_ids.contains(&turn_fold.run_id),
            });
        }

        if collapsed_entry_ids.contains(timeline_entry.id.as_str()) {
            index += 1;
            continue;
        }

        if let Some(superseded_fold) = superseded_folds_by_anchor_entry_id.get(&timeline_entry.id) {
            next_rows.push(MessagesTimelineRowContent::AttemptFold {
                id: format!("attempt-fold:{}", superseded_fold.attempt_id),
                created_at: superseded_fold.created_at.clone(),
                run_id: superseded_fold.run_id.clone(),
                attempt_id: superseded_fold.attempt_id.clone(),
                label: "Superseded attempt".to_string(),
                expanded: input
                    .expanded_attempt_ids
                    .contains(&superseded_fold.attempt_id),
            });
        }

        if collapsed_superseded_entry_ids.contains(timeline_entry.id.as_str())
            || active_work_entry_ids.contains(timeline_entry.id.as_str())
        {
            index += 1;
            continue;
        }

        match &timeline_entry.kind {
            TimelineEntryKind::Work { entry: work }
                if source_activity_kind_is(work, "context-compaction") =>
            {
                let active = work_entry_is_in_active_run(work);
                has_active_compaction |= active;
                next_rows.push(MessagesTimelineRowContent::ContextCompaction {
                    id: timeline_entry.id.clone(),
                    created_at: timeline_entry.created_at.clone(),
                    label: work.label.clone(),
                    active,
                });
                index += 1;
            }
            TimelineEntryKind::Work { entry: work } => {
                if work
                    .run_id
                    .as_ref()
                    .is_some_and(|run_id| failed_run_ids.contains(run_id))
                    || (work.item_type == Some(TurnItemType::Error)
                        && work.tool_lifecycle_status == Some(ToolLifecycleStatus::Failed))
                    || work.tone == WorkTone::Error
                    || source_activity_kind_is(work, "runtime.error")
                    || work.item_type == Some(TurnItemType::SystemNotice)
                    || work.item_type == Some(TurnItemType::Notification)
                {
                    next_rows.push(MessagesTimelineRowContent::Work {
                        id: timeline_entry.id.clone(),
                        created_at: timeline_entry.created_at.clone(),
                        grouped_entries: vec![work.clone()],
                        is_expanded_tool_group: false,
                        display_label: None,
                    });
                    index += 1;
                    continue;
                }
                let mut grouped_entries: Vec<&WorkLogEntry> = vec![work];
                let mut cursor = index + 1;
                while let Some(next_entry) = timeline_entries.get(cursor) {
                    let Some(next_work) = work_entry(next_entry) else {
                        break;
                    };
                    if source_activity_kind_is(next_work, "context-compaction")
                        || next_work.tone == WorkTone::Error
                        || source_activity_kind_is(next_work, "runtime.error")
                        || next_work.item_type == Some(TurnItemType::SystemNotice)
                        || next_work.item_type == Some(TurnItemType::Notification)
                        || active_work_entry_ids.contains(next_entry.id.as_str())
                        || collapsed_entry_ids.contains(next_entry.id.as_str())
                        || collapsed_superseded_entry_ids.contains(next_entry.id.as_str())
                        || folds_by_anchor_entry_id.contains_key(&next_entry.id)
                        || superseded_folds_by_anchor_entry_id.contains_key(&next_entry.id)
                        || next_work.run_id != work.run_id
                        || next_entry.attempt.as_ref().map(|attempt| &attempt.id)
                            != timeline_entry.attempt.as_ref().map(|attempt| &attempt.id)
                    {
                        break;
                    }
                    grouped_entries.push(next_work);
                    cursor += 1;
                }
                let visible_grouped_entries: Vec<&WorkLogEntry> = grouped_entries
                    .into_iter()
                    .filter(|entry| {
                        work_entry_is_visible_in_group(entry, work_entry_is_in_active_run(entry))
                    })
                    .collect();
                if !visible_grouped_entries.is_empty() {
                    let owned_visible_entries = || -> Vec<WorkLogEntry> {
                        visible_grouped_entries
                            .iter()
                            .map(|entry| (*entry).clone())
                            .collect()
                    };
                    let latest_active_tool_entry = visible_grouped_entries
                        .iter()
                        .rev()
                        .find(|entry| work_entry_is_in_active_run(entry));
                    if let Some(latest_active_tool_entry) = latest_active_tool_entry {
                        let group_id = work_group_id(&timeline_entry.id);
                        let expanded = input.expanded_work_group_ids.contains(&group_id);
                        next_rows.push(MessagesTimelineRowContent::WorkLive {
                            id: format!("work-live:{}", timeline_entry.id),
                            created_at: timeline_entry.created_at.clone(),
                            entry: (*latest_active_tool_entry).clone(),
                            grouped_entries: owned_visible_entries(),
                            group_id: group_id.clone(),
                            expanded,
                            active: true,
                            thought: None,
                        });
                        has_activity_row = true;
                        if expanded {
                            next_rows.push(expanded_work_group_row(
                                &group_id,
                                &timeline_entry.created_at,
                                owned_visible_entries(),
                            ));
                        }
                    } else if let [single_entry] = visible_grouped_entries.as_slice()
                        && work_log_entry_is_tool_like(single_entry)
                    {
                        let display_label =
                            if tool_group_action(single_entry) == ToolGroupAction::Edit {
                                summarize_tool_group(&visible_grouped_entries).summary
                            } else {
                                single_tool_call_label(single_entry)
                            };
                        next_rows.push(MessagesTimelineRowContent::Work {
                            id: timeline_entry.id.clone(),
                            created_at: timeline_entry.created_at.clone(),
                            grouped_entries: owned_visible_entries(),
                            is_expanded_tool_group: false,
                            display_label: Some(display_label),
                        });
                    } else {
                        let group_id = work_group_id(&timeline_entry.id);
                        let expanded = input.expanded_work_group_ids.contains(&group_id);
                        let summary_kind = tool_group_summary_kind(&visible_grouped_entries);
                        let primary_source_entry = visible_grouped_entries
                            .iter()
                            .find(|entry| entry.tool_source.is_some());
                        let primary_source_key = primary_source_entry
                            .and_then(|entry| entry.tool_source.as_ref())
                            .map(|source| source.key.as_str())
                            .filter(|key| !key.is_empty());
                        let primary_source_icon = primary_source_key.and_then(|key| {
                            visible_grouped_entries
                                .iter()
                                .find(|entry| {
                                    entry.tool_source.as_ref().map(|source| source.key.as_str())
                                        == Some(key)
                                        && entry.tool_icon.is_some()
                                })
                                .and_then(|entry| entry.tool_icon.clone())
                                .or_else(|| {
                                    primary_source_entry
                                        .and_then(|entry| entry.tool_source.as_ref())
                                        .and_then(|source| source.icon.clone())
                                })
                        });
                        let group_tool_surface = primary_source_entry
                            .and_then(|entry| entry.tool_surface)
                            .or_else(|| {
                                visible_grouped_entries
                                    .iter()
                                    .rev()
                                    .find_map(|entry| entry.tool_surface)
                            });
                        let group_tool_icon = primary_source_icon.or_else(|| {
                            visible_grouped_entries
                                .iter()
                                .rev()
                                .find_map(|entry| entry.tool_icon.clone())
                        });
                        let latest_tool_entry = visible_grouped_entries
                            .iter()
                            .rev()
                            .find(|entry| work_log_entry_is_tool_like(entry));
                        let single_entry = match visible_grouped_entries.as_slice() {
                            [single_entry] => Some(*single_entry),
                            _ => None,
                        };
                        let uses_single_tool_call_label = single_entry.filter(|single_entry| {
                            work_log_entry_is_tool_like(single_entry)
                                && tool_group_action(single_entry) != ToolGroupAction::Edit
                        });
                        let summary_tool_icon = uses_single_tool_call_label.and_then(|entry| {
                            resolve_work_entry_tool_presentation(
                                entry,
                                Some(ToolLifecycleStatus::Completed),
                            )
                            .map(|presentation| presentation.icon)
                        });
                        let summary = match (uses_single_tool_call_label, single_entry) {
                            (Some(entry), _) => single_tool_call_label(entry),
                            (None, Some(entry)) if !work_log_entry_is_tool_like(entry) => {
                                entry.label.clone()
                            }
                            _ => summarize_tool_group(&visible_grouped_entries).summary,
                        };
                        next_rows.push(MessagesTimelineRowContent::WorkToggle {
                            id: format!("work-toggle:{}", timeline_entry.id),
                            created_at: timeline_entry.created_at.clone(),
                            run_id: work.run_id.clone(),
                            group_id: group_id.clone(),
                            hidden_count: visible_grouped_entries.len(),
                            expanded,
                            summary,
                            summary_kind,
                            tool_surface: group_tool_surface,
                            tool_icon: group_tool_icon,
                            summary_tool_icon,
                            has_failure: latest_tool_entry.is_some_and(|entry| {
                                work_entry_display_indicates_tool_failure(entry)
                            }),
                        });
                        if expanded {
                            next_rows.push(expanded_work_group_row(
                                &group_id,
                                &timeline_entry.created_at,
                                owned_visible_entries(),
                            ));
                        }
                    }
                }
                index = cursor;
            }
            TimelineEntryKind::ProposedPlan { proposed_plan } => {
                next_rows.push(MessagesTimelineRowContent::ProposedPlan {
                    id: timeline_entry.id.clone(),
                    created_at: timeline_entry.created_at.clone(),
                    proposed_plan: proposed_plan.clone(),
                });
                index += 1;
            }
            TimelineEntryKind::Event { projected_item } => {
                let item = &projected_item.item;
                if item.item_type() == TurnItemType::Subagent
                    && let Some(MessagesTimelineRowContent::Event {
                        projected_item: previous_item,
                        subagents,
                        ..
                    }) = next_rows.last_mut()
                    && previous_item.item.item_type() == TurnItemType::Subagent
                    && previous_item.item.run_id == item.run_id
                    && previous_item.item.provider_turn_id == item.provider_turn_id
                {
                    let grouped = subagents.get_or_insert_with(|| vec![previous_item.clone()]);
                    grouped.push(projected_item.clone());
                    index += 1;
                    continue;
                }
                next_rows.push(MessagesTimelineRowContent::Event {
                    id: timeline_entry.id.clone(),
                    created_at: timeline_entry.created_at.clone(),
                    projected_item: projected_item.clone(),
                    subagents: None,
                    resource_summary: false,
                });
                index += 1;
            }
            TimelineEntryKind::Message {
                message,
                projected_item,
            } => {
                let assistant_response_still_in_progress = message.role == MessageRole::Assistant
                    && run_id_is_active_response(message.run_id.as_deref());

                let duration_start = duration_start_by_message_id
                    .get(&message.id)
                    .cloned()
                    .unwrap_or_else(|| message.created_at.clone());

                // While the turn is still running, the latest assistant message is only
                // provisionally terminal — withhold the metadata row until the turn
                // settles so commentary doesn't flash timestamps mid-work.
                let show_assistant_meta = message.role == MessageRole::Assistant
                    && terminal_assistant_message_ids.contains(&message.id)
                    && !assistant_response_still_in_progress;

                next_rows.push(MessagesTimelineRowContent::Message {
                    id: timeline_entry.id.clone(),
                    created_at: timeline_entry.created_at.clone(),
                    message: message.clone(),
                    projected_item: projected_item.clone(),
                    duration_start,
                    show_assistant_meta,
                    show_assistant_copy_button: show_assistant_meta,
                    assistant_copy_streaming: message.streaming
                        || assistant_response_still_in_progress,
                    assistant_turn_diff_summary: if message.role == MessageRole::Assistant {
                        turn_diff_summary_by_assistant_message_id
                            .get(message.id.as_str())
                            .map(|summary| (*summary).clone())
                    } else {
                        None
                    },
                    revert_turn_count: if message.role == MessageRole::User {
                        revert_turn_count_by_user_message_id
                            .get(&message.id)
                            .copied()
                    } else {
                        None
                    },
                });
                index += 1;
            }
        }
    }

    let has_working_row = next_rows
        .iter()
        .any(|row| matches!(row, MessagesTimelineRowContent::Working { .. }));
    if input.is_working && !has_working_row && active_turn_header_index == timeline_entries.len() {
        next_rows.push(working_row());
    }
    if input.is_working && !has_active_compaction && (!has_activity_row || latest_tool_failed) {
        // A failed latest tool hands the row back to thinking, but its group
        // stays reachable through the same disclosure the live row offers.
        let failed_group_anchor = if latest_tool_failed {
            active_work_anchor
        } else {
            None
        };
        if let Some(failed_group_anchor) = failed_group_anchor {
            let group_id = work_group_id(&failed_group_anchor.id);
            let expanded = input.expanded_work_group_ids.contains(&group_id);
            next_rows.push(MessagesTimelineRowContent::Thinking {
                id: LIVE_ACTIVITY_ROW_ID.to_string(),
                created_at: input.active_turn_started_at.clone(),
                group_id: Some(group_id.clone()),
                expanded: Some(expanded),
            });
            if expanded {
                next_rows.push(expanded_work_group_row(
                    &group_id,
                    &failed_group_anchor.created_at,
                    visible_active_tool_entries
                        .iter()
                        .map(|(_, work)| (*work).clone())
                        .collect(),
                ));
            }
        } else {
            next_rows.push(MessagesTimelineRowContent::Thinking {
                id: LIVE_ACTIVITY_ROW_ID.to_string(),
                created_at: input.active_turn_started_at.clone(),
                group_id: None,
                expanded: None,
            });
        }
    }

    let result = attach_trailing_tool_groups_to_assistant(attach_created_thread_summaries(
        next_rows,
        &timeline_entries,
    ));
    let continues: Vec<bool> = (0..result.len())
        .map(|index| {
            timeline_row_is_work_log(result.get(index))
                && timeline_row_is_work_log(result.get(index + 1))
        })
        .collect();
    result
        .into_iter()
        .zip(continues)
        .map(|(content, continues_work_log)| {
            Arc::new(MessagesTimelineRow {
                content,
                continues_work_log,
            })
        })
        .collect()
}

/// Adjacent work stays one visual list even when virtualization splits its groups.
fn timeline_row_is_work_log(row: Option<&MessagesTimelineRowContent>) -> bool {
    matches!(
        row,
        Some(
            MessagesTimelineRowContent::Work { .. }
                | MessagesTimelineRowContent::WorkToggle { .. }
                | MessagesTimelineRowContent::WorkLive { .. }
                | MessagesTimelineRowContent::Thinking { .. }
        )
    )
}

// Keep created chats below the final answer even when the work that created them folds away.
fn attach_created_thread_summaries(
    rows: Vec<MessagesTimelineRowContent>,
    timeline_entries: &[TimelineEntry],
) -> Vec<MessagesTimelineRowContent> {
    let mut terminal_indexes: HashMap<String, usize> = HashMap::new();
    let mut collapsed_runs: HashSet<String> = HashSet::new();
    let mut created_by_run: HashMap<String, Vec<MessagesTimelineRowContent>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        match row {
            MessagesTimelineRowContent::Message {
                message,
                show_assistant_meta: true,
                ..
            } => {
                if let Some(run_id) = non_empty(message.run_id.as_deref()) {
                    terminal_indexes.insert(run_id.to_string(), index);
                }
            }
            MessagesTimelineRowContent::TurnFold {
                run_id,
                expanded: false,
                ..
            } => {
                collapsed_runs.insert(run_id.clone());
            }
            _ => {}
        }
    }
    for entry in timeline_entries {
        let projected_item = match &entry.kind {
            TimelineEntryKind::Work { entry } => entry.projected_item.as_ref(),
            TimelineEntryKind::Event { projected_item } => Some(projected_item),
            _ => None,
        };
        if let Some(projected_item) = projected_item
            && projected_item.item.item_type() == TurnItemType::ThreadCreated
            && let Some(run_id) = non_empty(projected_item.item.run_id.as_deref())
        {
            created_by_run.entry(run_id.to_string()).or_default().push(
                MessagesTimelineRowContent::Event {
                    id: entry.id.clone(),
                    created_at: entry.created_at.clone(),
                    projected_item: projected_item.clone(),
                    subagents: None,
                    resource_summary: false,
                },
            );
        }
    }
    let mut result = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        if let MessagesTimelineRowContent::Event { projected_item, .. } = &row
            && projected_item.item.item_type() == TurnItemType::ThreadCreated
        {
            let run_id = projected_item.item.run_id.as_deref();
            if let Some(run_id) = run_id
                && let Some(terminal_index) = terminal_indexes.get(run_id)
                && (collapsed_runs.contains(run_id) || index > *terminal_index)
            {
                continue;
            }
        }
        let summary_run_id = match &row {
            MessagesTimelineRowContent::Message {
                message,
                show_assistant_meta: true,
                ..
            } => non_empty(message.run_id.as_deref()).map(str::to_string),
            _ => None,
        };
        result.push(row);
        if let Some(run_id) = summary_run_id {
            for entry in created_by_run.get(&run_id).into_iter().flatten() {
                if let MessagesTimelineRowContent::Event {
                    id,
                    created_at,
                    projected_item,
                    subagents,
                    ..
                } = entry
                {
                    result.push(MessagesTimelineRowContent::Event {
                        id: format!("summary:{id}"),
                        created_at: created_at.clone(),
                        projected_item: projected_item.clone(),
                        subagents: subagents.clone(),
                        resource_summary: true,
                    });
                }
            }
        }
    }
    result
}

/// `MessagesTimelineRowsProjection`.
#[derive(Clone, Debug, Default)]
pub struct MessagesTimelineRowsProjection {
    pub input: MessagesTimelineRowsInput,
    pub rows: Vec<Arc<MessagesTimelineRow>>,
}

/// The turn fold that holds an entry, keyed as `expanded_run_ids` expects. Runless
/// (imported V1) turns fold under a synthetic key, so the entry's own run id is
/// not enough to open them.
pub fn timeline_entry_turn_fold_run_id(
    input: &MessagesTimelineRowsInput,
    entry_id: &str,
) -> Option<String> {
    timeline_turn_fold_run_ids_by_entry_id(input).remove(entry_id)
}

/// Every folded entry's fold key, computed once for callers that check many entries.
pub fn timeline_turn_fold_run_ids_by_entry_id(
    input: &MessagesTimelineRowsInput,
) -> HashMap<String, String> {
    let mut by_entry_id = HashMap::new();
    for fold in derive_timeline_turn_folds(input).into_values() {
        for entry_id in fold.hidden_entry_ids {
            by_entry_id.insert(entry_id, fold.run_id.clone());
        }
    }
    by_entry_id
}

/// Upstream compares everything except entries, the latest run, checkpoints and
/// expansion state with `shallow`; here every remaining field compares by value.
fn same_context(previous: &MessagesTimelineRowsInput, next: &MessagesTimelineRowsInput) -> bool {
    previous.running_run_id == next.running_run_id
        && previous.is_working == next.is_working
        && previous.runless_work_active == next.runless_work_active
        && previous.active_turn_started_at == next.active_turn_started_at
        && previous.supports_conversation_rollback == next.supports_conversation_rollback
        && previous.live_agent_task_ids == next.live_agent_task_ids
}

fn replace_streaming_message_rows(
    input: &MessagesTimelineRowsInput,
    previous: &MessagesTimelineRowsProjection,
) -> Option<Vec<Arc<MessagesTimelineRow>>> {
    let previous_entries = &previous.input.timeline_entries;
    // V2 shell and checkpoint selectors produce fresh summaries for each event.
    // Equivalent summaries must not invalidate a text-only projection.
    if input.timeline_entries.len() != previous_entries.len()
        || !same_context(&previous.input, input)
        || previous.input.latest_run != input.latest_run
        || previous.input.expanded_run_ids != input.expanded_run_ids
        || previous.input.expanded_attempt_ids != input.expanded_attempt_ids
        || previous.input.expanded_work_group_ids != input.expanded_work_group_ids
        || previous.input.turn_diff_summaries != input.turn_diff_summaries
    {
        return None;
    }
    let mut replacements: HashMap<&str, (&ChatMessage, &TimelineEntry)> = HashMap::new();
    for (entry, previous_entry) in input.timeline_entries.iter().zip(previous_entries) {
        if entry == previous_entry {
            continue;
        }
        let (
            TimelineEntryKind::Message {
                message,
                projected_item,
            },
            TimelineEntryKind::Message {
                message: previous_message,
                projected_item: previous_projected_item,
            },
        ) = (&entry.kind, &previous_entry.kind)
        else {
            return None;
        };
        if entry.id != previous_entry.id
            || entry.created_at != previous_entry.created_at
            || entry.attempt != previous_entry.attempt
        {
            return None;
        }
        if projected_item != previous_projected_item {
            match (previous_projected_item, projected_item) {
                (Some(previous_projected_item), Some(projected_item))
                    if is_streaming_turn_item_text_update(
                        previous_projected_item,
                        projected_item,
                    ) => {}
                _ => return None,
            }
        }
        if message == previous_message && projected_item == previous_projected_item {
            continue;
        }
        if !is_streaming_message_text_update(previous_message, message) {
            return None;
        }
        replacements.insert(&previous_message.id, (previous_message, entry));
    }
    if replacements.is_empty() {
        return Some(previous.rows.clone());
    }
    let replacement_for = |message: &ChatMessage| {
        replacements
            .get(message.id.as_str())
            .filter(|(previous_message, _)| *previous_message == message)
            .map(|(_, entry)| *entry)
    };
    Some(
        previous
            .rows
            .iter()
            .map(|row| {
                let message = match &row.content {
                    MessagesTimelineRowContent::Message { message, .. }
                    | MessagesTimelineRowContent::AssistantMeta { message, .. } => message,
                    _ => return row.clone(),
                };
                let Some(TimelineEntryKind::Message {
                    message: next_message,
                    projected_item: next_projected_item,
                }) = replacement_for(message).map(|entry| &entry.kind)
                else {
                    return row.clone();
                };
                let mut next = MessagesTimelineRow::clone(row);
                match &mut next.content {
                    MessagesTimelineRowContent::Message {
                        message,
                        projected_item,
                        ..
                    }
                    | MessagesTimelineRowContent::AssistantMeta {
                        message,
                        projected_item,
                        ..
                    } => {
                        *message = next_message.clone();
                        if next_projected_item.is_some() {
                            *projected_item = next_projected_item.clone();
                        }
                    }
                    _ => {}
                }
                Arc::new(next)
            })
            .collect(),
    )
}

/// Keep one projection per timeline. Reuse rows only when streaming content changes.
pub fn derive_messages_timeline_rows_with_state(
    input: MessagesTimelineRowsInput,
    previous: Option<&MessagesTimelineRowsProjection>,
) -> MessagesTimelineRowsProjection {
    let rows = previous
        .and_then(|previous| replace_streaming_message_rows(&input, previous))
        .unwrap_or_else(|| derive_messages_timeline_rows(&input));
    MessagesTimelineRowsProjection { input, rows }
}

pub fn compute_stable_messages_timeline_rows(
    rows: Vec<Arc<MessagesTimelineRow>>,
    previous: &StableMessagesTimelineRowsState,
) -> StableMessagesTimelineRowsState {
    let mut next: HashMap<String, Arc<MessagesTimelineRow>> = HashMap::with_capacity(rows.len());
    let mut any_changed = rows.len() != previous.by_id.len();

    let result: Vec<Arc<MessagesTimelineRow>> = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let next_row = match previous.by_id.get(row.id()) {
                Some(previous_row) if is_row_unchanged(previous_row, &row) => previous_row.clone(),
                _ => row,
            };
            next.insert(next_row.id().to_string(), next_row.clone());
            if !any_changed
                && !previous
                    .result
                    .get(index)
                    .is_some_and(|previous_row| Arc::ptr_eq(previous_row, &next_row))
            {
                any_changed = true;
            }
            next_row
        })
        .collect();

    if any_changed {
        StableMessagesTimelineRowsState {
            by_id: Arc::new(next),
            result: Arc::new(result),
        }
    } else {
        previous.clone()
    }
}

/// Shallow field comparison per row variant — avoids deep equality cost.
///
/// Upstream's identity checks (`a.message === b.message`) are value
/// comparisons here. Like upstream, `continues_work_log` is not compared.
fn is_row_unchanged(a: &MessagesTimelineRow, b: &MessagesTimelineRow) -> bool {
    use MessagesTimelineRowContent as Row;
    if a.kind() != b.kind() || a.id() != b.id() {
        return false;
    }

    match (&a.content, &b.content) {
        (Row::Working { created_at: a, .. }, Row::Working { created_at: b, .. }) => a == b,
        (
            Row::Thinking {
                created_at: a_created_at,
                group_id: a_group_id,
                expanded: a_expanded,
                ..
            },
            Row::Thinking {
                created_at: b_created_at,
                group_id: b_group_id,
                expanded: b_expanded,
                ..
            },
        ) => a_created_at == b_created_at && a_group_id == b_group_id && a_expanded == b_expanded,
        (
            Row::AssistantMeta {
                created_at: a_created_at,
                projected_item: a_projected_item,
                message: a_message,
                show_assistant_copy_button: a_copy,
                assistant_copy_streaming: a_streaming,
                ..
            },
            Row::AssistantMeta {
                created_at: b_created_at,
                projected_item: b_projected_item,
                message: b_message,
                show_assistant_copy_button: b_copy,
                assistant_copy_streaming: b_streaming,
                ..
            },
        ) => {
            a_created_at == b_created_at
                && a_projected_item == b_projected_item
                && a_message == b_message
                && a_copy == b_copy
                && a_streaming == b_streaming
        }
        (
            Row::TurnFold {
                created_at: a_created_at,
                label: a_label,
                expanded: a_expanded,
                ..
            },
            Row::TurnFold {
                created_at: b_created_at,
                label: b_label,
                expanded: b_expanded,
                ..
            },
        )
        | (
            Row::AttemptFold {
                created_at: a_created_at,
                label: a_label,
                expanded: a_expanded,
                ..
            },
            Row::AttemptFold {
                created_at: b_created_at,
                label: b_label,
                expanded: b_expanded,
                ..
            },
        )
        | (
            Row::ContextCompaction {
                created_at: a_created_at,
                label: a_label,
                active: a_expanded,
                ..
            },
            Row::ContextCompaction {
                created_at: b_created_at,
                label: b_label,
                active: b_expanded,
                ..
            },
        ) => a_created_at == b_created_at && a_label == b_label && a_expanded == b_expanded,
        (
            Row::ProposedPlan {
                proposed_plan: a, ..
            },
            Row::ProposedPlan {
                proposed_plan: b, ..
            },
        ) => a == b,
        (
            Row::Event {
                projected_item: a_projected_item,
                resource_summary: a_resource_summary,
                subagents: a_subagents,
                ..
            },
            Row::Event {
                projected_item: b_projected_item,
                resource_summary: b_resource_summary,
                subagents: b_subagents,
                ..
            },
        ) => {
            a_projected_item == b_projected_item
                && a_resource_summary == b_resource_summary
                && a_subagents == b_subagents
        }
        (
            Row::Work {
                is_expanded_tool_group: a_expanded,
                display_label: a_label,
                grouped_entries: a_entries,
                ..
            },
            Row::Work {
                is_expanded_tool_group: b_expanded,
                display_label: b_label,
                grouped_entries: b_entries,
                ..
            },
        ) => a_expanded == b_expanded && a_label == b_label && a_entries == b_entries,
        (
            Row::WorkLive {
                created_at: a_created_at,
                group_id: a_group_id,
                expanded: a_expanded,
                active: a_active,
                entry: a_entry,
                grouped_entries: a_entries,
                ..
            },
            Row::WorkLive {
                created_at: b_created_at,
                group_id: b_group_id,
                expanded: b_expanded,
                active: b_active,
                entry: b_entry,
                grouped_entries: b_entries,
                ..
            },
        ) => {
            a_created_at == b_created_at
                && a_group_id == b_group_id
                && a_expanded == b_expanded
                && a_active == b_active
                && a_entry == b_entry
                && a_entries == b_entries
        }
        (
            Row::WorkToggle {
                created_at: a_created_at,
                run_id: a_run_id,
                group_id: a_group_id,
                hidden_count: a_hidden_count,
                expanded: a_expanded,
                summary: a_summary,
                summary_kind: a_summary_kind,
                tool_surface: a_tool_surface,
                tool_icon: a_tool_icon,
                has_failure: a_has_failure,
                ..
            },
            Row::WorkToggle {
                created_at: b_created_at,
                run_id: b_run_id,
                group_id: b_group_id,
                hidden_count: b_hidden_count,
                expanded: b_expanded,
                summary: b_summary,
                summary_kind: b_summary_kind,
                tool_surface: b_tool_surface,
                tool_icon: b_tool_icon,
                has_failure: b_has_failure,
                ..
            },
        ) => {
            a_created_at == b_created_at
                && a_run_id == b_run_id
                && a_group_id == b_group_id
                && a_hidden_count == b_hidden_count
                && a_expanded == b_expanded
                && a_summary == b_summary
                && a_summary_kind == b_summary_kind
                && a_tool_surface == b_tool_surface
                && a_tool_icon == b_tool_icon
                && a_has_failure == b_has_failure
        }
        (
            Row::Message {
                message: a_message,
                projected_item: a_projected_item,
                duration_start: a_duration_start,
                show_assistant_meta: a_meta,
                show_assistant_copy_button: a_copy,
                assistant_copy_streaming: a_streaming,
                assistant_turn_diff_summary: a_summary,
                revert_turn_count: a_revert,
                ..
            },
            Row::Message {
                message: b_message,
                projected_item: b_projected_item,
                duration_start: b_duration_start,
                show_assistant_meta: b_meta,
                show_assistant_copy_button: b_copy,
                assistant_copy_streaming: b_streaming,
                assistant_turn_diff_summary: b_summary,
                revert_turn_count: b_revert,
                ..
            },
        ) => {
            a_message == b_message
                && a_projected_item == b_projected_item
                && a_duration_start == b_duration_start
                && a_meta == b_meta
                && a_copy == b_copy
                && a_streaming == b_streaming
                && a_summary == b_summary
                && a_revert == b_revert
        }
        _ => false,
    }
}

const MAX_COLLAPSED_USER_MESSAGE_LINES: usize = 8;
const MAX_COLLAPSED_USER_MESSAGE_LENGTH: usize = 600;
static MARKDOWN_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"!?\[([^\]\n]*)\]\((?:<[^>\n]*>|[^\s)]*)\)").expect("static regex")
});

fn visible_user_message_text(text: &str) -> String {
    let link_labels = |segment: &str| MARKDOWN_LINK.replace_all(segment, "$1").into_owned();
    let mut visible = String::new();
    let mut cursor = 0;
    for found in collect_assistant_citations(text) {
        visible.push_str(&link_labels(&text[cursor..found.start]));
        visible.push_str(&assistant_citation_label(&found.citation));
        cursor = found.end;
    }
    visible.push_str(&link_labels(&text[cursor..]));
    visible
}

pub fn should_collapse_user_message(text: &str) -> bool {
    let visible = visible_user_message_text(text);
    if js_trim(&visible).is_empty() {
        return false;
    }

    // `.length` counts UTF-16 code units.
    utf16_len(&visible) > MAX_COLLAPSED_USER_MESSAGE_LENGTH
        || visible.split('\n').count() > MAX_COLLAPSED_USER_MESSAGE_LINES
}

#[cfg(test)]
#[path = "messages_timeline_logic_tests/mod.rs"]
mod tests;
