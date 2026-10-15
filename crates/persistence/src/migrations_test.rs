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
