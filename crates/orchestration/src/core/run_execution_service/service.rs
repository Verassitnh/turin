//! Port of upstream/apps/server/src/orchestration-v2/RunExecutionService.ts (lines 488-1510).
//!
//! The effectful layer of `RunExecutionService.ts`: errors, the
//! seams it reads and writes through, and `startRootRun`.
//!
//! `startRootRun` prepares the run, subscribes to the session's events,
//! spawns the ingestion task, and calls the provider. The task outlives the
//! call: it keeps writing provider events until the run's background work
//! settles. Upstream forks it with `Effect.forkDetach`; here it is a spawned
//! tokio task, and "interrupt the fiber" is a cancel token plus a join.

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use turin_contracts::{
    CommandId, NodeId, ProjectId, ProviderSessionId, ProviderThreadId, ProviderTurnId,
    RunAttemptId, RunId, ThreadId, TurnItemId,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2CheckpointScope, OrchestrationV2ExecutionNode,
        OrchestrationV2ExecutionNodeKind, OrchestrationV2ProviderFailure,
        OrchestrationV2ProviderFailureClass, OrchestrationV2ProviderThread,
        OrchestrationV2ProviderThreadDisposition, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2Run, OrchestrationV2RunAttempt, OrchestrationV2RunStatus,
        OrchestrationV2SubagentOrigin, OrchestrationV2TurnItem,
    },
};
use turin_provider_core::{
    failure::{ProviderFailureCause, ProviderFailureInput, make_provider_failure},
    id_allocator::derive,
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2Error, ProviderAdapterV2Event,
        ProviderAdapterV2EventSubscription, ProviderAdapterV2RuntimePolicy,
        ProviderAdapterV2TurnInput, ProviderAdapterV2TurnMessage,
        ProviderAdapterV2TurnTerminalStatus, ProviderSessionRuntime,
    },
    rt::BoxFuture,
};

