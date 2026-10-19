//! Port of upstream/packages/provider-opencode/src/server/usage.ts
//!
//! Usage history for OpenCode, read from its SQLite databases and legacy JSON
//! message store under each data directory. Nothing here writes to either store.
//!
//! Upstream reads through Effect's `SqlClient` and `FileSystem`; here the reads
//! are blocking `rusqlite` and `std::fs` calls that run on a blocking thread.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use regex::Regex;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};
use turin_contracts::usage::{UsageProviderKind, UsageSourceStatus};
use turin_provider_core::host_process::HostProcess;
use turin_provider_core::path_expansion::expand_home_path;
use turin_provider_core::rt::boxed;
use turin_provider_core::usage::{
    ProviderUsageFile, ProviderUsageReader, ProviderUsageScan, UsageRecord, UsageSpeed,
    token_count, total_tokens,
};
use turin_shared::js_string::locale_compare;

/// Rowids one statement covers, however few of them match.
const MESSAGE_PAGE_SIZE: i64 = 64;

/// Entries stat'd before any is processed, so one huge directory is not held in
/// memory at once.
const LEGACY_STAT_BATCH: usize = 256;

fn object(value: Option<&Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn text(value: Option<&Value>) -> &str {
    match value {
        Some(Value::String(text)) => text,
        _ => "",
    }
}

fn tokens(value: Option<&Value>) -> u64 {
    value.map_or(0, token_count)
}

/// What a stored row supplies when the message body leaves a field out.
#[derive(Debug, Default, Clone)]
pub struct MessageFallback<'a> {
    pub id: &'a str,
    pub session_id: &'a str,
    pub timestamp_ms: Option<f64>,
}

/// OpenCode stores uncached input and reasoning separately from input/output.
pub fn parse_open_code_message(
    source: &str,
    fallback: &MessageFallback<'_>,
) -> Option<UsageRecord> {
    let parsed: Value = serde_json::from_str(source).ok()?;
    let message = object(Some(&parsed));
    match message.get("role") {
        None => {}
        Some(Value::String(role)) if role == "assistant" => {}
        Some(_) => return None,
    }
    let usage = object(message.get("tokens"));
    let cache = object(usage.get("cache"));
    let model_reference = object(message.get("model"));
    let model = [
        text(model_reference.get("id")),
        text(model_reference.get("modelID")),
        text(message.get("modelID")),
    ]
    .into_iter()
    .find(|candidate| !candidate.is_empty())
    .unwrap_or("");
    // `object(message.time).created ?? fallback.timestampMs`: only a missing or null
    // `created` falls back; a non-number does not.
    let timestamp_ms = match object(message.get("time")).get("created") {
        None | Some(Value::Null) => fallback.timestamp_ms,
        Some(Value::Number(number)) => number.as_f64(),
        Some(_) => None,
    };
    let timestamp_ms = timestamp_ms.filter(|value| value.is_finite());
    if model.is_empty() {
        return None;
    }
    let timestamp_ms = timestamp_ms?;
    let reasoning_tokens = tokens(usage.get("reasoning"));
    let totals = turin_contracts::usage::UsageTokenTotals {
        uncached_input_tokens: tokens(usage.get("input")),
        cached_input_tokens: tokens(cache.get("read")),
        cache_creation_tokens: tokens(cache.get("write")),
        output_tokens: tokens(usage.get("output")) + reasoning_tokens,
        reasoning_tokens,
    };
    if total_tokens(&totals) == 0 {
        return None;
    }
    let id = if fallback.id.is_empty() {
        text(message.get("id"))
    } else {
        fallback.id
    };
    let session_id = if fallback.session_id.is_empty() {
        text(message.get("sessionID"))
    } else {
        fallback.session_id
    };
    Some(UsageRecord {
        provider: UsageProviderKind::Opencode,
        timestamp_ms,
        model: model.to_owned(),
        rate_model: None,
        session_id: session_id.to_owned(),
        totals,
        // OpenCode writes zero for models without a known rate, including paid
        // subscription models. Let the shared price table estimate those records.
        reported_cost_usd: message
            .get("cost")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost > 0.0),
        speed: UsageSpeed::Standard,
        dedupe_key: (!id.is_empty()).then(|| format!("opencode:{id}")),
    })
}

