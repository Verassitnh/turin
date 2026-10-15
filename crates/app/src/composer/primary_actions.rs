//! Port of the decision logic in upstream/apps/web/src/components/chat/ComposerPrimaryActions.tsx
//!
//! [`resolve_primary_actions`] picks which primary control the footer shows
//! (pending-question submit, plan follow-up Refine/Implement, Stop, or the
//! send button) with the exact labels, status text and tooltips upstream
//! renders. The view paints the result.

use crate::composer::dispatch::{
    ActiveTurnComposerAction, ComposerDispatchMode, ResolveComposerDispatchModeInput,
    alternate_composer_dispatch_action, resolve_composer_dispatch_mode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingActionState {
    pub question_index: usize,
    pub is_last_question: bool,
    pub can_advance: bool,
    pub is_responding: bool,
    pub is_complete: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerPrimaryActionsInput {
    pub compact: bool,
    pub can_operate_thread: bool,
    pub pending_action: Option<PendingActionState>,
    /// The turn is running: sending steers or queues instead of starting a turn.
    pub is_running: bool,
    /// Stop can reach a run, including one still preparing or starting.
    pub can_interrupt: bool,
    pub follow_up_behavior: ActiveTurnComposerAction,
    pub alternate_shortcut_label: Option<String>,
    pub show_plan_follow_up_prompt: bool,
    pub prompt_has_text: bool,
    pub is_send_busy: bool,
    pub send_disabled_reason: Option<String>,
    pub is_connecting: bool,
    pub is_environment_unavailable: bool,
    pub is_preparing_worktree: bool,
    pub has_sendable_content: bool,
    pub can_resume: bool,
    pub is_editing_queued_message: bool,
    /// Tokens a stale session would re-read.
    pub compact_before_send_tokens: Option<f64>,
    pub keep_full_history: bool,
    /// The alternate modifier (Ctrl/⌘) is held.
    pub alternate_modifier: bool,
}

impl Default for ComposerPrimaryActionsInput {
    fn default() -> Self {
        Self {
            compact: false,
            can_operate_thread: true,
            pending_action: None,
            is_running: false,
            can_interrupt: false,
            follow_up_behavior: ActiveTurnComposerAction::Steer,
            alternate_shortcut_label: None,
            show_plan_follow_up_prompt: false,
            prompt_has_text: false,
            is_send_busy: false,
            send_disabled_reason: None,
            is_connecting: false,
            is_environment_unavailable: false,
            is_preparing_worktree: false,
            has_sendable_content: false,
            can_resume: false,
            is_editing_queued_message: false,
            compact_before_send_tokens: None,
            keep_full_history: false,
            alternate_modifier: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendIcon {
    Spinner,
    Resume,
    Check,
    /// Running turn: `ListPlus` when queuing, `CornerUpRight` when steering.
    Queue,
    Steer,
    ArrowUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactChip {
    pub tokens: String,
    pub keep_full_history: bool,
    pub aria_label: String,
    pub tooltip: String,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendButton {
    pub label: &'static str,
    pub aria_label: String,
    pub tooltip: String,
    pub icon: SendIcon,
    pub disabled: bool,
    /// `type="button"` rather than submit.
    pub is_resume: bool,
    pub compact_chip: Option<CompactChip>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrimaryActions {
    Pending {
        show_stop: bool,
        /// Compact: an icon button; otherwise a "Previous" button.
        show_previous: bool,
        previous_disabled: bool,
        submit_label: &'static str,
        submit_disabled: bool,
    },
    Refine {
        label: &'static str,
        disabled: bool,
    },
    Implement {
        label: &'static str,
        disabled: bool,
    },
    Stop,
    Send(SendButton),
}

/// `formatPendingPrimaryActionLabel`.
pub fn format_pending_primary_action_label(
    compact: bool,
    is_last_question: bool,
    is_responding: bool,
    question_index: usize,
) -> &'static str {
    if is_responding {
        return "Submitting...";
    }
    if compact {
        return if is_last_question { "Submit" } else { "Next" };
    }
    if !is_last_question {
        return "Next question";
    }
    if question_index > 0 {
        "Submit answers"
    } else {
        "Submit answer"
    }
}

/// `formatContextWindowTokens`.
pub fn format_context_window_tokens(value: Option<f64>) -> String {
    let Some(value) = value.filter(|v| v.is_finite()) else {
        return "0".to_owned();
    };
    // JS `toFixed(1)` rounds ties up, where Rust's formatter rounds them to even.
    let fixed = |value: f64| format!("{:.1}", (value * 10.0).round() / 10.0);
    let trim = |text: String| text.strip_suffix(".0").map(str::to_owned).unwrap_or(text);
    if value < 1_000.0 {
        return format!("{}", value.round());
    }
    if value < 10_000.0 {
        return format!("{}k", trim(fixed(value / 1_000.0)));
    }
    // Anything from 999,500 rounds to 1000k, so it belongs to the next unit.
    if value < 999_500.0 {
        return format!("{}k", (value / 1_000.0).round());
    }
    format!("{}m", trim(fixed(value / 1_000_000.0)))
}

fn action_name(action: ActiveTurnComposerAction) -> &'static str {
    match action {
        ActiveTurnComposerAction::Queue => "queue",
        ActiveTurnComposerAction::Steer => "steer",
        ActiveTurnComposerAction::Restart => "restart",
    }
}

/// Whether the send button queues (rather than steers) while a turn runs.
pub fn is_queuing(input: &ComposerPrimaryActionsInput) -> bool {
    !input.is_editing_queued_message
        && resolve_composer_dispatch_mode(ResolveComposerDispatchModeInput {
            running: input.is_running,
            alternate_modifier: input.alternate_modifier,
            active_turn_default: Some(input.follow_up_behavior),
        }) == ComposerDispatchMode::Queue
}

/// What the footer's primary control is and says.
pub fn resolve_primary_actions(input: &ComposerPrimaryActionsInput) -> PrimaryActions {
    let is_send_disabled = !input.can_operate_thread || input.send_disabled_reason.is_some();

    if let Some(pending) = input.pending_action {
        return PrimaryActions::Pending {
            show_stop: input.can_interrupt,
            show_previous: pending.question_index > 0,
            previous_disabled: pending.is_responding,
            submit_label: format_pending_primary_action_label(
                input.compact,
                pending.is_last_question,
                pending.is_responding,
                pending.question_index,
            ),
            submit_disabled: !input.can_operate_thread
                || input.is_environment_unavailable
                || pending.is_responding
                || if pending.is_last_question {
                    !pending.is_complete
                } else {
                    !pending.can_advance
                },
        };
    }

    let blocked = input.is_send_busy
        || is_send_disabled
        || input.is_connecting
        || input.is_environment_unavailable;
    if input.show_plan_follow_up_prompt && (input.prompt_has_text || !input.can_resume) {
        let sending = input.is_connecting || input.is_send_busy;
        return if input.prompt_has_text {
            PrimaryActions::Refine {
                label: if sending { "Sending..." } else { "Refine" },
                disabled: blocked,
            }
        } else {
            PrimaryActions::Implement {
                label: if sending { "Sending..." } else { "Implement" },
                disabled: blocked,
            }
        };
    }

    if input.can_interrupt && !input.has_sendable_content && !input.is_editing_queued_message {
        return PrimaryActions::Stop;
    }

    let show_resume =
        input.can_resume && !input.has_sendable_content && !input.is_editing_queued_message;
    let compact_tokens = (input.compact_before_send_tokens.is_some()
        && !show_resume
        && !input.is_editing_queued_message)
        .then(|| format_context_window_tokens(input.compact_before_send_tokens));
    let compacts_before_send = compact_tokens.is_some() && !input.keep_full_history;
    let queuing = is_queuing(input);

    let submit_label = if show_resume {
        "Resume thread"
    } else if input.is_editing_queued_message {
        "Update queued message"
    } else if compacts_before_send {
        "Compact and send"
    } else if queuing {
        "Queue message"
    } else if input.is_running {
        "Steer message"
    } else {
        "Submit message"
    };
    let submit_status: Option<String> = if input.is_environment_unavailable {
        Some("Environment disconnected".to_owned())
    } else if let Some(reason) = &input.send_disabled_reason {
        Some(reason.clone())
    } else if input.is_connecting {
        Some("Connecting".to_owned())
    } else if input.is_preparing_worktree {
        Some("Preparing worktree".to_owned())
    } else if input.is_send_busy {
        Some(
            if input.is_editing_queued_message {
                "Updating queued message"
            } else {
                "Submitting message"
            }
            .to_owned(),
        )
    } else {
        None
    };
    let alternate_action = alternate_composer_dispatch_action(Some(input.follow_up_behavior));
    let submit_tooltip = submit_status.clone().unwrap_or_else(|| {
        if compacts_before_send {
            format!(
                "Summarize {} tokens of history, then send",
                compact_tokens.as_deref().unwrap_or("0")
            )
        } else if input.is_running && !input.is_editing_queued_message {
            format!(
                "Click to {}, Ctrl/⌘-click{} to {}",
                action_name(input.follow_up_behavior),
                input
                    .alternate_shortcut_label
                    .as_ref()
                    .map_or(String::new(), |label| format!(" or {label}")),
                action_name(alternate_action)
            )
        } else {
            submit_label.to_owned()
        }
    });
    let icon = if input.is_connecting || input.is_send_busy {
        SendIcon::Spinner
    } else if show_resume {
        SendIcon::Resume
    } else if input.is_editing_queued_message {
        SendIcon::Check
    } else if input.is_running {
        if queuing {
            SendIcon::Queue
        } else {
            SendIcon::Steer
        }
    } else {
        SendIcon::ArrowUp
    };
    let compact_chip = compact_tokens.map(|tokens| {
        let keep = input.keep_full_history;
        CompactChip {
            aria_label: format!("Compact {tokens} tokens of history before sending"),
            tooltip: if keep {
                format!("Next send keeps all {tokens} tokens. Click to compact first")
            } else {
                format!("Next send compacts {tokens} tokens first. Click to keep full history")
            },
            label: if keep { "Full" } else { "Compact" },
            tokens,
            keep_full_history: keep,
        }
    });
    PrimaryActions::Send(SendButton {
        label: submit_label,
        aria_label: submit_status.unwrap_or_else(|| submit_label.to_owned()),
        tooltip: submit_tooltip,
        icon,
        disabled: input.is_send_busy
            || is_send_disabled
            || input.is_connecting
            || input.is_environment_unavailable
            || (!input.has_sendable_content && !show_resume),
        is_resume: show_resume,
        compact_chip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(is_running: bool) -> ComposerPrimaryActionsInput {
        ComposerPrimaryActionsInput {
            compact: true,
            pending_action: Some(PendingActionState {
                question_index: 0,
                is_last_question: true,
                can_advance: true,
                is_responding: false,
                is_complete: true,
            }),
            is_running,
            can_interrupt: is_running,
            ..Default::default()
        }
    }

    fn send(reason: Option<&str>) -> ComposerPrimaryActionsInput {
        ComposerPrimaryActionsInput {
            compact: true,
            prompt_has_text: true,
            has_sendable_content: true,
            send_disabled_reason: reason.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn disables_and_labels_the_send_button_while_feedback_is_uploading() {
        let PrimaryActions::Send(button) = resolve_primary_actions(&send(Some("Sending feedback")))
        else {
            panic!("send button");
        };
        assert!(button.disabled);
        assert_eq!(button.aria_label, "Sending feedback");
    }

    #[test]
    fn offers_stop_generation_while_a_running_turn_is_waiting_for_user_input() {
        assert!(matches!(
            resolve_primary_actions(&pending(true)),
            PrimaryActions::Pending {
                show_stop: true,
                ..
            }
        ));
        assert!(matches!(
            resolve_primary_actions(&pending(false)),
            PrimaryActions::Pending {
                show_stop: false,
                ..
            }
        ));
    }

    #[test]
    fn labels_the_send_button_per_state() {
        let label = |input: ComposerPrimaryActionsInput| match resolve_primary_actions(&input) {
            PrimaryActions::Send(button) => button.aria_label,
            other => panic!("{other:?}"),
        };
        assert_eq!(label(send(None)), "Submit message");
        let running = ComposerPrimaryActionsInput {
            is_running: true,
            ..send(None)
        };
        assert_eq!(label(running.clone()), "Steer message");
        assert_eq!(
            label(ComposerPrimaryActionsInput {
                alternate_modifier: true,
                ..running.clone()
            }),
            "Queue message"
        );
        assert_eq!(
            label(ComposerPrimaryActionsInput {
                follow_up_behavior: ActiveTurnComposerAction::Queue,
                ..running
            }),
            "Queue message"
        );
        assert_eq!(
            label(ComposerPrimaryActionsInput {
                is_editing_queued_message: true,
                ..send(None)
            }),
            "Update queued message"
        );
        assert_eq!(
            label(ComposerPrimaryActionsInput {
                is_environment_unavailable: true,
                ..send(None)
            }),
            "Environment disconnected"
        );
        assert_eq!(
            label(ComposerPrimaryActionsInput {
                is_connecting: true,
                ..send(None)
            }),
            "Connecting"
        );
        assert_eq!(
            label(ComposerPrimaryActionsInput {
                is_preparing_worktree: true,
                ..send(None)
            }),
            "Preparing worktree"
        );
        assert_eq!(
            label(ComposerPrimaryActionsInput {
                is_send_busy: true,
                ..send(None)
            }),
            "Submitting message"
        );
    }

    #[test]
    fn running_tooltip_names_both_follow_up_actions() {
        let PrimaryActions::Send(button) = resolve_primary_actions(&ComposerPrimaryActionsInput {
            is_running: true,
            alternate_shortcut_label: Some("⌘⇧↵".into()),
            ..send(None)
        }) else {
            panic!("send");
        };
        assert_eq!(
            button.tooltip,
            "Click to steer, Ctrl/⌘-click or ⌘⇧↵ to queue"
        );
        assert_eq!(button.icon, SendIcon::Steer);
    }

    #[test]
    fn stops_when_interruptible_without_sendable_content_and_resumes_when_possible() {
        let stop = ComposerPrimaryActionsInput {
            can_interrupt: true,
            is_running: true,
            ..Default::default()
        };
        assert_eq!(resolve_primary_actions(&stop), PrimaryActions::Stop);
        let resume = ComposerPrimaryActionsInput {
            can_resume: true,
            ..Default::default()
        };
        let PrimaryActions::Send(button) = resolve_primary_actions(&resume) else {
            panic!("send");
        };
        assert_eq!(button.aria_label, "Resume thread");
        assert!(button.is_resume && !button.disabled);
    }

    #[test]
    fn compact_chip_sets_the_send_label_and_tooltips() {
        let input = ComposerPrimaryActionsInput {
            compact_before_send_tokens: Some(48_200.0),
            ..send(None)
        };
        let PrimaryActions::Send(button) = resolve_primary_actions(&input) else {
            panic!("send");
        };
        assert_eq!(button.label, "Compact and send");
        assert_eq!(button.tooltip, "Summarize 48k tokens of history, then send");
        let chip = button.compact_chip.expect("chip");
        assert_eq!(chip.label, "Compact");
        assert_eq!(
            chip.tooltip,
            "Next send compacts 48k tokens first. Click to keep full history"
        );
        let keep = ComposerPrimaryActionsInput {
            keep_full_history: true,
            ..input
        };
        let PrimaryActions::Send(button) = resolve_primary_actions(&keep) else {
            panic!("send");
        };
        assert_eq!(button.label, "Submit message");
        assert_eq!(button.compact_chip.unwrap().label, "Full");
    }

    #[test]
    fn plan_follow_up_offers_refine_with_text_and_implement_without() {
        let base = ComposerPrimaryActionsInput {
            show_plan_follow_up_prompt: true,
            ..Default::default()
        };
        assert_eq!(
            resolve_primary_actions(&ComposerPrimaryActionsInput {
                prompt_has_text: true,
                ..base.clone()
            }),
            PrimaryActions::Refine {
                label: "Refine",
                disabled: false
            }
        );
        assert_eq!(
            resolve_primary_actions(&base),
            PrimaryActions::Implement {
                label: "Implement",
                disabled: false
            }
        );
        assert_eq!(
            resolve_primary_actions(&ComposerPrimaryActionsInput {
                is_send_busy: true,
                ..base
            }),
            PrimaryActions::Implement {
                label: "Sending...",
                disabled: true
            }
        );
    }

    #[test]
    fn pending_labels() {
        assert_eq!(
            format_pending_primary_action_label(false, true, false, 0),
            "Submit answer"
        );
        assert_eq!(
            format_pending_primary_action_label(false, true, false, 2),
            "Submit answers"
        );
        assert_eq!(
            format_pending_primary_action_label(false, false, false, 0),
            "Next question"
        );
        assert_eq!(
            format_pending_primary_action_label(true, false, false, 0),
            "Next"
        );
        assert_eq!(
            format_pending_primary_action_label(true, true, false, 0),
            "Submit"
        );
        assert_eq!(
            format_pending_primary_action_label(true, true, true, 0),
            "Submitting..."
        );
    }

    #[test]
    fn formats_context_window_tokens() {
        assert_eq!(format_context_window_tokens(None), "0");
        assert_eq!(format_context_window_tokens(Some(412.0)), "412");
        assert_eq!(format_context_window_tokens(Some(1_000.0)), "1k");
        assert_eq!(format_context_window_tokens(Some(1_250.0)), "1.3k");
        assert_eq!(format_context_window_tokens(Some(48_200.0)), "48k");
        assert_eq!(format_context_window_tokens(Some(1_250_000.0)), "1.3m");
        assert_eq!(format_context_window_tokens(Some(999_600.0)), "1m");
    }
}
