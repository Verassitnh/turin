//! Port of upstream/apps/web/src/components/usage/usagePagePreferences.ts

use serde_json::{Value, json};
use turin_contracts::usage::UsageProviderKind;

use super::usage_providers::{provider_from_key, provider_key};
use crate::hooks::use_local_storage::LocalStorage;

const STORAGE_KEY: &str = "t3code:usage-page-preferences:v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsagePageMetric {
    Cost,
    Tokens,
    Limits,
}

impl UsagePageMetric {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cost => "cost",
            Self::Tokens => "tokens",
            Self::Limits => "limits",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "cost" => Some(Self::Cost),
            "tokens" => Some(Self::Tokens),
            "limits" => Some(Self::Limits),
            _ => None,
        }
    }
}

pub const WINDOW_DAYS: [u32; 4] = [1, 7, 30, 90];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsagePagePreferences {
    pub metric: UsagePageMetric,
    /// One of [`WINDOW_DAYS`].
    pub window_days: u32,
    /// Providers filtered out of the page. Stored as hidden so new providers
    /// show by default. `None` is an older save without the key.
    pub hidden_providers: Option<Vec<UsageProviderKind>>,
}

// Limits is what most people open the page for (how much subscription quota is
// left, and when it resets), so it is the first-visit default; the last picked
// tab sticks after that.
pub const DEFAULT_PREFERENCES: UsagePagePreferences = UsagePagePreferences {
    metric: UsagePageMetric::Limits,
    window_days: 30,
    hidden_providers: None,
};

/// `Schema.fromJsonString(UsagePagePreferencesSchema)`: anything that does not
/// match the schema fails to decode.
fn decode(raw: &str) -> Option<UsagePagePreferences> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    let metric = UsagePageMetric::from_str(object.get("metric")?.as_str()?)?;
    let window_days = object.get("windowDays")?.as_f64()?;
    let window_days = WINDOW_DAYS
        .into_iter()
        .find(|days| f64::from(*days) == window_days)?;
    let hidden_providers = match object.get("hiddenProviders") {
        None => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| item.as_str().and_then(provider_from_key))
                .collect::<Option<Vec<_>>>()?,
        ),
        Some(_) => return None,
    };
    Some(UsagePagePreferences {
        metric,
        window_days,
        hidden_providers,
    })
}

fn encode(preferences: &UsagePagePreferences) -> String {
    let mut object = json!({
        "metric": preferences.metric.as_str(),
        "windowDays": preferences.window_days,
    });
    if let Some(hidden) = &preferences.hidden_providers {
        object["hiddenProviders"] = hidden.iter().map(|p| provider_key(*p)).collect();
    }
    object.to_string()
}

pub fn read_usage_page_preferences(storage: &dyn LocalStorage) -> UsagePagePreferences {
    match storage.get_item(STORAGE_KEY) {
        Ok(None) => DEFAULT_PREFERENCES,
        Ok(Some(raw)) => decode(&raw).unwrap_or_else(|| {
            tracing::error!("Could not read Usage page preferences.");
            DEFAULT_PREFERENCES
        }),
        Err(error) => {
            tracing::error!(%error, "Could not read Usage page preferences.");
            DEFAULT_PREFERENCES
        }
    }
}

