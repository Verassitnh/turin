//! Hand-typed Codex app-server shapes that the pinned schema does not expose.
//! Also: Port of upstream/apps/server/src/provider/CodexThreadRevert.ts.
//!
//! Upstream decodes these from the raw request channel or assigns them onto a
//! generated schema (`client.ts` `fieldsAssign`, `CodexAdapterV2.ts`,
//! `CodexThreadRevert.ts`). `thread/reverted` is informational: rollback uses
//! the `thread` on the `thread/revert` or `thread/read` response and must not
//! wait for the notification. The `thread/read` branch never emits it.
use crate::{
    client::Client,
    errors::{AppServerError, ProtocolError},
    schema,
};
use serde_json::{Map, Value, json};
use std::{collections::HashSet, sync::OnceLock};

const REPEATED_CURSOR: &str = "Thread history pagination repeated a cursor.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadHistoryMode {
    Legacy,
    Paginated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadHistoryMetadata {
    pub history_mode: Option<ThreadHistoryMode>,
    /// `thread.status.type !== "notLoaded"`. Missing status counts as loaded.
    pub loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTerminal {
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTerminalsPage {
    pub data: Vec<BackgroundTerminal>,
    /// `null` nextCursor is the end of the list. The key is required.
    pub next_cursor: Option<String>,
}

/// `tools.update_plan.enabled` and the MCP startup grace, sent as the `config`
/// request parameter on thread/start, thread/resume and thread/fork. This is
/// not an environment variable.
pub fn thread_runtime_config() -> Value {
    json!({
        "tools.update_plan.enabled": true,
        "mcp_optional_startup_grace_ms": 10000
    })
}

pub fn sandbox_policy(kind: &str) -> Value {
    json!({ "type": kind })
}

/// Legacy `execCommandApproval` / `applyPatchApproval` replies. The decision
/// is wrapped; decline is `{denied:{rejection}}` inside that wrapper.
pub fn legacy_approval_reply(decision: &str) -> Value {
    let decision = match decision {
        "accept" => json!("approved"),
        "acceptForSession" | "acceptAlways" => json!("approved_for_session"),
        "decline" => json!({ "denied": { "rejection": "User declined the request." } }),
        "cancel" => json!("abort"),
        other => Value::String(other.to_owned()),
    };
    json!({ "decision": decision })
}

pub fn is_archived_session_error(message: &str) -> bool {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(r"(?i)\bsession \S+ is archived\b|\bcodex unarchive\b")
                .expect("archived session pattern")
        })
        .is_match(message)
}

/// Version after the first `/` in `initialize.userAgent`, up to the next space.
pub fn user_agent_version(user_agent: &str) -> Option<&str> {
    let rest = user_agent.split_once('/')?.1;
    let version = rest.split_whitespace().next().unwrap_or("");
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Probe returns before skills, models and rate limits only when there is no
/// account and `requiresOpenaiAuth` is set. A logged-in ChatGPT account also
/// reports `requiresOpenaiAuth: true`.
pub fn provider_probe_returns_early(account: Option<&Value>, requires_openai_auth: bool) -> bool {
    let missing = match account {
        None | Some(Value::Null) => true,
        Some(_) => false,
    };
    missing && requires_openai_auth
}

pub fn thread_turns_list_params(thread_id: &str, cursor: Option<&str>, remaining: u64) -> Value {
    json!({
        "threadId": thread_id,
        "cursor": cursor,
        "limit": remaining.min(100),
        "sortDirection": "desc",
        "itemsView": "summary",
    })
}

pub fn decode_thread_history_metadata(
    response: &Value,
) -> Result<ThreadHistoryMetadata, AppServerError> {
    let thread = object_field(response, "thread", "thread/read")?;
    let history_mode = match thread.get("historyMode") {
        None | Some(Value::Null) => None,
        Some(Value::String(mode)) if mode == "legacy" => Some(ThreadHistoryMode::Legacy),
        Some(Value::String(mode)) if mode == "paginated" => Some(ThreadHistoryMode::Paginated),
        Some(_) => {
            return Err(invalid_payload(
                "thread/read",
                "schema validation failed at /thread/historyMode",
            ));
        }
    };
    let loaded = thread
        .get("status")
        .and_then(Value::as_object)
        .and_then(|status| status.get("type"))
        .and_then(Value::as_str)
        != Some("notLoaded");
    Ok(ThreadHistoryMetadata {
        history_mode,
        loaded,
    })
}

pub fn decode_background_terminals_list(
    response: &Value,
) -> Result<BackgroundTerminalsPage, AppServerError> {
    let object = response.as_object().ok_or_else(|| {
        invalid_payload(
            "thread/backgroundTerminals/list",
            "schema validation failed at ",
        )
    })?;
    let data = object
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_payload(
                "thread/backgroundTerminals/list",
                "schema validation failed at /data",
            )
        })?;
    let mut terminals = Vec::with_capacity(data.len());
    for item in data {
        let process_id = item
            .get("processId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                invalid_payload(
                    "thread/backgroundTerminals/list",
                    "schema validation failed at /data/processId",
                )
            })?;
        terminals.push(BackgroundTerminal {
            process_id: process_id.to_owned(),
        });
    }
    let next_cursor = match object.get("nextCursor") {
        Some(Value::Null) => None,
        Some(Value::String(cursor)) => Some(cursor.clone()),
        _ => {
            return Err(invalid_payload(
                "thread/backgroundTerminals/list",
                "schema validation failed at /nextCursor",
            ));
        }
    };
    Ok(BackgroundTerminalsPage {
        data: terminals,
        next_cursor,
    })
}

