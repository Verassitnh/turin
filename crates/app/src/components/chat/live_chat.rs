//! The live thread view: one [`LiveChat`] per open thread binds the client
//! store's projection to the timeline and the composer, and routes the
//! composer's intent (send, stop, mode changes) to the command bus.
//!
//! ```text
//! ClientStore (thread projection) ─▶ LiveChat::refresh ─▶ ThreadTimelineStore ─▶ MessagesTimeline
//!                                                      └▶ ComposerStore       ─▶ ChatComposer
//! ChatComposer::submit ─▶ LiveSource::send ─▶ CommandBus ─▶ message.dispatch
//! ```
//!
//! Replaces `MockTimelineSource` / `MockComposerSource` for a routed thread.
//! Errors surface as toasts (`Failed to send message`) and a rejected send
//! keeps the draft, as upstream's `onSend` does.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, AppContext as _, Context, Entity, Subscription, WeakEntity};
use indexmap::IndexMap;
use serde_json::{Value, json};
use turin_contracts::ThreadId;

use super::composer_pending_bridge::{
    PendingSubmitPlan, apply_composer_prompt_to_session, message_mode_pending_input,
    pending_answer_for_message_input, sync_pending_input_session,
};
use super::timeline::live_projection::{decision_wire, snapshot_from_projection};
use super::timeline::source::{ThreadTimelineStore, TimelineSource};
use super::timeline::turn_item::ProviderApprovalDecision;
use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::{CommandError, Delivery, SendMessage};
use crate::composer::command_items::PathKind;
use crate::composer::dispatch::ComposerDispatchMode;
use crate::composer::live_catalog::provider_entries;
use crate::composer::prompt_history::ComposerPromptHistoryMessage;
use crate::composer::provider_options::{OptionSelection, OptionValue};
use crate::composer::view_model::{
    ComposerSource, ComposerStore, ComposerViewModel, DraftRestore, InteractionMode, PathEntry,
    QueueState, QueuedRun, RuntimeMode, SendRequest, SessionPhase,
};
use crate::state::client_state::ClientState;
use crate::state::local_environment_id;
use crate::state::models::scoped_thread_key;
use crate::state::store::{ClientStateHandle, ClientStore};
use crate::state::thread_projection::ThreadProjection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ConversationMessageRole, OrchestrationV2RunStatus,
};

/// How long a send may run before it is reported as failed.
const SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// Choices the user made in the composer that are not yet on the thread.
#[derive(Default)]
struct Overrides {
    model: Option<(String, String)>,
    model_options: Option<Vec<OptionSelection>>,
    runtime_mode: Option<RuntimeMode>,
    interaction_mode: Option<InteractionMode>,
    favorites: std::collections::HashSet<String>,
    /// The queued run whose message is loaded in the composer for editing.
    editing_run: Option<String>,
    /// A send is in flight (a second submit is refused until it settles).
    sending: bool,
    restore: Option<DraftRestore>,
    restore_nonce: u64,
    path_entries: Vec<PathEntry>,
    path_search_pending: bool,
    /// Bumped per search; an older search's result is dropped.
    path_search_generation: u64,
    pending_input: super::composer_pending_bridge::ComposerPendingInputSession,
}

pub struct LiveChat {
    source: Arc<LiveSource>,
    pub timeline: Entity<ThreadTimelineStore>,
    pub composer: Entity<ComposerStore>,
    _subscription: Subscription,
}

struct LiveSource {
    thread_id: ThreadId,
    bus: CommandBus,
    handle: ClientStateHandle,
    // Weak: the stores hold this source, so strong handles here would keep
    // the thread open for the life of the process.
    timeline: WeakEntity<ThreadTimelineStore>,
    composer: WeakEntity<ComposerStore>,
    overrides: RefCell<Overrides>,
    me: std::sync::Weak<LiveSource>,
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        self.handle.close_thread(self.thread_id.clone());
    }
}

pub fn push_error(cx: &mut App, title: &str, description: &str) {
    let (title, description) = (title.to_owned(), description.to_owned());
    if let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) {
        window
            .update(cx, |_, window, cx| {
                window.push_notification(Notification::error(description).title(title), cx)
            })
            .ok();
    }
}

