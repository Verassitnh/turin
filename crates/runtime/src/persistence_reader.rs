//! Read-only queries the settings service needs from the projection tables.
//!
//! The SQL is copied verbatim from upstream `serverSettings.ts`, so the rows
//! mean the same thing. Reads go through [`turin_persistence::Db::read`], which
//! never takes the writer connection.

use rusqlite::{Connection, Row};
use turin_persistence::{Db, PersistenceError};

/// A provider that ran in an older session, read for the cursor/grok/opencode
/// history check during the legacy provider migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHistoryRow {
    pub provider_name: String,
    pub provider_instance_id: Option<String>,
}

/// One project row, read for the one-time legacy project settings fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyProjectSettingsRow {
    pub project_id: String,
    pub default_model_selection: Option<String>,
    pub default_thread_env_mode: Option<String>,
    pub auto_pull: i64,
    pub scripts: String,
}

const PROVIDER_HISTORY_SQL: &str = r#"
      SELECT DISTINCT
        provider_name AS "providerName",
        provider_instance_id AS "providerInstanceId"
      FROM projection_thread_sessions
      WHERE provider_name IN ('cursor', 'grok', 'opencode')
      UNION
      SELECT DISTINCT
        provider_name AS "providerName",
        provider_instance_id AS "providerInstanceId"
      FROM provider_session_runtime
      WHERE provider_name IN ('cursor', 'grok', 'opencode')
    "#;

const LEGACY_PROJECT_SETTINGS_SQL: &str = r#"
          SELECT
            project_id AS "projectId",
            default_model_selection_json AS "defaultModelSelection",
            default_thread_env_mode AS "defaultThreadEnvMode",
            auto_pull AS "autoPull",
            scripts_json AS "scripts"
          FROM projection_projects
          WHERE deleted_at IS NULL
        "#;

fn provider_history_row(row: &Row<'_>) -> rusqlite::Result<ProviderHistoryRow> {
    Ok(ProviderHistoryRow {
        provider_name: row.get("providerName")?,
        provider_instance_id: row.get("providerInstanceId")?,
    })
}

fn legacy_project_settings_row(row: &Row<'_>) -> rusqlite::Result<LegacyProjectSettingsRow> {
    Ok(LegacyProjectSettingsRow {
        project_id: row.get("projectId")?,
        default_model_selection: row.get("defaultModelSelection")?,
        default_thread_env_mode: row.get("defaultThreadEnvMode")?,
        auto_pull: row.get("autoPull")?,
        scripts: row.get("scripts")?,
    })
}

fn query_rows<T>(
    conn: &Connection,
    operation: &str,
    sql: &str,
    map: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, PersistenceError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|error| PersistenceError::sql(operation, &error))?;
    let rows = statement
        .query_map([], map)
        .map_err(|error| PersistenceError::sql(operation, &error))?;
    rows.collect::<rusqlite::Result<Vec<T>>>()
        .map_err(|error| PersistenceError::sql(operation, &error))
}

/// Distinct sessions of the retired cursor, grok and opencode drivers, from
/// both the thread-session projection and the provider runtime table.
pub async fn provider_history(db: &Db) -> Result<Vec<ProviderHistoryRow>, PersistenceError> {
    db.read(|conn| {
        query_rows(
            conn,
            "read-provider-history",
            PROVIDER_HISTORY_SQL,
            provider_history_row,
        )
    })
    .await
}

/// Live (not deleted) projects, for the legacy project settings fold.
pub async fn legacy_project_settings_rows(
    db: &Db,
) -> Result<Vec<LegacyProjectSettingsRow>, PersistenceError> {
    db.read(|conn| {
        query_rows(
            conn,
            "read-project-settings",
            LEGACY_PROJECT_SETTINGS_SQL,
            legacy_project_settings_row,
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_configured(dir.path().join("state.sqlite"))
            .await
            .unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn an_empty_database_has_no_history_or_project_rows() {
        let (_dir, db) = open_db().await;
        assert!(provider_history(&db).await.unwrap().is_empty());
        assert!(legacy_project_settings_rows(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn history_keeps_only_the_retired_drivers_and_deduplicates() {
        let (_dir, db) = open_db().await;
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projection_thread_sessions (thread_id, status, provider_name, provider_instance_id, updated_at)
                   VALUES ('t1', 'idle', 'cursor', 'cursor', '2026-01-01T00:00:00.000Z'),
                          ('t2', 'idle', 'cursor', 'cursor', '2026-01-01T00:00:00.000Z'),
                          ('t3', 'idle', 'codex', 'codex', '2026-01-01T00:00:00.000Z');
                 INSERT INTO provider_session_runtime (thread_id, provider_name, provider_instance_id, adapter_key, status, last_seen_at)
                   VALUES ('t1', 'opencode', 'opencode_work', 'opencode', 'running', '2026-01-01T00:00:00.000Z');",
            )
            .map_err(|error| PersistenceError::sql("seed", &error))
        })
        .await
        .unwrap();
        let mut history = provider_history(&db).await.unwrap();
        history.sort_by(|a, b| a.provider_name.cmp(&b.provider_name));
        assert_eq!(
            history,
            vec![
                ProviderHistoryRow {
                    provider_name: "cursor".into(),
                    provider_instance_id: Some("cursor".into()),
                },
                ProviderHistoryRow {
                    provider_name: "opencode".into(),
                    provider_instance_id: Some("opencode_work".into()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn deleted_projects_are_not_read_for_the_fold() {
        let (_dir, db) = open_db().await;
        db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO projection_projects (project_id, title, workspace_root, scripts_json, created_at, updated_at, deleted_at, auto_pull, default_thread_env_mode)
                   VALUES ('live', 'Live', '/w', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', NULL, 1, 'worktree'),
                          ('gone', 'Gone', '/g', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', '2026-02-01T00:00:00.000Z', 0, NULL);",
            )
            .map_err(|error| PersistenceError::sql("seed", &error))
        })
        .await
        .unwrap();
        let rows = legacy_project_settings_rows(&db).await.unwrap();
        assert_eq!(
            rows,
            vec![LegacyProjectSettingsRow {
                project_id: "live".into(),
                default_model_selection: None,
                default_thread_env_mode: Some("worktree".into()),
                auto_pull: 1,
                scripts: "[]".into(),
            }]
        );
    }
}
