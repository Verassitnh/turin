//! Port of upstream/apps/web/src/components/pullRequest/pullRequestListPreferences.ts
//! The browser `localStorage` default and the `window` guard are replaced by the
//! `PreferenceStorage` trait: the caller supplies the store. The `Schema` decode runs over
//! serde, with `optional_key` rejecting explicit `null` the way `Schema.optional` does.
//! The `PullRequestListPreferencePatch` type is omitted: no upstream code reads it.

use crate::state::contracts::{EnvironmentId, ProjectId};
use serde::{Deserialize, Serialize};
use std::io;
use turin_contracts::pull_request::{
    PullRequestChecksFilter, PullRequestDraftFilter, PullRequestInvolvement, PullRequestListState,
    PullRequestReviewFilter,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestListSort {
    Ready,
    Blocked,
    Updated,
    Newest,
    Oldest,
    Largest,
    Smallest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListPreferences {
    pub involvement: PullRequestInvolvement,
    pub state: PullRequestListState,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub environment_id: Option<EnvironmentId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub project_id: Option<ProjectId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub host: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub q: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub draft: Option<PullRequestDraftFilter>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub review: Option<PullRequestReviewFilter>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub checks: Option<PullRequestChecksFilter>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub author: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub labels: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_key::deserialize"
    )]
    pub sort: Option<PullRequestListSort>,
}

/// `Schema.optional(X)`: an absent key is `None` (via `#[serde(default)]`), but an explicit
/// `null` is a decode error, not an absent value. Plain `Option<T>` would accept it.
mod optional_key {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        T::deserialize(deserializer).map(Some)
    }
}

impl Default for PullRequestListPreferences {
    fn default() -> Self {
        Self {
            involvement: PullRequestInvolvement::All,
            state: PullRequestListState::Open,
            environment_id: None,
            project_id: None,
            host: None,
            q: None,
            draft: None,
            review: None,
            checks: None,
            author: None,
            labels: None,
            sort: None,
        }
    }
}

/// The key upstream stores the list controls under in `localStorage`.
pub const PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY: &str = "t3.pullRequests.preferences";

/// The two `Storage` methods upstream uses. `get_item` errors are denied or failed storage
/// (upstream's `try`/`catch`); `set_item` errors are swallowed by the writer.
pub trait PreferenceStorage {
    fn get_item(&self, key: &str) -> io::Result<Option<String>>;
    fn set_item(&self, key: &str, value: &str) -> io::Result<()>;
}

/// `JS string.length` limit for free-text preferences (UTF-16 code units).
const BOUNDED_PREFERENCE_MAX_LENGTH: usize = 200;
/// Maximum number of remembered label filters.
const LABELS_MAX_LENGTH: usize = 10;

fn bounded(value: &str) -> bool {
    value.encode_utf16().count() <= BOUNDED_PREFERENCE_MAX_LENGTH
}

/// JS truthiness for an optional string: `""` is dropped like `undefined`.
fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

/// Only list controls are remembered. The selected row remains a URL and right-panel concern.
pub fn pull_request_list_preferences(
    search: &PullRequestListPreferences,
) -> PullRequestListPreferences {
    PullRequestListPreferences {
        involvement: search.involvement,
        state: search.state,
        environment_id: search.environment_id.clone(),
        project_id: search.project_id.clone(),
        host: nonempty(search.host.clone()),
        q: nonempty(search.q.clone()),
        draft: search.draft,
        review: search.review,
        checks: search.checks,
        author: nonempty(search.author.clone()),
        labels: search
            .labels
            .as_ref()
            .filter(|labels| !labels.is_empty())
            .cloned(),
        // The default sort is not stored, so an older build and a missing key read the same.
        sort: search
            .sort
            .filter(|sort| *sort != PullRequestListSort::Ready),
    }
}

/// Parse the stored JSON. Malformed, failed or out-of-bounds data gives the upstream defaults,
/// and so does storage that refuses to read.
pub fn read_pull_request_list_preferences(
    storage: &dyn PreferenceStorage,
) -> PullRequestListPreferences {
    let Ok(Some(raw)) = storage.get_item(PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY) else {
        return PullRequestListPreferences::default();
    };
    if raw.is_empty() {
        return PullRequestListPreferences::default();
    }
    let Ok(decoded) = serde_json::from_str::<PullRequestListPreferences>(&raw) else {
        return PullRequestListPreferences::default();
    };
    if decoded.host.as_deref().is_some_and(|s| !bounded(s))
        || decoded.q.as_deref().is_some_and(|s| !bounded(s))
        || decoded.author.as_deref().is_some_and(|s| !bounded(s))
        || decoded.labels.as_ref().is_some_and(|labels| {
            labels.len() > LABELS_MAX_LENGTH || labels.iter().any(|s| !bounded(s))
        })
    {
        return PullRequestListPreferences::default();
    }
    pull_request_list_preferences(&decoded)
}

