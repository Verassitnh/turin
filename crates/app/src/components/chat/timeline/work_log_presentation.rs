//! Port of upstream/packages/client-runtime/src/work-log/presentation.ts
//!
//! `WorkLogPresentationEntry` is the subset of `WorkLogEntry` both clients
//! render; Turin has one client, so these helpers take [`WorkLogEntry`]
//! directly. `resolveViewedImageAsset` is not ported: it builds media asset
//! URLs for the image viewer, which lands with the media work.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::change_request_url::parse_change_request_url;
use super::js_text::{js_trim, lowercase_first, utf16_prefix};
use super::t3_mcp_tool_presentation::{
    T3McpToolDefinition, T3McpToolIcon, T3McpToolSummaryAction, resolve_t3_mcp_tool_definition,
};
use super::t3_tool_summary::{
    T3ToolSummaryCall, T3ToolSummaryOutcome, summarize_t3_tool_calls,
    t3_tool_result_indicates_failure,
};
use super::tool_activity::{
    ClassifyToolActivityInput, ToolActivityAction, classify_tool_activity,
    is_tool_lifecycle_item_type,
};
use super::tool_output::tool_output_indicates_failure;
use super::turn_item::{
    ToolActivitySource, TurnItem, TurnItemPayload, TurnItemStatus, TurnItemType,
};
use super::types::{ToolLifecycleStatus, WorkLogEntry, WorkTone};
use turin_shared::file_preview::is_workspace_image_preview_path;
use turin_shared::usage_format::format_tokens;

/// Inspection and copying omit raw outputs, including values from older caches.
pub fn tool_item_for_display(item: &TurnItem) -> TurnItem {
    let mut display_item = item.clone();
    match &mut display_item.payload {
        TurnItemPayload::CommandExecution { output, .. } => *output = None,
        TurnItemPayload::DynamicTool { output, .. } => *output = None,
        TurnItemPayload::FileChange { diff, .. } => {
            // A failed edit's diffStr holds the provider's error, not a diff.
            // (The mirror has no oldStr/newStr to drop.)
            let keep = item.status == TurnItemStatus::Failed
                && diff
                    .as_deref()
                    .is_some_and(|diff| !js_trim(diff).is_empty());
            if !keep {
                *diff = None;
            }
        }
        _ => {}
    }
    display_item
}

pub fn context_compaction_label(
    status: TurnItemStatus,
    before_token_count: Option<u64>,
    after_token_count: Option<u64>,
) -> String {
    if status == TurnItemStatus::Running {
        return "Compacting context".to_string();
    }
    if let (Some(before), Some(after)) = (before_token_count, after_token_count) {
        return format!(
            "Context compacted {} → {} tokens",
            format_tokens(before as f64),
            format_tokens(after as f64)
        );
    }
    "Context compacted".to_string()
}

/// `ToolGroupAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolGroupAction {
    LinkPr,
    UnlinkPr,
    ListPrs,
    WatchPr,
    UnwatchPr,
    Read,
    Edit,
    Command,
    ThreadCreate,
    Browser,
    Device,
    CodeSearch,
    Search,
    Other,
    Update,
}

impl ToolGroupAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinkPr => "link-pr",
            Self::UnlinkPr => "unlink-pr",
            Self::ListPrs => "list-prs",
            Self::WatchPr => "watch-pr",
            Self::UnwatchPr => "unwatch-pr",
            Self::Read => "read",
            Self::Edit => "edit",
            Self::Command => "command",
            Self::ThreadCreate => "thread-create",
            Self::Browser => "browser",
            Self::Device => "device",
            Self::CodeSearch => "code-search",
            Self::Search => "search",
            Self::Other => "other",
            Self::Update => "update",
        }
    }
}

/// `ToolGroupSummaryKind`: `"pull-request" | ToolGroupAction | "dynamic-tool"
/// | "reasoning" | "agent-tool" | "tone-tool" | "mixed"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolGroupSummaryKind {
    PullRequest,
    Action(ToolGroupAction),
    DynamicTool,
    Reasoning,
    AgentTool,
    ToneTool,
    Mixed,
}

impl ToolGroupSummaryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PullRequest => "pull-request",
            Self::Action(action) => action.as_str(),
            Self::DynamicTool => "dynamic-tool",
            Self::Reasoning => "reasoning",
            Self::AgentTool => "agent-tool",
            Self::ToneTool => "tone-tool",
            Self::Mixed => "mixed",
        }
    }
}

static TRAILING_COMPLETION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(?:complete|completed)\s*$").expect("static regex"));

pub fn normalize_compact_tool_label(value: &str) -> String {
    js_trim(&TRAILING_COMPLETION.replace(value, "")).to_string()
}

