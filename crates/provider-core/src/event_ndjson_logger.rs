//! Port of upstream/apps/server/src/provider/EventNdjsonLogger.ts.
//!
//! Best-effort provider event logging with one shared writer per thread. The
//! native and canonical views share batching, rotation and retention state, so
//! they cannot race while appending to the same thread-scoped file.
//!
//! Turin drops upstream's resource-attribution hook: resource telemetry is cut
//! (see PORTING.md), so nothing reports logical write bytes.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Map, Value, json};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};
use turin_contracts::{ThreadId, UtcDateTime};

use crate::{
    attachment_paths::to_safe_thread_attachment_segment,
    js::utf16_len,
    provider_event_loggers::EventNdjsonLogger,
    rotating_file_sink::{RotatingFileSink, RotatingFileSinkError},
    rt::BoxFuture,
};

const MEBIBYTE: u64 = 1024 * 1024;
const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_RECORD_CHARACTERS: usize = 64 * 1024;
const MAX_RECORD_FIELDS: i64 = 1_024;
const MAX_RECORD_DEPTH: usize = 16;
const GLOBAL_THREAD_SEGMENT: &str = "_global";

const TRANSIENT_CANONICAL_EVENT_TYPES: [&str; 7] = [
    "content.delta",
    "hook.progress",
    "item.updated",
    "task.progress",
    "thread.realtime.audio.delta",
    "tool.progress",
    "turn.proposed.delta",
];
const TRANSIENT_NATIVE_METHODS: [&str; 9] = [
    "item/agentMessage/delta",
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "item/plan/delta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/textDelta",
    "thread/realtime/outputAudio/delta",
    "thread/realtime/transcript/delta",
    "turn/diff/updated",
];
const TRANSIENT_ACP_UPDATES: [&str; 2] = ["agent_message_chunk", "agent_thought_chunk"];

/// Fields a summary keeps, in the order upstream copies them.
const SUMMARY_FIELDS: [&str; 32] = [
    "provider",
    "protocol",
    "kind",
    "providerSessionId",
    "direction",
    "stage",
    "type",
    "subtype",
    "method",
    "id",
    "threadId",
    "turnId",
    "requestId",
    "session_id",
    "status",
    "is_error",
    "api_error_status",
    "terminal_reason",
    "stop_reason",
    "operation",
    "code",
    "willRetry",
    "message",
    "event",
    "payload",
    "params",
    "result",
    "thread",
    "turn",
    "error",
    "turns",
    "items",
];

static RETAINED_FILE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.log(?:\.\d+)?$").expect("valid regex"));
static LOG_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[[^\]\r\n]+\] (?:NTIVE|CANON|ORCH): ").expect("valid regex"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventNdjsonStream {
    Native,
    Canonical,
    Orchestration,
}

impl EventNdjsonStream {
    /// The label written after the timestamp in each line.
    fn label(self) -> &'static str {
        match self {
            Self::Native => "NTIVE",
            Self::Orchestration => "ORCH",
            Self::Canonical => "CANON",
        }
    }
}

/// `EventNdjsonLogStoreOptions` with upstream's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventNdjsonLogOptions {
    pub max_bytes: u64,
    pub max_files: u64,
    pub batch_window_ms: u64,
    pub max_total_bytes: u64,
    pub max_age_ms: u64,
    pub retention_check_interval_ms: u64,
    pub max_buffered_bytes: u64,
    pub max_buffered_records: u64,
}

