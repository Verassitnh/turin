//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`deriveComposerSendState`).

use turin_contracts::ThreadId;

use turin_shared::composer_context_references::replace_composer_context_references;

/// Upstream `TerminalContextDraft`. Sendability only reads `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalContextDraft {
    pub id: String,
    pub thread_id: ThreadId,
    pub created_at: String,
    pub terminal_id: String,
    pub terminal_label: String,
    pub line_start: u64,
    pub line_end: u64,
    pub text: String,
}

impl TerminalContextDraft {
    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            id: "ctx".to_owned(),
            thread_id: ThreadId::new("thread-1").expect("thread id"),
            created_at: "2026-03-29T00:00:00.000Z".to_owned(),
            terminal_id: "default".to_owned(),
            terminal_label: "Terminal 1".to_owned(),
            line_start: 1,
            line_end: 1,
            text: text.into(),
        }
    }
}

/// `normalizeTerminalContextText`: CRLF becomes LF, then only leading and trailing newlines go.
fn normalize_terminal_context_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_matches('\n').to_owned()
}

fn has_terminal_context_text(context: &TerminalContextDraft) -> bool {
    !normalize_terminal_context_text(&context.text).is_empty()
}

fn filter_terminal_contexts_with_text(
    contexts: &[TerminalContextDraft],
) -> Vec<TerminalContextDraft> {
    contexts
        .iter()
        .filter(|context| has_terminal_context_text(context))
        .cloned()
        .collect()
}

fn strip_inline_context_references(prompt: &str) -> String {
    replace_composer_context_references(prompt, |_| String::new())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerSendState {
    pub trimmed_prompt: String,
    pub sendable_terminal_contexts: Vec<TerminalContextDraft>,
    pub expired_terminal_context_count: usize,
    pub has_sendable_content: bool,
}

pub fn derive_composer_send_state(
    prompt: &str,
    image_count: usize,
    terminal_contexts: &[TerminalContextDraft],
    element_context_count: usize,
) -> ComposerSendState {
    let trimmed_prompt = strip_inline_context_references(prompt).trim().to_owned();
    let sendable_terminal_contexts = filter_terminal_contexts_with_text(terminal_contexts);
    let expired_terminal_context_count = terminal_contexts
        .len()
        .saturating_sub(sendable_terminal_contexts.len());
    let has_sendable_content = !trimmed_prompt.is_empty()
        || image_count > 0
        || !sendable_terminal_contexts.is_empty()
        || element_context_count > 0;
    ComposerSendState {
        trimmed_prompt,
        sendable_terminal_contexts,
        expired_terminal_context_count,
        has_sendable_content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treats_expired_terminal_pills_as_non_sendable_content() {
        let state = derive_composer_send_state(
            "[Terminal 1](t3-context://v1/terminal/ctx-expired)",
            0,
            &[TerminalContextDraft::with_text("")],
            0,
        );
        assert_eq!(state.trimmed_prompt, "");
        assert!(state.sendable_terminal_contexts.is_empty());
        assert_eq!(state.expired_terminal_context_count, 1);
        assert!(!state.has_sendable_content);
    }

    #[test]
    fn keeps_text_sendable_while_excluding_expired_terminal_pills() {
        let state = derive_composer_send_state(
            "yoo [Terminal 1](t3-context://v1/terminal/ctx-expired) waddup",
            0,
            &[TerminalContextDraft::with_text("")],
            0,
        );
        assert_eq!(state.trimmed_prompt, "yoo  waddup");
        assert_eq!(state.expired_terminal_context_count, 1);
        assert!(state.has_sendable_content);
    }

    #[test]
    fn element_context_count_gates_sendable_content() {
        assert!(derive_composer_send_state("", 0, &[], 1).has_sendable_content);
        assert!(!derive_composer_send_state("", 0, &[], 0).has_sendable_content);
    }

    #[test]
    fn whitespace_inside_newlines_is_still_terminal_text() {
        let state =
            derive_composer_send_state("", 0, &[TerminalContextDraft::with_text("\n  \n")], 0);
        assert_eq!(state.sendable_terminal_contexts.len(), 1);
        assert_eq!(state.expired_terminal_context_count, 0);
        assert!(state.has_sendable_content);
        let blank_lines =
            derive_composer_send_state("", 0, &[TerminalContextDraft::with_text("\n\n")], 0);
        assert!(blank_lines.sendable_terminal_contexts.is_empty());
        assert!(!blank_lines.has_sendable_content);
    }
}
