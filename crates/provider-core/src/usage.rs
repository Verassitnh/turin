//! Port of upstream/packages/provider-core/src/server/usage.ts.
//!
//! A driver either points at JSONL transcripts or scans its own sources.
//! Pricing, aggregation and the scan cache stay with the usage page.

use std::{collections::HashMap, sync::Arc};

use serde_json::Value;
use turin_contracts::{
    NonNegativeInt,
    provider_instance::ProviderInstanceId,
    settings::ServerSettings,
    usage::{
        UsageProviderKind, UsageReadError, UsageSourceAction, UsageSourceStatus, UsageTokenTotals,
    },
};

use crate::{host_process::ProcessEnv, rt::BoxFuture};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageSpeed {
    Standard,
    Fast,
    Ultrafast,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageRecord {
    pub provider: UsageProviderKind,
    pub timestamp_ms: f64,
    pub model: String,
    /// Rate-table key when the display name carries tiers the table does not
    /// know. Defaults to `model` when absent.
    pub rate_model: Option<String>,
    pub session_id: String,
    pub totals: UsageTokenTotals,
    pub reported_cost_usd: Option<f64>,
    pub speed: UsageSpeed,
    /// `None` when the record is inherently unique and needs no dedup.
    pub dedupe_key: Option<String>,
}

pub fn empty_totals() -> UsageTokenTotals {
    UsageTokenTotals {
        uncached_input_tokens: 0,
        cached_input_tokens: 0,
        cache_creation_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
    }
}

/// `reasoning_tokens` is a subset of `output_tokens` and must not be added again.
pub fn total_tokens(totals: &UsageTokenTotals) -> NonNegativeInt {
    totals.uncached_input_tokens
        + totals.cached_input_tokens
        + totals.cache_creation_tokens
        + totals.output_tokens
}

/// A token count from untrusted JSON: a positive finite number, truncated, else 0.
pub fn token_count(value: &Value) -> NonNegativeInt {
    match value.as_f64() {
        Some(number) if number.is_finite() && number > 0.0 => number.trunc() as u64,
        _ => 0,
    }
}

/// Epoch milliseconds of an ISO timestamp string, or `None`.
pub fn parse_timestamp_ms(value: &Value) -> Option<f64> {
    let text = value.as_str()?;
    crate::js::date_parse_ms(text).map(|millis| millis as f64)
}

/// A configured instance of the driver, its config already decoded.
pub struct ProviderUsageInstance {
    pub instance_id: ProviderInstanceId,
    /// `None` when the stored config does not decode. The instance still has
    /// history on disk, so readers that need no config read it anyway.
    pub config: Option<Value>,
    pub environment: ProcessEnv,
    /// Whether settings list the instance, rather than it being the implicit default.
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageFile {
    pub path: String,
    pub records: Vec<UsageRecord>,
}

pub struct ProviderUsageScan {
    pub dir: String,
    pub volume_id: Option<String>,
    pub host_id: Option<String>,
    pub status: Option<UsageSourceStatus>,
    pub message: Option<String>,
    pub action: Option<UsageSourceAction>,
    /// `None` when the source does not exist.
    pub files: Option<Vec<ProviderUsageFile>>,
    pub refreshing: bool,
}

pub struct ProviderUsageScanInput {
    pub instances: Vec<ProviderUsageInstance>,
    pub settings: ServerSettings,
    /// Files last written before this cannot hold records inside the window.
    pub window_start_ms: f64,
    /// The oldest usage the server keeps.
    pub retention_cutoff_ms: f64,
    /// Wait for a slow refresh instead of answering from a cache.
    pub await_refresh: bool,
}

pub struct TranscriptDirectory {
    pub dir: String,
    pub file_name: Option<String>,
}

/// JSON fields a format reads. `true` keeps the whole subtree.
pub type SelectedFields = HashMap<String, SelectedField>;

#[derive(Debug, Clone)]
pub enum SelectedField {
    Keep,
    Nested(SelectedFields),
}

type UsageStateInit = Arc<dyn Fn() -> Value + Send + Sync>;
type UsageStateEncode = Arc<dyn Fn(&Value) -> Value + Send + Sync>;
type UsageStateDecode = Arc<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;
type ParseUsageLine = Arc<dyn Fn(&str, &mut Value) -> Vec<UsageRecord> + Send + Sync>;
type ParseUsageProjected = Arc<dyn Fn(&Value, &mut Value) -> Vec<UsageRecord> + Send + Sync>;

pub struct TranscriptUsageState {
    pub initial: UsageStateInit,
    pub encode: UsageStateEncode,
    pub decode: UsageStateDecode,
}

pub struct TranscriptUsageFormat {
    pub select_fields: SelectedFields,
    /// ASCII substrings. At least one must appear on every line that matters.
    pub usage_markers: Vec<String>,
    pub parse_line: ParseUsageLine,
    pub parse_projected: ParseUsageProjected,
    pub state: Option<TranscriptUsageState>,
    pub shared_sessions_across_files: bool,
}

type UsageDirectories = Arc<
    dyn Fn(ProviderUsageInstance) -> BoxFuture<'static, Vec<TranscriptDirectory>> + Send + Sync,
>;
type UsageScan = Arc<
    dyn Fn(
            ProviderUsageScanInput,
        ) -> BoxFuture<'static, Result<Vec<ProviderUsageScan>, UsageReadError>>
        + Send
        + Sync,
>;

pub enum ProviderUsageReader {
    Transcripts {
        provider: UsageProviderKind,
        format: TranscriptUsageFormat,
        directories: UsageDirectories,
    },
    Scan {
        provider: UsageProviderKind,
        scan: UsageScan,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn total_tokens_does_not_add_reasoning_tokens() {
        let totals = UsageTokenTotals {
            uncached_input_tokens: 1,
            cached_input_tokens: 2,
            cache_creation_tokens: 4,
            output_tokens: 8,
            reasoning_tokens: 100,
        };
        assert_eq!(total_tokens(&totals), 15);
    }

    #[test]
    fn token_count_truncates_positive_finite_numbers() {
        assert_eq!(token_count(&json!(3.9)), 3);
        assert_eq!(token_count(&json!(0)), 0);
        assert_eq!(token_count(&json!(-2)), 0);
        assert_eq!(token_count(&json!("nope")), 0);
    }

    #[test]
    fn parse_timestamp_ms_reads_iso_strings() {
        assert_eq!(
            parse_timestamp_ms(&json!("1970-01-01T00:00:00.000Z")),
            Some(0.0)
        );
        assert_eq!(parse_timestamp_ms(&json!(1)), None);
    }

    #[test]
    fn parse_timestamp_ms_accepts_date_only_stamps_like_date_parse() {
        assert_eq!(
            parse_timestamp_ms(&json!("2026-10-10")),
            Some(1_791_590_400_000.0)
        );
    }
}