impl Default for EventNdjsonLogOptions {
    fn default() -> Self {
        Self {
            max_bytes: 10 * MEBIBYTE,
            max_files: 10,
            batch_window_ms: 1_000,
            max_total_bytes: 512 * MEBIBYTE,
            max_age_ms: 14 * DAY_MS,
            retention_check_interval_ms: 5 * 60 * 1_000,
            max_buffered_bytes: MEBIBYTE,
            max_buffered_records: 512,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EventNdjsonLogStoreError {
    #[error("Provider event log option '{option}' must be an integer >= {minimum}; received {value} for '{}'", file_path.display())]
    Configuration {
        file_path: PathBuf,
        option: &'static str,
        value: u64,
        minimum: u64,
    },
    #[error("Failed to create provider event log directory '{}'", directory.display())]
    Directory {
        directory: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
struct FileFailure {
    file_path: PathBuf,
    message: String,
}

#[derive(Debug, Clone)]
struct PendingRecord {
    thread_segment: String,
    line: String,
    bytes: u64,
}

#[derive(Default)]
struct StoreState {
    pending: Vec<PendingRecord>,
    pending_bytes: u64,
    sinks: IndexMap<String, Arc<RotatingFileSink>>,
    flush_scheduled: bool,
    closed: bool,
    last_retention_at: i64,
}

/// Shared by the store and every logger view it hands out.
struct Shared {
    file_path: PathBuf,
    directory: PathBuf,
    file_prefix: String,
    options: EventNdjsonLogOptions,
    state: Mutex<StoreState>,
    /// Serializes flushes, as upstream's `SynchronizedRef` does. Sink creation
    /// and writes run under it, so concurrent first writes share one sink.
    flush_lock: AsyncMutex<()>,
    timers: Mutex<Vec<JoinHandle<()>>>,
}

pub struct EventNdjsonLogStore {
    shared: Arc<Shared>,
}

fn resolve_thread_segment(thread_id: Option<&ThreadId>) -> String {
    thread_id
        .and_then(|id| to_safe_thread_attachment_segment(id.as_str()))
        .unwrap_or_else(|| GLOBAL_THREAD_SEGMENT.to_owned())
}

/// `NodePath.extname` on a basename: the last dot-suffix, empty for a leading dot.
fn extname(basename: &str) -> &str {
    match basename.rfind('.') {
        Some(index) if index > 0 => &basename[index..],
        _ => "",
    }
}

/// `providerLogPrefix`: `provider.log` → `provider.`, so every thread file is
/// `provider.<segment>.log`.
fn provider_log_prefix(file_path: &Path) -> String {
    let basename = file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = extname(&basename);
    let stem = if extension.is_empty() {
        basename.as_str()
    } else {
        &basename[..basename.len() - extension.len()]
    };
    format!("{stem}.")
}

fn provider_log_path(directory: &Path, prefix: &str, thread_segment: &str) -> PathBuf {
    directory.join(format!("{prefix}{thread_segment}.log"))
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object()?.get(key)
}

fn text(value: Option<&Value>) -> Option<&str> {
    value?.as_str()
}

fn is_object(value: Option<&Value>) -> bool {
    value.is_some_and(Value::is_object)
}

/// Whether a provider event belongs in its log. Transient deltas and snapshots
/// are dropped because the terminal event carries the same information.
pub fn should_persist_provider_event(stream: EventNdjsonStream, event: &Value) -> bool {
    if stream == EventNdjsonStream::Orchestration || !(event.is_object() || event.is_array()) {
        return true;
    }
    if text(field(event, "type"))
        .is_some_and(|kind| TRANSIENT_CANONICAL_EVENT_TYPES.contains(&kind))
    {
        return false;
    }
    if stream != EventNdjsonStream::Native {
        return true;
    }

    // Decoded frames carry the same information as raw frames without another
    // copy of every token delta. Decode failures have their own diagnostic frame.
    let nested = field(event, "event");
    let envelope = if is_object(nested) {
        nested.expect("object")
    } else {
        event
    };
    if text(field(envelope, "stage")) == Some("raw") {
        return false;
    }
    let decoded_payload = field(envelope, "payload");
    let native_event = match (text(field(envelope, "stage")), decoded_payload) {
        (Some("decoded"), Some(payload)) if payload.is_object() => payload,
        _ => envelope,
    };
    let method = text(field(native_event, "method"));
    if method.is_some_and(|method| {
        TRANSIENT_NATIVE_METHODS.contains(&method)
            || method.starts_with("claude/stream_event/content_block_delta/")
    }) {
        return false;
    }

    let native_type = text(field(native_event, "type"));
    if native_type == Some("message.part.delta") {
        return false;
    }
    if native_type == Some("stream_event") {
        let stream_event = field(native_event, "event");
        if is_object(stream_event)
            && text(stream_event.and_then(|event| field(event, "type")))
                == Some("content_block_delta")
        {
            return false;
        }
    }

    let Some(payload) = field(native_event, "payload").filter(|payload| payload.is_object()) else {
        return true;
    };

    if method == Some("session/update") {
        let Some(update) = field(payload, "update").filter(|update| update.is_object()) else {
            return true;
        };
        return match text(field(update, "sessionUpdate")) {
            Some(update_type) => !TRANSIENT_ACP_UPDATES.contains(&update_type),
            None => true,
        };
    }

    if native_type == Some("message.part.updated") {
        let Some(properties) = field(payload, "properties").filter(|value| value.is_object())
        else {
            return true;
        };
        let Some(part) = field(properties, "part").filter(|value| value.is_object()) else {
            return true;
        };
        match text(field(part, "type")) {
            Some("text") | Some("reasoning") => return false,
            Some("tool") => {
                // Running snapshots repeat growing output. Pending and terminal
                // states stay in the log.
                if let Some(state) = field(part, "state").filter(|value| value.is_object()) {
                    return text(field(state, "status")) != Some("running");
                }
            }
            _ => {}
        }
        return true;
    }

    true
}

/// `summarizeProviderEvent`: a bounded shape for an event that is too large to
/// log whole. Arrays become counts, long strings become their length.
pub fn summarize_provider_event(event: &Value) -> Value {
    let mut remaining_fields: i64 = 128;
    let mut remaining_characters: i64 = 8 * 1024;
    summarize(event, 0, &mut remaining_fields, &mut remaining_characters)
}

fn summarize(
    value: &Value,
    depth: usize,
    remaining_fields: &mut i64,
    remaining_characters: &mut i64,
) -> Value {
    match value {
        Value::String(text) => {
            let length = utf16_len(text) as i64;
            if length > (*remaining_characters).min(1_024) {
                json!({ "omittedCharacters": length })
            } else {
                *remaining_characters -= length;
                value.clone()
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::Array(items) => json!({ "itemCount": items.len() }),
        Value::Object(map) => {
            let mut summary = Map::new();
            summary.insert("truncated".to_owned(), Value::Bool(true));
            if depth >= 6 {
                return Value::Object(summary);
            }
            for key in SUMMARY_FIELDS {
                if *remaining_fields <= 0 {
                    break;
                }
                let Some(nested) = map.get(key) else {
                    continue;
                };
                *remaining_fields -= 1;
                summary.insert(
                    key.to_owned(),
                    summarize(nested, depth + 1, remaining_fields, remaining_characters),
                );
            }
            Value::Object(summary)
        }
    }
}

/// `boundProviderEventForLogging`: the event itself when it fits the field,
/// character and depth budgets, otherwise its summary.
pub fn bound_provider_event_for_logging(event: &Value) -> Value {
    let mut remaining_characters = MAX_RECORD_CHARACTERS as i64;
    let mut remaining_fields = MAX_RECORD_FIELDS;
    if fits(event, 0, &mut remaining_fields, &mut remaining_characters) {
        return event.clone();
    }
    summarize_provider_event(event)
}

fn fits(
    value: &Value,
    depth: usize,
    remaining_fields: &mut i64,
    remaining_characters: &mut i64,
) -> bool {
    match value {
        Value::String(text) => {
            *remaining_characters -= utf16_len(text) as i64;
            *remaining_characters >= 0
        }
        Value::Object(map) => {
            if depth > MAX_RECORD_DEPTH {
                return false;
            }
            for (key, nested) in map {
                *remaining_fields -= 1;
                *remaining_characters -= utf16_len(key) as i64;
                if *remaining_fields < 0
                    || *remaining_characters < 0
                    || !fits(nested, depth + 1, remaining_fields, remaining_characters)
                {
                    return false;
                }
            }
            true
        }
        Value::Array(items) => {
            if depth > MAX_RECORD_DEPTH || items.len() as i64 > *remaining_fields {
                return false;
            }
            for (index, nested) in items.iter().enumerate() {
                *remaining_fields -= 1;
                *remaining_characters -= index.to_string().len() as i64;
                if *remaining_fields < 0
                    || *remaining_characters < 0
                    || !fits(nested, depth + 1, remaining_fields, remaining_characters)
                {
                    return false;
                }
            }
            true
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
    }
}

/// Appends each record's line, batching up to `max_bytes` per write.
async fn write_batched(
    sink: &RotatingFileSink,
    records: &[PendingRecord],
    max_bytes: u64,
) -> Result<(), RotatingFileSinkError> {
    let mut batch = String::new();
    let mut batch_bytes = 0u64;
    for record in records {
        if batch_bytes > 0 && batch_bytes + record.bytes > max_bytes {
            sink.write(batch.as_bytes()).await?;
            batch.clear();
            batch_bytes = 0;
        }
        batch.push_str(&record.line);
        batch_bytes += record.bytes;
        if batch_bytes >= max_bytes {
            sink.write(batch.as_bytes()).await?;
            batch.clear();
            batch_bytes = 0;
        }
    }
    if !batch.is_empty() {
        sink.write(batch.as_bytes()).await?;
    }
    Ok(())
}

/// Whether `file_name` is one of this store's files: its own prefix, or a
/// `.log` / `.log.N` file whose first line carries the logger's label.
async fn is_provider_log_file(file_path: &Path, file_name: &str, file_prefix: &str) -> bool {
    if !RETAINED_FILE_NAME.is_match(file_name) {
        return false;
    }
    if file_name.starts_with(file_prefix) {
        return true;
    }
    let Ok(mut file) = tokio::fs::File::open(file_path).await else {
        return false;
    };
    use tokio::io::AsyncReadExt;
    let mut header = [0u8; 256];
    let Ok(read) = file.read(&mut header).await else {
        return false;
    };
    LOG_HEADER.is_match(&String::from_utf8_lossy(&header[..read]))
}

/// Deletes files older than `max_age_ms`, then the oldest of the rest until
/// the total is within `max_total_bytes`. Active sinks are never deleted.
async fn enforce_retention(
    directory: &Path,
    max_total_bytes: u64,
    max_age_ms: u64,
    active_file_paths: &HashSet<PathBuf>,
    file_prefix: &str,
    now: i64,
) -> Vec<FileFailure> {
    let mut failures = Vec::new();
    let mut files: Vec<(PathBuf, i64, u64)> = Vec::new();

    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) => {
            return vec![FileFailure {
                file_path: directory.to_path_buf(),
                message: error.to_string(),
            }];
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry.file_type().await.is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let file_path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_provider_log_file(&file_path, &name, file_prefix).await {
            continue;
        }
        match tokio::fs::metadata(&file_path).await {
            Ok(metadata) => {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_millis() as i64);
                files.push((file_path, modified, metadata.len()));
            }
            Err(error) => failures.push(FileFailure {
                file_path,
                message: error.to_string(),
            }),
        }
    }

    let mut total_bytes: u64 = files.iter().map(|(_, _, size)| size).sum();
    let max_age = max_age_ms as i64;
    let mut retained = Vec::new();
    for (file_path, modified, size) in files {
        let expired = now - modified > max_age;
        if !expired || active_file_paths.contains(&file_path) {
            retained.push((file_path, modified, size));
            continue;
        }
        match tokio::fs::remove_file(&file_path).await {
            Ok(()) => total_bytes = total_bytes.saturating_sub(size),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                total_bytes = total_bytes.saturating_sub(size);
            }
            Err(error) => {
                failures.push(FileFailure {
                    file_path: file_path.clone(),
                    message: error.to_string(),
                });
                retained.push((file_path, modified, size));
            }
        }
    }

    retained.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    for (file_path, _, size) in retained {
        if total_bytes <= max_total_bytes {
            break;
        }
        if active_file_paths.contains(&file_path) {
            continue;
        }
        match tokio::fs::remove_file(&file_path).await {
            Ok(()) => total_bytes = total_bytes.saturating_sub(size),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                total_bytes = total_bytes.saturating_sub(size);
            }
            Err(error) => failures.push(FileFailure {
                file_path,
                message: error.to_string(),
            }),
        }
    }
    failures
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Shared {
    /// One flush: drain pending records, write them per thread file, and run
    /// retention when due. A timer-driven flush also clears the scheduled flag.
    async fn flush(self: &Arc<Self>, timer_fired: bool, close: bool) {
        let _guard = self.flush_lock.lock().await;
        let started_at = now_ms();
        let (pending, mut sinks, last_retention_at) = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            let pending = std::mem::take(&mut state.pending);
            state.pending_bytes = 0;
            if timer_fired {
                state.flush_scheduled = false;
            }
            (pending, state.sinks.clone(), state.last_retention_at)
        };

        let mut failures = Vec::new();
        let mut by_segment: IndexMap<String, Vec<PendingRecord>> = IndexMap::new();
        for record in pending {
            by_segment
                .entry(record.thread_segment.clone())
                .or_default()
                .push(record);
        }
        let max_bytes = self.options.max_bytes;
        for (segment, records) in &by_segment {
            let file_path = provider_log_path(&self.directory, &self.file_prefix, segment);
            let sink = match sinks.get(segment) {
                Some(sink) => Arc::clone(sink),
                None => match RotatingFileSink::open(&file_path, max_bytes, self.options.max_files)
                    .await
                {
                    Ok(sink) => {
                        let sink = Arc::new(sink);
                        sinks.insert(segment.clone(), Arc::clone(&sink));
                        sink
                    }
                    Err(error) => {
                        failures.push(FileFailure {
                            file_path,
                            message: error.to_string(),
                        });
                        continue;
                    }
                },
            };
            if let Err(error) = write_batched(&sink, records, max_bytes).await {
                sinks.shift_remove(segment);
                failures.push(FileFailure {
                    file_path,
                    message: error.to_string(),
                });
            }
        }

        let retention_due =
            started_at - last_retention_at >= self.options.retention_check_interval_ms as i64;
        if retention_due {
            let active: HashSet<PathBuf> = sinks
                .keys()
                .map(|segment| provider_log_path(&self.directory, &self.file_prefix, segment))
                .collect();
            failures.extend(
                enforce_retention(
                    &self.directory,
                    self.options.max_total_bytes,
                    self.options.max_age_ms,
                    &active,
                    &self.file_prefix,
                    started_at,
                )
                .await,
            );
        }

        for failure in failures {
            tracing::warn!(
                file = %failure.file_path.display(),
                error = %failure.message,
                "provider event log write or retention failed"
            );
        }

        let mut state = self.state.lock();
        state.sinks = sinks;
        state.closed = close;
        if retention_due {
            state.last_retention_at = started_at;
        }
    }

    fn schedule_flush(self: &Arc<Self>) {
        let shared = Arc::clone(self);
        let window = Duration::from_millis(self.options.batch_window_ms);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(window).await;
            shared.flush(true, false).await;
        });
        let mut timers = self.timers.lock();
        timers.retain(|timer| !timer.is_finished());
        timers.push(handle);
    }

    async fn write(
        self: &Arc<Self>,
        stream: EventNdjsonStream,
        event: Value,
        thread_id: Option<ThreadId>,
    ) {
        if !should_persist_provider_event(stream, &event) {
            return;
        }
        let mut payload = bound_provider_event_for_logging(&event).to_string();
        // Escaping can grow a record past its input, so the bounded form is
        // checked again on the serialized bytes.
        if payload.len() > MAX_RECORD_CHARACTERS {
            payload = summarize_provider_event(&event).to_string();
        }
        let observed_at = UtcDateTime::now().to_string();
        let line = format!("[{observed_at}] {}: {payload}\n", stream.label());
        let bytes = line.len() as u64;
        let thread_segment = resolve_thread_segment(thread_id.as_ref());

        let (flush_now, schedule) = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.pending.push(PendingRecord {
                thread_segment,
                line,
                bytes,
            });
            state.pending_bytes += bytes;
            let flush_now = self.options.batch_window_ms == 0
                || state.pending.len() as u64 >= self.options.max_buffered_records
                || state.pending_bytes >= self.options.max_buffered_bytes;
            let schedule = !flush_now && !state.flush_scheduled;
            state.flush_scheduled = state.flush_scheduled || schedule;
            (flush_now, schedule)
        };
        if schedule {
            self.schedule_flush();
        }
        if flush_now {
            self.flush(false, false).await;
        }
    }

    /// Flushes what is pending, marks the store closed and stops its timers.
    async fn close(self: &Arc<Self>) {
        self.flush(false, true).await;
        for timer in self.timers.lock().drain(..) {
            timer.abort();
        }
    }
}

impl EventNdjsonLogStore {
    /// Creates the directory, prunes retained files at startup, and returns a
    /// store whose loggers write `<prefix><segment>.log` files beside `file_path`.
    pub async fn open(
        file_path: impl AsRef<Path>,
        options: EventNdjsonLogOptions,
    ) -> Result<Self, EventNdjsonLogStoreError> {
        let file_path = file_path.as_ref().to_path_buf();
        let checks = [
            ("maxBytes", options.max_bytes, 1),
            ("maxFiles", options.max_files, 1),
            ("maxTotalBytes", options.max_total_bytes, 1),
            ("maxAgeMs", options.max_age_ms, 1),
            (
                "retentionCheckIntervalMs",
                options.retention_check_interval_ms,
                1,
            ),
            ("maxBufferedBytes", options.max_buffered_bytes, 1),
            ("maxBufferedRecords", options.max_buffered_records, 1),
        ];
        for (option, value, minimum) in checks {
            if value < minimum {
                return Err(EventNdjsonLogStoreError::Configuration {
                    file_path,
                    option,
                    value,
                    minimum,
                });
            }
        }

        let directory = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let file_prefix = provider_log_prefix(&file_path);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|source| EventNdjsonLogStoreError::Directory {
                directory: directory.clone(),
                source,
            })?;