fn as_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// JS `a ?? b`: `null` and missing both fall through.
fn nullish(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

/// Structured identity is authoritative, including when it identifies a foreign server.
fn work_entry_tool_name(entry: &WorkLogEntry) -> String {
    if let Some(TurnItemPayload::DynamicTool {
        tool_name: Some(tool_name),
        ..
    }) = entry.structured_payload.as_ref().map(|item| &item.payload)
        && !tool_name.is_empty()
    {
        return tool_name.clone();
    }
    let data = as_record(entry.tool_data.as_ref());
    if let Some(data) = data {
        if let (Some(server), Some(tool)) = (
            data.get("server").and_then(Value::as_str),
            data.get("tool").and_then(Value::as_str),
        ) {
            return format!("{server}.{tool}");
        }
        if let Some(tool_name) = data.get("toolName").and_then(Value::as_str) {
            return tool_name.to_string();
        }
    }
    match entry.tool_title.as_deref() {
        Some(tool_title) if resolve_t3_mcp_tool_definition(Some(tool_title)).is_some() => {
            tool_title.to_string()
        }
        _ => entry.label.clone(),
    }
}

fn work_entry_tool_output(entry: &WorkLogEntry) -> Option<&Value> {
    if let Some(TurnItemPayload::DynamicTool { output, .. }) =
        entry.structured_payload.as_ref().map(|item| &item.payload)
    {
        return output.as_ref();
    }
    let data = as_record(entry.tool_data.as_ref())?;
    nullish(data.get("output"))
        .or_else(|| nullish(data.get("result")))
        .or_else(|| nullish(data.get("rawOutput")))
        .or_else(|| data.get("content"))
}

/// What [`resolve_work_entry_tool_presentation`] returns for a T3 MCP tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkEntryToolPresentation {
    pub display_name: String,
    pub icon: T3McpToolIcon,
    pub action: Option<ToolGroupAction>,
}

/// `Number.isSafeInteger(number) && number > 0`.
fn positive_safe_integer(value: &Value) -> Option<u64> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let number = value.as_f64()?;
    (number.fract() == 0.0 && number > 0.0 && number <= MAX_SAFE_INTEGER).then_some(number as u64)
}

fn resolve_t3_mcp_tool_presentation(
    definition: Option<&T3McpToolDefinition>,
    status: Option<ToolLifecycleStatus>,
    data: Option<&Value>,
) -> Option<WorkEntryToolPresentation> {
    let definition = definition?;
    let [action, running, completed, detail] = definition.labels;
    let verb = match status {
        Some(ToolLifecycleStatus::InProgress) => running.to_string(),
        Some(ToolLifecycleStatus::Completed) => completed.to_string(),
        Some(ToolLifecycleStatus::Failed) => format!("Failed to {}", action.to_lowercase()),
        Some(ToolLifecycleStatus::Declined) => format!("Declined to {}", action.to_lowercase()),
        Some(ToolLifecycleStatus::Stopped) => format!("Stopped {}", running.to_lowercase()),
        _ => running.to_string(),
    };

    let action_kind = match definition.summary_action {
        T3McpToolSummaryAction::LinkPr => Some(ToolGroupAction::LinkPr),
        T3McpToolSummaryAction::UnlinkPr => Some(ToolGroupAction::UnlinkPr),
        T3McpToolSummaryAction::ListPrs => Some(ToolGroupAction::ListPrs),
        T3McpToolSummaryAction::WatchPr => Some(ToolGroupAction::WatchPr),
        T3McpToolSummaryAction::UnwatchPr => Some(ToolGroupAction::UnwatchPr),
        _ => None,
    };
    let payload = as_record(data);
    let input = payload.and_then(|payload| {
        as_record(payload.get("arguments"))
            .or_else(|| as_record(payload.get("input")))
            .or_else(|| as_record(payload.get("rawInput")))
    });
    let url_target = input
        .and_then(|input| input.get("url"))
        .and_then(Value::as_str)
        .and_then(parse_change_request_url);
    let number = match url_target {
        Some(target) => Some(target.number),
        None => input
            .and_then(|input| input.get("number"))
            .and_then(positive_safe_integer),
    }
    .filter(|number| *number > 0);
    let target = match (action_kind, number) {
        (Some(kind), Some(number)) if kind != ToolGroupAction::ListPrs => format!("PR #{number}"),
        _ => detail.to_string(),
    };
    Some(WorkEntryToolPresentation {
        display_name: format!("{verb} {target}"),
        icon: definition.icon,
        action: action_kind,
    })
}

/// Only active or completed calls can inherit the live group’s present tense.
pub fn live_activity_tool_status(
    status: Option<ToolLifecycleStatus>,
    present_tense: bool,
) -> ToolLifecycleStatus {
    match status {
        Some(
            status @ (ToolLifecycleStatus::Failed
            | ToolLifecycleStatus::Declined
            | ToolLifecycleStatus::Stopped
            | ToolLifecycleStatus::Idle),
        ) => status,
        _ if present_tense || status == Some(ToolLifecycleStatus::InProgress) => {
            ToolLifecycleStatus::InProgress
        }
        _ => ToolLifecycleStatus::Completed,
    }
}

/// Resolves tool identity before choosing labels or icons in either client.
/// The entry's own lifecycle status wins over `fallback_status`.
pub fn resolve_work_entry_tool_presentation(
    entry: &WorkLogEntry,
    fallback_status: Option<ToolLifecycleStatus>,
) -> Option<WorkEntryToolPresentation> {
    resolve_work_entry_tool_presentation_with_status(
        entry,
        entry.tool_lifecycle_status.or(fallback_status),
    )
}

