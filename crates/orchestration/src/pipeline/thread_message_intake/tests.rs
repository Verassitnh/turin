//! Ports of upstream `ThreadMessageIntake.test.ts` (the launch cases are
//! gone with `ThreadLaunchService`; the poisoned-defect case is JS-specific).

use super::*;
use crate::pipeline::attachment_store::{create_pending_attachment_id, resolve_attachment_path};
use crate::pipeline::orchestrator_port::OrchestratorError;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::path::Path;
use turin_contracts::CommandId;
use turin_contracts::orchestration_v2::OrchestrationV2StoredEvent;

type DispatchFn = Box<
    dyn Fn(&OrchestrationV2ServerCommand) -> Result<DispatchResult, ThreadManagementError>
        + Send
        + Sync,
>;

struct MockThreads {
    captured: Mutex<Vec<OrchestrationV2ServerCommand>>,
    dispatch: DispatchFn,
    send: Mutex<Option<ThreadManagementError>>,
}

impl MockThreads {
    fn new(dispatch: DispatchFn) -> Arc<Self> {
        Arc::new(Self {
            captured: Mutex::new(vec![]),
            dispatch,
            send: Mutex::new(None),
        })
    }

    /// "Stop after intake": an error that may follow a commit.
    fn failing() -> Arc<Self> {
        Self::new(Box::new(|command| {
            let value = serde_json::to_value(command).unwrap();
            Err(ThreadManagementError::Orchestrator(
                OrchestratorError::Dispatch {
                    command_id: CommandId::new(value["commandId"].as_str().unwrap()).unwrap(),
                    command_type: value["type"].as_str().unwrap().to_owned(),
                    cause: "Stop after intake".into(),
                },
            ))
        }))
    }

    fn succeeding(events: Vec<OrchestrationV2StoredEvent>) -> Arc<Self> {
        Self::new(Box::new(move |_| {
            Ok(DispatchResult {
                sequence: 1,
                stored_events: events.clone(),
            })
        }))
    }
}

#[async_trait]
impl IntakeDispatcher for MockThreads {
    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError> {
        self.captured.lock().push(command.clone());
        (self.dispatch)(&command)
    }

    async fn send_to_thread(&self, _input: SendInput) -> Result<SendResult, ThreadManagementError> {
        Err(self.send.lock().take().expect("scripted send failure"))
    }
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn intake(&self, threads: Arc<dyn IntakeDispatcher>) -> ThreadMessageIntake {
        ThreadMessageIntake::new(AttachmentClaims::new(self.dir.path()), threads)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn stage(&self, bytes: &[u8]) -> String {
        let id = create_pending_attachment_id(None);
        std::fs::write(self.path().join(format!("{id}.png")), bytes).unwrap();
        id
    }

    fn with_prefix(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|name| name.starts_with(prefix))
            .collect();
        names.sort();
        names
    }

    fn pending_exists(&self, id: &str) -> bool {
        self.path().join(format!("{id}.png")).exists()
    }
}

fn image(id: &str, name: &str, size: u64) -> Value {
    json!({"type": "image", "id": id, "name": name, "mimeType": "image/png", "sizeBytes": size})
}

fn respond(
    command: &str,
    thread: &str,
    request: &str,
    answers: Value,
    attachments: Value,
) -> OrchestrationV2Command {
    serde_json::from_value(json!({
        "type": "runtime-request.respond", "commandId": command, "threadId": thread,
        "requestId": request, "answers": answers, "attachmentsByQuestionId": attachments,
    }))
    .unwrap()
}

fn answered_events(
    thread: &str,
    request: &str,
    attachments: Option<Value>,
) -> Vec<OrchestrationV2StoredEvent> {
    let mut payload = json!({
        "id": "item-recorded-answer", "type": "user_input_request", "threadId": thread,
        "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
        "nativeItemRef": null, "parentItemId": null, "ordinal": 1, "status": "completed",
        "title": null, "startedAt": null, "completedAt": "2026-09-13T00:00:00.000Z",
        "updatedAt": "2026-09-13T00:00:00.000Z", "requestId": request, "questions": [],
    });
    if let Some(attachments) = attachments {
        payload["questionAnswer"] = json!({
            "requestId": request, "answers": {}, "attachmentsByQuestionId": attachments,
        });
    }
    vec![crate::pipeline::test_support::stored(
        1,
        json!({
            "id": "evt-recorded-answer", "type": "turn-item.updated", "threadId": thread,
            "occurredAt": "2026-09-13T00:00:00.000Z", "payload": payload,
        }),
    )]
}

fn captured_respond(threads: &MockThreads) -> Value {
    serde_json::to_value(&threads.captured.lock()[0]).unwrap()
}

