//! Port of the work-log and lifecycle rows in
//! upstream/apps/web/src/components/chat/MessagesTimeline.tsx
//! (`WorkGroupSection`, `ExpandedWorkGroupEntries`, `WorkEntryLogRow`,
//! `LiveWorkEntryTimelineRow`, `WorkGroupToggleTimelineRow`,
//! `ThinkingTimelineRow`, `WorkingTimelineRow`, `TurnFoldTimelineRow`,
//! `AttemptFoldTimelineRow`, `ContextCompactionTimelineRow`,
//! `V2EventTimelineRow`, `V2SubagentGroup`, `ReasoningTraceContent`),
//! V2LifecycleRow.tsx (`SubagentTimelineLink`), TimelineSystemDivider.tsx and
//! a reduced V2ItemInspector.tsx.
//!
//! Port of upstream/apps/web/src/components/chat/V2LifecycleRow.tsx.
//! Port of upstream/apps/web/src/components/chat/V2ItemInspector.tsx (reduced).
//! Port of upstream/apps/web/src/components/chat/TimelineSystemDivider.tsx.

use std::rc::Rc;

use chrono::{DateTime, Utc};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
    relative, rems,
};

use super::agent_elapsed::AgentElapsed;
use super::markdown::ChatMarkdown;
use super::markdown::code_highlight::highlight_code;
use super::markdown::inline_flow::{FlowSpan, FlowSpanStyle, FlowText, InlineFlow, WhiteSpace};
use super::markdown::markdown_pipeline::{Block, parse_chat_markdown};
use super::messages_timeline::{RowCx, TimelineUiState};
use super::subagent_tooltip_content::{SubagentTooltipContent, with_subagent_tooltip};
use super::timeline::file_path_display::format_workspace_relative_path;
use super::timeline::item_detail::{
    ToolCallLinesInput, tool_call_lines, turn_item_has_detail, turn_item_output_text,
};
use super::timeline::messages_timeline_logic::{
    MessagesTimelineRow, MessagesTimelineRowContent, live_work_entry_label,
    work_entry_display_label, work_entry_is_visible_in_group, work_entry_read_output,
};
use super::timeline::orchestration_timing::{
    SubagentTiming, derive_subagent_elapsed_ms, format_duration,
};
use super::timeline::session_logic::{
    provider_error_presentation, work_entry_signals_severe_failure, work_log_entry_is_tool_like,
};
use super::timeline::t3_mcp_tool_presentation::T3McpToolIcon;
use super::timeline::turn_item::{
    NotificationOutcome, NotificationSource, ProjectedTurnItem, ProjectedVisibility,
    ProviderFailureClass, ToolActivitySurface, TurnItem, TurnItemPayload, TurnItemStatus,
    TurnItemType,
};
use super::timeline::types::{ToolLifecycleStatus, WorkLogEntry, WorkTone};
use super::timeline::user_input::{
    get_question_answer_preview, get_question_text_preview, has_question_answer,
};
use super::timeline::work_log_presentation::{
    ToolGroupAction, ToolGroupSummaryKind, live_thought_line, resolve_work_entry_tool_presentation,
    tool_group_action, work_entry_display_indicates_tool_failure,
};
use super::timestamp_format::{format_chat_timestamp_tooltip, format_day_aware_timestamp};
use super::work_log::{
    WorkLogDetailsKind, WorkLogLine, timeline_row_group, work_log_details, work_log_row_frame,
};
use crate::state::subagent_display::subagent_detail_preview;
use crate::state::subagent_runtime::RuntimeSubagentStatus;
use crate::theme::AppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{
    LEADING_RELAXED, RADIUS_LG, RADIUS_MD, TEXT_2XS, TEXT_3XS, TEXT_SM, TEXT_XS,
};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

// ── Icons ───────────────────────────────────────────────────────────────────

/// `WorkEntryIconName` → lucide. `Code` (JavaScript REPL tools) and `X`
/// keep upstream's full set.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkEntryIconName {
    PullRequest,
    Bot,
    Brain,
    Browser,
    Code,
    Device,
    T3Code,
    Check,
    CircleAlert,
    Eye,
    Globe,
    Hammer,
    Search,
    MessageCircle,
    SquarePen,
    Terminal,
    Wrench,
    X,
    Zap,
}

impl WorkEntryIconName {
    fn icon(self) -> IconName {
        match self {
            Self::PullRequest => IconName::GitPullRequestArrow,
            Self::Bot => IconName::Bot,
            Self::Brain => IconName::Brain,
            // `BrowserAppIcon` is a framed globe.
            Self::Browser | Self::Globe => IconName::Globe,
            Self::Code => IconName::Code,
            Self::Device => IconName::Smartphone,
            // The T3 wordmark is not bundled; the zap stands in.
            Self::T3Code => IconName::Zap,
            Self::Check => IconName::Check,
            Self::CircleAlert => IconName::CircleAlert,
            Self::Eye => IconName::Eye,
            Self::Hammer => IconName::Hammer,
            Self::Search => IconName::Search,
            Self::MessageCircle => IconName::MessageCircle,
            Self::SquarePen => IconName::SquarePen,
            Self::Terminal => IconName::Terminal,
            Self::Wrench => IconName::Wrench,
            Self::X => IconName::X,
            Self::Zap => IconName::Zap,
        }
    }
}

fn t3_icon_name(icon: T3McpToolIcon) -> WorkEntryIconName {
    match icon {
        T3McpToolIcon::T3Code => WorkEntryIconName::T3Code,
        T3McpToolIcon::Browser => WorkEntryIconName::Browser,
        T3McpToolIcon::Device => WorkEntryIconName::Device,
        T3McpToolIcon::PullRequest => WorkEntryIconName::PullRequest,
    }
}

/// `toolGroupSummaryIconName`.
pub fn tool_group_summary_icon_name(kind: ToolGroupSummaryKind) -> WorkEntryIconName {
    use ToolGroupAction as A;
    match kind {
        ToolGroupSummaryKind::PullRequest
        | ToolGroupSummaryKind::Action(
            A::LinkPr | A::UnlinkPr | A::ListPrs | A::WatchPr | A::UnwatchPr,
        ) => WorkEntryIconName::PullRequest,
        ToolGroupSummaryKind::Action(A::Read) => WorkEntryIconName::Eye,
        ToolGroupSummaryKind::Action(A::Edit) => WorkEntryIconName::SquarePen,
        ToolGroupSummaryKind::Action(A::Command) => WorkEntryIconName::Terminal,
        ToolGroupSummaryKind::Action(A::ThreadCreate) => WorkEntryIconName::T3Code,
        ToolGroupSummaryKind::Action(A::Browser) => WorkEntryIconName::Browser,
        ToolGroupSummaryKind::Action(A::Device) => WorkEntryIconName::Device,
        ToolGroupSummaryKind::Action(A::Search) => WorkEntryIconName::Globe,
        ToolGroupSummaryKind::Action(A::CodeSearch) => WorkEntryIconName::Search,
        ToolGroupSummaryKind::Action(A::Other) => WorkEntryIconName::Wrench,
        ToolGroupSummaryKind::DynamicTool => WorkEntryIconName::Hammer,
        ToolGroupSummaryKind::Reasoning => WorkEntryIconName::Brain,
        ToolGroupSummaryKind::AgentTool => WorkEntryIconName::Bot,
        ToolGroupSummaryKind::ToneTool => WorkEntryIconName::Zap,
        ToolGroupSummaryKind::Action(A::Update) | ToolGroupSummaryKind::Mixed => {
            WorkEntryIconName::Hammer
        }
    }
}

/// `workToneIcon`.
fn work_tone_icon(tone: WorkTone, theme: &AppTheme) -> (WorkEntryIconName, Hsla) {
    match tone {
        WorkTone::Error => (
            WorkEntryIconName::CircleAlert,
            with_alpha(theme.foreground, 0.92).into(),
        ),
        WorkTone::Thinking => (WorkEntryIconName::Brain, theme.icon_muted),
        WorkTone::Info => (WorkEntryIconName::Check, theme.muted_foreground),
        WorkTone::Tool => (
            WorkEntryIconName::Zap,
            with_alpha(theme.foreground, 0.92).into(),
        ),
    }
}

/// `workEntryIconName`.
pub fn work_entry_icon_name(entry: &WorkLogEntry) -> WorkEntryIconName {
    if let Some(TurnItemPayload::Notification {
        source, outcome, ..
    }) = entry.structured_payload.as_ref().map(|item| &item.payload)
    {
        if *outcome == NotificationOutcome::Failed {
            return WorkEntryIconName::CircleAlert;
        }
        return match source {
            NotificationSource::Subagent { .. } | NotificationSource::DelegatedTask { .. } => {
                WorkEntryIconName::Bot
            }
            NotificationSource::Command => WorkEntryIconName::Terminal,
            NotificationSource::Monitor => WorkEntryIconName::Eye,
            NotificationSource::BackgroundTask => WorkEntryIconName::Zap,
            NotificationSource::System => WorkEntryIconName::T3Code,
        };
    }
    if matches!(
        entry.item_type,
        Some(TurnItemType::UserInputRequest | TurnItemType::ApprovalRequest)
    ) {
        return WorkEntryIconName::MessageCircle;
    }
    if let Some(surface) = entry.tool_surface {
        return match surface {
            ToolActivitySurface::Browser | ToolActivitySurface::Computer => {
                WorkEntryIconName::Browser
            }
        };
    }
    if let Some(presentation) = resolve_work_entry_tool_presentation(entry, None) {
        return t3_icon_name(presentation.icon);
    }
    let action = tool_group_action(entry);
    if action != ToolGroupAction::Other {
        return tool_group_summary_icon_name(ToolGroupSummaryKind::Action(action));
    }
    match entry.item_type {
        Some(TurnItemType::DynamicTool) => return WorkEntryIconName::Wrench,
        Some(TurnItemType::Subagent) => return WorkEntryIconName::Bot,
        _ => {}
    }
    WorkEntryIconName::Zap.max_by_tone(entry.tone)
}

impl WorkEntryIconName {
    fn max_by_tone(self, tone: WorkTone) -> Self {
        match tone {
            WorkTone::Error => Self::CircleAlert,
            WorkTone::Thinking => Self::Brain,
            WorkTone::Info => Self::Check,
            WorkTone::Tool => self,
        }
    }
}

fn icon_box(name: WorkEntryIconName, color: Hsla) -> AnyElement {
    div()
        .flex()
        .size(rems(1.))
        .items_center()
        .justify_center()
        .child(Icon::new(name.icon()).size(rems(1.)).color(color))
        .into_any_element()
}

