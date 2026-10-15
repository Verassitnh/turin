//! Port of upstream/packages/client-runtime/src/state/vcsAction.ts
//!
//! Kept: the action state projection, target keys, transport ids, progress normalization and
//! consumption, the error types, and a `VcsActionManager` that owns the per-target state
//! (`track`, `reset_error`, `run_stacked_action`). The Effect atom runtime, the command
//! scheduler and the permission guard around the RPC stream are cut; the caller supplies the
//! progress stream and the refs invalidation.

use std::collections::HashMap;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::{Stream, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use turin_contracts::git::{
    GitActionProgressEvent, GitActionProgressPhase, GitFilePath, GitRunStackedActionInput,
    GitRunStackedActionResult, GitStackedAction,
};
use turin_contracts::{EnvironmentId, ProjectId, ThreadId, TrimmedNonEmptyString};

/// `VcsActionOperation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsActionOperation {
    RefreshStatus,
    RunChangeRequest,
    Pull,
    SwitchRef,
    CreateRef,
    CreateWorktree,
    Init,
    PublishRepository,
    PreparePullRequestThread,
}

impl VcsActionOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RefreshStatus => "refresh_status",
            Self::RunChangeRequest => "run_change_request",
            Self::Pull => "pull",
            Self::SwitchRef => "switch_ref",
            Self::CreateRef => "create_ref",
            Self::CreateWorktree => "create_worktree",
            Self::Init => "init",
            Self::PublishRepository => "publish_repository",
            Self::PreparePullRequestThread => "prepare_pull_request_thread",
        }
    }
}

fn stacked_action_str(action: GitStackedAction) -> &'static str {
    match action {
        GitStackedAction::Commit => "commit",
        GitStackedAction::Push => "push",
        GitStackedAction::CreatePr => "create_pr",
        GitStackedAction::CommitPush => "commit_push",
        GitStackedAction::CommitPushPr => "commit_push_pr",
    }
}

fn phase_str(phase: GitActionProgressPhase) -> &'static str {
    match phase {
        GitActionProgressPhase::Branch => "branch",
        GitActionProgressPhase::Commit => "commit",
        GitActionProgressPhase::Push => "push",
        GitActionProgressPhase::Pr => "pr",
    }
}

/// JS `String.prototype.length`: UTF-16 code units.
fn js_length(value: &str) -> usize {
    value.encode_utf16().count()
}

/// `VcsActionState`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VcsActionState {
    pub is_running: bool,
    pub operation: Option<VcsActionOperation>,
    pub action_id: Option<String>,
    pub action: Option<GitStackedAction>,
    pub current_label: Option<String>,
    pub current_phase_label: Option<String>,
    pub hook_name: Option<String>,
    pub last_output_line: Option<String>,
    pub phase_started_at_ms: Option<i64>,
    pub hook_started_at_ms: Option<i64>,
    pub error: Option<String>,
}

/// `EMPTY_VCS_ACTION_STATE`.
pub fn empty_vcs_action_state() -> VcsActionState {
    VcsActionState::default()
}

/// `VcsActionTarget`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VcsActionTarget {
    pub environment_id: Option<EnvironmentId>,
    pub cwd: Option<String>,
}

/// `ResolvedVcsActionTarget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVcsActionTarget {
    pub environment_id: EnvironmentId,
    pub cwd: String,
}

/// `BeginVcsActionInput`.
#[derive(Debug, Clone)]
pub struct BeginVcsActionInput {
    pub operation: VcsActionOperation,
    pub label: String,
    pub action_id: Option<String>,
}

/// `RunVcsStackedActionInput`; `onProgress` is an argument of `run_stacked_action`.
#[derive(Debug, Clone)]
pub struct RunVcsStackedActionInput {
    pub action_id: TrimmedNonEmptyString,
    pub action: GitStackedAction,
    pub commit_message: Option<String>,
    pub feature_branch: Option<bool>,
    pub file_paths: Option<Vec<String>>,
    /// The thread the action runs beside; the server links a pull request it creates to it.
    pub thread_id: Option<ThreadId>,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Source control operation '{}' is unavailable.", .operation.as_str().replace('_', " "))]
pub struct VcsActionUnavailableError {
    pub operation: VcsActionOperation,
    pub environment_id: Option<EnvironmentId>,
    pub cwd: Option<String>,
}

/// The remote message is never copied onto the error; only its length is kept.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Source control action '{}' failed during {}.",
    stacked_action_str(*.action),
    .phase.map_or("execution", phase_str)
)]
pub struct VcsActionRemoteFailureError {
    pub action_id: String,
    pub transport_action_id: String,
    pub action: GitStackedAction,
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub phase: Option<GitActionProgressPhase>,
    pub remote_message_length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Source control action '{}' ended without a terminal result.",
    stacked_action_str(*.action)
)]
pub struct VcsActionMissingTerminalEventError {
    pub action_id: String,
    pub transport_action_id: String,
    pub action: GitStackedAction,
    pub environment_id: EnvironmentId,
    pub cwd: String,
}

/// The key itself is not copied: it can embed paths.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Invalid source control action target key ({key_length} characters).")]
pub struct VcsActionTargetKeyParseError {
    pub key_length: usize,
    pub cause: String,
}