impl LiveChat {
    pub fn new(
        thread_id: ThreadId,
        client: &Entity<ClientStore>,
        bus: CommandBus,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = client.read(cx).handle().clone();
        handle.open_thread(thread_id.clone());
        let timeline = cx.new(|_| ThreadTimelineStore::new(Default::default()));
        let composer = cx.new(|_| ComposerStore::new(ComposerViewModel::default()));
        // `ComposerSource` / `TimelineSource` are `Arc<dyn _>` seams used on the
        // UI thread only, so the `RefCell` inside never crosses threads.
        #[allow(clippy::arc_with_non_send_sync)]
        let source = Arc::new_cyclic(|me| LiveSource {
            me: me.clone(),
            thread_id,
            bus,
            handle,
            timeline: timeline.downgrade(),
            composer: composer.downgrade(),
            overrides: RefCell::default(),
        });
        timeline.update(cx, |store, _| store.set_source(source.clone()));
        composer.update(cx, |store, _| store.set_source(source.clone()));
        let observed = source.clone();
        let subscription = cx.observe(client, move |_, client, cx| {
            let state = client.read(cx).state().clone();
            observed.refresh(&state, cx);
        });
        let state = client.read(cx).state().clone();
        source.refresh(&state, cx);
        // The provider catalogue changes when the registry refreshes.
        let mut providers = source.bus.providers();
        let weak = source.me.clone();
        let client = client.clone();
        cx.spawn(async move |_, cx| {
            while providers.changed().await.is_ok() {
                let alive = cx.update(|cx| {
                    let Some(source) = weak.upgrade() else {
                        return false;
                    };
                    let state = client.read(cx).state().clone();
                    source.refresh(&state, cx);
                    true
                });
                if !alive {
                    break;
                }
            }
        })
        .detach();
        Self {
            source,
            timeline,
            composer,
            _subscription: subscription,
        }
    }

    #[allow(dead_code)]
    pub fn thread_id(&self) -> &ThreadId {
        &self.source.thread_id
    }
}

fn attachment_names(
    attachments: &[turin_contracts::chat_attachment::ChatAttachment],
    images: bool,
) -> Vec<String> {
    use turin_contracts::chat_attachment::ChatAttachment;
    attachments
        .iter()
        .filter_map(|attachment| match attachment {
            ChatAttachment::Image(image) if images => Some(image.name.to_string()),
            ChatAttachment::File(file) if !images => Some(file.name.to_string()),
            _ => None,
        })
        .collect()
}

