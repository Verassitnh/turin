//! Port of upstream/apps/server/src/usage/usageTranscriptReader.ts.
//!
//! Raw filesystem access for transcript scanning.
//!
//! Transcripts are append-only, so a parse also reports the byte position it
//! stopped at. A later scan of the same file resumes from that position and
//! parses only the appended bytes, which is what keeps a warm scan cheap while a
//! session is actively writing a multi-hundred-megabyte rollout.
//!
//! Deviation: upstream streams lines over 8 MiB through a JSON projection
//! reader (`AgentSessionJson`, N2). Here every line is read whole and handed to
//! `parse_projected` after a full `serde_json` parse. Records are identical;
//! peak memory on a huge single line is higher. See PORTING.md.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use futures::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use turin_provider_core::usage::{TranscriptUsageFormat, UsageRecord};

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ms: f64,
}

/// Where a parse stopped, with enough state to continue from there.
///
/// The guard hash fingerprints the bytes immediately before `resume_offset`. A
/// resume only proceeds when those bytes still match: transcripts are
/// append-only by design, but a rotated or rewritten file silently mis-parsed
/// from the middle would corrupt usage totals. The window is a cheap tripwire
/// for those realistic failure shapes, all of which disturb the file's tail at
/// that exact offset; it deliberately does not hash the whole prefix, which
/// would cost the full re-read the resume exists to avoid.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptParsePosition {
    /// Byte offset just past the last newline-terminated line consumed.
    pub resume_offset: u64,
    /// Length of the fingerprinted window ending at `resume_offset`.
    pub guard_length: u64,
    /// FNV-1a hash of that window.
    pub guard_hash: u32,
    /// The format's reducer state as of `resume_offset`, encoded; `Null` for stateless formats.
    pub state: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptParseResult {
    /// Records from newline-terminated lines at or after the parse start.
    pub records: Vec<UsageRecord>,
    /// Records from a trailing segment the writer has not newline-terminated yet.
    /// Kept out of `records` because `position` deliberately excludes that
    /// segment: the next scan re-reads it once the writer finishes the line.
    pub tail_records: Vec<UsageRecord>,
    pub position: TranscriptParsePosition,
    /// Whether the parse continued from `resume_from` rather than byte 0.
    pub resumed: bool,
}

/// 64 bytes of JSONL tail is ample to distinguish a replaced file.
pub const GUARD_LENGTH: u64 = 64;
const NEWLINE: u8 = 0x0a;
const CARRIAGE_RETURN: u8 = 0x0d;
/// `stat` calls one transcript walk keeps in flight. The libuv pool has 4 threads.
const STAT_CONCURRENCY: usize = 32;

