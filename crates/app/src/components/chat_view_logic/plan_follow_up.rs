//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`restorePlanFollowUpComposer`).

use turin_contracts::composer_context::ThreadContextRecord;

use crate::composer::js_string::utf16_len;
use crate::composer::logic::collapse_expanded_composer_cursor;
use crate::diff::review_comment_context::ReviewCommentContext;

use super::terminal_send::TerminalContextDraft;

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickedElementStackFrame {
    pub function_name: Option<String>,
    pub file_name: Option<String>,
    pub line_number: Option<f64>,
    pub column_number: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickedElementPayload {
    pub page_url: String,
    pub page_title: Option<String>,
    pub tag_name: String,
    pub selector: Option<String>,
    pub html_preview: String,
    pub component_name: Option<String>,
    pub source: Option<PickedElementStackFrame>,
    pub stack: Vec<PickedElementStackFrame>,
    pub styles: String,
    pub picked_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationElementTarget {
    pub id: String,
    pub element: PickedElementPayload,
    pub rect: PreviewAnnotationRect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationRegionTarget {
    pub id: String,
    pub rect: PreviewAnnotationRect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationStrokeTarget {
    pub id: String,
    pub color: String,
    pub width: f64,
    pub points: Vec<PreviewAnnotationPoint>,
    pub bounds: PreviewAnnotationRect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewAnnotationStyleChange {
    pub target_id: String,
    pub selector: Option<String>,
    pub property: String,
    pub previous_value: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationScreenshot {
    pub data_url: String,
    pub width: f64,
    pub height: f64,
    pub crop_rect: PreviewAnnotationRect,
}

/// Upstream `PreviewAnnotationPayload`.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationPayload {
    pub id: String,
    pub page_url: String,
    pub page_title: Option<String>,
    pub comment: String,
    pub elements: Vec<PreviewAnnotationElementTarget>,
    pub regions: Vec<PreviewAnnotationRegionTarget>,
    pub strokes: Vec<PreviewAnnotationStrokeTarget>,
    pub style_changes: Vec<PreviewAnnotationStyleChange>,
    pub screenshot: Option<PreviewAnnotationScreenshot>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanFollowUpComposerSnapshot {
    pub prompt: String,
    pub terminal_contexts: Vec<TerminalContextDraft>,
    pub review_comments: Vec<ReviewCommentContext>,
    pub preview_annotations: Vec<PreviewAnnotationPayload>,
    pub thread_contexts: Vec<ThreadContextRecord>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanFollowUpComposerRestoreRecord {
    pub prompt: Option<String>,
    pub terminal_contexts: Option<Vec<TerminalContextDraft>>,
    pub review_comments: Option<Vec<ReviewCommentContext>>,
    pub preview_annotations: Option<Vec<PreviewAnnotationPayload>>,
    pub thread_contexts: Option<Vec<ThreadContextRecord>>,
    pub reset_cursor: Option<ResetCursorCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetCursorCall {
    pub cursor: usize,
    pub prompt: String,
    pub detect_trigger: bool,
}

pub fn restore_plan_follow_up_composer(
    snapshot: &PlanFollowUpComposerSnapshot,
    record: &mut PlanFollowUpComposerRestoreRecord,
) {
    record.prompt = Some(snapshot.prompt.clone());
    record.terminal_contexts = Some(snapshot.terminal_contexts.clone());
    record.review_comments = Some(snapshot.review_comments.clone());
    record.preview_annotations = Some(snapshot.preview_annotations.clone());
    record.thread_contexts = Some(snapshot.thread_contexts.clone());
    record.reset_cursor = Some(ResetCursorCall {
        cursor: collapse_expanded_composer_cursor(
            &snapshot.prompt,
            utf16_len(&snapshot.prompt),
            false,
        ),
        prompt: snapshot.prompt.clone(),
        detect_trigger: true,
    });
}

#[cfg(test)]
mod tests {
    use super::super::terminal_send::TerminalContextDraft;
    use super::*;
    use crate::diff::review_comment_context::ReviewCommentContext;
    use serde_json::json;
    use turin_contracts::composer_context::ThreadContextRecord;

    fn thread_context() -> ThreadContextRecord {
        serde_json::from_value(json!({
            "version": 1,
            "kind": "thread",
            "contextId": "thread_reference-thread",
            "label": "Reference work",
            "environmentId": "env-answer",
            "threadId": "reference-thread",
            "title": "Reference work",
        }))
        .expect("thread context")
    }

    fn review_comment() -> ReviewCommentContext {
        ReviewCommentContext {
            id: "comment-1".to_owned(),
            section_id: "file:src/a.rs".to_owned(),
            section_title: "File comment".to_owned(),
            file_path: "src/a.rs".to_owned(),
            start_index: 0,
            end_index: 1,
            range_label: "L1 to L2".to_owned(),
            text: "fix this".to_owned(),
            diff: "line".to_owned(),
            fence_language: Some("rust".to_owned()),
            selection: None,
        }
    }

    fn annotation() -> PreviewAnnotationPayload {
        let rect = PreviewAnnotationRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        };
        let frame = PickedElementStackFrame {
            function_name: Some("Button".to_owned()),
            file_name: Some("App.tsx".to_owned()),
            line_number: Some(10.0),
            column_number: Some(2.0),
        };
        PreviewAnnotationPayload {
            id: "ann-1".to_owned(),
            page_url: "https://example.com".to_owned(),
            page_title: Some("Example".to_owned()),
            comment: "move this".to_owned(),
            elements: vec![PreviewAnnotationElementTarget {
                id: "el-1".to_owned(),
                element: PickedElementPayload {
                    page_url: "https://example.com".to_owned(),
                    page_title: Some("Example".to_owned()),
                    tag_name: "button".to_owned(),
                    selector: Some("button.primary".to_owned()),
                    html_preview: "<button>Go</button>".to_owned(),
                    component_name: Some("GoButton".to_owned()),
                    source: Some(frame.clone()),
                    stack: vec![frame],
                    styles: "color: red".to_owned(),
                    picked_at: "2026-03-29T00:00:00.000Z".to_owned(),
                },
                rect: rect.clone(),
            }],
            regions: vec![PreviewAnnotationRegionTarget {
                id: "region-1".to_owned(),
                rect: rect.clone(),
            }],
            strokes: vec![PreviewAnnotationStrokeTarget {
                id: "stroke-1".to_owned(),
                color: "#000".to_owned(),
                width: 2.0,
                points: vec![PreviewAnnotationPoint { x: 1.0, y: 2.0 }],
                bounds: rect.clone(),
            }],
            style_changes: vec![PreviewAnnotationStyleChange {
                target_id: "el-1".to_owned(),
                selector: Some("button.primary".to_owned()),
                property: "color".to_owned(),
                previous_value: "black".to_owned(),
                value: "red".to_owned(),
            }],
            screenshot: Some(PreviewAnnotationScreenshot {
                data_url: "data:image/png;base64,aa".to_owned(),
                width: 8.0,
                height: 9.0,
                crop_rect: rect,
            }),
            created_at: "2026-03-29T00:00:00.000Z".to_owned(),
        }
    }

    #[test]
    fn restores_every_field_cleared_before_a_failed_plan_follow_up_send() {
        let terminal = TerminalContextDraft::with_text("ls\n");
        let review = review_comment();
        let preview = annotation();
        let thread = thread_context();
        let snapshot = PlanFollowUpComposerSnapshot {
            prompt: "continue the plan".to_owned(),
            terminal_contexts: vec![terminal.clone()],
            review_comments: vec![review.clone()],
            preview_annotations: vec![preview.clone()],
            thread_contexts: vec![thread.clone()],
        };
        let mut record = PlanFollowUpComposerRestoreRecord::default();
        restore_plan_follow_up_composer(&snapshot, &mut record);
        assert_eq!(record.prompt.as_deref(), Some("continue the plan"));
        assert_eq!(record.terminal_contexts, Some(vec![terminal]));
        assert_eq!(record.review_comments, Some(vec![review]));
        assert_eq!(record.preview_annotations, Some(vec![preview]));
        assert_eq!(record.thread_contexts, Some(vec![thread]));
        assert_eq!(
            record.reset_cursor,
            Some(ResetCursorCall {
                cursor: utf16_len("continue the plan"),
                prompt: "continue the plan".to_owned(),
                detect_trigger: true,
            })
        );
    }

    #[test]
    fn collapses_an_expanded_mention_cursor() {
        let text = "what's in my @AGENTS.md fsfdas";
        let collapsed_after_mention = "what's in my ".len() + 2;
        let expanded_after_mention = "what's in my @AGENTS.md ".len();
        assert_eq!(
            collapse_expanded_composer_cursor(text, expanded_after_mention, false),
            collapsed_after_mention
        );

        let snapshot = PlanFollowUpComposerSnapshot {
            prompt: text.to_owned(),
            terminal_contexts: Vec::new(),
            review_comments: Vec::new(),
            preview_annotations: Vec::new(),
            thread_contexts: Vec::new(),
        };
        let mut record = PlanFollowUpComposerRestoreRecord::default();
        restore_plan_follow_up_composer(&snapshot, &mut record);
        let cursor = record.reset_cursor.expect("cursor").cursor;
        assert_eq!(
            cursor,
            collapse_expanded_composer_cursor(text, utf16_len(text), false)
        );
        assert_eq!(cursor, "what's in my ".len() + 1 + " fsfdas".len());
    }
}
