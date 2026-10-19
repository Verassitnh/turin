//! Port of upstream/apps/server/src/provider/Drivers/claudeUsage.ts.
//!
//! Usage history for Claude Code: the transcript format of its
//! `<home>/projects/**.jsonl` session files and where each instance keeps them.

use std::{collections::HashMap, sync::Arc};

use serde_json::Value;
use turin_contracts::{
    settings::ClaudeSettings,
    usage::{UsageProviderKind, UsageTokenTotals},
};
use turin_provider_core::{
    host_process::HostProcess,
    rt::boxed,
    usage::{
        ProviderUsageInstance, ProviderUsageReader, SelectedField, TranscriptDirectory,
        TranscriptUsageFormat, UsageRecord, UsageSpeed, parse_timestamp_ms, token_count,
    },
};

use crate::claude_home::resolve_claude_home_path;

/// Parses one line of a Claude Code transcript.
///
/// T3 Code writes one record per assistant *content block*, and every one of
/// those records repeats the same complete `usage` object for the parent
/// message. Summing them overcounts by roughly 2.4x on a real workload, so the
/// caller must drop repeats by `dedupe_key` and keep the first.
pub fn parse_claude_line(line: &str) -> Option<UsageRecord> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    parse_claude_record(&parsed)
}

fn parse_claude_record(record: &Value) -> Option<UsageRecord> {
    if record.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = record.get("message").filter(|value| value.is_object())?;
    let usage = message.get("usage").filter(|value| value.is_object())?;
    let timestamp_ms = parse_timestamp_ms(record.get("timestamp").unwrap_or(&Value::Null))?;

    let model = message.get("model").and_then(Value::as_str).unwrap_or("");
    if model.is_empty() {
        return None;
    }

    let message_id = message.get("id").and_then(Value::as_str);
    let request_id = record.get("requestId").and_then(Value::as_str);
    // Matches ccusage: prefer the message/request pair, fall back to whichever
    // half exists. Records with neither cannot be de-duplicated.
    let dedupe_key = if message_id.is_none() && request_id.is_none() {
        None
    } else {
        Some(format!(
            "{}:{}",
            message_id.unwrap_or(""),
            request_id.unwrap_or("")
        ))
    };

    let count = |key: &str| usage.get(key).map_or(0, token_count);
    Some(UsageRecord {
        provider: UsageProviderKind::Claude,
        timestamp_ms,
        model: model.to_owned(),
        rate_model: None,
        session_id: record
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        totals: UsageTokenTotals {
            uncached_input_tokens: count("input_tokens"),
            cached_input_tokens: count("cache_read_input_tokens"),
            cache_creation_tokens: count("cache_creation_input_tokens"),
            output_tokens: count("output_tokens"),
            // Anthropic folds thinking tokens into output and does not break them out.
            reasoning_tokens: 0,
        },
        reported_cost_usd: record
            .get("costUSD")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite()),
        speed: if usage.get("speed").and_then(Value::as_str) == Some("fast") {
            UsageSpeed::Fast
        } else {
            UsageSpeed::Standard
        },
        dedupe_key,
    })
}

fn keep() -> SelectedField {
    SelectedField::Keep
}

pub fn claude_usage_format() -> TranscriptUsageFormat {
    TranscriptUsageFormat {
        // Keeps the dedupe and cost metadata. The `usage` subtree keeps future token fields.
        select_fields: HashMap::from([
            ("type".to_owned(), keep()),
            ("timestamp".to_owned(), keep()),
            ("requestId".to_owned(), keep()),
            ("sessionId".to_owned(), keep()),
            ("costUSD".to_owned(), keep()),
            (
                "message".to_owned(),
                SelectedField::Nested(HashMap::from([
                    ("id".to_owned(), keep()),
                    ("model".to_owned(), keep()),
                    ("usage".to_owned(), keep()),
                ])),
            ),
        ]),
        usage_markers: vec!["\"usage\"".to_owned()],
        parse_line: Arc::new(|line, _state| parse_claude_line(line).into_iter().collect()),
        parse_projected: Arc::new(|projected, _state| {
            parse_claude_record(projected).into_iter().collect()
        }),
        state: None,
        shared_sessions_across_files: false,
    }
}

