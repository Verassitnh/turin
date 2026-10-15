//! Port of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! at the provider-core `ProviderAdapterV2` boundary.
use crate::{
    adapter::{CodexAdapter, CodexEvent, CodexSession},
    approvals::{Decision, PendingRequest},
    params::RuntimePolicy,
    runtime::CodexRuntimeConfig,
};
use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::Arc,
};
use turin_codex_app_server::{client::Client, errors::AppServerError};
use turin_contracts::{
    PositiveInt, ProviderSessionId, RuntimeRequestId, ThreadId, TrimmedNonEmptyString, UtcDateTime,
    model::ProviderOptionSelectionValue,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2NativeRefStrength, OrchestrationV2ProviderCapabilities,
        OrchestrationV2ProviderRef, OrchestrationV2ProviderSession,
        OrchestrationV2ProviderSessionStatus, OrchestrationV2ProviderThread,
        OrchestrationV2ProviderThreadDisposition, OrchestrationV2ProviderThreadForkedFrom,
        OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::ProviderApprovalDecision,
};
use turin_provider_core::{
    failure::ProviderFailureCause,
    id_allocator::{IdAllocator, derive},
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput,
        ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderAdapterV2ForkThreadInput,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
        ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
        ProviderAdapterV2RollbackTarget, ProviderAdapterV2RollbackThreadInput,
        ProviderAdapterV2RuntimeRequestResponseInput, ProviderAdapterV2SteerInput,
        ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput,
        ProviderAdapterV2TurnTerminalStatus, ProviderEventStream, ProviderSessionRuntime,
    },
    selection_transition::{
        ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan,
        turn_scoped_selection_transition,
    },
};

fn driver() -> ProviderDriverKind {
    ProviderDriverKind::new("codex").expect("driver kind")
}
fn nonempty(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("nonempty provider value")
}
fn native_ref(id: &str) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: driver(),
        native_id: Some(nonempty(id)),
        strength: OrchestrationV2NativeRefStrength::Strong,
        fingerprint: None,
        ordinal: None,
    }
}
#[allow(clippy::result_large_err)]
fn native_id(thread: &OrchestrationV2ProviderThread) -> Result<&str, ProviderAdapterV2Error> {
    thread
        .native_thread_ref
        .as_ref()
        .and_then(|r| r.native_id.as_ref())
        .map(TrimmedNonEmptyString::as_str)
        .ok_or_else(|| {
            ProviderAdapterV2Error::new(ProviderAdapterErrorKind::Protocol {
                driver: driver(),
                detail: "Provider thread is missing a native Codex thread id".into(),
                payload: None,
            })
        })
}
fn time(seconds: Option<&Value>) -> UtcDateTime {
    seconds
        .and_then(Value::as_f64)
        .and_then(|s| chrono::DateTime::from_timestamp_millis((s * 1000.0) as i64))
        .map(UtcDateTime::from_datetime)
        .unwrap_or_else(UtcDateTime::now)
}
fn error(kind: ProviderAdapterErrorKind, cause: impl std::fmt::Display) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(kind).with_cause(
        ProviderFailureCause::tagged("CodexAppServerError").with_message(cause.to_string()),
    )
}

pub struct CodexProviderAdapter {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    config: CodexRuntimeConfig,
    injected_client: Mutex<Option<Client>>,
}

impl CodexProviderAdapter {
    pub fn new(instance_id: ProviderInstanceId, config: CodexRuntimeConfig) -> Self {
        Self {
            instance_id,
            driver: driver(),
            config,
            injected_client: Mutex::new(None),
        }
    }
    /// Test seam: use the pinned app-server replay transport without paid turns.
    pub fn with_client(
        instance_id: ProviderInstanceId,
        config: CodexRuntimeConfig,
        client: Client,
    ) -> Self {
        Self {
            instance_id,
            driver: driver(),
            config,
            injected_client: Mutex::new(Some(client)),
        }
    }
}

