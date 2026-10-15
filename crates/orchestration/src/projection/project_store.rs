//! Port of upstream/apps/server/src/orchestration-v2/ProjectStore.ts

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;
use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::project::{ProjectScript, ReceivedProjectIcon, StoredProjectIcon};
use turin_contracts::{ProjectId, TrimmedNonEmptyString};
use turin_persistence::{Db, PersistenceDecodeError, PersistenceError, ProjectEvent};

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    pub project_id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    pub default_model_selection: Option<ModelSelection>,
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    pub auto_pull: bool,
    pub favicon_path: Option<String>,
    pub project_icon: Option<StoredProjectIcon>,
    pub scripts: Vec<ProjectScript>,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

#[derive(Clone)]
pub struct ProjectStore { db: Db }

impl ProjectStore {
    pub fn new(db: Db) -> Self { Self { db } }

    /// Fold in the same writer transaction as the event append.
    pub fn apply_in_tx(tx: &Transaction<'_>, event: &ProjectEvent) -> Result<(), PersistenceError> {
        let payload = &event.payload;
        if event.event_type == "project.created" {
            let row = ProjectRow {
                project_id: id(payload)?, title: required_string(payload, "title")?,
                workspace_root: required_string(payload, "workspaceRoot")?,
                default_model_selection: decode_field(payload, "defaultModelSelection")?,
                default_thread_env_mode: decode_field(payload, "defaultThreadEnvMode")?,
                auto_pull: false, favicon_path: optional_string(payload, "faviconPath"),
                project_icon: decode_field(payload, "projectIcon")?,
                scripts: decode_field(payload, "scripts")?.unwrap_or_default(),
                created_at: required_string(payload, "createdAt")?,
                updated_at: required_string(payload, "updatedAt")?, deleted_at: None,
            };
            return upsert(tx, &row);
        }
        let project_id = id(payload)?;
        let Some(mut row) = get_conn(tx, &project_id, true)? else { return Ok(()); };
        if event.event_type == "project.deleted" {
            let deleted_at = required_string(payload, "deletedAt")?;
            row.updated_at = deleted_at.clone(); row.deleted_at = Some(deleted_at);
            return upsert(tx, &row);
        }
        if event.event_type != "project.meta-updated" { return Err(invalid("unknown project event type")); }
        if let Some(value) = payload.get("title") { row.title = string_value(value, "title")?; }
        if let Some(value) = payload.get("workspaceRoot") { row.workspace_root = string_value(value, "workspaceRoot")?; }
        if payload.get("defaultModelSelection").is_some() { row.default_model_selection = decode_field(payload, "defaultModelSelection")?; }
        if payload.get("defaultThreadEnvMode").is_some() { row.default_thread_env_mode = decode_field(payload, "defaultThreadEnvMode")?; }
        if let Some(value) = payload.get("autoPull") { row.auto_pull = value.as_bool().ok_or_else(|| invalid("autoPull must be boolean"))?; }
        if payload.get("faviconPath").is_some() { row.favicon_path = optional_string(payload, "faviconPath"); }
        if payload.get("projectIcon").is_some() { row.project_icon = decode_field(payload, "projectIcon")?; }
        if payload.get("scripts").is_some() { row.scripts = decode_field(payload, "scripts")?.ok_or_else(|| invalid("scripts must be array"))?; }
        row.updated_at = required_string(payload, "updatedAt")?;
        upsert(tx, &row)
    }

    pub async fn get(&self, project_id: ProjectId, include_deleted: bool) -> Result<Option<ProjectRow>, PersistenceError> {
        self.db.read(move |conn| get_conn(conn, &project_id, include_deleted)).await
    }

