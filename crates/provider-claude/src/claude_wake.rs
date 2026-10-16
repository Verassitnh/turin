//! Port of the wake and prompt-echo helpers in upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:
//! `claudeEchoedPromptUuids` (~2737), `claudePromptLifecycle` (~2749),
//! `isClaudeResultForOtherTurn` (~2763), `isClaudePromptEchoGatedFrame` (~2707),
//! `claudeFrameToolUseIds` (~2718), and the per-thread wake state of
//! `bufferWakeMessage` (~6338): the buffer, its detail, wake eligibility, replay
//! tombstones, requested continuations, background work reports, and the last
//! turn route.
//!
//! The actor (`claude_provider_adapter.rs`) runs the state machine; this module
//! holds the classification and the state it reads and writes.
use std::collections::{HashMap, HashSet};

use serde_json::Value;
use turin_contracts::{
    ProviderThreadId, ThreadId, orchestration_v2::OrchestrationV2PendingBackgroundTask,
};
use turin_provider_core::notification::BackgroundWorkReport;

/// Upstream `PromptEchoMode`, per live query. `Unknown` until the CLI shows
/// whether it echoes prompt uuids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptEchoMode {
    Unknown,
    /// Its lifecycle frames name the prompt, but no echo has come yet.
    Acknowledged,
    /// The echo lands on the turn's first gated frame, so a queued turn's
    /// output can be told from the prompt's.
    Early,
    /// The echo lands on the turn's result only.
    ResultOnly,
}

fn type_of(raw: &Value) -> Option<&str> {
    raw.get("type").and_then(Value::as_str)
}

fn subtype_of(raw: &Value) -> Option<&str> {
    raw.get("subtype").and_then(Value::as_str)
}

fn has_parent(raw: &Value) -> bool {
    raw.get("parent_tool_use_id")
        .is_some_and(|parent| !parent.is_null())
}

/// Upstream `claudeEchoedPromptUuids`: the prompts a frame says its turn consumed.
pub fn echoed_prompt_uuids(raw: &Value) -> Vec<&str> {
    if let Some(uuids) = raw.get("user_message_uuids").and_then(Value::as_array) {
        return uuids.iter().filter_map(Value::as_str).collect();
    }
    raw.get("user_message_uuid")
        .and_then(Value::as_str)
        .into_iter()
        .collect()
}

/// Upstream `claudePromptLifecycle`: the prompt a `command_lifecycle` frame
/// (queued, started, completed) acknowledges, and its state.
pub fn prompt_lifecycle(raw: &Value) -> Option<(&str, &str)> {
    if type_of(raw) != Some("command_lifecycle") {
        return None;
    }
    let uuid = raw.get("command_uuid").and_then(Value::as_str)?;
    let state = raw.get("state").and_then(Value::as_str)?;
    Some((uuid, state))
}

/// Upstream `isClaudeResultForOtherTurn`: a result that answers a turn other
/// than the pending prompt's. An echo names the prompts its turn consumed.
/// Without one, only a process known to echo proves the turn foreign, by its
/// non-human origin.
pub fn is_result_for_other_turn(raw: &Value, prompt_uuid: &str, mode: PromptEchoMode) -> bool {
    let echoed = echoed_prompt_uuids(raw);
    if !echoed.is_empty() {
        return !echoed.contains(&prompt_uuid);
    }
    mode != PromptEchoMode::Unknown
        && raw
            .get("origin")
            .is_some_and(|origin| origin.get("kind").and_then(Value::as_str) != Some("human"))
}

/// Upstream `isClaudePromptEchoGatedFrame`: root turn output whose owner (the
/// offered prompt or a queued wake turn) the echo decides. Subagent frames,
/// lifecycle frames, and anything else pass straight through.
pub fn is_gated_frame(raw: &Value) -> bool {
    match type_of(raw) {
        Some("assistant" | "stream_event" | "user") => !has_parent(raw),
        Some("result") => true,
        _ => false,
    }
}

