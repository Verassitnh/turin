//! Port of upstream/apps/server/src/orchestration-v2/ThreadPullRequestService.ts
//!
//! Finds the pull request a thread's branch belongs to and records it on the thread. Runs after
//! a run or checkpoint ends (refresh), when a thread is created or changed (read that thread),
//! and in a periodic sweep over unsettled threads, with a backfill pass for settled ones.
//!
//! Upstream's drainable worker serialises these passes. [`ThreadPullRequestService::refresh`]
//! takes a lock for the same effect; the caller decides when to run a pass, from
//! [`ThreadPullRequestService::request_for_event`].

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use indexmap::IndexMap;
use tokio::sync::Mutex as AsyncMutex;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::git::GitPullRequestState;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Command, OrchestrationV2DomainEvent, OrchestrationV2PullRequestSyncExpected,
    OrchestrationV2RunStatus, OrchestrationV2ServerCommand, OrchestrationV2SettleOverride,
    OrchestrationV2ThreadShell,
};
use turin_contracts::pull_request::{PullRequestRef, PullRequestState, PullRequestSummary};
use turin_contracts::thread_pull_request::ThreadLinkedPullRequest;
use turin_contracts::{CommandId, ProjectId, ThreadId};
use turin_git::git_manager::{GitBranchPullRequest, GitManager};
use turin_shared::source_control::{canonical_repository_key, source_control_repository_selector};
use turin_source_control::pull_request::service::PullRequestService;
use turin_source_control::pull_request::service::errors::PullRequestError;
use turin_source_control::pull_request::service::ports::RepositoryIdentityResolver;

use crate::pipeline::domain_event::event_thread_id;
use crate::pipeline::orchestrator_port::{
    BoxError, OrchestratorPort, ShellLocation, ShellSnapshotOptions,
};

/// Backfill attempts for a settled thread that still has no branch pull request.
const BACKFILL_ATTEMPTS: u32 = 5;
/// Branch lookups in flight at once, so they reach the provider together.
const DISCOVERY_CONCURRENCY: usize = 32;

/// `RefreshRequest`: one pass. `thread_id: None` sweeps every thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshRequest {
    pub thread_id: Option<ThreadId>,
    pub refresh: bool,
    pub backfill: bool,
}

/// `projectWorkspaceMatchesSnapshot`: a result read against one checkout is applied only while
/// the project still has that checkout. A deleted project fails the check.
pub fn project_workspace_matches_snapshot(
    current: Option<&OrchestrationProjectShell>,
    expected_workspace_root: &str,
) -> bool {
    current.is_some_and(|project| project.workspace_root.as_str() == expected_workspace_root)
}

/// `samePullRequest`: a missing link matches only a missing link. Repository compares
/// case-insensitively, as the host does.
pub fn same_pull_request(
    left: Option<&ThreadLinkedPullRequest>,
    right: Option<&ThreadLinkedPullRequest>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.project_id == right.project_id
                && left.repository.as_str().to_lowercase()
                    == right.repository.as_str().to_lowercase()
                && left.number == right.number
                && left.url == right.url
        }
        _ => false,
    }
}

/// `pullRequestMatchesProject`: the branch's pull request lives in this project's repository.
fn pull_request_matches_project(
    pull_request: &GitBranchPullRequest,
    project: &OrchestrationProjectShell,
) -> bool {
    let Some(repository_key) = pull_request.repository_key.as_deref() else {
        return false;
    };
    match project
        .repository_identity
        .as_ref()
        .and_then(Option::as_ref)
    {
        Some(identity) => {
            canonical_repository_key(repository_key)
                == canonical_repository_key(identity.canonical_key.as_str())
        }
        None => false,
    }
}

/// Whether a sweep over every thread reads only unsettled ones. Backfill (or a backfill still owed)
/// reads settled threads too.
pub fn periodic_reads_unsettled_only(request: &RefreshRequest, backfill_owed: bool) -> bool {
    !(request.backfill || backfill_owed)
}

