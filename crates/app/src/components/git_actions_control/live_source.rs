//! Live [`GitActionsBackend`] over the runtime's git manager and source control
//! services (`Commands::git_*`).
//!
//! Everything runs on the tokio runtime through the command bus. Results land
//! in per-checkout caches the view reads synchronously, and every landed result
//! repaints the windows (the backend has no entity to notify). Upstream gets the
//! same effect from `vcsEnvironment.status` subscriptions; here the owners ask
//! for a refresh on mount, on a new checkout, on window focus, on a slow poll and
//! after every action.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt as _;
use gpui_kit::{App, Task};
use turin_contracts::git::{
    GitActionProgressEvent, GitFilePath, GitRunStackedActionInput, GitRunStackedActionResult,
    VcsPullResult,
};
use turin_contracts::source_control::{
    SourceControlDiscoveryStatus, SourceControlProviderAuthStatus,
    SourceControlProviderDiscoveryItem, SourceControlProviderKind,
    SourceControlPublishRepositoryInput, SourceControlPublishRepositoryResult,
};
use turin_contracts::{ProjectId, TrimmedNonEmptyString};
use turin_review::action_state::source_control_actions::SourceControlActionKind;

use super::backend::{
    GitActionTarget, GitActionsBackend, GitStatusView, PublishProviderState,
    PublishRepositoryInput, RunStackedActionInput, VcsActionState,
};
use crate::app_runtime::write::bus::CommandBus;
use crate::timestamp_format::now_ms;

/// How long a provider discovery stays fresh. It shells out to `gh`.
const PROVIDER_DISCOVERY_TTL_MS: i64 = 60_000;

struct Inner {
    bus: CommandBus,
    status_by_cwd: RefCell<HashMap<String, GitStatusView>>,
    /// Checkouts with a status fetch in flight.
    fetching: RefCell<HashSet<String>>,
    action_by_cwd: RefCell<HashMap<String, VcsActionState>>,
    provider: RefCell<PublishProviderState>,
    provider_checked_at_ms: RefCell<Option<i64>>,
}

#[derive(Clone)]
pub struct LiveGitActionsBackend {
    inner: Rc<Inner>,
}

fn unknown_provider() -> PublishProviderState {
    PublishProviderState {
        label: "GitHub".into(),
        ready: false,
        hint: Some("Checking GitHub...".into()),
        account: None,
        host: "github.com".into(),
    }
}

/// `getPublishProviderReadiness` plus the account and host the dialog shows.
pub fn publish_provider_state(
    providers: &[SourceControlProviderDiscoveryItem],
) -> PublishProviderState {
    let Some(github) = providers
        .iter()
        .find(|provider| provider.kind.as_str() == "github")
    else {
        return PublishProviderState {
            hint: Some(
                "Provider status unavailable. Open Settings -> Source Control and rescan.".into(),
            ),
            ..unknown_provider()
        };
    };
    let label = github.label.as_str().to_owned();
    let account = github
        .auth
        .account
        .as_ref_option()
        .map(|account| account.as_str().to_owned());
    let host = github
        .auth
        .host
        .as_ref_option()
        .map(|host| host.as_str().to_owned())
        .unwrap_or_else(|| "github.com".into());
    let (ready, hint) = if github.status != SourceControlDiscoveryStatus::Available {
        (false, Some(github.install_hint.as_str().to_owned()))
    } else if github.auth.status == SourceControlProviderAuthStatus::Unauthenticated {
        (
            false,
            Some(
                github
                    .auth
                    .detail
                    .as_ref_option()
                    .map(|detail| detail.as_str().to_owned())
                    .unwrap_or_else(|| {
                        format!(
                            "{label} is not authenticated. Open Settings -> Source Control for setup guidance."
                        )
                    }),
            ),
        )
    } else {
        (true, None)
    };
    PublishProviderState {
        label,
        ready,
        hint,
        account,
        host,
    }
}

