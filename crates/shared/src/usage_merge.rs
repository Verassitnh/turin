//! Port of upstream/packages/shared/src/usageMerge.ts
//!
//! Merges per-environment usage summaries into the single view the page renders.
//! Pure, so the de-duplication and derivation rules can be tested without a
//! connected environment.
//!
//! Upstream tracks bucket membership by object identity. Here a bucket is
//! identified by its index in its environment's `summary.buckets`, which is the
//! same identity: every set holds buckets from that environment's own summary.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use turin_contracts::{
    EnvironmentId,
    usage::{
        USAGE_MERGE_COMPATIBLE_SINCE, UsageBucket, UsageCostSource, UsageProviderKind, UsageSource,
        UsageSourceFingerprint, UsageSourceStatus, UsageSummary, UsageTokenTotals,
    },
};

use crate::js_string::locale_compare;
use crate::usage_limits::parse_date_ms;

#[derive(Debug, Clone)]
pub struct EnvironmentUsage {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub summary: UsageSummary,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderTotals {
    pub provider: UsageProviderKind,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub records: u64,
    pub sessions: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelTotals {
    pub model: String,
    pub provider: UsageProviderKind,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub tokens: UsageTokenTotals,
    pub records: u64,
    /// Records whose tokens are counted here but which contributed nothing to
    /// `cost_usd`. When it equals `records` the cost is unknown, not zero.
    pub unpriced_records: u64,
    /// Tokens with no known rates, which a custom price would cover. A cell that
    /// mixes these with reported costs counts its tokens by record share.
    pub unpriced_tokens: f64,
    pub cost_share: f64,
    pub token_share: f64,
}

/// A model whose every record lacked rates has an unknown cost, not a zero one.
/// Clients must not present its `cost_usd` as a real dollar figure.
pub fn is_model_cost_unknown(model: &ModelTotals) -> bool {
    model.records > 0 && model.unpriced_records >= model.records
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderSpend {
    pub cost_usd: f64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DailyTotals {
    pub day: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: IndexMap<UsageProviderKind, ProviderSpend>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HourlyTotals {
    pub day: String,
    pub hour_start: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: IndexMap<UsageProviderKind, ProviderSpend>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostQuality {
    pub provider_reported_share: f64,
    pub model_priced_share: f64,
    pub unpriced_share: f64,
    pub cache_savings_usd: f64,
}

/// `costUsd` by token category. `unsplit` is cost no rates could split,
/// including all cost from servers that predate the split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CategoryCost {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
    pub unsplit: f64,
}

/// `costUsd` by request speed. Servers that predate speeds count as standard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeedCost {
    pub standard: f64,
    pub fast: f64,
    pub ultrafast: f64,
    /// What fast and ultrafast requests cost above the standard rate.
    pub premium: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageContractMismatchDirection {
    ServerBehind,
    ClientBehind,
}

impl UsageContractMismatchDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ServerBehind => "serverBehind",
            Self::ClientBehind => "clientBehind",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageContractMismatch {
    pub environment_id: EnvironmentId,
    pub direction: UsageContractMismatchDirection,
    pub contract_version: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergedUsage {
    pub cost_usd: f64,
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub records: u64,
    pub sessions: u64,
    pub providers: Vec<ProviderTotals>,
    pub models: Vec<ModelTotals>,
    pub daily: Vec<DailyTotals>,
    pub hourly: Vec<HourlyTotals>,
    pub cost_quality: CostQuality,
    pub category_cost: CategoryCost,
    pub speed_cost: SpeedCost,
    /// Environments whose data was dropped as a duplicate of another's.
    pub duplicate_sources: Vec<String>,
    pub contributing_environments: Vec<EnvironmentId>,
    pub contract_mismatches: Vec<UsageContractMismatch>,
}

impl MergedUsage {
    fn empty() -> Self {
        Self {
            cost_usd: 0.0,
            uncached_input_tokens: 0,
            cached_input_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            records: 0,
            sessions: 0,
            providers: Vec::new(),
            models: Vec::new(),
            daily: Vec::new(),
            hourly: Vec::new(),
            cost_quality: CostQuality {
                provider_reported_share: 0.0,
                model_priced_share: 0.0,
                unpriced_share: 0.0,
                cache_savings_usd: 0.0,
            },
            category_cost: CategoryCost {
                input: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                output: 0.0,
                unsplit: 0.0,
            },
            speed_cost: SpeedCost {
                standard: 0.0,
                fast: 0.0,
                ultrafast: 0.0,
                premium: 0.0,
            },
            duplicate_sources: Vec::new(),
            contributing_environments: Vec::new(),
            contract_mismatches: Vec::new(),
        }
    }
}

/// Wire name of a provider kind, as it appears in summaries and fingerprints.
fn provider_name(provider: UsageProviderKind) -> &'static str {
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

/// Two sources are the same physical transcript directory only when host,
/// provider, path and filesystem identity all agree.
///
/// `volumeId` is what stops two machines that happen to share a hostname and a
/// home path, which is every Mac in a fleet, from collapsing into one source and
/// having one of them silently dropped.
fn fingerprint_key(fingerprint: &UsageSourceFingerprint) -> String {
    [
        fingerprint.host_id.as_str(),
        provider_name(fingerprint.provider),
        fingerprint.resolved_home_path.as_str(),
        fingerprint.volume_id.as_str(),
    ]
    .join(" ")
}

/// Indexes into `summary.buckets` that belong to `source`.
fn buckets_for_source(summary: &UsageSummary, source: &UsageSource) -> Vec<usize> {
    let provider = source.fingerprint.provider;
    let provider_sources = summary
        .sources
        .iter()
        .filter(|entry| entry.fingerprint.provider == provider)
        .count();
    summary
        .buckets
        .iter()
        .enumerate()
        .filter(|(_, bucket)| {
            bucket.provider == provider
                && match &bucket.source_path {
                    Some(path) => path.as_str() == source.fingerprint.resolved_home_path.as_str(),
                    None => provider_sources == 1,
                }
        })
        .map(|(index, _)| index)
        .collect()
}

/// `JSON.stringify([day, hourStart ?? null, provider, model])`. The source path is not part of the cell.
fn bucket_key(bucket: &UsageBucket) -> String {
    serde_json::to_string(&(
        bucket.day.as_str(),
        bucket.hour_start.as_ref().map(|hour| hour.as_str()),
        provider_name(bucket.provider),
        bucket.model.as_str(),
    ))
    .unwrap_or_default()
}

/// `Date.parse(value) || 0`.
fn read_millis(read_at: &str) -> f64 {
    parse_date_ms(read_at).unwrap_or(0.0)
}

struct SourceClaims {
    owner_by_fingerprint: HashMap<String, EnvironmentId>,
    supplemental_buckets_by_environment: HashMap<EnvironmentId, HashSet<usize>>,
    sessions_by_fingerprint: HashMap<String, u64>,
    duplicates: Vec<String>,
}

/// Decides which environment owns each physical transcript directory.
///
/// Several environments on one machine (worktree servers, for instance) resolve
/// the same provider home and would otherwise double count every token. The
/// Complete scans claim a fingerprint ahead of partial scans, then the most
/// recently read scan wins within each status. A newer partial scan can still
/// contribute cells absent from an older complete scan. Environment ids break
/// ties so the result is stable when summaries have the same read time.
fn claim_sources(environments: &[&EnvironmentUsage]) -> SourceClaims {
    let mut ordered: Vec<&EnvironmentUsage> = environments.to_vec();
    ordered.sort_by(|a, b| {
        read_millis(&b.summary.read_at)
            .partial_cmp(&read_millis(&a.summary.read_at))
            .unwrap_or(Ordering::Equal)
            .then_with(|| locale_compare(a.environment_id.as_str(), b.environment_id.as_str()))
    });

    let mut owner_by_fingerprint: HashMap<String, EnvironmentId> = HashMap::new();
    // (index into `ordered`, index into that environment's sources)
    let mut owner_scan_by_fingerprint: HashMap<String, (usize, usize)> = HashMap::new();
    let mut sessions_by_fingerprint: HashMap<String, u64> = HashMap::new();
    let mut duplicates: Vec<String> = Vec::new();

    // A complete scan takes precedence over a newer partial scan of the same
    // directory. Partial history still contributes when no complete copy exists.
    for status in [
        UsageSourceStatus::Ok,
        UsageSourceStatus::Partial,
        UsageSourceStatus::Failed,
    ] {
        for (environment_index, environment) in ordered.iter().enumerate() {
            for (source_index, source) in environment.summary.sources.iter().enumerate() {
                if source.status != status {
                    continue;
                }
                let key = fingerprint_key(&source.fingerprint);
                if owner_by_fingerprint.contains_key(&key) {
                    duplicates.push(format!(
                        "{}: {}",
                        environment.label,
                        source.fingerprint.resolved_home_path.as_str()
                    ));
                    continue;
                }
                owner_by_fingerprint.insert(key.clone(), environment.environment_id.clone());
                owner_scan_by_fingerprint.insert(key.clone(), (environment_index, source_index));
                sessions_by_fingerprint.insert(key, source.distinct_sessions);
            }
        }
    }

    // A newer partial scan may contain usage recorded after an older complete
    // scan. Keep cells absent from the complete scan. Aggregated cells do not
    // reveal enough to reconcile overlapping records without double counting.
    let mut seen_bucket_keys_by_fingerprint: HashMap<String, HashSet<String>> = HashMap::new();
    let mut supplemental_buckets_by_environment: HashMap<EnvironmentId, HashSet<usize>> =
        HashMap::new();
    for environment in &ordered {
        for source in &environment.summary.sources {
            if source.status != UsageSourceStatus::Partial {
                continue;
            }
            let key = fingerprint_key(&source.fingerprint);
            let Some(&(owner_index, owner_source_index)) = owner_scan_by_fingerprint.get(&key)
            else {
                continue;
            };
            let owner = ordered[owner_index];
            let owner_source = &owner.summary.sources[owner_source_index];
            if owner_source.status != UsageSourceStatus::Ok {
                continue;
            }
            // JS `a <= b` is false for NaN, so an unparseable read time does not skip the source.
            if let (Some(read), Some(owner_read)) = (
                parse_date_ms(&environment.summary.read_at),
                parse_date_ms(&owner.summary.read_at),
            ) && read <= owner_read
            {
                continue;
            }
            if !seen_bucket_keys_by_fingerprint.contains_key(&key) {
                let seen: HashSet<String> = buckets_for_source(&owner.summary, owner_source)
                    .into_iter()
                    .map(|index| bucket_key(&owner.summary.buckets[index]))
                    .collect();
                seen_bucket_keys_by_fingerprint.insert(key.clone(), seen);
            }
            let seen = seen_bucket_keys_by_fingerprint
                .get_mut(&key)
                .expect("inserted above");
            let mut added = false;
            let mut new_indexes = Vec::new();
            for index in buckets_for_source(&environment.summary, source) {
                let cell = bucket_key(&environment.summary.buckets[index]);
                if seen.contains(&cell) {
                    continue;
                }
                seen.insert(cell);
                new_indexes.push(index);
                added = true;
            }
            if !added {
                continue;
            }
            supplemental_buckets_by_environment
                .entry(environment.environment_id.clone())
                .or_default()
                .extend(new_indexes);
            let previous = sessions_by_fingerprint.get(&key).copied().unwrap_or(0);
            sessions_by_fingerprint.insert(key, previous.max(source.distinct_sessions));
        }
    }

    SourceClaims {
        owner_by_fingerprint,
        supplemental_buckets_by_environment,
        sessions_by_fingerprint,
        duplicates,
    }
}

/// Sources this environment owns after fingerprint claims, plus their buckets.
/// Returns bucket indexes into `environment.summary.buckets`.
fn owned_contribution(
    environment: &EnvironmentUsage,
    claims: &SourceClaims,
    supplemental_buckets: &HashSet<usize>,
) -> (Vec<usize>, IndexMap<UsageProviderKind, u64>) {
    let mut owned_providers: HashSet<UsageProviderKind> = HashSet::new();
    let mut owned_sources: HashSet<String> = HashSet::new();
    let mut sessions_by_provider: IndexMap<UsageProviderKind, u64> = IndexMap::new();
    for source in &environment.summary.sources {
        if source.status == UsageSourceStatus::Missing {
            continue;
        }
        let key = fingerprint_key(&source.fingerprint);
        if claims.owner_by_fingerprint.get(&key) == Some(&environment.environment_id) {
            let provider = source.fingerprint.provider;
            owned_providers.insert(provider);
            owned_sources.insert(format!(
                "{}\u{0}{}",
                provider_name(provider),
                source.fingerprint.resolved_home_path.as_str()
            ));
            // Distinct within a directory. Summing per-bucket session counts instead
            // would count a session once per day and model it spans.
            let sessions = claims
                .sessions_by_fingerprint
                .get(&key)
                .copied()
                .unwrap_or(source.distinct_sessions);
            *sessions_by_provider.entry(provider).or_insert(0) += sessions;
        }
    }
    let buckets = environment
        .summary
        .buckets
        .iter()
        .enumerate()
        .filter(|(index, bucket)| {
            supplemental_buckets.contains(index)
                || match &bucket.source_path {
                    None => owned_providers.contains(&bucket.provider),
                    Some(path) => owned_sources.contains(&format!(
                        "{}\u{0}{}",
                        provider_name(bucket.provider),
                        path.as_str()
                    )),
                }
        })
        .map(|(index, _)| index)
        .collect();
    (buckets, sessions_by_provider)
}

fn bucket_tokens(bucket: &UsageBucket) -> u64 {
    // reasoningTokens is a subset of outputTokens and must not be added again.
    bucket.totals.uncached_input_tokens
        + bucket.totals.cached_input_tokens
        + bucket.totals.cache_creation_tokens
        + bucket.totals.output_tokens
}

pub fn is_compatible_usage_contract_version(version: f64, expected: f64) -> bool {
    version >= USAGE_MERGE_COMPATIBLE_SINCE && version <= expected
}

struct ProviderAccumulator {
    cost_usd: f64,
    total_tokens: u64,
    records: u64,
    sessions: u64,
}

struct ModelAccumulator {
    provider: UsageProviderKind,
    cost_usd: f64,
    total_tokens: u64,
    tokens: UsageTokenTotals,
    records: u64,
    unpriced_records: u64,
    unpriced_tokens: f64,
}

struct DayAccumulator {
    cost_usd: f64,
    total_tokens: u64,
    by_provider: IndexMap<UsageProviderKind, ProviderSpend>,
}

struct HourAccumulator {
    day: String,
    hour_start: String,
    cost_usd: f64,
    total_tokens: u64,
    by_provider: IndexMap<UsageProviderKind, ProviderSpend>,
}

fn add_spend(
    by_provider: &mut IndexMap<UsageProviderKind, ProviderSpend>,
    provider: UsageProviderKind,
    cost_usd: f64,
    total_tokens: u64,
) {
    let spend = by_provider.entry(provider).or_insert(ProviderSpend {
        cost_usd: 0.0,
        total_tokens: 0,
    });
    spend.cost_usd += cost_usd;
    spend.total_tokens += total_tokens;
}

/// Merges every connected environment's summary.
///
/// `expected_contract_version` guards against incompatible server code: rather
/// than blocking the page, its data is excluded and the mismatch direction is
/// reported so the UI can identify which side needs updating. Versions in
/// [`USAGE_MERGE_COMPATIBLE_SINCE`, expected] still merge, so an additive
/// provider expansion does not drop Claude/Codex totals from older servers.
pub fn merge_usage(
    environments: &[EnvironmentUsage],
    expected_contract_version: f64,
) -> MergedUsage {
    if environments.is_empty() {
        return MergedUsage::empty();
    }

    let mut current: Vec<&EnvironmentUsage> = Vec::new();
    let mut contract_mismatches: Vec<UsageContractMismatch> = Vec::new();
    for environment in environments {
        let version = environment.summary.contract_version;
        if is_compatible_usage_contract_version(version, expected_contract_version) {
            current.push(environment);
        } else {
            contract_mismatches.push(UsageContractMismatch {
                environment_id: environment.environment_id.clone(),
                direction: if version < expected_contract_version {
                    UsageContractMismatchDirection::ServerBehind
                } else {
                    UsageContractMismatchDirection::ClientBehind
                },
                contract_version: version,
            });
        }
    }

    let claims = claim_sources(&current);

    let mut cost_usd = 0.0;
    let mut uncached_input_tokens = 0_u64;
    let mut cached_input_tokens = 0_u64;
    let mut cache_creation_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut reasoning_tokens = 0_u64;
    let mut records = 0_u64;
    let mut sessions = 0_u64;
    let mut cache_savings_usd = 0.0;
    let mut provider_reported_records = 0_u64;
    let mut unpriced_records = 0_u64;
    let mut category_input = 0.0;
    let mut category_cache_read = 0.0;
    let mut category_cache_write = 0.0;
    let mut category_output = 0.0;
    let mut speed_fast = 0.0;
    let mut speed_ultrafast = 0.0;
    let mut speed_premium = 0.0;

    let mut provider_accumulator: IndexMap<UsageProviderKind, ProviderAccumulator> =
        IndexMap::new();
    let mut model_accumulator: IndexMap<(UsageProviderKind, String), ModelAccumulator> =
        IndexMap::new();
    let mut daily_accumulator: IndexMap<String, DayAccumulator> = IndexMap::new();
    let mut hourly_accumulator: IndexMap<String, HourAccumulator> = IndexMap::new();
    let mut contributing_environments: Vec<EnvironmentId> = Vec::new();

    let no_supplement = HashSet::new();
    for environment in &current {
        let supplemental = claims
            .supplemental_buckets_by_environment
            .get(&environment.environment_id)
            .unwrap_or(&no_supplement);
        let (bucket_indexes, sessions_by_provider) =
            owned_contribution(environment, &claims, supplemental);
        if !bucket_indexes.is_empty() {
            contributing_environments.push(environment.environment_id.clone());
        }

        for (provider, provider_sessions) in &sessions_by_provider {
            sessions += provider_sessions;
            if *provider_sessions == 0 {
                continue;
            }
            let entry = provider_accumulator
                .entry(*provider)
                .or_insert(ProviderAccumulator {
                    cost_usd: 0.0,
                    total_tokens: 0,
                    records: 0,
                    sessions: 0,
                });
            entry.sessions += provider_sessions;
        }

        for index in bucket_indexes {
            let bucket = &environment.summary.buckets[index];
            let tokens = bucket_tokens(bucket);

            cost_usd += bucket.cost_usd;
            cache_savings_usd += bucket.cache_savings_usd;
            uncached_input_tokens += bucket.totals.uncached_input_tokens;
            cached_input_tokens += bucket.totals.cached_input_tokens;
            cache_creation_tokens += bucket.totals.cache_creation_tokens;
            output_tokens += bucket.totals.output_tokens;
            reasoning_tokens += bucket.totals.reasoning_tokens;
            records += bucket.records;
            unpriced_records += bucket.unpriced_records;
            if bucket.cost_source == UsageCostSource::ProviderReported {
                provider_reported_records += bucket.records;
            }
            if let Some(category) = &bucket.category_cost_usd {
                category_input += category.input;
                category_cache_read += category.cache_read;
                category_cache_write += category.cache_write;
                category_output += category.output;
            }
            speed_fast += bucket.fast_cost_usd.unwrap_or(0.0);
            speed_ultrafast += bucket.ultrafast_cost_usd.unwrap_or(0.0);
            speed_premium += bucket.speed_premium_usd.unwrap_or(0.0);

            let provider =
                provider_accumulator
                    .entry(bucket.provider)
                    .or_insert(ProviderAccumulator {
                        cost_usd: 0.0,
                        total_tokens: 0,
                        records: 0,
                        sessions: 0,
                    });
            provider.cost_usd += bucket.cost_usd;
            provider.total_tokens += tokens;
            provider.records += bucket.records;

            let model = model_accumulator
                .entry((bucket.provider, bucket.model.as_str().to_owned()))
                .or_insert(ModelAccumulator {
                    provider: bucket.provider,
                    cost_usd: 0.0,
                    total_tokens: 0,
                    tokens: UsageTokenTotals::default(),
                    records: 0,
                    unpriced_records: 0,
                    unpriced_tokens: 0.0,
                });
            model.cost_usd += bucket.cost_usd;
            model.total_tokens += tokens;
            model.tokens = UsageTokenTotals {
                uncached_input_tokens: model.tokens.uncached_input_tokens
                    + bucket.totals.uncached_input_tokens,
                cached_input_tokens: model.tokens.cached_input_tokens
                    + bucket.totals.cached_input_tokens,
                cache_creation_tokens: model.tokens.cache_creation_tokens
                    + bucket.totals.cache_creation_tokens,
                output_tokens: model.tokens.output_tokens + bucket.totals.output_tokens,
                reasoning_tokens: model.tokens.reasoning_tokens + bucket.totals.reasoning_tokens,
            };
            model.records += bucket.records;
            model.unpriced_records += bucket.unpriced_records;
            if bucket.records > 0 {
                model.unpriced_tokens +=
                    (tokens as f64) * (bucket.unpriced_records as f64) / (bucket.records as f64);
            }

            let day = daily_accumulator
                .entry(bucket.day.as_str().to_owned())
                .or_insert_with(|| DayAccumulator {
                    cost_usd: 0.0,
                    total_tokens: 0,
                    by_provider: IndexMap::new(),
                });
            day.cost_usd += bucket.cost_usd;
            day.total_tokens += tokens;
            add_spend(
                &mut day.by_provider,
                bucket.provider,
                bucket.cost_usd,
                tokens,
            );

            if let Some(hour_start) = &bucket.hour_start {
                let hour = hourly_accumulator
                    .entry(hour_start.as_str().to_owned())
                    .or_insert_with(|| HourAccumulator {
                        day: bucket.day.as_str().to_owned(),
                        hour_start: hour_start.as_str().to_owned(),
                        cost_usd: 0.0,
                        total_tokens: 0,
                        by_provider: IndexMap::new(),
                    });
                hour.cost_usd += bucket.cost_usd;
                hour.total_tokens += tokens;
                add_spend(
                    &mut hour.by_provider,
                    bucket.provider,
                    bucket.cost_usd,
                    tokens,
                );
            }
        }
    }

    let total_tokens =
        uncached_input_tokens + cached_input_tokens + cache_creation_tokens + output_tokens;
    let share = |part: f64, whole: f64| if whole == 0.0 { 0.0 } else { part / whole };

    let mut providers: Vec<ProviderTotals> = provider_accumulator
        .into_iter()
        .map(|(provider, totals)| ProviderTotals {
            provider,
            cost_usd: totals.cost_usd,
            total_tokens: totals.total_tokens,
            records: totals.records,
            sessions: totals.sessions,
            cost_share: share(totals.cost_usd, cost_usd),
            token_share: share(totals.total_tokens as f64, total_tokens as f64),
        })
        .collect();
    providers.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(Ordering::Equal)
    });

    let mut models: Vec<ModelTotals> = model_accumulator
        .into_iter()
        .map(|((_, model), totals)| ModelTotals {
            model,
            provider: totals.provider,
            cost_usd: totals.cost_usd,
            total_tokens: totals.total_tokens,
            tokens: totals.tokens,
            records: totals.records,
            unpriced_records: totals.unpriced_records,
            unpriced_tokens: totals.unpriced_tokens,
            cost_share: share(totals.cost_usd, cost_usd),
            token_share: share(totals.total_tokens as f64, total_tokens as f64),
        })
        .collect();
    models.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.total_tokens.cmp(&a.total_tokens))
    });

