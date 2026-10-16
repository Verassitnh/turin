//! Port of the pure logic in upstream/apps/web/src/components/chat/useThreadFind.ts.
//!
//! The React hook keeps its state in `useState` and reads the server through
//! atoms. Here `ThreadFindState` is a plain struct with methods, and the caller
//! feeds in what the hook read: the open or close request, the server result
//! (`ThreadFindRemote`), and the content-changed flag. The query debounce (100 ms),
//! the 300 ms live-refresh timer and the atom subscription are the caller's; this
//! module says when each applies. Upstream compares responses by identity; here
//! they compare by value (see Deviations).

use std::sync::Arc;

use turin_contracts::base_schemas::js_trim;
use turin_contracts::orchestration_v2::{
    OrchestrationV2SearchThreadResult, OrchestrationV2SearchThreadSkill,
    OrchestrationV2ThreadFindNavigation,
};

use super::thread_find::{
    ThreadFindMatch, ThreadFindStart, format_thread_find_count, step_thread_find_index,
};

/// Upstream `useThreadFind` debounces the query for this long before it searches.
pub const THREAD_FIND_QUERY_DEBOUNCE_MS: u64 = 100;
/// Live content updates refresh the open search at most this often.
pub const THREAD_FIND_LIVE_REFRESH_MS: u64 = 300;
/// The toast shown when find is opened on a server that predates search.
pub const THREAD_FIND_UNAVAILABLE_TITLE: &str = "Thread search is unavailable on this server.";
pub const THREAD_FIND_UNAVAILABLE_DESCRIPTION: &str = "Update the server to enable it.";

/// A match the user stepped to locally, before the server's result caught up.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadFindLocalSelection {
    /// The result this selection was stepped from.
    pub base: Arc<OrchestrationV2SearchThreadResult>,
    pub base_navigation_id: u64,
    pub active_index: u64,
    pub r#match: ThreadFindMatch,
    pub navigation_id: u64,
}

/// `CLOSED_FIND` plus the fields that change while find is open.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThreadFindState {
    pub thread_key: Option<String>,
    pub query: String,
    pub offset: i64,
    pub start: Option<ThreadFindStart>,
    pub focus_request_id: u64,
    pub navigation_id: u64,
    pub request_id: u64,
    pub local_selection: Option<ThreadFindLocalSelection>,
}

/// The server side of the find, as the caller reads it each render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadFindRemote<'a> {
    pub data: Option<&'a OrchestrationV2SearchThreadResult>,
    pub navigation_id: u64,
    pub is_pending: bool,
    pub error: bool,
}

/// What the find bar shows while the server works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadFindStatus {
    Loading,
    Error,
}

/// The values `useThreadFind` derives from state and the remote result.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadFindView {
    pub status: Option<ThreadFindStatus>,
    pub count: u64,
    pub counting: bool,
    pub active_index: u64,
    pub navigation_id: u64,
    pub r#match: Option<ThreadFindMatch>,
    pub find_expanded: bool,
    pub find_query: String,
}

/// What the caller does after `open`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadFindOpenOutcome {
    /// No thread is selected; nothing changed.
    NoThread,
    /// Search is off on this server. The caller shows the unavailable toast.
    Unavailable,
    Opened,
}

/// What a live content refresh should do (`refreshLiveResultsRef`).
#[derive(Debug, Clone, PartialEq)]
pub enum ThreadFindLiveRefresh {
    /// Find is closed or the query is blank.
    Ignore,
    /// Paging is still counting; mark a refresh for when the search settles.
    DeferUntilSettled,
    /// Ask the server again for the current anchor.
    RemoteRefresh,
    /// Keep the same match but re-anchor the query on it, with no offset.
    Rebase(ThreadFindStart),
}

/// `query.trim()`, the form upstream searches and tests for emptiness.
fn normalized_query(query: &str) -> &str {
    js_trim(query)
}

/// The match the server reports for a result, as the find state keeps it.
fn remote_match(remote: ThreadFindRemote<'_>) -> Option<ThreadFindMatch> {
    remote.data.and_then(|data| {
        data.r#match.as_ref().map(|m| ThreadFindMatch {
            entry_id: m.entry_id.clone(),
            run_id: m.run_id.clone(),
            occurrence: m.occurrence,
        })
    })
}

