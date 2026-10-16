//! Upstream ships no test file for useComposerTypingGuard.ts; these cover the
//! hook's own behavior over `composerTypingGuard.ts`.

use super::*;

fn held(hook: &ComposerTypingGuardHook) -> Vec<&str> {
    hook.held_request_ids().iter().map(String::as_str).collect()
}

#[test]
fn holds_a_request_that_arrives_while_typing_in_the_focused_composer() {
    let mut hook = ComposerTypingGuardHook::new("thread-1", &[], 1_000.0);
    hook.on_focus();
    hook.on_draft_change(1_000.0);
    hook.sync("thread-1", &["req-1"], 1_200.0);
    assert_eq!(held(&hook), ["req-1"]);
    assert_eq!(hook.idle_delay_ms(1_200.0), Some(1_300.0));
}

#[test]
fn does_not_rerun_requests_when_scope_and_ids_are_unchanged() {
    let mut hook = ComposerTypingGuardHook::new("thread-1", &["req-1"], 0.0);
    hook.on_focus();
    hook.on_draft_change(10.0);
    hook.sync("thread-1", &["req-1"], 20.0);
    assert!(held(&hook).is_empty());
    assert_eq!(hook.idle_delay_ms(20.0), None);
}

#[test]
fn a_new_scope_forgets_seen_requests_but_keeps_focus() {
    let mut hook = ComposerTypingGuardHook::new("thread-1", &["req-1"], 0.0);
    hook.on_focus();
    hook.on_draft_change(100.0);
    hook.sync("thread-2", &["req-1"], 200.0);
    // The new scope has not seen req-1, and the carried focus is still set,
    // but typing time was reset with the fresh state.
    assert!(held(&hook).is_empty());
    hook.on_draft_change(210.0);
    hook.sync("thread-2", &["req-1", "req-2"], 220.0);
    assert_eq!(held(&hook), ["req-2"]);
}

#[test]
fn send_blur_and_idle_release_held_requests() {
    for release in [
        ComposerTypingGuardHook::on_send,
        ComposerTypingGuardHook::on_blur,
        ComposerTypingGuardHook::on_idle,
    ] {
        let mut hook = ComposerTypingGuardHook::new("t", &[], 0.0);
        hook.on_focus();
        hook.on_draft_change(0.0);
        hook.sync("t", &["req-1"], 1.0);
        assert_eq!(held(&hook), ["req-1"]);
        release(&mut hook);
        assert!(held(&hook).is_empty());
    }
}

#[test]
fn idle_delay_never_goes_negative() {
    let mut hook = ComposerTypingGuardHook::new("t", &[], 0.0);
    hook.on_focus();
    hook.on_draft_change(0.0);
    hook.sync("t", &["req-1"], 1.0);
    assert_eq!(hook.idle_delay_ms(10_000.0), Some(0.0));
}
