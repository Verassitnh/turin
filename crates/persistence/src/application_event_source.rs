//! Port of upstream/apps/server/src/persistence/Migrations/OrchestrationV2/ApplicationEventSource.ts
//!
//! Event-store setup and V1 project baseline composed by migration 55.

use rusqlite::{Connection, OptionalExtension, params};

use crate::errors::PersistenceError;

struct LegacyV2Event {
    event_id: String,
    command_id: Option<String>,
    thread_id: String,
    run_id: Option<String>,
    node_id: Option<String>,
    driver: Option<String>,
    provider_instance_id: Option<String>,
    raw_event_id: Option<String>,
    event_type: String,
    occurred_at: String,
    payload_json: String,
}

struct ProjectRow {
    project_id: String,
    title: String,
    workspace_root: String,
    default_model_selection_json: Option<String>,
    scripts_json: String,
    created_at: String,
    updated_at: String,
    deleted_at: Option<String>,
}

pub fn copy_application_event_source(conn: &Connection) -> Result<(), PersistenceError> {
    conn.execute_batch(
        "
        ALTER TABLE orchestration_events
          ADD COLUMN application_event_version INTEGER NOT NULL DEFAULT 1;
        CREATE INDEX idx_orchestration_events_application_sequence
          ON orchestration_events(application_event_version, sequence);
        ALTER TABLE orchestration_command_receipts
          ADD COLUMN command_type TEXT NOT NULL DEFAULT 'legacy';
        ",
    )
    .map_err(|err| PersistenceError::sql("ApplicationEventSource.schema", &err))?;

    let events = legacy_events(conn)?;
    for event in events {
        let metadata = metadata_json(&event)?;
        let actor = if event.raw_event_id.is_none() {
            "server"
        } else {
            "provider"
        };
        conn.execute(
            "
            INSERT INTO orchestration_events (
              event_id, aggregate_kind, stream_id, stream_version, event_type,
              occurred_at, command_id, causation_event_id, correlation_id,
              actor_kind, payload_json, metadata_json, application_event_version
            )
            SELECT
              ?1, 'thread', ?2,
              COALESCE(
                (
                  SELECT MAX(stream_version) + 1
                  FROM orchestration_events
                  WHERE aggregate_kind = 'thread' AND stream_id = ?2
                ),
                0
              ),
              ?3, ?4, ?5, NULL, ?5, ?6, ?7, ?8, 2
            WHERE NOT EXISTS (
              SELECT 1 FROM orchestration_events WHERE event_id = ?1
            )
            ",
            params![
                event.event_id,
                event.thread_id,
                event.event_type,
                event.occurred_at,
                event.command_id,
                actor,
                event.payload_json,
                metadata,
            ],
        )
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.copyEvent", &err))?;
    }

    let projects = project_rows(conn)?;
    for project in projects {
        let created_event_id = format!("migration:39:project:{}:baseline", project.project_id);
        let default_model = match &project.default_model_selection_json {
            None => serde_json::Value::Null,
            Some(json) => serde_json::from_str(json).map_err(|err| {
                PersistenceError::Decode(crate::errors::PersistenceDecodeError::new(
                    "ApplicationEventSource.defaultModelSelection",
                    err.to_string(),
                ))
            })?,
        };
        let scripts: serde_json::Value =
            serde_json::from_str(&project.scripts_json).map_err(|err| {
                PersistenceError::Decode(crate::errors::PersistenceDecodeError::new(
                    "ApplicationEventSource.scripts",
                    err.to_string(),
                ))
            })?;
        let created_payload = serde_json::json!({
            "projectId": project.project_id,
            "title": project.title,
            "workspaceRoot": project.workspace_root,
            "defaultModelSelection": default_model,
            "scripts": scripts,
            "createdAt": project.created_at,
            "updatedAt": project.updated_at,
        });
        let metadata = serde_json::json!({
            "applicationEventVersion": 2,
            "migration": 39,
        });
        let created_payload_json = serde_json::to_string(&created_payload).map_err(|err| {
            PersistenceError::Decode(crate::errors::PersistenceDecodeError::new(
                "ApplicationEventSource.encode",
                err.to_string(),
            ))
        })?;
        let metadata_json = serde_json::to_string(&metadata).map_err(|err| {
            PersistenceError::Decode(crate::errors::PersistenceDecodeError::new(
                "ApplicationEventSource.encode",
                err.to_string(),
            ))
        })?;
        conn.execute(
            "
            INSERT INTO orchestration_events (
              event_id, aggregate_kind, stream_id, stream_version, event_type,
              occurred_at, command_id, causation_event_id, correlation_id,
              actor_kind, payload_json, metadata_json, application_event_version
            )
            VALUES (
              ?1, 'project', ?2,
              COALESCE(
                (
                  SELECT MAX(stream_version) + 1
                  FROM orchestration_events
                  WHERE aggregate_kind = 'project' AND stream_id = ?2
                ),
                0
              ),
              'project.created', ?3, NULL, NULL, NULL, 'server', ?4, ?5, 2
            )
            ",
            params![
                created_event_id,
                project.project_id,
                project.updated_at,
                created_payload_json,
                metadata_json,
            ],
        )
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.projectCreated", &err))?;

        if let Some(deleted_at) = &project.deleted_at {
            let deleted_payload = serde_json::json!({
                "projectId": project.project_id,
                "deletedAt": deleted_at,
            });
            let deleted_payload_json = serde_json::to_string(&deleted_payload).map_err(|err| {
                PersistenceError::Decode(crate::errors::PersistenceDecodeError::new(
                    "ApplicationEventSource.encode",
                    err.to_string(),
                ))
            })?;
            let deleted_id = format!("migration:39:project:{}:deleted", project.project_id);
            conn.execute(
                "
                INSERT INTO orchestration_events (
                  event_id, aggregate_kind, stream_id, stream_version, event_type,
                  occurred_at, command_id, causation_event_id, correlation_id,
                  actor_kind, payload_json, metadata_json, application_event_version
                )
                VALUES (
                  ?1, 'project', ?2,
                  (
                    SELECT MAX(stream_version) + 1
                    FROM orchestration_events
                    WHERE aggregate_kind = 'project' AND stream_id = ?2
                  ),
                  'project.deleted', ?3, NULL, ?4, NULL, 'server', ?5, ?6, 2
                )
                ",
                params![
                    deleted_id,
                    project.project_id,
                    deleted_at,
                    created_event_id,
                    deleted_payload_json,
                    metadata_json,
                ],
            )
            .map_err(|err| PersistenceError::sql("ApplicationEventSource.projectDeleted", &err))?;
        }
    }

    conn.execute_batch(
        "
        INSERT INTO orchestration_command_receipts (
          command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence,
          status, error, command_type
        )
        SELECT
          command_id,
          'thread',
          thread_id,
          accepted_at,
          COALESCE(
            (
              SELECT MAX(events.sequence)
              FROM orchestration_events events
              WHERE events.application_event_version = 2
                AND events.command_id = receipts.command_id
            ),
            0
          ),
          status,
          error,
          command_type
        FROM orchestration_v2_command_receipts receipts
        WHERE TRUE
        ON CONFLICT(command_id) DO NOTHING
        ",
    )
    .map_err(|err| PersistenceError::sql("ApplicationEventSource.receipts", &err))?;
    Ok(())
}

