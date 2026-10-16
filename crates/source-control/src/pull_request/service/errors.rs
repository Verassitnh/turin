//! Error mapping of `PullRequestService.ts`: `toPullRequestError`, `isProviderUnusable`,
//! `providerDetail`, plus the refusal sentences the write paths share.

use serde::Serialize;
use serde_json::{Value, json};
use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestOperationError, PullRequestOperationErrorReason,
    PullRequestReviewVerdict, PullRequestUnavailableError, PullRequestUnavailableReason,
    pull_request_provider_requirement,
};
use turin_contracts::source_control::SourceControlProviderKind;

use crate::pull_request::read_cache::PullRequestReadError;
use crate::source_control_core::pull_request_provider::{
    PullRequestProviderError, PullRequestProviderErrorReason,
};

/// `PullRequestError = PullRequestUnavailableError | PullRequestOperationError`; the same type the
/// read cache and the review guide use.
pub type PullRequestError = PullRequestReadError;

/// Shown when a message that must be non-blank turns out to be (the contracts refuse a blank
/// `detail`; upstream's constructors do not check).
const BLANK_DETAIL: &str = "The operation failed.";

/// `TrimmedNonEmptyString` from a value that upstream would have passed through unchecked. A
/// blank value, which no host sends, becomes a visible placeholder instead of a panic.
pub(crate) fn tne(value: impl AsRef<str>) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value.as_ref())
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("(none)").expect("non-blank literal"))
}

pub(crate) fn tne_opt(value: Option<&str>) -> Option<TrimmedNonEmptyString> {
    value.map(tne)
}

/// `PositiveInt` from a number upstream would have passed through unchecked.
pub(crate) fn positive(value: u64) -> PositiveInt {
    PositiveInt::new(value).unwrap_or_else(|| PositiveInt::new(1).expect("one is positive"))
}

/// The wire spelling of a contract enum (`"update-branch"`, `"request-changes"`, ...), which is
/// what upstream interpolates into its messages.
pub(crate) fn wire<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// `new PullRequestOperationError({ operation, detail })`
pub(crate) fn operation_error(operation: &str, detail: impl AsRef<str>) -> PullRequestError {
    PullRequestError::Operation(PullRequestOperationError::Operation {
        operation: operation.to_owned(),
        detail: TrimmedNonEmptyString::new(detail.as_ref())
            .unwrap_or_else(|_| TrimmedNonEmptyString::new(BLANK_DETAIL).expect("non-blank")),
        reason: None,
        cause: None,
    })
}

/// `new PullRequestOperationError({ operation, detail, cause })`
pub(crate) fn operation_error_with_cause(
    operation: &str,
    detail: impl AsRef<str>,
    cause: Value,
) -> PullRequestError {
    PullRequestError::Operation(PullRequestOperationError::Operation {
        operation: operation.to_owned(),
        detail: TrimmedNonEmptyString::new(detail.as_ref())
            .unwrap_or_else(|_| TrimmedNonEmptyString::new(BLANK_DETAIL).expect("non-blank")),
        reason: None,
        cause: Some(cause),
    })
}

/// `new PullRequestUnavailableError({ reason })`
pub(crate) fn unavailable_error(reason: PullRequestUnavailableReason) -> PullRequestError {
    PullRequestError::Unavailable(PullRequestUnavailableError::Unavailable {
        reason,
        provider: None,
        cause: None,
    })
}

/// The provider failure as the `cause` of the error built from it. Upstream keeps the error
/// object itself (`Schema.is(PullRequestProviderError)` finds it again); the contract error here
/// carries JSON, so the same fields travel in that shape and [`provider_failure_reason`] reads
/// the reason back.
pub(crate) fn provider_error_cause(error: &PullRequestProviderError) -> Value {
    let mut cause = json!({
        "_tag": "PullRequestProviderError",
        "provider": error.provider.as_str(),
        "operation": error.operation,
        "reason": error.reason.as_str(),
        "detail": error.detail,
    });
    if let Some(retry_at) = error.retry_at {
        cause["retryAt"] = json!(retry_at);
    }
    cause
}

/// The reason of the provider failure an operation error was made from, where it was made from
/// one (`isPullRequestProviderError(error.cause)`).
pub(crate) fn provider_failure_reason(
    error: &PullRequestOperationError,
) -> Option<PullRequestProviderErrorReason> {
    let PullRequestOperationError::Operation { cause, .. } = error;
    let cause = cause.as_ref()?;
    if cause.get("_tag").and_then(Value::as_str) != Some("PullRequestProviderError") {
        return None;
    }
    match cause.get("reason").and_then(Value::as_str)? {
        "missing-tool" => Some(PullRequestProviderErrorReason::MissingTool),
        "unauthenticated" => Some(PullRequestProviderErrorReason::Unauthenticated),
        "rate-limited" => Some(PullRequestProviderErrorReason::RateLimited),
        "not-found" => Some(PullRequestProviderErrorReason::NotFound),
        "failed" => Some(PullRequestProviderErrorReason::Failed),
        _ => None,
    }
}