fn capabilities() -> OrchestrationV2ProviderCapabilities {
    serde_json::from_value(json!({
        "sessions":{"supportsMultipleProviderThreadsPerSession":true,"supportsModelSwitchInSession":true,"supportsProviderSwitchingViaHandoff":true,"supportsRuntimeModeSwitchInSession":true,"pendingRequestsSurviveRestart":false},
        "threads":{"canCreateEmptyThread":true,"canReadThreadSnapshot":true,"canRollbackThread":true,"canForkThread":true,"canForkFromTurn":true,"canForkFromSubagentThread":true,"exposesNativeThreadId":true},
        "turns":{"exposesNativeTurnId":true,"emitsTurnStarted":true,"emitsTurnCompleted":true,"supportsInterrupt":true,"supportsActiveSteering":true,"supportsSteeringByInterruptRestart":true,"supportsQueuedMessages":true,"terminalStatusQuality":"strong"},
        "streaming":{"streamsAssistantText":true,"streamsReasoning":true,"streamsToolOutput":true,"streamsPlanText":true,"emitsMessageCompleted":true},
        "tools":{"exposesToolItemIds":true,"emitsToolStarted":true,"emitsToolCompleted":true,"emitsToolOutput":true,"supportsMcpTools":true,"supportsDynamicToolCallbacks":true},
        "approvals":{"supportsCommandApproval":true,"supportsFileReadApproval":true,"supportsFileChangeApproval":true,"supportsApplyPatchApproval":true,"approvalsHaveNativeRequestIds":true,"approvalCallbacksAreLiveOnly":true,"approvalsCanOriginateFromSubagents":true},
        "planning":{"emitsPlanUpdated":true,"emitsTodoList":true,"emitsProposedPlan":true,"supportsStructuredQuestions":true,"planDeltasHaveItemIds":true},
        "subagents":{"supportsSubagents":true,"exposesSubagentThreadIds":true,"emitsSubagentLifecycle":true,"canWaitForSubagents":true,"canCloseSubagents":true,"canForkSubagentThread":true},
        "context":{"acceptsSystemContext":true,"acceptsDeveloperContext":true,"acceptsSyntheticUserContext":true,"canGenerateSummaries":true,"canConsumeHandoffSummaries":true,"supportsDeltaHandoff":true,"supportsFullThreadHandoff":true,"maxRecommendedHandoffChars":null},
        "checkpointing":{"appCanCheckpointFilesystem":true,"supportsNestedCheckpointScopes":true,"providerCanRollbackConversation":true,"providerRollbackReturnsSnapshot":true,"providerCanReadConversationSnapshot":true},
        "identity":{"nativeThreadIds":"strong","nativeTurnIds":"strong","nativeItemIds":"strong","nativeRequestIds":"strong"},
        "runtimePolicy":{"enforcement":"native"}
    })).expect("upstream Codex capabilities")
}

#[async_trait]
impl ProviderAdapterV2 for CodexProviderAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        Ok(capabilities())
    }
    async fn plan_selection_transition(
        &self,
        _input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        Ok(turn_scoped_selection_transition())
    }
    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        let client = if let Some(client) = self.injected_client.lock().take() {
            client
        } else {
            let home = std::env::var("HOME").unwrap_or_default();
            self.config.spawn(Path::new(&home)).map_err(|e| {
                error(
                    ProviderAdapterErrorKind::OpenSession {
                        driver: self.driver.clone(),
                        provider_session_id: input.provider_session_id.clone(),
                    },
                    e,
                )
            })?
        };
        let session = CodexAdapter::initialize(client, &self.config.client_version)
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::OpenSession {
                        driver: self.driver.clone(),
                        provider_session_id: input.provider_session_id.clone(),
                    },
                    e,
                )
            })?;
        let events = Mutex::new(session.take_events());
        let requests = Mutex::new(session.take_requests());
        let now = UtcDateTime::now();
        let cwd = input
            .runtime_policy
            .cwd
            .as_deref()
            .unwrap_or_else(|| self.config.cwd.to_str().unwrap_or("."));
        let provider_session = OrchestrationV2ProviderSession {
            id: input.provider_session_id.clone(),
            driver: self.driver.clone(),
            provider_instance_id: self.instance_id.clone(),
            status: OrchestrationV2ProviderSessionStatus::Ready,
            cwd: nonempty(cwd),
            model: Some(input.model_selection.model.clone()),
            capabilities: capabilities(),
            created_at: now,
            updated_at: now,
            last_error: None,
        };
        Ok(Arc::new(CodexProviderRuntime {
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
            provider_session_id: input.provider_session_id,
            provider_session,
            session: Arc::new(session),
            events,
            requests,
            state: Arc::new(Mutex::new(RuntimeState::default())),
            config: self.config.clone(),
        }))
    }
}

