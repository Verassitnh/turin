//! Port of upstream/apps/server/src/usage/usagePricing.ts.
//!
//! Model rate lookup and cost arithmetic. Rates come from LiteLLM's
//! `model_prices_and_context_window.json`, the same table `ccusage` prices
//! against. Everything here is pure: fetching and caching the table lives in
//! `usage_service`.

use indexmap::IndexMap;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::settings::UsageModelPriceOverride;
use turin_contracts::usage::{UsageCategoryCost, UsageCostSource, UsageTokenTotals};
use turin_provider_core::usage::{UsageRecord, UsageSpeed};

/// Token rates for one billing speed. All values are USD per token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenRates {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_read_cost_per_token: f64,
    pub cache_creation_cost_per_token: f64,
}

/// The subset of a LiteLLM entry we price against: standard rates, plus rates
/// for each faster speed the model publishes. A request at a speed with no
/// published rates bills at the standard rates.
///
/// LiteLLM also publishes `*_above_272k_tokens`, `*_flex`, and `*_batches`
/// variants. Transcripts don't record those, so we don't price them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRate {
    pub base: TokenRates,
    /// From LiteLLM's `provider_specific_entry.fast` multiple (Claude fast mode),
    /// or else its `*_priority` rates (Codex `priority`).
    pub fast: Option<TokenRates>,
    /// From LiteLLM's `*_ultrafast` rates (Codex `ultrafast`).
    pub ultrafast: Option<TokenRates>,
}

pub type RateTable = IndexMap<String, ModelRate>;

/// Custom IDs keep their case, provider prefix, and variant suffix. Custom rates
/// apply as entered, at every speed.
pub fn create_override_rate_table(
    overrides: &IndexMap<
        turin_contracts::base_schemas::TrimmedNonEmptyString,
        UsageModelPriceOverride,
    >,
) -> RateTable {
    overrides
        .iter()
        .map(|(model, prices)| {
            let input = prices.input_cost_per_million_tokens.get() / 1_000_000.0;
            let cache_read = prices
                .cache_read_cost_per_million_tokens
                .map_or(prices.input_cost_per_million_tokens.get(), |price| {
                    price.get()
                })
                / 1_000_000.0;
            let cache_creation = prices
                .cache_write_cost_per_million_tokens
                .map_or(prices.input_cost_per_million_tokens.get(), |price| {
                    price.get()
                })
                / 1_000_000.0;
            (
                js_trim(model.as_str()).to_owned(),
                ModelRate {
                    base: TokenRates {
                        input_cost_per_token: input,
                        output_cost_per_token: prices.output_cost_per_million_tokens.get()
                            / 1_000_000.0,
                        cache_read_cost_per_token: cache_read,
                        cache_creation_cost_per_token: cache_creation,
                    },
                    fast: None,
                    ultrafast: None,
                },
            )
        })
        .collect()
}

/// Reads one rate set, `suffix` selecting a tier such as `_priority`. Returns
/// `None` without both an input and an output rate.
///
/// Anthropic bills cache reads at a discount and cache writes at a premium.
/// When the standard tier omits them, cached input is priced as plain input
/// rather than as free. A faster tier that omits them keeps the standard tier's
/// cache-to-input ratio.
fn read_token_rates(
    entry: &serde_json::Map<String, serde_json::Value>,
    suffix: &str,
    standard: Option<&TokenRates>,
) -> Option<TokenRates> {
    let input = finite_number(entry.get(&format!("input_cost_per_token{suffix}")))?;
    let output = finite_number(entry.get(&format!("output_cost_per_token{suffix}")))?;
    let cache_rate = |name: &str, field: fn(&TokenRates) -> f64| {
        finite_number(entry.get(&format!("{name}{suffix}"))).unwrap_or_else(|| match standard {
            Some(standard) if standard.input_cost_per_token > 0.0 => {
                (field(standard) / standard.input_cost_per_token) * input
            }
            _ => input,
        })
    };
    Some(TokenRates {
        input_cost_per_token: input,
        output_cost_per_token: output,
        cache_read_cost_per_token: cache_rate("cache_read_input_token_cost", |rates| {
            rates.cache_read_cost_per_token
        }),
        cache_creation_cost_per_token: cache_rate("cache_creation_input_token_cost", |rates| {
            rates.cache_creation_cost_per_token
        }),
    })
}

