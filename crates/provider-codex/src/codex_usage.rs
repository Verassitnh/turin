//! Port of upstream/apps/server/src/provider/Drivers/codexUsage.ts.
//!
//! Usage history for Codex: the rollout format of `<home>/sessions/**.jsonl`
//! and where each instance's shared home keeps them.

use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turin_contracts::{
    settings::{CodexSettings, CodexSetupMode},
    usage::{UsageProviderKind, UsageTokenTotals},
};
use turin_provider_core::{
    host_process::HostProcess,
    rt::boxed,
    usage::{
        ProviderUsageInstance, ProviderUsageReader, SelectedField, TranscriptDirectory,
        TranscriptUsageFormat, TranscriptUsageState, UsageRecord, UsageSpeed, parse_timestamp_ms,
        token_count, total_tokens,
    },
};

use crate::codex_home_layout::resolve_codex_home_layout;

/// Rolling state for a single Codex rollout file.
///
/// Codex `token_count` events carry no model or service tier, so both are
/// carried forward: the model from the most recent `turn_context`, the tier from
/// the most recent `thread_settings_applied`. Sessions that switch either
/// mid-run attribute correctly from the switch onward.
///
/// Persisted in the scan cache at each resume point. The field order is the
/// stored JSON's, so keep it stable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexScanState {
    pub model: String,
    pub speed: StoredSpeed,
    pub session_id: String,
    pub last_usage_signature: Option<String>,
    pub saw_session_meta: bool,
    /// While true, leading usage events are re-stamped copies of parent history.
    pub suppressing_fork_copies: bool,
    pub fork_copy_anchor_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoredSpeed {
    Standard,
    Fast,
    Ultrafast,
}

impl From<StoredSpeed> for UsageSpeed {
    fn from(speed: StoredSpeed) -> Self {
        match speed {
            StoredSpeed::Standard => Self::Standard,
            StoredSpeed::Fast => Self::Fast,
            StoredSpeed::Ultrafast => Self::Ultrafast,
        }
    }
}

pub fn initial_codex_scan_state() -> CodexScanState {
    CodexScanState {
        model: String::new(),
        speed: StoredSpeed::Standard,
        session_id: String::new(),
        last_usage_signature: None,
        saw_session_meta: false,
        suppressing_fork_copies: false,
        fork_copy_anchor_ms: 0.0,
    }
}

/// A forked or subagent rollout opens with the parent's full history copied in,
/// every line re-stamped to the fork instant. Those copies are written in one
/// synchronous burst (observed gaps 0-40ms), while the child's first genuine
/// usage event only lands after a real model turn (observed 5s+). One second of
/// separation splits the two cleanly; `ccusage` uses the same threshold.
const FORK_COPY_MAX_GAP_MS: f64 = 1000.0;

/// Whether a `session_meta` payload marks the rollout as a fork or subagent.
fn is_forked_session_meta(payload: &Value) -> bool {
    if payload.get("forked_from_id").is_some_and(Value::is_string) {
        return true;
    }
    payload
        .get("source")
        .and_then(|source| source.get("subagent"))
        .and_then(|subagent| subagent.get("thread_spawn"))
        .and_then(|spawn| spawn.get("parent_thread_id"))
        .is_some_and(Value::is_string)
}

/// Feeds one line of a Codex rollout into `state`, returning a record when the
/// line was a usage event.
///
/// Deltas come from `last_token_usage`. Summing those across a session
/// reconciles with the session's final `total_token_usage`, provided
/// consecutive duplicate events are dropped, which this does.
pub fn parse_codex_line(line: &str, state: &mut CodexScanState) -> Option<UsageRecord> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    parse_codex_record(&parsed, state)
}

