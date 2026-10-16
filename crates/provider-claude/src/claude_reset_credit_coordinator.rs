//! Port of upstream/apps/server/src/provider/resetCreditCoordinator.ts.
//!
//! Redeeming a reset credit is an account-level action: instances that share
//! the directory holding a provider's login share the credit, so their
//! redemptions must serialise on that directory, not the instance. One lock
//! and one pending idempotency key per account key, so overlapping
//! confirmations from any instance queue rather than spending two credits, and
//! a retry after a timeout re-sends the same attempt.
//!
//! The driver holds one coordinator and shares it with every instance it
//! creates. Each instance must reach it through that shared handle.
use parking_lot::Mutex;
use std::{collections::HashMap, future::Future, sync::Arc};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Default)]
struct AccountRedemption {
    lock: AsyncMutex<()>,
    pending_key: Mutex<Option<String>>,
}

#[derive(Default)]
pub struct ResetCreditCoordinator {
    accounts: Mutex<HashMap<String, Arc<AccountRedemption>>>,
}

impl ResetCreditCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-create the account's state under one lock, so two first
    /// redemptions for the same account cannot each install their own lock.
    fn state_for(&self, account_key: &str) -> Arc<AccountRedemption> {
        self.accounts
            .lock()
            .entry(account_key.to_owned())
            .or_default()
            .clone()
    }

    /// Runs `consume` under the account's lock with a stable idempotency key.
    /// The key is cleared when the provider reports an outcome, or when
    /// `is_settled` says a failure was a final answer (such as a cooldown).
    /// Any other failure, timeouts included, keeps it so the next attempt is
    /// the same attempt.
    pub async fn redeem<T, E, F, Fut>(
        &self,
        account_key: &str,
        consume: F,
        is_settled: impl Fn(&E) -> bool,
    ) -> Result<T, E>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let state = self.state_for(account_key);
        let _permit = state.lock.lock().await;
        let key = state
            .pending_key
            .lock()
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        *state.pending_key.lock() = Some(key.clone());
        match consume(key).await {
            Ok(outcome) => {
                *state.pending_key.lock() = None;
                Ok(outcome)
            }
            Err(error) => {
                if is_settled(&error) {
                    *state.pending_key.lock() = None;
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Semaphore;

    #[derive(Debug, PartialEq)]
    enum Fail {
        TimedOut,
        Cooldown,
    }

    #[tokio::test]
    async fn re_sends_the_same_idempotency_key_after_a_failed_attempt_then_clears_it() {
        let coordinator = ResetCreditCoordinator::new();
        let keys = Mutex::new(Vec::new());
        let attempts = AtomicUsize::new(0);
        let consume = |key: String| {
            keys.lock().push(key);
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if attempt == 1 {
                    Err(Fail::TimedOut)
                } else {
                    Ok("reset")
                }
            }
        };

        let first = coordinator.redeem("acct", consume, |_: &Fail| false).await;
        assert_eq!(first, Err(Fail::TimedOut));
        let second = coordinator.redeem("acct", consume, |_: &Fail| false).await;
        assert_eq!(second, Ok("reset"));
        // A fresh redemption after success must be a fresh attempt.
        coordinator
            .redeem("acct", consume, |_: &Fail| false)
            .await
            .unwrap();

        let seen = keys.lock().clone();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], seen[1]);
        assert_ne!(seen[1], seen[2]);
    }

    #[tokio::test]
    async fn starts_a_fresh_attempt_after_a_settled_failure() {
        let coordinator = ResetCreditCoordinator::new();
        let keys = Mutex::new(Vec::new());
        let consume = |key: String| {
            keys.lock().push(key);
            async { Err::<(), _>(Fail::Cooldown) }
        };
        let is_settled = |error: &Fail| *error == Fail::Cooldown;

        let _ = coordinator.redeem("acct", consume, is_settled).await;
        let _ = coordinator.redeem("acct", consume, is_settled).await;

        let seen = keys.lock().clone();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0], seen[1]);
    }

    /// Each `consume` parks on `release` until the test adds a permit, so the
    /// test controls when each redemption finishes. Permits persist, so no
    /// wakeup can be lost.
    struct Gate {
        release: Arc<Semaphore>,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl Gate {
        fn new() -> Self {
            Self {
                release: Arc::new(Semaphore::new(0)),
                in_flight: Arc::new(AtomicUsize::new(0)),
                peak: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn redeem(
            &self,
            coordinator: Arc<ResetCreditCoordinator>,
            account: &'static str,
        ) -> tokio::task::JoinHandle<Result<&'static str, Fail>> {
            let release = self.release.clone();
            let in_flight = self.in_flight.clone();
            let peak = self.peak.clone();
            tokio::spawn(async move {
                coordinator
                    .redeem(
                        account,
                        move |_key| async move {
                            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(now, Ordering::SeqCst);
                            release.acquire().await.expect("gate open").forget();
                            in_flight.fetch_sub(1, Ordering::SeqCst);
                            Ok("reset")
                        },
                        |_: &Fail| false,
                    )
                    .await
            })
        }

        /// Waits until `count` consumes have entered, then reports the peak.
        async fn settle(&self, count: usize) -> usize {
            for _ in 0..200 {
                if self.in_flight.load(Ordering::SeqCst) >= count {
                    break;
                }
                tokio::task::yield_now().await;
            }
            self.peak.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn serialises_concurrent_redemptions_on_the_same_account_not_per_caller() {
        let coordinator = Arc::new(ResetCreditCoordinator::new());
        let gate = Gate::new();

        // Two instances of the same account redeem at once.
        let a = gate.redeem(coordinator.clone(), "acct");
        let b = gate.redeem(coordinator.clone(), "acct");
        // Only one may be inside `consume` while the other waits for the lock.
        assert_eq!(gate.settle(1).await, 1);
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.peak.load(Ordering::SeqCst), 1);

        gate.release.add_permits(1);
        a.await.unwrap().unwrap();
        gate.release.add_permits(1);
        b.await.unwrap().unwrap();
        assert_eq!(gate.peak.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn keeps_different_accounts_independent() {
        let coordinator = Arc::new(ResetCreditCoordinator::new());
        let gate = Gate::new();

        let a = gate.redeem(coordinator.clone(), "acct-a");
        let b = gate.redeem(coordinator.clone(), "acct-b");
        // Both are inside `consume` at once when the accounts do not block each other.
        assert_eq!(gate.settle(2).await, 2);

        gate.release.add_permits(2);
        a.await.unwrap().unwrap();
        b.await.unwrap().unwrap();
    }
}