/// [`resolve_work_entry_tool_presentation`] for `{ ...entry, toolLifecycleStatus: status }`
/// without cloning the entry.
pub fn resolve_work_entry_tool_presentation_with_status(
    entry: &WorkLogEntry,
    status: Option<ToolLifecycleStatus>,
) -> Option<WorkEntryToolPresentation> {
    let definition = resolve_t3_mcp_tool_definition(Some(&work_entry_tool_name(entry)));
    let status = if definition.is_some()
        && t3_tool_result_indicates_failure(work_entry_tool_output(entry))
    {
        Some(ToolLifecycleStatus::Failed)
    } else {
        status
    };
    resolve_t3_mcp_tool_presentation(definition, status, entry.tool_data.as_ref())
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !js_trim(value).is_empty())
}

fn command_result_content(value: Option<&Value>) -> Option<String> {
    if let Some(direct) = non_empty_string(value) {
        return Some(direct.to_string());
    }

    let direct_content = value.and_then(Value::as_array);
    let content = as_record(value).and_then(|record| record.get("content"));
    if let Some(content_text) = non_empty_string(content) {
        return Some(content_text.to_string());
    }
    let blocks = direct_content.or_else(|| content.and_then(Value::as_array))?;

    let chunks: Vec<&str> = blocks
        .iter()
        .filter_map(|entry| {
            non_empty_string(Some(entry))
                .or_else(|| non_empty_string(as_record(Some(entry)).and_then(|r| r.get("text"))))
        })
        .collect();
    (!chunks.is_empty()).then(|| chunks.join("\n"))
}

