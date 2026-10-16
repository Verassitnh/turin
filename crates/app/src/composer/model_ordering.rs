//! Port of upstream/apps/web/src/modelOrdering.ts (+ test).
//!
//! The two model sorts shared by the model picker, the provider settings
//! model list and the model-selection preference path:
//!
//! - [`sort_models_for_provider_instance`]: one instance's models, optionally
//!   grouping favourites first, then by the user's `modelOrder`, then by the
//!   order the provider reported them.
//! - [`sort_provider_model_items`]: the cross-instance favourites view, ordered
//!   by instance order, then by the original order.
//!
//! Upstream builds an `Order.combineAll` of comparators and runs `Arr.sort`
//! (stable). Here the same comparators are chained with `Ordering::then_with`
//! over a stable `sort_by`. Rank maps are built like upstream's `new Map(...)`,
//! so a duplicated key ranks at its *last* index.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// `ModelSlugItem`.
pub trait ModelSlugItem {
    fn slug(&self) -> &str;
}

/// `ProviderModelItem`: a [`ModelSlugItem`] that also names its instance.
pub trait ProviderModelItem: ModelSlugItem {
    fn instance_id(&self) -> &str;
}

/// `providerModelKey`: `${instanceId}:${slug}`, the favourites key.
pub fn provider_model_key(instance_id: &str, slug: &str) -> String {
    format!("{instance_id}:{slug}")
}

/// `rankByValue`: value to index; a later duplicate overwrites an earlier one.
fn rank_by_value<'a>(values: impl IntoIterator<Item = &'a str>) -> HashMap<&'a str, usize> {
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| (value, index))
        .collect()
}

/// `byOptionalRank`: ascending, with a missing rank sorting last (`Infinity`).
fn by_optional_rank(left: Option<usize>, right: Option<usize>) -> Ordering {
    left.unwrap_or(usize::MAX).cmp(&right.unwrap_or(usize::MAX))
}

/// `byTrueFirst`: `Order.flip(Order.Boolean)` over a predicate.
fn by_true_first(left: bool, right: bool) -> Ordering {
    right.cmp(&left)
}

/// The `options` of `sortModelsForProviderInstance`. Every field is optional
/// upstream; the default is an empty order, no favourites, no grouping.
#[derive(Debug, Clone, Copy, Default)]
pub struct SortModelsOptions<'a> {
    pub model_order: &'a [String],
    pub favorite_models: Option<&'a HashSet<String>>,
    pub group_favorites: bool,
}

/// `sortModelsForProviderInstance`.
pub fn sort_models_for_provider_instance<T: ModelSlugItem>(
    models: Vec<T>,
    options: SortModelsOptions<'_>,
) -> Vec<T> {
    let order_by_slug = rank_by_value(options.model_order.iter().map(String::as_str));
    let original_order = rank_by_value(models.iter().map(|model| model.slug()));
    let is_favorite = |model: &T| {
        options
            .favorite_models
            .is_some_and(|favorites| favorites.contains(model.slug()))
    };
    // Ranks are read from the maps up front: `sort_by` needs the items to stay
    // movable, so each carries its comparison keys with it.
    let mut keyed: Vec<(bool, Option<usize>, Option<usize>, T)> = models
        .into_iter()
        .map(|model| {
            let favorite = options.group_favorites && is_favorite(&model);
            let ordered = order_by_slug.get(model.slug()).copied();
            let original = original_order.get(model.slug()).copied();
            (favorite, ordered, original, model)
        })
        .collect();
    let group_favorites = options.group_favorites;
    keyed.sort_by(|left, right| {
        let favorites = if group_favorites {
            by_true_first(left.0, right.0)
        } else {
            Ordering::Equal
        };
        favorites
            .then_with(|| by_optional_rank(left.1, right.1))
            .then_with(|| by_optional_rank(left.2, right.2))
    });
    keyed.into_iter().map(|(_, _, _, model)| model).collect()
}

/// The `options` of `sortProviderModelItems`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SortProviderModelItemsOptions<'a> {
    pub favorite_model_keys: Option<&'a HashSet<String>>,
    pub group_favorites: bool,
    pub instance_order: &'a [String],
}

