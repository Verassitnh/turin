//! Port of upstream/apps/server/src/orchestration-v2/RuntimeRequestService.ts.
//!
//! Forwards an orchestrator-resolved approval or user-input answer to the
//! provider session that is waiting on it. Dispatch validates the request while
//! it is pending and persists it as resolved; this service only checks that the
//! stored request is still answerable on this session, then delivers it.

use std::{error::Error, fmt, sync::Arc};

use async_trait::async_trait;
use turin_contracts::{
    ProviderSessionId, RuntimeRequestId, ThreadId,
    orchestration_v2::{
        OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestStatus,
    },
    provider_policy::ProviderApprovalDecision,
    provider_policy::ProviderUserInputAnswers,
};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2RuntimeRequestResponseInput, ProviderSessionRuntime,
};

use super::provider_session_manager::ProviderSessionManager;
use crate::{
    pipeline::orchestrator_port::BoxError,
    projection::store::{ProjectionStore, canonical::ProjectionRecordField},
};

/// `RuntimeRequestResponseExecutionError`'s `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRequestResponseReason {
    RequestMissing,
    RequestNotReady,
    RequestNotResumable,
    ProviderSessionNotActive,
    UnexpectedFailure,
}

/// `RuntimeRequestResponseExecutionError`. Only `UnexpectedFailure` carries a cause.
#[derive(Debug)]
pub struct RuntimeRequestResponseExecutionError {
    pub reason: RuntimeRequestResponseReason,
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub request_id: RuntimeRequestId,
    pub cause: Option<BoxError>,
}

impl fmt::Display for RuntimeRequestResponseExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let request_id = self.request_id.as_str();
        let thread_id = self.thread_id.as_str();
        let session_id = self.provider_session_id.as_str();
        match self.reason {
            RuntimeRequestResponseReason::RequestMissing => write!(
                f,
                "Runtime request {request_id} no longer exists on thread {thread_id}."
            ),
            RuntimeRequestResponseReason::RequestNotReady => write!(
                f,
                "Runtime request {request_id} on thread {thread_id} is not ready for response execution."
            ),
            RuntimeRequestResponseReason::RequestNotResumable => write!(
                f,
                "Runtime request {request_id} on thread {thread_id} is not resumable on provider session {session_id}."
            ),
            RuntimeRequestResponseReason::ProviderSessionNotActive => write!(
                f,
                "Provider session {session_id} is not active for runtime request {request_id} on thread {thread_id}."
            ),
            RuntimeRequestResponseReason::UnexpectedFailure => write!(
                f,
                "Failed to respond to runtime request {request_id} on thread {thread_id} via provider session {session_id}."
            ),
        }
    }
}

impl Error for RuntimeRequestResponseExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause
            .as_deref()
            .map(|cause| cause as &(dyn Error + 'static))
    }
}

/// `RuntimeRequestServiceV2.respond` input.
#[derive(Debug, Clone)]
pub struct RuntimeRequestResponse {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub request_id: RuntimeRequestId,
    pub decision: Option<ProviderApprovalDecision>,
    pub answers: Option<ProviderUserInputAnswers>,
}

/// The stored request lookup (`ProjectionStore.getRuntimeRequest`).
#[async_trait]
pub trait RuntimeRequestReads: Send + Sync {
    async fn get_runtime_request(
        &self,
        thread_id: &ThreadId,
        request_id: &RuntimeRequestId,
    ) -> Result<Option<OrchestrationV2RuntimeRequest>, BoxError>;
}

