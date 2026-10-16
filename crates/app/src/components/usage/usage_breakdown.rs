//! Port of upstream/apps/web/src/components/usage/usageBreakdown.ts

use std::cmp::Ordering;

use turin_contracts::usage::UsageTokenTotals;
use turin_shared::usage_merge::{CategoryCost, ModelTotals, SpeedCost, is_model_cost_unknown};

use super::usage_providers::ChartColor;
use super::usage_share_bar::ShareSegment;

/// `ModelTotals` sorted by tokens, ties broken by cost; the input is left alone.
pub fn sort_models_by_tokens(models: &[ModelTotals]) -> Vec<ModelTotals> {
    let mut sorted = models.to_vec();
    // `toSorted` is stable, as is `sort_by`.
    sorted.sort_by(|left, right| {
        right.total_tokens.cmp(&left.total_tokens).then_with(|| {
            right
                .cost_usd
                .partial_cmp(&left.cost_usd)
                .unwrap_or(Ordering::Equal)
        })
    });
    sorted
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareMetric {
    Cost,
    Tokens,
}

/// A model's share of the selected metric, or `None` for a cost share of an
/// unknown cost. An unpriced model still has a real token share.
pub fn model_share(model: &ModelTotals, metric: ShareMetric) -> Option<f64> {
    match metric {
        ShareMetric::Tokens => Some(model.token_share),
        ShareMetric::Cost => (!is_model_cost_unknown(model)).then_some(model.cost_share),
    }
}

/// Share of a model's input read from cache, or `None` without input. Cache
/// writes count as misses: that input was processed in full.
pub fn cache_hit_rate(model: &ModelTotals) -> Option<f64> {
    let tokens = &model.tokens;
    let input =
        tokens.uncached_input_tokens + tokens.cached_input_tokens + tokens.cache_creation_tokens;
    (input != 0).then(|| tokens.cached_input_tokens as f64 / input as f64)
}

/// Effective USD per million priced tokens, or `None` when none were priced.
pub fn cost_per_million_tokens(model: &ModelTotals) -> Option<f64> {
    let priced_tokens = model.total_tokens as f64 - model.unpriced_tokens;
    (priced_tokens > 0. && !is_model_cost_unknown(model))
        .then(|| (model.cost_usd / priced_tokens) * 1_000_000.)
}

/// Token columns of a usage summary the type bar needs; upstream passes
/// `Omit<UsageTokenTotals, "reasoningTokens">`, which `MergedUsage` satisfies
/// structurally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenTypeTotals {
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
}

impl From<&UsageTokenTotals> for TokenTypeTotals {
    fn from(tokens: &UsageTokenTotals) -> Self {
        Self {
            uncached_input_tokens: tokens.uncached_input_tokens,
            cached_input_tokens: tokens.cached_input_tokens,
            cache_creation_tokens: tokens.cache_creation_tokens,
            output_tokens: tokens.output_tokens,
        }
    }
}

impl From<&turin_shared::usage_merge::MergedUsage> for TokenTypeTotals {
    fn from(usage: &turin_shared::usage_merge::MergedUsage) -> Self {
        Self {
            uncached_input_tokens: usage.uncached_input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            output_tokens: usage.output_tokens,
        }
    }
}

/// Adjacent segments stay above the 15 ΔE separation floor in both themes.
mod type_colors {
    use super::ChartColor;
    pub const INPUT: ChartColor = ChartColor::Ink(60);
    pub const CACHE_READ: ChartColor = ChartColor::Ink(30);
    pub const CACHE_WRITE: ChartColor = ChartColor::Ink(72);
    pub const OUTPUT: ChartColor = ChartColor::Ink(100);
    pub const OTHER: ChartColor = ChartColor::Ink(44);
}

pub fn cost_type_segments(cost: &CategoryCost) -> Vec<ShareSegment> {
    vec![
        ShareSegment {
            label: "Input",
            value: cost.input,
            color: type_colors::INPUT,
        },
        ShareSegment {
            label: "Cache read",
            value: cost.cache_read,
            color: type_colors::CACHE_READ,
        },
        ShareSegment {
            label: "Cache write",
            value: cost.cache_write,
            color: type_colors::CACHE_WRITE,
        },
        ShareSegment {
            label: "Output",
            value: cost.output,
            color: type_colors::OUTPUT,
        },
        // Reported cost with no rates to split it, or from older servers. Below a
        // cent it is rounding, not usage.
        ShareSegment {
            label: "Other",
            value: if cost.unsplit >= 0.005 {
                cost.unsplit
            } else {
                0.
            },
            color: type_colors::OTHER,
        },
    ]
}

