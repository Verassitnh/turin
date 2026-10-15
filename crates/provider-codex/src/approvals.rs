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
use turin_codex_app_server::{
    client::Client, errors::AppServerError, extensions::legacy_approval_reply,
};

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
            self.inner.allowances.lock().insert(key);
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
                    broker
                        .inner
                        .waiting
                        .lock()
                        .insert(id, Waiting { sender, allowance });
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
        ("item/permissions/requestApproval", Decision::Approval(choice)) => {
            if matches!(choice, "accept" | "acceptForSession" | "acceptAlways") {
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

fn as_answer(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}

fn persistence(value: &str) -> Option<&'static str> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("session") {
        Some("session")
    } else if ["always", "permanent", "forever", "persistent"]
        .iter()
        .any(|s| lower.contains(s))
    {
        Some("always")
    } else {
        None
    }
}

fn elicitation_response(params: &Value, choice: &str) -> Value {
    if matches!(choice, "decline" | "cancel") {
        return json!({"action":choice});
    }
    if params.get("mode").and_then(Value::as_str) == Some("url") {
        return json!({"action":"decline"});
    }
    let persist = match choice {
        "acceptForSession" => Some("session"),
        "acceptAlways" => Some("always"),
        _ => None,
    };
    let form = params.get("requestedSchema").filter(|v| v.is_object());
    let mut content = serde_json::Map::new();
    if let Some(properties) = form
        .and_then(|f| f.get("properties"))
        .and_then(Value::as_object)
    {
        for (key, field) in properties {
            let choices = field
                .get("oneOf")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.get("const").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    field
                        .get("enum")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect()
                });
            let selected = choices.into_iter().find(|value| match persist {
                Some(want) => persistence(value) == Some(want),
                None => {
                    persistence(value).is_none()
                        && ["once", "accept", "approve", "allow"]
                            .iter()
                            .any(|s| value.to_ascii_lowercase().contains(s))
                }
            });
            if let Some(value) = selected {
                content.insert(key.clone(), json!(value));
            } else if field.get("type").and_then(Value::as_str) == Some("boolean")
                && (persistence(key).is_some() || key.eq_ignore_ascii_case("persist"))
            {
                content.insert(key.clone(), json!(choice == "acceptAlways"));
            } else if let Some(default) = field.get("default").filter(|v| !v.is_null()) {
                content.insert(key.clone(), default.clone());
            }
        }
    }
    if form
        .and_then(|f| f.get("required"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|key| !content.contains_key(key))
    {
        return json!({"action":"decline"});
    }
    let mut response = json!({"action":"accept"});
    if let Some(persist) = persist {
        response["_meta"] = json!({"persist":persist});
    }
    if form.is_some() {
        response["content"] = Value::Object(content);
    }
    response
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
}
