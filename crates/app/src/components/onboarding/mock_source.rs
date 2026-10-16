//! Mock backend for the first-run gate and the welcome wizard (T22 wires the
//! live services). `TURIN_FIRST_RUN=fresh|pending|stalled|settings-failed`
//! picks the gate scenario (default: onboarding completed);
//! `TURIN_WIZARD_PROVIDERS=ready|mixed|install|signin` and
//! `TURIN_WIZARD_T3=found|none|error` shape the wizard's steps.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, Task};
use serde_json::{Value, json};
use turin_contracts::agent_sessions::AgentSessionScanResult;
use turin_contracts::environment::ExecutionEnvironmentPlatformOs;
use turin_contracts::{EnvironmentId, ProjectId};
use turin_persistence::{EventReadReport, ImportPlan, ImportReport, T3ImportError};

use super::backend::{
    AgentSessionImportCounts, CreateProjectInput, FirstRunBackend, FirstRunEvidence,
    HydrationStatus, OnboardingBackend, T3DatabaseCandidate,
};
use crate::onboarding::first_run_logic::BootstrapStatus;
use crate::onboarding::project_import_logic::OnboardingKnownProject;
use crate::onboarding::provider_readiness_logic::{
    OnboardingProvider, ProviderAuthStatus, ProviderSummary,
};
use crate::state::helpers::provider_types::ServerProviderState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstRunScenario {
    Completed,
    Fresh,
    Pending,
    Stalled,
    SettingsFailed,
}

