//! Port of upstream/apps/server/src/orchestration-v2/EffectWorker.ts (240-335).
//! The `provider-turn.steer` effect's two after-effects, from upstream
//! `EffectWorker.ts` (240-335):
//!
//! * a steer that landed in a delegated-completion message is accepted
//!   (`notification.delivery.accept`), and
//! * a steer that found its turn already completed re-dispatches the message as
//!   a follow-up, so the user's words start a new turn instead of vanishing.
//!
//! The effect executor runs the steer, then hands the result to [`after_steer`].

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2ServerCommand;

use super::provider_turn_control_service::ProviderTurnControlError;
use super::restart_continuation_service::RestartThreads;
use crate::pipeline::effect_outbox::{Effect, EffectRequest};
use crate::pipeline::orchestrator_port::{BoxError, RecordField, RecordFilter};
use crate::pipeline::thread_management_service::ThreadManagementError;

fn command(value: Value) -> Result<OrchestrationV2ServerCommand, BoxError> {
    Ok(serde_json::from_value(value)?)
}

/// What to do once a steer finished: accept a delegated-completion message the
/// provider took, or turn a steer that met a completed turn into a follow-up.
pub async fn after_steer(
    threads: &dyn RestartThreads,
    effect: &Effect,
    steered: Result<(), ProviderTurnControlError>,
) -> Result<(), BoxError> {
    let EffectRequest::ProviderTurnSteer { message_id, .. } = &effect.request else {
        return steered.map_err(|error| Box::new(error) as BoxError);
    };
    let records = |fields: &'static [RecordField]| {
        let message_ids = Some(vec![message_id.clone()]);
        async move {
            threads
                .get_thread_records(
                    &effect.thread_id,
                    fields,
                    &RecordFilter {
                        message_ids,
                        ..RecordFilter::default()
                    },
                )
                .await
        }
    };
    match steered {
        Ok(()) => {
            let projection = records(&[RecordField::Messages, RecordField::Runs]).await?;
            let Some(message) = projection
                .messages
                .iter()
                .find(|message| &message.id == message_id)
            else {
                return Ok(());
            };
            if message.delegated_completion.is_none() {
                return Ok(());
            }
            threads
                .dispatch(command(json!({
                    "type": "notification.delivery.accept",
                    "commandId": format!("command:mailbox-accepted:{}", effect.id),
                    "threadId": effect.thread_id,
                    "messageId": message.id,
                }))?)
                .await?;
            Ok(())
        }
        Err(error) if error.turn_completed == Some(true) => {
            let projection = records(&[RecordField::Messages, RecordField::Runs]).await?;
            let message = projection
                .messages
                .iter()
                .find(|message| &message.id == message_id);
            let run = message.and_then(|message| {
                projection
                    .runs
                    .iter()
                    .find(|run| Some(&run.id) == message.run_id.as_ref())
            });
            let (Some(message), Some(run)) = (message, run) else {
                return Err(Box::new(error));
            };
            let delegated = message.delegated_completion.is_some();
            let mut body = json!({
                "type": "message.dispatch",
                // Reuse the message identity and a stable command receipt so an
                // outbox retry cannot append a duplicate message or start a
                // second follow-up.
                "commandId": format!("command:steer-follow-up:{}", effect.id),
                "threadId": effect.thread_id,
                "messageId": message.id,
                "text": message.text,
                "attachments": message.attachments,
                // A user's follow-up starts on the thread's saved selection,
                // which already holds the steer's choice. A delegated completion
                // stays pinned to the run it reports to.
                "dispatchMode": {
                    "type": if delegated { "queue_after_active" } else { "start_immediately" }
                },
                "createdBy": message.created_by,
                "creationSource": message.creation_source,
            });
            let fields = body.as_object_mut().expect("object literal");
            if let Some(context) = &message.context {
                fields.insert("context".into(), json!(context));
            }
            if delegated {
                fields.insert("modelSelection".into(), json!(run.model_selection));
            }
            if let Some(completion) = &message.delegated_completion {
                fields.insert("delegatedCompletion".into(), json!(completion));
            }
            if let Some(notification) = &message.notification {
                fields.insert("notification".into(), json!(notification));
            }
            if let Some(task) = &message.scheduled_task_id {
                fields.insert("scheduledTaskId".into(), json!(task));
            }
            if let Some(sender) = &message.sender_thread_id {
                fields.insert("senderThreadId".into(), json!(sender));
            }
            match threads.dispatch(command(body)?).await {
                Ok(_) => Ok(()),
                // The turn that just completed leaves no further retry target
                // once the instance is off: the completed turn stands as the
                // thread's result instead of going through the outbox's bounded
                // retry on a refusal that retrying cannot change.
                Err(ThreadManagementError::ProviderInstanceDisabled { .. }) => {
                    tracing::info!(
                        thread_id = %effect.thread_id,
                        message_id = %message.id,
                        "orchestration-v2.effect-worker.steer-follow-up-disabled"
                    );
                    Ok(())
                }
                Err(other) => Err(Box::new(other)),
            }
        }
        Err(error) => Err(Box::new(error)),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use turin_contracts::orchestration_v2::OrchestrationV2ThreadProjection;
    use turin_contracts::{RunId, ThreadId};

    use super::*;
    use crate::core::provider_turn_control_service::ProviderTurnControlOperation;
    use crate::pipeline::effect_outbox::EffectStatus;
    use crate::pipeline::orchestrator_port::DispatchResult;
    use crate::projection::test_support::{message, run, thread};

    struct Threads {
        projection: OrchestrationV2ThreadProjection,
        dispatched: Mutex<Vec<Value>>,
        disabled: bool,
    }

    #[async_trait]
    impl RestartThreads for Threads {
        async fn get_thread_records(
            &self,
            _thread_id: &ThreadId,
            _fields: &[RecordField],
            _filter: &RecordFilter,
        ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
            Ok(self.projection.clone())
        }

        async fn dispatch(
            &self,
            command: OrchestrationV2ServerCommand,
        ) -> Result<DispatchResult, ThreadManagementError> {
            self.dispatched
                .lock()
                .push(serde_json::to_value(command).unwrap());
            if self.disabled {
                return Err(ThreadManagementError::ProviderInstanceDisabled {
                    project_id: turin_contracts::ProjectId::new("project:test").unwrap(),
                    instance_id: turin_contracts::provider_instance::ProviderInstanceId::new(
                        "codex",
                    )
                    .unwrap(),
                });
            }
            Ok(DispatchResult {
                sequence: 1,
                stored_events: Vec::new(),
            })
        }

        async fn recover_delegated_task(&self, _thread_id: &ThreadId, _source_run_id: &RunId) {}
    }

    fn projection(delegated: bool) -> OrchestrationV2ThreadProjection {
        let mut value = json!({
            "thread": thread("thread:steer"),
            "runs": [run("thread:steer", "run:1", 1, "completed")],
            "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
            "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
            "messages": [message("thread:steer", "message:steer", Some("run:1"), "user", "also this", "2026-01-01T00:00:00.000Z")],
            "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
            "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [],
            "updatedAt": "2026-01-01T00:00:00.000Z",
        });
        if delegated {
            value["messages"][0]["delegatedCompletion"] =
                json!({"parentRunId": "run:1", "generation": 1, "taskIds": ["node:task"]});
        }
        serde_json::from_value(value).expect("projection decodes")
    }

    fn effect() -> Effect {
        Effect {
            id: "effect:steer:1".to_owned(),
            command_id: turin_contracts::CommandId::new("command:steer").unwrap(),
            thread_id: ThreadId::new("thread:steer").unwrap(),
            request: serde_json::from_value(json!({
                "type": "provider-turn.steer",
                "providerSessionId": "session:1",
                "providerThreadId": "provider-thread:1",
                "providerTurnId": "provider-turn:1",
                "messageId": "message:steer",
            }))
            .unwrap(),
            status: EffectStatus::Running,
            attempt_count: 1,
            available_at: String::new(),
            lease_owner: None,
            lease_expires_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            completed_at: None,
            last_error: None,
        }
    }

    fn steer_error(turn_completed: Option<bool>) -> ProviderTurnControlError {
        ProviderTurnControlError {
            thread_id: ThreadId::new("thread:steer").unwrap(),
            operation: ProviderTurnControlOperation::Steer,
            provider_turn_id: turin_contracts::ProviderTurnId::new("provider-turn:1").unwrap(),
            turn_completed,
            cause: "turn already completed".to_owned(),
        }
    }

    #[tokio::test]
    async fn a_steer_that_met_a_completed_turn_becomes_a_follow_up() {
        let threads = Threads {
            projection: projection(false),
            dispatched: Mutex::new(Vec::new()),
            disabled: false,
        };
        after_steer(&threads, &effect(), Err(steer_error(Some(true))))
            .await
            .expect("the follow-up dispatches");
        let sent = threads.dispatched.lock();
        let [follow_up] = sent.as_slice() else {
            panic!("one follow-up: {sent:?}");
        };
        assert_eq!(follow_up["type"], "message.dispatch");
        assert_eq!(
            follow_up["commandId"],
            "command:steer-follow-up:effect:steer:1"
        );
        assert_eq!(follow_up["messageId"], "message:steer");
        assert_eq!(follow_up["text"], "also this");
        assert_eq!(follow_up["dispatchMode"]["type"], "start_immediately");
        assert!(follow_up.get("modelSelection").is_none());
    }

    #[tokio::test]
    async fn a_delegated_completion_follow_up_queues_pinned_to_its_run() {
        let threads = Threads {
            projection: projection(true),
            dispatched: Mutex::new(Vec::new()),
            disabled: false,
        };
        after_steer(&threads, &effect(), Err(steer_error(Some(true))))
            .await
            .expect("the follow-up dispatches");
        let sent = threads.dispatched.lock();
        assert_eq!(sent[0]["dispatchMode"]["type"], "queue_after_active");
        assert!(sent[0].get("modelSelection").is_some());
        assert!(sent[0].get("delegatedCompletion").is_some());
    }

    #[tokio::test]
    async fn another_steer_failure_stays_a_failure() {
        let threads = Threads {
            projection: projection(false),
            dispatched: Mutex::new(Vec::new()),
            disabled: false,
        };
        after_steer(&threads, &effect(), Err(steer_error(None)))
            .await
            .expect_err("the effect fails and retries");
        assert!(threads.dispatched.lock().is_empty());
    }

    #[tokio::test]
    async fn a_disabled_instance_leaves_the_completed_turn_as_the_result() {
        let threads = Threads {
            projection: projection(false),
            dispatched: Mutex::new(Vec::new()),
            disabled: true,
        };
        after_steer(&threads, &effect(), Err(steer_error(Some(true))))
            .await
            .expect("the refusal is not retried");
    }

    #[tokio::test]
    async fn an_accepted_delegated_steer_accepts_the_notification() {
        let threads = Threads {
            projection: projection(true),
            dispatched: Mutex::new(Vec::new()),
            disabled: false,
        };
        after_steer(&threads, &effect(), Ok(()))
            .await
            .expect("accepted");
        {
            let sent = threads.dispatched.lock();
            assert_eq!(sent[0]["type"], "notification.delivery.accept");
            assert_eq!(
                sent[0]["commandId"],
                "command:mailbox-accepted:effect:steer:1"
            );
        }
        // An ordinary steer sends nothing more.
        let plain = Threads {
            projection: projection(false),
            dispatched: Mutex::new(Vec::new()),
            disabled: false,
        };
        after_steer(&plain, &effect(), Ok(())).await.expect("ok");
        assert!(plain.dispatched.lock().is_empty());
    }
}