#[derive(Debug, Default, PartialEq)]
pub struct OpenCodeUsageReadResult {
    pub files: Vec<ProviderUsageFile>,
    pub missing: bool,
    pub error: bool,
}

fn is_not_found(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

/// Whether the path is itself a symlink, without following it.
fn is_symbolic_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

fn modified_ms(metadata: &std::fs::Metadata) -> Option<f64> {
    let modified = metadata.modified().ok()?;
    let millis = match modified.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs_f64() * 1000.0,
        Err(before) => -before.duration().as_secs_f64() * 1000.0,
    };
    Some(millis.floor())
}

/// The `[start, end]` rowid pages one table is read in: each covers at most
/// [`MESSAGE_PAGE_SIZE`] rowids, however few match, and rows appended mid-scan
/// (above `last_row_id`) are left out. A statement matches `rowid > start AND
/// rowid <= end`.
pub fn rowid_pages(first_row_id: i64, last_row_id: i64) -> Vec<(i64, i64)> {
    let mut pages = Vec::new();
    let mut after = first_row_id - 1;
    while after < last_row_id {
        pages.push((after, (after + MESSAGE_PAGE_SIZE).min(last_row_id)));
        after += MESSAGE_PAGE_SIZE;
    }
    pages
}

struct Collector {
    since_ms: f64,
    seen: HashSet<String>,
}

impl Collector {
    fn append(&mut self, records: &mut Vec<UsageRecord>, record: Option<UsageRecord>) {
        let Some(record) = record else {
            return;
        };
        if record.timestamp_ms < self.since_ms {
            return;
        }
        if let Some(key) = &record.dedupe_key
            && !self.seen.insert(key.clone())
        {
            return;
        }
        records.push(record);
    }
}

fn value_text<'a>(value: ValueRef<'a>) -> &'a str {
    match value {
        ValueRef::Text(bytes) => std::str::from_utf8(bytes).unwrap_or(""),
        _ => "",
    }
}

fn value_number(value: ValueRef<'_>) -> Option<f64> {
    match value {
        ValueRef::Integer(number) => Some(number as f64),
        ValueRef::Real(number) => Some(number).filter(|number| number.is_finite()),
        _ => None,
    }
}