fn run_is_active(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

fn composer_runtime_mode(mode: turin_contracts::provider_policy::RuntimeMode) -> RuntimeMode {
    RuntimeMode::from_wire(
        &serde_json::to_value(mode)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
    )
    .unwrap_or_default()
}

fn option_json(selection: &OptionSelection) -> Value {
    let value = match &selection.value {
        OptionValue::Str(text) => json!(text),
        OptionValue::Bool(flag) => json!(flag),
    };
    json!({"id": selection.id, "value": value})
}

impl LiveSource {
    fn refresh(&self, state: &ClientState, cx: &mut App) {
        let Some(entry) = state.thread(&self.thread_id) else {
            return;
        };
        let Some(projection) = entry.state.data.as_ref() else {
            return;
        };
        let workspace_root = state
            .shell_view
            .as_ref()
            .and_then(|view| view.project(&projection.thread.project_id))
            .map(|project| project.workspace_root.clone());
        let pending = entry
            .detail
            .as_ref()
            .map(|detail| detail.pending_requests.clone());
        let snapshot = snapshot_from_projection(projection, pending.as_deref(), workspace_root);
        self.timeline
            .update(cx, |store, cx| {
                if **store.snapshot() != snapshot {
                    store.publish(snapshot, cx);
                }
            })
            .ok();
        self.sync_pending_session(entry.detail.as_ref());
        let view_model = self.view_model(projection, entry.detail.as_ref(), cx);
        self.composer
            .update(cx, |store, cx| store.publish(view_model, cx))
            .ok();
    }

    fn view_model(
        &self,
        projection: &ThreadProjection,
        detail: Option<&crate::state::thread_detail::ThreadDetailView>,
        cx: &App,
    ) -> ComposerViewModel {
        let overrides = self.overrides.borrow();
        let thread = &projection.thread;
        let running = projection
            .runs
            .iter()
            .max_by_key(|run| run.ordinal)
            .is_some_and(|run| run_is_active(run.status));
        let (instance_id, model) = overrides.model.clone().unwrap_or_else(|| {
            (
                thread.model_selection.instance_id.to_string(),
                thread.model_selection.model.to_string(),
            )
        });
        let providers = provider_entries(&self.bus.providers().borrow());
        ComposerViewModel {
            thread_key: scoped_thread_key(&local_environment_id(), &self.thread_id),
            phase: if running {
                SessionPhase::Running
            } else {
                SessionPhase::Ready
            },
            can_interrupt: running,
            runtime_mode: overrides
                .runtime_mode
                .unwrap_or_else(|| composer_runtime_mode(thread.runtime_mode)),
            interaction_mode: overrides
                .interaction_mode
                .unwrap_or(match thread.interaction_mode {
                    turin_contracts::provider_policy::ProviderInteractionMode::Plan => {
                        InteractionMode::Plan
                    }
                    turin_contracts::provider_policy::ProviderInteractionMode::Default => {
                        InteractionMode::Default
                    }
                }),
            providers,
            selected_instance_id: instance_id,
            selected_model: model,
            selected_model_options: overrides.model_options.clone().unwrap_or_default(),
            favorite_models: overrides.favorites.clone(),
            queue: detail.and_then(|detail| {
                let workflow = &detail.queue_workflow;
                (!workflow.queued_runs.is_empty()).then(|| QueueState {
                    queued: workflow
                        .queued_runs
                        .iter()
                        .map(|queued| QueuedRun {
                            run_id: queued.run.id.to_string(),
                            message_id: queued.message_id.to_string(),
                            text: queued.text.clone(),
                            image_names: attachment_names(&queued.attachments, true),
                            file_names: attachment_names(&queued.attachments, false),
                            pending: false,
                        })
                        .collect(),
                    can_reorder: workflow.can_reorder,
                    editing_run_id: overrides.editing_run.clone(),
                })
            }),
            prompt_history_messages: projection
                .messages
                .iter()
                .filter(|message| message.role == OrchestrationV2ConversationMessageRole::User)
                .map(|message| ComposerPromptHistoryMessage {
                    id: message.id.to_string(),
                    role: "user".into(),
                    text: message.text.clone(),
                })
                .collect(),
            is_send_busy: overrides.sending,
            restore_draft: overrides.restore.clone(),
            path_entries: overrides.path_entries.clone(),
            path_search_pending: overrides.path_search_pending,
            has_pending_approval: detail
                .map(|detail| {
                    !super::timeline::live_projection::pending_approvals(&detail.pending_requests)
                        .is_empty()
                })
                .unwrap_or(false),
            banners: {
                let state = self.handle.snapshot();
                super::live_wiring::usage_limit_recovery_banner(&state, &self.thread_id)
                    .into_iter()
                    .collect()
            },
            environment_id: local_environment_id().to_string(),
            pending_answer: detail.and_then(|detail| {
                let pending = message_mode_pending_input(&detail.pending_requests)?;
                let session = self.overrides.borrow().pending_input.clone();
                Some(pending_answer_for_message_input(
                    pending,
                    &session,
                    local_environment_id().as_str(),
                    self.thread_id.as_str(),
                    "",
                    cx,
                ))
            }),
            ..ComposerViewModel::default()
        }
    }

    fn sync_pending_session(&self, detail: Option<&crate::state::thread_detail::ThreadDetailView>) {
        let input = detail.and_then(|detail| message_mode_pending_input(&detail.pending_requests));
        let mut overrides = self.overrides.borrow_mut();
        sync_pending_input_session(&mut overrides.pending_input, input);
    }

    fn update_overrides(&self, cx: &mut App, edit: impl FnOnce(&mut Overrides)) {
        edit(&mut self.overrides.borrow_mut());
        let state = self.handle.snapshot();
        self.refresh(&state, cx);
    }

    /// The first message of a thread seeds its temporary title, and the title
    /// effect replaces it with a generated one (`titleSeed`).
    fn title_seed(&self, prompt: &str) -> Option<String> {
        let state = self.handle.snapshot();
        let data = state.thread(&self.thread_id)?.state.data.clone()?;
        let has_user_message = data
            .messages
            .iter()
            .any(|message| message.role == OrchestrationV2ConversationMessageRole::User);
        if has_user_message {
            return None;
        }
        let line = prompt
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        Some(line.chars().take(60).collect())
    }

    /// The thread's active run, which a steer or restart targets.
    fn active_run_id(&self) -> Option<String> {
        let state = self.handle.snapshot();
        let data = state.thread(&self.thread_id)?.state.data.clone()?;
        data.runs
            .iter()
            .filter(|run| {
                !matches!(
                    run.status,
                    OrchestrationV2RunStatus::Completed
                        | OrchestrationV2RunStatus::Failed
                        | OrchestrationV2RunStatus::Cancelled
                        | OrchestrationV2RunStatus::Interrupted
                        | OrchestrationV2RunStatus::RolledBack
                )
            })
            .max_by_key(|run| run.ordinal)
            .map(|run| run.id.to_string())
    }

    fn thread_modes(&self) -> Option<(RuntimeMode, InteractionMode)> {
        let state = self.handle.snapshot();
        let data = state.thread(&self.thread_id)?.state.data.clone()?;
        Some((
            composer_runtime_mode(data.thread.runtime_mode),
            match data.thread.interaction_mode {
                turin_contracts::provider_policy::ProviderInteractionMode::Plan => {
                    InteractionMode::Plan
                }
                turin_contracts::provider_policy::ProviderInteractionMode::Default => {
                    InteractionMode::Default
                }
            },
        ))
    }

    fn queue_command(&self, title: &'static str, cx: &mut App, kind: &'static str, extra: Value) {
        let thread_id = self.thread_id.clone();
        self.run_command(title, cx, move |commands| async move {
            commands.thread_command(kind, &thread_id, extra).await
        });
    }

    /// `queued-run.edit`: replaces the queued message's text.
    fn edit_queued(&self, run_id: String, text: String, cx: &mut App) -> bool {
        let thread_id = self.thread_id.clone();
        let result = self.bus.run_blocking(move |commands| async move {
            commands
                .thread_command(
                    "queued-run.edit",
                    &thread_id,
                    json!({"runId": run_id, "text": text}),
                )
                .await
        });
        match result {
            Ok(()) => {
                self.update_overrides(cx, |overrides| overrides.editing_run = None);
                true
            }
            Err(error) => {
                push_error(cx, "Failed to edit the queued message", &error.message);
                false
            }
        }
    }

    /// Runs a command off the UI thread; a failure toasts `title`.
    fn run_command<Fut>(
        &self,
        title: &'static str,
        cx: &mut App,
        command: impl FnOnce(crate::app_runtime::write::commands::Commands) -> Fut + Send + 'static,
    ) where
        Fut: std::future::Future<
                Output = Result<(), crate::app_runtime::write::commands::CommandError>,
            > + Send
            + 'static,
    {
        let task = self.bus.task(cx, command);
        cx.spawn(async move |cx| {
            if let Err(error) = task.await {
                cx.update(|cx| push_error(cx, title, &error.message));
            }
        })
        .detach();
    }

    fn not_wired(&self, what: &str, cx: &mut App) {
        push_error(cx, what, "This is not available yet.");
    }
}

impl ComposerSource for LiveSource {
    fn send(&self, request: SendRequest, cx: &mut App) -> bool {
        if request.answering_pending_user_input {
            return self.submit_composer_pending_answer(&request.prompt, cx);
        }
        if let Some(run_id) = request.editing_queued_run.clone() {
            return self.edit_queued(run_id, request.prompt, cx);
        }
        let delivery = match request.dispatch_mode {
            ComposerDispatchMode::Auto => Delivery::StartImmediately,
            ComposerDispatchMode::Queue => Delivery::QueueAfterActive,
            ComposerDispatchMode::Steer | ComposerDispatchMode::Restart => {
                let Some(target_run_id) = self.active_run_id() else {
                    // The turn ended between the click and here: start a fresh one.
                    return self.send_with(request, Delivery::StartImmediately, cx);
                };
                if request.dispatch_mode == ComposerDispatchMode::Steer {
                    Delivery::SteerActive { target_run_id }
                } else {
                    Delivery::RestartActive { target_run_id }
                }
            }
        };
        self.send_with(request, delivery, cx)
    }

    fn interrupt(&self, cx: &mut App) {
        let thread_id = self.thread_id.clone();
        let (done, outcome) = futures::channel::oneshot::channel();
        self.bus.spawn(
            move |commands| async move { commands.stop_thread(&thread_id).await },
            move |result| {
                let _ = done.send(result);
            },
        );
        cx.spawn(async move |cx| {
            if let Ok(Err(error)) = outcome.await {
                cx.update(|cx| push_error(cx, "Failed to stop the turn", &error.message));
            }
        })
        .detach();
    }

    fn resume(&self, cx: &mut App) {
        self.not_wired("Resume", cx);
    }

    fn implement_plan_in_new_thread(&self, cx: &mut App) {
        self.not_wired("Implement plan in a new thread", cx);
    }

    fn select_model(&self, instance_id: &str, model: &str, cx: &mut App) {
        let (instance_id, model) = (instance_id.to_owned(), model.to_owned());
        self.update_overrides(cx, |overrides| {
            overrides.model = Some((instance_id, model));
            overrides.model_options = None;
        });
    }

    fn set_model_options(
        &self,
        instance_id: &str,
        model: &str,
        options: Option<Vec<OptionSelection>>,
        cx: &mut App,
    ) {
        let (instance_id, model) = (instance_id.to_owned(), model.to_owned());
        self.update_overrides(cx, |overrides| {
            overrides.model = Some((instance_id, model));
            overrides.model_options = options;
        });
    }

    fn set_runtime_mode(&self, mode: RuntimeMode, cx: &mut App) {
        self.update_overrides(cx, |overrides| overrides.runtime_mode = Some(mode));
    }

    fn set_interaction_mode(&self, mode: InteractionMode, cx: &mut App) {
        self.update_overrides(cx, |overrides| overrides.interaction_mode = Some(mode));
    }

    fn toggle_favorite_model(&self, instance_id: &str, model: &str, cx: &mut App) {
        let key = crate::composer::model_picker::provider_model_key(instance_id, model);
        self.update_overrides(cx, |overrides| {
            if !overrides.favorites.remove(&key) {
                overrides.favorites.insert(key);
            }
        });
    }

    fn open_provider_setup(&self, _instance_id: &str, cx: &mut App) {
        self.not_wired("Provider setup", cx);
    }

    fn toggle_keep_full_history(&self, _cx: &mut App) {}

    fn compact_context(&self, cx: &mut App) {
        self.not_wired("Compact context", cx);
    }

    fn dismiss_banner(&self, id: &str, _cx: &mut App) {
        if id.starts_with("usage-limit-recovery:") {}
    }

    fn banner_action(&self, banner_id: &str, action_id: &str, cx: &mut App) {
        if !banner_id.starts_with("usage-limit-recovery:") {
            self.not_wired("Composer banner action", cx);
            return;
        }
        use super::usage_limit_recovery_banner::{
            RecoveryAction, RecoveryToggle, plan_recovery_toggle,
        };
        let Some(action) = RecoveryAction::from_id(action_id) else {
            return;
        };
        let state = self.handle.snapshot();
        let Some(props) = super::live_wiring::usage_limit_recovery_props(&state, &self.thread_id)
        else {
            return;
        };
        match plan_recovery_toggle(&props, action, chrono::Utc::now().timestamp_millis()) {
            RecoveryToggle::Ignore => {}
            RecoveryToggle::Error(message) => push_error(cx, "Usage limit recovery", &message),
            RecoveryToggle::Update(input) => {
                let Some(update) = input.into_update() else {
                    return;
                };
                self.queue_command(
                    "Failed to save usage limit recovery",
                    cx,
                    "thread.metadata.update",
                    json!({ "limitRecovery": update }),
                );
            }
        }
    }

    fn edit_queued_run(&self, run_id: &str, cx: &mut App) {
        let run_id = run_id.to_owned();
        self.update_overrides(cx, |overrides| overrides.editing_run = Some(run_id));
    }

    fn cancel_queued_edit(&self, cx: &mut App) {
        self.update_overrides(cx, |overrides| overrides.editing_run = None);
    }

    fn cancel_queued_run(&self, run_id: &str, cx: &mut App) {
        self.queue_command(
            "Failed to cancel the queued message",
            cx,
            "queued-run.cancel",
            json!({"runId": run_id}),
        );
    }

    fn promote_queued_run(&self, run_id: &str, cx: &mut App) {
        let Some(target) = self.active_run_id() else {
            push_error(cx, "Failed to steer", "The turn already ended.");
            return;
        };
        self.queue_command(
            "Failed to steer with the queued message",
            cx,
            "queued-message.promote-to-steer",
            json!({"queuedRunId": run_id, "targetRunId": target}),
        );
    }

    fn reorder_queued_run(&self, run_id: &str, to_index: usize, cx: &mut App) {
        // `beforeRunId`: the run the moved one lands in front of, `null` for the end.
        let state = self.handle.snapshot();
        let before = state
            .thread(&self.thread_id)
            .and_then(|entry| entry.detail.as_ref())
            .map(|detail| {
                detail
                    .queue_workflow
                    .queued_runs
                    .iter()
                    .filter(|queued| queued.run.id.as_str() != run_id)
                    .nth(to_index)
                    .map(|queued| queued.run.id.to_string())
            })
            .unwrap_or_default();
        self.queue_command(
            "Failed to reorder the queue",
            cx,
            "queued-run.reorder",
            json!({"runId": run_id, "beforeRunId": before}),
        );
    }

    fn respond_to_pending_input(&self, _request_id: &str, answer: &str, cx: &mut App) {
        let _ = self.submit_composer_pending_answer(answer, cx);
    }

    fn previous_pending_question(&self, cx: &mut App) {
        self.update_overrides(cx, |overrides| {
            if overrides.pending_input.question_index > 0 {
                overrides.pending_input.question_index -= 1;
            }
        });
    }

    /// `@` completion over the project's workspace.
    fn search_paths(&self, query: &str, cx: &mut App) {
        let state = self.handle.snapshot();
        let Some(cwd) = state
            .thread(&self.thread_id)
            .and_then(|entry| entry.state.data.clone())
            .and_then(|data| {
                state
                    .shell_view
                    .as_ref()?
                    .project(&data.thread.project_id)
                    .map(|project| project.workspace_root.clone())
            })
        else {
            return;
        };
        let query = query.to_owned();
        let mut generation = 0;
        self.update_overrides(cx, |overrides| {
            overrides.path_search_generation += 1;
            overrides.path_search_pending = true;
            generation = overrides.path_search_generation;
        });
        let task = self.bus.task(cx, move |commands| async move {
            commands.search_paths(&cwd, &query, 80).await
        });
        let source = self.me.clone();
        cx.spawn(async move |cx| {
            let found = task.await;
            cx.update(|cx| {
                let Some(source) = source.upgrade() else {
                    return;
                };
                if source.overrides.borrow().path_search_generation != generation {
                    return;
                }
                source.update_overrides(cx, |overrides| {
                    overrides.path_search_pending = false;
                    if let Ok(entries) = found {
                        overrides.path_entries = entries
                            .into_iter()
                            .map(|(path, directory)| PathEntry {
                                path,
                                kind: if directory {
                                    PathKind::Directory
                                } else {
                                    PathKind::File
                                },
                            })
                            .collect();
                    }
                });
            });
        })
        .detach();
    }
}

impl LiveSource {
    fn deliver_pending_user_input(
        &self,
        request_id: &str,
        answers: IndexMap<String, String>,
        cx: &mut App,
    ) -> bool {
        let environment_id = local_environment_id();
        let state = self.handle.snapshot();
        let Some(detail) = state
            .thread(&self.thread_id)
            .and_then(|entry| entry.detail.as_ref())
        else {
            return false;
        };
        let Some(input) = detail
            .pending_requests
            .user_inputs
            .iter()
            .find(|input| input.request_id.as_str() == request_id)
        else {
            return false;
        };
        let question_ids: Vec<String> = input
            .questions
            .iter()
            .map(|question| question.id.to_string())
            .collect();
        let draft_attachments = super::question_attachment_submit::load_question_drafts(
            cx,
            environment_id.as_str(),
            self.thread_id.as_str(),
            request_id,
            &question_ids,
        );
        let thread_id = self.thread_id.clone();
        let request_id = request_id.to_owned();
        let request_to_send = request_id.clone();
        let result = self.bus.run_blocking(move |commands| async move {
            super::question_attachment_submit::respond_to_pending_user_input_with_attachments(
                &commands,
                &thread_id,
                request_to_send.as_str(),
                answers,
                &draft_attachments,
            )
            .await
        });
        match result {
            Ok(()) => {
                super::question_attachment_submit::clear_question_attachment_drafts_for_request(
                    cx,
                    environment_id.as_str(),
                    self.thread_id.as_str(),
                    request_id.as_str(),
                    &question_ids,
                );
                true
            }
            Err(error) => {
                push_error(cx, "Failed to submit your answer", &error.message);
                false
            }
        }
    }

    fn submit_composer_pending_answer(&self, composer_prompt: &str, cx: &mut App) -> bool {
        let state = self.handle.snapshot();
        let Some(detail) = state
            .thread(&self.thread_id)
            .and_then(|entry| entry.detail.as_ref())
        else {
            return false;
        };
        let Some(input) = message_mode_pending_input(&detail.pending_requests) else {
            return false;
        };
        let plan = {
            let mut overrides = self.overrides.borrow_mut();
            apply_composer_prompt_to_session(
                input,
                &mut overrides.pending_input,
                local_environment_id().as_str(),
                self.thread_id.as_str(),
                composer_prompt,
                cx,
            )
        };
        match plan {
            PendingSubmitPlan::Rejected => false,
            PendingSubmitPlan::Advanced => true,
            PendingSubmitPlan::Submit {
                request_id,
                answers,
            } => {
                self.update_overrides(cx, |overrides| overrides.pending_input.responding = true);
                let answers: IndexMap<_, _> = answers.into_iter().collect();
                let delivered = self.deliver_pending_user_input(request_id.as_str(), answers, cx);
                self.update_overrides(cx, |overrides| overrides.pending_input.responding = false);
                delivered
            }
        }
    }

    /// Dispatches `message.dispatch` (after any thread-mode change) on the
    /// runtime, without blocking the UI thread. The composer clears its draft
    /// when this returns; a failure toasts and hands the draft back
    /// (`DraftRestore`), as upstream restores its optimistic send.
    fn send_with(&self, request: SendRequest, delivery: Delivery, cx: &mut App) -> bool {
        if self.overrides.borrow().sending {
            return false;
        }
        let selection = {
            let mut selection = json!({
                "instanceId": request.instance_id,
                "model": request.model,
            });
            if let Some(options) = &request.model_options {
                selection["options"] = Value::Array(options.iter().map(option_json).collect());
            }
            selection
        };
        let mode_changes = self.thread_modes().map(|(runtime, interaction)| {
            (
                (request.runtime_mode != runtime).then(|| request.runtime_mode.wire()),
                (request.interaction_mode != interaction).then_some(
                    match request.interaction_mode {
                        InteractionMode::Plan => "plan",
                        InteractionMode::Default => "default",
                    },
                ),
            )
        });
        let title_seed = self.title_seed(&request.prompt);
        let restore = (
            request.prompt.clone(),
            request.images.clone(),
            request.files.clone(),
        );
        let (prompt, images, files) = (request.prompt, request.images, request.files);
        let thread_id = self.thread_id.clone();
        let task = self.bus.task(cx, move |commands| async move {
            let work = async {
                // Uploads are staged first; the intake claims them with the message.
                let mut attachments = Vec::new();
                for image in &images {
                    attachments.push(
                        commands
                            .stage_attachment(
                                true,
                                &image.name,
                                &image.mime_type,
                                (*image.data).clone(),
                            )
                            .await?,
                    );
                }
                for file in &files {
                    let Some(data) = &file.data else {
                        return Err(CommandError::new(format!(
                            "{} has no content to upload.",
                            file.name
                        )));
                    };
                    attachments.push(
                        commands
                            .stage_attachment(false, &file.name, &file.mime_type, (**data).clone())
                            .await?,
                    );
                }
                if let Some((runtime, interaction)) = mode_changes {
                    if let Some(runtime_mode) = runtime {
                        commands
                            .thread_command(
                                "thread.runtime-mode.set",
                                &thread_id,
                                json!({"runtimeMode": runtime_mode}),
                            )
                            .await?;
                    }
                    if let Some(interaction_mode) = interaction {
                        commands
                            .thread_command(
                                "thread.interaction-mode.set",
                                &thread_id,
                                json!({"interactionMode": interaction_mode}),
                            )
                            .await?;
                    }
                }
                commands
                    .send_message(SendMessage {
                        thread_id: thread_id.clone(),
                        text: prompt,
                        attachments,
                        model_selection: Some(selection),
                        delivery,
                        title_seed,
                    })
                    .await
            };
            tokio::time::timeout(SEND_TIMEOUT, work)
                .await
                .unwrap_or_else(|_| {
                    Err(CommandError::new("Timed out waiting for the orchestrator."))
                })
        });
        self.update_overrides(cx, |overrides| overrides.sending = true);
        let source = self.me.clone();
        cx.spawn(async move |cx| {
            let outcome = task.await;
            cx.update(|cx| {
                let Some(source) = source.upgrade() else {
                    return;
                };
                source.update_overrides(cx, |overrides| {
                    overrides.sending = false;
                    if outcome.is_err() {
                        overrides.restore_nonce += 1;
                        overrides.restore = Some(DraftRestore {
                            nonce: overrides.restore_nonce,
                            prompt: restore.0,
                            images: restore.1,
                            files: restore.2,
                        });
                    }
                });
                if let Err(error) = outcome {
                    push_error(cx, "Failed to send message", &error.message);
                }
            });
        })
        .detach();
        true
    }
}

impl TimelineSource for LiveSource {
    fn respond_to_approval(
        &self,
        request_id: &str,
        decision: ProviderApprovalDecision,
        cx: &mut App,
    ) {
        let (thread_id, request_id) = (self.thread_id.clone(), request_id.to_owned());
        self.run_command(
            "Failed to respond to approval",
            cx,
            move |commands| async move {
                commands
                    .respond_to_request(
                        &thread_id,
                        &request_id,
                        Some(decision_wire(decision)),
                        None,
                    )
                    .await
            },
        );
    }

    fn respond_to_user_input(
        &self,
        request_id: &str,
        answers: Vec<(String, String)>,
        cx: &mut App,
    ) -> bool {
        let answers: IndexMap<_, _> = answers.into_iter().collect();
        self.deliver_pending_user_input(request_id, answers, cx)
    }

    fn dismiss_user_input(&self, request_id: &str, cx: &mut App) {
        let (thread_id, request_id) = (self.thread_id.clone(), request_id.to_owned());
        self.run_command("Failed to dismiss the question", cx, move |commands| async move {
            commands.dismiss_user_input(&thread_id, &request_id).await
        });
    }

    fn revert_to_turn_count(&self, _turn_count: u64, _message_id: &str, cx: &mut App) {
        self.not_wired("Reverting to a turn", cx);
    }

    fn open_turn_diff(&self, run_id: &str, file_path: Option<&str>, cx: &mut App) {
        crate::diff::request_open_turn_diff(run_id, file_path, cx);
    }

    fn open_thread(&self, thread_id: &str, cx: &mut App) {
        super::thread_navigation::request_open_thread(thread_id, cx);
    }
}
