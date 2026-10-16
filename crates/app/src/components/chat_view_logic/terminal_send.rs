//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`deriveComposerSendState`).

use turin_shared::composer_context_references::replace_composer_context_references;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalContextDraft {
    pub text: String,
}

fn has_terminal_context_text(context: &TerminalContextDraft) -> bool {
    !context.text.replace("\r\n", "\n").trim().is_empty()
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
            &[TerminalContextDraft {
                text: String::new(),
            }],
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
            &[TerminalContextDraft {
                text: String::new(),
            }],
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
}