impl FirstRunScenario {
    pub fn from_env() -> Self {
        match std::env::var("TURIN_FIRST_RUN").as_deref() {
            Ok("fresh") => Self::Fresh,
            Ok("pending") => Self::Pending,
            Ok("stalled") => Self::Stalled,
            Ok("settings-failed") => Self::SettingsFailed,
            _ => Self::Completed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderScenario {
    Ready,
    Mixed,
    Install,
    SignIn,
}

struct State {
    scenario: FirstRunScenario,
    completed: bool,
    providers: ProviderScenario,
    t3: Option<PathBuf>,
    t3_error: bool,
    created: Vec<OnboardingKnownProject>,
}

pub struct MockOnboardingBackend {
    state: Rc<RefCell<State>>,
}

impl MockOnboardingBackend {
    pub fn new(scenario: FirstRunScenario) -> Self {
        let providers = match std::env::var("TURIN_WIZARD_PROVIDERS").as_deref() {
            Ok("ready") => ProviderScenario::Ready,
            Ok("install") => ProviderScenario::Install,
            Ok("signin") => ProviderScenario::SignIn,
            _ => ProviderScenario::Mixed,
        };
        let (t3, t3_error) = match std::env::var("TURIN_WIZARD_T3").as_deref() {
            Ok("none") => (None, false),
            Ok("error") => (
                Some(PathBuf::from("/Users/demo/.t3/userdata/statev2.sqlite")),
                true,
            ),
            _ => (
                Some(PathBuf::from("/Users/demo/.t3/userdata/statev2.sqlite")),
                false,
            ),
        };
        Self {
            state: Rc::new(RefCell::new(State {
                scenario,
                completed: scenario == FirstRunScenario::Completed,
                providers,
                t3,
                t3_error,
                created: Vec::new(),
            })),
        }
    }

    pub fn shared(scenario: FirstRunScenario) -> Rc<Self> {
        Rc::new(Self::new(scenario))
    }
}

#[allow(clippy::too_many_arguments)]
fn provider(
    instance: &str,
    driver: &str,
    display: Option<&str>,
    installed: bool,
    status: ServerProviderState,
    auth: ProviderAuthStatus,
    headline: &str,
    detail: Option<&str>,
) -> OnboardingProvider {
    OnboardingProvider {
        instance_id: instance.to_owned(),
        driver: driver.to_owned(),
        display_name: display.map(str::to_owned),
        enabled: true,
        installed,
        status,
        auth_status: auth,
        summary: ProviderSummary {
            headline: headline.to_owned(),
            detail: detail.map(str::to_owned),
        },
    }
}

fn candidates() -> AgentSessionScanResult {
    let candidate =
        |path: &str, title: &str, sources: &[&str], threads: u64, days_ago: i64, git: Value| {
            let last = (chrono::Utc::now() - chrono::Duration::days(days_ago))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            json!({
                "path": path, "title": title, "sources": sources, "threadCount": threads,
                "lastActiveAt": last, "alreadyImported": false, "git": git,
            })
        };
    let github = |repo: &str| json!({ "remoteKey": format!("github.com/{}", repo.to_lowercase()), "repository": repo });
    let local = json!({ "remoteKey": null, "repository": null });
    serde_json::from_value(json!({
        "candidates": [
            candidate("/Users/demo/code/turin", "turin", &["claudeAgent", "codex"], 79, 1, github("Verassitnh/turin")),
            candidate("/Users/demo/code/clones/turin-2", "turin-2", &["claudeAgent"], 13, 9, github("Verassitnh/turin")),
            candidate("/Users/demo/code/t3code", "t3code", &["codex"], 295, 2, github("pingdotgg/t3code")),
            candidate("/Users/demo/code/samwise", "samwise", &["claudeAgent"], 41, 4, github("samwise/app")),
            candidate("/Users/demo/code/scratch-repo", "scratch-repo", &["codex"], 3, 12, local),
            candidate("/Users/demo/code/old-experiment", "old-experiment", &["claudeAgent"], 6, 120, github("demo/old-experiment")),
            candidate("/Users/demo/notes", "notes", &["claudeAgent"], 2, 20, Value::Null),
            candidate("/Users/demo/Downloads/tmp", "tmp", &["codex"], 1, 40, Value::Null),
        ],
        "scannedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }))
    .expect("mock scan decodes")
}

pub fn mock_import_report() -> ImportReport {
    ImportReport {
        plan: ImportPlan {
            latest_id: 61,
            recorded: vec![(61, "ProjectionThreadsV2".to_owned())],
            pending: vec![(62, "TurinTables".to_owned())],
        },
        applied: vec![(62, "TurinTables".to_owned())],
        tables: 38,
        row_counts: BTreeMap::new(),
        events: EventReadReport {
            decoded: 4120,
            by_type: BTreeMap::new(),
            unknown: 0,
            unknown_by_type: BTreeMap::new(),
            legacy_thread_rows: 0,
            legacy_by_type: BTreeMap::new(),
            agent_decoded: 15200,
            agent_unknown: 0,
        },
        projects: 14,
        active_projects: 11,
        threads: 212,
        active_threads: 187,
        v2_threads: 212,
        active_v2_threads: 187,
    }
}

impl FirstRunBackend for MockOnboardingBackend {
    fn evidence(&self) -> FirstRunEvidence {
        let state = self.state.borrow();
        let live = !matches!(
            state.scenario,
            FirstRunScenario::Pending | FirstRunScenario::Stalled
        );
        FirstRunEvidence {
            hydration: match state.scenario {
                FirstRunScenario::SettingsFailed => HydrationStatus::Failed,
                _ => HydrationStatus::Ready,
            },
            completed: state.completed,
            bootstrapped: live,
            primary_shell_live: live,
            workspace_evidence_live: live,
            catalog_ready: live,
            server_config_available: live,
            welcome_received: live,
            bootstrap_status: Some(BootstrapStatus::Complete),
            server_cwd: Some("/Users/demo".to_owned()),
            primary_environment_id: Some("local".to_owned()),
            bootstrap_project_id: None,
            bootstrap_thread_id: None,
            bootstrap_project_created: None,
            bootstrap_thread_created: None,
            projects: Vec::new(),
            threads: Vec::new(),
        }
    }

    fn complete_onboarding(&self, cx: &mut App) -> Task<Result<(), ()>> {
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            state.borrow_mut().completed = true;
            Ok(())
        })
    }

    fn retry_settings(&self, _: &mut App) {}

    fn reload(&self, _: &mut App) {}
}

impl OnboardingBackend for MockOnboardingBackend {
    fn providers(&self) -> Option<Vec<OnboardingProvider>> {
        use ProviderAuthStatus::{Authenticated, Unauthenticated, Unknown};
        use ServerProviderState::{Error, Ready, Warning};
        let ready_codex = provider(
            "codex",
            "codex",
            Some("Codex"),
            true,
            Ready,
            Authenticated,
            "Ready",
            None,
        );
        let ready_claude = provider(
            "claudeAgent",
            "claudeAgent",
            Some("Claude Code"),
            true,
            Ready,
            Authenticated,
            "Ready",
            None,
        );
        Some(match self.state.borrow().providers {
            ProviderScenario::Ready => vec![ready_codex, ready_claude],
            ProviderScenario::Mixed => vec![
                ready_codex,
                provider(
                    "claudeAgent",
                    "claudeAgent",
                    Some("Claude Code"),
                    true,
                    Error,
                    Unauthenticated,
                    "Signed out",
                    Some("Run claude auth login"),
                ),
            ],
            ProviderScenario::Install => vec![
                provider(
                    "codex",
                    "codex",
                    Some("Codex"),
                    false,
                    Error,
                    Unknown,
                    "Not installed",
                    Some("Codex CLI was not found on PATH"),
                ),
                provider(
                    "claudeAgent",
                    "claudeAgent",
                    Some("Claude Code"),
                    false,
                    Warning,
                    Unknown,
                    "Checking",
                    None,
                ),
            ],
            ProviderScenario::SignIn => vec![
                provider(
                    "codex",
                    "codex",
                    Some("Codex"),
                    true,
                    Error,
                    Unauthenticated,
                    "Signed out",
                    Some("Run codex login"),
                ),
                provider(
                    "claudeAgent",
                    "claudeAgent",
                    Some("Claude Code"),
                    true,
                    Error,
                    Unauthenticated,
                    "Signed out",
                    Some("Run claude auth login"),
                ),
            ],
        })
    }

    fn refresh_providers(&self, cx: &mut App) -> Task<()> {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
        })
    }

    fn platform(&self) -> ExecutionEnvironmentPlatformOs {
        ExecutionEnvironmentPlatformOs::Darwin
    }

    fn machine_label(&self) -> String {
        "Jackson's MacBook Pro".to_owned()
    }

    fn instance_config(&self, _: &str) -> Option<Value> {
        None
    }

    fn scan_projects(&self, cx: &mut App) -> Task<Result<AgentSessionScanResult, String>> {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            Ok(candidates())
        })
    }