/// `VcsActionExecutionError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VcsActionExecutionError {
    #[error(transparent)]
    RemoteFailure(#[from] VcsActionRemoteFailureError),
    #[error(transparent)]
    MissingTerminalEvent(#[from] VcsActionMissingTerminalEventError),
}

/// `getVcsActionTargetKey`.
pub fn get_vcs_action_target_key(target: &VcsActionTarget) -> Option<String> {
    let (environment_id, cwd) = (target.environment_id.as_ref()?, target.cwd.as_ref()?);
    Some(target_key(environment_id, cwd))
}

fn target_key(environment_id: &EnvironmentId, cwd: &str) -> String {
    serde_json::to_string(&(environment_id.as_str(), cwd)).expect("strings always encode")
}

/// `parseVcsActionTargetKey`.
pub fn parse_vcs_action_target_key(
    key: &str,
) -> Result<ResolvedVcsActionTarget, VcsActionTargetKeyParseError> {
    serde_json::from_str::<(EnvironmentId, String)>(key)
        .map(|(environment_id, cwd)| ResolvedVcsActionTarget {
            environment_id,
            cwd,
        })
        .map_err(|cause| VcsActionTargetKeyParseError {
            key_length: js_length(key),
            cause: cause.to_string(),
        })
}

static NEXT_LOCAL_ACTION_ID: AtomicU64 = AtomicU64::new(0);

fn create_local_action_id() -> String {
    let next = NEXT_LOCAL_ACTION_ID.fetch_add(1, Ordering::Relaxed) + 1;
    format!("local-vcs-action:{next}")
}

/// `beginVcsActionState`; `now_ms` is `DateTime.nowUnsafe()` in epoch milliseconds.
pub fn begin_vcs_action_state(input: &BeginVcsActionInput, now_ms: i64) -> VcsActionState {
    let action_id = input
        .action_id
        .clone()
        .unwrap_or_else(create_local_action_id);
    VcsActionState {
        is_running: true,
        operation: Some(input.operation),
        action_id: Some(action_id),
        current_label: Some(input.label.clone()),
        current_phase_label: Some(input.label.clone()),
        phase_started_at_ms: Some(now_ms),
        ..empty_vcs_action_state()
    }
}

fn fail_vcs_action_state(
    operation: VcsActionOperation,
    action_id: &str,
    error: &dyn std::fmt::Display,
) -> VcsActionState {
    VcsActionState {
        operation: Some(operation),
        action_id: Some(action_id.to_owned()),
        error: Some(error.to_string()),
        ..empty_vcs_action_state()
    }
}

/// `createVcsActionTransportId`: `<length of the target key>:<target key><action id>`, which
/// scopes an action id to its environment and repository.
pub fn create_vcs_action_transport_id(target: &ResolvedVcsActionTarget, action_id: &str) -> String {
    let key = target_key(&target.environment_id, &target.cwd);
    format!("{}:{}{}", js_length(&key), key, action_id)
}

fn event_action_id(event: &GitActionProgressEvent) -> &TrimmedNonEmptyString {
    match event {
        GitActionProgressEvent::ActionStarted { action_id, .. }
        | GitActionProgressEvent::PhaseStarted { action_id, .. }
        | GitActionProgressEvent::HookStarted { action_id, .. }
        | GitActionProgressEvent::HookOutput { action_id, .. }
        | GitActionProgressEvent::HookFinished { action_id, .. }
        | GitActionProgressEvent::ActionFinished { action_id, .. }
        | GitActionProgressEvent::ActionFailed { action_id, .. } => action_id,
    }
}

fn event_cwd(event: &GitActionProgressEvent) -> &TrimmedNonEmptyString {
    match event {
        GitActionProgressEvent::ActionStarted { cwd, .. }
        | GitActionProgressEvent::PhaseStarted { cwd, .. }
        | GitActionProgressEvent::HookStarted { cwd, .. }
        | GitActionProgressEvent::HookOutput { cwd, .. }
        | GitActionProgressEvent::HookFinished { cwd, .. }
        | GitActionProgressEvent::ActionFinished { cwd, .. }
        | GitActionProgressEvent::ActionFailed { cwd, .. } => cwd,
    }
}

fn event_action(event: &GitActionProgressEvent) -> GitStackedAction {
    match event {
        GitActionProgressEvent::ActionStarted { action, .. }
        | GitActionProgressEvent::PhaseStarted { action, .. }
        | GitActionProgressEvent::HookStarted { action, .. }
        | GitActionProgressEvent::HookOutput { action, .. }
        | GitActionProgressEvent::HookFinished { action, .. }
        | GitActionProgressEvent::ActionFinished { action, .. }
        | GitActionProgressEvent::ActionFailed { action, .. } => *action,
    }
}

fn with_event_action_id(
    mut event: GitActionProgressEvent,
    id: TrimmedNonEmptyString,
) -> GitActionProgressEvent {
    match &mut event {
        GitActionProgressEvent::ActionStarted { action_id, .. }
        | GitActionProgressEvent::PhaseStarted { action_id, .. }
        | GitActionProgressEvent::HookStarted { action_id, .. }
        | GitActionProgressEvent::HookOutput { action_id, .. }
        | GitActionProgressEvent::HookFinished { action_id, .. }
        | GitActionProgressEvent::ActionFinished { action_id, .. }
        | GitActionProgressEvent::ActionFailed { action_id, .. } => *action_id = id,
    }
    event
}

/// `normalizeVcsActionProgressEvent`: `None` unless the event belongs to this transport action
/// and repository; otherwise the event with the caller's own action id.
pub fn normalize_vcs_action_progress_event(
    target: &ResolvedVcsActionTarget,
    transport_action_id: &str,
    action_id: &TrimmedNonEmptyString,
    event: &GitActionProgressEvent,
) -> Option<GitActionProgressEvent> {
    if event_action_id(event).as_str() != transport_action_id
        || event_cwd(event).as_str() != target.cwd
    {
        return None;
    }
    Some(with_event_action_id(event.clone(), action_id.clone()))
}

/// `consumeVcsActionProgress` input.
#[derive(Debug, Clone)]
pub struct ConsumeVcsActionProgressInput {
    pub target: ResolvedVcsActionTarget,
    pub transport_action_id: String,
    pub action_id: TrimmedNonEmptyString,
    pub action: GitStackedAction,
}

/// `E | VcsActionExecutionError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConsumeVcsActionProgressError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Stream(E),
    #[error(transparent)]
    Execution(#[from] VcsActionExecutionError),
}

/// Runs the progress stream to its end, reporting each event for this action, and returns the
/// result of its terminal event. Upstream's `onProgress` is an `Effect<void>`; here it is a
/// plain callback.
pub async fn consume_vcs_action_progress<S, E>(
    stream: S,
    input: &ConsumeVcsActionProgressInput,
    mut on_progress: impl FnMut(&GitActionProgressEvent),
) -> Result<GitRunStackedActionResult, ConsumeVcsActionProgressError<E>>
where
    S: Stream<Item = Result<GitActionProgressEvent, E>>,
    E: std::error::Error + 'static,
{
    let mut terminal_event: Option<GitActionProgressEvent> = None;
    let mut stream = std::pin::pin!(stream);
    while let Some(event) = stream.next().await {
        let event = event.map_err(ConsumeVcsActionProgressError::Stream)?;
        let Some(normalized) = normalize_vcs_action_progress_event(
            &input.target,
            &input.transport_action_id,
            &input.action_id,
            &event,
        ) else {
            continue;
        };
        if matches!(
            normalized,
            GitActionProgressEvent::ActionFinished { .. }
                | GitActionProgressEvent::ActionFailed { .. }
        ) {
            terminal_event = Some(normalized.clone());
        }
        on_progress(&normalized);
    }
    match terminal_event {
        Some(GitActionProgressEvent::ActionFinished { result, .. }) => Ok(result),
        Some(GitActionProgressEvent::ActionFailed {
            action,
            phase,
            message,
            ..
        }) => Err(
            VcsActionExecutionError::RemoteFailure(VcsActionRemoteFailureError {
                action_id: input.action_id.to_string(),
                transport_action_id: input.transport_action_id.clone(),
                action,
                environment_id: input.target.environment_id.clone(),
                cwd: input.target.cwd.clone(),
                phase,
                remote_message_length: js_length(message.as_str()),
            })
            .into(),
        ),
        _ => Err(VcsActionExecutionError::MissingTerminalEvent(
            VcsActionMissingTerminalEventError {
                action_id: input.action_id.to_string(),
                transport_action_id: input.transport_action_id.clone(),
                action: input.action,
                environment_id: input.target.environment_id.clone(),
                cwd: input.target.cwd.clone(),
            },
        )
        .into()),
    }
}

/// `applyVcsActionProgressEvent`: folds one progress event into the presentation state. Events
/// for an action that no longer owns the target leave it untouched.
pub fn apply_vcs_action_progress_event(
    current: &VcsActionState,
    event: &GitActionProgressEvent,
    now_ms: i64,
) -> VcsActionState {
    let event_id = event_action_id(event);
    if current.action_id.as_deref() != Some(event_id.as_str()) {
        return current.clone();
    }
    // Every event keeps the action running; a terminal event settles in the manager.
    let running = VcsActionState {
        is_running: true,
        action_id: Some(event_id.to_string()),
        action: Some(event_action(event)),
        operation: Some(VcsActionOperation::RunChangeRequest),
        ..current.clone()
    };
    match event {
        GitActionProgressEvent::ActionStarted { .. } => VcsActionState {
            phase_started_at_ms: Some(now_ms),
            hook_started_at_ms: None,
            hook_name: None,
            last_output_line: None,
            error: None,
            ..running
        },
        GitActionProgressEvent::PhaseStarted { label, .. } => VcsActionState {
            current_label: Some(label.to_string()),
            current_phase_label: Some(label.to_string()),
            phase_started_at_ms: Some(now_ms),
            hook_started_at_ms: None,
            hook_name: None,
            last_output_line: None,
            error: None,
            ..running
        },
        GitActionProgressEvent::HookStarted { hook_name, .. } => VcsActionState {
            current_label: Some(format!("Running {hook_name}...")),
            hook_name: Some(hook_name.to_string()),
            hook_started_at_ms: Some(now_ms),
            last_output_line: None,
            error: None,
            ..running
        },
        GitActionProgressEvent::HookOutput { text, .. } => VcsActionState {
            last_output_line: Some(text.to_string()),
            error: None,
            ..running
        },
        GitActionProgressEvent::HookFinished { .. } => VcsActionState {
            current_label: current.current_phase_label.clone(),
            hook_name: None,
            hook_started_at_ms: None,
            last_output_line: None,
            error: None,
            ..running
        },
        GitActionProgressEvent::ActionFinished { .. } => VcsActionState {
            error: None,
            ..running
        },
        GitActionProgressEvent::ActionFailed { message, .. } => VcsActionState {
            error: Some(message.to_string()),
            ..running
        },
    }
}

/// `E | VcsActionUnavailableError` from `track`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VcsActionTrackError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Unavailable(VcsActionUnavailableError),
    #[error(transparent)]
    Action(E),
}

