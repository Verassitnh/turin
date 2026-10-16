//! Port of upstream/apps/web/src/components/usage/usagePriceTable.ts

use std::collections::HashMap;

use indexmap::IndexMap;
use turin_contracts::settings::UsageModelPriceOverride;

use super::usage_price_form::{
    USAGE_PRICE_FIELDS, UsagePriceField, parse_usage_price_form, usage_price_form,
};
use super::usage_price_targets::{UsagePriceChange, UsagePriceTarget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsagePriceDraft {
    pub id: String,
    pub model: String,
    pub is_new: bool,
    /// Edited cells only (`Partial<Pick<UsagePriceForm, …>>`).
    pub values: HashMap<UsagePriceField, String>,
    /// Edited "Map to" model. Absent keeps each environment's mapping; blank removes it.
    pub alias: Option<String>,
    pub removed: bool,
}

impl UsagePriceDraft {
    pub fn new(id: impl Into<String>, model: impl Into<String>, is_new: bool) -> Self {
        Self {
            id: id.into(),
            model: model.into(),
            is_new,
            values: HashMap::new(),
            alias: None,
            removed: false,
        }
    }
}

fn js_trim(value: &str) -> &str {
    turin_contracts::base_schemas::js_trim(value)
}

pub fn is_empty_usage_price_draft(draft: &UsagePriceDraft) -> bool {
    draft.is_new
        && js_trim(&draft.model).is_empty()
        && js_trim(draft.alias.as_deref().unwrap_or("")).is_empty()
        && draft.values.values().all(|value| js_trim(value).is_empty())
}

fn model_price<'a>(
    target: &'a UsagePriceTarget,
    model: &str,
) -> Option<&'a UsageModelPriceOverride> {
    target.prices.as_ref().and_then(|prices| prices.get(model))
}

fn model_alias<'a>(target: &'a UsagePriceTarget, model: &str) -> Option<&'a str> {
    target
        .aliases
        .as_ref()
        .and_then(|aliases| aliases.get(model))
        .map(String::as_str)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsagePriceCell {
    pub value: String,
    pub placeholder: &'static str,
}

/// The "Map to" cell across the selected environments, read like a price cell.
pub fn usage_alias_cell(targets: &[UsagePriceTarget], model: &str) -> UsagePriceCell {
    if targets.iter().any(|target| target.aliases.is_none()) {
        return UsagePriceCell {
            value: String::new(),
            placeholder: "Unavailable",
        };
    }
    let values: Vec<&str> = targets
        .iter()
        .map(|target| model_alias(target, model).unwrap_or(""))
        .collect();
    if values.iter().any(|value| *value != values[0]) {
        return UsagePriceCell {
            value: String::new(),
            placeholder: "Mixed",
        };
    }
    UsagePriceCell {
        value: values.first().copied().unwrap_or("").to_owned(),
        placeholder: "None",
    }
}

