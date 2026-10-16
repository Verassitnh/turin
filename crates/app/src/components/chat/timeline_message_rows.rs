//! Port of the message rows in upstream/apps/web/src/components/chat/MessagesTimeline.tsx:
//! `UserTimelineRow`, `UserMessageIntentMarker`, `CollapsibleUserMessageBody`,
//! `RevertUserMessageButton`, `AssistantTimelineRow`, `AssistantMessageMeta`,
//! `AssistantMetaTimelineRow`, `AssistantCopyButton` and
//! `ProposedPlanTimelineRow` (with ProposedPlanCard.tsx).

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, linear_color_stop, linear_gradient, px, rems,
};

use super::changed_files_tree::ChangedFilesCard;
use super::markdown::ChatMarkdown;
use super::markdown::markdown_pipeline::should_preserve_assistant_line_breaks;
use super::message_copy_button::MessageCopyButton;
use super::messages_timeline::{RowCx, TimelineUiState};
use super::proposed_plan::{
    build_proposed_plan_markdown_filename, download_plan_as_text_file, downloads_dir,
    normalize_plan_markdown_for_export,
};
use super::timeline::messages_timeline_logic::{
    MessagesTimelineRow, MessagesTimelineRowContent, resolve_assistant_message_copy_state,
    should_collapse_user_message,
};
use super::timeline::turn_item::{ProjectedTurnItem, TurnItemStatus};
use super::timeline::types::{Actor, ChatMessage, CreationSource, InputIntent};
use super::timestamp_format::{format_chat_timestamp_tooltip, format_day_aware_timestamp};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{
    RADIUS_2XL, RADIUS_3XL, RADIUS_LG, TEXT_2XS, TEXT_3XS, TEXT_SM, TEXT_XS,
};
use crate::ui::{Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// `MESSAGE_HEADING_LEVEL`: message markdown headings nest under an h3.
const COLLAPSED_USER_MESSAGE_FADE_HEIGHT_REM: f32 = 1.75;

/// Hover-revealed timestamp with the long-form tooltip.
pub fn timestamp_label(
    id: impl Into<ElementId>,
    iso: &str,
    cx: &RowCx,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let tooltip: SharedString = format_chat_timestamp_tooltip(iso, cx.timestamp_format).into();
    div()
        .id(id)
        .flex_shrink_0()
        .whitespace_nowrap()
        .text_size(TEXT_XS.size)
        .line_height(TEXT_XS.line_height)
        .text_color(cx.theme.muted_foreground)
        .child(format_day_aware_timestamp(iso, cx.timestamp_format))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

/// `resolveUserMessagePresentation` attribution.
fn user_message_attribution(message: &ChatMessage) -> Option<&'static str> {
    if message.scheduled_task_id.is_some() {
        return Some("Sent by automation");
    }
    if message.created_by != Some(Actor::Agent) {
        return None;
    }
    // Restart continuations were sent as the agent before they became notices.
    Some(if message.creation_source == Some(CreationSource::Server) {
        "Sent by T3 Code"
    } else {
        "Sent by another agent"
    })
}

fn status_pill(status: &str, destructive: bool, cx: &RowCx) -> AnyElement {
    let theme = &cx.theme;
    div()
        .rounded_full()
        .border_1()
        .px(rems(0.375))
        .py(rems(0.125))
        .text_size(TEXT_3XS.size)
        .line_height(TEXT_3XS.line_height)
        .when(destructive, |this| {
            this.font_weight(FontWeight::MEDIUM)
                .border_color(with_alpha(theme.destructive, 0.25))
                .bg(with_alpha(theme.destructive, 0.08))
                .text_color(theme.destructive)
        })
        .when(!destructive, |this| {
            this.font_family(theme.fonts.mono.clone())
                .border_color(with_alpha(theme.border, 0.7))
                .text_color(theme.muted_foreground)
        })
        .child(status.to_string())
        .into_any_element()
}

/// `UserTimelineRow`.
pub fn user_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::Message {
        message,
        projected_item,
        revert_turn_count,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let theme = &cx.theme;
    let group: SharedString = format!("user-row-{}", message.id).into();
    let images: Vec<_> = message
        .attachments
        .iter()
        .filter(|a| a.is_image())
        .collect();
    let files: Vec<_> = message
        .attachments
        .iter()
        .filter(|a| !a.is_image())
        .collect();

    let mut bubble = div()
        .relative()
        .max_w(gpui_kit::relative(0.8))
        .min_w_0()
        .rounded(RADIUS_2XL)
        .bg(theme.message_surface)
        .p(rems(0.75))
        .text_color(theme.message_foreground);
    if !images.is_empty() {
        // `mb-2 grid max-w-[210px] grid-cols-2 gap-2`.
        let mut grid = div()
            .mb(rems(0.5))
            .flex()
            .flex_wrap()
            .gap(rems(0.5))
            .max_w(px(210.));
        for image in images {
            let frame = div()
                .w(px(101.))
                .h(px(76.))
                .overflow_hidden()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(with_alpha(theme.border, 0.8))
                .bg(with_alpha(theme.background, 0.7));
            grid = grid.child(match &image.preview_path {
                Some(path) => frame.child(
                    img(std::path::PathBuf::from(path))
                        .size_full()
                        .object_fit(gpui_kit::ObjectFit::Cover),
                ),
                None => frame
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(rems(0.5))
                    .text_size(TEXT_2XS.size)
                    .text_color(with_alpha(theme.muted_foreground, 0.7))
                    .child(image.name.clone()),
            });
        }
        bubble = bubble.child(grid);
    }
    if !files.is_empty() {
        let mut list = div().mb(rems(0.5)).flex().flex_col().gap(rems(0.25));
        for file in files {
            list = list.child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.5))
                    .py(rems(0.25))
                    .text_size(TEXT_SM.size)
                    .child(Icon::new(IconName::FileText).size(rems(1.)))
                    .child(div().min_w_0().flex_1().truncate().child(file.name.clone())),
            );
        }
        bubble = bubble.child(list);
    }
    bubble = bubble.child(collapsible_user_message_body(message, cx));

    let failed_status = projected_item.as_ref().and_then(|projected| {
        let status = projected.item.status;
        (!matches!(
            status,
            TurnItemStatus::Completed | TurnItemStatus::Pending | TurnItemStatus::Waiting
        ))
        .then(|| status.as_str())
    });

    let message_id = message.id.clone();
    let reverting_disabled = cx.snapshot.is_reverting_checkpoint || cx.snapshot.is_working;
    let source = cx.source.clone();
    let meta = div()
        .flex()
        .w_full()
        .max_w(gpui_kit::relative(0.8))
        .items_center()
        .justify_end()
        .pr(rems(0.25))
        .opacity(0.)
        .group_hover(group.clone(), |style| style.opacity(1.))
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(rems(0.5))
                .child(timestamp_label(
                    ("user-time", cx.order),
                    &message.created_at,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.125))
                        .when_some(*revert_turn_count, |this, turn_count| {
                            let source = source.clone();
                            let message_id = message_id.clone();
                            this.child(
                                Button::new(("revert", cx.order))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Xs)
                                    .disabled(reverting_disabled)
                                    .tooltip(Tooltip::new("Edit from here"))
                                    .child(Icon::new(IconName::Undo2).size(rems(0.75)))
                                    .on_click(move |_, _, cx| {
                                        if let Some(source) = source.as_ref() {
                                            source.revert_to_turn_count(
                                                turn_count,
                                                &message_id,
                                                cx,
                                            );
                                        }
                                    }),
                            )
                        })
                        .when(!message.text.is_empty(), |this| {
                            this.child(
                                MessageCopyButton::new(
                                    ("user-copy", cx.order),
                                    message.text.clone(),
                                )
                                .ghost(),
                            )
                        }),
                ),
        );

    div()
        .group(group)
        .flex()
        .flex_col()
        .items_end()
        .gap(rems(0.25))
        .when_some(user_message_attribution(message), |this, attribution| {
            this.child(
                div()
                    .mr(rems(0.25))
                    .text_size(TEXT_2XS.size)
                    .line_height(TEXT_2XS.line_height)
                    .text_color(with_alpha(theme.muted_foreground, 0.7))
                    .child(attribution),
            )
        })
        .when_some(
            message
                .input_intent
                .filter(|intent| *intent != InputIntent::TurnStart),
            |this, intent| this.child(user_message_intent_marker(intent, cx)),
        )
        .child(bubble)
        .when_some(failed_status, |this, status| {
            this.child(
                div()
                    .mr(rems(0.25))
                    .flex()
                    .items_center()
                    .gap(rems(0.375))
                    .child(status_pill(status, true, cx)),
            )
        })
        .child(meta)
        .into_any_element()
}