/// The live-session lookup (`ProviderSessionManagerV2.get`). The manager is the
/// production implementation; tests supply a fake.
#[async_trait]
pub trait LiveSessions: Send + Sync {
    async fn get_session(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Result<Option<Arc<dyn ProviderSessionRuntime>>, BoxError>;
}

#[async_trait]
impl RuntimeRequestReads for ProjectionStore {
    async fn get_runtime_request(
        &self,
        thread_id: &ThreadId,
        request_id: &RuntimeRequestId,
    ) -> Result<Option<OrchestrationV2RuntimeRequest>, BoxError> {
        let projection = self
            .get_thread_records(
                thread_id,
                vec![ProjectionRecordField::RuntimeRequests],
                None,
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(projection
            .runtime_requests
            .into_iter()
            .find(|request| &request.id == request_id))
    }
}

#[async_trait]
impl LiveSessions for ProviderSessionManager {
    async fn get_session(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Result<Option<Arc<dyn ProviderSessionRuntime>>, BoxError> {
        Ok(self.get(provider_session_id))
    }
}

/// `RuntimeRequestServiceV2`.
pub struct RuntimeRequestService {
    projections: Arc<dyn RuntimeRequestReads>,
    sessions: Arc<dyn LiveSessions>,
}

/// Why a response stopped before it reached the provider.
enum Failure {
    Reason(RuntimeRequestResponseReason),
    Cause(BoxError),
}

impl RuntimeRequestService {
    pub fn new(projections: Arc<dyn RuntimeRequestReads>, sessions: Arc<dyn LiveSessions>) -> Self {
        Self {
            projections,
            sessions,
        }
    }

    /// `respond`: forwards a resolved request's answer to its live session.
    pub async fn respond(
        &self,
        input: RuntimeRequestResponse,
    ) -> Result<(), RuntimeRequestResponseExecutionError> {
        self.respond_inner(&input).await.map_err(|failure| {
            let (reason, cause) = match failure {
                Failure::Reason(reason) => (reason, None),
                Failure::Cause(cause) => {
                    (RuntimeRequestResponseReason::UnexpectedFailure, Some(cause))
                }
            };
            RuntimeRequestResponseExecutionError {
                reason,
                thread_id: input.thread_id.clone(),
                provider_session_id: input.provider_session_id.clone(),
                request_id: input.request_id.clone(),
                cause,
            }
        })
    }

