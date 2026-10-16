//! Port of upstream/apps/server/src/orchestration-v2/Adapters/claudeWorkflowProgress.ts.
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use turin_contracts::orchestration_v2::OrchestrationV2SubagentWorkflow;

pub const CLAUDE_WORKFLOW_TASK_TYPE: &str = "local_workflow";
fn text(value: Option<&Value>) -> Option<&str> {
    value?.as_str().map(str::trim).filter(|s| !s.is_empty())
}
fn count(value: Option<&Value>) -> Option<u64> {
    value?.as_u64().filter(|n| *n <= 9_007_199_254_740_991)
}
fn timestamp(value: Option<&Value>) -> Option<u64> {
    count(value).filter(|n| *n <= 8_640_000_000_000_000)
}
fn put(map: &mut Map<String, Value>, target: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(target.into(), value);
    }
}
fn string(value: Option<&Value>) -> Option<Value> {
    text(value).map(|s| json!(s))
}
fn number(value: Option<&Value>) -> Option<Value> {
    count(value).map(|n| json!(n))
}

pub fn has_claude_workflow_snapshot(message: &Value) -> bool {
    message
        .get("workflow_progress")
        .and_then(Value::as_array)
        .is_some_and(|entries| !entries.is_empty())
}
pub fn parse_claude_workflow_run_handles(result: &Value) -> Option<Value> {
    if text(result.get("taskType")) != Some(CLAUDE_WORKFLOW_TASK_TYPE) {
        return None;
    }
    let mut handles = Map::new();
    for (source, target) in [
        ("runId", "runId"),
        ("transcriptDir", "transcriptDir"),
        ("scriptPath", "scriptPath"),
    ] {
        put(&mut handles, target, string(result.get(source)));
    }
    (!handles.is_empty()).then_some(Value::Object(handles))
}
fn phase(entry: &Value) -> Option<Value> {
    Some(json!({"index":count(entry.get("index"))?,"title":text(entry.get("title"))?}))
}
fn agent(entry: &Value) -> Option<Value> {
    let mut value = json!({"index":count(entry.get("index"))?,"label":text(entry.get("label"))?,"state":match text(entry.get("state")) { Some("queued") => "queued", Some("done") => "completed", Some("error") => "failed", _ => "running" }});
    let map = value.as_object_mut()?;
    for (source, target) in [
        ("agentId", "agentId"),
        ("phaseTitle", "phaseTitle"),
        ("model", "model"),
        ("promptPreview", "prompt"),
        ("resultPreview", "result"),
    ] {
        put(map, target, string(entry.get(source)));
    }
    for (source, target) in [
        ("phaseIndex", "phaseIndex"),
        ("attempt", "attempt"),
        ("tokens", "totalTokens"),
        ("toolCalls", "toolCalls"),
        ("durationMs", "durationMs"),
    ] {
        put(map, target, number(entry.get(source)));
    }
    for key in ["queuedAt", "startedAt"] {
        put(map, key, timestamp(entry.get(key)).map(|n| json!(n)));
    }
    Some(value)
}
pub fn merge_claude_workflow_progress(
    previous: Option<&OrchestrationV2SubagentWorkflow>,
    name: Option<&str>,
    run_handles: Option<&Value>,
    message: &Value,
) -> Option<OrchestrationV2SubagentWorkflow> {
    let mut phases = BTreeMap::<u64, Value>::new();
    let mut agents = BTreeMap::<u64, Value>::new();
    if let Some(previous) = previous {
        for phase in &previous.phases {
            let value = serde_json::to_value(phase).ok()?;
            phases.insert(phase.index, value);
        }
        for agent in &previous.agents {
            let value = serde_json::to_value(agent).ok()?;
            agents.insert(agent.index, value);
        }
    }
    let mut snapshot = false;
    if let Some(entries) = message.get("workflow_progress").and_then(Value::as_array) {
        for entry in entries {
            match text(entry.get("type")) {
                Some("workflow_phase") => {
                    if let Some(value) = phase(entry) {
                        phases.insert(value["index"].as_u64()?, value);
                        snapshot = true;
                    }
                }
                Some("workflow_agent") => {
                    if let Some(mut value) = agent(entry) {
                        let index = value["index"].as_u64()?;
                        if let Some(prior) = agents.get(&index) {
                            let prior_attempt =
                                prior.get("attempt").and_then(Value::as_u64).unwrap_or(1);
                            let next_attempt = value
                                .get("attempt")
                                .and_then(Value::as_u64)
                                .unwrap_or(prior_attempt);
                            if next_attempt < prior_attempt {
                                continue;
                            }
                            let restarted = next_attempt > prior_attempt;
                            let old_state = text(prior.get("state"));
                            let new_state = text(value.get("state"));
                            let regressed = !restarted
                                && ((matches!(old_state, Some("completed" | "failed"))
                                    && old_state != new_state)
                                    || old_state == Some("running") && new_state == Some("queued"));
                            let mut merged = prior.as_object()?.clone();
                            if restarted {
                                merged.retain(|key, _| {
                                    matches!(key.as_str(), "phaseIndex" | "phaseTitle")
                                });
                            }
                            if !regressed {
                                merged.extend(value.as_object()?.clone());
                            }
                            value = Value::Object(merged);
                        }
                        agents.insert(index, value);
                        snapshot = true;
                    }
                }
                _ => {}
            }
        }
    }
    if previous.is_none() && name.is_none() && run_handles.is_none() && !snapshot {
        return None;
    }
    let mut output = previous
        .and_then(|previous| serde_json::to_value(previous).ok())
        .unwrap_or_else(|| json!({}));
    let map = output.as_object_mut()?;
    put(
        map,
        "name",
        name.map(|s| json!(s.trim()))
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty())),
    );
    if let Some(handles) = run_handles {
        let mut existing = map
            .get("runHandles")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        existing.extend(handles.as_object()?.clone());
        map.insert("runHandles".into(), Value::Object(existing));
    }
    map.insert(
        "phases".into(),
        Value::Array(phases.into_values().collect()),
    );
    map.insert(
        "agents".into(),
        Value::Array(agents.into_values().collect()),
    );
    let usage = message.get("usage");
    for (source, target) in [
        ("total_tokens", "totalTokens"),
        ("tool_uses", "toolCalls"),
        ("duration_ms", "durationMs"),
    ] {
        put(map, target, number(usage.and_then(|u| u.get(source))));
    }
    serde_json::from_value(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workflow_snapshot_is_monotone_and_usage_survives() {
        let first = json!({"workflow_progress":[{"type":"workflow_phase","index":0,"title":"Plan"},{"type":"workflow_agent","index":0,"label":"Worker","state":"done","attempt":1}],"usage":{"total_tokens":15}});
        let workflow = merge_claude_workflow_progress(None, None, None, &first).unwrap();
        let late = json!({"workflow_progress":[{"type":"workflow_agent","index":0,"label":"Worker","state":"start","attempt":1}],"usage":{"tool_uses":2}});
        let merged = merge_claude_workflow_progress(Some(&workflow), None, None, &late).unwrap();
        assert_eq!(merged.phases.len(), 1);
        assert_eq!(
            serde_json::to_value(&merged).unwrap()["agents"][0]["state"],
            "completed"
        );
        assert_eq!(merged.total_tokens, Some(15));
        assert_eq!(merged.tool_calls, Some(2));
        assert!(
            merge_claude_workflow_progress(None, None, None, &json!({"usage":{"tool_uses":1}}))
                .is_none()
        );
    }
}