/// Folds one progress event into the tracked action state (`vcsActionManager`).
pub fn apply_progress(state: &mut VcsActionState, event: &GitActionProgressEvent, now_ms: i64) {
    match event {
        GitActionProgressEvent::ActionStarted { .. } => {
            state.phase_started_at_ms = Some(now_ms);
        }
        GitActionProgressEvent::PhaseStarted { label, .. } => {
            state.current_label = Some(label.as_str().to_owned());
            state.phase_started_at_ms = Some(now_ms);
            state.hook_started_at_ms = None;
            state.last_output_line = None;
        }
        GitActionProgressEvent::HookStarted { hook_name, .. } => {
            state.current_label = Some(format!("Running {}...", hook_name.as_str()));
            state.hook_started_at_ms = Some(now_ms);
            state.last_output_line = None;
        }
        GitActionProgressEvent::HookOutput { text, .. } => {
            state.last_output_line = text
                .as_str()
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(|line| line.trim().to_owned());
        }
        GitActionProgressEvent::HookFinished { .. } => {
            state.hook_started_at_ms = None;
            state.last_output_line = None;
        }
        GitActionProgressEvent::ActionFinished { .. }
        | GitActionProgressEvent::ActionFailed { .. } => {}
    }
}

fn running(kind: SourceControlActionKind) -> VcsActionState {
    VcsActionState {
        is_running: true,
        operation: Some(kind.operation().to_owned()),
        current_label: Some(kind.label().to_owned()),
        last_output_line: None,
        phase_started_at_ms: Some(now_ms()),
        hook_started_at_ms: None,
    }
}

fn repaint(cx: &mut App) {
    cx.refresh_windows();
}

impl LiveGitActionsBackend {
    pub fn new(bus: CommandBus) -> Self {
        Self {
            inner: Rc::new(Inner {
                bus,
                status_by_cwd: RefCell::new(HashMap::new()),
                fetching: RefCell::new(HashSet::new()),
                action_by_cwd: RefCell::new(HashMap::new()),
                provider: RefCell::new(unknown_provider()),
                provider_checked_at_ms: RefCell::new(None),
            }),
        }
    }

    fn fetch_status(&self, cwd: &str, cx: &mut App) {
        if !self.inner.fetching.borrow_mut().insert(cwd.to_owned()) {
            return;
        }
        let inner = self.inner.clone();
        let cwd = cwd.to_owned();
        let task = {
            let cwd = cwd.clone();
            inner.bus.task(cx, move |commands| async move {
                commands.git_status(&cwd).await
            })
        };
        cx.spawn(async move |cx| {
            let outcome = task.await;
            let view = match outcome {
                Ok(status) => GitStatusView {
                    status: Some(status),
                    error: None,
                },
                Err(error) => GitStatusView {
                    status: None,
                    error: Some(error.message),
                },
            };
            inner.fetching.borrow_mut().remove(&cwd);
            let changed = {
                let mut cache = inner.status_by_cwd.borrow_mut();
                let changed = cache.get(&cwd).is_none_or(|previous| {
                    previous.status != view.status || previous.error != view.error
                });
                cache.insert(cwd, view);
                changed
            };
            if changed {
                cx.update(repaint);
            }
        })
        .detach();
    }

    fn fetch_provider(&self, cx: &mut App) {
        let now = now_ms();
        {
            let mut checked = self.inner.provider_checked_at_ms.borrow_mut();
            if checked.is_some_and(|at| now - at < PROVIDER_DISCOVERY_TTL_MS) {
                return;
            }
            *checked = Some(now);
        }
        let inner = self.inner.clone();
        let task = inner.bus.task(cx, |commands| async move {
            Ok(commands.source_control_providers().await)
        });
        cx.spawn(async move |cx| {
            let Ok(providers) = task.await else {
                // Not started yet: look again on the next refresh.
                *inner.provider_checked_at_ms.borrow_mut() = None;
                return;
            };
            *inner.provider.borrow_mut() = publish_provider_state(&providers);
            cx.update(repaint);
        })
        .detach();
    }

