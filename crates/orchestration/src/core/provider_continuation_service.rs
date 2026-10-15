//! Port of upstream/apps/server/src/orchestration-v2/ProviderContinuationService.ts.
//!
//! Consumes provider-core's `ProviderContinuationRequests`. An adapter offers a
//! continuation when a background turn needs a wake: a buffered CLI wake, a
//! `message_text` wake, or an app-owned delegated completion. This worker
//! dispatches one internal `message.dispatch` per offer so the wake is ingested
//! as a normal run. Offers are handled one at a time, as upstream's loop does.
//! A failed delegated completion is re-offered with exponential backoff while
//! its delivery is still open.

use std::{collections::HashMap, fmt, future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;
use turin_contracts::{
    CommandId, MessageId, NodeId, ProjectId, ThreadId,
    base_schemas::TrimmedNonEmptyString,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2Command, OrchestrationV2CreationSource,
        OrchestrationV2DelegatedCompletionCohortDisposition,
        OrchestrationV2DelegatedCompletionDelivery, OrchestrationV2DispatchMode,
        OrchestrationV2MessageDelegatedCompletion, OrchestrationV2Notification,
        OrchestrationV2NotificationOutcome, OrchestrationV2NotificationSource,
        OrchestrationV2ServerCommand, OrchestrationV2ThreadProjection,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    id_allocator::IdAllocator,
    provider_continuation_requests::{
        ProviderContinuationDelegatedCompletion, ProviderContinuationDelivery,
        ProviderContinuationRequest, ProviderContinuationRequests,
    },
    rt::BoxFuture,
};

use crate::pipeline::{
    notification_mailbox::is_undelivered_mailbox_steer,
    orchestrator_port::{BoxError, RecordField, RecordFilter},
    thread_management_service::{ThreadManagementCore, ThreadManagementError},
};

const CONTINUATION_MESSAGE_TEXT: &str = "Background task completed.";
const RETRY_BASE_MS: u64 = 100;
const RETRY_CAP_MS: u64 = 5_000;
/// Doubling stops here: 100 ms * 2^6 is already past the cap.
const RETRY_MAX_DOUBLINGS: u32 = 6;

/// Provider-facing text for delegated tasks that reached a terminal state.
pub fn delegated_completion_text(task_ids: &[NodeId]) -> String {
    let list = task_ids
        .iter()
        .map(NodeId::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if task_ids.len() == 1 {
        format!(
            "Delegated task {list} reached a terminal state. Use task_status with taskId {list} to read the result."
        )
    } else {
        format!(
            "Delegated tasks {list} reached terminal states. Use task_status with each taskId to read the results."
        )
    }
}

/// The thread reads and dispatches the consumer needs. `ThreadManagementCore`
/// implements it; tests supply a fake.
#[async_trait]
pub trait ContinuationThreads: Send + Sync {
    /// `getThreadRecords` for messages, runs and provider turns. `message_ids`
    /// filters messages; an empty list reads none, as upstream's `[]` does.
    async fn records(
        &self,
        thread_id: &ThreadId,
        message_ids: Vec<MessageId>,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError>;

    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<(), ThreadManagementError>;
}

#[async_trait]
impl ContinuationThreads for ThreadManagementCore {
    async fn records(
        &self,
        thread_id: &ThreadId,
        message_ids: Vec<MessageId>,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
        let filter = RecordFilter {
            message_ids: Some(message_ids),
            ..RecordFilter::default()
        };
        self.orchestrator()
            .get_thread_records(
                thread_id,
                &[
                    RecordField::Messages,
                    RecordField::Runs,
                    RecordField::ProviderTurns,
                ],
                &filter,
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<(), ThreadManagementError> {
        ThreadManagementCore::dispatch(self, command)
            .await
            .map(|_| ())
    }
}

/// Why an offer did not reach the thread.
#[derive(Debug)]
enum Failure {
    /// The provider instance was turned off after the offer was made. Not retried.
    InstanceDisabled {
        project_id: ProjectId,
        instance_id: ProviderInstanceId,
    },
    Other(String),
}

impl Failure {
    fn from_thread_error(error: ThreadManagementError) -> Self {
        match error {
            ThreadManagementError::ProviderInstanceDisabled {
                project_id,
                instance_id,
            } => Self::InstanceDisabled {
                project_id,
                instance_id,
            },
            other => Self::Other(other.to_string()),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstanceDisabled {
                project_id,
                instance_id,
            } => write!(
                f,
                "provider instance {} is disabled for project {}",
                instance_id.as_str(),
                project_id.as_str()
            ),
            Self::Other(text) => f.write_str(text),
        }
    }
}

/// Runs `dispatch` through the offer's generation guard when it has one. A
/// stale offer runs nothing and counts as delivered: a newer generation owns it.
async fn run_if_current(
    request: &ProviderContinuationRequest,
    dispatch: impl Future<Output = Result<(), Failure>> + Send + 'static,
) -> Result<(), Failure> {
    let Some(dispatch_if_current) = request.dispatch_if_current.as_ref() else {
        return dispatch.await;
    };
    // The guard only sees a String, so the typed failure travels out of band.
    let failure = Arc::new(Mutex::new(None::<Failure>));
    let captured = failure.clone();
    let guarded: BoxFuture<'static, Result<(), String>> = Box::pin(async move {
        dispatch.await.map_err(|error| {
            let text = error.to_string();
            *captured.lock() = Some(error);
            text
        })
    });
    match dispatch_if_current(guarded).await {
        Ok(_) => Ok(()),
        Err(text) => Err(failure.lock().take().unwrap_or(Failure::Other(text))),
    }
}

/// Upstream's `clearIfCurrent ?? dispatchIfCurrent(Effect.void)`: releases the
/// adapter's sticky offer without dispatching anything.
async fn release_offer(request: &ProviderContinuationRequest) -> Result<(), Failure> {
    if let Some(clear) = request.clear_if_current.as_ref() {
        clear().await;
        return Ok(());
    }
    if request.dispatch_if_current.is_some() {
        return run_if_current(request, async { Ok(()) }).await;
    }
    Ok(())
}

fn retry_key(thread_id: &ThreadId, completion: &ProviderContinuationDelegatedCompletion) -> String {
    format!(
        "{}:{}:{}:{}",
        thread_id.as_str(),
        completion.parent_run_id.as_str(),
        completion.generation,
        completion.message_id.as_str()
    )
}

/// The delivery a delegated completion may still dispatch: its cohort is open,
/// the generation and message match, and the message is not already in the
/// thread unless it is an undelivered mailbox steer.
fn current_delegated_completion_delivery<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    completion: &ProviderContinuationDelegatedCompletion,
) -> Option<&'a OrchestrationV2DelegatedCompletionDelivery> {
    let source_run = projection
        .runs
        .iter()
        .find(|run| run.id == completion.parent_run_id)?;
    let cohort = source_run.delegated_completion.as_ref()?;
    if !matches!(
        cohort.disposition,
        OrchestrationV2DelegatedCompletionCohortDisposition::Open
    ) {
        return None;
    }
    let delivery = cohort.delivery.as_ref()?;
    let already_dispatched = projection
        .messages
        .iter()
        .any(|message| message.id == completion.message_id);
    if i64::try_from(delivery.generation.get()).ok() != Some(completion.generation)
        || delivery.message_id != completion.message_id
        || (already_dispatched && !is_undelivered_mailbox_steer(projection, &completion.message_id))
    {
        return None;
    }
    Some(delivery)
}

/// Upstream's `nextRetryDelay`: 100 ms doubling per failed attempt, capped at 5 s.
fn retry_delay(attempt: u32) -> Duration {
    let doublings = attempt.min(RETRY_MAX_DOUBLINGS);
    Duration::from_millis((RETRY_BASE_MS * 2u64.pow(doublings)).min(RETRY_CAP_MS))
}

fn default_notification() -> OrchestrationV2Notification {
    OrchestrationV2Notification {
        source: OrchestrationV2NotificationSource::BackgroundTask,
        outcome: OrchestrationV2NotificationOutcome::Updated,
        summary: TrimmedNonEmptyString::new("Background activity updated")
            .expect("default summary is non-empty"),
        detail: None,
    }
}

fn message_dispatch(
    command_id: CommandId,
    thread_id: &ThreadId,
    message_id: MessageId,
    text: String,
    creation_source: OrchestrationV2CreationSource,
    notification: Option<OrchestrationV2Notification>,
    delegated_completion: Option<OrchestrationV2MessageDelegatedCompletion>,
) -> OrchestrationV2ServerCommand {
    OrchestrationV2ServerCommand::Client(OrchestrationV2Command::MessageDispatch {
        notification,
        created_by: OrchestrationV2Actor::Agent,
        creation_source,
        scheduled_task_id: None,
        sender_thread_id: None,
        command_id,
        thread_id: thread_id.clone(),
        message_id,
        text,
        context: None,
        attachments: Vec::new(),
        title_seed: None,
        model_selection: None,
        source_plan_ref: None,
        restart_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_recovery_request_id: None,
        delivery_intent: None,
        delegated_completion,
        dispatch_mode: OrchestrationV2DispatchMode::QueueAfterActive,
    })
}

struct Worker {
    ids: IdAllocator,
    requests: Arc<ProviderContinuationRequests>,
    threads: Arc<dyn ContinuationThreads>,
    /// Failed delegated completions by `retry_key`. Cleared on success, on a
    /// closed delivery, and on archive or delete.
    retry_attempts: Mutex<HashMap<String, u32>>,
}

impl Worker {
    /// Drains offers until the owning service is dropped.
    async fn run(self: Arc<Self>) {
        // Retries are owned here, so dropping the loop aborts them too.
        let mut retries = JoinSet::new();
        loop {
            while retries.try_join_next().is_some() {}
            let request = self.requests.take().await;
            let key = request
                .delegated_completion
                .as_ref()
                .map(|completion| retry_key(&request.thread_id, completion));
            let Err(failure) = self.dispatch_continuation(&request).await else {
                continue;
            };
            tracing::warn!(
                thread_id = %request.thread_id.as_str(),
                provider_thread_id = %request.provider_thread_id.as_str(),
                error = %failure,
                "orchestration-v2.provider-continuation.dispatch-failed"
            );
            if let Some(key) = key {
                let delay = self.next_retry_delay(&key);
                let worker = self.clone();
                retries.spawn(async move { worker.retry(request, key, delay).await });
            }
        }
    }

    fn next_retry_delay(&self, key: &str) -> Duration {
        let mut attempts = self.retry_attempts.lock();
        let attempt = attempts.get(key).copied().unwrap_or(0);
        attempts.insert(key.to_owned(), attempt + 1);
        retry_delay(attempt)
    }

    fn clear_retry_attempt(&self, key: &str) {
        self.retry_attempts.lock().remove(key);
    }

    /// Re-offers a failed delegated completion while its delivery is still open.
    async fn retry(
        self: Arc<Self>,
        request: ProviderContinuationRequest,
        key: String,
        delay: Duration,
    ) {
        tokio::time::sleep(delay).await;
        let Some(completion) = request.delegated_completion.as_ref() else {
            return;
        };
        match self
            .records(&request.thread_id, vec![completion.message_id.clone()])
            .await
        {
            Ok(projection) => {
                if current_delegated_completion_delivery(&projection, completion).is_some() {
                    self.requests.offer(request);
                } else {
                    self.clear_retry_attempt(&key);
                }
            }
            Err(error) => {
                tracing::warn!(
                    thread_id = %request.thread_id.as_str(),
                    error = %error,
                    "orchestration-v2.provider-continuation.retry-check-failed"
                );
                self.requests.offer(request);
            }
        }
    }

    async fn records(
        &self,
        thread_id: &ThreadId,
        message_ids: Vec<MessageId>,
    ) -> Result<OrchestrationV2ThreadProjection, Failure> {
        self.threads
            .records(thread_id, message_ids)
            .await
            .map_err(|error| Failure::Other(error.to_string()))
    }

    async fn dispatch_continuation(
        &self,
        request: &ProviderContinuationRequest,
    ) -> Result<(), Failure> {
        let completion = request.delegated_completion.as_ref();
        let message_ids = completion
            .map(|completion| vec![completion.message_id.clone()])
            .unwrap_or_default();
        let projection = self.records(&request.thread_id, message_ids).await?;
        if projection.thread.archived_at.is_some() || projection.thread.deleted_at.is_some() {
            tracing::info!(
                thread_id = %request.thread_id.as_str(),
                provider_thread_id = %request.provider_thread_id.as_str(),
                "orchestration-v2.provider-continuation.thread-archived"
            );
            if let Some(completion) = completion {
                self.clear_retry_attempt(&retry_key(&request.thread_id, completion));
            }
            // No continuation turn will start to clear the adapter's sticky offer.
            return release_offer(request).await;
        }
        match completion {
            Some(completion) => {
                self.dispatch_delegated(request, &projection, completion)
                    .await
            }
            None => self.dispatch_wake(request, &projection).await,
        }
    }

    async fn dispatch_delegated(
        &self,
        request: &ProviderContinuationRequest,
        projection: &OrchestrationV2ThreadProjection,
        completion: &ProviderContinuationDelegatedCompletion,
    ) -> Result<(), Failure> {
        let key = retry_key(&request.thread_id, completion);
        let Some(delivery) = current_delegated_completion_delivery(projection, completion).cloned()
        else {
            self.clear_retry_attempt(&key);
            return Ok(());
        };
        let command_id = self.ids.command("delegated-completion", "dispatch");
        let command = message_dispatch(
            command_id,
            &request.thread_id,
            delivery.message_id.clone(),
            delegated_completion_text(&delivery.task_ids),
            OrchestrationV2CreationSource::Server,
            None,
            Some(OrchestrationV2MessageDelegatedCompletion {
                parent_run_id: completion.parent_run_id.clone(),
                generation: delivery.generation,
                task_ids: delivery.task_ids.clone(),
            }),
        );
        // Delegated completions are durable mailbox offers and are not guarded
        // by `dispatch_if_current`: the delivery check above is their guard.
        match self.threads.dispatch(command).await {
            Ok(()) => {}
            Err(error) => match Failure::from_thread_error(error) {
                Failure::InstanceDisabled {
                    project_id,
                    instance_id,
                } => {
                    tracing::info!(
                        thread_id = %request.thread_id.as_str(),
                        project_id = %project_id.as_str(),
                        instance_id = %instance_id.as_str(),
                        "orchestration-v2.provider-continuation.delegated-completion-disabled"
                    );
                    // The finalize that claimed this delivery already wrote the
                    // results to the parent's history, so acknowledge each task
                    // administratively to consume the delivery rather than retry it.
                    for task_id in &delivery.task_ids {
                        let command_id = self
                            .ids
                            .command("delegated-completion", "acknowledge-disabled");
                        self.threads
                            .dispatch(OrchestrationV2ServerCommand::Client(
                                OrchestrationV2Command::DelegatedTaskCompletionDeliveryAcknowledge {
                                    command_id,
                                    parent_thread_id: request.thread_id.clone(),
                                    task_id: task_id.clone(),
                                    observed_by_run_id: None,
                                },
                            ))
                            .await
                            .map_err(Failure::from_thread_error)?;
                    }
                    release_offer(request).await?;
                }
                other => return Err(other),
            },
        }
        self.clear_retry_attempt(&key);
        Ok(())
    }

    async fn dispatch_wake(
        &self,
        request: &ProviderContinuationRequest,
        projection: &OrchestrationV2ThreadProjection,
    ) -> Result<(), Failure> {
        // The ordinal is display metadata only: the id allocator appends a
        // random suffix, so a stale projection read cannot collide ids.
        let message_id = match request.message_id.clone() {
            Some(message_id) => message_id,
            None => self
                .ids
                .message(&request.thread_id, projection.messages.len() as u64 + 1),
        };
        let command_id = CommandId::new(format!("provider-continuation:{}", message_id.as_str()))
            .expect("message id is non-empty");
        // A `message_text` wake has no buffered CLI output, so it must not carry
        // the "provider" marker: ClaudeAdapterV2 would drop the text and settle the
        // turn having prompted nothing.
        let creation_source = match request.delivery {
            Some(ProviderContinuationDelivery::MessageText) => {
                OrchestrationV2CreationSource::Server
            }
            _ => OrchestrationV2CreationSource::Provider,
        };
        let command = message_dispatch(
            command_id,
            &request.thread_id,
            message_id,
            request
                .detail
                .clone()
                .unwrap_or_else(|| CONTINUATION_MESSAGE_TEXT.to_owned()),
            creation_source,
            Some(
                request
                    .notification
                    .clone()
                    .unwrap_or_else(default_notification),
            ),
            None,
        );
        let threads = self.threads.clone();
        let outcome = run_if_current(request, async move {
            threads
                .dispatch(command)
                .await
                .map_err(Failure::from_thread_error)
        })
        .await;
        match outcome {
            Err(Failure::InstanceDisabled {
                project_id,
                instance_id,
            }) => {
                // The adapter's buffered wake has nowhere to go once the instance
                // is off. Drop it: nothing here retries a non-delegated wake.
                tracing::info!(
                    thread_id = %request.thread_id.as_str(),
                    provider_thread_id = %request.provider_thread_id.as_str(),
                    project_id = %project_id.as_str(),
                    instance_id = %instance_id.as_str(),
                    "orchestration-v2.provider-continuation.instance-disabled"
                );
                if let Some(clear) = request.clear_if_current.as_ref() {
                    clear().await;
                }
                Ok(())
            }
            other => other,
        }
    }
}

/// Owns the drain loop. Dropping it stops the loop and any pending retries.
pub struct ProviderContinuationService {
    _loop: AbortOnDropHandle<()>,
}

impl ProviderContinuationService {
    /// Starts draining `requests` on the tokio runtime.
    pub fn start(
        ids: IdAllocator,
        requests: Arc<ProviderContinuationRequests>,
        threads: Arc<dyn ContinuationThreads>,
    ) -> Self {
        let worker = Arc::new(Worker {
            ids,
            requests,
            threads,
            retry_attempts: Mutex::new(HashMap::new()),
        });
        Self {
            _loop: AbortOnDropHandle::new(tokio::spawn(worker.run())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use parking_lot::Mutex;
    use serde_json::json;
    use tokio::sync::mpsc;
    use turin_contracts::{
        MessageId, RunId, ThreadId,
        orchestration_v2::{OrchestrationV2ServerCommand, OrchestrationV2ThreadProjection},
        provider_instance::{ProviderDriverKind, ProviderInstanceId},
    };
    use turin_provider_core::{
        id_allocator::IdAllocator,
        provider_continuation_requests::{
            DispatchIfCurrent, ProviderContinuationDelegatedCompletion,
            ProviderContinuationDelivery, ProviderContinuationRequest,
            ProviderContinuationRequests,
        },
        rt::BoxFuture,
    };

    use super::*;
    use crate::projection::test_support;

    const THREAD: &str = "thread-provider-continuation";
    const PARENT_RUN: &str = "run-provider-continuation-parent";
    const DELEGATED_MESSAGE: &str = "message-provider-continuation-delegated";
    const TASKS: [&str; 2] = [
        "node-provider-continuation-first",
        "node-provider-continuation-second",
    ];

    /// A thread projection. `delegated` adds a parent run whose delegated cohort
    /// is open with generation 1 and the two test tasks.
    fn projection(
        disposition: &str,
        archived: bool,
        delegated: bool,
    ) -> OrchestrationV2ThreadProjection {
        let mut thread = test_support::thread(THREAD);
        if archived {
            thread["archivedAt"] = json!("2026-07-20T00:00:00.000Z");
        }
        let runs = if delegated {
            let mut run = test_support::run(THREAD, PARENT_RUN, 1, "completed");
            run["delegatedCompletion"] = json!({
                "disposition": disposition,
                "nextGeneration": 2,
                "delivery": {
                    "generation": 1,
                    "messageId": DELEGATED_MESSAGE,
                    "taskIds": TASKS,
                },
            });
            vec![run]
        } else {
            Vec::new()
        };
        serde_json::from_value(json!({
            "thread": thread,
            "runs": runs,
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": [],
            "providerThreads": [],
            "providerTurns": [],
            "runtimeRequests": [],
            "messages": [],
            "plans": [],
            "turnItems": [],
            "checkpointScopes": [],
            "checkpoints": [],
            "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": [],
            "updatedAt": "2026-07-20T00:00:00.000Z",
        }))
        .expect("projection fixture")
    }

    struct FakeThreads {
        projection: Mutex<OrchestrationV2ThreadProjection>,
        /// Dispatches that fail before the next one succeeds.
        failures: AtomicUsize,
        /// Every dispatch fails as an instance-disabled error.
        disabled: AtomicBool,
        dispatched: mpsc::UnboundedSender<OrchestrationV2ServerCommand>,
    }

    #[async_trait]
    impl ContinuationThreads for FakeThreads {
        async fn records(
            &self,
            _thread_id: &ThreadId,
            _message_ids: Vec<MessageId>,
        ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
            Ok(self.projection.lock().clone())
        }

        async fn dispatch(
            &self,
            command: OrchestrationV2ServerCommand,
        ) -> Result<(), ThreadManagementError> {
            // The enablement gate covers message dispatch only; the administrative
            // acknowledge still goes through, as in upstream.
            if matches!(
                message(&command),
                OrchestrationV2Command::MessageDispatch { .. }
            ) {
                if self.disabled.load(Ordering::SeqCst) {
                    return Err(ThreadManagementError::ProviderInstanceDisabled {
                        project_id: ProjectId::new("project-disabled").expect("project id"),
                        instance_id: ProviderInstanceId::new("codex").expect("instance id"),
                    });
                }
                // Tests drive dispatch from one task, so load-then-sub is enough.
                if self.failures.load(Ordering::SeqCst) > 0 {
                    self.failures.fetch_sub(1, Ordering::SeqCst);
                    return Err(transient());
                }
            }
            let _ = self.dispatched.send(command);
            Ok(())
        }
    }

    fn transient() -> ThreadManagementError {
        ThreadManagementError::ProjectionLoad {
            project_id: turin_contracts::ProjectId::new("project-transient").expect("project id"),
            thread_id: ThreadId::new(THREAD).expect("thread id"),
            cause: "simulated transient dispatch failure".into(),
        }
    }

    struct Harness {
        requests: Arc<ProviderContinuationRequests>,
        threads: Arc<FakeThreads>,
        dispatched: mpsc::UnboundedReceiver<OrchestrationV2ServerCommand>,
        _service: ProviderContinuationService,
    }

    fn harness(projection: OrchestrationV2ThreadProjection, failures: usize) -> Harness {
        let (dispatched_tx, dispatched) = mpsc::unbounded_channel();
        let threads = Arc::new(FakeThreads {
            projection: Mutex::new(projection),
            failures: AtomicUsize::new(failures),
            disabled: AtomicBool::new(false),
            dispatched: dispatched_tx,
        });
        let requests = Arc::new(ProviderContinuationRequests::queue());
        let service =
            ProviderContinuationService::start(IdAllocator, requests.clone(), threads.clone());
        Harness {
            requests,
            threads,
            dispatched,
            _service: service,
        }
    }

    fn wake() -> ProviderContinuationRequest {
        ProviderContinuationRequest {
            thread_id: ThreadId::new(THREAD).expect("thread id"),
            provider_thread_id: turin_contracts::ProviderThreadId::new(
                "provider-thread-continuation",
            )
            .expect("provider thread id"),
            driver: ProviderDriverKind::new("continuation-test").expect("driver"),
            detail: None,
            notification: None,
            delegated_completion: None,
            delivery: None,
            message_id: None,
            dispatch_if_current: None,
            clear_if_current: None,
        }
    }

    fn delegated() -> ProviderContinuationRequest {
        ProviderContinuationRequest {
            delivery: Some(ProviderContinuationDelivery::MessageText),
            delegated_completion: Some(ProviderContinuationDelegatedCompletion {
                parent_run_id: RunId::new(PARENT_RUN).expect("run id"),
                generation: 1,
                message_id: MessageId::new(DELEGATED_MESSAGE).expect("message id"),
            }),
            ..wake()
        }
    }

    fn message(command: &OrchestrationV2ServerCommand) -> &OrchestrationV2Command {
        match command {
            OrchestrationV2ServerCommand::Client(command) => command,
            OrchestrationV2ServerCommand::Internal(_) => panic!("expected a client command"),
        }
    }

    /// Lets the drain loop and any spawned retry run until they block.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    async fn next(harness: &mut Harness) -> OrchestrationV2ServerCommand {
        tokio::time::timeout(Duration::from_secs(60), harness.dispatched.recv())
            .await
            .expect("a dispatch")
            .expect("dispatch channel open")
    }

    fn guard(current: Arc<AtomicBool>) -> DispatchIfCurrent {
        Box::new(move |dispatch| {
            let current = current.clone();
            Box::pin(async move {
                if !current.load(Ordering::SeqCst) {
                    return Ok(None);
                }
                dispatch.await.map(|()| Some(()))
            })
        })
    }

    fn flag_setter(flag: Arc<AtomicBool>) -> Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync> {
        Box::new(move || {
            let flag = flag.clone();
            Box::pin(async move { flag.store(true, Ordering::SeqCst) })
        })
    }

    #[tokio::test(start_paused = true)]
    async fn adapter_buffered_wake_is_marked_provider_with_the_default_notification() {
        let mut harness = harness(projection("open", false, false), 0);
        harness.requests.offer(wake());
        let command = next(&mut harness).await;
        let OrchestrationV2Command::MessageDispatch {
            created_by,
            creation_source,
            notification,
            dispatch_mode,
            text,
            ..
        } = message(&command)
        else {
            panic!("expected message.dispatch");
        };
        // ClaudeAdapterV2 keys on this to attach buffered CLI output.
        assert!(matches!(created_by, OrchestrationV2Actor::Agent));
        assert!(matches!(
            creation_source,
            OrchestrationV2CreationSource::Provider
        ));
        assert!(matches!(
            dispatch_mode,
            OrchestrationV2DispatchMode::QueueAfterActive
        ));
        assert_eq!(text, CONTINUATION_MESSAGE_TEXT);
        let notification = notification.as_ref().expect("default notification");
        assert!(matches!(
            notification.source,
            OrchestrationV2NotificationSource::BackgroundTask
        ));
        assert!(matches!(
            notification.outcome,
            OrchestrationV2NotificationOutcome::Updated
        ));
        assert_eq!(notification.summary.as_str(), "Background activity updated");
    }

    #[tokio::test(start_paused = true)]
    async fn message_text_wake_is_server_created_and_keeps_its_own_notification() {
        let mut harness = harness(projection("open", false, false), 0);
        harness.requests.offer(ProviderContinuationRequest {
            detail: Some("Delegated task completed.".to_owned()),
            notification: Some(OrchestrationV2Notification {
                source: OrchestrationV2NotificationSource::Monitor,
                outcome: OrchestrationV2NotificationOutcome::Updated,
                summary: TrimmedNonEmptyString::new("Monitor updated").expect("summary"),
                detail: None,
            }),
            delivery: Some(ProviderContinuationDelivery::MessageText),
            ..wake()
        });
        let command = next(&mut harness).await;
        let OrchestrationV2Command::MessageDispatch {
            creation_source,
            text,
            notification,
            ..
        } = message(&command)
        else {
            panic!("expected message.dispatch");
        };
        // An app-owned wake buffers nothing, so the text is the whole prompt.
        // Marking it "provider" would make ClaudeAdapterV2 drop it.
        assert!(matches!(
            creation_source,
            OrchestrationV2CreationSource::Server
        ));
        assert_eq!(text, "Delegated task completed.");
        let notification = notification.as_ref().expect("notification");
        assert!(matches!(
            notification.source,
            OrchestrationV2NotificationSource::Monitor
        ));
        assert_eq!(notification.summary.as_str(), "Monitor updated");
    }

    #[tokio::test(start_paused = true)]
    async fn wake_dispatches_under_the_message_id_its_adapter_named() {
        let mut harness = harness(projection("open", false, false), 0);
        let named =
            MessageId::new("message-provider-continuation-adapter-named").expect("message id");
        harness.requests.offer(ProviderContinuationRequest {
            message_id: Some(named.clone()),
            ..wake()
        });
        let command = next(&mut harness).await;
        let OrchestrationV2Command::MessageDispatch { message_id, .. } = message(&command) else {
            panic!("expected message.dispatch");
        };
        assert_eq!(message_id, &named);
    }

    #[tokio::test(start_paused = true)]
    async fn delegated_completion_dispatches_one_queued_server_owned_message() {
        let mut harness = harness(projection("open", false, true), 0);
        harness.requests.offer(delegated());
        let command = next(&mut harness).await;
        let OrchestrationV2Command::MessageDispatch {
            created_by,
            creation_source,
            dispatch_mode,
            message_id,
            text,
            delegated_completion,
            ..
        } = message(&command)
        else {
            panic!("expected message.dispatch");
        };
        assert!(matches!(created_by, OrchestrationV2Actor::Agent));
        assert!(matches!(
            creation_source,
            OrchestrationV2CreationSource::Server
        ));
        assert!(matches!(
            dispatch_mode,
            OrchestrationV2DispatchMode::QueueAfterActive
        ));
        assert_eq!(message_id.as_str(), DELEGATED_MESSAGE);
        assert!(text.contains("task_status"));
        let ownership = delegated_completion.as_ref().expect("delegated ownership");
        assert_eq!(ownership.parent_run_id.as_str(), PARENT_RUN);
        assert_eq!(ownership.generation.get(), 1);
        let task_ids: Vec<&str> = ownership.task_ids.iter().map(NodeId::as_str).collect();
        assert_eq!(task_ids, TASKS);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_a_delegated_completion_after_a_transient_dispatch_failure() {
        let mut harness = harness(projection("open", false, true), 1);
        harness.requests.offer(delegated());
        let command = next(&mut harness).await;
        let OrchestrationV2Command::MessageDispatch { message_id, .. } = message(&command) else {
            panic!("expected message.dispatch");
        };
        assert_eq!(message_id.as_str(), DELEGATED_MESSAGE);
    }

    #[tokio::test(start_paused = true)]
    async fn backs_off_repeated_delegated_completion_failures() {
        // Attempts at t=0, 100 ms, 300 ms and 700 ms: the delays double from 100 ms.
        let mut harness = harness(projection("open", false, true), 3);
        harness.requests.offer(delegated());
        settle().await;
        assert_eq!(harness.threads.failures.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_millis(99)).await;
        settle().await;
        assert_eq!(harness.threads.failures.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(harness.threads.failures.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(200)).await;
        settle().await;
        assert_eq!(harness.threads.failures.load(Ordering::SeqCst), 0);
        assert!(harness.dispatched.try_recv().is_err());

        tokio::time::advance(Duration::from_millis(400)).await;
        settle().await;
        assert!(harness.dispatched.try_recv().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn drops_a_delegated_retry_once_its_delivery_closes() {
        let mut harness = harness(projection("open", false, true), 1);
        harness.requests.offer(delegated());
        settle().await;
        // The cohort is stopped before the retry fires.
        *harness.threads.projection.lock() = projection("stopped", false, true);
        tokio::time::advance(Duration::from_millis(100)).await;
        settle().await;
        assert!(harness.dispatched.try_recv().is_err());
        assert_eq!(harness.threads.failures.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn dispatches_a_current_request_exactly_once() {
        let mut harness = harness(projection("open", false, false), 0);
        harness.requests.offer(ProviderContinuationRequest {
            dispatch_if_current: Some(guard(Arc::new(AtomicBool::new(true)))),
            ..wake()
        });
        next(&mut harness).await;
        settle().await;
        assert!(harness.dispatched.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn drops_a_request_invalidated_before_dispatch() {
        let mut harness = harness(projection("open", false, false), 0);
        harness.requests.offer(ProviderContinuationRequest {
            dispatch_if_current: Some(guard(Arc::new(AtomicBool::new(false)))),
            ..wake()
        });
        settle().await;
        assert!(harness.dispatched.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn clears_a_sticky_offer_when_its_thread_is_archived() {
        let mut harness = harness(projection("open", true, false), 0);
        let cleared = Arc::new(AtomicBool::new(false));
        harness.requests.offer(ProviderContinuationRequest {
            clear_if_current: Some(flag_setter(cleared.clone())),
            ..wake()
        });
        settle().await;
        assert!(cleared.load(Ordering::SeqCst));
        assert!(harness.dispatched.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_disabled_instance_drops_a_wake_and_clears_its_offer() {
        let mut harness = harness(projection("open", false, false), 0);
        harness.threads.disabled.store(true, Ordering::SeqCst);
        let cleared = Arc::new(AtomicBool::new(false));
        harness.requests.offer(ProviderContinuationRequest {
            clear_if_current: Some(flag_setter(cleared.clone())),
            ..wake()
        });
        settle().await;
        assert!(cleared.load(Ordering::SeqCst));
        assert!(harness.dispatched.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_disabled_instance_acknowledges_each_delegated_task_instead_of_retrying() {
        let mut harness = harness(projection("open", false, true), 0);
        harness.threads.disabled.store(true, Ordering::SeqCst);
        harness.requests.offer(delegated());
        for expected in TASKS {
            let command = next(&mut harness).await;
            let OrchestrationV2Command::DelegatedTaskCompletionDeliveryAcknowledge {
                task_id,
                observed_by_run_id,
                ..
            } = message(&command)
            else {
                panic!("expected an acknowledge");
            };
            assert_eq!(task_id.as_str(), expected);
            assert!(observed_by_run_id.is_none());
        }
        // Acknowledged, so the delivery is consumed and never re-offered.
        settle().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        settle().await;
        assert!(harness.dispatched.try_recv().is_err());
    }
}
