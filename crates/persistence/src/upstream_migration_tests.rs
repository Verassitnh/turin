//! Ports of upstream/apps/server/src/persistence/Migrations/*.test.ts
//! and the pure SQL cases from upstream/apps/server/src/persistence/Errors.test.ts.
//!
//! Effect Schema diagnostics stay the existing Errors.ts deviation: decode
//! failures carry the serde error string, so the two schema cases are not
//! reimplemented here.

use rusqlite::Connection;
use serde_json::{Value, json};

use super::*;
use crate::errors::{sql_error, sqlite_failure_detail};
use crate::migration_manifest::MIGRATIONS;

fn memory() -> Connection {
    Connection::open_in_memory().unwrap()
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

fn exec(conn: &Connection, sql: &str) {
    conn.execute_batch(sql).unwrap();
}

fn column_names(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get(1))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn column_notnull(conn: &Connection, table: &str, column: &str) -> i64 {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?))
    })
    .unwrap()
    .map(|row| row.unwrap())
    .find(|(name, _)| name == column)
    .map(|(_, notnull)| notnull)
    .unwrap_or_else(|| panic!("{table}.{column} missing"))
}

fn index_names(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_list({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get(1))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn index_columns(conn: &Connection, index: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_info('{index}')"))
        .unwrap();
    stmt.query_map([], |row| row.get(2))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn strings(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn text_pairs(conn: &Connection, sql: &str) -> Vec<(String, String)> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn opt_text_pairs(conn: &Connection, sql: &str) -> Vec<(String, Option<String>)> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn payloads(conn: &Connection, sql: &str) -> Vec<Value> {
    strings(conn, sql)
        .into_iter()
        .map(|text| serde_json::from_str(&text).unwrap())
        .collect()
}

fn history_from(conn: &Connection, id: i64) -> Vec<(i64, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT migration_id, name FROM effect_sql_migrations
             WHERE migration_id >= ?1 ORDER BY migration_id",
        )
        .unwrap();
    stmt.query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[test]
fn errors_keep_sql_operation_context_without_a_tautological_detail() {
    let err = rusqlite::Error::InvalidParameterName(":missing".into());
    let error = sql_error("AuthSessionRepository.list:query", &err);
    assert_eq!(error.operation, "AuthSessionRepository.list:query");
    assert_eq!(error.detail, None);
    assert_eq!(
        error.message(),
        "SQL error in AuthSessionRepository.list:query"
    );
    assert_eq!(sqlite_failure_detail(&err), None);
}

#[test]
fn errors_name_a_real_sqlite_condition_without_copying_query_data() {
    let conn = memory();
    let payload = "sql-private-sentinel";
    conn.execute(
        "CREATE TABLE error_test (private_column TEXT PRIMARY KEY)",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO error_test VALUES (?1)", [payload])
        .unwrap();
    let cause = conn
        .execute("INSERT INTO error_test VALUES (?1)", [payload])
        .unwrap_err();
    let error = sql_error("OrchestrationCommandReceiptRepository.upsert:query", &cause);
    assert_eq!(
        error.detail.as_deref(),
        Some("SQLITE(1555) constraint failed")
    );
    let message = error.message();
    assert!(!message.contains(payload));
    assert!(!message.contains("private_column"));
}

#[test]
fn errors_read_a_sqlite_failure_code() {
    // rusqlite reports `SqliteFailure` on the error itself. Upstream's wrapping
    // `Error.cause.errcode` walk is the same detail once that code is visible.
    let err = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: rusqlite::ffi::ErrorCode::DatabaseLocked,
            extended_code: 5,
        },
        Some("database is locked".into()),
    );
    assert_eq!(
        sqlite_failure_detail(&err).as_deref(),
        Some("SQLITE(5) database is locked")
    );
}

#[test]
fn errors_omit_a_detail_for_a_cause_they_cannot_categorize() {
    let causes = [
        rusqlite::Error::InvalidParameterName("unhelpful".into()),
        rusqlite::Error::ExecuteReturnedResults,
    ];
    for cause in causes {
        let error = sql_error("AuthSessionRepository.list:query", &cause);
        assert_eq!(error.detail, None);
        assert_eq!(
            error.message(),
            "SQL error in AuthSessionRepository.list:query"
        );
    }
}

#[test]
fn migration_016_canonicalizes_legacy_model_selections() {
    let conn = memory();
    migrate_to(&conn, 15);
    exec(
        &conn,
        "
        INSERT INTO projection_projects (
          project_id, title, workspace_root, default_model, scripts_json,
          created_at, updated_at, deleted_at
        ) VALUES
          ('project-codex', 'Codex project', '/tmp/project-codex', 'gpt-5.4', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL),
          ('project-claude', 'Claude project', '/tmp/project-claude', 'claude-sonnet-4-6', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL),
          ('project-null', 'Null project', '/tmp/project-null', NULL, '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL);
        UPDATE projection_projects SET default_model = 'claude-opus-4-6' WHERE project_id = 'project-claude';
        INSERT INTO projection_threads (
          thread_id, project_id, title, model, branch, worktree_path, latest_turn_id,
          created_at, updated_at, deleted_at, runtime_mode, interaction_mode
        ) VALUES
          ('thread-session', 'project-codex', 'Session thread', 'gpt-5.4', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, 'full-access', 'default'),
          ('thread-claude', 'project-claude', 'Claude thread', 'claude-opus-4-6', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, 'full-access', 'default'),
          ('thread-codex', 'project-codex', 'Codex thread', 'gpt-5.4', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, 'full-access', 'default'),
          ('thread-legacy-options', 'project-claude', 'Legacy options thread', 'claude-opus-4-6', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, 'full-access', 'default');
        INSERT INTO projection_thread_sessions (
          thread_id, status, provider_name, provider_session_id, provider_thread_id,
          active_turn_id, last_error, updated_at, runtime_mode
        ) VALUES (
          'thread-session', 'running', 'claudeAgent', 'provider-session-1', 'provider-thread-1',
          NULL, NULL, '2026-01-01T00:00:00.000Z', 'full-access'
        );
        ",
    );
    exec(
        &conn,
        r#"
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at,
          command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json
        ) VALUES
        ('event-project-created', 'project', 'project-1', 1, 'project.created', '2026-01-01T00:00:00.000Z', 'command-project-created', NULL, 'correlation-project-created', 'user', '{"projectId":"project-1","title":"Project","workspaceRoot":"/tmp/project","defaultModel":"claude-opus-4-6","defaultModelOptions":{"codex":{"reasoningEffort":"high"},"claudeAgent":{"effort":"max"}},"scripts":[],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
        ('event-project-created-fallback', 'project', 'project-2', 1, 'project.created', '2026-01-01T00:00:00.000Z', 'command-project-created-fallback', NULL, 'correlation-project-created-fallback', 'user', '{"projectId":"project-2","title":"Fallback Project","workspaceRoot":"/tmp/project-2","defaultModel":"claude-opus-4-6","defaultModelOptions":{"codex":{"reasoningEffort":"low"}},"scripts":[],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
        ('event-project-created-null-model', 'project', 'project-3', 1, 'project.created', '2026-01-01T00:00:00.000Z', 'command-project-created-null-model', NULL, 'correlation-project-created-null-model', 'user', '{"projectId":"project-3","title":"Null Model Project","workspaceRoot":"/tmp/project-3","defaultModel":null,"scripts":[],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
        ('event-thread-created', 'thread', 'thread-1', 1, 'thread.created', '2026-01-01T00:00:00.000Z', 'command-thread-created', NULL, 'correlation-thread-created', 'user', '{"threadId":"thread-1","projectId":"project-1","title":"Thread","model":"claude-opus-4-6","modelOptions":{"codex":{"reasoningEffort":"high"},"claudeAgent":{"effort":"max","thinking":false}},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
        ('event-thread-created-fallback', 'thread', 'thread-2', 1, 'thread.created', '2026-01-01T00:00:00.000Z', 'command-thread-created-fallback', NULL, 'correlation-thread-created-fallback', 'user', '{"threadId":"thread-2","projectId":"project-1","title":"Fallback Thread","model":"gpt-5.4","modelOptions":{"claudeAgent":{"effort":"max"}},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
        ('event-turn-start-requested', 'thread', 'thread-1', 2, 'thread.turn-start-requested', '2026-01-01T00:00:00.000Z', 'command-turn-start-requested', NULL, 'correlation-turn-start-requested', 'user', '{"threadId":"thread-1","turnId":"turn-1","input":"hi","model":"gpt-5.4","modelOptions":{"codex":{"fastMode":true},"claudeAgent":{"effort":"max"}},"deliveryMode":"buffered"}', '{}'),
        ('event-thread-created-no-model', 'thread', 'thread-3', 1, 'thread.created', '2026-01-01T00:00:00.000Z', 'command-thread-created-no-model', NULL, 'correlation-thread-created-no-model', 'user', '{"threadId":"thread-3","projectId":"project-1","title":"Ancient Thread","runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}');
        "#,
    );
    migrate_to(&conn, 16);

    assert_eq!(
        opt_text_pairs(
            &conn,
            "SELECT project_id, default_model_selection_json FROM projection_projects ORDER BY project_id",
        ),
        vec![
            (
                "project-claude".into(),
                Some(r#"{"provider":"claudeAgent","model":"claude-opus-4-6"}"#.into()),
            ),
            (
                "project-codex".into(),
                Some(r#"{"provider":"codex","model":"gpt-5.4"}"#.into()),
            ),
            ("project-null".into(), None),
        ]
    );
    assert_eq!(
        opt_text_pairs(
            &conn,
            "SELECT thread_id, model_selection_json FROM projection_threads ORDER BY thread_id",
        ),
        vec![
            (
                "thread-claude".into(),
                Some(r#"{"provider":"claudeAgent","model":"claude-opus-4-6"}"#.into()),
            ),
            (
                "thread-codex".into(),
                Some(r#"{"provider":"codex","model":"gpt-5.4"}"#.into()),
            ),
            (
                "thread-legacy-options".into(),
                Some(r#"{"provider":"claudeAgent","model":"claude-opus-4-6"}"#.into()),
            ),
            (
                "thread-session".into(),
                Some(r#"{"provider":"claudeAgent","model":"gpt-5.4"}"#.into()),
            ),
        ]
    );
    let events = payloads(
        &conn,
        "SELECT payload_json FROM orchestration_events ORDER BY rowid ASC",
    );
    assert_eq!(
        events[0],
        json!({
            "projectId": "project-1",
            "title": "Project",
            "workspaceRoot": "/tmp/project",
            "defaultModelSelection": {
                "provider": "claudeAgent",
                "model": "claude-opus-4-6",
                "options": { "effort": "max" }
            },
            "scripts": [],
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z"
        })
    );
    assert_eq!(
        events[1]["defaultModelSelection"],
        json!({
            "provider": "claudeAgent",
            "model": "claude-opus-4-6",
            "options": { "reasoningEffort": "low" }
        })
    );
    assert_eq!(events[2]["defaultModelSelection"], Value::Null);
    assert_eq!(
        events[3]["modelSelection"],
        json!({
            "provider": "claudeAgent",
            "model": "claude-opus-4-6",
            "options": { "effort": "max", "thinking": false }
        })
    );
    assert_eq!(
        events[4]["modelSelection"],
        json!({
            "provider": "codex",
            "model": "gpt-5.4",
            "options": { "effort": "max" }
        })
    );
    assert_eq!(
        events[5]["modelSelection"],
        json!({
            "provider": "codex",
            "model": "gpt-5.4",
            "options": { "fastMode": true }
        })
    );
    assert_eq!(
        events[6]["modelSelection"],
        json!({ "provider": "codex", "model": "gpt-5.4" })
    );
}

#[test]
fn migration_019_creates_snapshot_lookup_indexes() {
    let conn = memory();
    migrate_to(&conn, 18);
    migrate_to(&conn, 19);
    assert!(
        index_names(&conn, "projection_projects")
            .contains(&"idx_projection_projects_workspace_root_deleted_at".into())
    );
    assert_eq!(
        index_columns(&conn, "idx_projection_projects_workspace_root_deleted_at"),
        ["workspace_root", "deleted_at"]
    );
    assert!(
        index_names(&conn, "projection_threads")
            .contains(&"idx_projection_threads_project_deleted_created".into())
    );
    assert_eq!(
        index_columns(&conn, "idx_projection_threads_project_deleted_created"),
        ["project_id", "deleted_at", "created_at"]
    );
}

#[test]
fn migration_024_backfills_thread_shell_summary() {
    let conn = memory();
    migrate_to(&conn, 23);
    exec(
        &conn,
        r#"
        INSERT INTO projection_threads (
          thread_id, project_id, title, model_selection_json, runtime_mode, interaction_mode,
          branch, worktree_path, latest_turn_id, created_at, updated_at, archived_at,
          latest_user_message_at, pending_approval_count, pending_user_input_count,
          has_actionable_proposed_plan, deleted_at
        ) VALUES (
          'thread-1', 'project-1', 'Thread 1', '{"provider":"codex","model":"gpt-5-codex"}',
          'approval-required', 'plan', NULL, NULL, 'turn-1',
          '2026-02-24T00:00:00.000Z', '2026-02-24T00:00:00.000Z', NULL, NULL, 0, 0, 0, NULL
        );
        INSERT INTO projection_thread_messages (
          message_id, thread_id, turn_id, role, text, attachments_json, is_streaming, created_at, updated_at
        ) VALUES (
          'message-user-1', 'thread-1', 'turn-1', 'user', 'Need help', NULL, 0,
          '2026-02-24T00:01:00.000Z', '2026-02-24T00:01:00.000Z'
        );
        INSERT INTO projection_thread_activities (
          activity_id, thread_id, turn_id, tone, kind, summary, payload_json, sequence, created_at
        ) VALUES
          ('activity-approval-requested', 'thread-1', 'turn-1', 'approval', 'approval.requested', 'Command approval requested', '{"requestId":"approval-1","requestKind":"command"}', NULL, '2026-02-24T00:02:00.000Z'),
          ('activity-approval-stale', 'thread-1', 'turn-1', 'error', 'provider.approval.respond.failed', 'Provider approval response failed', '{"requestId":"approval-1","detail":"Unknown pending permission request: approval-1"}', NULL, '2026-02-24T00:03:00.000Z'),
          ('activity-user-input-requested', 'thread-1', 'turn-1', 'info', 'user-input.requested', 'User input requested', '{"requestId":"input-1","questions":[{"id":"area","header":"Area","question":"Which repo area should I inspect next?","options":[{"label":"Server","description":"Server orchestration."}]}]}', NULL, '2026-02-24T00:04:00.000Z');
        INSERT INTO projection_thread_proposed_plans (
          plan_id, thread_id, turn_id, plan_markdown, implemented_at, implementation_thread_id, created_at, updated_at
        ) VALUES (
          'plan-1', 'thread-1', 'turn-1', '# Do the thing', NULL, NULL,
          '2026-02-24T00:05:00.000Z', '2026-02-24T00:05:00.000Z'
        );
        INSERT INTO projection_pending_approvals (
          request_id, thread_id, turn_id, status, decision, created_at, resolved_at
        ) VALUES (
          'approval-1', 'thread-1', 'turn-1', 'pending', NULL, '2026-02-24T00:02:00.000Z', NULL
        );
        "#,
    );
    migrate_to(&conn, 24);
    let row: (Option<String>, i64, i64, i64) = conn
        .query_row(
            "SELECT latest_user_message_at, pending_approval_count, pending_user_input_count, has_actionable_proposed_plan FROM projection_threads WHERE thread_id = 'thread-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, (Some("2026-02-24T00:01:00.000Z".into()), 0, 1, 1));
    let approval: (String, Option<String>) = conn
        .query_row(
            "SELECT status, resolved_at FROM projection_pending_approvals WHERE request_id = 'approval-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        approval,
        ("resolved".into(), Some("2026-02-24T00:03:00.000Z".into()))
    );
}

#[test]
fn migration_025_removes_pending_approvals_that_are_not_approval_requests() {
    let conn = memory();
    migrate_to(&conn, 24);
    exec(
        &conn,
        r#"
        INSERT INTO projection_threads (
          thread_id, project_id, title, model_selection_json, runtime_mode, interaction_mode,
          branch, worktree_path, latest_turn_id, created_at, updated_at, archived_at,
          latest_user_message_at, pending_approval_count, pending_user_input_count,
          has_actionable_proposed_plan, deleted_at
        ) VALUES
          ('thread-valid', 'project-1', 'Valid thread', '{"provider":"codex","model":"gpt-5-codex"}', 'approval-required', 'default', NULL, NULL, 'turn-valid', '2026-04-13T00:00:00.000Z', '2026-04-13T00:00:00.000Z', NULL, NULL, 2, 0, 0, NULL),
          ('thread-invalid', 'project-1', 'Invalid thread', '{"provider":"codex","model":"gpt-5-codex"}', 'approval-required', 'default', NULL, NULL, 'turn-invalid', '2026-04-13T00:00:00.000Z', '2026-04-13T00:00:00.000Z', NULL, NULL, 1, 0, 0, NULL);
        INSERT INTO projection_thread_activities (
          activity_id, thread_id, turn_id, tone, kind, summary, payload_json, sequence, created_at
        ) VALUES
          ('activity-approval-requested', 'thread-valid', 'turn-valid', 'approval', 'approval.requested', 'Command approval requested', '{"requestId":"approval-valid","requestKind":"command"}', NULL, '2026-04-13T00:01:00.000Z'),
          ('activity-user-input-requested', 'thread-invalid', 'turn-invalid', 'info', 'user-input.requested', 'User input requested', '{"requestId":"input-invalid","questions":[{"id":"scope","header":"Scope","question":"What should I inspect?","options":[{"label":"Server","description":"Inspect server code."}]}]}', NULL, '2026-04-13T00:02:00.000Z');
        INSERT INTO projection_pending_approvals (
          request_id, thread_id, turn_id, status, decision, created_at, resolved_at
        ) VALUES
          ('approval-valid', 'thread-valid', 'turn-valid', 'pending', NULL, '2026-04-13T00:01:00.000Z', NULL),
          ('input-invalid', 'thread-invalid', 'turn-invalid', 'pending', NULL, '2026-04-13T00:02:00.000Z', NULL),
          ('input-invalid-resolved', 'thread-valid', 'turn-valid', 'resolved', NULL, '2026-04-13T00:03:00.000Z', '2026-04-13T00:04:00.000Z');
        "#,
    );
    migrate_to(&conn, 25);
    assert_eq!(
        text_pairs(
            &conn,
            "SELECT request_id, status FROM projection_pending_approvals ORDER BY request_id ASC",
        ),
        vec![("approval-valid".into(), "pending".into())]
    );
    let counts: Vec<(String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT thread_id, pending_approval_count FROM projection_threads ORDER BY thread_id ASC")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    assert_eq!(
        counts,
        vec![("thread-invalid".into(), 0), ("thread-valid".into(), 1),]
    );
}

#[test]
fn migration_026_converts_object_options_into_arrays() {
    let conn = memory();
    migrate_to(&conn, 25);
    exec(
        &conn,
        r#"
        INSERT INTO projection_projects (
          project_id, title, workspace_root, default_model_selection_json, scripts_json, created_at, updated_at, deleted_at
        ) VALUES
          ('project-legacy', 'Legacy options project', '/tmp/legacy', '{"provider":"claudeAgent","model":"claude-opus-4-6","options":{"effort":"max","fastMode":true}}', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL),
          ('project-no-options', 'No options project', '/tmp/no-options', '{"provider":"codex","model":"gpt-5.4"}', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL),
          ('project-null-selection', 'Null model selection project', '/tmp/null-selection', NULL, '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL),
          ('project-already-array', 'Already-canonical options project', '/tmp/already-array', '{"provider":"codex","model":"gpt-5.4","options":[{"id":"reasoningEffort","value":"high"}]}', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL);
        INSERT INTO projection_threads (
          thread_id, project_id, title, model_selection_json, branch, worktree_path, latest_turn_id,
          created_at, updated_at, archived_at, latest_user_message_at, pending_approval_count,
          pending_user_input_count, has_actionable_proposed_plan, deleted_at, runtime_mode, interaction_mode
        ) VALUES
          ('thread-legacy', 'project-legacy', 'Legacy thread', '{"provider":"claudeAgent","model":"claude-opus-4-6","options":{"effort":"max","thinking":false,"contextWindow":"1m"}}', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, NULL, 0, 0, 0, NULL, 'full-access', 'default'),
          ('thread-empty-options', 'project-legacy', 'Empty options thread', '{"provider":"codex","model":"gpt-5.4","options":{}}', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, NULL, 0, 0, 0, NULL, 'full-access', 'default'),
          ('thread-drop-garbage', 'project-legacy', 'Thread with non-scalar entries', '{"provider":"claudeAgent","model":"claude-opus-4-6","options":{"effort":"high","thinking":{"enabled":true,"budgetTokens":2000},"emptyStr":"   ","nullish":null}}', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, NULL, 0, 0, 0, NULL, 'full-access', 'default'),
          ('thread-no-options', 'project-legacy', 'No options thread', '{"provider":"codex","model":"gpt-5.4"}', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, NULL, 0, 0, 0, NULL, 'full-access', 'default'),
          ('thread-already-array', 'project-legacy', 'Already array thread', '{"provider":"codex","model":"gpt-5.4","options":[{"id":"fastMode","value":true}]}', NULL, NULL, NULL, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, NULL, 0, 0, 0, NULL, 'full-access', 'default');
        "#,
    );
    exec(
        &conn,
        r#"
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at,
          command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json
        ) VALUES
          ('event-project-created', 'project', 'project-legacy', 1, 'project.created', '2026-01-01T00:00:00.000Z', 'cmd-pc', NULL, 'corr-pc', 'user', '{"projectId":"project-legacy","title":"Project","workspaceRoot":"/tmp/legacy","defaultModelSelection":{"provider":"claudeAgent","model":"claude-opus-4-6","options":{"effort":"max","fastMode":true}},"scripts":[],"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
          ('event-project-meta-updated', 'project', 'project-legacy', 2, 'project.meta-updated', '2026-01-01T00:00:00.000Z', 'cmd-pmu', NULL, 'corr-pmu', 'user', '{"projectId":"project-legacy","defaultModelSelection":{"provider":"codex","model":"gpt-5.4","options":{"reasoningEffort":"low"}},"updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
          ('event-project-null-selection', 'project', 'project-legacy', 3, 'project.meta-updated', '2026-01-01T00:00:00.000Z', 'cmd-null', NULL, 'corr-null', 'user', '{"projectId":"project-legacy","defaultModelSelection":null,"updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
          ('event-thread-created', 'thread', 'thread-legacy', 1, 'thread.created', '2026-01-01T00:00:00.000Z', 'cmd-tc', NULL, 'corr-tc', 'user', '{"threadId":"thread-legacy","projectId":"project-legacy","title":"Thread","modelSelection":{"provider":"claudeAgent","model":"claude-opus-4-6","options":{"effort":"max","thinking":false}},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
          ('event-thread-meta-updated', 'thread', 'thread-legacy', 2, 'thread.meta-updated', '2026-01-01T00:00:00.000Z', 'cmd-tmu', NULL, 'corr-tmu', 'user', '{"threadId":"thread-legacy","modelSelection":{"provider":"codex","model":"gpt-5.4","options":{"fastMode":true}},"updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
          ('event-thread-turn-start', 'thread', 'thread-legacy', 3, 'thread.turn-start-requested', '2026-01-01T00:00:00.000Z', 'cmd-tts', NULL, 'corr-tts', 'user', '{"threadId":"thread-legacy","messageId":"msg-1","modelSelection":{"provider":"claudeAgent","model":"claude-opus-4-6","options":{"effort":"high","contextWindow":"1m"}},"runtimeMode":"full-access","interactionMode":"default","createdAt":"2026-01-01T00:00:00.000Z"}', '{}'),
          ('event-thread-already-array', 'thread', 'thread-legacy', 4, 'thread.created', '2026-01-01T00:00:00.000Z', 'cmd-taa', NULL, 'corr-taa', 'user', '{"threadId":"thread-already-array","projectId":"project-legacy","title":"Already Array","modelSelection":{"provider":"codex","model":"gpt-5.4","options":[{"id":"reasoningEffort","value":"medium"}]},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}', '{}'),
          ('event-activity-append', 'thread', 'thread-legacy', 5, 'thread.activity-appended', '2026-01-01T00:00:00.000Z', 'cmd-aa', NULL, 'corr-aa', 'user', '{"threadId":"thread-legacy","activity":{"id":"a","tone":"info","kind":"k","summary":"s","payload":null,"turnId":null,"createdAt":"2026-01-01T00:00:00.000Z"}}', '{}');
        "#,
    );
    migrate_to(&conn, 26);
    let projects = payloads(
        &conn,
        "SELECT COALESCE(default_model_selection_json, 'null') FROM projection_projects ORDER BY project_id",
    );
    assert_eq!(
        projects,
        vec![
            json!({"provider":"codex","model":"gpt-5.4","options":[{"id":"reasoningEffort","value":"high"}]}),
            json!({"provider":"claudeAgent","model":"claude-opus-4-6","options":[{"id":"effort","value":"max"},{"id":"fastMode","value":true}]}),
            json!({"provider":"codex","model":"gpt-5.4"}),
            Value::Null,
        ]
    );
    let threads = payloads(
        &conn,
        "SELECT model_selection_json FROM projection_threads ORDER BY thread_id",
    );
    assert_eq!(
        threads[0],
        json!({"provider":"codex","model":"gpt-5.4","options":[{"id":"fastMode","value":true}]})
    );
    assert_eq!(
        threads[1]["options"],
        json!([{"id":"effort","value":"high"}])
    );
    assert_eq!(threads[2]["options"], json!([]));
    assert_eq!(
        threads[3]["options"],
        json!([
            {"id":"effort","value":"max"},
            {"id":"thinking","value":false},
            {"id":"contextWindow","value":"1m"}
        ])
    );
    assert!(threads[4].get("options").is_none());

    let events = payloads(
        &conn,
        "SELECT payload_json FROM orchestration_events ORDER BY event_id",
    );
    let by_id: std::collections::BTreeMap<String, Value> = strings(
        &conn,
        "SELECT event_id FROM orchestration_events ORDER BY event_id",
    )
    .into_iter()
    .zip(events)
    .collect();
    assert_eq!(
        by_id["event-project-created"]["defaultModelSelection"]["options"],
        json!([{"id":"effort","value":"max"},{"id":"fastMode","value":true}])
    );
    assert_eq!(
        by_id["event-project-meta-updated"]["defaultModelSelection"]["options"],
        json!([{"id":"reasoningEffort","value":"low"}])
    );
    assert_eq!(
        by_id["event-thread-created"]["modelSelection"]["options"],
        json!([{"id":"effort","value":"max"},{"id":"thinking","value":false}])
    );
    assert_eq!(
        by_id["event-thread-meta-updated"]["modelSelection"]["options"],
        json!([{"id":"fastMode","value":true}])
    );
    assert_eq!(
        by_id["event-thread-turn-start"]["modelSelection"]["options"],
        json!([{"id":"effort","value":"high"},{"id":"contextWindow","value":"1m"}])
    );
    assert_eq!(
        by_id["event-project-null-selection"]["defaultModelSelection"],
        Value::Null
    );
    assert!(
        by_id["event-activity-append"]
            .get("modelSelection")
            .is_none()
    );
    assert!(
        by_id["event-activity-append"]
            .get("defaultModelSelection")
            .is_none()
    );
    assert_eq!(
        by_id["event-thread-already-array"]["modelSelection"]["options"],
        json!([{"id":"reasoningEffort","value":"medium"}])
    );
}

#[test]
fn migration_029_creates_thread_detail_ordering_indexes() {
    let conn = memory();
    migrate_to(&conn, 28);
    migrate_to(&conn, 29);
    assert!(
        index_names(&conn, "projection_thread_activities")
            .contains(&"idx_projection_thread_activities_thread_sequence_created_id".into())
    );
    assert_eq!(
        index_columns(
            &conn,
            "idx_projection_thread_activities_thread_sequence_created_id"
        ),
        ["thread_id", "sequence", "created_at", "activity_id"]
    );
    assert!(
        index_names(&conn, "projection_thread_messages")
            .contains(&"idx_projection_thread_messages_thread_created_id".into())
    );
    assert_eq!(
        index_columns(&conn, "idx_projection_thread_messages_thread_created_id"),
        ["thread_id", "created_at", "message_id"]
    );
}

#[test]
fn migration_031_invalidates_role_based_auth_and_installs_scopes() {
    let conn = memory();
    migrate_to(&conn, 30);
    exec(
        &conn,
        "
        INSERT INTO auth_pairing_links (id, credential, method, role, subject, created_at, expires_at)
        VALUES ('link-owner', 'bootstrap-owner', 'desktop-bootstrap', 'owner', 'desktop', '2026-05-29T00:00:00.000Z', '2026-05-29T01:00:00.000Z');
        INSERT INTO auth_sessions (session_id, subject, role, method, issued_at, expires_at)
        VALUES ('session-owner', 'desktop', 'owner', 'browser-session-cookie', '2026-05-29T00:00:00.000Z', '2026-05-29T01:00:00.000Z');
        ",
    );
    migrate_to(&conn, 31);
    let pairing = column_names(&conn, "auth_pairing_links");
    let sessions = column_names(&conn, "auth_sessions");
    assert!(pairing.iter().any(|name| name == "scopes"));
    assert!(!pairing.iter().any(|name| name == "role"));
    assert!(sessions.iter().any(|name| name == "scopes"));
    assert!(!sessions.iter().any(|name| name == "role"));
    assert!(strings(&conn, "SELECT id FROM auth_pairing_links").is_empty());
    assert!(strings(&conn, "SELECT session_id FROM auth_sessions").is_empty());
}

#[test]
fn migration_035_adds_title_regeneration_columns() {
    let conn = memory();
    migrate_to(&conn, 34);
    migrate_to(&conn, 35);
    let names = column_names(&conn, "projection_threads");
    assert!(
        names
            .iter()
            .any(|name| name == "title_regeneration_request_id")
    );
    assert!(
        names
            .iter()
            .any(|name| name == "title_regeneration_started_at")
    );
}

#[test]
fn migration_040_adds_nullable_favicon_path() {
    let conn = memory();
    migrate_to(&conn, 39);
    migrate_to(&conn, 40);
    assert_eq!(
        column_notnull(&conn, "projection_projects", "favicon_path"),
        0
    );
}

#[test]
fn migration_041_adds_nullable_client_connection_columns() {
    let conn = memory();
    migrate_to(&conn, 40);
    migrate_to(&conn, 41);
    assert_eq!(column_notnull(&conn, "auth_sessions", "client_surface"), 0);
    assert_eq!(
        column_notnull(&conn, "auth_sessions", "client_app_version"),
        0
    );
}

#[test]
fn migration_042_adds_linked_pull_request_column() {
    let conn = memory();
    migrate_to(&conn, 41);
    migrate_to(&conn, 42);
    assert!(
        column_names(&conn, "projection_threads")
            .iter()
            .any(|name| name == "linked_pull_request_json")
    );
}

#[test]
fn migration_044_clears_create_time_seeds_and_preserves_explicit_defaults() {
    let conn = memory();
    migrate_to(&conn, 43);
    exec(
        &conn,
        r#"
        INSERT INTO projection_projects (
          project_id, title, workspace_root, default_model_selection_json, default_thread_env_mode,
          favicon_path, scripts_json, created_at, updated_at, deleted_at
        ) VALUES
          ('project-auto', 'Auto', '/tmp/auto', '{"instanceId":"codex","model":"gpt-5.6-sol"}', NULL, NULL, '[]', '2026-08-01T00:00:00.000Z', '2026-08-01T00:00:00.000Z', NULL),
          ('project-title-only', 'Title only', '/tmp/title-only', '{"instanceId":"codex","model":"gpt-5.6-sol"}', NULL, NULL, '[]', '2026-08-01T00:00:00.000Z', '2026-08-02T00:00:00.000Z', NULL),
          ('project-explicit', 'Explicit', '/tmp/explicit', '{"instanceId":"codex","model":"gpt-5.6-sol","options":[{"id":"reasoningEffort","value":"high"}]}', NULL, NULL, '[]', '2026-08-01T00:00:00.000Z', '2026-08-02T00:00:00.000Z', NULL);
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at,
          command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json
        ) VALUES
          ('event-auto-create', 'project', 'project-auto', 0, 'project.created', '2026-08-01T00:00:00.000Z', 'command-auto-create', NULL, 'command-auto-create', 'client', '{"defaultModelSelection":{"instanceId":"codex","model":"gpt-5.6-sol"}}', '{}'),
          ('event-title-create', 'project', 'project-title-only', 0, 'project.created', '2026-08-01T00:00:00.000Z', 'command-title-create', NULL, 'command-title-create', 'client', '{"defaultModelSelection":{"instanceId":"codex","model":"gpt-5.6-sol"}}', '{}'),
          ('event-title-update', 'project', 'project-title-only', 1, 'project.meta-updated', '2026-08-02T00:00:00.000Z', 'command-title-update', NULL, 'command-title-update', 'client', '{"title":"Renamed"}', '{}'),
          ('event-explicit-create', 'project', 'project-explicit', 0, 'project.created', '2026-08-01T00:00:00.000Z', 'command-explicit-create', NULL, 'command-explicit-create', 'client', '{"defaultModelSelection":{"instanceId":"codex","model":"gpt-5.6-sol"}}', '{}'),
          ('event-explicit-update', 'project', 'project-explicit', 1, 'project.meta-updated', '2026-08-02T00:00:00.000Z', 'command-explicit-update', NULL, 'command-explicit-update', 'client', '{"defaultModelSelection":{"instanceId":"codex","model":"gpt-5.6-sol","options":[{"id":"reasoningEffort","value":"high"}]}}', '{}');
        "#,
    );
    migrate_to(&conn, 44);
    assert_eq!(
        opt_text_pairs(
            &conn,
            "SELECT project_id, default_model_selection_json FROM projection_projects ORDER BY project_id",
        ),
        vec![
            ("project-auto".into(), None),
            (
                "project-explicit".into(),
                Some(
                    r#"{"instanceId":"codex","model":"gpt-5.6-sol","options":[{"id":"reasoningEffort","value":"high"}]}"#.into(),
                ),
            ),
            ("project-title-only".into(), None),
        ]
    );
    let models: Vec<(String, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT stream_id, json_extract(payload_json, '$.defaultModelSelection.model')
                 FROM orchestration_events WHERE event_type = 'project.created' ORDER BY stream_id",
            )
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    assert_eq!(
        models,
        vec![
            ("project-auto".into(), None),
            ("project-explicit".into(), Some("gpt-5.6-sol".into())),
            ("project-title-only".into(), None),
        ]
    );
}

#[test]
fn migration_046_repairs_automatic_settlement_and_leaves_manual_alone() {
    let conn = memory();
    migrate_to(&conn, 45);
    let model = r#"{"instanceId":"codex","model":"gpt-5.6-sol"}"#;
    conn.execute_batch(&format!(
        "
        INSERT INTO projection_threads (
          thread_id, project_id, title, model_selection_json, latest_turn_id, created_at, updated_at,
          latest_user_message_at, settled_override, settled_at, deleted_at
        ) VALUES
          ('thread-auto', 'project-1', 'Automatic', '{model}', 'turn-auto', '2026-05-01T00:00:00.000Z', '2026-09-01T00:00:00.000Z', '2026-06-01T00:00:00.000Z', 'settled', '2026-09-01T00:00:00.000Z', NULL),
          ('thread-auto-no-activity', 'project-1', 'Automatic without activity', '{model}', NULL, '2026-05-02T00:00:00.000Z', '2026-09-01T00:00:00.000Z', NULL, 'settled', '2026-09-01T00:00:00.000Z', NULL),
          ('thread-auto-later-activity', 'project-1', 'Automatic then active', '{model}', NULL, '2026-05-03T00:00:00.000Z', '2026-09-03T00:00:00.000Z', '2026-09-03T00:00:00.000Z', 'settled', '2026-09-01T00:00:00.000Z', NULL),
          ('thread-manual', 'project-1', 'Manual', '{model}', NULL, '2026-05-01T00:00:00.000Z', '2026-08-10T00:00:00.000Z', '2026-06-10T00:00:00.000Z', 'settled', '2026-08-10T00:00:00.000Z', NULL),
          ('thread-resettled', 'project-1', 'Manually re-settled', '{model}', NULL, '2026-05-01T00:00:00.000Z', '2026-09-02T00:00:00.000Z', '2026-06-05T00:00:00.000Z', 'settled', '2026-09-02T00:00:00.000Z', NULL);
        INSERT INTO projection_thread_messages (
          message_id, thread_id, turn_id, role, text, is_streaming, created_at, updated_at
        ) VALUES
          ('message-auto', 'thread-auto', 'turn-auto', 'user', 'Prompt', 0, '2026-06-01T00:00:00.000Z', '2026-06-01T00:00:00.000Z'),
          ('message-later-old', 'thread-auto-later-activity', NULL, 'user', 'Prompt', 0, '2026-06-20T00:00:00.000Z', '2026-06-20T00:00:00.000Z'),
          ('message-later-new', 'thread-auto-later-activity', NULL, 'user', 'Prompt', 0, '2026-09-03T00:00:00.000Z', '2026-09-03T00:00:00.000Z'),
          ('message-manual', 'thread-manual', NULL, 'user', 'Prompt', 0, '2026-06-10T00:00:00.000Z', '2026-06-10T00:00:00.000Z'),
          ('message-resettled', 'thread-resettled', NULL, 'user', 'Prompt', 0, '2026-06-05T00:00:00.000Z', '2026-06-05T00:00:00.000Z');
        INSERT INTO projection_turns (
          thread_id, turn_id, state, requested_at, started_at, completed_at, checkpoint_files_json
        ) VALUES (
          'thread-auto', 'turn-auto', 'completed', '2026-06-02T00:00:00.000Z', '2026-06-02T00:01:00.000Z', '2026-06-03T00:00:00.000Z', '[]'
        );
        "
    ))
    .unwrap();
    let swept = "2026-09-01T00:00:00.000Z";
    for (event_id, thread_id, version, occurred_at, command_id, settled_at) in [
        (
            "event-auto",
            "thread-auto",
            0,
            swept,
            "server:auto-settle:thread-auto:uuid",
            swept,
        ),
        (
            "event-auto-repeat",
            "thread-auto",
            1,
            "2026-09-01T00:00:05.000Z",
            "command-repeat",
            swept,
        ),
        (
            "event-auto-no-activity",
            "thread-auto-no-activity",
            0,
            swept,
            "server:auto-settle:thread-auto-no-activity:uuid",
            swept,
        ),
        (
            "event-auto-later-activity",
            "thread-auto-later-activity",
            0,
            swept,
            "server:auto-settle:thread-auto-later-activity:uuid",
            swept,
        ),
        (
            "event-manual",
            "thread-manual",
            0,
            "2026-08-10T00:00:00.000Z",
            "command-manual",
            "2026-08-10T00:00:00.000Z",
        ),
        (
            "event-resettled-auto",
            "thread-resettled",
            0,
            swept,
            "server:auto-settle:thread-resettled:uuid",
            swept,
        ),
        (
            "event-resettled-manual",
            "thread-resettled",
            1,
            "2026-09-02T00:00:00.000Z",
            "command-resettled-manual",
            "2026-09-02T00:00:00.000Z",
        ),
    ] {
        let actor = if command_id.starts_with("server:") {
            "server"
        } else {
            "client"
        };
        let payload = serde_json::json!({
            "threadId": thread_id,
            "settledAt": settled_at,
            "updatedAt": occurred_at,
        })
        .to_string();
        conn.execute(
            "INSERT INTO orchestration_events (
               event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at,
               command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json
             ) VALUES (?1, 'thread', ?2, ?3, 'thread.settled', ?4, ?5, NULL, ?5, ?6, ?7, '{}')",
            rusqlite::params![event_id, thread_id, version, occurred_at, command_id, actor, payload],
        )
        .unwrap();
    }
    let before = strings(
        &conn,
        "SELECT payload_json FROM orchestration_events ORDER BY event_id",
    );
    migrate_to(&conn, 46);
    let threads: Vec<(String, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT thread_id, settled_at, updated_at FROM projection_threads ORDER BY thread_id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    assert_eq!(
        threads,
        vec![
            (
                "thread-auto".into(),
                "2026-06-03T00:00:00.000Z".into(),
                "2026-09-01T00:00:00.000Z".into()
            ),
            (
                "thread-auto-later-activity".into(),
                "2026-06-20T00:00:00.000Z".into(),
                "2026-09-03T00:00:00.000Z".into()
            ),
            (
                "thread-auto-no-activity".into(),
                "2026-05-02T00:00:00.000Z".into(),
                "2026-09-01T00:00:00.000Z".into()
            ),
            (
                "thread-manual".into(),
                "2026-08-10T00:00:00.000Z".into(),
                "2026-08-10T00:00:00.000Z".into()
            ),
            (
                "thread-resettled".into(),
                "2026-09-02T00:00:00.000Z".into(),
                "2026-09-02T00:00:00.000Z".into()
            ),
        ]
    );
    assert_eq!(
        strings(
            &conn,
            "SELECT payload_json FROM orchestration_events ORDER BY event_id"
        ),
        before
    );
}

#[test]
fn migration_047_adds_nullable_project_icon() {
    let conn = memory();
    migrate_to(&conn, 46);
    migrate_to(&conn, 47);
    assert_eq!(
        column_notnull(&conn, "projection_projects", "project_icon_json"),
        0
    );
}

#[test]
fn migration_050_backfills_legacy_single_links() {
    let conn = memory();
    migrate_to(&conn, 49);
    exec(
        &conn,
        r#"
        INSERT INTO projection_projects (
          project_id, title, workspace_root, scripts_json, created_at, updated_at, deleted_at
        ) VALUES (
          'project-1', 'Project 1', '/tmp/project-1', '[]',
          '2026-03-01T00:00:00.000Z', '2026-03-01T00:00:00.000Z', NULL
        );
        INSERT INTO projection_threads (
          thread_id, project_id, title, model_selection_json, linked_pull_request_json, created_at, updated_at
        ) VALUES
          ('thread-github', 'project-1', 'GitHub link', '{"instanceId":"codex","model":"gpt-5.4"}', '{"projectId":"project-1","repository":"PingDotGG/T3Code","number":42,"url":"https://GitHub.com/pingdotgg/t3code/pull/42"}', '2026-03-01T00:00:01.000Z', '2026-03-02T00:00:00.000Z'),
          ('thread-bad-url', 'project-1', 'Unparseable URL', '{"instanceId":"codex","model":"gpt-5.4"}', '{"projectId":"project-1","repository":"acme/widgets","number":7,"url":"not a url"}', '2026-03-01T00:00:02.000Z', '2026-03-03T00:00:00.000Z'),
          ('thread-malformed', 'project-1', 'Malformed JSON', '{"instanceId":"codex","model":"gpt-5.4"}', '{"repository":"acme/widgets"}', '2026-03-01T00:00:03.000Z', '2026-03-04T00:00:00.000Z'),
          ('thread-unlinked', 'project-1', 'No link', '{"instanceId":"codex","model":"gpt-5.4"}', NULL, '2026-03-01T00:00:04.000Z', '2026-03-05T00:00:00.000Z');
        "#,
    );
    migrate_to(&conn, 50);
    let rows: Vec<(String, String, String, i64, String, String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, host, repository, number, url, source, linked_at
                 FROM projection_thread_pull_requests ORDER BY thread_id ASC",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
    };
    assert_eq!(
        rows,
        vec![
            (
                "thread-bad-url".into(),
                "unknown".into(),
                "acme/widgets".into(),
                7,
                "not a url".into(),
                "manual".into(),
                "2026-03-03T00:00:00.000Z".into(),
            ),
            (
                "thread-github".into(),
                "github.com".into(),
                "pingdotgg/t3code".into(),
                42,
                "https://GitHub.com/pingdotgg/t3code/pull/42".into(),
                "manual".into(),
                "2026-03-02T00:00:00.000Z".into(),
            ),
        ]
    );
    assert!(
        column_names(&conn, "projection_threads")
            .iter()
            .any(|name| name == "linked_pull_request_json")
    );
    assert!(
        index_names(&conn, "projection_thread_pull_requests")
            .contains(&"idx_projection_thread_pull_requests_pr".into())
    );
}

#[test]
fn migration_050_keeps_azure_repositories_distinct_across_organizations() {
    let conn = memory();
    migrate_to(&conn, 49);
    for organization in ["org-a", "org-b"] {
        let url = format!("https://dev.azure.com/{organization}/project/_git/web/pullrequest/7");
        let link = serde_json::json!({
            "projectId": organization,
            "repository": "web",
            "number": 7,
            "url": url,
        })
        .to_string();
        conn.execute(
            "INSERT INTO projection_threads (thread_id, project_id, title, model_selection_json, linked_pull_request_json, created_at, updated_at)
             VALUES (?1, ?1, 'Azure', '{\"instanceId\":\"codex\",\"model\":\"gpt-5.4\"}', ?2, '2026-03-01T00:00:00.000Z', '2026-03-01T00:00:00.000Z')",
            rusqlite::params![organization, link],
        )
        .unwrap();
    }
    migrate_to(&conn, 50);
    let rows: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT host, repository, number FROM projection_thread_pull_requests ORDER BY repository")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            ("dev.azure.com".into(), "org-a/project/_git/web".into(), 7),
            ("dev.azure.com".into(), "org-b/project/_git/web".into(), 7),
        ]
    );
}

#[test]
fn migration_055_baselines_v1_projects_in_the_event_log() {
    let conn = memory();
    migrate_to(&conn, 49);
    exec(
        &conn,
        r#"
        INSERT INTO projection_projects (
          project_id, title, workspace_root, default_model_selection_json, scripts_json,
          created_at, updated_at, deleted_at
        ) VALUES (
          'project:existing', 'Existing project', '/work/existing',
          '{"instanceId":"codex","model":"gpt-5.4"}', '[]',
          '2026-06-19T00:00:00.000Z', '2026-06-20T00:00:00.000Z', NULL
        );
        "#,
    );
    run_migrations(&conn, RunMigrationsOptions::default()).unwrap();
    let row: (String, String, String, i64) = conn
        .query_row(
            "SELECT aggregate_kind, stream_id, event_type, application_event_version
             FROM orchestration_events WHERE stream_id = 'project:existing' ORDER BY sequence ASC",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "project".into(),
            "project:existing".into(),
            "project.created".into(),
            2
        )
    );
}

#[test]
fn migration_055_keeps_released_migrations_contiguous() {
    let ids: Vec<i64> = MIGRATIONS.iter().map(|migration| migration.id).collect();
    assert_eq!(ids, (1..=62).collect::<Vec<_>>());
}

#[test]
fn migration_055_upgrades_released_schema_53() {
    let conn = memory();
    migrate_to(&conn, 53);
    let executed = run_migrations(&conn, RunMigrationsOptions::default()).unwrap();
    assert_eq!(
        executed,
        vec![
            (54, "ProjectionThreadsAutoSettleDisabledAt"),
            (55, "OrchestrationV2"),
            (56, "RemoveRedundantProjectionIndexes"),
            (57, "ScheduledTaskWebhooks"),
            (58, "WebhookRelayDeliveries"),
            (59, "McpAppModelContext"),
            (60, "ThreadSnapshotWindowIndexes"),
            (61, "PullRequestReviewGuides"),
            (62, "ThreadFailureLookupIndex"),
        ]
    );
    assert!(
        run_migrations(&conn, RunMigrationsOptions::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        history_from(&conn, 48),
        [
            (48, "ProjectionThreadBranchPullRequest"),
            (49, "ProjectionThreadsActiveOrderKey"),
            (50, "ProjectionThreadPullRequests"),
            (51, "ProjectionThreadMessageContext"),
            (52, "ProjectionThreadTitleState"),
            (53, "PullRequestFilesViewed"),
            (54, "ProjectionThreadsAutoSettleDisabledAt"),
            (55, "OrchestrationV2"),
            (56, "RemoveRedundantProjectionIndexes"),
            (57, "ScheduledTaskWebhooks"),
            (58, "WebhookRelayDeliveries"),
            (59, "McpAppModelContext"),
            (60, "ThreadSnapshotWindowIndexes"),
            (61, "PullRequestReviewGuides"),
            (62, "ThreadFailureLookupIndex"),
        ]
        .into_iter()
        .map(|(id, name)| (id, name.to_owned()))
        .collect::<Vec<_>>()
    );
    assert_eq!(
        strings(
            &conn,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (
               'orchestration_v2_projection_threads',
               'orchestration_v2_projection_subagents',
               'orchestration_v2_effect_outbox',
               'orchestration_v2_turn_item_positions',
               'orchestration_v2_projection_metadata',
               'orchestration_v2_projection_provider_session_bindings',
               'orchestration_v2_thread_launch_workflows',
               'orchestration_v2_legacy_imports',
               'scheduled_tasks'
             ) ORDER BY name",
        ),
        [
            "orchestration_v2_effect_outbox",
            "orchestration_v2_legacy_imports",
            "orchestration_v2_projection_metadata",
            "orchestration_v2_projection_provider_session_bindings",
            "orchestration_v2_projection_subagents",
            "orchestration_v2_projection_threads",
            "orchestration_v2_thread_launch_workflows",
            "orchestration_v2_turn_item_positions",
            "scheduled_tasks",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    );
    assert!(
        column_names(&conn, "orchestration_events")
            .iter()
            .any(|name| name == "application_event_version")
    );
    assert!(
        column_names(&conn, "orchestration_command_receipts")
            .iter()
            .any(|name| name == "command_type")
    );
    assert!(
        column_names(&conn, "orchestration_v2_projection_threads")
            .iter()
            .any(|name| name == "provider_instance_id")
    );
    let subagents = column_names(&conn, "orchestration_v2_projection_subagents");
    assert!(subagents.iter().any(|name| name == "driver"));
    assert!(subagents.iter().any(|name| name == "provider_instance_id"));
    assert_eq!(
        strings(
            &conn,
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN (
               'idx_orchestration_events_application_high_water',
               'orchestration_events_v2_created_threads_idx',
               'orchestration_v2_projection_turn_items_shell_pending_idx'
             ) ORDER BY name",
        ),
        [
            "idx_orchestration_events_application_high_water",
            "orchestration_events_v2_created_threads_idx",
            "orchestration_v2_projection_turn_items_shell_pending_idx",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    );
}

#[test]
fn migration_055_accepts_a_cancelled_effect_outbox_row() {
    let conn = memory();
    run_migrations(&conn, RunMigrationsOptions::default()).unwrap();
    exec(
        &conn,
        r#"
        INSERT INTO orchestration_v2_effect_outbox (
          effect_id, command_id, thread_id, effect_type, payload_json, status,
          attempt_count, available_at, created_at, updated_at, completed_at
        ) VALUES (
          'effect:cancelled', 'command:cancelled', 'thread:cancelled', 'provider-turn.start',
          '{"type":"provider-turn.start","runId":"run:cancelled"}', 'cancelled', 1,
          '2026-06-20T00:00:00.000Z', '2026-06-20T00:00:00.000Z', '2026-06-20T00:01:00.000Z',
          '2026-06-20T00:01:00.000Z'
        );
        "#,
    );
    let row: (String, String, i64) = conn
        .query_row(
            "SELECT effect_id, status, attempt_count FROM orchestration_v2_effect_outbox",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("effect:cancelled".into(), "cancelled".into(), 1));
}

#[test]
fn migration_056_keeps_covering_indexes_and_removes_prefixes() {
    let conn = memory();
    migrate_to(&conn, 55);
    migrate_to(&conn, 56);
    let indexes: std::collections::HashSet<String> = strings(
        &conn,
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_projection_%'",
    )
    .into_iter()
    .collect();
    for removed in [
        "idx_projection_threads_project_id",
        "idx_projection_thread_messages_thread_created",
        "idx_projection_thread_activities_thread_sequence",
        "idx_projection_turns_thread_requested",
    ] {
        assert!(!indexes.contains(removed), "{removed}");
    }
    for covering in [
        "idx_projection_threads_project_deleted_created",
        "idx_projection_thread_messages_thread_created_id",
        "idx_projection_thread_activities_thread_sequence_created_id",
        "idx_projection_turns_thread_keyset",
    ] {
        assert!(indexes.contains(covering), "{covering}");
    }
}

#[test]
fn migration_057_keeps_existing_scheduled_tasks_and_adds_webhook_storage() {
    let conn = memory();
    migrate_to(&conn, 56);
    exec(
        &conn,
        r#"
        INSERT INTO scheduled_tasks (
          task_id, title, prompt, enabled, schedule_json, project_id, thread_id,
          workspace_strategy_json, model_selection_json, runtime_mode, interaction_mode,
          created_by, creation_source, created_at, updated_at, next_run_at, last_run_at,
          last_run_status, last_run_error, run_count
        ) VALUES (
          'existing', 'task', 'Run', 1, '{"type":"interval","everyMs":60000}', 'project', NULL,
          '{"type":"root"}', '{"instanceId":"codex","model":"gpt-5"}', 'full-access', 'default',
          'user', 'web', '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z', NULL, NULL,
          'never', NULL, 0
        );
        "#,
    );
    migrate_to(&conn, 57);
    assert_eq!(
        opt_text_pairs(&conn, "SELECT task_id, webhook_token FROM scheduled_tasks"),
        vec![("existing".into(), None)]
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM scheduled_task_webhook_deliveries",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
}