        let initialized_at = now_ms();
        for failure in enforce_retention(
            &directory,
            options.max_total_bytes,
            options.max_age_ms,
            &HashSet::new(),
            &file_prefix,
            initialized_at,
        )
        .await
        {
            tracing::warn!(
                file = %failure.file_path.display(),
                error = %failure.message,
                "provider event log retention failed"
            );
        }

        Ok(Self {
            shared: Arc::new(Shared {
                file_path,
                directory,
                file_prefix,
                options,
                state: Mutex::new(StoreState {
                    last_retention_at: initialized_at,
                    ..StoreState::default()
                }),
                flush_lock: AsyncMutex::new(()),
                timers: Mutex::new(Vec::new()),
            }),
        })
    }

    pub fn file_path(&self) -> &Path {
        &self.shared.file_path
    }

    /// A view that writes `stream` records into this store. Views share the
    /// store's batching, so they do not own it: closing one does not close another.
    pub fn logger(&self, stream: EventNdjsonStream) -> EventNdjsonLogger {
        let shared = Arc::clone(&self.shared);
        let write: Arc<dyn Fn(Value, Option<ThreadId>) -> BoxFuture<'static, ()> + Send + Sync> =
            Arc::new(move |event, thread_id| {
                let shared = Arc::clone(&shared);
                Box::pin(async move { shared.write(stream, event, thread_id).await })
            });
        EventNdjsonLogger {
            file_path: self.shared.file_path.to_string_lossy().into_owned(),
            write,
            close: Arc::new(|| Box::pin(async {})),
        }
    }

