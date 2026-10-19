//! Port of upstream/packages/provider-opencode/src/server/usage.test.ts, plus the
//! message parser and the data-directory resolution, which upstream covers through
//! the same reads.
//!
//! Not ported: the two `node:sqlite` spies. One bounds the bytes each `StatementSync.all()`
//! returns; the other records each statement's rowid span. A `rusqlite` statement
//! streams its rows, so the first has nothing to bound, and the second is covered
//! through `rowid_pages`, the one function the paging loop takes its spans from.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use pretty_assertions::assert_eq;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use turin_contracts::usage::{UsageSourceStatus, UsageTokenTotals};
use turin_provider_core::usage::ProviderUsageScanInput;

use super::*;

fn dedupe_keys(result: &OpenCodeUsageReadResult) -> Vec<String> {
    result
        .files
        .iter()
        .flat_map(|file| file.records.iter())
        .map(|record| record.dedupe_key.clone().expect("dedupe key"))
        .collect()
}

fn paths(result: &OpenCodeUsageReadResult) -> Vec<String> {
    result.files.iter().map(|file| file.path.clone()).collect()
}

fn set_mtime(path: &Path, ms: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open for mtime")
        .set_modified(UNIX_EPOCH + Duration::from_millis(ms))
        .expect("set mtime");
}

const SINCE_MS: u64 = 1_780_000_000_000;

/// A legacy message file with a record inside the window.
fn legacy_message(id: &str) -> String {
    json!({
        "id": id,
        "sessionID": "session-1",
        "role": "assistant",
        "modelID": "claude-sonnet-4-5",
        "time": {"created": SINCE_MS + 1000},
        "tokens": {"input": 100, "output": 20},
    })
    .to_string()
}

fn write_legacy(directory: &Path, id: &str, mtime_ms: u64) -> PathBuf {
    let file = directory.join(format!("{id}.json"));
    fs::write(&file, legacy_message(id)).expect("write legacy message");
    set_mtime(&file, mtime_ms);
    file
}

#[tokio::test]
async fn counts_every_eligible_message_once_across_sparse_rowids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let connection = Connection::open(dir.path().join("opencode.db")).expect("db");
    connection
        .execute(
            "CREATE TABLE message (id TEXT, session_id TEXT, data TEXT, time_created INTEGER)",
            [],
        )
        .expect("create");
    let message = json!({
        "role": "assistant",
        "modelID": "claude-sonnet-4-5",
        "time": {"created": SINCE_MS},
        "tokens": {"input": 100, "output": 20},
        // Transcript bodies are irrelevant to usage, but can be very large.
        "body": "x".repeat(64 * 1024),
    });
    let data = message.to_string();
    let insert = |row_id: i64, id: &str, payload: &str, created: u64| {
        connection
            .execute(
                "INSERT INTO message (rowid, id, session_id, data, time_created) VALUES (?, ?, ?, ?, ?)",
                params![row_id, id, "session-1", payload, created as i64],
            )
            .expect("insert");
    };
    // Sparse IDs, including negative rowids, must not skip records at page boundaries.
    for index in 0..130 {
        insert(index * 2 - 10, &format!("msg-{index}"), &data, SINCE_MS);
    }
    insert(300, "old", &data, SINCE_MS - 1);
    let mut user = message.clone();
    user["role"] = json!("user");
    insert(301, "user", &user.to_string(), SINCE_MS);
    insert(302, "invalid", "{invalid", SINCE_MS);
    insert(303, "msg-0", &data, SINCE_MS);

    let result = read_open_code_usage(dir.path(), SINCE_MS as f64).await;

    assert!(!result.error);
    assert_eq!(
        dedupe_keys(&result),
        (0..130)
            .map(|index| format!("opencode:msg-{index}"))
            .collect::<Vec<_>>()
    );
    let uncached: u64 = result
        .files
        .iter()
        .flat_map(|file| file.records.iter())
        .map(|record| record.totals.uncached_input_tokens)
        .sum();
    assert_eq!(uncached, 13_000);
}