/// Upstream `claudeFrameToolUseIds`: the tool_use a non-root frame belongs to.
pub fn frame_tool_use_ids(raw: &Value) -> Vec<&str> {
    let mut ids = Vec::new();
    if let Some(parent) = raw.get("parent_tool_use_id").and_then(Value::as_str) {
        ids.push(parent);
    }
    if type_of(raw) == Some("system")
        && let Some(tool_use_id) = raw.get("tool_use_id").and_then(Value::as_str)
    {
        ids.push(tool_use_id);
    }
    ids
}

/// The tool_use ids in a root assistant frame.
pub fn tool_use_ids(raw: &Value) -> Vec<&str> {
    if type_of(raw) != Some("assistant") || has_parent(raw) {
        return Vec::new();
    }
    raw.pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|block| block.get("id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .collect()
}

/// A frame that counts as wake evidence while the root is idle (upstream
/// `isWakeEvidence`, with the classification it needs done by the caller).
pub struct WakeEvidence {
    pub pending_task_notification: bool,
    pub pending_subagent_notification: bool,
    pub nested_subagent_notification: bool,
    pub known_subagent_task_started: bool,
    pub new_subagent_task_started: bool,
}

impl WakeEvidence {
    pub fn is_evidence(&self, raw: &Value) -> bool {
        self.pending_task_notification
            || self.pending_subagent_notification
            || self.nested_subagent_notification
            || self.known_subagent_task_started
            || self.new_subagent_task_started
            || is_turn_start(raw)
            || matches!(
                type_of(raw),
                Some("assistant" | "user" | "result" | "rate_limit_event")
            )
    }
}

/// Upstream `isClaudeTurnStartMessage`: the `init` every turn opens with.
pub fn is_turn_start(raw: &Value) -> bool {
    type_of(raw) == Some("system") && subtype_of(raw) == Some("init")
}

/// Upstream `message.type === "system" && message.subtype === "task_notification"`.
pub fn is_task_notification(raw: &Value) -> bool {
    type_of(raw) == Some("system") && subtype_of(raw) == Some("task_notification")
}

pub fn is_task_started(raw: &Value) -> bool {
    type_of(raw) == Some("system") && subtype_of(raw) == Some("task_started")
}

/// The task id a system frame names.
pub fn task_id_of(raw: &Value) -> Option<&str> {
    raw.get("task_id").and_then(Value::as_str)
}

/// The summary a notification carries, when it is not empty.
pub fn summary_of(raw: &Value) -> Option<&str> {
    raw.get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.is_empty())
}

/// One background work report, kept until the continuation it belongs to.
pub struct WakeReport {
    pub task_id: String,
    /// The user turn the report arrived in.
    pub turn: u64,
    pub report: BackgroundWorkReport,
}

/// Per-thread wake state (upstream `wakeBuffers`, `wakeEligibleBackgroundTasksByNativeThread`,
/// `opaqueBackgroundTaskReplayTombstonesByNativeThread`, `requestedContinuations`,
/// `lastTurnRouteByNativeThread`, `wakeReportsByNativeThread`, `lastKnownOpaqueTasks`).
#[derive(Default)]
pub struct WakeState {
    /// Frames parked for the next continuation drain, in arrival order.
    pub buffer: Vec<Value>,
    /// The latest notification summary the buffer carries.
    pub detail: Option<String>,
    /// Local_bash tasks whose first idle notification may still wake the root.
    pub eligible: HashSet<String>,
    /// Tasks whose idle notification is buffered; the drain classifies them by it.
    pub tombstones: HashSet<String>,
    /// A continuation is offered and not yet drained.
    pub requested: bool,
    /// The thread and provider thread the last turn ran on.
    pub route: Option<(ThreadId, ProviderThreadId)>,
    pub user_turns: u64,
    pub reports: Vec<WakeReport>,
    /// The last roster entry per opaque task, for a report after it leaves the roster.
    pub last_known: HashMap<String, OrchestrationV2PendingBackgroundTask>,
}

