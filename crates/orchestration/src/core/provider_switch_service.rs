//! Port of upstream/apps/server/src/orchestration-v2/ProviderSwitchService.ts.
//!
//! Plans how a thread moves to a new model selection: keep the live session,
//! switch the model in it, restart and resume the native thread, or hand off to
//! a fresh session. Pure decision over the projection plus the adapters'
//! metadata and selection classification. The caller applies the plan.

use std::{cmp::Reverse, fmt, sync::Arc};

use async_trait::async_trait;
use turin_contracts::{
    ProviderSessionId, ProviderThreadId, ThreadId,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2ProviderCapabilities, OrchestrationV2ProviderSession,
        OrchestrationV2ProviderSessionStatus, OrchestrationV2ThreadProjection,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    driver::ProviderContinuationIdentity,
    provider_adapter_registry::{ProviderAdapterMetadata, ProviderAdapterRegistry},
    selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
};

use super::provider_session_transition_policy::{
    ProviderSessionTransition, ProviderSessionTransitionState, ProviderSessionTransitionTarget,
    decide_provider_session_transition, model_selections_equal,
};

/// What the caller applies: release sessions, attach or create a native thread,
/// and carry the selection change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSwitchPlan {
    pub instance_changed: bool,
    pub model_changed: bool,
    pub target_provider_thread_id: Option<ProviderThreadId>,
    pub release_provider_session_ids: Vec<ProviderSessionId>,
    /// Never `Reject`: a rejected transition is a [`ProviderSwitchPlanError`].
    pub transition: ProviderSessionTransition,
}

/// `ProviderSwitchPlanError`. `cause` is the rejection reason or the failure text.
#[derive(Debug)]
pub struct ProviderSwitchPlanError {
    pub thread_id: ThreadId,
    pub target_provider_instance_id: ProviderInstanceId,
    pub cause: String,
}

impl fmt::Display for ProviderSwitchPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Cannot switch thread {} to provider instance {}: {}",
            self.thread_id.as_str(),
            self.target_provider_instance_id.as_str(),
            self.cause
        )
    }
}

impl std::error::Error for ProviderSwitchPlanError {}

/// The adapter lookups the planner needs. The registry implements it; tests fake it.
#[async_trait]
pub trait SwitchAdapters: Send + Sync {
    /// Instance metadata, or `None` when the instance is unknown.
    async fn metadata(&self, instance_id: &ProviderInstanceId) -> Option<ProviderAdapterMetadata>;

    /// Whether an adapter is registered for the instance.
    async fn has_adapter(&self, instance_id: &ProviderInstanceId) -> bool;

    /// The target adapter's classification of a selection change. Called only
    /// when [`Self::has_adapter`] holds for the instance.
    async fn plan_selection_transition(
        &self,
        instance_id: &ProviderInstanceId,
        input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, String>;
}

#[async_trait]
impl SwitchAdapters for ProviderAdapterRegistry {
    async fn metadata(&self, instance_id: &ProviderInstanceId) -> Option<ProviderAdapterMetadata> {
        // Upstream falls back to `get` + `getCapabilities` when the registry has
        // no metadata lookup. Turin's registry always has one, so that fallback
        // is not ported (see docs/handoff/T13e.md, Deviations).
        self.get_metadata(instance_id).await.ok()
    }

    async fn has_adapter(&self, instance_id: &ProviderInstanceId) -> bool {
        self.get(instance_id).is_ok()
    }