/// A host that cannot be read at all, as opposed to one request that failed.
pub fn is_provider_unusable(error: &PullRequestProviderError) -> bool {
    matches!(
        error.reason,
        PullRequestProviderErrorReason::MissingTool
            | PullRequestProviderErrorReason::Unauthenticated
    )
}

/// Why a host is not readable, told as the thing to do about it. A host that is simply not set up
/// says so in the same words the whole-page state uses, rather than repeating whatever its tool
/// printed — "HTTP 401" names the symptom, not the fix.
pub(crate) fn provider_detail(error: &PullRequestProviderError) -> String {
    if !is_provider_unusable(error) {
        return error.detail.clone();
    }
    pull_request_provider_requirement(
        &error.provider,
        if error.reason == PullRequestProviderErrorReason::MissingTool {
            PullRequestUnavailableReason::CliMissing
        } else {
            PullRequestUnavailableReason::CliUnauthenticated
        },
    )
    .map(str::to_owned)
    .unwrap_or_else(|| error.detail.clone())
}

pub(crate) fn to_unavailable_error(error: &PullRequestProviderError) -> PullRequestError {
    PullRequestError::Unavailable(PullRequestUnavailableError::Unavailable {
        reason: if error.reason == PullRequestProviderErrorReason::MissingTool {
            PullRequestUnavailableReason::CliMissing
        } else {
            PullRequestUnavailableReason::CliUnauthenticated
        },
        provider: Some(error.provider.clone()),
        cause: Some(provider_error_cause(error)),
    })
}

/// `toPullRequestError(operation)(error)`
pub fn to_pull_request_error(operation: &str, error: PullRequestProviderError) -> PullRequestError {
    if is_provider_unusable(&error) {
        return to_unavailable_error(&error);
    }
    PullRequestError::Operation(PullRequestOperationError::Operation {
        operation: operation.to_owned(),
        detail: TrimmedNonEmptyString::new(&error.detail)
            .unwrap_or_else(|_| TrimmedNonEmptyString::new(BLANK_DETAIL).expect("non-blank")),
        reason: (error.reason == PullRequestProviderErrorReason::NotFound)
            .then_some(PullRequestOperationErrorReason::NotFound),
        cause: Some(provider_error_cause(&error)),
    })
}

/// The `_tag` of an error, for logs.
pub(crate) fn error_tag(error: &PullRequestError) -> &'static str {
    match error {
        PullRequestError::Operation(_) => "PullRequestOperationError",
        PullRequestError::Unavailable(_) => "PullRequestUnavailableError",
    }
}

/// What a verdict is called when refusing it, so the sentence reads as an action.
pub(crate) fn verdict_label(verdict: PullRequestReviewVerdict) -> &'static str {
    match verdict {
        PullRequestReviewVerdict::Comment => "review",
        PullRequestReviewVerdict::Approve => "approve",
        PullRequestReviewVerdict::RequestChanges => "request changes on",
    }
}

/// Why an action is refused to this viewer, said as the access it would take rather than as the
/// refusal the host would have answered with. Merging is the one that needs write and nothing
/// else; the other four are also the author's to take, whatever access they have.
pub(crate) fn action_access_refusal(action: PullRequestAction) -> &'static str {
    match action {
        PullRequestAction::Merge => "You need write access on this repository to merge.",
        PullRequestAction::Ready => {
            "You need write access on this repository, or to have opened this change request, to mark it ready for review."
        }
        PullRequestAction::Draft => {
            "You need write access on this repository, or to have opened this change request, to return it to a draft."
        }
        PullRequestAction::Close => {
            "You need write access on this repository, or to have opened this change request, to close it."
        }
        PullRequestAction::UpdateBranch => {
            "You need write access on this repository, or to have opened this change request, to update its branch."
        }
        PullRequestAction::Reopen => {
            "You need write access on this repository, or to have opened this change request, to reopen it."
        }
        PullRequestAction::EnableAutoMerge => {
            "You need write access on this repository to have it merged for you once it is ready."
        }
        PullRequestAction::DisableAutoMerge => {
            "You need write access on this repository to stop it being merged for you once it is ready."
        }
        PullRequestAction::Revert => {
            "You need write access on this repository to open a revert pull request."
        }
        PullRequestAction::ApproveWorkflows => {
            "You need write access on this repository to approve workflows from a fork pull request."
        }
    }
}

/// Why asking for a review is refused, and why the menu behind it is too. Write access is what the
/// hosts that state anything about this want; the ones that state nothing grant it, so this
/// sentence is only ever the answer where a host said no.
pub(crate) const REVIEWER_REQUEST_REFUSAL: &str =
    "You need write access on this repository to ask for a review.";
pub(crate) const LABEL_CHANGE_REFUSAL: &str =
    "You need triage access on this repository to change its labels.";

/// Convenience for `Option<SourceControlProviderKind>` free call sites.
#[allow(dead_code)]
pub(crate) fn kind_str(kind: &SourceControlProviderKind) -> &str {
    kind.as_str()
}
