//! Live agent-session scan and import over the write runtime.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::agent_sessions::{
    AgentSessionImportInput, AgentSessionImportSource, AgentSessionScanResult,
};
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_contracts::project::ReceivedProjectIcon;
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::{ProjectId, ThreadId};
use turin_orchestration::pipeline::event_sink::{EventSink, WriteInput};
use turin_orchestration::pipeline::orchestrator_port::{OrchestratorPort as _, RecordFilter};
use turin_orchestration::projection::project_store::ProjectStore;
use turin_persistence::{
    ProviderSessionRuntime, ProviderSessionStatus, UpsertConflict, list_provider_sessions,
    record_imported_transcript, upsert_provider_session,
};
use turin_projects::agent_session_discovery::{
    AgentSessionDiscovery, AgentSessionDiscoveryHost, AgentSessionDiscoverySettings,
};
use turin_projects::agent_session_importer::AgentSessionImporter;
use turin_projects::agent_session_scanner::AgentSessionScanner;
use turin_projects::project_ports::{
    ImportedProviderRuntime, OrchestrationEventWriter, PortError, ProjectRow,
    ProjectStoreReads, ProviderRuntimeRows, ProviderThreadIds, ThreadRecordSummary,
    ThreadRecordReader,
};
use turin_projects::project_service::{
    ProjectOperation, ProjectOperationError, ProjectServiceApi,
};
use turin_provider_core::id_allocator::IdAllocator;

use crate::app_runtime::write::runtime::WriteRuntime;

pub struct AgentSessionLive {
    discovery: Arc<AgentSessionDiscovery>,
    importer: Arc<AgentSessionImporter>,
}

impl AgentSessionLive {
    pub fn new(runtime: Arc<WriteRuntime>, data_dir: &std::path::Path) -> Self {
        let host = discovery_host(data_dir);
        let settings = Arc::new(RuntimeDiscoverySettings {
            service: Arc::clone(&runtime.server_settings),
        });
        let projects = Arc::new(StoreReads(Arc::clone(&runtime.stores.projects)));
        let discovery = Arc::new(AgentSessionDiscovery::with_local_filesystem(
            host, settings, projects,
        ));
        let scanner: Arc<dyn AgentSessionScanner> = Arc::clone(&discovery);
        let importer = Arc::new(build_importer(scanner, Arc::clone(&runtime)));
        Self {
            discovery,
            importer,
        }
    }