    async fn plan_selection_transition(
        &self,
        instance_id: &ProviderInstanceId,
        input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, String> {
        let adapter = self.get(instance_id).map_err(|error| error.to_string())?;
        adapter
            .plan_selection_transition(input)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Stopped and errored records stay in session history but can no longer be
/// restarted or released; only live sessions take part in a transition.
fn is_live_session(session: &OrchestrationV2ProviderSession) -> bool {
    !matches!(
        session.status,
        OrchestrationV2ProviderSessionStatus::Stopped | OrchestrationV2ProviderSessionStatus::Error
    )
}

/// Upstream's `toSorted` by `updatedAt`, newest first. The sort is stable.
fn newest_first<T>(items: &mut [&T], updated_at: impl Fn(&T) -> chrono::DateTime<chrono::Utc>) {
    items.sort_by_key(|item| Reverse(updated_at(item)));
}

pub struct ProviderSwitchService {
    adapters: Arc<dyn SwitchAdapters>,
}

impl ProviderSwitchService {
    pub fn new(adapters: Arc<dyn SwitchAdapters>) -> Self {
        Self { adapters }
    }

    /// `plan`: decides the transition from the thread's current instance and
    /// session state to `target`. A rejected transition is an error.
    pub async fn plan(
        &self,
        projection: &OrchestrationV2ThreadProjection,
        target: &ModelSelection,
    ) -> Result<ProviderSwitchPlan, ProviderSwitchPlanError> {
        let thread = &projection.thread;
        self.plan_inner(projection, target)
            .await
            .map_err(|cause| ProviderSwitchPlanError {
                thread_id: thread.id.clone(),
                target_provider_instance_id: target.instance_id.clone(),
                cause,
            })
    }

    async fn plan_inner(
        &self,
        projection: &OrchestrationV2ThreadProjection,
        target: &ModelSelection,
    ) -> Result<ProviderSwitchPlan, String> {
        let thread = &projection.thread;
        let current = &thread.model_selection;
        let instance_changed = current.instance_id != target.instance_id;
        let model_changed = current.model != target.model;
        let current_instance = self.adapters.metadata(&current.instance_id).await;
        let target_instance = self.adapters.metadata(&target.instance_id).await;
        let target_has_adapter = self.adapters.has_adapter(&target.instance_id).await;

        let mut current_sessions: Vec<&OrchestrationV2ProviderSession> = projection
            .provider_sessions
            .iter()
            .filter(|session| session.provider_instance_id == current.instance_id)
            .collect();
        newest_first(&mut current_sessions, |session| {
            session.updated_at.as_datetime()
        });
        let current_session = current_sessions
            .iter()
            .copied()
            .find(|session| is_live_session(session));
        // Negotiated capabilities describe the provider, not the dead process.
        // The newest record still reports what the instance supports after its
        // session stops.
        let negotiated_capabilities: Option<OrchestrationV2ProviderCapabilities> = current_session
            .or_else(|| current_sessions.first().copied())
            .map(|session| session.capabilities);
        // Detaching a process removes its session binding, not its native history.
        let current_provider_thread = projection.provider_threads.iter().find(|provider_thread| {
            Some(&provider_thread.id) == thread.active_provider_thread_id.as_ref()
                && provider_thread.provider_instance_id == current.instance_id
                && provider_thread.native_thread_ref.is_some()
        });
        let has_current_binding = current_session.is_some() || current_provider_thread.is_some();

        let selection_transition = match &current_instance {
            Some(current_meta)
                if current.instance_id == target.instance_id
                    && !model_selections_equal(current, target)
                    && target_has_adapter
                    && has_current_binding =>
            {
                let session_capabilities =
                    negotiated_capabilities.unwrap_or(current_meta.capabilities);
                Some(
                    self.adapters
                        .plan_selection_transition(
                            &target.instance_id,
                            ProviderSelectionTransitionInput {
                                current: current.clone(),
                                target: target.clone(),
                                session_capabilities,
                            },
                        )
                        .await?,
                )
            }
            _ => None,
        };

        let transition = match (&target_instance, target_has_adapter) {
            (Some(target_meta), true) => {
                let current_state = match &current_instance {
                    Some(current_meta) if has_current_binding => {
                        Some(ProviderSessionTransitionState {
                            driver: current_meta.driver.clone(),
                            continuation_identity: ProviderContinuationIdentity {
                                driver_kind: current_meta.driver.clone(),
                                continuation_key: current_meta.continuation_key.clone(),
                            },
                            model_selection: current.clone(),
                            runtime_mode: thread.runtime_mode,
                            interaction_mode: thread.interaction_mode,
                            workspace: current_session
                                .map(|session| session.cwd.as_str().to_owned())
                                .or_else(|| {
                                    thread
                                        .worktree_path
                                        .as_ref()
                                        .map(|path| path.as_str().to_owned())
                                })
                                .unwrap_or_else(|| "<unresolved-workspace>".to_owned()),
                            capabilities: negotiated_capabilities
                                .unwrap_or(current_meta.capabilities),
                        })
                    }
                    _ => None,
                };
                let target_state = ProviderSessionTransitionTarget {
                    state: ProviderSessionTransitionState {
                        driver: target_meta.driver.clone(),
                        continuation_identity: ProviderContinuationIdentity {
                            driver_kind: target_meta.driver.clone(),
                            continuation_key: target_meta.continuation_key.clone(),
                        },
                        model_selection: target.clone(),
                        runtime_mode: thread.runtime_mode,
                        interaction_mode: thread.interaction_mode,
                        workspace: thread
                            .worktree_path
                            .as_ref()
                            .map(|path| path.as_str().to_owned())
                            .or_else(|| {
                                current_session.map(|session| session.cwd.as_str().to_owned())
                            })
                            .unwrap_or_else(|| "<unresolved-workspace>".to_owned()),
                        capabilities: target_meta.capabilities,
                    },
                    available: target_meta.enabled,
                };
                decide_provider_session_transition(
                    current_state.as_ref(),
                    &target_state,
                    selection_transition.as_ref(),
                )
            }
            _ => ProviderSessionTransition::Reject {
                reason: "The target provider instance is unavailable.".to_owned(),
            },
        };
        if let ProviderSessionTransition::Reject { reason } = transition {
            return Err(reason);
        }

        let mut target_threads: Vec<
            &turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        > = projection
            .provider_threads
            .iter()
            .filter(|provider_thread| {
                provider_thread.app_thread_id.as_ref() == Some(&thread.id)
                    && provider_thread.owner_node_id.is_none()
                    && provider_thread.provider_instance_id == target.instance_id
            })
            .collect();
        newest_first(&mut target_threads, |provider_thread| {
            provider_thread.updated_at.as_datetime()
        });
        let target_provider_thread_id = target_threads
            .first()
            .map(|provider_thread| provider_thread.id.clone());

        let release_provider_session_ids = projection
            .provider_sessions
            .iter()
            .filter(|session| is_live_session(session))
            .filter(|session| match &transition {
                // Other live records may serve pooled or delegated bindings;
                // only the session being replaced is released.
                ProviderSessionTransition::RestartAndResume => {
                    current_session.is_some_and(|current| current.id == session.id)
                }
                ProviderSessionTransition::CreateWithHandoff => {
                    session.provider_instance_id != target.instance_id
                }
                _ => false,
            })
            .map(|session| session.id.clone())
            .collect();

        Ok(ProviderSwitchPlan {
            instance_changed,
            model_changed,
            target_provider_thread_id,
            release_provider_session_ids,
            transition,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex as StdMutex};

    use serde_json::{Value, json};
    use turin_provider_core::selection_transition::acp_selection_transition;

    use super::*;
    use crate::projection::test_support;

    const INSTANCE: &str = "codex_primary";
    const SESSION: &str = "session_primary";
    const T0: &str = "2026-06-20T00:00:00.000Z";

    /// Test capabilities. `switch` controls in-session model switching.
    fn capabilities(switch: bool) -> Value {
        let mut value = test_support::capabilities();
        value["sessions"]["supportsModelSwitchInSession"] = json!(switch);
        value
    }

    fn at(offset_seconds: i64) -> String {
        let base = chrono::DateTime::parse_from_rfc3339(T0).expect("base time");
        (base + chrono::Duration::seconds(offset_seconds))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    fn session(id: &str, status: &str, offset: i64, switch: bool) -> Value {
        json!({
            "id": id,
            "driver": "codex",
            "providerInstanceId": INSTANCE,
            "status": status,
            "cwd": "/repo",
            "model": null,
            "capabilities": capabilities(switch),
            "createdAt": T0,
            "updatedAt": at(offset),
            "lastError": null,
        })
    }

    fn provider_thread(
        native: bool,
        instance: &str,
        owner: Option<&str>,
        app_thread: &str,
    ) -> Value {
        json!({
            "id": "provider-thread:native",
            "driver": "codex",
            "providerInstanceId": instance,
            "providerSessionId": SESSION,
            "appThreadId": app_thread,
            "ownerNodeId": owner,
            "nativeThreadRef": if native {
                json!({"driver": "codex", "nativeId": "native-thread:abc", "strength": "strong"})
            } else {
                Value::Null
            },
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": null,
            "lastRunOrdinal": null,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": T0,
            "updatedAt": T0,
        })
    }

    /// A thread on the current instance. `sessions` are its provider session
    /// records; `native` attaches a native provider thread as the active one.
    fn projection(
        sessions: Vec<Value>,
        native: bool,
        threads: Vec<Value>,
    ) -> OrchestrationV2ThreadProjection {
        let mut thread = test_support::thread("thread_switch_service");
        thread["modelSelection"] = json!({"instanceId": INSTANCE, "model": "gpt-5.1-codex"});
        thread["worktreePath"] = json!("/repo");
        if native {
            thread["activeProviderThreadId"] = json!("provider-thread:native");
        }
        let mut provider_threads = threads;
        if native && provider_threads.is_empty() {
            provider_threads.push(provider_thread(
                true,
                INSTANCE,
                None,
                "thread_switch_service",
            ));
        }
        serde_json::from_value(json!({
            "thread": thread,
            "runs": [],
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": sessions,
            "providerThreads": provider_threads,
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
            "updatedAt": T0,
        }))
        .expect("projection fixture")
    }

    fn live(switch: bool) -> Value {
        session(SESSION, "ready", 0, switch)
    }

    fn target(instance: &str, model: &str) -> ModelSelection {
        serde_json::from_value(json!({"instanceId": instance, "model": model}))
            .expect("model selection fixture")
    }

    struct FakeAdapters {
        /// Instance id -> continuation key. Absent instances have no metadata or adapter.
        instances: HashMap<String, String>,
        /// The classification the target adapter returns. Defaults to a restart.
        classify: Box<
            dyn Fn(&ProviderSelectionTransitionInput) -> ProviderSelectionTransitionPlan
                + Send
                + Sync,
        >,
        classified: StdMutex<usize>,
    }

    impl FakeAdapters {
        fn new(instances: &[(&str, &str)]) -> Self {
            Self {
                instances: instances
                    .iter()
                    .map(|(id, key)| (id.to_string(), key.to_string()))
                    .collect(),
                classify: Box::new(|_| ProviderSelectionTransitionPlan::RestartSession),
                classified: StdMutex::new(0),
            }
        }

        fn classifying(
            mut self,
            classify: impl Fn(&ProviderSelectionTransitionInput) -> ProviderSelectionTransitionPlan
            + Send
            + Sync
            + 'static,
        ) -> Self {
            self.classify = Box::new(classify);
            self
        }
    }

    #[async_trait]
    impl SwitchAdapters for FakeAdapters {
        async fn metadata(
            &self,
            instance_id: &ProviderInstanceId,
        ) -> Option<ProviderAdapterMetadata> {
            let key = self.instances.get(instance_id.as_str())?;
            Some(ProviderAdapterMetadata {
                driver: turin_contracts::provider_instance::ProviderDriverKind::new("codex")
                    .expect("driver"),
                continuation_key: key.clone(),
                enabled: true,
                capabilities: serde_json::from_value(capabilities(false)).expect("capabilities"),
            })
        }

        async fn has_adapter(&self, instance_id: &ProviderInstanceId) -> bool {
            self.instances.contains_key(instance_id.as_str())
        }

        async fn plan_selection_transition(
            &self,
            _instance_id: &ProviderInstanceId,
            input: ProviderSelectionTransitionInput,
        ) -> Result<ProviderSelectionTransitionPlan, String> {
            *self.classified.lock().expect("lock") += 1;
            Ok((self.classify)(&input))
        }
    }

    fn service(adapters: FakeAdapters) -> ProviderSwitchService {
        ProviderSwitchService::new(Arc::new(adapters))
    }

    fn primary() -> FakeAdapters {
        FakeAdapters::new(&[(INSTANCE, "codex:account:primary")])
    }

    fn session_id(value: &str) -> ProviderSessionId {
        ProviderSessionId::new(value).expect("session id")
    }

    #[tokio::test]
    async fn restarts_and_releases_the_current_session_for_an_unsupported_in_session_model_change()
    {
        let thread = projection(vec![live(false)], false, vec![]);
        let plan = service(primary())
            .plan(&thread, &target(INSTANCE, "gpt-5.2-codex"))
            .await
            .expect("plan");
        assert_eq!(plan.transition, ProviderSessionTransition::RestartAndResume);
        assert_eq!(plan.release_provider_session_ids, vec![session_id(SESSION)]);
    }

    #[tokio::test]
    async fn restarts_and_releases_the_live_session_when_a_newer_dead_session_exists() {
        for status in ["stopped", "error"] {
            let thread = projection(
                vec![live(false), session("dead_session", status, 1, false)],
                false,
                vec![],
            );
            let plan = service(primary())
                .plan(&thread, &target(INSTANCE, "gpt-5.2-codex"))
                .await
                .expect("plan");
            assert_eq!(plan.transition, ProviderSessionTransition::RestartAndResume);
            assert_eq!(plan.release_provider_session_ids, vec![session_id(SESSION)]);
        }
    }

    #[tokio::test]
    async fn releases_the_newest_live_session_not_the_newest_record_overall() {
        let thread = projection(
            vec![
                live(false),
                session("session_newer_live", "ready", 1, false),
                session("dead_session", "stopped", 2, false),
            ],
            false,
            vec![],
        );
        let plan = service(primary())
            .plan(&thread, &target(INSTANCE, "gpt-5.2-codex"))
            .await
            .expect("plan");
        assert_eq!(plan.transition, ProviderSessionTransition::RestartAndResume);
        assert_eq!(
            plan.release_provider_session_ids,
            vec![session_id("session_newer_live")]
        );
    }

    #[tokio::test]
    async fn creates_a_fresh_session_with_handoff_when_every_recorded_session_is_dead() {
        let thread = projection(
            vec![
                session("dead_session_older", "stopped", 0, false),
                session("dead_session_newer", "error", 2, false),
            ],
            false,
            vec![],
        );
        let plan = service(primary())
            .plan(&thread, &target(INSTANCE, "gpt-5.2-codex"))
            .await
            .expect("plan");
        assert_eq!(
            plan.transition,
            ProviderSessionTransition::CreateWithHandoff
        );
        assert!(plan.release_provider_session_ids.is_empty());
    }

    #[tokio::test]
    async fn falls_back_to_the_native_provider_thread_when_every_recorded_session_is_dead() {
        let thread = projection(vec![session(SESSION, "stopped", 0, false)], true, vec![]);
        let plan = service(primary())
            .plan(&thread, &target(INSTANCE, "gpt-5.2-codex"))
            .await
            .expect("plan");
        assert_eq!(plan.transition, ProviderSessionTransition::RestartAndResume);
        assert!(plan.release_provider_session_ids.is_empty());
    }

    #[tokio::test]
    async fn hands_off_from_a_native_provider_thread_after_its_session_detaches() {
        let thread = projection(vec![], true, vec![]);
        let adapters = FakeAdapters::new(&[
            (INSTANCE, "codex:account:primary"),
            ("codex_other", "codex:account:other"),
        ]);
        let plan = service(adapters)
            .plan(&thread, &target("codex_other", "gpt-5.2-codex"))
            .await
            .expect("plan");
        assert_eq!(
            plan.transition,
            ProviderSessionTransition::CreateWithHandoff
        );
        assert!(plan.release_provider_session_ids.is_empty());
    }

    #[tokio::test]
    async fn applies_a_model_change_on_the_next_turn_when_a_dead_session_negotiated_switching() {
        for status in ["stopped", "error"] {
            // Static capabilities report no in-session switch, but the dead
            // record's negotiated capabilities do. Without them the ACP
            // classification rejects the selection instead of reopening.
            let thread = projection(vec![session(SESSION, status, 0, true)], true, vec![]);
            let adapters = primary().classifying(acp_selection_transition);
            let plan = service(adapters)
                .plan(&thread, &target(INSTANCE, "gpt-5.2-codex"))
                .await
                .expect("plan");
            assert_eq!(
                plan.transition,
                ProviderSessionTransition::SwitchModelInSession
            );
            assert!(plan.release_provider_session_ids.is_empty());
        }
    }

    #[tokio::test]
    async fn rejects_a_model_change_the_dead_record_never_negotiated_support_for() {
        let thread = projection(vec![session(SESSION, "stopped", 0, false)], true, vec![]);
        let adapters = primary().classifying(acp_selection_transition);
        let error = service(adapters)
            .plan(&thread, &target(INSTANCE, "gpt-5.2-codex"))
            .await
            .expect_err("rejected");
        assert_eq!(error.thread_id.as_str(), "thread_switch_service");
        assert_eq!(error.target_provider_instance_id.as_str(), INSTANCE);
    }

    #[tokio::test]
    async fn distinguishes_compatible_and_incompatible_instances_of_the_same_driver() {
        let thread = projection(vec![live(false)], false, vec![]);
        let adapters = FakeAdapters::new(&[
            (INSTANCE, "codex:account:primary"),
            ("codex_compatible", "codex:account:primary"),
            ("codex_incompatible", "codex:account:other"),
        ]);
        let service = service(adapters);
        let compatible = service
            .plan(&thread, &target("codex_compatible", "gpt-5.1-codex"))
            .await
            .expect("compatible plan");
        let incompatible = service
            .plan(&thread, &target("codex_incompatible", "gpt-5.1-codex"))
            .await
            .expect("incompatible plan");
        assert_eq!(
            compatible.transition,
            ProviderSessionTransition::RestartAndResume
        );
        assert_eq!(
            incompatible.transition,
            ProviderSessionTransition::CreateWithHandoff
        );
    }
}
