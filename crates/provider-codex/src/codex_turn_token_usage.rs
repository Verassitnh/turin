//! Port of upstream/apps/server/src/provider/CodexTurnTokenUsage.ts.
use serde_json::{Value, json};
use std::collections::HashMap;
use turin_contracts::provider_runtime::TurnTokenUsage;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    input: u64,
    cached: u64,
    written: Option<u64>,
    output: u64,
    reasoning: u64,
}

impl Counts {
    fn from_json(value: &Value) -> Self {
        let get = |key| value.get(key).and_then(Value::as_u64).unwrap_or(0);
        Self {
            input: get("inputTokens"),
            cached: get("cachedInputTokens"),
            written: value.get("cacheWriteInputTokens").and_then(Value::as_u64),
            output: get("outputTokens"),
            reasoning: get("reasoningOutputTokens"),
        }
    }
    fn reset_from(self, previous: Self) -> bool {
        self.input < previous.input
            || self.cached < previous.cached
            || self.output < previous.output
            || self.reasoning < previous.reasoning
    }
    fn delta(self, previous: Self) -> Self {
        Self {
            input: self.input - previous.input,
            cached: self.cached - previous.cached,
            written: match (self.written, previous.written) {
                (Some(now), Some(old)) if now >= old => Some(now - old),
                _ => None,
            },
            output: self.output - previous.output,
            reasoning: self.reasoning - previous.reasoning,
        }
    }
}

#[derive(Debug, Default)]
struct Accumulator {
    counts: Counts,
    observed: bool,
    has_subagents: bool,
}

#[derive(Debug, Default)]
pub struct CodexTurnTokenUsageState {
    baseline: Option<Counts>,
    active_turn_id: Option<String>,
    by_turn_id: HashMap<String, Accumulator>,
}

impl CodexTurnTokenUsageState {
    pub fn begin(&mut self, turn_id: &str) {
        self.active_turn_id = Some(turn_id.into());
        self.by_turn_id.entry(turn_id.into()).or_default();
    }
    pub fn mark_subagent(&mut self, turn_id: &str) {
        self.by_turn_id
            .entry(turn_id.into())
            .or_default()
            .has_subagents = true;
    }
    pub fn accumulate(&mut self, turn_id: &str, usage: &Value) {
        let current = Counts::from_json(&usage["total"]);
        if self.active_turn_id.as_deref() != Some(turn_id) {
            self.baseline = Some(current);
            return;
        }
        let last = Counts::from_json(&usage["last"]);
        let delta = self
            .baseline
            .filter(|old| !current.reset_from(*old))
            .map_or(last, |old| current.delta(old));
        self.baseline = Some(current);
        let acc = self.by_turn_id.entry(turn_id.into()).or_default();
        acc.observed |=
            delta.input > 0 || delta.cached > 0 || delta.output > 0 || delta.reasoning > 0;
        acc.counts.input += delta.input;
        acc.counts.cached += delta.cached;
        acc.counts.output += delta.output;
        acc.counts.reasoning += delta.reasoning;
        acc.counts.written = match (acc.counts.written, delta.written) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
    }
    pub fn complete(&mut self, turn_id: &str, completed: bool) -> TurnTokenUsage {
        let acc = self.by_turn_id.remove(turn_id).unwrap_or_default();
        if self.active_turn_id.as_deref() == Some(turn_id) {
            self.active_turn_id = None;
        }
        let value = if !acc.observed {
            json!({"usageStatus":"unavailable","usageScope":"main_agent","hasSubagents":acc.has_subagents})
        } else {
            let status = if completed { "complete" } else { "partial" };
            let mut value = json!({"usageStatus":status,"usageScope":"main_agent","hasSubagents":acc.has_subagents,"inputTokens":acc.counts.input,"cachedInputTokens":acc.counts.cached.min(acc.counts.input),"outputTokens":acc.counts.output,"reasoningTokens":acc.counts.reasoning.min(acc.counts.output)});
            if let Some(written) = acc.counts.written {
                value["cacheCreationTokens"] = json!(written.min(acc.counts.input));
            }
            value
        };
        serde_json::from_value(value).expect("Codex usage shape")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cumulative_delta_and_reset_fallback() {
        let mut s = CodexTurnTokenUsageState::default();
        s.begin("one");
        s.accumulate("one",&json!({"total":{"inputTokens":10,"cachedInputTokens":5,"outputTokens":2,"reasoningOutputTokens":1,"cacheWriteInputTokens":0},"last":{"inputTokens":10,"cachedInputTokens":5,"outputTokens":2,"reasoningOutputTokens":1,"cacheWriteInputTokens":0}}));
        s.accumulate("one",&json!({"total":{"inputTokens":14,"cachedInputTokens":6,"outputTokens":3,"reasoningOutputTokens":1,"cacheWriteInputTokens":0},"last":{"inputTokens":4,"cachedInputTokens":1,"outputTokens":1,"reasoningOutputTokens":0,"cacheWriteInputTokens":0}}));
        let v = serde_json::to_value(s.complete("one", true)).unwrap();
        assert_eq!(v["inputTokens"], 14);
        assert_eq!(v["cachedInputTokens"], 6);
        assert_eq!(v["outputTokens"], 3);
        s.begin("two");
        s.accumulate("two",&json!({"total":{"inputTokens":1,"cachedInputTokens":0,"outputTokens":1,"reasoningOutputTokens":0},"last":{"inputTokens":1,"cachedInputTokens":0,"outputTokens":1,"reasoningOutputTokens":0}}));
        assert_eq!(
            serde_json::to_value(s.complete("two", false)).unwrap()["inputTokens"],
            1
        );
    }
}
