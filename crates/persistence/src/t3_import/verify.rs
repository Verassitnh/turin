//! Migrate a snapshot forward and check it against a database this build created.
//!
//! `sqlite_stat1` / `sqlite_stat4` are left out of the schema diff. Upstream's
//! `PRAGMA optimize` creates them, and this build's SQLite also creates
//! `sqlite_stat4` (see PORTING.md). They are statistics, not schema.
//! `effect_sql_migrations` is allowed to grow by exactly the migrations that ran.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use crate::errors::PersistenceError;
use crate::migrations::{RunMigrationsOptions, run_migrations};
use crate::orchestration_event_store::{
    ApplicationStoredEvent, ReadAgentEvents, ReadApplicationEvents, latest_application_sequence,
    read_agent_events, read_application_events,
};

use super::T3ImportError;
use super::plan::{ImportPlan, plan};

const PAGE: i64 = 200;

/// Counts from one pass through the event-store read APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventReadReport {
    /// Application rows (project, or v2 thread) that decoded.
    pub decoded: u64,
    pub by_type: BTreeMap<String, u64>,
    /// Known-unknown types. A strict read skips these; they are not decode errors.
    pub unknown: u64,
    pub unknown_by_type: BTreeMap<String, u64>,
    /// Thread rows the v2 reader does not return (`application_event_version` ≠ 2).
    pub legacy_thread_rows: u64,
    pub legacy_by_type: BTreeMap<String, u64>,
    /// Rows returned by `read_agent_events`.
    pub agent_decoded: u64,
    pub agent_unknown: u64,
}

/// Result of migrating and verifying a snapshot. No thread text, paths, or tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub plan: ImportPlan,
    /// Migrations `run_migrations` actually applied, in order.
    pub applied: Vec<(i64, String)>,
    /// User tables after migration (`sqlite_%` excluded).
    pub tables: usize,
    /// Row count per pre-existing user table. `effect_sql_migrations` is omitted.
    pub row_counts: BTreeMap<String, i64>,
    pub events: EventReadReport,
    pub projects: i64,
    pub active_projects: i64,
    pub threads: i64,
    pub active_threads: i64,
    pub v2_threads: i64,
    pub active_v2_threads: i64,
}

pub(crate) fn migrate_and_verify(conn: &Connection) -> Result<ImportReport, T3ImportError> {
    let plan = plan(conn)?;
    let before = row_counts(conn)?;
    let ledger_before = count_ledger(conn)?;
    let executed = run_migrations(conn, RunMigrationsOptions::default())?;
    let applied: Vec<(i64, String)> = executed
        .into_iter()
        .map(|(id, name)| (id, name.to_owned()))
        .collect();
    if applied.len() != plan.pending.len()
        || applied.iter().map(|(id, _)| *id).collect::<Vec<_>>()
            != plan.pending.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    {
        return Err(T3ImportError::WouldSkipMigrations {
            message: format!(
                "expected to apply [{}] but applied [{}]",
                names(&plan.pending),
                names(&applied)
            ),
        });
    }

    let fresh = fresh_schema()?;
    let got = schema_objects(conn)?;
    if fresh != got {
        return Err(T3ImportError::SchemaMismatch {
            message: schema_diff(&fresh, &got),
        });
    }

    let after = row_counts(conn)?;
    if before != after {
        return Err(T3ImportError::RowCountChanged {
            message: count_diff(&before, &after),
        });
    }
    let ledger_after = count_ledger(conn)?;
    let expected_ledger = ledger_before + applied.len() as i64;
    if ledger_after != expected_ledger {
        return Err(T3ImportError::RowCountChanged {
            message: format!(
                "effect_sql_migrations row count changed from {ledger_before} to {ledger_after} (expected {expected_ledger})"
            ),
        });
    }

    let events = read_events(conn)?;
    Ok(ImportReport {
        plan,
        applied,
        tables: after.len(),
        row_counts: after,
        events,
        projects: count_sql(conn, "SELECT COUNT(*) FROM projection_projects")?,
        active_projects: count_sql(
            conn,
            "SELECT COUNT(*) FROM projection_projects WHERE deleted_at IS NULL",
        )?,
        threads: count_sql(conn, "SELECT COUNT(*) FROM projection_threads")?,
        active_threads: count_sql(
            conn,
            "SELECT COUNT(*) FROM projection_threads WHERE deleted_at IS NULL",
        )?,
        v2_threads: count_sql(
            conn,
            "SELECT COUNT(*) FROM orchestration_v2_projection_threads",
        )?,
        active_v2_threads: count_sql(
            conn,
            "SELECT COUNT(*) FROM orchestration_v2_projection_threads WHERE deleted_at IS NULL",
        )?,
    })
}

