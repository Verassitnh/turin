//! Port of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! (server-request decisions and Codex response shapes).
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot};
use turin_contracts::provider_policy::ProviderApprovalDecision;

use crate::codex_mcp_elicitation::to_mcp_elicitation_response;
use turin_codex_app_server::{
    client::Client, errors::AppServerError, extensions::legacy_approval_reply,
};
use turin_contracts::base_schemas::js_trim;

#[derive(Debug, Clone)]
pub struct PendingRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone)]
pub enum Decision {
    Approval(&'static str),
    Answers(Value),
    /// Answers the request with a protocol error (`toProtocolError`).
    Reject(String),
}

#[derive(Default)]
struct BrokerInner {
    next_id: AtomicU64,
    waiting: Mutex<HashMap<u64, Waiting>>,
    allowances: Mutex<HashSet<(String, String)>>,
    runtime_modes: Mutex<HashMap<String, String>>,
}

struct Waiting {
    sender: oneshot::Sender<Decision>,
    allowance: Option<(String, String)>,
    /// The thread's runtime mode when the request arrived. A session allowance is only
    /// granted while that mode still stands.
    mode: Option<String>,
}

fn allowance_key(method: &str, params: &Value) -> Option<(String, String)> {
    let kind = match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => "command",
        "item/fileChange/requestApproval" | "applyPatchApproval" => "file-change",
        _ => return None,
    };
    let thread = params
        .get("threadId")
        .or_else(|| params.get("conversationId"))?
        .as_str()?;
    Some((thread.into(), kind.into()))
}

#[derive(Clone)]
pub struct RequestBroker {
    inner: Arc<BrokerInner>,
    sender: mpsc::UnboundedSender<PendingRequest>,
}

impl RequestBroker {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<PendingRequest>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                inner: Arc::new(BrokerInner::default()),
                sender,
            },
            receiver,
        )
    }

    pub fn reply(&self, id: u64, decision: Decision) -> bool {
        let Some(waiting) = self.inner.waiting.lock().remove(&id) else {
            return false;
        };
        if matches!(
            decision,
            Decision::Approval("acceptForSession" | "acceptAlways")
        ) && let Some(key) = waiting.allowance
        {
            // A late answer after a mode change or an unload must not re-add what was cleared.
            let still_current =
                self.inner.runtime_modes.lock().get(&key.0) == waiting.mode.as_ref();
            if still_current {
                self.inner.allowances.lock().insert(key);
            }
        }
        waiting.sender.send(decision).is_ok()
    }

    pub fn set_runtime_mode(&self, thread_id: &str, runtime_mode: &str) {
        let mut modes = self.inner.runtime_modes.lock();
        if modes
            .get(thread_id)
            .is_some_and(|previous| previous != runtime_mode)
        {
            self.inner
                .allowances
                .lock()
                .retain(|(thread, _)| thread != thread_id);
        }
        modes.insert(thread_id.into(), runtime_mode.into());
    }

    pub fn unload_thread(&self, thread_id: &str) {
        self.inner
            .allowances
            .lock()
            .retain(|(thread, _)| thread != thread_id);
        self.inner.runtime_modes.lock().remove(thread_id);
    }

    pub fn install(&self, client: &Client) {
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval",
            "item/tool/requestUserInput",
            "mcpServer/elicitation/request",
            "execCommandApproval",
            "applyPatchApproval",
        ] {
            let broker = self.clone();
            let method_name = method.to_owned();
            client.handle_server_request(method, move |params| {
                let broker = broker.clone();
                let method_name = method_name.clone();
                async move {
                    let params = params.unwrap_or(Value::Null);
                    if method_name == "mcpServer/elicitation/request"
                        && elicitation_response(&params, "accept")["action"] != "accept"
                    {
                        return Ok(Some(json!({"action":"decline"})));
                    }
                    let allowance = allowance_key(&method_name, &params);
                    if allowance
                        .as_ref()
                        .is_some_and(|key| broker.inner.allowances.lock().contains(key))
                    {
                        return Ok(Some(response(
                            &method_name,
                            &params,
                            Decision::Approval("acceptForSession"),
                        )));
                    }
                    // Codex can send an approval before turn/started. Holding the
                    // request here does not block notifications or the turn reply.
                    let id = broker.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
                    let (sender, receiver) = oneshot::channel();
                    let mode = allowance.as_ref().and_then(|(thread, _)| {
                        broker.inner.runtime_modes.lock().get(thread).cloned()
                    });
                    broker.inner.waiting.lock().insert(
                        id,
                        Waiting {
                            sender,
                            allowance,
                            mode,
                        },
                    );
                    if broker
                        .sender
                        .send(PendingRequest {
                            id,
                            method: method_name.clone(),
                            params: params.clone(),
                        })
                        .is_err()
                    {
                        broker.inner.waiting.lock().remove(&id);
                        return Err(AppServerError::Cancelled);
                    }
                    let decision = receiver.await.map_err(|_| AppServerError::Cancelled)?;
                    if let Decision::Reject(detail) = decision {
                        return Err(AppServerError::InvalidPayload {
                            method: method_name,
                            detail,
                        });
                    }
                    Ok(Some(response(&method_name, &params, decision)))
                }
            });
        }
    }
}