pub fn decode_background_terminal_terminate(response: &Value) -> Result<bool, AppServerError> {
    response
        .get("terminated")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            invalid_payload(
                "thread/backgroundTerminals/terminate",
                "schema validation failed at /terminated",
            )
        })
}

pub fn validate_turn_start_params(payload: Option<&Value>) -> Result<(), String> {
    let Some(value) = payload else {
        return Err("missing payload".into());
    };
    let Value::Object(map) = value else {
        return schema::validate_type("V2TurnStartParams", value);
    };
    let mut stripped = map.clone();
    let collaboration = stripped.remove("collaborationMode");
    let additional = stripped.remove("additionalContext");
    schema::validate_type("V2TurnStartParams", &Value::Object(stripped))?;
    if let Some(mode) = collaboration {
        schema::validate_type("ClientRequest__CollaborationMode", &mode)
            .map_err(|_| "schema validation failed at /collaborationMode".to_owned())?;
    }
    if let Some(context) = additional {
        let Value::Object(entries) = &context else {
            return Err("schema validation failed at /additionalContext".into());
        };
        for (key, entry) in entries {
            schema::validate_type("V2TurnStartParams__AdditionalContextEntry", entry)
                .map_err(|_| format!("schema validation failed at /additionalContext/{key}"))?;
        }
    }
    Ok(())
}

/// Page newest-first turns and revert before the Nth, or `thread/read` when
/// the thread has no turns to drop. Returns that response. Does not wait for
/// `thread/reverted`.
pub async fn revert_codex_thread(
    client: &Client,
    thread_id: &str,
    num_turns: u64,
) -> Result<Value, AppServerError> {
    let mut remaining = num_turns;
    let mut before_turn_id = None;
    let mut cursor: Option<String> = None;
    let mut visited = HashSet::<Option<String>>::new();
    while remaining > 0 {
        if !visited.insert(cursor.clone()) {
            return Err(AppServerError::Request {
                method: "thread/turns/list".into(),
                request_id: String::new(),
                error: Box::new(ProtocolError::internal_error(REPEATED_CURSOR, None)),
            });
        }
        let params = thread_turns_list_params(thread_id, cursor.as_deref(), remaining);
        let page = client
            .request("thread/turns/list", Some(params))
            .await?
            .unwrap_or(Value::Null);
        let data = page.get("data").and_then(Value::as_array).ok_or_else(|| {
            invalid_payload("thread/turns/list", "schema validation failed at /data")
        })?;
        for turn in data {
            let id = turn.get("id").and_then(Value::as_str).ok_or_else(|| {
                invalid_payload("thread/turns/list", "schema validation failed at /data/id")
            })?;
            before_turn_id = Some(id.to_owned());
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
        cursor = match page.get("nextCursor") {
            Some(Value::String(next)) => Some(next.clone()),
            _ => None,
        };
        if cursor.is_none() {
            break;
        }
    }
    let response = if let Some(before_turn_id) = before_turn_id {
        client
            .request(
                "thread/revert",
                Some(json!({ "threadId": thread_id, "beforeTurnId": before_turn_id })),
            )
            .await?
    } else {
        client
            .request(
                "thread/read",
                Some(json!({ "threadId": thread_id, "includeTurns": false })),
            )
            .await?
    };
    response.ok_or_else(|| invalid_payload("thread/revert", "missing payload"))
}

pub fn response_thread(response: &Value) -> Result<&Value, AppServerError> {
    response
        .get("thread")
        .ok_or_else(|| invalid_payload("thread/revert", "schema validation failed at /thread"))
}

fn object_field<'a>(
    value: &'a Value,
    field: &str,
    method: &str,
) -> Result<&'a Map<String, Value>, AppServerError> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_payload(method, format!("schema validation failed at /{field}")))
}

fn invalid_payload(method: &str, detail: impl Into<String>) -> AppServerError {
    AppServerError::InvalidPayload {
        method: method.into(),
        detail: detail.into(),
    }
}