#[derive(Default)]
struct RuntimeState {
    turns: HashMap<String, ProviderAdapterV2TurnInput>,
    requests: HashMap<RuntimeRequestId, u64>,
    /// Native item ids per native turn, in first-seen order.
    item_positions: HashMap<String, Vec<String>>,
}

pub struct CodexProviderRuntime {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    provider_session_id: ProviderSessionId,
    provider_session: OrchestrationV2ProviderSession,
    session: Arc<CodexSession>,
    events: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<CodexEvent>>>,
    requests: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PendingRequest>>>,
    state: Arc<Mutex<RuntimeState>>,
    config: CodexRuntimeConfig,
}

fn thread_from(
    response: &Value,
    instance_id: &ProviderInstanceId,
    session_id: &ProviderSessionId,
    app_thread_id: ThreadId,
    owner_node_id: Option<turin_contracts::NodeId>,
    forked_from: Option<OrchestrationV2ProviderThreadForkedFrom>,
) -> Result<OrchestrationV2ProviderThread, AppServerError> {
    let thread = response.get("thread").unwrap_or(response);
    let id =
        thread
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| AppServerError::InvalidPayload {
                method: "thread/start".into(),
                detail: "missing thread.id".into(),
            })?;
    Ok(OrchestrationV2ProviderThread {
        id: derive::provider_thread(&driver(), Some(instance_id), id),
        driver: driver(),
        provider_instance_id: instance_id.clone(),
        provider_session_id: Some(session_id.clone()),
        app_thread_id: Some(app_thread_id),
        owner_node_id,
        native_thread_ref: Some(native_ref(id)),
        native_conversation_head_ref: None,
        status: OrchestrationV2ProviderThreadStatus::Idle,
        first_run_ordinal: None,
        last_run_ordinal: None,
        handoff_ids: vec![],
        forked_from,
        pending_background_tasks: vec![],
        context_usage: None,
        native_metadata: None,
        goal: None,
        created_at: time(thread.get("createdAt")),
        updated_at: time(thread.get("updatedAt")),
    })
}

fn runtime_policy(
    policy: &turin_provider_core::provider_adapter::ProviderAdapterV2RuntimePolicy,
    fallback: &Path,
) -> RuntimePolicy {
    RuntimePolicy {
        cwd: policy
            .cwd
            .clone()
            .unwrap_or_else(|| fallback.to_string_lossy().into_owned()),
        runtime_mode: policy.runtime_mode,
        interaction_mode: policy.interaction_mode,
        approval_policy: policy.approval_policy.clone(),
        sandbox_policy: policy.sandbox_policy.clone(),
    }
}
fn option<'a>(selection: &'a ModelSelection, key: &str) -> Option<&'a str> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|o| o.id.as_str() == key)
        .and_then(|o| match &o.value {
            ProviderOptionSelectionValue::Str(s) => Some(s.as_str()),
            _ => None,
        })
}
fn turn_input(text: &str) -> Vec<Value> {
    if text.is_empty() {
        vec![]
    } else {
        vec![json!({"type":"text","text":text})]
    }
}
fn snapshot(
    thread: OrchestrationV2ProviderThread,
    payload: Option<Value>,
) -> ProviderAdapterV2ThreadSnapshot {
    ProviderAdapterV2ThreadSnapshot {
        provider_thread: thread,
        provider_turns: vec![],
        messages: vec![],
        runtime_requests: vec![],
        provider_payload: payload,
    }
}