// ── Shared pieces ───────────────────────────────────────────────────────────

/// `TimelineRowTimestamp`: out of flow and invisible until the row is hovered.
fn row_timestamp(
    id: impl Into<ElementId>,
    iso: &str,
    group: &SharedString,
    always: bool,
    cx: &RowCx,
) -> AnyElement {
    let tooltip: SharedString = format_chat_timestamp_tooltip(iso, cx.timestamp_format).into();
    let el = div()
        .id(id)
        .flex_shrink_0()
        .mr(rems(0.25))
        .whitespace_nowrap()
        .text_size(TEXT_XS.size)
        .line_height(TEXT_XS.line_height)
        .text_color(cx.theme.muted_foreground)
        .child(format_day_aware_timestamp(iso, cx.timestamp_format))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
    if always {
        el.into_any_element()
    } else {
        el.absolute()
            .right_0()
            .opacity(0.)
            .group_hover(group.clone(), |style| style.relative().opacity(1.))
            .into_any_element()
    }
}

fn chevron(expanded: bool, visible: bool, cx: &RowCx) -> AnyElement {
    div()
        .flex()
        .flex_shrink_0()
        .size(rems(1.))
        .items_center()
        .justify_center()
        .when(!visible, |this| this.invisible())
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(rems(0.75))
            .color(with_alpha(cx.theme.icon_muted, 0.7).into()),
        )
        .into_any_element()
}

fn mono_style(cx: &RowCx, color: Hsla) -> FlowSpanStyle {
    FlowSpanStyle {
        family: cx.theme.fonts.mono.clone().into(),
        weight: FontWeight::NORMAL,
        italic: false,
        size: px(cx.theme.fonts.size_code),
        line_height: LEADING_RELAXED,
        color,
        strikethrough: false,
        chrome: None,
        link: None,
        background: None,
        atomic: false,
    }
}

/// Selectable monospace text (`whitespace-pre-wrap break-words font-mono`).
fn mono_text(id: impl Into<ElementId>, text: &str, color: Hsla, cx: &RowCx) -> InlineFlow {
    let style = mono_style(cx, color);
    InlineFlow::new(
        id,
        FlowText {
            text: text.to_string().into(),
            spans: vec![FlowSpan {
                len: text.len(),
                style: style.clone(),
            }],
            links: Vec::new(),
        },
        WhiteSpace::PreWrap,
    )
    .strut(style.family, style.size, LEADING_RELAXED)
    .document_order(cx.order + 10)
    .selection_color(selection_color(&cx.theme))
}

/// `ShellCommandBlock`: the command, highlighted as shell.
fn shell_command_block(id: impl Into<ElementId>, command: &str, cx: &RowCx) -> InlineFlow {
    let command = command.trim();
    let highlighted = highlight_code(command, "bash", cx.theme.is_dark());
    let base = mono_style(cx, with_alpha(cx.theme.foreground, 0.85).into());
    let mut spans = Vec::new();
    let mut cursor = 0;
    for (range, color) in &highlighted.tokens {
        if range.start < cursor || range.end > command.len() {
            continue;
        }
        if range.start > cursor {
            spans.push(FlowSpan {
                len: range.start - cursor,
                style: base.clone(),
            });
        }
        spans.push(FlowSpan {
            len: range.end - range.start,
            style: FlowSpanStyle {
                color: *color,
                ..base.clone()
            },
        });
        cursor = range.end;
    }
    if cursor < command.len() {
        spans.push(FlowSpan {
            len: command.len() - cursor,
            style: base.clone(),
        });
    }
    InlineFlow::new(
        id,
        FlowText {
            text: command.to_string().into(),
            spans,
            links: Vec::new(),
        },
        WhiteSpace::PreWrap,
    )
    .strut(base.family.clone(), base.size, LEADING_RELAXED)
    .document_order(cx.order + 9)
    .selection_color(selection_color(&cx.theme))
}

fn selection_color(theme: &AppTheme) -> Hsla {
    with_alpha(
        theme.theme_accent,
        if theme.is_dark() { 0.35 } else { 0.25 },
    )
    .into()
}

/// `remarkThoughtPreview`: the markdown flattened to one line.
fn thought_preview(markdown: &str, fallback: &str) -> String {
    fn collect(blocks: &[Block], out: &mut Vec<String>) {
        for block in blocks {
            match block {
                Block::Paragraph(inlines)
                | Block::Heading {
                    content: inlines, ..
                } => out.push(inlines.text.clone()),
                Block::Blockquote(children) | Block::Alert { children, .. } => {
                    collect(children, out)
                }
                Block::List { items, .. } => {
                    for item in items {
                        collect(&item.children, out);
                    }
                }
                Block::Code { code, .. } => out.push(code.clone()),
                Block::Table { header, rows, .. } => {
                    for cell in header.iter().chain(rows.iter().flatten()) {
                        out.push(cell.text.clone());
                    }
                }
                Block::Html(html) => out.push(html.clone()),
                Block::ThematicBreak | Block::Footnotes(_) => {}
            }
        }
    }
    let mut parts = Vec::new();
    collect(&parse_chat_markdown(markdown, false).blocks, &mut parts);
    let joined = parts.join(" ");
    let collapsed = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        fallback.to_string()
    } else {
        collapsed
    }
}

// ── Work groups ─────────────────────────────────────────────────────────────

/// `WorkGroupSection`.
pub fn work_group_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::Work {
        id,
        grouped_entries,
        is_expanded_tool_group,
        display_label,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let entries: Vec<&WorkLogEntry> = grouped_entries
        .iter()
        .filter(|entry| work_entry_is_visible_in_group(entry, *is_expanded_tool_group))
        .collect();
    if entries.is_empty() {
        return div().into_any_element();
    }
    if *is_expanded_tool_group
        && entries
            .iter()
            .all(|entry| entry.item_type == Some(TurnItemType::Reasoning))
    {
        return reasoning_trace_content(id, &entries, cx);
    }
    if *is_expanded_tool_group {
        // `ExpandedWorkGroupEntries`: a bounded scroller of compact rows. Each
        // open row adds its own height to the bound, so opening one never
        // shrinks the visible list.
        let open = entries
            .iter()
            .filter(|entry| cx.ui.expanded_entries.contains(&entry.id))
            .count();
        let mut list = div()
            .id(ElementId::Name(format!("tool-group-{id}").into()))
            .flex()
            .flex_col()
            .min_w_0()
            .max_h(rems(18. + 16. * open as f32))
            .overflow_y_scroll()
            .rounded(RADIUS_MD);
        for (ix, entry) in entries.iter().enumerate() {
            list = list.child(work_entry_log_row(
                entry,
                None,
                true,
                cx.order + ix as u64 * 64,
                cx,
            ));
        }
        return list.into_any_element();
    }
    let mut section = div().flex().flex_col().min_w_0();
    for (ix, entry) in entries.iter().enumerate() {
        section = section.child(work_entry_log_row(
            entry,
            display_label.as_deref(),
            false,
            cx.order + ix as u64 * 64,
            cx,
        ));
    }
    section.into_any_element()
}

/// `ReasoningTraceContent`.
fn reasoning_trace_content(id: &str, entries: &[&WorkLogEntry], cx: &RowCx) -> AnyElement {
    let mut details = work_log_details(
        ElementId::Name(format!("reasoning-{id}").into()),
        WorkLogDetailsKind::Text,
    );
    for (ix, entry) in entries.iter().enumerate() {
        let streaming = cx.snapshot.is_working
            && entry.run_id.as_deref() == cx.latest_run_id()
            && entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress);
        details = details.child(
            ChatMarkdown::new(
                ElementId::Name(format!("reasoning-md-{}", entry.id).into()),
                entry.detail.clone().unwrap_or_default(),
            )
            .full_foreground(true)
            .line_breaks(true)
            .streaming(streaming)
            .document_order(cx.order + 32 * ix as u64),
        );
    }
    details.into_any_element()
}

/// `workEntryRawCommand`.
fn work_entry_raw_command(entry: &WorkLogEntry) -> Option<String> {
    let raw = entry.raw_command.as_deref()?.trim();
    let command = entry.command.as_deref()?;
    (!raw.is_empty() && raw != command.trim()).then(|| raw.to_string())
}

/// `buildToolCallExpandedBody`.
fn build_tool_call_expanded_body(
    entry: &WorkLogEntry,
    workspace_root: Option<&str>,
    visible_label: &str,
) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    seen.insert(visible_label.trim().to_string());
    let mut add = |value: Option<&str>, blocks: &mut Vec<String>| {
        let Some(text) = value.map(str::trim).filter(|t| !t.is_empty()) else {
            return;
        };
        if seen.insert(text.to_string()) {
            blocks.push(text.to_string());
        }
    };
    if entry.item_type == Some(TurnItemType::DynamicTool)
        && let Some(data) = entry.tool_data.as_ref()
    {
        let input = match entry.structured_payload.as_ref().map(|item| &item.payload) {
            Some(TurnItemPayload::DynamicTool { input, .. }) => Some(input.clone()),
            _ => data.get("input").cloned(),
        };
        if let Some(input) = input {
            let pretty = serde_json::to_string_pretty(&input).unwrap_or_default();
            add(Some(&format!("Tool input\n{pretty}")), &mut blocks);
        }
    }
    let command = entry.command.as_deref().map(str::trim);
    let raw = work_entry_raw_command(entry);
    if command != Some(visible_label.trim()) {
        add(raw.as_deref().or(command), &mut blocks);
    }
    add(entry.detail.as_deref(), &mut blocks);
    let detail = entry.detail.as_deref().map(str::trim);
    let files: Vec<String> = entry
        .changed_files
        .iter()
        .flatten()
        .map(|path| format_workspace_relative_path(path, workspace_root))
        .filter(|path| Some(path.as_str()) != detail)
        .collect();
    if !files.is_empty() {
        let mut unique = Vec::new();
        for file in files {
            if !unique.contains(&file) {
                unique.push(file);
            }
        }
        add(Some(&unique.join("\n")), &mut blocks);
    }
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

