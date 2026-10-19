//! The seams between the first-run gate / welcome wizard and the services
//! behind them.
//!
//! Upstream reads Effect atoms (client settings, environment shells, the
//! primary server's welcome, provider snapshots, agent-session scan and
//! import commands). Turin takes these traits instead; `MockOnboardingBackend`
//! backs the screenshot harness and the tests, and T22 wires the real
//! settings store, orchestrator, provider registry, `AgentSessionScanner` /
//! `AgentSessionImporter` (turin-projects) and `T3Import` (turin-persistence).

use std::path::PathBuf;

use gpui_kit::{App, Task};
use serde_json::Value;
use turin_contracts::agent_sessions::AgentSessionScanResult;
use turin_contracts::environment::ExecutionEnvironmentPlatformOs;
use turin_contracts::{EnvironmentId, ProjectId};
use turin_persistence::{ImportReport, T3ImportError};

use crate::onboarding::first_run_logic::{BootstrapStatus, FirstRunProject, FirstRunThread};
use crate::onboarding::project_import_logic::OnboardingKnownProject;
use crate::onboarding::provider_readiness_logic::OnboardingProvider;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydrationStatus {
    Loading,
    Ready,
    Failed,
    Retrying,
}

/// Everything `FirstRunGate` reads to reach a decision.
#[derive(Clone, Debug)]
pub struct FirstRunEvidence {
    pub hydration: HydrationStatus,
    /// `onboardingCompletedAt !== null`.
    pub completed: bool,
    /// `useAllEnvironmentShellsBootstrapped`.
    pub bootstrapped: bool,
    /// `primaryShellLiveAtom`.
    pub primary_shell_live: bool,
    /// `workspaceEvidenceLiveAtom`.
    pub workspace_evidence_live: bool,
    pub catalog_ready: bool,
    pub server_config_available: bool,
    pub welcome_received: bool,
    pub bootstrap_status: Option<BootstrapStatus>,
    pub server_cwd: Option<String>,
    pub primary_environment_id: Option<String>,
    pub bootstrap_project_id: Option<String>,
    pub bootstrap_thread_id: Option<String>,
    pub bootstrap_project_created: Option<bool>,
    pub bootstrap_thread_created: Option<bool>,
    pub projects: Vec<FirstRunProject>,
    pub threads: Vec<FirstRunThread>,
}

pub trait FirstRunBackend {
    fn evidence(&self) -> FirstRunEvidence;

    /// `useCompleteOnboarding`: hydrate settings, then persist
    /// `onboardingCompletedAt`. `Err` is a failed save.
    fn complete_onboarding(&self, cx: &mut App) -> Task<Result<(), ()>>;

    /// `ensureClientSettingsHydrated` (the recovery screen's Retry). Resolves once the read
    /// settled and `evidence()` reflects it, so the gate can re-render.
    fn retry_settings(&self, cx: &mut App) -> Task<()>;

    /// The recovery screen's Reload for a stalled connection: upstream reloads the window, which
    /// re-reads the settings and re-evaluates the evidence. Resolves once `evidence()` is fresh.
    fn reload(&self, cx: &mut App) -> Task<()>;
}

#[derive(Clone, Debug)]
pub struct CreateProjectInput {
    pub project_id: ProjectId,
    pub title: String,
    pub workspace_root: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentSessionImportCounts {
    pub imported: u64,
    pub skipped: u64,
}

/// A T3 Code database the wizard offers to import (`~/.t3/userdata/statev2.sqlite`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct T3DatabaseCandidate {
    pub path: PathBuf,
}

pub trait OnboardingBackend {
    /// The primary environment's provider snapshots; `None` until they arrive.
    fn providers(&self) -> Option<Vec<OnboardingProvider>>;
    /// Re-probe providers (single-flighted).
    fn refresh_providers(&self, cx: &mut App) -> Task<()>;
    fn platform(&self) -> ExecutionEnvironmentPlatformOs;
    /// The environment's label, shown above its agents.
    fn machine_label(&self) -> String;
    /// `settings.providerInstances[instanceId]?.config`.
    fn instance_config(&self, instance_id: &str) -> Option<Value>;

    /// `agentSessionScan`: directories Claude Code or Codex has run in.
    fn scan_projects(&self, cx: &mut App) -> Task<Result<AgentSessionScanResult, String>>;
    /// `readProjects()`: the client's current projects.
    fn known_projects(&self) -> Vec<OnboardingKnownProject>;
    fn environment_id(&self) -> EnvironmentId;
    /// `projectEnvironment.create`.
    fn create_project(&self, input: CreateProjectInput, cx: &mut App) -> Task<Result<(), String>>;
    /// `agentSessionImport`.
    fn import_agent_sessions(
        &self,
        project_id: ProjectId,
        expected_workspace_root: String,
        cx: &mut App,
    ) -> Task<Result<AgentSessionImportCounts, String>>;

    /// A T3 Code database the user can adopt, when one exists and Turin has none yet.
    fn t3_database(&self) -> Option<T3DatabaseCandidate>;
    /// `T3Import::import_into`: snapshot, migrate, verify and adopt.
    fn import_t3_database(&self, cx: &mut App) -> Task<Result<ImportReport, T3ImportError>>;

    fn complete_onboarding(&self, cx: &mut App) -> Task<Result<(), ()>>;
    fn copy_to_clipboard(&self, text: &str, cx: &mut App);
}