/// Serialize the caller's preferences as written; upstream normalizes on read, not write.
/// Storage that is full or denied is ignored: the URL stays the source of truth for this visit.
pub fn write_pull_request_list_preferences(
    preferences: &PullRequestListPreferences,
    storage: &dyn PreferenceStorage,
) {
    let Ok(raw) = serde_json::to_string(preferences) else {
        return;
    };
    let _ = storage.set_item(PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY, &raw);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Upstream's `makeStorage()`: a map-backed `getItem`/`setItem`.
    #[derive(Default)]
    struct MemoryStorage {
        held: RefCell<HashMap<String, String>>,
    }

    impl PreferenceStorage for MemoryStorage {
        fn get_item(&self, key: &str) -> io::Result<Option<String>> {
            Ok(self.held.borrow().get(key).cloned())
        }
        fn set_item(&self, key: &str, value: &str) -> io::Result<()> {
            self.held.borrow_mut().insert(key.to_owned(), value.to_owned());
            Ok(())
        }
    }

    /// Upstream's `denied`: both methods throw.
    struct DeniedStorage;

    impl PreferenceStorage for DeniedStorage {
        fn get_item(&self, _key: &str) -> io::Result<Option<String>> {
            Err(io::Error::other("storage denied"))
        }
        fn set_item(&self, _key: &str, _value: &str) -> io::Result<()> {
            Err(io::Error::other("storage denied"))
        }
    }

    fn controls(involvement: PullRequestInvolvement, state: PullRequestListState) -> PullRequestListPreferences {
        PullRequestListPreferences {
            involvement,
            state,
            ..PullRequestListPreferences::default()
        }
    }

    fn default_controls() -> PullRequestListPreferences {
        controls(PullRequestInvolvement::All, PullRequestListState::Open)
    }

    #[test]
    fn restores_every_filter_and_the_selected_sort() {
        let storage = MemoryStorage::default();
        let preferences = PullRequestListPreferences {
            involvement: PullRequestInvolvement::Reviewing,
            state: PullRequestListState::Merged,
            environment_id: Some(EnvironmentId::new("env-1")),
            project_id: Some(ProjectId::new("project-1")),
            host: Some("github.com".into()),
            q: Some("workflow".into()),
            draft: Some(PullRequestDraftFilter::Hide),
            review: Some(PullRequestReviewFilter::Approved),
            checks: Some(PullRequestChecksFilter::Passing),
            author: Some("octocat".into()),
            labels: Some(vec!["bug".into(), "priority".into()]),
            sort: Some(PullRequestListSort::Largest),
        };

        write_pull_request_list_preferences(&preferences, &storage);
        assert_eq!(read_pull_request_list_preferences(&storage), preferences);
    }

    #[test]
    fn omits_the_default_sort_from_storage() {
        let preferences = PullRequestListPreferences {
            sort: Some(PullRequestListSort::Ready),
            ..default_controls()
        };
        assert_eq!(
            pull_request_list_preferences(&preferences),
            default_controls()
        );
    }

    #[test]
    fn keeps_an_explicit_recency_sort() {
        let preferences = PullRequestListPreferences {
            sort: Some(PullRequestListSort::Updated),
            ..default_controls()
        };
        assert_eq!(
            pull_request_list_preferences(&preferences),
            PullRequestListPreferences {
                sort: Some(PullRequestListSort::Updated),
                ..default_controls()
            }
        );
    }

    #[test]
    fn falls_back_to_the_default_controls_when_storage_is_corrupt() {
        let storage = MemoryStorage::default();
        storage.set_item(PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY, "{not json").unwrap();
        assert_eq!(read_pull_request_list_preferences(&storage), default_controls());
    }

    #[test]
    fn falls_back_when_browser_policy_denies_storage() {
        assert_eq!(
            read_pull_request_list_preferences(&DeniedStorage),
            default_controls()
        );
        // Must not panic: a denied write is ignored.
        write_pull_request_list_preferences(&default_controls(), &DeniedStorage);
    }

    #[test]
    fn explicit_null_for_an_optional_key_discards_the_stored_preferences() {
        // `Schema.optional` rejects `null`, so the whole decode fails and the defaults apply.
        let storage = MemoryStorage::default();
        storage
            .set_item(
                PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY,
                r#"{"involvement":"reviewing","state":"open","host":null}"#,
            )
            .unwrap();
        assert_eq!(read_pull_request_list_preferences(&storage), default_controls());
    }

    #[test]
    fn out_of_bounds_stored_text_discards_the_stored_preferences() {
        let storage = MemoryStorage::default();
        let long = "x".repeat(201);
        storage
            .set_item(
                PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY,
                &format!(r#"{{"involvement":"all","state":"open","q":"{long}"}}"#),
            )
            .unwrap();
        assert_eq!(read_pull_request_list_preferences(&storage), default_controls());
    }

    #[test]
    fn length_bound_counts_utf16_code_units() {
        // Two code units per emoji: 100 emoji is exactly 200 and still in bounds.
        let storage = MemoryStorage::default();
        let emoji = "\u{1F600}".repeat(100);
        storage
            .set_item(
                PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY,
                &format!(r#"{{"involvement":"all","state":"open","q":"{emoji}"}}"#),
            )
            .unwrap();
        assert_eq!(
            read_pull_request_list_preferences(&storage).q.as_deref(),
            Some(emoji.as_str())
        );
    }

    #[test]
    fn stored_absent_storage_key_reads_as_defaults() {
        assert_eq!(
            read_pull_request_list_preferences(&MemoryStorage::default()),
            default_controls()
        );
    }
}