/// `sortProviderModelItems`.
pub fn sort_provider_model_items<T: ProviderModelItem>(
    items: Vec<T>,
    options: SortProviderModelItemsOptions<'_>,
) -> Vec<T> {
    let instance_order = rank_by_value(options.instance_order.iter().map(String::as_str));
    let keys: Vec<String> = items
        .iter()
        .map(|item| provider_model_key(item.instance_id(), item.slug()))
        .collect();
    let original_order = rank_by_value(keys.iter().map(String::as_str));
    let mut keyed: Vec<(bool, Option<usize>, Option<usize>, T)> = items
        .into_iter()
        .zip(keys.iter())
        .map(|(item, key)| {
            let favorite = options.group_favorites
                && options
                    .favorite_model_keys
                    .is_some_and(|favorites| favorites.contains(key));
            let instance = instance_order.get(item.instance_id()).copied();
            let original = original_order.get(key.as_str()).copied();
            (favorite, instance, original, item)
        })
        .collect();
    let group_favorites = options.group_favorites;
    keyed.sort_by(|left, right| {
        let favorites = if group_favorites {
            by_true_first(left.0, right.0)
        } else {
            Ordering::Equal
        };
        favorites
            .then_with(|| by_optional_rank(left.1, right.1))
            .then_with(|| by_optional_rank(left.2, right.2))
    });
    keyed.into_iter().map(|(_, _, _, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Slug(&'static str);

    impl ModelSlugItem for Slug {
        fn slug(&self) -> &str {
            self.0
        }
    }

    struct Item {
        instance_id: &'static str,
        slug: &'static str,
    }

    impl ModelSlugItem for Item {
        fn slug(&self) -> &str {
            self.slug
        }
    }

    impl ProviderModelItem for Item {
        fn instance_id(&self) -> &str {
            self.instance_id
        }
    }

    const CODEX_WORK_ID: &str = "codex_work";
    const CLAUDE_ID: &str = "claudeAgent";

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn set(values: &[&str]) -> HashSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn groups_favorites_first_while_preserving_provider_model_order_inside_each_group() {
        let models = vec![
            Slug("gpt-5.5"),
            Slug("gpt-5.4-mini"),
            Slug("crest-alpha"),
            Slug("gpt-5.3-codex"),
        ];
        let favorites = set(&["gpt-5.5", "gpt-5.4-mini", "crest-alpha"]);
        let model_order = strings(&["gpt-5.4-mini", "gpt-5.5", "crest-alpha", "gpt-5.3-codex"]);

        let sorted = sort_models_for_provider_instance(
            models,
            SortModelsOptions {
                favorite_models: Some(&favorites),
                group_favorites: true,
                model_order: &model_order,
            },
        );

        assert_eq!(
            sorted.iter().map(|model| model.0).collect::<Vec<_>>(),
            ["gpt-5.4-mini", "gpt-5.5", "crest-alpha", "gpt-5.3-codex"]
        );
    }

    #[test]
    fn sorts_the_favorites_view_by_provider_order_then_provider_model_order() {
        let items = vec![
            Item {
                instance_id: CODEX_WORK_ID,
                slug: "gpt-5.4-mini",
            },
            Item {
                instance_id: CODEX_WORK_ID,
                slug: "gpt-5.5",
            },
            Item {
                instance_id: CODEX_WORK_ID,
                slug: "crest-alpha",
            },
            Item {
                instance_id: CLAUDE_ID,
                slug: "claude-opus-4-6",
            },
        ];
        let favorite_keys: HashSet<String> = [
            provider_model_key(CODEX_WORK_ID, "gpt-5.5"),
            provider_model_key(CLAUDE_ID, "claude-opus-4-6"),
            provider_model_key(CODEX_WORK_ID, "gpt-5.4-mini"),
            provider_model_key(CODEX_WORK_ID, "crest-alpha"),
        ]
        .into_iter()
        .collect();
        let instance_order = strings(&[CODEX_WORK_ID, CLAUDE_ID]);

        let sorted = sort_provider_model_items(
            items,
            SortProviderModelItemsOptions {
                favorite_model_keys: Some(&favorite_keys),
                group_favorites: false,
                instance_order: &instance_order,
            },
        );

        assert_eq!(
            sorted.iter().map(|item| item.slug).collect::<Vec<_>>(),
            ["gpt-5.4-mini", "gpt-5.5", "crest-alpha", "claude-opus-4-6"]
        );
    }

    // Beyond the upstream cases: the `groupFavorites: false` modelOrder-only
    // path modelSelection.ts uses, and the rank rules.

    #[test]
    fn model_order_alone_ranks_listed_slugs_first_and_keeps_the_rest_in_original_order() {
        let models = vec![Slug("a"), Slug("b"), Slug("c"), Slug("d")];
        let model_order = strings(&["c", "a"]);
        let sorted = sort_models_for_provider_instance(
            models,
            SortModelsOptions {
                model_order: &model_order,
                ..Default::default()
            },
        );
        assert_eq!(
            sorted.iter().map(|model| model.0).collect::<Vec<_>>(),
            ["c", "a", "b", "d"]
        );
    }

    #[test]
    fn favorites_are_ignored_without_group_favorites() {
        let models = vec![Slug("a"), Slug("b"), Slug("c")];
        let favorites = set(&["c"]);
        let sorted = sort_models_for_provider_instance(
            models,
            SortModelsOptions {
                favorite_models: Some(&favorites),
                ..Default::default()
            },
        );
        assert_eq!(
            sorted.iter().map(|model| model.0).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn provider_items_group_favorites_before_instance_order_when_asked() {
        let items = vec![
            Item {
                instance_id: CODEX_WORK_ID,
                slug: "a",
            },
            Item {
                instance_id: CLAUDE_ID,
                slug: "b",
            },
        ];
        let favorites = set(&[&provider_model_key(CLAUDE_ID, "b")]);
        let instance_order = strings(&[CODEX_WORK_ID, CLAUDE_ID]);
        let sorted = sort_provider_model_items(
            items,
            SortProviderModelItemsOptions {
                favorite_model_keys: Some(&favorites),
                group_favorites: true,
                instance_order: &instance_order,
            },
        );
        assert_eq!(
            sorted.iter().map(|item| item.slug).collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[test]
    fn instances_missing_from_the_order_sort_last_in_original_order() {
        let items = vec![
            Item {
                instance_id: "z",
                slug: "1",
            },
            Item {
                instance_id: CLAUDE_ID,
                slug: "2",
            },
            Item {
                instance_id: "y",
                slug: "3",
            },
        ];
        let instance_order = strings(&[CLAUDE_ID]);
        let sorted = sort_provider_model_items(
            items,
            SortProviderModelItemsOptions {
                instance_order: &instance_order,
                ..Default::default()
            },
        );
        assert_eq!(
            sorted.iter().map(|item| item.slug).collect::<Vec<_>>(),
            ["2", "1", "3"]
        );
    }
}
