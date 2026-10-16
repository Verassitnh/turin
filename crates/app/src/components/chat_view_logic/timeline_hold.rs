//! Port of upstream/apps/web/src/components/ChatView.logic.ts (held thread timelines).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::state::models::parse_scoped_thread_key;

const MAX_REMEMBERED_THREAD_TIMELINES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldThreadTimeline<T> {
    pub thread_key: Option<String>,
    pub entries: Vec<T>,
    pub markdown_cwd: Option<String>,
    pub workspace_root: Option<String>,
}

struct StoredHeldTimeline {
    thread_key: Option<String>,
    entries: serde_json::Value,
    markdown_cwd: Option<String>,
    workspace_root: Option<String>,
}

struct HeldState {
    timelines: HashMap<String, StoredHeldTimeline>,
    order: Vec<String>,
    last_ready_thread_key: Option<String>,
}

static HELD: LazyLock<Mutex<HeldState>> = LazyLock::new(|| {
    Mutex::new(HeldState {
        timelines: HashMap::new(),
        order: Vec::new(),
        last_ready_thread_key: None,
    })
});

fn remember_thread_timeline_entries<T: Clone + serde::Serialize>(held: &HeldThreadTimeline<T>) {
    let Some(thread_key) = held.thread_key.as_deref() else {
        return;
    };
    let entries = serde_json::to_value(&held.entries).expect("timeline entries json");
    let mut state = HELD.lock().expect("held timeline lock");
    state.timelines.insert(
        thread_key.to_owned(),
        StoredHeldTimeline {
            thread_key: Some(thread_key.to_owned()),
            entries,
            markdown_cwd: held.markdown_cwd.clone(),
            workspace_root: held.workspace_root.clone(),
        },
    );
    state.order.retain(|key| key != thread_key);
    state.order.push(thread_key.to_owned());
    while state.order.len() > MAX_REMEMBERED_THREAD_TIMELINES {
        if let Some(evicted) = state.order.first().cloned() {
            state.order.remove(0);
            state.timelines.remove(&evicted);
        }
    }
    state.last_ready_thread_key = Some(thread_key.to_owned());
}

pub fn remember_ready_thread_timeline<T: Clone + serde::Serialize>(held: &HeldThreadTimeline<T>) {
    if held.thread_key.is_none() || held.entries.is_empty() {
        return;
    }
    remember_thread_timeline_entries(held);
}

pub fn peek_remembered_thread_timeline<T: for<'de> serde::Deserialize<'de>>(
    thread_key: Option<&str>,
) -> Option<Vec<T>> {
    let thread_key = thread_key?;
    let state = HELD.lock().expect("held timeline lock");
    let held = state.timelines.get(thread_key)?;
    serde_json::from_value(held.entries.clone()).ok()
}

pub fn peek_held_thread_timeline<T: for<'de> serde::Deserialize<'de>>()
-> Option<HeldThreadTimeline<T>> {
    let state = HELD.lock().expect("held timeline lock");
    let thread_key = state.last_ready_thread_key.as_deref()?;
    let held = state.timelines.get(thread_key)?;
    if held
        .entries
        .as_array()
        .is_some_and(|entries| entries.is_empty())
    {
        return None;
    }
    Some(HeldThreadTimeline {
        thread_key: held.thread_key.clone(),
        entries: serde_json::from_value(held.entries.clone()).ok()?,
        markdown_cwd: held.markdown_cwd.clone(),
        workspace_root: held.workspace_root.clone(),
    })
}

pub fn reset_held_thread_timeline() {
    let mut state = HELD.lock().expect("held timeline lock");
    state.timelines.clear();
    state.order.clear();
    state.last_ready_thread_key = None;
}

pub fn thread_keys_share_environment(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    let Some((left_env, _)) = parse_scoped_thread_key(left) else {
        return false;
    };
    let Some((right_env, _)) = parse_scoped_thread_key(right) else {
        return false;
    };
    left_env == right_env
}

