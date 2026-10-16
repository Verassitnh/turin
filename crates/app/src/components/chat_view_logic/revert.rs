//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`waitForRevertedMessage`).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use turin_contracts::{CommandId, MessageId, RunId};

use crate::state::models::RunStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertWaitRollbackFailure {
    pub request_id: CommandId,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertWaitRun {
    pub id: RunId,
    pub ordinal: u64,
    pub status: RunStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertWaitProjection {
    pub message_ids: Vec<MessageId>,
    pub message_run_id: Option<RunId>,
    pub rollback_failure: Option<RevertWaitRollbackFailure>,
    pub runs: Vec<RevertWaitRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevertWaitError {
    MessageUnavailable,
    RollbackFailed(String),
    TimedOut,
}

pub fn evaluate_revert_wait(
    projection: Option<&RevertWaitProjection>,
    message_run_id: Option<&RunId>,
    turn_count: u64,
    request_id: &CommandId,
    accepted: bool,
) -> Option<Result<(), RevertWaitError>> {
    let projection = projection?;
    if let Some(failure) = &projection.rollback_failure
        && failure.request_id == *request_id
    {
        return Some(Err(RevertWaitError::RollbackFailed(
            failure.message.clone(),
        )));
    }
    let message_run_id = message_run_id?;
    if accepted
        && projection.runs.iter().any(|run| {
            &run.id == message_run_id
                && run.ordinal > turn_count
                && run.status == RunStatus::RolledBack
        })
    {
        return Some(Ok(()));
    }
    None
}

pub async fn wait_for_reverted_message<F, Fut>(
    message_id: MessageId,
    turn_count: u64,
    request_id: CommandId,
    revert: F,
    read: Arc<dyn Fn() -> Option<RevertWaitProjection> + Send + Sync>,
    timeout_ms: u64,
) -> Result<(), RevertWaitError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let initial = read();
    if initial
        .as_ref()
        .is_none_or(|projection| !projection.message_ids.iter().any(|id| id == &message_id))
    {
        return Err(RevertWaitError::MessageUnavailable);
    }
    let message_run_id = initial.and_then(|projection| projection.message_run_id);

    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

    if let Some(result) = evaluate_revert_wait(
        read().as_ref(),
        message_run_id.as_ref(),
        turn_count,
        &request_id,
        false,
    ) {
        return result;
    }

    revert().await;
    if let Some(result) = evaluate_revert_wait(
        read().as_ref(),
        message_run_id.as_ref(),
        turn_count,
        &request_id,
        true,
    ) {
        return result;
    }

    let mut interval = tokio::time::interval(Duration::from_millis(5));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Some(result) = evaluate_revert_wait(
                    read().as_ref(),
                    message_run_id.as_ref(),
                    turn_count,
                    &request_id,
                    true,
                ) {
                    return result;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(RevertWaitError::TimedOut);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_reports_matching_rollback_failures() {
        let message_id = MessageId::new("message-2").unwrap();
        let request_id = CommandId::new("rollback-1").unwrap();
        let projection = RevertWaitProjection {
            message_ids: vec![message_id.clone()],
            message_run_id: Some(RunId::new("run-2").unwrap()),
            rollback_failure: Some(RevertWaitRollbackFailure {
                request_id: request_id.clone(),
                message: "The provider could not roll back.".to_owned(),
            }),
            runs: Vec::new(),
        };
        assert_eq!(
            evaluate_revert_wait(
                Some(&projection),
                projection.message_run_id.as_ref(),
                1,
                &request_id,
                false,
            ),
            Some(Err(RevertWaitError::RollbackFailed(
                "The provider could not roll back.".to_owned()
            )))
        );
    }

    #[test]
    fn evaluate_ignores_failures_for_other_request_ids() {
        let message_id = MessageId::new("message-2").unwrap();
        let request_id = CommandId::new("rollback-1").unwrap();
        let projection = RevertWaitProjection {
            message_ids: vec![message_id.clone()],
            message_run_id: Some(RunId::new("run-2").unwrap()),
            rollback_failure: Some(RevertWaitRollbackFailure {
                request_id: CommandId::new("rollback-0").unwrap(),
                message: "Old failure.".to_owned(),
            }),
            runs: Vec::new(),
        };
        assert_eq!(
            evaluate_revert_wait(
                Some(&projection),
                projection.message_run_id.as_ref(),
                1,
                &request_id,
                true,
            ),
            None
        );
    }

    #[tokio::test]
    async fn wait_surfaces_projected_rollback_failures() {
        let message_id = MessageId::new("message-2").unwrap();
        let request_id = CommandId::new("rollback-1").unwrap();
        let projection = Arc::new(std::sync::Mutex::new(RevertWaitProjection {
            message_ids: vec![message_id.clone()],
            message_run_id: Some(RunId::new("run-2").unwrap()),
            rollback_failure: None,
            runs: Vec::new(),
        }));
        let read = {
            let projection = Arc::clone(&projection);
            Arc::new(move || Some(projection.lock().expect("lock").clone()))
        };
        let request_for_wait = request_id.clone();
        let handle = tokio::spawn(async move {
            wait_for_reverted_message(message_id, 1, request_for_wait, || async {}, read, 120_000)
                .await
        });
        tokio::task::yield_now().await;
        projection.lock().expect("lock").rollback_failure = Some(RevertWaitRollbackFailure {
            request_id,
            message: "The provider could not roll back.".to_owned(),
        });
        let result = handle.await.expect("join");
        assert!(matches!(
            result,
            Err(RevertWaitError::RollbackFailed(message)) if message == "The provider could not roll back."
        ));
    }

    #[tokio::test]
    async fn wait_times_out_when_only_an_old_failure_is_recorded() {
        let message_id = MessageId::new("message-2").unwrap();
        let request_id = CommandId::new("rollback-1").unwrap();
        let projection = RevertWaitProjection {
            message_ids: vec![message_id.clone()],
            message_run_id: Some(RunId::new("run-2").unwrap()),
            rollback_failure: Some(RevertWaitRollbackFailure {
                request_id: CommandId::new("rollback-0").unwrap(),
                message: "Old failure.".to_owned(),
            }),
            runs: Vec::new(),
        };
        let read = Arc::new(move || Some(projection.clone()));
        let result =
            wait_for_reverted_message(message_id, 1, request_id, || async {}, read, 50).await;
        assert!(matches!(result, Err(RevertWaitError::TimedOut)));
    }

    #[tokio::test]
    async fn wait_keeps_the_original_run_after_the_message_leaves() {
        let message_id = MessageId::new("message-2").unwrap();
        let request_id = CommandId::new("rollback-1").unwrap();
        let run_id = RunId::new("run-2").unwrap();
        let projection = Arc::new(std::sync::Mutex::new(RevertWaitProjection {
            message_ids: vec![message_id.clone()],
            message_run_id: Some(run_id.clone()),
            rollback_failure: None,
            runs: Vec::new(),
        }));
        let read = {
            let projection = Arc::clone(&projection);
            Arc::new(move || Some(projection.lock().expect("lock").clone()))
        };
        let request_for_wait = request_id.clone();
        let handle = tokio::spawn(async move {
            wait_for_reverted_message(message_id, 1, request_for_wait, || async {}, read, 120_000)
                .await
        });
        tokio::task::yield_now().await;
        {
            let mut current = projection.lock().expect("lock");
            current.message_ids.clear();
            current.message_run_id = Some(RunId::new("other-run").unwrap());
            current.runs.push(RevertWaitRun {
                id: run_id,
                ordinal: 2,
                status: RunStatus::RolledBack,
            });
        }
        assert_eq!(handle.await.expect("join"), Ok(()));
    }
}
