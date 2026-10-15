use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{Connection, params};

use crate::migrations::{RunMigrationsOptions, migration_manifest, run_migrations};

use super::snapshot::{CopyLimits, STALE_AFTER, backup_with_limits, remove_stale_dirs};
use super::*;

fn file_db(dir: &Path, name: &str, through: Option<i64>) -> PathBuf {
    let path = dir.join(name);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
    run_migrations(
        &conn,
        RunMigrationsOptions {
            to_migration_inclusive: through,
        },
    )
    .unwrap();
    path
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn checksum(path: &Path) -> u64 {
    std::fs::read(path)
        .unwrap()
        .into_iter()
        .fold(0u64, |acc, byte| {
            acc.wrapping_mul(16_777_619).wrapping_add(u64::from(byte))
        })
}

fn column(path: &Path, sql: &str) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[test]
fn snapshot_copies_committed_rows_and_not_an_open_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.sqlite");
    let conn = Connection::open(&src).unwrap();
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        CREATE TABLE notes (v TEXT NOT NULL);
        INSERT INTO notes (v) VALUES ('kept');
        ",
    )
    .unwrap();
    drop(conn);

    let writer = Connection::open(&src).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE;").unwrap();
    writer
        .execute("INSERT INTO notes (v) VALUES ('hidden')", [])
        .unwrap();
    let before = checksum(&src);

    let snapshot = T3Import::snapshot(&src).unwrap();
    let snapshot_path = snapshot.path().canonicalize().unwrap();
    assert!(
        snapshot_path.starts_with("/tmp") || snapshot_path.starts_with("/private/tmp"),
        "{}",
        snapshot_path.display()
    );
    assert_eq!(
        column(snapshot.path(), "SELECT v FROM notes ORDER BY v"),
        vec!["kept".to_owned()]
    );
    assert_eq!(checksum(&src), before);

    writer.execute_batch("COMMIT;").unwrap();
    assert_eq!(
        column(&src, "SELECT v FROM notes ORDER BY v"),
        vec!["hidden".to_owned(), "kept".to_owned()]
    );
}

#[test]
fn a_failed_open_does_not_delete_an_existing_destination() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("statev2.sqlite");
    std::fs::write(&dest, b"already here").unwrap();
    let missing = dir.path().join("missing.sqlite");
    let err = super::snapshot::backup_to(&missing, &dest).unwrap_err();
    assert!(err.to_string().contains("t3Import.snapshot.open"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"already here");
}

#[test]
fn an_unfinished_backup_deletes_the_partial_copy() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("statev2.sqlite");
    std::fs::write(&dest, b"partial").unwrap();
    std::fs::write(dir.path().join("statev2.sqlite-journal"), b"journal").unwrap();
    std::fs::write(dir.path().join("statev2.sqlite-wal"), b"wal").unwrap();
    super::snapshot::discard_partial(&dest);
    assert!(!dest.exists());
    assert!(!dir.path().join("statev2.sqlite-journal").exists());
    assert!(!dir.path().join("statev2.sqlite-wal").exists());
}

#[test]
fn plan_lists_pending_migrations() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", Some(60));
    let plan = T3Import::plan(&path).unwrap();
    assert_eq!(plan.latest_id, 60);
    assert_eq!(
        plan.pending.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![61, 62]
    );
}

#[test]
fn plan_refuses_an_unknown_migration_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", None);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO effect_sql_migrations (migration_id, name) VALUES (63, 'FromTheFuture')",
        [],
    )
    .unwrap();
    drop(conn);
    let err = T3Import::plan(&path).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("63_FromTheFuture"), "{message}");
    assert!(message.contains("does not understand"), "{message}");
}

#[test]
fn plan_refuses_a_renamed_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", None);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE effect_sql_migrations SET name = 'NotTheSame' WHERE migration_id = 62",
        [],
    )
    .unwrap();
    drop(conn);
    let message = T3Import::plan(&path).unwrap_err().to_string();
    assert!(message.contains("62_NotTheSame"), "{message}");
    assert!(message.contains("ThreadFailureLookupIndex"), "{message}");
}