/// Upstream `rebaseLocalSelection`: a response for an older anchor can update counts
/// without undoing newer local steps.
pub fn rebase_local_selection(
    selection: Option<&ThreadFindLocalSelection>,
    data: Option<&OrchestrationV2SearchThreadResult>,
    navigation_id: u64,
) -> Option<ThreadFindLocalSelection> {
    let (selection, data) = (selection?, data?);
    if *selection.base == *data && selection.base_navigation_id == navigation_id {
        return Some(selection.clone());
    }
    if navigation_id >= selection.navigation_id {
        return None;
    }
    let entry = data.navigation.as_ref().and_then(|navigation| {
        navigation
            .iter()
            .find(|entry| entry.entry_id == selection.r#match.entry_id)
    });
    let Some(entry) = entry else {
        return Some(selection.clone());
    };
    if selection.r#match.occurrence >= entry.count {
        return Some(selection.clone());
    }
    let active_index = entry.start_index + selection.r#match.occurrence;
    if active_index == selection.active_index {
        return Some(selection.clone());
    }
    Some(ThreadFindLocalSelection {
        active_index,
        ..selection.clone()
    })
}

fn navigation_entry_for(
    navigation: &[OrchestrationV2ThreadFindNavigation],
    index: u64,
) -> Option<&OrchestrationV2ThreadFindNavigation> {
    navigation
        .iter()
        .find(|entry| index >= entry.start_index && index < entry.start_index + entry.count)
}

fn match_from_navigation(
    entry: &OrchestrationV2ThreadFindNavigation,
    occurrence: u64,
) -> ThreadFindMatch {
    ThreadFindMatch {
        entry_id: entry.entry_id.clone(),
        run_id: entry.run_id.clone(),
        occurrence,
    }
}

impl ThreadFindState {
    /// `CLOSED_FIND` for a new thread key.
    pub fn closed() -> Self {
        Self::default()
    }

    /// Upstream's render-time reset: find closes when the thread changes or search is
    /// turned off. Returns whether the state was reset.
    pub fn reconcile(&mut self, enabled: bool, thread_key: Option<&str>) -> bool {
        let stale =
            self.thread_key.is_some() && (!enabled || self.thread_key.as_deref() != thread_key);
        if stale {
            *self = Self::closed();
        }
        stale
    }

    /// Whether find is open on `thread_key`.
    pub fn is_open(&self, enabled: bool, thread_key: Option<&str>) -> bool {
        enabled && thread_key.is_some() && self.thread_key.as_deref() == thread_key
    }

    /// Upstream `open`. `position` reads the anchor for a query from the rows on screen.
    pub fn open(
        &mut self,
        thread_key: Option<&str>,
        enabled: bool,
        position: impl Fn(&str) -> Option<ThreadFindStart>,
    ) -> ThreadFindOpenOutcome {
        let Some(thread_key) = thread_key else {
            return ThreadFindOpenOutcome::NoThread;
        };
        if !enabled {
            return ThreadFindOpenOutcome::Unavailable;
        }
        let start = if self.thread_key.as_deref() == Some(thread_key)
            && !normalized_query(&self.query).is_empty()
        {
            position(normalized_query(&self.query))
        } else {
            None
        };
        let previous_focus = self.focus_request_id;
        let mut current = if self.thread_key.as_deref() == Some(thread_key) {
            self.clone()
        } else {
            Self::closed()
        };
        current.thread_key = Some(thread_key.to_owned());
        current.focus_request_id = previous_focus + 1;
        if !normalized_query(&current.query).is_empty() {
            current.start = start;
            current.offset = 0;
            current.local_selection = None;
            current.navigation_id += 1;
            current.request_id = current.navigation_id;
        }
        *self = current;
        ThreadFindOpenOutcome::Opened
    }

    /// Upstream `close`.
    pub fn close(&mut self) {
        *self = Self::closed();
    }

    /// Upstream `onQueryChange`. `position` is the anchor reader, and the caller passes
    /// the query trimmed as upstream does.
    pub fn set_query(&mut self, query: &str, position: Option<ThreadFindStart>) {
        self.query = query.to_owned();
        self.offset = 0;
        self.start = position;
        self.local_selection = None;
        self.navigation_id += 1;
        self.request_id = self.navigation_id;
    }

