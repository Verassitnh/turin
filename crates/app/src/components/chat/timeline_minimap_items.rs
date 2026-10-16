//! Port of upstream/apps/web/src/components/chat/timelineMinimapItems.ts

use super::timeline::messages_timeline_logic::{
    MessagesTimelineRow, MessagesTimelineRowContent, MessagesTimelineRowKind,
};
use super::timeline::types::MessageRole;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineMinimapItem {
    pub id: String,
    pub row_index: usize,
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
}

/// Keep full source text untouched until a minimap preview is opened.
pub fn derive_timeline_minimap_items(rows: &[MessagesTimelineRow]) -> Vec<TimelineMinimapItem> {
    let mut items = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.kind() != MessagesTimelineRowKind::Message {
            continue;
        }
        let MessagesTimelineRowContent::Message { message, .. } = &row.content else {
            continue;
        };
        if message.role != MessageRole::User {
            continue;
        }
        items.push(TimelineMinimapItem {
            id: row.content.id().to_owned(),
            row_index: index,
            user_text: Some(message.text.clone()),
            assistant_text: resolve_final_assistant_text_for_turn(rows, index),
        });
    }
    items
}

fn resolve_final_assistant_text_for_turn(
    rows: &[MessagesTimelineRow],
    user_row_index: usize,
) -> Option<String> {
    let mut final_assistant_text = None;
    for row in rows.iter().skip(user_row_index + 1) {
        if row.kind() != MessagesTimelineRowKind::Message {
            continue;
        }
        let MessagesTimelineRowContent::Message { message, .. } = &row.content else {
            continue;
        };
        if message.role == MessageRole::User {
            break;
        }
        if message.role == MessageRole::Assistant {
            final_assistant_text = Some(message.text.clone());
        }
    }
    final_assistant_text
}

fn compact_minimap_preview(text: Option<&str>) -> Option<String> {
    let compact = text
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    if compact.is_empty() {
        None
    } else {
        Some(compact)
    }
}

pub fn resolve_timeline_minimap_preview(
    item: Option<&TimelineMinimapItem>,
) -> Option<TimelineMinimapItem> {
    let item = item?;
    Some(TimelineMinimapItem {
        id: item.id.clone(),
        row_index: item.row_index,
        user_text: compact_minimap_preview(item.user_text.as_deref()),
        assistant_text: compact_minimap_preview(item.assistant_text.as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::super::timeline::types::ChatMessage;
    use super::*;

    fn rows(entries: &[(&str, &str)]) -> Vec<MessagesTimelineRow> {
        entries
            .iter()
            .enumerate()
            .map(|(index, (role, text))| {
                let message = ChatMessage {
                    id: format!("message-{index}"),
                    role: if *role == "user" {
                        MessageRole::User
                    } else {
                        MessageRole::Assistant
                    },
                    text: text.to_string(),
                    attachments: vec![],
                    streaming: false,
                    run_id: None,
                    created_by: None,
                    creation_source: None,
                    scheduled_task_id: None,
                    sender_thread_id: None,
                    created_at: format!("2026-01-01T00:00:{index:02}.000Z"),
                    updated_at: format!("2026-01-01T00:00:{index:02}.000Z"),
                    input_intent: None,
                };
                MessagesTimelineRow {
                    content: MessagesTimelineRowContent::Message {
                        id: message.id.clone(),
                        created_at: message.created_at.clone(),
                        message,
                        projected_item: None,
                        duration_start: format!("2026-01-01T00:00:{index:02}.000Z"),
                        show_assistant_meta: false,
                        show_assistant_copy_button: false,
                        assistant_copy_streaming: false,
                        assistant_turn_diff_summary: None,
                        revert_turn_count: None,
                    },
                    continues_work_log: false,
                }
            })
            .collect()
    }

    #[test]
    fn previews_last_assistant_before_next_prompt() {
        let source = rows(&[
            ("user", "  Inspect\n this  "),
            ("assistant", "Working"),
            ("assistant", " Done\t now "),
            ("user", "Next"),
            ("assistant", "Second answer"),
        ]);
        let items = derive_timeline_minimap_items(&source);
        assert_eq!(items.len(), 2);
        assert_eq!(
            resolve_timeline_minimap_preview(items.first()),
            Some(TimelineMinimapItem {
                id: items[0].id.clone(),
                row_index: items[0].row_index,
                user_text: Some("Inspect this".into()),
                assistant_text: Some("Done now".into()),
            })
        );
        assert_eq!(source[items[0].row_index].content.id(), items[0].id);
        assert_eq!(
            resolve_timeline_minimap_preview(Some(&items[1]))
                .unwrap()
                .assistant_text,
            Some("Second answer".into())
        );
        assert_eq!(items[0].assistant_text.as_deref(), Some(" Done\t now "));
    }

    #[test]
    fn handles_unanswered_prompt_and_empty_responses() {
        let items = derive_timeline_minimap_items(&rows(&[
            ("user", "First"),
            ("assistant", " \n\t"),
            ("user", "Next"),
        ]));
        assert_eq!(
            items
                .iter()
                .map(|item| resolve_timeline_minimap_preview(Some(item))
                    .unwrap()
                    .assistant_text)
                .collect::<Vec<_>>(),
            vec![None, None]
        );
        assert!(resolve_timeline_minimap_preview(None).is_none());
    }

    #[test]
    fn shows_fresh_streaming_text_without_changing_jump_target() {
        let first =
            derive_timeline_minimap_items(&rows(&[("user", "Explain"), ("assistant", "First")]))[0]
                .clone();
        let next = TimelineMinimapItem {
            assistant_text: Some("First\n second".into()),
            ..first.clone()
        };
        assert_eq!(
            resolve_timeline_minimap_preview(Some(&next)),
            Some(TimelineMinimapItem {
                assistant_text: Some("First second".into()),
                ..first.clone()
            })
        );
        assert_eq!(
            resolve_timeline_minimap_preview(Some(&first))
                .unwrap()
                .assistant_text,
            Some("First".into())
        );
    }
}
