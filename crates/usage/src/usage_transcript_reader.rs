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
    walk(
        root.to_path_buf(),
        file_name,
        &mut candidates,
        &mut failed_paths,
    )
    .await;

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
            let is_dir = entry
                .file_type()
                .await
                .map(|kind| kind.is_dir())
                .unwrap_or(false);
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
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Deepest container nesting a line may keep. serde_json refuses past 128, and
/// usage fields sit within a handful of levels, so deeper containers are cut.
const MAX_LINE_DEPTH: usize = 64;

/// Line text as the parser sees it: a trailing CR is part of the terminator, and
/// containers nested past `MAX_LINE_DEPTH` are replaced by `null`, as the
/// streaming reader discards them. No format reads that deep.
fn line_text(line: &[u8]) -> String {
    let content = match line.last() {
        Some(&CARRIAGE_RETURN) => &line[..line.len() - 1],
        _ => line,
    };
    String::from_utf8_lossy(&cap_nesting(content, MAX_LINE_DEPTH)).into_owned()
}

/// Index just past the container that opens at `start`, or the end of `bytes`
/// when it never closes.
fn container_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            match byte {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'[' | b'{' => depth += 1,
                b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    bytes.len()
}

/// Replaces every container nested deeper than `max_depth` with `null`. The
/// result is still JSON with the same shallow structure; without a nested
/// container the input is borrowed unchanged.
fn cap_nesting(bytes: &[u8], max_depth: usize) -> std::borrow::Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut emitted = 0;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            match byte {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'[' | b'{' if depth == max_depth => {
                    let end = container_end(bytes, i);
                    out.get_or_insert_with(|| Vec::with_capacity(bytes.len()))
                        .extend_from_slice(&bytes[emitted..i]);
                    out.get_or_insert_with(Vec::new).extend_from_slice(b"null");
                    emitted = end;
                    i = end;
                    continue;
                }
                b'[' | b'{' => depth += 1,
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        i += 1;
    }
    match out {
        None => std::borrow::Cow::Borrowed(bytes),
        Some(mut out) => {
            out.extend_from_slice(&bytes[emitted..]);
            std::borrow::Cow::Owned(out)
        }
    }
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
    // The guard check moves the handle, so the parse seeks even when it starts at 0.
    let mut positioned = false;
    let saved_state = match (resume_from, &format.state) {
        (Some(position), Some(codec)) if !position.state.is_null() => {
            (codec.decode)(&position.state).ok()
        }
        _ => None,
    };
    if let Some(position) = resume_from
        && position.resume_offset > 0
        && (format.state.is_none() || saved_state.is_some())
        && {
            positioned = true;
            guard_matches(&mut file, position).await
        }
    {
        if let Some(saved) = saved_state {
            state = saved;
        }
        start = position.resume_offset;
        resumed = true;
    }

    // Seeking a FIFO (or any non-seekable file) fails even at offset 0, so only seek when moving.
    if start > 0 || positioned {
        file.seek(std::io::SeekFrom::Start(start)).await.ok()?;
    }
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

    finish_tail(
        reader.into_inner(),
        format,
        state,
        resume_offset,
        &[],
        records,
        resumed,
    )
    .await
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
    use super::*;
    use crate::test_formats::{claude_format, codex_format};
    use serde_json::json;
    use turin_provider_core::usage::UsageSpeed;

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
        records
            .iter()
            .map(|record| record.totals.output_tokens)
            .collect()
    }

    fn write(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn append(path: &Path, contents: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(contents.as_bytes())
            .unwrap();
    }

    #[tokio::test]
    async fn parses_only_appended_lines_when_resuming_a_grown_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "claude.jsonl",
            &(claude_line(1, 5) + &claude_line(2, 7)),
        );
        let format = claude_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();
        assert_eq!(first.records.len(), 2);
        assert!(!first.resumed);

        append(&path, &claude_line(3, 11));
        let second = read_transcript_records(&path, &format, Some(&first.position))
            .await
            .unwrap();
        assert!(second.resumed);
        assert_eq!(outputs(&second.records), vec![11]);

        // The stitched result matches a from-scratch parse of the whole file.
        let full = read_transcript_records(&path, &format, None).await.unwrap();
        let stitched: Vec<UsageRecord> = first
            .records
            .iter()
            .chain(&second.records)
            .cloned()
            .collect();
        assert_eq!(stitched, full.records);
    }

    #[tokio::test]
    async fn carries_the_codex_reducer_state_across_the_resume_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "rollout.jsonl",
            &(codex_meta_line()
                + &codex_model_line("gpt-5.2-codex")
                + &codex_tier_line("ultrafast")),
        );
        let format = codex_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();
        assert!(first.records.is_empty());

        // The appended usage event has no turn_context, thread settings, or
        // session_meta of its own; model, tier, and session come from the saved state.
        append(&path, &codex_usage_line(9, 5));
        let second = read_transcript_records(&path, &format, Some(&first.position))
            .await
            .unwrap();
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
        let second = read_transcript_records(&path, &format, Some(&first.position))
            .await
            .unwrap();
        assert!(second.resumed);
        assert_eq!(outputs(&second.records), vec![21]);
    }

    #[tokio::test]
    async fn defers_an_unterminated_trailing_line_to_tail_records_then_consumes_it_once_terminated()
    {
        let dir = tempfile::tempdir().unwrap();
        let unterminated = claude_line(2, 7).trim_end().to_owned();
        let path = write(&dir, "claude.jsonl", &(claude_line(1, 5) + &unterminated));
        let format = claude_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();
        assert_eq!(first.records.len(), 1);
        assert_eq!(outputs(&first.tail_records), vec![7]);

        // Completing the line and appending another re-reads from the resume point.
        append(&path, &format!("\n{}", claude_line(3, 11)));
        let second = read_transcript_records(&path, &format, Some(&first.position))
            .await
            .unwrap();
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
        let second = read_transcript_records(&path, &format, Some(&first.position))
            .await
            .unwrap();
        assert!(!second.resumed);
        assert_eq!(outputs(&second.records), vec![13, 17]);
    }

    #[tokio::test]
    async fn re_parses_from_the_start_when_the_file_shrank_below_the_resume_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "claude.jsonl",
            &(claude_line(1, 5) + &claude_line(2, 7)),
        );
        let format = claude_format();
        let first = read_transcript_records(&path, &format, None).await.unwrap();

        std::fs::write(&path, claude_line(3, 11)).unwrap();
        let second = read_transcript_records(&path, &format, Some(&first.position))
            .await
            .unwrap();
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
        let parsed = read_transcript_records(&path, &claude_format(), None)
            .await
            .unwrap();
        assert_eq!(outputs(&parsed.records), vec![42, 7]);
    }

    #[tokio::test]
    async fn strips_a_carriage_return_and_counts_its_bytes_toward_the_resume_offset() {
        let dir = tempfile::tempdir().unwrap();
        let crlf = |line: String| line.replace('\n', "\r\n");
        let content = crlf(codex_meta_line())
            + &crlf(codex_model_line("gpt-5.2-codex"))
            + &crlf(codex_usage_line(40, 2));
        let path = write(&dir, "rollout.jsonl", &content);
        let result = read_transcript_records(&path, &codex_format(), None)
            .await
            .unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].model, "gpt-5.2-codex");
        assert_eq!(result.records[0].session_id, "codex-session-1");
        assert_eq!(result.position.resume_offset, content.len() as u64);
    }

    #[tokio::test]
    async fn returns_none_for_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            read_transcript_records(&dir.path().join("missing.jsonl"), &claude_format(), None)
                .await
                .is_none()
        );
    }

    #[test]
    fn line_gate_passes_only_lines_containing_one_of_its_markers() {
        let one = vec!["\"usage\"".to_owned()];
        let several: Vec<String> = ["\"token_count\"", "\"session_meta\"", "\"a.b(c)\""]
            .map(str::to_owned)
            .to_vec();
        for markers in [one, several] {
            let gate = transcript_line_gate(&markers);
            for marker in &markers {
                assert!(gate(format!("{{\"payload\":{{{marker}:1}}}}").as_bytes()));
                // The same text without its quotes is not the marker.
                assert!(!gate(
                    format!("{{\"text\":\"see {} here\"}}", &marker[1..marker.len() - 1])
                        .as_bytes()
                ));
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

    /// Matches `usageTranscriptStreaming.test.ts`: large irrelevant content must not
    /// change what a line yields. The cases that depend on the real provider formats
    /// (Pi, Grok, fork-copy suppression, fast-mode cost) are not ported here.
    mod streaming {
        use super::*;
        use serde_json::Value;

        const TIMESTAMP: &str = "2026-08-01T10:00:00Z";

        fn content() -> String {
            "工具 output \\\" usage token_count ".repeat(40_000)
        }

        fn claude_json(id: &str, output: u64, tool_text: Option<&str>) -> Value {
            let content = match tool_text {
                Some(text) => json!([{ "type": "tool_use", "input": { "text": text } }]),
                None => json!([]),
            };
            json!({
                "type": "assistant",
                "timestamp": TIMESTAMP,
                "sessionId": "s1",
                "requestId": format!("r-{id}"),
                "costUSD": 0.25,
                "message": {
                    "content": content,
                    "id": id,
                    "model": "claude-fable-5",
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": output,
                        "cache_read_input_tokens": 20,
                        "cache_creation_input_tokens": 5,
                        "speed": "fast",
                    },
                },
            })
        }

        fn codex_lines() -> Vec<Value> {
            vec![
                json!({ "type": "session_meta", "timestamp": TIMESTAMP, "payload": { "id": "s1" } }),
                json!({ "type": "turn_context", "timestamp": TIMESTAMP, "payload": { "model": "gpt-5.6-sol" } }),
                json!({
                    "type": "event_msg",
                    "timestamp": TIMESTAMP,
                    "payload": {
                        "type": "token_count",
                        "info": { "last_token_usage": { "input_tokens": 100, "output_tokens": 99 } },
                    },
                }),
            ]
        }

        fn lines_text(lines: &[Value]) -> String {
            lines.iter().map(|line| format!("{line}\n")).collect()
        }

        fn write_lines(dir: &tempfile::TempDir, name: &str, lines: &[Value]) -> PathBuf {
            write(dir, name, &lines_text(lines))
        }

        fn outputs_of(records: &[UsageRecord]) -> Vec<u64> {
            records
                .iter()
                .map(|record| record.totals.output_tokens)
                .collect()
        }

        #[tokio::test]
        async fn matches_ordinary_claude_and_codex_records_with_large_irrelevant_fields_in_either_order()
         {
            let dir = tempfile::tempdir().unwrap();
            let small_claude = vec![claude_json("m1", 99, None)];
            let cases = [
                ("claude", small_claude, claude_format()),
                ("codex", codex_lines(), codex_format()),
            ];
            for (name, small, format) in cases {
                let expected = read_transcript_records(
                    &write_lines(&dir, &format!("{name}-small.jsonl"), &small),
                    &format,
                    None,
                )
                .await
                .unwrap();
                for padding_first in [true, false] {
                    let large: Vec<Value> = small
                        .iter()
                        .map(|record| {
                            let mut padded = serde_json::Map::new();
                            if padding_first {
                                padded.insert("padding".to_owned(), json!(content()));
                            }
                            padded.extend(record.as_object().unwrap().clone());
                            if !padding_first {
                                padded.insert("padding".to_owned(), json!(content()));
                            }
                            Value::Object(padded)
                        })
                        .collect();
                    let path = write_lines(&dir, &format!("{name}-large.jsonl"), &large);
                    let actual = read_transcript_records(&path, &format, None).await.unwrap();
                    assert_eq!(
                        actual.records, expected.records,
                        "{name} padding_first={padding_first}"
                    );
                    assert_eq!(actual.position.state, expected.position.state, "{name}");
                }
            }
        }

        #[tokio::test]
        async fn does_not_count_usage_looking_text_or_nested_objects_inside_tool_output() {
            let dir = tempfile::tempdir().unwrap();
            let inner = claude_json("m1", 99, None);
            let lines = [
                json!({ "type": "user", "padding": content(), "toolOutput": inner }),
                json!({ "padding": content(), "message": { "content": inner.to_string() } }),
                json!({ "type": "assistant", "timestamp": TIMESTAMP, "message": { "model": "claude-fable-5", "content": [inner] } }),
            ];
            let path = write_lines(&dir, "tool-output.jsonl", &lines);
            let result = read_transcript_records(&path, &claude_format(), None)
                .await
                .unwrap();
            assert!(result.records.is_empty());
        }

        #[tokio::test]
        async fn replays_partial_utf8_and_json_tails_commits_exact_crlf_offsets_and_replaces_the_tail_once()
         {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tail.jsonl");
            let first = format!("{}\r\n", claude_json("first", 5, Some(&content())));
            let second = serde_json::to_vec(&claude_json("second", 7, Some(&content()))).unwrap();
            // Split inside the 3-byte "工" so the cut lands mid-character.
            let split = second
                .windows(3)
                .rposition(|window| window == "工".as_bytes())
                .unwrap()
                + 1;
            let mut partial_bytes = first.clone().into_bytes();
            partial_bytes.extend_from_slice(&second[..split]);
            std::fs::write(&path, &partial_bytes).unwrap();

            let partial = read_transcript_records(&path, &claude_format(), None)
                .await
                .unwrap();
            assert_eq!(outputs_of(&partial.records), vec![5]);
            assert!(partial.tail_records.is_empty());
            assert_eq!(partial.position.resume_offset, first.len() as u64);

            append_bytes(&path, &second[split..]);
            let complete =
                read_transcript_records(&path, &claude_format(), Some(&partial.position))
                    .await
                    .unwrap();
            assert!(complete.resumed);
            assert!(complete.records.is_empty());
            assert_eq!(outputs_of(&complete.tail_records), vec![7]);
            assert_eq!(complete.position.resume_offset, first.len() as u64);

            append_bytes(
                &path,
                format!("\r\n{}\n", claude_json("third", 11, Some(&content()))).as_bytes(),
            );
            let appended =
                read_transcript_records(&path, &claude_format(), Some(&complete.position))
                    .await
                    .unwrap();
            assert_eq!(outputs_of(&appended.records), vec![7, 11]);
            assert!(appended.tail_records.is_empty());
            assert_eq!(
                appended.position.resume_offset,
                std::fs::metadata(&path).unwrap().len()
            );

            let full = read_transcript_records(&path, &claude_format(), None)
                .await
                .unwrap();
            let stitched: Vec<UsageRecord> = partial
                .records
                .iter()
                .chain(&appended.records)
                .cloned()
                .collect();
            assert_eq!(stitched, full.records);
        }

        #[tokio::test]
        async fn preserves_codex_model_switches_and_duplicate_suppression_across_resumes() {
            let dir = tempfile::tempdir().unwrap();
            let lines = codex_lines();
            let padded = |value: &Value| {
                let mut padded = serde_json::Map::new();
                padded.insert("padding".to_owned(), json!(content()));
                padded.extend(value.as_object().unwrap().clone());
                Value::Object(padded)
            };
            let path = write_lines(
                &dir,
                "history.jsonl",
                &lines.iter().map(padded).collect::<Vec<_>>(),
            );
            let format = codex_format();
            let first = read_transcript_records(&path, &format, None).await.unwrap();
            assert_eq!(outputs_of(&first.records), vec![99]);

            // A repeated token event (same timestamp and output) is suppressed; the
            // model switch and the new event after it are not.
            append_bytes(
                &path,
                lines_text(&[
                    padded(&lines[2]),
                    json!({ "type": "turn_context", "payload": { "padding": content(), "model": "gpt-6" } }),
                    json!({
                        "type": "event_msg",
                        "timestamp": TIMESTAMP,
                        "payload": {
                            "type": "token_count",
                            "padding": content(),
                            "info": { "last_token_usage": { "input_tokens": 200, "output_tokens": 101 } },
                        },
                    }),
                ])
                .as_bytes(),
            );
            let resumed = read_transcript_records(&path, &format, Some(&first.position))
                .await
                .unwrap();
            assert!(resumed.resumed);
            assert_eq!(resumed.records.len(), 1);
            assert_eq!(resumed.records[0].model, "gpt-6");
            assert_eq!(resumed.records[0].session_id, "s1");
            assert_eq!(resumed.records[0].totals.output_tokens, 101);

            let full = read_transcript_records(&path, &format, None).await.unwrap();
            let stitched: Vec<UsageRecord> = first
                .records
                .iter()
                .chain(&resumed.records)
                .cloned()
                .collect();
            assert_eq!(stitched, full.records);
        }

        #[tokio::test]
        async fn rejects_malformed_large_lines_without_accepting_partial_usage_or_losing_following_lines()
         {
            let dir = tempfile::tempdir().unwrap();
            let valid = serde_json::to_string(&claude_json("m1", 99, Some(&content()))).unwrap();
            let good = serde_json::to_string(&claude_json("good", 7, Some(&content()))).unwrap();
            let body = [
                format!("{valid} junk"),
                valid[..valid.len() - 1].to_owned(),
                format!("{valid}{valid}"),
                good,
            ]
            .join("\n")
                + "\n";
            let path = write(&dir, "broken.jsonl", &body);
            let result = read_transcript_records(&path, &claude_format(), None)
                .await
                .unwrap();
            assert_eq!(outputs_of(&result.records), vec![7]);
        }

        #[tokio::test]
        async fn matches_the_same_record_for_reordered_escaped_duplicate_and_unusual_keys() {
            let dir = tempfile::tempdir().unwrap();
            let small = serde_json::to_string(&claude_json("m1", 99, None)).unwrap();
            let variants = [
                small.replacen("\"message\":", "\"mess\\u0061ge\":", 1),
                small.replacen("\"costUSD\":0.25", "\"costUSD\":5,\"costUSD\":0.25", 1),
                small.replacen(
                    "\"type\":\"assistant\"",
                    "\"type\":\"user\",\"type\":\"assistant\"",
                    1,
                ),
                small.replacen(
                    "\"message\":",
                    "\"__proto__\":{\"type\":\"user\"},\"message\":",
                    1,
                ),
                small.replacen(
                    "\"message\":",
                    "\"message.usage\":{\"output_tokens\":1234},\"message\":",
                    1,
                ),
            ];
            let format = claude_format();
            for line in variants {
                let expected = read_transcript_records(
                    &write(&dir, "odd.jsonl", &format!("{line}\n")),
                    &format,
                    None,
                )
                .await
                .unwrap();
                let padded = format!("{{\"padding\":{},{}\n", json!(content()), &line[1..]);
                let actual =
                    read_transcript_records(&write(&dir, "odd.jsonl", &padded), &format, None)
                        .await
                        .unwrap();
                assert_eq!(actual.records, expected.records, "{line}");
            }
        }

        #[tokio::test]
        async fn reads_usage_after_deeply_nested_discarded_tool_content() {
            let dir = tempfile::tempdir().unwrap();
            let record = serde_json::to_string(&claude_json("m1", 99, None)).unwrap();
            let nested = format!("{}0{}", "[".repeat(300), "]".repeat(300));
            let path = write(
                &dir,
                "deep.jsonl",
                &format!("{{\"toolOutput\":{nested},{}\n", &record[1..]),
            );
            let result = read_transcript_records(&path, &claude_format(), None)
                .await
                .unwrap();
            assert_eq!(outputs_of(&result.records), vec![99]);
        }

        #[test]
        fn cap_nesting_borrows_shallow_lines_and_nulls_only_the_deep_containers() {
            let shallow = br#"{"a":[1,{"b":"]]"}]}"#;
            assert!(matches!(
                cap_nesting(shallow, 4),
                std::borrow::Cow::Borrowed(_)
            ));

            let deep = format!("{}1{}", "[".repeat(10), "]".repeat(10));
            let capped = cap_nesting(deep.as_bytes(), 3);
            assert_eq!(std::str::from_utf8(&capped).unwrap(), "[[[null]]]");
        }

        fn append_bytes(path: &Path, bytes: &[u8]) {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }
    }
}
