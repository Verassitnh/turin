//! Port of upstream's provider retry progress and terminal failure in
//! `CodexAdapterV2.ts`: the `error` notification (`willRetry` or not),
//! `completeProviderRetry`, the retry turn item a turn ends with, and the
//! failure a failed root turn terminates with (`makeRootTerminalEvent`).
use crate::{items, orchestration_adapter::driver, projection::TurnState};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use turin_contracts::{
    PositiveInt, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ProviderFailure, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderRetry, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::{
    failure::{
        ProviderFailureInput, ProviderRetryTurnItemInput, make_provider_failure,
        make_provider_retry_turn_item,
    },
    id_allocator::derive,
    provider_adapter::ProviderAdapterV2Event,
};

/// The last non-retried provider error of a turn (upstream `latestProviderFailure`).
#[derive(Clone)]
pub struct LatestFailure {
    pub native_message: String,
    pub failure: OrchestrationV2ProviderFailure,
}

/// A provider turn that is retrying (upstream `ActiveCodexProviderRetry`).
#[derive(Clone)]
pub struct ProviderRetry {
    pub native_message: String,
    pub retry: OrchestrationV2ProviderRetry,
    pub failure: OrchestrationV2ProviderFailure,
    pub started_at: UtcDateTime,
    pub item_ordinal: u64,
}

/// Upstream `parseCodexRetryProgress`: an `n/m` attempt count in the message.
fn parse_progress(message: &str) -> Option<(u64, u64)> {
    static PROGRESS: OnceLock<Regex> = OnceLock::new();
    let progress = PROGRESS.get_or_init(|| Regex::new(r"\b(\d+)\s*/\s*(\d+)\b").expect("regex"));
    let captures = progress.captures(message)?;
    let attempt: u64 = captures[1].parse().ok()?;
    let max: u64 = captures[2].parse().ok()?;
    (attempt >= 1 && max >= 1).then_some((attempt, max))
}

/// Upstream `codexErrorInfoCode`: a string code, or the first key of an object.
pub fn error_info_code(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(code) => Some(code.clone()),
        Value::Object(object) => object.keys().next().cloned(),
        _ => None,
    }
}

fn class_for(code: Option<&str>, transport: bool) -> OrchestrationV2ProviderFailureClass {
    match code {
        Some("usageLimitExceeded" | "rateLimitExceeded") => {
            OrchestrationV2ProviderFailureClass::UsageLimit
        }
        Some(code)
            if transport && (code.starts_with("http") || code.starts_with("responseStream")) =>
        {
            OrchestrationV2ProviderFailureClass::TransportError
        }
        _ => OrchestrationV2ProviderFailureClass::ProviderError,
    }
}

fn text(value: Option<&Value>, key: &str) -> Option<String> {
    value?.get(key)?.as_str().map(str::to_owned)
}

/// The turn-item ordinal reserved for a turn's terminal failure. Upstream keys it
/// by the provider turn, so a retry and its failure share one item position.
fn failure_ordinal(turn: &mut TurnState) -> u64 {
    let key = format!(
        "terminal-failure:{}",
        derive::provider_turn(&driver(), &turn.native_turn).as_str()
    );
    items::resolve_position(turn, &key, None, UtcDateTime::now()).0
}

/// Upstream's `error` notification handler. A final error is kept for the turn's
/// terminal failure. One that will retry opens or advances the turn's retry.
pub fn error(
    turn: &mut TurnState,
    params: &Value,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(error) = params.get("error") else {
        return vec![];
    };
    let message = text(Some(error), "message").unwrap_or_default();
    let code = error_info_code(error.get("codexErrorInfo"));
    let details = text(Some(error), "additionalDetails")
        .map(|details| details.trim().to_owned())
        .filter(|details| !details.is_empty());
    if params.get("willRetry").and_then(Value::as_bool) != Some(true) {
        let failure = make_provider_failure(ProviderFailureInput {
            cause: None,
            message: Some(details.as_deref().unwrap_or(&message)),
            code: Some(code.as_deref()),
            class: Some(class_for(code.as_deref(), false)),
            retryable: None,
            reset_at: None,
        });
        turn.latest_failure = Some(LatestFailure {
            native_message: message,
            failure,
        });
        return vec![];
    }
    let previous = turn.provider_retry.clone();
    let progress = parse_progress(&message);
    let attempt = progress
        .map(|(attempt, _)| attempt)
        .or_else(|| {
            previous
                .as_ref()
                .map(|retry| retry.retry.attempt.0.get() + 1)
        })
        .unwrap_or(1);
    let max_attempts = progress.map(|(_, max)| max).or_else(|| {
        previous
            .as_ref()
            .and_then(|retry| retry.retry.max_attempts.map(|max| max.0.get()))
    });
    let retry = OrchestrationV2ProviderRetry {
        attempt: PositiveInt::new(attempt).expect("positive attempt"),
        max_attempts: max_attempts.map(|max| PositiveInt::new(max).expect("positive max")),
        retry_delay_ms: None,
    };
    let failure = make_provider_failure(ProviderFailureInput {
        cause: None,
        message: Some(details.as_deref().unwrap_or(&message)),
        code: Some(code.as_deref()),
        class: Some(class_for(code.as_deref(), true)),
        retryable: Some(Some(true)),
        reset_at: None,
    });
    let item_ordinal = match &previous {
        Some(previous) => previous.item_ordinal,
        None => failure_ordinal(turn),
    };
    let state = ProviderRetry {
        native_message: message,
        retry,
        failure,
        started_at: previous.map_or(now, |previous| previous.started_at),
        item_ordinal,
    };
    turn.provider_retry = Some(state.clone());
    vec![retry_item(
        turn,
        &state,
        OrchestrationV2TurnItemStatus::Running,
        now,
    )]
}