    let mut daily: Vec<DailyTotals> = daily_accumulator
        .into_iter()
        .map(|(day, totals)| DailyTotals {
            day,
            cost_usd: totals.cost_usd,
            total_tokens: totals.total_tokens,
            by_provider: totals.by_provider,
        })
        .collect();
    daily.sort_by(|a, b| locale_compare(&a.day, &b.day));

    let mut hourly: Vec<HourlyTotals> = hourly_accumulator
        .into_values()
        .map(|hour| HourlyTotals {
            day: hour.day,
            hour_start: hour.hour_start,
            cost_usd: hour.cost_usd,
            total_tokens: hour.total_tokens,
            by_provider: hour.by_provider,
        })
        .collect();
    hourly.sort_by(|a, b| locale_compare(&a.hour_start, &b.hour_start));

    let records_f = records as f64;
    let cost_quality = CostQuality {
        provider_reported_share: share(provider_reported_records as f64, records_f),
        unpriced_share: share(unpriced_records as f64, records_f),
        model_priced_share: share(
            records_f - provider_reported_records as f64 - unpriced_records as f64,
            records_f,
        ),
        cache_savings_usd,
    };

    // Clamped so float error never shows as a negative remainder.
    let category_cost = CategoryCost {
        input: category_input,
        cache_read: category_cache_read,
        cache_write: category_cache_write,
        output: category_output,
        unsplit: (cost_usd
            - category_input
            - category_cache_read
            - category_cache_write
            - category_output)
            .max(0.0),
    };
    let speed_cost = SpeedCost {
        fast: speed_fast,
        ultrafast: speed_ultrafast,
        premium: speed_premium,
        standard: (cost_usd - speed_fast - speed_ultrafast).max(0.0),
    };