fn parse_codex_record(record: &Value, state: &mut CodexScanState) -> Option<UsageRecord> {
    let payload = record.get("payload").filter(|value| value.is_object())?;
    let payload_type = payload.get("type").and_then(Value::as_str);
    let record_type = record.get("type").and_then(Value::as_str);
    let timestamp = record.get("timestamp").unwrap_or(&Value::Null);

    if record_type == Some("session_meta") {
        // Only the first meta describes this file's own session. A forked rollout
        // repeats the ancestors' metas right after it; letting those through would
        // reassign every subsequent record to an ancestor session.
        if state.saw_session_meta {
            return None;
        }
        state.saw_session_meta = true;
        let id = payload.get("id").or_else(|| payload.get("session_id"));
        if let Some(id) = id.and_then(Value::as_str) {
            state.session_id = id.to_owned();
        }
        if let Some(meta_timestamp_ms) = parse_timestamp_ms(timestamp)
            && is_forked_session_meta(payload)
        {
            state.suppressing_fork_copies = true;
            state.fork_copy_anchor_ms = meta_timestamp_ms;
        }
        return None;
    }

    if record_type == Some("turn_context") {
        if let Some(model) = payload.get("model").and_then(Value::as_str) {
            state.model = model.to_owned();
        }
        return None;
    }

    if payload_type == Some("thread_settings_applied") {
        if let Some(settings) = payload
            .get("thread_settings")
            .filter(|value| value.is_object())
        {
            state.speed = codex_speed(settings.get("service_tier"));
        }
        return None;
    }

    if payload_type != Some("token_count") {
        return None;
    }

    let last = payload
        .get("info")
        .filter(|value| value.is_object())?
        .get("last_token_usage")
        .filter(|value| value.is_object())?;

    // Only an event that is otherwise eligible may consume the duplicate
    // signature. A token_count arriving before its turn_context (no model yet)
    // must not poison it, or the re-emitted copy after the model is known would
    // be skipped as a duplicate and those tokens never counted.
    let timestamp_ms = parse_timestamp_ms(timestamp)?;
    if state.model.is_empty() {
        return None;
    }

    // Codex re-emits an unchanged token_count on some stream boundaries. Summing
    // those would double count, so identical consecutive payloads are skipped.
    let signature = last.to_string();
    if state.last_usage_signature.as_deref() == Some(signature.as_str()) {
        return None;
    }
    state.last_usage_signature = Some(signature);

    // In a forked rollout the copied parent history was already counted from the
    // parent's own file. Drop the leading burst; the first usage event separated
    // from its predecessor by a real turn's worth of time ends it for good.
    if state.suppressing_fork_copies {
        if timestamp_ms - state.fork_copy_anchor_ms < FORK_COPY_MAX_GAP_MS {
            state.fork_copy_anchor_ms = timestamp_ms;
            return None;
        }
        state.suppressing_fork_copies = false;
    }

    let count = |key: &str| last.get(key).map_or(0, token_count);
    let input_tokens = count("input_tokens");
    let cached_input_tokens = count("cached_input_tokens");
    let cache_creation_tokens = count("cache_write_input_tokens");
    let output_tokens = count("output_tokens");

    let totals = UsageTokenTotals {
        // Codex reports `input_tokens` inclusive of the cached portion.
        uncached_input_tokens: input_tokens
            .saturating_sub(cached_input_tokens)
            .saturating_sub(cache_creation_tokens),
        cached_input_tokens,
        cache_creation_tokens,
        output_tokens,
        // Reported inside output_tokens, surfaced separately for the token mix.
        reasoning_tokens: output_tokens.min(count("reasoning_output_tokens")),
    };

    if total_tokens(&totals) == 0 {
        return None;
    }

    Some(UsageRecord {
        provider: UsageProviderKind::Codex,
        timestamp_ms,
        model: state.model.clone(),
        rate_model: None,
        session_id: state.session_id.clone(),
        totals,
        // Codex does not report cost in the rollout.
        reported_cost_usd: None,
        speed: state.speed.into(),
        // Events surviving the fork-copy suppression above are unique to this
        // rollout, so they need no global dedup.
        dedupe_key: None,
    })
}

/// Maps a Codex `service_tier` to its billing speed. Codex omits the field when
/// no tier was requested, which bills as standard, as do `default` and
/// `standard`. `fast` is accepted as an alias of `priority`.
fn codex_speed(service_tier: Option<&Value>) -> StoredSpeed {
    match service_tier.and_then(Value::as_str) {
        Some("priority" | "fast") => StoredSpeed::Fast,
        Some("ultrafast") => StoredSpeed::Ultrafast,
        _ => StoredSpeed::Standard,
    }
}

fn keep() -> SelectedField {
    SelectedField::Keep
}

fn nested<const N: usize>(fields: [(&str, SelectedField); N]) -> SelectedField {
    SelectedField::Nested(
        fields
            .into_iter()
            .map(|(key, field)| (key.to_owned(), field))
            .collect(),
    )
}

/// The reducer state travels as JSON; a state that no longer decodes is dropped
/// by the scan cache, which then reads the file from the start.
fn reduce(
    state: &mut Value,
    record: impl FnOnce(&mut CodexScanState) -> Option<UsageRecord>,
) -> Vec<UsageRecord> {
    let Ok(mut decoded) = serde_json::from_value::<CodexScanState>(state.clone()) else {
        return Vec::new();
    };
    let produced = record(&mut decoded);
    if let Ok(encoded) = serde_json::to_value(&decoded) {
        *state = encoded;
    }
    produced.into_iter().collect()
}

