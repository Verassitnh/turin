//! Port of upstream/apps/web/src/uiStateStore.ts (+ test): the UI-owned
//! preferences (sidebar expansion and order, thread visit markers, changed-file
//! expansion, endpoint and merge-method choices) and their persisted shape.
//!
//! Upstream's functions return the same object when nothing changed, and tests
//! check that identity. Here the mutators take `&mut UiState` and return whether
//! anything changed. Persisted JSON is read and written as `serde_json::Value`
//! so the sanitizers see what the file holds. The file I/O, debouncer and
//! zustand subscription are the settings task's (T20) seam.
// Consumers are wired by the settings and shell tasks (T20/T22).
#![allow(dead_code)]

use chrono::{DateTime, SecondsFormat};
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};
use turin_contracts::pull_request::PullRequestMergeMethod;

use crate::state::projects::normalize_project_path_for_comparison;
use crate::state::time::parse_ms;

/// Version 1 stored card visibility, not folder expansion.
const THREAD_CHANGED_FILES_EXPANSION_VERSION: i64 = 2;
const LEGACY_PROJECT_CWD_PREFERENCE_PREFIX: &str = "legacy-project-cwd:";
const LEGACY_PROJECT_EXPANSION_DEFAULT_KEY: &str = "legacy-project-expansion-default";

/// `UiState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiState {
    pub project_expanded_by_id: IndexMap<String, bool>,
    pub project_order: Vec<String>,
    /// Logical project key the sidebar list is scoped to, or `None` for all
    /// projects. Lives here so routes that unmount the sidebar cannot reset it.
    pub sidebar_project_scope_key: Option<String>,
    pub thread_last_visited_at_by_id: IndexMap<String, String>,
    pub thread_changed_files_expanded_by_id: IndexMap<String, IndexMap<String, bool>>,
    pub default_advertised_endpoint_key: Option<String>,
    pub pull_request_merge_method: PullRequestMergeMethod,
}

impl Default for UiState {
    /// `initialState`.
    fn default() -> Self {
        Self {
            project_expanded_by_id: IndexMap::new(),
            project_order: Vec::new(),
            sidebar_project_scope_key: None,
            thread_last_visited_at_by_id: IndexMap::new(),
            thread_changed_files_expanded_by_id: IndexMap::new(),
            default_advertised_endpoint_key: None,
            pull_request_merge_method: PullRequestMergeMethod::Merge,
        }
    }
}

/// `PersistedUiState`, read loosely the way upstream sanitizes it. `None` is an
/// absent key; a present `null` is `Some(Value::Null)`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedUiState {
    #[serde(default, deserialize_with = "present")]
    pub project_expanded_by_id: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub project_order: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub thread_last_visited_at_by_id: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub collapsed_project_cwds: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub expanded_project_cwds: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub project_order_cwds: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub default_advertised_endpoint_key: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub sidebar_project_scope_key: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub thread_changed_files_expansion_version: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub thread_changed_files_expanded_by_id: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub pull_request_merge_method: Option<Value>,
}

/// Keeps an explicit `null` as `Some(Value::Null)`, so only a missing key is `None`.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// `legacyProjectCwdPreferenceKey`.
pub fn legacy_project_cwd_preference_key(cwd: &str) -> String {
    format!(
        "{LEGACY_PROJECT_CWD_PREFERENCE_PREFIX}{}",
        normalize_project_path_for_comparison(cwd)
    )
}

/// `sanitizeStringArray`: strings only, non-empty, first occurrence kept.
fn sanitize_string_array(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(entries)) = value else {
        return Vec::new();
    };
    let mut unique: Vec<String> = Vec::new();
    for entry in entries {
        if let Value::String(text) = entry
            && !text.is_empty()
            && !unique.contains(text)
        {
            unique.push(text.clone());
        }
    }
    unique
}