    /// The match, status and counts the find bar and timeline read.
    pub fn view(&self, remote: ThreadFindRemote<'_>, is_open: bool) -> ThreadFindView {
        let local = rebase_local_selection(
            self.local_selection.as_ref(),
            remote.data,
            remote.navigation_id,
        );
        let status = if remote.error {
            Some(ThreadFindStatus::Error)
        } else if remote.is_pending && remote.data.is_none() {
            Some(ThreadFindStatus::Loading)
        } else {
            None
        };
        let selected = local
            .as_ref()
            .map(|local| local.r#match.clone())
            .or_else(|| remote_match(remote));
        let query = normalized_query(&self.query);
        ThreadFindView {
            status,
            count: remote.data.map_or(0, |data| data.total_matches),
            counting: remote.data.is_some_and(|data| data.complete == Some(false)),
            active_index: local
                .as_ref()
                .map(|local| local.active_index)
                .or_else(|| remote.data.map(|data| data.active_index))
                .unwrap_or(0),
            navigation_id: local
                .as_ref()
                .map_or(remote.navigation_id, |local| local.navigation_id),
            r#match: selected,
            find_expanded: is_open && !query.is_empty(),
            find_query: if is_open && remote.data.is_some_and(|data| data.r#match.is_some()) {
                self.query.clone()
            } else {
                String::new()
            },
        }
    }

    /// Upstream `step`. `content_unchanged` is `lastContentRef === content`, and
    /// `refresh_pending` is whether a live refresh timer is armed.
    pub fn step(
        &mut self,
        delta: i64,
        remote: ThreadFindRemote<'_>,
        content_unchanged: bool,
        refresh_pending: bool,
    ) {
        let count = remote.data.map_or(0, |data| data.total_matches);
        let counting = remote.data.is_some_and(|data| data.complete == Some(false));
        if count == 0 || counting {
            return;
        }
        let previous = self.clone();
        let local = rebase_local_selection(
            previous.local_selection.as_ref(),
            remote.data,
            remote.navigation_id,
        );
        let pending = previous.navigation_id
            != local
                .as_ref()
                .map_or(remote.navigation_id, |l| l.navigation_id);
        let selected = local
            .as_ref()
            .map(|local| local.r#match.clone())
            .or_else(|| remote_match(remote));
        let current_index = local
            .as_ref()
            .map(|local| local.active_index)
            .or_else(|| remote.data.map(|data| data.active_index))
            .unwrap_or(0);
        let next_index = step_thread_find_index(current_index, count, delta);
        let entry = remote
            .data
            .and_then(|data| data.navigation.as_deref())
            .and_then(|navigation| navigation_entry_for(navigation, next_index));
        let next_navigation_id = previous.navigation_id + 1;

        // Step locally when the server's navigation already covers the next match.
        if let Some((data, entry)) = remote.data.zip(entry) {
            let base_is_current = local.as_ref().is_none_or(|local| *local.base == *data)
                || local.as_ref().is_some_and(|local| {
                    data.navigation.as_deref().unwrap_or(&[]).iter().any(|e| {
                        e.entry_id == local.r#match.entry_id && local.r#match.occurrence < e.count
                    })
                });
            if !pending
                && !remote.is_pending
                && base_is_current
                && !refresh_pending
                && content_unchanged
            {
                self.navigation_id = next_navigation_id;
                self.local_selection = Some(ThreadFindLocalSelection {
                    base: Arc::new(data.clone()),
                    base_navigation_id: remote.navigation_id,
                    active_index: next_index,
                    r#match: match_from_navigation(entry, next_index - entry.start_index),
                    navigation_id: next_navigation_id,
                });
                return;
            }
        }

        // Otherwise ask the server for the next anchor.
        self.start = if pending || selected.is_none() {
            previous.start.clone()
        } else {
            selected.map(|selected| ThreadFindStart {
                entry_id: selected.entry_id,
                occurrence: selected.occurrence,
            })
        };
        self.offset = if pending {
            previous.offset + delta
        } else {
            delta
        };
        self.navigation_id = next_navigation_id;
        self.request_id = next_navigation_id;
    }

    /// Upstream `refreshLiveResultsRef`: the decision only. The caller arms the
    /// refresh and, for `Rebase`, applies it with `apply_rebase`.
    pub fn live_refresh(
        &self,
        is_open: bool,
        progressive: bool,
        remote: ThreadFindRemote<'_>,
        selected: Option<&ThreadFindMatch>,
    ) -> ThreadFindLiveRefresh {
        if !is_open || normalized_query(&self.query).is_empty() {
            return ThreadFindLiveRefresh::Ignore;
        }
        // Paging can change client content while the server is still counting.
        if progressive && remote.is_pending {
            return ThreadFindLiveRefresh::DeferUntilSettled;
        }
        let local = rebase_local_selection(
            self.local_selection.as_ref(),
            remote.data,
            remote.navigation_id,
        );
        let navigation_id = local
            .as_ref()
            .map_or(remote.navigation_id, |l| l.navigation_id);
        let Some(selected) = selected.filter(|_| self.navigation_id == navigation_id) else {
            return ThreadFindLiveRefresh::RemoteRefresh;
        };
        let start = ThreadFindStart {
            entry_id: selected.entry_id.clone(),
            occurrence: selected.occurrence,
        };
        if self.offset == 0
            && self.start.as_ref() == Some(&start)
            && self.request_id == self.navigation_id
        {
            ThreadFindLiveRefresh::RemoteRefresh
        } else {
            ThreadFindLiveRefresh::Rebase(start)
        }
    }

    /// Applies `ThreadFindLiveRefresh::Rebase`: re-anchor on the selected match.
    pub fn apply_rebase(&mut self, start: ThreadFindStart) {
        self.start = Some(start);
        self.offset = 0;
        self.request_id = self.navigation_id;
    }

    /// Whether a content change should arm the live-refresh timer.
    pub fn content_refresh_due(&self, is_open: bool) -> bool {
        is_open && !normalized_query(&self.query).is_empty()
    }
}

/// The find bar's count label, `"n/total"` (or `"0/0"`), for the view's values.
pub fn thread_find_count_label(view: &ThreadFindView) -> String {
    format_thread_find_count(view.active_index, view.count)
}

/// The search key `useServerResults` compares, `JSON.stringify([environmentId,
/// threadId, query, skillLabels])`. `None` when there is no thread.
pub fn thread_find_search_key(
    thread: Option<(&str, &str)>,
    query: &str,
    skills: &[OrchestrationV2SearchThreadSkill],
) -> Option<String> {
    let (environment_id, thread_id) = thread?;
    let labels: Vec<serde_json::Value> = skills
        .iter()
        .map(|skill| {
            let mut label = serde_json::Map::new();
            label.insert(
                "name".to_owned(),
                serde_json::Value::String(skill.name.clone()),
            );
            if let Some(display_name) = &skill.display_name {
                label.insert(
                    "displayName".to_owned(),
                    serde_json::Value::String(display_name.clone()),
                );
            }
            serde_json::Value::Object(label)
        })
        .collect();
    Some(
        serde_json::Value::Array(vec![
            serde_json::Value::String(environment_id.to_owned()),
            serde_json::Value::String(thread_id.to_owned()),
            serde_json::Value::String(normalized_query(query).to_owned()),
            serde_json::Value::Array(labels),
        ])
        .to_string(),
    )
}

/// Whether the server search runs for this query: upstream creates the atom only once
/// the debounced query equals the typed one.
pub fn thread_find_search_eligible(
    thread: Option<(&str, &str)>,
    query: &str,
    debounced_query: &str,
) -> bool {
    let normalized = normalized_query(query);
    thread.is_some() && !debounced_query.is_empty() && normalized == debounced_query
}

/// Upstream's `isPending` for the search result: typing or an in-flight request.
pub fn thread_find_is_pending(
    thread_present: bool,
    query: &str,
    debounced_query: &str,
    query_in_flight: bool,
) -> bool {
    let normalized = normalized_query(query);
    thread_present && !normalized.is_empty() && (normalized != debounced_query || query_in_flight)
}

/// `useServerResults`' memo of the last response, so a key change or a settled response
/// is the only thing that replaces the shown data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThreadFindResultMemo {
    pub key: Option<String>,
    pub data: Option<Arc<OrchestrationV2SearchThreadResult>>,
    pub navigation_id: u64,
}

impl ThreadFindResultMemo {
    /// Upstream's update-during-render rule. `result` is the query's current response.
    pub fn update(
        &mut self,
        key: Option<String>,
        result: Option<&Arc<OrchestrationV2SearchThreadResult>>,
        result_pending: bool,
        navigation_id: u64,
        progressive: bool,
    ) {
        let changed = self.key != key
            || (result.is_some()
                && (!result_pending || (progressive && self.data.as_ref() != result))
                && (self.data.as_ref() != result || self.navigation_id != navigation_id));
        if changed {
            self.key = key;
            self.data = result.cloned();
            self.navigation_id = navigation_id;
        }
    }