/// Returns provider command output before it is formatted for a work-log row.
pub fn extract_command_output_text(data_value: Option<&Value>) -> Option<String> {
    let data = as_record(data_value);
    let get = |key: &str| data.and_then(|data| data.get(key));
    let item = as_record(get("item"));
    let item_result = as_record(item.and_then(|item| item.get("result")));
    let raw_output = as_record(get("rawOutput"));
    let output_streams: Vec<&str> = [
        non_empty_string(raw_output.and_then(|raw| raw.get("stdout"))),
        non_empty_string(raw_output.and_then(|raw| raw.get("stderr"))),
    ]
    .into_iter()
    .flatten()
    .collect();
    let acp_content = get("content").and_then(Value::as_array).map(|content| {
        content
            .iter()
            .filter_map(|entry_value| {
                let entry = as_record(Some(entry_value))?;
                let content = as_record(entry.get("content"));
                if entry.get("type").and_then(Value::as_str) == Some("content") {
                    non_empty_string(content.and_then(|content| content.get("text")))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    });

    let joined_streams =
        (!output_streams.is_empty()).then(|| Value::String(output_streams.join("\n")));
    let acp_content = acp_content.map(Value::String);
    let candidates: [Option<&Value>; 8] = [
        item.and_then(|item| item.get("aggregatedOutput")),
        item_result.and_then(|result| result.get("content")),
        get("rawOutput"),
        raw_output.and_then(|raw| raw.get("content")),
        joined_streams.as_ref(),
        raw_output.and_then(|raw| raw.get("output")),
        acp_content.as_ref(),
        get("result"),
    ];
    candidates.into_iter().find_map(command_result_content)
}

/// Ingestion caps tool details at 180 chars and appends "...", so a long command
/// echo no longer equals the command it repeats. Treat a truncated prefix of the
/// command as the same echo.
fn text_repeats_command(text: &str, commands: &[Option<&str>]) -> bool {
    let truncated = text
        .strip_suffix("...")
        .or_else(|| text.strip_suffix('\u{2026}'));
    commands.iter().any(|candidate| {
        let Some(command) = candidate.map(js_trim).filter(|command| !command.is_empty()) else {
            return false;
        };
        if command == text {
            return true;
        }
        truncated.is_some_and(|truncated| {
            !truncated.is_empty()
                && command.len() > truncated.len()
                && command.starts_with(truncated)
        })
    })
}

/// Input of [`command_detail_repeats_command`].
#[derive(Clone, Copy, Debug)]
pub struct CommandDetailRepeatsCommandInput<'a> {
    pub detail: &'a str,
    pub command: Option<&'a str>,
    pub raw_command: Option<&'a str>,
    pub tool_name: Option<&'a Value>,
    pub data: Option<&'a Value>,
}

/// Decides whether a command row's `detail` is a synthetic echo of the command
/// rather than real output. OpenCode stores completed output in `detail` with no
/// other output channel, so plain equality is only treated as synthetic when the
/// payload shape shows the detail came from the command: Codex item metadata,
/// an ACP tool call (`data.toolCallId`, `kind: "execute"`), a Claude tool-name
/// prefix, or no structured command at all.
pub fn command_detail_repeats_command(input: CommandDetailRepeatsCommandInput<'_>) -> bool {
    let tool_name = non_empty_string(input.tool_name).map(js_trim);
    let detail = js_trim(input.detail);
    let commands = [input.command, input.raw_command];
    if let Some(tool_name) = tool_name {
        let prefix = format!("{tool_name}:");
        if detail.to_lowercase().starts_with(&prefix.to_lowercase())
            && let Some(unprefixed) = detail.get(prefix.len()..)
            && text_repeats_command(js_trim(unprefixed), &commands)
        {
            return true;
        }
    }

    if !text_repeats_command(detail, &commands) {
        return false;
    }

    let data = as_record(input.data);
    let item = as_record(data.and_then(|data| data.get("item")));
    let item_input = as_record(item.and_then(|item| item.get("input")));
    let item_result = as_record(item.and_then(|item| item.get("result")));
    let has_structured_command = [
        item.and_then(|item| item.get("command")),
        item_input.and_then(|input| input.get("command")),
        item_result.and_then(|result| result.get("command")),
        data.and_then(|data| data.get("command")),
    ]
    .into_iter()
    .any(|value| match value {
        Some(Value::Array(parts)) => parts
            .iter()
            .any(|part| non_empty_string(Some(part)).is_some()),
        value => non_empty_string(value).is_some(),
    });
    !has_structured_command
        || item.is_some()
        || data.is_some_and(|data| data.contains_key("toolCallId"))
        || non_empty_string(data.and_then(|data| data.get("kind")))
            .is_some_and(|kind| kind.to_lowercase() == "execute")
}

fn work_log_entry_is_tool_like(entry: &WorkLogEntry) -> bool {
    if matches!(
        entry.tone,
        WorkTone::Tool | WorkTone::Thinking | WorkTone::Error
    ) {
        return true;
    }
    if entry
        .command
        .as_deref()
        .is_some_and(|command| !js_trim(command).is_empty())
    {
        return true;
    }
    if entry.request_kind.is_some() {
        return true;
    }
    entry
        .item_type
        .is_some_and(|item_type| is_tool_lifecycle_item_type(item_type.as_str()))
}

fn work_entry_indicates_tool_failure_from_output(
    entry: &WorkLogEntry,
    include_command: bool,
) -> bool {
    if entry.tone == WorkTone::Error
        || matches!(
            entry.tool_lifecycle_status,
            Some(ToolLifecycleStatus::Failed | ToolLifecycleStatus::Declined)
        )
    {
        return true;
    }
    if !work_log_entry_is_tool_like(entry) {
        return false;
    }
    if resolve_t3_mcp_tool_definition(Some(&work_entry_tool_name(entry))).is_some()
        && t3_tool_result_indicates_failure(work_entry_tool_output(entry))
    {
        return true;
    }
    if let Some(TurnItemPayload::CommandExecution {
        output,
        output_indicates_failure,
        exit_code,
        ..
    }) = entry.structured_payload.as_ref().map(|item| &item.payload)
    {
        if *output_indicates_failure == Some(true) || exit_code.is_some_and(|code| code != 0) {
            return true;
        }
        // Older servers/caches can still carry output. Read only the previous
        // preview-sized prefix for status, without exposing it in the row detail.
        if let Some(output) = output.as_deref().filter(|output| !output.is_empty())
            && tool_output_indicates_failure(utf16_prefix(output, 32_768))
        {
            return true;
        }
    }
    let output = if include_command {
        [entry.detail.as_deref(), entry.command.as_deref()]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        entry.detail.clone().unwrap_or_default()
    };
    !output.is_empty() && tool_output_indicates_failure(&output)
}

/// Includes legacy activities that stored error output in the command field.
pub fn work_entry_indicates_tool_failure(entry: &WorkLogEntry) -> bool {
    work_entry_indicates_tool_failure_from_output(entry, true)
}

/// Checks rendered output without treating the user's command as an error.
pub fn work_entry_display_indicates_tool_failure(entry: &WorkLogEntry) -> bool {
    work_entry_indicates_tool_failure_from_output(entry, false)
}

/// Decides whether the row can show a success marker.
pub fn work_entry_indicates_tool_success(entry: &WorkLogEntry) -> bool {
    work_log_entry_is_tool_like(entry)
        && !work_entry_indicates_tool_failure(entry)
        && entry.tone != WorkTone::Thinking
        && !matches!(
            entry.tool_lifecycle_status,
            Some(
                ToolLifecycleStatus::Idle
                    | ToolLifecycleStatus::InProgress
                    | ToolLifecycleStatus::Stopped
            )
        )
}

static GREP_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?-u:\b)grep(?-u:\b)").expect("static regex"));

fn work_log_entry_is_local_code_search(entry: &WorkLogEntry) -> bool {
    entry.item_type == Some(TurnItemType::FileSearch)
        || (entry.item_type == Some(TurnItemType::WebSearch)
            && GREP_WORD.is_match(&normalize_compact_tool_label(
                entry.tool_title.as_deref().unwrap_or(&entry.label),
            )))
}

pub fn tool_group_action(entry: &WorkLogEntry) -> ToolGroupAction {
    if entry.item_type == Some(TurnItemType::ThreadCreated) {
        return ToolGroupAction::ThreadCreate;
    }
    let presentation = resolve_work_entry_tool_presentation(entry, None);
    if let Some(action) = presentation
        .as_ref()
        .and_then(|presentation| presentation.action)
    {
        return action;
    }
    match presentation.as_ref().map(|presentation| presentation.icon) {
        Some(T3McpToolIcon::Browser) => return ToolGroupAction::Browser,
        Some(T3McpToolIcon::Device) => return ToolGroupAction::Device,
        _ => {}
    }
    if entry.request_kind.as_deref() == Some("file-read") || entry.viewed_image_path.is_some() {
        return ToolGroupAction::Read;
    }
    // Approvals and questions describe requested work, not work that ran.
    if matches!(
        entry.item_type,
        Some(TurnItemType::ApprovalRequest | TurnItemType::UserInputRequest)
    ) {
        return if work_log_entry_is_tool_like(entry) {
            ToolGroupAction::Other
        } else {
            ToolGroupAction::Update
        };
    }
    let mut data = as_record(entry.tool_data.as_ref())
        .cloned()
        .unwrap_or_default();
    let tool_name = match entry.structured_payload.as_ref().map(|item| &item.payload) {
        Some(TurnItemPayload::DynamicTool { tool_name, .. }) => tool_name.clone(),
        _ => match data.get("toolName").and_then(Value::as_str) {
            Some(tool_name) => Some(tool_name.to_string()),
            None => entry.tool_title.clone(),
        },
    };
    if let Some(tool_name) = tool_name.filter(|name| !name.is_empty()) {
        data.insert("toolName".to_string(), Value::String(tool_name));
    }
    let classified = classify_tool_activity(ClassifyToolActivityInput {
        item_type: match entry.item_type {
            Some(TurnItemType::CommandExecution) => Some("command_execution"),
            Some(TurnItemType::FileChange) => Some("file_change"),
            Some(TurnItemType::WebSearch) => Some("web_search"),
            Some(TurnItemType::DynamicTool) => Some("dynamic_tool_call"),
            _ => None,
        },
        request_kind: None,
        title: Some(entry.tool_title.as_deref().unwrap_or(&entry.label)),
        data: Some(&data),
    });
    if classified == ToolActivityAction::Read {
        return ToolGroupAction::Read;
    }
    if classified == ToolActivityAction::FileChange
        || entry.item_type == Some(TurnItemType::FileChange)
    {
        return ToolGroupAction::Edit;
    }
    if classified == ToolActivityAction::Command
        || entry.item_type == Some(TurnItemType::CommandExecution)
        || entry
            .command
            .as_deref()
            .is_some_and(|command| !command.is_empty())
    {
        return ToolGroupAction::Command;
    }
    if classified == ToolActivityAction::Search {
        return if entry.item_type == Some(TurnItemType::WebSearch)
            && !work_log_entry_is_local_code_search(entry)
        {
            ToolGroupAction::Search
        } else {
            ToolGroupAction::CodeSearch
        };
    }
    if work_log_entry_is_local_code_search(entry) {
        return ToolGroupAction::CodeSearch;
    }
    if entry.item_type == Some(TurnItemType::WebSearch) {
        return ToolGroupAction::Search;
    }
    if entry
        .changed_files
        .as_ref()
        .is_some_and(|files| !files.is_empty())
    {
        return ToolGroupAction::Edit;
    }
    if work_log_entry_is_tool_like(entry) {
        ToolGroupAction::Other
    } else {
        ToolGroupAction::Update
    }
}

fn is_single_line(text: &str) -> bool {
    !text.contains(['\r', '\n'])
}

pub fn work_entry_viewed_image_path(entry: &WorkLogEntry) -> Option<String> {
    if let Some(viewed_image_path) = entry.viewed_image_path.as_deref().map(js_trim)
        && is_single_line(viewed_image_path)
        && is_workspace_image_preview_path(viewed_image_path)
    {
        return Some(viewed_image_path.to_string());
    }
    let detail = entry.detail.as_deref().map(js_trim)?;
    (tool_group_action(entry) == ToolGroupAction::Read
        && is_single_line(detail)
        && is_workspace_image_preview_path(detail))
    .then(|| detail.to_string())
}

fn tool_group_action_count<E: Borrow<WorkLogEntry>>(
    action: ToolGroupAction,
    entries: &[E],
) -> usize {
    if action != ToolGroupAction::Edit {
        return entries.len();
    }
    let mut changed_files: HashSet<&str> = HashSet::new();
    let mut edits_without_file_details = 0;
    for entry in entries {
        match entry.borrow().changed_files.as_ref() {
            Some(files) if !files.is_empty() => {
                changed_files.extend(files.iter().map(String::as_str));
            }
            _ => edits_without_file_details += 1,
        }
    }
    changed_files.len() + edits_without_file_details
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn tool_group_action_label(action: ToolGroupAction, count: usize) -> String {
    match action {
        ToolGroupAction::LinkPr => {
            format!("Linked {}", plural(count, "pull request", "pull requests"))
        }
        ToolGroupAction::UnlinkPr => {
            format!(
                "Unlinked {}",
                plural(count, "pull request", "pull requests")
            )
        }
        ToolGroupAction::WatchPr => {
            format!(
                "Watching {}",
                plural(count, "pull request", "pull requests")
            )
        }
        ToolGroupAction::UnwatchPr => format!(
            "Stopped watching {}",
            plural(count, "pull request", "pull requests")
        ),
        ToolGroupAction::ListPrs => {
            if count == 1 {
                "Checked linked pull requests".to_string()
            } else {
                format!("Checked linked pull requests {count} times")
            }
        }
        ToolGroupAction::Read => format!("Read {}", plural(count, "file", "files")),
        ToolGroupAction::Edit => format!("Changed {}", plural(count, "file", "files")),
        ToolGroupAction::Command => format!("Ran {}", plural(count, "command", "commands")),
        ToolGroupAction::ThreadCreate => format!("Created {}", plural(count, "thread", "threads")),
        ToolGroupAction::Device => {
            format!("Used device controls {}", plural(count, "time", "times"))
        }
        ToolGroupAction::Browser => format!("Used browser {}", plural(count, "time", "times")),
        ToolGroupAction::Search => format!("Searched the web {}", plural(count, "time", "times")),
        ToolGroupAction::CodeSearch => format!("Searched code {}", plural(count, "time", "times")),
        ToolGroupAction::Other => format!("Used {}", plural(count, "tool", "tools")),
        ToolGroupAction::Update => format!("Received {}", plural(count, "update", "updates")),
    }
}

fn t3_tool_summary_call(entry: &WorkLogEntry) -> T3ToolSummaryCall<'_> {
    let input = match entry.structured_payload.as_ref().map(|item| &item.payload) {
        Some(TurnItemPayload::DynamicTool { input, .. }) => Some(input),
        _ => {
            // `typeof toolData === "object"` also admits arrays, which have none of these keys.
            let data = as_record(entry.tool_data.as_ref());
            data.and_then(|data| {
                nullish(data.get("arguments"))
                    .or_else(|| nullish(data.get("input")))
                    .or_else(|| data.get("rawInput"))
            })
        }
    };
    T3ToolSummaryCall {
        input,
        output: work_entry_tool_output(entry),
        outcome: if matches!(
            entry.tool_lifecycle_status,
            Some(ToolLifecycleStatus::Failed | ToolLifecycleStatus::Declined)
        ) || entry.tone == WorkTone::Error
        {
            T3ToolSummaryOutcome::Failed
        } else if entry.tool_lifecycle_status == Some(ToolLifecycleStatus::Completed) {
            T3ToolSummaryOutcome::Completed
        } else {
            T3ToolSummaryOutcome::Unfinished
        },
    }
}

/// `summaryActionPriority`, keyed by the action literal: a T3 summary action
/// and a tool group action with the same name share one group upstream.
fn summary_action_priority(action: &str) -> u8 {
    match action {
        "command" | "edit" | "delegate" | "task-cancel" | "thread-create" | "thread-send"
        | "thread-interrupt" | "workflow-stop" | "schedule-create" | "schedule-update"
        | "schedule-delete" | "schedule-run" | "thread-configure" | "thread-fork"
        | "thread-merge" | "thread-organize" | "thread-update" | "queue-edit" | "queue-cancel"
        | "queue-reorder" | "queue-steer" | "question-respond" | "worktree-handoff"
        | "project-create" | "project-update" | "project-delete" | "project-clone"
        | "environment-update" | "attachment-prepare" | "attachment-discard"
        | "attachment-send" => 0,
        "other" | "update" => 2,
        _ => 1,
    }
}

/// Return value of [`summarize_tool_group`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolGroupSummary {
    pub summary: String,
    pub has_failure: bool,
}