fn names(rows: &[(i64, String)]) -> String {
    rows.iter()
        .map(|(id, name)| format!("{id}_{name}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn fresh_schema() -> Result<Vec<SchemaObject>, T3ImportError> {
    let dir = tempfile::TempDir::with_prefix("turin-t3-fresh-")
        .map_err(|err| T3ImportError::io("create fresh schema database", err))?;
    let conn = super::snapshot::open_snapshot(&dir.path().join("fresh.sqlite"))?;
    run_migrations(&conn, RunMigrationsOptions::default())?;
    schema_objects(&conn)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SchemaObject {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

fn schema_objects(conn: &Connection) -> Result<Vec<SchemaObject>, T3ImportError> {
    let mut stmt = conn
        .prepare(
            "
            SELECT type, name, tbl_name, sql
            FROM sqlite_master
            WHERE name NOT LIKE 'sqlite_stat%'
            ORDER BY type, name, tbl_name
            ",
        )
        .map_err(|err| PersistenceError::sql("t3Import.schema", &err))?;
    let rows = stmt
        .query_map([], |row| {
            let sql: Option<String> = row.get(3)?;
            Ok(SchemaObject {
                kind: row.get(0)?,
                name: row.get(1)?,
                table: row.get(2)?,
                sql: normalize_sql(sql.as_deref()),
            })
        })
        .map_err(|err| PersistenceError::sql("t3Import.schema", &err))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql("t3Import.schema", &err).into())
}

fn normalize_sql(sql: Option<&str>) -> String {
    let Some(sql) = sql else {
        return String::new();
    };
    let mut out = String::new();
    let mut spaced = false;
    for ch in sql.chars() {
        if ch.is_whitespace() {
            if !spaced && !out.is_empty() {
                out.push(' ');
                spaced = true;
            }
        } else {
            out.push(ch);
            spaced = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn schema_diff(fresh: &[SchemaObject], got: &[SchemaObject]) -> String {
    let left: BTreeMap<(&str, &str), &SchemaObject> = fresh
        .iter()
        .map(|object| ((object.kind.as_str(), object.name.as_str()), object))
        .collect();
    let right: BTreeMap<(&str, &str), &SchemaObject> = got
        .iter()
        .map(|object| ((object.kind.as_str(), object.name.as_str()), object))
        .collect();
    let mut lines = Vec::new();
    for key in left.keys() {
        if !right.contains_key(key) {
            lines.push(format!("missing in import: {} {}", key.0, key.1));
        }
    }
    for (key, object) in &right {
        match left.get(key) {
            None => lines.push(format!("extra in import: {} {}", key.0, key.1)),
            Some(expected) if expected.sql != object.sql || expected.table != object.table => {
                lines.push(format!(
                    "schema differs for {} {}: fresh [{}] import [{}]",
                    key.0, key.1, expected.sql, object.sql
                ));
            }
            Some(_) => {}
        }
    }
    if lines.is_empty() {
        return "schema objects differ in order or duplicates".to_owned();
    }
    let extra = lines.len().saturating_sub(12);
    lines.truncate(12);
    if extra > 0 {
        lines.push(format!("{extra} more differences"));
    }
    format!(
        "imported schema does not match a fresh Turin database: {}",
        lines.join("; ")
    )
}

fn row_counts(conn: &Connection) -> Result<BTreeMap<String, i64>, T3ImportError> {
    let mut stmt = conn
        .prepare(
            "
            SELECT name FROM sqlite_master
            WHERE type = 'table'
              AND name NOT LIKE 'sqlite_%'
              AND name != 'effect_sql_migrations'
            ORDER BY name
            ",
        )
        .map_err(|err| PersistenceError::sql("t3Import.tables", &err))?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .map_err(|err| PersistenceError::sql("t3Import.tables", &err))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql("t3Import.tables", &err))?;
    let mut counts = BTreeMap::new();
    for name in names {
        let quoted = quote_ident(&name);
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {quoted}"), [], |row| {
                row.get(0)
            })
            .map_err(|err| PersistenceError::sql("t3Import.count", &err))?;
        counts.insert(name, count);
    }
    Ok(counts)
}

fn count_ledger(conn: &Connection) -> Result<i64, T3ImportError> {
    count_sql(conn, "SELECT COUNT(*) FROM effect_sql_migrations")
}

fn count_sql(conn: &Connection, sql: &str) -> Result<i64, T3ImportError> {
    conn.query_row(sql, [], |row| row.get(0))
        .map_err(|err| PersistenceError::sql("t3Import.count", &err).into())
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn count_diff(before: &BTreeMap<String, i64>, after: &BTreeMap<String, i64>) -> String {
    let mut lines = Vec::new();
    for (name, count) in before {
        match after.get(name) {
            None => lines.push(format!("{name} disappeared (was {count} rows)")),
            Some(now) if now != count => {
                lines.push(format!("{name} changed from {count} to {now} rows"))
            }
            Some(_) => {}
        }
    }
    for name in after.keys() {
        if !before.contains_key(name) {
            lines.push(format!("{name} appeared during migration"));
        }
    }
    if lines.is_empty() {
        return "row counts changed".to_owned();
    }
    format!("row counts were not preserved: {}", lines.join("; "))
}

fn read_events(conn: &Connection) -> Result<EventReadReport, T3ImportError> {
    let high = latest_application_sequence(conn)?;
    let application = scan_application(conn, high)?;
    let agent = scan_agent(conn, high)?;
    let (legacy_thread_rows, legacy_by_type) = legacy_threads(conn)?;
    let application_agents = count_sql(
        conn,
        "
        SELECT COUNT(*) FROM orchestration_events
        WHERE application_event_version = 2 AND aggregate_kind = 'thread'
        ",
    )? as u64;
    let agent_total = agent.decoded + agent.unknown;
    if agent_total != application_agents {
        return Err(T3ImportError::Decode {
            message: format!(
                "read_agent_events returned {agent_total} rows but the v2 thread table has {application_agents}"
            ),
        });
    }
    let application_rows = count_sql(
        conn,
        "
        SELECT COUNT(*) FROM orchestration_events
        WHERE aggregate_kind = 'project'
           OR (application_event_version = 2 AND aggregate_kind = 'thread')
        ",
    )? as u64;
    let application_total = application.decoded + application.unknown;
    if application_total != application_rows {
        return Err(T3ImportError::Decode {
            message: format!(
                "read_application_events returned {application_total} rows but the read model has {application_rows}"
            ),
        });
    }
    Ok(EventReadReport {
        decoded: application.decoded,
        by_type: application.by_type,
        unknown: application.unknown,
        unknown_by_type: application.unknown_by_type,
        legacy_thread_rows,
        legacy_by_type,
        agent_decoded: agent.decoded,
        agent_unknown: agent.unknown,
    })
}

#[derive(Default)]
struct Scan {
    decoded: u64,
    by_type: BTreeMap<String, u64>,
    unknown: u64,
    unknown_by_type: BTreeMap<String, u64>,
}

fn scan_application(conn: &Connection, high: i64) -> Result<Scan, T3ImportError> {
    let mut scan = Scan::default();
    let mut after = 0i64;
    while after < high {
        let through = after.saturating_add(PAGE).min(high);
        match read_application_events(
            conn,
            &ReadApplicationEvents {
                after_sequence: after,
                through_sequence: through,
                skip_unknown_event_types: false,
            },
        ) {
            Ok(events) => record_decoded(&mut scan, &events),
            Err(err) if is_unknown(&err) => {
                match read_application_events(
                    conn,
                    &ReadApplicationEvents {
                        after_sequence: after,
                        through_sequence: through,
                        skip_unknown_event_types: true,
                    },
                ) {
                    Ok(events) => {
                        reconcile_unknown(
                            &mut scan,
                            &events,
                            &type_counts(conn, after, through, false)?,
                        )?;
                    }
                    Err(_) => walk_application(&mut scan, conn, after, through)?,
                }
            }
            Err(_) => walk_application(&mut scan, conn, after, through)?,
        }
        after = through;
    }
    Ok(scan)
}

fn scan_agent(conn: &Connection, high: i64) -> Result<Scan, T3ImportError> {
    let mut scan = Scan::default();
    let mut after = 0i64;
    while after < high {
        let through = after.saturating_add(PAGE).min(high);
        let read = |skip| {
            read_agent_events(
                conn,
                &ReadAgentEvents {
                    after_sequence: Some(after),
                    through_sequence: Some(through),
                    thread_id: None,
                    command_id: None,
                    event_type: None,
                    limit: Some(u64::try_from(PAGE).unwrap_or(200) + 1),
                    skip_unknown_event_types: skip,
                },
            )
        };
        match read(false) {
            Ok(events) => record_agent(&mut scan, &events),
            Err(err) if is_unknown(&err) => match read(true) {
                Ok(events) => {
                    let events = events
                        .into_iter()
                        .map(ApplicationStoredEvent::Agent)
                        .collect::<Vec<_>>();
                    reconcile_unknown(
                        &mut scan,
                        &events,
                        &type_counts(conn, after, through, true)?,
                    )?;
                }
                Err(_) => walk_agent(&mut scan, conn, after, through)?,
            },
            Err(_) => walk_agent(&mut scan, conn, after, through)?,
        }
        after = through;
    }
    Ok(scan)
}

fn walk_application(
    scan: &mut Scan,
    conn: &Connection,
    after: i64,
    through: i64,
) -> Result<(), T3ImportError> {
    let mut cursor = after;
    while cursor < through {
        let next = cursor + 1;
        match read_application_events(
            conn,
            &ReadApplicationEvents {
                after_sequence: cursor,
                through_sequence: next,
                skip_unknown_event_types: false,
            },
        ) {
            Ok(events) => record_decoded(scan, &events),
            Err(err) if is_unknown(&err) => note_unknown_row(scan, conn, next)?,
            Err(err) => return Err(decode_failure(conn, next, &err)?),
        }
        cursor = next;
    }
    Ok(())
}

fn walk_agent(
    scan: &mut Scan,
    conn: &Connection,
    after: i64,
    through: i64,
) -> Result<(), T3ImportError> {
    let mut cursor = after;
    while cursor < through {
        let next = cursor + 1;
        match read_agent_events(
            conn,
            &ReadAgentEvents {
                after_sequence: Some(cursor),
                through_sequence: Some(next),
                thread_id: None,
                command_id: None,
                event_type: None,
                limit: Some(2),
                skip_unknown_event_types: false,
            },
        ) {
            Ok(events) => record_agent(scan, &events),
            Err(err) if is_unknown(&err) => note_unknown_row(scan, conn, next)?,
            Err(err) => return Err(decode_failure(conn, next, &err)?),
        }
        cursor = next;
    }
    Ok(())
}

fn record_decoded(scan: &mut Scan, events: &[ApplicationStoredEvent]) {
    for event in events {
        scan.decoded += 1;
        *scan
            .by_type
            .entry(event_type(event).to_owned())
            .or_default() += 1;
    }
}

fn record_agent(scan: &mut Scan, events: &[crate::orchestration_event_store::StoredAgentEvent]) {
    for event in events {
        scan.decoded += 1;
        *scan
            .by_type
            .entry(event.event.event_type.clone())
            .or_default() += 1;
    }
}

fn reconcile_unknown(
    scan: &mut Scan,
    events: &[ApplicationStoredEvent],
    sql_counts: &BTreeMap<String, i64>,
) -> Result<(), T3ImportError> {
    let mut decoded: BTreeMap<String, i64> = BTreeMap::new();
    for event in events {
        scan.decoded += 1;
        let name = event_type(event).to_owned();
        *scan.by_type.entry(name.clone()).or_default() += 1;
        *decoded.entry(name).or_default() += 1;
    }
    for (name, sql_count) in sql_counts {
        let got = decoded.remove(name).unwrap_or(0);
        if got == *sql_count {
            continue;
        }
        if got == 0 {
            let unknown = u64::try_from(*sql_count).unwrap_or(0);
            scan.unknown += unknown;
            *scan.unknown_by_type.entry(name.clone()).or_default() += unknown;
            continue;
        }
        return Err(T3ImportError::Decode {
            message: format!("event type {name} decoded {got} of {sql_count} rows in a page"),
        });
    }
    if let Some((name, count)) = decoded.into_iter().next() {
        return Err(T3ImportError::Decode {
            message: format!("event type {name} decoded {count} rows that were not in the table"),
        });
    }
    Ok(())
}

fn note_unknown_row(
    scan: &mut Scan,
    conn: &Connection,
    sequence: i64,
) -> Result<(), T3ImportError> {
    let name = event_type_at(conn, sequence)?;
    scan.unknown += 1;
    *scan.unknown_by_type.entry(name).or_default() += 1;
    Ok(())
}

fn decode_failure(
    conn: &Connection,
    sequence: i64,
    err: &PersistenceError,
) -> Result<T3ImportError, T3ImportError> {
    let name = event_type_at(conn, sequence).unwrap_or_else(|_| "unknown".to_owned());
    Ok(T3ImportError::Decode {
        message: format!(
            "sequence {sequence} type {name}: {}",
            decode_category(&err.to_string())
        ),
    })
}

fn decode_category(issue: &str) -> &'static str {
    if issue.contains("unknown event type") {
        "unknown event type"
    } else if issue.contains("non-empty object") {
        "payload was empty or not an object"
    } else if issue.contains("RFC 3339") {
        "occurred_at was not RFC 3339"
    } else if issue.contains("non-blank") {
        "blank id"
    } else if issue.contains("expected") || issue.contains("EOF") || issue.contains("invalid") {
        "invalid json or value"
    } else {
        "decode failed"
    }
}

fn is_unknown(err: &PersistenceError) -> bool {
    err.is_decode() && err.to_string().contains("unknown event type")
}

fn event_type(event: &ApplicationStoredEvent) -> &str {
    match event {
        ApplicationStoredEvent::Project(event) => &event.event_type,
        ApplicationStoredEvent::Agent(event) => &event.event.event_type,
    }
}

fn event_type_at(conn: &Connection, sequence: i64) -> Result<String, T3ImportError> {
    conn.query_row(
        "SELECT event_type FROM orchestration_events WHERE sequence = ?1",
        params![sequence],
        |row| row.get(0),
    )
    .map_err(|err| PersistenceError::sql("t3Import.eventType", &err).into())
}

fn type_counts(
    conn: &Connection,
    after: i64,
    through: i64,
    agent_only: bool,
) -> Result<BTreeMap<String, i64>, T3ImportError> {
    let filter = if agent_only {
        "application_event_version = 2 AND aggregate_kind = 'thread'"
    } else {
        "aggregate_kind = 'project' OR (application_event_version = 2 AND aggregate_kind = 'thread')"
    };
    let mut stmt = conn
        .prepare(&format!(
            "
            SELECT event_type, COUNT(*)
            FROM orchestration_events
            WHERE sequence > ?1 AND sequence <= ?2
              AND ({filter})
            GROUP BY event_type
            "
        ))
        .map_err(|err| PersistenceError::sql("t3Import.eventTypes", &err))?;
    let rows = stmt
        .query_map(params![after, through], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|err| PersistenceError::sql("t3Import.eventTypes", &err))?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(|err| PersistenceError::sql("t3Import.eventTypes", &err).into())
}

fn legacy_threads(conn: &Connection) -> Result<(u64, BTreeMap<String, u64>), T3ImportError> {
    let mut stmt = conn
        .prepare(
            "
            SELECT event_type, COUNT(*)
            FROM orchestration_events
            WHERE aggregate_kind = 'thread' AND application_event_version != 2
            GROUP BY event_type
            ORDER BY event_type
            ",
        )
        .map_err(|err| PersistenceError::sql("t3Import.legacy", &err))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)?)))
        .map_err(|err| PersistenceError::sql("t3Import.legacy", &err))?;
    let mut by_type = BTreeMap::new();
    let mut total = 0u64;
    for row in rows {
        let (name, count) = row.map_err(|err| PersistenceError::sql("t3Import.legacy", &err))?;
        let count = u64::try_from(count).unwrap_or(0);
        total += count;
        by_type.insert(name, count);
    }
    Ok((total, by_type))
}