pub fn codex_usage_format() -> TranscriptUsageFormat {
    TranscriptUsageFormat {
        // Keeps the fields the reducer reads as well as the usage itself.
        select_fields: HashMap::from([
            ("type".to_owned(), keep()),
            ("timestamp".to_owned(), keep()),
            (
                "payload".to_owned(),
                nested([
                    ("type", keep()),
                    ("id", keep()),
                    ("session_id", keep()),
                    ("model", keep()),
                    ("thread_settings", nested([("service_tier", keep())])),
                    ("forked_from_id", keep()),
                    (
                        "source",
                        nested([(
                            "subagent",
                            nested([("thread_spawn", nested([("parent_thread_id", keep())]))]),
                        )]),
                    ),
                    ("info", nested([("last_token_usage", keep())])),
                ]),
            ),
        ]),
        // `turn_context`, `thread_settings_applied` and `session_meta` lines hold no
        // usage, but they carry the model, tier and session the reducer needs.
        usage_markers: [
            "\"token_count\"",
            "\"turn_context\"",
            "\"thread_settings_applied\"",
            "\"session_meta\"",
        ]
        .map(str::to_owned)
        .to_vec(),
        parse_line: Arc::new(|line, state| {
            reduce(state, |decoded| parse_codex_line(line, decoded))
        }),
        parse_projected: Arc::new(|projected, state| {
            reduce(state, |decoded| parse_codex_record(projected, decoded))
        }),
        state: Some(TranscriptUsageState {
            initial: Arc::new(|| {
                serde_json::to_value(initial_codex_scan_state()).unwrap_or(Value::Null)
            }),
            encode: Arc::new(Value::clone),
            decode: Arc::new(|encoded| {
                serde_json::from_value::<CodexScanState>(encoded.clone())
                    .map(|_| encoded.clone())
                    .map_err(|error| error.to_string())
            }),
        }),
        // A rollout that moved after it was read leaves a copy in both places.
        shared_sessions_across_files: true,
    }
}

/// Where one instance's shared home keeps its rollouts: `<shared home>/sessions`.
/// An undecodable config has no trustworthy home to read.
fn codex_usage_directories(instance: &ProviderUsageInstance) -> Vec<TranscriptDirectory> {
    let Some(config) = instance.config.clone() else {
        return Vec::new();
    };
    let Ok(settings) = serde_json::from_value::<CodexSettings>(config) else {
        return Vec::new();
    };
    let environment_home = instance
        .environment
        .get("CODEX_HOME")
        .map(|value| value.trim())
        .unwrap_or("");
    let home_path = settings.home_path.as_str().trim();
    let shadow_home_path = settings.shadow_home_path.as_str().trim();
    let adopt_environment_home = settings.setup_mode != Some(CodexSetupMode::Managed)
        && home_path.is_empty()
        && shadow_home_path.is_empty()
        && !environment_home.is_empty();
    let layout = resolve_codex_home_layout(
        if adopt_environment_home {
            environment_home
        } else {
            settings.home_path.as_str()
        },
        settings.shadow_home_path.as_str(),
        &HostProcess::current().home_directory,
    );
    vec![TranscriptDirectory {
        dir: layout
            .shared_home_path
            .join("sessions")
            .to_string_lossy()
            .into_owned(),
        file_name: None,
    }]
}