/// Summarizes at most two action categories; every omitted call still counts in the remainder.
pub fn summarize_tool_group<E: Borrow<WorkLogEntry>>(entries: &[E]) -> ToolGroupSummary {
    let all_entries: Vec<&WorkLogEntry> = entries.iter().map(Borrow::borrow).collect();
    let entries: Vec<&WorkLogEntry> = all_entries
        .iter()
        .copied()
        .filter(|entry| entry.item_type != Some(TurnItemType::Reasoning))
        .collect();
    if !all_entries.is_empty() && entries.is_empty() {
        return ToolGroupSummary {
            summary: if all_entries.len() == 1 {
                "Thought".to_string()
            } else {
                format!("Thought (×{})", all_entries.len())
            },
            has_failure: false,
        };
    }
    struct Group<'e> {
        action: ToolGroupAction,
        t3_action: Option<T3McpToolSummaryAction>,
        entries: Vec<&'e WorkLogEntry>,
    }
    let mut groups: Vec<(&'static str, Group<'_>)> = Vec::new();
    let mut group_index: HashMap<&'static str, usize> = HashMap::new();
    let mut sources: Vec<&ToolActivitySource> = Vec::new();
    let mut source_index: HashMap<&str, usize> = HashMap::new();
    for entry in &entries {
        let t3_action = resolve_t3_mcp_tool_definition(Some(&work_entry_tool_name(entry)))
            .map(|definition| definition.summary_action);
        if let Some(tool_source) = entry.tool_source.as_ref()
            && t3_action.is_none()
        {
            match source_index.get(tool_source.key.as_str()) {
                Some(index) => sources[*index] = tool_source,
                None => {
                    source_index.insert(&tool_source.key, sources.len());
                    sources.push(tool_source);
                }
            }
            continue;
        }
        let action = tool_group_action(entry);
        let key = t3_action.map_or(action.as_str(), T3McpToolSummaryAction::as_str);
        match group_index.get(key) {
            Some(index) => groups[*index].1.entries.push(entry),
            None => {
                group_index.insert(key, groups.len());
                groups.push((
                    key,
                    Group {
                        action,
                        t3_action,
                        entries: vec![entry],
                    },
                ));
            }
        }
    }
    struct Summary {
        index: usize,
        count: usize,
        priority: u8,
        label: String,
        failed_count: usize,
    }
    let summaries: Vec<Summary> = groups
        .iter()
        .enumerate()
        .map(|(index, (key, group))| {
            let (label, failed_count) = match group.t3_action {
                Some(t3_action) => {
                    let calls: Vec<T3ToolSummaryCall<'_>> = group
                        .entries
                        .iter()
                        .map(|entry| t3_tool_summary_call(entry))
                        .collect();
                    let summary = summarize_t3_tool_calls(t3_action, &calls);
                    (summary.label, summary.failed_count)
                }
                None => (
                    tool_group_action_label(
                        group.action,
                        tool_group_action_count(group.action, &group.entries),
                    ),
                    group
                        .entries
                        .iter()
                        .filter(|entry| work_entry_display_indicates_tool_failure(entry))
                        .count(),
                ),
            };
            Summary {
                index,
                count: group.entries.len(),
                priority: summary_action_priority(key),
                label,
                failed_count,
            }
        })
        .collect();
    let mut selected: Vec<&Summary> = summaries.iter().collect();
    selected.sort_by_key(|summary| (summary.priority, summary.index));
    selected.truncate(2);
    selected.sort_by_key(|summary| summary.index);
    let mut labels: Vec<String> = selected
        .iter()
        .map(|summary| summary.label.clone())
        .collect();
    if !sources.is_empty() {
        let mut source_names: Vec<&str> = Vec::new();
        for source in &sources {
            if !source_names.contains(&source.name.as_str()) {
                source_names.push(&source.name);
            }
        }
        let formatted_names = match source_names.as_slice() {
            [] => String::new(),
            [only] => (*only).to_string(),
            [first, second] => format!("{first} and {second}"),
            [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
        };
        let all_integrations = sources.iter().all(|source| source.kind == "integration");
        let suffix = if all_integrations {
            if source_names.len() == 1 {
                " integration"
            } else {
                " integrations"
            }
        } else {
            ""
        };
        labels.insert(0, format!("Used {formatted_names}{suffix}"));
    }
    let sourced_count = entries
        .iter()
        .filter(|entry| {
            entry.tool_source.is_some()
                && resolve_t3_mcp_tool_definition(Some(&work_entry_tool_name(entry))).is_none()
        })
        .count();
    let selected_count: usize = selected.iter().map(|summary| summary.count).sum();
    let remaining_count = entries
        .len()
        .saturating_sub(sourced_count)
        .saturating_sub(selected_count);
    if remaining_count > 0 {
        labels.push(format!(
            "Performed {remaining_count} other {}",
            if remaining_count == 1 {
                "action"
            } else {
                "actions"
            }
        ));
    }
    let sentence_labels: Vec<String> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            if index == 0 {
                label.clone()
            } else {
                lowercase_first(label)
            }
        })
        .collect();
    let summary = match sentence_labels.as_slice() {
        [rest @ .., last] if sentence_labels.len() >= 3 => {
            format!("{}, and {last}", rest.join(", "))
        }
        _ => sentence_labels.join(" and "),
    };
    ToolGroupSummary {
        summary,
        has_failure: summaries.iter().any(|summary| summary.failed_count > 0),
    }
}