    pub async fn scan(&self) -> Result<AgentSessionScanResult, String> {
        self.discovery
            .scan()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn import(
        &self,
        project_id: ProjectId,
        expected_workspace_root: String,
    ) -> Result<(u64, u64), String> {
        let result = self
            .importer
            .import_recent_agent_threads(&AgentSessionImportInput {
                project_id,
                expected_workspace_root: TrimmedNonEmptyString::new(&expected_workspace_root).ok(),
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok((result.imported_count, result.skipped_count))
    }
}

fn discovery_host(data_dir: &std::path::Path) -> AgentSessionDiscoveryHost {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    AgentSessionDiscoveryHost {
        home_dir: home.clone(),
        tmp_dir: std::env::temp_dir().to_string_lossy().into_owned(),
        base_dir: data_dir.to_string_lossy().into_owned(),
        worktrees_dir: format!("{home}/.t3/worktrees"),
        case_fold: cfg!(target_os = "windows"),
        environment: std::env::vars().collect(),
    }
}

fn build_importer(
    discovery: Arc<dyn AgentSessionScanner>,
    runtime: Arc<WriteRuntime>,
) -> AgentSessionImporter {
    AgentSessionImporter::new(
        discovery,
        Arc::new(ImportProjects(Arc::clone(&runtime.stores.projects))),
        Arc::new(OrchestratorThreads(Arc::clone(&runtime.orchestrator))),
        Arc::new(SinkWriter(runtime.stores.sink.clone())),
        Arc::new(ProviderThreadIdsLive),
        Arc::new(RuntimeRows(runtime.stores.db.clone())),
    )
}

struct RuntimeDiscoverySettings {
    service: Arc<turin_runtime::server_settings::ServerSettingsService>,
}

#[async_trait]
impl AgentSessionDiscoverySettings for RuntimeDiscoverySettings {
    async fn provider_instances(
        &self,
    ) -> Result<turin_contracts::provider_instance::ProviderInstanceConfigMap, PortError> {
        Ok(self.service.get_settings().await?.provider_instances)
    }
}

struct StoreReads(Arc<ProjectStore>);

#[async_trait]
impl ProjectStoreReads for StoreReads {
    async fn get(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<ProjectRow>, PortError> {
        Ok(self
            .0
            .get(project_id.clone(), include_deleted)
            .await?
            .map(project_row))
    }

    async fn find_active_by_workspace_root(
        &self,
        workspace_root: &str,
    ) -> Result<Option<ProjectRow>, PortError> {
        Ok(self
            .0
            .find_active_by_workspace_root(workspace_root.to_owned())
            .await?
            .map(project_row))
    }

    async fn list(&self, include_deleted: bool) -> Result<Vec<ProjectRow>, PortError> {
        Ok(self
            .0
            .list(None, include_deleted)
            .await?
            .into_iter()
            .map(project_row)
            .collect())
    }

    async fn get_shell(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<turin_contracts::orchestration_project::OrchestrationProjectShell>, PortError>
    {
        Ok(self.0.get_shell(project_id.clone()).await?)
    }

    async fn list_shells(
        &self,
        project_ids: Option<&[ProjectId]>,
    ) -> Result<Vec<turin_contracts::orchestration_project::OrchestrationProjectShell>, PortError>
    {
        Ok(self
            .0
            .list_shells(project_ids.map(|ids| ids.to_vec()))
            .await?)
    }
}

fn trimmed(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("project store row fields are non-empty")
}

fn project_row(row: turin_orchestration::projection::project_store::ProjectRow) -> ProjectRow {
    ProjectRow {
        project_id: row.project_id,
        title: trimmed(&row.title),
        workspace_root: trimmed(&row.workspace_root),
        favicon_path: row
            .favicon_path
            .and_then(|path| TrimmedNonEmptyString::new(&path).ok()),
        default_model_selection: row.default_model_selection,
        default_thread_env_mode: row.default_thread_env_mode,
        auto_pull: row.auto_pull,
        project_icon: row.project_icon.map(|icon| icon.0),
        scripts: row.scripts,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
    }
}

struct ImportProjects(Arc<ProjectStore>);

fn unimplemented_project_service() -> turin_projects::project_service::ProjectServiceError {
    turin_projects::project_service::ProjectServiceError::Operation(ProjectOperationError {
        operation: ProjectOperation::ReadProject,
        project_id: None,
        workspace_root: None,
        cause: anyhow::anyhow!("project service method not wired for onboarding import").into(),
    })
}

#[async_trait]
impl ProjectServiceApi for ImportProjects {
    async fn create(
        &self,
        _: turin_projects::project_service::ProjectCreateInput,
    ) -> Result<turin_contracts::project::Project, turin_projects::project_service::ProjectServiceError>
    {
        Err(unimplemented_project_service())
    }

    async fn bootstrap(
        &self,
        _: turin_projects::project_service::ProjectCreateInput,
    ) -> Result<
        turin_projects::project_service::ProjectBootstrap,
        turin_projects::project_service::ProjectServiceError,
    > {
        Err(unimplemented_project_service())
    }

    async fn update(
        &self,
        _: turin_projects::project_service::ProjectUpdateInput,
    ) -> Result<turin_contracts::project::Project, turin_projects::project_service::ProjectServiceError>
    {
        Err(turin_projects::project_service::ProjectServiceError::NotFound {
            project_id: ProjectId::new("unused").expect("id"),
        })
    }

    async fn delete(
        &self,
        _: turin_projects::project_service::ProjectDeleteInput,
    ) -> Result<turin_contracts::project::Project, turin_projects::project_service::ProjectServiceError>
    {
        Err(turin_projects::project_service::ProjectServiceError::NotFound {
            project_id: ProjectId::new("unused").expect("id"),
        })
    }

    async fn get_by_id(
        &self,
        project_id: &ProjectId,
        include_deleted: bool,
    ) -> Result<Option<turin_contracts::project::Project>, ProjectOperationError> {
        let row = self
            .0
            .get(project_id.clone(), include_deleted)
            .await
            .map_err(|cause| ProjectOperationError {
                operation: ProjectOperation::ReadProject,
                project_id: Some(project_id.clone()),
                workspace_root: None,
                cause: cause.into(),
            })?;
        Ok(row.map(|row| turin_contracts::project::Project {
            id: row.project_id,
            title: trimmed(&row.title),
            workspace_root: trimmed(&row.workspace_root),
            repository_identity: None,
            favicon_path: Some(
                row.favicon_path
                    .and_then(|path| TrimmedNonEmptyString::new(&path).ok()),
            ),
            project_icon: Some(row.project_icon.map(|icon| {
                ReceivedProjectIcon(Some(icon.0))
            })),
            default_model_selection: row.default_model_selection,
            default_thread_env_mode: Some(row.default_thread_env_mode),
            auto_pull: Some(row.auto_pull),
            scripts: row.scripts,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
        }))
    }

    async fn get_by_workspace_root(
        &self,
        _: &str,
        _: bool,
    ) -> Result<Option<turin_contracts::project::Project>, ProjectOperationError> {
        Ok(None)
    }

    async fn snapshot(
        &self,
    ) -> Result<turin_contracts::project::ProjectSnapshot, ProjectOperationError> {
        Err(ProjectOperationError {
            operation: ProjectOperation::ListProjects,
            project_id: None,
            workspace_root: None,
            cause: anyhow::anyhow!("not implemented").into(),
        })
    }

    async fn get_shell(
        &self,
        _: &ProjectId,
    ) -> Result<
        Option<turin_contracts::orchestration_project::OrchestrationProjectShell>,
        ProjectOperationError,
    > {
        Ok(None)
    }

    async fn list_shells(
        &self,
        _: Option<&[ProjectId]>,
    ) -> Result<Vec<turin_contracts::orchestration_project::OrchestrationProjectShell>, ProjectOperationError>
    {
        Ok(Vec::new())
    }
}

struct OrchestratorThreads(Arc<turin_orchestration::core::orchestrator::Orchestrator>);

#[async_trait]
impl ThreadRecordReader for OrchestratorThreads {
    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<ThreadRecordSummary>, PortError> {
        let records = match self
            .0
            .get_thread_records(thread_id, &[], &RecordFilter::default())
            .await
        {
            Ok(records) => records,
            Err(_) => return Ok(None),
        };
        Ok(Some(ThreadRecordSummary {
            project_id: records.thread.project_id.clone(),
            history_origin: records.thread.history_origin,
        }))
    }
}

struct SinkWriter(EventSink);

#[async_trait]
impl OrchestrationEventWriter for SinkWriter {
    async fn write(&self, events: Vec<OrchestrationV2DomainEvent>) -> Result<(), PortError> {
        self.0
            .write(WriteInput::events(events))
            .await
            .map(|_| ())
            .map_err(|error| error.into())
    }
}

struct ProviderThreadIdsLive;

impl ProviderThreadIds for ProviderThreadIdsLive {
    fn derive_provider_thread(
        &self,
        driver: &ProviderDriverKind,
        native_thread_id: &str,
    ) -> turin_contracts::ProviderThreadId {
        IdAllocator::provider_thread(driver, None, native_thread_id)
    }
}

struct RuntimeRows(turin_persistence::Db);

#[async_trait]
impl ProviderRuntimeRows for RuntimeRows {
    async fn list_runtime_payloads(&self) -> Result<Vec<Option<Value>>, PortError> {
        let db = self.0.clone();
        let rows = db
            .read(|conn| list_provider_sessions(conn, false))
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| row.runtime_payload)
            .collect())
    }

    async fn upsert_imported(&self, row: &ImportedProviderRuntime) -> Result<(), PortError> {
        let db = self.0.clone();
        let runtime = ProviderSessionRuntime {
            thread_id: row.thread_id.clone(),
            provider_name: row.provider_name.clone(),
            provider_instance_id: Some(row.provider_instance_id.to_string()),
            adapter_key: row.adapter_key.clone(),
            runtime_mode: serde_json::to_value(row.runtime_mode)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "auto".to_owned()),
            status: ProviderSessionStatus::Stopped,
            last_seen_at: row.last_seen_at.clone(),
            resume_cursor: Some(row.resume_cursor.clone()),
            runtime_payload: Some(row.runtime_payload.clone()),
        };
        db.write(move |conn| upsert_provider_session(conn, &runtime, UpsertConflict::Ignore))
        .await?;
        Ok(())
    }

    async fn record_imported_transcript(
        &self,
        thread_id: &ThreadId,
        source: &AgentSessionImportSource,
    ) -> Result<(), PortError> {
        let db = self.0.clone();
        let thread_id = thread_id.clone();
        let source = serde_json::to_value(source)?;
        db.write(move |conn| record_imported_transcript(conn, &thread_id, &source))
            .await?;
        Ok(())
    }
}
