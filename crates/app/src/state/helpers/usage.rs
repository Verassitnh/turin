//! Port of upstream/packages/client-runtime/src/state/usage.ts (pure parts).
//!
//! `refreshUsage` drives Effect atoms (`usageSummary`, `refreshUsageRates`, the presentation
//! atom) and is cut with the atom registry; the Cursor Keychain predicates and the limits
//! refresh cooldown are kept.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use turin_contracts::EnvironmentId;
use turin_contracts::usage::{UsageProviderKind, UsageSourceAction, UsageSummary};

use super::provider_types::{ServerProvider, ServerProviderState};

/// Offer the Cursor Keychain prompt only where a working Cursor provider could use it.
pub fn needs_cursor_keychain_access(
    summary: Option<&UsageSummary>,
    providers: Option<&[ServerProvider]>,
) -> bool {
    summary.is_some_and(|summary| {
        summary
            .sources
            .iter()
            .any(|source| source.action == Some(UsageSourceAction::EnableCursorKeychain))
    }) && providers.is_some_and(|providers| {
        providers.iter().any(|provider| {
            provider.driver.as_str() == "cursor" && provider.status == ServerProviderState::Ready
        })
    })
}

/// The two fields `cursorKeychainAccessEnvironments` reads from an environment row.
pub trait CursorKeychainEnvironment {
    fn summary(&self) -> Option<&UsageSummary>;
    fn needs_cursor_keychain_access(&self) -> bool;
}

/// Environments to offer the Cursor Keychain prompt in the usage summary.
///
/// Any environment reading a Cursor account already reports that account's
/// history from every machine, so the prompt only adds duplicates there. A
/// different account on another machine stays reachable from provider settings.
pub fn cursor_keychain_access_environments<E: CursorKeychainEnvironment>(
    environments: &[E],
) -> Vec<&E> {
    let has_cursor_account = environments.iter().any(|environment| {
        environment.summary().is_some_and(|summary| {
            summary.sources.iter().any(|source| {
                source.fingerprint.provider == UsageProviderKind::Cursor
                    && source.fingerprint.host_id.as_str() == "cursor.com"
            })
        })
    });
    if has_cursor_account {
        Vec::new()
    } else {
        environments
            .iter()
            .filter(|environment| environment.needs_cursor_keychain_access())
            .collect()
    }
}

/// How long automatic limits refreshes stay quiet after a check settles.
pub const LIMITS_REFRESH_COOLDOWN_MS: i64 = 5 * 60_000;

type SharedRefresh<A, E> = Shared<BoxFuture<'static, Result<A, E>>>;

struct LimitsState<A, E> {
    refresh_after: HashMap<EnvironmentId, i64>,
    pending: HashMap<EnvironmentId, SharedRefresh<A, E>>,
}

/// `refreshUsageLimits`: at most one limits check per environment at a time, with a cooldown
/// for automatic refreshes. Upstream keeps the maps at module scope and reads `Date.now()`;
/// here the maps live on the value and the clock is injected so tests can move it.
pub struct UsageLimitsRefresher<A, E> {
    state: Arc<Mutex<LimitsState<A, E>>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl<A, E> Clone for UsageLimitsRefresher<A, E> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            now_ms: self.now_ms.clone(),
        }
    }
}

enum Step<A, E> {
    Join(SharedRefresh<A, E>),
    Skip,
    Started(SharedRefresh<A, E>),
}