/// `UserMessageIntentMarker`.
fn user_message_intent_marker(intent: InputIntent, cx: &RowCx) -> AnyElement {
    let (label, icon, tooltip) = match intent {
        InputIntent::QueuedTurn => ("Queued", None, "Queued behind the active turn"),
        InputIntent::PromotedQueuedToSteer => (
            "Steer",
            Some(IconName::Redo2),
            "Originally queued, then promoted to steer the active turn",
        ),
        _ => ("Steer", Some(IconName::Redo2), "Steered the active turn"),
    };
    div()
        .id(("intent", cx.order))
        .mr(rems(0.25))
        .flex()
        .items_center()
        .justify_end()
        .gap(rems(0.25))
        .text_size(TEXT_XS.size)
        .line_height(rems(0.75))
        .text_color(cx.theme.muted_foreground)
        .when_some(icon, |this, icon| {
            this.child(Icon::new(icon).size(rems(0.75)))
        })
        .child(label)
        .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
        .into_any_element()
}

/// `CollapsibleUserMessageBody` + `UserMessageBody`.
fn collapsible_user_message_body(message: &ChatMessage, cx: &RowCx) -> AnyElement {
    let text = message.text.clone();
    if text.trim().is_empty() {
        return div().into_any_element();
    }
    let can_collapse = should_collapse_user_message(&text);
    let expanded = cx.ui.expanded_user_messages.contains(&message.id);
    let is_collapsed = can_collapse && !expanded;
    let surface = cx.theme.message_surface;
    let markdown = ChatMarkdown::new(("user-md", cx.order), text)
        .line_breaks(true)
        .full_foreground(true)
        .document_order(cx.order + 1);
    let body = div()
        .relative()
        .when(is_collapsed, |this| this.max_h(rems(11.)).overflow_hidden())
        .child(markdown)
        .when(is_collapsed, |this| {
            // The mask fade (`black calc(100% - 1.75rem) → transparent`),
            // painted as a gradient into the bubble surface.
            this.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(rems(COLLAPSED_USER_MESSAGE_FADE_HEIGHT_REM))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(with_alpha(surface, 0.), 0.),
                        linear_color_stop(surface, 1.),
                    )),
            )
        });
    let message_id = message.id.clone();
    let row_cx = cx.clone();
    div()
        .child(body)
        .when(can_collapse, |this| {
            this.child(
                div()
                    .mt(rems(0.375))
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(
                        div().mr_auto().ml(rems(-0.25)).child(
                            Button::new(("user-expand", cx.order))
                                .variant(ButtonVariant::GhostMuted)
                                .size(ButtonSize::Xs)
                                .label(if expanded {
                                    "Show less"
                                } else {
                                    "Show full message"
                                })
                                .on_click(move |_, _, cx| {
                                    let id = message_id.clone();
                                    row_cx.update_ui(cx, |ui: &mut TimelineUiState| {
                                        RowCx::toggle(&mut ui.expanded_user_messages, &id)
                                    });
                                }),
                        ),
                    ),
            )
        })
        .into_any_element()
}