    async fn respond_inner(&self, input: &RuntimeRequestResponse) -> Result<(), Failure> {
        let request = self
            .projections
            .get_runtime_request(&input.thread_id, &input.request_id)
            .await
            .map_err(Failure::Cause)?
            .ok_or(Failure::Reason(
                RuntimeRequestResponseReason::RequestMissing,
            ))?;
        // Dispatch validates the request while it is pending, then persists the
        // resolved projection before this effect is executed.
        if request.status != OrchestrationV2RuntimeRequestStatus::Resolved {
            return Err(Failure::Reason(
                RuntimeRequestResponseReason::RequestNotReady,
            ));
        }
        match &request.response_capability {
            OrchestrationV2ResponseCapability::Live {
                provider_session_id,
            } if provider_session_id == &input.provider_session_id => {}
            _ => {
                return Err(Failure::Reason(
                    RuntimeRequestResponseReason::RequestNotResumable,
                ));
            }
        }
        let session = self
            .sessions
            .get_session(&input.provider_session_id)
            .await
            .map_err(Failure::Cause)?
            .ok_or(Failure::Reason(
                RuntimeRequestResponseReason::ProviderSessionNotActive,
            ))?;
        session
            .respond_to_runtime_request(ProviderAdapterV2RuntimeRequestResponseInput {
                request_id: input.request_id.clone(),
                decision: input.decision.clone(),
                answers: input.answers.clone(),
                response: None,
            })
            .await
            .map_err(|error| Failure::Cause(Box::new(error)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use parking_lot::Mutex;
    use serde_json::{Value, json};
    use turin_contracts::{
        orchestration_v2::{OrchestrationV2ProviderSession, OrchestrationV2ProviderThread},
        provider_instance::{ProviderDriverKind, ProviderInstanceId},
    };
    use turin_provider_core::provider_adapter::{
        ProviderAdapterV2Error, ProviderAdapterV2RuntimeRequestResponseInput, ProviderEventStream,
    };

    use super::*;

    const THREAD: &str = "thread-runtime-request";
    const SESSION: &str = "provider-session-runtime-request";
    const REQUEST: &str = "request-validation";

    fn request(status: &str, capability: Value) -> OrchestrationV2RuntimeRequest {
        serde_json::from_value(json!({
            "id": REQUEST,
            "nodeId": "node:runtime-request",
            "providerTurnId": "provider-turn:runtime-request",
            "nativeRequestRef": null,
            "kind": "command",
            "status": status,
            "responseCapability": capability,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "resolvedAt": null,
            "decision": null,
            "answers": null,
        }))
        .expect("runtime request fixture")
    }

    fn live(session: &str) -> Value {
        json!({"type": "live", "providerSessionId": session})
    }

    fn input() -> RuntimeRequestResponse {
        RuntimeRequestResponse {
            thread_id: ThreadId::new(THREAD).expect("thread id"),
            provider_session_id: ProviderSessionId::new(SESSION).expect("session id"),
            request_id: RuntimeRequestId::new(REQUEST).expect("request id"),
            decision: Some(ProviderApprovalDecision::Accept),
            answers: None,
        }
    }

    struct FakeReads {
        request: Option<OrchestrationV2RuntimeRequest>,
        fail: bool,
    }

    #[async_trait]
    impl RuntimeRequestReads for FakeReads {
        async fn get_runtime_request(
            &self,
            _thread_id: &ThreadId,
            _request_id: &RuntimeRequestId,
        ) -> Result<Option<OrchestrationV2RuntimeRequest>, BoxError> {
            if self.fail {
                return Err("lookup failed".into());
            }
            Ok(self.request.clone())
        }
    }

    struct FakeSessions {
        runtime: Option<Arc<FakeRuntime>>,
        lookups: AtomicUsize,
    }

    #[async_trait]
    impl LiveSessions for FakeSessions {
        async fn get_session(
            &self,
            _provider_session_id: &ProviderSessionId,
        ) -> Result<Option<Arc<dyn ProviderSessionRuntime>>, BoxError> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .runtime
                .clone()
                .map(|runtime| runtime as Arc<dyn ProviderSessionRuntime>))
        }
    }

    /// Records the responses it receives. Every other runtime method is unused here.
    struct FakeRuntime {
        responses: Mutex<Vec<ProviderAdapterV2RuntimeRequestResponseInput>>,
        instance: ProviderInstanceId,
        driver: ProviderDriverKind,
        session_id: ProviderSessionId,
        session: OrchestrationV2ProviderSession,
    }

    fn unused() -> ! {
        unreachable!("RuntimeRequestService only calls respond_to_runtime_request")
    }

    #[async_trait]
    impl ProviderSessionRuntime for FakeRuntime {
        fn instance_id(&self) -> &ProviderInstanceId {
            &self.instance
        }
        fn driver(&self) -> &ProviderDriverKind {
            &self.driver
        }
        fn provider_session_id(&self) -> &ProviderSessionId {
            &self.session_id
        }
        fn provider_session(&self) -> &OrchestrationV2ProviderSession {
            &self.session
        }
        fn events(&self) -> ProviderEventStream {
            Box::pin(futures::stream::empty())
        }
        async fn ensure_thread(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2EnsureThreadInput,
        ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
            unused()
        }
        async fn resume_thread(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2ResumeThreadInput,
        ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
            unused()
        }
        async fn start_turn(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2TurnInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            unused()
        }
        async fn steer_turn(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2SteerInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            unused()
        }
        async fn interrupt_turn(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2InterruptInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            unused()
        }
        async fn respond_to_runtime_request(
            &self,
            input: ProviderAdapterV2RuntimeRequestResponseInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            self.responses.lock().push(input);
            Ok(())
        }
        async fn read_thread_snapshot(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2ReadThreadSnapshotInput,
        ) -> Result<
            turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
            ProviderAdapterV2Error,
        > {
            unused()
        }
        async fn rollback_thread(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2RollbackThreadInput,
        ) -> Result<
            turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
            ProviderAdapterV2Error,
        > {
            unused()
        }
        async fn fork_thread(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2ForkThreadInput,
        ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
            unused()
        }
    }

    fn runtime() -> Arc<FakeRuntime> {
        let session = serde_json::from_value(json!({
            "id": SESSION,
            "driver": "codex",
            "providerInstanceId": "codex",
            "status": "ready",
            "cwd": "/",
            "model": null,
            "capabilities": crate::pipeline::test_support::default_capabilities(),
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z",
            "lastError": null,
        }))
        .expect("session fixture");
        Arc::new(FakeRuntime {
            responses: Mutex::new(Vec::new()),
            instance: ProviderInstanceId::new("codex").expect("instance id"),
            driver: ProviderDriverKind::new("codex").expect("driver"),
            session_id: ProviderSessionId::new(SESSION).expect("session id"),
            session,
        })
    }

    fn service(
        request: Option<OrchestrationV2RuntimeRequest>,
        sessions: Option<Arc<FakeRuntime>>,
    ) -> (RuntimeRequestService, Arc<FakeSessions>) {
        let sessions = Arc::new(FakeSessions {
            runtime: sessions,
            lookups: AtomicUsize::new(0),
        });
        let service = RuntimeRequestService::new(
            Arc::new(FakeReads {
                request,
                fail: false,
            }),
            sessions.clone(),
        );
        (service, sessions)
    }

    #[tokio::test]
    async fn forwards_orchestrator_resolved_runtime_requests_to_the_live_adapter() {
        let runtime = runtime();
        let (service, _) = service(
            Some(request("resolved", live(SESSION))),
            Some(runtime.clone()),
        );
        service.respond(input()).await.expect("respond");
        let responses = runtime.responses.lock();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].request_id.as_str(), REQUEST);
        assert_eq!(
            responses[0].decision,
            Some(ProviderApprovalDecision::Accept)
        );
        assert!(responses[0].answers.is_none());
    }

