//! Port of upstream/packages/contracts/src/usage.ts
//!
//! Usage reporting contract. Each environment scans native session files and
//! databases, including work driven outside T3 Code. Raw transcript records
//! never cross the wire; environments return pre-aggregated
//! `(day, hourStart?, provider, model, sourcePath?)` buckets.

use crate::base_schemas::{
    ContractError, NonNegativeInt, TrimmedNonEmptyString, forward_compatible_array, js_trim,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Bumped whenever the shape of [`UsageSummary`] changes incompatibly. The
/// client renders partial coverage when an environment reports an older
/// version rather than failing the whole page. Adding providers or other
/// array-element variants is additive: unknown entries are skipped on decode
/// and do not require a version bump. So are optional bucket fields, which
/// older clients ignore.
pub const USAGE_CONTRACT_VERSION: f64 = 6.0;

/// Oldest [`UsageSummary`] version a current client will still merge.
///
/// v5/v6 add providers and optional source attribution; v4 Claude/Codex
/// buckets remain valid in mixed-version environments.
pub const USAGE_MERGE_COMPATIBLE_SINCE: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageProviderKind {
    Claude,
    Codex,
    Grok,
    Cursor,
    Opencode,
    Antigravity,
    Pi,
}

/// A calendar day in the reporting time zone, formatted `YYYY-MM-DD`.
///
/// Days are bucketed server-side so that a turn always lands on the day the
/// user experienced it, not the UTC day.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageDay(String);

impl UsageDay {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = js_trim(value.as_ref());
        let bytes = trimmed.as_bytes();
        let valid = trimmed.len() == 10
            && bytes[..4].iter().all(|b| b.is_ascii_digit())
            && bytes[4] == b'-'
            && bytes[5..7].iter().all(|b| b.is_ascii_digit())
            && bytes[7] == b'-'
            && bytes[8..].iter().all(|b| b.is_ascii_digit());
        if !valid {
            return Err(ContractError {
                expected: "a calendar day formatted YYYY-MM-DD",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for UsageDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for UsageDay {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for UsageDay {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for UsageDay {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageResolution {
    Day,
    Hour,
}

/// Why a bucket's cost is what it is.
///
/// - `providerReported` - the transcript carried an explicit cost figure.
/// - `modelPriced` - we used a custom price override or the LiteLLM rate table.
/// - `unpriced` - tokens are known, rates are not. Counted in totals, excluded
///   from cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageCostSource {
    ProviderReported,
    ModelPriced,
    Unpriced,
}

/// Token counts for a bucket.
///
/// `cachedInputTokens` and `cacheCreationTokens` are disjoint from
/// `uncachedInputTokens`; summing all three gives total input.
/// `reasoningTokens` is a *subset* of `outputTokens` (Codex reports it that
/// way, and Anthropic folds thinking into output), so it must never be added
/// on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTokenTotals {
    pub uncached_input_tokens: NonNegativeInt,
    pub cached_input_tokens: NonNegativeInt,
    pub cache_creation_tokens: NonNegativeInt,
    pub output_tokens: NonNegativeInt,
    pub reasoning_tokens: NonNegativeInt,
}

/// A bucket's cost split by token category, in USD. A provider-reported cost
/// is split in proportion to the model's list rates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCategoryCost {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

/// One `(day, hourStart?, provider, model)` cell. `hourStart` is the UTC
/// start instant of a rolling bucket and is present only for hourly requests.
///
/// `costUsd` is the raw API-equivalent cost of these tokens. It is not money
/// spent: subscription plans bill separately. `unpricedRecords` counts
/// records whose tokens are included in the token totals but which
/// contributed nothing to `costUsd`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub day: UsageDay,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hour_start: Option<TrimmedNonEmptyString>,
    pub provider: UsageProviderKind,
    pub model: TrimmedNonEmptyString,
    /// Source directory, so overlapping multi-home environments merge once per source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<TrimmedNonEmptyString>,
    pub totals: UsageTokenTotals,
    pub cost_usd: f64,
    /// What the cached input would have cost at full input rates minus what
    /// it actually cost. Requires the rate table, so it is computed alongside
    /// cost rather than derived on the client.
    pub cache_savings_usd: f64,
    /// `costUsd` by token category. Cost with no known rates stays out of it,
    /// and it is absent when nothing could be split or the server predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_cost_usd: Option<UsageCategoryCost>,
    /// Cost of fast and ultrafast requests. Absent when zero; the rest is standard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultrafast_cost_usd: Option<f64>,
    /// What fast and ultrafast requests cost above the standard rate. Absent when zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_premium_usd: Option<f64>,
    pub cost_source: UsageCostSource,
    /// Distinct assistant responses, after de-duplication.
    pub records: NonNegativeInt,
    pub unpriced_records: NonNegativeInt,
    /// Distinct transcript sessions that contributed to this cell.
    pub sessions: NonNegativeInt,
}

/// Identifies the physical transcript directory a source read from.
///
/// Two environments on the same machine (worktree servers, for example)
/// resolve the same provider home and would otherwise double count. The
/// client drops duplicate fingerprints before merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSourceFingerprint {
    pub host_id: TrimmedNonEmptyString,
    pub provider: UsageProviderKind,
    pub resolved_home_path: TrimmedNonEmptyString,
    /// Filesystem identity of the transcript directory, as `device:inode`.
    ///
    /// Hostname and path alone are not enough: every Mac in a fleet resolves
    /// `/Users/<user>/.claude`, so two machines that happen to share a
    /// hostname would look like one source and have their usage silently
    /// dropped. The device/inode pair is stable for two servers reading the
    /// same directory and effectively never collides across machines. Empty
    /// when it cannot be read.
    pub volume_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageSourceStatus {
    Ok,
    Missing,
    Partial,
    Failed,
}

/// An action the client can offer to make a source available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UsageSourceAction {
    #[serde(rename = "enableCursorKeychain")]
    EnableCursorKeychain,
}

mod refreshing_field {
    use super::*;

    /// `Schema.optionalKey(Schema.Literal(true))`: absent, or exactly `true`.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
        let value = Option::<serde_json::Value>::deserialize(d)?;
        match value {
            None => Ok(None),
            Some(serde_json::Value::Bool(true)) => Ok(Some(true)),
            Some(_) => Err(serde::de::Error::custom("expected the literal true")),
        }
    }

    pub fn serialize<S: Serializer>(value: &Option<bool>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(true) => s.serialize_bool(true),
            _ => s.serialize_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSource {
    pub fingerprint: UsageSourceFingerprint,
    pub status: UsageSourceStatus,
    pub scanned_files: NonNegativeInt,
    pub skipped_files: NonNegativeInt,
    /// Records that parsed but carried no recognisable usage payload.
    pub malformed_records: NonNegativeInt,
    /// Distinct transcript sessions seen under this directory. Buckets also
    /// carry per-bucket session counts, but a session spans days and models,
    /// so summing those overcounts; this is the figure clients should total.
    pub distinct_sessions: NonNegativeInt,
    pub message: Option<TrimmedNonEmptyString>,
    /// An action the client can offer to make this source available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<UsageSourceAction>,
    /// Present when this source answered from its cache while a slow refresh
    /// (an account API, for example) runs. Repeat the request with
    /// `awaitRefresh` to get the refreshed source.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "refreshing_field"
    )]
    pub refreshing: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsagePricingStatus {
    Fresh,
    Cached,
    Unavailable,
}

/// Provenance for the rate table, so the UI can be honest about how good the
/// cost figures are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePricing {
    pub status: UsagePricingStatus,
    pub source: TrimmedNonEmptyString,
    pub fetched_at: Option<String>,
    pub known_models: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummaryInput {
    /// Inclusive first day of the window, in `timeZone`.
    pub since_day: UsageDay,
    /// Inclusive last day of the window, in `timeZone`.
    pub until_day: UsageDay,
    /// IANA zone the client wants days bucketed in. An offset would be wrong
    /// for any window that crosses a DST boundary.
    pub time_zone: TrimmedNonEmptyString,
    /// Defaults to daily for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<UsageResolution>,
    /// Inclusive UTC instant for an hourly rolling window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_time: Option<TrimmedNonEmptyString>,
    /// Exclusive UTC instant for an hourly rolling window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_time: Option<TrimmedNonEmptyString>,
    /// Wait for slow sources to finish refreshing instead of answering from
    /// their cache. Clients send it as the follow-up to a summary with a
    /// `refreshing` source. Older servers ignore it and always wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub await_refresh: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub contract_version: f64,
    pub read_at: String,
    pub time_zone: TrimmedNonEmptyString,
    pub since_day: UsageDay,
    pub until_day: UsageDay,
    #[serde(with = "forward_compatible_array")]
    pub buckets: Vec<UsageBucket>,
    #[serde(with = "forward_compatible_array")]
    pub sources: Vec<UsageSource>,
    pub pricing: UsagePricing,
    /// Wall-clock cost of the scan, surfaced in diagnostics.
    pub scan_duration_ms: NonNegativeInt,
}

/// `UsageReadError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UsageReadError {
    #[serde(rename = "UsageReadError")]
    #[error("Usage read failed ({reason}): {detail}")]
    ReadError {
        reason: UsageReadErrorReason,
        /// Stable, bounded description. The underlying failure travels in `cause`.
        detail: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageReadErrorReason {
    ScanFailed,
    InvalidWindow,
}

impl fmt::Display for UsageReadErrorReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ScanFailed => "scanFailed",
            Self::InvalidWindow => "invalidWindow",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bucket() -> serde_json::Value {
        json!({
            "day": "2026-08-20",
            "provider": "claude",
            "model": "claude-opus",
            "totals": {
                "uncachedInputTokens": 1, "cachedInputTokens": 2, "cacheCreationTokens": 3,
                "outputTokens": 4, "reasoningTokens": 1,
            },
            "costUsd": 0.25,
            "cacheSavingsUsd": 0.5,
            "categoryCostUsd": { "input": 0.1, "cacheRead": 0.05, "cacheWrite": 0.05, "output": 0.05 },
            "costSource": "modelPriced",
            "records": 5,
            "unpricedRecords": 0,
            "sessions": 1,
        })
    }

    fn source() -> serde_json::Value {
        json!({
            "fingerprint": { "hostId": "h", "provider": "codex", "resolvedHomePath": "/h", "volumeId": "" },
            "status": "ok",
            "scannedFiles": 1, "skippedFiles": 0, "malformedRecords": 0, "distinctSessions": 1,
            "message": null,
        })
    }

    #[test]
    fn bucket_round_trips_with_fractional_cost_and_omitted_optionals() {
        let decoded: UsageBucket = serde_json::from_value(bucket()).unwrap();
        assert_eq!(decoded.cost_usd, 0.25);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), bucket());
    }

    #[test]
    fn day_pattern_and_trimming() {
        assert_eq!(
            UsageDay::new(" 2026-08-20 ").unwrap().as_str(),
            "2026-08-20"
        );
        assert!(UsageDay::new("2026-8-20").is_err());
        assert!(UsageDay::new("2026-08-20x").is_err());
        assert!(UsageDay::new("２０２６-08-20").is_err());
    }

    #[test]
    fn summary_drops_undecodable_buckets_and_sources() {
        let summary: UsageSummary = serde_json::from_value(json!({
            "contractVersion": 6,
            "readAt": "2026-08-22T00:00:00.000Z",
            "timeZone": "UTC",
            "sinceDay": "2026-08-01",
            "untilDay": "2026-08-22",
            "buckets": [bucket(), { "day": "bad" }],
            "sources": [source(), 7],
            "pricing": { "status": "fresh", "source": "litellm", "fetchedAt": null, "knownModels": 3 },
            "scanDurationMs": 12,
        }))
        .unwrap();
        assert_eq!(summary.buckets.len(), 1);
        assert_eq!(summary.sources.len(), 1);
        assert_eq!(summary.contract_version, 6.0);
    }

    #[test]
    fn source_refreshing_is_only_ever_true() {
        let mut refreshing = source();
        refreshing["refreshing"] = json!(true);
        refreshing["action"] = json!("enableCursorKeychain");
        let decoded: UsageSource = serde_json::from_value(refreshing.clone()).unwrap();
        assert_eq!(decoded.refreshing, Some(true));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), refreshing);
        let mut bad = source();
        bad["refreshing"] = json!(false);
        assert!(serde_json::from_value::<UsageSource>(bad).is_err());
        // `message` is a required nullable key and must encode `null`.
        let plain: UsageSource = serde_json::from_value(source()).unwrap();
        assert_eq!(
            serde_json::to_value(&plain).unwrap()["message"],
            json!(null)
        );
    }

    #[test]
    fn read_error_message() {
        let error: UsageReadError = serde_json::from_value(json!({
            "_tag": "UsageReadError", "reason": "invalidWindow", "detail": "since after until",
        }))
        .unwrap();
        assert_eq!(
            error.to_string(),
            "Usage read failed (invalidWindow): since after until"
        );
    }
}