#[test]
fn reads_at_most_64_rowids_per_statement_and_covers_every_rowid() {
    for (first, last) in [(1, 305), (-10, 248), (5, 5), (0, 63), (0, 64), (0, 65)] {
        let pages = rowid_pages(first, last);
        assert!(!pages.is_empty());
        assert!(
            pages.iter().all(|(after, upto)| upto - after <= 64),
            "{first}..{last}: {pages:?}"
        );
        // Contiguous: every page starts where the last ended, from `first - 1` to `last`.
        assert_eq!(pages[0].0, first - 1);
        assert_eq!(pages.last().map(|page| page.1), Some(last));
        assert!(pages.windows(2).all(|pair| pair[0].1 == pair[1].0));
    }
}

#[tokio::test]
async fn reads_only_the_window_of_a_history_that_is_mostly_older() {
    let dir = tempfile::tempdir().expect("tempdir");
    let connection = Connection::open(dir.path().join("opencode.db")).expect("db");
    connection
        .execute(
            "CREATE TABLE message (id TEXT, session_id TEXT, data TEXT, time_created INTEGER)",
            [],
        )
        .expect("create");
    let data = json!({
        "role": "assistant",
        "modelID": "claude-sonnet-4-5",
        "tokens": {"input": 100, "output": 20},
    })
    .to_string();
    // OpenCode appends, so the history before the window comes first.
    connection.execute_batch("BEGIN").expect("begin");
    for index in 0..305_i64 {
        connection
            .execute(
                "INSERT INTO message VALUES (?, ?, ?, ?)",
                params![
                    format!("msg-{index}"),
                    "session-1",
                    data,
                    SINCE_MS as i64 + index - 300
                ],
            )
            .expect("insert");
    }
    connection.execute_batch("COMMIT").expect("commit");

    let result = read_open_code_usage(dir.path(), SINCE_MS as f64).await;

    assert!(!result.error);
    assert_eq!(
        dedupe_keys(&result),
        (300..305)
            .map(|index| format!("opencode:msg-{index}"))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn counts_migrated_messages_once_and_sees_subsequent_wal_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let connection = Connection::open(dir.path().join("opencode.db")).expect("db");
    connection
        .query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
        .expect("wal");
    connection
        .execute_batch("PRAGMA wal_autocheckpoint = 0; CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
        .expect("create");
    let message = json!({
        "id": "msg-1",
        "sessionID": "session-1",
        "role": "assistant",
        "modelID": "claude-sonnet-4-5",
        "time": {"created": 1_780_000_000_000_u64},
        "cost": 0.25,
        "tokens": {"input": 100, "output": 20, "reasoning": 5, "cache": {"read": 30, "write": 10}},
    });
    let insert = |id: &str, session_id: &str, data: &str| {
        connection
            .execute(
                "INSERT INTO message VALUES (?, ?, ?)",
                params![id, session_id, data],
            )
            .expect("insert");
    };
    insert("msg-1", "session-1", &message.to_string());
    let legacy = dir.path().join("storage/message/session-1");
    fs::create_dir_all(&legacy).expect("legacy dir");
    fs::write(legacy.join("msg-1.json"), message.to_string()).expect("legacy file");

    let first = read_open_code_usage(dir.path(), 0.0).await;

    assert!(!first.error);
    let records: Vec<_> = first.files.iter().flat_map(|file| &file.records).collect();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].totals,
        UsageTokenTotals {
            uncached_input_tokens: 100,
            cached_input_tokens: 30,
            cache_creation_tokens: 10,
            output_tokens: 25,
            reasoning_tokens: 5,
        }
    );
    assert_eq!(records[0].reported_cost_usd, Some(0.25));

    let mut next_message = message.clone();
    next_message["id"] = json!("msg-2");
    next_message["time"] = json!({"created": 1_780_000_001_000_u64});
    insert("msg-2", "session-1", &next_message.to_string());
    let next = read_open_code_usage(dir.path(), 1_780_000_001_000.0).await;

    assert!(!next.error);
    assert_eq!(dedupe_keys(&next), vec!["opencode:msg-2"]);
    let wal = fs::metadata(dir.path().join("opencode.db-wal")).expect("wal file");
    assert!(wal.len() > 0);
}

