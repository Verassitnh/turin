//! Port of upstream/packages/client-runtime/src/delayedStatus.ts
//!
//! Upstream calls `setTimeout`/`clearTimeout` directly (display timing for React hooks, outside
//! an Effect runtime). Turin has no ambient timer, so the two calls go through [`Timers`]; the
//! GPUI side implements it with its executor's timer. The state machine is unchanged.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// How long a status must last before a client shows it.
pub const STATUS_SHOW_DELAY_MS: u64 = 400;
/// How long a shown status stays up, so it cannot flash at the show delay.
pub const STATUS_MIN_VISIBLE_MS: u64 = 400;

/// The status a client should show, and the key it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownStatus<A> {
    pub key: String,
    pub value: A,
}

/// What `setTimeout` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerHandle(pub u64);

pub type TimerCallback = Box<dyn FnOnce()>;

/// `setTimeout` / `clearTimeout`. A callback must run on the thread that owns the
/// [`DelayedStatus`], and never from inside `set_timeout` or `clear_timeout`.
pub trait Timers {
    fn set_timeout(&self, delay_ms: u64, callback: TimerCallback) -> TimerHandle;
    fn clear_timeout(&self, handle: TimerHandle);
}

struct State<A> {
    key: String,
    latest: Option<A>,
    shown: Option<A>,
    // While hidden, this is the show delay. While shown, the minimum visible time.
    timer: Option<TimerHandle>,
}

struct Inner<A> {
    state: RefCell<State<A>>,
    on_change: Box<dyn Fn(Option<ShownStatus<A>>)>,
    timers: Rc<dyn Timers>,
}

pub struct DelayedStatus<A> {
    inner: Rc<Inner<A>>,
}

impl<A: Clone + PartialEq + 'static> Inner<A> {
    fn clear_timer(&self) {
        let handle = self.state.borrow_mut().timer.take();
        if let Some(handle) = handle {
            self.timers.clear_timeout(handle);
        }
    }

    fn hide(&self) {
        self.state.borrow_mut().shown = None;
        (self.on_change)(None);
    }

    // Every shown value, including a new label, gets the full minimum visible time.
    fn show(self: &Rc<Self>, value: A) {
        let key = {
            let mut state = self.state.borrow_mut();
            state.shown = Some(value.clone());
            state.key.clone()
        };
        (self.on_change)(Some(ShownStatus { key, value }));
        self.clear_timer();
        let weak: Weak<Self> = Rc::downgrade(self);
        let handle = self.timers.set_timeout(
            STATUS_MIN_VISIBLE_MS,
            Box::new(move || {
                let Some(inner) = weak.upgrade() else { return };
                let latest_is_none = {
                    let mut state = inner.state.borrow_mut();
                    state.timer = None;
                    state.latest.is_none()
                };
                if latest_is_none {
                    inner.hide();
                }
            }),
        );
        self.state.borrow_mut().timer = Some(handle);
    }
}

/// Turns a real status (for example the thread sync phase) into the status a
/// client shows. A status that clears within `STATUS_SHOW_DELAY_MS` is never
/// shown. A shown status stays for at least `STATUS_MIN_VISIBLE_MS`. Values
/// compare by identity upstream, so use strings or other stable values.
///
/// Web and mobile wrap this in a small `useDelayedStatus` hook.
pub fn create_delayed_status<A: Clone + PartialEq + 'static>(
    on_change: impl Fn(Option<ShownStatus<A>>) + 'static,
    timers: Rc<dyn Timers>,
) -> DelayedStatus<A> {
    DelayedStatus {
        inner: Rc::new(Inner {
            state: RefCell::new(State {
                key: String::new(),
                latest: None,
                shown: None,
                timer: None,
            }),
            on_change: Box::new(on_change),
            timers,
        }),
    }
}