/// `AssistantTimelineRow`.
pub fn assistant_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::Message {
        message,
        projected_item,
        show_assistant_meta,
        show_assistant_copy_button,
        assistant_copy_streaming,
        assistant_turn_diff_summary,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let text = if message.text.is_empty() && !message.streaming {
        "(empty response)".to_string()
    } else {
        message.text.clone()
    };
    let line_breaks = should_preserve_assistant_line_breaks(&text);
    let group: SharedString = format!("assistant-row-{}", message.id).into();
    let source = cx.source.clone();
    div()
        .group(group.clone())
        .relative()
        .min_w_0()
        .px(rems(0.25))
        .py(rems(0.125))
        .child(
            ChatMarkdown::new(("assistant-md", cx.order), text)
                .line_breaks(line_breaks)
                .streaming(message.streaming)
                .document_order(cx.order + 1),
        )
        .when_some(
            assistant_turn_diff_summary
                .as_ref()
                .filter(|summary| !summary.files.is_empty()),
            |this, summary| {
                this.child(ChangedFilesCard {
                    id: ElementId::Name(format!("changed-files-{}", summary.run_id).into()),
                    run_id: summary.run_id.clone().into(),
                    files: Arc::new(summary.files.clone()),
                    on_open_turn_diff: Rc::new(move |run_id, path, _, cx| {
                        if let Some(source) = source.as_ref() {
                            source.open_turn_diff(run_id, path, cx);
                        }
                    }),
                })
            },
        )
        .when(*show_assistant_meta, |this| {
            this.child(div().mt(rems(0.375)).child(assistant_message_meta(
                projected_item.as_ref(),
                message,
                *show_assistant_copy_button,
                *assistant_copy_streaming,
                false,
                &group,
                cx,
            )))
        })
        .into_any_element()
}