    pub async fn list(&self, project_ids: Option<Vec<ProjectId>>, include_deleted: bool) -> Result<Vec<ProjectRow>, PersistenceError> {
        self.db.read(move |conn| {
            let mut statement = conn.prepare("SELECT project_id, title, workspace_root, default_model_selection_json, default_thread_env_mode, auto_pull, favicon_path, project_icon_json, scripts_json, created_at, updated_at, deleted_at FROM projection_projects ORDER BY created_at ASC, project_id ASC").map_err(|e| PersistenceError::sql("ProjectStore.list", &e))?;
            let rows = statement.query_map([], |row| raw_row(row)).map_err(|e| PersistenceError::sql("ProjectStore.list", &e))?;
            let mut result = Vec::new();
            for raw in rows {
                let row = decode_row(raw.map_err(|e| PersistenceError::sql("ProjectStore.list", &e))?)?;
                if !include_deleted && row.deleted_at.is_some() { continue; }
                if project_ids.as_ref().is_some_and(|ids| !ids.contains(&row.project_id)) { continue; }
                result.push(row);
            }
            Ok(result)
        }).await
    }

    pub async fn find_active_by_workspace_root(&self, root: String) -> Result<Option<ProjectRow>, PersistenceError> {
        self.db.read(move |conn| {
            let raw = conn.query_row("SELECT project_id, title, workspace_root, default_model_selection_json, default_thread_env_mode, auto_pull, favicon_path, project_icon_json, scripts_json, created_at, updated_at, deleted_at FROM projection_projects WHERE workspace_root=?1 AND deleted_at IS NULL ORDER BY created_at ASC, project_id ASC LIMIT 1", [root], raw_row).optional().map_err(|e| PersistenceError::sql("ProjectStore.findActiveByWorkspaceRoot", &e))?;
            raw.map(decode_row).transpose()
        }).await
    }

    pub async fn get_shell(&self, project_id: ProjectId) -> Result<Option<OrchestrationProjectShell>, PersistenceError> {
        self.get(project_id, false).await?.map(to_shell).transpose()
    }

    pub async fn list_shells(&self, ids: Option<Vec<ProjectId>>) -> Result<Vec<OrchestrationProjectShell>, PersistenceError> {
        self.list(ids, false).await?.into_iter().map(to_shell).collect()
    }
}

fn invalid(message: impl Into<String>) -> PersistenceError { PersistenceError::Decode(PersistenceDecodeError::new("ProjectStore", message)) }
fn string_value(value: &Value, name: &str) -> Result<String, PersistenceError> { value.as_str().map(str::to_owned).ok_or_else(|| invalid(format!("{name} must be string"))) }
fn required_string(value: &Value, name: &str) -> Result<String, PersistenceError> { string_value(&value[name], name) }
fn optional_string(value: &Value, name: &str) -> Option<String> { value[name].as_str().map(str::to_owned) }
fn id(value: &Value) -> Result<ProjectId, PersistenceError> { ProjectId::new(required_string(value, "projectId")?).map_err(|e| invalid(e.to_string())) }
fn decode_field<T: serde::de::DeserializeOwned>(value: &Value, name: &str) -> Result<Option<T>, PersistenceError> {
    if value[name].is_null() { return Ok(None); }
    serde_json::from_value(value[name].clone()).map(Some).map_err(|e| invalid(format!("{name}: {e}")))
}
fn decode_json<T: serde::de::DeserializeOwned>(text: Option<String>, name: &str) -> Result<Option<T>, PersistenceError> {
    text.map(|text| serde_json::from_str(&text).map_err(|e| invalid(format!("{name}: {e}")))).transpose()
}

