//! Port of upstream/apps/web/src/components/usage/usagePriceTargets.ts

use std::collections::HashMap;
use std::future::Future;

use futures::future::join_all;
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::settings::UsageModelPriceOverride;

#[derive(Clone, Debug, PartialEq)]
pub struct UsagePriceTarget {
    pub environment_id: EnvironmentId,
    pub label: String,
    /// `None`: not loaded, or the server cannot store prices.
    pub prices: Option<IndexMap<String, UsageModelPriceOverride>>,
    /// Model mappings. `None` when they are not loaded or the server cannot store them.
    pub aliases: Option<IndexMap<String, String>>,
    pub unavailable: Option<String>,
}

/// `None` removes the model's custom price or mapping.
#[derive(Clone, Debug, PartialEq)]
pub enum UsagePriceChange {
    Price {
        model: String,
        price: Option<UsageModelPriceOverride>,
    },
    Alias {
        model: String,
        alias: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsagePriceWriteResult {
    Saved,
    Failed { error: String },
}

/// The `usagePriceOverrides` / `usageModelAliases` part of a `ServerSettingsPatch`
/// (`null` entries delete). The caller maps it onto the full settings patch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsagePricePatch {
    pub usage_price_overrides: Option<IndexMap<String, Option<UsageModelPriceOverride>>>,
    pub usage_model_aliases: Option<IndexMap<String, Option<String>>>,
}

/// Outcome of one settings write (`{ _tag: "Success" | "Failure" }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageWriteOutcome {
    Success,
    Failure,
}

/// A thrown write (`Promise` rejection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageWriteError;

/// Each destination settles independently; retry callers pass only the failed destinations.
/// Prices and mappings for one destination go out in a single patch.
pub async fn write_usage_prices<W, Fut>(
    targets: &[UsagePriceTarget],
    changes: &HashMap<EnvironmentId, Vec<UsagePriceChange>>,
    write: W,
    on_result: impl Fn(&EnvironmentId, UsagePriceWriteResult),
) where
    W: Fn(EnvironmentId, UsagePricePatch) -> Fut,
    Fut: Future<Output = Result<UsageWriteOutcome, UsageWriteError>>,
{
    let write = &write;
    let on_result = &on_result;
    join_all(targets.iter().map(|target| async move {
        let result = write_target(target, changes, write).await;
        on_result(&target.environment_id, result);
    }))
    .await;
}

