//! Port of upstream/apps/web/src/confirmDialog.ts (+ confirmDialog.test.ts).

use parking_lot::Mutex;
use std::sync::OnceLock;

/// `ConfirmDialogVariant` from upstream/packages/contracts/src/ipc.ts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmDialogVariant {
    Default,
    Destructive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmDialogState {
    Idle,
    Confirming {
        message: String,
        variant: ConfirmDialogVariant,
    },
    Closing {
        message: String,
        variant: ConfirmDialogVariant,
    },
}

struct PendingConfirmation {
    message: String,
    variant: ConfirmDialogVariant,
    resolve: Option<tokio::sync::oneshot::Sender<bool>>,
}

struct ConfirmDialogInner {
    state: ConfirmDialogState,
    active: Option<PendingConfirmation>,
    queued: Vec<PendingConfirmation>,
    registered_hosts: u32,
}

fn inner() -> &'static Mutex<ConfirmDialogInner> {
    static INNER: OnceLock<Mutex<ConfirmDialogInner>> = OnceLock::new();
    INNER.get_or_init(|| {
        Mutex::new(ConfirmDialogInner {
            state: ConfirmDialogState::Idle,
            active: None,
            queued: Vec::new(),
            registered_hosts: 0,
        })
    })
}

fn resolve_pending(inner: &mut ConfirmDialogInner, confirmed: bool) {
    if let Some(mut active) = inner.active.take() {
        if let Some(resolve) = active.resolve.take() {
            let _ = resolve.send(confirmed);
        }
    }
    for mut queued in inner.queued.drain(..) {
        if let Some(resolve) = queued.resolve.take() {
            let _ = resolve.send(confirmed);
        }
    }
}

/// `readConfirmDialogState`.
pub fn read_confirm_dialog_state() -> ConfirmDialogState {
    inner().lock().state.clone()
}

/// `registerConfirmDialogHost`.
pub fn register_confirm_dialog_host() -> ConfirmDialogHostGuard {
    inner().lock().registered_hosts += 1;
    ConfirmDialogHostGuard { registered: true }
}

pub struct ConfirmDialogHostGuard {
    registered: bool,
}

impl Drop for ConfirmDialogHostGuard {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        self.registered = false;
        let mut guard = inner().lock();
        guard.registered_hosts = guard.registered_hosts.saturating_sub(1);
        if guard.registered_hosts == 0 {
            resolve_pending(&mut guard, false);
            guard.state = ConfirmDialogState::Idle;
        }
    }
}

/// `requestConfirmDialog`.
pub fn request_confirm_dialog(
    message: impl Into<String>,
    variant: Option<ConfirmDialogVariant>,
) -> Option<ConfirmDialogRequest> {
    let mut guard = inner().lock();
    if guard.registered_hosts == 0 {
        return None;
    }
    let message = message.into();
    let variant = variant.unwrap_or(ConfirmDialogVariant::Default);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let pending = PendingConfirmation {
        message: message.clone(),
        variant,
        resolve: Some(tx),
    };
    if guard.active.is_some() || matches!(guard.state, ConfirmDialogState::Closing { .. }) {
        guard.queued.push(pending);
        return Some(ConfirmDialogRequest { receiver: rx });
    }
    guard.active = Some(pending);
    guard.state = ConfirmDialogState::Confirming { message, variant };
    Some(ConfirmDialogRequest { receiver: rx })
}

pub struct ConfirmDialogRequest {
    receiver: tokio::sync::oneshot::Receiver<bool>,
}

impl ConfirmDialogRequest {
    pub async fn wait(self) -> bool {
        self.receiver.await.unwrap_or(false)
    }

    pub fn try_wait(&mut self) -> Option<bool> {
        self.receiver.try_recv().ok()
    }
}

/// `isConfirmDialogActive`.
pub fn is_confirm_dialog_active() -> bool {
    let guard = inner().lock();
    !matches!(guard.state, ConfirmDialogState::Idle)
        || guard.active.is_some()
        || !guard.queued.is_empty()
}