fn finite_number(value: Option<&serde_json::Value>) -> Option<f64> {
    value?.as_f64().filter(|number| number.is_finite())
}

fn scale_token_rates(rates: TokenRates, multiple: f64) -> TokenRates {
    TokenRates {
        input_cost_per_token: rates.input_cost_per_token * multiple,
        output_cost_per_token: rates.output_cost_per_token * multiple,
        cache_read_cost_per_token: rates.cache_read_cost_per_token * multiple,
        cache_creation_cost_per_token: rates.cache_creation_cost_per_token * multiple,
    }
}

/// Reads `provider_specific_entry.fast`, e.g. `2` for Claude Opus 5.5.
fn fast_multiplier(entry: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
    let specific = entry.get("provider_specific_entry")?.as_object()?;
    finite_number(specific.get("fast")).filter(|fast| *fast > 0.0)
}

/// Projects the LiteLLM document into a rate table.
///
/// Entries without both an input and an output rate are dropped: a half-priced
/// model would silently under-report cost, which is worse than reporting the
/// model as unpriced.
///
/// Entries keep their full normalized key; a bare name is aliased only when no
/// canonical entry exists and every qualified entry has the same rate.
pub fn parse_rate_table(document: &serde_json::Value) -> RateTable {
    let mut table = RateTable::new();
    let Some(document) = document.as_object() else {
        return table;
    };

    for (name, raw) in document {
        let Some(entry) = raw.as_object() else {
            continue;
        };
        let Some(standard) = read_token_rates(entry, "", None) else {
            continue;
        };

        let key = normalize_rate_key(name);
        if key.is_empty() {
            continue;
        }
        let fast = match fast_multiplier(entry) {
            None => read_token_rates(entry, "_priority", Some(&standard)),
            Some(multiple) => Some(scale_token_rates(standard, multiple)),
        };
        let ultrafast = read_token_rates(entry, "_ultrafast", Some(&standard));
        table.insert(
            key,
            ModelRate {
                base: standard,
                fast,
                ultrafast,
            },
        );
    }

    // `None` marks a bare name claimed at conflicting rates: no alias for it.
    let mut alias_candidates: IndexMap<String, Option<ModelRate>> = IndexMap::new();
    for (key, rate) in &table {
        let alias = bare_model_name(key);
        if alias.is_empty() || alias == key || table.contains_key(alias) {
            continue;
        }
        match alias_candidates.get(alias) {
            None => {
                alias_candidates.insert(alias.to_owned(), Some(*rate));
            }
            Some(Some(held)) if !same_rate(held, rate) => {
                alias_candidates.insert(alias.to_owned(), None);
            }
            Some(_) => {}
        }
    }
    for (alias, rate) in alias_candidates {
        if let Some(rate) = rate {
            table.insert(alias, rate);
        }
    }

    table
}