/// `WorkEntryLogRow` (`SimpleWorkEntryRow`).
pub fn work_entry_log_row(
    entry: &WorkLogEntry,
    display_label: Option<&str>,
    grouped: bool,
    order: u64,
    cx: &RowCx,
) -> AnyElement {
    let theme = &cx.theme;
    let cx = &RowCx {
        order,
        ..cx.clone()
    };
    let row_id = ElementId::Name(format!("work-entry-{}", entry.id).into());
    let group = timeline_row_group(&row_id);
    let expanded = cx.ui.expanded_entries.contains(&entry.id);
    let workspace_root = cx.workspace_root();

    // Provider failures keep their own row.
    if let Some(item) = entry
        .projected_item
        .as_ref()
        .map(|projected| &projected.item)
        && let TurnItemPayload::Error { failure, .. } = &item.payload
        && item.status == TurnItemStatus::Failed
    {
        let warning = failure.class == ProviderFailureClass::UsageLimit;
        let color = if warning {
            theme.warning
        } else {
            theme.destructive
        };
        let label = if warning {
            "Usage limit reached.".to_string()
        } else {
            entry.label.clone()
        };
        return work_log_row_frame(row_id, grouped)
            .group(group.clone())
            .child(WorkLogLine {
                icon: Some(
                    Icon::new(IconName::CircleAlert)
                        .size(rems(1.))
                        .color(color)
                        .into_any_element(),
                ),
                label: div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child(label)
                    .into_any_element(),
                trailing: vec![row_timestamp(
                    ("err-time", order),
                    &entry.created_at,
                    &group,
                    true,
                    cx,
                )],
                wrap_label: true,
            })
            .when(!warning, |this| {
                this.child(
                    div()
                        .ml(rems(1.75))
                        .py(rems(0.25))
                        .text_size(TEXT_SM.size)
                        .line_height(relative(LEADING_RELAXED))
                        .text_color(with_alpha(theme.foreground, 0.8))
                        .child(failure.message.clone()),
                )
            })
            .into_any_element();
    }

    let show_warning = entry.source_activity_kind.as_deref() == Some("runtime.warning");
    let show_failed = !show_warning && work_entry_display_indicates_tool_failure(entry);
    let destructive = !show_warning
        && show_failed
        && (work_entry_signals_severe_failure(entry) || !work_log_entry_is_tool_like(entry));
    let icon_name = if show_warning || destructive {
        WorkEntryIconName::CircleAlert
    } else {
        work_entry_icon_name(entry)
    };
    let (_, tone_color) = work_tone_icon(entry.tone, theme);
    let failed_icon: Hsla = with_alpha(theme.tool_error_icon, 0.4).into();
    let icon_color = if show_warning {
        theme.warning
    } else if destructive {
        theme.destructive
    } else if show_failed {
        failed_icon
    } else if entry.tone == WorkTone::Tool {
        theme.icon_muted
    } else {
        tone_color
    };
    let heading_color: Hsla = if show_warning {
        theme.warning
    } else if destructive {
        theme.destructive
    } else if work_log_entry_is_tool_like(entry) {
        theme.secondary_label
    } else {
        with_alpha(theme.foreground, 0.8).into()
    };
    let is_reasoning = entry.item_type == Some(TurnItemType::Reasoning);
    let question_heading = entry
        .question_answer
        .as_ref()
        .map(get_question_text_preview)
        .unwrap_or_default();
    let in_progress = entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress);
    let preview_text = if is_reasoning && expanded {
        if in_progress { "Thinking" } else { "Thought" }.to_string()
    } else if !question_heading.is_empty() {
        question_heading
    } else if let Some(label) = display_label {
        label.to_string()
    } else {
        work_entry_display_label(entry, workspace_root)
    };
    let label_text = if is_reasoning && !expanded {
        thought_preview(
            entry.detail.as_deref().unwrap_or(&preview_text),
            if in_progress { "Thinking" } else { "Thought" },
        )
    } else {
        preview_text.clone()
    };
    let answer_preview = entry
        .question_answer
        .as_ref()
        .filter(|answer| has_question_answer(answer))
        .map(get_question_answer_preview);
    let plain_output = if tool_group_action(entry) == ToolGroupAction::Read {
        Some(work_entry_read_output(entry, workspace_root))
    } else {
        None
    };
    let can_expand = match (&plain_output, &entry.projected_item) {
        (Some(output), _) => output.is_some() || entry.question_answer.is_some(),
        (None, None) => {
            !preview_text.trim().is_empty()
                || entry
                    .command
                    .as_deref()
                    .is_some_and(|c| !c.trim().is_empty())
                || entry
                    .detail
                    .as_deref()
                    .is_some_and(|d| !d.trim().is_empty())
                || entry.changed_files.as_ref().is_some_and(|f| !f.is_empty())
        }
        (None, Some(projected)) => {
            if is_reasoning {
                entry
                    .detail
                    .as_deref()
                    .is_some_and(|d| !d.trim().is_empty())
            } else {
                entry.question_answer.is_some() || inspector_has_detail(&projected.item)
            }
        }
    };

    let label = div()
        .flex()
        .min_w_0()
        .w_full()
        .items_baseline()
        .gap(rems(0.375))
        .child(
            div()
                .min_w_0()
                .when(answer_preview.is_none(), |this| this.flex_1())
                .truncate()
                .text_color(heading_color)
                .when(show_warning || destructive, |this| {
                    this.font_weight(FontWeight::MEDIUM)
                })
                .child(label_text),
        )
        .when_some(answer_preview.clone(), |this, answer| {
            this.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if expanded {
                        theme.muted_foreground
                    } else {
                        theme.foreground
                    })
                    .child(answer),
            )
        });

    let mut trailing = Vec::new();
    if let Some(TurnItemPayload::ThreadCreated {
        target_thread_id, ..
    }) = entry.projected_item.as_ref().map(|p| &p.item.payload)
    {
        let source = cx.source.clone();
        let target = target_thread_id.clone();
        trailing.push(
            div()
                .id(("open-chat", order))
                .flex_shrink_0()
                .text_size(TEXT_SM.size)
                .text_color(theme.primary)
                .cursor_pointer()
                .hover(|s| s.underline())
                .child("Open chat")
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation()
                })
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    if let Some(source) = source.as_ref() {
                        source.open_thread(&target, cx);
                    }
                })
                .into_any_element(),
        );
    }
    trailing.push(row_timestamp(
        ("work-time", order),
        &entry.created_at,
        &group,
        false,
        cx,
    ));
    trailing.push(chevron(expanded, can_expand, cx));

    let line = WorkLogLine {
        icon: Some(icon_box(icon_name, icon_color)),
        label: label.into_any_element(),
        trailing,
        wrap_label: false,
    };
    let entry_id = entry.id.clone();
    let row_cx = cx.clone();
    let mut frame = work_log_row_frame(row_id, grouped)
        .group(group.clone())
        .child(line);
    if can_expand {
        frame = frame
            .cursor_pointer()
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation()
            })
            .on_click(move |_, _, cx| {
                let id = entry_id.clone();
                row_cx.update_ui(cx, |ui: &mut TimelineUiState| {
                    RowCx::toggle(&mut ui.expanded_entries, &id)
                });
            });
    }
    if expanded {
        if let Some(answer) = entry.question_answer.as_ref() {
            frame = frame.child(question_answer_history(answer, cx));
        } else if is_reasoning {
            frame = frame.child(reasoning_trace_content(&entry.id, &[entry], cx));
        } else if let Some(output) = plain_output {
            if let Some(output) = output {
                frame = frame.child(
                    work_log_details(("details", order), WorkLogDetailsKind::Panel).child(
                        mono_text(("read-out", order), &output, theme.secondary_label, cx),
                    ),
                );
            }
        } else if let Some(projected) = entry.projected_item.as_ref() {
            frame = frame.child(
                work_log_details(("details", order), WorkLogDetailsKind::Panel)
                    .child(v2_item_inspector(projected, cx)),
            );
        } else if let Some(body) =
            build_tool_call_expanded_body(entry, workspace_root, &preview_text)
        {
            frame = frame.child(
                work_log_details(("details", order), WorkLogDetailsKind::Panel).child(
                    div().max_h(rems(16.)).overflow_hidden().child(mono_text(
                        ("body", order),
                        &body,
                        theme.secondary_label,
                        cx,
                    )),
                ),
            );
        }
    }
    frame.into_any_element()
}

/// `QuestionAnswerHistory`.
fn question_answer_history(
    answer: &super::timeline::turn_item::UserInputAnswerPayload,
    cx: &RowCx,
) -> AnyElement {
    let theme = &cx.theme;
    let mut ids: Vec<String> = Vec::new();
    for (id, _) in answer
        .question_text_by_id
        .iter()
        .map(|(id, text)| (id, text.clone()))
        .chain(answer.answers.iter().map(|(id, _)| (id, String::new())))
    {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    let mut list = div()
        .ml(rems(1.75))
        .mt(rems(0.5))
        .flex()
        .flex_col()
        .gap(rems(0.5));
    for id in ids {
        let question = answer
            .question_text_by_id
            .iter()
            .find(|(qid, _)| *qid == id)
            .map(|(_, text)| text.clone());
        let answer_text = super::timeline::user_input::get_question_answer_text(
            answer
                .answers
                .iter()
                .find(|(qid, _)| *qid == id)
                .map(|(_, value)| value),
        );
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.25))
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .when_some(question, |this, q| this.child(q))
                .when(!answer_text.is_empty(), |this| {
                    this.child(div().ml(rems(0.75)).child(answer_text))
                }),
        );
    }
    list.into_any_element()
}

/// Whether the reduced inspector has anything to show.
fn inspector_has_detail(item: &TurnItem) -> bool {
    match &item.payload {
        TurnItemPayload::CommandExecution { .. }
        | TurnItemPayload::FileChange { .. }
        | TurnItemPayload::DynamicTool { .. } => true,
        TurnItemPayload::FileSearch { pattern, results } => {
            pattern.is_some() || !results.is_empty()
        }
        TurnItemPayload::WebSearch { patterns, .. } => !patterns.is_empty(),
        TurnItemPayload::Reasoning { text, .. } => !text.trim().is_empty(),
        _ => turn_item_has_detail(item) || turn_item_output_text(item).is_some(),
    }
}

