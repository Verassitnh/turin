//! Port of upstream/apps/server/src/project/AgentSessionScanner.ts: the model
//! types and the `recentThreads` seam. The transcript scan itself follows in
//! later slices of this module.

use std::collections::{HashSet, VecDeque};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Deserialize;
use turin_contracts::agent_sessions::{
    AgentSessionImportSource, AgentSessionScanError, AgentSessionScanResult, AgentSessionSource,
};
use turin_contracts::base_schemas::{UtcDateTime, js_trim};
use turin_contracts::provider_instance::ProviderInstanceId;

/// Author of a transcript message. Upstream `"user" | "assistant"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionMessageRole {
    User,
    Assistant,
}

/// One user or assistant message recovered from a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSessionThreadMessage {
    pub role: AgentSessionMessageRole,
    pub text: String,
    /// ISO-8601, normalized by the scanner (`normalizeTimestamp`).
    pub created_at: String,
}

/// A transcript read back into a thread the importer can write.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSessionThread {
    pub source: AgentSessionSource,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: String,
    pub title: String,
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<AgentSessionThreadMessage>,
}

/// What the scanner decided about one transcript. Upstream `AgentSessionRecentThread`.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentSessionRecentThread {
    /// New transcript: the importer should write it as a thread.
    Importable {
        thread: AgentSessionThread,
        source: AgentSessionImportSource,
    },
    /// Its file identity matches a source an earlier import completed.
    AlreadyImported {
        source: AgentSessionImportSource,
    },
    /// Same provider session appears under another file in this scan.
    Duplicate {
        source: AgentSessionImportSource,
    },
    Skipped,
}

/// Transcript discovery. The scanner is best-effort: unreadable transcripts
/// are `Skipped`, and only a failing read of the scan itself becomes an error.
#[async_trait]
pub trait AgentSessionScanner: Send + Sync {
    /// Every directory the configured homes ran a session in, newest first.
    /// Upstream `scan`.
    async fn scan(&self) -> Result<AgentSessionScanResult, AgentSessionScanError>;

    /// Recent transcripts whose `cwd` is `workspace_root`, newest first.
    /// `completed_sources` are the file identities already imported for it.
    fn recent_threads<'a>(
        &'a self,
        workspace_root: &'a str,
        completed_sources: &'a [AgentSessionImportSource],
    ) -> BoxStream<'a, Result<AgentSessionRecentThread, AgentSessionScanError>>;
}

/// Upstream `MAX_IMPORTED_MESSAGES`: the retained window of a long transcript.
const MAX_IMPORTED_MESSAGES: usize = 200;
/// Upstream `MAX_IMPORT_RECORDS`: a transcript with more lines is not imported.
pub(crate) const MAX_IMPORT_RECORDS: usize = 100_000;

/// What the parser needs besides the transcript text.
pub struct AgentSessionTranscriptMetadata<'a> {
    pub source: AgentSessionSource,
    pub provider_instance_id: ProviderInstanceId,
    /// The session id for Claude, whose file names are session ids.
    pub fallback_session_id: &'a str,
    pub last_active_at_ms: i64,
}

/// Upstream `TranscriptContentBlock`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TranscriptContentBlock {
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) text: Option<String>,
}

/// A message's `content`: a plain string, or a list of blocks.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum TranscriptMessageContent {
    Text(String),
    Blocks(Vec<TranscriptContentBlock>),
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TranscriptMessage {
    pub(crate) content: Option<TranscriptMessageContent>,
    pub(crate) model: Option<String>,
}

/// Codex `internal_chat_message_metadata_passthrough`, the part that names the turn.
#[derive(Debug, Clone, Deserialize)]
struct CodexTurnMetadata {
    turn_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscriptPayload {
    pub(crate) id: Option<String>,
    pub(crate) cwd: Option<String>,
    #[serde(rename = "session_id")]
    pub(crate) native_session_id: Option<String>,
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) content: Option<Vec<TranscriptContentBlock>>,
    #[serde(rename = "internal_chat_message_metadata_passthrough")]
    pub(crate) turn_metadata: Option<serde_json::Value>,
}