#[tokio::test]
async fn claims_question_uploads_and_passes_readable_paths_through_the_v2_request_command() {
    let f = Fixture::new();
    let id = f.stage(&[1, 2, 3]);
    let threads = MockThreads::failing();
    let result = f
        .intake(threads.clone())
        .dispatch_command(respond(
            "answer-1",
            "thread-answer",
            "request-1",
            json!({"q": ["Selected option"]}),
            json!({"q": [image(&id, "screen.png", 3)]}),
        ))
        .await;
    assert!(result.is_err());
    assert_eq!(threads.captured.lock().len(), 1);
    let command = captured_respond(&threads);
    assert_eq!(command["type"], "runtime-request.respond");
    let attachment: ChatAttachment =
        serde_json::from_value(command["attachmentsByQuestionId"]["q"][0].clone()).unwrap();
    let path = resolve_attachment_path(f.path(), &attachment).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
    assert_eq!(
        command["answers"]["q"],
        json!([
            "Selected option",
            format!(
                "Attached image \"screen.png\": {}",
                serde_json::to_string(&path.to_string_lossy()).unwrap()
            )
        ])
    );
}

#[tokio::test]
async fn rolls_back_earlier_question_claims_when_a_later_pending_upload_is_missing() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    let existing = "thread-q-fail-00000000-0000-4000-8000-000000000001";
    std::fs::write(f.path().join(format!("{existing}.png")), [9, 9, 9, 9]).unwrap();
    let threads = MockThreads::failing();
    let error = f
        .intake(threads.clone())
        .dispatch_command(respond(
            "answer-missing-pending",
            "thread-q-fail",
            "request-missing-pending",
            json!({"q1": ["one"], "q2": ["two"]}),
            json!({
                "q1": [image(existing, "keep.png", 4), image(&pending, "screen.png", 3)],
                "q2": [image(&create_pending_attachment_id(None), "gone.png", 3)],
            }),
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, IntakeError::Claim(_)));
    assert!(threads.captured.lock().is_empty());
    // The pending sources stay behind so the user can retry the response.
    assert_eq!(
        std::fs::read(f.path().join(format!("{pending}.png"))).unwrap(),
        [1, 2, 3]
    );
    // The pre-existing thread attachment is retained; only the new copy is rolled back.
    assert_eq!(f.with_prefix("thread-q-fail-"), [format!("{existing}.png")]);
}

#[tokio::test]
async fn rolls_back_earlier_question_claims_when_attachment_path_preparation_fails() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    let threads = MockThreads::failing();
    let error = f
        .intake(threads.clone())
        .dispatch_command(respond(
            "answer-missing-claimed",
            "thread-path-fail",
            "request-missing-claimed",
            json!({"q1": ["one"], "q2": ["two"]}),
            json!({
                "q1": [image(&pending, "screen.png", 3)],
                "q2": [image("thread-path-fail-00000000-0000-4000-8000-000000000099", "missing.png", 3)],
            }),
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, IntakeError::Claim(_)));
    assert!(threads.captured.lock().is_empty());
    assert!(f.pending_exists(&pending));
    assert!(f.with_prefix("thread-path-fail-").is_empty());
}

#[tokio::test]
async fn retains_claimed_copies_when_dispatch_failure_may_have_been_accepted() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    let threads = MockThreads::failing();
    let error = f
        .intake(threads.clone())
        .dispatch_command(respond(
            "answer-ambiguous",
            "thread-ambiguous",
            "request-ambiguous",
            json!({"q": ["one"]}),
            json!({"q": [image(&pending, "screen.png", 3)]}),
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, IntakeError::Management(_)));
    assert_eq!(threads.captured.lock().len(), 1);
    // The response may have been accepted before the error, so the copy that
    // backs the answer's path stays for the provider to read.
    assert_eq!(f.with_prefix("thread-ambiguous-").len(), 1);
    assert!(f.pending_exists(&pending));
}

#[tokio::test]
async fn releases_claimed_copies_when_the_command_was_already_rejected() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    // A retry of a previously rejected command claims fresh copies, then the
    // orchestrator replays the rejection without touching them.
    let threads = MockThreads::new(Box::new(|command| {
        let value = serde_json::to_value(command).unwrap();
        Err(ThreadManagementError::Orchestrator(
            OrchestratorError::CommandPreviouslyRejected {
                command_id: CommandId::new(value["commandId"].as_str().unwrap()).unwrap(),
                command_type: "runtime-request.respond".into(),
                detail: "rejected earlier".into(),
            },
        ))
    }));
    let result = f
        .intake(threads)
        .dispatch_command(respond(
            "answer-rejected",
            "thread-rejected",
            "request-rejected",
            json!({"q": ["one"]}),
            json!({"q": [image(&pending, "screen.png", 3)]}),
        ))
        .await;
    assert!(result.is_err());
    assert!(f.with_prefix("thread-rejected-").is_empty());
    assert!(f.pending_exists(&pending));
}