/// `StructuredValue`: a muted monospace block, scrolling past `max-h-80`.
fn inspector_text(id: (&'static str, u64), text: &str, cx: &RowCx) -> AnyElement {
    div()
        .id(id)
        .max_h(rems(20.))
        .overflow_y_scroll()
        .child(mono_text(
            (id.0, id.1 + 1),
            text,
            cx.theme.muted_foreground,
            cx,
        ))
        .into_any_element()
}

/// `V2ItemInspector`, reduced to what the turn item already carries (Turin
/// has no withheld output to fetch: the item holds it).
fn v2_item_inspector(projected: &ProjectedTurnItem, cx: &RowCx) -> AnyElement {
    let theme = &cx.theme;
    let item = &projected.item;
    let order = cx.order;
    let muted = theme.muted_foreground;
    let mut column = div()
        .flex()
        .flex_col()
        .gap(rems(0.375))
        .text_size(TEXT_XS.size);
    match &item.payload {
        TurnItemPayload::CommandExecution {
            input,
            output,
            exit_code,
            ..
        } => {
            let call = tool_call_lines(ToolCallLinesInput {
                command: Some(input),
                args: None,
            });
            if let Some(command) = call.command {
                column = column.child(shell_command_block(("cmd", order), &command, cx));
            }
            match output
                .as_deref()
                .map(str::trim_end)
                .filter(|o| !o.is_empty())
            {
                Some(output) => {
                    column = column.child(
                        div()
                            .id(("cmd-out", order))
                            .max_h(rems(20.))
                            .overflow_y_scroll()
                            .child(mono_text(("cmd-out-text", order), output, muted, cx)),
                    )
                }
                None => {
                    column = column.child(div().italic().text_color(muted).child(
                        if item.status.is_live() {
                            "Running…"
                        } else {
                            "No output."
                        },
                    ))
                }
            }
            if let Some(code) = exit_code.filter(|code| *code != 0) {
                column = column.child(
                    div()
                        .font_family(theme.fonts.mono.clone())
                        .text_color(theme.destructive)
                        .child(format!("exit {code}")),
                );
            }
        }
        TurnItemPayload::DynamicTool { input, output, .. } => {
            let call = tool_call_lines(ToolCallLinesInput {
                command: None,
                args: Some(input),
            });
            if let Some(args) = call.args {
                let text = args
                    .iter()
                    .map(|(key, value)| format!("{key} {value}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                column = column.child(mono_text(
                    ("args", order),
                    &text,
                    with_alpha(theme.foreground, 0.85).into(),
                    cx,
                ));
            } else if let Some(text) = call.args_text {
                column = column.child(mono_text(
                    ("args", order),
                    &text,
                    with_alpha(theme.foreground, 0.85).into(),
                    cx,
                ));
            }
            if let Some(output) = output {
                let text = match output {
                    serde_json::Value::String(text) => text.clone(),
                    other => serde_json::to_string_pretty(other).unwrap_or_default(),
                };
                if !text.trim().is_empty() {
                    column = column.child(
                        div()
                            .id(("tool-out", order))
                            .max_h(rems(20.))
                            .overflow_y_scroll()
                            .child(mono_text(("tool-out-text", order), &text, muted, cx)),
                    );
                }
            }
        }
        TurnItemPayload::FileChange {
            file_name,
            additions,
            deletions,
            diff,
            changes,
        } => {
            let path = format_workspace_relative_path(file_name, cx.workspace_root());
            let source = cx.source.clone();
            let run_id = item.run_id.clone();
            let file = file_name.clone();
            column = column.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(0.5))
                    .child(
                        div()
                            .font_family(theme.fonts.mono.clone())
                            .text_color(muted)
                            .child(path),
                    )
                    .when(additions.is_some() || deletions.is_some(), |this| {
                        this.child(
                            div()
                                .flex()
                                .gap(rems(0.25))
                                .child(
                                    div()
                                        .text_color(theme.success)
                                        .child(format!("+{}", additions.unwrap_or(0))),
                                )
                                .child(
                                    div()
                                        .text_color(theme.destructive)
                                        .child(format!("-{}", deletions.unwrap_or(0))),
                                ),
                        )
                    })
                    .when_some(run_id, |this, run_id| {
                        this.child(
                            crate::ui::Button::new(("open-diff", order))
                                .variant(crate::ui::ButtonVariant::Outline)
                                .size(crate::ui::ButtonSize::Xs)
                                .label("Open diff")
                                .on_click(move |_, _, cx| {
                                    if let Some(source) = source.as_ref() {
                                        source.open_turn_diff(&run_id, Some(&file), cx);
                                    }
                                }),
                        )
                    }),
            );
            // A failed edit shows the diff it attempted.
            if item.status == TurnItemStatus::Failed
                && let Some(diff) = diff.as_deref().filter(|diff| !diff.trim().is_empty())
            {
                column = column.child(
                    div()
                        .id(("failed-diff", order))
                        .max_h(rems(20.))
                        .overflow_y_scroll()
                        .child(mono_text(("failed-diff-text", order), diff, muted, cx)),
                );
            }
            if let Some(changes) = changes.as_ref().filter(|changes| !changes.is_empty()) {
                column = column.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.25))
                        .font_family(theme.fonts.mono.clone())
                        .text_color(muted)
                        .children(changes.iter().map(|change| {
                            let kind = [change.file_type.as_deref(), change.mime_type.as_deref()]
                                .into_iter()
                                .flatten()
                                .filter(|part| !part.is_empty())
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!(
                                "{} {}{}{}",
                                change.operation,
                                change
                                    .old_path
                                    .as_deref()
                                    .map(|old| format!("{old} → "))
                                    .unwrap_or_default(),
                                format_workspace_relative_path(&change.path, cx.workspace_root()),
                                if kind.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({kind})")
                                }
                            )
                        })),
                );
            }
        }
        TurnItemPayload::FileSearch { pattern, results } => {
            if let Some(pattern) = pattern.as_deref().filter(|p| !p.trim().is_empty()) {
                column = column.child(mono_text(
                    ("pattern", order),
                    pattern,
                    with_alpha(theme.foreground, 0.85).into(),
                    cx,
                ));
            }
            for (ix, result) in results.iter().enumerate() {
                column = column.child(
                    div()
                        .font_family(theme.fonts.mono.clone())
                        .text_color(with_alpha(theme.foreground, 0.8))
                        .child(format_workspace_relative_path(result, cx.workspace_root()))
                        .id(("result", order + ix as u64)),
                );
            }
        }
        TurnItemPayload::WebSearch { patterns, results } => {
            if !patterns.is_empty() {
                column = column.child(mono_text(
                    ("queries", order),
                    &patterns.join("\n"),
                    with_alpha(theme.foreground, 0.85).into(),
                    cx,
                ));
            }
            for (title, url) in results {
                column = column.child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(
                            title
                                .clone()
                                .or_else(|| url.clone())
                                .unwrap_or_else(|| "Search result".into()),
                        ),
                );
            }
        }
        TurnItemPayload::Reasoning { text, .. } => {
            column = column.child(
                div()
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.45))
                    .bg(with_alpha(theme.muted, 0.15))
                    .p(rems(0.5))
                    .italic()
                    .text_color(muted)
                    .child(
                        ChatMarkdown::new(("inspect-reasoning", order), text.clone())
                            .line_breaks(true),
                    ),
            );
        }
        TurnItemPayload::ApprovalRequest { prompt, .. } => {
            if let Some(prompt) = prompt.as_deref().filter(|p| !p.is_empty()) {
                column = column.child(inspector_text(("approval", order), prompt, cx));
            }
        }
        TurnItemPayload::UserInputRequest { questions, .. } => {
            let text = questions
                .iter()
                .map(|question| question.question.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            column = column.child(inspector_text(("questions", order), &text, cx));
        }
        TurnItemPayload::Notification { detail, .. } => {
            if let Some(detail) = detail.as_deref().filter(|d| !d.is_empty()) {
                column = column.child(inspector_text(("notification", order), detail, cx));
            }
        }
        TurnItemPayload::SystemNotice { message } => {
            column = column.child(inspector_text(("notice", order), message, cx));
        }
        TurnItemPayload::Error { failure, .. } => {
            column = column.child(inspector_text(("error", order), &failure.message, cx));
        }
        TurnItemPayload::ProposedPlan { markdown, .. } => {
            column = column.child(inspector_text(("plan", order), markdown, cx));
        }
        TurnItemPayload::TodoList { steps, .. } => {
            let text = steps
                .iter()
                .map(|step| {
                    let mark =
                        if step.status == super::timeline::turn_item::PlanStepStatus::Completed {
                            "✓"
                        } else {
                            "○"
                        };
                    format!("{mark} {}", step.text)
                })
                .collect::<Vec<_>>()
                .join("\n");
            column = column.child(inspector_text(("todo", order), &text, cx));
        }
        TurnItemPayload::Checkpoint { files, .. } => {
            column = column.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(0.5))
                    .child(div().text_color(muted).child(format!(
                        "{} · {} files",
                        item.status.as_str(),
                        files.len()
                    ))),
            );
        }
        TurnItemPayload::Fork {
            target_thread_id, ..
        } => {
            let source = cx.source.clone();
            let target = target_thread_id.clone();
            column = column.child(
                div().child(
                    crate::ui::Button::new(("open-fork", order))
                        .variant(crate::ui::ButtonVariant::Outline)
                        .size(crate::ui::ButtonSize::Xs)
                        .icon(IconName::GitBranch)
                        .icon_size(rems(0.75))
                        .label("Open fork")
                        .on_click(move |_, _, cx| {
                            if let Some(source) = source.as_ref() {
                                source.open_thread(&target, cx);
                            }
                        }),
                ),
            );
        }
        TurnItemPayload::Subagent {
            child_thread_id: Some(child_thread_id),
            ..
        } => {
            let source = cx.source.clone();
            let target = child_thread_id.clone();
            column = column.child(
                div().child(
                    crate::ui::Button::new(("open-subagent", order))
                        .variant(crate::ui::ButtonVariant::Outline)
                        .size(crate::ui::ButtonSize::Xs)
                        .label("Open subagent thread")
                        .on_click(move |_, _, cx| {
                            if let Some(source) = source.as_ref() {
                                source.open_thread(&target, cx);
                            }
                        }),
                ),
            );
        }
        TurnItemPayload::Handoff { endpoints, .. } => {
            if let Some(endpoints) = endpoints {
                let from = endpoints
                    .from
                    .iter()
                    .map(|endpoint| endpoint.instance_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                column = column.child(
                    div()
                        .text_color(muted)
                        .child(format!("{from} → {}", endpoints.to.instance_id.as_str())),
                );
            }
        }
        _ => {
            if let Some(output) = turn_item_output_text(item) {
                column = column.child(mono_text(("output", order), &output, muted, cx));
            }
        }
    }
    if projected.visibility == ProjectedVisibility::Inherited {
        column = column.child(
            div()
                .font_family(theme.fonts.mono.clone())
                .text_size(TEXT_3XS.size)
                .text_color(with_alpha(muted, 0.65))
                .child(format!("From {}", projected.source_thread_id)),
        );
    }
    div().ml(rems(1.75)).child(column).into_any_element()
}