    /// The data the find shows: nothing from another key, nothing on error.
    pub fn shown(
        &self,
        key: Option<&str>,
        error: bool,
    ) -> Option<&Arc<OrchestrationV2SearchThreadResult>> {
        if self.key.as_deref() == key && !error {
            self.data.as_ref()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use turin_contracts::base_schemas::{RunId, TrimmedNonEmptyString};
    use turin_contracts::orchestration_v2::{
        OrchestrationV2SearchThreadResult, OrchestrationV2ThreadFindMatch,
        OrchestrationV2ThreadFindNavigation,
    };

    use super::*;

    fn entry(id: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(id).expect("non-blank entry id")
    }

    fn nav(id: &str, start_index: u64, count: u64) -> OrchestrationV2ThreadFindNavigation {
        OrchestrationV2ThreadFindNavigation {
            entry_id: entry(id),
            run_id: Some(RunId::new("run-1").expect("run id")),
            start_index,
            count,
        }
    }

    /// Two entries: "a" holds matches 0-1, "b" holds match 2.
    fn result(active_index: u64) -> OrchestrationV2SearchThreadResult {
        OrchestrationV2SearchThreadResult {
            complete: Some(true),
            snapshot_sequence: 7,
            total_matches: 3,
            active_index,
            r#match: Some(OrchestrationV2ThreadFindMatch {
                entry_id: entry("a"),
                run_id: None,
                occurrence: active_index,
            }),
            navigation: Some(vec![nav("a", 0, 2), nav("b", 2, 1)]),
        }
    }

