use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::migrations::{RunMigrationsOptions, migration_manifest, run_migrations};

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
    let report = T3Import::migrate_and_verify(snapshot.path()).unwrap();
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

    let report = T3Import::migrate_and_verify(&path).unwrap();
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

    let message = T3Import::migrate_and_verify(&path).unwrap_err().to_string();
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