async fn write_target<W, Fut>(
    target: &UsagePriceTarget,
    changes: &HashMap<EnvironmentId, Vec<UsagePriceChange>>,
    write: &W,
) -> UsagePriceWriteResult
where
    W: Fn(EnvironmentId, UsagePricePatch) -> Fut,
    Fut: Future<Output = Result<UsageWriteOutcome, UsageWriteError>>,
{
    let changes = changes
        .get(&target.environment_id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let prices: IndexMap<String, Option<UsageModelPriceOverride>> = changes
        .iter()
        .filter_map(|change| match change {
            UsagePriceChange::Price { model, price } => Some((model.clone(), price.clone())),
            UsagePriceChange::Alias { .. } => None,
        })
        .collect();
    let aliases: IndexMap<String, Option<String>> = changes
        .iter()
        .filter_map(|change| match change {
            UsagePriceChange::Alias { model, alias } => Some((model.clone(), alias.clone())),
            UsagePriceChange::Price { .. } => None,
        })
        .collect();
    if let Some(unavailable) = &target.unavailable {
        return UsagePriceWriteResult::Failed {
            error: unavailable.clone(),
        };
    }
    if !aliases.is_empty() && target.aliases.is_none() {
        return UsagePriceWriteResult::Failed {
            error: "Update server to map models".to_owned(),
        };
    }
    let failed = || UsagePriceWriteResult::Failed {
        error: "Could not save. Try again.".to_owned(),
    };
    if changes.is_empty() {
        return UsagePriceWriteResult::Saved;
    }
    let patch = UsagePricePatch {
        usage_price_overrides: (!prices.is_empty()).then_some(prices),
        usage_model_aliases: (!aliases.is_empty()).then_some(aliases),
    };
    match write(target.environment_id.clone(), patch).await {
        Ok(UsageWriteOutcome::Success) => UsagePriceWriteResult::Saved,
        Ok(UsageWriteOutcome::Failure) | Err(_) => failed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::usage::usage_price_form::basic_price;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    fn id(value: &str) -> EnvironmentId {
        EnvironmentId::new(value).expect("test id")
    }

    fn price() -> UsageModelPriceOverride {
        basic_price(2., 8.)
    }

    fn target(name: &str) -> UsagePriceTarget {
        UsagePriceTarget {
            environment_id: id(name),
            label: name.to_owned(),
            prices: Some(IndexMap::from([("example".to_owned(), price())])),
            aliases: Some(IndexMap::new()),
            unavailable: None,
        }
    }

    fn price_change(model: &str, price: Option<UsageModelPriceOverride>) -> UsagePriceChange {
        UsagePriceChange::Price {
            model: model.to_owned(),
            price,
        }
    }

    type Calls = Arc<Mutex<Vec<(EnvironmentId, UsagePricePatch)>>>;

    fn recorder(
        outcome: Result<UsageWriteOutcome, UsageWriteError>,
    ) -> (
        Calls,
        impl Fn(
            EnvironmentId,
            UsagePricePatch,
        ) -> std::future::Ready<Result<UsageWriteOutcome, UsageWriteError>>,
    ) {
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        (calls, move |environment, patch| {
            sink.lock().push((environment, patch));
            std::future::ready(outcome.clone())
        })
    }

    #[tokio::test]
    async fn sends_all_table_changes_in_one_patch_per_environment_and_skips_unchanged() {
        let (calls, write) = recorder(Ok(UsageWriteOutcome::Success));
        let results: Mutex<Vec<(EnvironmentId, UsagePriceWriteResult)>> = Mutex::default();
        write_usage_prices(
            &[target("edited"), target("unchanged")],
            &HashMap::from([(
                id("edited"),
                vec![
                    price_change("new-model", Some(price())),
                    price_change("example", None),
                ],
            )]),
            write,
            |environment, result| results.lock().push((environment.clone(), result)),
        )
        .await;
        let calls = calls.lock();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, id("edited"));
        assert_eq!(
            calls[0].1,
            UsagePricePatch {
                usage_price_overrides: Some(IndexMap::from([
                    ("new-model".to_owned(), Some(price())),
                    ("example".to_owned(), None),
                ])),
                usage_model_aliases: None,
            }
        );
        assert!(
            results
                .lock()
                .contains(&(id("unchanged"), UsagePriceWriteResult::Saved))
        );
    }

    #[tokio::test]
    async fn saves_independently_and_retries_failures_without_rewriting_successes() {
        let (release_slow, slow) = oneshot::channel::<UsageWriteOutcome>();
        let slow = Arc::new(tokio::sync::Mutex::new(Some(slow)));
        let calls: Calls = Arc::default();
        let results: Arc<Mutex<HashMap<EnvironmentId, UsagePriceWriteResult>>> = Arc::default();
        let mut offline = target("offline");
        offline.unavailable = Some("Offline".to_owned());
        let targets = vec![target("fast"), target("slow"), offline];
        let change = price_change("example", Some(price()));
        let changes: HashMap<_, _> = targets
            .iter()
            .map(|t| (t.environment_id.clone(), vec![change.clone()]))
            .collect();
        let (fast_saved_tx, fast_saved_rx) = oneshot::channel::<()>();
        let fast_saved_tx = Arc::new(Mutex::new(Some(fast_saved_tx)));

        let running = {
            let calls = calls.clone();
            let results = results.clone();
            let slow = slow.clone();
            let targets = targets.clone();
            let changes = changes.clone();
            tokio::spawn(async move {
                write_usage_prices(
                    &targets,
                    &changes,
                    move |environment, patch| {
                        calls.lock().push((environment.clone(), patch));
                        let slow = slow.clone();
                        async move {
                            if environment == id("slow") {
                                let receiver = slow.lock().await.take().unwrap();
                                Ok(receiver.await.unwrap())
                            } else {
                                Ok(UsageWriteOutcome::Success)
                            }
                        }
                    },
                    move |environment, result| {
                        results.lock().insert(environment.clone(), result);
                        if *environment == id("fast")
                            && let Some(tx) = fast_saved_tx.lock().take()
                        {
                            tx.send(()).ok();
                        }
                    },
                )
                .await;
            })
        };
        fast_saved_rx.await.unwrap();
        assert_eq!(
            results.lock().get(&id("fast")),
            Some(&UsagePriceWriteResult::Saved)
        );
        assert_eq!(
            results.lock().get(&id("offline")),
            Some(&UsagePriceWriteResult::Failed {
                error: "Offline".to_owned()
            })
        );
        assert!(!results.lock().contains_key(&id("slow")));
        release_slow.send(UsageWriteOutcome::Failure).unwrap();
        running.await.unwrap();
        assert_eq!(calls.lock().len(), 2);

        let (retry_calls, retry) = recorder(Ok(UsageWriteOutcome::Success));
        let failed: Vec<_> = targets
            .iter()
            .filter(|t| {
                matches!(
                    results.lock().get(&t.environment_id),
                    Some(UsagePriceWriteResult::Failed { .. })
                )
            })
            .cloned()
            .map(|t| UsagePriceTarget {
                unavailable: None,
                ..t
            })
            .collect();
        write_usage_prices(&failed, &changes, retry, |environment, result| {
            results.lock().insert(environment.clone(), result);
        })
        .await;
        assert_eq!(retry_calls.lock().len(), 2);
        assert!(retry_calls.lock().iter().all(|(env, _)| *env != id("fast")));
        assert!(
            results
                .lock()
                .values()
                .all(|result| *result == UsagePriceWriteResult::Saved)
        );
    }

    #[tokio::test]
    async fn resets_only_the_chosen_model_on_the_selected_destination() {
        let (calls, write) = recorder(Ok(UsageWriteOutcome::Success));
        write_usage_prices(
            &[target("selected")],
            &HashMap::from([(id("selected"), vec![price_change("example", None)])]),
            write,
            |environment, result| {
                assert_eq!(*environment, id("selected"));
                assert_eq!(result, UsagePriceWriteResult::Saved);
            },
        )
        .await;
        let calls = calls.lock();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1.usage_price_overrides,
            Some(IndexMap::from([("example".to_owned(), None)]))
        );
    }

    #[tokio::test]
    async fn reports_thrown_writes_and_restrictions_without_aborting_other_saves() {
        let (calls, write) = recorder(Err(UsageWriteError));
        let results: Mutex<HashMap<EnvironmentId, UsagePriceWriteResult>> = Mutex::default();
        let mut denied = target("denied");
        denied.unavailable = Some("Read-only access".to_owned());
        let mut old = target("old");
        old.unavailable = Some("Update server to edit prices".to_owned());
        write_usage_prices(
            &[target("lost"), denied, old],
            &HashMap::from([(id("lost"), vec![price_change("example", Some(price()))])]),
            write,
            |environment, result| {
                results.lock().insert(environment.clone(), result);
            },
        )
        .await;
        assert_eq!(calls.lock().len(), 1);
        let results = results.lock();
        let failed = |error: &str| UsagePriceWriteResult::Failed {
            error: error.to_owned(),
        };
        assert_eq!(results[&id("lost")], failed("Could not save. Try again."));
        assert_eq!(results[&id("denied")], failed("Read-only access"));
        assert_eq!(results[&id("old")], failed("Update server to edit prices"));
    }

    #[tokio::test]
    async fn sends_mappings_beside_prices_and_fails_servers_that_cannot_store_them() {
        let (calls, write) = recorder(Ok(UsageWriteOutcome::Success));
        let results: Mutex<HashMap<EnvironmentId, UsagePriceWriteResult>> = Mutex::default();
        let mut old = target("old");
        old.aliases = None;
        let changes = vec![
            UsagePriceChange::Alias {
                model: "example".to_owned(),
                alias: Some("example-model".to_owned()),
            },
            price_change("example", None),
        ];
        write_usage_prices(
            &[target("current"), old],
            &HashMap::from([(id("current"), changes.clone()), (id("old"), changes)]),
            write,
            |environment, result| {
                results.lock().insert(environment.clone(), result);
            },
        )
        .await;
        let calls = calls.lock();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, id("current"));
        assert_eq!(
            calls[0].1,
            UsagePricePatch {
                usage_price_overrides: Some(IndexMap::from([("example".to_owned(), None)])),
                usage_model_aliases: Some(IndexMap::from([(
                    "example".to_owned(),
                    Some("example-model".to_owned())
                )])),
            }
        );
        assert_eq!(
            results.lock()[&id("old")],
            UsagePriceWriteResult::Failed {
                error: "Update server to map models".to_owned()
            }
        );
    }
}
