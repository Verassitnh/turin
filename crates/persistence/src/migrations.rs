//! Port of upstream/apps/server/src/persistence/Migrations.ts
//!
//! Bookkeeping table and id rules match Effect's `Migrator.make` for SQLite:
//! pending ids are inserted, then executed, in one transaction. An id at or
//! below the highest recorded id is skipped even when the recorded name differs.

use rusqlite::{Connection, OptionalExtension, params};

use crate::application_event_source::copy_application_event_source;
use crate::errors::{MigrationErrorKind, PersistenceError};
use crate::legacy_pull_request_key::backfill_projection_thread_pull_requests;
use crate::migration_manifest::{APPLICATION_EVENT_SOURCE_MARKER, MIGRATIONS, MigrationKind};

const MIGRATIONS_TABLE: &str = "effect_sql_migrations";
const FIRST_V2_MIGRATION_ID: i64 = 55;

const ENSURE_TABLE: &str = "
CREATE TABLE IF NOT EXISTS effect_sql_migrations (
  migration_id integer PRIMARY KEY NOT NULL,
  created_at datetime NOT NULL DEFAULT current_timestamp,
  name VARCHAR(255) NOT NULL
)";

#[derive(Debug, Clone, Copy, Default)]
pub struct RunMigrationsOptions {
    pub to_migration_inclusive: Option<i64>,
}

pub fn migration_manifest() -> Vec<(i64, &'static str)> {
    MIGRATIONS.iter().map(|m| (m.id, m.name)).collect()
}

pub fn run_migrations(
    conn: &Connection,
    options: RunMigrationsOptions,
) -> Result<Vec<(i64, &'static str)>, PersistenceError> {
    let includes_v2 = options
        .to_migration_inclusive
        .map(|id| id >= FIRST_V2_MIGRATION_ID)
        .unwrap_or(true);
    // `reconcileV2PreviewMigration` is intentionally not run. See PORTING.md.
    if includes_v2 {
        assert_v2_migrations_can_run(conn, options.to_migration_inclusive)?;
    }
    let executed = migrate(conn, options.to_migration_inclusive)?;
    if executed.is_empty() {
        tracing::debug!("Database schema is current");
    } else {
        let migrations: Vec<String> = executed
            .iter()
            .map(|(id, name)| format!("{id}_{name}"))
            .collect();
        tracing::info!(?migrations, "Migrations ran successfully");
    }
    warn_divergent(conn)?;
    Ok(executed)
}

fn assert_v2_migrations_can_run(
    conn: &Connection,
    through_id: Option<i64>,
) -> Result<(), PersistenceError> {
    if !table_exists(conn, MIGRATIONS_TABLE)? {
        return Ok(());
    }
    let recorded = recorded_migrations(conn)?;
    let latest = recorded.iter().map(|(id, _)| *id).max().unwrap_or(0);
    let names: std::collections::HashMap<i64, String> = recorded.into_iter().collect();
    let mut skipped = Vec::new();
    for migration in MIGRATIONS {
        if migration.id < FIRST_V2_MIGRATION_ID || migration.id > latest {
            continue;
        }
        if through_id.is_some_and(|limit| migration.id > limit) {
            continue;
        }
        let recorded_name = names.get(&migration.id).map(String::as_str);
        if recorded_name == Some(migration.name) {
            continue;
        }
        skipped.push(format!(
            "{}_{} (recorded: {})",
            migration.id,
            migration.name,
            recorded_name.unwrap_or("nothing")
        ));
    }
    if skipped.is_empty() {
        return Ok(());
    }
    Err(PersistenceError::migration(
        MigrationErrorKind::BadState,
        format!(
            "This database's migration history would skip required schema migrations: {}. The migrator skips every id up to the highest recorded id ({latest}), regardless of name. It was probably written by a different build. Start the build that wrote it, or report this migration history so an upgrade path can be added.",
            skipped.join(", ")
        ),
    ))
}

