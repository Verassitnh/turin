//! Port of upstream/packages/source-control-core/src/server/SourceControlRateLimit.ts
//!
//! `CredentialScope` and `Interactive` live in [`super::ambient`].

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use turin_contracts::source_control::SourceControlProviderKind;

use super::ambient;
use super::clock::Clock;

const FALLBACK_COOLDOWN_MS: f64 = 30_000.0;
const MAX_FALLBACK_COOLDOWN_MS: f64 = 15.0 * 60.0 * 1000.0;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RateLimitKey {
    pub provider: SourceControlProviderKind,
    pub host: String,
}

#[derive(Debug, Clone)]
pub struct RateLimitLease {
    pub provider: SourceControlProviderKind,
    pub host: String,
    pub lease: i64,
}

impl RateLimitLease {
    pub fn new(key: &RateLimitKey, lease: i64) -> Self {
        Self {
            provider: key.provider.clone(),
            host: key.host.clone(),
            lease,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RateLimitEntry {
    attempt: i64,
    generation: i64,
    retry_at: i64,
}

/// `SourceControlRateLimitPausedError`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{}", self.detail())]
pub struct SourceControlRateLimitPausedError {
    pub provider: SourceControlProviderKind,
    pub host: String,
    pub retry_at: i64,
}

impl SourceControlRateLimitPausedError {
    pub fn detail(&self) -> String {
        format!(
            "{} requests to {} are paused until the rate limit resets.",
            self.provider, self.host
        )
    }
}

fn normalized_key(provider: &SourceControlProviderKind, host: &str, scope: &str) -> String {
    format!("{provider}\0{}\0{scope}", host.trim().to_lowercase())
}

fn fallback_cooldown_ms(attempt: i64) -> i64 {
    let exponent = (attempt - 1).max(0).min(60) as i32;
    (FALLBACK_COOLDOWN_MS * 2f64.powi(exponent)).min(MAX_FALLBACK_COOLDOWN_MS) as i64
}

/// The pause a `Retry-After` header asks for: delta-seconds or an HTTP date.
pub fn retry_at_from_header(value: Option<&str>, now: i64) -> Option<i64> {
    let normalized = value?.trim();
    if !normalized.is_empty() && normalized.bytes().all(|b| b.is_ascii_digit()) {
        let seconds: f64 = normalized.parse().ok()?;
        let retry_at = now as f64 + seconds * 1_000.0;
        // Number.isSafeInteger
        return (retry_at.abs() <= 9_007_199_254_740_991.0 && retry_at.fract() == 0.0)
            .then_some(retry_at as i64);
    }
    let parsed = chrono::DateTime::parse_from_rfc2822(normalized)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(normalized))
        .ok()?
        .timestamp_millis();
    (parsed > now).then_some(parsed)
}

pub struct SourceControlRateLimit {
    entries: Mutex<HashMap<String, RateLimitEntry>>,
    clock: Arc<dyn Clock>,
}

impl SourceControlRateLimit {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            clock,
        })
    }

    /// Returns the lease (generation) to record the outcome under, or the pause in force.
    pub fn check(
        &self,
        key: &RateLimitKey,
        allow_paused: bool,
    ) -> Result<i64, SourceControlRateLimitPausedError> {
        let now = self.clock.now_millis();
        let map_key = normalized_key(&key.provider, &key.host, &ambient::credential_scope());
        let entries = self.entries.lock();
        let entry = entries.get(&map_key).copied();
        if let Some(entry) = entry
            && entry.retry_at > now
            && !allow_paused
        {
            return Err(SourceControlRateLimitPausedError {
                provider: key.provider.clone(),
                host: key.host.trim().to_lowercase(),
                retry_at: entry.retry_at,
            });
        }
        Ok(entry.map_or(0, |e| e.generation))
    }

    pub fn record_rate_limit(&self, input: &RateLimitLease, retry_at: Option<i64>) {
        let now = self.clock.now_millis();
        let map_key = normalized_key(&input.provider, &input.host, &ambient::credential_scope());
        let mut entries = self.entries.lock();
        let previous = entries.get(&map_key).copied();
        if let Some(previous) = previous
            && previous.generation > input.lease
        {
            if previous.retry_at <= now && retry_at.is_none_or(|r| r <= now) {
                return;
            }
            let next_retry_at = match retry_at {
                Some(r) if r > previous.retry_at => r,
                _ => previous.retry_at,
            };
            if next_retry_at == previous.retry_at {
                return;
            }
            entries.insert(
                map_key,
                RateLimitEntry {
                    retry_at: next_retry_at,
                    ..previous
                },
            );
            return;
        }
        let attempt = previous.map_or(0, |p| p.attempt) + 1;
        let proposed = match retry_at {
            Some(r) if r > now => r,
            _ => now + fallback_cooldown_ms(attempt),
        };
        let next_retry_at = match previous {
            Some(p) if p.retry_at > now => p.retry_at.max(proposed),
            _ => proposed,
        };
        entries.insert(
            map_key,
            RateLimitEntry {
                attempt,
                generation: previous.map_or(0, |p| p.generation).max(input.lease) + 1,
                retry_at: next_retry_at,
            },
        );
    }

    pub fn record_success(&self, input: &RateLimitLease) {
        let now = self.clock.now_millis();
        let map_key = normalized_key(&input.provider, &input.host, &ambient::credential_scope());
        let mut entries = self.entries.lock();
        let Some(previous) = entries.get(&map_key).copied() else {
            return;
        };
        if previous.generation != input.lease || previous.retry_at > now {
            return;
        }
        entries.insert(
            map_key,
            RateLimitEntry {
                attempt: 0,
                generation: previous.generation,
                retry_at: 0,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_control_core::clock::ManualClock;

    fn github() -> RateLimitKey {
        RateLimitKey {
            provider: SourceControlProviderKind::new("github").unwrap(),
            host: "github.com".into(),
        }
    }
    fn lease(key: &RateLimitKey, n: i64) -> RateLimitLease {
        RateLimitLease::new(key, n)
    }

    #[tokio::test]
    async fn isolates_cooldowns_for_verified_credentials_on_the_same_host() {
        let limits = SourceControlRateLimit::new(ManualClock::new(0));
        let key = github();
        ambient::with_credential_scope("first".into(), async {
            limits.record_rate_limit(&lease(&key, 0), None);
        })
        .await;
        ambient::with_credential_scope("second".into(), async {
            limits.check(&key, false).unwrap();
        })
        .await;
        let error = ambient::with_credential_scope("first".into(), async {
            limits.check(&key, false).unwrap_err()
        })
        .await;
        assert_eq!(error.provider.as_str(), "github");
    }

    #[test]
    fn parses_retry_after_seconds_and_http_dates() {
        assert_eq!(retry_at_from_header(Some("120"), 1_000), Some(121_000));
        assert_eq!(
            retry_at_from_header(Some("Thu, 01 Jan 1970 00:02:01 GMT"), 1_000),
            Some(121_000)
        );
        assert_eq!(retry_at_from_header(Some("later"), 1_000), None);
        assert_eq!(retry_at_from_header(None, 1_000), None);
    }

    #[test]
    fn backs_off_repeated_rate_limits_until_a_successful_request() {
        let clock = ManualClock::new(0);
        let limits = SourceControlRateLimit::new(clock.clone());
        let key = github();

        let first = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, first), None);
        let pause = limits.check(&key, false).unwrap_err();
        assert_eq!(pause.retry_at, 30_000);
        assert_eq!(pause.host, "github.com");
        assert_eq!(
            pause.detail(),
            "github requests to github.com are paused until the rate limit resets."
        );
        assert_eq!(pause.to_string(), pause.detail());

        clock.advance(30_000);
        let second = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, second), None);
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 90_000);

        clock.advance(60_000);
        let success = limits.check(&key, false).unwrap();
        limits.record_success(&lease(&key, success));
        let reset = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, reset), None);
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 120_000);
    }

    #[test]
    fn honors_a_provider_reset_time() {
        let clock = ManualClock::new(1_000);
        let limits = SourceControlRateLimit::new(clock.clone());
        let key = github();
        let l = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, l), Some(121_000));
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 121_000);
        clock.advance(119_999);
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 121_000);
        clock.advance(1);
        assert_eq!(limits.check(&key, false).unwrap(), 1);
    }

    #[test]
    fn lets_an_interactive_request_through_without_clearing_an_active_pause() {
        let clock = ManualClock::new(1_000);
        let limits = SourceControlRateLimit::new(clock);
        let key = github();
        let l = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, l), Some(121_000));

        let interactive = limits.check(&key, true).unwrap();
        limits.record_success(&lease(&key, interactive));
        assert_eq!(interactive, 1);
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 121_000);

        limits.record_rate_limit(&lease(&key, interactive), Some(61_000));
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 121_000);
    }

    #[test]
    fn keeps_providers_and_hosts_isolated() {
        let limits = SourceControlRateLimit::new(ManualClock::new(0));
        let key = github();
        let l = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, l), None);
        let gitlab = RateLimitKey {
            provider: SourceControlProviderKind::new("gitlab").unwrap(),
            host: "github.com".into(),
        };
        assert_eq!(limits.check(&gitlab, false).unwrap(), 0);
        let other = RateLimitKey {
            host: "github.example.com".into(),
            ..github()
        };
        assert_eq!(limits.check(&other, false).unwrap(), 0);
    }

    #[test]
    fn does_not_let_an_older_success_clear_a_concurrent_pause() {
        let limits = SourceControlRateLimit::new(ManualClock::new(0));
        let key = github();
        let first = limits.check(&key, false).unwrap();
        let concurrent = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, first), None);
        limits.record_success(&lease(&key, concurrent));
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 30_000);
    }

    #[test]
    fn keeps_a_fresh_provider_reset_from_an_older_request() {
        let clock = ManualClock::new(0);
        let limits = SourceControlRateLimit::new(clock.clone());
        let key = github();
        let stale = limits.check(&key, false).unwrap();
        limits.record_rate_limit(&lease(&key, stale), None);
        clock.advance(31_000);
        limits.record_rate_limit(&lease(&key, stale), Some(61_000));
        assert_eq!(limits.check(&key, false).unwrap_err().retry_at, 61_000);
    }
}