pub fn token_type_segments(tokens: TokenTypeTotals) -> Vec<ShareSegment> {
    vec![
        ShareSegment {
            label: "Input",
            value: tokens.uncached_input_tokens as f64,
            color: type_colors::INPUT,
        },
        ShareSegment {
            label: "Cache read",
            value: tokens.cached_input_tokens as f64,
            color: type_colors::CACHE_READ,
        },
        ShareSegment {
            label: "Cache write",
            value: tokens.cache_creation_tokens as f64,
            color: type_colors::CACHE_WRITE,
        },
        ShareSegment {
            label: "Output",
            value: tokens.output_tokens as f64,
            color: type_colors::OUTPUT,
        },
    ]
}

/// Speeds are ordered by price, so they brighten from standard to ultrafast.
pub fn speed_cost_segments(cost: &SpeedCost) -> Vec<ShareSegment> {
    vec![
        ShareSegment {
            label: "Standard",
            value: cost.standard,
            color: ChartColor::Ink(34),
        },
        ShareSegment {
            label: "Fast",
            value: cost.fast,
            color: ChartColor::Ink(66),
        },
        ShareSegment {
            label: "Ultrafast",
            value: cost.ultrafast,
            color: ChartColor::Ink(100),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::usage::UsageProviderKind;

    fn model(name: &str, total_tokens: u64, cost_usd: f64) -> ModelTotals {
        ModelTotals {
            model: name.to_owned(),
            provider: UsageProviderKind::Codex,
            cost_usd,
            total_tokens,
            tokens: UsageTokenTotals {
                uncached_input_tokens: total_tokens,
                cached_input_tokens: 0,
                cache_creation_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: 0,
            },
            records: 1,
            unpriced_records: 0,
            unpriced_tokens: 0.,
            cost_share: 0.,
            token_share: 0.,
        }
    }

    #[test]
    fn sorts_by_tokens_breaks_ties_by_cost_and_leaves_the_input_alone() {
        let models = vec![
            model("lower-cost", 100, 1.),
            model("more-tokens", 200, 2.),
            model("higher-cost", 100, 3.),
        ];
        let names: Vec<_> = sort_models_by_tokens(&models)
            .into_iter()
            .map(|item| item.model)
            .collect();
        assert_eq!(names, ["more-tokens", "higher-cost", "lower-cost"]);
        let original: Vec<_> = models.iter().map(|item| item.model.as_str()).collect();
        assert_eq!(original, ["lower-cost", "more-tokens", "higher-cost"]);
    }

    #[test]
    fn model_share_follows_the_selected_metric() {
        let mut priced = model("priced", 100, 9.);
        priced.cost_share = 0.9;
        priced.token_share = 0.25;
        assert_eq!(model_share(&priced, ShareMetric::Cost), Some(0.9));
        assert_eq!(model_share(&priced, ShareMetric::Tokens), Some(0.25));
    }

    #[test]
    fn unknown_cost_has_no_cost_share_but_keeps_its_token_share() {
        let mut unpriced = model("unpriced", 300, 0.);
        unpriced.unpriced_records = 1;
        unpriced.unpriced_tokens = 300.;
        unpriced.token_share = 0.75;
        assert_eq!(model_share(&unpriced, ShareMetric::Cost), None);
        assert_eq!(model_share(&unpriced, ShareMetric::Tokens), Some(0.75));
    }

    #[test]
    fn cache_writes_count_as_misses_and_unpriced_tokens_leave_cost_per_million() {
        let mut mixed = model("mixed", 4_000_000, 6.);
        mixed.tokens = UsageTokenTotals {
            uncached_input_tokens: 1_000_000,
            cached_input_tokens: 1_000_000,
            cache_creation_tokens: 2_000_000,
            output_tokens: 0,
            reasoning_tokens: 0,
        };
        mixed.records = 4;
        mixed.unpriced_records = 1;
        mixed.unpriced_tokens = 1_000_000.;
        assert_eq!(cache_hit_rate(&mixed), Some(0.25));
        assert_eq!(cost_per_million_tokens(&mixed), Some(2.));
        let all_unpriced = ModelTotals {
            unpriced_records: 4,
            ..mixed
        };
        assert_eq!(cost_per_million_tokens(&all_unpriced), None);
    }

    #[test]
    fn small_unsplit_cost_is_rounding_not_usage() {
        let cost = CategoryCost {
            input: 1.,
            cache_read: 0.,
            cache_write: 0.,
            output: 2.,
            unsplit: 0.004,
        };
        assert_eq!(cost_type_segments(&cost)[4].value, 0.);
        let cost = CategoryCost {
            unsplit: 0.005,
            ..cost
        };
        assert_eq!(cost_type_segments(&cost)[4].value, 0.005);
    }
}
