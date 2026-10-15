//! Port of upstream/apps/server/src/usage/usageAggregation.ts.
//!
//! Folds parsed transcript records into `(day, hourStart?, provider, model,
//! sourcePath?)` buckets.
//!
//! Pure, so the bucketing and de-duplication rules are testable without touching
//! the filesystem or the network. Nothing here reads the clock.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, FixedOffset, Utc};
use chrono_tz::Tz;
use indexmap::IndexMap;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::usage::{
    UsageBucket, UsageCategoryCost, UsageCostSource, UsageDay, UsageProviderKind, UsageResolution,
    UsageTokenTotals,
};
use turin_provider_core::usage::{UsageRecord, UsageSpeed, empty_totals};

use crate::usage_pricing::{RateTable, cache_savings_usd, price_usage};

const HOUR_MS: i64 = 60 * 60 * 1000;

/// A reporting zone. Intl accepts IANA names and fixed offsets such as
/// `+00:01`; an unknown name degrades to UTC rather than failing the scan.
enum Zone {
    Named(Tz),
    Fixed(FixedOffset),
}

fn parse_zone(name: &str) -> Zone {
    if let Ok(zone) = name.parse::<Tz>() {
        return Zone::Named(zone);
    }
    parse_fixed_offset(name).map_or(Zone::Named(Tz::UTC), Zone::Fixed)
}