#[tokio::test]
async fn reads_only_legacy_messages_written_since_the_window_opened() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join("storage/message/session-1");
    fs::create_dir_all(&legacy).expect("legacy dir");
    write_legacy(&legacy, "msg-old", SINCE_MS - 60_000);

    let stale = read_open_code_usage(dir.path(), SINCE_MS as f64).await;
    assert_eq!(
        stale,
        OpenCodeUsageReadResult {
            files: Vec::new(),
            missing: false,
            error: false
        }
    );

    let recent = write_legacy(&legacy, "msg-new", SINCE_MS + 60_000);
    let result = read_open_code_usage(dir.path(), SINCE_MS as f64).await;
    assert!(!result.missing);
    assert!(!result.error);
    assert_eq!(paths(&result), vec![recent.to_string_lossy().into_owned()]);
    assert_eq!(dedupe_keys(&result), vec!["opencode:msg-new"]);
}

#[cfg(unix)]
#[tokio::test]
async fn never_follows_symlinks_in_the_legacy_store() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");
    let store = dir.path().join("storage/message");
    let session = store.join("session-1");
    fs::create_dir_all(&session).expect("session");
    let target = write_legacy(outside.path(), "msg-linked", SINCE_MS + 60_000);
    let stale = write_legacy(outside.path(), "msg-stale", SINCE_MS - 60_000);
    symlink(&target, session.join("msg-linked.json")).expect("link");
    symlink(&stale, session.join("msg-stale.json")).expect("link");
    symlink(outside.path(), store.join("session-linked")).expect("link");
    symlink(&store, session.join("cycle")).expect("link");
    symlink(outside.path().join("gone.json"), session.join("gone.json")).expect("link");
    symlink(session.join("loop.json"), session.join("loop.json")).expect("link");

    // Links alone are not a store, even when one points at a stale message.
    let links_only = read_open_code_usage(dir.path(), SINCE_MS as f64).await;
    assert_eq!(
        links_only,
        OpenCodeUsageReadResult {
            files: Vec::new(),
            missing: true,
            error: false
        }
    );

    let real = write_legacy(&session, "msg-real", SINCE_MS + 60_000);
    let result = read_open_code_usage(dir.path(), SINCE_MS as f64).await;
    assert!(!result.missing);
    assert!(!result.error);
    assert_eq!(paths(&result), vec![real.to_string_lossy().into_owned()]);
    assert_eq!(dedupe_keys(&result), vec!["opencode:msg-real"]);
}

#[tokio::test]
async fn reads_a_large_legacy_session_in_directory_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let session = dir.path().join("storage/message/session-1");
    fs::create_dir_all(&session).expect("session");
    // More entries than the walk stats at once.
    for index in 0..600 {
        fs::write(
            session.join(format!("msg-{index}.json")),
            json!({
                "id": format!("msg-{index}"),
                "sessionID": "session-1",
                "role": "assistant",
                "modelID": "claude-sonnet-4-5",
                "time": {"created": 1_780_000_000_000_u64},
                "tokens": {"input": 100, "output": 20},
            })
            .to_string(),
        )
        .expect("write");
    }
    let names: Vec<String> = fs::read_dir(&session)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let result = read_open_code_usage(dir.path(), 0.0).await;

    assert!(!result.error);
    assert_eq!(
        paths(&result),
        names
            .iter()
            .map(|name| session.join(name).to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        result
            .files
            .iter()
            .map(|file| file.records.len())
            .sum::<usize>(),
        600
    );
}

#[tokio::test]
async fn a_missing_root_is_missing_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");

    let result = read_open_code_usage(&dir.path().join("absent"), 0.0).await;

    assert_eq!(
        result,
        OpenCodeUsageReadResult {
            files: Vec::new(),
            missing: true,
            error: false
        }
    );
}

#[tokio::test]
async fn a_database_without_message_tables_reports_a_partial_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let connection = Connection::open(dir.path().join("opencode.db")).expect("db");
    connection
        .execute("CREATE TABLE other (id TEXT)", [])
        .expect("create");
    drop(connection);

    let result = read_open_code_usage(dir.path(), 0.0).await;

    assert!(result.error);
    assert!(!result.missing);
    assert_eq!(result.files.len(), 1);
}