pub fn is_paint_only_thread_timeline(
    display_thread_key: Option<&str>,
    active_thread_key: Option<&str>,
) -> bool {
    matches!(
        (display_thread_key, active_thread_key),
        (Some(display), Some(active)) if display != active
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSwitchTimelineResult<T> {
    pub entries: Vec<T>,
    pub display_thread_key: Option<String>,
}

pub fn resolve_thread_switch_timeline<
    T: Clone + serde::Serialize + for<'de> serde::Deserialize<'de>,
>(
    loading: bool,
    active_thread_key: Option<&str>,
    next_entries: &[T],
    remembered_for_active: Option<Vec<T>>,
    last_ready: Option<HeldThreadTimeline<T>>,
) -> ThreadSwitchTimelineResult<T> {
    if !next_entries.is_empty() {
        return ThreadSwitchTimelineResult {
            entries: next_entries.to_vec(),
            display_thread_key: active_thread_key.map(str::to_owned),
        };
    }

    let remembered_for_active =
        remembered_for_active.or_else(|| peek_remembered_thread_timeline(active_thread_key));
    if loading
        && remembered_for_active
            .as_ref()
            .is_some_and(|entries| !entries.is_empty())
    {
        return ThreadSwitchTimelineResult {
            entries: remembered_for_active.expect("checked some"),
            display_thread_key: active_thread_key.map(str::to_owned),
        };
    }

    let last_ready = last_ready.or_else(peek_held_thread_timeline);
    if let Some(last_ready) = last_ready
        && loading
        && last_ready.thread_key.is_some()
        && last_ready.thread_key.as_deref() != active_thread_key
        && !last_ready.entries.is_empty()
        && thread_keys_share_environment(last_ready.thread_key.as_deref(), active_thread_key)
    {
        return ThreadSwitchTimelineResult {
            entries: last_ready.entries,
            display_thread_key: last_ready.thread_key,
        };
    }

    ThreadSwitchTimelineResult {
        entries: next_entries.to_vec(),
        display_thread_key: active_thread_key.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held() -> HeldThreadTimeline<String> {
        HeldThreadTimeline {
            thread_key: Some("env-1:thread-a".to_owned()),
            entries: vec!["a1".to_owned(), "a2".to_owned()],
            markdown_cwd: None,
            workspace_root: None,
        }
    }

    #[test]
    fn keeps_previous_thread_entries_while_loading() {
        let result = resolve_thread_switch_timeline(
            true,
            Some("env-1:thread-b"),
            &[] as &[String],
            None,
            Some(held()),
        );
        assert_eq!(result.entries, vec!["a1".to_owned(), "a2".to_owned()]);
        assert_eq!(result.display_thread_key.as_deref(), Some("env-1:thread-a"));
    }

    #[test]
    fn shows_new_thread_once_detail_is_ready() {
        let result = resolve_thread_switch_timeline(
            false,
            Some("env-1:thread-b"),
            &[String::from("b1")],
            None,
            Some(held()),
        );
        assert_eq!(result.entries, vec!["b1".to_owned()]);
        assert_eq!(result.display_thread_key.as_deref(), Some("env-1:thread-b"));
    }

    #[test]
    fn does_not_invent_timeline_on_first_open() {
        let result = resolve_thread_switch_timeline(
            true,
            Some("env-1:thread-a"),
            &[] as &[String],
            None,
            None,
        );
        assert!(result.entries.is_empty());
        assert_eq!(result.display_thread_key.as_deref(), Some("env-1:thread-a"));
    }

    #[test]
    fn held_timeline_module_state() {
        reset_held_thread_timeline();
        remember_ready_thread_timeline(&HeldThreadTimeline {
            thread_key: Some("env-1:thread-a".to_owned()),
            entries: vec!["a1"],
            markdown_cwd: Some("/repo/a".to_owned()),
            workspace_root: Some("/repo/a".to_owned()),
        });
        let peeked: HeldThreadTimeline<String> =
            peek_held_thread_timeline().expect("held timeline");
        assert_eq!(peeked.entries, vec!["a1".to_owned()]);
        assert_eq!(peeked.markdown_cwd.as_deref(), Some("/repo/a"));
    }

    #[test]
    fn paint_only_and_environment_guards() {
        assert!(!thread_keys_share_environment(
            Some("env-1:thread-a"),
            Some("env-2:thread-b")
        ));
        assert!(is_paint_only_thread_timeline(
            Some("env-1:thread-a"),
            Some("env-1:thread-b")
        ));
        assert!(!is_paint_only_thread_timeline(
            Some("env-1:thread-b"),
            Some("env-1:thread-b")
        ));
    }
}
