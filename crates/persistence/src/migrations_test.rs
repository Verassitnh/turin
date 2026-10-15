use rusqlite::Connection;

use super::*;

fn memory() -> Connection {
    Connection::open_in_memory().unwrap()
}

fn history(conn: &Connection) -> Vec<(i64, String)> {
    let mut stmt = conn
        .prepare("SELECT migration_id, name FROM effect_sql_migrations ORDER BY migration_id")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn v2_tables(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name LIKE 'orchestration_v2_%'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn migrates_a_fresh_database_through_the_whole_manifest() {
    let conn = memory();
    run_migrations(&conn, RunMigrationsOptions::default()).unwrap();
    let manifest: Vec<(i64, String)> = migration_manifest()
        .into_iter()
        .map(|(id, name)| (id, name.to_owned()))
        .collect();
    assert_eq!(history(&conn), manifest);
    assert!(v2_tables(&conn) > 0);
}

#[test]
fn refuses_when_another_migration_took_a_v2_id() {
    let conn = memory();
    run_migrations(
        &conn,
        RunMigrationsOptions {
            to_migration_inclusive: Some(54),
        },
    )
    .unwrap();
    conn.execute(
        "INSERT INTO effect_sql_migrations (migration_id, name) VALUES (55, 'FutureMainMigration')",
        [],
    )
    .unwrap();
    let before = history(&conn);
    assert!(
        run_migrations(
            &conn,
            RunMigrationsOptions {
                to_migration_inclusive: Some(54),
            },
        )
        .unwrap()
        .is_empty()
    );
    let err = run_migrations(&conn, RunMigrationsOptions::default()).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("55_OrchestrationV2 (recorded: FutureMainMigration)"));
    assert_eq!(history(&conn), before);
    assert_eq!(v2_tables(&conn), 0);
}

#[test]
fn refuses_when_a_later_id_would_skip_v2_migrations() {
    let conn = memory();
    let manifest = migration_manifest();
    let newer = manifest.iter().map(|(id, _)| *id).max().unwrap() + 1;
    let v2: Vec<_> = manifest.iter().filter(|(id, _)| *id >= 55).collect();
    run_migrations(
        &conn,
        RunMigrationsOptions {
            to_migration_inclusive: Some(54),
        },
    )
    .unwrap();
    conn.execute(
        "INSERT INTO effect_sql_migrations (migration_id, name) VALUES (?1, 'FutureMainMigration')",
        [newer],
    )
    .unwrap();
    let before = history(&conn);
    let err = run_migrations(&conn, RunMigrationsOptions::default()).unwrap_err();
    let message = err.to_string();
    for (id, name) in v2 {
        assert!(message.contains(&format!("{id}_{name} (recorded: nothing)")));
    }
    assert_eq!(history(&conn), before);
    assert_eq!(v2_tables(&conn), 0);
}

#[test]
fn upgrades_a_database_at_exactly_54() {
    let conn = memory();
    run_migrations(
        &conn,
        RunMigrationsOptions {
            to_migration_inclusive: Some(54),
        },
    )
    .unwrap();
    let ran = run_migrations(&conn, RunMigrationsOptions::default()).unwrap();
    let v2: Vec<_> = migration_manifest()
        .into_iter()
        .filter(|(id, _)| *id >= 55)
        .collect();
    assert_eq!(ran, v2);
    let manifest: Vec<(i64, String)> = migration_manifest()
        .into_iter()
        .map(|(id, name)| (id, name.to_owned()))
        .collect();
    assert_eq!(history(&conn), manifest);
    assert!(v2_tables(&conn) > 0);
}

#[test]
fn still_starts_with_a_site_local_migration_before_v2() {
    for id in [30_i64, 54] {
        let conn = memory();
        run_migrations(
            &conn,
            RunMigrationsOptions {
                to_migration_inclusive: Some(54),
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE effect_sql_migrations SET name = 'SiteLocalMigration' WHERE migration_id = ?1",
            [id],
        )
        .unwrap();
        let ran = run_migrations(&conn, RunMigrationsOptions::default()).unwrap();
        let v2: Vec<_> = migration_manifest()
            .into_iter()
            .filter(|(mid, _)| *mid >= 55)
            .collect();
        assert_eq!(ran, v2);
        assert!(v2_tables(&conn) > 0);
    }
}

#[test]
fn only_checks_v2_migrations_within_the_requested_limit() {
    let conn = memory();
    let newer = migration_manifest()
        .iter()
        .map(|(id, _)| *id)
        .max()
        .unwrap()
        + 1;
    run_migrations(
        &conn,
        RunMigrationsOptions {
            to_migration_inclusive: Some(55),
        },
    )
    .unwrap();
    conn.execute(
        "INSERT INTO effect_sql_migrations (migration_id, name) VALUES (56, 'SiteLocalMigration'), (?1, 'NewerV2Migration')",
        [newer],
    )
    .unwrap();
    let before = history(&conn);
    assert!(
        run_migrations(
            &conn,
            RunMigrationsOptions {
                to_migration_inclusive: Some(55),
            },
        )
        .unwrap()
        .is_empty()
    );
    let err = run_migrations(
        &conn,
        RunMigrationsOptions {
            to_migration_inclusive: Some(56),
        },
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("56_RemoveRedundantProjectionIndexes (recorded: SiteLocalMigration)"));
    assert!(!message.contains("57_ScheduledTaskWebhooks"));
    assert_eq!(history(&conn), before);
}

#[test]
fn still_starts_after_a_newer_build_recorded_a_later_migration() {
    let conn = memory();
    let newer = migration_manifest()
        .iter()
        .map(|(id, _)| *id)
        .max()
        .unwrap()
        + 1;
    run_migrations(&conn, RunMigrationsOptions::default()).unwrap();
    conn.execute(
        "INSERT INTO effect_sql_migrations (migration_id, name) VALUES (?1, 'NewerV2Migration')",
        [newer],
    )
    .unwrap();
    assert!(
        run_migrations(&conn, RunMigrationsOptions::default())
            .unwrap()
            .is_empty()
    );
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |row| row.get(1))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    names.iter().any(|name| name == column)
}

fn column_notnull(conn: &Connection, table: &str, column: &str) -> Option<i64> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |row| Ok((row.get(1)?, row.get(3)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    rows.into_iter()
        .find(|(name, _)| name == column)
        .map(|(_, notnull)| notnull)
}

fn has_index(conn: &Connection, table: &str, index: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_list({table})"))
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |row| row.get(1))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    names.iter().any(|name| name == index)
}

fn migrate_to(conn: &Connection, id: i64) {
    run_migrations(
        conn,
        RunMigrationsOptions {
            to_migration_inclusive: Some(id),
        },
    )
    .unwrap();
}

/// Port of `027_028_ProviderInstanceIdColumns.test.ts`.
#[test]
fn continues_when_provider_session_runtime_was_partially_migrated() {
    let conn = memory();
    migrate_to(&conn, 26);
    conn.execute(
        "ALTER TABLE provider_session_runtime ADD COLUMN provider_instance_id TEXT",
        [],
    )
    .unwrap();
    migrate_to(&conn, 28);
    assert_eq!(
        history(&conn)
            .into_iter()
            .filter(|(id, _)| *id == 27 || *id == 28)
            .collect::<Vec<_>>(),
        vec![
            (27, "ProviderSessionRuntimeInstanceId".to_owned()),
            (28, "ProjectionThreadSessionInstanceId".to_owned()),
        ]
    );
    assert!(has_column(
        &conn,
        "provider_session_runtime",
        "provider_instance_id"
    ));
    assert!(has_column(
        &conn,
        "projection_thread_sessions",
        "provider_instance_id"
    ));
    assert!(has_index(
        &conn,
        "provider_session_runtime",
        "idx_provider_session_runtime_instance"
    ));
    assert!(has_index(
        &conn,
        "projection_thread_sessions",
        "idx_projection_thread_sessions_instance"
    ));
}

/// Port of `049_ProjectionThreadsActiveOrderKey.test.ts`.
#[test]
fn active_order_key_keeps_existing_timestamps_and_survives_a_rerun() {
    let conn = memory();
    migrate_to(&conn, 48);
    let now = "2026-01-01T00:00:00.000Z";
    conn.execute(
        "
        INSERT INTO projection_threads (
          thread_id, project_id, title, model_selection_json, runtime_mode,
          created_at, updated_at
        ) VALUES (
          'thread-1', 'project-1', 'Existing thread',
          '{\"instanceId\":\"codex\",\"model\":\"gpt-5.4\"}', 'full-access', ?1, ?1
        )
        ",
        [now],
    )
    .unwrap();
    migrate_to(&conn, 49);
    let key: Option<String> = conn
        .query_row(
            "SELECT active_order_key FROM projection_threads WHERE thread_id = 'thread-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(key, None);
    conn.execute(
        "UPDATE projection_threads SET active_order_key = 'gm' WHERE thread_id = 'thread-1'",
        [],
    )
    .unwrap();
    crate::migrations::rerun_migration_body(&conn, 49).unwrap();
    let row: (Option<String>, String, String) = conn
        .query_row(
            "
            SELECT active_order_key, created_at, updated_at
            FROM projection_threads WHERE thread_id = 'thread-1'
            ",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, (Some("gm".to_owned()), now.to_owned(), now.to_owned()));
}

/// Port of `051_ProjectionThreadMessageContext.test.ts`.
#[test]
fn accepts_context_added_by_an_earlier_development_migration() {
    let conn = memory();
    migrate_to(&conn, 50);
    conn.execute(
        "ALTER TABLE projection_thread_messages ADD COLUMN context_json TEXT",
        [],
    )
    .unwrap();
    migrate_to(&conn, 51);
    assert_eq!(
        column_notnull(&conn, "projection_thread_messages", "context_json"),
        Some(0)
    );
    let recorded: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM effect_sql_migrations WHERE migration_id = 51",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 1);
}

/// Port of `054_ProjectionThreadsAutoSettleDisabledAt.test.ts`.
#[test]
fn auto_settle_disabled_at_stays_null_and_survives_a_rerun() {
    let conn = memory();
    migrate_to(&conn, 53);
    let now = "2026-01-01T00:00:00.000Z";
    conn.execute(
        "
        INSERT INTO projection_threads (
          thread_id, project_id, title, model_selection_json, runtime_mode,
          created_at, updated_at
        ) VALUES (
          'thread-1', 'project-1', 'Existing thread',
          '{\"instanceId\":\"codex\",\"model\":\"gpt-5.4\"}', 'full-access', ?1, ?1
        )
        ",
        [now],
    )
    .unwrap();
    migrate_to(&conn, 54);
    let value: Option<String> = conn
        .query_row(
            "SELECT auto_settle_disabled_at FROM projection_threads WHERE thread_id = 'thread-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, None);
    conn.execute(
        "UPDATE projection_threads SET auto_settle_disabled_at = ?1 WHERE thread_id = 'thread-1'",
        [now],
    )
    .unwrap();
    crate::migrations::rerun_migration_body(&conn, 54).unwrap();
    let kept: Option<String> = conn
        .query_row(
            "SELECT auto_settle_disabled_at FROM projection_threads WHERE thread_id = 'thread-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept.as_deref(), Some(now));
}

/// Migration 023 swallows a duplicate-column error on each ALTER.
#[test]
fn shell_summary_columns_already_present_do_not_fail_migration_23() {
    let conn = memory();
    migrate_to(&conn, 22);
    conn.execute(
        "ALTER TABLE projection_threads ADD COLUMN latest_user_message_at TEXT",
        [],
    )
    .unwrap();
    migrate_to(&conn, 23);
    assert!(has_column(
        &conn,
        "projection_threads",
        "pending_approval_count"
    ));
    assert!(history(&conn).iter().any(|(id, _)| *id == 23));
}
