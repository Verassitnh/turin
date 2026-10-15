//! Cursor permission outcomes.
//!
//! `allow_always` calls Cursor's `persistAllowlistEntry` and appends to the
//! global `~/.cursor/cli-config.json` `permissions.allow`. A T3 "allow for
//! this thread" decision therefore stays on the host and is answered with
//! `allow_once`. `acceptAlways` is the labelled option that does write the
//! global allowlist. The process is never launched with `--force` or `--yolo`.

use serde_json::{Value, json};
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequestKind;
use turin_contracts::provider_policy::{
    ProviderApprovalDecision, ProviderApprovalOption, ProviderRequestKind,
};

pub const ALWAYS_ALLOW_LABEL: &str = "Always allow (saved to Cursor config)";
pub const ALLOW_FOR_THREAD_LABEL: &str = "Allow for this thread";
pub const ASK_QUESTION_SKIP_OPTION: &str = "__ask_question_skip__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_kind: Option<String>,
    pub options: Vec<PermissionOption>,
    pub raw: Value,
}

pub fn parse_permission_request(value: &Value) -> PermissionRequest {
    let tool = value.get("toolCall");
    PermissionRequest {
        session_id: value
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                tool.and_then(|tool| tool.get("title"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
        tool_call_id: tool
            .and_then(|tool| tool.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_kind: tool
            .and_then(|tool| tool.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        options: value
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        Some(PermissionOption {
                            option_id: option.get("optionId")?.as_str()?.to_owned(),
                            name: option
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            kind: option
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        raw: value.clone(),
    }
}

pub fn request_kind(request: &PermissionRequest) -> OrchestrationV2RuntimeRequestKind {
    match request.tool_kind.as_deref() {
        Some("read") => OrchestrationV2RuntimeRequestKind::FileRead,
        Some("edit" | "delete" | "move") => OrchestrationV2RuntimeRequestKind::FileChange,
        _ => OrchestrationV2RuntimeRequestKind::Command,
    }
}

/// The text an approval card shows: the command in `rawInput` when there is one,
/// else the tool title, else the session. Upstream `runtimeModel.ts` detail chain.
pub fn card_detail(request: &PermissionRequest) -> String {
    let command = request
        .raw
        .get("toolCall")
        .and_then(|tool| tool.get("rawInput"))
        .and_then(|input| {
            ["command", "cmd"]
                .iter()
                .find_map(|key| input.get(*key).and_then(Value::as_str))
        })
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    match command.or(title) {
        Some(text) => text.to_owned(),
        None => format!(
            "Session {}",
            request.session_id.as_deref().unwrap_or("unknown")
        ),
    }
}

/// The request kind shown on an approval turn item.
pub fn provider_request_kind(request: &PermissionRequest) -> ProviderRequestKind {
    match request.tool_kind.as_deref() {
        Some("read") => ProviderRequestKind::FileRead,
        Some("edit" | "delete" | "move") => ProviderRequestKind::FileChange,
        Some("execute") => ProviderRequestKind::Command,
        _ => ProviderRequestKind::Permission,
    }
}

pub fn approval_options(request: &PermissionRequest) -> Vec<ProviderApprovalOption> {
    let mut options = Vec::new();
    if let Some(option) = option_for_kind(request, "allow_once") {
        options.push(card_option(
            ProviderApprovalDecision::Accept,
            if option.name.is_empty() {
                "Allow once"
            } else {
                &option.name
            },
        ));
    }
    options.push(card_option(
        ProviderApprovalDecision::AcceptForSession,
        ALLOW_FOR_THREAD_LABEL,
    ));
    if option_for_kind(request, "allow_always").is_some() {
        options.push(card_option(
            ProviderApprovalDecision::AcceptAlways,
            ALWAYS_ALLOW_LABEL,
        ));
    }
    if option_for_kind(request, "reject_once").is_some() {
        options.push(card_option(ProviderApprovalDecision::Decline, "Decline"));
    }
    options.push(card_option(ProviderApprovalDecision::Cancel, "Cancel"));
    options
}

/// Stable identity for the host-side per-thread allowance. Tool call ids are
/// unique per request, so they are not part of the key. A request with neither a
/// title nor `rawInput` has nothing to match on, so it gets no key and always
/// shows its card. Otherwise one "allow for this thread" would cover every later
/// request of the same kind.
pub fn session_grant_key(request: &PermissionRequest) -> Option<String> {
    let raw_input = request
        .raw
        .get("toolCall")
        .and_then(|tool| tool.get("rawInput"))
        .map(Value::to_string);
    if request.title.is_none() && raw_input.is_none() {
        return None;
    }
    Some(format!(
        "{}\n{}\n{}",
        request.tool_kind.as_deref().unwrap_or(""),
        request.title.as_deref().unwrap_or(""),
        raw_input.unwrap_or_default()
    ))
}

/// The CLI's `ask_question` fallback sends one permission per question, with the
/// tool call id `<toolCallId>_q<index>` (docs/protocols/cursor-acp.md).
pub fn is_ask_question_fallback(request: &PermissionRequest) -> bool {
    request
        .options
        .iter()
        .any(|option| option.option_id == ASK_QUESTION_SKIP_OPTION)
        || request
            .tool_call_id
            .as_deref()
            .is_some_and(is_question_call_id)
}

fn is_question_call_id(id: &str) -> bool {
    id.rsplit_once("_q").is_some_and(|(_, index)| {
        !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAnswer {
    Selected { option_id: String },
    Cancelled,
}

pub fn answer_for_decision(
    request: &PermissionRequest,
    decision: ProviderApprovalDecision,
) -> PermissionAnswer {
    let kind = match decision {
        ProviderApprovalDecision::Accept | ProviderApprovalDecision::AcceptForSession => {
            "allow_once"
        }
        ProviderApprovalDecision::AcceptAlways => "allow_always",
        ProviderApprovalDecision::Decline => "reject_once",
        ProviderApprovalDecision::Cancel => {
            return PermissionAnswer::Cancelled;
        }
    };
    match option_for_kind(request, kind) {
        Some(option) => PermissionAnswer::Selected {
            option_id: option.option_id.clone(),
        },
        None => PermissionAnswer::Cancelled,
    }
}

pub fn permission_result(answer: &PermissionAnswer) -> Value {
    match answer {
        PermissionAnswer::Selected { option_id } => {
            json!({"outcome": {"outcome": "selected", "optionId": option_id}})
        }
        PermissionAnswer::Cancelled => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

fn option_for_kind<'a>(request: &'a PermissionRequest, kind: &str) -> Option<&'a PermissionOption> {
    request.options.iter().find(|option| option.kind == kind)
}

fn card_option(decision: ProviderApprovalDecision, label: &str) -> ProviderApprovalOption {
    ProviderApprovalOption {
        decision,
        label: TrimmedNonEmptyString::new(label).expect("approval label"),
        warning: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> PermissionRequest {
        parse_permission_request(&json!({
            "sessionId": "s1",
            "toolCall": {"toolCallId": "call-1", "title": "Run ls", "kind": "execute", "rawInput": {"command": "ls"}},
            "options": [
                {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "always", "name": "Always", "kind": "allow_always"},
                {"optionId": "no", "name": "Reject", "kind": "reject_once"}
            ]
        }))
    }

    #[test]
    fn thread_allowance_stays_allow_once_and_always_is_global() {
        let request = sample();
        assert_eq!(
            answer_for_decision(&request, ProviderApprovalDecision::Accept),
            PermissionAnswer::Selected {
                option_id: "once".to_owned()
            }
        );
        assert_eq!(
            answer_for_decision(&request, ProviderApprovalDecision::AcceptForSession),
            PermissionAnswer::Selected {
                option_id: "once".to_owned()
            }
        );
        assert_eq!(
            answer_for_decision(&request, ProviderApprovalDecision::AcceptAlways),
            PermissionAnswer::Selected {
                option_id: "always".to_owned()
            }
        );
        let labels: Vec<_> = approval_options(&request)
            .iter()
            .map(|option| option.label.as_str().to_owned())
            .collect();
        assert!(labels.iter().any(|label| label == ALWAYS_ALLOW_LABEL));
        assert!(labels.iter().any(|label| label == ALLOW_FOR_THREAD_LABEL));
        assert!(
            !permission_result(&PermissionAnswer::Selected {
                option_id: "once".into()
            })
            .to_string()
            .contains("allow_always")
        );
    }

    #[test]
    fn grant_key_needs_a_title_or_raw_input() {
        let titled = parse_permission_request(&json!({
            "toolCall": {"toolCallId": "c1", "title": "Run ls", "kind": "execute"}
        }));
        assert!(session_grant_key(&titled).is_some());
        let untitled = parse_permission_request(&json!({
            "toolCall": {"toolCallId": "c2", "kind": "execute"}
        }));
        assert_eq!(session_grant_key(&untitled), None);
        let with_input = parse_permission_request(&json!({
            "toolCall": {"toolCallId": "c3", "kind": "execute", "rawInput": {"command": "ls"}}
        }));
        assert_ne!(session_grant_key(&with_input), session_grant_key(&titled));
    }

    #[test]
    fn ask_question_fallback_matches_the_documented_id_suffix_only() {
        let fallback = |id: &str| {
            is_ask_question_fallback(&parse_permission_request(
                &json!({"toolCall": {"toolCallId": id}, "options": []}),
            ))
        };
        assert!(fallback("tool-1_q0"));
        assert!(fallback("tool-1_q12"));
        assert!(!fallback("tool_queue_3"));
        assert!(!fallback("tool-1_q"));
        assert!(!fallback("tool-1_q1x"));
    }

    #[test]
    fn ask_question_fallback_is_recognised() {
        let request = parse_permission_request(&json!({
            "toolCall": {"toolCallId": "tool_q0", "title": "Which?"},
            "options": [
                {"optionId": "a", "name": "A", "kind": "allow_once"},
                {"optionId": "__ask_question_skip__", "name": "Skip", "kind": "reject_once"}
            ]
        }));
        assert!(is_ask_question_fallback(&request));
        assert_eq!(
            answer_for_decision(&request, ProviderApprovalDecision::Decline),
            PermissionAnswer::Selected {
                option_id: ASK_QUESTION_SKIP_OPTION.to_owned()
            }
        );
    }
}