pub fn tool_group_summary_kind<E: Borrow<WorkLogEntry>>(entries: &[E]) -> ToolGroupSummaryKind {
    let all_entries: Vec<&WorkLogEntry> = entries.iter().map(Borrow::borrow).collect();
    let entries: Vec<&WorkLogEntry> = all_entries
        .iter()
        .copied()
        .filter(|entry| entry.item_type != Some(TurnItemType::Reasoning))
        .collect();
    if !all_entries.is_empty() && entries.is_empty() {
        return ToolGroupSummaryKind::Reasoning;
    }
    if !entries.is_empty()
        && entries.iter().all(|entry| {
            resolve_work_entry_tool_presentation(entry, None)
                .is_some_and(|presentation| presentation.icon == T3McpToolIcon::PullRequest)
        })
    {
        return ToolGroupSummaryKind::PullRequest;
    }
    let mut actions: Vec<ToolGroupAction> = Vec::new();
    for entry in &entries {
        let action = tool_group_action(entry);
        if !actions.contains(&action) {
            actions.push(action);
        }
    }
    let [action] = actions.as_slice() else {
        return ToolGroupSummaryKind::Mixed;
    };
    if *action != ToolGroupAction::Other {
        return ToolGroupSummaryKind::Action(*action);
    }
    let mut fallback_kinds: Vec<ToolGroupSummaryKind> = Vec::new();
    for entry in &entries {
        let kind = if entry.item_type == Some(TurnItemType::DynamicTool) {
            ToolGroupSummaryKind::DynamicTool
        } else if entry.item_type == Some(TurnItemType::Subagent)
            || entry.tone == WorkTone::Thinking
        {
            ToolGroupSummaryKind::AgentTool
        } else if entry.tone == WorkTone::Tool {
            ToolGroupSummaryKind::ToneTool
        } else {
            ToolGroupSummaryKind::Action(ToolGroupAction::Other)
        };
        if !fallback_kinds.contains(&kind) {
            fallback_kinds.push(kind);
        }
    }
    match fallback_kinds.as_slice() {
        [kind] => *kind,
        _ => ToolGroupSummaryKind::Mixed,
    }
}