    fn remote(
        data: Option<&OrchestrationV2SearchThreadResult>,
        navigation_id: u64,
    ) -> ThreadFindRemote<'_> {
        ThreadFindRemote {
            data,
            navigation_id,
            is_pending: false,
            error: false,
        }
    }

    fn open_state(query: &str) -> ThreadFindState {
        let mut state = ThreadFindState::closed();
        state.open(Some("env:thread"), true, |_| None);
        state.set_query(query, None);
        state
    }

    #[test]
    fn open_reports_no_thread_and_unavailable_without_changing_state() {
        let mut state = ThreadFindState::closed();
        assert_eq!(
            state.open(None, true, |_| None),
            ThreadFindOpenOutcome::NoThread
        );
        assert_eq!(
            state.open(Some("t"), false, |_| None),
            ThreadFindOpenOutcome::Unavailable
        );
        assert_eq!(state, ThreadFindState::closed());
    }

    #[test]
    fn opening_bumps_focus_and_keeps_the_query_for_the_same_thread() {
        let mut state = open_state("COD4");
        let before = state.focus_request_id;
        assert_eq!(
            state.open(Some("env:thread"), true, |_| None),
            ThreadFindOpenOutcome::Opened
        );
        assert_eq!(state.focus_request_id, before + 1);
        assert_eq!(state.query, "COD4");
        // Reopening restarts the search from the anchor the reader gives.
        assert_eq!(state.offset, 0);
        assert!(state.local_selection.is_none());
    }

