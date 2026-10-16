//! Port of upstream/packages/source-control-github/src/server/GitHubQuota.ts

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use turin_contracts::source_control::SourceControlProviderKind;

use crate::source_control_core::ambient;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::rate_limit::SourceControlRateLimitPausedError;

/// The share of each quota a background read leaves for a user's next click.
const RESERVE_RATIO: f64 = 0.1;

#[derive(Debug, Clone, Copy)]
struct QuotaSnapshot {
    limit: f64,
    remaining: f64,
    reset_at_ms: f64,
}

struct ObservedQuota {
    resource: String,
    snapshot: QuotaSnapshot,
}

/// JS `Number(value)` for a header: absent is NaN, blank is 0.
pub(crate) fn js_number(value: Option<&String>) -> f64 {
    let Some(value) = value else {
        return f64::NAN;
    };
    let value = crate::shared::js::js_trim(value);
    if value.is_empty() {
        return 0.0;
    }
    value.parse::<f64>().unwrap_or(f64::NAN)
}

/// What GitHub says is left of one quota, from the `x-ratelimit-*` headers every API answer
/// carries. None when the answer named no quota, which an Enterprise host with limits off does.
fn quota_from_headers(headers: &HashMap<String, String>) -> Option<ObservedQuota> {
    let resource = headers
        .get("x-ratelimit-resource")
        .map(|value| crate::shared::js::js_trim(value))
        .unwrap_or_default();
    let limit = js_number(headers.get("x-ratelimit-limit"));
    let remaining = js_number(headers.get("x-ratelimit-remaining"));
    let reset = js_number(headers.get("x-ratelimit-reset"));
    if resource.is_empty()
        || !limit.is_finite()
        || limit <= 0.0
        || !remaining.is_finite()
        || remaining < 0.0
        || !reset.is_finite()
    {
        return None;
    }
    Some(ObservedQuota {
        resource: resource.to_owned(),
        snapshot: QuotaSnapshot {
            limit,
            remaining,
            reset_at_ms: reset * 1_000.0,
        },
    })
}

fn key_of(host: &str, resource: &str, scope: &str) -> String {
    format!("{}\0{resource}\0{scope}", host.trim().to_lowercase())
}

/// Keeps the last tenth of each GitHub quota for interactive requests. A background sweep that
/// would dip below it is refused until the quota resets, so a user's next click still has budget.
///
/// GitHub's quotas: `core` for REST, `graphql` for GraphQL, and a few smaller ones (`search`, ...).
/// Each is spent and reset on its own, so each is tracked on its own.
///
/// The balance is GitHub's own count from the latest answer's headers, so it already includes
/// what anything else spent with the same token, such as an agent's `gh` commands. Requests in
/// flight together all see that one balance; the reserve absorbs what they spend before their
/// answers land. Nothing is debited locally, because a 304 or a request that never reached GitHub
/// spends nothing, and a local count would drift below GitHub's for the rest of the window.
///
/// Interactive requests may spend the reserve, but not a quota GitHub already reported empty.
pub struct GitHubQuota {
    snapshots: Mutex<HashMap<String, QuotaSnapshot>>,
    clock: Arc<dyn Clock>,
}