    /// Flushes pending records and stops accepting writes. Idempotent.
    pub async fn close(&self) {
        self.shared.close().await;
    }
}

/// `makeEventNdjsonLogger`: a standalone logger for one stream. Its `close`
/// closes the whole store. A store that fails to open logs a warning and
/// yields `None`, so diagnostics never block a provider.
pub async fn make_event_ndjson_logger(
    file_path: impl AsRef<Path>,
    stream: EventNdjsonStream,
    options: EventNdjsonLogOptions,
) -> Option<EventNdjsonLogger> {
    let store = match EventNdjsonLogStore::open(file_path, options).await {
        Ok(store) => Arc::new(store),
        Err(error) => {
            tracing::warn!(error = %error, "provider event log could not start");
            return None;
        }
    };
    let mut logger = store.logger(stream);
    let shared = Arc::clone(&store.shared);
    logger.close = Arc::new(move || {
        let shared = Arc::clone(&shared);
        Box::pin(async move { shared.close().await })
    });
    Some(logger)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    /// Writes flush immediately, so tests read files without waiting on timers.
    fn immediate() -> EventNdjsonLogOptions {
        EventNdjsonLogOptions {
            max_buffered_records: 1,
            ..EventNdjsonLogOptions::default()
        }
    }

    fn thread(value: &str) -> Option<ThreadId> {
        Some(ThreadId::new(value).unwrap())
    }

    fn lines(path: &Path) -> Vec<String> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn filters_transient_canonical_and_native_deltas() {
        assert!(!should_persist_provider_event(
            EventNdjsonStream::Canonical,
            &json!({"type": "content.delta"})
        ));
        assert!(should_persist_provider_event(
            EventNdjsonStream::Orchestration,
            &json!({"type": "content.delta"})
        ));
        assert!(!should_persist_provider_event(
            EventNdjsonStream::Native,
            &json!({"method": "item/agentMessage/delta"})
        ));
        assert!(!should_persist_provider_event(
            EventNdjsonStream::Native,
            &json!({"stage": "raw", "method": "turn/started"})
        ));
        assert!(!should_persist_provider_event(
            EventNdjsonStream::Native,
            &json!({"stage": "decoded", "payload": {"type": "message.part.delta"}})
        ));
        assert!(!should_persist_provider_event(
            EventNdjsonStream::Native,
            &json!({"method": "session/update", "payload": {"update": {"sessionUpdate": "agent_message_chunk"}}})
        ));
        assert!(should_persist_provider_event(
            EventNdjsonStream::Native,
            &json!({"method": "session/update", "payload": {"update": {"sessionUpdate": "tool_call"}}})
        ));
    }

    #[test]
    fn running_tool_snapshots_are_dropped_but_terminal_states_are_kept() {
        let snapshot = |status: &str| json!({"type": "message.part.updated", "payload": {"properties": {"part": {"type": "tool", "state": {"status": status}}}}});
        assert!(!should_persist_provider_event(
            EventNdjsonStream::Native,
            &snapshot("running")
        ));
        assert!(should_persist_provider_event(
            EventNdjsonStream::Native,
            &snapshot("completed")
        ));
        assert!(should_persist_provider_event(
            EventNdjsonStream::Native,
            &snapshot("pending")
        ));
    }

    #[test]
    fn summarizes_large_histories_without_reading_their_items() {
        let event = json!({
            "type": "thread.read",
            "thread": { "turns": [1, 2, 3], "message": "y".repeat(2_000), "title": "not a summary field" },
        });
        assert_eq!(
            summarize_provider_event(&event),
            json!({
                "truncated": true,
                "type": "thread.read",
                "thread": {
                    "truncated": true,
                    "turns": { "itemCount": 3 },
                    "message": { "omittedCharacters": 2_000 },
                },
            })
        );
    }

    #[test]
    fn bounds_oversized_records_and_keeps_failure_details() {
        let event = json!({
            "method": "turn/failed",
            "message": "x".repeat(MAX_RECORD_CHARACTERS),
            "error": { "code": "boom" },
        });
        let bounded = bound_provider_event_for_logging(&event);
        assert_eq!(bounded["method"], json!("turn/failed"));
        assert_eq!(
            bounded["message"],
            json!({ "omittedCharacters": MAX_RECORD_CHARACTERS })
        );
        assert_eq!(
            bounded["error"],
            json!({ "truncated": true, "code": "boom" })
        );
        let small = json!({"type": "turn.started"});
        assert_eq!(bound_provider_event_for_logging(&small), small);
    }

    #[test]
    fn log_prefix_and_thread_segments_follow_upstream_names() {
        assert_eq!(
            provider_log_prefix(Path::new("/logs/provider.log")),
            "provider."
        );
        assert_eq!(
            provider_log_prefix(Path::new("/logs/provider")),
            "provider."
        );
        assert_eq!(resolve_thread_segment(None), "_global");
        assert_eq!(
            resolve_thread_segment(thread("Thread One").as_ref()),
            "thread-one"
        );
    }

    #[tokio::test]
    async fn writes_effect_style_lines_to_thread_scoped_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventNdjsonLogStore::open(dir.path().join("provider.log"), immediate())
            .await
            .unwrap();
        let canonical = store.logger(EventNdjsonStream::Canonical);
        (canonical.write)(json!({"type": "turn.started"}), thread("thread-1")).await;
        (canonical.write)(json!({"type": "turn.completed"}), None).await;

        let thread_lines = lines(&dir.path().join("provider.thread-1.log"));
        assert_eq!(thread_lines.len(), 1);
        assert!(thread_lines[0].starts_with('['));
        assert!(thread_lines[0].ends_with("Z] CANON: {\"type\":\"turn.started\"}"));
        assert_eq!(lines(&dir.path().join("provider._global.log")).len(), 1);
        store.close().await;
    }

    #[tokio::test]
    async fn native_and_canonical_views_share_one_thread_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventNdjsonLogStore::open(dir.path().join("provider.log"), immediate())
            .await
            .unwrap();
        (store.logger(EventNdjsonStream::Native).write)(
            json!({"method": "turn/started"}),
            thread("t"),
        )
        .await;
        (store.logger(EventNdjsonStream::Canonical).write)(
            json!({"type": "turn.started"}),
            thread("t"),
        )
        .await;
        let written = lines(&dir.path().join("provider.t.log"));
        assert_eq!(written.len(), 2);
        assert!(written[0].contains("NTIVE: "));
        assert!(written[1].contains("CANON: "));
        store.close().await;
    }

    #[tokio::test]
    async fn batches_until_the_window_closes_and_close_flushes_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let options = EventNdjsonLogOptions {
            max_buffered_records: 3,
            ..EventNdjsonLogOptions::default()
        };
        let store = EventNdjsonLogStore::open(dir.path().join("provider.log"), options)
            .await
            .unwrap();
        let logger = store.logger(EventNdjsonStream::Canonical);
        (logger.write)(json!({"type": "turn.started"}), thread("t")).await;
        (logger.write)(json!({"type": "turn.completed"}), thread("t")).await;
        assert!(!dir.path().join("provider.t.log").exists());
        store.close().await;
        assert_eq!(lines(&dir.path().join("provider.t.log")).len(), 2);
    }

    #[tokio::test]
    async fn writes_after_close_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventNdjsonLogStore::open(dir.path().join("provider.log"), immediate())
            .await
            .unwrap();
        let logger = store.logger(EventNdjsonStream::Canonical);
        (logger.write)(json!({"type": "turn.started"}), thread("t")).await;
        store.close().await;
        (logger.write)(json!({"type": "turn.completed"}), thread("t")).await;
        assert_eq!(lines(&dir.path().join("provider.t.log")).len(), 1);
    }

    #[tokio::test]
    async fn concurrent_first_writes_to_one_segment_all_land() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventNdjsonLogStore::open(dir.path().join("provider.log"), immediate())
            .await
            .unwrap();
        let logger = store.logger(EventNdjsonStream::Canonical);
        let writes = (0..20).map(|index| {
            (logger.write)(json!({"type": "turn.started", "index": index}), thread("t"))
        });
        futures::future::join_all(writes).await;
        assert_eq!(lines(&dir.path().join("provider.t.log")).len(), 20);
        store.close().await;
    }

    #[tokio::test]
    async fn rotates_per_thread_files_when_max_size_is_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let options = EventNdjsonLogOptions {
            max_bytes: 120,
            ..immediate()
        };
        let store = EventNdjsonLogStore::open(dir.path().join("provider.log"), options)
            .await
            .unwrap();
        let logger = store.logger(EventNdjsonStream::Canonical);
        for index in 0..4 {
            (logger.write)(json!({"type": "turn.started", "index": index}), thread("t")).await;
        }
        assert!(dir.path().join("provider.t.log.1").exists());
        assert!(dir.path().join("provider.t.log").exists());
        store.close().await;
    }

    #[tokio::test]
    async fn enforces_aggregate_byte_retention_on_startup_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["provider.a.log", "provider.b.log", "provider.c.log"] {
            fs::write(dir.path().join(name), "x".repeat(100)).unwrap();
        }
        let options = EventNdjsonLogOptions {
            max_total_bytes: 150,
            ..EventNdjsonLogOptions::default()
        };
        EventNdjsonLogStore::open(dir.path().join("provider.log"), options)
            .await
            .unwrap();
        assert!(!dir.path().join("provider.a.log").exists());
        assert!(!dir.path().join("provider.b.log").exists());
        assert!(dir.path().join("provider.c.log").exists());
    }

    #[tokio::test]
    async fn rejects_configuration_below_its_minimum() {
        let dir = tempfile::tempdir().unwrap();
        let options = EventNdjsonLogOptions {
            max_files: 0,
            ..EventNdjsonLogOptions::default()
        };
        let error = EventNdjsonLogStore::open(dir.path().join("provider.log"), options)
            .await
            .err()
            .unwrap();
        assert!(matches!(
            error,
            EventNdjsonLogStoreError::Configuration {
                option: "maxFiles",
                value: 0,
                minimum: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_standalone_logger_closes_its_store() {
        let dir = tempfile::tempdir().unwrap();
        let logger = make_event_ndjson_logger(
            dir.path().join("provider.log"),
            EventNdjsonStream::Native,
            immediate(),
        )
        .await
        .unwrap();
        (logger.write)(json!({"method": "turn/started"}), thread("t")).await;
        (logger.close)().await;
        (logger.write)(json!({"method": "turn/completed"}), thread("t")).await;
        assert_eq!(lines(&dir.path().join("provider.t.log")).len(), 1);
    }
}