    MergedUsage {
        cost_usd,
        uncached_input_tokens,
        cached_input_tokens,
        cache_creation_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
        records,
        sessions,
        providers,
        models,
        daily,
        hourly,
        cost_quality,
        category_cost,
        speed_cost,
        duplicate_sources: claims.duplicates,
        contributing_environments,
        contract_mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const CONTRACT: f64 = turin_contracts::usage::USAGE_CONTRACT_VERSION;

    /// Shallow merge, as a JS object spread.
    fn merge(base: Value, overrides: Value) -> Value {
        let mut merged = base;
        for (key, value) in overrides.as_object().expect("overrides object") {
            merged[key] = value.clone();
        }
        merged
    }

    fn bucket(overrides: Value) -> Value {
        merge(
            json!({
                "day": "2026-08-07",
                "provider": "claude",
                "model": "claude-fable-5",
                "totals": {
                    "uncachedInputTokens": 100,
                    "cachedInputTokens": 1000,
                    "cacheCreationTokens": 10,
                    "outputTokens": 50,
                    "reasoningTokens": 0,
                },
                "costUsd": 10,
                "cacheSavingsUsd": 2,
                "costSource": "modelPriced",
                "records": 5,
                "unpricedRecords": 0,
                "sessions": 1,
            }),
            overrides,
        )
    }

    /// One `sources` entry. `volume` defaults to `vol-<host>`, as the TS fixture does.
    fn source(provider: &str, host: &str, home: &str, overrides: Value) -> Value {
        merge(
            json!({
                "fingerprint": {
                    "hostId": host,
                    "provider": provider,
                    "resolvedHomePath": home,
                    "volumeId": format!("vol-{host}"),
                },
                "status": "ok",
                "scannedFiles": 1,
                "skippedFiles": 0,
                "malformedRecords": 0,
                "distinctSessions": 1,
                "message": null,
            }),
            overrides,
        )
    }

    fn summary(buckets: Vec<Value>, sources: Vec<Value>, version: f64) -> Value {
        json!({
            "contractVersion": version,
            "readAt": "2026-08-07T00:00:00.000Z",
            "timeZone": "UTC",
            "sinceDay": "2026-08-01",
            "untilDay": "2026-08-31",
            "buckets": buckets,
            "sources": sources,
            "pricing": {"status": "fresh", "source": "litellm", "fetchedAt": null, "knownModels": 10},
            "scanDurationMs": 1,
        })
    }

    fn with_status(mut value: Value, status: &str) -> Value {
        for entry in value["sources"].as_array_mut().expect("sources") {
            entry["status"] = json!(status);
        }
        value
    }

    fn with_read_at(mut value: Value, read_at: &str) -> Value {
        value["readAt"] = json!(read_at);
        value
    }

    fn env(id: &str, value: Value) -> EnvironmentUsage {
        let summary: UsageSummary = serde_json::from_value(value).expect("summary fixture decodes");
        EnvironmentUsage {
            environment_id: EnvironmentId::new(id).expect("environment id"),
            label: id.to_owned(),
            summary,
        }
    }

    fn claude_home() -> Value {
        source("claude", "mac", "/home/theo/.claude", json!({}))
    }

    fn merged(environments: Vec<EnvironmentUsage>) -> MergedUsage {
        merge_usage(&environments, CONTRACT)
    }

    fn ids(merged: &MergedUsage) -> Vec<&str> {
        merged
            .contributing_environments
            .iter()
            .map(|id| id.as_str())
            .collect()
    }

    #[test]
    fn counts_a_cursor_account_once_across_servers_while_retaining_each_servers_other_providers() {
        let account = |volume: &str| {
            source(
                "cursor",
                "cursor.com",
                "cursor-account:account-hash",
                json!({
                    "fingerprint": {"hostId": "cursor.com", "provider": "cursor", "resolvedHomePath": "cursor-account:account-hash", "volumeId": volume},
                }),
            )
        };
        let cursor_bucket =
            bucket(json!({"provider": "cursor", "sourcePath": "cursor-account:account-hash"}));
        let merged_usage = merged(vec![
            env(
                "mac",
                summary(
                    vec![cursor_bucket.clone()],
                    vec![account("account-hash")],
                    CONTRACT,
                ),
            ),
            env(
                "linux",
                summary(
                    vec![
                        cursor_bucket,
                        bucket(json!({"provider": "opencode", "sourcePath": "/opencode"})),
                    ],
                    vec![
                        account("account-hash"),
                        source("opencode", "linux", "/opencode", json!({})),
                    ],
                    CONTRACT,
                ),
            ),
        ]);
        let mut totals: Vec<(String, f64)> = merged_usage
            .providers
            .iter()
            .map(|p| (provider_name(p.provider).to_owned(), p.cost_usd))
            .collect();
        totals.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            totals,
            vec![("cursor".to_owned(), 10.0), ("opencode".to_owned(), 10.0)]
        );
        assert_eq!(merged_usage.duplicate_sources.len(), 1);
    }

