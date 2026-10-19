//! Port of upstream/apps/server/src/persistence/Migrations.ts
//! Also: Port of upstream/apps/server/src/persistence/Migrations/050_ProjectionThreadPullRequests.ts.
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
/// Upstream `FIRST_V2_MIGRATION_ID`. Ids below it are never refused, only warned about.
pub(crate) const FIRST_V2_MIGRATION_ID: i64 = 55;

const ENSURE_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS "effect_sql_migrations" (
  migration_id integer PRIMARY KEY NOT NULL,
  created_at datetime NOT NULL DEFAULT current_timestamp,
  name VARCHAR(255) NOT NULL
)"#;

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

    // A deferred BEGIN only takes the write lock at the first write. If another
    // process commits after this transaction's first read, that write fails at
    // once with SQLITE_BUSY_SNAPSHOT, which busy_timeout cannot wait out.
    conn.execute_batch("BEGIN IMMEDIATE")
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

/// Re-executes one migration's body without touching the ledger.
/// Upstream tests call the migration effect again after the id is already recorded.
#[cfg(test)]
pub(crate) fn rerun_migration_body(conn: &Connection, id: i64) -> Result<(), PersistenceError> {
    let migration = MIGRATIONS
        .iter()
        .find(|migration| migration.id == id)
        .ok_or_else(|| {
            PersistenceError::migration(
                MigrationErrorKind::Failed,
                format!("unknown migration {id}"),
            )
        })?;
    run_one(conn, migration.kind, migration.sql)
}

fn run_one(conn: &Connection, kind: MigrationKind, sql: &str) -> Result<(), PersistenceError> {
    match kind {
        MigrationKind::Sql => execute_script(conn, sql),
        MigrationKind::AddColumnIfMissing => execute_add_column_if_missing(conn, sql),
        MigrationKind::CatchStatementErrors => execute_catching_statements(conn, sql),
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

/// Upstream checks `PRAGMA table_info` before each `ADD COLUMN` so a partially
/// migrated database (the column exists, the ledger row does not) still opens.
fn execute_add_column_if_missing(conn: &Connection, sql: &str) -> Result<(), PersistenceError> {
    for statement in sql_statements(sql) {
        if let Some((table, column)) = add_column_target(statement)
            && column_exists(conn, table, column)?
        {
            continue;
        }
        execute_script(conn, statement)?;
    }
    Ok(())
}

/// Migration 023 wraps each `ALTER` in `Effect.catch(() => Effect.void)`.
fn execute_catching_statements(conn: &Connection, sql: &str) -> Result<(), PersistenceError> {
    for statement in sql_statements(sql) {
        let _ = execute_script(conn, statement);
    }
    Ok(())
}

/// Splits a migration script on `;`. Quotes and `;` inside `--` or `/* */`
/// comments do not count, as in SQLite's own tokenizer. A comment stays in the
/// statement it precedes, so an apostrophe in a note cannot hide the `ALTER`
/// after it.
fn sql_statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(q) = quote {
            if byte == q {
                if i + 1 < bytes.len() && bytes[i + 1] == q {
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if byte == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i = bytes[i..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |newline| i + newline + 1);
            continue;
        }
        if byte == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = sql[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |end| i + 2 + end + 2);
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
            i += 1;
            continue;
        }
        if byte == b';' {
            let statement = sql[start..i].trim();
            if !statement.is_empty() {
                out.push(statement);
            }
            start = i + 1;
        }
        i += 1;
    }
    let tail = sql[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn add_column_target(statement: &str) -> Option<(&str, &str)> {
    let statement = strip_leading_sql_comments(statement);
    let parts: Vec<&str> = statement.split_whitespace().collect();
    if parts.len() >= 6
        && parts[0].eq_ignore_ascii_case("alter")
        && parts[1].eq_ignore_ascii_case("table")
        && parts[3].eq_ignore_ascii_case("add")
        && parts[4].eq_ignore_ascii_case("column")
        && is_ident(parts[2])
        && is_ident(parts[5])
    {
        return Some((parts[2], parts[5]));
    }
    None
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, PersistenceError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|err| PersistenceError::sql("runMigrations.tableInfo", &err))?;
    let mut rows = stmt
        .query([])
        .map_err(|err| PersistenceError::sql("runMigrations.tableInfo", &err))?;
    while let Some(row) = rows
        .next()
        .map_err(|err| PersistenceError::sql("runMigrations.tableInfo", &err))?
    {
        let name: String = row
            .get(1)
            .map_err(|err| PersistenceError::sql("runMigrations.tableInfo", &err))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn strip_leading_sql_comments(sql: &str) -> &str {
    let mut rest = sql;
    loop {
        let trimmed = rest.trim_start();
        if let Some(after) = trimmed.strip_prefix("--") {
            let Some(newline) = after.find('\n') else {
                return "";
            };
            rest = &after[newline + 1..];
        } else if let Some(after) = trimmed.strip_prefix("/*") {
            let Some(end) = after.find("*/") else {
                return "";
            };
            rest = &after[end + 2..];
        } else {
            return trimmed;
        }
    }
}

fn is_ident(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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

#[cfg(test)]
mod sql_splitter_tests {
    use super::*;

    const SCRIPT: &str = "-- don't touch the next column; it's a note\nALTER TABLE t ADD COLUMN a TEXT;\n/* a; b's block */\nALTER TABLE t ADD COLUMN b TEXT;\n-- trailing; it's\nALTER TABLE t ADD COLUMN c TEXT;";

    #[test]
    fn comments_do_not_split_or_quote_statements() {
        let statements = sql_statements(SCRIPT);
        assert_eq!(statements.len(), 3, "{statements:?}");
        assert_eq!(
            statements
                .iter()
                .map(|s| add_column_target(s))
                .collect::<Vec<_>>(),
            vec![Some(("t", "a")), Some(("t", "b")), Some(("t", "c"))]
        );
    }

    #[test]
    fn add_column_guard_still_adds_the_columns_after_a_commented_one() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER, a TEXT);")
            .unwrap();
        execute_add_column_if_missing(&conn, SCRIPT).unwrap();
        assert!(column_exists(&conn, "t", "b").unwrap());
        assert!(column_exists(&conn, "t", "c").unwrap());
    }

    #[test]
    fn a_quote_inside_a_string_is_not_a_comment() {
        let statements = sql_statements("SELECT '--not a comment; x'; SELECT '/*' ; SELECT 2");
        assert_eq!(statements.len(), 3, "{statements:?}");
    }
}
