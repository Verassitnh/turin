//! Narrow seams between the project services and the orchestrator and
//! persistence layers. The orchestrator wiring implements these over
//! `turin-orchestration` and `turin-persistence`; the tests here use fakes.
//!
//! Each trait lists exactly what the upstream project files call. Payloads use
//! `turin-contracts` types. Nothing here decides orchestration policy: planning
//! lives behind [`ProjectCommandPlanner`] and thread deletion behind
//! [`ThreadDeletions`].

use async_trait::async_trait;
use serde_json::{Value, json};
use turin_contracts::base_schemas::{IsoDateTime, TrimmedNonEmptyString, UtcDateTime};
use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::project::{ProjectIconOverride, ProjectScript};
use turin_contracts::{CommandId, EventId, ProjectId, ThreadId};

/// Failure from a seam. Upstream carries these as `Schema.Defect` causes.
pub type PortError = anyhow::Error;

/// One row of the project read model (upstream `ProjectStore.ProjectRow`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    pub project_id: ProjectId,
    pub title: TrimmedNonEmptyString,
    pub workspace_root: TrimmedNonEmptyString,
    pub favicon_path: Option<TrimmedNonEmptyString>,
    pub default_model_selection: Option<ModelSelection>,
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    pub auto_pull: bool,
    pub project_icon: Option<ProjectIconOverride>,
    pub scripts: Vec<ProjectScript>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    pub deleted_at: Option<IsoDateTime>,
}

/// Upstream `ProjectStore` reads. Deleted rows are hidden unless asked for.
#[async_trait]
pub trait ProjectStoreReads: Send + Sync {
    async fn get(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<ProjectRow>, PortError>;
    async fn find_active_by_workspace_root(
        &self,
        workspace_root: &str,
    ) -> Result<Option<ProjectRow>, PortError>;
    async fn list(&self, include_deleted: bool) -> Result<Vec<ProjectRow>, PortError>;
    async fn get_shell(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, PortError>;
    async fn list_shells(
        &self,
        project_ids: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, PortError>;
}

/// Upstream `IdAllocator.allocate.event`: the event id for a command.
pub trait EventIdAllocator: Send + Sync {
    fn allocate_event(&self, command_id: &CommandId) -> Result<EventId, PortError>;
}

/// The project command a `ProjectService` method dispatches. Field shapes
/// follow the upstream `ProjectCommand` union the service builds.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectCommand {
    Create {
        command_id: CommandId,
        project_id: ProjectId,
        title: TrimmedNonEmptyString,
        workspace_root: String,
        scripts: Option<Vec<ProjectScript>>,
    },
    MetaUpdate {
        command_id: CommandId,
        project_id: ProjectId,
        title: Option<TrimmedNonEmptyString>,
        workspace_root: Option<String>,
        default_model_selection: Option<Option<ModelSelection>>,
        auto_pull: Option<bool>,
        project_icon: Option<Option<ProjectIconOverride>>,
        favicon_path: Option<Option<TrimmedNonEmptyString>>,
        default_thread_env_mode: Option<Option<ThreadEnvMode>>,
        scripts: Option<Vec<ProjectScript>>,
    },
    Delete {
        command_id: CommandId,
        project_id: ProjectId,
    },
}

impl ProjectCommand {
    pub fn command_id(&self) -> &CommandId {
        match self {
            Self::Create { command_id, .. }
            | Self::MetaUpdate { command_id, .. }
            | Self::Delete { command_id, .. } => command_id,
        }
    }

    pub fn project_id(&self) -> &ProjectId {
        match self {
            Self::Create { project_id, .. }
            | Self::MetaUpdate { project_id, .. }
            | Self::Delete { project_id, .. } => project_id,
        }
    }

    /// The `commandType` recorded on the receipt.
    pub fn command_type(&self) -> &'static str {
        match self {
            Self::Create { .. } => "project.create",
            Self::MetaUpdate { .. } => "project.meta.update",
            Self::Delete { .. } => "project.delete",
        }
    }

    /// Workspace root the command claims, if any. Deletes claim none.
    pub fn claimed_workspace_root(&self) -> Option<&str> {
        match self {
            Self::Create { workspace_root, .. } => Some(workspace_root),
            Self::MetaUpdate { workspace_root, .. } => workspace_root.as_deref(),
            Self::Delete { .. } => None,
        }
    }
}

/// What the planner sees: the project row (deleted rows included) and the
/// active project that already holds the command's workspace root.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectPlanState {
    pub project: Option<ProjectRow>,
    pub workspace_owner: Option<ProjectRow>,
}

