//! Live [`FirstRunBackend`] and [`OnboardingBackend`]: client settings, the
//! client store shell, the command bus, provider registry snapshots, agent
//! session scan/import, and T3 adoption. Replaces `mock_source` in the running
//! app; the mock stays for screenshots (`TURIN_SHOT`, `TURIN_FIRST_RUN`, …).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{App, Entity, Task};
use turin_contracts::agent_sessions::AgentSessionScanResult;
use turin_contracts::environment::ExecutionEnvironmentPlatformOs;
use turin_contracts::settings::{ClientSettingsPatch, ServerSettings};
use turin_contracts::{EnvironmentId, ProjectId};
use turin_persistence::{ImportReport, T3Import, T3ImportError};

use super::backend::{
    AgentSessionImportCounts, CreateProjectInput, FirstRunBackend, FirstRunEvidence,
    OnboardingBackend, T3DatabaseCandidate,
};
use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::CommandError;
use crate::onboarding::agent_session_live::AgentSessionLive;
use crate::onboarding::live_evidence::{first_run_evidence, known_projects_from_shell};
use crate::onboarding::live_providers::{
    instance_config_from_map, onboarding_providers_from_registry,
};
use crate::onboarding::project_import_logic::OnboardingKnownProject;
use crate::onboarding::provider_readiness_logic::OnboardingProvider;
use crate::settings_logic::use_settings::{
    ensure_client_settings_hydrated, get_client_settings, try_persist_client_settings_patch,
};
use crate::state::local_environment_id;
use crate::state::store::ClientStore;

pub struct LiveOnboarding;

impl LiveOnboarding {
    /// Binds provider snapshots and returns the backend the first-run gate and
    /// welcome wizard share.
    pub fn start(
        client: Entity<ClientStore>,
        bus: CommandBus,
        data_dir: PathBuf,
        cx: &mut App,
    ) -> Rc<LiveOnboardingBackend> {
        let providers = Rc::new(RefCell::new(None::<Vec<OnboardingProvider>>));
        let server_settings = Arc::new(Mutex::new(ServerSettings::default()));
        let evidence = Rc::new(RefCell::new(first_run_evidence(
            client.read(cx).state(),
            &get_client_settings(),
            bus.commands().is_ok(),
        )));
        let known_projects = Rc::new(RefCell::new(known_projects_from_shell(
            client.read(cx).state(),
        )));
        let refresh_evidence = {
            let evidence = evidence.clone();
            let known_projects = known_projects.clone();
            let client = client.clone();
            let bus = bus.clone();
            Rc::new(move |cx: &mut App| {
                let state = client.read(cx).state();
                *evidence.borrow_mut() =
                    first_run_evidence(state, &get_client_settings(), bus.commands().is_ok());
                *known_projects.borrow_mut() = known_projects_from_shell(state);
            })
        };
        refresh_evidence(cx);
        cx.observe(&client, {
            let refresh_evidence = refresh_evidence.clone();
            move |_, cx| refresh_evidence(cx)
        })
        .detach();
        let sync_providers = {
            let providers = providers.clone();
            let bus = bus.clone();
            Rc::new(move || {
                let list = bus.providers().borrow().clone();
                *providers.borrow_mut() = Some(onboarding_providers_from_registry(list.as_ref()));
            })
        };
        sync_providers();
        let mut receiver = bus.providers();
        let watch = sync_providers.clone();
        cx.spawn(async move |_| {
            while receiver.changed().await.is_ok() {
                watch();
            }
        })
        .detach();
        let backend = Rc::new(LiveOnboardingBackend {
            bus,
            data_dir,
            providers,
            server_settings,
            evidence,
            known_projects,
            refresh_evidence,
            refresh_in_flight: Arc::new(AtomicBool::new(false)),
        });
        backend.warm_settings();
        backend
    }
}

pub struct LiveOnboardingBackend {
    bus: CommandBus,
    data_dir: PathBuf,
    providers: Rc<RefCell<Option<Vec<OnboardingProvider>>>>,
    server_settings: Arc<Mutex<ServerSettings>>,
    evidence: Rc<RefCell<FirstRunEvidence>>,
    known_projects: Rc<RefCell<Vec<OnboardingKnownProject>>>,
    /// Re-reads the evidence cell. Hydration status lives outside the client store, so nothing
    /// else refreshes it when a settings read settles.
    refresh_evidence: Rc<dyn Fn(&mut App)>,
    refresh_in_flight: Arc<AtomicBool>,
}