    #[tokio::test]
    async fn rejects_pending_requests_without_consulting_the_live_session() {
        let (service, sessions) = service(Some(request("pending", live(SESSION))), Some(runtime()));
        let error = service.respond(input()).await.expect_err("not ready");
        assert_eq!(error.reason, RuntimeRequestResponseReason::RequestNotReady);
        assert_eq!(
            error.to_string(),
            format!(
                "Runtime request {REQUEST} on thread {THREAD} is not ready for response execution."
            )
        );
        assert!(error.cause.is_none());
        assert_eq!(sessions.lookups.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn classifies_runtime_request_response_validation_failures() {
        let (missing, missing_sessions) = service(None, Some(runtime()));
        let error = missing.respond(input()).await.expect_err("missing");
        assert_eq!(error.reason, RuntimeRequestResponseReason::RequestMissing);
        assert_eq!(
            error.to_string(),
            format!("Runtime request {REQUEST} no longer exists on thread {THREAD}.")
        );
        assert!(error.cause.is_none());

        let (not_resumable, _) = service(
            Some(request("resolved", live("provider-session-other"))),
            Some(runtime()),
        );
        let error = not_resumable
            .respond(input())
            .await
            .expect_err("not resumable");
        assert_eq!(
            error.reason,
            RuntimeRequestResponseReason::RequestNotResumable
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Runtime request {REQUEST} on thread {THREAD} is not resumable on provider session {SESSION}."
            )
        );
        assert!(error.cause.is_none());

        let (inactive, inactive_sessions) = service(Some(request("resolved", live(SESSION))), None);
        let error = inactive.respond(input()).await.expect_err("inactive");
        assert_eq!(
            error.reason,
            RuntimeRequestResponseReason::ProviderSessionNotActive
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Provider session {SESSION} is not active for runtime request {REQUEST} on thread {THREAD}."
            )
        );
        assert!(error.cause.is_none());
        assert_eq!(missing_sessions.lookups.load(Ordering::SeqCst), 0);
        assert_eq!(inactive_sessions.lookups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn preserves_genuine_lookup_failures_as_the_cause() {
        let service = RuntimeRequestService::new(
            Arc::new(FakeReads {
                request: Some(request("resolved", live(SESSION))),
                fail: true,
            }),
            Arc::new(FakeSessions {
                runtime: None,
                lookups: AtomicUsize::new(0),
            }),
        );
        let error = service.respond(input()).await.expect_err("lookup failure");
        assert_eq!(
            error.reason,
            RuntimeRequestResponseReason::UnexpectedFailure
        );
        assert_eq!(
            error.to_string(),
            format!(
                "Failed to respond to runtime request {REQUEST} on thread {THREAD} via provider session {SESSION}."
            )
        );
        assert_eq!(
            error.source().map(|cause| cause.to_string()),
            Some("lookup failed".to_owned())
        );
    }
}