#[async_trait]
impl ProviderSessionRuntime for CodexProviderRuntime {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.provider_session_id
    }
    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.provider_session
    }
    fn events(&self) -> ProviderEventStream {
        let Some(events) = self.events.lock().take() else {
            return Box::pin(stream::empty());
        };
        let requests = self.requests.lock().take();
        let state = self.state.clone();
        let session_id = self.provider_session_id.clone();
        Box::pin(stream::unfold(
            (events, requests, VecDeque::<ProviderAdapterV2Event>::new()),
            move |(mut events, mut requests, mut queue)| {
                let state = state.clone();
                let session_id = session_id.clone();
                async move {
                    loop {
                        if let Some(event) = queue.pop_front() {
                            return Some((Ok(event), (events, requests, queue)));
                        }
                        tokio::select! {
                            raw=events.recv()=>{let raw=raw?;queue.extend(project_event(raw,&state));},
                            request=async {match &mut requests {Some(rx)=>rx.recv().await,None=>std::future::pending().await}}=>{
                                let request=request?;queue.extend(project_request(request,&state,&session_id));
                            }
                        }
                    }
                }
            },
        ))
    }
    fn can_reuse_context_usage(
        &self,
        previous: &ModelSelection,
        next: &ModelSelection,
    ) -> Option<bool> {
        Some(
            previous.instance_id == next.instance_id
                && previous.model == next.model
                && previous
                    .options
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .filter(|o| o.id.as_str() != "reasoningEffort")
                    .eq(next
                        .options
                        .as_ref()
                        .into_iter()
                        .flatten()
                        .filter(|o| o.id.as_str() != "reasoningEffort")),
        )
    }
    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let cwd = input
            .runtime_policy
            .cwd
            .as_deref()
            .unwrap_or_else(|| self.config.cwd.to_str().unwrap_or("."));
        let response = self
            .session
            .start_thread(cwd, input.model_selection.model.as_str())
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::EnsureThread {
                        driver: self.driver.clone(),
                        thread_id: input.thread_id.clone(),
                    },
                    e,
                )
            })?;
        thread_from(
            &response,
            &self.instance_id,
            &self.provider_session_id,
            input.thread_id.clone(),
            None,
            None,
        )
        .map_err(|e| {
            error(
                ProviderAdapterErrorKind::EnsureThread {
                    driver: self.driver.clone(),
                    thread_id: input.thread_id,
                },
                e,
            )
        })
    }
    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let native = native_id(&input.provider_thread)?.to_owned();
        let cwd = input.runtime_policy.as_ref().and_then(|p| p.cwd.as_deref());
        let model = input.model_selection.as_ref().map(|m| m.model.as_str());
        let response = self
            .session
            .resume_thread(&native, cwd, model)
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::ResumeThread {
                        driver: self.driver.clone(),
                        provider_session_id: self.provider_session_id.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                    },
                    e,
                )
            })?;
        let mut thread = input.provider_thread;
        thread.provider_session_id = Some(self.provider_session_id.clone());
        thread.provider_instance_id = self.instance_id.clone();
        thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.native_thread_ref = Some(native_ref(&native));
        thread.updated_at = time(response.pointer("/thread/updatedAt"));
        Ok(thread)
    }
    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let policy = runtime_policy(&input.runtime_policy, &self.config.cwd);
        let model = input.model_selection.model.as_str();
        let native_turn = self
            .session
            .start_turn(
                &thread,
                turn_input(&input.message.text),
                model,
                option(&input.model_selection, "reasoningEffort"),
                option(&input.model_selection, "serviceTier"),
                &policy,
            )
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::TurnStart {
                        driver: self.driver.clone(),
                        thread_id: input.thread_id.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                        run_id: input.run_id.clone(),
                    },
                    e,
                )
            })?;
        self.state.lock().turns.insert(native_turn, input);
        Ok(())
    }
    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let native_turn = self
            .state
            .lock()
            .turns
            .iter()
            .find(|(id, _)| derive::provider_turn(&self.driver, id) == input.provider_turn_id)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| input.provider_turn_id.as_str().to_owned());
        self.session
            .steer_turn(&thread, &native_turn, turn_input(&input.message.text))
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::SteerRun {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id,
                        provider_turn_id: input.provider_turn_id,
                    },
                    e,
                )
            })
    }
    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let native_turn = self
            .state
            .lock()
            .turns
            .iter()
            .find(|(id, _)| derive::provider_turn(&self.driver, id) == input.provider_turn_id)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| input.provider_turn_id.as_str().to_owned());
        self.session
            .interrupt_turn(&thread, &native_turn)
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::Interrupt {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id,
                        provider_turn_id: input.provider_turn_id,
                    },
                    e,
                )
            })
    }
    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let Some(broker_id) = self.state.lock().requests.remove(&input.request_id) else {
            return Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::RuntimeRequestResponse {
                    driver: self.driver.clone(),
                    request_id: input.request_id,
                },
            ));
        };
        let decision = if let Some(answers) = input.answers {
            Decision::Answers(serde_json::to_value(answers).expect("answers"))
        } else {
            let choice = match input.decision {
                Some(ProviderApprovalDecision::Accept) => "accept",
                Some(ProviderApprovalDecision::AcceptForSession) => "acceptForSession",
                Some(ProviderApprovalDecision::AcceptAlways) => "acceptAlways",
                Some(ProviderApprovalDecision::Decline) => "decline",
                Some(ProviderApprovalDecision::Cancel) => "cancel",
                None => {
                    return Err(ProviderAdapterV2Error::new(
                        ProviderAdapterErrorKind::RuntimeRequestResponse {
                            driver: self.driver.clone(),
                            request_id: input.request_id,
                        },
                    ));
                }
            };
            Decision::Approval(choice)
        };
        if self.session.broker.reply(broker_id, decision) {
            Ok(())
        } else {
            Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::RuntimeRequestResponse {
                    driver: self.driver.clone(),
                    request_id: input.request_id,
                },
            ))
        }
    }
    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let native = native_id(&input.provider_thread)?.to_owned();
        let response = self
            .session
            .client()
            .request(
                "thread/read",
                Some(json!({"threadId":native,"includeTurns":true})),
            )
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::ReadThreadSnapshot {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                    },
                    e,
                )
            })?
            .unwrap_or(Value::Null);
        let mut thread = input.provider_thread;
        thread.updated_at = time(response.pointer("/thread/updatedAt"));
        Ok(snapshot(thread, response.get("thread").cloned()))
    }
    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let native = native_id(&input.provider_thread)?.to_owned();
        let count = match &input.target {
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => input
                .provider_thread_turns
                .iter()
                .filter(|t| {
                    matches!(
                        t.status,
                        OrchestrationV2ProviderTurnStatus::Completed
                            | OrchestrationV2ProviderTurnStatus::Interrupted
                            | OrchestrationV2ProviderTurnStatus::Failed
                            | OrchestrationV2ProviderTurnStatus::Cancelled
                    )
                })
                .count(),
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => input
                .provider_thread_turns
                .iter()
                .filter(|t| {
                    t.ordinal > provider_turn.ordinal
                        && matches!(
                            t.status,
                            OrchestrationV2ProviderTurnStatus::Completed
                                | OrchestrationV2ProviderTurnStatus::Interrupted
                                | OrchestrationV2ProviderTurnStatus::Failed
                                | OrchestrationV2ProviderTurnStatus::Cancelled
                        )
                })
                .count(),
        };
        let native_thread = self
            .session
            .revert_thread(&native, count as u64)
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::RollbackThread {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                        checkpoint_id: None,
                    },
                    e,
                )
            })?;
        let mut thread = input.provider_thread;
        thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.updated_at = time(native_thread.get("updatedAt"));
        thread.native_conversation_head_ref = match input.target {
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => {
                provider_turn.native_turn_ref
            }
            _ => None,
        };
        Ok(snapshot(thread, Some(native_thread)))
    }
    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let native = native_id(&input.source_provider_thread)?.to_owned();
        let mut params = crate::params::thread_runtime_params(
            input.runtime_policy.as_ref().and_then(|p| p.cwd.as_deref()),
            input.model_selection.as_ref().map(|m| m.model.as_str()),
        );
        params["threadId"] = json!(native);
        if let (Some(target), Some(turns)) = (&input.provider_turn_id, &input.source_provider_turns)
            && let Some(turn) = turns.iter().find(|turn| &turn.id == target)
            && let Some(id) = turn
                .native_turn_ref
                .as_ref()
                .and_then(|r| r.native_id.as_ref())
        {
            params["lastTurnId"] = json!(id.as_str());
        }
        let response = self
            .session
            .client()
            .request("thread/fork", Some(params))
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::ForkThread {
                        driver: self.driver.clone(),
                        provider_thread_id: input.source_provider_thread.id.clone(),
                    },
                    e,
                )
            })?
            .unwrap_or(Value::Null);
        thread_from(
            &response,
            &self.instance_id,
            &self.provider_session_id,
            input.target_thread_id,
            input.owner_node_id,
            Some(OrchestrationV2ProviderThreadForkedFrom {
                provider_thread_id: input.source_provider_thread.id.clone(),
                provider_turn_id: input.provider_turn_id,
                checkpoint_id: None,
            }),
        )
        .map_err(|e| {
            error(
                ProviderAdapterErrorKind::ForkThread {
                    driver: self.driver.clone(),
                    provider_thread_id: input.source_provider_thread.id,
                },
                e,
            )
        })
    }
}