impl WakeState {
    /// Upstream `recordWakeReport`: a task's report replaces its earlier one.
    pub fn record_report(&mut self, task_id: &str, report: BackgroundWorkReport) {
        let turn = self.user_turns;
        match self
            .reports
            .iter_mut()
            .find(|entry| entry.task_id == task_id)
        {
            Some(entry) => {
                entry.report = report;
                entry.turn = turn;
            }
            None => self.reports.push(WakeReport {
                task_id: task_id.to_owned(),
                turn,
                report,
            }),
        }
    }

    /// Upstream `startUserTurnForWakeReports`: a user turn ages the reports of
    /// the turn before last out.
    pub fn start_user_turn(&mut self) {
        self.user_turns += 1;
        let turn = self.user_turns;
        self.reports.retain(|entry| entry.turn + 1 >= turn);
    }

    /// Upstream `takeWakeReports`: removes and returns every report.
    pub fn take_reports(&mut self) -> Vec<BackgroundWorkReport> {
        self.reports.drain(..).map(|entry| entry.report).collect()
    }

    pub fn has_buffered(&self, predicate: impl Fn(&Value) -> bool) -> bool {
        self.buffer.iter().any(predicate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_echo_names_the_prompts_its_turn_consumed() {
        let echoed =
            json!({"type": "result", "user_message_uuids": ["p1"], "user_message_uuid": "p1"});
        assert_eq!(echoed_prompt_uuids(&echoed), vec!["p1"]);
        assert!(!is_result_for_other_turn(
            &echoed,
            "p1",
            PromptEchoMode::Early
        ));
        assert!(is_result_for_other_turn(
            &echoed,
            "p2",
            PromptEchoMode::Early
        ));
        let single = json!({"user_message_uuid": "p9"});
        assert_eq!(echoed_prompt_uuids(&single), vec!["p9"]);
    }

    #[test]
    fn a_result_without_an_echo_is_foreign_only_on_a_known_echoing_process() {
        let peer = json!({"type": "result", "origin": {"kind": "peer"}});
        assert!(!is_result_for_other_turn(
            &peer,
            "p1",
            PromptEchoMode::Unknown
        ));
        assert!(is_result_for_other_turn(
            &peer,
            "p1",
            PromptEchoMode::ResultOnly
        ));
        let human = json!({"type": "result", "origin": {"kind": "human"}});
        assert!(!is_result_for_other_turn(
            &human,
            "p1",
            PromptEchoMode::Early
        ));
        let plain = json!({"type": "result"});
        assert!(!is_result_for_other_turn(
            &plain,
            "p1",
            PromptEchoMode::Early
        ));
    }

    #[test]
    fn gated_frames_are_root_output_and_results() {
        assert!(is_gated_frame(
            &json!({"type": "assistant", "parent_tool_use_id": null})
        ));
        assert!(is_gated_frame(&json!({"type": "stream_event"})));
        assert!(!is_gated_frame(
            &json!({"type": "assistant", "parent_tool_use_id": "t"})
        ));
        assert!(is_gated_frame(&json!({"type": "result"})));
        assert!(!is_gated_frame(
            &json!({"type": "system", "subtype": "init"})
        ));
    }

    #[test]
    fn lifecycle_names_its_prompt_and_state() {
        let frame = json!({"type": "command_lifecycle", "command_uuid": "p1", "state": "queued"});
        assert_eq!(prompt_lifecycle(&frame), Some(("p1", "queued")));
        assert_eq!(prompt_lifecycle(&json!({"type": "result"})), None);
    }

    #[test]
    fn reports_age_out_after_the_turn_before_last() {
        let mut wake = WakeState::default();
        let report = |label: &str| BackgroundWorkReport {
            work: turin_provider_core::notification::BackgroundWork {
                label: Some(label.to_owned()),
                kind: turin_provider_core::notification::BackgroundWorkKind::BackgroundTask,
            },
            outcome:
                turin_contracts::orchestration_v2::OrchestrationV2NotificationOutcome::Completed,
        };
        wake.start_user_turn();
        wake.record_report("a", report("a"));
        wake.start_user_turn();
        wake.start_user_turn();
        wake.record_report("b", report("b"));
        wake.start_user_turn();
        // "a" was recorded two user turns ago, so it has aged out.
        let kept = wake.take_reports();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].work.label.as_deref(), Some("b"));
    }
}