pub fn response(method: &str, params: &Value, decision: Decision) -> Value {
    match (method, decision) {
        ("item/tool/requestUserInput", Decision::Answers(answers)) => {
            let allowed: Vec<&str> = params
                .get("questions")
                .and_then(Value::as_array)
                .into_iter()
                .flat_map(|questions| questions.iter())
                .filter_map(|q| q.get("id").and_then(Value::as_str))
                .collect();
            let mut mapped = serde_json::Map::new();
            for id in allowed {
                if let Some(value) = answers.get(id) {
                    let values = match value {
                        Value::Array(items) => items.iter().map(as_answer).collect(),
                        Value::Null => vec![],
                        _ => vec![as_answer(value)],
                    };
                    mapped.insert(id.into(), json!({"answers": values}));
                }
            }
            json!({"answers":mapped})
        }
        // `permissionsResponseFromDecision`: only accept and acceptForSession grant;
        // anything else, including acceptAlways, grants nothing.
        ("item/permissions/requestApproval", Decision::Approval(choice)) => {
            if matches!(choice, "accept" | "acceptForSession") {
                json!({"permissions":params.get("permissions").cloned().unwrap_or_else(||json!({})),"scope":if choice == "accept" {"turn"} else {"session"}})
            } else {
                json!({"permissions":{},"scope":"turn"})
            }
        }
        ("execCommandApproval" | "applyPatchApproval", Decision::Approval(choice)) => {
            legacy_approval_reply(choice)
        }
        ("mcpServer/elicitation/request", Decision::Approval(choice)) => {
            elicitation_response(params, choice)
        }
        (_, Decision::Approval(choice)) => {
            json!({"decision":if choice == "acceptAlways" {"acceptForSession"} else {choice}})
        }
        _ => json!({}),
    }
}