fn project_event(raw: CodexEvent, state: &Mutex<RuntimeState>) -> Vec<ProviderAdapterV2Event> {
    let Some(native_turn) = raw
        .params
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .or_else(|| raw.params.get("turnId").and_then(Value::as_str))
    else {
        return vec![];
    };
    let Some(context) = state.lock().turns.get(native_turn).cloned() else {
        return vec![];
    };
    if raw.method == "item/completed" {
        return assistant_message_item(&raw, native_turn, &context, state)
            .into_iter()
            .collect();
    }
    let ordinal =
        PositiveInt::new(context.provider_turn_ordinal.max(1) as u64).expect("positive ordinal");
    let provider_turn_id = derive::provider_turn(&driver(), native_turn);
    let (status, terminal) = match raw.method.as_str() {
        "turn/started" => (OrchestrationV2ProviderTurnStatus::Running, None),
        "turn/completed" => {
            let (status, terminal) =
                match raw.params.pointer("/turn/status").and_then(Value::as_str) {
                    Some("completed") => (
                        OrchestrationV2ProviderTurnStatus::Completed,
                        ProviderAdapterV2TurnTerminalStatus::Completed,
                    ),
                    Some("interrupted") => (
                        OrchestrationV2ProviderTurnStatus::Interrupted,
                        ProviderAdapterV2TurnTerminalStatus::Interrupted,
                    ),
                    Some("cancelled") => (
                        OrchestrationV2ProviderTurnStatus::Cancelled,
                        ProviderAdapterV2TurnTerminalStatus::Cancelled,
                    ),
                    _ => (
                        OrchestrationV2ProviderTurnStatus::Failed,
                        ProviderAdapterV2TurnTerminalStatus::Failed,
                    ),
                };
            (status, Some(terminal))
        }
        _ => return vec![],
    };
    let turn = OrchestrationV2ProviderTurn {
        id: provider_turn_id.clone(),
        provider_thread_id: context.provider_thread.id.clone(),
        node_id: context.root_node_id.clone(),
        run_attempt_id: Some(context.attempt_id.clone()),
        native_turn_ref: Some(native_ref(native_turn)),
        ordinal,
        status,
        started_at: Some(time(raw.params.pointer("/turn/startedAt"))),
        completed_at: terminal.map(|_| time(raw.params.pointer("/turn/completedAt"))),
        token_usage: None,
        turn_token_usage: None,
    };
    let mut events = vec![ProviderAdapterV2Event::ProviderTurnUpdated {
        driver: driver(),
        thread_id: Some(context.thread_id.clone()),
        provider_turn: turn,
    }];
    if let Some(status) = terminal {
        events.push(ProviderAdapterV2Event::TurnTerminal {
            driver: driver(),
            provider_thread_id: context.provider_thread.id,
            provider_turn_id,
            run_ordinal: PositiveInt::new(context.run_ordinal.max(1) as u64)
                .expect("positive ordinal"),
            status,
            failure: None,
            failure_item_ordinal: None,
            retry: None,
            retry_started_at: None,
            thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
        });
        state.lock().turns.remove(native_turn);
    }
    events
}

