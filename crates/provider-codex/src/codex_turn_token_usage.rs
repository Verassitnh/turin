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

#[derive(Debug)]
struct Accumulator {
    counts: Counts,
    observed: bool,
    has_subagents: bool,
}

impl Default for Accumulator {
    /// Upstream starts `cacheCreationTokens` at 0. It stays reported until an update
    /// arrives without the cache-write counter.
    fn default() -> Self {
        Self {
            counts: Counts {
                written: Some(0),
                ..Counts::default()
            },
            observed: false,
            has_subagents: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct CodexTurnTokenUsageState {
    baseline: Option<Counts>,
    active_turn_id: Option<String>,
    by_turn_id: HashMap<String, Accumulator>,
}

impl CodexTurnTokenUsageState {
    /// `beginTurnTokenUsage`: a new active turn drops what the previous one left behind.
    pub fn begin(&mut self, turn_id: &str) {
        if self.active_turn_id.as_deref() != Some(turn_id) {
            self.by_turn_id.clear();
            self.active_turn_id = Some(turn_id.into());
            self.by_turn_id.entry(turn_id.into()).or_default();
        }
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

    /// `TurnTokenUsage.test.ts` "billable native turn usage".
    fn breakdown(input: u64, cached: u64, output: u64) -> Value {
        json!({"totalTokens": input + output, "inputTokens": input, "cachedInputTokens": cached,
               "outputTokens": output, "reasoningOutputTokens": 0})
    }

    fn update(total: Value, last: Option<Value>) -> Value {
        json!({"last": last.unwrap_or_else(|| total.clone()), "total": total, "modelContextWindow": 200_000})
    }

    fn active(turn: &str) -> CodexTurnTokenUsageState {
        CodexTurnTokenUsageState {
            active_turn_id: Some(turn.into()),
            ..CodexTurnTokenUsageState::default()
        }
    }

    #[test]
    fn adds_each_codex_response_once_and_excludes_the_prior_resumed_history() {
        let mut state = active("turn");
        let first = update(breakdown(1010, 505, 102), Some(breakdown(10, 5, 2)));
        state.accumulate("turn", &first);
        state.accumulate("turn", &first);
        state.accumulate(
            "turn",
            &update(breakdown(1030, 515, 108), Some(breakdown(20, 10, 6))),
        );
        assert_eq!(
            serde_json::to_value(state.complete("turn", true)).unwrap(),
            json!({"usageStatus": "complete", "usageScope": "main_agent", "hasSubagents": false,
                   "inputTokens": 30, "cachedInputTokens": 15, "outputTokens": 8, "reasoningTokens": 0})
        );
    }

    #[test]
    fn keeps_previous_responses_when_compaction_resets_codex_cumulative_counters() {
        let mut state = active("turn");
        state.accumulate("turn", &update(breakdown(100, 40, 20), None));
        state.accumulate("turn", &update(breakdown(20, 5, 3), None));
        state.mark_subagent("turn");
        let usage = serde_json::to_value(state.complete("turn", false)).unwrap();
        assert_eq!(usage["usageStatus"], "partial");
        assert_eq!(usage["hasSubagents"], true);
        assert_eq!(usage["inputTokens"], 120);
        assert_eq!(usage["cachedInputTokens"], 45);
        assert_eq!(usage["outputTokens"], 23);
    }

    #[test]
    fn does_not_bill_late_codex_updates_against_the_next_active_turn() {
        let mut state = active("next");
        state.accumulate("previous", &update(breakdown(100, 10, 20), None));
        state.accumulate(
            "next",
            &update(breakdown(104, 12, 21), Some(breakdown(4, 2, 1))),
        );
        let usage = serde_json::to_value(state.complete("next", true)).unwrap();
        assert_eq!(usage["inputTokens"], 4);
        assert_eq!(usage["outputTokens"], 1);
        assert_eq!(
            serde_json::to_value(state.complete("missing", false)).unwrap()["usageStatus"],
            "unavailable"
        );
    }

    #[test]
    fn reports_cache_creation_tokens_while_every_update_carries_the_counter() {
        let mut state = CodexTurnTokenUsageState::default();
        state.begin("turn");
        let with_write = |input, written| {
            json!({"total": {"inputTokens": input, "cachedInputTokens": 0, "outputTokens": 1,
                             "reasoningOutputTokens": 0, "cacheWriteInputTokens": written},
                   "last": {"inputTokens": input, "cachedInputTokens": 0, "outputTokens": 1,
                            "reasoningOutputTokens": 0, "cacheWriteInputTokens": written}})
        };
        state.accumulate("turn", &with_write(10, 4));
        state.accumulate("turn", &with_write(30, 9));
        let usage = serde_json::to_value(state.complete("turn", true)).unwrap();
        // The first update is the baseline's own `last`; the second adds the growth of 5.
        assert_eq!(usage["cacheCreationTokens"], 9);

        // An update that drops the counter drops it for the whole turn.
        state.begin("two");
        state.accumulate("two", &with_write(10, 4));
        state.accumulate("two", &update(breakdown(20, 0, 2), None));
        let usage = serde_json::to_value(state.complete("two", true)).unwrap();
        assert!(usage.get("cacheCreationTokens").is_none());
    }

    #[test]
    fn a_new_active_turn_clears_what_the_previous_one_left_behind() {
        let mut state = CodexTurnTokenUsageState::default();
        state.begin("one");
        state.accumulate("one", &update(breakdown(10, 0, 1), None));
        state.begin("two");
        assert!(!state.by_turn_id.contains_key("one"));
        assert!(state.by_turn_id.contains_key("two"));
    }
}
