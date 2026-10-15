//! Ports of `upstream/apps/server/src/persistence/Sqlite.test.ts`,
//! `upstream/apps/server/src/persistence/initializeV2Database.test.ts`, and the
//! persistence repository tests that do not depend on the Effect runtime.

use std::time::Duration;

use rusqlite::Connection;
use turin_contracts::{CommandId, EventId, ThreadId, UtcDateTime};

use crate::db::{CHECKPOINT_FRAMES, Db, WAL_SIZE_LIMIT_BYTES, WRITER_BACKSTOP_FRAMES};
use crate::errors::PersistenceError;
use crate::initialize_v2_database::initialize_v2_database;
use crate::orchestration_command_receipts::{
    AggregateKind, OrchestrationCommandReceipt, ReceiptStatus, get_by_command_id, insert_if_absent,
    upsert,
};
use crate::orchestration_event_store::{
    AgentEvent, AppendAgentEvents, ApplicationStoredEvent, OrchestrationEventStore,
    ProjectEventInput, ReadAgentEvents, ReadApplicationEvents, StoredAgentEvent,
};
use crate::provider_session_runtime::{
    ProviderSessionRuntime, ProviderSessionStatus, UpsertConflict, get_by_thread_id, list,
    record_imported_transcript, upsert as upsert_runtime,
};
use crate::pull_request_files_viewed::{
    MAX_FILES_VIEWED_ROWS, PullRequestFileViewedWrite, PullRequestFilesViewedScope,
    list as list_files, set as set_files,
};
use crate::pull_request_review_guides::{ThreadPullRequestKey, get as get_guide, put as put_guide};

const NOW: &str = "2026-01-02T03:04:05.006Z";

fn event_id(value: &str) -> EventId {
    EventId::new(value).unwrap()
}

fn thread_id(value: &str) -> ThreadId {
    ThreadId::new(value).unwrap()
}

fn command_id(value: &str) -> CommandId {
    CommandId::new(value).unwrap()
}

fn at(value: &str) -> UtcDateTime {
    value.parse().unwrap()
}

async fn memory_db() -> Db {
    Db::open_in_memory().await.expect("memory db")
}

async fn memory_store() -> OrchestrationEventStore {
    OrchestrationEventStore::new(memory_db().await)
}

fn sql_err(err: rusqlite::Error) -> PersistenceError {
    PersistenceError::sql("test", &err)
}

fn subagent(id: &str, node: &str, prompt: &str, progress: &str) -> AgentEvent {
    AgentEvent {
        id: event_id(id),
        thread_id: thread_id("thread:subagent-prompt"),
        event_type: "subagent.updated".into(),
        occurred_at: at(NOW),
        payload: serde_json::json!({
            "id": node,
            "prompt": prompt,
            "progress": progress,
            "title": "Audit",
        }),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
    }
}