// ── Live activity, toggles, thinking ────────────────────────────────────────

/// `LiveActivityRow` / `LiveActivityContent`.
fn live_activity_line(
    label: AnyElement,
    icon: WorkEntryIconName,
    failed: bool,
    highlighted: bool,
    cx: &RowCx,
) -> WorkLogLine {
    let theme = &cx.theme;
    let color: Hsla = if failed {
        with_alpha(theme.tool_error_icon, 0.4).into()
    } else if highlighted {
        theme.foreground
    } else {
        theme.icon_muted
    };
    WorkLogLine {
        icon: Some(icon_box(icon, color)),
        label,
        trailing: Vec::new(),
        wrap_label: false,
    }
}

/// `LiveWorkEntryTimelineRow`.
pub fn work_live_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::WorkLive {
        id,
        entry,
        group_id,
        expanded,
        active,
        thought,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let theme = &cx.theme;
    let question_heading = entry
        .question_answer
        .as_ref()
        .map(get_question_text_preview)
        .unwrap_or_default();
    let label = if question_heading.is_empty() {
        live_work_entry_label(entry, cx.workspace_root(), *active)
    } else {
        question_heading
    };
    let failed = work_entry_display_indicates_tool_failure(entry);
    // The expanded group already lists the thought, so the preview steps aside.
    let thought_line = thought
        .as_ref()
        .filter(|_| !*expanded)
        .map(|thought| live_thought_line(thought.detail.as_deref().unwrap_or("")))
        .unwrap_or_default();
    let thought_is_status =
        !thought_line.is_empty() && thought.as_ref().is_some_and(|t| t.id == entry.id);
    let status_label = if thought_is_status {
        if *active { "Thinking" } else { "Thought" }.to_string()
    } else if entry.item_type == Some(TurnItemType::Reasoning) {
        thought_preview(
            entry.detail.as_deref().unwrap_or(&label),
            if *active { "Thinking" } else { "Thought" },
        )
    } else {
        label
    };
    let label_el = div()
        .truncate()
        .text_color(theme.secondary_label)
        .child(status_label)
        .when_some(
            entry
                .question_answer
                .as_ref()
                .filter(|answer| has_question_answer(answer)),
            |this, answer| {
                this.child(
                    div()
                        .ml(rems(0.375))
                        .text_color(theme.foreground)
                        .child(get_question_answer_preview(answer)),
                )
            },
        )
        .into_any_element();
    let group_id = group_id.clone();
    let anchor = id.clone();
    let row_cx = cx.clone();
    let _ = anchor;
    div()
        .id(ElementId::Name(format!("live-{id}").into()))
        .flex()
        .flex_col()
        .w_full()
        .rounded(RADIUS_MD)
        .cursor_pointer()
        .when(!thought_line.is_empty() && !thought_is_status, |this| {
            this.child(WorkLogLine {
                icon: Some(icon_box(WorkEntryIconName::Brain, theme.icon_muted)),
                label: div()
                    .text_color(theme.secondary_label)
                    .line_clamp(4)
                    .child(thought_line.clone())
                    .into_any_element(),
                trailing: Vec::new(),
                wrap_label: true,
            })
        })
        .child(live_activity_line(
            label_el,
            work_entry_icon_name(entry),
            failed,
            false,
            cx,
        ))
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation()
        })
        .on_click(move |_, _, cx| {
            let group = group_id.clone();
            row_cx.update_ui(cx, |ui| {
                RowCx::toggle(&mut ui.expanded_work_group_ids, &group)
            });
        })
        .into_any_element()
}

/// `WorkGroupToggleTimelineRow` → `WorkGroupHeader`.
pub fn work_toggle_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::WorkToggle {
        id,
        created_at,
        group_id,
        summary,
        summary_kind,
        tool_surface,
        summary_tool_icon,
        has_failure,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let theme = &cx.theme;
    let icon = summary_tool_icon
        .map(t3_icon_name)
        .or_else(|| tool_surface.map(|_| WorkEntryIconName::Browser))
        .unwrap_or_else(|| tool_group_summary_icon_name(*summary_kind));
    let row_id = ElementId::Name(format!("toggle-{id}").into());
    let group = timeline_row_group(&row_id);
    let group_id = group_id.clone();
    let row_cx = cx.clone();
    let _ = has_failure;
    super::work_log::work_log_button(
        row_id,
        false,
        WorkLogLine {
            icon: Some(icon_box(icon, theme.icon_muted)),
            label: div().truncate().child(summary.clone()).into_any_element(),
            trailing: vec![row_timestamp(
                ("toggle-time", cx.order),
                created_at,
                &group,
                false,
                cx,
            )],
            wrap_label: false,
        },
        move |_, _, cx| {
            let group = group_id.clone();
            row_cx.update_ui(cx, |ui| {
                RowCx::toggle(&mut ui.expanded_work_group_ids, &group)
            });
        },
    )
    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
        cx.stop_propagation()
    })
    .group(group)
    .into_any_element()
}

/// `ThinkingTimelineRow`.
pub fn thinking_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::Thinking { id, group_id, .. } = &row.content else {
        return div().into_any_element();
    };
    if cx.snapshot.is_compacting {
        // Reserve the activity row so the handoff keeps the same height.
        return div().min_h(rems(1.5)).into_any_element();
    }
    let label = div()
        .truncate()
        .text_color(cx.theme.secondary_label)
        .child("Thinking")
        .into_any_element();
    let line = live_activity_line(label, WorkEntryIconName::Brain, false, false, cx);
    match group_id {
        None => div().child(line).into_any_element(),
        Some(group_id) => {
            let group_id = group_id.clone();
            let row_cx = cx.clone();
            div()
                .id(ElementId::Name(format!("thinking-{id}").into()))
                .w_full()
                .cursor_pointer()
                .child(line)
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation()
                })
                .on_click(move |_, _, cx| {
                    let group = group_id.clone();
                    row_cx.update_ui(cx, |ui| {
                        RowCx::toggle(&mut ui.expanded_work_group_ids, &group)
                    });
                })
                .into_any_element()
        }
    }
}

/// `formatWorkingTimer`.
pub fn format_working_timer(start: &str, end: DateTime<Utc>) -> Option<String> {
    let start = DateTime::parse_from_rfc3339(start).ok()?;
    let elapsed = (end.timestamp_millis() - start.timestamp_millis()).max(0) / 1000;
    if elapsed < 60 {
        return Some(format!("{elapsed}s"));
    }
    Some(format_duration((elapsed * 1000) as f64))
}

/// `WorkingTimelineRow`.
pub fn working_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::Working { created_at, .. } = &row.content else {
        return div().into_any_element();
    };
    let theme = &cx.theme;
    let label: AnyElement = if cx.snapshot.is_compacting {
        div()
            .flex()
            .items_center()
            .gap(rems(0.375))
            .child(Icon::new(IconName::Minimize2).size(rems(0.75)))
            .child("Compacting…")
            .into_any_element()
    } else if let Some(created_at) = created_at {
        let timer = format_working_timer(created_at, Utc::now()).unwrap_or_else(|| "0s".into());
        div()
            .child(format!("Working for {timer}"))
            .into_any_element()
    } else {
        div().child("Working...").into_any_element()
    };
    div()
        .border_b_1()
        .border_color(with_alpha(theme.border, 0.6))
        .pb(rems(0.5))
        .pt(rems(0.25))
        .child(
            div()
                .flex()
                .h(rems(1.5))
                .min_w_0()
                .items_center()
                .gap(rems(0.5))
                .px(rems(0.25))
                .text_size(TEXT_SM.size)
                .line_height(relative(LEADING_RELAXED))
                .text_color(theme.muted_foreground)
                .child(div().flex_shrink_0().whitespace_nowrap().child(label)),
        )
        .into_any_element()
}

// ── Folds and dividers ──────────────────────────────────────────────────────

/// `TurnFoldTimelineRow`.
pub fn turn_fold_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::TurnFold {
        id,
        created_at,
        run_id,
        label,
        expanded,
    } = &row.content
    else {
        return div().into_any_element();
    };
    let theme = &cx.theme;
    let row_id = ElementId::Name(format!("fold-{id}").into());
    let group = timeline_row_group(&row_id);
    let run_id = run_id.clone();
    let row_cx = cx.clone();
    div()
        .id(row_id)
        .group(group.clone())
        .relative()
        .flex()
        .items_center()
        .gap(rems(0.25))
        .border_b_1()
        .border_color(with_alpha(theme.border, 0.6))
        .pb(rems(0.5))
        .pr(rems(0.125))
        .pt(rems(0.25))
        .child(
            div()
                .id(ElementId::Name(format!("fold-button-{id}").into()))
                .flex()
                .items_center()
                .gap(rems(0.25))
                .rounded(RADIUS_MD)
                .px(rems(0.25))
                .cursor_pointer()
                .text_size(TEXT_SM.size)
                .line_height(relative(LEADING_RELAXED))
                .text_color(theme.muted_foreground)
                .hover(|s| s.text_color(theme.foreground))
                .child(label.clone())
                .child(
                    Icon::new(if *expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(rems(0.875)),
                )
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation()
                })
                .on_click(move |_, _, cx| {
                    let run = run_id.clone();
                    row_cx.update_ui(cx, |ui| RowCx::toggle(&mut ui.expanded_run_ids, &run));
                }),
        )
        .child(div().ml_auto().child(row_timestamp(
            ("fold-time", cx.order),
            created_at,
            &group,
            false,
            cx,
        )))
        .into_any_element()
}

/// `AttemptFoldTimelineRow`.
pub fn attempt_fold_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::AttemptFold {
        id,
        attempt_id,
        label,
        expanded,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let theme = &cx.theme;
    let attempt_id = attempt_id.clone();
    let row_cx = cx.clone();
    div()
        .id(ElementId::Name(format!("attempt-{id}").into()))
        .flex()
        .w_full()
        .items_center()
        .gap(rems(0.5))
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(with_alpha(theme.border, 0.6))
        .bg(with_alpha(theme.muted, 0.2))
        .px(rems(0.625))
        .py(rems(0.5))
        .cursor_pointer()
        .hover(|s| s.bg(with_alpha(theme.muted, 0.35)))
        .child(
            Icon::new(if *expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(rems(0.875))
            .color(theme.muted_foreground),
        )
        .child(
            div()
                .text_size(TEXT_XS.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(with_alpha(theme.foreground, 0.8))
                .child(label.clone()),
        )
        .child(
            div()
                .text_size(TEXT_2XS.size)
                .text_color(theme.muted_foreground)
                .child("Cut off by a steer"),
        )
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation()
        })
        .on_click(move |_, _, cx| {
            let attempt = attempt_id.clone();
            row_cx.update_ui(cx, |ui| {
                RowCx::toggle(&mut ui.expanded_attempt_ids, &attempt)
            });
        })
        .into_any_element()
}

