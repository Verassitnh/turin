//! Port of upstream/apps/web/src/components/chat/threadFindActionBus.ts
//!
//! The shortcut and the command palette ask the open chat to show its find bar without
//! holding a handle to it. Upstream dispatches a window event; here the request is a
//! counter in a GPUI global, and the chat's find controller observes it.

use gpui_kit::{App, BorrowAppContext as _, Context, Global, Subscription, Window};

/// `THREAD_FIND_OPEN_EVENT`, as the number of requests made so far.
#[derive(Debug, Default)]
pub struct ThreadFindOpenRequests {
    requested: u64,
}

impl ThreadFindOpenRequests {
    /// Counts one request and returns the new total.
    pub fn request(&mut self) -> u64 {
        self.requested += 1;
        self.requested
    }

    pub fn requested(&self) -> u64 {
        self.requested
    }
}

impl Global for ThreadFindOpenRequests {}

/// `requestThreadFindOpen`.
pub fn request_thread_find_open(cx: &mut App) {
    cx.update_default_global(|requests: &mut ThreadFindOpenRequests, _| {
        requests.request();
    });
}

/// Makes sure the global exists, so a request made before any chat mounted still counts.
pub fn init(cx: &mut App) {
    cx.default_global::<ThreadFindOpenRequests>();
}

/// `subscribeThreadFindOpen`: `listener` runs on every request while the returned
/// subscription lives.
pub fn subscribe_thread_find_open<T: 'static>(
    window: &mut Window,
    cx: &mut Context<T>,
    mut listener: impl FnMut(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> Subscription {
    cx.default_global::<ThreadFindOpenRequests>();
    cx.observe_global_in::<ThreadFindOpenRequests>(window, move |this, window, cx| {
        listener(this, window, cx)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_each_request() {
        let mut requests = ThreadFindOpenRequests::default();
        assert_eq!(requests.requested(), 0);
        assert_eq!(requests.request(), 1);
        assert_eq!(requests.request(), 2);
        assert_eq!(requests.requested(), 2);
    }
}