/// `AssistantMetaTimelineRow`.
pub fn assistant_meta_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::AssistantMeta {
        projected_item,
        message,
        show_assistant_copy_button,
        assistant_copy_streaming,
        ..
    } = &row.content
    else {
        return div().into_any_element();
    };
    let group: SharedString = format!("assistant-meta-{}", message.id).into();
    div()
        .group(group.clone())
        .px(rems(0.25))
        .child(div().mt(rems(0.125)).child(assistant_message_meta(
            projected_item.as_ref(),
            message,
            *show_assistant_copy_button,
            *assistant_copy_streaming,
            true,
            &group,
            cx,
        )))
        .into_any_element()
}

/// `AssistantMessageMeta`.
fn assistant_message_meta(
    projected_item: Option<&ProjectedTurnItem>,
    message: &ChatMessage,
    show_copy_button: bool,
    copy_streaming: bool,
    always_visible: bool,
    group: &SharedString,
    cx: &RowCx,
) -> AnyElement {
    let copy =
        resolve_assistant_message_copy_state(Some(&message.text), show_copy_button, copy_streaming);
    div()
        .flex()
        .items_center()
        .gap(rems(0.5))
        .text_size(TEXT_XS.size)
        .when(!always_visible, |this| {
            this.opacity(0.)
                .group_hover(group.clone(), |style| style.opacity(1.))
        })
        .when_some(
            projected_item.filter(|projected| projected.item.status != TurnItemStatus::Completed),
            |this, projected| this.child(status_pill(projected.item.status.as_str(), false, cx)),
        )
        .when(copy.visible, |this| {
            this.child(
                MessageCopyButton::new(("assistant-copy", cx.order), copy.text.unwrap_or_default())
                    .ghost(),
            )
        })
        .when(!message.streaming, |this| {
            this.child(timestamp_label(
                ("assistant-time", cx.order),
                &message.updated_at,
                cx,
            ))
        })
        .into_any_element()
}

/// `ProposedPlanTimelineRow` → `ProposedPlanCard`.
pub fn proposed_plan_row(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    let MessagesTimelineRowContent::ProposedPlan { proposed_plan, .. } = &row.content else {
        return div().into_any_element();
    };
    div()
        .min_w_0()
        .px(rems(0.25))
        .py(rems(0.125))
        .child(proposed_plan_card(
            &proposed_plan.id,
            &proposed_plan.plan_markdown,
            cx,
        ))
        .into_any_element()
}

/// `proposedPlanTitle` (upstream/packages/shared/src/proposedPlanText.ts).
pub fn proposed_plan_title(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            return None;
        }
        let rest = line.trim_start_matches(' ');
        let hashes = rest.len() - rest.trim_start_matches('#').len();
        if !(1..=6).contains(&hashes) {
            return None;
        }
        let after = &rest[hashes..];
        if !after.starts_with([' ', '\t']) {
            return None;
        }
        let title = after.trim();
        (!title.is_empty()).then(|| title.to_string())
    })
}

fn is_heading_line(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = line.trim_start_matches(' ');
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = &rest[hashes..];
    after.starts_with([' ', '\t']).then(|| after.trim())
}

/// `stripDisplayedPlanMarkdown`: drop the title heading and a leading "Summary" heading.
pub fn strip_displayed_plan_markdown(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.trim_end().lines().collect();
    let mut source: Vec<&str> = if lines
        .first()
        .is_some_and(|line| is_heading_line(line).is_some())
    {
        lines[1..].to_vec()
    } else {
        lines
    };
    while source.first().is_some_and(|line| line.trim().is_empty()) {
        source.remove(0);
    }
    if source
        .first()
        .and_then(|line| is_heading_line(line))
        .is_some_and(|title| title.eq_ignore_ascii_case("summary"))
    {
        source.remove(0);
        while source.first().is_some_and(|line| line.trim().is_empty()) {
            source.remove(0);
        }
    }
    source.join("\n")
}

