//! Port of upstream/apps/server/src/provider/CodexManagedErrors.ts.
//!
//! Maps a managed-sharing error payload to a user-facing message. Codes are
//! matched by substring in insertion order, so the first match wins, as upstream.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexManagedFailure {
    pub code: &'static str,
    pub message: &'static str,
    pub revoke: bool,
}

/// `legacyFailures` followed by the aliases and `chatpass_v2_*` entries of `failures`.
const FAILURES: &[(&str, &str)] = &[
    (
        "subscription_sharing_v2_user_not_eligible",
        "ChatGPT sharing is unavailable for this account or workspace. Use another provider or check its sharing policy.",
    ),
    (
        "subscription_sharing_usage_limit_exceeded",
        "Your ChatGPT usage limit was reached. Check ChatGPT Usage settings for your available allowance.",
    ),
    (
        "subscription_sharing_usage_unavailable",
        "ChatGPT usage is temporarily unavailable. Try again shortly.",
    ),
    (
        "subscription_sharing_unsupported_capability",
        "Codex used a feature that ChatGPT sharing does not support. Use another provider for this request.",
    ),
    (
        "subscription_sharing_v2_client_not_enabled",
        "This app is not enabled for this ChatGPT connection. Use your existing CLI or another provider.",
    ),
    (
        "subscription_sharing_v2_route_not_supported",
        "ChatGPT does not support this request route.",
    ),
    (
        "subscription_sharing_v2_invalid_user",
        "ChatGPT could not validate this connection. Check the selected account and sharing permissions.",
    ),
    (
        "subscription_sharing_v2_user_unavailable",
        "ChatGPT is temporarily unavailable. Try again shortly.",
    ),
    (
        "subscription_sharing_user_not_eligible",
        "ChatGPT sharing is unavailable for this account or workspace. Use another provider or check its sharing policy.",
    ),
    (
        "subscription_sharing_route_not_supported",
        "ChatGPT does not support this request route.",
    ),
    (
        "subscription_sharing_invalid_user",
        "ChatGPT could not validate this connection. Check the selected account and sharing permissions.",
    ),
    (
        "subscription_sharing_user_unavailable",
        "ChatGPT is temporarily unavailable. Try again shortly.",
    ),
    (
        "chatpass_v2_scope_not_authorized",
        "This ChatGPT grant does not authorize the request. Check the connection's sharing permissions.",
    ),
    (
        "chatpass_v2_invalid_authorization_context",
        "ChatGPT could not authorize this connection. Check the client and sharing permissions.",
    ),
];

const UNSUPPORTED_CAPABILITY: &str = "subscription_sharing_unsupported_capability";

/// `classifyCodexManagedError`. A string payload is matched as-is; any other
/// value is matched against its JSON text.
pub fn classify_codex_managed_error(value: &Value) -> Option<CodexManagedFailure> {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let (code, message) = FAILURES.iter().find(|(code, _)| text.contains(code))?;
    let message = if *code == UNSUPPORTED_CAPABILITY {
        if text.contains("tool 'namespace'") {
            "Codex sent a tool namespace that ChatGPT sharing does not support. Use another provider for this request."
        } else if text.contains("additional_tools") {
            "Codex sent an input item that ChatGPT sharing does not support. Use another provider for this request."
        } else {
            message
        }
    } else {
        message
    };
    Some(CodexManagedFailure {
        code,
        message,
        revoke: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_code_in_a_string_maps_to_its_message() {
        let failure = classify_codex_managed_error(&json!(
            "error: subscription_sharing_usage_limit_exceeded"
        ))
        .expect("known code");
        assert_eq!(failure.code, "subscription_sharing_usage_limit_exceeded");
        assert!(failure.message.starts_with("Your ChatGPT usage limit"));
        assert!(!failure.revoke);
    }

    #[test]
    fn alias_keeps_its_own_code_and_shared_message() {
        let failure = classify_codex_managed_error(&json!({
            "error": "chatpass_v2_scope_not_authorized"
        }))
        .expect("known code");
        assert_eq!(failure.code, "chatpass_v2_scope_not_authorized");
        assert_eq!(
            failure.message,
            "This ChatGPT grant does not authorize the request. Check the connection's sharing permissions."
        );
    }

    #[test]
    fn unsupported_capability_names_the_rejected_item() {
        let namespace = classify_codex_managed_error(&json!(
            "subscription_sharing_unsupported_capability: tool 'namespace'"
        ))
        .expect("known code");
        assert!(namespace.message.starts_with("Codex sent a tool namespace"));

        let items = classify_codex_managed_error(&json!(
            "subscription_sharing_unsupported_capability additional_tools"
        ))
        .expect("known code");
        assert!(items.message.starts_with("Codex sent an input item"));

        let other =
            classify_codex_managed_error(&json!("subscription_sharing_unsupported_capability"))
                .expect("known code");
        assert!(other.message.starts_with("Codex used a feature"));
    }

    #[test]
    fn unknown_payload_is_not_classified() {
        assert_eq!(classify_codex_managed_error(&json!("network reset")), None);
        assert_eq!(classify_codex_managed_error(&Value::Null), None);
    }
}
