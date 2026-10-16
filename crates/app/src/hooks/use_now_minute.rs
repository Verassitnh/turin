//! Port of upstream/apps/web/src/hooks/useNowMinute.ts
//!
//! Minute-quantized UI clock (`YYYY-MM-DDTHH:MM`, UTC). One module-level timer feeds every
//! consumer: the first [`subscribe_now_minute`] starts it (aligned to the next UTC minute
//! boundary, then every 60 s) and the last unsubscribe stops it, as upstream's `subscribe` does.
//! The timer is a thread parked on a condvar, so stopping it is prompt; [`use_now_minute`] is
//! `getSnapshot`.

#![allow(dead_code)]

use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

type Listener = Arc<dyn Fn() + Send + Sync>;

/// Wakes the timer thread to stop (`clearTimeout` / `clearInterval`).
type StopSignal = Arc<(Mutex<bool>, Condvar)>;

struct NowMinuteState {
    now_minute: String,
    timer: Option<StopSignal>,
    next_listener_id: u64,
    listeners: Vec<(u64, Listener)>,
}

static STATE: OnceLock<Mutex<NowMinuteState>> = OnceLock::new();

fn state() -> &'static Mutex<NowMinuteState> {
    STATE.get_or_init(|| {
        Mutex::new(NowMinuteState {
            now_minute: current_minute(),
            timer: None,
            next_listener_id: 0,
            listeners: Vec::new(),
        })
    })
}

fn current_minute() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M").to_string()
}

/// With no timer running the stored minute may be stale (the first render after a full unmount),
/// so it is re-read; while the timer runs the cached value is returned untouched.
fn get_snapshot(guard: &mut NowMinuteState) -> String {
    if guard.timer.is_none() {
        guard.now_minute = current_minute();
    }
    guard.now_minute.clone()
}

/// Milliseconds until the next UTC minute boundary (for the timer).
pub fn millis_until_next_minute(now_ms: u64) -> u64 {
    60_000 - (now_ms % 60_000)
}

/// The aligned timer's tick; also re-reads the clock, so a late tick self-corrects. Listeners run
/// with the lock released, so one may call [`use_now_minute`] or subscribe.
pub fn tick_now_minute() {
    let listeners: Vec<Listener> = {
        let mut guard = state().lock().unwrap();
        let next = current_minute();
        if next == guard.now_minute {
            return;
        }
        guard.now_minute = next;
        guard
            .listeners
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect()
    };
    for listener in listeners {
        listener();
    }
}

/// `startTimer`: align to the next UTC minute boundary, then tick every 60 s.
fn start_now_minute_timer() -> StopSignal {
    let stop: StopSignal = Arc::new((Mutex::new(false), Condvar::new()));
    let signal = stop.clone();
    std::thread::spawn(move || {
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let mut wait = millis_until_next_minute(now_ms);
        loop {
            let (lock, condvar) = &*signal;
            let stopped = lock.lock().unwrap();
            let (stopped, _) = condvar
                .wait_timeout_while(stopped, Duration::from_millis(wait), |stopped| !*stopped)
                .unwrap();
            if *stopped {
                return;
            }
            drop(stopped);
            tick_now_minute();
            wait = 60_000;
        }
    });
    stop
}

fn stop_now_minute_timer(stop: StopSignal) {
    let (lock, condvar) = &*stop;
    *lock.lock().unwrap() = true;
    condvar.notify_all();
}

/// Subscribe to minute updates; returns the current minute and the function that unsubscribes
/// exactly this listener.
pub fn subscribe_now_minute(
    listener: impl Fn() + Send + Sync + 'static,
) -> (String, impl FnOnce()) {
    let mut guard = state().lock().unwrap();
    let id = guard.next_listener_id;
    guard.next_listener_id += 1;
    if guard.listeners.is_empty() && guard.timer.is_none() {
        guard.now_minute = current_minute();
        guard.timer = Some(start_now_minute_timer());
    }
    guard.listeners.push((id, Arc::new(listener)));
    let snapshot = get_snapshot(&mut guard);
    drop(guard);
    (snapshot, move || {
        let stop = {
            let mut guard = state().lock().unwrap();
            guard
                .listeners
                .retain(|(listener_id, _)| *listener_id != id);
            if guard.listeners.is_empty() {
                guard.timer.take()
            } else {
                None
            }
        };
        if let Some(stop) = stop {
            stop_now_minute_timer(stop);
        }
    })
}

/// Current minute string (`YYYY-MM-DDTHH:MM`).
pub fn use_now_minute() -> String {
    let mut guard = state().lock().unwrap();
    get_snapshot(&mut guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The state is process-wide, so these tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn force_a_new_minute() {
        state().lock().unwrap().now_minute = "stale".to_owned();
        tick_now_minute();
    }

    fn counter() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        (count, move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[test]
    fn snapshot_matches_minute_format() {
        let _turn = TURN.lock().unwrap();
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

    #[test]
    fn unsubscribing_removes_that_listener_and_not_the_newest() {
        let _turn = TURN.lock().unwrap();
        let (first_count, first) = counter();
        let (second_count, second) = counter();
        let (_, unsubscribe_first) = subscribe_now_minute(first);
        let (_, unsubscribe_second) = subscribe_now_minute(second);
        force_a_new_minute();
        assert_eq!(first_count.load(Ordering::SeqCst), 1);
        assert_eq!(second_count.load(Ordering::SeqCst), 1);
        unsubscribe_first();
        force_a_new_minute();
        // Only the listener that stayed hears the next minute.
        assert_eq!(first_count.load(Ordering::SeqCst), 1);
        assert_eq!(second_count.load(Ordering::SeqCst), 2);
        unsubscribe_second();
    }

    #[test]
    fn the_timer_runs_while_anyone_listens_and_stops_with_the_last() {
        let _turn = TURN.lock().unwrap();
        assert!(state().lock().unwrap().timer.is_none());
        let (_, first) = counter();
        let (_, second) = counter();
        let (_, unsubscribe_first) = subscribe_now_minute(first);
        assert!(state().lock().unwrap().timer.is_some());
        let (_, unsubscribe_second) = subscribe_now_minute(second);
        unsubscribe_first();
        assert!(state().lock().unwrap().timer.is_some());
        unsubscribe_second();
        assert!(state().lock().unwrap().timer.is_none());
    }

    #[test]
    fn a_listener_may_read_the_clock_while_it_is_called() {
        let _turn = TURN.lock().unwrap();
        let heard = Arc::new(Mutex::new(None));
        let sink = heard.clone();
        let (_, unsubscribe) = subscribe_now_minute(move || {
            *sink.lock().unwrap() = Some(use_now_minute());
        });
        force_a_new_minute();
        assert_eq!(heard.lock().unwrap().as_deref().map(str::len), Some(16));
        unsubscribe();
    }
}