/// `ContextCompactionTimelineRow`.
pub fn context_compaction_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::ContextCompaction { label, .. } = &row.content else {
        return div().into_any_element();
    };
    let theme = &cx.theme;
    let rule = || div().h(px(1.)).flex_1().bg(with_alpha(theme.border, 0.7));
    div()
        .flex()
        .w_full()
        .items_center()
        .gap(rems(0.75))
        .py(rems(0.25))
        .text_size(TEXT_XS.size)
        .text_color(theme.muted_foreground)
        .child(rule())
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(rems(0.375))
                .child(Icon::new(IconName::Minimize2).size(rems(0.75)))
                .child(label.clone()),
        )
        .child(rule())
        .into_any_element()
}

type DividerAction = Rc<dyn Fn(&mut App)>;

/// The props of `TimelineSystemDivider`.
struct SystemDivider<'a> {
    label: &'a str,
    /// A plain-string detail (`max-w-80 truncate`).
    detail: Option<&'a str>,
    /// A rich detail (`ReactNode`), such as the handoff's endpoints.
    detail_element: Option<AnyElement>,
    danger: bool,
    icon: Option<IconName>,
    /// `showDetailSeparator !== false`: the "·" before the detail.
    show_detail_separator: bool,
    /// `actionLabel` + `onAction`: the content becomes a pill button.
    action: Option<(String, DividerAction)>,
}

impl<'a> SystemDivider<'a> {
    fn new(label: &'a str) -> Self {
        Self {
            label,
            detail: None,
            detail_element: None,
            danger: false,
            icon: None,
            show_detail_separator: true,
            action: None,
        }
    }
}

/// `TimelineSystemDivider`.
fn timeline_system_divider(
    label: &str,
    detail: Option<&str>,
    danger: bool,
    icon: Option<IconName>,
    cx: &RowCx,
) -> AnyElement {
    system_divider(
        SystemDivider {
            detail,
            danger,
            icon,
            ..SystemDivider::new(label)
        },
        cx,
    )
}

fn system_divider(props: SystemDivider<'_>, cx: &RowCx) -> AnyElement {
    let theme = &cx.theme;
    let rule = || div().h(px(1.)).flex_1().bg(with_alpha(theme.border, 0.7));
    let SystemDivider {
        label,
        detail,
        detail_element,
        danger,
        icon,
        show_detail_separator,
        action,
    } = props;
    let separator = if show_detail_separator {
        "·\u{a0}"
    } else {
        ""
    };
    let content = div()
        .flex()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .justify_center()
        .gap(rems(0.375))
        .when_some(icon, |this, icon| {
            this.child(
                div()
                    .flex_shrink_0()
                    .child(Icon::new(icon).size(rems(0.75))),
            )
        })
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .when_some(detail, |this, detail| {
            this.child(
                div()
                    .max_w(rems(20.))
                    .truncate()
                    .opacity(0.7)
                    .child(format!("{separator}{detail}")),
            )
        })
        .when_some(detail_element, |this, element| {
            this.child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .opacity(0.7)
                    .child(separator)
                    .child(element),
            )
        });
    let middle = match action {
        Some((action_label, on_action)) => {
            let hover_bg = theme.accent;
            let hover_fg = theme.foreground;
            div()
                .id(ElementId::Name(format!("system-divider-{label}").into()))
                .flex()
                .min_w_0()
                .cursor_pointer()
                .items_center()
                .gap(rems(0.375))
                .rounded_full()
                .border_1()
                .border_color(with_alpha(theme.border, 0.7))
                .bg(theme.background)
                .px(rems(0.625))
                .py(rems(0.25))
                .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
                .child(content)
                .on_click(move |_, _, cx| on_action(cx))
                .tooltip({
                    let action_label = action_label.clone();
                    move |window, cx| Tooltip::new(action_label.clone()).build(window, cx)
                })
                .into_any_element()
        }
        None => content
            .rounded_full()
            .px(rems(0.5))
            .py(rems(0.25))
            .into_any_element(),
    };
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap(rems(0.5))
        .py(rems(0.5))
        .text_size(TEXT_2XS.size)
        .text_color(if danger {
            theme.destructive
        } else {
            theme.muted_foreground
        })
        .child(rule())
        .child(middle)
        .child(rule())
        .into_any_element()
}

/// `HandoffEndpoint`: the provider glyph and the model (else the provider) name, with a tooltip.
fn handoff_endpoint(
    id: ElementId,
    endpoint: &crate::state::helpers::handoff::HandoffEndpoint,
    cx: &RowCx,
) -> AnyElement {
    let theme = &cx.theme;
    let instance = endpoint.instance_id.as_str().to_owned();
    let label = endpoint
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| instance.clone());
    let glyph = crate::assets::provider_icon_path(&instance).map(|path| {
        crate::ui::Icon::path(path)
            .size(rems(0.75))
            .color(theme.foreground)
    });
    let tooltip = format!("{instance} · {label}");
    div()
        .id(id)
        .flex()
        .min_w_0()
        .items_center()
        .gap(rems(0.25))
        .rounded_sm()
        .children(glyph)
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}

/// `HandoffRow`.
fn handoff_row(
    endpoints: &crate::state::helpers::handoff::HandoffEndpoints,
    failed: bool,
    cx: &RowCx,
) -> AnyElement {
    let from = endpoints.from.iter().enumerate().map(|(index, endpoint)| {
        div()
            .flex()
            .min_w_0()
            .items_center()
            .child(handoff_endpoint(
                ElementId::Name(format!("handoff-from-{index}").into()),
                endpoint,
                cx,
            ))
            .when(index + 1 < endpoints.from.len(), |this| {
                this.child(div().ml(rems(0.125)).child(","))
            })
    });
    let detail = div()
        .flex()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .justify_center()
        .gap(rems(0.375))
        .children(from)
        .when(!endpoints.from.is_empty(), |this| {
            this.child(
                div()
                    .flex_shrink_0()
                    .child(Icon::new(IconName::ArrowRight).size(rems(0.75))),
            )
        })
        .child(handoff_endpoint(
            ElementId::Name("handoff-to".into()),
            &endpoints.to,
            cx,
        ))
        .into_any_element();
    system_divider(
        SystemDivider {
            detail_element: Some(detail),
            danger: failed,
            icon: Some(IconName::ArrowRightLeft),
            show_detail_separator: false,
            ..SystemDivider::new("Context handoff")
        },
        cx,
    )
}

// ── Events and subagents ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum EventTone {
    Muted,
    Warning,
    Danger,
    Success,
}

/// `v2EventPresentation`.
fn v2_event_presentation(item: &TurnItem) -> (String, Option<String>, EventTone, IconName) {
    match &item.payload {
        TurnItemPayload::Error { failure, .. } => {
            let presentation = provider_error_presentation(item);
            let tone = if item.status == TurnItemStatus::Completed {
                EventTone::Success
            } else if item.status == TurnItemStatus::Running
                || failure.class == ProviderFailureClass::UsageLimit
            {
                EventTone::Warning
            } else {
                EventTone::Danger
            };
            let (label, detail) = presentation
                .map(|p| (p.label, Some(p.detail)))
                .unwrap_or_else(|| ("Provider error".into(), None));
            (label, detail, tone, IconName::CircleAlert)
        }
        TurnItemPayload::RunInterruptRequest { message } => (
            "Interrupt requested".into(),
            Some(message.clone()),
            EventTone::Warning,
            IconName::CircleAlert,
        ),
        TurnItemPayload::RunInterruptResult { message } => (
            "Run interrupted".into(),
            Some(message.clone()),
            EventTone::Danger,
            IconName::X,
        ),
        TurnItemPayload::TodoList {
            steps, explanation, ..
        } => {
            let steps = steps
                .iter()
                .map(|step| {
                    let status = match step.status {
                        super::timeline::turn_item::PlanStepStatus::Pending => "pending",
                        super::timeline::turn_item::PlanStepStatus::Running => "running",
                        super::timeline::turn_item::PlanStepStatus::Completed => "completed",
                    };
                    format!("{status}: {}", step.text)
                })
                .collect::<Vec<_>>()
                .join("\n");
            let detail = [explanation.clone().unwrap_or_default(), steps]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            (
                "Plan updated".into(),
                (!detail.is_empty()).then_some(detail),
                if item.status == TurnItemStatus::Failed {
                    EventTone::Danger
                } else {
                    EventTone::Success
                },
                IconName::Check,
            )
        }
        _ => (
            item.title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| item.item_type().as_str().replace('_', " ")),
            None,
            if item.status == TurnItemStatus::Failed {
                EventTone::Danger
            } else {
                EventTone::Muted
            },
            IconName::Wrench,
        ),
    }
}