/// `buildCollapsedProposedPlanPreviewMarkdown`.
pub fn build_collapsed_plan_preview(markdown: &str, max_lines: usize) -> String {
    let stripped = strip_displayed_plan_markdown(markdown);
    let mut preview: Vec<&str> = Vec::new();
    let mut visible = 0;
    let mut has_more = false;
    for line in stripped.trim_end().lines().map(str::trim_end) {
        let is_visible = !line.trim().is_empty();
        if is_visible && visible >= max_lines {
            has_more = true;
            break;
        }
        preview.push(line);
        if is_visible {
            visible += 1;
        }
    }
    while preview.last().is_some_and(|line| line.trim().is_empty()) {
        preview.pop();
    }
    if preview.is_empty() {
        return proposed_plan_title(markdown).unwrap_or_else(|| "Plan preview unavailable.".into());
    }
    let mut out = preview.join("\n");
    if has_more {
        out.push_str("\n\n...");
    }
    out
}

/// `ProposedPlanCard`: the header's "Plan actions" menu (copy, download, save to workspace).
/// The "Plan actions" `<Menu>`.
fn plan_actions_menu(markdown: &str, cx: &RowCx) -> AnyElement {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::menu::PopupMenuItem;
    use gpui_kit::component::notification::Notification;

    let contents = normalize_plan_markdown_for_export(markdown);
    let filename = build_proposed_plan_markdown_filename(markdown);
    let workspace_root = cx.workspace_root().map(str::to_owned);
    let source = cx.source.clone();
    let can_save = source
        .as_ref()
        .is_some_and(|source| source.can_write_workspace_files())
        && workspace_root.is_some();
    crate::pull_request::dropdown::dropdown(
        ("plan-actions", cx.order as usize),
        gpui_kit::Anchor::TopRight,
        Button::new(("plan-actions-trigger", cx.order as usize))
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::IconXs)
            .tooltip(Tooltip::new("Plan actions"))
            .child(Icon::new(IconName::Ellipsis).size(rems(1.))),
        move |menu, _, _| {
            let copy_contents = contents.clone();
            let download_contents = contents.clone();
            let download_filename = filename.clone();
            let save_contents = contents.clone();
            let save_filename = filename.clone();
            let save_root = workspace_root.clone();
            let save_source = source.clone();
            menu.item(
                PopupMenuItem::new("Copy to clipboard").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                        copy_contents.clone(),
                    ));
                }),
            )
            .item(
                PopupMenuItem::new("Download as markdown").on_click(move |_, window, cx| {
                    let note = match downloads_dir()
                        .ok_or_else(|| "The Downloads folder is unavailable.".to_owned())
                        .and_then(|dir| {
                            download_plan_as_text_file(&dir, &download_filename, &download_contents)
                                .map_err(|error| error.to_string())
                        }) {
                        Ok(path) => Notification::success(path.display().to_string())
                            .title("Plan downloaded"),
                        Err(error) => Notification::error(error).title("Could not download plan"),
                    };
                    window.push_notification(note, cx);
                }),
            )
            .item(
                PopupMenuItem::new("Save to workspace")
                    .disabled(!can_save)
                    .on_click(move |_, window, cx| {
                        if let (Some(root), Some(source)) = (&save_root, &save_source) {
                            open_save_plan_dialog(
                                root.clone(),
                                save_filename.clone(),
                                save_contents.clone(),
                                source.clone(),
                                window,
                                cx,
                            );
                        }
                    }),
            )
        },
    )
    .into_any_element()
}