/// Turn-item position, as upstream's `resolveItemPosition`: the provider turn
/// ordinal times 100, plus the item's first-seen position within the turn.
fn item_ordinal(
    state: &Mutex<RuntimeState>,
    native_turn: &str,
    native_item: &str,
    provider_turn_ordinal: i64,
) -> u64 {
    let mut state = state.lock();
    let items = state
        .item_positions
        .entry(native_turn.to_owned())
        .or_default();
    let index = match items.iter().position(|id| id == native_item) {
        Some(index) => index,
        None => {
            items.push(native_item.to_owned());
            items.len() - 1
        }
    };
    provider_turn_ordinal.max(0) as u64 * 100 + index as u64 + 1
}

/// `item/completed` for an agent message becomes the assistant turn item.
/// Streaming deltas are not mapped yet; the completed text is authoritative.
fn assistant_message_item(
    raw: &CodexEvent,
    native_turn: &str,
    context: &ProviderAdapterV2TurnInput,
    state: &Mutex<RuntimeState>,
) -> Option<ProviderAdapterV2Event> {
    use turin_contracts::orchestration_v2::{
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    };
    let item = raw.params.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
        return None;
    }
    let native_item = item.get("id").and_then(Value::as_str)?;
    let text = item
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let now = UtcDateTime::now();
    let turn_item = OrchestrationV2TurnItem::AssistantMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: derive::turn_item_from_provider_item(&driver(), native_item),
        thread_id: context.thread_id.clone(),
        run_id: Some(context.run_id.clone()),
        node_id: Some(derive::node_from_provider_item(&driver(), native_item)),
        provider_thread_id: Some(context.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), native_turn)),
        native_item_ref: Some(native_ref(native_item)),
        parent_item_id: None,
        ordinal: item_ordinal(
            state,
            native_turn,
            native_item,
            context.provider_turn_ordinal,
        ),
        status: OrchestrationV2TurnItemStatus::Completed,
        title: None,
        started_at: None,
        completed_at: Some(now),
        updated_at: now,
        message_id: derive::message_from_provider_item(&driver(), native_item),
        text,
        attachments: None,
        streaming: false,
    };
    Some(ProviderAdapterV2Event::TurnItemUpdated {
        driver: driver(),
        turn_item,
    })
}