static THOUGHT_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\s\x{feff}]*\*\*([^*\r\n]+)\*\*[ \t]*\r?(?:\n|$)").expect("static regex")
});
static THOUGHT_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").expect("static regex"));
static THOUGHT_LIST_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*(?:#{1,6}|[-*+]|[0-9]+\.)[ \t]+").expect("static regex")
});
static THOUGHT_INLINE_MARKUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`+|\*\*|~~").expect("static regex"));
static THOUGHT_EMPHASIS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[^A-Za-z0-9_*])[*_]([^*_\n]+)[*_]").expect("static regex"));

fn is_word_or_star(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '*'
}

/// `.replace(/(^|[^\w*])[*_]([^*_\n]+)[*_](?![\w*])/g, "$1$2")`: the regex
/// crate has no lookahead, so a match whose next char is `[\w*]` is retried
/// one char later, as the JS engine does.
fn strip_emphasis(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut search_from = 0;
    while search_from <= text.len() {
        let Some(captures) = THOUGHT_EMPHASIS.captures_at(text, search_from) else {
            break;
        };
        let (Some(whole), Some(prefix), Some(content)) =
            (captures.get(0), captures.get(1), captures.get(2))
        else {
            break;
        };
        let followed_by_word = text[whole.end()..]
            .chars()
            .next()
            .is_some_and(is_word_or_star);
        if followed_by_word {
            // Retry from the next char boundary after this match's start.
            search_from = text[whole.start()..]
                .chars()
                .next()
                .map_or(text.len() + 1, |ch| whole.start() + ch.len_utf8());
            continue;
        }
        out.push_str(&text[copied..whole.start()]);
        out.push_str(prefix.as_str());
        out.push_str(content.as_str());
        copied = whole.end();
        search_from = if whole.end() > whole.start() {
            whole.end()
        } else {
            whole.end() + 1
        };
    }
    out.push_str(&text[copied..]);
    out
}

/// Plain-text line for the latest thought in the live activity row. A
/// bold-only opening line (the Codex summary heading) wins; otherwise this is
/// the first sentence of the reasoning text. Web and mobile both render it.
pub fn live_thought_line(markdown: &str) -> String {
    let heading = THOUGHT_HEADING
        .captures(markdown)
        .and_then(|captures| captures.get(1))
        .map(|heading| heading.as_str());
    let source = heading.unwrap_or(markdown);
    let text = THOUGHT_LINK.replace_all(source, "${1}");
    let text = THOUGHT_LIST_MARKER.replace_all(&text, "");
    let text = THOUGHT_INLINE_MARKUP.replace_all(&text, "");
    let text = strip_emphasis(&text);
    let text = super::js_text::collapse_whitespace(&text);
    let text = js_trim(&text).to_string();
    if heading.is_some() {
        return text;
    }
    // Cut after the first . ? or ! (plus a closing quote or paren) that a space follows.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (position, (_, ch)) in chars.iter().enumerate() {
        if !matches!(ch, '.' | '?' | '!') {
            continue;
        }
        let next = chars.get(position + 1).map(|(_, ch)| *ch);
        let after_next = chars.get(position + 2).map(|(_, ch)| *ch);
        let is_closer = |ch: char| matches!(ch, '"' | '\'' | '”' | '’' | ')');
        let is_space = |ch: Option<char>| ch.is_some_and(super::js_text::is_js_whitespace);
        let end_position = if next.is_some_and(is_closer) && is_space(after_next) {
            Some(position + 2)
        } else if is_space(next) {
            Some(position + 1)
        } else {
            None
        };
        if let Some(end_position) = end_position {
            let end = chars
                .get(end_position)
                .map_or(text.len(), |(byte_index, _)| *byte_index);
            return text[..end].to_string();
        }
    }
    text
}
