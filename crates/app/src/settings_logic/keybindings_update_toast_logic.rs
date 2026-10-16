//! Port of upstream/apps/web/src/components/KeybindingsUpdateToast.logic.ts.

// Ported ahead of the views that consume it.
#![allow(dead_code)]

pub const KEYBINDINGS_SUCCESS_TOAST_COOLDOWN_MS: u64 = 2_000;

/// `ServerConfigStreamEvent`, reduced to what the policy reads (the contracts
/// crate has not ported the stream union).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerConfigStreamEvent {
    KeybindingsUpdated {
        issues: Vec<KeybindingsIssue>,
    },
    /// Any other event (`settingsUpdated`, …).
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybindingsIssue {
    pub kind: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeybindingsUpdateToastDecision {
    Success,
    InvalidConfiguration { message: String },
}

pub struct KeybindingsUpdateToastController {
    now: Box<dyn Fn() -> u64>,
    last_success_toast_at: Option<u64>,
}

/// `createKeybindingsUpdateToastController`; `now` defaults to wall-clock ms.
pub fn create_keybindings_update_toast_controller(
    now: Option<Box<dyn Fn() -> u64>>,
) -> KeybindingsUpdateToastController {
    KeybindingsUpdateToastController {
        now: now.unwrap_or_else(|| {
            Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_millis() as u64)
            })
        }),
        last_success_toast_at: None,
    }
}

impl KeybindingsUpdateToastController {
    pub fn handle(
        &mut self,
        event: Option<&ServerConfigStreamEvent>,
    ) -> Option<KeybindingsUpdateToastDecision> {
        let Some(ServerConfigStreamEvent::KeybindingsUpdated { issues }) = event else {
            return None;
        };
        if let Some(issue) = issues
            .iter()
            .find(|entry| entry.kind.starts_with("keybindings."))
        {
            return Some(KeybindingsUpdateToastDecision::InvalidConfiguration {
                message: issue.message.clone(),
            });
        }

        let current_time = (self.now)();
        if self.last_success_toast_at.is_some_and(|last| {
            current_time.saturating_sub(last) < KEYBINDINGS_SUCCESS_TOAST_COOLDOWN_MS
        }) {
            return None;
        }

        self.last_success_toast_at = Some(current_time);
        Some(KeybindingsUpdateToastDecision::Success)
    }
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/components/KeybindingsUpdateToast.logic.test.ts.
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn keybindings_event(issues: Vec<KeybindingsIssue>) -> ServerConfigStreamEvent {
        ServerConfigStreamEvent::KeybindingsUpdated { issues }
    }

    #[test]
    fn coalesces_repeated_successful_reload_notifications_during_the_cooldown() {
        let now = Rc::new(Cell::new(1_000u64));
        let clock = Rc::clone(&now);
        let mut controller =
            create_keybindings_update_toast_controller(Some(Box::new(move || clock.get())));

        assert_eq!(
            controller.handle(Some(&keybindings_event(vec![]))),
            Some(KeybindingsUpdateToastDecision::Success)
        );

        now.set(now.get() + KEYBINDINGS_SUCCESS_TOAST_COOLDOWN_MS - 1);
        assert_eq!(controller.handle(Some(&keybindings_event(vec![]))), None);

        now.set(now.get() + 1);
        assert_eq!(
            controller.handle(Some(&keybindings_event(vec![]))),
            Some(KeybindingsUpdateToastDecision::Success)
        );
    }

    #[test]
    fn surfaces_keybinding_configuration_issues() {
        let mut controller = create_keybindings_update_toast_controller(None);
        assert_eq!(
            controller.handle(Some(&keybindings_event(vec![KeybindingsIssue {
                kind: "keybindings.malformed-config".to_string(),
                message: "Expected JSON array".to_string(),
            }]))),
            Some(KeybindingsUpdateToastDecision::InvalidConfiguration {
                message: "Expected JSON array".to_string(),
            })
        );
    }

    #[test]
    fn ignores_unrelated_server_config_notifications() {
        let mut controller = create_keybindings_update_toast_controller(None);
        assert_eq!(
            controller.handle(Some(&ServerConfigStreamEvent::Other)),
            None
        );
        assert_eq!(controller.handle(None), None);
    }

    #[test]
    fn issues_without_the_keybindings_prefix_do_not_block_the_success_toast() {
        let mut controller = create_keybindings_update_toast_controller(None);
        assert_eq!(
            controller.handle(Some(&keybindings_event(vec![KeybindingsIssue {
                kind: "other.thing".to_string(),
                message: "ignored".to_string(),
            }]))),
            Some(KeybindingsUpdateToastDecision::Success)
        );
    }
}