fn metadata_json(event: &LegacyV2Event) -> Result<String, PersistenceError> {
    let mut meta = serde_json::Map::new();
    meta.insert("applicationEventVersion".into(), serde_json::json!(2));
    if let Some(run_id) = &event.run_id {
        meta.insert("runId".into(), serde_json::Value::String(run_id.clone()));
    }
    if let Some(node_id) = &event.node_id {
        meta.insert("nodeId".into(), serde_json::Value::String(node_id.clone()));
    }
    if let Some(driver) = &event.driver {
        meta.insert("driver".into(), serde_json::Value::String(driver.clone()));
    }
    if let Some(instance) = &event.provider_instance_id {
        meta.insert(
            "providerInstanceId".into(),
            serde_json::Value::String(instance.clone()),
        );
    }
    if let Some(raw) = &event.raw_event_id {
        meta.insert("rawEventId".into(), serde_json::Value::String(raw.clone()));
    }
    serde_json::to_string(&serde_json::Value::Object(meta)).map_err(|err| {
        PersistenceError::Decode(crate::errors::PersistenceDecodeError::new(
            "ApplicationEventSource.metadata",
            err.to_string(),
        ))
    })
}

fn legacy_events(conn: &Connection) -> Result<Vec<LegacyV2Event>, PersistenceError> {
    let mut stmt = conn
        .prepare(
            "
            SELECT event_id, command_id, thread_id, run_id, node_id, driver,
                   provider_instance_id, raw_event_id, event_type, occurred_at, payload_json
            FROM orchestration_v2_events
            ORDER BY sequence ASC
            ",
        )
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.legacyEvents", &err))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(LegacyV2Event {
                event_id: row.get(0)?,
                command_id: row.get(1)?,
                thread_id: row.get(2)?,
                run_id: row.get(3)?,
                node_id: row.get(4)?,
                driver: row.get(5)?,
                provider_instance_id: row.get(6)?,
                raw_event_id: row.get(7)?,
                event_type: row.get(8)?,
                occurred_at: row.get(9)?,
                payload_json: row.get(10)?,
            })
        })
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.legacyEvents", &err))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.legacyEvents", &err))
}

fn project_rows(conn: &Connection) -> Result<Vec<ProjectRow>, PersistenceError> {
    let exists = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projection_projects'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.projects", &err))?
        .unwrap_or(0);
    if exists == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "
            SELECT project_id, title, workspace_root, default_model_selection_json,
                   scripts_json, created_at, updated_at, deleted_at
            FROM projection_projects
            ORDER BY created_at ASC, project_id ASC
            ",
        )
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.projects", &err))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProjectRow {
                project_id: row.get(0)?,
                title: row.get(1)?,
                workspace_root: row.get(2)?,
                default_model_selection_json: row.get(3)?,
                scripts_json: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                deleted_at: row.get(7)?,
            })
        })
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.projects", &err))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql("ApplicationEventSource.projects", &err))
}