    fn known_projects(&self) -> Vec<OnboardingKnownProject> {
        self.state.borrow().created.clone()
    }

    fn environment_id(&self) -> EnvironmentId {
        EnvironmentId::new("local").expect("static id")
    }

    fn create_project(&self, input: CreateProjectInput, cx: &mut App) -> Task<Result<(), String>> {
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            state.borrow_mut().created.push(OnboardingKnownProject {
                id: input.project_id,
                environment_id: EnvironmentId::new("local").expect("static id"),
                workspace_root: input.workspace_root,
            });
            Ok(())
        })
    }

    fn import_agent_sessions(
        &self,
        _project_id: ProjectId,
        _expected_workspace_root: String,
        cx: &mut App,
    ) -> Task<Result<AgentSessionImportCounts, String>> {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            Ok(AgentSessionImportCounts {
                imported: 4,
                skipped: 0,
            })
        })
    }

    fn t3_database(&self) -> Option<T3DatabaseCandidate> {
        self.state
            .borrow()
            .t3
            .clone()
            .map(|path| T3DatabaseCandidate { path })
    }

    fn import_t3_database(&self, cx: &mut App) -> Task<Result<ImportReport, T3ImportError>> {
        let fail = self.state.borrow().t3_error;
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(900)).await;
            if fail {
                Err(T3ImportError::NewerThanTurin {
                    message: "this T3 database records migration 71_ProjectionThreadsV3, which this build does not have".to_owned(),
                })
            } else {
                Ok(mock_import_report())
            }
        })
    }

    fn complete_onboarding(&self, cx: &mut App) -> Task<Result<(), ()>> {
        FirstRunBackend::complete_onboarding(self, cx)
    }

    fn copy_to_clipboard(&self, text: &str, cx: &mut App) {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_owned()));
    }
}