#[test]
fn plan_refuses_a_gap_the_migrator_would_skip() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", None);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "DELETE FROM effect_sql_migrations WHERE migration_id = 55",
        [],
    )
    .unwrap();
    drop(conn);
    let message = T3Import::plan(&path).unwrap_err().to_string();
    assert!(
        message.contains("55_OrchestrationV2 (recorded: nothing)"),
        "{message}"
    );
    assert!(message.contains("highest recorded id (62)"), "{message}");
}

#[test]
fn plan_refuses_a_database_without_a_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.sqlite");
    Connection::open(&path).unwrap();
    let err = T3Import::plan(&path).unwrap_err();
    assert!(matches!(err, T3ImportError::NotV2), "{err}");
}

#[test]
fn migrate_preserves_rows_and_matches_a_fresh_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", Some(61));
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "
        INSERT INTO pull_request_review_guides (host, repository, number, guide_json)
        VALUES ('github', 'example', 1, '{}')
        ",
        [],
    )
    .unwrap();
    drop(conn);

    let snapshot = T3Import::snapshot(&path).unwrap();
    let report = T3Import::migrate_and_verify(&snapshot).unwrap();
    assert_eq!(
        report.applied,
        vec![(62, "ThreadFailureLookupIndex".to_owned())]
    );
    assert_eq!(report.row_counts["pull_request_review_guides"], 1);
    assert_eq!(report.events.decoded, 0);
    assert_eq!(report.events.unknown, 0);
    assert_eq!(
        report.plan.recorded.len() + report.applied.len(),
        migration_manifest().len()
    );
}

#[test]
fn unknown_event_types_are_counted_and_legacy_rows_stay_out_of_the_read_model() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", None);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type,
          occurred_at, actor_kind, payload_json, metadata_json, application_event_version
        ) VALUES
          ('e1', 'thread', 'thread-1', 1, 'thread.created',
           '2026-01-02T03:04:05.006Z', 'server', '{\"threadId\":\"thread-1\"}', '{}', 2),
          ('e2', 'thread', 'thread-1', 2, 'thread.from-the-future',
           '2026-01-02T03:04:05.006Z', 'server', '{\"threadId\":\"thread-1\"}', '{}', 2),
          ('e3', 'thread', 'thread-1', 3, 'thread.activity-appended',
           '2026-01-02T03:04:05.006Z', 'server', '{\"kind\":\"note\"}', '{}', 1);
        ",
    )
    .unwrap();
    drop(conn);

    let snapshot = T3Import::snapshot(&path).unwrap();
    let report = T3Import::migrate_and_verify(&snapshot).unwrap();
    assert_eq!(report.events.decoded, 1);
    assert_eq!(report.events.by_type["thread.created"], 1);
    assert_eq!(report.events.unknown, 1);
    assert_eq!(report.events.unknown_by_type["thread.from-the-future"], 1);
    assert_eq!(report.events.agent_decoded, 1);
    assert_eq!(report.events.agent_unknown, 1);
    assert_eq!(report.events.legacy_thread_rows, 1);
    assert_eq!(report.events.legacy_by_type["thread.activity-appended"], 1);
}

#[test]
fn a_bad_payload_fails_verification() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", None);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type,
          occurred_at, actor_kind, payload_json, metadata_json, application_event_version
        ) VALUES ('bad', 'project', 'p', 1, 'project.created',
                  '2026-01-02T03:04:05.006Z', 'server', 'not-json', '{}', 2)
        ",
        [],
    )
    .unwrap();
    drop(conn);

    let snapshot = T3Import::snapshot(&path).unwrap();
    let message = T3Import::migrate_and_verify(&snapshot)
        .unwrap_err()
        .to_string();
    assert!(message.contains("project.created"), "{message}");
    assert!(message.contains("invalid json or value"), "{message}");
    assert!(!message.contains("not-json"), "{message}");
}