#[tokio::test]
async fn releases_claimed_copies_when_dispatch_replays_an_earlier_accepted_response() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    // The first attempt was accepted but its response was lost. The retry's
    // dispatch replays the stored events, which reference the first claim's
    // attachment id, not this attempt's fresh copy.
    let earlier = "thread-replay-00000000-0000-4000-8000-0000000000aa";
    std::fs::write(f.path().join(format!("{earlier}.png")), [4, 4, 4]).unwrap();
    let events = answered_events(
        "thread-replay",
        "request-replay",
        Some(json!({"q": [image(earlier, "screen.png", 3)]})),
    );
    let result = f
        .intake(MockThreads::succeeding(events))
        .dispatch_command(respond(
            "answer-replay",
            "thread-replay",
            "request-replay",
            json!({"q": ["one"]}),
            json!({"q": [image(&pending, "screen.png", 3)]}),
        ))
        .await;
    assert!(result.is_ok());
    // The accepted copy from the first attempt is untouched; only the
    // retry's unreferenced copy is removed.
    assert_eq!(f.with_prefix("thread-replay-"), [format!("{earlier}.png")]);
    assert_eq!(
        std::fs::read(f.path().join(format!("{earlier}.png"))).unwrap(),
        [4, 4, 4]
    );
    assert!(f.pending_exists(&pending));
}

#[tokio::test]
async fn releases_claimed_copies_when_the_recorded_answer_has_no_attachments() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    let events = answered_events(
        "thread-empty-answer",
        "request-empty-answer",
        Some(json!({})),
    );
    let result = f
        .intake(MockThreads::succeeding(events))
        .dispatch_command(respond(
            "answer-empty",
            "thread-empty-answer",
            "request-empty-answer",
            json!({"q": ["one"]}),
            json!({"q": [image(&pending, "screen.png", 3)]}),
        ))
        .await;
    assert!(result.is_ok());
    assert!(f.with_prefix("thread-empty-answer-").is_empty());
    assert!(f.pending_exists(&pending));
}

#[tokio::test]
async fn releases_claimed_copies_when_the_recorded_answer_was_text_only() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    // The first attempt accepted a text-only response, so its recorded turn
    // item has no questionAnswer at all.
    let events = answered_events("thread-text-only-answer", "request-text-only", None);
    let result = f
        .intake(MockThreads::succeeding(events))
        .dispatch_command(respond(
            "answer-text-only",
            "thread-text-only-answer",
            "request-text-only",
            json!({"q": ["one"]}),
            json!({"q": [image(&pending, "screen.png", 3)]}),
        ))
        .await;
    assert!(result.is_ok());
    assert!(f.with_prefix("thread-text-only-answer-").is_empty());
    assert!(f.pending_exists(&pending));
}

#[tokio::test]
async fn retains_claimed_copies_referenced_by_a_fresh_recorded_answer() {
    let f = Fixture::new();
    let first = f.stage(&[1, 2, 3]);
    let second = f.stage(&[7, 7]);
    let thread = "thread-fresh-answer";
    let threads = MockThreads::new(Box::new(move |command| {
        // A fresh commit records exactly the attachments this attempt claimed.
        let value = serde_json::to_value(command).unwrap();
        Ok(DispatchResult {
            sequence: 1,
            stored_events: answered_events(
                thread,
                "request-fresh",
                Some(value["attachmentsByQuestionId"].clone()),
            ),
        })
    }));
    let result = f
        .intake(threads)
        .dispatch_command(respond(
            "answer-fresh",
            thread,
            "request-fresh",
            json!({"q1": ["one"], "q2": ["two"]}),
            json!({"q1": [image(&first, "one.png", 3)], "q2": [image(&second, "two.png", 2)]}),
        ))
        .await;
    assert!(result.is_ok());
    assert_eq!(f.with_prefix("thread-fresh-answer-").len(), 2);
}

#[tokio::test]
async fn a_retried_response_re_claims_the_preserved_pending_uploads() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    let late = create_pending_attachment_id(None);
    let command = respond(
        "answer-retry",
        "thread-retry",
        "request-retry",
        json!({"q1": ["one"], "q2": ["two"]}),
        json!({"q1": [image(&pending, "screen.png", 3)], "q2": [image(&late, "late.png", 2)]}),
    );
    let first_threads = MockThreads::failing();
    assert!(
        f.intake(first_threads.clone())
            .dispatch_command(command.clone())
            .await
            .is_err()
    );
    assert!(first_threads.captured.lock().is_empty());

    std::fs::write(f.path().join(format!("{late}.png")), [7, 7]).unwrap();
    let retry_threads = MockThreads::succeeding(vec![]);
    f.intake(retry_threads.clone())
        .dispatch_command(command)
        .await
        .unwrap();
    let dispatched = captured_respond(&retry_threads);
    for question in ["q1", "q2"] {
        for value in dispatched["attachmentsByQuestionId"][question]
            .as_array()
            .unwrap()
        {
            let attachment: ChatAttachment = serde_json::from_value(value.clone()).unwrap();
            assert!(
                resolve_attachment_path(f.path(), &attachment)
                    .unwrap()
                    .exists()
            );
        }
    }
    assert_eq!(f.with_prefix("thread-retry-").len(), 2);
}