pub fn save_usage_page_preferences(storage: &dyn LocalStorage, preferences: &UsagePagePreferences) {
    if let Err(error) = storage.set_item(STORAGE_KEY, &encode(preferences)) {
        tracing::error!(%error, "Could not save Usage page preferences.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::use_local_storage::{LocalStorageError, MemoryLocalStorage};
    use parking_lot::Mutex;
    use std::collections::HashMap;

    /// A store whose reads or writes can be made to fail, like a full or blocked `localStorage`.
    #[derive(Default)]
    struct FlakyStorage {
        values: Mutex<HashMap<String, String>>,
        fail_writes: Mutex<bool>,
        fail_reads: Mutex<bool>,
    }

    impl LocalStorage for FlakyStorage {
        fn get_item(&self, key: &str) -> Result<Option<String>, LocalStorageError> {
            if *self.fail_reads.lock() {
                return Err(LocalStorageError {
                    operation: "read",
                    key: key.to_owned(),
                });
            }
            Ok(self.values.lock().get(key).cloned())
        }
        fn set_item(&self, key: &str, value: &str) -> Result<(), LocalStorageError> {
            if *self.fail_writes.lock() {
                return Err(LocalStorageError {
                    operation: "write",
                    key: key.to_owned(),
                });
            }
            self.values.lock().insert(key.to_owned(), value.to_owned());
            Ok(())
        }
        fn remove_item(&self, key: &str) -> Result<(), LocalStorageError> {
            self.values.lock().remove(key);
            Ok(())
        }
    }

    fn prefs(metric: UsagePageMetric, window_days: u32) -> UsagePagePreferences {
        UsagePagePreferences {
            metric,
            window_days,
            hidden_providers: None,
        }
    }

    #[test]
    fn uses_defaults_when_no_preference_has_been_saved() {
        let storage = MemoryLocalStorage::default();
        assert_eq!(
            read_usage_page_preferences(&storage),
            prefs(UsagePageMetric::Limits, 30)
        );
    }

    #[test]
    fn round_trips_every_metric_with_every_range() {
        for window_days in WINDOW_DAYS {
            for metric in [
                UsagePageMetric::Cost,
                UsagePageMetric::Tokens,
                UsagePageMetric::Limits,
            ] {
                let storage = MemoryLocalStorage::default();
                save_usage_page_preferences(&storage, &prefs(metric, window_days));
                assert_eq!(
                    read_usage_page_preferences(&storage),
                    prefs(metric, window_days)
                );
            }
        }
    }

    #[test]
    fn round_trips_hidden_providers_and_reads_older_saves_without_them() {
        let storage = MemoryLocalStorage::default();
        save_usage_page_preferences(
            &storage,
            &UsagePagePreferences {
                metric: UsagePageMetric::Cost,
                window_days: 7,
                hidden_providers: Some(vec![UsageProviderKind::Codex]),
            },
        );
        assert_eq!(
            read_usage_page_preferences(&storage).hidden_providers,
            Some(vec![UsageProviderKind::Codex])
        );

        storage
            .set_item(STORAGE_KEY, r#"{"metric":"tokens","windowDays":30}"#)
            .unwrap();
        assert_eq!(
            read_usage_page_preferences(&storage),
            prefs(UsagePageMetric::Tokens, 30)
        );
    }

    #[test]
    fn invalid_preferences_are_replaced_on_the_next_save() {
        for invalid in [
            "not-json",
            r#"{"metric":"unknown","windowDays":7}"#,
            r#"{"metric":"cost","windowDays":365}"#,
        ] {
            let storage = MemoryLocalStorage::default();
            storage.set_item(STORAGE_KEY, invalid).unwrap();
            assert_eq!(
                read_usage_page_preferences(&storage),
                prefs(UsagePageMetric::Limits, 30),
                "{invalid}"
            );
            save_usage_page_preferences(&storage, &prefs(UsagePageMetric::Tokens, 7));
            assert_eq!(
                read_usage_page_preferences(&storage),
                prefs(UsagePageMetric::Tokens, 7)
            );
        }
    }

    #[test]
    fn contains_write_failures_and_saves_again_after_storage_recovers() {
        let storage = FlakyStorage::default();
        save_usage_page_preferences(&storage, &prefs(UsagePageMetric::Cost, 30));
        *storage.fail_writes.lock() = true;
        save_usage_page_preferences(&storage, &prefs(UsagePageMetric::Tokens, 7));
        assert_eq!(
            read_usage_page_preferences(&storage),
            prefs(UsagePageMetric::Cost, 30)
        );
        *storage.fail_writes.lock() = false;
        save_usage_page_preferences(&storage, &prefs(UsagePageMetric::Limits, 7));
        assert_eq!(
            read_usage_page_preferences(&storage),
            prefs(UsagePageMetric::Limits, 7)
        );
    }

    #[test]
    fn contains_failures_when_storage_access_is_blocked() {
        let storage = FlakyStorage::default();
        *storage.fail_reads.lock() = true;
        *storage.fail_writes.lock() = true;
        assert_eq!(
            read_usage_page_preferences(&storage),
            prefs(UsagePageMetric::Limits, 30)
        );
        save_usage_page_preferences(&storage, &prefs(UsagePageMetric::Tokens, 7));
    }

    #[test]
    fn rejects_unknown_hidden_provider_names() {
        let storage = MemoryLocalStorage::default();
        storage
            .set_item(
                STORAGE_KEY,
                r#"{"metric":"cost","windowDays":7,"hiddenProviders":["nope"]}"#,
            )
            .unwrap();
        assert_eq!(
            read_usage_page_preferences(&storage),
            prefs(UsagePageMetric::Limits, 30)
        );
    }
}