use super::{
    InheritedBackgroundTurnItemRoute, OpenRunOwnedSubagentProjection, ProviderEventRouteIdentity,
    ProviderEventRoutingState, finalize::FinalRunInput, finalize::RunCurrentGate,
    finalize::run_owned_terminal_status, is_background_capable, is_open_execution_node_status,
    is_subagent_work_active, is_turn_item_work_active, make_provider_event_routing_state,
    route_provider_event,
};
use crate::{
    core::provider_event_ingestor::{
        IngestGuard, ProviderEventIngestor, ProviderEventInput, ProviderThreadOwnerGuard,
        ProviderTurnAnalyticsContext, RunCurrentGuard,
    },
    pipeline::{
        event_sink::{EventSink, RunCurrentWrite, WriteInput},
        orchestrator_port::BoxError,
        seams::ServerSettingsSource,
    },
    projection::{assistant_streaming::AssistantStreamingFilter, turn_item},
    reactors::checkpoint_service::CheckpointService,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `RunExecutionStartError`.
#[derive(Debug, thiserror::Error)]
#[error("Failed to start orchestration V2 run execution {run_id}.")]
pub struct RunExecutionStartError {
    pub command_id: CommandId,
    pub run_id: String,
    #[source]
    pub cause: BoxError,
}

/// `RunExecutionIngestError`.
#[derive(Debug, thiserror::Error)]
#[error("Failed while ingesting orchestration V2 run execution {run_id}.")]
pub struct RunExecutionIngestError {
    pub run_id: String,
    #[source]
    pub cause: BoxError,
}

/// `RunExecutionServiceV2Error`.
#[derive(Debug, thiserror::Error)]
pub enum RunExecutionServiceError {
    #[error(transparent)]
    Start(#[from] RunExecutionStartError),
    #[error(transparent)]
    Ingest(#[from] RunExecutionIngestError),
}

/// Upstream `cause: { start, write }` / `{ ingest, write }`: the original
/// failure and the failure of the terminal write that tried to record it.
#[derive(Debug, thiserror::Error)]
#[error("{label} failed ({first}), and recording the failed run also failed ({write}).")]
pub struct FailureAndWriteCause {
    pub label: &'static str,
    pub first: BoxError,
    pub write: BoxError,
}

// ---------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------

/// The writes of `EventSinkV2` this service makes.
#[async_trait]
pub trait RunEventWriter: Send + Sync {
    /// `writeWithEffects`.
    async fn write(&self, input: WriteInput) -> Result<(), BoxError>;
    /// `writeIfRunCurrent`; `true` when the write committed.
    async fn write_if_run_current(&self, input: RunCurrentWrite) -> Result<bool, BoxError>;
}

#[async_trait]
impl RunEventWriter for EventSink {
    async fn write(&self, input: WriteInput) -> Result<(), BoxError> {
        EventSink::write(self, input)
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn write_if_run_current(&self, input: RunCurrentWrite) -> Result<bool, BoxError> {
        EventSink::write_if_run_current(self, input)
            .await
            .map(|result| result.committed)
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// `ProviderEventIngestorV2.ingestNormalized`; returns the number of stored events.
#[async_trait]
pub trait RunEventIngest: Send + Sync {
    async fn ingest_normalized(
        &self,
        input: &ProviderEventInput,
        guard: &IngestGuard,
    ) -> Result<usize, BoxError>;
}

#[async_trait]
impl RunEventIngest for ProviderEventIngestor {
    async fn ingest_normalized(
        &self,
        input: &ProviderEventInput,
        guard: &IngestGuard,
    ) -> Result<usize, BoxError> {
        self.ingest_under_thread_lock(input, guard)
            .await
            .map(|stored| stored.len())
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// `CheckpointServiceV2.captureBaseline`.
#[async_trait]
pub trait RunBaselineCapture: Send + Sync {
    async fn capture_baseline(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        ordinal_within_scope: u64,
    ) -> Result<(), BoxError>;
}

#[async_trait]
impl RunBaselineCapture for CheckpointService {
    async fn capture_baseline(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        ordinal_within_scope: u64,
    ) -> Result<(), BoxError> {
        CheckpointService::capture_baseline(self, scope, ordinal_within_scope)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// `RunFinalizationObserver.refreshAfterTurn`: refreshes the project's pull
/// requests after a run ended. A failure is logged and never fails the run.
#[async_trait]
pub trait AfterTurnRefresh: Send + Sync {
    async fn refresh_after_turn(&self, project_id: &ProjectId) -> Result<(), BoxError>;
}

/// [`AfterTurnRefresh`] until the pull request reactors are wired (T13d).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAfterTurnRefresh;

#[async_trait]
impl AfterTurnRefresh for NoAfterTurnRefresh {
    async fn refresh_after_turn(&self, _project_id: &ProjectId) -> Result<(), BoxError> {
        Ok(())
    }
}

/// An async check the caller supplies (`shouldStartProviderTurn`,
/// `shouldFinalizeRun`, `hasUnpairedRunInterruptRequest`). It is called more
/// than once, so it must read fresh state each time.
pub type AsyncBool = Arc<dyn Fn() -> BoxFuture<'static, Result<bool, BoxError>> + Send + Sync>;

/// `loadInheritedBackgroundTurnItems`.
pub type InheritedBackgroundLoader = Arc<
    dyn Fn() -> BoxFuture<'static, Result<Vec<InheritedBackgroundTurnItemRoute>, BoxError>>
        + Send
        + Sync,
>;

/// `RunExecutionServiceV2StartRootRunInput`.
pub struct StartRootRunInput {
    pub command_id: CommandId,
    pub app_thread: OrchestrationV2AppThread,
    pub provider_session_id: ProviderSessionId,
    pub session: Arc<dyn ProviderSessionRuntime>,
    pub run: OrchestrationV2Run,
    pub root_node: OrchestrationV2ExecutionNode,
    pub checkpoint_scope: OrchestrationV2CheckpointScope,
    pub provider_thread: OrchestrationV2ProviderThread,
    pub attempt: OrchestrationV2RunAttempt,
    pub attempt_id: RunAttemptId,
    pub provider_turn_ordinal: i64,
    pub native_thread_has_turns: Option<bool>,
    pub load_inherited_background_turn_items: Option<InheritedBackgroundLoader>,
    pub related_thread_ids: Vec<ThreadId>,
    pub related_provider_thread_ids: Vec<ProviderThreadId>,
    pub should_start_provider_turn: Option<AsyncBool>,
    pub should_finalize_run: Option<AsyncBool>,
    pub has_unpaired_run_interrupt_request: Option<AsyncBool>,
    pub message: ProviderAdapterV2TurnMessage,
    pub model_selection: ModelSelection,
    pub runtime_policy: ProviderAdapterV2RuntimePolicy,
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// `ProviderTerminalEvent`: the `turn.terminal` fields finalization reads.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSummary {
    pub driver: turin_contracts::provider_instance::ProviderDriverKind,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub status: ProviderAdapterV2TurnTerminalStatus,
    pub failure: Option<OrchestrationV2ProviderFailure>,
    pub failure_item_ordinal: Option<turin_contracts::PositiveInt>,
    pub thread_disposition: OrchestrationV2ProviderThreadDisposition,
}

impl TerminalSummary {
    fn from_event(event: &ProviderAdapterV2Event) -> Option<Self> {
        let ProviderAdapterV2Event::TurnTerminal {
            driver,
            provider_thread_id,
            provider_turn_id,
            status,
            failure,
            failure_item_ordinal,
            thread_disposition,
            ..
        } = event
        else {
            return None;
        };
        Some(Self {
            driver: driver.clone(),
            provider_thread_id: provider_thread_id.clone(),
            provider_turn_id: provider_turn_id.clone(),
            status: *status,
            failure: failure.clone(),
            failure_item_ordinal: *failure_item_ordinal,
            thread_disposition: *thread_disposition,
        })
    }
}

/// `Effect.cached(finalizationObserver.refreshAfterTurn(...))`: startup
/// failure and stream shutdown can report the same attempt, so the refresh
/// runs at most once per run.
pub struct RefreshOnce {
    refresh: Arc<dyn AfterTurnRefresh>,
    project_id: ProjectId,
    thread_id: ThreadId,
    run_id: RunId,
    done: tokio::sync::OnceCell<()>,
}

impl RefreshOnce {
    fn new(
        refresh: Arc<dyn AfterTurnRefresh>,
        project_id: ProjectId,
        thread_id: ThreadId,
        run_id: RunId,
    ) -> Self {
        Self {
            refresh,
            project_id,
            thread_id,
            run_id,
            done: tokio::sync::OnceCell::new(),
        }
    }

    pub(super) async fn run(&self) {
        self.done
            .get_or_init(|| async {
                if let Err(cause) = self.refresh.refresh_after_turn(&self.project_id).await {
                    tracing::warn!(
                        thread_id = %self.thread_id,
                        run_id = %self.run_id,
                        error = %cause,
                        "failed to refresh pull requests after run termination"
                    );
                }
            })
            .await;
    }
}

pub(super) struct RunExecutionInner {
    pub(super) baseline: Arc<dyn RunBaselineCapture>,
    pub(super) events: Arc<dyn RunEventWriter>,
    pub(super) ingestor: Arc<dyn RunEventIngest>,
    pub(super) settings: Arc<dyn ServerSettingsSource>,
    pub(super) refresh: Arc<dyn AfterTurnRefresh>,
}

/// `RunExecutionServiceV2`.
#[derive(Clone)]
pub struct RunExecutionService {
    inner: Arc<RunExecutionInner>,
}

impl RunExecutionService {
    pub fn new(
        baseline: Arc<dyn RunBaselineCapture>,
        events: Arc<dyn RunEventWriter>,
        ingestor: Arc<dyn RunEventIngest>,
        settings: Arc<dyn ServerSettingsSource>,
        refresh: Arc<dyn AfterTurnRefresh>,
    ) -> Self {
        Self {
            inner: Arc::new(RunExecutionInner {
                baseline,
                events,
                ingestor,
                settings,
                refresh,
            }),
        }
    }

    /// `startRootRun`. Returns once the provider accepted the turn (or the run
    /// was recorded as failed or skipped). Ingestion continues in a task.
    pub async fn start_root_run(
        &self,
        input: StartRootRunInput,
    ) -> Result<(), RunExecutionServiceError> {
        let input = Arc::new(input);
        let refresh = Arc::new(RefreshOnce::new(
            self.inner.refresh.clone(),
            input.app_thread.project_id.clone(),
            input.run.thread_id.clone(),
            input.run.id.clone(),
        ));
        let start_error = |cause: BoxError| RunExecutionStartError {
            command_id: input.command_id.clone(),
            run_id: input.run.id.to_string(),
            cause,
        };

        let streaming_mode = match self.prepare(&input).await {
            Ok(Some(mode)) => mode,
            Ok(None) => return Ok(()),
            Err(cause) => {
                tracing::error!(
                    run_id = %input.run.id,
                    error = %cause,
                    "orchestration V2 run preparation failed"
                );
                let failure = make_provider_failure(ProviderFailureInput {
                    cause: Some(&failure_cause(&*cause)),
                    // Keep exact underlying text in the logged cause only;
                    // the persisted turn item gets a bounded curated message.
                    message: Some("Run preparation failed."),
                    code: None,
                    class: Some(OrchestrationV2ProviderFailureClass::Unknown),
                    retryable: None,
                    reset_at: None,
                });
                let terminal = make_failed_terminal_event(
                    &input,
                    failure,
                    u64::try_from(input.provider_turn_ordinal)
                        .unwrap_or(0)
                        .saturating_mul(100)
                        .saturating_add(1),
                );
                self.inner
                    .write_final_run_events(FinalRunInput {
                        run: &input.run,
                        root_node: &input.root_node,
                        checkpoint_scope: &input.checkpoint_scope,
                        provider_thread: &input.provider_thread,
                        attempt: &input.attempt,
                        should_finalize_run: None,
                        has_unpaired_run_interrupt_request: None,
                        open_run_owned_subagents: None,
                        terminal: &terminal,
                        failure_item_persisted: false,
                        refresh: &refresh,
                        write_if_run_current: Some(RunCurrentGate {
                            active_attempt_id: input.attempt_id.clone(),
                            expected_status: OrchestrationV2RunStatus::Running,
                        }),
                    })
                    .await
                    .map_err(start_error)?;
                return Ok(());
            }
        };

        let subscription = match input.session.subscribe_events() {
            Some(subscribe) => subscribe
                .await
                .map_err(|error| start_error(Box::new(error)))?,
            None => ProviderAdapterV2EventSubscription {
                events: input.session.events(),
                close: Box::pin(async {}),
            },
        };
        let inherited = match &input.load_inherited_background_turn_items {
            Some(load) => match load().await {
                Ok(items) => items,
                Err(cause) => {
                    subscription.close.await;
                    return Err(start_error(cause).into());
                }
            },
            None => Vec::new(),
        };

        let identity = ProviderEventRouteIdentity {
            thread_id: input.run.thread_id.clone(),
            run_id: input.run.id.clone(),
            attempt_id: input.attempt.id.clone(),
            provider_thread_id: input.provider_thread.id.clone(),
        };
        let routing = make_provider_event_routing_state(
            &identity,
            &inherited,
            input.attempt.provider_turn_id.as_ref(),
            &input.related_thread_ids,
            &input.related_provider_thread_ids,
        );
        let ctx = Arc::new(RunContext {
            service: self.inner.clone(),
            input: input.clone(),
            refresh: refresh.clone(),
            identity,
            inherited_by_id: inherited
                .iter()
                .map(|item| (item.id.clone(), item.run_id.clone()))
                .collect(),
            state: Mutex::new(RunState {
                terminal: None,
                latest_turn_item_ordinal: u64::try_from(input.provider_turn_ordinal)
                    .unwrap_or(0)
                    .saturating_mul(100),
                latest_provider_thread: input.provider_thread.clone(),
                routing,
                root_terminal_seen: false,
                root_run_finalized: false,
                provider_thread_owner_lost: false,
                active_child_provider_turns: HashSet::new(),
                active_child_subagents: HashSet::new(),
                active_background_turn_items: inherited
                    .iter()
                    .map(|item| item.id.clone())
                    .collect(),
                open_run_owned_subagents: OpenRunOwnedSubagentProjection::default(),
            }),
        });
        let cancel = CancellationToken::new();
        let task = tokio::spawn(ctx.clone().run_ingestion(
            subscription,
            streaming_mode,
            cancel.clone(),
        ));

        // A failed read fails the start below, so the run is recorded as
        // failed instead of staying active with no provider turn.
        let should_start: Result<bool, BoxError> = match &input.should_start_provider_turn {
            None => Ok(true),
            Some(check) => check().await,
        };
        if matches!(should_start, Ok(false)) {
            interrupt_task(&cancel, task).await;
            return Ok(());
        }

        // The thread's MCP app context and credential liveness are not ported:
        // the MCP apps host is on the cut list, so there is no context to read.
        let turn_input = ProviderAdapterV2TurnInput {
            app_thread: input.app_thread.clone(),
            thread_id: input.run.thread_id.clone(),
            run_id: input.run.id.clone(),
            run_ordinal: i64::try_from(input.run.ordinal.get()).unwrap_or(i64::MAX),
            native_thread_has_turns: input.native_thread_has_turns,
            provider_turn_ordinal: input.provider_turn_ordinal,
            restart_continuation_of_run_id: input.run.restart_continuation_of_run_id.clone(),
            attempt_id: input.attempt_id.clone(),
            root_node_id: input.root_node.id.clone(),
            provider_thread: input.provider_thread.clone(),
            message: input.message.clone(),
            model_selection: input.model_selection.clone(),
            runtime_policy: input.runtime_policy.clone(),
            app_context: None,
        };
        let compact = input.message.attachments.is_empty()
            && turin_contracts::base_schemas::js_trim(&input.message.text).to_lowercase()
                == "/compact";
        let started: Result<(), BoxError> = match &should_start {
            Err(_) => Ok(()),
            Ok(_) if compact => match input.session.compact_thread(turn_input) {
                Some(compacting) => compacting
                    .await
                    .map_err(|error| Box::new(error) as BoxError),
                None => Err(Box::new(
                    ProviderAdapterV2Error::new(ProviderAdapterErrorKind::TurnStart {
                        driver: input.session.driver().clone(),
                        thread_id: input.run.thread_id.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                        run_id: input.run.id.clone(),
                    })
                    .with_cause(
                        ProviderFailureCause::tagged("Error")
                            .with_message("This provider does not support context compaction."),
                    ),
                )),
            },
            Ok(_) => input
                .session
                .start_turn(turn_input)
                .await
                .map_err(|error| Box::new(error) as BoxError),
        };
        let start_failure = match (should_start, started) {
            (Err(cause), _) => Some((cause, OrchestrationV2ProviderFailureClass::Unknown)),
            (Ok(_), Err(cause)) => {
                Some((cause, OrchestrationV2ProviderFailureClass::ProviderError))
            }
            (Ok(_), Ok(())) => None,
        };
        let Some((cause, class)) = start_failure else {
            return Ok(());
        };

        tracing::error!(
            run_id = %input.run.id,
            error = %cause,
            "orchestration V2 provider turn start failed"
        );
        interrupt_task(&cancel, task).await;
        let (provider_thread, latest_item_ordinal, open) = {
            let state = ctx.state.lock();
            (
                state.latest_provider_thread.clone(),
                state.latest_turn_item_ordinal,
                state.open_run_owned_subagents.clone(),
            )
        };
        let terminal = make_failed_terminal_event(
            &input,
            make_provider_failure(ProviderFailureInput {
                cause: Some(&failure_cause(&*cause)),
                message: None,
                code: None,
                // A failed ownership read is not the provider's fault.
                class: Some(class),
                retryable: None,
                reset_at: None,
            }),
            latest_item_ordinal + 1,
        );
        self.inner
            .write_final_run_events(FinalRunInput {
                run: &input.run,
                root_node: &input.root_node,
                checkpoint_scope: &input.checkpoint_scope,
                provider_thread: &provider_thread,
                attempt: &input.attempt,
                should_finalize_run: None,
                has_unpaired_run_interrupt_request: None,
                open_run_owned_subagents: Some(&open),
                terminal: &terminal,
                failure_item_persisted: false,
                refresh: &refresh,
                // Checked in the write transaction, not by another read that
                // can fail like the one before the start.
                write_if_run_current: Some(RunCurrentGate {
                    active_attempt_id: input.attempt.id.clone(),
                    expected_status: OrchestrationV2RunStatus::Running,
                }),
            })
            .await
            .map_err(|write| {
                start_error(Box::new(FailureAndWriteCause {
                    label: "start",
                    first: cause,
                    write,
                }))
            })?;
        Ok(())
    }

    /// The first phase of `startRootRun`: the response streaming mode, the
    /// checkpoint baseline, and the ownership check. `None` means the provider
    /// turn must not start.
    async fn prepare(
        &self,
        input: &StartRootRunInput,
    ) -> Result<Option<turin_contracts::settings::ResponseStreamingMode>, BoxError> {
        let settings = self.inner.settings.get_settings().await?;
        let mode = turin_shared::project_settings::resolve_project_settings(
            &settings,
            Some(&input.app_thread.project_id),
            None,
            &|_, _, _| None,
        )
        .settings
        .response_streaming_mode;
        if let Err(cause) = self
            .inner
            .baseline
            .capture_baseline(
                &input.checkpoint_scope,
                input.run.ordinal.get().saturating_sub(1),
            )
            .await
        {
            tracing::warn!(
                run_id = %input.run.id,
                error = %cause,
                "orchestration V2 checkpoint baseline capture failed; starting provider without a baseline"
            );
        }
        if let Some(check) = &input.should_start_provider_turn
            && !check().await?
        {
            return Ok(None);
        }
        Ok(Some(mode))
    }
}

/// `Fiber.interrupt`: stop the ingestion task and wait for it, which runs the
/// subscription's `close`.
async fn interrupt_task(cancel: &CancellationToken, task: tokio::task::JoinHandle<()>) {
    cancel.cancel();
    let _ = task.await;
}

/// The cause `makeProviderFailure` classifies for an error: the tagged chain
/// upstream walks through `.cause`. Adapter errors carry their own tag; this
/// service's errors are tagged by name so the chain reaches the adapter error
/// they wrap.
pub(crate) fn failure_cause(error: &(dyn std::error::Error + 'static)) -> ProviderFailureCause {
    if let Some(adapter_error) = error.downcast_ref::<ProviderAdapterV2Error>() {
        return adapter_error.as_cause();
    }
    let tag = if error.is::<RunExecutionIngestError>() {
        "RunExecutionIngestError"
    } else if error.is::<turin_provider_core::failure::ContextHandoffBudgetError>() {
        "ContextHandoffBudgetError"
    } else if error
        .is::<crate::core::context_handoff_delivery::ContextHandoffDeliveryUncertainError>()
    {
        "ContextHandoffDeliveryUncertainError"
    } else if error.is::<RunExecutionStartError>() {
        "RunExecutionStartError"
    } else {
        "Error"
    };
    let cause = ProviderFailureCause::tagged(tag).with_message(error.to_string());
    match error.source() {
        Some(source) => cause.with_cause(failure_cause(source)),
        None => cause,
    }
}

/// `makeFailedTerminalEvent`.
fn make_failed_terminal_event(
    input: &StartRootRunInput,
    failure: OrchestrationV2ProviderFailure,
    failure_item_ordinal: u64,
) -> TerminalSummary {
    let driver = input.provider_thread.driver.clone();
    TerminalSummary {
        provider_thread_id: input.provider_thread.id.clone(),
        provider_turn_id: input.attempt.provider_turn_id.clone().unwrap_or_else(|| {
            derive::provider_turn(&driver, &format!("failed:{}", input.attempt.id))
        }),
        driver,
        status: ProviderAdapterV2TurnTerminalStatus::Failed,
        failure: Some(failure),
        failure_item_ordinal: turin_contracts::PositiveInt::new(failure_item_ordinal),
        thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
    }
}

// ---------------------------------------------------------------------------
// The ingestion task
// ---------------------------------------------------------------------------

/// The refs `startRootRun` shares between its main flow and the event fiber.
struct RunState {
    terminal: Option<TerminalSummary>,
    latest_turn_item_ordinal: u64,
    latest_provider_thread: OrchestrationV2ProviderThread,
    routing: ProviderEventRoutingState,
    root_terminal_seen: bool,
    root_run_finalized: bool,
    provider_thread_owner_lost: bool,
    active_child_provider_turns: HashSet<ProviderTurnId>,
    active_child_subagents: HashSet<NodeId>,
    active_background_turn_items: HashSet<TurnItemId>,
    open_run_owned_subagents: OpenRunOwnedSubagentProjection,
}

struct RunContext {
    service: Arc<RunExecutionInner>,
    input: Arc<StartRootRunInput>,
    refresh: Arc<RefreshOnce>,
    identity: ProviderEventRouteIdentity,
    /// `inheritedBackgroundTurnItemsById`, as selected at start. Routing
    /// removes settled items from its own copy; this one stays.
    inherited_by_id: std::collections::HashMap<TurnItemId, RunId>,
    state: Mutex<RunState>,
}

type EventStream = turin_provider_core::provider_adapter::ProviderEventStream;

impl RunContext {
    fn ingest_error(&self, cause: BoxError) -> BoxError {
        Box::new(RunExecutionIngestError {
            run_id: self.input.run.id.to_string(),
            cause,
        })
    }

    /// The fiber body: ingest until done, then always close the subscription.
    async fn run_ingestion(
        self: Arc<Self>,
        subscription: ProviderAdapterV2EventSubscription,
        mode: turin_contracts::settings::ResponseStreamingMode,
        cancel: CancellationToken,
    ) {
        let ProviderAdapterV2EventSubscription { events, close } = subscription;
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            () = self.ingest(events, mode) => {}
        }
        close.await;
    }

    async fn ingest(
        &self,
        mut events: EventStream,
        mode: turin_contracts::settings::ResponseStreamingMode,
    ) {
        let mut filter = AssistantStreamingFilter::new(mode);
        let drained = self.drain(&mut events, &mut filter).await;
        let result = match drained {
            Ok(()) => {
                let terminal = self.state.lock().terminal.clone();
                match terminal {
                    Some(terminal) => self.finalize_root_run(&terminal).await,
                    None => Ok(()),
                }
            }
            Err(cause) => Err(cause),
        };
        let Err(cause) = result else {
            return;
        };
        let finalized = self.state.lock().root_run_finalized;
        tracing::warn!(
            run_id = %self.input.run.id,
            error = %cause,
            "orchestration V2 provider event ingestion failed"
        );
        if finalized {
            return;
        }
        let (provider_thread, latest_item_ordinal, open) = {
            let state = self.state.lock();
            (
                state.latest_provider_thread.clone(),
                state.latest_turn_item_ordinal,
                state.open_run_owned_subagents.clone(),
            )
        };
        let terminal = make_failed_terminal_event(
            &self.input,
            make_provider_failure(ProviderFailureInput {
                cause: Some(&failure_cause(&*cause)),
                message: None,
                code: None,
                class: Some(OrchestrationV2ProviderFailureClass::Unknown),
                retryable: None,
                reset_at: None,
            }),
            latest_item_ordinal + 1,
        );
        let written = self
            .service
            .write_final_run_events(FinalRunInput {
                run: &self.input.run,
                root_node: &self.input.root_node,
                checkpoint_scope: &self.input.checkpoint_scope,
                provider_thread: &provider_thread,
                attempt: &self.input.attempt,
                should_finalize_run: None,
                has_unpaired_run_interrupt_request: None,
                // The failure may be the ownership read itself, so check in the write.
                write_if_run_current: Some(RunCurrentGate {
                    active_attempt_id: self.input.attempt.id.clone(),
                    expected_status: OrchestrationV2RunStatus::Running,
                }),
                open_run_owned_subagents: Some(&open),
                terminal: &terminal,
                failure_item_persisted: false,
                refresh: &self.refresh,
            })
            .await;
        if let Err(write) = written {
            tracing::error!(
                run_id = %self.input.run.id,
                error = %RunExecutionIngestError {
                    run_id: self.input.run.id.to_string(),
                    cause: Box::new(FailureAndWriteCause {
                        label: "ingest",
                        first: cause,
                        write,
                    }),
                },
                "failed to record the failed run"
            );
        }
    }

    /// The event pipeline: route, deliver, track, then check whether to stop.
    async fn drain(
        &self,
        events: &mut EventStream,
        filter: &mut AssistantStreamingFilter,
    ) -> Result<(), BoxError> {
        while let Some(item) = events.next().await {
            let event = item.map_err(|error| self.ingest_error(Box::new(error)))?;
            let belongs = {
                let mut state = self.state.lock();
                route_provider_event(&event, &self.identity, &mut state.routing)
            };
            if !belongs {
                continue;
            }
            self.process_event(event, filter).await?;
            if self.should_stop_ingestion().await {
                break;
            }
        }
        Ok(())
    }

    async fn process_event(
        &self,
        event: ProviderAdapterV2Event,
        filter: &mut AssistantStreamingFilter,
    ) -> Result<(), BoxError> {
        let input = &self.input;
        let mut stored_event_count = 0;
        let delivered =
            filter_assistant_event(filter, &event, chrono::Utc::now().timestamp_millis());
        if let Some(delivered) = &delivered {
            // Root provider_thread.updated always uses an ownership gate:
            // pre-terminal writeIfRunCurrent (attempt still running), or
            // post-terminal writeIfProviderThreadOwner so late roster
            // clears still land while this attempt owns the run and this
            // run owns lastRunOrdinal.
            let root_terminal_already_seen = self.state.lock().root_terminal_seen;
            let is_root_provider_thread_update = matches!(
                &event,
                ProviderAdapterV2Event::ProviderThreadUpdated { provider_thread, .. }
                    if provider_thread.id == input.provider_thread.id
            );
            let guard = if !is_root_provider_thread_update {
                IngestGuard::default()
            } else if root_terminal_already_seen {
                IngestGuard {
                    provider_thread_owner: Some(ProviderThreadOwnerGuard {
                        provider_thread_id: input.provider_thread.id.clone(),
                        run_id: input.run.id.clone(),
                        active_attempt_id: input.attempt.id.clone(),
                        expected_last_run_ordinal: input.run.ordinal.get(),
                    }),
                    run_current: None,
                }
            } else {
                IngestGuard {
                    run_current: Some(RunCurrentGuard {
                        run_id: input.run.id.clone(),
                        active_attempt_id: input.attempt.id.clone(),
                        expected_status: OrchestrationV2RunStatus::Running,
                    }),
                    provider_thread_owner: None,
                }
            };
            stored_event_count = self
                .service
                .ingestor
                .ingest_normalized(
                    &ProviderEventInput {
                        provider_session_id: input.provider_session_id.clone(),
                        provider_instance_id: input.run.provider_instance_id.clone(),
                        command_id: None,
                        thread_id: input.run.thread_id.clone(),
                        run_id: Some(input.run.id.clone()),
                        node_id: Some(input.root_node.id.clone()),
                        raw_event_id: None,
                        event: delivered.clone(),
                        analytics_context: Some(ProviderTurnAnalyticsContext {
                            model_selection: input.model_selection.clone(),
                            runtime_mode: Some(input.runtime_policy.runtime_mode),
                            interaction_mode: Some(input.runtime_policy.interaction_mode),
                        }),
                    },
                    &guard,
                )
                .await
                .map_err(|cause| self.ingest_error(cause))?;
            if is_root_provider_thread_update
                && root_terminal_already_seen
                && stored_event_count == 0
            {
                // Ownership lost (or thread row missing). Stop pinning the
                // stream on this run's background probe.
                self.state.lock().provider_thread_owner_lost = true;
            }
        }
        match &event {
            ProviderAdapterV2Event::ProviderThreadUpdated {
                provider_thread, ..
            } if provider_thread.id == input.provider_thread.id && stored_event_count > 0 => {
                self.state.lock().latest_provider_thread = provider_thread.clone();
            }
            ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => {
                let common = turn_item::common(turn_item);
                let mut state = self.state.lock();
                if common.provider_turn_id == state.routing.root_provider_turn_id.as_ref() {
                    state.latest_turn_item_ordinal =
                        state.latest_turn_item_ordinal.max(common.ordinal);
                }
            }
            ProviderAdapterV2Event::TurnTerminal { .. } => {
                if let Some(terminal) = TerminalSummary::from_event(&event) {
                    {
                        let mut state = self.state.lock();
                        state.terminal = Some(terminal.clone());
                        state.root_terminal_seen = true;
                    }
                    self.finalize_root_run(&terminal).await?;
                }
            }
            _ => {}
        }
        self.track_child_lifecycle(&event, delivered.is_some());
        Ok(())
    }

    /// `finalizeRootRun`.
    async fn finalize_root_run(&self, terminal: &TerminalSummary) -> Result<(), BoxError> {
        let input = &self.input;
        let (provider_thread, open) = {
            let state = self.state.lock();
            if state.root_run_finalized {
                return Ok(());
            }
            (
                state.latest_provider_thread.clone(),
                state.open_run_owned_subagents.clone(),
            )
        };
        self.service
            .write_final_run_events(FinalRunInput {
                run: &input.run,
                root_node: &input.root_node,
                checkpoint_scope: &input.checkpoint_scope,
                provider_thread: &provider_thread,
                attempt: &input.attempt,
                should_finalize_run: input.should_finalize_run.as_ref(),
                has_unpaired_run_interrupt_request: input
                    .has_unpaired_run_interrupt_request
                    .as_ref(),
                open_run_owned_subagents: Some(&open),
                terminal,
                failure_item_persisted: terminal.status
                    == ProviderAdapterV2TurnTerminalStatus::Failed,
                refresh: &self.refresh,
                // Stop may commit between the ownership read and this
                // terminal write. Gate the events and checkpoint together.
                write_if_run_current: input.should_finalize_run.as_ref().map(|_| RunCurrentGate {
                    active_attempt_id: input.attempt.id.clone(),
                    expected_status: OrchestrationV2RunStatus::Running,
                }),
            })
            .await
            .map_err(|cause| self.ingest_error(cause))?;
        let mut state = self.state.lock();
        if run_owned_terminal_status(terminal.status).is_some() {
            state.open_run_owned_subagents = OpenRunOwnedSubagentProjection::default();
        }
        state.root_run_finalized = true;
        Ok(())
    }

    /// `trackChildLifecycle`.
    fn track_child_lifecycle(&self, event: &ProviderAdapterV2Event, deliverable: bool) {
        let input = &self.input;
        let mut guard = self.state.lock();
        let state = &mut *guard;
        let owns_child_thread = |state: &RunState, thread_id: &ThreadId| {
            thread_id != &input.run.thread_id && state.routing.owned_thread_ids.contains(thread_id)
        };
        match event {
            ProviderAdapterV2Event::ProviderTurnUpdated { provider_turn, .. } => {
                let is_root = provider_turn.run_attempt_id.as_ref() == Some(&input.attempt.id)
                    || Some(&provider_turn.id) == state.routing.root_provider_turn_id.as_ref();
                if !is_root {
                    if matches!(
                        provider_turn.status,
                        OrchestrationV2ProviderTurnStatus::Completed
                            | OrchestrationV2ProviderTurnStatus::Interrupted
                            | OrchestrationV2ProviderTurnStatus::Failed
                            | OrchestrationV2ProviderTurnStatus::Cancelled
                    ) {
                        state.active_child_provider_turns.remove(&provider_turn.id);
                    } else {
                        state
                            .active_child_provider_turns
                            .insert(provider_turn.id.clone());
                    }
                }
            }
            ProviderAdapterV2Event::SubagentUpdated { subagent, .. } => {
                let belongs_to_root_run = subagent.run_id.as_ref() == Some(&input.run.id);
                let belongs_to_owned_child_thread = owns_child_thread(state, &subagent.thread_id);
                if belongs_to_root_run || belongs_to_owned_child_thread {
                    if is_subagent_work_active(subagent.status) {
                        state.active_child_subagents.insert(subagent.id.clone());
                    } else {
                        state.active_child_subagents.remove(&subagent.id);
                    }
                }
                // Snapshot run-owned subagents for interrupt cascade.
                // Preserve childThreadId linkage for the root-run lifetime even
                // after the subagent row terminalizes, so open child-thread
                // nodes can still be proven linked on a later root interrupt.
                // A provider records nested native subagents on the child
                // thread, runless; they die with this run too.
                if belongs_to_root_run
                    || (belongs_to_owned_child_thread
                        && subagent.run_id.is_none()
                        && subagent.origin == OrchestrationV2SubagentOrigin::ProviderNative)
                {
                    let open = &mut state.open_run_owned_subagents;
                    with_linked_child_thread_id(open, subagent.child_thread_id.as_ref());
                    if is_subagent_work_active(subagent.status) {
                        open.subagents.insert(subagent.id.clone(), subagent.clone());
                    } else {
                        open.subagents.shift_remove(&subagent.id);
                    }
                }
            }
            ProviderAdapterV2Event::NodeUpdated { node, .. } => {
                let belongs_to_root_subagent = node.kind
                    == OrchestrationV2ExecutionNodeKind::Subagent
                    && node.run_id.as_ref() == Some(&input.run.id);
                let belongs_to_owned_child_thread = owns_child_thread(state, &node.thread_id);
                if !belongs_to_root_subagent
                    && !(belongs_to_owned_child_thread
                        && (node.run_id.is_none() || node.run_id.as_ref() == Some(&input.run.id)))
                {
                    return;
                }
                let open = &mut state.open_run_owned_subagents;
                if is_open_execution_node_status(node.status) {
                    open.nodes.insert(node.id.clone(), node.clone());
                } else {
                    open.nodes.shift_remove(&node.id);
                }
            }
            ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => {
                let common = turn_item::common(turn_item);
                let belongs_to_root_run = common.run_id == Some(&input.run.id);
                let belongs_to_owned_child_thread = owns_child_thread(state, common.thread_id);
                let belongs_to_inherited_background_item = common.thread_id == &input.run.thread_id
                    && common.run_id.is_some()
                    && self.inherited_by_id.get(common.id) == common.run_id;
                let active = is_turn_item_work_active(common.status);
                if is_background_capable(turn_item::type_tag(turn_item))
                    && (belongs_to_root_run
                        || belongs_to_owned_child_thread
                        || belongs_to_inherited_background_item)
                {
                    if active {
                        state.active_background_turn_items.insert(common.id.clone());
                    } else {
                        state.active_background_turn_items.remove(common.id);
                    }
                }
                if belongs_to_owned_child_thread
                    && (common.run_id.is_none() || belongs_to_root_run)
                    && deliverable
                {
                    let open = &mut state.open_run_owned_subagents;
                    if active {
                        open.child_turn_items
                            .insert(common.id.clone(), turn_item.clone());
                    } else {
                        open.child_turn_items.shift_remove(common.id);
                    }
                }
                if belongs_to_root_run
                    && let OrchestrationV2TurnItem::Subagent {
                        subagent_id,
                        child_thread_id,
                        ..
                    } = turn_item
                {
                    let open = &mut state.open_run_owned_subagents;
                    with_linked_child_thread_id(open, child_thread_id.as_ref());
                    if active {
                        open.turn_items
                            .insert(subagent_id.clone(), turn_item.clone());
                    } else {
                        open.turn_items.shift_remove(subagent_id);
                    }
                }
            }
            _ => {}
        }
    }

    /// `shouldStopProviderEventIngestion`.
    async fn should_stop_ingestion(&self) -> bool {
        let latest_provider_thread = {
            let state = self.state.lock();
            if !state.root_terminal_seen {
                return false;
            }
            // Non-completed terminals drop background tracking immediately.
            if let Some(terminal) = &state.terminal
                && terminal.status != ProviderAdapterV2TurnTerminalStatus::Completed
            {
                return true;
            }
            if !state.active_child_provider_turns.is_empty()
                || !state.active_child_subagents.is_empty()
            {
                return false;
            }
            // Keep ingesting past root settlement while background-capable
            // items owned by this run (or an owned child thread) are still
            // non-terminal, so their late completion events reach the
            // projection (stuck-spinner fix). Only for completed runs:
            // interrupted/failed turns intentionally drop background tracking
            // rather than pinning the stream open. Newly owned items depend on
            // adapters emitting a non-terminal event before the root terminal.
            // Exact inherited items are seeded from their selected durable rows.
            //
            // Owner loss (a newer run claimed lastRunOrdinal) must not close
            // this stream while these sets are non-empty: turn_item.updated
            // writes are not ownership-gated, so late completions still land.
            if !state.active_background_turn_items.is_empty() {
                return false;
            }
            // Owner loss means do not hold the stream open solely for the
            // roster probe; once background sets are empty, release.
            if state.provider_thread_owner_lost {
                return true;
            }
            state.latest_provider_thread.clone()
        };
        // Claude background Bash has no turn-item projection. Keep the
        // stream open while this root's provider thread still reports
        // pending roster work so late empty updates can clear Waiting.
        // Use only the thread-scoped probe: session-wide pending work
        // (siblings, wake buffers, session subagents) must not pin this
        // root subscription. Session idle release still uses
        // hasPendingBackgroundWork via ProviderSessionManager.
        if let Some(probe) = self
            .input
            .session
            .has_pending_background_work_for_thread(&latest_provider_thread)
            && probe.await.unwrap_or(false)
        {
            return false;
        }
        true
    }
}

/// `withLinkedChildThreadId`.
fn with_linked_child_thread_id(
    open: &mut OpenRunOwnedSubagentProjection,
    child_thread_id: Option<&ThreadId>,
) {
    if let Some(child_thread_id) = child_thread_id {
        open.linked_child_thread_ids.insert(child_thread_id.clone());
    }
}

/// `makeAssistantStreamingFilter` over a typed event. The filter works on the
/// camelCase wire shape, so the event goes through JSON and back; only the
/// kinds the filter can change take that trip.
fn filter_assistant_event(
    filter: &mut AssistantStreamingFilter,
    event: &ProviderAdapterV2Event,
    now_ms: i64,
) -> Option<ProviderAdapterV2Event> {
    let affected = match event {
        ProviderAdapterV2Event::NodeUpdated { node, .. } => {
            node.kind == OrchestrationV2ExecutionNodeKind::AssistantMessage
        }
        ProviderAdapterV2Event::MessageUpdated { .. } => true,
        ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => {
            matches!(
                turn_item::type_tag(turn_item),
                "assistant_message" | "reasoning"
            )
        }
        _ => false,
    };
    if !affected {
        return Some(event.clone());
    }
    let Ok(mut wire) = serde_json::to_value(event) else {
        return Some(event.clone());
    };
    rename_key(&mut wire, "turn_item", "turnItem");
    let mut filtered = filter.filter(wire, now_ms)?;
    rename_key(&mut filtered, "turnItem", "turn_item");
    match serde_json::from_value(filtered) {
        Ok(delivered) => Some(delivered),
        Err(error) => {
            tracing::warn!(error = %error, "assistant streaming filter output did not decode");
            Some(event.clone())
        }
    }
}

fn rename_key(value: &mut Value, from: &str, to: &str) {
    if let Value::Object(fields) = value
        && let Some(field) = fields.remove(from)
    {
        fields.insert(to.to_owned(), field);
    }
}