impl LiveOnboardingBackend {
    fn warm_settings(&self) {
        let server_settings = Arc::clone(&self.server_settings);
        self.bus.spawn(
            move |commands| async move {
                commands
                    .runtime()
                    .server_settings
                    .get_settings()
                    .await
                    .map_err(|error| CommandError::new(error.to_string()))
            },
            move |result| {
                if let Ok(settings) = result
                    && let Ok(mut guard) = server_settings.lock()
                {
                    *guard = settings;
                }
            },
        );
    }

    /// A T3 database to offer, while Turin holds no projects or threads of its own. The Turin
    /// database file always exists by now (the runtime creates it at startup), so the check is
    /// on content: the shell evidence, plus a copy already staged for the next start.
    fn t3_source_path(&self) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let candidate = PathBuf::from(home).join(".t3/userdata/statev2.sqlite");
        if !candidate.is_file() {
            return None;
        }
        let staged = self
            .data_dir
            .join(turin_persistence::STAGED_IMPORT_DIR)
            .join(turin_persistence::TURIN_DATABASE_FILE)
            .is_file();
        t3_import_offered(
            self.evidence.borrow().projects.len(),
            self.evidence.borrow().threads.len(),
            self.known_projects.borrow().len(),
            staged,
        )
        .then_some(candidate)
    }
}

/// The card shows only before the user has anything in Turin; a staged import keeps it up so the
/// result stays on screen until the restart that loads it.
fn t3_import_offered(projects: usize, threads: usize, known_projects: usize, staged: bool) -> bool {
    staged || (projects == 0 && threads == 0 && known_projects == 0)
}

impl FirstRunBackend for LiveOnboardingBackend {
    fn evidence(&self) -> FirstRunEvidence {
        self.evidence.borrow().clone()
    }

    fn complete_onboarding(&self, cx: &mut App) -> Task<Result<(), ()>> {
        let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        cx.spawn(async move |_| {
            try_persist_client_settings_patch(ClientSettingsPatch {
                onboarding_completed_at: Some(Some(stamp)),
                ..Default::default()
            })
            .await
            .map_err(|error| {
                tracing::error!(%error, "onboarding completion persist failed");
            })
            .map_err(|_| ())
        })
    }

    fn retry_settings(&self, cx: &mut App) -> Task<()> {
        let refresh_evidence = self.refresh_evidence.clone();
        cx.spawn(async move |cx| {
            let _ = ensure_client_settings_hydrated().await;
            cx.update(|cx| refresh_evidence(cx));
        })
    }

    fn reload(&self, cx: &mut App) -> Task<()> {
        // No window reload here: re-read the settings and re-evaluate the evidence.
        self.retry_settings(cx)
    }
}

impl OnboardingBackend for LiveOnboardingBackend {
    fn providers(&self) -> Option<Vec<OnboardingProvider>> {
        self.providers.borrow().clone()
    }