/// One JSONL record, decoded leniently: unknown keys are ignored, and a
/// known key with the wrong shape fails the whole record (upstream
/// `Schema.Struct` behaves the same).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscriptRecord {
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) timestamp: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) ai_title: Option<String>,
    is_sidechain: Option<bool>,
    is_meta: Option<bool>,
    is_compact_summary: Option<bool>,
    pub(crate) message: Option<TranscriptMessage>,
    pub(crate) payload: Option<TranscriptPayload>,
}

/// A message kept while parsing. `seq` gives it an identity: upstream compares
/// and removes specific message objects.
#[derive(Debug, Clone)]
struct PendingMessage {
    seq: u64,
    role: AgentSessionMessageRole,
    text: String,
    created_at: String,
    codex_response_user: bool,
}

/// A Codex user response item, kept until its turn ends.
struct ResponseUser {
    index: usize,
    turn_id: String,
    text: String,
}

/// The message window and the first prompt. Upstream keeps the first user
/// message even after it has left the window, so it is held apart from it.
#[derive(Default)]
struct MessageWindow {
    messages: VecDeque<PendingMessage>,
    next_seq: u64,
    first_user: Option<PendingMessage>,
}

impl MessageWindow {
    fn retain(
        &mut self,
        role: AgentSessionMessageRole,
        text: String,
        created_at: String,
        codex_response_user: bool,
    ) {
        let message = PendingMessage {
            seq: self.next_seq,
            role,
            text,
            created_at,
            codex_response_user,
        };
        self.next_seq += 1;
        if self.first_user.is_none() && role == AgentSessionMessageRole::User {
            self.first_user = Some(message.clone());
        }
        self.messages.push_back(message);
        if self.messages.len() > MAX_IMPORTED_MESSAGES {
            self.messages.pop_front();
        }
    }

    /// Codex wrote a prompt as both a response item and an event. Removes the
    /// response copy of `text`, searching back to the last assistant message.
    fn remove_response_copy(&mut self, text: &str) {
        let comparison = js_trim(text);
        for index in (0..self.messages.len()).rev() {
            let message = &self.messages[index];
            if message.role == AgentSessionMessageRole::Assistant {
                break;
            }
            if message.codex_response_user && js_trim(&message.text) == comparison {
                if self.first_user.as_ref().map(|first| first.seq) == Some(message.seq) {
                    self.first_user = None;
                }
                self.messages.remove(index);
                break;
            }
        }
    }

    /// Upstream `hasMatchingCodexEventInTurn`.
    fn has_matching_codex_event_in_turn(&self, text: &str) -> bool {
        let comparison = js_trim(text);
        for message in self.messages.iter().rev() {
            if message.role == AgentSessionMessageRole::Assistant {
                return false;
            }
            if message.role == AgentSessionMessageRole::User
                && !message.codex_response_user
                && js_trim(&message.text) == comparison
            {
                return true;
            }
        }
        false
    }
}

/// Codex response users of the current turn, and the canonical event texts
/// seen in it. Upstream `finishCodexTurn` closes over these.
#[derive(Default)]
struct CodexTurnState {
    canonical_user_texts: HashSet<String>,
    response_users: Vec<ResponseUser>,
}

impl CodexTurnState {
    /// Marks the response users whose turn has a verbatim event copy. Their
    /// records are then suppressed as generated context.
    fn finish(&mut self, canonical_indices: &mut HashSet<usize>) {
        let canonical_turn_ids: HashSet<&str> = self
            .response_users
            .iter()
            .filter(|response_user| self.canonical_user_texts.contains(&response_user.text))
            .map(|response_user| response_user.turn_id.as_str())
            .collect();
        for response_user in &self.response_users {
            if canonical_turn_ids.contains(response_user.turn_id.as_str()) {
                canonical_indices.insert(response_user.index);
            }
        }
        self.canonical_user_texts.clear();
        self.response_users.clear();
    }
}

fn js_trim_owned(value: &str) -> String {
    js_trim(value).to_owned()
}

/// Upstream `extractText` over a message's `content` (string or blocks).
fn extract_message_text(content: Option<&TranscriptMessageContent>) -> String {
    match content {
        None => String::new(),
        Some(TranscriptMessageContent::Text(text)) => js_trim_owned(text),
        Some(TranscriptMessageContent::Blocks(blocks)) => extract_blocks(blocks),
    }
}