fn same_token_rates(a: Option<&TokenRates>, b: Option<&TokenRates>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

fn same_rate(a: &ModelRate, b: &ModelRate) -> bool {
    a.base == b.base
        && same_token_rates(a.fast.as_ref(), b.fast.as_ref())
        && same_token_rates(a.ultrafast.as_ref(), b.ultrafast.as_ref())
}

/// The rates a request at `speed` bills at.
fn rates_at(rate: &ModelRate, speed: UsageSpeed) -> TokenRates {
    match speed {
        UsageSpeed::Standard => None,
        UsageSpeed::Fast => rate.fast,
        UsageSpeed::Ultrafast => rate.ultrafast,
    }
    .unwrap_or(rate.base)
}

fn normalize_rate_key(model: &str) -> String {
    js_trim(model).to_lowercase()
}

fn bare_model_name(key: &str) -> &str {
    match key.rfind('/') {
        None => key,
        Some(slash) => &key[slash + 1..],
    }
}

/// Drops a bracketed variant suffix such as `claude-fable-5-1[1m]`, which
/// Claude Code writes for the 1M context tier. The rate table only knows the
/// base name, and we price at the base tier anyway.
fn strip_variant_suffix(key: &str) -> &str {
    match key.find('[') {
        None => key,
        Some(bracket) => &key[..bracket],
    }
}

/// Models we never price, regardless of the table.
///
/// `<synthetic>` marks locally generated messages that were never billed. Bare
/// family names ("opus", "sonnet") are genuinely ambiguous across generations,
/// so we report them as unpriced instead of guessing a generation.
const UNPRICEABLE_MODELS: &[&str] = &[
    "<synthetic>",
    "synthetic",
    "opus",
    "sonnet",
    "haiku",
    "fable",
];

pub fn lookup_rate<'a>(table: &'a RateTable, model: &str) -> Option<&'a ModelRate> {
    let key = strip_variant_suffix(&normalize_rate_key(model)).to_owned();
    let bare_name = bare_model_name(&key);
    if bare_name.is_empty() || UNPRICEABLE_MODELS.contains(&bare_name) {
        return None;
    }
    table.get(&key)
}

/// What `cost_usd` is split into, and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct PricedUsage {
    pub cost_usd: f64,
    pub cost_source: UsageCostSource,
    /// `cost_usd` by token category, or `None` when no rates are known to split it.
    pub category_cost_usd: Option<UsageCategoryCost>,
    /// What `cost_usd` exceeds the same tokens at standard rates. `0` without rates.
    pub speed_premium_usd: f64,
}

fn cost_by_category(totals: &UsageTokenTotals, rates: &TokenRates) -> UsageCategoryCost {
    UsageCategoryCost {
        input: totals.uncached_input_tokens as f64 * rates.input_cost_per_token,
        cache_read: totals.cached_input_tokens as f64 * rates.cache_read_cost_per_token,
        cache_write: totals.cache_creation_tokens as f64 * rates.cache_creation_cost_per_token,
        output: totals.output_tokens as f64 * rates.output_cost_per_token,
    }
}

fn sum_categories(cost: &UsageCategoryCost) -> f64 {
    cost.input + cost.cache_read + cost.cache_write + cost.output
}

fn unsplit(cost_usd: f64, cost_source: UsageCostSource) -> PricedUsage {
    PricedUsage {
        cost_usd,
        cost_source,
        category_cost_usd: None,
        speed_premium_usd: 0.0,
    }
}