pub fn codex_usage_reader() -> ProviderUsageReader {
    ProviderUsageReader::Transcripts {
        provider: UsageProviderKind::Codex,
        format: codex_usage_format(),
        directories: Arc::new(|instance| boxed(async move { codex_usage_directories(&instance) })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session_meta() -> String {
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-01T05:17:41.289Z",
            "payload": { "type": "session_meta", "id": "019fbbc1-b12c-7360-a685-28c181f0025f" },
        })
        .to_string()
    }

    fn turn_context() -> String {
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-01T05:17:42.694Z",
            "payload": { "type": "turn_context", "model": "gpt-5.6-sol" },
        })
        .to_string()
    }

    fn token_count(input_tokens: u64, cached: u64, output: u64, reasoning: u64) -> String {
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-01T05:17:49.919Z",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": input_tokens,
                        "cached_input_tokens": cached,
                        "cache_write_input_tokens": 0,
                        "output_tokens": output,
                        "reasoning_output_tokens": reasoning,
                    },
                },
            },
        })
        .to_string()
    }

    #[test]
    fn attributes_usage_to_the_model_from_the_preceding_turn_context() {
        let mut state = initial_codex_scan_state();
        parse_codex_line(&session_meta(), &mut state);
        parse_codex_line(&turn_context(), &mut state);
        let record =
            parse_codex_line(&token_count(19239, 11008, 299, 116), &mut state).expect("record");
        assert_eq!(record.provider, UsageProviderKind::Codex);
        assert_eq!(record.model, "gpt-5.6-sol");
        assert_eq!(record.session_id, "019fbbc1-b12c-7360-a685-28c181f0025f");
        // Codex reports input_tokens inclusive of the cached portion.
        assert_eq!(record.totals.uncached_input_tokens, 19239 - 11008);
        assert_eq!(record.totals.cached_input_tokens, 11008);
        assert_eq!(record.totals.reasoning_tokens, 116);
    }

    #[test]
    fn skips_a_repeated_token_count_so_deltas_are_not_double_counted() {
        let mut state = initial_codex_scan_state();
        parse_codex_line(&turn_context(), &mut state);
        let first = parse_codex_line(&token_count(100, 0, 10, 0), &mut state);
        let repeat = parse_codex_line(&token_count(100, 0, 10, 0), &mut state);
        assert!(first.is_some());
        assert!(repeat.is_none());
    }

    #[test]
    fn drops_usage_that_arrives_before_any_model_is_known() {
        let mut state = initial_codex_scan_state();
        assert!(parse_codex_line(&token_count(100, 0, 10, 0), &mut state).is_none());
    }

    #[test]
    fn does_not_let_a_pre_model_event_poison_the_duplicate_signature() {
        // A token_count before its turn_context is dropped; the identical event
        // re-emitted once the model is known must still be counted.
        let mut state = initial_codex_scan_state();
        assert!(parse_codex_line(&token_count(100, 0, 10, 0), &mut state).is_none());
        parse_codex_line(&turn_context(), &mut state);
        assert!(parse_codex_line(&token_count(100, 0, 10, 0), &mut state).is_some());
    }

    #[test]
    fn carries_the_service_tier_from_the_latest_thread_settings() {
        let settings = |thread_settings: Value| {
            json!({
                "type": "event_msg",
                "timestamp": "2026-08-01T05:17:42.000Z",
                "payload": { "type": "thread_settings_applied", "thread_settings": thread_settings },
            })
            .to_string()
        };
        let mut state = initial_codex_scan_state();
        parse_codex_line(&turn_context(), &mut state);
        let mut speed_after = |line: String, output: u64| {
            parse_codex_line(&line, &mut state);
            parse_codex_line(&token_count(100, 0, output, 0), &mut state).map(|r| r.speed)
        };
        assert_eq!(
            speed_after(settings(json!({ "service_tier": "default" })), 1),
            Some(UsageSpeed::Standard)
        );
        assert_eq!(
            speed_after(settings(json!({ "service_tier": "ultrafast" })), 2),
            Some(UsageSpeed::Ultrafast)
        );
        assert_eq!(
            speed_after(settings(json!({ "service_tier": "priority" })), 3),
            Some(UsageSpeed::Fast)
        );
        // Codex omits the field when no tier was requested.
        assert_eq!(
            speed_after(settings(json!({ "model": "gpt-6-astra" })), 4),
            Some(UsageSpeed::Standard)
        );
    }

    // A forked/subagent rollout opens with the parent's history copied in and
    // every line re-stamped to the fork instant, then the ancestors' session
    // metas. Counting those again multiplied usage ~1.85x on real data (#5758).
    mod forked_rollouts {
        use super::*;

        fn meta(
            id: &str,
            timestamp: &str,
            forked_from_id: Option<&str>,
            spawn_parent: Option<&str>,
        ) -> String {
            let mut payload = json!({ "type": "session_meta", "id": id });
            if let Some(parent) = forked_from_id {
                payload["forked_from_id"] = json!(parent);
            }
            if let Some(parent) = spawn_parent {
                payload["source"] =
                    json!({ "subagent": { "thread_spawn": { "parent_thread_id": parent } } });
            }
            json!({ "type": "session_meta", "timestamp": timestamp, "payload": payload })
                .to_string()
        }

        fn stamped(timestamp: &str, line: String) -> String {
            let mut parsed: Value = serde_json::from_str(&line).expect("json");
            parsed["timestamp"] = json!(timestamp);
            parsed.to_string()
        }

        #[test]
        fn keeps_the_child_session_id_over_copied_ancestor_metas() {
            let mut state = initial_codex_scan_state();
            parse_codex_line(
                &meta("child", "2026-08-01T05:00:00.000Z", None, None),
                &mut state,
            );
            parse_codex_line(
                &meta("parent", "2026-08-01T05:00:00.000Z", None, None),
                &mut state,
            );
            parse_codex_line(&turn_context(), &mut state);
            let record = parse_codex_line(&token_count(100, 0, 10, 0), &mut state).expect("record");
            assert_eq!(record.session_id, "child");
        }

        #[test]
        fn drops_the_restamped_copied_burst_and_keeps_the_first_real_event() {
            let mut state = initial_codex_scan_state();
            let fork_instant = "2026-08-01T05:00:00.000Z";
            parse_codex_line(
                &meta("child", fork_instant, Some("parent"), None),
                &mut state,
            );
            parse_codex_line(&meta("parent", fork_instant, None, None), &mut state);
            parse_codex_line(&stamped(fork_instant, turn_context()), &mut state);

            // Copied history: written in one burst at the fork instant.
            assert!(
                parse_codex_line(
                    &stamped("2026-08-01T05:00:00.001Z", token_count(100, 0, 10, 0)),
                    &mut state
                )
                .is_none()
            );
            assert!(
                parse_codex_line(
                    &stamped("2026-08-01T05:00:00.002Z", token_count(200, 0, 20, 0)),
                    &mut state
                )
                .is_none()
            );

            // The child's first genuine turn lands seconds later and must count.
            let real = parse_codex_line(
                &stamped("2026-08-01T05:00:06.000Z", token_count(300, 0, 30, 0)),
                &mut state,
            )
            .expect("real event");
            assert_eq!(real.totals.output_tokens, 30);

            // Suppression never restarts, even for closely spaced later events.
            assert!(
                parse_codex_line(
                    &stamped("2026-08-01T05:00:06.100Z", token_count(400, 0, 40, 0)),
                    &mut state
                )
                .is_some()
            );
        }

        #[test]
        fn recognizes_subagent_spawns_without_forked_from_id() {
            let mut state = initial_codex_scan_state();
            let spawn_instant = "2026-08-01T05:00:00.000Z";
            parse_codex_line(
                &meta("child", spawn_instant, None, Some("parent")),
                &mut state,
            );
            parse_codex_line(&stamped(spawn_instant, turn_context()), &mut state);
            assert!(
                parse_codex_line(
                    &stamped("2026-08-01T05:00:00.001Z", token_count(100, 0, 10, 0)),
                    &mut state
                )
                .is_none()
            );
        }

        #[test]
        fn does_not_suppress_anything_in_a_rollout_that_is_not_a_fork() {
            let mut state = initial_codex_scan_state();
            parse_codex_line(
                &meta("root", "2026-08-01T05:00:00.000Z", None, None),
                &mut state,
            );
            parse_codex_line(
                &stamped("2026-08-01T05:00:00.100Z", turn_context()),
                &mut state,
            );
            assert!(
                parse_codex_line(
                    &stamped("2026-08-01T05:00:00.200Z", token_count(100, 0, 10, 0)),
                    &mut state
                )
                .is_some()
            );
        }
    }

    #[test]
    fn the_format_reduces_state_carried_as_json() {
        let format = codex_usage_format();
        let state = format.state.as_ref().expect("state");
        let mut encoded = (state.initial)();
        assert!((format.parse_line)(&turn_context(), &mut encoded).is_empty());
        let records = (format.parse_line)(&token_count(100, 0, 10, 0), &mut encoded);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, "gpt-5.6-sol");
        assert!((state.decode)(&encoded).is_ok());
        assert!((state.decode)(&json!({ "model": 1 })).is_err());
    }

    #[test]
    fn the_reader_adopts_codex_home_only_for_an_existing_default_setup() {
        let instance = |config: Value| ProviderUsageInstance {
            instance_id: serde_json::from_value(json!("codex")).expect("instance id"),
            config: Some(config),
            environment: HashMap::from([("CODEX_HOME".to_owned(), "/tmp/env-codex".to_owned())]),
            configured: true,
        };
        let dir = |config: Value| codex_usage_directories(&instance(config))[0].dir.clone();
        assert_eq!(
            dir(json!({ "setupMode": "existing" })),
            "/tmp/env-codex/sessions"
        );
        assert_eq!(
            dir(json!({ "homePath": "/tmp/own-codex" })),
            "/tmp/own-codex/sessions"
        );
    }
}
