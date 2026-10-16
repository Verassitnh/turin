//! Port of upstream/apps/web/src/components/KeybindingsUpdateToast.logic.ts
//! (+ test).
//!
//! Decides what the keybindings-reloaded toast says: a configuration problem
//! in `keybindings.json` always surfaces, while repeated successful reloads
//! inside [`KEYBINDINGS_SUCCESS_TOAST_COOLDOWN_MS`] collapse into one toast.
//! The toast itself is the caller's.
// Seam for the shell, which owns the toast host; nothing calls this yet.
#![allow(dead_code)]

use turin_runtime::keybindings::{KeybindingsConfigState, ServerConfigIssue};

pub const KEYBINDINGS_SUCCESS_TOAST_COOLDOWN_MS: i64 = 2_000;

/// The slice of upstream's `ServerConfigStreamEvent` the controller reads.
/// `KeybindingsUpdated` carries the same payload shape as the stream event
/// (`keybindings` and `issues`); `Other` stands for every other event type
/// (`settingsUpdated`, `providerStatuses`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerConfigStreamEvent {
    KeybindingsUpdated(KeybindingsConfigState),
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindingsUpdateToastDecision {
    Success,
    InvalidConfiguration { message: String },
}

fn issue_kind(issue: &ServerConfigIssue) -> &'static str {
    match issue {
        ServerConfigIssue::MalformedConfig { .. } => "keybindings.malformed-config",
        ServerConfigIssue::InvalidEntry { .. } => "keybindings.invalid-entry",
    }
}

fn issue_message(issue: &ServerConfigIssue) -> &str {
    match issue {
        ServerConfigIssue::MalformedConfig { message }
        | ServerConfigIssue::InvalidEntry { message, .. } => message,
    }
}

/// `createKeybindingsUpdateToastController`. The clock is passed to
/// [`KeybindingsUpdateToastController::handle`] as epoch milliseconds
/// (upstream injects `now`, defaulting to `Date.now`).
#[derive(Debug, Default)]
pub struct KeybindingsUpdateToastController {
    last_success_toast_at: Option<i64>,
}

impl KeybindingsUpdateToastController {
    pub fn new() -> Self {
        Self::default()
    }

    /// `handle`: `None` for a missing event, a foreign event type or a
    /// success inside the cooldown.
    pub fn handle(
        &mut self,
        event: Option<&ServerConfigStreamEvent>,
        now: i64,
    ) -> Option<KeybindingsUpdateToastDecision> {
        let Some(ServerConfigStreamEvent::KeybindingsUpdated(payload)) = event else {
            return None;
        };

        if let Some(issue) = payload
            .issues
            .iter()
            .find(|entry| issue_kind(entry).starts_with("keybindings."))
        {
            return Some(KeybindingsUpdateToastDecision::InvalidConfiguration {
                message: issue_message(issue).to_string(),
            });
        }

        if let Some(last) = self.last_success_toast_at
            && now - last < KEYBINDINGS_SUCCESS_TOAST_COOLDOWN_MS
        {
            return None;
        }

        self.last_success_toast_at = Some(now);
        Some(KeybindingsUpdateToastDecision::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keybindings_event(issues: Vec<ServerConfigIssue>) -> ServerConfigStreamEvent {
        ServerConfigStreamEvent::KeybindingsUpdated(KeybindingsConfigState {
            keybindings: Vec::new(),
            issues,
        })
    }

    #[test]
    fn coalesces_repeated_successful_reload_notifications_during_the_cooldown() {
        let mut now = 1_000;
        let mut controller = KeybindingsUpdateToastController::new();
        let event = keybindings_event(Vec::new());

        assert_eq!(
            controller.handle(Some(&event), now),
            Some(KeybindingsUpdateToastDecision::Success)
        );

        now += KEYBINDINGS_SUCCESS_TOAST_COOLDOWN_MS - 1;
        assert_eq!(controller.handle(Some(&event), now), None);

        now += 1;
        assert_eq!(
            controller.handle(Some(&event), now),
            Some(KeybindingsUpdateToastDecision::Success)
        );
    }

    #[test]
    fn surfaces_keybinding_configuration_issues() {
        let mut controller = KeybindingsUpdateToastController::new();
        let event = keybindings_event(vec![ServerConfigIssue::MalformedConfig {
            message: "Expected JSON array".to_string(),
        }]);

        assert_eq!(
            controller.handle(Some(&event), 0),
            Some(KeybindingsUpdateToastDecision::InvalidConfiguration {
                message: "Expected JSON array".to_string()
            })
        );
    }

    #[test]
    fn ignores_unrelated_server_config_notifications() {
        let mut controller = KeybindingsUpdateToastController::new();

        assert_eq!(
            controller.handle(Some(&ServerConfigStreamEvent::Other), 0),
            None
        );
        assert_eq!(controller.handle(None, 0), None);
    }

    #[test]
    fn an_issue_does_not_start_the_success_cooldown() {
        let mut controller = KeybindingsUpdateToastController::new();
        let bad = keybindings_event(vec![ServerConfigIssue::InvalidEntry {
            index: 1,
            message: "bad rule".to_string(),
        }]);
        let good = keybindings_event(Vec::new());

        assert!(matches!(
            controller.handle(Some(&bad), 100),
            Some(KeybindingsUpdateToastDecision::InvalidConfiguration { .. })
        ));
        assert_eq!(
            controller.handle(Some(&good), 101),
            Some(KeybindingsUpdateToastDecision::Success)
        );
    }
}
