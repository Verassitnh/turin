//! Port of upstream/apps/web/src/hooks/usePullRequestChecksRefresh.ts

use turin_contracts::pull_request::{PullRequestCheck, PullRequestCheckStatus};

/// Interval for live refresh while checks are pending (ms).
pub fn pull_request_checks_refresh_interval_ms(checks: &[PullRequestCheck]) -> i64 {
    if checks.is_empty()
        || checks.iter().any(|check| {
            matches!(
                check.status,
                PullRequestCheckStatus::Pending | PullRequestCheckStatus::ActionRequired
            )
        })
    {
        45_000
    } else {
        60_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::TrimmedNonEmptyString;

    fn check(status: PullRequestCheckStatus) -> PullRequestCheck {
        PullRequestCheck {
            name: TrimmedNonEmptyString::new("ci").unwrap(),
            status,
            description: None,
            url: None,
            required: None,
        }
    }

    #[test]
    fn refreshes_faster_while_pending() {
        assert_eq!(pull_request_checks_refresh_interval_ms(&[]), 45_000);
        assert_eq!(
            pull_request_checks_refresh_interval_ms(&[check(PullRequestCheckStatus::Pending)]),
            45_000
        );
        assert_eq!(
            pull_request_checks_refresh_interval_ms(&[check(PullRequestCheckStatus::Success)]),
            60_000
        );
    }
}