/// Why a planned command was refused. Stored on the receipt as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCommandRejection {
    WorkspaceConflict {
        workspace_root: String,
        conflicting_project_id: ProjectId,
    },
    MissingProject {
        project_id: ProjectId,
    },
}

impl ProjectCommandRejection {
    pub fn encode(&self) -> Value {
        match self {
            Self::WorkspaceConflict {
                workspace_root,
                conflicting_project_id,
            } => json!({
                "_tag": "ProjectWorkspaceConflictError",
                "workspaceRoot": workspace_root,
                "conflictingProjectId": conflicting_project_id.as_str(),
            }),
            Self::MissingProject { project_id } => json!({
                "_tag": "ProjectCommandMissingProjectError",
                "projectId": project_id.as_str(),
            }),
        }
    }

    /// `None` for an unrecognized or malformed receipt error.
    pub fn decode(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
        match value.get("_tag")?.as_str()? {
            "ProjectWorkspaceConflictError" => Some(Self::WorkspaceConflict {
                workspace_root: text("workspaceRoot")?,
                conflicting_project_id: ProjectId::new(text("conflictingProjectId")?).ok()?,
            }),
            "ProjectCommandMissingProjectError" => Some(Self::MissingProject {
                project_id: ProjectId::new(text("projectId")?).ok()?,
            }),
            _ => None,
        }
    }
}

/// Opaque project event payload. The planner owns its shape.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectEventPayload(pub Value);

/// Upstream `planProjectCommand`: pure, decides the event or the rejection.
pub trait ProjectCommandPlanner: Send + Sync {
    fn plan(
        &self,
        command: &ProjectCommand,
        state: &ProjectPlanState,
        event_id: &EventId,
        now: &UtcDateTime,
    ) -> Result<ProjectEventPayload, ProjectCommandRejection>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    Accepted,
    Rejected,
}

/// Upstream command receipt for a project command.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCommandReceipt {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub command_type: String,
    pub status: CommandStatus,
    pub error: Option<Value>,
}

/// Upstream `EventSink` for project commands. Each call is one transaction:
/// the event (or rejection), the project row, and the receipt commit together,
/// or none do. A reused command id returns the receipt it already has.
#[async_trait]
pub trait ProjectEventSink: Send + Sync {
    async fn commit_project_command(
        &self,
        command_id: &CommandId,
        project_id: &ProjectId,
        command_type: &str,
        accepted_at: &UtcDateTime,
        event: &ProjectEventPayload,
    ) -> Result<ProjectCommandReceipt, PortError>;

    async fn commit_rejected_project_command(
        &self,
        command_id: &CommandId,
        project_id: &ProjectId,
        command_type: &str,
        rejected_at: &UtcDateTime,
        error: &Value,
    ) -> Result<ProjectCommandReceipt, PortError>;
}

/// Thread work that project deletion triggers. The orchestrator owns the
/// thread command lock, thread projections, and `thread.delete` planning.
#[async_trait]
pub trait ThreadDeletions: Send + Sync {
    /// Every live and archived thread of the project.
    async fn project_thread_ids(&self, project_id: &ProjectId) -> Result<Vec<ThreadId>, PortError>;

    /// Deletes one child thread under its thread lock. Returns `Ok` without
    /// effect when the thread is already deleted or belongs to another project.
    /// The command id is `{command_id}:delete-thread:{thread_id}`, so a retry
    /// resumes the cascade. Fails when that id was already used for another
    /// thread or was rejected.
    async fn delete_child_thread(
        &self,
        project_id: &ProjectId,
        command_id: &CommandId,
        thread_id: &ThreadId,
    ) -> Result<(), PortError>;
}

/// Clears a deleted project's per-project settings overrides (upstream
/// `serverSettings.updateSettings({ projectSettingsOverrides: { id: null } })`).
#[async_trait]
pub trait ProjectSettingsOverrides: Send + Sync {
    async fn clear_project_override(&self, project_id: &ProjectId) -> Result<(), PortError>;
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::ProjectId;

    use super::ProjectCommandRejection;

    #[test]
    fn rejections_round_trip_through_the_receipt_error() {
        let conflict = ProjectCommandRejection::WorkspaceConflict {
            workspace_root: "/repo".to_owned(),
            conflicting_project_id: ProjectId::new("p-owner").expect("id"),
        };
        assert_eq!(
            ProjectCommandRejection::decode(&conflict.encode()),
            Some(conflict)
        );
        let missing = ProjectCommandRejection::MissingProject {
            project_id: ProjectId::new("p-gone").expect("id"),
        };
        assert_eq!(
            ProjectCommandRejection::decode(&missing.encode()),
            Some(missing)
        );
        assert_eq!(
            ProjectCommandRejection::decode(&json!({"_tag": "Other"})),
            None
        );
    }
}