/// Where one instance keeps its transcripts: `<home>/projects`. An undecodable
/// config has no trustworthy home to read.
fn claude_usage_directories(instance: &ProviderUsageInstance) -> Vec<TranscriptDirectory> {
    let Some(config) = instance.config.clone() else {
        return Vec::new();
    };
    let Ok(settings) = serde_json::from_value::<ClaudeSettings>(config) else {
        return Vec::new();
    };
    let home = HostProcess::current().home_directory;
    let claude_home = resolve_claude_home_path(
        settings.home_path.as_str(),
        Some(&instance.environment),
        &home,
    );
    vec![TranscriptDirectory {
        dir: claude_home.join("projects").to_string_lossy().into_owned(),
        file_name: None,
    }]
}

pub fn claude_usage_reader() -> ProviderUsageReader {
    ProviderUsageReader::Transcripts {
        provider: UsageProviderKind::Claude,
        format: claude_usage_format(),
        directories: Arc::new(|instance| boxed(async move { claude_usage_directories(&instance) })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Shaped after a real Claude Code assistant record.
    fn claude_line(message_id: &str, content_type: &str, speed: Option<&str>) -> String {
        let mut usage = json!({
            "input_tokens": 2,
            "cache_creation_input_tokens": 66818,
            "cache_read_input_tokens": 1000,
            "output_tokens": 286,
        });
        if let Some(speed) = speed {
            usage["speed"] = json!(speed);
        }
        json!({
            "type": "assistant",
            "timestamp": "2026-08-07T04:05:13.944Z",
            "sessionId": "5a128faa-8253-489e-b935-6c08e8e670c0",
            "cwd": "/home/theo/project",
            "message": {
                "id": message_id,
                "role": "assistant",
                "model": "claude-fable-5",
                "content": [{ "type": content_type }],
                "usage": usage,
            },
        })
        .to_string()
    }

    #[test]
    fn extracts_token_totals_and_a_dedupe_key() {
        let record = parse_claude_line(&claude_line("msg_1", "text", None)).expect("record");
        assert_eq!(record.provider, UsageProviderKind::Claude);
        assert_eq!(record.model, "claude-fable-5");
        assert_eq!(
            record.totals,
            UsageTokenTotals {
                uncached_input_tokens: 2,
                cached_input_tokens: 1000,
                cache_creation_tokens: 66818,
                output_tokens: 286,
                reasoning_tokens: 0,
            }
        );
        assert_eq!(record.dedupe_key.as_deref(), Some("msg_1:"));
        assert_eq!(record.speed, UsageSpeed::Standard);
    }

    #[test]
    fn marks_fast_mode_requests() {
        let speed = |speed: &str| {
            parse_claude_line(&claude_line("msg_1", "text", Some(speed)))
                .expect("record")
                .speed
        };
        assert_eq!(speed("fast"), UsageSpeed::Fast);
        assert_eq!(speed("standard"), UsageSpeed::Standard);
    }

    #[test]
    fn gives_every_content_block_of_one_message_the_same_dedupe_key() {
        // T3 Code writes one record per content block, each repeating the parent
        // message's full usage. Summing them would overcount ~2.4x on real data.
        let text = parse_claude_line(&claude_line("msg_2", "text", None)).expect("text");
        let tool_use = parse_claude_line(&claude_line("msg_2", "tool_use", None)).expect("tool");
        assert_eq!(text.dedupe_key, tool_use.dedupe_key);
        assert_eq!(text.totals, tool_use.totals);
    }

    #[test]
    fn ignores_records_that_are_not_assistant_messages() {
        assert_eq!(
            parse_claude_line(&json!({ "type": "user", "message": {} }).to_string()),
            None
        );
        assert_eq!(parse_claude_line("not json"), None);
    }

    #[test]
    fn the_reader_names_the_projects_directory_of_the_instance_home() {
        let instance = ProviderUsageInstance {
            instance_id: serde_json::from_value(json!("claudeAgent")).expect("instance id"),
            config: Some(json!({ "homePath": "/tmp/claude-home" })),
            environment: HashMap::new(),
            configured: true,
        };
        let directories = claude_usage_directories(&instance);
        assert_eq!(directories.len(), 1);
        assert!(directories[0].dir.ends_with("claude-home/projects"));
        let undecodable = ProviderUsageInstance {
            config: None,
            ..instance
        };
        assert!(claude_usage_directories(&undecodable).is_empty());
    }
}