type RawRow = (String, String, String, Option<String>, Option<String>, i64, Option<String>, Option<String>, String, String, String, Option<String>);
fn raw_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> { Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?)) }
fn decode_row(raw: RawRow) -> Result<ProjectRow, PersistenceError> {
    Ok(ProjectRow { project_id: ProjectId::new(raw.0).map_err(|e| invalid(e.to_string()))?, title:raw.1, workspace_root:raw.2,
      default_model_selection:decode_json(raw.3,"defaultModelSelection")?, default_thread_env_mode:raw.4.map(|s| serde_json::from_value(Value::String(s)).map_err(|e| invalid(format!("defaultThreadEnvMode: {e}")))).transpose()?,
      auto_pull:raw.5 != 0, favicon_path:raw.6, project_icon:decode_json(raw.7,"projectIcon")?, scripts:serde_json::from_str(&raw.8).map_err(|e| invalid(format!("scripts: {e}")))?,
      created_at:raw.9, updated_at:raw.10, deleted_at:raw.11 })
}
fn get_conn(conn: &Connection, project_id: &ProjectId, include_deleted: bool) -> Result<Option<ProjectRow>, PersistenceError> {
    let raw = conn.query_row("SELECT project_id, title, workspace_root, default_model_selection_json, default_thread_env_mode, auto_pull, favicon_path, project_icon_json, scripts_json, created_at, updated_at, deleted_at FROM projection_projects WHERE project_id=?1 AND (?2 OR deleted_at IS NULL) LIMIT 1", params![project_id.as_str(), include_deleted], raw_row).optional().map_err(|e| PersistenceError::sql("ProjectStore.get", &e))?;
    raw.map(decode_row).transpose()
}
fn upsert(tx: &Transaction<'_>, row: &ProjectRow) -> Result<(), PersistenceError> {
    let model = row.default_model_selection.as_ref().map(|value| serde_json::to_string(value)).transpose().map_err(|e| invalid(e.to_string()))?;
    let env = row.default_thread_env_mode.as_ref().map(|value| serde_json::to_value(value).ok().and_then(|v| v.as_str().map(str::to_owned))).flatten();
    let icon = row.project_icon.as_ref().map(serde_json::to_string).transpose().map_err(|e| invalid(e.to_string()))?;
    let scripts = serde_json::to_string(&row.scripts).map_err(|e| invalid(e.to_string()))?;
    tx.execute("INSERT INTO projection_projects (project_id,title,workspace_root,default_model_selection_json,default_thread_env_mode,auto_pull,favicon_path,project_icon_json,scripts_json,created_at,updated_at,deleted_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(project_id) DO UPDATE SET title=excluded.title, workspace_root=excluded.workspace_root, default_model_selection_json=excluded.default_model_selection_json, default_thread_env_mode=excluded.default_thread_env_mode, auto_pull=excluded.auto_pull, favicon_path=excluded.favicon_path, project_icon_json=excluded.project_icon_json, scripts_json=excluded.scripts_json, created_at=excluded.created_at, updated_at=excluded.updated_at, deleted_at=excluded.deleted_at", params![row.project_id.as_str(), row.title, row.workspace_root, model, env, row.auto_pull, row.favicon_path, icon, scripts, row.created_at, row.updated_at, row.deleted_at]).map_err(|e| PersistenceError::sql("ProjectStore.apply", &e))?;
    Ok(())
}
fn to_shell(row: ProjectRow) -> Result<OrchestrationProjectShell, PersistenceError> {
    let icon: Option<ReceivedProjectIcon> = row.project_icon.map(|icon| serde_json::from_value(serde_json::to_value(icon).map_err(|e| invalid(e.to_string()))?).map_err(|e| invalid(e.to_string()))).transpose()?;
    Ok(OrchestrationProjectShell { id:row.project_id, title:TrimmedNonEmptyString::new(row.title).map_err(|e| invalid(e.to_string()))?, workspace_root:TrimmedNonEmptyString::new(row.workspace_root).map_err(|e| invalid(e.to_string()))?, repository_identity:Some(None), default_model_selection:row.default_model_selection, default_thread_env_mode:Some(row.default_thread_env_mode), auto_pull:Some(row.auto_pull), favicon_path:Some(row.favicon_path.map(|text| TrimmedNonEmptyString::new(text).map_err(|e| invalid(e.to_string()))).transpose()?), project_icon:Some(icon), scripts:row.scripts, created_at:row.created_at, updated_at:row.updated_at })
}