/// Prices one record's tokens.
///
/// A provider-reported cost is kept as is, and split by category and speed in
/// proportion to the model's list rates when those are known.
///
/// `reasoningTokens` is intentionally not charged separately: it is already
/// counted inside `outputTokens`.
pub fn price_usage(
    table: &RateTable,
    record: &UsageRecord,
    overrides: Option<&RateTable>,
) -> PricedUsage {
    let override_rate = overrides.and_then(|overrides| overrides.get(js_trim(&record.model)));
    let reported = match (override_rate, record.reported_cost_usd) {
        (None, Some(cost)) if cost.is_finite() => Some(cost),
        _ => None,
    };
    let rate = match override_rate {
        Some(rate) => Some(rate),
        None => lookup_rate(table, record.rate_model.as_deref().unwrap_or(&record.model)),
    };
    let Some(rate) = rate else {
        return match reported {
            None => unsplit(0.0, UsageCostSource::Unpriced),
            Some(cost) => unsplit(cost, UsageCostSource::ProviderReported),
        };
    };

    let list_cost = cost_by_category(&record.totals, &rates_at(rate, record.speed));
    let list_cost_usd = sum_categories(&list_cost);
    if let Some(reported) = reported.filter(|_| list_cost_usd <= 0.0) {
        return unsplit(reported, UsageCostSource::ProviderReported);
    }
    let premium_usd = if record.speed == UsageSpeed::Standard {
        0.0
    } else {
        list_cost_usd - sum_categories(&cost_by_category(&record.totals, &rate.base))
    };
    let scale = match reported {
        None => 1.0,
        Some(reported) => reported / list_cost_usd,
    };
    PricedUsage {
        cost_usd: reported.unwrap_or(list_cost_usd),
        cost_source: if reported.is_none() {
            UsageCostSource::ModelPriced
        } else {
            UsageCostSource::ProviderReported
        },
        category_cost_usd: Some(UsageCategoryCost {
            input: list_cost.input * scale,
            cache_read: list_cost.cache_read * scale,
            cache_write: list_cost.cache_write * scale,
            output: list_cost.output * scale,
        }),
        speed_premium_usd: premium_usd * scale,
    }
}