#[tokio::test]
async fn writer_pragmas_match_upstream() {
    let db = memory_db().await;
    let (limit, timeout, fullfsync) = db
        .read(|conn| {
            let limit: i64 = conn
                .query_row("PRAGMA journal_size_limit", [], |row| row.get(0))
                .map_err(sql_err)?;
            let timeout: i64 = conn
                .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .map_err(sql_err)?;
            let fullfsync: i64 = conn
                .query_row("PRAGMA checkpoint_fullfsync", [], |row| row.get(0))
                .map_err(sql_err)?;
            Ok((limit, timeout, fullfsync))
        })
        .await
        .unwrap();
    assert_eq!(limit, WAL_SIZE_LIMIT_BYTES);
    assert_eq!(timeout, 5000);
    assert_eq!(fullfsync, 1);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let file_db = Db::open(&path).await.unwrap();
    let synchronous: i64 = file_db
        .write(|conn| {
            conn.query_row("PRAGMA synchronous", [], |row| row.get(0))
                .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(synchronous, 1);
}

#[tokio::test]
async fn collects_planner_statistics_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    {
        let db = Db::open(&path).await.unwrap();
        db.write(|tx| {
            tx.execute_batch(
                "
                CREATE TABLE stats_probe(id INTEGER PRIMARY KEY, thread_id TEXT NOT NULL);
                CREATE INDEX stats_probe_thread_idx ON stats_probe(thread_id);
                WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000)
                INSERT INTO stats_probe(thread_id) SELECT 'thread-' || (i % 10) FROM n;
                ",
            )
            .map_err(sql_err)?;
            Ok(())
        })
        .await
        .unwrap();
    }
    let db = Db::open(&path).await.unwrap();
    let stat: String = db
        .read(|conn| {
            conn.query_row(
                "SELECT stat FROM sqlite_stat1 WHERE idx = 'stats_probe_thread_idx'",
                [],
                |row| row.get(0),
            )
            .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(stat, "1000 100");
}

#[tokio::test]
async fn waits_out_a_concurrent_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let db = Db::open(&path).await.unwrap();
    db.write(|tx| {
        tx.execute_batch("CREATE TABLE busy_probe(id INTEGER PRIMARY KEY)")
            .map_err(sql_err)?;
        Ok(())
    })
    .await
    .unwrap();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let lock_path = path.clone();
    let holder = std::thread::spawn(move || {
        let conn = Connection::open(&lock_path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        conn.execute_batch("COMMIT").unwrap();
    });
    ready_rx.recv().unwrap();
    db.write(|tx| {
        tx.execute("INSERT INTO busy_probe(id) VALUES (1)", [])
            .map_err(sql_err)?;
        Ok(())
    })
    .await
    .unwrap();
    holder.join().unwrap();

    let id: i64 = db
        .read(|conn| {
            conn.query_row("SELECT id FROM busy_probe", [], |row| row.get(0))
                .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(id, 1);
}

#[tokio::test]
async fn file_reader_sees_committed_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("state.sqlite")).await.unwrap();
    db.write(|tx| {
        tx.execute_batch("CREATE TABLE seen(id INTEGER PRIMARY KEY); INSERT INTO seen VALUES (7);")
            .map_err(sql_err)?;
        Ok(())
    })
    .await
    .unwrap();
    let id: i64 = db
        .read(|conn| {
            conn.query_row("SELECT id FROM seen", [], |row| row.get(0))
                .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(id, 7);
}

#[tokio::test]
async fn v2_import_copies_committed_rows_only() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("state.sqlite");
    let dest = dir.path().join("statev2.sqlite");
    std::fs::write(&source, b"invalid SQLite").unwrap();
    let err = initialize_v2_database(&dest).await.unwrap_err();
    let message = err.to_string();
    assert!(message.contains("Could not copy the V1 database"));
    assert!(message.contains(&source.display().to_string()));
    assert!(!dest.exists());

    std::fs::remove_file(&source).unwrap();
    let conn = Connection::open(&source).unwrap();
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        CREATE TABLE messages(text TEXT);
        INSERT INTO messages VALUES ('committed');
        BEGIN;
        INSERT INTO messages VALUES ('uncommitted');
        ",
    )
    .unwrap();
    initialize_v2_database(&dest).await.unwrap();
    let copy =
        Connection::open_with_flags(&dest, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let rows: Vec<String> = copy
        .prepare("SELECT text FROM messages")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, vec!["committed".to_owned()]);
    conn.execute_batch("ROLLBACK").unwrap();
}

#[tokio::test]
async fn v2_import_does_not_overwrite_an_existing_destination() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("userdata").join("statev2.sqlite");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let db = Db::open(&dest).await.unwrap();
    db.write(|tx| {
        tx.execute_batch(
            "CREATE TABLE v2_work(text TEXT); INSERT INTO v2_work VALUES ('fresh V2 work');",
        )
        .map_err(sql_err)?;
        Ok(())
    })
    .await
    .unwrap();
    drop(db);
    let source = dir.path().join("userdata").join("state.sqlite");
    std::fs::write(&source, b"This source must never be opened once V2 exists").unwrap();
    initialize_v2_database(&dest).await.unwrap();
    let reopened = Db::open(&dest).await.unwrap();
    let text: String = reopened
        .read(|conn| {
            conn.query_row("SELECT text FROM v2_work", [], |row| row.get(0))
                .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(text, "fresh V2 work");
}

#[tokio::test]
async fn missing_v1_database_opens_a_fresh_v2_schema() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("statev2.sqlite");
    let db = Db::open_configured(&dest).await.unwrap();
    let tables: i64 = db
        .read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'orchestration_events'",
                [],
                |row| row.get(0),
            )
            .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(tables, 1);
    assert!(!dir.path().join("state.sqlite").exists());
}

#[tokio::test]
async fn stores_a_subagent_prompt_once_and_replays_it() {
    let store = memory_store().await;
    let prompt = "Audit the repository. ".repeat(200);
    let events = vec![
        subagent("e0", "node", &prompt, "Reading files"),
        subagent("e1", "node", &prompt, "Running tests"),
        subagent("e2", "node", &prompt, "Writing report"),
        subagent("e3", "node", "Audit again.", "Retrying"),
    ];
    store
        .append_agent_events(AppendAgentEvents {
            command_id: None,
            events: events.clone(),
        })
        .await
        .unwrap();
    let includes: Vec<i64> = store
        .db()
        .read({
            let prompt = prompt.clone();
            move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT instr(payload_json, ?1) > 0 FROM orchestration_events ORDER BY sequence",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([&prompt], |row| row.get(0))
                .map_err(sql_err)?
                .collect::<Result<Vec<i64>, _>>()
                .map_err(sql_err)?;
            Ok(rows)
        }
        })
        .await
        .unwrap();
    assert_eq!(includes, vec![1, 0, 0, 0]);

    let replayed = store
        .read_agent_events(ReadAgentEvents {
            after_sequence: Some(0),
            through_sequence: None,
            thread_id: Some(thread_id("thread:subagent-prompt")),
            command_id: None,
            event_type: None,
            limit: None,
            skip_unknown_event_types: false,
        })
        .await
        .unwrap();
    assert_eq!(
        replayed
            .iter()
            .map(|event| event.event.payload.clone())
            .collect::<Vec<_>>(),
        events
            .into_iter()
            .map(|event| event.payload)
            .collect::<Vec<_>>()
    );

    let through = store.latest_application_sequence().await.unwrap();
    let stats = store
        .get_agent_replay_stats("thread:subagent-prompt".into(), 0, through, 4)
        .await
        .unwrap();
    assert!(stats.raw_payload_bytes > prompt.len() as i64 * 3);
}