/// The "Save plan to workspace" dialog.
fn open_save_plan_dialog(
    workspace_root: String,
    default_path: String,
    contents: String,
    source: Arc<dyn super::timeline::source::TimelineSource>,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::AppContext as _;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::input::{Input, InputState};
    use gpui_kit::component::notification::Notification;

    let placeholder = default_path.clone();
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(default_path)
            .placeholder(placeholder)
    });
    let focus_once = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        use gpui_kit::Focusable as _;
        if focus_once.replace(false) {
            input.focus_handle(cx).focus(window, cx);
        }
        let input_for_ok = input.clone();
        let source = source.clone();
        let root = workspace_root.clone();
        let contents = contents.clone();
        dialog
            .title("Save plan to workspace")
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .child(format!("Enter a path relative to {workspace_root}.")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.375))
                            .child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Workspace path"),
                            )
                            .child(Input::new(&input)),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let relative_path = input_for_ok.read(cx).value().trim().to_owned();
                if relative_path.is_empty() {
                    window.push_notification(
                        Notification::warning("Enter a workspace path")
                            .title("Enter a workspace path"),
                        cx,
                    );
                    return false;
                }
                let window_handle = window.window_handle();
                source.write_workspace_file(
                    &root,
                    &relative_path,
                    &contents,
                    Box::new(move |result, cx| {
                        window_handle
                            .update(cx, |_, window, cx| {
                                let note = match result {
                                    Ok(saved) => Notification::success(saved)
                                        .title("Plan saved to workspace"),
                                    Err(error) => {
                                        Notification::error(error).title("Could not save plan")
                                    }
                                };
                                window.push_notification(note, cx);
                            })
                            .ok();
                    }),
                    cx,
                );
                true
            })
    });
}

fn proposed_plan_card(plan_id: &str, markdown: &str, cx: &RowCx) -> AnyElement {
    let theme = &cx.theme;
    let title = proposed_plan_title(markdown).unwrap_or_else(|| "Proposed plan".into());
    let line_count = markdown.split('\n').count();
    let can_collapse = markdown.len() > 900 || line_count > 20;
    let expanded = cx.ui.expanded_plans.contains(plan_id);
    let is_collapsed = can_collapse && !expanded;
    let text = if is_collapsed {
        build_collapsed_plan_preview(markdown, 10)
    } else {
        strip_displayed_plan_markdown(markdown)
    };
    let card_bg = with_alpha(theme.card, 0.7);
    let plan_key = plan_id.to_string();
    let row_cx = cx.clone();
    div()
        .rounded(RADIUS_3XL)
        .border_1()
        .border_color(with_alpha(theme.border, 0.8))
        .bg(card_bg)
        .p(rems(1.25))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(rems(0.75))
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(rems(0.5))
                        .child(Badge::label("Plan").variant(BadgeVariant::Secondary))
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_SM.size)
                                .line_height(TEXT_SM.line_height)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(title),
                        ),
                )
                .child(plan_actions_menu(markdown, cx)),
        )
        .child(
            div()
                .mt(rems(1.))
                .relative()
                .when(is_collapsed, |this| this.max_h(rems(26.)).overflow_hidden())
                .child(ChatMarkdown::new(("plan-md", cx.order), text).document_order(cx.order + 1))
                .when(is_collapsed, |this| {
                    // `bg-linear-to-t from-card/95 via-card/80 to-transparent`.
                    this.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(rems(6.))
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(with_alpha(theme.card, 0.), 0.),
                                linear_color_stop(with_alpha(theme.card, 0.95), 1.),
                            )),
                    )
                }),
        )
        .when(can_collapse, |this| {
            this.child(
                div().mt(rems(1.)).flex().justify_center().child(
                    Button::new(("plan-expand", cx.order))
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label(if expanded {
                            "Collapse plan"
                        } else {
                            "Expand plan"
                        })
                        .on_click(move |_, _, cx| {
                            let key = plan_key.clone();
                            row_cx.update_ui(cx, |ui: &mut TimelineUiState| {
                                RowCx::toggle(&mut ui.expanded_plans, &key)
                            });
                        }),
                ),
            )
        })
        .into_any_element()
}

#[allow(dead_code)]
fn _window_marker(_: &mut Window, _: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_title_and_strip() {
        let plan = "# Port the view\n\n## Summary\n\nDo it.\n\n## Steps\n1. a";
        assert_eq!(proposed_plan_title(plan), Some("Port the view".into()));
        assert_eq!(
            strip_displayed_plan_markdown(plan),
            "Do it.\n\n## Steps\n1. a"
        );
    }

    #[test]
    fn collapsed_preview_caps_visible_lines() {
        let plan = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = build_collapsed_plan_preview(&plan, 10);
        assert!(preview.ends_with("\n\n..."));
        assert_eq!(
            preview.lines().filter(|l| l.starts_with("line")).count(),
            10
        );
    }
}