    fn set_action(&self, cwd: &str, state: Option<VcsActionState>) {
        let mut actions = self.inner.action_by_cwd.borrow_mut();
        match state {
            Some(state) => {
                actions.insert(cwd.to_owned(), state);
            }
            None => {
                actions.remove(cwd);
            }
        }
    }

    /// Runs `work` on the runtime with the checkout's action state set to
    /// `kind` until it settles, then refreshes the checkout's status.
    fn run_tracked<T: Send + 'static>(
        &self,
        cwd: &str,
        kind: SourceControlActionKind,
        cx: &mut App,
        work: impl FnOnce(
            crate::app_runtime::write::commands::Commands,
        ) -> futures::future::BoxFuture<
            'static,
            Result<T, crate::app_runtime::write::commands::CommandError>,
        > + Send
        + 'static,
    ) -> Task<Result<T, String>> {
        self.set_action(cwd, Some(running(kind)));
        let task = self.inner.bus.task(cx, work);
        let backend = self.clone();
        let cwd = cwd.to_owned();
        cx.spawn(async move |cx| {
            let outcome = task.await;
            backend.set_action(&cwd, None);
            cx.update(|cx| {
                backend.fetch_status(&cwd, cx);
                repaint(cx);
            });
            outcome.map_err(|error| error.message)
        })
    }
}

impl GitActionsBackend for LiveGitActionsBackend {
    fn status(&self, cwd: &str, _: &App) -> GitStatusView {
        self.inner
            .status_by_cwd
            .borrow()
            .get(cwd)
            .cloned()
            .unwrap_or_default()
    }

    fn action_state(&self, cwd: &str, _: &App) -> VcsActionState {
        self.inner
            .action_by_cwd
            .borrow()
            .get(cwd)
            .cloned()
            .unwrap_or_default()
    }

    fn refresh_status(&self, cwd: &str, cx: &mut App) {
        self.fetch_status(cwd, cx);
        self.fetch_provider(cx);
    }

    fn run_stacked_action(
        &self,
        target: &GitActionTarget,
        input: RunStackedActionInput,
        cx: &mut App,
    ) -> Task<Result<GitRunStackedActionResult, String>> {
        let cwd = target.cwd.clone();
        let request = match stacked_action_input(&cwd, &input) {
            Ok(request) => request,
            Err(message) => return Task::ready(Err(message)),
        };
        let (events, mut feed) = futures::channel::mpsc::unbounded::<GitActionProgressEvent>();
        let progress: Arc<dyn Fn(GitActionProgressEvent) + Send + Sync> = Arc::new(move |event| {
            let _ = events.unbounded_send(event);
        });
        // The feed ends when the action drops its progress handle.
        {
            let backend = self.clone();
            let cwd = cwd.clone();
            cx.spawn(async move |cx| {
                while let Some(event) = feed.next().await {
                    {
                        let mut actions = backend.inner.action_by_cwd.borrow_mut();
                        if let Some(state) = actions.get_mut(&cwd) {
                            apply_progress(state, &event, now_ms());
                        }
                    }
                    cx.update(repaint);
                }
            })
            .detach();
        }
        self.run_tracked(
            &cwd,
            SourceControlActionKind::RunStackedAction,
            cx,
            move |commands| {
                Box::pin(async move {
                    commands
                        .git_run_stacked_action(request, Some(progress))
                        .await
                })
            },
        )
    }

    fn pull(&self, target: &GitActionTarget, cx: &mut App) -> Task<Result<VcsPullResult, String>> {
        let cwd = target.cwd.clone();
        self.run_tracked(
            &cwd.clone(),
            SourceControlActionKind::Pull,
            cx,
            move |commands| Box::pin(async move { commands.git_pull(&cwd).await }),
        )
    }

    fn init(&self, target: &GitActionTarget, cx: &mut App) -> Task<Result<(), String>> {
        let cwd = target.cwd.clone();
        self.run_tracked(
            &cwd.clone(),
            SourceControlActionKind::Init,
            cx,
            move |commands| Box::pin(async move { commands.git_init(&cwd).await }),
        )
    }