/// The thread fields a discovery pass reads and writes. Upstream groups threads by
/// `[projectId, worktreePath, branch]`, so one git lookup serves every thread on that checkout.
type GroupKey = (String, Option<String>, Option<String>);

/// The branch's pull request, as a thread link would name it.
fn detected_reference(
    project_id: &ProjectId,
    repository: &str,
    pull_request: &GitBranchPullRequest,
) -> ThreadLinkedPullRequest {
    ThreadLinkedPullRequest {
        project_id: project_id.clone(),
        repository: TrimmedNonEmptyString::new(repository).expect("selector is non-empty"),
        number: pull_request.change_request.number,
        url: TrimmedNonEmptyString::new(&pull_request.change_request.url)
            .expect("change request URL is non-empty"),
    }
}

/// The git lookups the service makes (`GitManager.branchPullRequest`).
#[async_trait]
pub trait BranchPullRequests: Send + Sync {
    async fn branch_pull_request(
        &self,
        cwd: &str,
        branch: &str,
        refresh: bool,
    ) -> Result<Option<GitBranchPullRequest>, BoxError>;
}

#[async_trait]
impl BranchPullRequests for GitManager {
    async fn branch_pull_request(
        &self,
        cwd: &str,
        branch: &str,
        refresh: bool,
    ) -> Result<Option<GitBranchPullRequest>, BoxError> {
        GitManager::branch_pull_request(self, cwd, branch, refresh)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// The pull request reads the service makes (`PullRequestService.summary`).
#[async_trait]
pub trait PullRequestSummaries: Send + Sync {
    async fn summary(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestSummary, PullRequestError>;
}

#[async_trait]
impl PullRequestSummaries for PullRequestService {
    async fn summary(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestSummary, PullRequestError> {
        PullRequestService::summary(self, reference, Some(false)).await
    }
}

/// The project shells the passes read (`ProjectStoreV2.listShells` / `getShell`).
#[async_trait]
pub trait ProjectShells: Send + Sync {
    async fn list(&self) -> Result<Vec<OrchestrationProjectShell>, BoxError>;
    async fn get(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, BoxError>;
}

#[async_trait]
impl ProjectShells for crate::projection::project_store::ProjectStore {
    async fn list(&self) -> Result<Vec<OrchestrationProjectShell>, BoxError> {
        crate::projection::project_store::ProjectStore::list_shells(self, None)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn get(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, BoxError> {
        crate::projection::project_store::ProjectStore::get_shell(self, project_id.clone())
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

pub struct ThreadPullRequestService {
    orchestrator: Arc<dyn OrchestratorPort>,
    projects: Arc<dyn ProjectShells>,
    git: Arc<dyn BranchPullRequests>,
    pulls: Arc<dyn PullRequestSummaries>,
    identities: Arc<dyn RepositoryIdentityResolver>,
    /// Threads still owed a backfill attempt, counted down as each pass fails for them.
    pending_backfill: parking_lot::Mutex<HashMap<ThreadId, u32>>,
    /// One pass at a time, as the drainable worker runs them.
    pass: AsyncMutex<()>,
}

impl ThreadPullRequestService {
    pub fn new(
        orchestrator: Arc<dyn OrchestratorPort>,
        projects: Arc<dyn ProjectShells>,
        git: Arc<dyn BranchPullRequests>,
        pulls: Arc<dyn PullRequestSummaries>,
        identities: Arc<dyn RepositoryIdentityResolver>,
    ) -> Self {
        Self {
            orchestrator,
            projects,
            git,
            pulls,
            identities,
            pending_backfill: parking_lot::Mutex::new(HashMap::new()),
            pass: AsyncMutex::new(()),
        }
    }

    /// Which pass an event asks for, if any (`processEvent`). Run finishes and checkpoints
    /// refresh (the branch may have a new pull request), a settled thread reopening or being
    /// renamed reads only that thread.
    pub fn request_for_event(event: &OrchestrationV2DomainEvent) -> Option<RefreshRequest> {
        // An event without a thread must not become a periodic sweep, so it asks for nothing.
        let thread = event_thread_id(event)?;
        let for_thread = |refresh: bool| {
            Some(RefreshRequest {
                thread_id: Some(thread.clone()),
                refresh,
                backfill: false,
            })
        };
        match event {
            OrchestrationV2DomainEvent::ThreadCreated { .. }
            | OrchestrationV2DomainEvent::ThreadUnarchived { .. }
            | OrchestrationV2DomainEvent::ThreadMetadataUpdated { .. } => for_thread(false),
            OrchestrationV2DomainEvent::ThreadUnsettled { .. }
            | OrchestrationV2DomainEvent::CheckpointCaptured { .. } => for_thread(true),
            OrchestrationV2DomainEvent::RunUpdated { payload, .. }
                if matches!(
                    payload.status,
                    OrchestrationV2RunStatus::Completed
                        | OrchestrationV2RunStatus::Failed
                        | OrchestrationV2RunStatus::Cancelled
                        | OrchestrationV2RunStatus::Interrupted
                ) =>
            {
                for_thread(true)
            }
            _ => None,
        }
    }

    /// The periodic pass, run every minute after startup: backfill first, then unsettled.
    pub fn periodic_request() -> RefreshRequest {
        RefreshRequest {
            thread_id: None,
            refresh: false,
            backfill: false,
        }
    }

    /// Runs one pass. A failed pass is logged and does not reach the caller, as upstream's worker
    /// catches it.
    pub async fn refresh(&self, request: RefreshRequest) {
        let _pass = self.pass.lock().await;
        if let Err(error) = self.synchronize(&request).await {
            tracing::warn!(error = %error, "thread pull request refresh failed");
        }
    }

    async fn synchronize(&self, request: &RefreshRequest) -> Result<(), BoxError> {
        let (snapshot_sequence, threads) = self.read_threads(request).await?;
        let projects: HashMap<ProjectId, OrchestrationProjectShell> = self
            .projects
            .list()
            .await?
            .into_iter()
            .map(|project| (project.id.clone(), project))
            .collect();

        if request.backfill {
            for thread in &threads {
                if (thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
                    || thread.settled_at.is_some())
                    && thread
                        .branch_pull_request
                        .as_ref()
                        .is_none_or(Option::is_none)
                {
                    self.pending_backfill
                        .lock()
                        .insert(thread.id.clone(), BACKFILL_ATTEMPTS);
                }
            }
        }
        // A thread with no branch has nothing to look up, and its entry would keep every periodic
        // pass on the full read.
        let visible: std::collections::HashSet<ThreadId> = threads
            .iter()
            .filter(|thread| thread.branch.is_some())
            .map(|thread| thread.id.clone())
            .collect();
        let checked: Vec<ThreadId> = match &request.thread_id {
            None => self.pending_backfill.lock().keys().cloned().collect(),
            Some(thread_id) => vec![thread_id.clone()],
        };
        for thread_id in checked {
            if !visible.contains(&thread_id) {
                self.pending_backfill.lock().remove(&thread_id);
            }
        }

        let candidates: Vec<OrchestrationV2ThreadShell> = threads
            .into_iter()
            .filter(|thread| {
                thread.archived_at.is_none()
                    && ((thread.settled_override != Some(OrchestrationV2SettleOverride::Settled)
                        && thread.settled_at.is_none())
                        || request.thread_id.is_some()
                        || self.pending_backfill.lock().contains_key(&thread.id))
                    && (thread.branch.is_some()
                        || thread
                            .branch_pull_request
                            .as_ref()
                            .is_some_and(Option::is_some))
            })
            .collect();

        let mut groups: IndexMap<GroupKey, Vec<OrchestrationV2ThreadShell>> = IndexMap::new();
        for thread in candidates {
            let key: GroupKey = (
                thread.project_id.as_str().to_owned(),
                thread
                    .worktree_path
                    .as_ref()
                    .map(|path| path.as_str().to_owned()),
                thread
                    .branch
                    .as_ref()
                    .map(|branch| branch.as_str().to_owned()),
            );
            groups.entry(key).or_default().push(thread);
        }

        let projects = &projects;
        let sequence = snapshot_sequence;
        let refresh = request.refresh;
        futures::stream::iter(groups.into_values())
            .map(|group| self.discover_group(group, projects, sequence, refresh))
            .buffer_unordered(DISCOVERY_CONCURRENCY)
            .collect::<Vec<()>>()
            .await;
        Ok(())
    }

    /// The threads a pass reads: one thread's shell, or every active thread's (only unsettled
    /// ones unless backfill is still owed).
    async fn read_threads(
        &self,
        request: &RefreshRequest,
    ) -> Result<(i64, Vec<OrchestrationV2ThreadShell>), BoxError> {
        match &request.thread_id {
            None => {
                let unsettled_only = periodic_reads_unsettled_only(
                    request,
                    !self.pending_backfill.lock().is_empty(),
                );
                let snapshot = self
                    .orchestrator
                    .get_shell_snapshot(&ShellSnapshotOptions {
                        location: ShellLocation::Active,
                        project_id: None,
                        unsettled_only,
                    })
                    .await
                    .map_err(|error| Box::new(error) as BoxError)?;
                Ok((snapshot.snapshot_sequence.try_into()?, snapshot.threads))
            }
            Some(thread_id) => {
                // Read the sequence first. The thread is then at least this new, so a sync guarded
                // by the sequence is rejected rather than missing a change.
                let sequence = self
                    .orchestrator
                    .get_thread_event_sequence(thread_id)
                    .await
                    .map_err(|error| Box::new(error) as BoxError)?;
                let thread = self
                    .orchestrator
                    .get_thread_shell(thread_id)
                    .await
                    .map_err(|error| Box::new(error) as BoxError)?;
                Ok((sequence, thread.into_iter().collect()))
            }
        }
    }

    async fn discover_group(
        &self,
        group: Vec<OrchestrationV2ThreadShell>,
        projects: &HashMap<ProjectId, OrchestrationProjectShell>,
        snapshot_sequence: i64,
        refresh: bool,
    ) {
        if let Err(error) = self
            .discover_group_checked(&group, projects, snapshot_sequence, refresh)
            .await
        {
            tracing::warn!(error = %error, "thread branch pull request lookup failed");
            self.fail_backfill(&group);
        }
    }

    async fn discover_group_checked(
        &self,
        group: &[OrchestrationV2ThreadShell],
        projects: &HashMap<ProjectId, OrchestrationProjectShell>,
        snapshot_sequence: i64,
        refresh: bool,
    ) -> Result<(), BoxError> {
        let first = &group[0];
        let Some(project) = projects.get(&first.project_id) else {
            self.finish_backfill(group);
            return Ok(());
        };
        let identity = self
            .identities
            .resolve(project.workspace_root.as_str())
            .await;
        let repository = identity
            .as_ref()
            .and_then(|identity| source_control_repository_selector(Some(identity.into())));
        if first.branch.is_some() && repository.is_none() {
            self.finish_backfill(group);
            return Ok(());
        }
        let worktree_exists = match &first.worktree_path {
            Some(path) => tokio::fs::try_exists(path.as_str()).await.unwrap_or(false),
            None => false,
        };
        let cwd = match (&first.worktree_path, worktree_exists) {
            (Some(path), true) => path.as_str().to_owned(),
            _ => project.workspace_root.as_str().to_owned(),
        };
        let detected = match &first.branch {
            None => None,
            Some(branch) => {
                self.git
                    .branch_pull_request(&cwd, branch.as_str(), refresh)
                    .await?
            }
        };
        // A pull request in another repository is not this project's.
        if let Some(pull_request) = &detected
            && !pull_request_matches_project(pull_request, project)
        {
            self.finish_backfill(group);
            return Ok(());
        }
        let detected_reference = match (&detected, &repository) {
            (Some(pull_request), Some(repository)) => {
                Some(detected_reference(&project.id, repository, pull_request))
            }
            _ => None,
        };

        let mut updates: Vec<PlannedUpdate> = Vec::new();
        for thread in group {
            match self
                .plan_thread(thread, &detected, detected_reference.as_ref())
                .await
            {
                Ok(Some(update)) => updates.push(update),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(thread_id = %thread.id, error = %error, "thread pull request discovery failed");
                    self.fail_backfill(std::slice::from_ref(thread));
                }
            }
        }
        if updates.is_empty() {
            return Ok(());
        }

        if let (Some(detected), Some(branch)) = (&detected, &first.branch) {
            let current = self
                .git
                .branch_pull_request(&cwd, branch.as_str(), true)
                .await?;
            let current_identity = self
                .identities
                .resolve(project.workspace_root.as_str())
                .await;
            let mut refreshed_project = project.clone();
            refreshed_project.repository_identity = Some(current_identity);
            let unchanged = current.as_ref().is_some_and(|current| {
                current.change_request.number == detected.change_request.number
                    && current.change_request.url == detected.change_request.url
                    && current.change_request.state == detected.change_request.state
                    && current.repository_key == detected.repository_key
                    && pull_request_matches_project(current, &refreshed_project)
            });
            if !unchanged {
                let threads: Vec<OrchestrationV2ThreadShell> =
                    updates.iter().map(|update| update.thread.clone()).collect();
                self.fail_backfill(&threads);
                return Ok(());
            }
        }

        for update in updates {
            if let Err(error) = self.apply_update(&update, project, snapshot_sequence).await {
                tracing::warn!(thread_id = %update.thread.id, error = %error, "thread pull request update failed");
                self.fail_backfill(std::slice::from_ref(&update.thread));
            }
        }
        Ok(())
    }

    /// The change a thread needs, or `None` when it already has it.
    async fn plan_thread(
        &self,
        thread: &OrchestrationV2ThreadShell,
        detected: &Option<GitBranchPullRequest>,
        detected_reference: Option<&ThreadLinkedPullRequest>,
    ) -> Result<Option<PlannedUpdate>, BoxError> {
        let mut branch_pull_request: Option<ThreadLinkedPullRequest> = detected_reference.cloned();
        let stored_branch_pull_request: Option<ThreadLinkedPullRequest> =
            thread.branch_pull_request.clone().flatten();
        if branch_pull_request.is_none()
            && thread.branch.is_some()
            && thread.worktree_path.is_none()
            && let Some(previous) = stored_branch_pull_request.as_ref()
        {
            let summary = self.pulls.summary(&link_as_reference(previous)).await?;
            if matches!(
                summary.state,
                PullRequestState::Merged | PullRequestState::Closed
            ) {
                branch_pull_request = Some(previous.clone());
            }
        }

        let mut replacement: Option<ThreadLinkedPullRequest> = None;
        let stored_linked: Option<ThreadLinkedPullRequest> =
            thread.linked_pull_request.clone().flatten();
        let has_no_links = thread.pull_requests.as_ref().is_none_or(Vec::is_empty);
        let detected_open = detected.as_ref().is_some_and(|pull_request| {
            pull_request.change_request.state == GitPullRequestState::Open
        });
        if has_no_links
            && let Some(linked) = stored_linked.as_ref()
            && detected_open
            && let Some(reference) = detected_reference
            && !same_pull_request(Some(linked), Some(reference))
        {
            let summary = self.pulls.summary(&link_as_reference(linked)).await?;
            if matches!(
                summary.state,
                PullRequestState::Merged | PullRequestState::Closed
            ) {
                replacement = Some(reference.clone());
            }
        }

        if same_pull_request(
            stored_branch_pull_request.as_ref(),
            branch_pull_request.as_ref(),
        ) && replacement.is_none()
        {
            self.pending_backfill.lock().remove(&thread.id);
            return Ok(None);
        }
        Ok(Some(PlannedUpdate {
            thread: thread.clone(),
            branch_pull_request,
            replacement,
        }))
    }

    async fn apply_update(
        &self,
        update: &PlannedUpdate,
        project: &OrchestrationProjectShell,
        snapshot_sequence: i64,
    ) -> Result<(), BoxError> {
        // Discovery can perform network I/O. Re-read the project at the transaction boundary so a
        // deleted project or changed workspace root cannot apply a result from the old checkout.
        let current = self.projects.get(&project.id).await?;
        if !project_workspace_matches_snapshot(current.as_ref(), project.workspace_root.as_str()) {
            self.fail_backfill(std::slice::from_ref(&update.thread));
            return Ok(());
        }
        let thread = &update.thread;
        let command_id = CommandId::new(format!(
            "server:thread-pull-request:{}:{}",
            thread.id,
            uuid::Uuid::new_v4()
        ))?;
        let expected = OrchestrationV2PullRequestSyncExpected {
            workspace_root: project.workspace_root.clone(),
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            linked_pull_request: thread.linked_pull_request.clone().flatten(),
            branch_pull_request: thread.branch_pull_request.clone().flatten(),
        };
        self.orchestrator
            .dispatch(OrchestrationV2ServerCommand::Client(
                OrchestrationV2Command::ThreadPullRequestSync {
                    command_id,
                    thread_id: thread.id.clone(),
                    project_id: project.id.clone(),
                    snapshot_sequence: u64::try_from(snapshot_sequence)?,
                    expected,
                    branch_pull_request: update.branch_pull_request.clone(),
                    linked_pull_request: update.replacement.clone(),
                },
            ))
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        self.pending_backfill.lock().remove(&thread.id);
        Ok(())
    }

    fn finish_backfill(&self, threads: &[OrchestrationV2ThreadShell]) {
        let ids: Vec<ThreadId> = threads.iter().map(|thread| thread.id.clone()).collect();
        self.finish_backfill_ids(&ids);
    }

    fn fail_backfill(&self, threads: &[OrchestrationV2ThreadShell]) {
        let ids: Vec<ThreadId> = threads.iter().map(|thread| thread.id.clone()).collect();
        self.fail_backfill_ids(&ids);
    }

    fn finish_backfill_ids(&self, ids: &[ThreadId]) {
        let mut pending = self.pending_backfill.lock();
        for id in ids {
            pending.remove(id);
        }
    }

    /// One failed attempt for each thread still owed backfill. The last attempt drops the entry.
    fn fail_backfill_ids(&self, ids: &[ThreadId]) {
        let mut pending = self.pending_backfill.lock();
        for id in ids {
            let Some(remaining) = pending.get(id).copied() else {
                continue;
            };
            if remaining <= 1 {
                pending.remove(id);
            } else {
                pending.insert(id.clone(), remaining - 1);
            }
        }
    }
}

struct PlannedUpdate {
    thread: OrchestrationV2ThreadShell,
    branch_pull_request: Option<ThreadLinkedPullRequest>,
    replacement: Option<ThreadLinkedPullRequest>,
}

fn link_as_reference(link: &ThreadLinkedPullRequest) -> PullRequestRef {
    PullRequestRef {
        project_id: link.project_id.clone(),
        host: None,
        expected_account_id: None,
        allow_stale: None,
        repository: link.repository.clone(),
        number: link.number,
    }
}

#[cfg(test)]
mod tests;