impl<A, E> UsageLimitsRefresher<A, E>
where
    A: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    pub fn new(now_ms: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new(Mutex::new(LimitsState {
                refresh_after: HashMap::new(),
                pending: HashMap::new(),
            })),
            now_ms: Arc::new(now_ms),
        }
    }

    /// `Ok(None)` is upstream's `undefined`: an automatic refresh that was skipped.
    ///
    /// The check runs on its own task, like a JS promise, so it finishes and starts the cooldown
    /// even if every caller stops waiting. Must be called inside a tokio runtime.
    pub async fn refresh<F, Fut>(
        &self,
        environment_id: &EnvironmentId,
        refresh: F,
        automatic: bool,
        after_pending: bool,
    ) -> Result<Option<A>, E>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<A, E>> + Send + 'static,
    {
        let mut automatic = automatic;
        let mut refresh = Some(refresh);
        loop {
            let step = {
                let mut state = self.state.lock();
                if let Some(pending) = state.pending.get(environment_id) {
                    Step::Join(pending.clone())
                } else if automatic
                    && (self.now_ms)()
                        < state
                            .refresh_after
                            .get(environment_id)
                            .copied()
                            .unwrap_or(0)
                {
                    Step::Skip
                } else {
                    let run = refresh.take().expect("the check starts at most once");
                    let shared = self.start(environment_id, run);
                    state.pending.insert(environment_id.clone(), shared.clone());
                    Step::Started(shared)
                }
            };
            match step {
                Step::Join(pending) => {
                    if after_pending {
                        // The new check still needs to run if the earlier one failed.
                        let _ = pending.await;
                        automatic = false;
                        continue;
                    }
                    // Manual refresh waits for the current check; automatic refresh does not repeat it.
                    return if automatic {
                        Ok(None)
                    } else {
                        pending.await.map(Some)
                    };
                }
                Step::Skip => return Ok(None),
                Step::Started(current) => return current.await.map(Some),
            }
        }
    }

    fn start<F, Fut>(&self, environment_id: &EnvironmentId, refresh: F) -> SharedRefresh<A, E>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<A, E>> + Send + 'static,
    {
        let state = self.state.clone();
        let now_ms = self.now_ms.clone();
        let id = environment_id.clone();
        let handle = tokio::spawn(async move {
            let result = refresh().await;
            let mut state = state.lock();
            state.pending.remove(&id);
            state
                .refresh_after
                .insert(id, now_ms() + LIMITS_REFRESH_COOLDOWN_MS);
            result
        });
        async move {
            match handle.await {
                Ok(result) => result,
                Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Err(error) => panic!("usage limits check was cancelled: {error}"),
            }
        }
        .boxed()
        .shared()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use tokio::sync::oneshot;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
    use turin_contracts::usage::{
        USAGE_CONTRACT_VERSION, UsageDay, UsagePricing, UsagePricingStatus, UsageSource,
        UsageSourceFingerprint, UsageSourceStatus,
    };

    fn text(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn summary(sources: Vec<UsageSource>) -> UsageSummary {
        UsageSummary {
            contract_version: USAGE_CONTRACT_VERSION,
            read_at: "2026-09-05T12:00:00Z".into(),
            time_zone: text("UTC"),
            since_day: UsageDay::new("2026-09-05").unwrap(),
            until_day: UsageDay::new("2026-09-05").unwrap(),
            buckets: vec![],
            sources,
            pricing: UsagePricing {
                status: UsagePricingStatus::Fresh,
                source: text("test"),
                fetched_at: None,
                known_models: 1,
            },
            scan_duration_ms: 1,
        }
    }

    fn cursor_source(
        host_id: &str,
        home: &str,
        volume: &str,
        scanned: u64,
        action: Option<UsageSourceAction>,
    ) -> UsageSource {
        UsageSource {
            fingerprint: UsageSourceFingerprint {
                host_id: text(host_id),
                provider: UsageProviderKind::Cursor,
                resolved_home_path: text(home),
                volume_id: volume.into(),
            },
            status: UsageSourceStatus::Ok,
            scanned_files: scanned,
            skipped_files: 0,
            malformed_records: 0,
            distinct_sessions: scanned,
            message: action.map(|_| text("Cursor account usage is off on this environment.")),
            action,
            refreshing: None,
        }
    }

    fn cursor_prompt() -> UsageSummary {
        summary(vec![cursor_source(
            "host",
            "/Users/me/.cursor/auth.json",
            "volume",
            0,
            Some(UsageSourceAction::EnableCursorKeychain),
        )])
    }

    fn cursor(status: ServerProviderState) -> ServerProvider {
        let mut provider = ServerProvider::new(
            ProviderInstanceId::new("cursor").unwrap(),
            ProviderDriverKind::new("cursor").unwrap(),
        );
        provider.enabled = status != ServerProviderState::Disabled;
        provider.status = status;
        provider
    }

    #[test]
    fn offers_access_only_when_cursor_is_ready_on_that_environment() {
        let prompt = cursor_prompt();
        assert!(needs_cursor_keychain_access(
            Some(&prompt),
            Some(&[cursor(ServerProviderState::Ready)])
        ));
        assert!(!needs_cursor_keychain_access(
            Some(&prompt),
            Some(&[cursor(ServerProviderState::Error)])
        ));
        assert!(!needs_cursor_keychain_access(
            Some(&prompt),
            Some(&[cursor(ServerProviderState::Disabled)])
        ));
        assert!(!needs_cursor_keychain_access(Some(&prompt), Some(&[])));
        assert!(!needs_cursor_keychain_access(Some(&prompt), None));
        assert!(!needs_cursor_keychain_access(
            Some(&summary(vec![])),
            Some(&[cursor(ServerProviderState::Ready)])
        ));
    }

    struct Row {
        summary: UsageSummary,
        needs: bool,
    }

    impl CursorKeychainEnvironment for Row {
        fn summary(&self) -> Option<&UsageSummary> {
            Some(&self.summary)
        }
        fn needs_cursor_keychain_access(&self) -> bool {
            self.needs
        }
    }

    #[test]
    fn stops_offering_access_once_any_environment_reads_the_cursor_account() {
        let off = || Row {
            summary: cursor_prompt(),
            needs: true,
        };
        let account = Row {
            summary: summary(vec![cursor_source(
                "cursor.com",
                "cursor-account:abc",
                "abc",
                1,
                None,
            )]),
            needs: false,
        };
        let both = [off(), off()];
        let offered = cursor_keychain_access_environments(&both);
        assert_eq!(offered.len(), 2);
        assert!(std::ptr::eq(offered[0], &both[0]));
        assert!(std::ptr::eq(offered[1], &both[1]));
        assert!(cursor_keychain_access_environments(&[off(), account]).is_empty());
    }

    fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).unwrap()
    }

    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    type Pending = Shared<BoxFuture<'static, Result<String, String>>>;

    fn pending_pair() -> (oneshot::Sender<Result<String, String>>, Pending) {
        let (tx, rx) = oneshot::channel();
        (tx, async move { rx.await.unwrap() }.boxed().shared())
    }

    fn counted(
        calls: &Arc<AtomicUsize>,
        pending: &Pending,
    ) -> impl FnOnce() -> Pending + Send + 'static {
        let calls = calls.clone();
        let pending = pending.clone();
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            pending
        }
    }

    #[tokio::test]
    async fn runs_a_fresh_check_after_an_in_flight_check_when_settings_change() {
        let refresher = UsageLimitsRefresher::<String, String>::new(|| 0);
        let id = env("limits-after-enable");
        let (old_tx, old_check) = pending_pair();
        let old_calls = Arc::new(AtomicUsize::new(0));
        let new_calls = Arc::new(AtomicUsize::new(0));
        let first = tokio::spawn({
            let (refresher, id) = (refresher.clone(), id.clone());
            let check = counted(&old_calls, &old_check);
            async move { refresher.refresh(&id, check, true, false).await }
        });
        settle().await;
        let after_enable = tokio::spawn({
            let (refresher, id, new_calls) = (refresher.clone(), id.clone(), new_calls.clone());
            async move {
                refresher
                    .refresh(
                        &id,
                        move || async move {
                            new_calls.fetch_add(1, Ordering::SeqCst);
                            Ok("new limits".to_owned())
                        },
                        false,
                        true,
                    )
                    .await
            }
        });
        settle().await;
        assert_eq!(new_calls.load(Ordering::SeqCst), 0);
        old_tx.send(Ok("old limits".into())).unwrap();
        assert_eq!(first.await.unwrap(), Ok(Some("old limits".to_owned())));
        assert_eq!(
            after_enable.await.unwrap(),
            Ok(Some("new limits".to_owned()))
        );
        assert_eq!(new_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn joins_manual_calls_and_gates_automatic_refreshes_after_success_or_failure() {
        let clock = Arc::new(AtomicI64::new(1_000));
        let refresher = UsageLimitsRefresher::<String, String>::new({
            let clock = clock.clone();
            move || clock.load(Ordering::SeqCst)
        });
        for fails in [false, true] {
            let id = env(&format!("limits-{fails}"));
            let (tx, pending) = pending_pair();
            let calls = Arc::new(AtomicUsize::new(0));
            let first = tokio::spawn({
                let (refresher, id) = (refresher.clone(), id.clone());
                let check = counted(&calls, &pending);
                async move { refresher.refresh(&id, check, true, false).await }
            });
            settle().await;
            // A second automatic call neither waits nor repeats the check.
            assert_eq!(
                refresher
                    .refresh(&id, counted(&calls, &pending), true, false)
                    .await,
                Ok(None)
            );
            let manual = tokio::spawn({
                let (refresher, id) = (refresher.clone(), id.clone());
                let check = counted(&calls, &pending);
                async move { refresher.refresh(&id, check, false, false).await }
            });
            settle().await;
            assert!(!manual.is_finished());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            if fails {
                tx.send(Err("unavailable".into())).unwrap();
                assert_eq!(first.await.unwrap(), Err("unavailable".to_owned()));
                assert_eq!(manual.await.unwrap(), Err("unavailable".to_owned()));
            } else {
                tx.send(Ok("quota".into())).unwrap();
                assert_eq!(first.await.unwrap(), Ok(Some("quota".to_owned())));
                assert_eq!(manual.await.unwrap(), Ok(Some("quota".to_owned())));
            }
            let next_calls = Arc::new(AtomicUsize::new(0));
            let next = || {
                let next_calls = next_calls.clone();
                move || async move {
                    next_calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>("next".to_owned())
                }
            };
            clock.store(300_999, Ordering::SeqCst);
            refresher.refresh(&id, next(), true, false).await.unwrap();
            assert_eq!(next_calls.load(Ordering::SeqCst), 0);
            clock.store(301_000, Ordering::SeqCst);
            refresher.refresh(&id, next(), true, false).await.unwrap();
            assert_eq!(next_calls.load(Ordering::SeqCst), 1);
            refresher.refresh(&id, next(), false, false).await.unwrap();
            assert_eq!(next_calls.load(Ordering::SeqCst), 2);
            clock.store(1_000, Ordering::SeqCst);
        }
    }
}
