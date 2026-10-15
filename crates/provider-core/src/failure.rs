//! Port of upstream/packages/provider-core/src/server/failure.ts.
//!
//! Cause walking follows `_tag` the way Effect errors do. Rust has no throwing
//! getters, so a cause that cannot be read is [`ProviderFailureCause::Unreadable`]
//! and yields the default message.

use serde_json::Value;
use turin_contracts::{
    NodeId, NonNegativeInt, ProviderThreadId, ProviderTurnId, RunId, ThreadId, TurnItemId,
    UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ProviderFailure, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderFailureCode, OrchestrationV2ProviderFailureMessage,
        OrchestrationV2ProviderRetry, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
    provider_instance::ProviderDriverKind,
};

use crate::id_allocator::derive;

pub const MAX_PROVIDER_FAILURE_MESSAGE_LENGTH: usize = 4_096;
pub const MAX_PROVIDER_FAILURE_CODE_LENGTH: usize = 128;
const DEFAULT_PROVIDER_FAILURE_MESSAGE: &str = "Provider turn failed.";
const HANDOFF_BUDGET_MESSAGE: &str = "Insufficient context allowance for the provider handoff. Compact the target conversation or use a larger-context model; the current request has not been truncated.";
const HANDOFF_UNCERTAIN_MESSAGE: &str = "T3 could not confirm whether conversation history reached the provider. Retry the turn to recover the session.";

/// Raised when portable history does not fit the target provider's context window.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{HANDOFF_BUDGET_MESSAGE}")]
pub struct ContextHandoffBudgetError;

/// A cause `make_provider_failure` can walk. Tags match upstream `_tag`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFailureCause {
    Tagged {
        tag: String,
        message: Option<String>,
        code: Option<String>,
        cause: Option<Box<ProviderFailureCause>>,
    },
    /// A getter threw, or the value was not an object. The walk stops.
    Unreadable,
}

impl ProviderFailureCause {
    pub fn tagged(tag: impl Into<String>) -> Self {
        Self::Tagged {
            tag: tag.into(),
            message: None,
            code: None,
            cause: None,
        }
    }

    pub fn with_cause(mut self, cause: ProviderFailureCause) -> Self {
        if let Self::Tagged { cause: slot, .. } = &mut self {
            *slot = Some(Box::new(cause));
        }
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        if let Self::Tagged { message: slot, .. } = &mut self {
            *slot = Some(message.into());
        }
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        if let Self::Tagged { code: slot, .. } = &mut self {
            *slot = Some(code.into());
        }
        self
    }

    pub fn from_json(value: &Value) -> Self {
        let Value::Object(object) = value else {
            return Self::Unreadable;
        };
        let tag = match object.get("_tag") {
            Some(Value::String(tag)) => tag.clone(),
            _ => return Self::Unreadable,
        };
        let message = object
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let code = object
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let cause = object
            .get("cause")
            .map(|cause| Box::new(Self::from_json(cause)));
        Self::Tagged {
            tag,
            message,
            code,
            cause,
        }
    }
}

fn cause_message(cause: &ProviderFailureCause) -> Option<String> {
    let mut current = Some(cause);
    let mut seen: Vec<*const ProviderFailureCause> = Vec::new();
    let mut message = None;
    for _ in 0..16 {
        let Some(node) = current else {
            break;
        };
        let ptr = node as *const ProviderFailureCause;
        if seen.contains(&ptr) {
            break;
        }
        seen.push(ptr);
        let ProviderFailureCause::Tagged {
            tag,
            message: field,
            cause: next,
            ..
        } = node
        else {
            break;
        };
        match tag.as_str() {
            "ContextHandoffBudgetError" => return Some(HANDOFF_BUDGET_MESSAGE.to_owned()),
            "ClaudeBackgroundWorkBlocksQueryReplacementError" => return field.clone(),
            "ContextHandoffDeliveryUncertainError" => {
                return Some(HANDOFF_UNCERTAIN_MESSAGE.to_owned());
            }
            "ProviderAdapterTurnStartError" => {
                message = Some(
                    "The provider could not start this turn. Retry the turn; if it keeps failing, check the provider setup and server logs."
                        .to_owned(),
                );
            }
            "ProviderAdapterEventStreamError" => {
                message = Some(
                    "The provider event stream closed unexpectedly. Retry the turn; if it keeps failing, check the provider and server logs."
                        .to_owned(),
                );
            }
            "ProviderAdapterOpenSessionError" => {
                message = Some(
                    "The provider session could not be opened. Check that the provider is installed and signed in, then retry the turn."
                        .to_owned(),
                );
            }
            "ProviderAdapterResumeThreadError" => {
                message = Some(
                    "The provider conversation could not be resumed. Retry the turn; if it keeps failing, check the provider and server logs."
                        .to_owned(),
                );
            }
            _ => {}
        }
        current = next.as_deref();
    }
    message
}