    #[test]
    fn sums_environments_that_read_different_transcript_directories() {
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(
                    vec![bucket(json!({}))],
                    vec![source("claude", "mac", "/a/.claude", json!({}))],
                    CONTRACT,
                ),
            ),
            env(
                "env-b",
                summary(
                    vec![bucket(json!({}))],
                    vec![source("claude", "linux", "/b/.claude", json!({}))],
                    CONTRACT,
                ),
            ),
        ]);
        assert_eq!(merged_usage.cost_usd, 20.0);
        assert_eq!(merged_usage.records, 10);
        assert!(merged_usage.duplicate_sources.is_empty());
    }

    #[test]
    fn counts_a_shared_transcript_directory_once() {
        // Two worktree servers on one machine resolve the same provider home.
        let shared = claude_home();
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(vec![bucket(json!({}))], vec![shared.clone()], CONTRACT),
            ),
            env(
                "env-b",
                summary(vec![bucket(json!({}))], vec![shared], CONTRACT),
            ),
        ]);
        assert_eq!(merged_usage.cost_usd, 10.0);
        assert_eq!(merged_usage.records, 5);
        assert_eq!(merged_usage.sessions, 1);
        assert_eq!(merged_usage.duplicate_sources.len(), 1);
        assert_eq!(ids(&merged_usage), vec!["env-a"]);
    }

    #[test]
    fn drops_only_the_duplicated_provider_keeping_the_environments_other_one() {
        let shared_claude = claude_home();
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(
                    vec![bucket(json!({}))],
                    vec![shared_claude.clone()],
                    CONTRACT,
                ),
            ),
            env(
                "env-b",
                summary(
                    vec![
                        bucket(json!({})),
                        bucket(json!({"provider": "codex", "model": "gpt-5.6-sol", "costUsd": 4})),
                    ],
                    vec![
                        shared_claude,
                        source("codex", "mac", "/home/theo/.codex", json!({})),
                    ],
                    CONTRACT,
                ),
            ),
        ]);
        // env-b's claude bucket is dropped, its codex bucket survives.
        assert_eq!(merged_usage.cost_usd, 14.0);
        let mut names: Vec<&str> = merged_usage
            .providers
            .iter()
            .map(|p| provider_name(p.provider))
            .collect();
        names.sort();
        assert_eq!(names, vec!["claude", "codex"]);
        assert_eq!(merged_usage.sessions, 2);
        let sessions: Vec<(&str, u64)> = merged_usage
            .providers
            .iter()
            .map(|p| (provider_name(p.provider), p.sessions))
            .collect();
        assert!(sessions.contains(&("claude", 1)) && sessions.contains(&("codex", 1)));
    }

    #[test]
    fn counts_overlapping_provider_roots_once_while_keeping_each_environments_unique_root() {
        let source_at = |home: &str| source("opencode", "host", home, json!({}));
        let usage = |path: &str, cost: f64| {
            bucket(json!({"provider": "opencode", "sourcePath": path, "costUsd": cost}))
        };
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(
                    vec![usage("/shared", 10.0), usage("/a", 2.0)],
                    vec![source_at("/shared"), source_at("/a")],
                    CONTRACT,
                ),
            ),
            env(
                "env-b",
                summary(
                    vec![usage("/shared", 10.0), usage("/b", 3.0)],
                    vec![source_at("/shared"), source_at("/b")],
                    CONTRACT,
                ),
            ),
        ]);
        assert_eq!(merged_usage.cost_usd, 15.0);
        assert_eq!(merged_usage.sessions, 3);
    }

    #[test]
    fn uses_the_newest_scan_when_environments_share_the_same_transcript_directory() {
        let shared = claude_home();
        let older = env(
            "env-a",
            summary(
                vec![bucket(json!({"costUsd": 4, "records": 2}))],
                vec![shared.clone()],
                CONTRACT,
            ),
        );
        let newer = env(
            "env-b",
            with_read_at(
                summary(vec![bucket(json!({}))], vec![shared], CONTRACT),
                "2026-08-07T01:00:00.000Z",
            ),
        );
        for ordered in [vec![older.clone(), newer.clone()], vec![newer, older]] {
            let merged_usage = merged(ordered);
            assert_eq!(merged_usage.cost_usd, 10.0);
            assert_eq!(merged_usage.records, 5);
            assert_eq!(merged_usage.sessions, 1);
            assert_eq!(ids(&merged_usage), vec!["env-b"]);
            assert_eq!(
                merged_usage.duplicate_sources,
                vec!["env-a: /home/theo/.claude"]
            );
        }
    }

    #[test]
    fn prefers_a_complete_scan_over_a_newer_partial_scan_of_the_same_directory() {
        let shared = claude_home();
        let incomplete = summary(
            vec![bucket(json!({"costUsd": 4, "records": 2}))],
            vec![shared.clone()],
            CONTRACT,
        );
        let partial = env(
            "new",
            with_status(
                with_read_at(incomplete, "2026-08-07T01:00:00.000Z"),
                "partial",
            ),
        );
        let complete = env(
            "old",
            summary(vec![bucket(json!({}))], vec![shared], CONTRACT),
        );
        for ordered in [
            vec![partial.clone(), complete.clone()],
            vec![complete.clone(), partial.clone()],
        ] {
            let merged_usage = merged(ordered);
            assert_eq!(merged_usage.cost_usd, 10.0);
            assert_eq!(ids(&merged_usage), vec!["old"]);
            assert_eq!(
                merged_usage.duplicate_sources,
                vec!["new: /home/theo/.claude"]
            );
        }
        assert_eq!(merged(vec![partial]).cost_usd, 4.0);
    }

    #[test]
    fn keeps_new_cells_from_a_later_partial_scan_without_recounting_older_cells() {
        let shared = claude_home();
        let complete = env(
            "old",
            summary(
                vec![bucket(json!({}))],
                vec![shared.clone()],
                USAGE_MERGE_COMPATIBLE_SINCE,
            ),
        );
        let partial_summary = summary(
            vec![
                bucket(json!({"sourcePath": "/home/theo/.claude", "costUsd": 4, "records": 2})),
                bucket(
                    json!({"day": "2026-08-08", "sourcePath": "/home/theo/.claude", "costUsd": 3, "records": 1}),
                ),
            ],
            vec![merge(shared, json!({"distinctSessions": 2}))],
            CONTRACT,
        );
        let partial = env(
            "new",
            with_status(
                with_read_at(partial_summary, "2026-08-08T01:00:00.000Z"),
                "partial",
            ),
        );
        for ordered in [
            vec![complete.clone(), partial.clone()],
            vec![partial.clone(), complete.clone()],
        ] {
            let order_ids: Vec<EnvironmentId> =
                ordered.iter().map(|e| e.environment_id.clone()).collect();
            let merged_usage = merged(ordered);
            assert_eq!(merged_usage.cost_usd, 13.0);
            assert_eq!(merged_usage.records, 6);
            assert_eq!(merged_usage.sessions, 2);
            let daily: Vec<(&str, f64)> = merged_usage
                .daily
                .iter()
                .map(|d| (d.day.as_str(), d.cost_usd))
                .collect();
            assert_eq!(daily, vec![("2026-08-07", 10.0), ("2026-08-08", 3.0)]);
            assert_eq!(merged_usage.contributing_environments, order_ids);
            assert_eq!(
                merged_usage.duplicate_sources,
                vec!["new: /home/theo/.claude"]
            );
        }
    }

    #[test]
    fn retains_a_complete_cell_when_a_larger_partial_cell_may_have_skipped_old_records() {
        let shared = claude_home();
        let complete = env(
            "old",
            summary(vec![bucket(json!({}))], vec![shared.clone()], CONTRACT),
        );
        let partial_summary = summary(
            vec![bucket(json!({
                "costUsd": 4,
                "records": 6,
                "totals": {
                    "uncachedInputTokens": 80,
                    "cachedInputTokens": 500,
                    "cacheCreationTokens": 10,
                    "outputTokens": 30,
                    "reasoningTokens": 0,
                },
            }))],
            vec![merge(shared, json!({"distinctSessions": 2}))],
            CONTRACT,
        );
        let partial = env(
            "new",
            with_status(
                with_read_at(partial_summary, "2026-08-07T01:00:00.000Z"),
                "partial",
            ),
        );
        let merged_usage = merged(vec![complete, partial]);
        assert_eq!(merged_usage.cost_usd, 10.0);
        assert_eq!(merged_usage.total_tokens, 1160);
        assert_eq!(merged_usage.records, 5);
        assert_eq!(merged_usage.sessions, 1);
        assert_eq!(ids(&merged_usage), vec!["old"]);
    }

    #[test]
    fn identifies_an_environment_reporting_an_older_contract_version() {
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(
                    vec![bucket(json!({}))],
                    vec![source("claude", "mac", "/a", json!({}))],
                    CONTRACT,
                ),
            ),
            env(
                "env-b",
                summary(
                    vec![bucket(json!({}))],
                    vec![source("claude", "linux", "/b", json!({}))],
                    USAGE_MERGE_COMPATIBLE_SINCE - 1.0,
                ),
            ),
        ]);
        assert_eq!(merged_usage.cost_usd, 10.0);
        assert_eq!(
            merged_usage.contract_mismatches,
            vec![UsageContractMismatch {
                environment_id: EnvironmentId::new("env-b").unwrap(),
                direction: UsageContractMismatchDirection::ServerBehind,
                contract_version: USAGE_MERGE_COMPATIBLE_SINCE - 1.0,
            }]
        );
    }

    #[test]
    fn identifies_an_environment_reporting_a_newer_contract_version() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![bucket(json!({}))],
                vec![source("claude", "mac", "/a", json!({}))],
                CONTRACT + 1.0,
            ),
        )]);
        assert_eq!(merged_usage.cost_usd, 0.0);
        assert_eq!(
            merged_usage.contract_mismatches,
            vec![UsageContractMismatch {
                environment_id: EnvironmentId::new("env-a").unwrap(),
                direction: UsageContractMismatchDirection::ClientBehind,
                contract_version: CONTRACT + 1.0,
            }]
        );
    }

    #[test]
    fn keeps_the_previous_compatible_contract_version_so_additive_provider_expansions_still_merge()
    {
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(
                    vec![bucket(json!({"costUsd": 10}))],
                    vec![source("claude", "mac", "/a", json!({}))],
                    CONTRACT,
                ),
            ),
            env(
                "env-b",
                summary(
                    vec![bucket(
                        json!({"costUsd": 4, "provider": "codex", "model": "gpt-5.6-sol"}),
                    )],
                    vec![source("codex", "linux", "/b", json!({}))],
                    CONTRACT - 1.0,
                ),
            ),
        ]);
        assert_eq!(merged_usage.cost_usd, 14.0);
        assert!(merged_usage.contract_mismatches.is_empty());
    }

    #[test]
    fn excludes_a_future_incompatible_contract_even_when_its_buckets_still_decode() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![bucket(json!({}))],
                vec![source("claude", "mac", "/a", json!({}))],
                CONTRACT + 1.0,
            ),
        )]);
        assert_eq!(merged_usage.cost_usd, 0.0);
        assert_eq!(
            merged_usage.contract_mismatches[0].direction,
            UsageContractMismatchDirection::ClientBehind
        );
    }

    #[test]
    fn derives_provider_shares_and_cost_quality() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![
                    bucket(json!({"costUsd": 75})),
                    bucket(
                        json!({"provider": "codex", "model": "gpt-5.6-sol", "costUsd": 25, "unpricedRecords": 5}),
                    ),
                ],
                vec![
                    source("claude", "mac", "/a/.claude", json!({})),
                    source("codex", "mac", "/a/.codex", json!({})),
                ],
                CONTRACT,
            ),
        )]);
        assert_eq!(
            merged_usage.providers[0].provider,
            UsageProviderKind::Claude
        );
        assert!((merged_usage.providers[0].cost_share - 0.75).abs() < 1e-9);
        assert!((merged_usage.cost_quality.unpriced_share - 0.5).abs() < 1e-9);
        assert_eq!(merged_usage.cost_quality.cache_savings_usd, 4.0);
    }

    #[test]
    fn derives_model_token_shares_independently_of_their_cost_shares() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![
                    bucket(json!({"costUsd": 90})),
                    bucket(json!({
                        "provider": "codex",
                        "model": "gpt-5.6-sol",
                        "costUsd": 10,
                        "totals": {"uncachedInputTokens": 3 * 1160, "cachedInputTokens": 0, "cacheCreationTokens": 0, "outputTokens": 0, "reasoningTokens": 0},
                    })),
                ],
                vec![
                    source("claude", "mac", "/a/.claude", json!({})),
                    source("codex", "mac", "/a/.codex", json!({})),
                ],
                CONTRACT,
            ),
        )]);
        let by_model = |name: &str| {
            merged_usage
                .models
                .iter()
                .find(|m| m.model == name)
                .unwrap()
        };
        assert!((by_model("claude-fable-5").cost_share - 0.9).abs() < 1e-9);
        assert!((by_model("claude-fable-5").token_share - 0.25).abs() < 1e-9);
        assert!((by_model("gpt-5.6-sol").cost_share - 0.1).abs() < 1e-9);
        assert!((by_model("gpt-5.6-sol").token_share - 0.75).abs() < 1e-9);
    }

    #[test]
    fn marks_a_model_with_no_known_rates_as_unpriced_rather_than_free() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![
                    bucket(json!({"costUsd": 75})),
                    bucket(json!({
                        "provider": "codex",
                        "model": "unknown-model",
                        "costUsd": 0,
                        "costSource": "unpriced",
                        "unpricedRecords": 5,
                    })),
                ],
                vec![
                    source("claude", "mac", "/a/.claude", json!({})),
                    source("codex", "mac", "/a/.codex", json!({})),
                ],
                CONTRACT,
            ),
        )]);
        assert_eq!(
            merged_usage
                .models
                .iter()
                .find(|m| m.model == "unknown-model")
                .unwrap()
                .unpriced_records,
            5
        );
        let unknown: Vec<&str> = merged_usage
            .models
            .iter()
            .filter(|m| is_model_cost_unknown(m))
            .map(|m| m.model.as_str())
            .collect();
        assert_eq!(unknown, vec!["unknown-model"]);
    }

    #[test]
    fn splits_cost_by_category_and_speed_counting_older_servers_as_unsplit_standard_cost() {
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(
                    vec![
                        bucket(json!({
                            "costUsd": 10,
                            "categoryCostUsd": {"input": 1, "cacheRead": 2, "cacheWrite": 3, "output": 4},
                            "fastCostUsd": 6,
                            "speedPremiumUsd": 3,
                        })),
                        bucket(json!({
                            "provider": "codex",
                            "model": "unknown-model",
                            "costUsd": 0,
                            "costSource": "unpriced",
                            "unpricedRecords": 5,
                        })),
                        // Reported cost on one record, no rates for the other four.
                        bucket(json!({
                            "provider": "codex",
                            "model": "partly-reported",
                            "costUsd": 0,
                            "unpricedRecords": 4,
                        })),
                    ],
                    vec![
                        source("claude", "mac", "/a/.claude", json!({})),
                        source("codex", "mac", "/a/.codex", json!({})),
                    ],
                    CONTRACT,
                ),
            ),
            env(
                "env-b",
                summary(
                    vec![bucket(json!({"costUsd": 5}))],
                    vec![source("claude", "linux", "/b/.claude", json!({}))],
                    USAGE_MERGE_COMPATIBLE_SINCE,
                ),
            ),
        ]);
        assert_eq!(
            merged_usage.category_cost,
            CategoryCost {
                input: 1.0,
                cache_read: 2.0,
                cache_write: 3.0,
                output: 4.0,
                unsplit: 5.0,
            }
        );
        assert_eq!(
            merged_usage.speed_cost,
            SpeedCost {
                standard: 9.0,
                fast: 6.0,
                ultrafast: 0.0,
                premium: 3.0,
            }
        );
        let unpriced: Vec<(&str, f64)> = merged_usage
            .models
            .iter()
            .map(|m| (m.model.as_str(), m.unpriced_tokens))
            .collect();
        assert_eq!(
            unpriced,
            vec![
                ("claude-fable-5", 0.0),
                ("unknown-model", 1160.0),
                ("partly-reported", 928.0)
            ]
        );
    }

    #[test]
    fn orders_models_by_cost_descending() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![
                    bucket(json!({"provider": "claude", "model": "lower-cost", "costUsd": 4})),
                    bucket(json!({"provider": "codex", "model": "higher-cost", "costUsd": 9})),
                ],
                vec![
                    source("claude", "mac", "/a/.claude", json!({})),
                    source("codex", "mac", "/a/.codex", json!({})),
                ],
                CONTRACT,
            ),
        )]);
        let order: Vec<&str> = merged_usage
            .models
            .iter()
            .map(|m| m.model.as_str())
            .collect();
        assert_eq!(order, vec!["higher-cost", "lower-cost"]);
    }

    #[test]
    fn keeps_two_machines_apart_when_hostname_and_home_path_collide() {
        // Every Mac resolves /Users/theo/.claude, so a hostname clash used to make
        // one machine's usage vanish. Filesystem identity separates them.
        let shape = |volume: &str| {
            source(
                "claude",
                "mac",
                "/Users/theo/.claude",
                json!({
                    "fingerprint": {"hostId": "mac", "provider": "claude", "resolvedHomePath": "/Users/theo/.claude", "volumeId": volume},
                }),
            )
        };
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(
                    vec![bucket(json!({}))],
                    vec![shape("16777220:1234")],
                    CONTRACT,
                ),
            ),
            env(
                "env-b",
                summary(
                    vec![bucket(json!({}))],
                    vec![shape("16777221:9999")],
                    CONTRACT,
                ),
            ),
        ]);
        assert_eq!(merged_usage.cost_usd, 20.0);
        assert!(merged_usage.duplicate_sources.is_empty());
    }

    #[test]
    fn still_collapses_two_servers_reading_the_same_directory() {
        let same = source(
            "claude",
            "mac",
            "/Users/theo/.claude",
            json!({
                "fingerprint": {"hostId": "mac", "provider": "claude", "resolvedHomePath": "/Users/theo/.claude", "volumeId": "16777220:1234"},
            }),
        );
        let merged_usage = merged(vec![
            env(
                "env-a",
                summary(vec![bucket(json!({}))], vec![same.clone()], CONTRACT),
            ),
            env(
                "env-b",
                summary(vec![bucket(json!({}))], vec![same], CONTRACT),
            ),
        ]);
        assert_eq!(merged_usage.cost_usd, 10.0);
        assert_eq!(merged_usage.duplicate_sources.len(), 1);
    }

    #[test]
    fn totals_sessions_from_per_directory_distinct_counts_not_per_bucket_sums() {
        // One session that spans two days appears in two buckets. Summing bucket
        // sessions would say 2; the source's distinct count says 1.
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![
                    bucket(json!({"day": "2026-08-06"})),
                    bucket(json!({"day": "2026-08-07"})),
                ],
                vec![source(
                    "claude",
                    "mac",
                    "/a/.claude",
                    json!({"distinctSessions": 1}),
                )],
                CONTRACT,
            ),
        )]);
        assert_eq!(merged_usage.sessions, 1);
        assert_eq!(merged_usage.providers[0].sessions, 1);
    }

    #[test]
    fn returns_empty_totals_with_no_environments() {
        let merged_usage = merged(vec![]);
        assert_eq!(merged_usage.cost_usd, 0.0);
        assert!(merged_usage.daily.is_empty());
        assert!(merged_usage.hourly.is_empty());
    }

    #[test]
    fn omits_providers_with_no_sessions_or_usage() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![],
                vec![source(
                    "claude",
                    "mac",
                    "/a/.claude",
                    json!({"distinctSessions": 0}),
                )],
                CONTRACT,
            ),
        )]);
        assert!(merged_usage.providers.is_empty());
    }

    #[test]
    fn derives_hourly_totals_without_losing_the_daily_rollup() {
        let merged_usage = merged(vec![env(
            "env-a",
            summary(
                vec![
                    bucket(json!({"hourStart": "2026-08-07T09:37:00.000Z", "costUsd": 3})),
                    bucket(json!({"hourStart": "2026-08-07T10:37:00.000Z", "costUsd": 7})),
                ],
                vec![source("claude", "mac", "/a/.claude", json!({}))],
                CONTRACT,
            ),
        )]);
        let hourly: Vec<(&str, f64)> = merged_usage
            .hourly
            .iter()
            .map(|h| (h.hour_start.as_str(), h.cost_usd))
            .collect();
        assert_eq!(
            hourly,
            vec![
                ("2026-08-07T09:37:00.000Z", 3.0),
                ("2026-08-07T10:37:00.000Z", 7.0)
            ]
        );
        assert_eq!(merged_usage.daily.len(), 1);
        assert_eq!(merged_usage.daily[0].cost_usd, 10.0);
    }
}
