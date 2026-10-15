//! Local stand-ins for the Claude and Codex transcript formats, which are
//! ported with their provider crates (T09/T10). They keep the reader and cache
//! mechanics under test: a stateless Claude-shaped format and a stateful
//! Codex-shaped one whose reducer carries model, session, tier, and a
//! duplicate-suppression marker across lines.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use turin_contracts::usage::{UsageProviderKind, UsageTokenTotals};
use turin_provider_core::usage::{
    TranscriptUsageFormat, TranscriptUsageState, UsageRecord, UsageSpeed, empty_totals,
};

/// The line's `timestamp` as epoch milliseconds, or 0 when absent.
fn timestamp_ms(line: &Value) -> f64 {
    line["timestamp"]
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map_or(0.0, |parsed| parsed.timestamp_millis() as f64)
}

fn claude_record(line: &Value) -> Option<UsageRecord> {
    let usage = line.get("message")?.get("usage")?;
    let output = usage.get("output_tokens")?.as_u64()?;
    Some(UsageRecord {
        provider: UsageProviderKind::Claude,
        timestamp_ms: timestamp_ms(line),
        model: line["message"]["model"].as_str()?.to_owned(),
        rate_model: None,
        session_id: line["sessionId"].as_str().unwrap_or_default().to_owned(),
        totals: UsageTokenTotals {
            uncached_input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: output,
            ..empty_totals()
        },
        reported_cost_usd: None,
        speed: UsageSpeed::Standard,
        dedupe_key: line["message"]["id"].as_str().map(|id| format!("{id}:")),
    })
}

pub fn claude_format() -> TranscriptUsageFormat {
    TranscriptUsageFormat {
        select_fields: HashMap::new(),
        usage_markers: vec!["\"usage\"".to_owned()],
        parse_line: Arc::new(|text, _state| {
            serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|line| claude_record(&line))
                .into_iter()
                .collect()
        }),
        parse_projected: Arc::new(|value, _state| claude_record(value).into_iter().collect()),
        state: None,
        shared_sessions_across_files: false,
    }
}

/// Reducer state: session, model, tier, and the last usage line seen.
pub fn codex_format() -> TranscriptUsageFormat {
    let initial = || json!({ "session": "", "model": "", "speed": "standard", "last": null });
    TranscriptUsageFormat {
        select_fields: HashMap::new(),
        usage_markers: [
            "\"token_count\"",
            "\"session_meta\"",
            "\"turn_context\"",
            "\"thread_settings_applied\"",
        ]
        .map(str::to_owned)
        .to_vec(),
        parse_line: Arc::new(|text, state| {
            let Ok(line) = serde_json::from_str::<Value>(text) else {
                return Vec::new();
            };
            let payload = &line["payload"];
            match line["type"].as_str() {
                Some("session_meta") => {
                    state["session"] = payload["id"].clone();
                    Vec::new()
                }
                Some("turn_context") => {
                    state["model"] = payload["model"].clone();
                    Vec::new()
                }
                Some("event_msg") if payload["type"] == "thread_settings_applied" => {
                    state["speed"] = payload["thread_settings"]["service_tier"].clone();
                    Vec::new()
                }
                Some("event_msg") if payload["type"] == "token_count" => {
                    let output = payload["info"]["last_token_usage"]["output_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    let stamp = json!([line["timestamp"], output]);
                    // Codex re-emits an unchanged token_count on stream boundaries.
                    if state["last"] == stamp {
                        return Vec::new();
                    }
                    state["last"] = stamp;
                    let speed = match state["speed"].as_str() {
                        Some("ultrafast") => UsageSpeed::Ultrafast,
                        _ => UsageSpeed::Standard,
                    };
                    vec![UsageRecord {
                        provider: UsageProviderKind::Codex,
                        timestamp_ms: timestamp_ms(&line),
                        model: state["model"].as_str().unwrap_or_default().to_owned(),
                        rate_model: None,
                        session_id: state["session"].as_str().unwrap_or_default().to_owned(),
                        totals: UsageTokenTotals {
                            output_tokens: output,
                            ..empty_totals()
                        },
                        reported_cost_usd: None,
                        speed,
                        dedupe_key: None,
                    }]
                }
                _ => Vec::new(),
            }
        }),
        parse_projected: Arc::new(|_value, _state| Vec::new()),
        state: Some(TranscriptUsageState {
            initial: Arc::new(initial),
            encode: Arc::new(|state| state.clone()),
            decode: Arc::new(|encoded| {
                let valid = encoded["session"].is_string()
                    && encoded["model"].is_string()
                    && encoded["speed"].is_string()
                    && (encoded["last"].is_null() || encoded["last"].is_array());
                if valid {
                    Ok(encoded.clone())
                } else {
                    Err("not a codex reducer state".to_owned())
                }
            }),
        }),
        shared_sessions_across_files: false,
    }
}

/// The two fixture formats by provider, as `usageTestFormats.ts` exposes them.
pub fn test_format_map() -> HashMap<UsageProviderKind, TranscriptUsageFormat> {
    HashMap::from([
        (UsageProviderKind::Claude, claude_format()),
        (UsageProviderKind::Codex, codex_format()),
    ])
}