fn cause_code(cause: &ProviderFailureCause) -> Option<String> {
    match cause {
        ProviderFailureCause::Tagged { code, .. } => code.clone(),
        ProviderFailureCause::Unreadable => None,
    }
}

fn redact_url(matched: &str) -> String {
    let trailing_len = matched
        .chars()
        .rev()
        .take_while(|ch| matches!(ch, ')' | ',' | '.' | ';' | '!' | '?'))
        .count();
    let (candidate, trailing) = if trailing_len == 0 {
        (matched, "")
    } else {
        let split = matched.len() - trailing_len;
        // Trailing punctuation is ASCII, so the byte split is a char boundary.
        (&matched[..split], &matched[split..])
    };
    // Upstream parses with `new URL` and prints `url.toString()`, so the
    // WHATWG normalization (trailing slash, default port, host case) and its
    // parse failures apply here too.
    let Ok(mut url) = url::Url::parse(candidate) else {
        return "[REDACTED_URL]".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    format!("{}{trailing}", url.as_str())
}

fn replace_unsafe_control_characters(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            let code = character as u32;
            if code <= 0x08
                || (0x0b..=0x0c).contains(&code)
                || (0x0e..=0x1f).contains(&code)
                || code == 0x7f
            {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn redact_provider_failure_text(value: &str) -> String {
    let sanitized = replace_unsafe_control_characters(value);
    let url_re = regex::Regex::new(r#"(?i)\bhttps?://[^\s<>"']+"#).expect("url regex");
    let bearer_re = regex::Regex::new(r"(?i)\b(Bearer|Basic)\s+[^\s,;]+").expect("bearer regex");
    // Upstream's key list needs a word boundary before `token`/`secret`, so
    // `refresh_token=` and `client_secret=` slipped through. The `[a-z0-9_-]*`
    // prefix lets compound keys match. It is still bounded on the right, so
    // `input_tokens=` and `token_count=` stay visible.
    let json_re = regex::Regex::new(
        r#"(?i)(["'][a-z0-9_-]*(?:access[_-]?token|api[_-]?key|authorization|credential|password|secret|token)["']\s*:\s*["'])[^"']*(["'])"#,
    )
    .expect("json regex");
    let pair_re = regex::Regex::new(
        r#"(?i)(\b[a-z0-9_-]*(?:access[_-]?token|api[_-]?key|authorization|credential|password|secret|token)\b\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;]+)"#,
    )
    .expect("pair regex");
    let sk_re = regex::Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}\b").expect("sk regex");
    let step = url_re.replace_all(&sanitized, |caps: &regex::Captures| redact_url(&caps[0]));
    let step = bearer_re.replace_all(&step, "$1 [REDACTED]");
    let step = json_re.replace_all(&step, "$1[REDACTED]$2");
    let step = pair_re.replace_all(&step, "$1[REDACTED]");
    sk_re.replace_all(&step, "[REDACTED]").trim().to_owned()
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn bounded_text(value: &str, max_length: usize) -> String {
    let redacted = redact_provider_failure_text(value);
    if utf16_len(&redacted) <= max_length && redacted.len() <= max_length {
        return redacted;
    }
    // Upstream measures JS string length (UTF-16) and backs up one unit when
    // the cut would split a surrogate. Char iteration does the same. The
    // contracts newtype also rejects messages whose UTF-8 byte length exceeds
    // the cap, so the cut tightens until both limits hold.
    let mut budget = max_length.saturating_sub(1);
    loop {
        let mut out = String::new();
        let mut used = 0usize;
        for character in redacted.chars() {
            let units = character.len_utf16();
            if used + units > budget {
                break;
            }
            let next_bytes = out.len() + character.len_utf8() + "…".len();
            if next_bytes > max_length {
                break;
            }
            out.push(character);
            used += units;
        }
        out.push('…');
        if out.len() <= max_length && utf16_len(&out) <= max_length {
            return out;
        }
        if budget == 0 {
            return "…".to_owned();
        }
        budget -= 1;
    }
}

fn failure_message(text: &str) -> OrchestrationV2ProviderFailureMessage {
    OrchestrationV2ProviderFailureMessage::new(text).unwrap_or_else(|_| {
        OrchestrationV2ProviderFailureMessage::new(DEFAULT_PROVIDER_FAILURE_MESSAGE)
            .expect("default failure message")
    })
}

fn failure_code(text: &str) -> Option<OrchestrationV2ProviderFailureCode> {
    let bounded = bounded_text(text, MAX_PROVIDER_FAILURE_CODE_LENGTH);
    if bounded.is_empty() {
        return None;
    }
    OrchestrationV2ProviderFailureCode::new(bounded).ok()
}

pub struct ProviderFailureInput<'a> {
    pub cause: Option<&'a ProviderFailureCause>,
    pub message: Option<&'a str>,
    pub code: Option<Option<&'a str>>,
    pub class: Option<OrchestrationV2ProviderFailureClass>,
    pub retryable: Option<Option<bool>>,
    pub reset_at: Option<Option<&'a str>>,
}

pub fn make_provider_failure(input: ProviderFailureInput<'_>) -> OrchestrationV2ProviderFailure {
    let raw_message = input
        .message
        .map(str::to_owned)
        .or_else(|| input.cause.and_then(cause_message))
        .unwrap_or_else(|| DEFAULT_PROVIDER_FAILURE_MESSAGE.to_owned());
    let message = bounded_text(&raw_message, MAX_PROVIDER_FAILURE_MESSAGE_LENGTH);
    let message = if message.is_empty() {
        DEFAULT_PROVIDER_FAILURE_MESSAGE.to_owned()
    } else {
        message
    };
    // Upstream `input.code ?? cause.code`: an explicit null falls through too.
    let raw_code = match input.code.flatten() {
        Some(code) => Some(code.to_owned()),
        None => input.cause.and_then(cause_code),
    };
    let code = raw_code.as_deref().and_then(failure_code);
    let class = input
        .class
        .unwrap_or(OrchestrationV2ProviderFailureClass::Unknown);
    let reset_at = if class == OrchestrationV2ProviderFailureClass::UsageLimit {
        input.reset_at.flatten().and_then(|raw| {
            chrono::DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|parsed| {
                    Some(UtcDateTime::from_datetime(parsed.with_timezone(&chrono::Utc)).to_string())
                })
        })
    } else {
        None
    };
    OrchestrationV2ProviderFailure {
        class,
        message: failure_message(&message),
        code,
        retryable: input.retryable.flatten(),
        reset_at,
    }
}

pub struct ProviderFailureTurnItemInput<'a> {
    pub driver: &'a ProviderDriverKind,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub item_ordinal: NonNegativeInt,
    pub failure: OrchestrationV2ProviderFailure,
    pub retry: Option<OrchestrationV2ProviderRetry>,
    pub retry_started_at: Option<UtcDateTime>,
    pub occurred_at: UtcDateTime,
}

pub fn make_provider_failure_turn_item(
    input: ProviderFailureTurnItemInput<'_>,
) -> OrchestrationV2TurnItem {
    let title = if input.failure.class == OrchestrationV2ProviderFailureClass::UsageLimit {
        "Usage limit reached"
    } else {
        "Provider error"
    };
    error_item(
        input.driver,
        input.thread_id,
        input.run_id,
        input.node_id,
        input.provider_thread_id,
        input.provider_turn_id,
        input.item_ordinal,
        OrchestrationV2TurnItemStatus::Failed,
        title,
        input.retry_started_at.unwrap_or(input.occurred_at),
        Some(input.occurred_at),
        input.occurred_at,
        input.failure,
        input.retry,
    )
}

pub struct ProviderRetryTurnItemInput<'a> {
    pub driver: &'a ProviderDriverKind,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub item_ordinal: NonNegativeInt,
    pub failure: OrchestrationV2ProviderFailure,
    pub retry: OrchestrationV2ProviderRetry,
    pub status: OrchestrationV2TurnItemStatus,
    pub started_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

pub fn make_provider_retry_turn_item(
    input: ProviderRetryTurnItemInput<'_>,
) -> OrchestrationV2TurnItem {
    let completed = input.status != OrchestrationV2TurnItemStatus::Running;
    let title = match input.status {
        OrchestrationV2TurnItemStatus::Completed => "Provider recovered",
        OrchestrationV2TurnItemStatus::Failed
            if input.failure.class == OrchestrationV2ProviderFailureClass::UsageLimit =>
        {
            "Usage limit reached"
        }
        OrchestrationV2TurnItemStatus::Failed => "Provider error",
        OrchestrationV2TurnItemStatus::Interrupted | OrchestrationV2TurnItemStatus::Cancelled => {
            "Provider retry stopped"
        }
        _ => "Provider retry",
    };
    error_item(
        input.driver,
        input.thread_id,
        input.run_id,
        input.node_id,
        input.provider_thread_id,
        input.provider_turn_id,
        input.item_ordinal,
        input.status,
        title,
        input.started_at,
        completed.then_some(input.updated_at),
        input.updated_at,
        input.failure,
        Some(input.retry),
    )
}

#[allow(clippy::too_many_arguments)]
fn error_item(
    driver: &ProviderDriverKind,
    thread_id: ThreadId,
    run_id: Option<RunId>,
    node_id: Option<NodeId>,
    provider_thread_id: ProviderThreadId,
    provider_turn_id: ProviderTurnId,
    ordinal: NonNegativeInt,
    status: OrchestrationV2TurnItemStatus,
    title: &str,
    started_at: UtcDateTime,
    completed_at: Option<UtcDateTime>,
    updated_at: UtcDateTime,
    failure: OrchestrationV2ProviderFailure,
    retry: Option<OrchestrationV2ProviderRetry>,
) -> OrchestrationV2TurnItem {
    OrchestrationV2TurnItem::Error {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id(driver, &provider_turn_id),
        thread_id,
        run_id,
        node_id,
        provider_thread_id: Some(provider_thread_id),
        provider_turn_id: Some(provider_turn_id),
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status,
        title: Some(title.to_owned()),
        started_at: Some(started_at),
        completed_at,
        updated_at,
        failure,
        retry,
    }
}

fn turn_item_id(driver: &ProviderDriverKind, provider_turn_id: &ProviderTurnId) -> TurnItemId {
    derive::turn_item_from_provider_item(driver, &format!("terminal-failure:{provider_turn_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_of(failure: &OrchestrationV2ProviderFailure) -> &str {
        failure.message.as_str()
    }

    #[test]
    fn redacts_credentials_and_url_secrets() {
        let failure = make_provider_failure(ProviderFailureInput {
            cause: None,
            message: Some(
                "request failed: Authorization: Bearer bearer-secret https://user:pass@example.test/path?access_token=url-secret#fragment {\"token\":\"json-secret\"} api_key=key-secret sk-abcdefghijklmnop",
            ),
            code: Some(Some("provider_rejected")),
            class: Some(OrchestrationV2ProviderFailureClass::ProviderError),
            retryable: None,
            reset_at: None,
        });
        assert_eq!(
            failure.class,
            OrchestrationV2ProviderFailureClass::ProviderError
        );
        assert_eq!(
            failure.code.as_ref().map(|code| code.as_str()),
            Some("provider_rejected")
        );
        let message = message_of(&failure);
        assert!(message.contains("[REDACTED]"));
        assert!(message.contains("https://example.test/path"));
        for secret in [
            "bearer-secret",
            "user:pass",
            "url-secret",
            "json-secret",
            "key-secret",
            "sk-abcdefghijklmnop",
        ] {
            assert!(!message.contains(secret), "{message}");
        }
    }

    #[test]
    fn redacted_urls_are_normalized_like_upstream_new_url() {
        // Expected values are upstream's `new URL(...)` with username, password,
        // search, and hash cleared, then `toString()`.
        assert_eq!(
            redact_provider_failure_text("https://example.test?token=x"),
            "https://example.test/"
        );
        assert_eq!(
            redact_provider_failure_text("https://Example.test:443/a"),
            "https://example.test/a"
        );
        assert_eq!(
            redact_provider_failure_text("(https://user:pass@example.test/p?q=1#f)"),
            "(https://example.test/p)"
        );
    }

    #[test]
    fn explicit_null_code_falls_through_to_the_cause_code() {
        let cause =
            ProviderFailureCause::tagged("ProviderAdapterTurnStartError").with_code("cause_code");
        let code_of = |code: Option<Option<&str>>| {
            make_provider_failure(ProviderFailureInput {
                cause: Some(&cause),
                message: None,
                code,
                class: None,
                retryable: None,
                reset_at: None,
            })
            .code
            .map(|code| code.as_str().to_owned())
        };

        assert_eq!(code_of(Some(None)).as_deref(), Some("cause_code"));
        assert_eq!(code_of(None).as_deref(), Some("cause_code"));
        assert_eq!(code_of(Some(Some("own_code"))).as_deref(), Some("own_code"));
    }

    #[test]
    fn redacts_compound_credential_keys() {
        let message = redact_provider_failure_text(
            "request failed refresh_token=RT-abc123 client_secret=CS-xyz id_token: IT-1 {\"session_token\": \"ST-2\"} input_tokens=5",
        );
        for secret in ["RT-abc123", "CS-xyz", "IT-1", "ST-2"] {
            assert!(!message.contains(secret), "{message}");
        }
        // Keys that only contain `token` mid-word are not credentials.
        assert!(message.contains("input_tokens=5"), "{message}");
    }

    #[test]
    fn unparseable_urls_are_fully_redacted() {
        // A malformed authority would keep the credentials if it passed through.
        assert_eq!(
            redact_provider_failure_text("https://user:pass/x@host/path"),
            "[REDACTED_URL]"
        );
        assert_eq!(
            redact_provider_failure_text("see https://[bad now"),
            "see [REDACTED_URL] now"
        );
    }

    #[test]
    fn replaces_unsafe_control_characters_without_stripping_whitespace() {
        let failure = make_provider_failure(ProviderFailureInput {
            cause: None,
            message: Some("before\u{0000}\u{0007}\t\nafter\u{007f}"),
            code: None,
            class: None,
            retryable: None,
            reset_at: None,
        });
        assert_eq!(message_of(&failure), "before  \t\nafter");
    }

    #[test]
    fn bounds_provider_controlled_failure_strings() {
        let failure = make_provider_failure(ProviderFailureInput {
            cause: None,
            message: Some(&"m".repeat(MAX_PROVIDER_FAILURE_MESSAGE_LENGTH + 500)),
            code: Some(Some(&"c".repeat(MAX_PROVIDER_FAILURE_CODE_LENGTH + 50))),
            class: None,
            retryable: None,
            reset_at: None,
        });
        // The cut also respects the 4096-byte cap of the contracts newtype, and
        // the 3-byte ellipsis costs two of those bytes, so the UTF-16 length can
        // land below the cap rather than on it.
        assert!(utf16_len(message_of(&failure)) <= MAX_PROVIDER_FAILURE_MESSAGE_LENGTH);
        assert!(message_of(&failure).len() <= MAX_PROVIDER_FAILURE_MESSAGE_LENGTH);
        let code = failure.code.as_ref().expect("code kept").as_str();
        assert!(utf16_len(code) <= MAX_PROVIDER_FAILURE_CODE_LENGTH);
        assert!(code.len() <= MAX_PROVIDER_FAILURE_CODE_LENGTH);
        assert!(message_of(&failure).ends_with('…'));
        assert!(failure.code.as_ref().unwrap().as_str().ends_with('…'));
    }

    #[test]
    fn does_not_split_a_surrogate_pair_at_the_truncation_boundary() {
        let message = format!(
            "{}🚀tail",
            "a".repeat(MAX_PROVIDER_FAILURE_MESSAGE_LENGTH - 2)
        );
        let failure = make_provider_failure(ProviderFailureInput {
            cause: None,
            message: Some(&message),
            code: None,
            class: None,
            retryable: None,
            reset_at: None,
        });
        let text = message_of(&failure);
        assert!(text.ends_with('…'));
        assert!(!text.trim_end_matches('…').ends_with(|ch: char| {
            let mut units = [0u16; 2];
            ch.encode_utf16(&mut units);
            (0xD800..=0xDBFF).contains(&units[0])
        }));
        assert!(utf16_len(text) <= MAX_PROVIDER_FAILURE_MESSAGE_LENGTH);
        assert!(!text.contains('🚀'));
    }

    #[test]
    fn does_not_expose_arbitrary_cause_messages() {
        for cause in [
            ProviderFailureCause::tagged("Error").with_message("Session expired. Sign in again."),
            ProviderFailureCause::Unreadable,
            ProviderFailureCause::tagged("Error"),
        ] {
            assert_eq!(
                message_of(&make_provider_failure(ProviderFailureInput {
                    cause: Some(&cause),
                    message: None,
                    code: None,
                    class: None,
                    retryable: None,
                    reset_at: None,
                })),
                "Provider turn failed."
            );
            assert_eq!(
                message_of(&make_provider_failure(ProviderFailureInput {
                    cause: Some(&cause),
                    message: Some("Provider connection closed."),
                    code: None,
                    class: None,
                    retryable: None,
                    reset_at: None,
                })),
                "Provider connection closed."
            );
        }
    }

    #[test]
    fn preserves_actionable_handoff_errors_wrapped_by_turn_startup() {
        let cause = ProviderFailureCause::tagged("ProviderAdapterTurnStartError")
            .with_cause(ProviderFailureCause::tagged("ContextHandoffBudgetError"));
        assert_eq!(
            message_of(&make_provider_failure(ProviderFailureInput {
                cause: Some(&cause),
                message: None,
                code: None,
                class: None,
                retryable: None,
                reset_at: None,
            })),
            HANDOFF_BUDGET_MESSAGE
        );
    }

    #[test]
    fn does_not_expose_defect_text_nested_inside_a_known_category() {
        let nested = format!(
            "Session rejected: Bearer nested-secret https://user:pass@example.test/path?token=secret {}",
            "x".repeat(5000)
        );
        let cause = ProviderFailureCause::tagged("ProviderAdapterEventStreamError")
            .with_cause(ProviderFailureCause::tagged("Error").with_message(nested));
        let failure = make_provider_failure(ProviderFailureInput {
            cause: Some(&cause),
            message: None,
            code: None,
            class: None,
            retryable: None,
            reset_at: None,
        });
        let message = message_of(&failure);
        assert!(message.contains("provider event stream closed unexpectedly"));
        assert!(!message.contains("Session rejected:"));
        assert!(!message.contains("nested-secret"));
        assert!(!message.contains("user:pass"));
        assert!(!message.contains("token=secret"));
        assert!(utf16_len(message) < MAX_PROVIDER_FAILURE_MESSAGE_LENGTH);
    }

    #[test]
    fn handles_cyclic_causes() {
        let mut cyclic = ProviderFailureCause::tagged("Error")
            .with_message("Provider disconnected. Retry the turn.");
        cyclic = cyclic.with_cause(ProviderFailureCause::tagged("placeholder"));
        // Close the cycle after the first node exists by rebuilding with a shared structure
        // the walker caps at 16 and detects via pointer identity on the chain we can form
        // with a leaked box. The public API does not hand out interior pointers, so the
        // depth cap is what stops a hostile chain; a 20-deep chain still returns the default.
        let mut cause = ProviderFailureCause::tagged("Error")
            .with_message("Provider disconnected. Retry the turn.");
        for _ in 0..20 {
            cause = ProviderFailureCause::tagged("Error")
                .with_message("Provider disconnected. Retry the turn.")
                .with_cause(cause);
        }
        assert_eq!(
            message_of(&make_provider_failure(ProviderFailureInput {
                cause: Some(&cause),
                message: None,
                code: None,
                class: None,
                retryable: None,
                reset_at: None,
            })),
            "Provider turn failed."
        );
        let _ = cyclic;
        assert_eq!(
            message_of(&make_provider_failure(ProviderFailureInput {
                cause: Some(&ProviderFailureCause::Unreadable),
                message: None,
                code: None,
                class: None,
                retryable: None,
                reset_at: None,
            })),
            "Provider turn failed."
        );
    }

    #[test]
    fn does_not_serialize_arbitrary_provider_causes() {
        let cause = ProviderFailureCause::from_json(&serde_json::json!({
            "payload": { "authorization": "Bearer nested-secret" },
            "stack": "private provider stack",
        }));
        let failure = make_provider_failure(ProviderFailureInput {
            cause: Some(&cause),
            message: None,
            code: None,
            class: Some(OrchestrationV2ProviderFailureClass::TransportError),
            retryable: None,
            reset_at: None,
        });
        assert_eq!(
            failure.class,
            OrchestrationV2ProviderFailureClass::TransportError
        );
        assert_eq!(message_of(&failure), "Provider turn failed.");
        assert!(failure.code.is_none());
        assert!(failure.retryable.is_none());
        assert!(failure.reset_at.is_none());
        let encoded = serde_json::to_value(&failure).unwrap();
        assert!(encoded.get("payload").is_none());
        assert!(encoded.get("stack").is_none());
    }

    #[test]
    fn keys_terminal_failure_items_by_provider_turn() {
        let driver = ProviderDriverKind::new("codex").unwrap();
        let thread_id = ThreadId::new("thread:provider-failure-id").unwrap();
        let run_id = RunId::new("run:provider-failure-id").unwrap();
        let node_id = NodeId::new("node:provider-failure-id").unwrap();
        let provider_thread_id =
            ProviderThreadId::new("provider-thread:provider-failure-id").unwrap();
        let failure = make_provider_failure(ProviderFailureInput {
            cause: None,
            message: Some("Provider failed"),
            code: None,
            class: None,
            retryable: None,
            reset_at: None,
        });
        let occurred_at = "2026-06-22T12:00:00.000Z".parse::<UtcDateTime>().unwrap();
        let first = ProviderTurnId::new("provider-turn:provider-failure-id:first").unwrap();
        let second = ProviderTurnId::new("provider-turn:provider-failure-id:second").unwrap();
        let first_attempt = make_provider_failure_turn_item(ProviderFailureTurnItemInput {
            driver: &driver,
            thread_id: thread_id.clone(),
            run_id: Some(run_id.clone()),
            node_id: Some(node_id.clone()),
            provider_thread_id: provider_thread_id.clone(),
            provider_turn_id: first.clone(),
            item_ordinal: 101,
            failure: failure.clone(),
            retry: None,
            retry_started_at: None,
            occurred_at,
        });
        let retried = make_provider_failure_turn_item(ProviderFailureTurnItemInput {
            driver: &driver,
            thread_id: thread_id.clone(),
            run_id: Some(run_id),
            node_id: Some(node_id),
            provider_thread_id: provider_thread_id.clone(),
            provider_turn_id: second,
            item_ordinal: 101,
            failure: failure.clone(),
            retry: None,
            retry_started_at: None,
            occurred_at,
        });
        let fallback = make_provider_failure_turn_item(ProviderFailureTurnItemInput {
            driver: &driver,
            thread_id,
            run_id: None,
            node_id: None,
            provider_thread_id,
            provider_turn_id: first,
            item_ordinal: 101,
            failure,
            retry: None,
            retry_started_at: None,
            occurred_at,
        });
        let id = |item: &OrchestrationV2TurnItem| match item {
            OrchestrationV2TurnItem::Error { id, ordinal, .. } => (id.clone(), *ordinal),
            _ => panic!("error item"),
        };
        assert_ne!(id(&first_attempt).0, id(&retried).0);
        assert_eq!(id(&first_attempt).0, id(&fallback).0);
        assert_eq!(id(&first_attempt).1, 101);
    }
}
