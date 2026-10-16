//! Port of upstream/apps/web/src/hooks/useNowMinute.ts
//!
//! The DOM timer lives on the shell entity; this module holds the shared minute
//! value and listener list (`useSyncExternalStore` → [`subscribe_now_minute`] +
//! [`tick_now_minute`] on a GPUI timer aligned to UTC minute boundaries).

use std::sync::{Mutex, OnceLock};

type Listener = Box<dyn Fn() + Send + Sync>;

struct NowMinuteState {
    now_minute: String,
    timer_running: bool,
    listeners: Vec<Listener>,
}

static STATE: OnceLock<Mutex<NowMinuteState>> = OnceLock::new();

fn state() -> &'static Mutex<NowMinuteState> {
    STATE.get_or_init(|| {
        Mutex::new(NowMinuteState {
            now_minute: current_minute(),
            timer_running: false,
            listeners: Vec::new(),
        })
    })
}

fn current_minute() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M").to_string()
}

fn get_snapshot(guard: &NowMinuteState) -> String {
    if !guard.timer_running {
        current_minute()
    } else {
        guard.now_minute.clone()
    }
}

/// Milliseconds until the next UTC minute boundary (for the shell timer).
pub fn millis_until_next_minute(now_ms: u64) -> u64 {
    60_000 - (now_ms % 60_000)
}

/// Called on the aligned timer; re-reads the clock so a late tick self-corrects.
pub fn tick_now_minute() {
    let mut guard = state().lock().unwrap();
    let next = current_minute();
    if next != guard.now_minute {
        guard.now_minute = next;
        for listener in &guard.listeners {
            listener();
        }
    }
}

/// Subscribe to minute updates. The shell must call [`start_now_minute_timer`] once
/// when the first listener attaches and [`stop_now_minute_timer`] when the last
/// detaches (mirroring upstream's `subscribe`).
pub fn subscribe_now_minute(
    listener: impl Fn() + Send + Sync + 'static,
) -> (String, impl FnOnce()) {
    let listener: Listener = Box::new(listener);
    let mut guard = state().lock().unwrap();
    let first = guard.listeners.is_empty();
    guard.listeners.push(listener);
    if first {
        guard.timer_running = true;
        guard.now_minute = current_minute();
    }
    let snapshot = get_snapshot(&guard);
    drop(guard);
    (snapshot, || {
        let mut guard = state().lock().unwrap();
        if !guard.listeners.is_empty() {
            guard.listeners.pop();
        }
        if guard.listeners.is_empty() {
            guard.timer_running = false;
        }
    })
}

/// Current minute string (`YYYY-MM-DDTHH:MM`).
pub fn use_now_minute() -> String {
    let guard = state().lock().unwrap();
    get_snapshot(&guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_matches_minute_format() {
        let value = use_now_minute();
        assert_eq!(value.len(), 16);
        assert_eq!(value.as_bytes().get(10), Some(&b'T'));
    }

    #[test]
    fn aligns_to_minute_boundary() {
        assert_eq!(millis_until_next_minute(0), 60_000);
        assert_eq!(millis_until_next_minute(30_000), 30_000);
        assert_eq!(millis_until_next_minute(59_999), 1);
    }
}