#[tokio::test]
async fn applies_the_image_budget_across_all_questions_before_dispatch() {
    let f = Fixture::new();
    let attachments: Vec<Value> = (0..5)
        .map(|i| {
            image(
                &format!("existing-budget-{i}"),
                &format!("{i}.png"),
                10 * 1024 * 1024,
            )
        })
        .collect();
    let threads = MockThreads::failing();
    let error = f
        .intake(threads.clone())
        .dispatch_command(respond(
            "command-image-budget",
            "thread-image-budget",
            "question-image-budget",
            json!({}),
            json!({"q1": attachments.clone(), "q2": attachments}),
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("80 MiB"));
    assert!(threads.captured.lock().is_empty());
}

#[tokio::test]
async fn releases_claimed_copies_when_a_project_disables_the_messages_provider_instance() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2, 3]);
    let threads = MockThreads::failing();
    *threads.send.lock() = Some(ThreadManagementError::ProviderInstanceDisabled {
        project_id: turin_contracts::ProjectId::new("project-disabled-send").unwrap(),
        instance_id: turin_contracts::provider_instance::ProviderInstanceId::new("codex").unwrap(),
    });
    let result = f
        .intake(threads)
        .send_to_thread(SendInput {
            project_id: turin_contracts::ProjectId::new("project-disabled-send").unwrap(),
            command_id: CommandId::new("command-disabled-send").unwrap(),
            thread_id: turin_contracts::ThreadId::new("thread-disabled-send").unwrap(),
            message_id: turin_contracts::MessageId::new("message-disabled-send").unwrap(),
            scheduled_task_id: None,
            sender_thread_id: None,
            text: "Hello".into(),
            attachments: vec![serde_json::from_value(image(&pending, "screen.png", 3)).unwrap()],
            model_selection: None,
            mode: crate::pipeline::thread_management_service::SendMode::Auto,
            created_by: turin_contracts::orchestration_v2::OrchestrationV2Actor::User,
            creation_source: turin_contracts::orchestration_v2::OrchestrationV2CreationSource::Web,
        })
        .await;
    assert!(result.is_err());
    assert!(f.with_prefix("thread-disabled-send-").is_empty());
    assert!(f.pending_exists(&pending));
}

#[tokio::test]
async fn message_dispatch_claims_uploads_and_remaps_context_bindings() {
    let f = Fixture::new();
    let pending = f.stage(&[1, 2]);
    let threads = MockThreads::succeeding(vec![]);
    let command: OrchestrationV2Command = serde_json::from_value(json!({
        "type": "message.dispatch", "createdBy": "user", "creationSource": "web",
        "commandId": "command-msg", "threadId": "thread-msg", "messageId": "message-msg",
        "text": "look", "attachments": [image(&pending, "shot.png", 2)],
        "context": {"version": 1, "records": [{
            "version": 1, "contextId": "c1", "label": "shot", "kind": "image",
            "attachmentId": pending, "name": "shot.png", "mimeType": "image/png", "sizeBytes": 2}]},
        "dispatchMode": {"type": "start_immediately"},
    }))
    .unwrap();
    f.intake(threads.clone())
        .dispatch_command(command)
        .await
        .unwrap();
    let dispatched = captured_respond(&threads);
    let claimed_id = dispatched["attachments"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(claimed_id.starts_with("thread-msg-"));
    assert_eq!(
        dispatched["context"]["records"][0]["attachmentId"],
        claimed_id
    );
    // No stored `message.updated` references the copy (no events at all), so
    // the intake treats the outcome as unreferenced and releases it.
    assert!(f.with_prefix("thread-msg-").is_empty());
    assert!(f.pending_exists(&pending));
}

#[tokio::test]
async fn commands_without_attachments_pass_straight_through() {
    let f = Fixture::new();
    let threads = MockThreads::succeeding(vec![]);
    let command: OrchestrationV2Command = serde_json::from_value(json!({
        "type": "thread.archive", "commandId": "command-archive", "threadId": "thread-a",
    }))
    .unwrap();
    f.intake(threads.clone())
        .dispatch_command(command)
        .await
        .unwrap();
    assert_eq!(threads.captured.lock().len(), 1);
}
