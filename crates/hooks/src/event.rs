//! New functionality (PORTING.md "Hooks (new)"): the events hooks can
//! subscribe to. No upstream counterpart — these replace the triggers that
//! used to come from `ThreadLaunchService` / worktree creation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Event a hook can subscribe to. The serde renames are the exact names used
/// in `hooks.toml`, in the `TURIN_EVENT` environment variable, and in the
/// stdin envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEventKind {
    #[serde(rename = "thread.created")]
    ThreadCreated,
    #[serde(rename = "turn.started")]
    TurnStarted,
    #[serde(rename = "turn.completed")]
    TurnCompleted,
    #[serde(rename = "turn.failed")]
    TurnFailed,
    #[serde(rename = "approval.requested")]
    ApprovalRequested,
    #[serde(rename = "pr.linked")]
    PrLinked,
    #[serde(rename = "pr.checks.completed")]
    PrChecksCompleted,
    #[serde(rename = "git.committed")]
    GitCommitted,
}

impl HookEventKind {
    /// Every valid event, in spec order.
    pub const ALL: [HookEventKind; 8] = [
        Self::ThreadCreated,
        Self::TurnStarted,
        Self::TurnCompleted,
        Self::TurnFailed,
        Self::ApprovalRequested,
        Self::PrLinked,
        Self::PrChecksCompleted,
        Self::GitCommitted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThreadCreated => "thread.created",
            Self::TurnStarted => "turn.started",
            Self::TurnCompleted => "turn.completed",
            Self::TurnFailed => "turn.failed",
            Self::ApprovalRequested => "approval.requested",
            Self::PrLinked => "pr.linked",
            Self::PrChecksCompleted => "pr.checks.completed",
            Self::GitCommitted => "git.committed",
        }
    }
}

impl std::fmt::Display for HookEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error text kept in one place so a test can pin it against `ALL`.
const VALID_EVENT_NAMES: &str = "thread.created, turn.started, turn.completed, turn.failed, \
     approval.requested, pr.linked, pr.checks.completed, git.committed";

/// A config entry named an event this build does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown hook event '{event}' (valid events: {VALID_EVENT_NAMES})")]
pub struct UnknownHookEventError {
    pub event: String,
}

impl std::str::FromStr for HookEventKind {
    type Err = UnknownHookEventError;

    fn from_str(event: &str) -> Result<Self, Self::Err> {
        Ok(match event {
            "thread.created" => Self::ThreadCreated,
            "turn.started" => Self::TurnStarted,
            "turn.completed" => Self::TurnCompleted,
            "turn.failed" => Self::TurnFailed,
            "approval.requested" => Self::ApprovalRequested,
            "pr.linked" => Self::PrLinked,
            "pr.checks.completed" => Self::PrChecksCompleted,
            "git.committed" => Self::GitCommitted,
            _ => {
                return Err(UnknownHookEventError {
                    event: event.to_owned(),
                });
            }
        })
    }
}

/// One event occurrence plus its JSON payload, handed to
/// [`crate::HookRunner::dispatch`]. The payload travels unchanged under
/// `payload` in the stdin envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookEvent {
    pub kind: HookEventKind,
    pub payload: Value,
}

impl HookEvent {
    pub fn new(kind: HookEventKind, payload: Value) -> Self {
        Self { kind, payload }
    }

    pub fn thread_created(payload: Value) -> Self {
        Self::new(HookEventKind::ThreadCreated, payload)
    }

    pub fn turn_started(payload: Value) -> Self {
        Self::new(HookEventKind::TurnStarted, payload)
    }

    pub fn turn_completed(payload: Value) -> Self {
        Self::new(HookEventKind::TurnCompleted, payload)
    }

    pub fn turn_failed(payload: Value) -> Self {
        Self::new(HookEventKind::TurnFailed, payload)
    }

    pub fn approval_requested(payload: Value) -> Self {
        Self::new(HookEventKind::ApprovalRequested, payload)
    }

    pub fn pr_linked(payload: Value) -> Self {
        Self::new(HookEventKind::PrLinked, payload)
    }

    pub fn pr_checks_completed(payload: Value) -> Self {
        Self::new(HookEventKind::PrChecksCompleted, payload)
    }

    pub fn git_committed(payload: Value) -> Self {
        Self::new(HookEventKind::GitCommitted, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn serde_names_roundtrip_every_event() {
        for kind in HookEventKind::ALL {
            let encoded = serde_json::to_string(&kind).unwrap();
            assert_eq!(encoded, format!("\"{}\"", kind.as_str()));
            let decoded: HookEventKind = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, kind);
        }
    }

    #[test]
    fn from_str_accepts_every_event() {
        for kind in HookEventKind::ALL {
            assert_eq!(kind.as_str().parse::<HookEventKind>().unwrap(), kind);
        }
    }

    #[test]
    fn from_str_rejects_unknown_event_with_full_list() {
        let err = "thread.create".parse::<HookEventKind>().unwrap_err();
        assert_eq!(err.event, "thread.create");
        let message = err.to_string();
        assert!(
            message.contains("unknown hook event 'thread.create'"),
            "{message}"
        );
        for kind in HookEventKind::ALL {
            assert!(
                message.contains(kind.as_str()),
                "missing {kind} in {message}"
            );
        }
    }

    #[test]
    fn hardcoded_valid_names_match_all() {
        let joined = HookEventKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(joined, VALID_EVENT_NAMES);
    }

    #[test]
    fn event_serializes_camel_case_with_dotted_kind() {
        let event = HookEvent::thread_created(serde_json::json!({"branch": "feature/x"}));
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["kind"], "thread.created");
        assert_eq!(encoded["payload"]["branch"], "feature/x");
    }
}