/// Visible text of content blocks: text-like blocks only, each trimmed,
/// empty ones dropped, joined with newlines.
fn extract_blocks(blocks: &[TranscriptContentBlock]) -> String {
    blocks
        .iter()
        .filter(|block| {
            matches!(
                block.kind.as_deref(),
                Some("text" | "input_text" | "output_text")
            )
        })
        .map(|block| js_trim_owned(block.text.as_deref().unwrap_or("")))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Upstream `normalizeTimestamp`: canonical form when parseable, else the fallback.
fn normalize_timestamp(value: Option<&str>, fallback: &str) -> String {
    match value {
        None => fallback.to_owned(),
        Some(value) => value
            .parse::<UtcDateTime>()
            .map(|at| at.to_string())
            .unwrap_or_else(|_| fallback.to_owned()),
    }
}

/// Upstream `codexTurnId`: a non-blank turn id, or `None`.
fn codex_turn_id(metadata: Option<&serde_json::Value>) -> Option<String> {
    let metadata = serde_json::from_value::<CodexTurnMetadata>(metadata?.clone()).ok()?;
    let turn_id = metadata.turn_id?;
    (!js_trim(&turn_id).is_empty()).then_some(turn_id)
}

/// Upstream `String.slice(0, units)`, which counts UTF-16 code units. A cut
/// through a surrogate pair becomes U+FFFD.
fn utf16_prefix(value: &str, units: usize) -> String {
    let encoded: Vec<u16> = value.encode_utf16().take(units).collect();
    String::from_utf16_lossy(&encoded)
}

/// Parses one transcript into a thread. `None` when it has no resumable
/// session id, no visible user message, or too many records. Upstream
/// `parseAgentSessionTranscript`.
pub fn parse_agent_session_transcript(
    input: &AgentSessionTranscriptMetadata<'_>,
    contents: &str,
) -> Option<AgentSessionThread> {
    // One line past the limit is enough to know the transcript is too long.
    let lines: Vec<&str> = contents
        .strip_suffix('\n')
        .unwrap_or(contents)
        .split('\n')
        .take(MAX_IMPORT_RECORDS + 1)
        .collect();
    if lines.len() > MAX_IMPORT_RECORDS {
        return None;
    }
    let records: Vec<TranscriptRecord> = lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    parse_records(input, &records)
}

pub(crate) fn parse_records(
    input: &AgentSessionTranscriptMetadata<'_>,
    records: &[TranscriptRecord],
) -> Option<AgentSessionThread> {
    let fallback_timestamp = UtcDateTime::from_datetime(chrono::DateTime::from_timestamp_millis(
        input.last_active_at_ms,
    )?)
    .to_string();
    let is_codex = input.source == AgentSessionSource::Codex;
    // Claude filenames are session ids. Codex rollout filenames carry extra
    // timestamp text, so only transcript metadata gives a resumable id.
    let mut provider_session_id = if is_codex {
        String::new()
    } else {
        input.fallback_session_id.to_owned()
    };
    let mut title: Option<String> = None;
    let mut model: Option<String> = None;
    let mut has_codex_session_id = false;
    let mut window = MessageWindow::default();

    let mut canonical_response_user_indices = HashSet::<usize>::new();
    if is_codex {
        let mut turn = CodexTurnState::default();
        for (record_index, record) in records.iter().enumerate() {
            let payload = record.payload.as_ref();
            let payload_kind = payload.and_then(|payload| payload.kind.as_deref());
            let payload_role = payload.and_then(|payload| payload.role.as_deref());
            if record.kind.as_deref() == Some("response_item")
                && payload_kind == Some("message")
                && payload_role == Some("assistant")
            {
                turn.finish(&mut canonical_response_user_indices);
                continue;
            }
            if record.kind.as_deref() == Some("event_msg") && payload_kind == Some("user_message") {
                let text = js_trim_owned(
                    payload
                        .and_then(|payload| payload.message.as_deref())
                        .unwrap_or(""),
                );
                if !text.is_empty() {
                    turn.canonical_user_texts.insert(text);
                }
                continue;
            }
            if record.kind.as_deref() == Some("response_item")
                && payload_kind == Some("message")
                && payload_role == Some("user")
            {
                let Some(payload) = payload else { continue };
                let turn_id = codex_turn_id(payload.turn_metadata.as_ref());
                let text = extract_blocks(payload.content.as_deref().unwrap_or(&[]));
                if let Some(turn_id) = turn_id
                    && !text.is_empty()
                {
                    turn.response_users.push(ResponseUser {
                        index: record_index,
                        turn_id,
                        text,
                    });
                }
            }
        }
        turn.finish(&mut canonical_response_user_indices);
    }

    for (record_index, record) in records.iter().enumerate() {
        let created_at = || normalize_timestamp(record.timestamp.as_deref(), &fallback_timestamp);
        if !is_codex {
            if record.is_sidechain == Some(true)
                || record.is_meta == Some(true)
                || record.is_compact_summary == Some(true)
            {
                continue;
            }
            if let Some(session_id) = record
                .session_id
                .as_deref()
                .map(js_trim)
                .filter(|s| !s.is_empty())
            {
                provider_session_id = session_id.to_owned();
            }
            if let Some(ai_title) = record
                .ai_title
                .as_deref()
                .map(js_trim)
                .filter(|s| !s.is_empty())
            {
                title = Some(ai_title.to_owned());
            }
            let message_model = record
                .message
                .as_ref()
                .and_then(|message| message.model.as_deref())
                .map(js_trim)
                .filter(|model| !model.is_empty());
            // Claude uses this sentinel for local error responses. It is not a
            // model id that can be selected when the imported session resumes.
            if let Some(message_model) = message_model
                && message_model != "<synthetic>"
            {
                model = Some(message_model.to_owned());
            }
            let role = match record.kind.as_deref() {
                Some("user") => AgentSessionMessageRole::User,
                Some("assistant") => AgentSessionMessageRole::Assistant,
                _ => continue,
            };
            let text =
                extract_message_text(record.message.as_ref().and_then(|m| m.content.as_ref()));
            if text.is_empty() {
                continue;
            }
            window.retain(role, text, created_at(), false);
            continue;
        }

        let payload = record.payload.as_ref();
        let payload_kind = payload.and_then(|payload| payload.kind.as_deref());
        if record.kind.as_deref() == Some("session_meta") {
            let session_id = payload
                .and_then(|payload| payload.id.as_deref().map(js_trim).filter(|s| !s.is_empty()))
                .or_else(|| {
                    payload.and_then(|payload| {
                        payload
                            .native_session_id
                            .as_deref()
                            .map(js_trim)
                            .filter(|s| !s.is_empty())
                    })
                });
            if !has_codex_session_id && let Some(session_id) = session_id {
                provider_session_id = session_id.to_owned();
                has_codex_session_id = true;
            }
            continue;
        }
        if record.kind.as_deref() == Some("turn_context")
            && let Some(turn_model) = payload
                .and_then(|payload| payload.model.as_deref())
                .map(js_trim)
                .filter(|model| !model.is_empty())
        {
            model = Some(turn_model.to_owned());
            continue;
        }
        if record.kind.as_deref() == Some("event_msg") && payload_kind == Some("user_message") {
            let text = payload
                .and_then(|payload| payload.message.clone())
                .unwrap_or_default();
            if js_trim(&text).is_empty() {
                continue;
            }
            window.remove_response_copy(&text);
            window.retain(AgentSessionMessageRole::User, text, created_at(), false);
            continue;
        }
        let Some(payload) = payload else { continue };
        if record.kind.as_deref() != Some("response_item")
            || payload.kind.as_deref() != Some("message")
        {
            continue;
        }
        let role = match payload.role.as_deref() {
            Some("user") => AgentSessionMessageRole::User,
            Some("assistant") => AgentSessionMessageRole::Assistant,
            _ => continue,
        };
        let extracted_text = extract_blocks(payload.content.as_deref().unwrap_or(&[]));
        if extracted_text.is_empty() {
            continue;
        }
        if role == AgentSessionMessageRole::User
            && (canonical_response_user_indices.contains(&record_index)
                || window.has_matching_codex_event_in_turn(&extracted_text))
        {
            continue;
        }
        let codex_response_user = role == AgentSessionMessageRole::User;
        window.retain(role, extracted_text, created_at(), codex_response_user);
    }

    if js_trim(&provider_session_id).is_empty() {
        return None;
    }
    let first_user = window.first_user.clone()?;
    let visible = |message: &PendingMessage| AgentSessionThreadMessage {
        role: message.role,
        text: message.text.clone(),
        created_at: message.created_at.clone(),
    };
    let visible_first = visible(&first_user);
    let visible_messages: Vec<AgentSessionThreadMessage> =
        window.messages.iter().map(visible).collect();
    // Keeps the first prompt in front of the newest window when it was dropped.
    let first_retained = window
        .messages
        .iter()
        .any(|message| message.seq == first_user.seq);
    let messages = if first_retained {
        visible_messages
    } else {
        let tail_start = visible_messages
            .len()
            .saturating_sub(MAX_IMPORTED_MESSAGES - 1);
        std::iter::once(visible_first.clone())
            .chain(visible_messages[tail_start..].iter().cloned())
            .collect()
    };
    let derived_title = utf16_prefix(
        js_trim(&visible_first.text)
            .split('\n')
            .next()
            .unwrap_or(""),
        100,
    );
    let derived_title = js_trim(&derived_title);

    Some(AgentSessionThread {
        source: input.source,
        provider_instance_id: input.provider_instance_id.clone(),
        provider_session_id,
        title: title.unwrap_or_else(|| {
            if derived_title.is_empty() {
                "Imported thread".to_owned()
            } else {
                derived_title.to_owned()
            }
        }),
        model,
        created_at: messages
            .first()
            .map(|message| message.created_at.clone())
            .unwrap_or_else(|| fallback_timestamp.clone()),
        updated_at: fallback_timestamp,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;

    const LAST_ACTIVE: &str = "2026-08-24T12:00:00.000Z";

    fn at_ms(value: &str) -> i64 {
        value
            .parse::<UtcDateTime>()
            .expect("test timestamp parses")
            .as_datetime()
            .timestamp_millis()
    }

    fn lines(records: &[Value]) -> String {
        records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn parse(
        source: AgentSessionSource,
        fallback: &str,
        contents: &str,
        last_active: &str,
    ) -> Option<AgentSessionThread> {
        let provider_instance_id = ProviderInstanceId::new(match source {
            AgentSessionSource::Codex => "codex",
            AgentSessionSource::ClaudeAgent => "claudeAgent",
        })
        .expect("slug");
        parse_agent_session_transcript(
            &AgentSessionTranscriptMetadata {
                source,
                provider_instance_id,
                fallback_session_id: fallback,
                last_active_at_ms: at_ms(last_active),
            },
            contents,
        )
    }

    fn codex(contents: &str) -> Option<AgentSessionThread> {
        parse(AgentSessionSource::Codex, "fallback", contents, LAST_ACTIVE)
    }

    fn texts(thread: &Option<AgentSessionThread>) -> Vec<String> {
        thread
            .as_ref()
            .map(|thread| thread.messages.iter().map(|m| m.text.clone()).collect())
            .unwrap_or_default()
    }

    fn session_meta(id: &str) -> Value {
        json!({ "type": "session_meta", "payload": { "id": id } })
    }

    fn event_user(message: &str) -> Value {
        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": message } })
    }

    fn response_user(text: &str, turn: Option<&str>) -> Value {
        let mut payload = json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }],
        });
        if let Some(turn) = turn {
            payload["internal_chat_message_metadata_passthrough"] = json!({ "turn_id": turn });
        }
        json!({ "type": "response_item", "payload": payload })
    }

    fn response_assistant(text: &str) -> Value {
        json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }],
            },
        })
    }

    fn record_limit_transcript(overflow: bool) -> String {
        let mut contents = lines(&[
            json!({ "type": "session_meta", "payload": { "id": "record-limit-session", "cwd": "/project" } }),
            event_user("First prompt"),
        ]);
        contents.push('\n');
        contents.push_str(&"{}\n".repeat(99_998));
        if overflow {
            contents.push('\n');
            contents.push_str(&event_user("Overflow prompt").to_string());
            contents.push('\n');
        }
        contents
    }

    #[test]
    fn handles_the_exact_record_limit_and_an_interior_blank() {
        let thread = codex(&record_limit_transcript(false));
        assert_eq!(texts(&thread), ["First prompt"]);
        assert!(codex(&record_limit_transcript(true)).is_none());
    }

    #[test]
    fn keeps_claude_text_and_titles_while_dropping_malformed_and_tool_records() {
        let contents = [
            "not valid json".to_owned(),
            json!({ "type": "ai-title", "aiTitle": "Fix authentication" }).to_string(),
            json!({
                "type": "user", "sessionId": "claude-session", "isMeta": true,
                "message": { "role": "user", "content": "Injected skill instructions" },
            })
            .to_string(),
            json!({
                "type": "user", "sessionId": "claude-session", "isCompactSummary": true,
                "message": { "role": "user", "content": "Injected compaction summary" },
            })
            .to_string(),
            json!({
                "type": "user", "sessionId": "claude-session", "timestamp": "2026-08-24T10:00:00.000Z",
                "message": { "role": "user", "content": [{ "type": "text", "text": "Fix authentication" }] },
            })
            .to_string(),
            json!({
                "type": "user", "sessionId": "claude-session",
                "message": { "role": "user", "content": [{ "type": "tool_result", "text": "hidden" }] },
            })
            .to_string(),
            json!({
                "type": "assistant", "sessionId": "claude-session",
                "message": {
                    "role": "assistant", "model": "claude-sonnet-5",
                    "content": [{ "type": "text", "text": "Updated the login flow" }],
                },
            })
            .to_string(),
            json!({
                "type": "assistant", "sessionId": "claude-session",
                "message": {
                    "role": "assistant", "model": "<synthetic>",
                    "content": [{ "type": "text", "text": "The provider request failed" }],
                },
            })
            .to_string(),
        ]
        .join("\n");
        let thread = parse(
            AgentSessionSource::ClaudeAgent,
            "fallback",
            &contents,
            LAST_ACTIVE,
        )
        .expect("thread");
        assert_eq!(thread.provider_session_id, "claude-session");
        assert_eq!(thread.title, "Fix authentication");
        assert_eq!(thread.model.as_deref(), Some("claude-sonnet-5"));
        let messages: Vec<(AgentSessionMessageRole, &str)> = thread
            .messages
            .iter()
            .map(|m| (m.role, m.text.as_str()))
            .collect();
        assert_eq!(
            messages,
            [
                (AgentSessionMessageRole::User, "Fix authentication"),
                (AgentSessionMessageRole::Assistant, "Updated the login flow"),
                (
                    AgentSessionMessageRole::Assistant,
                    "The provider request failed"
                ),
            ]
        );
    }

    #[test]
    fn drops_injected_codex_instructions_while_keeping_the_visible_user_event() {
        let thread = codex(&lines(&[
            session_meta("codex-session"),
            response_user(
                "<user_instructions>\nInternal setup instructions\n</user_instructions>",
                Some("turn-1"),
            ),
            event_user("Fix the actual bug"),
            response_user("Fix the actual bug", Some("turn-1")),
            response_assistant("Fixed"),
        ]));
        assert_eq!(texts(&thread), ["Fix the actual bug", "Fixed"]);
    }

    #[test]
    fn keeps_the_canonical_first_prompt_after_long_codex_transcripts_are_capped() {
        let canonical_prompt = "\n  Keep the canonical prompt  \n";
        let mut records = vec![
            session_meta("codex-session"),
            json!({
                "type": "response_item", "timestamp": "2026-08-24T10:00:00.000Z",
                "payload": {
                    "type": "message", "role": "user",
                    "content": [{ "type": "input_text", "text": "Keep the canonical prompt" }],
                },
            }),
            json!({
                "type": "event_msg", "timestamp": "2026-08-24T10:01:00.000Z",
                "payload": { "type": "user_message", "message": canonical_prompt },
            }),
        ];
        for index in 0..200 {
            records.push(json!({
                "type": "response_item",
                "timestamp": format!("2026-08-24T11:{:02}:00.000Z", index % 60),
                "payload": {
                    "type": "message", "role": "assistant",
                    "content": [{ "type": "output_text", "text": format!("Assistant message {index}") }],
                },
            }));
        }
        let thread = codex(&lines(&records)).expect("thread");
        assert_eq!(thread.messages.len(), 200);
        assert_eq!(thread.messages[0].role, AgentSessionMessageRole::User);
        assert_eq!(thread.messages[0].text, canonical_prompt);
        assert_eq!(thread.messages[0].created_at, "2026-08-24T10:01:00.000Z");
    }

    #[test]
    fn restores_the_canonical_first_prompt_when_a_later_user_message_remains() {
        let canonical_prompt = "\n  Keep the canonical prompt  \n";
        let mut records = vec![
            session_meta("codex-session"),
            response_user("Keep the canonical prompt", Some("turn-1")),
            json!({
                "type": "event_msg", "timestamp": "2026-08-24T10:01:00.000Z",
                "payload": { "type": "user_message", "message": canonical_prompt },
            }),
        ];
        for index in 0..198 {
            records.push(json!({
                "type": "response_item",
                "timestamp": format!("2026-08-24T11:{:02}:00.000Z", index % 60),
                "payload": {
                    "type": "message", "role": "assistant",
                    "content": [{ "type": "output_text", "text": format!("Assistant message {index}") }],
                },
            }));
        }
        records.push(json!({
            "type": "event_msg", "timestamp": "2026-08-24T11:58:30.000Z",
            "payload": { "type": "user_message", "message": "Keep this later prompt" },
        }));
        records.push(json!({
            "type": "response_item", "timestamp": "2026-08-24T11:59:00.000Z",
            "payload": {
                "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": "Keep this latest response" }],
            },
        }));
        let thread = codex(&lines(&records)).expect("thread");
        assert_eq!(thread.messages.len(), 200);
        assert_eq!(thread.messages[0].text, canonical_prompt);
        assert_eq!(thread.messages[0].created_at, "2026-08-24T10:01:00.000Z");
        assert_eq!(
            thread
                .messages
                .iter()
                .filter(|m| js_trim(&m.text) == js_trim(canonical_prompt))
                .count(),
            1
        );
        assert!(
            thread
                .messages
                .iter()
                .any(|m| m.text == "Keep this later prompt")
        );
        assert_eq!(
            thread.messages.last().map(|m| m.text.as_str()),
            Some("Keep this latest response")
        );
    }

    #[test]
    fn keeps_mixed_format_response_users_when_turn_ids_repeat_after_an_assistant() {
        let thread = codex(&lines(&[
            session_meta("codex-session"),
            response_user("Keep this older prompt", Some("turn-older")),
            event_user("Keep this newer prompt"),
            response_user("Keep this newer prompt", Some("turn-newer")),
            response_assistant("Ask again when needed"),
            response_user("Keep this newer prompt", Some("turn-newer")),
        ]));
        assert_eq!(
            texts(&thread),
            [
                "Keep this older prompt",
                "Keep this newer prompt",
                "Ask again when needed",
                "Keep this newer prompt",
            ]
        );
    }

    #[test]
    fn preserves_response_user_text_when_codex_turn_metadata_is_ambiguous() {
        let legacy = json!({
            "type": "response_item",
            "payload": {
                "type": "message", "role": "user",
                "internal_chat_message_metadata_passthrough": ["unexpected"],
                "content": [{ "type": "input_text", "text": "Keep this legacy prompt" }],
            },
        });
        let blank = json!({
            "type": "response_item",
            "payload": {
                "type": "message", "role": "user",
                "internal_chat_message_metadata_passthrough": { "turn_id": "   " },
                "content": [{ "type": "input_text", "text": "Keep this prompt with a blank turn ID" }],
            },
        });
        let thread = codex(&lines(&[session_meta("codex-session"), legacy, blank]));
        assert_eq!(
            texts(&thread),
            [
                "Keep this legacy prompt",
                "Keep this prompt with a blank turn ID"
            ]
        );
    }

    #[test]
    fn uses_the_first_valid_codex_session_id_when_a_fork_copies_ancestor_metadata() {
        let thread = codex(&lines(&[
            json!({ "type": "session_meta", "payload": { "id": "fork-session", "forked_from_id": "parent-session" } }),
            session_meta("parent-session"),
            event_user("Continue in the fork"),
        ]))
        .expect("thread");
        assert_eq!(thread.provider_session_id, "fork-session");
    }

    #[test]
    fn skips_codex_transcripts_without_a_resumable_session_id() {
        let thread = codex(&lines(&[event_user(
            "This transcript has no session metadata",
        )]));
        assert!(thread.is_none());
    }

    #[test]
    fn uses_the_canonical_codex_event_when_its_turn_has_generated_response_context() {
        let thread = codex(&lines(&[
            session_meta("codex-session"),
            response_user(
                "<environment_context>\n<cwd>/tmp/project</cwd>\n<shell>zsh</shell>\n</environment_context>",
                Some("turn-1"),
            ),
            response_user(
                "# AGENTS.md instructions for /tmp/project\n\n<INSTRUCTIONS>\nPrivate project rules\n</INSTRUCTIONS>",
                Some("turn-1"),
            ),
            event_user("Do something here so it looks like a real project."),
            response_user(
                "Do something here so it looks like a real project.",
                Some("turn-1"),
            ),
            response_assistant("Created the project."),
        ]));
        let thread = thread.expect("thread");
        assert_eq!(
            thread.title,
            "Do something here so it looks like a real project."
        );
        assert_eq!(
            texts(&Some(thread)),
            [
                "Do something here so it looks like a real project.",
                "Created the project.",
            ]
        );
    }

    #[test]
    fn preserves_context_markup_in_response_only_codex_messages() {
        let context = "<environment_context>\n<cwd>/tmp/project</cwd>\n</environment_context>";
        let thread = codex(&lines(&[
            session_meta("codex-session"),
            response_user(context, None),
            response_user("Initialize Git and add a README.", None),
        ]))
        .expect("thread");
        assert_eq!(thread.title, "<environment_context>");
        assert_eq!(
            texts(&Some(thread)),
            [context, "Initialize Git and add a README."]
        );
    }

    #[test]
    fn preserves_a_canonical_codex_event_that_starts_with_context_markup() {
        let prompt = "<environment_context>\n<cwd>/tmp/project</cwd>\n</environment_context>\n\nCreate a useful project.";
        let thread =
            codex(&lines(&[session_meta("codex-session"), event_user(prompt)])).expect("thread");
        assert_eq!(thread.title, "<environment_context>");
        assert_eq!(texts(&Some(thread)), [prompt]);
    }

    #[test]
    fn preserves_a_codex_request_heading_in_a_canonical_event() {
        let prompt = "\n  ## My request for Codex:\n\nFix the visible bug";
        let thread =
            codex(&lines(&[session_meta("codex-session"), event_user(prompt)])).expect("thread");
        assert_eq!(thread.title, "## My request for Codex:");
        assert_eq!(texts(&Some(thread)), [prompt]);
    }

    #[test]
    fn keeps_context_markup_quoted_inside_visible_codex_user_text() {
        let quoted = "Do not remove this example:\n<environment_context>\n<cwd>/tmp/example</cwd>\n</environment_context>";
        let thread = codex(&lines(&[session_meta("codex-session"), event_user(quoted)]));
        assert_eq!(texts(&thread), [quoted]);
    }

    #[test]
    fn skips_sessions_without_a_visible_user_message() {
        let thread = parse(
            AgentSessionSource::ClaudeAgent,
            "claude-session",
            &json!({ "type": "assistant", "message": { "role": "assistant", "content": "Done" } })
                .to_string(),
            LAST_ACTIVE,
        );
        assert!(thread.is_none());
    }

    #[test]
    fn keeps_the_first_prompt_when_later_assistant_output_exceeds_the_message_limit() {
        let mut records = vec![json!({
            "type": "user", "sessionId": "claude-session",
            "message": { "role": "user", "content": "Keep this prompt" },
        })];
        for index in 0..250 {
            records.push(json!({
                "type": "assistant",
                "message": { "role": "assistant", "content": format!("Assistant update {index}") },
            }));
        }
        let thread = parse(
            AgentSessionSource::ClaudeAgent,
            "fallback",
            &lines(&records),
            LAST_ACTIVE,
        )
        .expect("thread");
        assert_eq!(thread.messages.len(), 200);
        assert_eq!(thread.messages[0].text, "Keep this prompt");
        assert_eq!(thread.messages[199].text, "Assistant update 249");
    }
}