/// `+HH`, `+HH:MM`, or `+HHMM`, with either sign.
fn parse_fixed_offset(name: &str) -> Option<FixedOffset> {
    let sign = match name.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = name[1..].chars().filter(|c| *c != ':').collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) || !matches!(digits.len(), 2 | 4) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = if digits.len() == 4 {
        digits[2..].parse().ok()?
    } else {
        0
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

/// Days of instants in one reporting zone. Formatting a day is the expensive part
/// of bucketing, and a quarter-hour slot almost never straddles midnight, so each
/// slot is formatted once: when both its ends fall on one day, every instant in it
/// does too. A slot that does straddle one (a fixed offset such as `+00:01`) is
/// formatted per record.
struct DayFormatter {
    zone: Zone,
    /// `None` marks a slot that straddles midnight.
    slots: HashMap<i64, Option<String>>,
}

const QUARTER_HOUR_MS: i64 = 15 * 60 * 1000;

impl DayFormatter {
    fn new(zone: Zone) -> Self {
        Self {
            zone,
            slots: HashMap::new(),
        }
    }

    fn day(&mut self, timestamp_ms: i64) -> String {
        let slot = timestamp_ms.div_euclid(QUARTER_HOUR_MS);
        let zone = &self.zone;
        let cached = self.slots.entry(slot).or_insert_with(|| {
            let first = day_of(zone, slot * QUARTER_HOUR_MS);
            let last = day_of(zone, (slot + 1) * QUARTER_HOUR_MS - 1);
            (first == last).then_some(first)
        });
        match cached {
            Some(day) => day.clone(),
            None => day_of(&self.zone, timestamp_ms),
        }
    }
}

/// Formats an instant as a `YYYY-MM-DD` day in `zone`.
fn day_of(zone: &Zone, timestamp_ms: i64) -> String {
    let Some(instant) = DateTime::<Utc>::from_timestamp_millis(timestamp_ms) else {
        return String::new();
    };
    match zone {
        Zone::Named(zone) => instant.with_timezone(zone).format("%Y-%m-%d").to_string(),
        Zone::Fixed(offset) => instant.with_timezone(offset).format("%Y-%m-%d").to_string(),
    }
}

/// `Date#toISOString` for an instant in epoch milliseconds.
fn iso_millis(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BucketKey {
    day: String,
    hour_start: String,
    provider: UsageProviderKind,
    model: String,
    source_path: String,
}

struct MutableBucket {
    totals: UsageTokenTotals,
    cost_usd: f64,
    cache_savings_usd: f64,
    category_cost_usd: Option<UsageCategoryCost>,
    fast_cost_usd: f64,
    ultrafast_cost_usd: f64,
    speed_premium_usd: f64,
    records: u64,
    unpriced_records: u64,
    provider_reported_records: u64,
    sessions: HashSet<String>,
}

impl MutableBucket {
    fn new() -> Self {
        Self {
            totals: empty_totals(),
            cost_usd: 0.0,
            cache_savings_usd: 0.0,
            category_cost_usd: None,
            fast_cost_usd: 0.0,
            ultrafast_cost_usd: 0.0,
            speed_premium_usd: 0.0,
            records: 0,
            unpriced_records: 0,
            provider_reported_records: 0,
            sessions: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AggregateOptions {
    pub time_zone: String,
    pub since_day: String,
    pub until_day: String,
    pub rates: RateTable,
    pub price_overrides: Option<RateTable>,
    /// From [`resolve_model_aliases`]. Mapped records bucket and price as their target.
    pub model_aliases: HashMap<String, String>,
    pub resolution: UsageResolution,
    pub since_time_ms: Option<i64>,
    pub until_time_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AggregateError {
    #[error("Hourly usage aggregation requires exact time bounds")]
    MissingHourlyBounds,
}

/// Resolves user model mappings to their final target, so `a -> b` and
/// `b -> c` both land on `c`. A model whose chain enters a loop is left
/// unmapped.
pub fn resolve_model_aliases(aliases: &IndexMap<String, String>) -> HashMap<String, String> {
    let mut resolved = HashMap::new();
    for model in aliases.keys() {
        let mut seen: HashSet<&str> = HashSet::from([model.as_str()]);
        let mut target = aliases[model].as_str();
        while let Some(next) = aliases.get(target).map(String::as_str) {
            if seen.contains(target) {
                break;
            }
            seen.insert(target);
            target = next;
        }
        // Stopping on a mapped model means the chain entered a loop.
        if !aliases.contains_key(target) {
            resolved.insert(model.clone(), target.to_owned());
        }
    }
    resolved
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregateResult {
    pub buckets: Vec<UsageBucket>,
    /// Records dropped because an earlier record carried the same dedupe key.
    pub duplicates_dropped: u64,
    /// Records whose day fell outside the requested window.
    pub out_of_window: u64,
}

/// Accumulates records across many files.
///
/// De-duplication is global across the whole scan, not per file: Claude Code
/// copies a message's records forward when a session is resumed or forked, so
/// the same `dedupeKey` legitimately appears in several transcripts.
pub struct UsageAggregator {
    buckets: IndexMap<BucketKey, MutableBucket>,
    seen: HashSet<String>,
    days: DayFormatter,
    /// `(sinceTimeMs, untilTimeMs)` for hourly requests, `None` for daily ones.
    hourly_window: Option<(i64, i64)>,
    options: AggregateOptions,
    duplicates_dropped: u64,
    out_of_window: u64,
}

impl UsageAggregator {
    pub fn new(options: AggregateOptions) -> Result<Self, AggregateError> {
        let hourly_window = match options.resolution {
            UsageResolution::Hour => match (options.since_time_ms, options.until_time_ms) {
                (Some(since), Some(until)) => Some((since, until)),
                _ => return Err(AggregateError::MissingHourlyBounds),
            },
            UsageResolution::Day => None,
        };
        Ok(Self {
            buckets: IndexMap::new(),
            seen: HashSet::new(),
            days: DayFormatter::new(parse_zone(&options.time_zone)),
            hourly_window,
            options,
            duplicates_dropped: 0,
            out_of_window: 0,
        })
    }

    /// Folds one record in. Returns whether it actually contributed, so callers
    /// can derive per-window facts (distinct sessions, for one) from the records
    /// that landed rather than everything the mtime prefilter happened to admit.
    pub fn add(&mut self, input: &UsageRecord, source_path: &str) -> bool {
        // The target's own rate applies, so a provider-specific `rateModel` is dropped.
        let mapped;
        let record = match self.options.model_aliases.get(&input.model) {
            None => input,
            Some(target) => {
                mapped = UsageRecord {
                    model: target.clone(),
                    rate_model: None,
                    ..input.clone()
                };
                &mapped
            }
        };

        if let Some(dedupe_key) = &record.dedupe_key {
            if self.seen.contains(dedupe_key) {
                self.duplicates_dropped += 1;
                return false;
            }
            self.seen.insert(dedupe_key.clone());
        }

        let timestamp_ms = record.timestamp_ms as i64;
        if self
            .hourly_window
            .is_some_and(|(since, until)| timestamp_ms < since || timestamp_ms >= until)
        {
            self.out_of_window += 1;
            return false;
        }

        let day = self.days.day(timestamp_ms);
        if self.hourly_window.is_none()
            && (day < self.options.since_day || day > self.options.until_day)
        {
            self.out_of_window += 1;
            return false;
        }

        let hour_start = match self.hourly_window {
            None => String::new(),
            Some((since, _)) => {
                let hour_index = (timestamp_ms - since).div_euclid(HOUR_MS);
                iso_millis(since + hour_index * HOUR_MS)
            }
        };
        let key = BucketKey {
            day,
            hour_start,
            provider: record.provider,
            model: record.model.clone(),
            source_path: source_path.to_owned(),
        };
        let bucket = self.buckets.entry(key).or_insert_with(MutableBucket::new);

        let priced = price_usage(
            &self.options.rates,
            record,
            self.options.price_overrides.as_ref(),
        );
        let totals = &mut bucket.totals;
        totals.uncached_input_tokens += record.totals.uncached_input_tokens;
        totals.cached_input_tokens += record.totals.cached_input_tokens;
        totals.cache_creation_tokens += record.totals.cache_creation_tokens;
        totals.output_tokens += record.totals.output_tokens;
        totals.reasoning_tokens += record.totals.reasoning_tokens;
        bucket.cost_usd += priced.cost_usd;
        if let Some(add) = priced.category_cost_usd {
            bucket.category_cost_usd = Some(match bucket.category_cost_usd {
                None => add,
                Some(sum) => UsageCategoryCost {
                    input: sum.input + add.input,
                    cache_read: sum.cache_read + add.cache_read,
                    cache_write: sum.cache_write + add.cache_write,
                    output: sum.output + add.output,
                },
            });
        }
        if record.speed == UsageSpeed::Fast {
            bucket.fast_cost_usd += priced.cost_usd;
        }
        if record.speed == UsageSpeed::Ultrafast {
            bucket.ultrafast_cost_usd += priced.cost_usd;
        }
        bucket.speed_premium_usd += priced.speed_premium_usd;
        bucket.cache_savings_usd += cache_savings_usd(
            &self.options.rates,
            record,
            self.options.price_overrides.as_ref(),
        );
        bucket.records += 1;
        if priced.cost_source == UsageCostSource::Unpriced {
            bucket.unpriced_records += 1;
        }
        if priced.cost_source == UsageCostSource::ProviderReported {
            bucket.provider_reported_records += 1;
        }
        if !record.session_id.is_empty() {
            bucket.sessions.insert(record.session_id.clone());
        }
        true
    }

    pub fn finish(self) -> AggregateResult {
        let mut buckets: Vec<UsageBucket> = self
            .buckets
            .into_iter()
            .map(|(key, bucket)| {
                let fast = round_usd(bucket.fast_cost_usd);
                let ultrafast = round_usd(bucket.ultrafast_cost_usd);
                let speed_premium = round_usd(bucket.speed_premium_usd);
                UsageBucket {
                    day: UsageDay::new(&key.day).expect("bucket days are formatted YYYY-MM-DD"),
                    hour_start: non_empty(&key.hour_start),
                    provider: key.provider,
                    // Empty model names never come from a parser; keep the cell anyway.
                    model: TrimmedNonEmptyString::new(if key.model.is_empty() {
                        "unknown"
                    } else {
                        &key.model
                    })
                    .expect("fallback model is non-empty"),
                    source_path: non_empty(&key.source_path),
                    totals: bucket.totals,
                    cost_usd: bucket.cost_usd,
                    cache_savings_usd: bucket.cache_savings_usd,
                    // Zero and unknown figures are omitted to keep payloads small.
                    category_cost_usd: bucket.category_cost_usd.map(|category| UsageCategoryCost {
                        input: round_usd(category.input),
                        cache_read: round_usd(category.cache_read),
                        cache_write: round_usd(category.cache_write),
                        output: round_usd(category.output),
                    }),
                    fast_cost_usd: (fast != 0.0).then_some(fast),
                    ultrafast_cost_usd: (ultrafast != 0.0).then_some(ultrafast),
                    speed_premium_usd: (speed_premium != 0.0).then_some(speed_premium),
                    cost_source: resolve_cost_source(&bucket),
                    records: bucket.records,
                    unpriced_records: bucket.unpriced_records,
                    sessions: bucket.sessions.len() as u64,
                }
            })
            .collect();
        // Stable ordering keeps payloads diffable and snapshot tests meaningful.
        buckets.sort_by(|a, b| {
            a.day
                .as_str()
                .cmp(b.day.as_str())
                .then_with(|| {
                    a.hour_start
                        .as_ref()
                        .map_or("", |hour| hour.as_str())
                        .cmp(b.hour_start.as_ref().map_or("", |hour| hour.as_str()))
                })
                .then_with(|| provider_name(a.provider).cmp(provider_name(b.provider)))
                .then_with(|| a.model.as_str().cmp(b.model.as_str()))
        });

        AggregateResult {
            buckets,
            duplicates_dropped: self.duplicates_dropped,
            out_of_window: self.out_of_window,
        }
    }
}

fn non_empty(value: &str) -> Option<TrimmedNonEmptyString> {
    if value.is_empty() {
        None
    } else {
        TrimmedNonEmptyString::new(value).ok()
    }
}

/// The wire name of a provider, for sorting in the same order as the TS server.
pub(crate) fn provider_name(provider: UsageProviderKind) -> &'static str {
    match provider {
        UsageProviderKind::Claude => "claude",
        UsageProviderKind::Codex => "codex",
        UsageProviderKind::Grok => "grok",
        UsageProviderKind::Cursor => "cursor",
        UsageProviderKind::Opencode => "opencode",
        UsageProviderKind::Antigravity => "antigravity",
        UsageProviderKind::Pi => "pi",
    }
}

/// Rounds to micro-dollars. The split and speed figures need no more precision,
/// and shorter numbers keep them cheap on the wire.
fn round_usd(value: f64) -> f64 {
    (value * 1e6).round() / 1e6
}

/// A bucket mixes records from one model, but their cost provenance can differ
/// when only some records carried a reported cost. The weakest provenance in the
/// bucket wins so the UI never overstates confidence.
fn resolve_cost_source(bucket: &MutableBucket) -> UsageCostSource {
    if bucket.unpriced_records == bucket.records {
        UsageCostSource::Unpriced
    } else if bucket.provider_reported_records == bucket.records {
        UsageCostSource::ProviderReported
    } else {
        UsageCostSource::ModelPriced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_pricing::parse_rate_table;
    use serde_json::json;

    fn rates() -> RateTable {
        // Stands in for the LiteLLM entry the TS fixture builds by hand.
        parse_rate_table(&json!({
            "claude-fable-5": {
                "input_cost_per_token": 1e-5,
                "output_cost_per_token": 5e-5,
                "cache_read_input_token_cost": 1e-6,
                "cache_creation_input_token_cost": 1.25e-5,
                "input_cost_per_token_priority": 2e-5,
                "output_cost_per_token_priority": 1e-4,
                "cache_read_input_token_cost_priority": 2e-6,
                "cache_creation_input_token_cost_priority": 2.5e-5,
            },
        }))
    }

    fn ms(text: &str) -> f64 {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis() as f64
    }

    fn record() -> UsageRecord {
        UsageRecord {
            provider: UsageProviderKind::Claude,
            // 2026-08-07T04:05Z is still Aug 6 in Los Angeles.
            timestamp_ms: ms("2026-08-07T04:05:13.944Z"),
            model: "claude-fable-5".to_owned(),
            rate_model: None,
            session_id: "session-a".to_owned(),
            totals: UsageTokenTotals {
                uncached_input_tokens: 100,
                cached_input_tokens: 1000,
                cache_creation_tokens: 10,
                output_tokens: 50,
                reasoning_tokens: 0,
            },
            reported_cost_usd: None,
            speed: UsageSpeed::Standard,
            dedupe_key: None,
        }
    }

    fn options(time_zone: &str, resolution: UsageResolution) -> AggregateOptions {
        let hourly = resolution == UsageResolution::Hour;
        AggregateOptions {
            time_zone: time_zone.to_owned(),
            since_day: "2026-08-01".to_owned(),
            until_day: "2026-08-31".to_owned(),
            rates: rates(),
            price_overrides: None,
            model_aliases: HashMap::new(),
            resolution,
            since_time_ms: hourly.then(|| ms("2026-08-06T04:37:00.000Z") as i64),
            until_time_ms: hourly.then(|| ms("2026-08-07T04:37:00.000Z") as i64),
        }
    }

    fn aggregate(
        records: &[UsageRecord],
        time_zone: &str,
        resolution: UsageResolution,
    ) -> AggregateResult {
        let mut aggregator = UsageAggregator::new(options(time_zone, resolution)).unwrap();
        for item in records {
            aggregator.add(item, "");
        }
        aggregator.finish()
    }

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0),
            "{actual} != {expected}"
        );
    }

    #[test]
    fn requires_exact_bounds_for_hourly_aggregation() {
        let mut options = options("UTC", UsageResolution::Hour);
        options.since_time_ms = None;
        let error = UsageAggregator::new(options).err().unwrap();
        assert!(error.to_string().contains("requires exact time bounds"));
    }

    #[test]
    fn splits_a_bucket_cost_by_category_and_speed() {
        let mut fast = record();
        fast.speed = UsageSpeed::Fast;
        let result = aggregate(&[record(), fast], "UTC", UsageResolution::Day);
        let bucket = &result.buckets[0];

        // Standard costs $0.004625 and fast twice that. Upstream asserts these
        // with vitest's default two-digit `closeTo`, which admits the looser
        // figures it wrote; the exact values are asserted here.
        close(bucket.cost_usd, 0.013875);
        let category = bucket.category_cost_usd.unwrap();
        close(category.input, 0.003);
        close(category.cache_read, 0.003);
        close(category.cache_write, 0.000375);
        close(category.output, 0.0075);
        close(bucket.fast_cost_usd.unwrap(), 0.00925);
        close(bucket.speed_premium_usd.unwrap(), 0.004625);
        assert_eq!(bucket.ultrafast_cost_usd, None);
    }

    #[test]
    fn keeps_only_the_first_record_for_a_repeated_dedupe_key() {
        let mut duplicate = record();
        duplicate.dedupe_key = Some("msg_1:".to_owned());
        let result = aggregate(
            &[duplicate.clone(), duplicate.clone(), duplicate],
            "UTC",
            UsageResolution::Day,
        );

        assert_eq!(result.duplicates_dropped, 2);
        assert_eq!(result.buckets.len(), 1);
        assert_eq!(result.buckets[0].records, 1);
        assert_eq!(result.buckets[0].totals.output_tokens, 50);
    }

    #[test]
    fn still_sums_records_that_carry_no_dedupe_key() {
        let result = aggregate(&[record(), record()], "UTC", UsageResolution::Day);

        assert_eq!(result.duplicates_dropped, 0);
        assert_eq!(result.buckets[0].totals.output_tokens, 100);
    }

    #[test]
    fn buckets_by_the_day_in_the_requested_time_zone() {
        let utc = aggregate(&[record()], "UTC", UsageResolution::Day);
        let los_angeles = aggregate(&[record()], "America/Los_Angeles", UsageResolution::Day);

        assert_eq!(utc.buckets[0].day.as_str(), "2026-08-07");
        assert_eq!(los_angeles.buckets[0].day.as_str(), "2026-08-06");
    }

    #[test]
    fn finds_the_day_at_a_quarter_hour_zones_midnight_across_interleaved_buckets() {
        // Kathmandu is UTC+5:45, so its midnight falls at 18:15 UTC.
        let mut other_model = record();
        other_model.timestamp_ms = ms("2026-08-01T18:15:00.000Z");
        other_model.model = "claude-opus-5".to_owned();
        let mut at = record();
        at.timestamp_ms = ms("2026-08-01T18:14:59.999Z");
        let mut midnight = record();
        midnight.timestamp_ms = ms("2026-08-01T18:15:00.000Z");
        let mut after = record();
        after.timestamp_ms = ms("2026-08-01T18:16:00.000Z");
        let result = aggregate(
            &[at, midnight.clone(), other_model, after],
            "Asia/Kathmandu",
            UsageResolution::Day,
        );

        let got: Vec<(String, String, u64)> = result
            .buckets
            .iter()
            .map(|bucket| {
                (
                    bucket.day.as_str().to_owned(),
                    bucket.model.as_str().to_owned(),
                    bucket.records,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("2026-08-01".to_owned(), "claude-fable-5".to_owned(), 1),
                ("2026-08-02".to_owned(), "claude-fable-5".to_owned(), 2),
                ("2026-08-02".to_owned(), "claude-opus-5".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn finds_the_day_at_a_fixed_offsets_midnight_between_quarter_hours() {
        // At +00:01, midnight falls at 23:59 UTC.
        let mut before = record();
        before.timestamp_ms = ms("2026-08-01T23:58:59.999Z");
        let mut at = record();
        at.timestamp_ms = ms("2026-08-01T23:59:00.000Z");
        let result = aggregate(&[before, at], "+00:01", UsageResolution::Day);

        let got: Vec<(String, u64)> = result
            .buckets
            .iter()
            .map(|bucket| (bucket.day.as_str().to_owned(), bucket.records))
            .collect();
        assert_eq!(
            got,
            vec![("2026-08-01".to_owned(), 1), ("2026-08-02".to_owned(), 1)]
        );
    }

    #[test]
    fn splits_an_hourly_request_into_fixed_buckets_anchored_to_its_exact_start() {
        let mut first = record();
        first.timestamp_ms = ms("2026-08-07T02:40:13.944Z");
        let mut second = record();
        second.timestamp_ms = ms("2026-08-07T03:40:13.944Z");
        let result = aggregate(
            &[first, second],
            "America/Los_Angeles",
            UsageResolution::Hour,
        );

        let got: Vec<(String, Option<String>)> = result
            .buckets
            .iter()
            .map(|bucket| {
                (
                    bucket.day.as_str().to_owned(),
                    bucket.hour_start.as_ref().map(|h| h.as_str().to_owned()),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "2026-08-06".to_owned(),
                    Some("2026-08-07T02:37:00.000Z".to_owned())
                ),
                (
                    "2026-08-06".to_owned(),
                    Some("2026-08-07T03:37:00.000Z".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn uses_an_inclusive_start_and_exclusive_end_for_rolling_windows() {
        let times = [
            "2026-08-06T04:36:59.999Z",
            "2026-08-06T04:37:00.000Z",
            "2026-08-07T04:36:59.999Z",
            "2026-08-07T04:37:00.000Z",
        ];
        let records: Vec<UsageRecord> = times
            .iter()
            .map(|time| {
                let mut item = record();
                item.timestamp_ms = ms(time);
                item
            })
            .collect();
        let result = aggregate(&records, "UTC", UsageResolution::Hour);

        assert_eq!(result.out_of_window, 2);
        let hours: Vec<String> = result
            .buckets
            .iter()
            .map(|bucket| bucket.hour_start.as_ref().unwrap().as_str().to_owned())
            .collect();
        assert_eq!(
            hours,
            vec!["2026-08-06T04:37:00.000Z", "2026-08-07T03:37:00.000Z"]
        );
    }

    #[test]
    fn keeps_daily_payloads_collapsed_when_hourly_resolution_is_not_requested() {
        let mut later = record();
        later.timestamp_ms = ms("2026-08-07T05:05:13.944Z");
        let result = aggregate(&[record(), later], "UTC", UsageResolution::Day);

        assert_eq!(result.buckets.len(), 1);
        assert_eq!(result.buckets[0].hour_start, None);
        assert_eq!(result.buckets[0].records, 2);
    }

    #[test]
    fn prices_against_the_rate_table() {
        let result = aggregate(&[record()], "UTC", UsageResolution::Day);

        // 100*1e-5 + 1000*1e-6 + 10*1.25e-5 + 50*5e-5
        close(result.buckets[0].cost_usd, 0.004625);
        assert_eq!(result.buckets[0].cost_source, UsageCostSource::ModelPriced);
    }

    #[test]
    fn counts_tokens_but_not_cost_for_a_model_with_no_rate() {
        let mut unknown = record();
        unknown.model = "kimi-k3".to_owned();
        let result = aggregate(&[unknown], "UTC", UsageResolution::Day);

        assert_eq!(result.buckets[0].cost_usd, 0.0);
        assert_eq!(result.buckets[0].cost_source, UsageCostSource::Unpriced);
        assert_eq!(result.buckets[0].unpriced_records, 1);
        assert_eq!(result.buckets[0].totals.output_tokens, 50);
    }

    #[test]
    fn prefers_a_reported_cost_over_the_rate_table() {
        let mut reported = record();
        reported.reported_cost_usd = Some(1.25);
        let result = aggregate(&[reported], "UTC", UsageResolution::Day);

        assert_eq!(result.buckets[0].cost_usd, 1.25);
        assert_eq!(
            result.buckets[0].cost_source,
            UsageCostSource::ProviderReported
        );
    }

    #[test]
    fn drops_records_outside_the_window() {
        let mut old = record();
        old.timestamp_ms = ms("2026-07-01T12:00:00Z");
        let result = aggregate(&[old], "UTC", UsageResolution::Day);

        assert_eq!(result.out_of_window, 1);
        assert!(result.buckets.is_empty());
    }

    #[test]
    fn reports_whether_a_record_contributed() {
        let mut aggregator = UsageAggregator::new(AggregateOptions {
            time_zone: "UTC".to_owned(),
            ..options("UTC", UsageResolution::Day)
        })
        .unwrap();
        let mut keyed = record();
        keyed.dedupe_key = Some("msg_1:".to_owned());
        let mut old = record();
        old.timestamp_ms = ms("2026-07-01T12:00:00Z");

        assert!(aggregator.add(&keyed, ""));
        assert!(!aggregator.add(&keyed, ""));
        assert!(!aggregator.add(&old, ""));
    }

    #[test]
    fn folds_a_mapped_model_into_its_target_and_prices_it_there() {
        let mut options = options("UTC", UsageResolution::Day);
        let mut aliases = IndexMap::new();
        aliases.insert("example-preview".to_owned(), "claude-fable-5".to_owned());
        options.model_aliases = resolve_model_aliases(&aliases);
        let mut aggregator = UsageAggregator::new(options).unwrap();
        aggregator.add(&record(), "");
        let mut preview = record();
        preview.model = "example-preview".to_owned();
        preview.rate_model = Some("example-preview-high".to_owned());
        aggregator.add(&preview, "");
        let result = aggregator.finish();

        assert_eq!(result.buckets.len(), 1);
        assert_eq!(result.buckets[0].model.as_str(), "claude-fable-5");
        assert_eq!(result.buckets[0].records, 2);
        close(result.buckets[0].cost_usd, 0.00925);
        assert_eq!(result.buckets[0].unpriced_records, 0);
    }

    #[test]
    fn separates_providers_and_models_into_their_own_buckets() {
        let mut codex = record();
        codex.provider = UsageProviderKind::Codex;
        codex.model = "gpt-5.6-sol".to_owned();
        let mut opus = record();
        opus.model = "claude-opus-5".to_owned();
        let result = aggregate(&[record(), codex, opus], "UTC", UsageResolution::Day);

        assert_eq!(result.buckets.len(), 3);
    }

    #[test]
    fn resolve_model_aliases_follows_chains_and_drops_chains_that_enter_a_loop() {
        let aliases: IndexMap<String, String> = [
            ("preview[1m]", "preview"),
            ("preview", "example-model"),
            ("loop", "back"),
            ("back", "loop"),
            ("intoLoop", "loop"),
            ("self", "self"),
        ]
        .into_iter()
        .map(|(model, target)| (model.to_owned(), target.to_owned()))
        .collect();

        let resolved = resolve_model_aliases(&aliases);
        let expected: HashMap<String, String> = [
            ("preview[1m]", "example-model"),
            ("preview", "example-model"),
        ]
        .into_iter()
        .map(|(model, target)| (model.to_owned(), target.to_owned()))
        .collect();
        assert_eq!(resolved, expected);
    }
}
