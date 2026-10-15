//! Ledger check before any migration runs.
//!
//! Matches upstream `assertV2MigrationsCanRun` (`Migrations.ts`): only V2 ids
//! (55 and up) are checked. The migrator skips every id at or below the
//! highest recorded id, so a V2 id recorded under another name, or a V2 gap
//! below the highest recorded id, would start without V2's schema. Ids below
//! 55 are accepted as upstream accepts them, with a warning from
//! `run_migrations`. A V2 id this build does not have is a newer T3 and is
//! refused, since its schema has never been created here.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::errors::PersistenceError;
use crate::migrations::{FIRST_V2_MIGRATION_ID, migration_manifest};

use super::T3ImportError;

const MIGRATIONS_TABLE: &str = "effect_sql_migrations";

/// What Turin would do with a snapshot, after the ledger has been accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlan {
    /// Highest `migration_id` in `effect_sql_migrations`.
    pub latest_id: i64,
    /// Recorded `(migration_id, name)` pairs, ordered by id.
    pub recorded: Vec<(i64, String)>,
    /// Turin migrations with an id above `latest_id`, in apply order.
    pub pending: Vec<(i64, String)>,
}

pub(crate) fn plan(conn: &Connection) -> Result<ImportPlan, T3ImportError> {
    if !table_exists(conn, MIGRATIONS_TABLE)? {
        return Err(T3ImportError::NotV2);
    }
    let recorded = recorded_migrations(conn)?;
    if recorded.is_empty() {
        return Err(T3ImportError::NotV2);
    }
    let manifest = migration_manifest();
    let by_id: BTreeMap<i64, &str> = manifest.iter().copied().collect();

    let mut unknown = Vec::new();
    let mut mismatched = Vec::new();
    for (id, name) in recorded
        .iter()
        .filter(|(id, _)| *id >= FIRST_V2_MIGRATION_ID)
    {
        match by_id.get(id) {
            None => unknown.push(format!("{id}_{name}")),
            Some(expected) if *expected != name => {
                mismatched.push(format!("{id}_{name} (this build: {expected})"));
            }
            Some(_) => {}
        }
    }
    if !unknown.is_empty() || !mismatched.is_empty() {
        let mut parts = Vec::new();
        if !unknown.is_empty() {
            parts.push(format!(
                "recorded migration ids this build does not have: {}",
                unknown.join(", ")
            ));
        }
        if !mismatched.is_empty() {
            parts.push(format!(
                "recorded migration names differ from this build: {}",
                mismatched.join(", ")
            ));
        }
        return Err(T3ImportError::NewerThanTurin {
            message: parts.join("; "),
        });
    }

    let latest_id = recorded.iter().map(|(id, _)| *id).max().unwrap_or(0);
    let recorded_ids: BTreeMap<i64, &str> = recorded
        .iter()
        .map(|(id, name)| (*id, name.as_str()))
        .collect();
    let mut skipped = Vec::new();
    for (id, name) in &manifest {
        if *id < FIRST_V2_MIGRATION_ID || *id > latest_id {
            continue;
        }
        match recorded_ids.get(id) {
            Some(recorded_name) if *recorded_name == *name => {}
            Some(recorded_name) => skipped.push(format!("{id}_{name} (recorded: {recorded_name})")),
            None => skipped.push(format!("{id}_{name} (recorded: nothing)")),
        }
    }
    if !skipped.is_empty() {
        return Err(T3ImportError::WouldSkipMigrations {
            message: format!(
                "This database's migration history would skip required schema migrations: {}. The migrator skips every id up to the highest recorded id ({latest_id}), regardless of name. It was probably written by a different build. Start the build that wrote it, or report this migration history so an upgrade path can be added.",
                skipped.join(", ")
            ),
        });
    }

    let pending = manifest
        .into_iter()
        .filter(|(id, _)| *id > latest_id)
        .map(|(id, name)| (id, name.to_owned()))
        .collect();
    Ok(ImportPlan {
        latest_id,
        recorded,
        pending,
    })
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, T3ImportError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get(0),
        )
        .map_err(|err| PersistenceError::sql("t3Import.tableExists", &err))?;
    Ok(count > 0)
}

fn recorded_migrations(conn: &Connection) -> Result<Vec<(i64, String)>, T3ImportError> {
    let exists: Option<i64> = conn
        .query_row("SELECT 1 FROM effect_sql_migrations LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|err| PersistenceError::sql("t3Import.history", &err))?;
    if exists.is_none() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT migration_id, name FROM effect_sql_migrations ORDER BY migration_id")
        .map_err(|err| PersistenceError::sql("t3Import.history", &err))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|err| PersistenceError::sql("t3Import.history", &err))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql("t3Import.history", &err).into())
}