/// `V2EventTimelineRow`.
pub fn event_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::Event {
        id,
        created_at,
        projected_item,
        subagents,
        resource_summary,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let item = &projected_item.item;
    let theme = &cx.theme;
    if item.item_type() == TurnItemType::Subagent {
        let members: Vec<&TurnItem> = subagents
            .as_ref()
            .map(|members| members.iter().map(|m| &m.item).collect())
            .unwrap_or_else(|| vec![item]);
        let inner = if members.len() > 1 {
            subagent_group(id, &members, cx)
        } else {
            subagent_link(item, cx)
        };
        return super::work_log::work_log_block(
            super::work_log::WorkLogLayout::Standalone,
            row.continues_work_log,
            inner,
        )
        .into_any_element();
    }
    // `V2LifecycleRow`.
    match &item.payload {
        TurnItemPayload::RunInterruptRequest { message } => {
            return div()
                .flex()
                .justify_end()
                .px(rems(0.25))
                .py(rems(0.25))
                .child(
                    div()
                        .flex()
                        .max_w(relative(0.8))
                        .items_center()
                        .gap(rems(0.5))
                        .text_size(TEXT_XS.size)
                        .text_color(theme.destructive)
                        .child(div().font_family(theme.fonts.mono.clone()).child("■"))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child("Interrupt requested"),
                        )
                        .child(div().opacity(0.5).child("·"))
                        .child(div().font_weight(FontWeight::MEDIUM).child(message.clone()))
                        .child(
                            div()
                                .text_size(TEXT_3XS.size)
                                .text_color(theme.muted_foreground)
                                .child(super::timestamp_format::format_short_timestamp(
                                    created_at,
                                    cx.timestamp_format,
                                )),
                        ),
                )
                .into_any_element();
        }
        TurnItemPayload::RunInterruptResult { message } => {
            return timeline_system_divider(
                "Run interrupted",
                Some(message),
                true,
                Some(IconName::X),
                cx,
            );
        }
        TurnItemPayload::Compaction {
            summary,
            before_token_count,
            after_token_count,
        } => {
            let token_detail =
                (before_token_count.is_some() || after_token_count.is_some()).then(|| {
                    format!(
                        "{} → {} tokens",
                        before_token_count.map_or("?".into(), |v| v.to_string()),
                        after_token_count.map_or("?".into(), |v| v.to_string())
                    )
                });
            let label = match item.status {
                TurnItemStatus::Failed => "Context compaction failed",
                TurnItemStatus::Cancelled | TurnItemStatus::Interrupted => {
                    "Context compaction stopped"
                }
                status if status.is_live() => "Compacting context",
                _ => "Context compacted",
            };
            return timeline_system_divider(
                label,
                summary.as_deref().or(token_detail.as_deref()),
                false,
                Some(IconName::Minus),
                cx,
            );
        }
        TurnItemPayload::Handoff {
            to_model,
            summary,
            endpoints,
        } => {
            if let Some(endpoints) = endpoints {
                return handoff_row(endpoints, item.status == TurnItemStatus::Failed, cx);
            }
            let detail = summary
                .clone()
                .or_else(|| to_model.clone().map(|m| format!("→ {m}")));
            return timeline_system_divider(
                "Context handoff",
                detail.as_deref(),
                item.status == TurnItemStatus::Failed,
                Some(IconName::ArrowRightLeft),
                cx,
            );
        }
        TurnItemPayload::Fork {
            target_thread_id,
            source_thread_id,
        } => {
            let from_run = source_thread_id.is_some();
            let related = source_thread_id
                .clone()
                .unwrap_or_else(|| target_thread_id.clone());
            let source = cx.source.clone();
            return system_divider(
                SystemDivider {
                    icon: Some(IconName::GitFork),
                    action: Some((
                        if from_run {
                            "Open source conversation"
                        } else {
                            "Open fork"
                        }
                        .to_owned(),
                        Rc::new(move |cx| {
                            if let Some(source) = &source {
                                source.open_thread(&related, cx);
                            }
                        }),
                    )),
                    ..SystemDivider::new(if from_run {
                        "Forked from conversation"
                    } else {
                        "Conversation fork"
                    })
                },
                cx,
            );
        }
        TurnItemPayload::ThreadCreated {
            target_thread_id, ..
        } => {
            let title = item.title.clone();
            let source = cx.source.clone();
            let target = target_thread_id.clone();
            let open: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
                if let Some(source) = &source {
                    source.open_thread(&target, cx);
                }
            });
            if *resource_summary {
                // The resource card: icon tile, title, "Open chat".
                let open_chat = open.clone();
                return div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.75))
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .p(rems(0.75))
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .size(rems(2.25))
                            .items_center()
                            .justify_center()
                            .rounded(RADIUS_LG)
                            .bg(with_alpha(theme.muted, 0.6))
                            .child(
                                Icon::new(IconName::MessageSquare)
                                    .size(rems(1.))
                                    .color(theme.muted_foreground),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child(title.clone().unwrap_or_else(|| "Created thread".to_owned())),
                    )
                    .child(
                        Button::new(("open-created-thread", cx.order as usize))
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Xs)
                            .label("Open chat")
                            .on_click(move |_, _, cx| open_chat(cx)),
                    )
                    .into_any_element();
            }
            let open_chat = open.clone();
            return WorkLogLine {
                icon: Some(icon_box(WorkEntryIconName::T3Code, theme.icon_muted)),
                label: div()
                    .child(format!(
                        "Created thread{}",
                        title.map(|t| format!(" · {t}")).unwrap_or_default()
                    ))
                    .into_any_element(),
                trailing: vec![
                    // `InlineButton`: `font-medium text-foreground`, underlined on hover.
                    div()
                        .id(("open-created-thread", cx.order as usize))
                        .flex_shrink_0()
                        .cursor_pointer()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .hover(|style| style.underline())
                        .child("Open chat")
                        .on_click(move |_, _, cx| open_chat(cx))
                        .into_any_element(),
                ],
                wrap_label: false,
            }
            .into_any_element();
        }
        _ => {}
    }
    let (label, detail, tone, icon) = v2_event_presentation(item);
    let (border, bg, icon_color): (Hsla, Hsla, Hsla) = match tone {
        EventTone::Warning => (
            with_alpha(theme.warning, 0.25).into(),
            with_alpha(theme.warning, 0.05).into(),
            theme.warning,
        ),
        EventTone::Danger => (
            with_alpha(theme.destructive, 0.25).into(),
            with_alpha(theme.destructive, 0.05).into(),
            theme.destructive,
        ),
        EventTone::Success => (
            with_alpha(theme.success, 0.2).into(),
            with_alpha(theme.success, 0.05).into(),
            theme.success,
        ),
        EventTone::Muted => (
            with_alpha(theme.border, 0.6).into(),
            with_alpha(theme.card, 0.3).into(),
            theme.muted_foreground,
        ),
    };
    let status_pill = (item.status != TurnItemStatus::Completed).then(|| {
        let failed = item.status == TurnItemStatus::Failed;
        div()
            .flex_shrink_0()
            .rounded_full()
            .border_1()
            .px(rems(0.375))
            .py(rems(0.125))
            .font_family(theme.fonts.mono.clone())
            .text_size(TEXT_3XS.size)
            .line_height(TEXT_3XS.line_height)
            .border_color(if failed {
                with_alpha(theme.destructive, 0.4)
            } else {
                with_alpha(theme.border, 0.7)
            })
            .text_color(if failed {
                theme.destructive
            } else {
                theme.muted_foreground
            })
            .child(item.status.as_str())
    });
    if item.item_type() == TurnItemType::Error {
        // `<details>` disclosure.
        let open = cx.ui.open_events.contains(id);
        let event_id = id.clone();
        let row_cx = cx.clone();
        return div()
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(border)
            .bg(bg)
            .child(
                div()
                    .id(ElementId::Name(format!("event-summary-{id}").into()))
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.5))
                    .px(rems(0.625))
                    .py(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .cursor_pointer()
                    .child(Icon::new(icon).size(rems(0.875)).color(icon_color))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(with_alpha(theme.foreground, 0.9))
                            .child(label),
                    )
                    .children(status_pill)
                    .when_some(detail.clone(), |this, detail| {
                        this.child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_color(with_alpha(theme.muted_foreground, 0.65))
                                .child(detail),
                        )
                    })
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(rems(0.75))
                        .color(with_alpha(theme.muted_foreground, 0.6).into()),
                    )
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation()
                    })
                    .on_click(move |_, _, cx| {
                        let id = event_id.clone();
                        row_cx.update_ui(cx, |ui| RowCx::toggle(&mut ui.open_events, &id));
                    }),
            )
            .when(open, |this| {
                this.child(
                    div()
                        .border_t_1()
                        .border_color(with_alpha(theme.border, 0.45))
                        .px(rems(0.75))
                        .py(rems(0.5))
                        .pl(rems(2.))
                        .when_some(detail, |this, detail| {
                            this.child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        ChatMarkdown::new(("event-md", cx.order), detail)
                                            .line_breaks(true),
                                    ),
                            )
                        }),
                )
            })
            .into_any_element();
    }
    div()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(border)
        .bg(bg)
        .px(rems(0.75))
        .py(rems(0.5))
        .child(
            div()
                .flex()
                .items_start()
                .gap(rems(0.625))
                .child(
                    div()
                        .mt(rems(0.125))
                        .child(Icon::new(icon).size(rems(0.875)).color(icon_color)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_x(rems(0.5))
                                .gap_y(rems(0.25))
                                .child(
                                    div()
                                        .text_size(TEXT_XS.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(with_alpha(theme.foreground, 0.9))
                                        .child(label),
                                )
                                .children(status_pill),
                        )
                        .when_some(detail, |this, detail| {
                            this.child(
                                div()
                                    .mt(rems(0.25))
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        ChatMarkdown::new(("event-md", cx.order), detail)
                                            .line_breaks(true),
                                    ),
                            )
                        }),
                ),
        )
        .into_any_element()
}

/// `STATUS_VISUALS`.
fn subagent_status_visual(status: TurnItemStatus, theme: &AppTheme) -> (Hsla, &'static str) {
    match status {
        TurnItemStatus::Pending => (theme.info, "Queued"),
        TurnItemStatus::Running => (theme.info, "Running"),
        TurnItemStatus::Waiting => (theme.info, "Waiting"),
        TurnItemStatus::Idle => (
            with_alpha(theme.muted_foreground, 0.5).into(),
            "Idle · resumable",
        ),
        TurnItemStatus::Completed => (theme.success, "Completed"),
        TurnItemStatus::Failed => (theme.destructive, "Failed"),
        TurnItemStatus::Cancelled | TurnItemStatus::Interrupted => {
            (with_alpha(theme.muted_foreground, 0.6).into(), "Stopped")
        }
    }
}

/// `SubagentAvatar`.
fn subagent_avatar(status: Option<TurnItemStatus>, theme: &AppTheme) -> AnyElement {
    div()
        .relative()
        .flex()
        .flex_shrink_0()
        .size(rems(1.5))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(with_alpha(theme.border, 0.7))
        .bg(theme.muted)
        .child(
            Icon::new(IconName::Bot)
                .size(rems(0.875))
                .color(theme.muted_foreground),
        )
        .when_some(status, |this, status| {
            this.child(
                div()
                    .absolute()
                    .right(px(-1.))
                    .bottom(px(-1.))
                    .size(rems(0.5))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.background)
                    .bg(subagent_status_visual(status, theme).0),
            )
        })
        .into_any_element()
}

