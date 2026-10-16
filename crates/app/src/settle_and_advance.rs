//! Port of upstream/apps/web/src/settleAndAdvance.ts (+ test).

use std::sync::{Arc, Mutex};

use turin_contracts::environment::ScopedThreadRef;

type SettleAndAdvance = Arc<dyn Fn(ScopedThreadRef) + Send + Sync>;

static REGISTERED: Mutex<Option<SettleAndAdvance>> = Mutex::new(None);

/// Called by the thread sidebar while it is mounted. Returns the unregister
/// function, which leaves a newer registration in place.
pub fn register_settle_and_advance(
    handler: impl Fn(ScopedThreadRef) + Send + Sync + 'static,
) -> impl FnOnce() {
    let handler: SettleAndAdvance = Arc::new(handler);
    *REGISTERED.lock().unwrap() = Some(Arc::clone(&handler));
    move || {
        let mut slot = REGISTERED.lock().unwrap();
        if slot
            .as_ref()
            .is_some_and(|registered| Arc::ptr_eq(registered, &handler))
        {
            *slot = None;
        }
    }
}

/// Settles the open thread the way the sidebar Settle button does.
pub fn settle_and_advance(thread_ref: ScopedThreadRef) -> bool {
    if let Some(handler) = REGISTERED.lock().unwrap().clone() {
        handler(thread_ref);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::{EnvironmentId, ThreadId};

    fn thread_ref() -> ScopedThreadRef {
        ScopedThreadRef {
            environment_id: EnvironmentId::new("environment-local"),
            thread_id: ThreadId::new("thread-1"),
        }
    }

    #[test]
    fn hands_open_thread_to_mounted_sidebar() {
        let settled = Mutex::new(Vec::<ScopedThreadRef>::new());
        let unregister = register_settle_and_advance({
            let settled = &settled;
            move |thread| settled.lock().unwrap().push(thread)
        });
        assert!(settle_and_advance(thread_ref()));
        assert_eq!(settled.lock().unwrap().len(), 1);
        unregister();
        assert!(!settle_and_advance(thread_ref()));
    }

    #[test]
    fn settles_in_place_when_no_sidebar() {
        assert!(!settle_and_advance(thread_ref()));
    }

    #[test]
    fn keeps_remounted_handler_when_old_unmounts_late() {
        let settled_by = Mutex::new(Vec::<String>::new());
        let unregister_old = register_settle_and_advance({
            let settled_by = &settled_by;
            move |_| settled_by.lock().unwrap().push("old".into())
        });
        let unregister_new = register_settle_and_advance({
            let settled_by = &settled_by;
            move |_| settled_by.lock().unwrap().push("new".into())
        });
        unregister_old();
        assert!(settle_and_advance(thread_ref()));
        assert_eq!(settled_by.lock().unwrap(), vec!["new"]);
        unregister_new();
        assert!(!settle_and_advance(thread_ref()));
    }
}