    fn publish_provider(&self, _: &App) -> PublishProviderState {
        self.inner.provider.borrow().clone()
    }

    fn publish_repository(
        &self,
        target: &GitActionTarget,
        input: PublishRepositoryInput,
        cx: &mut App,
    ) -> Task<Result<SourceControlPublishRepositoryResult, String>> {
        let cwd = target.cwd.clone();
        let request = (|| -> Result<SourceControlPublishRepositoryInput, String> {
            Ok(SourceControlPublishRepositoryInput {
                cwd: TrimmedNonEmptyString::new(cwd.clone()).map_err(|error| error.to_string())?,
                provider: SourceControlProviderKind::new("github")
                    .map_err(|error| error.to_string())?,
                repository: TrimmedNonEmptyString::new(input.repository.clone())
                    .map_err(|error| error.to_string())?,
                visibility: input.visibility,
                remote_name: TrimmedNonEmptyString::new(input.remote_name.clone()).ok(),
                protocol: Some(input.protocol),
            })
        })();
        let request = match request {
            Ok(request) => request,
            Err(message) => return Task::ready(Err(message)),
        };
        self.run_tracked(
            &cwd,
            SourceControlActionKind::PublishRepository,
            cx,
            move |commands| Box::pin(async move { commands.git_publish_repository(request).await }),
        )
    }

    fn update_thread_branch(
        &self,
        target: &GitActionTarget,
        branch: Option<String>,
        _expected_branch: Option<String>,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        let thread_id = target.thread_id.clone();
        let task = self.inner.bus.task(cx, move |commands| async move {
            commands
                .update_thread_branch(&thread_id, branch, None, false)
                .await
        });
        cx.spawn(async move |_| task.await.map_err(|error| error.message))
    }

    /// `openChangedFileInEditor`: the changed file, joined onto the checkout,
    /// opens in the preferred editor.
    fn open_in_editor(&self, path: String, cwd: &str, cx: &mut App) -> Task<Result<(), String>> {
        let target = crate::diff::diff_file_actions::resolve_path_link_target(&path, cwd);
        let opened = crate::hooks::open_in_editor::EditorLauncherService::global().spawn_open(
            self.inner.bus.handle(),
            target,
            cx,
        );
        cx.spawn(async move |_| opened.await.map(drop))
    }

    fn open_link(&self, url: String, cx: &mut App) {
        cx.open_url(&url);
    }
}