    fn refresh_providers(&self, cx: &mut App) -> Task<()> {
        if self
            .refresh_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Task::ready(());
        }
        let bus = self.bus.clone();
        let server_settings = Arc::clone(&self.server_settings);
        let refresh_in_flight = Arc::clone(&self.refresh_in_flight);
        let task = bus.task(cx, |commands| async move {
            let runtime = commands.runtime();
            runtime.providers.registry.refresh(None).await;
            runtime
                .server_settings
                .get_settings()
                .await
                .map_err(|error| CommandError::new(error.to_string()))
        });
        cx.spawn(async move |_| {
            if let Ok(settings) = task.await
                && let Ok(mut guard) = server_settings.lock()
            {
                *guard = settings;
            }
            refresh_in_flight.store(false, Ordering::SeqCst);
        })
        .detach();
        Task::ready(())
    }

    fn platform(&self) -> ExecutionEnvironmentPlatformOs {
        if cfg!(target_os = "macos") {
            ExecutionEnvironmentPlatformOs::Darwin
        } else if cfg!(target_os = "windows") {
            ExecutionEnvironmentPlatformOs::Windows
        } else if cfg!(target_os = "linux") {
            ExecutionEnvironmentPlatformOs::Linux
        } else {
            ExecutionEnvironmentPlatformOs::Unknown
        }
    }

    fn machine_label(&self) -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "This device".into())
    }

    fn instance_config(&self, instance_id: &str) -> Option<serde_json::Value> {
        self.server_settings.lock().ok().and_then(|settings| {
            instance_config_from_map(&settings.provider_instances, instance_id)
        })
    }

    fn scan_projects(&self, cx: &mut App) -> Task<Result<AgentSessionScanResult, String>> {
        let bus = self.bus.clone();
        let data_dir = self.data_dir.clone();
        let task = bus.task(cx, move |commands| async move {
            let live = AgentSessionLive::new(commands.runtime(), &data_dir);
            live.scan().await.map_err(CommandError::new)
        });
        cx.spawn(async move |_| task.await.map_err(|error| error.message))
    }

    fn known_projects(&self) -> Vec<OnboardingKnownProject> {
        self.known_projects.borrow().clone()
    }

    fn environment_id(&self) -> EnvironmentId {
        EnvironmentId::new(local_environment_id()).expect("static id")
    }

    fn create_project(&self, input: CreateProjectInput, cx: &mut App) -> Task<Result<(), String>> {
        let bus = self.bus.clone();
        let project_id = input.project_id;
        let title = input.title;
        let root = input.workspace_root;
        let task = bus.task(cx, move |commands| async move {
            commands
                .create_project_with_id(project_id, &title, &root)
                .await?;
            Ok(())
        });
        cx.spawn(async move |_| task.await.map_err(|error| error.message))
    }

    fn import_agent_sessions(
        &self,
        project_id: ProjectId,
        expected_workspace_root: String,
        cx: &mut App,
    ) -> Task<Result<AgentSessionImportCounts, String>> {
        let bus = self.bus.clone();
        let data_dir = self.data_dir.clone();
        let task = bus.task(cx, move |commands| async move {
            let live = AgentSessionLive::new(commands.runtime(), &data_dir);
            live.import(project_id, expected_workspace_root)
                .await
                .map(|(imported, skipped)| AgentSessionImportCounts { imported, skipped })
                .map_err(CommandError::new)
        });
        cx.spawn(async move |_| task.await.map_err(|error| error.message))
    }

    fn t3_database(&self) -> Option<T3DatabaseCandidate> {
        self.t3_source_path()
            .map(|path| T3DatabaseCandidate { path })
    }

    fn import_t3_database(&self, cx: &mut App) -> Task<Result<ImportReport, T3ImportError>> {
        let Some(source) = self.t3_source_path() else {
            return Task::ready(Err(T3ImportError::NotV2));
        };
        let dest = self.data_dir.clone();
        // The runtime has the database open, so the verified copy is staged and swapped in at the
        // next start (`Stores::open`).
        let task = self
            .bus
            .handle()
            .spawn_blocking(move || T3Import::stage_into(source, dest));
        cx.spawn(async move |_| {
            task.await.unwrap_or_else(|error| {
                Err(T3ImportError::Io {
                    context: "stage the T3 import".to_owned(),
                    source: std::io::Error::other(error),
                })
            })
        })
    }

    fn complete_onboarding(&self, cx: &mut App) -> Task<Result<(), ()>> {
        FirstRunBackend::complete_onboarding(self, cx)
    }

    fn copy_to_clipboard(&self, text: &str, cx: &mut App) {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::t3_import_offered;

    #[test]
    fn the_t3_card_is_offered_to_an_empty_turin_whose_database_file_already_exists() {
        // Nothing here looks at the database file: the runtime creates it before the wizard.
        assert!(t3_import_offered(0, 0, 0, false));
    }

    #[test]
    fn the_t3_card_is_hidden_once_turin_has_projects_or_threads() {
        assert!(!t3_import_offered(1, 0, 0, false));
        assert!(!t3_import_offered(0, 1, 0, false));
        assert!(!t3_import_offered(0, 0, 1, false));
    }

    #[test]
    fn a_staged_import_keeps_the_card_up() {
        assert!(t3_import_offered(2, 3, 2, true));
    }
}