/// The failure channel of `run_stacked_action`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunVcsStackedActionError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Unavailable(VcsActionUnavailableError),
    #[error(transparent)]
    Consume(ConsumeVcsActionProgressError<E>),
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
type States = Arc<Mutex<HashMap<String, VcsActionState>>>;

/// Clears the state a dropped (interrupted) operation still owns.
struct OwnedStateGuard {
    states: States,
    key: String,
    action_id: String,
    armed: bool,
}

impl OwnedStateGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OwnedStateGuard {
    fn drop(&mut self) {
        if self.armed {
            clear_if_owned(&self.states, &self.key, &self.action_id);
        }
    }
}

fn clear_if_owned(states: &States, key: &str, action_id: &str) {
    let mut states = states.lock();
    if let Some(current) = states.get_mut(key)
        && current.action_id.as_deref() == Some(action_id)
    {
        *current = empty_vcs_action_state();
    }
}

/// `createVcsActionManager`: presentation state per `[environment, cwd]` target.
pub struct VcsActionManager {
    states: States,
    now_ms: Clock,
}

impl Default for VcsActionManager {
    fn default() -> Self {
        Self::with_clock(|| chrono::Utc::now().timestamp_millis())
    }
}

impl VcsActionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_clock(now_ms: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self {
            states: Arc::new(Mutex::new(HashMap::new())),
            now_ms: Arc::new(now_ms),
        }
    }

    /// `stateAtom`: the empty state for a target that is not fully known.
    pub fn state(&self, target: &VcsActionTarget) -> VcsActionState {
        get_vcs_action_target_key(target)
            .and_then(|key| self.states.lock().get(&key).cloned())
            .unwrap_or_default()
    }

    fn guard(&self, key: &str, action_id: &str) -> OwnedStateGuard {
        OwnedStateGuard {
            states: self.states.clone(),
            key: key.to_owned(),
            action_id: action_id.to_owned(),
            armed: true,
        }
    }

    fn set(&self, key: &str, state: VcsActionState) {
        self.states.lock().insert(key.to_owned(), state);
    }

    /// Tracks a finite mutation: running while it is pending, cleared when it succeeds, and the
    /// error retained when it fails. An older completion never clears newer state, and a
    /// dropped (cancelled) operation clears the state it still owns.
    pub async fn track<A, E, Fut>(
        &self,
        target: &VcsActionTarget,
        input: BeginVcsActionInput,
        action: impl FnOnce() -> Fut,
    ) -> Result<A, VcsActionTrackError<E>>
    where
        E: std::error::Error + 'static,
        Fut: Future<Output = Result<A, E>>,
    {
        let Some(key) = get_vcs_action_target_key(target) else {
            return Err(VcsActionTrackError::Unavailable(
                VcsActionUnavailableError {
                    operation: input.operation,
                    environment_id: target.environment_id.clone(),
                    cwd: target.cwd.clone(),
                },
            ));
        };
        let next = begin_vcs_action_state(&input, (self.now_ms)());
        let action_id = next.action_id.clone().expect("begin sets an action id");
        self.set(&key, next);
        let mut guard = self.guard(&key, &action_id);
        let result = action().await;
        guard.disarm();
        let mut states = self.states.lock();
        let owned = states
            .get(&key)
            .is_some_and(|current| current.action_id.as_deref() == Some(action_id.as_str()));
        if owned {
            match &result {
                Ok(_) => {
                    states.insert(key, empty_vcs_action_state());
                }
                Err(error) => {
                    states.insert(
                        key,
                        fail_vcs_action_state(input.operation, &action_id, error),
                    );
                }
            }
        }
        result.map_err(VcsActionTrackError::Action)
    }

    /// `resetError`: forgets a settled failure of `operation`.
    pub fn reset_error(&self, target: &VcsActionTarget, operation: VcsActionOperation) {
        let Some(key) = get_vcs_action_target_key(target) else {
            return;
        };
        let mut states = self.states.lock();
        let current = states.get(&key).cloned().unwrap_or_default();
        if !current.is_running && current.operation == Some(operation) {
            states.insert(key, empty_vcs_action_state());
        }
    }

    /// The `runStackedAction` command body. `run` starts the `gitRunStackedAction` stream for the
    /// RPC input; `invalidate` runs after the stream settles whether it succeeded or failed
    /// (upstream's `invalidateCachedVcsRefs`). Presentation state follows the progress events
    /// and clears on success; on failure the error stays until the next action.
    ///
    /// Callbacks that panic do not fail the operation.
    pub async fn run_stacked_action<S, E, Inv>(
        &self,
        target: &VcsActionTarget,
        input: RunVcsStackedActionInput,
        mut on_progress: Option<&mut dyn FnMut(&GitActionProgressEvent)>,
        run: impl FnOnce(GitRunStackedActionInput) -> S,
        invalidate: impl FnOnce(ResolvedVcsActionTarget) -> Inv,
    ) -> Result<GitRunStackedActionResult, RunVcsStackedActionError<E>>
    where
        S: Stream<Item = Result<GitActionProgressEvent, E>>,
        E: std::error::Error + 'static,
        Inv: Future<Output = ()>,
    {
        let unavailable = || {
            RunVcsStackedActionError::Unavailable(VcsActionUnavailableError {
                operation: VcsActionOperation::RunChangeRequest,
                environment_id: target.environment_id.clone(),
                cwd: target.cwd.clone(),
            })
        };
        let Some(key) = get_vcs_action_target_key(target) else {
            return Err(unavailable());
        };
        let resolved = parse_vcs_action_target_key(&key).map_err(|_| unavailable())?;
        let transport_action_id =
            create_vcs_action_transport_id(&resolved, input.action_id.as_str());
        let rpc_input = build_git_run_stacked_action_input(&resolved, &transport_action_id, &input)
            .map_err(|_| unavailable())?;
        let action_id = input.action_id.to_string();
        self.set(
            &key,
            begin_vcs_action_state(
                &BeginVcsActionInput {
                    operation: VcsActionOperation::RunChangeRequest,
                    label: "Running source control action".to_owned(),
                    action_id: Some(action_id.clone()),
                },
                (self.now_ms)(),
            ),
        );
        let mut guard = self.guard(&key, &action_id);

        let consume_input = ConsumeVcsActionProgressInput {
            target: resolved.clone(),
            transport_action_id,
            action_id: input.action_id.clone(),
            action: input.action,
        };
        let result = consume_vcs_action_progress(run(rpc_input), &consume_input, |event| {
            {
                let mut states = self.states.lock();
                let current = states.get(&key).cloned().unwrap_or_default();
                if current.action_id.as_deref() != Some(action_id.as_str()) {
                    return;
                }
                states.insert(
                    key.clone(),
                    apply_vcs_action_progress_event(&current, event, (self.now_ms)()),
                );
            }
            if let Some(callback) = on_progress.as_deref_mut() {
                // Presentation callbacks must not fail the source-control operation.
                let _ = catch_unwind(AssertUnwindSafe(|| callback(event)));
            }
        })
        .await;

        invalidate(resolved).await;
        guard.disarm();
        match &result {
            Ok(_) => clear_if_owned(&self.states, &key, &action_id),
            Err(error) => {
                let mut states = self.states.lock();
                let owned_and_running = states.get(&key).is_some_and(|current| {
                    current.action_id.as_deref() == Some(action_id.as_str()) && current.is_running
                });
                if owned_and_running {
                    states.insert(
                        key,
                        fail_vcs_action_state(
                            VcsActionOperation::RunChangeRequest,
                            &action_id,
                            error,
                        ),
                    );
                }
            }
        }
        result.map_err(RunVcsStackedActionError::Consume)
    }
}