/// `runImmediateGitAction.run`'s input, as the runtime takes it.
fn stacked_action_input(
    cwd: &str,
    input: &RunStackedActionInput,
) -> Result<GitRunStackedActionInput, String> {
    let text = |value: &str| TrimmedNonEmptyString::new(value).map_err(|error| error.to_string());
    let file_paths = match &input.file_paths {
        Some(paths) => Some(
            paths
                .iter()
                .map(|path| GitFilePath::new(path.clone()))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => None,
    };
    Ok(GitRunStackedActionInput {
        action_id: text(&input.action_id)?,
        cwd: text(cwd)?,
        action: input.action,
        commit_message: input
            .commit_message
            .as_deref()
            .and_then(|message| TrimmedNonEmptyString::new(message).ok()),
        feature_branch: input.feature_branch.then_some(true),
        file_paths,
        thread_id: input.thread_id.clone(),
        project_id: input
            .project_id
            .as_deref()
            .and_then(|id| ProjectId::new(id).ok()),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use turin_contracts::git::GitStackedAction;

    use super::*;

    fn provider(value: serde_json::Value) -> SourceControlProviderDiscoveryItem {
        serde_json::from_value(value).expect("provider item decodes")
    }

    fn github(status: &str, auth: serde_json::Value) -> SourceControlProviderDiscoveryItem {
        provider(json!({
            "kind": "github",
            "label": "GitHub",
            "executable": "gh",
            "status": status,
            "version": { "_tag": "None" },
            "installHint": "Install the GitHub CLI (`gh`).",
            "detail": { "_tag": "None" },
            "auth": auth,
        }))
    }

    fn auth(status: &str) -> serde_json::Value {
        json!({
            "status": status,
            "account": { "_tag": "Some", "value": "octo" },
            "host": { "_tag": "Some", "value": "github.example.com" },
            "detail": { "_tag": "None" },
        })
    }

    #[test]
    fn publish_is_ready_for_an_authenticated_github() {
        let state = publish_provider_state(&[github("available", auth("authenticated"))]);
        assert!(state.ready);
        assert_eq!(state.hint, None);
        assert_eq!(state.account.as_deref(), Some("octo"));
        assert_eq!(state.host, "github.example.com");
    }

    #[test]
    fn publish_is_blocked_with_a_hint_until_github_is_installed_and_signed_in() {
        let missing = publish_provider_state(&[github("missing", auth("unknown"))]);
        assert!(!missing.ready);
        assert_eq!(
            missing.hint.as_deref(),
            Some("Install the GitHub CLI (`gh`).")
        );

        let signed_out = publish_provider_state(&[github("available", auth("unauthenticated"))]);
        assert!(!signed_out.ready);
        assert!(signed_out.hint.unwrap().contains("not authenticated"));

        let undiscovered = publish_provider_state(&[]);
        assert!(!undiscovered.ready);
        assert!(undiscovered.hint.unwrap().contains("rescan"));
    }

    #[test]
    fn progress_events_drive_the_running_label_and_hook_output() {
        let mut state = running(SourceControlActionKind::RunStackedAction);
        let event = |value: serde_json::Value| -> GitActionProgressEvent {
            let mut base = json!({ "actionId": "a", "cwd": "/r", "action": "commit" });
            base.as_object_mut()
                .unwrap()
                .extend(value.as_object().unwrap().clone());
            serde_json::from_value(base).expect("event decodes")
        };
        apply_progress(
            &mut state,
            &event(json!({ "kind": "phase_started", "phase": "commit", "label": "Committing..." })),
            10,
        );
        assert_eq!(state.current_label.as_deref(), Some("Committing..."));
        assert_eq!(state.phase_started_at_ms, Some(10));
        apply_progress(
            &mut state,
            &event(json!({ "kind": "hook_started", "hookName": "pre-commit" })),
            20,
        );
        assert_eq!(
            state.current_label.as_deref(),
            Some("Running pre-commit...")
        );
        assert_eq!(state.hook_started_at_ms, Some(20));
        apply_progress(
            &mut state,
            &event(json!({
                "kind": "hook_output", "hookName": "pre-commit", "stream": "stdout",
                "text": "checking\nall good\n",
            })),
            21,
        );
        assert_eq!(state.last_output_line.as_deref(), Some("all good"));
        apply_progress(
            &mut state,
            &event(
                json!({ "kind": "hook_finished", "hookName": "pre-commit", "exitCode": 0, "durationMs": 5 }),
            ),
            30,
        );
        assert_eq!(state.hook_started_at_ms, None);
        assert_eq!(state.last_output_line, None);
    }

    #[test]
    fn the_stacked_action_request_keeps_the_dialog_inputs() {
        let request = stacked_action_input(
            "/repo",
            &RunStackedActionInput {
                action_id: "run-1".into(),
                action: GitStackedAction::Commit,
                commit_message: Some("feat: x".into()),
                feature_branch: true,
                file_paths: Some(vec!["a.rs".into()]),
                thread_id: None,
                project_id: None,
            },
        )
        .expect("valid request");
        assert_eq!(request.cwd.as_str(), "/repo");
        assert_eq!(request.commit_message.unwrap().as_str(), "feat: x");
        assert_eq!(request.feature_branch, Some(true));
        assert_eq!(request.file_paths.unwrap()[0].0, "a.rs");
    }
}