#[tokio::test]
async fn reads_the_main_database_before_named_ones_and_ignores_other_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["opencode-beta.db", "opencode.db", "opencode-alpha.db"] {
        let connection = Connection::open(dir.path().join(name)).expect("db");
        connection
            .execute(
                "CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)",
                [],
            )
            .expect("create");
    }
    fs::write(dir.path().join("opencode.db-wal"), b"").expect("wal");
    fs::write(dir.path().join("other.db"), b"").expect("other");

    let result = read_open_code_usage(dir.path(), 0.0).await;

    let names: Vec<_> = result
        .files
        .iter()
        .map(|file| {
            Path::new(&file.path)
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        vec!["opencode.db", "opencode-alpha.db", "opencode-beta.db"]
    );
    assert!(!result.error);
}

fn parse(source: Value) -> Option<UsageRecord> {
    parse_open_code_message(&source.to_string(), &MessageFallback::default())
}

#[test]
fn parses_uncached_input_cache_and_reasoning_separately() {
    let record = parse(json!({
        "id": "m1", "sessionID": "s1", "role": "assistant",
        "model": {"id": "claude-sonnet-4-5"},
        "time": {"created": 5},
        "cost": 0.5,
        "tokens": {"input": 10, "output": 4, "reasoning": 2, "cache": {"read": 3, "write": 1}},
    }))
    .expect("record");

    assert_eq!(record.model, "claude-sonnet-4-5");
    assert_eq!(record.session_id, "s1");
    assert_eq!(record.timestamp_ms, 5.0);
    assert_eq!(
        record.totals,
        UsageTokenTotals {
            uncached_input_tokens: 10,
            cached_input_tokens: 3,
            cache_creation_tokens: 1,
            output_tokens: 6,
            reasoning_tokens: 2,
        }
    );
    assert_eq!(record.reported_cost_usd, Some(0.5));
    assert_eq!(record.dedupe_key.as_deref(), Some("opencode:m1"));
}

#[test]
fn takes_the_model_from_the_first_present_field() {
    let base = |model: Value| {
        let mut message = json!({"time": {"created": 1}, "tokens": {"input": 1}});
        for (key, value) in model.as_object().expect("object") {
            message[key] = value.clone();
        }
        message
    };
    let model = |message: Value| parse(message).map(|record| record.model);
    assert_eq!(
        model(base(
            json!({"model": {"id": "a", "modelID": "b"}, "modelID": "c"})
        ))
        .as_deref(),
        Some("a")
    );
    assert_eq!(
        model(base(json!({"model": {"modelID": "b"}, "modelID": "c"}))).as_deref(),
        Some("b")
    );
    assert_eq!(model(base(json!({"modelID": "c"}))).as_deref(), Some("c"));
    assert_eq!(model(base(json!({}))), None);
}

#[test]
fn skips_user_messages_empty_usage_and_unusable_timestamps() {
    let base = json!({"modelID": "m", "time": {"created": 1}, "tokens": {"input": 1}});
    assert!(parse(base.clone()).is_some());
    for (key, value) in [
        ("role", json!("user")),
        ("role", Value::Null),
        ("tokens", json!({"input": 0, "output": -3})),
        ("time", json!({"created": "soon"})),
        ("time", json!({})),
    ] {
        let mut message = base.clone();
        message[key] = value.clone();
        assert!(parse(message).is_none(), "{key}={value}");
    }
    assert!(parse_open_code_message("{invalid", &MessageFallback::default()).is_none());
    assert!(parse_open_code_message("[]", &MessageFallback::default()).is_none());
}

#[test]
fn falls_back_to_the_stored_rows_identity_and_timestamp() {
    let source = json!({"modelID": "m", "tokens": {"input": 1}}).to_string();

    let record = parse_open_code_message(
        &source,
        &MessageFallback {
            id: "row-1",
            session_id: "row-session",
            timestamp_ms: Some(9.0),
        },
    )
    .expect("record");
    assert_eq!(record.session_id, "row-session");
    assert_eq!(record.timestamp_ms, 9.0);
    assert_eq!(record.dedupe_key.as_deref(), Some("opencode:row-1"));

    // A null `created` falls back too; a missing fallback leaves no timestamp.
    let null_created = json!({"modelID": "m", "tokens": {"input": 1}, "time": {"created": null}});
    assert!(parse(null_created).is_none());
}