/// The `gitRunStackedAction` RPC input: optional fields are sent only when they carry a value.
pub fn build_git_run_stacked_action_input(
    target: &ResolvedVcsActionTarget,
    transport_action_id: &str,
    input: &RunVcsStackedActionInput,
) -> Result<GitRunStackedActionInput, turin_contracts::ContractError> {
    let file_paths = input
        .file_paths
        .as_ref()
        .filter(|paths| !paths.is_empty())
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| GitFilePath::new(path.clone()).ok())
                .collect::<Vec<_>>()
        });
    Ok(GitRunStackedActionInput {
        action_id: TrimmedNonEmptyString::new(transport_action_id)?,
        cwd: TrimmedNonEmptyString::new(&target.cwd)?,
        action: input.action,
        commit_message: input
            .commit_message
            .as_deref()
            .and_then(|message| TrimmedNonEmptyString::new(message).ok()),
        feature_branch: (input.feature_branch == Some(true)).then_some(true),
        file_paths,
        thread_id: input.thread_id.clone(),
        project_id: input.project_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use serde_json::{Value, json};
    use std::convert::Infallible;
    use std::time::Duration;
    use tokio::sync::oneshot;

    const ACTION_ID: &str = "action-123";
    const CWD: &str = "/repo";

    fn environment_id() -> EnvironmentId {
        EnvironmentId::new("environment-1").unwrap()
    }

    fn action_id() -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(ACTION_ID).unwrap()
    }

    fn resolved() -> ResolvedVcsActionTarget {
        ResolvedVcsActionTarget {
            environment_id: environment_id(),
            cwd: CWD.to_owned(),
        }
    }

    fn target() -> VcsActionTarget {
        VcsActionTarget {
            environment_id: Some(environment_id()),
            cwd: Some(CWD.to_owned()),
        }
    }

    fn result_json() -> Value {
        json!({
            "action": "commit_push",
            "branch": { "status": "skipped_not_requested" },
            "commit": { "status": "created", "commitSha": "abc123", "subject": "Test commit" },
            "push": { "status": "pushed", "branch": "feature" },
            "pr": { "status": "skipped_not_requested" },
            "toast": { "title": "Changes pushed", "cta": { "kind": "none" } },
        })
    }

    fn result() -> GitRunStackedActionResult {
        serde_json::from_value(result_json()).unwrap()
    }

    /// `progress({ actionId, action: "commit_push", cwd, ...fields })`.
    fn progress(action_id: &str, fields: Value) -> GitActionProgressEvent {
        let mut event = json!({ "actionId": action_id, "action": "commit_push", "cwd": CWD });
        event
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_value(event).unwrap()
    }

    fn begin(action_id: &str) -> VcsActionState {
        begin_vcs_action_state(
            &BeginVcsActionInput {
                operation: VcsActionOperation::RunChangeRequest,
                label: "Running source control action".into(),
                action_id: Some(action_id.into()),
            },
            1_000,
        )
    }

    fn apply(current: &VcsActionState, event: &GitActionProgressEvent) -> VcsActionState {
        apply_vcs_action_progress_event(current, event, 2_000)
    }

    #[test]
    fn preserves_malformed_target_key_diagnostics_without_copying_the_key() {
        let key = "not-json-with-credential=do-not-log";
        let error = parse_vcs_action_target_key(key).unwrap_err();
        assert_eq!(error.key_length, key.len());
        assert!(!error.cause.is_empty());
        assert!(!error.to_string().contains(key));
    }

    #[test]
    fn rejects_invalid_target_key_shapes() {
        let key = serde_json::to_string(&[environment_id().as_str()]).unwrap();
        assert!(parse_vcs_action_target_key(&key).is_err());
        assert!(parse_vcs_action_target_key("[1,\"/repo\"]").is_err());
    }

    #[test]
    fn target_keys_round_trip() {
        let key = get_vcs_action_target_key(&target()).unwrap();
        assert_eq!(key, r#"["environment-1","/repo"]"#);
        assert_eq!(parse_vcs_action_target_key(&key).unwrap(), resolved());
    }

    #[test]
    fn projects_phase_and_hook_progress_without_owning_the_async_operation() {
        let initial = begin(ACTION_ID);
        let phase = apply(
            &initial,
            &progress(
                ACTION_ID,
                json!({ "kind": "phase_started", "phase": "commit", "label": "Committing..." }),
            ),
        );
        let hook = apply(
            &phase,
            &progress(
                ACTION_ID,
                json!({ "kind": "hook_started", "hookName": "post-commit" }),
            ),
        );
        let output = apply(
            &hook,
            &progress(
                ACTION_ID,
                json!({
                    "kind": "hook_output",
                    "hookName": "post-commit",
                    "stream": "stdout",
                    "text": "hook output",
                }),
            ),
        );
        let finished = apply(
            &output,
            &progress(
                ACTION_ID,
                json!({
                    "kind": "hook_finished",
                    "hookName": "post-commit",
                    "exitCode": 0,
                    "durationMs": 12,
                }),
            ),
        );

        assert!(phase.is_running);
        assert_eq!(phase.current_label.as_deref(), Some("Committing..."));
        assert_eq!(phase.current_phase_label.as_deref(), Some("Committing..."));
        assert_eq!(phase.phase_started_at_ms, Some(2_000));
        assert_eq!(
            output.current_label.as_deref(),
            Some("Running post-commit...")
        );
        assert_eq!(output.hook_name.as_deref(), Some("post-commit"));
        assert_eq!(output.last_output_line.as_deref(), Some("hook output"));
        assert_eq!(output.hook_started_at_ms, Some(2_000));
        assert_eq!(finished.current_label.as_deref(), Some("Committing..."));
        assert_eq!(finished.hook_name, None);
        assert_eq!(finished.last_output_line, None);
        assert_eq!(finished.hook_started_at_ms, None);
    }

    #[test]
    fn keeps_running_presentation_state_until_the_finished_action_settles() {
        let pushing = apply(
            &begin(ACTION_ID),
            &progress(
                ACTION_ID,
                json!({ "kind": "phase_started", "phase": "push", "label": "Pushing..." }),
            ),
        );
        let finished = apply(
            &pushing,
            &progress(
                ACTION_ID,
                json!({ "kind": "action_finished", "result": result_json() }),
            ),
        );
        assert!(finished.is_running);
        assert_eq!(
            finished.operation,
            Some(VcsActionOperation::RunChangeRequest)
        );
        assert_eq!(finished.action_id.as_deref(), Some(ACTION_ID));
        assert_eq!(finished.action, Some(GitStackedAction::CommitPush));
        assert_eq!(finished.current_label.as_deref(), Some("Pushing..."));
        assert_eq!(finished.current_phase_label.as_deref(), Some("Pushing..."));
        assert_eq!(finished.last_output_line, None);
        assert_eq!(finished.error, None);
    }

    #[test]
    fn keeps_running_presentation_state_and_retains_a_terminal_action_error_until_settle() {
        let failed = apply(
            &begin(ACTION_ID),
            &progress(
                ACTION_ID,
                json!({ "kind": "action_failed", "phase": null, "message": "Push failed." }),
            ),
        );
        assert!(failed.is_running);
        assert_eq!(failed.operation, Some(VcsActionOperation::RunChangeRequest));
        assert_eq!(failed.action_id.as_deref(), Some(ACTION_ID));
        assert_eq!(failed.action, Some(GitStackedAction::CommitPush));
        assert_eq!(failed.error.as_deref(), Some("Push failed."));
    }

    #[test]
    fn action_started_resets_the_hook_and_error_fields() {
        let mut current = begin(ACTION_ID);
        current.hook_name = Some("hook".into());
        current.last_output_line = Some("old".into());
        current.error = Some("old error".into());
        let started = apply(
            &current,
            &progress(
                ACTION_ID,
                json!({ "kind": "action_started", "phases": ["commit", "push"] }),
            ),
        );
        assert_eq!(started.hook_name, None);
        assert_eq!(started.last_output_line, None);
        assert_eq!(started.error, None);
        assert_eq!(started.phase_started_at_ms, Some(2_000));
    }

    #[test]
    fn ignores_progress_after_a_newer_action_owns_the_target() {
        let current = begin_vcs_action_state(
            &BeginVcsActionInput {
                operation: VcsActionOperation::Pull,
                label: "Pulling latest changes".into(),
                action_id: Some("newer-action".into()),
            },
            1_000,
        );
        let event = progress(
            ACTION_ID,
            json!({ "kind": "phase_started", "phase": "push", "label": "Pushing..." }),
        );
        assert_eq!(apply(&current, &event), current);
    }

    #[test]
    fn keys_presentation_state_only_when_the_environment_and_repository_are_known() {
        assert_eq!(
            get_vcs_action_target_key(&target()),
            Some(serde_json::to_string(&[environment_id().as_str(), CWD]).unwrap())
        );
        assert_eq!(
            get_vcs_action_target_key(&VcsActionTarget {
                environment_id: None,
                cwd: Some(CWD.into())
            }),
            None
        );
        assert_eq!(
            get_vcs_action_target_key(&VcsActionTarget {
                environment_id: Some(environment_id()),
                cwd: None
            }),
            None
        );
    }

    #[test]
    fn local_action_ids_are_unique() {
        let input = BeginVcsActionInput {
            operation: VcsActionOperation::Pull,
            label: "Pulling".into(),
            action_id: None,
        };
        let first = begin_vcs_action_state(&input, 0).action_id.unwrap();
        let second = begin_vcs_action_state(&input, 0).action_id.unwrap();
        assert!(first.starts_with("local-vcs-action:"));
        assert_ne!(first, second);
    }

    #[test]
    fn normalizes_progress_only_for_the_matching_environment_scoped_action() {
        let other = ResolvedVcsActionTarget {
            environment_id: EnvironmentId::new("environment-2").unwrap(),
            cwd: CWD.to_owned(),
        };
        let transport = create_vcs_action_transport_id(&resolved(), ACTION_ID);
        let foreign = progress(
            &create_vcs_action_transport_id(&other, ACTION_ID),
            json!({ "kind": "phase_started", "phase": "push", "label": "Pushing..." }),
        );
        assert_eq!(
            normalize_vcs_action_progress_event(&resolved(), &transport, &action_id(), &foreign),
            None
        );
        let own = progress(
            &transport,
            json!({ "kind": "phase_started", "phase": "push", "label": "Pushing..." }),
        );
        assert_eq!(
            normalize_vcs_action_progress_event(&resolved(), &transport, &action_id(), &own),
            Some(progress(
                ACTION_ID,
                json!({ "kind": "phase_started", "phase": "push", "label": "Pushing..." })
            ))
        );
        // A different working directory is not this action either.
        let mut elsewhere = own.clone();
        if let GitActionProgressEvent::PhaseStarted { cwd, .. } = &mut elsewhere {
            *cwd = TrimmedNonEmptyString::new("/elsewhere").unwrap();
        }
        assert_eq!(
            normalize_vcs_action_progress_event(&resolved(), &transport, &action_id(), &elsewhere),
            None
        );
    }

    #[test]
    fn transport_ids_are_prefixed_with_the_target_key_length() {
        let key = r#"["environment-1","/repo"]"#;
        assert_eq!(
            create_vcs_action_transport_id(&resolved(), ACTION_ID),
            format!("{}:{key}{ACTION_ID}", key.len())
        );
    }

    fn consume_input() -> ConsumeVcsActionProgressInput {
        ConsumeVcsActionProgressInput {
            target: resolved(),
            transport_action_id: create_vcs_action_transport_id(&resolved(), ACTION_ID),
            action_id: action_id(),
            action: GitStackedAction::CommitPush,
        }
    }

    fn events(
        events: Vec<GitActionProgressEvent>,
    ) -> impl Stream<Item = Result<GitActionProgressEvent, Infallible>> {
        stream::iter(events.into_iter().map(Ok))
    }

    #[tokio::test]
    async fn consumes_progress_through_the_terminal_event_and_returns_its_result() {
        let input = consume_input();
        let transport = input.transport_action_id.clone();
        let mut observed = Vec::new();
        let actual = consume_vcs_action_progress(
            events(vec![
                progress(
                    "unrelated-action",
                    json!({ "kind": "phase_started", "phase": "commit", "label": "Ignored" }),
                ),
                progress(
                    &transport,
                    json!({ "kind": "phase_started", "phase": "push", "label": "Pushing..." }),
                ),
                progress(
                    &transport,
                    json!({ "kind": "action_finished", "result": result_json() }),
                ),
            ]),
            &input,
            |event| observed.push(event.clone()),
        )
        .await;

        assert_eq!(actual, Ok(result()));
        assert_eq!(
            observed
                .iter()
                .map(|event| event_action_id(event).as_str().to_owned())
                .collect::<Vec<_>>(),
            vec![ACTION_ID, ACTION_ID]
        );
        assert!(matches!(
            observed[0],
            GitActionProgressEvent::PhaseStarted { .. }
        ));
        assert!(matches!(
            observed[1],
            GitActionProgressEvent::ActionFinished { .. }
        ));
    }

    #[tokio::test]
    async fn retains_structural_remote_failure_context_without_copying_the_remote_payload() {
        let input = consume_input();
        let remote_message = "The remote rejected the push with credential=do-not-log.";
        let error = consume_vcs_action_progress(
            events(vec![progress(
                &input.transport_action_id,
                json!({ "kind": "action_failed", "phase": "push", "message": remote_message }),
            )]),
            &input,
            |_| {},
        )
        .await
        .unwrap_err();

        let ConsumeVcsActionProgressError::Execution(VcsActionExecutionError::RemoteFailure(
            failure,
        )) = &error
        else {
            panic!("expected a remote failure, got {error:?}");
        };
        assert_eq!(failure.action_id, ACTION_ID);
        assert_eq!(failure.transport_action_id, input.transport_action_id);
        assert_eq!(failure.action, GitStackedAction::CommitPush);
        assert_eq!(failure.environment_id, environment_id());
        assert_eq!(failure.cwd, CWD);
        assert_eq!(failure.phase, Some(GitActionProgressPhase::Push));
        assert_eq!(failure.remote_message_length, remote_message.len());
        assert_eq!(
            error.to_string(),
            "Source control action 'commit_push' failed during push."
        );
        assert!(!error.to_string().contains(remote_message));
    }

    #[tokio::test]
    async fn reports_a_missing_terminal_event_as_a_protocol_failure() {
        let input = consume_input();
        let error = consume_vcs_action_progress(
            events(vec![progress(
                &input.transport_action_id,
                json!({ "kind": "phase_started", "phase": "commit", "label": "Committing..." }),
            )]),
            &input,
            |_| {},
        )
        .await
        .unwrap_err();
        let ConsumeVcsActionProgressError::Execution(
            VcsActionExecutionError::MissingTerminalEvent(missing),
        ) = &error
        else {
            panic!("expected a missing terminal event, got {error:?}");
        };
        assert_eq!(missing.action_id, ACTION_ID);
        assert_eq!(missing.transport_action_id, input.transport_action_id);
        assert_eq!(missing.action, GitStackedAction::CommitPush);
        assert_eq!(missing.environment_id, environment_id());
        assert_eq!(missing.cwd, CWD);
        assert_eq!(
            error.to_string(),
            "Source control action 'commit_push' ended without a terminal result."
        );
    }

    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    #[error("{0}")]
    struct Failure(String);

    fn manager() -> VcsActionManager {
        VcsActionManager::with_clock(|| 1_000)
    }

    #[test]
    fn unknown_targets_have_the_empty_state() {
        let manager = manager();
        assert_eq!(manager.state(&target()), empty_vcs_action_state());
        assert_eq!(
            manager.state(&VcsActionTarget::default()),
            empty_vcs_action_state()
        );
    }

    #[tokio::test]
    async fn retains_the_incomplete_target_and_operation_when_tracking_is_unavailable() {
        let manager = manager();
        let error = manager
            .track(
                &VcsActionTarget {
                    environment_id: Some(environment_id()),
                    cwd: None,
                },
                BeginVcsActionInput {
                    operation: VcsActionOperation::Pull,
                    label: "Pulling latest changes".into(),
                    action_id: None,
                },
                || async { Ok::<(), Failure>(()) },
            )
            .await
            .unwrap_err();
        let VcsActionTrackError::Unavailable(error) = error else {
            panic!("expected an unavailable error");
        };
        assert_eq!(error.operation, VcsActionOperation::Pull);
        assert_eq!(error.environment_id, Some(environment_id()));
        assert_eq!(error.cwd, None);
        assert_eq!(
            error.to_string(),
            "Source control operation 'pull' is unavailable."
        );
    }

    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn tracks_finite_mutations_without_letting_an_older_completion_clear_newer_state() {
        let manager = Arc::new(manager());
        let (finish_first, first_action) = oneshot::channel::<Result<(), Failure>>();
        let (fail_second, second_action) = oneshot::channel::<Result<(), Failure>>();

        let first = tokio::spawn({
            let manager = manager.clone();
            async move {
                manager
                    .track(
                        &target(),
                        BeginVcsActionInput {
                            operation: VcsActionOperation::Pull,
                            label: "Pulling latest changes".into(),
                            action_id: None,
                        },
                        || async { first_action.await.unwrap() },
                    )
                    .await
            }
        });
        settle().await;
        let first_action_id = manager.state(&target()).action_id;
        let second = tokio::spawn({
            let manager = manager.clone();
            async move {
                manager
                    .track(
                        &target(),
                        BeginVcsActionInput {
                            operation: VcsActionOperation::SwitchRef,
                            label: "Switching branch".into(),
                            action_id: None,
                        },
                        || async { second_action.await.unwrap() },
                    )
                    .await
            }
        });
        settle().await;
        let second_action_id = manager.state(&target()).action_id;

        finish_first.send(Ok(())).unwrap();
        first.await.unwrap().unwrap();
        let state = manager.state(&target());
        assert_eq!(state.action_id, second_action_id);
        assert!(state.is_running);
        assert_eq!(state.operation, Some(VcsActionOperation::SwitchRef));
        assert_ne!(second_action_id, first_action_id);

        fail_second
            .send(Err(Failure("switch failed".into())))
            .unwrap();
        let failure = second.await.unwrap().unwrap_err();
        assert_eq!(
            failure,
            VcsActionTrackError::Action(Failure("switch failed".into()))
        );
        let state = manager.state(&target());
        assert_eq!(state.action_id, second_action_id);
        assert_eq!(state.error.as_deref(), Some("switch failed"));
        assert!(!state.is_running);
        assert_eq!(state.operation, Some(VcsActionOperation::SwitchRef));

        manager.reset_error(&target(), VcsActionOperation::Pull);
        assert_eq!(
            manager.state(&target()).error.as_deref(),
            Some("switch failed")
        );
        manager.reset_error(&target(), VcsActionOperation::SwitchRef);
        assert_eq!(manager.state(&target()), empty_vcs_action_state());
    }

    #[tokio::test]
    async fn a_cancelled_track_clears_the_state_it_owns() {
        let manager = manager();
        let cancelled = tokio::time::timeout(
            Duration::from_millis(20),
            manager.track(
                &target(),
                BeginVcsActionInput {
                    operation: VcsActionOperation::Pull,
                    label: "Pulling latest changes".into(),
                    action_id: None,
                },
                std::future::pending::<Result<(), Failure>>,
            ),
        )
        .await;
        assert!(cancelled.is_err());
        assert_eq!(manager.state(&target()), empty_vcs_action_state());
    }

    #[test]
    fn the_rpc_input_carries_only_values_and_the_thread_that_links_a_created_pull_request() {
        let transport = create_vcs_action_transport_id(&resolved(), ACTION_ID);
        let input = RunVcsStackedActionInput {
            action_id: action_id(),
            action: GitStackedAction::CommitPush,
            commit_message: Some(String::new()),
            feature_branch: Some(false),
            file_paths: Some(vec![]),
            thread_id: Some(ThreadId::new("thread-stacked-action").unwrap()),
            project_id: Some(ProjectId::new("project-stacked-action").unwrap()),
        };
        let rpc = build_git_run_stacked_action_input(&resolved(), &transport, &input).unwrap();
        assert_eq!(
            serde_json::to_value(&rpc).unwrap(),
            json!({
                "actionId": transport,
                "cwd": CWD,
                "action": "commit_push",
                "threadId": "thread-stacked-action",
                "projectId": "project-stacked-action",
            })
        );

        let full = RunVcsStackedActionInput {
            commit_message: Some("Fix it".into()),
            feature_branch: Some(true),
            file_paths: Some(vec!["a.rs".into(), "b.rs".into()]),
            thread_id: None,
            project_id: None,
            ..input
        };
        let rpc = build_git_run_stacked_action_input(&resolved(), &transport, &full).unwrap();
        assert_eq!(
            serde_json::to_value(&rpc).unwrap(),
            json!({
                "actionId": transport,
                "cwd": CWD,
                "action": "commit_push",
                "commitMessage": "Fix it",
                "featureBranch": true,
                "filePaths": ["a.rs", "b.rs"],
            })
        );
    }

    fn run_input(id: &str) -> RunVcsStackedActionInput {
        RunVcsStackedActionInput {
            action_id: TrimmedNonEmptyString::new(id).unwrap(),
            action: GitStackedAction::CommitPush,
            commit_message: None,
            feature_branch: None,
            file_paths: None,
            thread_id: None,
            project_id: None,
        }
    }

    #[tokio::test]
    async fn invalidates_refs_after_successful_and_failed_stacked_actions() {
        let manager = manager();
        let invalidations = Arc::new(Mutex::new(Vec::<ResolvedVcsActionTarget>::new()));
        let invalidate = |invalidations: Arc<Mutex<Vec<ResolvedVcsActionTarget>>>| {
            move |target: ResolvedVcsActionTarget| async move {
                invalidations.lock().push(target);
            }
        };
        let rpc_inputs = Arc::new(Mutex::new(Vec::<GitRunStackedActionInput>::new()));
        let success_transport = create_vcs_action_transport_id(&resolved(), "invalidate-success");
        let failure_transport = create_vcs_action_transport_id(&resolved(), "invalidate-failure");

        let mut observed = Vec::new();
        let mut on_progress = |event: &GitActionProgressEvent| observed.push(event.clone());
        let mut with_thread = run_input("invalidate-success");
        with_thread.thread_id = Some(ThreadId::new("thread-stacked-action").unwrap());
        with_thread.project_id = Some(ProjectId::new("project-stacked-action").unwrap());
        let finished = manager
            .run_stacked_action(
                &target(),
                with_thread,
                Some(&mut on_progress),
                {
                    let (rpc_inputs, transport) = (rpc_inputs.clone(), success_transport.clone());
                    move |rpc| {
                        rpc_inputs.lock().push(rpc);
                        events(vec![progress(
                            &transport,
                            json!({ "kind": "action_finished", "result": result_json() }),
                        )])
                    }
                },
                invalidate(invalidations.clone()),
            )
            .await;
        assert_eq!(finished, Ok(result()));
        assert_eq!(manager.state(&target()), empty_vcs_action_state());
        assert_eq!(invalidations.lock().len(), 1);
        assert_eq!(observed.len(), 1);
        // The server links a created pull request to this thread, so the id must ride along.
        let sent = rpc_inputs.lock()[0].clone();
        assert_eq!(sent.action_id.as_str(), success_transport);
        assert_eq!(sent.thread_id.unwrap().as_str(), "thread-stacked-action");
        assert_eq!(sent.project_id.unwrap().as_str(), "project-stacked-action");

        let failed = manager
            .run_stacked_action(
                &target(),
                run_input("invalidate-failure"),
                None,
                {
                    let transport = failure_transport.clone();
                    move |_| {
                        events(vec![progress(
                            &transport,
                            json!({
                                "kind": "action_failed",
                                "phase": "push",
                                "message": "push failed after creating the branch",
                            }),
                        )])
                    }
                },
                invalidate(invalidations.clone()),
            )
            .await;
        assert!(matches!(
            failed,
            Err(RunVcsStackedActionError::Consume(
                ConsumeVcsActionProgressError::Execution(VcsActionExecutionError::RemoteFailure(_))
            ))
        ));
        let state = manager.state(&target());
        assert!(!state.is_running);
        assert_eq!(state.operation, Some(VcsActionOperation::RunChangeRequest));
        assert_eq!(state.action_id.as_deref(), Some("invalidate-failure"));
        assert_eq!(
            state.error.as_deref(),
            Some("Source control action 'commit_push' failed during push.")
        );
        assert_eq!(invalidations.lock().len(), 2);
        assert!(
            invalidations
                .lock()
                .iter()
                .all(|target| *target == resolved())
        );
    }

    #[tokio::test]
    async fn an_interrupted_stacked_action_clears_its_state() {
        let manager = manager();
        let interrupted = tokio::time::timeout(
            Duration::from_millis(20),
            manager.run_stacked_action(
                &target(),
                run_input("invalidate-interrupted"),
                None,
                |_| stream::pending::<Result<GitActionProgressEvent, Infallible>>(),
                |_| async {},
            ),
        )
        .await;
        assert!(interrupted.is_err());
        assert_eq!(manager.state(&target()), empty_vcs_action_state());
    }

    #[tokio::test]
    async fn an_incomplete_target_makes_the_stacked_action_unavailable() {
        let manager = manager();
        let error = manager
            .run_stacked_action(
                &VcsActionTarget {
                    environment_id: None,
                    cwd: Some(CWD.into()),
                },
                run_input("unavailable"),
                None,
                |_| events(vec![]),
                |_| async {},
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Source control operation 'run change request' is unavailable."
        );
    }

    #[tokio::test]
    async fn a_panicking_progress_callback_does_not_fail_the_action() {
        let manager = manager();
        let transport = create_vcs_action_transport_id(&resolved(), "panics");
        let mut panics = |_: &GitActionProgressEvent| panic!("presentation bug");
        let outcome = manager
            .run_stacked_action(
                &target(),
                run_input("panics"),
                Some(&mut panics),
                move |_| {
                    events(vec![progress(
                        &transport,
                        json!({ "kind": "action_finished", "result": result_json() }),
                    )])
                },
                |_| async {},
            )
            .await;
        assert_eq!(outcome, Ok(result()));
    }
}
