//! Port of upstream/apps/web/src/components/chat/useComposerTriggerState.test.tsx
//!
//! The probe component and `act` are gone: the state is a plain struct, so
//! each step calls the method directly.

use turin_shared::composer_trigger::{ComposerTriggerKind, detect_composer_trigger};

use super::*;

const COMMAND: &str = "pnpm install -g @openai/codex@latest";
const INITIAL_PROMPT: &str = "pnpm install -g @openai";

fn detect(text: &str, cursor: usize) -> Option<ComposerTrigger> {
    detect_composer_trigger(text, cursor as f64, None)
}

fn composer() -> ComposerTriggerState {
    ComposerTriggerState::new(|| detect(INITIAL_PROMPT, INITIAL_PROMPT.len()))
}

fn update_prompt(composer: &mut ComposerTriggerState, text: &str, cursor: usize) {
    composer.set_trigger(detect(text, cursor));
}

fn update_prompt_at_end(composer: &mut ComposerTriggerState, text: &str) {
    update_prompt(composer, text, text.len());
}

fn dismiss_current(composer: &mut ComposerTriggerState) {
    let current = composer.trigger().cloned();
    composer.dismiss_trigger(current);
}

#[test]
fn closes_suggestions_and_rejects_keyboard_selection_before_the_next_render() {
    let mut composer = composer();
    let candidate = detect(INITIAL_PROMPT, INITIAL_PROMPT.len());
    assert_eq!(composer.trigger(), candidate.as_ref());

    composer.dismiss_trigger(candidate.clone());
    assert_eq!(composer.resolve_trigger(candidate), None);
    assert_eq!(composer.trigger(), None);
}

#[test]
fn stays_dismissed_while_typing_a_scoped_package_including_its_second_at() {
    let mut composer = composer();
    dismiss_current(&mut composer);

    for cursor in INITIAL_PROMPT.len()..=COMMAND.len() {
        let text = &COMMAND[..cursor];
        update_prompt_at_end(&mut composer, text);
        assert_eq!(composer.trigger(), None);
        assert_eq!(composer.resolve_trigger(detect(text, cursor)), None);
    }
}

#[test]
fn stays_dismissed_while_deleting_characters_or_moving_within_the_same_word() {
    let mut composer = composer();
    dismiss_current(&mut composer);

    let at = INITIAL_PROMPT.find('@').unwrap();
    for cursor in (at + 1..INITIAL_PROMPT.len()).rev() {
        update_prompt(&mut composer, INITIAL_PROMPT, cursor);
        assert_eq!(composer.trigger(), None);
        update_prompt_at_end(&mut composer, &INITIAL_PROMPT[..cursor]);
        assert_eq!(composer.trigger(), None);
    }
}

#[test]
fn opens_suggestions_for_a_new_at_word_after_a_space() {
    let mut composer = composer();
    dismiss_current(&mut composer);
    update_prompt_at_end(&mut composer, &format!("{COMMAND} "));
    update_prompt_at_end(&mut composer, &format!("{COMMAND} @src"));

    let trigger = composer.trigger().unwrap();
    assert_eq!(trigger.query, "src");
    assert_eq!(trigger.range_start, COMMAND.len() + 1);
}

#[test]
fn opens_a_different_token_when_the_caret_moves_directly_to_it() {
    let mut composer = composer();
    dismiss_current(&mut composer);
    update_prompt_at_end(&mut composer, &format!("{COMMAND} @src"));

    assert_eq!(composer.trigger().unwrap().query, "src");
}

#[test]
fn can_reopen_after_the_caret_leaves_the_dismissed_word() {
    let mut composer = composer();
    dismiss_current(&mut composer);
    update_prompt(&mut composer, INITIAL_PROMPT, 0);
    update_prompt_at_end(&mut composer, INITIAL_PROMPT);

    assert_eq!(composer.trigger().unwrap().query, "openai");
}

#[test]
fn can_reopen_at_the_same_position_after_deleting_and_retyping_at() {
    let mut composer = composer();
    dismiss_current(&mut composer);
    let prefix = &INITIAL_PROMPT[..INITIAL_PROMPT.find('@').unwrap()];
    update_prompt_at_end(&mut composer, prefix);
    update_prompt_at_end(&mut composer, &format!("{prefix}@"));

    assert_eq!(composer.trigger().unwrap().query, "");
}

#[test]
fn clears_dismissal_when_switching_drafts_or_pending_questions() {
    let mut composer = composer();
    dismiss_current(&mut composer);
    let candidate = detect(INITIAL_PROMPT, INITIAL_PROMPT.len());
    composer.reset_trigger(candidate.clone());

    assert_eq!(composer.trigger(), candidate.as_ref());
    assert_eq!(composer.resolve_trigger(candidate.clone()), candidate);
}

#[test]
fn also_dismisses_slash_skill_and_pull_request_suggestions() {
    for text in ["/plan", "$skill", "#123"] {
        let mut composer = composer();
        update_prompt_at_end(&mut composer, text);
        dismiss_current(&mut composer);
        update_prompt_at_end(&mut composer, &format!("{text}x"));

        assert_eq!(composer.trigger(), None, "{text}");
        update_prompt_at_end(&mut composer, "@src");
        assert_eq!(composer.trigger().unwrap().kind, ComposerTriggerKind::Path);
    }
}