fn migrate(
    conn: &Connection,
    through_id: Option<i64>,
) -> Result<Vec<(i64, &'static str)>, PersistenceError> {
    conn.execute_batch(ENSURE_TABLE)
        .map_err(|err| PersistenceError::sql("runMigrations.ensureTable", &err))?;

    let ids: Vec<i64> = MIGRATIONS.iter().map(|m| m.id).collect();
    if ids.len() != ids.iter().collect::<std::collections::HashSet<_>>().len() {
        return Err(PersistenceError::migration(
            MigrationErrorKind::Duplicates,
            "Found duplicate migration id's",
        ));
    }

    conn.execute_batch("BEGIN")
        .map_err(|err| PersistenceError::sql("runMigrations.begin", &err))?;
    let result = migrate_in_transaction(conn, through_id);
    match &result {
        Ok(_) => {
            conn.execute_batch("COMMIT")
                .map_err(|err| PersistenceError::sql("runMigrations.commit", &err))?;
        }
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    result
}

fn migrate_in_transaction(
    conn: &Connection,
    through_id: Option<i64>,
) -> Result<Vec<(i64, &'static str)>, PersistenceError> {
    let latest = conn
        .query_row(
            "SELECT migration_id FROM effect_sql_migrations ORDER BY migration_id DESC LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|err| PersistenceError::sql("runMigrations.latest", &err))?
        .unwrap_or(0);

    let required: Vec<_> = MIGRATIONS
        .iter()
        .filter(|m| m.id > latest && through_id.is_none_or(|limit| m.id <= limit))
        .collect();
    if required.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = std::iter::repeat_n("(?, ?)", required.len())
        .collect::<Vec<_>>()
        .join(", ");
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(required.len() * 2);
    for migration in &required {
        values.push(Box::new(migration.id));
        values.push(Box::new(migration.name.to_owned()));
    }
    let insert =
        format!("INSERT INTO effect_sql_migrations (migration_id, name) VALUES {placeholders}");
    if let Err(err) = conn.execute(&insert, rusqlite::params_from_iter(values.iter())) {
        if is_constraint(&err) {
            tracing::debug!("Migrations already running");
            return Ok(Vec::new());
        }
        return Err(PersistenceError::sql("runMigrations.insert", &err));
    }

    let mut executed = Vec::with_capacity(required.len());
    for migration in required {
        tracing::debug!(
            migration_id = migration.id,
            migration_name = migration.name,
            "Running migration"
        );
        if let Err(err) = run_one(conn, migration.kind, migration.sql) {
            return Err(match err {
                PersistenceError::Migration { .. } => err,
                other => PersistenceError::migration(
                    MigrationErrorKind::Failed,
                    format!(
                        "Migration \"{}_{}\" failed: {other}",
                        migration.id, migration.name
                    ),
                ),
            });
        }
        executed.push((migration.id, migration.name));
    }
    Ok(executed)
}

fn run_one(conn: &Connection, kind: MigrationKind, sql: &str) -> Result<(), PersistenceError> {
    match kind {
        MigrationKind::Sql => execute_script(conn, sql),
        MigrationKind::PullRequests => {
            execute_script(conn, sql)?;
            backfill_projection_thread_pull_requests(conn)
        }
        MigrationKind::OrchestrationV2 => {
            let (before, after) = sql
                .split_once(APPLICATION_EVENT_SOURCE_MARKER)
                .ok_or_else(|| {
                    PersistenceError::migration(
                        MigrationErrorKind::Failed,
                        "Migration \"55_OrchestrationV2\" failed: missing application event source marker",
                    )
                })?;
            execute_script(conn, before)?;
            copy_application_event_source(conn)?;
            execute_script(conn, after)
        }
    }
}

fn execute_script(conn: &Connection, sql: &str) -> Result<(), PersistenceError> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Ok(());
    }
    conn.execute_batch(sql)
        .map_err(|err| PersistenceError::sql("runMigrations.script", &err))
}

fn warn_divergent(conn: &Connection) -> Result<(), PersistenceError> {
    if !table_exists(conn, MIGRATIONS_TABLE)? {
        return Ok(());
    }
    let recorded = recorded_migrations(conn)?;
    let mut divergent = Vec::new();
    for (id, name) in recorded {
        match MIGRATIONS.iter().find(|m| m.id == id) {
            None => divergent.push(format!("{id}:{name} (unknown to this build)")),
            Some(expected) if expected.name != name => {
                divergent.push(format!("{id}:{name} (this build: {})", expected.name));
            }
            Some(_) => {}
        }
    }
    if !divergent.is_empty() {
        tracing::warn!(
            ?divergent,
            "Database migration history diverges from this build; recorded migration ids are skipped, not reconciled by name."
        );
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, PersistenceError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get(0),
        )
        .map_err(|err| PersistenceError::sql("runMigrations.tableExists", &err))?;
    Ok(count > 0)
}

fn recorded_migrations(conn: &Connection) -> Result<Vec<(i64, String)>, PersistenceError> {
    let mut stmt = conn
        .prepare("SELECT migration_id, name FROM effect_sql_migrations")
        .map_err(|err| PersistenceError::sql("runMigrations.history", &err))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|err| PersistenceError::sql("runMigrations.history", &err))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql("runMigrations.history", &err))
}

fn is_constraint(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}
