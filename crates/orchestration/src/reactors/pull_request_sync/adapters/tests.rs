//! The rate-limit mapping reads the provider cause's JSON, as T07 writes it.

use serde_json::json;
use turin_contracts::pull_request::PullRequestOperationError;
use turin_source_control::pull_request::service::errors::PullRequestError;

use super::rate_limited_retry_at;

fn operation(cause: serde_json::Value) -> PullRequestError {
    PullRequestError::Operation(PullRequestOperationError::Operation {
        operation: "PullRequestService.summary".to_owned(),
        detail: turin_contracts::base_schemas::TrimmedNonEmptyString::new("failed")
            .expect("detail"),
        reason: None,
        cause: Some(cause),
    })
}

#[test]
fn a_rate_limited_provider_cause_yields_its_retry_time() {
    let error = operation(json!({
        "_tag": "PullRequestProviderError",
        "reason": "rate-limited",
        "retryAt": 1_800_000_000_000_i64,
    }));
    assert_eq!(rate_limited_retry_at(&error), Some(1_800_000_000_000));
}

#[test]
fn other_provider_failures_do_not_pause_the_host() {
    let error = operation(json!({
        "_tag": "PullRequestProviderError",
        "reason": "not-found",
        "retryAt": 1_800_000_000_000_i64,
    }));
    assert_eq!(rate_limited_retry_at(&error), None);
}

#[test]
fn an_operation_without_a_provider_cause_does_not_pause_the_host() {
    assert_eq!(
        rate_limited_retry_at(&operation(json!({ "_tag": "Other" }))),
        None
    );
}