    #[test]
    fn reconcile_closes_find_on_thread_change_or_when_search_is_off() {
        let mut state = open_state("COD4");
        assert!(!state.reconcile(true, Some("env:thread")));
        assert!(state.reconcile(true, Some("env:other")));
        assert_eq!(state, ThreadFindState::closed());
        let mut state = open_state("COD4");
        assert!(state.reconcile(false, Some("env:thread")));
        assert!(!state.is_open(true, Some("env:thread")));
    }

    #[test]
    fn blank_query_does_not_request_an_anchor_or_refresh() {
        let state = open_state("   ");
        let is_open = state.is_open(true, Some("env:thread"));
        assert!(!state.content_refresh_due(is_open));
        assert_eq!(
            state.live_refresh(is_open, false, remote(None, 0), None),
            ThreadFindLiveRefresh::Ignore
        );
    }

    #[test]
    fn rebase_keeps_a_newer_local_step_when_the_server_answers_an_older_anchor() {
        let base = result(0);
        let selection = ThreadFindLocalSelection {
            base: Arc::new(base.clone()),
            base_navigation_id: 1,
            active_index: 2,
            r#match: ThreadFindMatch {
                entry_id: entry("b"),
                run_id: None,
                occurrence: 0,
            },
            navigation_id: 3,
        };
        // Same result, older navigation: the local step still stands.
        assert_eq!(
            rebase_local_selection(Some(&selection), Some(&base), 1).map(|s| s.active_index),
            Some(2)
        );
        // A response for an older anchor with a different index moves the local step to it.
        let mut moved = result(0);
        moved.navigation = Some(vec![nav("a", 0, 1), nav("b", 1, 2)]);
        assert_eq!(
            rebase_local_selection(Some(&selection), Some(&moved), 2).map(|s| s.active_index),
            Some(1)
        );
        // A response from a newer navigation drops the local step.
        assert_eq!(
            rebase_local_selection(Some(&selection), Some(&base), 9),
            None
        );
    }

    #[test]
    fn stepping_inside_loaded_navigation_is_local_and_leaves_the_server_request_alone() {
        let mut state = open_state("COD4");
        let data = result(0);
        state.step(1, remote(Some(&data), 1), true, false);
        let local = state.local_selection.as_ref().expect("local step");
        assert_eq!(local.active_index, 1);
        assert_eq!(local.r#match.entry_id, entry("a"));
        assert_eq!(local.r#match.occurrence, 1);
        assert_eq!(state.start, None);
    }

    #[test]
    fn stepping_past_loaded_navigation_asks_the_server_from_the_selected_match() {
        let mut state = open_state("COD4");
        let data = result(1);
        // Index 2 is in entry "b", which the server gave only as navigation metadata
        // before this step. Refresh from the current selection (entry "a", occurrence 1).
        let mut data_without_b = data.clone();
        data_without_b.navigation = Some(vec![nav("a", 0, 2)]);
        state.step(1, remote(Some(&data_without_b), 1), true, false);
        assert!(state.local_selection.is_none());
        assert_eq!(
            state.start,
            Some(ThreadFindStart {
                entry_id: entry("a"),
                occurrence: 1,
            })
        );
        assert_eq!(state.offset, 1);
        assert_eq!(state.request_id, state.navigation_id);
    }

    #[test]
    fn step_is_ignored_while_the_server_is_still_counting() {
        let mut state = open_state("COD4");
        let mut data = result(0);
        data.complete = Some(false);
        state.step(1, remote(Some(&data), 1), true, false);
        assert!(state.local_selection.is_none());
        assert_eq!(state.navigation_id, 1);
    }

    #[test]
    fn view_reports_loading_error_and_counts() {
        let state = open_state("COD4");
        let view = state.view(
            ThreadFindRemote {
                data: None,
                navigation_id: 1,
                is_pending: true,
                error: false,
            },
            true,
        );
        assert_eq!(view.status, Some(ThreadFindStatus::Loading));
        assert_eq!(view.count, 0);
        let error_view = state.view(
            ThreadFindRemote {
                data: None,
                navigation_id: 1,
                is_pending: false,
                error: true,
            },
            true,
        );
        assert_eq!(error_view.status, Some(ThreadFindStatus::Error));
        let data = result(2);
        let view = state.view(remote(Some(&data), 1), true);
        assert_eq!(view.count, 3);
        assert_eq!(view.active_index, 2);
        assert!(view.find_expanded);
        assert_eq!(view.find_query, "COD4");
        assert_eq!(thread_find_count_label(&view), "3/3");
    }

    #[test]
    fn live_refresh_rebases_when_the_selected_match_moved_off_the_anchor() {
        let state = open_state("COD4");
        let data = result(1);
        let selected = ThreadFindMatch {
            entry_id: entry("a"),
            run_id: None,
            occurrence: 1,
        };
        assert_eq!(
            state.live_refresh(true, false, remote(Some(&data), 1), Some(&selected)),
            ThreadFindLiveRefresh::Rebase(ThreadFindStart {
                entry_id: entry("a"),
                occurrence: 1,
            })
        );
        assert_eq!(
            state.live_refresh(true, false, remote(Some(&data), 1), None),
            ThreadFindLiveRefresh::RemoteRefresh
        );
        let pending = ThreadFindRemote {
            data: Some(&data),
            navigation_id: 1,
            is_pending: true,
            error: false,
        };
        assert_eq!(
            state.live_refresh(true, true, pending, Some(&selected)),
            ThreadFindLiveRefresh::DeferUntilSettled
        );
    }

    #[test]
    fn live_refresh_reanchors_after_a_local_step_then_a_server_step_response() {
        let mut state = open_state("COD4");
        let loaded = result(0);
        // Only entry "a" is loaded, so the second step leaves the window.
        let mut window = loaded.clone();
        window.navigation = Some(vec![nav("a", 0, 2)]);
        state.step(1, remote(Some(&window), 1), true, false);
        assert!(state.local_selection.is_some());
        state.step(1, remote(Some(&window), 1), true, false);
        assert_eq!(state.offset, 1);
        assert!(state.local_selection.is_some());

        // The server answers that step. Its navigation is newer than the local
        // one, so the local step drops and the response match is the anchor.
        let mut answered = result(2);
        answered.r#match = Some(OrchestrationV2ThreadFindMatch {
            entry_id: entry("b"),
            run_id: None,
            occurrence: 0,
        });
        let selected = ThreadFindMatch {
            entry_id: entry("b"),
            run_id: None,
            occurrence: 0,
        };
        assert_eq!(
            state.live_refresh(
                true,
                false,
                remote(Some(&answered), state.navigation_id),
                Some(&selected),
            ),
            ThreadFindLiveRefresh::Rebase(ThreadFindStart {
                entry_id: entry("b"),
                occurrence: 0,
            })
        );
    }

    #[test]
    fn search_key_and_eligibility_follow_the_debounced_query() {
        let skills = [OrchestrationV2SearchThreadSkill {
            name: "review".to_owned(),
            display_name: None,
        }];
        assert_eq!(
            thread_find_search_key(Some(("env", "thread")), " COD4 ", &skills).as_deref(),
            Some(r#"["env","thread","COD4",[{"name":"review"}]]"#)
        );
        assert_eq!(thread_find_search_key(None, "COD4", &skills), None);
        assert!(thread_find_search_eligible(
            Some(("e", "t")),
            "COD4",
            "COD4"
        ));
        assert!(!thread_find_search_eligible(
            Some(("e", "t")),
            "COD44",
            "COD4"
        ));
        assert!(!thread_find_search_eligible(Some(("e", "t")), "   ", ""));
        assert!(thread_find_is_pending(true, "COD44", "COD4", false));
        assert!(!thread_find_is_pending(true, "COD4", "COD4", false));
    }

    #[test]
    fn result_memo_keeps_the_last_settled_result_for_its_key() {
        let mut memo = ThreadFindResultMemo::default();
        let data = Arc::new(result(0));
        memo.update(Some("k1".to_owned()), Some(&data), false, 1, false);
        assert!(memo.shown(Some("k1"), false).is_some());
        assert!(memo.shown(Some("k2"), false).is_none());
        assert!(memo.shown(Some("k1"), true).is_none());
        // A pending response for the same key does not replace the settled one.
        let newer = Arc::new(result(1));
        memo.update(Some("k1".to_owned()), Some(&newer), true, 2, false);
        assert_eq!(
            memo.shown(Some("k1"), false).map(|d| d.active_index),
            Some(0)
        );
    }
}