/// `plainDetail`.
fn plain_detail(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    // `[label](href)` → label.
    while let Some(c) = chars.next() {
        if c == '[' {
            let label: String = chars.by_ref().take_while(|&ch| ch != ']').collect();
            if chars.peek() == Some(&'(') {
                for ch in chars.by_ref() {
                    if ch == ')' {
                        break;
                    }
                }
            }
            out.push_str(&label);
        } else if c != '`' {
            out.push(c);
        }
    }
    let lines: Vec<String> = out
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .unwrap_or(line)
                .to_string()
        })
        .collect();
    lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn subagent_elapsed(item: &TurnItem) -> String {
    let timing = SubagentTiming {
        status: item.status,
        started_at: item.started_at.as_deref(),
        completed_at: item.completed_at.as_deref(),
    };
    derive_subagent_elapsed_ms(timing, Utc::now().timestamp_millis() as f64)
        .map(|ms| format_elapsed_seconds(ms / 1000.))
        .unwrap_or_default()
}

/// `formatElapsedSeconds` (AgentElapsed.tsx).
pub fn format_elapsed_seconds(total: f64) -> String {
    let seconds = total.max(0.).floor() as u64;
    let minutes = seconds / 60;
    if minutes == 0 {
        return format!("{seconds}s");
    }
    let hours = minutes / 60;
    if hours == 0 {
        return format!("{minutes}m {:02}s", seconds % 60);
    }
    format!("{hours}h {:02}m", minutes % 60)
}

fn runtime_status(status: TurnItemStatus) -> RuntimeSubagentStatus {
    match status {
        TurnItemStatus::Idle => RuntimeSubagentStatus::Idle,
        TurnItemStatus::Pending => RuntimeSubagentStatus::Pending,
        TurnItemStatus::Running => RuntimeSubagentStatus::Running,
        TurnItemStatus::Waiting => RuntimeSubagentStatus::Waiting,
        TurnItemStatus::Completed => RuntimeSubagentStatus::Completed,
        TurnItemStatus::Failed => RuntimeSubagentStatus::Failed,
        TurnItemStatus::Cancelled => RuntimeSubagentStatus::Cancelled,
        TurnItemStatus::Interrupted => RuntimeSubagentStatus::Interrupted,
    }
}

/// `SubagentTimelineLink`.
fn subagent_link(item: &TurnItem, cx: &RowCx) -> AnyElement {
    let theme = &cx.theme;
    let TurnItemPayload::Subagent {
        progress,
        result,
        child_thread_id,
        ..
    } = &item.payload
    else {
        return div().into_any_element();
    };
    let status = item.status;
    let (_, status_label) = subagent_status_visual(status, theme);
    let settled = matches!(
        status,
        TurnItemStatus::Completed
            | TurnItemStatus::Failed
            | TurnItemStatus::Cancelled
            | TurnItemStatus::Interrupted
    );
    let result = result.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let progress = progress.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let raw = if settled {
        result.or(progress)
    } else {
        progress.or(result)
    };
    let detail = raw
        .filter(|raw| {
            !raw.to_ascii_lowercase()
                .starts_with("child task ended with status")
        })
        .map(plain_detail)
        .filter(|d| !d.is_empty());
    let failed = status == TurnItemStatus::Failed;
    let title = item.title.clone().unwrap_or_else(|| "Subagent".into());
    let source = cx.source.clone();
    let thread = child_thread_id.clone();
    let link = div()
        .id(ElementId::Name(format!("subagent-{}", item.id).into()))
        .group("subagent")
        .flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(rems(0.625))
        .rounded(RADIUS_MD)
        .px(rems(0.5))
        .py(rems(0.375))
        .when(thread.is_some(), |this| {
            this.cursor_pointer()
                .hover(|s| s.bg(with_alpha(theme.accent, 0.2)))
        })
        .child(subagent_avatar(Some(status), theme))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(rems(0.5))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(title),
                        )
                        .when(
                            detail.is_some() && status != TurnItemStatus::Completed,
                            |this| {
                                this.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(TEXT_3XS.size)
                                        .text_color(if failed {
                                            theme.destructive
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(status_label),
                                )
                            },
                        ),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(TEXT_2XS.size)
                        .line_height(relative(LEADING_RELAXED))
                        .text_color(if failed {
                            theme.destructive
                        } else {
                            theme.muted_foreground
                        })
                        .child(match detail.as_deref() {
                            // `MiddleTruncate` for a path-like detail (a slash, no spaces).
                            Some(path) if path.contains('/') && !path.contains(' ') => {
                                crate::ui::middle_truncate::MiddleTruncate::new(path.to_owned())
                                    .into_any_element()
                            }
                            Some(detail) => {
                                div().truncate().child(detail.to_owned()).into_any_element()
                            }
                            None => div().child(status_label.to_string()).into_any_element(),
                        }),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(subagent_elapsed(item)),
        )
        .when(thread.is_some(), |this| {
            this.child(
                Icon::new(IconName::ChevronRight)
                    .size(rems(0.875))
                    .color(with_alpha(theme.muted_foreground, 0.6).into()),
            )
        })
        .on_click(move |_, _, cx| {
            if let (Some(source), Some(thread)) = (source.as_ref(), thread.as_ref()) {
                source.open_thread(thread, cx);
            }
        });
    // The hover card (`ThreadHoverCardPopup` + `SubagentTooltipContent`).
    let preview = subagent_detail_preview(
        runtime_status(status),
        match &item.payload {
            TurnItemPayload::Subagent { result, .. } => result.as_deref(),
            _ => None,
        },
        match &item.payload {
            TurnItemPayload::Subagent { progress, .. } => progress.as_deref(),
            _ => None,
        },
    );
    let tooltip = SubagentTooltipContent::new(
        item.title.clone().unwrap_or_else(|| "Subagent".into()),
        turn_status_to_contract(status),
    )
    .elapsed(Some(AgentElapsed {
        status: runtime_status(status),
        started_at: item.started_at.clone(),
        completed_at: item.completed_at.clone(),
        compact: false,
    }))
    .preview(preview);
    with_subagent_tooltip(
        ElementId::Name(format!("subagent-tip-{}", item.id).into()),
        tooltip,
        link,
    )
    .w_full()
    .into_any_element()
}

fn turn_status_to_contract(
    status: TurnItemStatus,
) -> crate::state::contracts::OrchestrationV2TurnItemStatus {
    use crate::state::contracts::OrchestrationV2TurnItemStatus as C;
    match status {
        TurnItemStatus::Idle => C::Idle,
        TurnItemStatus::Pending => C::Pending,
        TurnItemStatus::Running => C::Running,
        TurnItemStatus::Waiting => C::Waiting,
        TurnItemStatus::Completed => C::Completed,
        TurnItemStatus::Failed => C::Failed,
        TurnItemStatus::Cancelled => C::Cancelled,
        TurnItemStatus::Interrupted => C::Interrupted,
    }
}

/// `V2SubagentGroup`.
fn subagent_group(id: &str, members: &[&TurnItem], cx: &RowCx) -> AnyElement {
    let theme = &cx.theme;
    let group_key = format!("subagent-group:{id}");
    let expanded = cx.ui.expanded_entries.contains(&group_key);
    let active = members.iter().any(|m| m.status.is_live());
    let failed = members.iter().any(|m| m.status == TurnItemStatus::Failed);
    let running = members.iter().filter(|m| m.status.is_live()).count();
    let done = members
        .iter()
        .filter(|m| m.status == TurnItemStatus::Completed)
        .count();
    let label = format!(
        "{} {}",
        members.len(),
        if members.len() == 1 {
            "subagent"
        } else {
            "subagents"
        }
    );
    let mut summary_parts = Vec::new();
    if running > 0 {
        summary_parts.push(format!("{running} running"));
    }
    if done > 0 {
        summary_parts.push(format!("{done} completed"));
    }
    let failed_count = members
        .iter()
        .filter(|m| m.status == TurnItemStatus::Failed)
        .count();
    if failed_count > 0 {
        summary_parts.push(format!("{failed_count} failed"));
    }
    let status_summary = summary_parts.join(" · ");
    let row_cx = cx.clone();
    let key = group_key.clone();
    let mut avatars = div().flex().flex_shrink_0().items_center();
    for (ix, member) in members.iter().take(3).enumerate() {
        avatars = avatars.child(div().when(ix > 0, |this| this.ml(rems(-0.375))).child(
            subagent_avatar((members.len() == 1).then_some(member.status), theme),
        ));
    }
    if members.len() > 3 {
        avatars = avatars.child(
            div()
                .ml(rems(-0.375))
                .flex()
                .size(rems(1.5))
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.muted)
                .text_size(TEXT_3XS.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(format!("+{}", members.len() - 3)),
        );
    }
    div()
        .child(
            div()
                .id(ElementId::Name(format!("subagent-group-{id}").into()))
                .flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(rems(0.75))
                .py(rems(0.5))
                .cursor_pointer()
                .text_color(if expanded || active {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(!(expanded || active), |this| {
                    this.opacity(0.55).hover(|s| s.opacity(1.))
                })
                .child(avatars)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .text_size(TEXT_XS.size)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(label),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_3XS.size)
                                .text_color(if active {
                                    theme.info
                                } else if failed {
                                    theme.destructive
                                } else {
                                    theme.muted_foreground
                                })
                                .child(status_summary),
                        ),
                )
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(rems(0.875))
                    .color(theme.muted_foreground),
                )
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation()
                })
                .on_click(move |_, _, cx| {
                    let key = key.clone();
                    row_cx.update_ui(cx, |ui| RowCx::toggle(&mut ui.expanded_entries, &key));
                }),
        )
        .when(expanded, |this| {
            let mut list = div()
                .mt(rems(0.25))
                .mb(rems(0.25))
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(with_alpha(theme.border, 0.6))
                .bg(with_alpha(theme.card, 0.3))
                .p(rems(0.25));
            for member in members {
                list = list.child(subagent_link(member, cx));
            }
            this.child(list)
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_timer_formats() {
        let start = "2026-01-01T00:00:00.000Z";
        let end = DateTime::parse_from_rfc3339("2026-01-01T00:00:42.000Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_working_timer(start, end).as_deref(), Some("42s"));
    }

    #[test]
    fn elapsed_seconds() {
        assert_eq!(format_elapsed_seconds(45.), "45s");
        assert_eq!(format_elapsed_seconds(125.), "2m 05s");
        assert_eq!(format_elapsed_seconds(3725.), "1h 02m");
    }

    #[test]
    fn plain_detail_strips_markdown() {
        assert_eq!(
            plain_detail("- see [docs](https://x) and `code`"),
            "see docs and code"
        );
    }

    #[test]
    fn thought_preview_flattens() {
        assert_eq!(
            thought_preview("**Plan**\n\n- a\n- b", "Thought"),
            "Plan a b"
        );
        assert_eq!(thought_preview("", "Thinking"), "Thinking");
    }
}
