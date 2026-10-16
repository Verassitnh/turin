//! Port of `upstream/packages/provider-cursor/src/server/accountCache.ts`.
//!
//! Cursor's dashboard API is slow, so fetched records are cached for the
//! retention period and refreshes fetch only the newest edge.

use std::collections::HashMap;

use serde_json::{Map, Value};
use turin_contracts::usage::UsageProviderKind;

use crate::account_usage::{UsageRecord, UsageSpeed, cursor_rate_model};

/// Matches the clients' stale time, so a page refetching on focus reuses one fetch.
pub const CURSOR_ACCOUNT_TTL_MS: f64 = 60.0 * 1000.0;

/// Dashboard events can be finalized after they were first read.
const REFETCH_OVERLAP_MS: f64 = 60.0 * 60.0 * 1000.0;

pub const CURSOR_ACCOUNT_CACHE_FILE_NAME: &str = "usage-cursor-account-cache-v1.json";

const CURSOR_ACCOUNT_CACHE_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub enum CursorCredentialSource {
    File(std::path::PathBuf),
    Keychain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CursorAccountCache {
    pub account_key: String,
    /// Inclusive range of event timestamps the records are complete for.
    pub since_ms: f64,
    pub until_ms: f64,
    /// When the newest edge was fetched.
    pub fetched_at_ms: f64,
    pub records: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorFetchRange {
    pub since_ms: f64,
    pub until_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeCursorFetch {
    pub changed: bool,
    pub cache: CursorAccountCache,
}

/// Whether the cache answers without a refresh.
pub fn is_cursor_cache_fresh(cache: Option<&CursorAccountCache>, now_ms: f64) -> bool {
    cache.is_some_and(|cache| now_ms - cache.fetched_at_ms < CURSOR_ACCOUNT_TTL_MS)
}

/// The range to fetch so the cache covers `[retention_start_ms, now_ms]`.
pub fn cursor_fetch_range(
    cache: Option<&CursorAccountCache>,
    retention_start_ms: f64,
    now_ms: f64,
) -> CursorFetchRange {
    match cache {
        None => CursorFetchRange {
            since_ms: retention_start_ms,
            until_ms: now_ms,
        },
        Some(cache)
            if cache.since_ms > retention_start_ms
                || cache.until_ms - REFETCH_OVERLAP_MS < retention_start_ms =>
        {
            CursorFetchRange {
                since_ms: retention_start_ms,
                until_ms: now_ms,
            }
        }
        Some(cache) => CursorFetchRange {
            since_ms: cache.until_ms - REFETCH_OVERLAP_MS,
            until_ms: now_ms,
        },
    }
}

/// Applies a fetched range to the cache, replacing the cached records it covers.
pub fn merge_cursor_fetch(
    cache: Option<&CursorAccountCache>,
    account_key: &str,
    range: CursorFetchRange,
    fetched: &[UsageRecord],
    now_ms: f64,
    retention_start_ms: f64,
) -> MergeCursorFetch {
    let in_range = |record: &UsageRecord| {
        record.timestamp_ms >= range.since_ms && record.timestamp_ms <= range.until_ms
    };
    let previous = cache.map(|cache| cache.records.as_slice()).unwrap_or(&[]);
    let added: Vec<_> = fetched
        .iter()
        .filter(|record| in_range(record))
        .cloned()
        .collect();
    let replaced_keys: std::collections::HashSet<_> = previous
        .iter()
        .filter(|record| in_range(record))
        .map(|record| record.dedupe_key.clone())
        .collect();
    let changed = cache.is_none()
        || replaced_keys.len() != added.len()
        || added
            .iter()
            .any(|record| !replaced_keys.contains(&record.dedupe_key));
    let records = previous
        .iter()
        .filter(|record| !in_range(record) && record.timestamp_ms >= retention_start_ms)
        .chain(added.iter())
        .cloned()
        .collect();
    MergeCursorFetch {
        changed,
        cache: CursorAccountCache {
            account_key: account_key.to_owned(),
            since_ms: retention_start_ms,
            until_ms: range.until_ms,
            fetched_at_ms: now_ms,
            records,
        },
    }
}

type SerializedRecord = (
    f64,
    usize,
    usize,
    u64,
    u64,
    u64,
    u64,
    Option<f64>,
    Option<String>,
);

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SerializedAccount<'a> {
    account_key: &'a str,
    since_ms: f64,
    until_ms: f64,
    fetched_at_ms: f64,
    records: Vec<SerializedRecord>,
}

/// Serialises the caches by credential source.
pub fn encode_cursor_account_caches(caches: &HashMap<String, CursorAccountCache>) -> String {
    let mut strings: Vec<String> = Vec::new();
    let mut indexes: HashMap<String, usize> = HashMap::new();
    let intern = |value: &str, strings: &mut Vec<String>, indexes: &mut HashMap<String, usize>| {
        if let Some(index) = indexes.get(value) {
            return *index;
        }
        let index = strings.len();
        strings.push(value.to_owned());
        indexes.insert(value.to_owned(), index);
        index
    };
    let mut accounts: Map<String, Value> = Map::new();
    for (credential_key, cache) in caches {
        let records: Vec<SerializedRecord> = cache
            .records
            .iter()
            .map(|record| {
                (
                    record.timestamp_ms,
                    intern(&record.model, &mut strings, &mut indexes),
                    intern(&record.session_id, &mut strings, &mut indexes),
                    record.totals.uncached_input_tokens,
                    record.totals.cached_input_tokens,
                    record.totals.cache_creation_tokens,
                    record.totals.output_tokens,
                    record.reported_cost_usd,
                    Some(record.dedupe_key.clone()),
                )
            })
            .collect();
        accounts.insert(
            credential_key.clone(),
            serde_json::to_value(SerializedAccount {
                account_key: &cache.account_key,
                since_ms: cache.since_ms,
                until_ms: cache.until_ms,
                fetched_at_ms: cache.fetched_at_ms,
                records,
            })
            .expect("serialize account cache"),
        );
    }
    serde_json::json!({
        "version": CURSOR_ACCOUNT_CACHE_VERSION,
        "strings": strings,
        "accounts": accounts,
    })
    .to_string()
}

fn is_finite_number(value: &Value) -> bool {
    value.as_f64().is_some_and(f64::is_finite)
}

/// Rebuilds the caches from a parsed document.
pub fn decode_cursor_account_caches(document: &Value) -> HashMap<String, CursorAccountCache> {
    let mut caches = HashMap::new();
    let Some(root) = document.as_object() else {
        return caches;
    };
    let strings = root.get("strings").and_then(Value::as_array);
    if root.get("version").and_then(Value::as_i64) != Some(CURSOR_ACCOUNT_CACHE_VERSION)
        || !strings.is_some_and(|strings| strings.iter().all(Value::is_string))
    {
        return caches;
    }
    let strings: Vec<String> = strings
        .expect("checked")
        .iter()
        .map(|value| value.as_str().expect("string").to_owned())
        .collect();
    let accounts = match root.get("accounts") {
        Some(Value::Object(accounts)) => accounts,
        _ => return caches,
    };
    let string_at = |index: &Value| -> Option<String> {
        index
            .as_u64()
            .and_then(|index| strings.get(index as usize))
            .cloned()
    };

    let decode_record = |row: &Value| -> Option<UsageRecord> {
        let row = row.as_array()?;
        if row.len() != 9 {
            return None;
        }
        let timestamp_ms = row[0].as_f64()?;
        let model = string_at(&row[1])?;
        let session_id = string_at(&row[2])?;
        let uncached = row[3].as_u64()?;
        let cached = row[4].as_u64()?;
        let creation = row[5].as_u64()?;
        let output = row[6].as_u64()?;
        let cost = match &row[7] {
            Value::Null => None,
            value if is_finite_number(value) => value.as_f64(),
            _ => return None,
        };
        let key = match &row[8] {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            _ => return None,
        };
        if !timestamp_ms.is_finite() {
            return None;
        }
        Some(UsageRecord {
            provider: UsageProviderKind::Cursor,
            timestamp_ms,
            model: model.clone(),
            rate_model: cursor_rate_model(&model),
            session_id,
            totals: turin_contracts::usage::UsageTokenTotals {
                uncached_input_tokens: uncached,
                cached_input_tokens: cached,
                cache_creation_tokens: creation,
                output_tokens: output,
                reasoning_tokens: 0,
            },
            reported_cost_usd: cost,
            speed: UsageSpeed::Standard,
            dedupe_key: key?,
        })
    };

    for (credential_key, raw) in accounts {
        let Some(account) = raw.as_object() else {
            continue;
        };
        let Some(account_key) = account.get("accountKey").and_then(Value::as_str) else {
            continue;
        };
        let Some(since_ms) = account.get("sinceMs").and_then(Value::as_f64) else {
            continue;
        };
        let Some(until_ms) = account.get("untilMs").and_then(Value::as_f64) else {
            continue;
        };
        let Some(fetched_at_ms) = account.get("fetchedAtMs").and_then(Value::as_f64) else {
            continue;
        };
        let Some(rows) = account.get("records").and_then(Value::as_array) else {
            continue;
        };
        if !since_ms.is_finite() || !until_ms.is_finite() || !fetched_at_ms.is_finite() {
            continue;
        }
        let mut records = Vec::new();
        for row in rows {
            let Some(record) = decode_record(row) else {
                break;
            };
            records.push(record);
        }
        if records.len() != rows.len() {
            continue;
        }
        caches.insert(
            credential_key.clone(),
            CursorAccountCache {
                account_key: account_key.to_owned(),
                since_ms,
                until_ms,
                fetched_at_ms,
                records,
            },
        );
    }
    caches
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::usage::UsageTokenTotals;

    const DAY_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;

    fn record(timestamp_ms: f64) -> UsageRecord {
        UsageRecord {
            provider: UsageProviderKind::Cursor,
            timestamp_ms,
            model: "claude-fable-5".to_owned(),
            rate_model: cursor_rate_model("claude-fable-5"),
            session_id: "conversation-1".to_owned(),
            totals: UsageTokenTotals {
                uncached_input_tokens: 0,
                cached_input_tokens: 0,
                cache_creation_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: 0,
            },
            reported_cost_usd: None,
            speed: UsageSpeed::Standard,
            dedupe_key: format!("cursor-account:a:{timestamp_ms}:0"),
        }
    }

    #[test]
    fn refetches_everything_once_the_cache_no_longer_reaches_the_retention_start() {
        let cache = CursorAccountCache {
            account_key: "a".to_owned(),
            since_ms: 0.0,
            until_ms: 10.0 * DAY_MS,
            fetched_at_ms: 10.0 * DAY_MS,
            records: vec![record(5.0 * DAY_MS)],
        };
        let now_ms = 100.0 * DAY_MS;
        let range = cursor_fetch_range(Some(&cache), 10.0 * DAY_MS, now_ms);
        assert_eq!(range.since_ms, 10.0 * DAY_MS);
        assert_eq!(range.until_ms, now_ms);

        let merged = merge_cursor_fetch(
            Some(&cache),
            "a",
            range,
            &[record(50.0 * DAY_MS)],
            now_ms,
            10.0 * DAY_MS,
        );
        assert_eq!(
            merged
                .cache
                .records
                .iter()
                .map(|entry| entry.timestamp_ms)
                .collect::<Vec<_>>(),
            vec![50.0 * DAY_MS]
        );
        let next = cursor_fetch_range(Some(&merged.cache), 11.0 * DAY_MS, now_ms + DAY_MS);
        assert_eq!(next.since_ms, now_ms - 60.0 * 60.0 * 1000.0);
        assert_eq!(next.until_ms, now_ms + DAY_MS);
    }

    #[test]
    fn reports_an_edge_that_only_confirms_the_cache_as_unchanged() {
        let cache = CursorAccountCache {
            account_key: "a".to_owned(),
            since_ms: 0.0,
            until_ms: 10.0 * DAY_MS,
            fetched_at_ms: 10.0 * DAY_MS,
            records: vec![record(DAY_MS), record(10.0 * DAY_MS)],
        };
        let range = cursor_fetch_range(Some(&cache), 0.0, 11.0 * DAY_MS);
        assert!(
            !merge_cursor_fetch(
                Some(&cache),
                "a",
                range,
                &[record(10.0 * DAY_MS)],
                11.0 * DAY_MS,
                0.0,
            )
            .changed
        );
        assert!(
            merge_cursor_fetch(
                Some(&cache),
                "a",
                range,
                &[record(10.0 * DAY_MS), record(10.5 * DAY_MS)],
                11.0 * DAY_MS,
                0.0,
            )
            .changed
        );
    }

    #[test]
    fn round_trips_encoded_caches() {
        let mut caches = HashMap::new();
        caches.insert(
            "/auth.json".to_owned(),
            CursorAccountCache {
                account_key: "hash".to_owned(),
                since_ms: 0.0,
                until_ms: 1.0,
                fetched_at_ms: 2.0,
                records: vec![record(0.5)],
            },
        );
        let encoded = encode_cursor_account_caches(&caches);
        let parsed: Value = serde_json::from_str(&encoded).unwrap();
        let decoded = decode_cursor_account_caches(&parsed);
        assert_eq!(decoded, caches);
    }
}
