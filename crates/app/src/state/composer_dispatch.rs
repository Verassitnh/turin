//! Port of upstream/packages/client-runtime/src/state/composerDispatch.ts
//!
//! How a composer submission is delivered when the thread already has a turn in
//! flight. Shared by web (where the alternate is Mod+Enter) and mobile (where it
//! is a long-press on the send button), so both clients agree on what the user's
//! configured follow-up behavior means.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerDispatchMode {
    Auto,
    Queue,
    Steer,
    Restart,
}

/// `Exclude<ComposerDispatchMode, "auto">`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActiveTurnComposerAction {
    Queue,
    #[default]
    Steer,
    Restart,
}

impl From<ActiveTurnComposerAction> for ComposerDispatchMode {
    fn from(action: ActiveTurnComposerAction) -> Self {
        match action {
            ActiveTurnComposerAction::Queue => Self::Queue,
            ActiveTurnComposerAction::Steer => Self::Steer,
            ActiveTurnComposerAction::Restart => Self::Restart,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ResolveComposerDispatchModeInput {
    /// A turn is in flight, so the follow-up has to queue behind it or steer it.
    pub running: bool,
    pub alternate_modifier: bool,
    /// Defaults to steering.
    pub active_turn_default: Option<ActiveTurnComposerAction>,
}

/// The alternate switches between queue and steer relative to the configured action.
pub fn resolve_composer_dispatch_mode(
    input: ResolveComposerDispatchModeInput,
) -> ComposerDispatchMode {
    if !input.running {
        return ComposerDispatchMode::Auto;
    }
    let default_action = input.active_turn_default.unwrap_or_default();
    if input.alternate_modifier {
        return if default_action == ActiveTurnComposerAction::Queue {
            ComposerDispatchMode::Steer
        } else {
            ComposerDispatchMode::Queue
        };
    }
    default_action.into()
}

/// What the alternate would do, for labelling the affordance that triggers it.
pub fn alternate_composer_dispatch_action(
    active_turn_default: Option<ActiveTurnComposerAction>,
) -> ActiveTurnComposerAction {
    match resolve_composer_dispatch_mode(ResolveComposerDispatchModeInput {
        running: true,
        alternate_modifier: true,
        active_turn_default,
    }) {
        ComposerDispatchMode::Steer => ActiveTurnComposerAction::Steer,
        ComposerDispatchMode::Restart => ActiveTurnComposerAction::Restart,
        // `auto` cannot come back while `running`.
        ComposerDispatchMode::Queue | ComposerDispatchMode::Auto => ActiveTurnComposerAction::Queue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ActiveTurnComposerAction as Action;
    use ComposerDispatchMode as Mode;

    fn resolve(
        running: bool,
        alternate_modifier: bool,
        active_turn_default: Option<Action>,
    ) -> Mode {
        resolve_composer_dispatch_mode(ResolveComposerDispatchModeInput {
            running,
            alternate_modifier,
            active_turn_default,
        })
    }

    #[test]
    fn starts_an_ordinary_turn_while_idle() {
        assert_eq!(resolve(false, false, None), Mode::Auto);
    }

    #[test]
    fn steers_by_default_and_reserves_the_alternate_for_queueing_while_running() {
        assert_eq!(resolve(true, false, None), Mode::Steer);
        assert_eq!(resolve(true, true, None), Mode::Queue);
    }

    #[test]
    fn queues_as_the_alternate_action_when_restarting_is_the_default() {
        assert_eq!(resolve(true, false, Some(Action::Restart)), Mode::Restart);
        assert_eq!(resolve(true, true, Some(Action::Restart)), Mode::Queue);
    }

    #[test]
    fn uses_configured_behavior_only_during_a_running_turn() {
        for (default, mode, alternate) in [
            (Action::Queue, Mode::Queue, Mode::Steer),
            (Action::Steer, Mode::Steer, Mode::Queue),
        ] {
            assert_eq!(resolve(true, false, Some(default)), mode);
            assert_eq!(resolve(true, true, Some(default)), alternate);
            assert_eq!(resolve(false, false, Some(default)), Mode::Auto);
            assert_eq!(resolve(false, true, Some(default)), Mode::Auto);
        }
    }

    #[test]
    fn names_the_alternate_action_so_the_affordance_can_be_labelled() {
        assert_eq!(
            alternate_composer_dispatch_action(Some(Action::Queue)),
            Action::Steer
        );
        assert_eq!(
            alternate_composer_dispatch_action(Some(Action::Steer)),
            Action::Queue
        );
        assert_eq!(alternate_composer_dispatch_action(None), Action::Queue);
    }
}
