//! Port of upstream/apps/server/src/provider/ClaudeTurnTokenUsage.ts.
use serde_json::{Map, Value, json};
use turin_contracts::provider_runtime::{RuntimeTurnState, TurnTokenUsage};

fn count(value: Option<&Value>) -> Option<u64> {
    let n = value?.as_f64()?;
    (n.is_finite() && n >= 0.0).then(|| n.round() as u64)
}

/// Earlier aborted steer segments each report their own billed usage. Only
/// billed fields are additive; other final-result fields describe that result.
pub fn normalize_claude_turn_token_usage(
    result: Option<&Value>,
    has_subagents: bool,
    terminal_status: RuntimeTurnState,
    earlier_segment_usage: &[Value],
) -> TurnTokenUsage {
    let mut unavailable = json!({
        "usageStatus":"unavailable", "usageScope":"main_agent", "hasSubagents":has_subagents
    });
    let Some(mut usage) = result
        .and_then(|r| r.get("usage"))
        .and_then(Value::as_object)
        .cloned()
    else {
        return serde_json::from_value(unavailable).expect("valid unavailable usage");
    };
    for field in [
        "input_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "output_tokens",
    ] {
        add_earlier(&mut usage, field, earlier_segment_usage);
    }
    if let Some(details) = usage
        .get_mut("output_tokens_details")
        .and_then(Value::as_object_mut)
    {
        let total = count(details.get("thinking_tokens"));
        if let Some(mut total) = total {
            for earlier in earlier_segment_usage {
                total += count(
                    earlier
                        .get("output_tokens_details")
                        .and_then(|d| d.get("thinking_tokens")),
                )
                .unwrap_or(0);
            }
            details.insert("thinking_tokens".into(), json!(total));
        }
    }
    let uncached = count(usage.get("input_tokens"));
    let cached = count(usage.get("cache_read_input_tokens"));
    let created = count(usage.get("cache_creation_input_tokens"));
    let output = count(usage.get("output_tokens"));
    let thinking = count(
        usage
            .get("output_tokens_details")
            .and_then(|d| d.get("thinking_tokens")),
    );
    let input = uncached
        .zip(if usage.get("cache_read_input_tokens").is_some() {
            cached
        } else {
            Some(0)
        })
        .zip(if usage.get("cache_creation_input_tokens").is_some() {
            created
        } else {
            Some(0)
        })
        .map(|((a, b), c)| a + b + c);
    let known = uncached.is_some() || cached.is_some() || created.is_some() || output.is_some();
    let positive =
        uncached.unwrap_or(0) + cached.unwrap_or(0) + created.unwrap_or(0) + output.unwrap_or(0)
            > 0;
    if !known
        || (result
            .and_then(|r| r.get("subtype"))
            .and_then(Value::as_str)
            != Some("success")
            && !positive)
    {
        return serde_json::from_value(unavailable).expect("valid unavailable usage");
    }
    let complete = terminal_status == RuntimeTurnState::Completed
        && result
            .and_then(|r| r.get("subtype"))
            .and_then(Value::as_str)
            == Some("success")
        && input.is_some()
        && output.is_some();
    unavailable["usageStatus"] = json!(if complete { "complete" } else { "partial" });
    let map = unavailable.as_object_mut().unwrap();
    insert_count(map, "cachedInputTokens", cached);
    insert_count(map, "cacheCreationTokens", created);
    insert_count(
        map,
        "reasoningTokens",
        thinking.zip(output).map(|(t, o)| t.min(o)),
    );
    insert_count(map, "inputTokens", input);
    insert_count(map, "outputTokens", output);
    serde_json::from_value(unavailable).expect("valid normalized usage")
}

fn add_earlier(usage: &mut Map<String, Value>, field: &str, earlier: &[Value]) {
    if let Some(mut total) = count(usage.get(field)) {
        for segment in earlier {
            total += count(segment.get(field)).unwrap_or(0);
        }
        usage.insert(field.into(), json!(total));
    }
}
fn insert_count(map: &mut Map<String, Value>, field: &str, value: Option<u64>) {
    if let Some(value) = value {
        map.insert(field.into(), json!(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adds_steer_segments_and_clamps_thinking() {
        let result = json!({"subtype":"success","usage":{"input_tokens":3,"cache_read_input_tokens":7,"cache_creation_input_tokens":2,"output_tokens":5,"output_tokens_details":{"thinking_tokens":8}}});
        let earlier = [
            json!({"input_tokens":1,"output_tokens":2,"output_tokens_details":{"thinking_tokens":1}}),
        ];
        let value = serde_json::to_value(normalize_claude_turn_token_usage(
            Some(&result),
            true,
            RuntimeTurnState::Completed,
            &earlier,
        ))
        .unwrap();
        assert_eq!(value["inputTokens"], 13);
        assert_eq!(value["outputTokens"], 7);
        assert_eq!(value["reasoningTokens"], 7);
        assert_eq!(value["usageStatus"], "complete");
    }
    #[test]
    fn failed_empty_usage_is_unavailable() {
        let result = json!({"subtype":"error_during_execution","usage":{"input_tokens":0,"output_tokens":0}});
        let value = serde_json::to_value(normalize_claude_turn_token_usage(
            Some(&result),
            false,
            RuntimeTurnState::Failed,
            &[],
        ))
        .unwrap();
        assert_eq!(value["usageStatus"], "unavailable");
    }
}