#[test]
fn a_zero_or_missing_cost_is_left_for_the_price_table() {
    for cost in [json!(0), json!(-1), json!("1"), Value::Null] {
        let record = parse(json!({
            "modelID": "m", "time": {"created": 1}, "tokens": {"input": 1}, "cost": cost,
        }))
        .expect("record");
        assert_eq!(record.reported_cost_usd, None, "{cost}");
    }
}

fn process(environment: &[(&str, &str)], home: &str) -> HostProcess {
    HostProcess {
        platform: turin_provider_core::host_process::Platform::Linux,
        home_directory: home.to_owned(),
        environment: environment
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>(),
    }
}

#[test]
fn resolves_the_xdg_default_and_the_explicit_directories() {
    let home = tempfile::tempdir().expect("home");
    let home_path = home.path().to_string_lossy().into_owned();

    // A directory that does not exist stays as resolved, as `realPath` failing does upstream.
    assert_eq!(
        resolve_open_code_data_dirs(&process(&[], &home_path)),
        vec![home.path().join(".local/share/opencode")]
    );
    // A relative XDG_DATA_HOME is ignored.
    assert_eq!(
        resolve_open_code_data_dirs(&process(&[("XDG_DATA_HOME", "relative")], &home_path)),
        vec![home.path().join(".local/share/opencode")]
    );
    let xdg = tempfile::tempdir().expect("xdg");
    assert_eq!(
        resolve_open_code_data_dirs(&process(
            &[("XDG_DATA_HOME", &xdg.path().to_string_lossy())],
            &home_path
        )),
        vec![xdg.path().join("opencode")]
    );
    // OPENCODE_DATA_DIR wins, is comma-separated, expands `~`, and counts aliases once;
    // a directory that exists is canonicalized.
    let a = tempfile::tempdir().expect("a");
    let list = format!(
        " {} , ~/opencode-data ,{}/../{},, ",
        a.path().display(),
        a.path().display(),
        a.path().file_name().expect("name").to_string_lossy()
    );
    assert_eq!(
        resolve_open_code_data_dirs(&process(&[("OPENCODE_DATA_DIR", &list)], &home_path)),
        vec![
            fs::canonicalize(a.path()).expect("canonical a"),
            home.path().join("opencode-data"),
        ]
    );
}

#[tokio::test]
async fn the_reader_scans_each_data_directory() {
    let present = tempfile::tempdir().expect("present");
    let absent = tempfile::tempdir().expect("absent");
    let broken = tempfile::tempdir().expect("broken");
    let session = present.path().join("storage/message/session-1");
    fs::create_dir_all(&session).expect("session");
    write_legacy(&session, "msg-1", SINCE_MS + 1);
    let connection = Connection::open(broken.path().join("opencode.db")).expect("db");
    connection
        .execute("CREATE TABLE other (id TEXT)", [])
        .expect("create");
    drop(connection);
    let list = [present.path(), &absent.path().join("nope"), broken.path()]
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(",");
    let reader =
        open_code_usage_reader_for(process(&[("OPENCODE_DATA_DIR", &list)], "/nonexistent"));
    let ProviderUsageReader::Scan { provider, scan } = reader else {
        panic!("OpenCode keeps a scan reader");
    };
    assert_eq!(provider, UsageProviderKind::Opencode);

    let scans = scan(ProviderUsageScanInput {
        instances: Vec::new(),
        settings: serde_json::from_value(json!({})).expect("settings"),
        window_start_ms: SINCE_MS as f64,
        retention_cutoff_ms: 0.0,
        await_refresh: false,
    })
    .await
    .expect("scan");

    assert_eq!(scans.len(), 3);
    assert_eq!(scans[0].status, Some(UsageSourceStatus::Ok));
    assert_eq!(scans[0].files.as_ref().map(Vec::len), Some(1));
    assert_eq!(scans[0].message, None);
    // A directory that does not exist is a missing source, not a failure.
    assert_eq!(scans[1].status, Some(UsageSourceStatus::Ok));
    assert!(scans[1].files.is_none());
    assert_eq!(scans[2].status, Some(UsageSourceStatus::Partial));
    assert_eq!(
        scans[2].message.as_deref(),
        Some("Some OpenCode history could not be read.")
    );
}
