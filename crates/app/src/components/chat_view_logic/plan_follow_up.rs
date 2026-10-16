//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`restorePlanFollowUpComposer`).

/// Upstream `collapseExpandedComposerCursor` for plain prompts without rich segments.
fn collapse_expanded_composer_cursor(text: &str, cursor_input: usize) -> usize {
    cursor_input.min(text.len())
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanFollowUpComposerSnapshot {
    pub prompt: String,
    pub terminal_context_count: usize,
    pub review_comment_count: usize,
    pub preview_annotation_count: usize,
    pub thread_context_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanFollowUpComposerRestoreRecord {
    pub prompt: Option<String>,
    pub terminal_context_count: Option<usize>,
    pub review_comment_count: Option<usize>,
    pub preview_annotation_count: Option<usize>,
    pub thread_context_count: Option<usize>,
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
    record.terminal_context_count = Some(snapshot.terminal_context_count);
    record.review_comment_count = Some(snapshot.review_comment_count);
    record.preview_annotation_count = Some(snapshot.preview_annotation_count);
    record.thread_context_count = Some(snapshot.thread_context_count);
    record.reset_cursor = Some(ResetCursorCall {
        cursor: collapse_expanded_composer_cursor(&snapshot.prompt, snapshot.prompt.len()),
        prompt: snapshot.prompt.clone(),
        detect_trigger: true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_every_field_cleared_before_a_failed_plan_follow_up_send() {
        let snapshot = PlanFollowUpComposerSnapshot {
            prompt: "continue the plan".to_owned(),
            terminal_context_count: 2,
            review_comment_count: 1,
            preview_annotation_count: 3,
            thread_context_count: 4,
        };
        let mut record = PlanFollowUpComposerRestoreRecord::default();
        restore_plan_follow_up_composer(&snapshot, &mut record);
        assert_eq!(record.prompt.as_deref(), Some("continue the plan"));
        assert_eq!(record.terminal_context_count, Some(2));
        assert_eq!(record.review_comment_count, Some(1));
        assert_eq!(record.preview_annotation_count, Some(3));
        assert_eq!(record.thread_context_count, Some(4));
        assert_eq!(
            record.reset_cursor,
            Some(ResetCursorCall {
                cursor: "continue the plan".len(),
                prompt: "continue the plan".to_owned(),
                detect_trigger: true,
            })
        );
    }
}