#[test]
fn import_into_links_the_copy_and_refuses_to_replace_it() {
    let dir = tempfile::tempdir().unwrap();
    let src = file_db(dir.path(), "source.sqlite", None);
    let before = checksum(&src);
    let turin = tempfile::tempdir().unwrap();

    let report = T3Import::import_into(&src, turin.path()).unwrap();
    assert!(report.applied.is_empty());
    let dest = turin.path().join(TURIN_DATABASE_FILE);
    assert!(dest.is_file());
    assert_eq!(mode(&dest), 0o600);
    assert_eq!(checksum(&src), before);
    let leftovers: Vec<_> = std::fs::read_dir(turin.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        leftovers,
        vec![std::ffi::OsString::from(TURIN_DATABASE_FILE)]
    );

    let err = T3Import::import_into(&src, turin.path()).unwrap_err();
    assert!(
        matches!(err, T3ImportError::DestinationExists { .. }),
        "{err}"
    );
    assert_eq!(checksum(&src), before);
}

#[test]
fn import_into_leaves_no_destination_when_verification_fails() {
    let dir = tempfile::tempdir().unwrap();
    let src = file_db(dir.path(), "source.sqlite", None);
    let conn = Connection::open(&src).unwrap();
    conn.execute(
        "
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type,
          occurred_at, actor_kind, payload_json, metadata_json, application_event_version
        ) VALUES ('bad', 'project', 'p', 1, 'project.created',
                  '2026-01-02T03:04:05.006Z', 'server', '[]', '{}', 2)
        ",
        [],
    )
    .unwrap();
    drop(conn);
    let turin = tempfile::tempdir().unwrap();
    let err = T3Import::import_into(&src, turin.path()).unwrap_err();
    assert!(matches!(err, T3ImportError::Decode { .. }), "{err}");
    assert!(!turin.path().join(TURIN_DATABASE_FILE).exists());
    let names: Vec<_> = std::fs::read_dir(turin.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.is_empty(), "{names:?}");
}

/// A WAL source larger than one copy step, and a writer that commits as fast
/// as it can until `stop` is set. Every commit makes the copy restart.
fn churning_source(dir: &Path) -> (PathBuf, Arc<AtomicBool>, JoinHandle<()>) {
    let src = dir.join("churning.sqlite");
    let conn = Connection::open(&src).unwrap();
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        CREATE TABLE blobs (id INTEGER PRIMARY KEY, body BLOB NOT NULL);
        CREATE TABLE tick (n INTEGER NOT NULL);
        INSERT INTO tick (n) VALUES (0);
        BEGIN;
        ",
    )
    .unwrap();
    for id in 0..300 {
        conn.execute(
            "INSERT INTO blobs (id, body) VALUES (?1, zeroblob(4000))",
            params![id],
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT;").unwrap();
    drop(conn);

    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = Arc::clone(&stop);
    let writer_src = src.clone();
    let writer = thread::spawn(move || {
        let conn = Connection::open(&writer_src).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute_batch("PRAGMA synchronous = OFF;").unwrap();
        while !writer_stop.load(Ordering::Relaxed) {
            conn.execute("UPDATE tick SET n = n + 1", []).unwrap();
        }
    });
    (src, stop, writer)
}

#[test]
fn a_copy_that_keeps_restarting_gives_up_after_the_restart_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (src, stop, writer) = churning_source(dir.path());
    let dest = dir.path().join("copy.sqlite");
    let limits = CopyLimits {
        step_pages: 1,
        max_restarts: 5,
        deadline: Duration::from_secs(60),
    };
    let result = backup_with_limits(&src, &dest, limits);
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();

    let err = result.unwrap_err();
    assert!(err.to_string().contains("t3Import.snapshot.step"), "{err}");
    assert!(!dest.exists());
}

#[test]
fn a_copy_that_keeps_restarting_gives_up_at_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (src, stop, writer) = churning_source(dir.path());
    let dest = dir.path().join("copy.sqlite");
    let limits = CopyLimits {
        step_pages: 1,
        max_restarts: u32::MAX,
        deadline: Duration::from_millis(300),
    };
    let started = Instant::now();
    let result = backup_with_limits(&src, &dest, limits);
    let elapsed = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();

    let err = result.unwrap_err();
    assert!(err.to_string().contains("t3Import.snapshot.step"), "{err}");
    assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
    assert!(!dest.exists());
}