/// Lists `.jsonl` transcripts under `root` last modified at or after `since_ms`.
///
/// Unreadable directories and files are counted in `failed_paths` rather than
/// failing the page, so the source can report incomplete usage. Files that
/// vanish between `readdir` and `stat` are ordinary rotation and not counted.
///
/// `file_name` restricts the walk to a single basename (Grok's `updates.jsonl`).
///
/// Directories are listed depth-first, one at a time, then the candidates are
/// stat'd by a fixed pool of workers. Results keep `readdir` order, which the
/// aggregator's first-seen dedupe relies on.
pub async fn list_transcript_files(
    root: &Path,
    since_ms: f64,
    file_name: Option<&str>,
) -> (Vec<TranscriptFile>, u64) {
    let mut candidates = Vec::new();
    let mut failed_paths = 0;
    walk(root.to_path_buf(), file_name, &mut candidates, &mut failed_paths).await;

    let stats: Vec<(PathBuf, std::io::Result<Metadata>)> = futures::stream::iter(candidates)
        .map(|path| async move {
            let stats = tokio::fs::metadata(&path).await;
            (path, stats)
        })
        .buffered(STAT_CONCURRENCY)
        .collect()
        .await;

    let mut files = Vec::new();
    for (path, stats) in stats {
        match stats {
            Ok(stats) => {
                let mtime_ms = mtime_ms(&stats);
                if mtime_ms >= since_ms {
                    files.push(TranscriptFile {
                        path,
                        size: stats.len(),
                        mtime_ms,
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => failed_paths += 1,
        }
    }
    (files, failed_paths)
}

type WalkFuture<'a> = Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

fn walk<'a>(
    dir: PathBuf,
    file_name: Option<&'a str>,
    candidates: &'a mut Vec<PathBuf>,
    failed_paths: &'a mut u64,
) -> WalkFuture<'a> {
    Box::pin(async move {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(_) => {
                *failed_paths += 1;
                return;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => {
                    *failed_paths += 1;
                    break;
                }
            };
            let child = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // `DirEntry::file_type` does not follow symlinks, like `Dirent.isDirectory`.
            let is_dir = entry.file_type().await.map(|kind| kind.is_dir()).unwrap_or(false);
            if is_dir {
                walk(child, file_name, candidates, failed_paths).await;
            } else if file_name.map_or_else(|| name.ends_with(".jsonl"), |wanted| name == wanted) {
                candidates.push(child);
            }
        }
    })
}

#[cfg(unix)]
fn mtime_ms(stats: &Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    stats.mtime() as f64 * 1000.0 + stats.mtime_nsec() as f64 / 1_000_000.0
}

#[cfg(not(unix))]
fn mtime_ms(stats: &Metadata) -> f64 {
    stats
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

/// Filesystem identity of a directory, as `device:inode`.
///
/// Used to tell "two servers reading the same transcript directory" apart from
/// "two machines whose hostname and home path happen to match". Returns an empty
/// string when the directory cannot be stat'd.
pub async fn read_directory_volume_id(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match tokio::fs::metadata(path).await {
            Ok(stats) => format!("{}:{}", stats.dev(), stats.ino()),
            Err(_) => String::new(),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        String::new()
    }
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

async fn read_window(file: &mut tokio::fs::File, offset: u64, length: u64) -> Option<Vec<u8>> {
    file.seek(std::io::SeekFrom::Start(offset)).await.ok()?;
    let mut window = vec![0u8; length as usize];
    file.read_exact(&mut window).await.ok()?;
    Some(window)
}

async fn guard_matches(file: &mut tokio::fs::File, position: &TranscriptParsePosition) -> bool {
    if position.guard_length == 0 || position.guard_length > GUARD_LENGTH {
        return false;
    }
    if position.resume_offset < position.guard_length {
        return false;
    }
    match read_window(
        file,
        position.resume_offset - position.guard_length,
        position.guard_length,
    )
    .await
    {
        Some(window) => fnv1a(&window) == position.guard_hash,
        None => false,
    }
}

/// Builds the byte-level test for a format's `usageMarkers`: whether a line
/// contains any of them.
///
/// Markers are ASCII, so a byte-wise search matches exactly where the TS latin1
/// alternation does.
pub fn transcript_line_gate(markers: &[String]) -> impl Fn(&[u8]) -> bool + '_ {
    move |line: &[u8]| {
        markers
            .iter()
            .any(|marker| contains_bytes(line, marker.as_bytes()))
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|window| window == needle)
}

/// Line text as the parser sees it: a trailing CR is part of the terminator.
fn line_text(line: &[u8]) -> String {
    let content = match line.last() {
        Some(&CARRIAGE_RETURN) => &line[..line.len() - 1],
        _ => line,
    };
    String::from_utf8_lossy(content).into_owned()
}

/// Streams one transcript and returns the usage records it contains, or `None`
/// when the file could not be read.
///
/// The distinction matters to the caller's cache: a genuinely empty transcript
/// is a stable fact worth memoising, while a transient read failure memoised
/// under the same `(size, mtime)` key would silently drop that file's usage
/// until the file next changes.
///
/// With `resume_from`, parsing continues from that position when its guard bytes
/// still match, so only appended lines are read; otherwise the whole file is
/// re-parsed from the start and `resumed` reports `false`.
///
/// A stateful format resumes with the state saved at that position; a saved
/// state its decoder rejects re-parses the file whole.
pub async fn read_transcript_records(
    file_path: &Path,
    format: &TranscriptUsageFormat,
    resume_from: Option<&TranscriptParsePosition>,
) -> Option<TranscriptParseResult> {
    let mut file = tokio::fs::File::open(file_path).await.ok()?;
    let mentions_usage = transcript_line_gate(&format.usage_markers);

    let mut state = match &format.state {
        Some(codec) => (codec.initial)(),
        None => Value::Null,
    };
    let mut resumed = false;
    let mut start = 0u64;
    let saved_state = match (resume_from, &format.state) {
        (Some(position), Some(codec)) if !position.state.is_null() => {
            (codec.decode)(&position.state).ok()
        }
        _ => None,
    };
    if let Some(position) = resume_from {
        if position.resume_offset > 0
            && (format.state.is_none() || saved_state.is_some())
            && guard_matches(&mut file, position).await
        {
            if let Some(saved) = saved_state {
                state = saved;
            }
            start = position.resume_offset;
            resumed = true;
        }
    }

    file.seek(std::io::SeekFrom::Start(start)).await.ok()?;
    let mut reader = BufReader::new(file);
    let mut records = Vec::new();
    let mut resume_offset = start;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(NEWLINE, &mut line).await.ok()?;
        if read == 0 {
            break;
        }
        if line.last() != Some(&NEWLINE) {
            // Unterminated tail: parsed separately, below, against a copy of the state.
            let tail = line.clone();
            return finish_tail(
                reader.into_inner(),
                format,
                state,
                resume_offset,
                &tail,
                records,
                resumed,
            )
            .await;
        }
        if mentions_usage(&line) {
            for record in (format.parse_line)(&line_text(&line[..line.len() - 1]), &mut state) {
                records.push(record);
            }
        }
        resume_offset += read as u64;
    }

    finish_tail(reader.into_inner(), format, state, resume_offset, &[], records, resumed).await
}

/// Finishes a parse: the unfinished tail parses against a copy of the state
/// saved at `resume_offset`, so the next scan replays the tail from there.
async fn finish_tail(
    mut file: tokio::fs::File,
    format: &TranscriptUsageFormat,
    state: Value,
    resume_offset: u64,
    tail: &[u8],
    records: Vec<UsageRecord>,
    resumed: bool,
) -> Option<TranscriptParseResult> {
    let encoded_state = match &format.state {
        Some(codec) => (codec.encode)(&state),
        None => Value::Null,
    };
    let tail_state = match &format.state {
        Some(codec) => (codec.decode)(&encoded_state).ok()?,
        None => Value::Null,
    };
    let mut tail_records = Vec::new();
    if !tail.is_empty() && transcript_line_gate(&format.usage_markers)(tail) {
        let mut tail_state = tail_state;
        tail_records = (format.parse_line)(&line_text(tail), &mut tail_state);
    }

    let guard_length = GUARD_LENGTH.min(resume_offset);
    let guard_hash = if guard_length > 0 {
        let window = read_window(&mut file, resume_offset - guard_length, guard_length).await?;
        fnv1a(&window)
    } else {
        0
    };

    Some(TranscriptParseResult {
        records,
        tail_records,
        position: TranscriptParsePosition {
            resume_offset,
            guard_length,
            guard_hash,
            state: encoded_state,
        },
        resumed,
    })
}

#[cfg(test)]
mod tests {
    //! The real Claude and Codex formats live in their provider crates, which are
    //! not ported yet. These fixtures keep the reader mechanics under test: a
    //! stateless format (Claude-shaped) and a stateful one (Codex-shaped, with a
    //! reducer that carries model, session, tier, and a duplicate-suppression
    //! marker across lines).

    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use serde_json::json;
    use turin_contracts::usage::{UsageProviderKind, UsageTokenTotals};
    use turin_provider_core::usage::{
        TranscriptUsageState, UsageSpeed, empty_totals,
    };

    fn claude_record(line: &Value) -> Option<UsageRecord> {
        let usage = line.get("message")?.get("usage")?;
        let output = usage.get("output_tokens")?.as_u64()?;
        Some(UsageRecord {
            provider: UsageProviderKind::Claude,
            timestamp_ms: 0.0,
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

    fn claude_format() -> TranscriptUsageFormat {
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
    fn codex_format() -> TranscriptUsageFormat {
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
                            timestamp_ms: 0.0,
                            model: state["model"].as_str().unwrap_or_default().to_owned(),
                            rate_model: None,
                            session_id: state["session"].as_str().unwrap_or_default().to_owned(),
                            totals: UsageTokenTotals { output_tokens: output, ..empty_totals() },
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
                    if encoded.is_object() { Ok(encoded.clone()) } else { Err("not an object".to_owned()) }
                }),
            }),
            shared_sessions_across_files: false,
        }
    }

    fn claude_line(id: u32, output_tokens: u64) -> String {
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "timestamp": "2026-08-01T10:00:00Z",
                "requestId": format!("req_{id}"),
                "sessionId": "session-1",
                "message": {
                    "id": format!("msg_{id}"),
                    "model": "claude-fable-5",
                    "usage": { "input_tokens": 10, "output_tokens": output_tokens },
                },
            })
        )
    }

    fn codex_meta_line() -> String {
        format!(
            "{}\n",
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-01T10:00:00Z",
                "payload": { "type": "session_meta", "id": "codex-session-1" },
            })
        )
    }

    fn codex_model_line(model: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type": "turn_context",
                "timestamp": "2026-08-01T10:00:01Z",
                "payload": { "type": "turn_context", "model": model },
            })
        )
    }

    fn codex_tier_line(service_tier: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type": "event_msg",
                "timestamp": "2026-08-01T10:00:01Z",
                "payload": {
                    "type": "thread_settings_applied",
                    "thread_settings": { "service_tier": service_tier },
                },
            })
        )
    }

    fn codex_usage_line(output_tokens: u64, seconds: u32) -> String {
        format!(
            "{}\n",
            json!({
                "type": "event_msg",
                "timestamp": format!("2026-08-01T10:00:{seconds:02}Z"),
                "payload": {
                    "type": "token_count",
                    "info": { "last_token_usage": { "input_tokens": 100, "output_tokens": output_tokens } },
                },
            })
        )
    }

    fn outputs(records: &[UsageRecord]) -> Vec<u64> {
        records.iter().map(|record| record.totals.output_tokens).collect()
    }

    fn write(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn append(path: &Path, contents: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(path).unwrap().write_all(contents.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn parses_only_appended_lines_when_resuming_a_grown_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "claude.jsonl", &(claude_line(1, 5) + &claude_line(2, 7)));
        let format = claude_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();
        assert_eq!(first.records.len(), 2);
        assert!(!first.resumed);

        append(&path, &claude_line(3, 11));
        let second = read_transcript_records(&path, &format, Some(&first.position)).await.unwrap();
        assert!(second.resumed);
        assert_eq!(outputs(&second.records), vec![11]);

        // The stitched result matches a from-scratch parse of the whole file.
        let full = read_transcript_records(&path, &format, None).await.unwrap();
        let stitched: Vec<UsageRecord> = first.records.iter().chain(&second.records).cloned().collect();
        assert_eq!(stitched, full.records);
    }

    #[tokio::test]
    async fn carries_the_codex_reducer_state_across_the_resume_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "rollout.jsonl",
            &(codex_meta_line() + &codex_model_line("gpt-5.2-codex") + &codex_tier_line("ultrafast")),
        );
        let format = codex_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();
        assert!(first.records.is_empty());

        // The appended usage event has no turn_context, thread settings, or
        // session_meta of its own; model, tier, and session come from the saved state.
        append(&path, &codex_usage_line(9, 5));
        let second = read_transcript_records(&path, &format, Some(&first.position)).await.unwrap();
        assert!(second.resumed);
        assert_eq!(second.records.len(), 1);
        assert_eq!(second.records[0].model, "gpt-5.2-codex");
        assert_eq!(second.records[0].speed, UsageSpeed::Ultrafast);
        assert_eq!(second.records[0].session_id, "codex-session-1");
    }

    #[tokio::test]
    async fn suppresses_a_codex_duplicate_usage_event_that_straddles_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "rollout.jsonl",
            &(codex_meta_line() + &codex_model_line("gpt-5.2-codex") + &codex_usage_line(9, 5)),
        );
        let format = codex_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();
        assert_eq!(first.records.len(), 1);

        // The copy lands after the resume point and must still be dropped.
        append(&path, &(codex_usage_line(9, 5) + &codex_usage_line(21, 8)));
        let second = read_transcript_records(&path, &format, Some(&first.position)).await.unwrap();
        assert!(second.resumed);
        assert_eq!(outputs(&second.records), vec![21]);
    }

    #[tokio::test]
    async fn defers_an_unterminated_trailing_line_to_tail_records_then_consumes_it_once_terminated() {
        let dir = tempfile::tempdir().unwrap();
        let unterminated = claude_line(2, 7).trim_end().to_owned();
        let path = write(&dir, "claude.jsonl", &(claude_line(1, 5) + &unterminated));
        let format = claude_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();
        assert_eq!(first.records.len(), 1);
        assert_eq!(outputs(&first.tail_records), vec![7]);

        // Completing the line and appending another re-reads from the resume point.
        append(&path, &format!("\n{}", claude_line(3, 11)));
        let second = read_transcript_records(&path, &format, Some(&first.position)).await.unwrap();
        assert!(second.resumed);
        assert_eq!(outputs(&second.records), vec![7, 11]);
        assert!(second.tail_records.is_empty());
    }

    #[tokio::test]
    async fn re_parses_from_the_start_when_the_guard_bytes_no_longer_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "claude.jsonl", &claude_line(1, 5));
        let format = claude_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();

        // Same path, larger size, different content: a replaced file, not growth.
        std::fs::write(&path, claude_line(4, 13) + &claude_line(5, 17)).unwrap();
        let second = read_transcript_records(&path, &format, Some(&first.position)).await.unwrap();
        assert!(!second.resumed);
        assert_eq!(outputs(&second.records), vec![13, 17]);
    }

    #[tokio::test]
    async fn re_parses_from_the_start_when_the_file_shrank_below_the_resume_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "claude.jsonl", &(claude_line(1, 5) + &claude_line(2, 7)));
        let format = claude_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();

        std::fs::write(&path, claude_line(3, 11)).unwrap();
        let second = read_transcript_records(&path, &format, Some(&first.position)).await.unwrap();
        assert!(!second.resumed);
        assert_eq!(outputs(&second.records), vec![11]);
    }

    #[tokio::test]
    async fn parses_a_line_larger_than_one_read_buffer() {
        // Tool-heavy transcripts carry multi-megabyte single lines.
        let dir = tempfile::tempdir().unwrap();
        let big = format!(
            "{}\n",
            json!({
                "type": "assistant",
                "timestamp": "2026-08-01T10:00:00Z",
                "requestId": "req_big",
                "sessionId": "session-1",
                "padding": "x".repeat(512 * 1024),
                "message": {
                    "id": "msg_big",
                    "model": "claude-fable-5",
                    "usage": { "input_tokens": 10, "output_tokens": 42 },
                },
            })
        );
        let path = write(&dir, "claude.jsonl", &(big + &claude_line(2, 7)));
        let parsed = read_transcript_records(&path, &claude_format(), None).await.unwrap();
        assert_eq!(outputs(&parsed.records), vec![42, 7]);
    }

    #[tokio::test]
    async fn strips_a_carriage_return_and_counts_its_bytes_toward_the_resume_offset() {
        let dir = tempfile::tempdir().unwrap();
        let crlf = |line: String| line.replace('\n', "\r\n");
        let content = crlf(codex_meta_line()) + &crlf(codex_model_line("gpt-5.2-codex")) + &crlf(codex_usage_line(40, 2));
        let path = write(&dir, "rollout.jsonl", &content);
        let result = read_transcript_records(&path, &codex_format(), None).await.unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].model, "gpt-5.2-codex");
        assert_eq!(result.records[0].session_id, "codex-session-1");
        assert_eq!(result.position.resume_offset, content.len() as u64);
    }

    #[tokio::test]
    async fn returns_none_for_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_transcript_records(&dir.path().join("missing.jsonl"), &claude_format(), None).await.is_none());
    }

    #[test]
    fn line_gate_passes_only_lines_containing_one_of_its_markers() {
        let one = vec!["\"usage\"".to_owned()];
        let several: Vec<String> = ["\"token_count\"", "\"session_meta\"", "\"a.b(c)\""].map(str::to_owned).to_vec();
        for markers in [one, several] {
            let gate = transcript_line_gate(&markers);
            for marker in &markers {
                assert!(gate(format!("{{\"payload\":{{{marker}:1}}}}").as_bytes()));
                // The same text without its quotes is not the marker.
                assert!(!gate(format!("{{\"text\":\"see {} here\"}}", &marker[1..marker.len() - 1]).as_bytes()));
            }
            assert!(!gate(b""));
        }
    }

    #[test]
    fn line_gate_matches_markers_beside_multi_byte_and_malformed_utf8() {
        let markers = vec!["\"usage\"".to_owned()];
        let gate = transcript_line_gate(&markers);
        let invalid = [0xff, 0xc3];
        assert!(gate("{\"text\":\"日本語 é\",\"usage\":1}".as_bytes()));
        assert!(gate(&[&invalid[..], b"\"usage\"", &invalid[..]].concat()));
        assert!(!gate("{\"text\":\"日本語 é usage\"}".as_bytes()));
    }
}