#[tokio::test]
async fn stores_the_full_prompt_when_the_hint_row_rolled_back() {
    let store = memory_store().await;
    let mut event = subagent(
        "event:subagent-rollback",
        "node:subagent-rollback",
        &"Audit the repository. ".repeat(20),
        "Reading files",
    );
    event.thread_id = thread_id("thread:subagent-rollback");
    let store_for_rollback = store.clone();
    let rolled = event.clone();
    let err: Result<(), PersistenceError> = store
        .db()
        .write(move |tx| {
            store_for_rollback.append_agent_events_in(
                tx,
                &AppendAgentEvents {
                    command_id: None,
                    events: vec![rolled],
                },
            )?;
            Err(PersistenceError::Closed)
        })
        .await;
    assert!(matches!(err, Err(PersistenceError::Closed)));

    let mut second = event.clone();
    second.id = event_id("event:subagent-after-rollback");
    let appended = store
        .append_agent_events(AppendAgentEvents {
            command_id: None,
            events: vec![event.clone(), second],
        })
        .await
        .unwrap();
    let prompts: Vec<(Option<String>, Option<i64>)> = store
        .db()
        .read(|conn| {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT json_extract(payload_json, '$.prompt'),
                           json_extract(payload_json, '$.promptSequence')
                    FROM orchestration_events
                    WHERE stream_id = 'thread:subagent-rollback'
                    ORDER BY sequence
                    ",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(
        prompts,
        vec![
            (
                Some(event.payload["prompt"].as_str().unwrap().to_owned()),
                None
            ),
            (Some(String::new()), Some(appended[0].sequence)),
        ]
    );
}

#[tokio::test]
async fn stores_json_columns_as_text_and_rejects_invalid_json() {
    let store = memory_store().await;
    let appended = store
        .append_project_event(ProjectEventInput {
            event_id: event_id("evt-store-roundtrip"),
            aggregate_id: "project-roundtrip".into(),
            event_type: "project.created".into(),
            occurred_at: NOW.into(),
            command_id: Some(command_id("cmd-store-roundtrip")),
            causation_event_id: None,
            correlation_id: Some(command_id("cmd-store-roundtrip")),
            payload: serde_json::json!({"title": "Roundtrip Project"}),
            metadata: serde_json::json!({"adapterKey": "codex", "origin": {"surface": "cli"}}),
        })
        .await
        .unwrap();
    let event_id = appended.event_id.clone();
    let kinds: (String, String) = store
        .db()
        .read(move |conn| {
            conn.query_row(
                "SELECT typeof(payload_json), typeof(metadata_json) FROM orchestration_events WHERE event_id = ?1",
                [event_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(kinds, ("text".into(), "text".into()));

    let replayed = store
        .read_application_events(ReadApplicationEvents {
            after_sequence: appended.sequence - 1,
            through_sequence: appended.sequence,
            skip_unknown_event_types: false,
        })
        .await
        .unwrap();
    let ApplicationStoredEvent::Project(project) = &replayed[0] else {
        panic!("expected a project event");
    };
    assert_eq!(project.metadata["adapterKey"], "codex");

    store
        .db()
        .write(|tx| {
            tx.execute(
                "
                INSERT INTO orchestration_events (
                  event_id, aggregate_kind, stream_id, stream_version, event_type,
                  occurred_at, actor_kind, payload_json, metadata_json, application_event_version
                ) VALUES ('bad', 'project', 'p', 1, 'project.created', ?1, 'server', 'not-json', '{}', 2)
                ",
                [NOW],
            )
            .map_err(sql_err)?;
            Ok(())
        })
        .await
        .unwrap();
    let err = store
        .read_application_events(ReadApplicationEvents {
            after_sequence: 0,
            through_sequence: 9_007_199_254_740_991,
            skip_unknown_event_types: false,
        })
        .await
        .unwrap_err();
    assert!(err.is_decode());
}

#[tokio::test]
async fn high_water_ignores_legacy_rows_and_pages_application_events() {
    let store = memory_store().await;
    assert_eq!(store.latest_application_sequence().await.unwrap(), 0);
    assert_eq!(store.latest_agent_sequence(None).await.unwrap(), 0);

    store
        .db()
        .write(|tx| {
            tx.execute_batch(
                "
                INSERT INTO orchestration_events (
                  event_id, aggregate_kind, stream_id, stream_version, event_type,
                  occurred_at, actor_kind, payload_json, metadata_json, application_event_version
                ) VALUES
                  ('legacy', 'thread', 'legacy-only', 1, 'provider-session.detached',
                   '2026-09-03T00:00:00.000Z', 'server',
                   '{\"providerSessionId\":\"s\",\"detachedAt\":\"2026-09-03T00:00:00.000Z\"}', '{}', 1);
                ",
            )
            .map_err(sql_err)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(store.latest_application_sequence().await.unwrap(), 0);
    assert_eq!(
        store
            .latest_agent_sequence(Some(thread_id("legacy-only")))
            .await
            .unwrap(),
        0
    );

    store
        .db()
        .write(|tx| {
            tx.execute_batch(
                "
                WITH RECURSIVE history(n) AS (
                  SELECT 0 UNION ALL SELECT n + 1 FROM history WHERE n < 1559
                )
                INSERT INTO orchestration_events (
                  event_id, aggregate_kind, stream_id, stream_version, event_type,
                  occurred_at, command_id, actor_kind, payload_json, metadata_json,
                  application_event_version
                )
                SELECT 'event:' || n,
                  CASE WHEN n % 3 = 1 THEN 'project' ELSE 'thread' END,
                  CASE
                    WHEN n % 3 = 1 THEN 'project:' || n
                    WHEN n % 2 = 0 THEN 'target'
                    ELSE 'other'
                  END,
                  n,
                  CASE WHEN n % 3 = 1 THEN 'project.created' ELSE 'provider-session.detached' END,
                  '2026-09-03T00:00:00.000Z',
                  CASE WHEN n % 5 = 0 THEN 'selected-command' ELSE 'other-command' END,
                  'server',
                  CASE WHEN n % 3 = 1
                    THEN '{\"title\":\"Sequence fixture\"}'
                    ELSE '{\"providerSessionId\":\"session\",\"detachedAt\":\"2026-09-03T00:00:00.000Z\"}'
                  END,
                  '{}',
                  CASE WHEN n % 3 = 0 THEN 1 ELSE 2 END
                FROM history;
                ",
            )
            .map_err(sql_err)?;
            Ok(())
        })
        .await
        .unwrap();

    let application = store.latest_application_sequence().await.unwrap();
    assert!(application > 0);
    let page = store
        .read_application_events(ReadApplicationEvents {
            after_sequence: 0,
            through_sequence: application,
            skip_unknown_event_types: false,
        })
        .await
        .unwrap();
    assert!(page.len() > 500);
    assert!(
        page.windows(2)
            .all(|pair| pair[0].sequence() < pair[1].sequence())
    );

    let scoped = store
        .read_agent_events(ReadAgentEvents {
            after_sequence: Some(0),
            through_sequence: Some(application),
            thread_id: Some(thread_id("target")),
            command_id: Some(command_id("selected-command")),
            event_type: None,
            limit: Some(17),
            skip_unknown_event_types: false,
        })
        .await
        .unwrap();
    assert_eq!(scoped.len(), 17);
    assert!(
        scoped
            .iter()
            .all(|event| event.command_id.as_ref().map(CommandId::as_str)
                == Some("selected-command"))
    );
}

#[tokio::test]
async fn high_water_and_command_reads_use_their_indexes() {
    let store = memory_store().await;
    store
        .db()
        .write(|tx| {
            tx.execute_batch(
                "
                WITH RECURSIVE history(n) AS (
                  SELECT 1 UNION ALL SELECT n + 1 FROM history WHERE n < 25000
                )
                INSERT INTO orchestration_events (
                  event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at,
                  command_id, actor_kind, payload_json, metadata_json, application_event_version
                )
                SELECT 'history:' || n, 'thread',
                  CASE WHEN n = 1 THEN 'target' ELSE 'other' END,
                  n, 'provider-session.detached', '2026-09-03T00:00:00.000Z',
                  CASE WHEN n IN (12, 24990) THEN 'retried-command' ELSE 'command:' || n END,
                  'server',
                  '{\"providerSessionId\":\"session\",\"detachedAt\":\"2026-09-03T00:00:00.000Z\"}',
                  '{}',
                  2
                FROM history;
                ",
            )
            .map_err(sql_err)?;
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(store.latest_application_sequence().await.unwrap(), 25_000);
    assert_eq!(
        store
            .latest_agent_sequence(Some(thread_id("target")))
            .await
            .unwrap(),
        1
    );
    let replayed = store
        .read_agent_events(ReadAgentEvents {
            after_sequence: None,
            through_sequence: None,
            thread_id: None,
            command_id: Some(command_id("retried-command")),
            event_type: None,
            limit: None,
            skip_unknown_event_types: false,
        })
        .await
        .unwrap();
    assert_eq!(
        replayed
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![12, 24_990]
    );

    let plan = store
        .db()
        .read(|conn| {
            let mut stmt = conn
                .prepare(
                    "
                    EXPLAIN QUERY PLAN
                    SELECT MAX(sequence) AS sequence
                    FROM orchestration_events INDEXED BY idx_orchestration_events_application_high_water
                    WHERE aggregate_kind = 'project'
                      OR (application_event_version = 2 AND aggregate_kind = 'thread')
                    ",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(3))
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            Ok(rows.join("\n"))
        })
        .await
        .unwrap();
    assert!(
        plan.contains("idx_orchestration_events_application_high_water"),
        "{plan}"
    );

    let command_plan = store
        .db()
        .read(|conn| {
            let mut stmt = conn
                .prepare(
                    "
                    EXPLAIN QUERY PLAN
                    SELECT sequence FROM orchestration_events INDEXED BY idx_orch_events_command_id
                    WHERE sequence > 0 AND sequence <= 9007199254740991
                      AND command_id = 'retried-command'
                    LIMIT 500
                    ",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(3))
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            Ok(rows.join("\n"))
        })
        .await
        .unwrap();
    assert!(
        command_plan.contains("idx_orch_events_command_id"),
        "{command_plan}"
    );
}

#[tokio::test]
async fn a_panicking_write_does_not_poison_the_connection() {
    let db = memory_db().await;
    let err = db
        .write(|_| -> Result<(), PersistenceError> { panic!("boom") })
        .await
        .unwrap_err();
    match err {
        PersistenceError::Panicked { message } => assert!(message.contains("boom"), "{message}"),
        other => panic!("expected a panic error, got {other}"),
    }
    db.write(|_| Ok(())).await.unwrap();
    db.read(|_| Ok(())).await.unwrap();
}

#[tokio::test]
async fn releases_consumed_application_replay_pages() {
    let store = memory_store().await;
    store
        .db()
        .write(|tx| {
            for index in 0..1501 {
                crate::orchestration_event_store::append_project_event(
                    tx,
                    &ProjectEventInput {
                        event_id: event_id(&format!("retention-{index}")),
                        aggregate_id: format!("retention-{index}"),
                        event_type: "project.created".into(),
                        occurred_at: NOW.into(),
                        command_id: None,
                        causation_event_id: None,
                        correlation_id: None,
                        payload: serde_json::json!({
                            "projectId": format!("retention-{index}"),
                            "title": "Replay",
                        }),
                        metadata: serde_json::json!({}),
                    },
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut subscription = store.stream_application_events(0);
    let mut count = 0;
    while count < 1501 {
        let event = tokio::time::timeout(Duration::from_secs(5), subscription.rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(event.sequence() > 0);
        count += 1;
    }
    assert_eq!(count, 1501);
    let (max_live, max_len, fetches) = subscription.replay_page_stats();
    assert!(max_live <= 1, "retained {max_live} replay pages");
    assert!(max_len <= 500, "page held {max_len} events");
    assert!(fetches >= 4, "fetched {fetches} pages");
}

#[tokio::test]
async fn live_buffer_counts_the_full_agent_event() {
    let store = memory_store().await;
    let mut subscription = store.stream_application_events(0);
    let body = "x".repeat(1024 * 1024);
    let events = (0..9)
        .map(|index| {
            ApplicationStoredEvent::Agent(StoredAgentEvent {
                sequence: index + 1,
                command_id: None,
                event: AgentEvent {
                    id: event_id(&format!("e{index}")),
                    thread_id: thread_id("t"),
                    event_type: "message.updated".into(),
                    occurred_at: at(NOW),
                    payload: serde_json::json!({ "body": body }),
                    run_id: None,
                    node_id: None,
                    driver: None,
                    provider_instance_id: None,
                    raw_event_id: None,
                },
            })
        })
        .collect();
    store.publish_committed(events);
    let mut saw_error = false;
    for _ in 0..10 {
        let next = tokio::time::timeout(Duration::from_secs(5), subscription.rx.recv())
            .await
            .unwrap()
            .unwrap();
        match next {
            Ok(_) => continue,
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("streamApplicationEvents:buffer"),
                    "{message}"
                );
                assert!(!message.contains("SQLITE"), "{message}");
                saw_error = true;
                break;
            }
        }
    }
    assert!(saw_error);
}

#[tokio::test]
async fn dropping_a_subscription_removes_it_from_the_live_hub() {
    let store = memory_store().await;
    let subscription = store.stream_application_events(0);
    assert_eq!(store.live_subscriber_count(), 1);
    drop(subscription);
    assert_eq!(store.live_subscriber_count(), 0);
}

#[tokio::test]
async fn live_stream_replays_then_delivers_a_committed_publish() {
    let store = memory_store().await;
    store
        .append_project_event(ProjectEventInput {
            event_id: event_id("live-seed"),
            aggregate_id: "project".into(),
            event_type: "project.created".into(),
            occurred_at: NOW.into(),
            command_id: None,
            causation_event_id: None,
            correlation_id: None,
            payload: serde_json::json!({"title": "Live"}),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let mut subscription = store.stream_application_events(0);
    let first = tokio::time::timeout(Duration::from_secs(5), subscription.rx.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.sequence(), 1);

    let stored = store
        .append_agent_events(AppendAgentEvents {
            command_id: None,
            events: vec![AgentEvent {
                id: event_id("live-after"),
                thread_id: thread_id("target"),
                event_type: "provider-session.detached".into(),
                occurred_at: at(NOW),
                payload: serde_json::json!({
                    "providerSessionId": "live-session",
                    "detachedAt": NOW,
                }),
                run_id: None,
                node_id: None,
                driver: None,
                provider_instance_id: None,
                raw_event_id: None,
            }],
        })
        .await
        .unwrap();
    store.publish_committed(
        stored
            .iter()
            .cloned()
            .map(ApplicationStoredEvent::Agent)
            .collect(),
    );
    let live = tokio::time::timeout(Duration::from_secs(5), subscription.rx.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(live.sequence(), stored[0].sequence);
}

#[tokio::test]
async fn command_receipts_insert_once_and_upsert() {
    let db = memory_db().await;
    let receipt = OrchestrationCommandReceipt {
        command_id: command_id("cmd"),
        aggregate_kind: AggregateKind::Thread,
        aggregate_id: "thread".into(),
        command_type: "turn.start".into(),
        accepted_at: NOW.into(),
        result_sequence: 4,
        status: ReceiptStatus::Accepted,
        error: None,
    };
    db.write({
        let receipt = receipt.clone();
        move |tx| {
            assert!(insert_if_absent(tx, &receipt)?);
            assert!(!insert_if_absent(tx, &receipt)?);
            Ok(())
        }
    })
    .await
    .unwrap();
    db.write({
        let mut rejected = receipt.clone();
        rejected.status = ReceiptStatus::Rejected;
        rejected.error = Some("no".into());
        move |tx| {
            upsert(tx, &rejected)?;
            let loaded = get_by_command_id(tx, &command_id("cmd"))?.unwrap();
            assert_eq!(loaded.status, ReceiptStatus::Rejected);
            assert_eq!(loaded.error.as_deref(), Some("no"));
            Ok(())
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn provider_runtime_keeps_imported_transcripts_and_skips_bad_rows() {
    let db = memory_db().await;
    db.write(|tx| {
        let runtime = ProviderSessionRuntime {
            thread_id: thread_id("thread-1"),
            provider_name: "codex".into(),
            provider_instance_id: Some("codex".into()),
            adapter_key: "codex".into(),
            runtime_mode: "full-access".into(),
            status: ProviderSessionStatus::Running,
            last_seen_at: NOW.into(),
            resume_cursor: None,
            runtime_payload: Some(serde_json::json!({"cwd": "/tmp"})),
        };
        upsert_runtime(tx, &runtime, UpsertConflict::Update)?;
        record_imported_transcript(
            tx,
            &thread_id("thread-1"),
            &serde_json::json!({"providerInstanceId": "codex", "filePath": "/tmp/a.jsonl"}),
        )?;
        let mut next = runtime.clone();
        next.runtime_payload = Some(serde_json::json!({"cwd": "/work"}));
        next.last_seen_at = "2026-01-02T03:04:06.000Z".into();
        upsert_runtime(tx, &next, UpsertConflict::Update)?;
        let loaded = get_by_thread_id(tx, &thread_id("thread-1"))?.unwrap();
        assert_eq!(loaded.runtime_payload.as_ref().unwrap()["cwd"], "/work");
        assert_eq!(
            loaded.runtime_payload.as_ref().unwrap()["importedTranscripts"][0]["filePath"],
            "/tmp/a.jsonl"
        );

        tx.execute(
            "
            INSERT INTO provider_session_runtime (
              thread_id, provider_name, adapter_key, runtime_mode, status, last_seen_at
            ) VALUES ('bad', 'codex', 'codex', 'full-access', 'nope', ?1)
            ",
            [NOW],
        )
        .map_err(sql_err)?;
        let listed = list(tx, false)?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].thread_id.as_str(), "thread-1");

        tx.execute(
            "
            INSERT INTO provider_session_runtime (
              thread_id, provider_name, adapter_key, runtime_mode, status, last_seen_at,
              provider_instance_id
            ) VALUES
              ('thread-yolo', 'codex', 'codex', 'yolo', 'running', ?1, 'codex'),
              ('thread-slug', 'codex', 'codex', 'full-access', 'running', ?1, '1bad'),
              ('thread-stamp', 'codex', 'codex', 'full-access', 'running', X'01', 'codex')
            ",
            [NOW],
        )
        .map_err(sql_err)?;
        let listed = list(tx, false)?;
        assert_eq!(listed.len(), 1);
        assert!(get_by_thread_id(tx, &thread_id("thread-yolo")).is_err());
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn files_viewed_and_review_guides_round_trip() {
    let db = memory_db().await;
    db.write(|tx| {
        let scope = PullRequestFilesViewedScope {
            provider: "github".into(),
            host: "github.com".into(),
            repository: "org/repo".into(),
            number: 12,
            viewer: "me".into(),
        };
        let files: Vec<_> = (0..=MAX_FILES_VIEWED_ROWS)
            .map(|index| PullRequestFileViewedWrite {
                path: format!("f{index}"),
                revision: Some("abc".into()),
                viewed: true,
            })
            .collect();
        set_files(tx, &scope, &files, NOW)?;
        let page = list_files(tx, &scope)?;
        assert!(page.truncated);
        assert_eq!(page.files.len() as i64, MAX_FILES_VIEWED_ROWS);

        let key = ThreadPullRequestKey {
            host: "github.com".into(),
            repository: "org/repo".into(),
            number: 12,
        };
        put_guide(tx, &key, &serde_json::json!({"summary": "look here"}))?;
        assert_eq!(get_guide(tx, &key)?.unwrap()["summary"], "look here");
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn checkpoint_worker_starts_on_a_file_database() {
    let memory = memory_db().await;
    assert_eq!(
        memory.writer_autocheckpoint().await.unwrap(),
        CHECKPOINT_FRAMES
    );

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("turin.sqlite")).await.unwrap();
    let version: String = db
        .read(|conn| {
            conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))
                .map_err(|err| PersistenceError::sql("sqlite.version", &err))
        })
        .await
        .unwrap();
    let mut parts = version.split('.');
    let major: u32 = parts.next().unwrap().parse().unwrap();
    let minor: u32 = parts.next().unwrap().parse().unwrap();
    assert!(major > 3 || (major == 3 && minor >= 51), "{version}");
    assert_eq!(
        db.writer_autocheckpoint().await.unwrap(),
        WRITER_BACKSTOP_FRAMES
    );
}

#[tokio::test]
async fn unknown_event_types_are_rejected_before_insert() {
    let store = memory_store().await;
    let err = store
        .append_project_event(ProjectEventInput {
            event_id: event_id("evt-unknown"),
            aggregate_id: "project-unknown".into(),
            event_type: "project.nope".into(),
            occurred_at: NOW.into(),
            command_id: None,
            causation_event_id: None,
            correlation_id: None,
            payload: serde_json::json!({"title": "Nope"}),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("appendProjectEvent:decode"), "{message}");
    assert!(message.contains("unknown event type"));
    let count: i64 = store
        .db()
        .read(|conn| {
            conn.query_row("SELECT COUNT(*) FROM orchestration_events", [], |row| {
                row.get(0)
            })
            .map_err(sql_err)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);

    let err = store
        .append_agent_events(AppendAgentEvents {
            command_id: None,
            events: vec![AgentEvent {
                id: event_id("evt-agent-unknown"),
                thread_id: thread_id("thread-unknown"),
                event_type: "thread.nope".into(),
                occurred_at: at(NOW),
                payload: serde_json::json!({"title": "Nope"}),
                run_id: None,
                node_id: None,
                driver: None,
                provider_instance_id: None,
                raw_event_id: None,
            }],
        })
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("appendAgentEvents:decode"), "{message}");
}

#[tokio::test]
async fn agent_replay_stats_accepts_a_huge_event_cap() {
    let store = memory_store().await;
    let stats = store
        .get_agent_replay_stats("thread".into(), 0, 1, u64::MAX)
        .await
        .unwrap();
    assert_eq!(stats.event_count, 0);
}
