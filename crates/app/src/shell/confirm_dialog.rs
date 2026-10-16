#![allow(dead_code)]

//! Port of upstream/apps/web/src/confirmDialog.ts (+ confirmDialog.test.ts).

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

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
    listeners: Vec<(u64, Arc<dyn Fn() + Send + Sync>)>,
}

fn inner() -> &'static Mutex<ConfirmDialogInner> {
    static INNER: OnceLock<Mutex<ConfirmDialogInner>> = OnceLock::new();
    INNER.get_or_init(|| {
        Mutex::new(ConfirmDialogInner {
            state: ConfirmDialogState::Idle,
            active: None,
            queued: Vec::new(),
            registered_hosts: 0,
            listeners: Vec::new(),
        })
    })
}

fn resolve_pending(inner: &mut ConfirmDialogInner, confirmed: bool) {
    if let Some(mut active) = inner.active.take()
        && let Some(resolve) = active.resolve.take()
    {
        let _ = resolve.send(confirmed);
    }
    for mut queued in inner.queued.drain(..) {
        if let Some(resolve) = queued.resolve.take() {
            let _ = resolve.send(confirmed);
        }
    }
}

fn notify_listeners() {
    let listeners: Vec<Arc<dyn Fn() + Send + Sync>> = inner()
        .lock()
        .listeners
        .iter()
        .map(|(_, listener)| Arc::clone(listener))
        .collect();
    for listener in listeners {
        listener();
    }
}

/// Wakes the confirm host when the queue changes. The listener must not
/// re-enter the dialog lock on the calling stack.
pub fn subscribe_confirm_dialog(listener: impl Fn() + Send + Sync + 'static) -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    inner().lock().listeners.push((id, Arc::new(listener)));
    id
}

pub fn unsubscribe_confirm_dialog(id: u64) {
    inner()
        .lock()
        .listeners
        .retain(|(existing, _)| *existing != id);
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
        drop(guard);
        notify_listeners();
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
    let queued =
        guard.active.is_some() || matches!(guard.state, ConfirmDialogState::Closing { .. });
    if queued {
        guard.queued.push(pending);
    } else {
        guard.active = Some(pending);
        guard.state = ConfirmDialogState::Confirming { message, variant };
    }
    drop(guard);
    notify_listeners();
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
    drop(guard);
    notify_listeners();
}

/// `completeConfirmDialogClose`.
pub fn complete_confirm_dialog_close() {
    let mut guard = inner().lock();
    if !matches!(guard.state, ConfirmDialogState::Closing { .. }) {
        return;
    }
    let next = if guard.queued.is_empty() {
        None
    } else {
        Some(guard.queued.remove(0))
    };
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
    drop(guard);
    notify_listeners();
}

/// `resetConfirmDialogForTests`.
pub fn reset_confirm_dialog_for_tests() {
    let mut guard = inner().lock();
    resolve_pending(&mut guard, false);
    guard.registered_hosts = 0;
    guard.state = ConfirmDialogState::Idle;
    guard.listeners.clear();
}

#[cfg(test)]
mod tests {
    use super::ConfirmDialogVariant;
    use super::*;

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn require_confirmation(request: Option<ConfirmDialogRequest>) -> ConfirmDialogRequest {
        request.expect("expected a registered confirmation host")
    }

    #[test]
    fn returns_none_without_host() {
        let _exclusive = exclusive();
        reset_confirm_dialog_for_tests();
        assert!(request_confirm_dialog("Confirm this action?", None).is_none());
        assert_eq!(read_confirm_dialog_state(), ConfirmDialogState::Idle);
    }

    #[test]
    fn resolves_and_waits_for_close_transition() {
        let _exclusive = exclusive();
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

    #[test]
    fn serializes_concurrent_confirmations() {
        let _exclusive = exclusive();
        reset_confirm_dialog_for_tests();
        let _host = register_confirm_dialog_host();
        let mut first = require_confirmation(request_confirm_dialog("Delete the project?", None));
        let mut second =
            require_confirmation(request_confirm_dialog("Delete the worktree too?", None));
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

    #[test]
    fn queued_confirmations_are_fifo() {
        let _exclusive = exclusive();
        reset_confirm_dialog_for_tests();
        let _host = register_confirm_dialog_host();
        let _a = require_confirmation(request_confirm_dialog("A", None));
        let _b = require_confirmation(request_confirm_dialog("B", None));
        let _c = require_confirmation(request_confirm_dialog("C", None));
        assert!(matches!(
            read_confirm_dialog_state(),
            ConfirmDialogState::Confirming { ref message, .. } if message == "A"
        ));
        respond_to_confirm_dialog(true);
        complete_confirm_dialog_close();
        assert!(matches!(
            read_confirm_dialog_state(),
            ConfirmDialogState::Confirming { ref message, .. } if message == "B"
        ));
        respond_to_confirm_dialog(false);
        complete_confirm_dialog_close();
        assert!(matches!(
            read_confirm_dialog_state(),
            ConfirmDialogState::Confirming { ref message, .. } if message == "C"
        ));
    }
}