pub fn usage_price_cell(
    targets: &[UsagePriceTarget],
    model: &str,
    field: UsagePriceField,
) -> UsagePriceCell {
    let optional = field.spec().optional;
    let values: Vec<Option<String>> = targets
        .iter()
        .map(|target| {
            model_price(target, model)
                .map(|price| usage_price_form(model, Some(price)).get(field).to_owned())
        })
        .collect();
    if targets.iter().any(|target| target.prices.is_none()) {
        return UsagePriceCell {
            value: String::new(),
            placeholder: "Unavailable",
        };
    }
    if values.iter().any(|value| *value != values[0]) {
        return UsagePriceCell {
            value: String::new(),
            placeholder: "Mixed",
        };
    }
    let first = values.into_iter().next().flatten();
    UsagePriceCell {
        placeholder: if first.is_none() {
            "Automatic"
        } else if optional {
            "Input rate"
        } else {
            "0.00"
        },
        value: first.unwrap_or_default(),
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct UsagePriceTableChanges {
    pub changes: Vec<UsagePriceChange>,
    /// Draft id → message, in the order the drafts were visited.
    pub errors: IndexMap<String, String>,
}

/// Only edited cells replace rates; untouched cells retain each environment's own values.
pub fn usage_price_table_changes(
    target: &UsagePriceTarget,
    drafts: &[UsagePriceDraft],
) -> UsagePriceTableChanges {
    let mut changes = Vec::new();
    let mut errors = IndexMap::new();
    for draft in drafts {
        if is_empty_usage_price_draft(draft) {
            continue;
        }
        let model = js_trim(&draft.model).to_owned();
        let current_alias = model_alias(target, &model);
        if draft.removed {
            if model_price(target, &model).is_some() {
                changes.push(UsagePriceChange::Price {
                    model: model.clone(),
                    price: None,
                });
            }
            if current_alias.is_some() {
                changes.push(UsagePriceChange::Alias {
                    model: model.clone(),
                    alias: None,
                });
            }
            continue;
        }
        // A mapped model is priced as its target, so mapping it drops its own price.
        let alias = js_trim(draft.alias.as_deref().or(current_alias).unwrap_or("")).to_owned();
        if !alias.is_empty() {
            if model.is_empty() {
                errors.insert(draft.id.clone(), "Enter a model ID.".to_owned());
            } else if alias == model {
                errors.insert(draft.id.clone(), "Map to a different model.".to_owned());
            } else {
                if Some(alias.as_str()) != current_alias {
                    changes.push(UsagePriceChange::Alias {
                        model: model.clone(),
                        alias: Some(alias),
                    });
                }
                if model_price(target, &model).is_some() {
                    changes.push(UsagePriceChange::Price {
                        model: model.clone(),
                        price: None,
                    });
                }
            }
            continue;
        }
        if current_alias.is_some() {
            changes.push(UsagePriceChange::Alias {
                model: model.clone(),
                alias: None,
            });
            // Clearing a mapping without entering prices returns to automatic pricing.
            if model_price(target, &model).is_none()
                && draft.values.values().all(|value| js_trim(value).is_empty())
            {
                continue;
            }
        }
        let original = usage_price_form(&model, model_price(target, &model));
        let mut form = original.clone();
        for (field, value) in &draft.values {
            form.set(*field, value.clone());
        }
        let Some((parsed_model, parsed_price)) = parse_usage_price_form(&form) else {
            let missing = USAGE_PRICE_FIELDS
                .iter()
                .find(|field| !field.optional && js_trim(form.get(field.field)).is_empty());
            errors.insert(
                draft.id.clone(),
                if model.is_empty() {
                    "Enter a model ID.".to_owned()
                } else if let Some(missing) = missing {
                    format!("{} is required on {}.", missing.label, target.label)
                } else {
                    "Use non-negative numbers for prices.".to_owned()
                },
            );
            continue;
        };
        let next = usage_price_form(&model, Some(&parsed_price));
        if USAGE_PRICE_FIELDS
            .iter()
            .any(|field| original.get(field.field) != next.get(field.field))
        {
            changes.push(UsagePriceChange::Price {
                model: parsed_model,
                price: Some(parsed_price),
            });
        }
    }
    UsagePriceTableChanges { changes, errors }
}

/// Unavailable destinations report a save failure without blocking writable environments.
pub fn usage_price_table_errors(
    targets: &[UsagePriceTarget],
    drafts: &[UsagePriceDraft],
) -> IndexMap<String, String> {
    targets
        .iter()
        .filter(|target| target.unavailable.is_none() && target.prices.is_some())
        .flat_map(|target| usage_price_table_changes(target, drafts).errors)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::usage::usage_price_form::basic_price;
    use turin_contracts::EnvironmentId;

    fn price() -> UsageModelPriceOverride {
        basic_price(2., 8.)
    }

    fn target(
        name: &str,
        prices: Option<Vec<(&str, UsageModelPriceOverride)>>,
        aliases: Option<Vec<(&str, &str)>>,
    ) -> UsagePriceTarget {
        UsagePriceTarget {
            environment_id: EnvironmentId::new(name).unwrap(),
            label: name.to_owned(),
            prices: prices.map(|prices| {
                prices
                    .into_iter()
                    .map(|(model, price)| (model.to_owned(), price))
                    .collect()
            }),
            aliases: aliases.map(|aliases| {
                aliases
                    .into_iter()
                    .map(|(model, alias)| (model.to_owned(), alias.to_owned()))
                    .collect()
            }),
            unavailable: None,
        }
    }

    fn with_prices(name: &str, prices: Vec<(&str, UsageModelPriceOverride)>) -> UsagePriceTarget {
        target(name, Some(prices), Some(vec![]))
    }

    fn draft(values: &[(UsagePriceField, &str)]) -> UsagePriceDraft {
        UsagePriceDraft {
            values: values.iter().map(|(f, v)| (*f, (*v).to_owned())).collect(),
            ..UsagePriceDraft::new("model:example", "example", false)
        }
    }

    fn price_change(model: &str, price: Option<UsageModelPriceOverride>) -> UsagePriceChange {
        UsagePriceChange::Price {
            model: model.to_owned(),
            price,
        }
    }

    fn alias_change(model: &str, alias: Option<&str>) -> UsagePriceChange {
        UsagePriceChange::Alias {
            model: model.to_owned(),
            alias: alias.map(str::to_owned),
        }
    }

    use UsagePriceField::{CacheRead, CacheWrite, Input, Output};

    #[test]
    fn shows_shared_and_mixed_cells_separately_and_never_treats_missing_settings_as_automatic() {
        let mut other = price();
        other.output_cost_per_million_tokens =
            turin_contracts::settings::UsageModelTokenPrice::new(10.).unwrap();
        let targets = vec![
            with_prices("a", vec![("example", price())]),
            with_prices("b", vec![("example", other)]),
        ];
        assert_eq!(usage_price_cell(&targets, "example", Input).value, "2");
        assert_eq!(
            usage_price_cell(&targets, "example", Output),
            UsagePriceCell {
                value: String::new(),
                placeholder: "Mixed"
            }
        );
        assert_eq!(
            usage_price_cell(&targets, "example", CacheRead),
            UsagePriceCell {
                value: String::new(),
                placeholder: "Input rate"
            }
        );
        assert_eq!(
            usage_price_cell(&[with_prices("a", vec![])], "example", Input).placeholder,
            "Automatic"
        );
        let unavailable = [targets[0].clone(), target("b", None, Some(vec![]))];
        assert_eq!(
            usage_price_cell(&unavailable, "example", Input).placeholder,
            "Unavailable"
        );
    }

    #[test]
    fn handles_an_exact_model_id_named_like_an_object_member() {
        for model in ["constructor", "toString", "__proto__"] {
            let environment = with_prices("a", vec![]);
            assert_eq!(
                usage_price_cell(std::slice::from_ref(&environment), model, Input),
                UsagePriceCell {
                    value: String::new(),
                    placeholder: "Automatic"
                }
            );
            let mut new_draft = draft(&[(Input, "2"), (Output, "8")]);
            new_draft.model = model.to_owned();
            new_draft.is_new = true;
            let result = usage_price_table_changes(&environment, &[new_draft]);
            assert_eq!(result.changes, vec![price_change(model, Some(price()))]);
            assert_eq!(
                usage_price_cell(&[with_prices("a", vec![(model, price())])], model, Input).value,
                "2"
            );
        }
    }

    #[test]
    fn changes_only_edited_columns_while_preserving_other_prices_and_models() {
        use turin_contracts::settings::UsageModelTokenPrice as Rate;
        let edits = [draft(&[(Input, "3")])];
        let a_price = UsageModelPriceOverride {
            cache_read_cost_per_million_tokens: Some(Rate::new(0.).unwrap()),
            ..price()
        };
        let a = usage_price_table_changes(
            &with_prices("a", vec![("example", a_price), ("untouched", price())]),
            &edits,
        );
        let b_price = UsageModelPriceOverride {
            output_cost_per_million_tokens: Rate::new(10.).unwrap(),
            ..price()
        };
        let b = usage_price_table_changes(&with_prices("b", vec![("example", b_price)]), &edits);
        assert_eq!(
            a.changes,
            vec![price_change(
                "example",
                Some(UsageModelPriceOverride {
                    input_cost_per_million_tokens: Rate::new(3.).unwrap(),
                    cache_read_cost_per_million_tokens: Some(Rate::new(0.).unwrap()),
                    ..price()
                })
            )]
        );
        assert_eq!(
            b.changes,
            vec![price_change("example", Some(basic_price(3., 10.)))]
        );
    }

    #[test]
    fn batches_new_rows_and_resets_and_distinguishes_blank_cache_rates_from_explicit_zero() {
        use turin_contracts::settings::UsageModelTokenPrice as Rate;
        let mut removed = draft(&[]);
        removed.removed = true;
        let mut fresh = UsagePriceDraft::new("new:1", "  vendor/Model  ", true);
        fresh.values = [
            (Input, "2"),
            (Output, "8"),
            (CacheRead, "0"),
            (CacheWrite, ""),
        ]
        .into_iter()
        .map(|(f, v)| (f, v.to_owned()))
        .collect();
        let plan = usage_price_table_changes(
            &with_prices("a", vec![("example", price())]),
            &[removed, fresh],
        );
        assert!(plan.errors.is_empty());
        assert_eq!(
            plan.changes,
            vec![
                price_change("example", None),
                price_change(
                    "vendor/Model",
                    Some(UsageModelPriceOverride {
                        cache_read_cost_per_million_tokens: Some(Rate::new(0.).unwrap()),
                        ..price()
                    })
                ),
            ]
        );
    }

    #[test]
    fn requires_complete_prices_before_turning_automatic_pricing_into_an_override() {
        let edits = [draft(&[(Input, "3")])];
        assert!(
            usage_price_table_changes(&with_prices("a", vec![("example", price())]), &edits)
                .errors
                .is_empty()
        );
        let missing = usage_price_table_changes(&with_prices("b", vec![]), &edits);
        assert!(missing.changes.is_empty());
        assert!(missing.errors.contains_key("model:example"));
        assert_eq!(missing.errors["model:example"], "Output is required on b.");
    }

    #[test]
    fn ignores_empty_new_rows_without_blocking_other_edits() {
        let environment = with_prices("a", vec![("example", price())]);
        let mut empty = UsagePriceDraft::new("new:1", " ", true);
        empty.values.insert(Input, " ".to_owned());
        let plan =
            usage_price_table_changes(&environment, &[draft(&[(Input, "3")]), empty.clone()]);
        assert!(plan.errors.is_empty());
        assert_eq!(
            plan.changes,
            vec![price_change("example", Some(basic_price(3., 8.)))]
        );
        assert!(
            usage_price_table_changes(&environment, std::slice::from_ref(&empty))
                .changes
                .is_empty()
        );
        let mut zero = empty.clone();
        zero.values.insert(Input, "0".to_owned());
        assert_eq!(
            usage_price_table_changes(&environment, &[zero]).errors["new:1"],
            "Enter a model ID."
        );
    }

    #[test]
    fn unavailable_destinations_do_not_block_a_valid_edit_elsewhere() {
        for unavailable in [
            "Offline",
            "Read-only access",
            "Update server to edit prices",
        ] {
            let edits = [draft(&[(Input, "3")])];
            let writable = with_prices("writable", vec![("example", price())]);
            let other = with_prices("other", vec![]);
            let blocked = UsagePriceTarget {
                unavailable: Some(unavailable.to_owned()),
                ..other.clone()
            };
            assert!(usage_price_table_errors(&[writable.clone(), blocked], &edits).is_empty());
            assert!(
                usage_price_table_errors(&[writable, other], &edits).contains_key("model:example")
            );
        }
    }

    #[test]
    fn does_not_write_unchanged_rates_and_rejects_invalid_edited_cells() {
        let environment = with_prices("a", vec![("example", price())]);
        assert!(
            usage_price_table_changes(&environment, &[draft(&[(Input, "2.00")])])
                .changes
                .is_empty()
        );
        assert_eq!(
            usage_price_table_changes(&environment, &[draft(&[(CacheRead, "-1")])])
                .errors
                .len(),
            1
        );
        assert_eq!(
            usage_price_table_changes(&environment, &[draft(&[(Output, "")])])
                .errors
                .len(),
            1
        );
    }

    #[test]
    fn mapping_a_model_drops_its_own_price_and_resetting_removes_the_mapping() {
        let priced = with_prices("a", vec![("example", price())]);
        let mut mapped_draft = draft(&[]);
        mapped_draft.alias = Some(" example-model ".to_owned());
        let plan = usage_price_table_changes(&priced, &[mapped_draft]);
        assert_eq!(
            plan.changes,
            vec![
                alias_change("example", Some("example-model")),
                price_change("example", None)
            ]
        );
        assert!(plan.errors.is_empty());

        let mapped = target("b", Some(vec![]), Some(vec![("example", "example-model")]));
        let mut reset = draft(&[]);
        reset.removed = true;
        assert_eq!(
            usage_price_table_changes(&mapped, &[reset]).changes,
            vec![alias_change("example", None)]
        );
        let mut same = draft(&[]);
        same.alias = Some("example".to_owned());
        assert_eq!(
            usage_price_table_changes(&mapped, &[same]).errors["model:example"],
            "Map to a different model."
        );
    }

    #[test]
    fn clearing_a_mapping_returns_to_automatic_pricing_unless_prices_are_entered() {
        let mapped = target("a", Some(vec![]), Some(vec![("example", "example-model")]));
        let mut cleared = draft(&[]);
        cleared.alias = Some(String::new());
        assert_eq!(
            usage_price_table_changes(&mapped, &[cleared]).changes,
            vec![alias_change("example", None)]
        );
        let mut priced = draft(&[(Input, "2"), (Output, "8")]);
        priced.alias = Some(String::new());
        assert_eq!(
            usage_price_table_changes(&mapped, &[priced]).changes,
            vec![
                alias_change("example", None),
                price_change("example", Some(price()))
            ]
        );
    }

    #[test]
    fn keeps_each_environments_mapping_when_the_column_is_untouched() {
        let edits = [draft(&[(Input, "3")])];
        assert!(
            usage_price_table_changes(
                &target("a", Some(vec![]), Some(vec![("example", "example-model")])),
                &edits
            )
            .changes
            .is_empty()
        );
        assert_eq!(
            usage_alias_cell(
                &[
                    target("a", Some(vec![]), Some(vec![("example", "example-model")])),
                    with_prices("b", vec![("example", price())]),
                ],
                "example"
            ),
            UsagePriceCell {
                value: String::new(),
                placeholder: "Mixed"
            }
        );
    }
}