#[test]
fn plan_never_creates_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.sqlite");
    let err = T3Import::plan(&path).unwrap_err();
    assert!(matches!(err, T3ImportError::Persistence(_)), "{err}");
    assert!(!path.exists());
}

#[test]
fn the_read_only_open_refuses_writes_and_leaves_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_db(dir.path(), "statev2.sqlite", None);
    let before = checksum(&path);
    let conn = super::snapshot::open_read_only(&path).unwrap();
    let err = conn
        .execute("DELETE FROM effect_sql_migrations", [])
        .unwrap_err();
    assert!(err.to_string().contains("readonly"), "{err}");
    drop(conn);
    assert_eq!(checksum(&path), before);
}

#[test]
fn copies_and_their_directories_are_private_from_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let src = file_db(dir.path(), "source.sqlite", None);

    let snapshot = T3Import::snapshot(&src).unwrap();
    assert_eq!(mode(snapshot.path().parent().unwrap()), 0o700);
    assert_eq!(mode(snapshot.path()), 0o600);

    let dest = dir.path().join("direct.sqlite");
    super::snapshot::backup_to(&src, &dest).unwrap();
    assert_eq!(mode(&dest), 0o600);
}

#[test]
fn the_copy_is_never_created_over_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = file_db(dir.path(), "source.sqlite", None);
    let dest = dir.path().join("statev2.sqlite");
    std::fs::write(&dest, b"keep me").unwrap();
    let err = super::snapshot::backup_to(&src, &dest).unwrap_err();
    assert!(err.to_string().contains("create t3 import copy"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
}

fn dir_last_changed_at(path: &Path, when: SystemTime) {
    std::fs::File::open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn recent_import_dirs_are_kept() {
    let parent = tempfile::tempdir().unwrap();
    let work = parent.path().join(".t3-import-1");
    std::fs::create_dir(&work).unwrap();
    remove_stale_dirs(parent.path(), ".t3-import-", SystemTime::now());
    assert!(work.is_dir());
}

#[test]
fn stale_import_dirs_are_removed_and_other_entries_are_kept() {
    let parent = tempfile::tempdir().unwrap();
    let stale = parent.path().join(".t3-import-1");
    std::fs::create_dir(&stale).unwrap();
    let old = SystemTime::now() - STALE_AFTER - Duration::from_secs(60);
    dir_last_changed_at(&stale, old);
    let other_prefix = parent.path().join("unrelated");
    std::fs::create_dir(&other_prefix).unwrap();
    dir_last_changed_at(&other_prefix, old);
    let stale_file = parent.path().join(".t3-import-file");
    std::fs::write(&stale_file, b"not a dir").unwrap();
    dir_last_changed_at(parent.path(), SystemTime::now());

    remove_stale_dirs(parent.path(), ".t3-import-", SystemTime::now());

    assert!(!stale.exists());
    assert!(other_prefix.is_dir());
    assert!(stale_file.is_file());
}

#[test]
fn a_stale_dir_with_a_recent_file_is_kept() {
    let parent = tempfile::tempdir().unwrap();
    let live = parent.path().join(".t3-import-live");
    std::fs::create_dir(&live).unwrap();
    std::fs::write(live.join("statev2.sqlite"), b"copy in progress").unwrap();
    let old = SystemTime::now() - STALE_AFTER - Duration::from_secs(60);
    dir_last_changed_at(&live, old);

    remove_stale_dirs(parent.path(), ".t3-import-", SystemTime::now());

    assert!(live.join("statev2.sqlite").is_file());
}

#[test]
fn import_into_clears_stale_work_dirs_first() {
    let src_dir = tempfile::tempdir().unwrap();
    let src = file_db(src_dir.path(), "source.sqlite", None);
    let turin = tempfile::tempdir().unwrap();
    let stale = turin.path().join(".t3-import-killed");
    std::fs::create_dir(&stale).unwrap();
    dir_last_changed_at(
        &stale,
        SystemTime::now() - STALE_AFTER - Duration::from_secs(60),
    );

    T3Import::import_into(&src, turin.path()).unwrap();

    assert!(!stale.exists());
    assert!(turin.path().join(TURIN_DATABASE_FILE).is_file());
}