impl<A: Clone + PartialEq + 'static> DelayedStatus<A> {
    /// Reports the real status for `key`. A new key drops the shown status at once.
    pub fn update(&self, next_key: &str, value: Option<A>) {
        let inner = &self.inner;
        let key_changed = inner.state.borrow().key != next_key;
        if key_changed {
            inner.state.borrow_mut().key = next_key.to_owned();
            inner.clear_timer();
            let is_shown = inner.state.borrow().shown.is_some();
            if is_shown {
                inner.hide();
            }
        }
        let shown = {
            let mut state = inner.state.borrow_mut();
            state.latest = value.clone();
            state.shown.clone()
        };

        let Some(shown) = shown else {
            match value {
                None => inner.clear_timer(),
                Some(_) => {
                    let timer_is_none = inner.state.borrow().timer.is_none();
                    if timer_is_none {
                        let weak: Weak<Inner<A>> = Rc::downgrade(inner);
                        let handle = inner.timers.set_timeout(
                            STATUS_SHOW_DELAY_MS,
                            Box::new(move || {
                                let Some(inner) = weak.upgrade() else { return };
                                let latest = {
                                    let mut state = inner.state.borrow_mut();
                                    state.timer = None;
                                    state.latest.clone()
                                };
                                if let Some(latest) = latest {
                                    inner.show(latest);
                                }
                            }),
                        );
                        inner.state.borrow_mut().timer = Some(handle);
                    }
                }
            }
            return;
        };

        match value {
            Some(value) => {
                if value != shown {
                    inner.show(value);
                }
            }
            None => {
                let timer_is_none = inner.state.borrow().timer.is_none();
                if timer_is_none {
                    inner.hide();
                }
            }
        }
    }

    /// Cancels pending timers. A later `update` starts again from the real status.
    pub fn dispose(&self) {
        self.inner.clear_timer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// `vi.useFakeTimers()`: timers fire in due order as the clock advances.
    #[derive(Default)]
    struct FakeTimers {
        now: Cell<u64>,
        next_id: Cell<u64>,
        pending: RefCell<Vec<(u64, u64, TimerCallback)>>,
    }

    impl FakeTimers {
        fn pop_due(&self, limit: Option<u64>) -> Option<(u64, TimerCallback)> {
            let mut pending = self.pending.borrow_mut();
            let index = pending
                .iter()
                .enumerate()
                .filter(|(_, (_, due, _))| limit.is_none_or(|limit| *due <= limit))
                .min_by_key(|(_, (id, due, _))| (*due, *id))
                .map(|(index, _)| index)?;
            let (_, due, callback) = pending.remove(index);
            Some((due, callback))
        }

        /// `vi.advanceTimersByTime`
        fn advance(&self, ms: u64) {
            let target = self.now.get() + ms;
            while let Some((due, callback)) = self.pop_due(Some(target)) {
                self.now.set(self.now.get().max(due));
                callback();
            }
            self.now.set(target);
        }

        /// `vi.runAllTimers`
        fn run_all(&self) {
            while let Some((due, callback)) = self.pop_due(None) {
                self.now.set(self.now.get().max(due));
                callback();
            }
        }
    }

    impl Timers for FakeTimers {
        fn set_timeout(&self, delay_ms: u64, callback: TimerCallback) -> TimerHandle {
            let id = self.next_id.get();
            self.next_id.set(id + 1);
            self.pending
                .borrow_mut()
                .push((id, self.now.get() + delay_ms, callback));
            TimerHandle(id)
        }

        fn clear_timeout(&self, handle: TimerHandle) {
            self.pending
                .borrow_mut()
                .retain(|(id, _, _)| *id != handle.0);
        }
    }

    type Changes = Rc<RefCell<Vec<Option<ShownStatus<String>>>>>;

    fn track() -> (Rc<FakeTimers>, Changes, DelayedStatus<String>) {
        let timers = Rc::new(FakeTimers::default());
        let changes: Changes = Rc::default();
        let sink = Rc::clone(&changes);
        let status = create_delayed_status::<String>(
            move |shown| sink.borrow_mut().push(shown),
            Rc::clone(&timers) as Rc<dyn Timers>,
        );
        (timers, changes, status)
    }

    fn shown(key: &str, value: &str) -> Option<ShownStatus<String>> {
        Some(ShownStatus {
            key: key.to_owned(),
            value: value.to_owned(),
        })
    }

    fn last(changes: &Changes) -> Option<ShownStatus<String>> {
        changes
            .borrow()
            .last()
            .cloned()
            .expect("a change was reported")
    }

    #[test]
    fn never_shows_a_status_that_clears_before_the_show_delay() {
        let (timers, changes, status) = track();
        status.update("a", Some("syncing".to_owned()));
        timers.advance(STATUS_SHOW_DELAY_MS - 1);
        status.update("a", None);
        timers.run_all();

        assert!(changes.borrow().is_empty());
    }

    #[test]
    fn holds_each_shown_status_for_the_minimum_time_then_hides_it_at_once() {
        let (timers, changes, status) = track();
        status.update("a", Some("loading".to_owned()));
        timers.advance(STATUS_SHOW_DELAY_MS);
        assert_eq!(*changes.borrow(), vec![shown("a", "loading")]);

        // A new label gets its own full hold, even during the first label's hold.
        timers.advance(1);
        status.update("a", Some("syncing".to_owned()));
        status.update("a", None);
        timers.advance(STATUS_MIN_VISIBLE_MS - 1);
        assert_eq!(last(&changes), shown("a", "syncing"));
        timers.advance(1);
        assert_eq!(last(&changes), None);

        status.update("a", Some("syncing".to_owned()));
        timers.advance(STATUS_SHOW_DELAY_MS + STATUS_MIN_VISIBLE_MS);
        status.update("a", None);
        assert_eq!(last(&changes), None);
    }

    #[test]
    fn drops_the_shown_status_at_once_when_the_key_changes() {
        let (timers, changes, status) = track();
        status.update("a", Some("syncing".to_owned()));
        timers.advance(STATUS_SHOW_DELAY_MS);
        status.update("b", Some("syncing".to_owned()));
        assert_eq!(last(&changes), None);

        timers.advance(STATUS_SHOW_DELAY_MS);
        assert_eq!(last(&changes), shown("b", "syncing"));
    }
}