/// What the cached input would have cost at full input rates, minus what it
/// actually cost. Drives the "cache savings" figure.
pub fn cache_savings_usd(
    table: &RateTable,
    record: &UsageRecord,
    overrides: Option<&RateTable>,
) -> f64 {
    let rate = overrides
        .and_then(|overrides| overrides.get(js_trim(&record.model)))
        .or_else(|| lookup_rate(table, record.rate_model.as_deref().unwrap_or(&record.model)));
    let Some(rate) = rate else {
        return 0.0;
    };
    let rates = rates_at(rate, record.speed);
    record.totals.cached_input_tokens as f64
        * (rates.input_cost_per_token - rates.cache_read_cost_per_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::settings::UsageModelTokenPrice;
    use turin_contracts::usage::UsageProviderKind;

    fn rate(input: f64, cache_read: Option<f64>) -> Value {
        let mut entry = json!({
            "input_cost_per_token": input,
            "output_cost_per_token": input * 5.0,
        });
        if let Some(cache_read) = cache_read {
            entry["cache_read_input_token_cost"] = json!(cache_read);
        }
        entry
    }

    /// `(model, input $/M, output $/M, cache read $/M, cache write $/M)`.
    type OverrideFixture = (&'static str, f64, f64, Option<f64>, Option<f64>);

    fn override_prices(
        entries: &[OverrideFixture],
    ) -> IndexMap<TrimmedNonEmptyString, UsageModelPriceOverride> {
        entries
            .iter()
            .map(|(model, input, output, cache_read, cache_write)| {
                (
                    TrimmedNonEmptyString::new(model).unwrap(),
                    UsageModelPriceOverride {
                        input_cost_per_million_tokens: UsageModelTokenPrice::new(*input).unwrap(),
                        output_cost_per_million_tokens: UsageModelTokenPrice::new(*output).unwrap(),
                        cache_read_cost_per_million_tokens: cache_read
                            .map(|price| UsageModelTokenPrice::new(price).unwrap()),
                        cache_write_cost_per_million_tokens: cache_write
                            .map(|price| UsageModelTokenPrice::new(price).unwrap()),
                    },
                )
            })
            .collect()
    }

    fn totals() -> UsageTokenTotals {
        UsageTokenTotals {
            uncached_input_tokens: 1_000_000,
            cached_input_tokens: 1_000_000,
            cache_creation_tokens: 1_000_000,
            output_tokens: 1_000_000,
            reasoning_tokens: 500_000,
        }
    }

    fn record(model: &str, reported: Option<f64>, speed: UsageSpeed) -> UsageRecord {
        UsageRecord {
            provider: UsageProviderKind::Claude,
            timestamp_ms: 0.0,
            model: model.to_owned(),
            rate_model: None,
            session_id: String::new(),
            totals: totals(),
            reported_cost_usd: reported,
            speed,
            dedupe_key: None,
        }
    }

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0),
            "{actual} != {expected}"
        );
    }

    #[test]
    fn uses_custom_token_rates_ahead_of_public_and_provider_reported_costs() {
        let table = parse_rate_table(&json!({ "example-model": rate(1.0, None) }));
        let overrides = create_override_rate_table(&override_prices(&[(
            "example-model",
            2.0,
            8.0,
            Some(0.5),
            Some(3.0),
        )]));

        for reported in [None, Some(99.0)] {
            let priced = price_usage(
                &table,
                &record("example-model", reported, UsageSpeed::Standard),
                Some(&overrides),
            );
            close(priced.cost_usd, 13.5);
            assert_eq!(priced.cost_source, UsageCostSource::ModelPriced);
        }
        close(
            cache_savings_usd(
                &table,
                &record("example-model", None, UsageSpeed::Standard),
                Some(&overrides),
            ),
            1.5,
        );
    }

    #[test]
    fn prices_unknown_models_offline_and_uses_input_prices_for_omitted_cache_rates() {
        let table = parse_rate_table(&json!({}));
        let overrides = create_override_rate_table(&override_prices(&[(
            "example-model",
            2.0,
            8.0,
            None,
            None,
        )]));

        let priced = price_usage(
            &table,
            &record("example-model", None, UsageSpeed::Standard),
            Some(&overrides),
        );
        close(priced.cost_usd, 14.0);
        assert_eq!(priced.cost_source, UsageCostSource::ModelPriced);
        assert_eq!(
            cache_savings_usd(
                &table,
                &record("example-model", None, UsageSpeed::Standard),
                Some(&overrides)
            ),
            0.0
        );
    }

    #[test]
    fn preserves_explicit_zero_rates_and_matches_only_the_exact_trimmed_model_id() {
        let table = parse_rate_table(&json!({}));
        let overrides = create_override_rate_table(&override_prices(&[(
            " vendor/example-model[1m] ",
            0.0,
            0.0,
            None,
            None,
        )]));
        let priced = price_usage(
            &table,
            &record(
                " vendor/example-model[1m] ",
                Some(99.0),
                UsageSpeed::Standard,
            ),
            Some(&overrides),
        );
        close(priced.cost_usd, 0.0);
        assert_eq!(priced.cost_source, UsageCostSource::ModelPriced);

        for model in [
            "example-model[1m]",
            "vendor/example-model",
            "vendor/Example-model[1m]",
            "other/example-model[1m]",
        ] {
            assert_eq!(
                price_usage(
                    &table,
                    &record(model, None, UsageSpeed::Standard),
                    Some(&overrides)
                )
                .cost_source,
                UsageCostSource::Unpriced
            );
            assert_eq!(
                price_usage(
                    &table,
                    &record(model, Some(99.0), UsageSpeed::Standard),
                    Some(&overrides)
                ),
                PricedUsage {
                    cost_usd: 99.0,
                    cost_source: UsageCostSource::ProviderReported,
                    category_cost_usd: None,
                    speed_premium_usd: 0.0,
                }
            );
        }
    }

    #[test]
    fn prices_fast_mode_requests_at_the_models_published_fast_multiple() {
        let table = parse_rate_table(&json!({
            "claude-opus-5-5": {
                "input_cost_per_token": 4e-6,
                "output_cost_per_token": 2e-5,
                "cache_read_input_token_cost": 2e-7,
                "provider_specific_entry": { "fast": 2, "us": 1.1 },
            },
            "claude-fable-5-1": {
                "input_cost_per_token": 1e-5,
                "output_cost_per_token": 5e-5,
                "cache_read_input_token_cost": 2.5e-7,
                "provider_specific_entry": { "us": 1.1 },
            },
        }));
        let overrides = create_override_rate_table(&override_prices(&[(
            "claude-opus-5-5",
            4.0,
            20.0,
            None,
            None,
        )]));
        let cost = |model: &str, speed: UsageSpeed, custom: Option<&RateTable>| {
            price_usage(&table, &record(model, None, speed), custom).cost_usd
        };

        close(
            cost("claude-opus-5-5", UsageSpeed::Fast, None),
            2.0 * cost("claude-opus-5-5", UsageSpeed::Standard, None),
        );
        close(
            cache_savings_usd(
                &table,
                &record("claude-opus-5-5", None, UsageSpeed::Fast),
                None,
            ),
            2.0 * cache_savings_usd(
                &table,
                &record("claude-opus-5-5", None, UsageSpeed::Standard),
                None,
            ),
        );
        // No published fast tier, and custom prices, both stay at the standard rate.
        assert_eq!(
            cost("claude-fable-5-1", UsageSpeed::Fast, None),
            cost("claude-fable-5-1", UsageSpeed::Standard, None)
        );
        assert_eq!(
            cost("claude-opus-5-5", UsageSpeed::Fast, Some(&overrides)),
            cost("claude-opus-5-5", UsageSpeed::Standard, Some(&overrides))
        );
    }

    #[test]
    fn splits_cost_by_category_and_prices_the_speed_premium() {
        let table = parse_rate_table(&json!({
            "claude-opus-5-5": {
                "input_cost_per_token": 4e-6,
                "output_cost_per_token": 2e-5,
                "cache_read_input_token_cost": 4e-7,
                "cache_creation_input_token_cost": 5e-6,
                "provider_specific_entry": { "fast": 2 },
            },
        }));
        let split =
            |input: f64, cache_read: f64, cache_write: f64, output: f64| UsageCategoryCost {
                input,
                cache_read,
                cache_write,
                output,
            };
        let assert_split = |actual: Option<UsageCategoryCost>, expected: UsageCategoryCost| {
            let actual = actual.expect("category split");
            close(actual.input, expected.input);
            close(actual.cache_read, expected.cache_read);
            close(actual.cache_write, expected.cache_write);
            close(actual.output, expected.output);
        };

        let fast = price_usage(
            &table,
            &record("claude-opus-5-5", None, UsageSpeed::Fast),
            None,
        );
        close(fast.cost_usd, 58.8);
        assert_eq!(fast.cost_source, UsageCostSource::ModelPriced);
        assert_split(fast.category_cost_usd, split(8.0, 0.8, 10.0, 40.0));
        close(fast.speed_premium_usd, 29.4);

        // A reported cost keeps its total and splits in proportion to list rates.
        let reported = price_usage(
            &table,
            &record("claude-opus-5-5", Some(29.4), UsageSpeed::Fast),
            None,
        );
        close(reported.cost_usd, 29.4);
        assert_eq!(reported.cost_source, UsageCostSource::ProviderReported);
        assert_split(reported.category_cost_usd, split(4.0, 0.4, 5.0, 20.0));
        close(reported.speed_premium_usd, 14.7);

        // Without rates there is nothing to split it by.
        let unknown = price_usage(
            &table,
            &record("unknown-model", Some(29.4), UsageSpeed::Fast),
            None,
        );
        close(unknown.cost_usd, 29.4);
        assert_eq!(unknown.category_cost_usd, None);
        assert_eq!(unknown.speed_premium_usd, 0.0);
    }

    #[test]
    fn prices_codex_priority_and_ultrafast_requests_at_their_published_tier_rates() {
        let table = parse_rate_table(&json!({
            "gpt-6-astra": {
                "input_cost_per_token": 1e-5,
                "output_cost_per_token": 5e-5,
                "cache_read_input_token_cost": 1e-6,
                "input_cost_per_token_priority": 2e-5,
                "output_cost_per_token_priority": 1e-4,
                "cache_read_input_token_cost_priority": 2e-6,
                "input_cost_per_token_ultrafast": 6e-5,
                "output_cost_per_token_ultrafast": 3e-4,
                // No ultrafast cache rate: keeps the standard 10:1 input-to-cache ratio.
            },
            "gpt-6-sol": {
                "input_cost_per_token": 2e-6,
                "output_cost_per_token": 1e-5,
                "cache_read_input_token_cost": 2e-7,
            },
        }));
        let cost = |model: &str, speed: UsageSpeed| {
            price_usage(&table, &record(model, None, speed), None).cost_usd
        };
        let standard = cost("gpt-6-astra", UsageSpeed::Standard);

        close(cost("gpt-6-astra", UsageSpeed::Fast), 2.0 * standard);
        close(cost("gpt-6-astra", UsageSpeed::Ultrafast), 6.0 * standard);
        close(
            cache_savings_usd(
                &table,
                &record("gpt-6-astra", None, UsageSpeed::Ultrafast),
                None,
            ),
            6.0 * cache_savings_usd(
                &table,
                &record("gpt-6-astra", None, UsageSpeed::Standard),
                None,
            ),
        );
        // A tier the model does not publish bills at the standard rate.
        assert_eq!(
            cost("gpt-6-sol", UsageSpeed::Ultrafast),
            cost("gpt-6-sol", UsageSpeed::Standard)
        );
    }

    #[test]
    fn keeps_the_canonical_fable_rate_separate_from_deepinfra_in_either_order() {
        let canonical = ("claude-fable-5", rate(1e-5, Some(1e-6)));
        let deep_infra = ("deepinfra/anthropic/claude-fable-5", rate(1e-5, None));

        for entries in [
            [canonical.clone(), deep_infra.clone()],
            [deep_infra.clone(), canonical.clone()],
        ] {
            let document: serde_json::Map<String, Value> = entries
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect();
            let table = parse_rate_table(&Value::Object(document));

            assert_eq!(
                lookup_rate(&table, "claude-fable-5")
                    .unwrap()
                    .base
                    .cache_read_cost_per_token,
                1e-6
            );
            assert_eq!(
                lookup_rate(&table, "deepinfra/anthropic/claude-fable-5")
                    .unwrap()
                    .base
                    .cache_read_cost_per_token,
                1e-5
            );
            assert!(lookup_rate(&table, "other/claude-fable-5").is_none());
        }
    }

    #[test]
    fn prices_a_bracketed_context_tier_variant_at_the_base_models_rate() {
        let table = parse_rate_table(&json!({ "claude-fable-5-1": rate(1e-5, Some(2.5e-7)) }));

        assert_eq!(
            lookup_rate(&table, "claude-fable-5-1[1m]"),
            lookup_rate(&table, "claude-fable-5-1")
        );
        assert!(lookup_rate(&table, "anthropic/Claude-Fable-5-1[1m]").is_none());
    }

    #[test]
    fn adds_a_bare_alias_when_every_qualified_entry_has_the_same_rate() {
        let table = parse_rate_table(&json!({
            "provider-a/example-model": rate(1.0, None),
            "provider-b/example-model": rate(1.0, None),
        }));

        assert_eq!(
            lookup_rate(&table, "example-model"),
            lookup_rate(&table, "provider-a/example-model")
        );
    }

    #[test]
    fn leaves_an_ambiguous_bare_name_unpriced() {
        let table = parse_rate_table(&json!({
            "provider-a/example-model": rate(1.0, None),
            "provider-b/example-model": rate(3.0, None),
        }));

        assert_eq!(
            lookup_rate(&table, "provider-a/example-model")
                .unwrap()
                .base
                .input_cost_per_token,
            1.0
        );
        assert_eq!(
            lookup_rate(&table, "provider-b/example-model")
                .unwrap()
                .base
                .input_cost_per_token,
            3.0
        );
        assert!(lookup_rate(&table, "example-model").is_none());
    }
}
