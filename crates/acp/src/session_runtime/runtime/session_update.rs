//! Session update parsing and event emission (upstream `handleSessionUpdate`).

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::runtime_model::{
    AcpParsedSessionEvent, AcpSessionModeState, AcpSessionSetupResponse, AcpToolCallState,
    decide_tool_call_update_emission, merge_tool_call_state, parse_session_mode_state,
    parse_session_update_event, tool_call_progress_length,
};

use super::events::AcpSessionRuntimeEvent;

const MAX_SHOWN_TOOL_CALL_IDS: usize = 256;

#[derive(Debug, Clone, Default)]
pub struct AssistantSegmentState {
    pub next_segment_index: u32,
    pub active_item_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolCallTrackedState {
    state: AcpToolCallState,
    last_emitted_detail_length: Option<usize>,
    skipped_since_emit: usize,
}

/// Insertion-ordered set. Eviction drops the oldest id, matching a JS `Set`.
#[derive(Debug, Clone, Default)]
pub struct ShownToolCallIds {
    order: VecDeque<String>,
    membership: HashSet<String>,
}

impl ShownToolCallIds {
    pub fn contains(&self, id: &str) -> bool {
        self.membership.contains(id)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn insert(&mut self, id: String) {
        if self.membership.insert(id.clone()) {
            self.order.push_back(id);
        }
    }

    pub fn evict_oldest_past(&mut self, max: usize) {
        while self.order.len() > max {
            if let Some(oldest) = self.order.pop_front() {
                self.membership.remove(&oldest);
            }
        }
    }
}

pub struct SessionUpdateContext {
    pub mode_state: Option<AcpSessionModeState>,
    pub config_options: Value,
    pub tool_calls: HashMap<String, ToolCallTrackedState>,
    pub shown_tool_call_ids: ShownToolCallIds,
    pub assistant_segment: AssistantSegmentState,
    pub assistant_item_runtime_id: String,
}

pub fn handle_session_update(
    ctx: &mut SessionUpdateContext,
    events: &broadcast::Sender<AcpSessionRuntimeEvent>,
    notification: &Value,
) {
    if let Some(config_options) = notification
        .get("update")
        .filter(|update| {
            update.get("sessionUpdate").and_then(Value::as_str) == Some("config_option_update")
        })
        .and_then(|update| update.get("configOptions"))
    {
        ctx.config_options = config_options.clone();
        if let Ok(setup) =
            serde_json::from_value::<AcpSessionSetupResponse>(json_setup(config_options))
        {
            ctx.mode_state = parse_session_mode_state(&setup).or(ctx.mode_state.clone());
        }
    }

    let parsed = parse_session_update_event(notification);
    if let Some(mode_id) = &parsed.mode_id
        && let Some(mode_state) = ctx.mode_state.as_mut()
    {
        update_mode_state(mode_state, mode_id);
    }

    for event in parsed.events {
        match event {
            AcpParsedSessionEvent::ToolCallUpdated {
                tool_call,
                raw_payload,
            } => {
                let tool_call_id = tool_call.tool_call_id.clone();
                let (merged, decision, active) = {
                    let tracked = ctx.tool_calls.get(&tool_call_id);
                    let previous = tracked.map(|entry| &entry.state);
                    let next_tool_call = merge_tool_call_state(previous, tool_call.clone());
                    let decision = decide_tool_call_update_emission(
                        &crate::runtime_model::AcpToolCallEmitDecisionInput {
                            previous,
                            next: &next_tool_call,
                            last_emitted_detail_length: tracked
                                .and_then(|entry| entry.last_emitted_detail_length),
                            skipped_since_emit: tracked
                                .map(|entry| entry.skipped_since_emit)
                                .unwrap_or(0),
                        },
                    );
                    let active = tracked.is_some();
                    if matches!(
                        next_tool_call.status,
                        Some(crate::runtime_model::AcpToolCallStatus::Completed)
                            | Some(crate::runtime_model::AcpToolCallStatus::Failed)
                    ) {
                        ctx.tool_calls.remove(&tool_call_id);
                    } else {
                        ctx.tool_calls.insert(
                            tool_call_id.clone(),
                            ToolCallTrackedState {
                                state: next_tool_call.clone(),
                                last_emitted_detail_length: if decision.emit {
                                    Some(tool_call_progress_length(&next_tool_call))
                                } else {
                                    tracked.and_then(|entry| entry.last_emitted_detail_length)
                                },
                                skipped_since_emit: decision.skipped_since_emit,
                            },
                        );
                    }
                    (next_tool_call, decision, active)
                };
                if !decision.emit {
                    continue;
                }
                if !ctx.shown_tool_call_ids.contains(&merged.tool_call_id) {
                    ctx.shown_tool_call_ids.insert(merged.tool_call_id.clone());
                    ctx.shown_tool_call_ids
                        .evict_oldest_past(MAX_SHOWN_TOOL_CALL_IDS);
                    if !active {
                        close_active_assistant_segment(ctx, events);
                    }
                }
                let _ = events.send(AcpSessionRuntimeEvent::Session(
                    AcpParsedSessionEvent::ToolCallUpdated {
                        tool_call: merged,
                        raw_payload,
                    },
                ));
            }
            AcpParsedSessionEvent::ContentDelta {
                item_id: _,
                message_id,
                text,
                raw_payload,
            } => {
                if text.trim().is_empty() && ctx.assistant_segment.active_item_id.is_none() {
                    continue;
                }
                let item_id = Some(ensure_active_assistant_segment(
                    ctx,
                    events,
                    notification
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                ));
                let _ = events.send(AcpSessionRuntimeEvent::Session(
                    AcpParsedSessionEvent::ContentDelta {
                        item_id,
                        message_id,
                        text,
                        raw_payload,
                    },
                ));
            }
            other => {
                let _ = events.send(AcpSessionRuntimeEvent::Session(other));
            }
        }
    }
}

pub fn close_active_assistant_segment(
    ctx: &mut SessionUpdateContext,
    events: &broadcast::Sender<AcpSessionRuntimeEvent>,
) {
    let Some(item_id) = ctx.assistant_segment.active_item_id.take() else {
        return;
    };
    let _ = events.send(AcpSessionRuntimeEvent::Session(
        AcpParsedSessionEvent::AssistantItemCompleted { item_id },
    ));
}

fn ensure_active_assistant_segment(
    ctx: &mut SessionUpdateContext,
    events: &broadcast::Sender<AcpSessionRuntimeEvent>,
    session_id: &str,
) -> String {
    if let Some(item_id) = ctx.assistant_segment.active_item_id.clone() {
        return item_id;
    }
    let item_id = assistant_item_id(
        session_id,
        &ctx.assistant_item_runtime_id,
        ctx.assistant_segment.next_segment_index,
    );
    ctx.assistant_segment.next_segment_index += 1;
    ctx.assistant_segment.active_item_id = Some(item_id.clone());
    let _ = events.send(AcpSessionRuntimeEvent::Session(
        AcpParsedSessionEvent::AssistantItemStarted {
            item_id: item_id.clone(),
        },
    ));
    item_id
}

fn assistant_item_id(session_id: &str, runtime_id: &str, segment_index: u32) -> String {
    format!("assistant:{session_id}:runtime:{runtime_id}:segment:{segment_index}")
}

fn update_mode_state(mode_state: &mut AcpSessionModeState, next_mode_id: &str) {
    let normalized = next_mode_id.trim();
    if normalized.is_empty() {
        return;
    }
    if mode_state
        .available_modes
        .iter()
        .any(|mode| mode.id == normalized)
    {
        mode_state.current_mode_id = normalized.to_owned();
    }
}

fn json_setup(config_options: &Value) -> Value {
    serde_json::json!({ "configOptions": config_options })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evicts_shown_tool_calls_oldest_first() {
        let (events, _rx) = broadcast::channel(8);
        let mut ctx = SessionUpdateContext {
            mode_state: None,
            config_options: json!([]),
            tool_calls: HashMap::new(),
            shown_tool_call_ids: ShownToolCallIds::default(),
            assistant_segment: AssistantSegmentState::default(),
            assistant_item_runtime_id: "runtime".to_owned(),
        };
        for index in 0..=MAX_SHOWN_TOOL_CALL_IDS {
            handle_session_update(
                &mut ctx,
                &events,
                &json!({
                    "sessionId": "session-1",
                    "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": format!("tool-{index}"),
                        "title": "Terminal",
                        "kind": "execute",
                        "status": "pending",
                    },
                }),
            );
        }
        assert_eq!(ctx.shown_tool_call_ids.len(), MAX_SHOWN_TOOL_CALL_IDS);
        assert!(!ctx.shown_tool_call_ids.contains("tool-0"));
        assert!(
            ctx.shown_tool_call_ids
                .contains(&format!("tool-{MAX_SHOWN_TOOL_CALL_IDS}"))
        );
        assert!(ctx.shown_tool_call_ids.contains("tool-1"));
    }
}
