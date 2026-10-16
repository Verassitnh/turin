//! Backoff for live feed resubscribe (upstream `subscribeDynamic`
//! `retryExpectedFailureAfter`: 250 ms doubling, capped at 30 s).

const RETRY_BASE_MS: u64 = 250;
const RETRY_MAX_MS: u64 = 30_000;
const REFETCH_MAX_ATTEMPTS: u32 = 5;

/// Delay before resubscribing after `failure_count` consecutive stream failures
/// (1-based: first retry uses `failure_count == 1`).
pub fn stream_resubscribe_delay_ms(failure_count: u32) -> u64 {
    let exponent = failure_count.saturating_sub(1).min(20);
    RETRY_BASE_MS
        .saturating_mul(2u64.saturating_pow(exponent))
        .min(RETRY_MAX_MS)
}

pub fn refetch_max_attempts() -> u32 {
    REFETCH_MAX_ATTEMPTS
}

pub fn refetch_retry_delay_ms(attempt: u32) -> u64 {
    stream_resubscribe_delay_ms(attempt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_delay_doubles_from_250_ms_and_caps_at_30_s() {
        assert_eq!(stream_resubscribe_delay_ms(1), 250);
        assert_eq!(stream_resubscribe_delay_ms(2), 500);
        assert_eq!(stream_resubscribe_delay_ms(8), 32_000.min(RETRY_MAX_MS));
        assert_eq!(stream_resubscribe_delay_ms(20), RETRY_MAX_MS);
    }
}