/// `respondToConfirmDialog`.
pub fn respond_to_confirm_dialog(confirmed: bool) {
    let mut guard = inner().lock();
    let ConfirmDialogState::Confirming { message, variant } = guard.state.clone() else {
        return;
    };
    let Some(mut active) = guard.active.take() else {
        return;
    };
    if let Some(resolve) = active.resolve.take() {
        let _ = resolve.send(confirmed);
    }
    guard.state = ConfirmDialogState::Closing { message, variant };
}

/// `completeConfirmDialogClose`.
pub fn complete_confirm_dialog_close() {
    let mut guard = inner().lock();
    if !matches!(guard.state, ConfirmDialogState::Closing { .. }) {
        return;
    }
    let next = guard.queued.pop();
    match next {
        None => {
            guard.state = ConfirmDialogState::Idle;
        }
        Some(pending) => {
            guard.state = ConfirmDialogState::Confirming {
                message: pending.message.clone(),
                variant: pending.variant,
            };
            guard.active = Some(pending);
        }
    }
}

/// `resetConfirmDialogForTests`.
pub fn reset_confirm_dialog_for_tests() {
    let mut guard = inner().lock();
    resolve_pending(&mut guard, false);
    guard.registered_hosts = 0;
    guard.state = ConfirmDialogState::Idle;
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::settings::ConfirmDialogVariant;

    fn require_confirmation(request: Option<ConfirmDialogRequest>) -> ConfirmDialogRequest {
        request.expect("expected a registered confirmation host")
    }

    #[test]
    fn returns_none_without_host() {
        reset_confirm_dialog_for_tests();
        assert!(request_confirm_dialog("Confirm this action?").is_none());
        assert_eq!(read_confirm_dialog_state(), ConfirmDialogState::Idle);
    }

    #[tokio::test]
    async fn resolves_and_waits_for_close_transition() {
        reset_confirm_dialog_for_tests();
        let _host = register_confirm_dialog_host();
        let mut confirmation = require_confirmation(request_confirm_dialog(
            "Delete this thread?",
            Some(ConfirmDialogVariant::Destructive),
        ));
        assert_eq!(
            read_confirm_dialog_state(),
            ConfirmDialogState::Confirming {
                message: "Delete this thread?".into(),
                variant: ConfirmDialogVariant::Destructive,
            }
        );
        respond_to_confirm_dialog(true);
        assert_eq!(confirmation.try_wait(), Some(true));
        assert!(is_confirm_dialog_active());
        assert_eq!(
            read_confirm_dialog_state(),
            ConfirmDialogState::Closing {
                message: "Delete this thread?".into(),
                variant: ConfirmDialogVariant::Destructive,
            }
        );
        complete_confirm_dialog_close();
        assert_eq!(read_confirm_dialog_state(), ConfirmDialogState::Idle);
        assert!(!is_confirm_dialog_active());
    }

    #[tokio::test]
    async fn serializes_concurrent_confirmations() {
        reset_confirm_dialog_for_tests();
        let _host = register_confirm_dialog_host();
        let mut first = require_confirmation(request_confirm_dialog("Delete the project?"));
        let mut second = require_confirmation(request_confirm_dialog("Delete the worktree too?"));
        respond_to_confirm_dialog(false);
        assert_eq!(first.try_wait(), Some(false));
        complete_confirm_dialog_close();
        assert_eq!(
            read_confirm_dialog_state(),
            ConfirmDialogState::Confirming {
                message: "Delete the worktree too?".into(),
                variant: ConfirmDialogVariant::Default,
            }
        );
        respond_to_confirm_dialog(true);
        assert_eq!(second.try_wait(), Some(true));
        complete_confirm_dialog_close();
        assert_eq!(read_confirm_dialog_state(), ConfirmDialogState::Idle);
    }
}