/// Upstream `completeProviderRetry`: the next progress of a retrying turn ends its
/// retry as recovered.
pub fn complete(turn: &mut TurnState, now: UtcDateTime) -> Vec<ProviderAdapterV2Event> {
    let Some(state) = turn.provider_retry.take() else {
        return vec![];
    };
    vec![retry_item(
        turn,
        &state,
        OrchestrationV2TurnItemStatus::Completed,
        now,
    )]
}

/// The retry item a turn ends with. Upstream's `finalizeCodexTurn` emits it for a
/// non-failed end, and for any end of a subagent turn.
pub fn finish(
    turn: &mut TurnState,
    retry: Option<ProviderRetry>,
    status: OrchestrationV2ProviderTurnStatus,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(retry) = retry else {
        return vec![];
    };
    if status == OrchestrationV2ProviderTurnStatus::Failed && turn.subagent.is_none() {
        return vec![];
    }
    vec![retry_item(turn, &retry, item_status(status), now)]
}

fn item_status(status: OrchestrationV2ProviderTurnStatus) -> OrchestrationV2TurnItemStatus {
    match status {
        OrchestrationV2ProviderTurnStatus::Completed => OrchestrationV2TurnItemStatus::Completed,
        OrchestrationV2ProviderTurnStatus::Interrupted => {
            OrchestrationV2TurnItemStatus::Interrupted
        }
        OrchestrationV2ProviderTurnStatus::Failed => OrchestrationV2TurnItemStatus::Failed,
        OrchestrationV2ProviderTurnStatus::Cancelled => OrchestrationV2TurnItemStatus::Cancelled,
        OrchestrationV2ProviderTurnStatus::Pending | OrchestrationV2ProviderTurnStatus::Running => {
            OrchestrationV2TurnItemStatus::Running
        }
    }
}

fn retry_item(
    turn: &TurnState,
    retry: &ProviderRetry,
    status: OrchestrationV2TurnItemStatus,
    now: UtcDateTime,
) -> ProviderAdapterV2Event {
    ProviderAdapterV2Event::TurnItemUpdated {
        driver: driver(),
        turn_item: make_provider_retry_turn_item(ProviderRetryTurnItemInput {
            driver: &driver(),
            thread_id: turn.input.thread_id.clone(),
            run_id: turn.run_id(),
            node_id: Some(turn.input.root_node_id.clone()),
            provider_thread_id: turn.input.provider_thread.id.clone(),
            provider_turn_id: derive::provider_turn(&driver(), &turn.native_turn),
            item_ordinal: retry.item_ordinal,
            failure: retry.failure.clone(),
            retry: retry.retry.clone(),
            status,
            started_at: retry.started_at,
            updated_at: now,
        }),
    }
}

/// Upstream `makeRootTerminalEvent`'s failure: the turn's last provider failure, or
/// the retry's, when the terminal's message and code do not contradict it. The
/// failure item's ordinal is reserved here, as upstream does.
pub fn terminal_failure(
    turn: &mut TurnState,
    retry: Option<&ProviderRetry>,
    native_message: Option<&str>,
    code: Option<&str>,
) -> (OrchestrationV2ProviderFailure, PositiveInt) {
    let previous = turn
        .latest_failure
        .as_ref()
        .map(|latest| (latest.native_message.clone(), latest.failure.clone()))
        .or_else(|| retry.map(|retry| (retry.native_message.clone(), retry.failure.clone())));
    let failure = match previous {
        Some((previous_message, failure))
            if native_message.is_none_or(|message| message == previous_message)
                && code.is_none_or(|code| {
                    failure.code.as_ref().map(|known| known.as_str()) == Some(code)
                }) =>
        {
            failure
        }
        _ => make_provider_failure(ProviderFailureInput {
            cause: None,
            message: native_message,
            code: Some(code),
            class: Some(class_for(code, false)),
            retryable: None,
            reset_at: None,
        }),
    };
    let ordinal = failure_ordinal(turn);
    (
        failure,
        PositiveInt::new(ordinal).expect("positive ordinal"),
    )
}