fn project_request(
    request: PendingRequest,
    state: &Mutex<RuntimeState>,
    session_id: &ProviderSessionId,
) -> Vec<ProviderAdapterV2Event> {
    use turin_contracts::orchestration_v2::{
        OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
    };
    let native_turn = request.params.get("turnId").and_then(Value::as_str);
    let context = native_turn.and_then(|id| state.lock().turns.get(id).cloned());
    let Some(context) = context else {
        return vec![];
    };
    let native_req = request
        .params
        .get("approvalId")
        .or_else(|| request.params.get("itemId"))
        .and_then(Value::as_str);
    let provider_turn_id = native_turn.map(|id| derive::provider_turn(&driver(), id));
    let id = IdAllocator.runtime_request(&driver(), provider_turn_id.as_ref(), native_req);
    state.lock().requests.insert(id.clone(), request.id);
    let kind = match request.method.as_str() {
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            OrchestrationV2RuntimeRequestKind::FileChange
        }
        "item/permissions/requestApproval" => OrchestrationV2RuntimeRequestKind::Permission,
        "mcpServer/elicitation/request" => OrchestrationV2RuntimeRequestKind::McpElicitation,
        "item/tool/requestUserInput" => OrchestrationV2RuntimeRequestKind::UserInput,
        _ => OrchestrationV2RuntimeRequestKind::Command,
    };
    let runtime_request = OrchestrationV2RuntimeRequest {
        id,
        node_id: context.root_node_id,
        provider_turn_id,
        native_request_ref: native_req.map(native_ref),
        kind,
        status: OrchestrationV2RuntimeRequestStatus::Pending,
        response_capability: OrchestrationV2ResponseCapability::Live {
            provider_session_id: session_id.clone(),
        },
        created_at: UtcDateTime::now(),
        resolved_at: None,
        decision: None,
        answers: None,
    };
    vec![ProviderAdapterV2Event::RuntimeRequestUpdated {
        driver: driver(),
        thread_id: Some(context.thread_id),
        runtime_request,
    }]
}