/// Reads one `opencode*.db` read-only. `Err` is a `SqlError`: the caller reports a
/// partial read and keeps what was collected before it.
fn read_database(
    path: &Path,
    collector: &mut Collector,
    records: &mut Vec<UsageRecord>,
    error: &mut bool,
) -> rusqlite::Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // A busy live provider should fail this source promptly rather than stalling
    // while SQLite waits for its writer.
    connection.busy_timeout(Duration::from_millis(100))?;
    let tables: HashSet<String> = {
        let mut statement =
            connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !tables.contains("message") && !tables.contains("session_message") {
        *error = true;
    }
    for table in ["message", "session_message"] {
        if !tables.contains(table) {
            continue;
        }
        let columns: HashSet<String> = {
            let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
            let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let timestamp = if columns.contains("time_created") {
            "time_created"
        } else {
            "NULL"
        };
        let mut predicates: Vec<&str> = Vec::new();
        if table == "session_message" {
            predicates.push("type = 'assistant'");
        }
        if timestamp != "NULL" {
            predicates.push("time_created >= ?");
        }
        predicates.push("rowid > ?");
        predicates.push("rowid <= ?");
        let (first_row_id, last_row_id): (Option<i64>, Option<i64>) = connection.query_row(
            &format!(
                "SELECT (SELECT MIN(rowid) FROM {table}) AS first_row_id, (SELECT MAX(rowid) FROM {table}) AS last_row_id"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (Some(first_row_id), Some(last_row_id)) = (first_row_id, last_row_id) else {
            continue;
        };
        let mut statement = connection.prepare(&format!(
            "SELECT id, session_id, data, {timestamp} AS created FROM {table} WHERE {} ORDER BY rowid",
            predicates.join(" AND ")
        ))?;
        // Each statement covers a page of rowids, however few match, so a window that
        // excludes most of a large history never materializes it at once.
        for (after, upto) in rowid_pages(first_row_id, last_row_id) {
            let mut rows = if timestamp == "NULL" {
                statement.query(rusqlite::params![after, upto])?
            } else {
                statement.query(rusqlite::params![collector.since_ms, after, upto])?
            };
            while let Some(row) = rows.next()? {
                let created = value_number(row.get_ref(3)?);
                let record = parse_open_code_message(
                    value_text(row.get_ref(2)?),
                    &MessageFallback {
                        id: value_text(row.get_ref(0)?),
                        session_id: value_text(row.get_ref(1)?),
                        timestamp_ms: created,
                    },
                );
                collector.append(records, record);
            }
        }
    }
    Ok(())
}

/// The `opencode.db` first, then `opencode-<name>.db` files in name order.
fn list_databases(root: &Path) -> std::io::Result<Vec<String>> {
    let pattern = Regex::new(r"^opencode(?:-[a-zA-Z0-9_-]+)?\.db$").expect("database pattern");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if !pattern.is_match(&name) {
            continue;
        }
        // One database removed between listing and stat must not hide the rest. A
        // symlink reports as itself and is never followed.
        let path = root.join(&name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_file() {
                    names.push(name);
                }
            }
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    names.sort_by(|a, b| {
        if a == "opencode.db" {
            std::cmp::Ordering::Less
        } else if b == "opencode.db" {
            std::cmp::Ordering::Greater
        } else {
            locale_compare(a, b)
        }
    });
    Ok(names)
}

/// One directory of the legacy store. `Err` is a failure that is not "not found": the
/// rest of the directory is left unread, as upstream's walk does.
fn walk_legacy_directory(
    directory: &Path,
    directories: &mut Vec<PathBuf>,
    collector: &mut Collector,
    files: &mut Vec<ProviderUsageFile>,
    found: &mut bool,
    error: &mut bool,
) -> std::io::Result<()> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    for batch in names.chunks(LEGACY_STAT_BATCH) {
        for name in batch {
            let entry = directory.join(name);
            // `stat` follows symlinks. A symlink whose target cannot be stat'd is
            // skipped, not an error.
            let info = match std::fs::metadata(&entry) {
                Ok(info) => Some(info),
                Err(cause) if is_not_found(&cause) => None,
                Err(cause) => {
                    if is_symbolic_link(&entry) {
                        None
                    } else {
                        return Err(cause);
                    }
                }
            };
            let Some(info) = info else {
                continue;
            };
            if info.is_dir() {
                if !is_symbolic_link(&entry) {
                    directories.push(entry);
                }
            } else if info.is_file()
                && let Some(id) = name.strip_suffix(".json")
            {
                // A message cannot be created after its file was last written, so a
                // file untouched since the window opened holds nothing in range.
                let skip = collector.seen.contains(&format!("opencode:{id}"))
                    || modified_ms(&info).is_some_and(|mtime| mtime < collector.since_ms);
                // A skipped file only matters as proof that the store exists.
                if *found && skip {
                    continue;
                }
                if is_symbolic_link(&entry) {
                    continue;
                }
                *found = true;
                if skip {
                    continue;
                }
                let mut records = Vec::new();
                match std::fs::read_to_string(&entry) {
                    Ok(source) => {
                        let record = parse_open_code_message(
                            &source,
                            &MessageFallback {
                                id,
                                ..Default::default()
                            },
                        );
                        collector.append(&mut records, record);
                    }
                    Err(cause) => {
                        if !is_not_found(&cause) {
                            *error = true;
                        }
                    }
                }
                files.push(ProviderUsageFile {
                    path: entry.to_string_lossy().into_owned(),
                    records,
                });
            }
        }
    }
    Ok(())
}

fn read_open_code_usage_blocking(root: &Path, since_ms: f64) -> OpenCodeUsageReadResult {
    let mut files: Vec<ProviderUsageFile> = Vec::new();
    let mut collector = Collector {
        since_ms,
        seen: HashSet::new(),
    };
    let mut found = false;
    let mut error = false;

    let databases = match list_databases(root) {
        Ok(names) => names,
        Err(cause) => {
            if !is_not_found(&cause) {
                error = true;
            }
            Vec::new()
        }
    };
    for name in databases {
        found = true;
        let path = root.join(&name);
        let mut records = Vec::new();
        if read_database(&path, &mut collector, &mut records, &mut error).is_err() {
            error = true;
        }
        files.push(ProviderUsageFile {
            path: path.to_string_lossy().into_owned(),
            records,
        });
    }

    // Do not follow symlinks, including cycles. Database records win over their old
    // JSON copies when OpenCode has migrated a store in place.
    let mut directories = vec![root.join("storage").join("message")];
    while let Some(directory) = directories.pop() {
        if let Err(cause) = walk_legacy_directory(
            &directory,
            &mut directories,
            &mut collector,
            &mut files,
            &mut found,
            &mut error,
        ) && !is_not_found(&cause)
        {
            error = true;
        }
    }
    OpenCodeUsageReadResult {
        files,
        missing: !found && !error,
        error,
    }
}

/// Reads current SQLite and pre-migration JSON stores without modifying either.
pub async fn read_open_code_usage(root: &Path, since_ms: f64) -> OpenCodeUsageReadResult {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || read_open_code_usage_blocking(&root, since_ms))
        .await
        .unwrap_or(OpenCodeUsageReadResult {
            files: Vec::new(),
            missing: false,
            error: true,
        })
}

/// `path.resolve`: absolute, with `.` and `..` folded away lexically.
fn resolve_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// The data directories to read: `OPENCODE_DATA_DIR` (comma-separated) or the XDG
/// default, canonicalized so aliases count once.
pub fn resolve_open_code_data_dirs(process: &HostProcess) -> Vec<PathBuf> {
    let roots: Vec<String> = process
        .environment
        .get("OPENCODE_DATA_DIR")
        .map(|value| {
            value
                .split(',')
                .map(|root| root.trim().to_owned())
                .filter(|root| !root.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let data_home = process
        .environment
        .get("XDG_DATA_HOME")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let default_root = match data_home {
        Some(home) if Path::new(home).is_absolute() => Path::new(home).join("opencode"),
        _ => Path::new(&process.home_directory)
            .join(".local")
            .join("share")
            .join("opencode"),
    };
    let roots = if roots.is_empty() {
        vec![default_root.to_string_lossy().into_owned()]
    } else {
        roots
    };
    let mut canonical: Vec<PathBuf> = Vec::new();
    for root in roots {
        let resolved = resolve_path(Path::new(&expand_home_path(&root, &process.home_directory)));
        let real = std::fs::canonicalize(&resolved).unwrap_or(resolved);
        if !canonical.contains(&real) {
            canonical.push(real);
        }
    }
    canonical
}

/// One scan result per data directory, read concurrently.
async fn scan_open_code_usage(roots: Vec<PathBuf>, window_start_ms: f64) -> Vec<ProviderUsageScan> {
    let reads = roots.into_iter().map(|dir| async move {
        let result = read_open_code_usage(&dir, window_start_ms).await;
        ProviderUsageScan {
            dir: dir.to_string_lossy().into_owned(),
            volume_id: None,
            host_id: None,
            status: Some(if result.error {
                UsageSourceStatus::Partial
            } else {
                UsageSourceStatus::Ok
            }),
            message: result
                .error
                .then(|| "Some OpenCode history could not be read.".to_owned()),
            action: None,
            files: if result.missing && !result.error {
                None
            } else {
                Some(result.files)
            },
            refreshing: false,
        }
    });
    futures::future::join_all(reads).await
}

/// `openCodeUsageReader` over the given process facts (`HostProcess.Environment`,
/// `HostProcess.HomeDirectory`).
pub fn open_code_usage_reader_for(process: HostProcess) -> ProviderUsageReader {
    ProviderUsageReader::Scan {
        provider: UsageProviderKind::Opencode,
        scan: Arc::new(move |input| {
            let roots = resolve_open_code_data_dirs(&process);
            boxed(async move { Ok(scan_open_code_usage(roots, input.window_start_ms).await) })
        }),
    }
}

/// `openCodeUsageReader`: the running process's environment.
pub fn open_code_usage_reader() -> ProviderUsageReader {
    open_code_usage_reader_for(HostProcess::current())
}

#[cfg(test)]
mod tests;