/// `codexFileChangeApprovalPrompt`: describes the approval's scope even when Codex omits
/// or blanks the optional reason. A reason wins; then up to 20 changed paths, sorted;
/// then the grant root.
pub fn codex_file_change_approval_prompt(params: &Value) -> Option<String> {
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(js_trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    if let Some(reason) = text("reason") {
        return Some(reason);
    }
    if let Some(changes) = params.get("fileChanges").and_then(Value::as_object)
        && !changes.is_empty()
    {
        let mut entries: Vec<(&String, &Value)> = changes.iter().collect();
        // `localeCompare`: case-insensitive first, then by code point.
        entries.sort_by(|(left, _), (right, _)| {
            left.to_lowercase()
                .cmp(&right.to_lowercase())
                .then_with(|| left.cmp(right))
        });
        let described: Vec<String> = entries
            .iter()
            .take(20)
            .map(|(path, change)| {
                let kind = change
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let moved = (kind == "update")
                    .then(|| change.get("move_path").and_then(Value::as_str))
                    .flatten()
                    .filter(|moved| !moved.is_empty());
                match moved {
                    Some(moved) => format!("{kind} {path} -> {moved}"),
                    None => format!("{kind} {path}"),
                }
            })
            .collect();
        let remaining = entries.len() - described.len();
        return Some(if remaining > 0 {
            format!("{}\n+{remaining} more", described.join("\n"))
        } else {
            described.join("\n")
        });
    }
    text("grantRoot")
}

/// Maps the broker's choice to the MCP elicitation wire response.
fn elicitation_response(params: &Value, choice: &str) -> Value {
    let decision = match choice {
        "accept" => ProviderApprovalDecision::Accept,
        "acceptForSession" => ProviderApprovalDecision::AcceptForSession,
        "acceptAlways" => ProviderApprovalDecision::AcceptAlways,
        "decline" => ProviderApprovalDecision::Decline,
        "cancel" => ProviderApprovalDecision::Cancel,
        _ => return json!({"action":"decline"}),
    };
    to_mcp_elicitation_response(params, decision)
}

fn as_answer(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_shapes() {
        assert_eq!(
            response(
                "item/commandExecution/requestApproval",
                &json!({}),
                Decision::Approval("acceptAlways")
            ),
            json!({"decision":"acceptForSession"})
        );
        assert_eq!(
            response(
                "execCommandApproval",
                &json!({}),
                Decision::Approval("decline")
            ),
            json!({"decision":{"denied":{"rejection":"User declined the request."}}})
        );
        assert_eq!(
            response(
                "item/permissions/requestApproval",
                &json!({"permissions":{"network":{}}}),
                Decision::Approval("decline")
            ),
            json!({"permissions":{},"scope":"turn"})
        );
        assert_eq!(
            response(
                "item/tool/requestUserInput",
                &json!({"questions":[{"id":"a"}]}),
                Decision::Answers(json!({"a":["x","y"],"unknown":"z"}))
            ),
            json!({"answers":{"a":{"answers":["x","y"]}}})
        );
        assert_eq!(
            elicitation_response(&json!({"mode":"url"}), "accept"),
            json!({"action":"decline"})
        );
        assert_eq!(
            elicitation_response(
                &json!({"mode":"form","requestedSchema":{"properties":{"allow":{"enum":["once","always"]}},"required":["allow"]}}),
                "acceptAlways"
            ),
            json!({"action":"accept","_meta":{"persist":"always"},"content":{"allow":"always"}})
        );
    }

    #[test]
    fn permissions_grant_only_on_accept_and_accept_for_session() {
        let params = json!({"permissions": {"fileSystem": {"write": ["/w"]}}});
        let respond = |choice| {
            response(
                "item/permissions/requestApproval",
                &params,
                Decision::Approval(choice),
            )
        };
        assert_eq!(
            respond("accept"),
            json!({"permissions": {"fileSystem": {"write": ["/w"]}}, "scope": "turn"})
        );
        assert_eq!(
            respond("acceptForSession"),
            json!({"permissions": {"fileSystem": {"write": ["/w"]}}, "scope": "session"})
        );
        // Fail closed: acceptAlways grants nothing, as upstream's permissionsResponseFromDecision.
        assert_eq!(
            respond("acceptAlways"),
            json!({"permissions": {}, "scope": "turn"})
        );
        assert_eq!(
            respond("cancel"),
            json!({"permissions": {}, "scope": "turn"})
        );
    }

    /// `CodexAdapterV2.test.ts` "CodexAdapterV2 file change approvals".
    fn changes() -> Value {
        json!({
            "/tmp/removed.md": {"type": "delete", "content": "gone"},
            "/tmp/added.ts": {"type": "add", "content": "export {};"},
            "/tmp/moved.ts": {"type": "update", "unified_diff": "@@", "move_path": "/tmp/renamed.ts"}
        })
    }

    #[test]
    fn uses_nonblank_reasons_before_sorted_file_operations_and_renamed_paths() {
        assert_eq!(
            codex_file_change_approval_prompt(
                &json!({"reason": "  Update configuration. ", "fileChanges": changes()})
            )
            .as_deref(),
            Some("Update configuration.")
        );
        assert_eq!(
            codex_file_change_approval_prompt(&json!({"reason": " ", "fileChanges": changes()}))
                .as_deref(),
            Some(
                "add /tmp/added.ts\nupdate /tmp/moved.ts -> /tmp/renamed.ts\ndelete /tmp/removed.md"
            )
        );
    }

    #[test]
    fn falls_back_to_a_nonblank_grant_root_and_omits_empty_details() {
        assert_eq!(
            codex_file_change_approval_prompt(&json!({"reason": " ", "grantRoot": " /workspace "}))
                .as_deref(),
            Some("/workspace")
        );
        assert_eq!(
            codex_file_change_approval_prompt(
                &json!({"fileChanges": {}, "grantRoot": "/workspace"})
            )
            .as_deref(),
            Some("/workspace")
        );
        assert_eq!(
            codex_file_change_approval_prompt(
                &json!({"reason": " ", "grantRoot": " ", "fileChanges": {}})
            ),
            None
        );
    }

    #[test]
    fn bounds_large_patch_descriptions_without_losing_the_remaining_count() {
        let changes: serde_json::Map<String, Value> = (0..25)
            .map(|index| {
                (
                    format!("/tmp/file-{index:02}.ts"),
                    json!({"type": "add", "content": ""}),
                )
            })
            .collect();
        let detail = codex_file_change_approval_prompt(&json!({ "fileChanges": changes })).unwrap();
        assert_eq!(detail.split('\n').count(), 21);
        assert!(detail.starts_with("add /tmp/file-00.ts"));
        assert!(detail.ends_with("+5 more"));
        assert!(!detail.contains("file-20.ts"));
    }

    #[tokio::test]
    async fn a_late_session_answer_does_not_re_add_a_cleared_allowance() {
        let (broker, mut requests) = RequestBroker::new();
        broker.set_runtime_mode("thread-1", "full-access");
        let key = ("thread-1".to_owned(), "command".to_owned());
        let (sender, _receiver) = oneshot::channel();
        broker.inner.waiting.lock().insert(
            1,
            Waiting {
                sender,
                allowance: Some(key.clone()),
                mode: Some("full-access".to_owned()),
            },
        );
        // The mode changes while the question is open.
        broker.set_runtime_mode("thread-1", "auto");
        assert!(broker.reply(1, Decision::Approval("acceptForSession")));
        assert!(!broker.inner.allowances.lock().contains(&key));

        // The same answer in the same mode stands.
        let (sender, _receiver) = oneshot::channel();
        broker.inner.waiting.lock().insert(
            2,
            Waiting {
                sender,
                allowance: Some(key.clone()),
                mode: Some("auto".to_owned()),
            },
        );
        assert!(broker.reply(2, Decision::Approval("acceptForSession")));
        assert!(broker.inner.allowances.lock().contains(&key));
        assert!(requests.try_recv().is_err());
    }
}