impl GitHubQuota {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            snapshots: Mutex::new(HashMap::new()),
            clock,
        }
    }

    pub fn admit(
        &self,
        host: &str,
        resource: &str,
        allow_reserve: bool,
    ) -> Result<(), SourceControlRateLimitPausedError> {
        let now = self.clock.now_millis() as f64;
        let key = key_of(host, resource, &ambient::credential_scope());
        let Some(snapshot) = self.snapshots.lock().get(&key).copied() else {
            return Ok(());
        };
        if snapshot.reset_at_ms <= now {
            return Ok(());
        }
        let floor = if allow_reserve {
            1.0
        } else {
            snapshot.limit * RESERVE_RATIO
        };
        if snapshot.remaining >= floor {
            return Ok(());
        }
        Err(SourceControlRateLimitPausedError {
            provider: SourceControlProviderKind::new("github").expect("github is a valid kind"),
            host: host.trim().to_lowercase(),
            retry_at: snapshot.reset_at_ms as i64,
        })
    }

    pub fn observe(&self, host: &str, headers: &HashMap<String, String>) {
        let Some(quota) = quota_from_headers(headers) else {
            return;
        };
        let key = key_of(host, &quota.resource, &ambient::credential_scope());
        let mut snapshots = self.snapshots.lock();
        // The latest answer wins. GitHub serves requests from several regions, so a later answer
        // can report more left in the same window, and keeping the lower one would hold
        // background work at the reserve until the reset. An answer from an older window says
        // nothing of this one.
        if let Some(previous) = snapshots.get(&key)
            && quota.snapshot.reset_at_ms < previous.reset_at_ms
        {
            return;
        }
        snapshots.insert(key, quota.snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_control_core::clock::ManualClock;

    /// 2099-08-13T14:00:00Z in epoch milliseconds.
    const RESET: i64 = 4_088_149_200_000;

    fn headers(remaining: i64, resource: &str, reset: i64) -> HashMap<String, String> {
        HashMap::from([
            ("x-ratelimit-resource".to_owned(), resource.to_owned()),
            ("x-ratelimit-limit".to_owned(), "5000".to_owned()),
            ("x-ratelimit-remaining".to_owned(), remaining.to_string()),
            ("x-ratelimit-reset".to_owned(), (reset / 1000).to_string()),
        ])
    }

    fn graphql(remaining: i64) -> HashMap<String, String> {
        headers(remaining, "graphql", RESET)
    }

    fn quota() -> (GitHubQuota, Arc<ManualClock>) {
        let clock = ManualClock::new(0);
        (GitHubQuota::new(clock.clone()), clock)
    }

    #[tokio::test]
    async fn refuses_a_background_request_below_the_reserve_per_quota_and_account() {
        let (quota, _) = quota();
        quota.observe("github.com", &graphql(499));
        let refused = quota.admit("github.com", "graphql", false).unwrap_err();
        assert_eq!(refused.retry_at, RESET);
        // Another quota, another host and another account each have their own balance.
        quota.admit("github.com", "core", false).unwrap();
        quota.admit("ghe.example", "graphql", false).unwrap();
        ambient::with_credential_scope("b".into(), async {
            quota.admit("github.com", "graphql", false).unwrap();
        })
        .await;
        quota.admit("github.com", "graphql", true).unwrap();
    }

    #[test]
    fn takes_the_latest_balance_in_a_window_and_ignores_an_older_window() {
        let (quota, _) = quota();
        quota.observe("github.com", &graphql(400));
        quota.admit("github.com", "graphql", false).unwrap_err();
        // GitHub can report more left later in the same window, from another region.
        quota.observe("github.com", &graphql(900));
        quota.admit("github.com", "graphql", false).unwrap();
        quota.observe("github.com", &graphql(400));
        quota.admit("github.com", "graphql", false).unwrap_err();
        // An answer from an older window says nothing about this one.
        quota.observe("github.com", &headers(4000, "graphql", RESET - 3_600_000));
        quota.admit("github.com", "graphql", false).unwrap_err();
        // A later window replaces it.
        quota.observe("github.com", &headers(4000, "graphql", RESET + 3_600_000));
        quota.admit("github.com", "graphql", false).unwrap();
    }

    #[test]
    fn lets_interactive_requests_spend_the_reserve_but_not_an_empty_quota() {
        let (quota, _) = quota();
        quota.observe("github.com", &graphql(1));
        quota.admit("github.com", "graphql", true).unwrap();
        quota.observe("github.com", &graphql(0));
        let refused = quota.admit("github.com", "graphql", true).unwrap_err();
        assert_eq!(refused.retry_at, RESET);
    }

    #[test]
    fn spends_nothing_locally_so_a_free_304_cannot_run_the_balance_down() {
        let (quota, _) = quota();
        quota.observe("github.com", &headers(501, "core", RESET));
        // Conditional reads answer 304 with the balance unchanged, however many are sent.
        for _ in 0..5 {
            quota.admit("github.com", "core", false).unwrap();
            quota.observe("github.com", &headers(501, "core", RESET));
        }
        quota.admit("github.com", "core", false).unwrap();
    }

    #[test]
    fn forgets_a_balance_once_its_window_resets_and_ignores_answers_naming_no_quota() {
        let (quota, clock) = quota();
        quota.observe("github.com", &graphql(0));
        quota.admit("github.com", "graphql", false).unwrap_err();
        clock.set(RESET);
        quota.admit("github.com", "graphql", false).unwrap();
        quota.observe(
            "other.example",
            &HashMap::from([("x-ratelimit-remaining".to_owned(), "0".to_owned())]),
        );
        quota.admit("other.example", "graphql", false).unwrap();
    }
}