/// `Object.entries` for the sanitizers. Upstream guards with `typeof value === "object"`,
/// which also admits arrays, so an array yields its index keys. Anything else yields none.
fn object_entries(value: Option<&Value>) -> Vec<(String, &Value)> {
    match value {
        Some(Value::Object(entries)) => entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry))
            .collect(),
        Some(Value::Array(entries)) => entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (index.to_string(), entry))
            .collect(),
        _ => Vec::new(),
    }
}

/// `sanitizeBooleanRecord`: boolean values under non-empty keys.
fn sanitize_boolean_record(value: Option<&Value>) -> IndexMap<String, bool> {
    object_entries(value)
        .into_iter()
        .filter_map(|(key, entry)| match entry {
            Value::Bool(expanded) if !key.is_empty() => Some((key, *expanded)),
            _ => None,
        })
        .collect()
}

/// `sanitizeOptionalKey`: a non-empty string, else `None`.
fn sanitize_optional_key(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// `sanitizeTimestampRecord`: string timestamps that `Date.parse` accepts.
fn sanitize_timestamp_record(value: Option<&Value>) -> IndexMap<String, String> {
    object_entries(value)
        .into_iter()
        .filter_map(|(key, entry)| match entry {
            Value::String(text)
                if !key.is_empty() && !text.is_empty() && parse_ms(text).is_some() =>
            {
                Some((key, text.clone()))
            }
            _ => None,
        })
        .collect()
}

fn is_pull_request_merge_method(value: &Value) -> Option<PullRequestMergeMethod> {
    match value.as_str()? {
        "merge" => Some(PullRequestMergeMethod::Merge),
        "squash" => Some(PullRequestMergeMethod::Squash),
        "rebase" => Some(PullRequestMergeMethod::Rebase),
        _ => None,
    }
}

/// `sanitizePersistedThreadChangedFilesExpanded`: drops threads with no
/// boolean turn entries.
fn sanitize_thread_changed_files_expanded(
    value: Option<&Value>,
) -> IndexMap<String, IndexMap<String, bool>> {
    let mut next = IndexMap::new();
    for (thread_id, turns) in object_entries(value) {
        if thread_id.is_empty() {
            continue;
        }
        let next_turns: IndexMap<String, bool> = object_entries(Some(turns))
            .into_iter()
            .filter_map(|(turn_id, expanded)| match expanded {
                Value::Bool(expanded) if !turn_id.is_empty() => Some((turn_id, *expanded)),
                _ => None,
            })
            .collect();
        if !next_turns.is_empty() {
            next.insert(thread_id, next_turns);
        }
    }
    next
}

/// `parsePersistedState`: sanitizes a stored snapshot, migrating the legacy
/// CWD preference keys when the current project map is absent.
pub fn parse_persisted_state(parsed: &PersistedUiState) -> UiState {
    let project_expanded_by_id = match parsed.project_expanded_by_id.as_ref() {
        None => {
            let mut migrated = IndexMap::new();
            let collapsed = sanitize_string_array(parsed.collapsed_project_cwds.as_ref());
            let expanded = sanitize_string_array(parsed.expanded_project_cwds.as_ref());
            for cwd in &collapsed {
                migrated.insert(legacy_project_cwd_preference_key(cwd), false);
            }
            for cwd in &expanded {
                migrated.insert(legacy_project_cwd_preference_key(cwd), true);
            }
            let collapsed_is_array = matches!(
                parsed.collapsed_project_cwds.as_ref(),
                Some(Value::Array(_))
            );
            if !collapsed_is_array && !expanded.is_empty() {
                migrated.insert(LEGACY_PROJECT_EXPANSION_DEFAULT_KEY.to_owned(), false);
            }
            migrated
        }
        Some(value) => sanitize_boolean_record(Some(value)),
    };
    let project_order = match parsed.project_order.as_ref() {
        None => sanitize_string_array(parsed.project_order_cwds.as_ref())
            .iter()
            .map(|cwd| legacy_project_cwd_preference_key(cwd))
            .collect(),
        Some(value) => sanitize_string_array(Some(value)),
    };
    let version_is_current = parsed
        .thread_changed_files_expansion_version
        .as_ref()
        .and_then(Value::as_i64)
        == Some(THREAD_CHANGED_FILES_EXPANSION_VERSION);

    UiState {
        project_expanded_by_id,
        project_order,
        thread_last_visited_at_by_id: sanitize_timestamp_record(
            parsed.thread_last_visited_at_by_id.as_ref(),
        ),
        thread_changed_files_expanded_by_id: if version_is_current {
            sanitize_thread_changed_files_expanded(
                parsed.thread_changed_files_expanded_by_id.as_ref(),
            )
        } else {
            IndexMap::new()
        },
        default_advertised_endpoint_key: sanitize_optional_key(
            parsed.default_advertised_endpoint_key.as_ref(),
        ),
        sidebar_project_scope_key: sanitize_optional_key(parsed.sidebar_project_scope_key.as_ref()),
        pull_request_merge_method: parsed
            .pull_request_merge_method
            .as_ref()
            .and_then(is_pull_request_merge_method)
            .unwrap_or(PullRequestMergeMethod::Merge),
    }
}

/// The JSON `persistState` writes. Key order follows upstream. The temporary
/// expanded-only fallback key is dropped, so a rewrite never keeps it.
pub fn persisted_state_json(state: &UiState) -> Value {
    let project_expanded_by_id: Map<String, Value> = state
        .project_expanded_by_id
        .iter()
        .filter(|(key, _)| key.as_str() != LEGACY_PROJECT_EXPANSION_DEFAULT_KEY)
        .map(|(key, expanded)| (key.clone(), Value::Bool(*expanded)))
        .collect();
    let timestamps: Map<String, Value> = state
        .thread_last_visited_at_by_id
        .iter()
        .map(|(key, at)| (key.clone(), Value::String(at.clone())))
        .collect();
    let changed_files: Map<String, Value> = state
        .thread_changed_files_expanded_by_id
        .iter()
        .map(|(thread_id, turns)| {
            let turns: Map<String, Value> = turns
                .iter()
                .map(|(turn_id, expanded)| (turn_id.clone(), Value::Bool(*expanded)))
                .collect();
            (thread_id.clone(), Value::Object(turns))
        })
        .collect();

    let mut object = Map::new();
    object.insert(
        "projectExpandedById".to_owned(),
        Value::Object(project_expanded_by_id),
    );
    object.insert(
        "projectOrder".to_owned(),
        Value::Array(
            state
                .project_order
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    object.insert(
        "threadLastVisitedAtById".to_owned(),
        Value::Object(timestamps),
    );
    object.insert(
        "defaultAdvertisedEndpointKey".to_owned(),
        state
            .default_advertised_endpoint_key
            .clone()
            .map_or(Value::Null, Value::String),
    );
    object.insert(
        "sidebarProjectScopeKey".to_owned(),
        state
            .sidebar_project_scope_key
            .clone()
            .map_or(Value::Null, Value::String),
    );
    object.insert(
        "threadChangedFilesExpansionVersion".to_owned(),
        Value::from(THREAD_CHANGED_FILES_EXPANSION_VERSION),
    );
    object.insert(
        "threadChangedFilesExpandedById".to_owned(),
        Value::Object(changed_files),
    );
    object.insert(
        "pullRequestMergeMethod".to_owned(),
        serde_json::to_value(state.pull_request_merge_method).unwrap_or(Value::Null),
    );
    Value::Object(object)
}

/// `markThreadVisited`. Keeps the later of the stored and the new timestamp.
/// Returns whether the state changed.
pub fn mark_thread_visited(state: &mut UiState, thread_id: &str, visited_at: &str) -> bool {
    let Some(visited_at_ms) = parse_ms(visited_at) else {
        return false;
    };
    let previous_ms = state
        .thread_last_visited_at_by_id
        .get(thread_id)
        .and_then(|previous| parse_ms(previous));
    if previous_ms.is_some_and(|previous| previous >= visited_at_ms) {
        return false;
    }
    state
        .thread_last_visited_at_by_id
        .insert(thread_id.to_owned(), visited_at.to_owned());
    true
}

/// `markThreadUnread`: a completed thread reads as visited one millisecond
/// before its completion. Returns whether the state changed.
pub fn mark_thread_unread(
    state: &mut UiState,
    thread_id: &str,
    latest_turn_completed_at: Option<&str>,
) -> bool {
    let Some(completed_at) = latest_turn_completed_at.filter(|at| !at.is_empty()) else {
        return false;
    };
    let Some(completed_ms) = parse_ms(completed_at) else {
        return false;
    };
    let Some(unread_at) = DateTime::from_timestamp_millis(completed_ms - 1) else {
        return false;
    };
    let unread_visited_at = unread_at.to_rfc3339_opts(SecondsFormat::Millis, true);
    if state.thread_last_visited_at_by_id.get(thread_id) == Some(&unread_visited_at) {
        return false;
    }
    state
        .thread_last_visited_at_by_id
        .insert(thread_id.to_owned(), unread_visited_at);
    true
}

/// `setThreadChangedFilesExpanded`. Returns whether the state changed.
pub fn set_thread_changed_files_expanded(
    state: &mut UiState,
    thread_id: &str,
    turn_id: &str,
    expanded: bool,
) -> bool {
    let turns = state
        .thread_changed_files_expanded_by_id
        .entry(thread_id.to_owned())
        .or_default();
    if turns.get(turn_id) == Some(&expanded) {
        return false;
    }
    turns.insert(turn_id.to_owned(), expanded);
    true
}

/// `setDefaultAdvertisedEndpointKey`. An empty key clears the choice.
pub fn set_default_advertised_endpoint_key(state: &mut UiState, key: Option<&str>) -> bool {
    let next = key.filter(|key| !key.is_empty()).map(str::to_owned);
    if state.default_advertised_endpoint_key == next {
        return false;
    }
    state.default_advertised_endpoint_key = next;
    true
}

/// `setSidebarProjectScopeKey`. An empty key means all projects.
pub fn set_sidebar_project_scope_key(state: &mut UiState, project_key: Option<&str>) -> bool {
    let next = project_key.filter(|key| !key.is_empty()).map(str::to_owned);
    if state.sidebar_project_scope_key == next {
        return false;
    }
    state.sidebar_project_scope_key = next;
    true
}

/// `setPullRequestMergeMethod`.
pub fn set_pull_request_merge_method(state: &mut UiState, method: PullRequestMergeMethod) -> bool {
    if state.pull_request_merge_method == method {
        return false;
    }
    state.pull_request_merge_method = method;
    true
}

/// `resolveProjectExpanded`: the first preference key that has a value wins,
/// then the legacy default, then expanded.
pub fn resolve_project_expanded(
    project_expanded_by_id: &IndexMap<String, bool>,
    preference_keys: &[&str],
) -> bool {
    for key in preference_keys {
        if let Some(expanded) = project_expanded_by_id.get(*key) {
            return *expanded;
        }
    }
    project_expanded_by_id
        .get(LEGACY_PROJECT_EXPANSION_DEFAULT_KEY)
        .copied()
        .unwrap_or(true)
}

/// `setProjectExpanded` for every key of a logical project. Returns whether the
/// state changed.
pub fn set_project_expanded(state: &mut UiState, project_ids: &[&str], expanded: bool) -> bool {
    let mut changed = false;
    for project_id in project_ids {
        if state.project_expanded_by_id.get(*project_id) != Some(&expanded) {
            state
                .project_expanded_by_id
                .insert((*project_id).to_owned(), expanded);
            changed = true;
        }
    }
    changed
}

/// `reorderProjects`: moves the dragged group in front of the target. Groups
/// move together. Returns whether the order was set.
pub fn reorder_projects(
    state: &mut UiState,
    current_project_order: &[String],
    dragged_project_ids: &[String],
    target_project_ids: &[String],
) -> bool {
    if dragged_project_ids.is_empty() {
        return false;
    }
    if dragged_project_ids
        .iter()
        .all(|id| target_project_ids.contains(id))
    {
        return false;
    }

    let Some(original_target_index) = current_project_order
        .iter()
        .position(|id| target_project_ids.contains(id))
    else {
        return false;
    };

    let mut project_order = current_project_order.to_vec();
    let mut removed: Vec<String> = Vec::new();
    let mut dragged_before_target = 0usize;
    for index in (0..project_order.len()).rev() {
        if dragged_project_ids.contains(&project_order[index]) {
            removed.insert(0, project_order.remove(index));
            if index < original_target_index {
                dragged_before_target += 1;
            }
        }
    }
    if removed.is_empty() {
        return false;
    }

    let insert_index = original_target_index - dragged_before_target.saturating_sub(1);
    for (offset, id) in removed.into_iter().enumerate() {
        project_order.insert(insert_index + offset, id);
    }
    state.project_order = project_order;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> UiState {
        UiState::default()
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn parse(value: Value) -> UiState {
        let persisted: PersistedUiState = serde_json::from_value(value).expect("persisted shape");
        parse_persisted_state(&persisted)
    }

    #[test]
    fn stores_server_timestamps_without_moving_visit_state_backwards() {
        let mut ui = state();
        assert!(mark_thread_visited(
            &mut ui,
            "thread-1",
            "2026-02-25T12:30:00.700Z"
        ));
        assert_eq!(
            ui.thread_last_visited_at_by_id
                .get("thread-1")
                .map(String::as_str),
            Some("2026-02-25T12:30:00.700Z")
        );
        assert!(!mark_thread_visited(
            &mut ui,
            "thread-1",
            "2026-02-25T12:30:00.000Z"
        ));
        assert!(!mark_thread_visited(&mut ui, "thread-1", "not-a-date"));
    }

    #[test]
    fn marks_a_completed_thread_unread_using_the_server_completion_timestamp() {
        let mut ui = state();
        ui.thread_last_visited_at_by_id
            .insert("thread-1".to_owned(), "2026-02-25T12:35:00.000Z".to_owned());

        assert!(mark_thread_unread(
            &mut ui,
            "thread-1",
            Some("2026-02-25T12:30:00.000Z")
        ));
        assert_eq!(
            ui.thread_last_visited_at_by_id
                .get("thread-1")
                .map(String::as_str),
            Some("2026-02-25T12:29:59.999Z")
        );
        assert!(!mark_thread_unread(&mut ui, "thread-1", None));
    }

    #[test]
    fn resolves_project_expansion_from_logical_physical_and_legacy_preference_keys() {
        let physical_key = "environment:/repo/project";
        let legacy_key = legacy_project_cwd_preference_key("/repo/project");

        let mut logical_first = IndexMap::new();
        logical_first.insert("logical".to_owned(), false);
        logical_first.insert(physical_key.to_owned(), true);
        assert!(!resolve_project_expanded(&logical_first, &["logical"]));

        let mut physical = IndexMap::new();
        physical.insert(physical_key.to_owned(), false);
        assert!(!resolve_project_expanded(
            &physical,
            &["new-logical", physical_key]
        ));

        let mut legacy = IndexMap::new();
        legacy.insert(legacy_key.clone(), false);
        assert!(!resolve_project_expanded(
            &legacy,
            &["new-logical", legacy_key.as_str()]
        ));

        assert!(resolve_project_expanded(&IndexMap::new(), &["new-logical"]));
    }

    #[test]
    fn sets_expansion_for_every_stable_key_belonging_to_a_logical_project() {
        let mut ui = state();
        let keys = ["logical", "environment-a:/repo", "environment-b:/repo"];

        assert!(set_project_expanded(&mut ui, &keys, false));
        let expected: IndexMap<String, bool> =
            keys.iter().map(|key| ((*key).to_owned(), false)).collect();
        assert_eq!(ui.project_expanded_by_id, expected);
        assert!(!set_project_expanded(&mut ui, &keys, false));
    }

    #[test]
    fn reorders_from_the_current_atom_derived_project_order() {
        let mut ui = state();
        let current = ids(&["project-1", "project-2", "project-3"]);

        assert!(reorder_projects(
            &mut ui,
            &current,
            &ids(&["project-1"]),
            &ids(&["project-3"]),
        ));
        assert_eq!(
            ui.project_order,
            ids(&["project-2", "project-3", "project-1"])
        );
    }

    #[test]
    fn moves_grouped_project_members_together() {
        let mut ui = state();
        let current = ids(&[
            "env-local:proj-a",
            "env-remote:proj-a",
            "env-local:proj-b",
            "env-local:proj-c",
        ]);

        assert!(reorder_projects(
            &mut ui,
            &current,
            &ids(&["env-local:proj-a", "env-remote:proj-a"]),
            &ids(&["env-local:proj-c"]),
        ));
        assert_eq!(
            ui.project_order,
            ids(&[
                "env-local:proj-b",
                "env-local:proj-c",
                "env-local:proj-a",
                "env-remote:proj-a",
            ])
        );
    }

    #[test]
    fn does_not_reorder_missing_or_identical_groups() {
        let mut ui = state();
        let current = ids(&["env-local:proj-a", "env-local:proj-b"]);

        assert!(!reorder_projects(
            &mut ui,
            &current,
            &ids(&["env-local:missing"]),
            &ids(&["env-local:proj-b"]),
        ));
        assert!(!reorder_projects(
            &mut ui,
            &current,
            &ids(&["env-local:proj-a"]),
            &ids(&["env-local:proj-a"]),
        ));
        assert!(ui.project_order.is_empty());
    }

    #[test]
    fn stores_explicit_changed_file_expansion_choices() {
        let mut ui = state();

        assert!(set_thread_changed_files_expanded(
            &mut ui, "thread-1", "turn-1", false
        ));
        let mut turns = IndexMap::new();
        turns.insert("turn-1".to_owned(), false);
        assert_eq!(
            ui.thread_changed_files_expanded_by_id.get("thread-1"),
            Some(&turns)
        );

        assert!(set_thread_changed_files_expanded(
            &mut ui, "thread-1", "turn-1", true
        ));
        assert!(ui.thread_changed_files_expanded_by_id["thread-1"]["turn-1"]);
    }

    #[test]
    fn stores_the_endpoint_preference_by_stable_key() {
        let mut ui = state();

        assert!(set_default_advertised_endpoint_key(
            &mut ui,
            Some("desktop-core:lan:http")
        ));
        assert_eq!(
            ui.default_advertised_endpoint_key.as_deref(),
            Some("desktop-core:lan:http")
        );
        assert!(!set_default_advertised_endpoint_key(
            &mut ui,
            Some("desktop-core:lan:http")
        ));
        assert!(set_default_advertised_endpoint_key(&mut ui, Some("")));
        assert_eq!(ui.default_advertised_endpoint_key, None);
    }

    #[test]
    fn stores_the_sidebar_project_scope_and_resets_it_to_all_projects() {
        let mut ui = state();

        assert!(set_sidebar_project_scope_key(
            &mut ui,
            Some("github.com/pingdotgg/t3code")
        ));
        assert_eq!(
            ui.sidebar_project_scope_key.as_deref(),
            Some("github.com/pingdotgg/t3code")
        );
        assert!(!set_sidebar_project_scope_key(
            &mut ui,
            Some("github.com/pingdotgg/t3code")
        ));
        assert!(set_sidebar_project_scope_key(&mut ui, None));
        assert_eq!(ui.sidebar_project_scope_key, None);
        assert!(!set_sidebar_project_scope_key(&mut ui, Some("")));
    }

    #[test]
    fn hydrates_the_last_selected_pull_request_merge_method() {
        assert_eq!(
            parse(json!({ "pullRequestMergeMethod": "squash" })).pull_request_merge_method,
            PullRequestMergeMethod::Squash
        );
        assert_eq!(
            parse(json!({ "pullRequestMergeMethod": "fast-forward" })).pull_request_merge_method,
            PullRequestMergeMethod::Merge
        );
    }

    #[test]
    fn hydrates_raw_ui_owned_state_without_server_entities() {
        let parsed = parse(json!({
            "projectExpandedById": { "logical": false, "invalid": "no" },
            "projectOrder": ["physical-b", "", "physical-a", "physical-b"],
            "threadLastVisitedAtById": {
                "environment:thread-1": "2026-02-25T12:35:00.000Z",
                "invalid": "not-a-date",
            },
            "defaultAdvertisedEndpointKey": "desktop-core:lan:http",
            "threadChangedFilesExpansionVersion": 2,
            "threadChangedFilesExpandedById": {
                "environment:thread-1": { "turn-1": false, "turn-2": true },
            },
        }));

        let mut expanded = IndexMap::new();
        expanded.insert("logical".to_owned(), false);
        let mut turns = IndexMap::new();
        turns.insert("turn-1".to_owned(), false);
        turns.insert("turn-2".to_owned(), true);
        let mut changed = IndexMap::new();
        changed.insert("environment:thread-1".to_owned(), turns);
        let mut visited = IndexMap::new();
        visited.insert(
            "environment:thread-1".to_owned(),
            "2026-02-25T12:35:00.000Z".to_owned(),
        );

        assert_eq!(
            parsed,
            UiState {
                project_expanded_by_id: expanded,
                project_order: ids(&["physical-b", "physical-a"]),
                thread_last_visited_at_by_id: visited,
                thread_changed_files_expanded_by_id: changed,
                default_advertised_endpoint_key: Some("desktop-core:lan:http".to_owned()),
                sidebar_project_scope_key: None,
                pull_request_merge_method: PullRequestMergeMethod::Merge,
            }
        );
    }

    #[test]
    fn ignores_changed_file_expansion_when_the_version_is_not_current() {
        for version in [None, Some(json!(1))] {
            let mut value = json!({
                "threadChangedFilesExpandedById": {
                    "environment:thread-1": { "turn-1": false },
                },
            });
            if let Some(version) = version {
                value["threadChangedFilesExpansionVersion"] = version;
            }
            assert!(parse(value).thread_changed_files_expanded_by_id.is_empty());
        }
    }

    #[test]
    fn migrates_legacy_cwd_project_preferences_into_local_alias_keys() {
        let parsed = parse(json!({
            "collapsedProjectCwds": ["/repo/b"],
            "expandedProjectCwds": ["/repo/a"],
            "projectOrderCwds": ["/repo/b", "/repo/a"],
        }));
        let project_a = legacy_project_cwd_preference_key("/repo/a");
        let project_b = legacy_project_cwd_preference_key("/repo/b");

        assert_eq!(
            parsed.project_order,
            vec![project_b.clone(), project_a.clone()]
        );
        assert!(resolve_project_expanded(
            &parsed.project_expanded_by_id,
            &[project_a.as_str()]
        ));
        assert!(!resolve_project_expanded(
            &parsed.project_expanded_by_id,
            &[project_b.as_str()]
        ));
        assert!(resolve_project_expanded(
            &parsed.project_expanded_by_id,
            &["unknown"]
        ));
    }

    #[test]
    fn preserves_legacy_expanded_only_semantics_for_one_way_migration() {
        let parsed = parse(json!({ "expandedProjectCwds": ["/repo/a"] }));

        assert!(resolve_project_expanded(
            &parsed.project_expanded_by_id,
            &[legacy_project_cwd_preference_key("/repo/a").as_str()]
        ));
        assert!(!resolve_project_expanded(
            &parsed.project_expanded_by_id,
            &[legacy_project_cwd_preference_key("/repo/b").as_str()]
        ));
    }

    #[test]
    fn persists_raw_ui_preferences_including_thread_visit_markers() {
        let mut ui = state();
        ui.project_expanded_by_id
            .insert("logical".to_owned(), false);
        ui.project_order = ids(&["physical-b", "physical-a"]);
        ui.thread_last_visited_at_by_id.insert(
            "environment:thread-1".to_owned(),
            "2026-02-25T12:35:00.000Z".to_owned(),
        );
        let mut turns = IndexMap::new();
        turns.insert("turn-1".to_owned(), false);
        turns.insert("turn-2".to_owned(), true);
        ui.thread_changed_files_expanded_by_id
            .insert("environment:thread-1".to_owned(), turns);
        ui.default_advertised_endpoint_key = Some("desktop-core:lan:http".to_owned());

        let persisted = persisted_state_json(&ui);
        assert_eq!(
            persisted,
            json!({
                "projectExpandedById": { "logical": false },
                "projectOrder": ["physical-b", "physical-a"],
                "threadLastVisitedAtById": {
                    "environment:thread-1": "2026-02-25T12:35:00.000Z",
                },
                "defaultAdvertisedEndpointKey": "desktop-core:lan:http",
                "sidebarProjectScopeKey": null,
                "threadChangedFilesExpansionVersion": 2,
                "threadChangedFilesExpandedById": {
                    "environment:thread-1": { "turn-1": false, "turn-2": true },
                },
                "pullRequestMergeMethod": "merge",
            })
        );
        assert_eq!(parse(persisted), ui);
    }

    #[test]
    fn restores_the_sidebar_project_scope_across_reloads() {
        let mut ui = state();
        ui.sidebar_project_scope_key = Some("github.com/pingdotgg/t3code".to_owned());

        assert_eq!(
            parse(persisted_state_json(&ui))
                .sidebar_project_scope_key
                .as_deref(),
            Some("github.com/pingdotgg/t3code")
        );
    }

    #[test]
    fn drops_the_temporary_expanded_only_migration_fallback_when_rewriting_state() {
        let migrated = parse(json!({ "expandedProjectCwds": ["/repo/a"] }));

        let persisted = persisted_state_json(&migrated);
        let stored = persisted["projectExpandedById"]
            .as_object()
            .expect("object");
        assert!(!stored.contains_key(LEGACY_PROJECT_EXPANSION_DEFAULT_KEY));
        assert!(
            !persisted
                .as_object()
                .expect("object")
                .contains_key("threadPanelOpen")
        );
        let reread = parse(persisted);
        assert!(resolve_project_expanded(
            &reread.project_expanded_by_id,
            &["unknown"]
        ));
    }

    #[test]
    fn sanitizers_read_array_entries_by_index_like_object_entries() {
        // Upstream's `typeof value === "object"` admits arrays, so `Object.entries` yields index keys.
        let parsed = parse(json!({
            "projectExpandedById": [true, false, "no"],
            "threadLastVisitedAtById": ["2026-02-25T12:35:00.000Z", ""],
            "threadChangedFilesExpansionVersion": 2,
            "threadChangedFilesExpandedById": [{ "turn-1": true }, ["not", "a", "record"]],
        }));

        let mut expanded = IndexMap::new();
        expanded.insert("0".to_owned(), true);
        expanded.insert("1".to_owned(), false);
        let mut visited = IndexMap::new();
        visited.insert("0".to_owned(), "2026-02-25T12:35:00.000Z".to_owned());
        let mut turns = IndexMap::new();
        turns.insert("turn-1".to_owned(), true);
        let mut changed = IndexMap::new();
        changed.insert("0".to_owned(), turns);

        assert_eq!(parsed.project_expanded_by_id, expanded);
        assert_eq!(parsed.thread_last_visited_at_by_id, visited);
        assert_eq!(parsed.thread_changed_files_expanded_by_id, changed);
    }
}
