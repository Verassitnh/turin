use super::*;
use crate::pipeline::command_receipt_store::CommandReceiptStore;
use crate::pipeline::hooks_bridge::NoHooks;
use crate::pipeline::orchestrator_port::DispatchResult;
use crate::pipeline::orchestrator_port::ThreadRecords;
use crate::pipeline::test_support::{
    MockOrchestrator, NoSessions, StaticProjects, StaticSettings, projection,
};
use crate::pipeline::thread_command_executor::ThreadCommandExecutor;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use turin_contracts::git::TextGenerationError;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ThreadProjection, OrchestrationV2TitleRegeneration,
};
use turin_persistence::Db;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ReviewGuideGenerationInput, ReviewGuideGenerationResult, ThreadTitleGenerationResult,
};

const PROJECT: &str = "project:title";
const THREAD: &str = "thread:title";

type Respond =
    Box<dyn FnMut(usize) -> Result<ThreadTitleGenerationResult, TextGenerationError> + Send>;

/// Scripted `generateThreadTitle`; the closure sees the 1-based call number.
struct FakeText {
    inputs: Mutex<Vec<ThreadTitleGenerationInput>>,
    respond: Mutex<Respond>,
}

fn unused(operation: &str) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.to_owned(),
        detail: "not used by title regeneration".to_owned(),
        cause: None,
    }
}

#[async_trait]
impl ProviderTextGeneration for FakeText {
    async fn generate_commit_message(
        &self,
        _: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        Err(unused("generateCommitMessage"))
    }

    async fn generate_pr_content(
        &self,
        _: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        Err(unused("generatePrContent"))
    }

    async fn generate_branch_name(
        &self,
        _: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        Err(unused("generateBranchName"))
    }

    async fn generate_thread_title(
        &self,
        input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        let call = {
            let mut inputs = self.inputs.lock();
            inputs.push(input);
            inputs.len()
        };
        let mut respond = self.respond.lock();
        respond(call)
    }

    async fn generate_review_guide(
        &self,
        _: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        Err(unused("generateReviewGuide"))
    }
}

fn titled(title: &str) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
    Ok(ThreadTitleGenerationResult {
        title: title.to_owned(),
        needs_refinement: None,
    })
}

fn failed() -> Result<ThreadTitleGenerationResult, TextGenerationError> {
    Err(TextGenerationError::TextGenerationError {
        operation: "generateThreadTitle".to_owned(),
        detail: "Temporary failure".to_owned(),
        cause: None,
    })
}

fn message(id: &str, role: &str, text: &str, streaming: bool) -> Value {
    json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": id,
        "threadId": THREAD,
        "runId": null,
        "nodeId": null,
        "role": role,
        "text": text,
        "attachments": [],
        "streaming": streaming,
        "createdAt": "2026-06-21T00:00:00.000Z",
        "updatedAt": "2026-06-21T00:00:00.000Z",
    })
}

fn marker(request_id: &str) -> OrchestrationV2TitleRegeneration {
    serde_json::from_value(json!({
        "requestId": request_id,
        "startedAt": "2026-06-21T00:00:00.000Z",
    }))
    .unwrap()
}

/// A thread titled "Seed title" with `request_id` armed and the given messages.
fn seeded_thread(request_id: Option<&str>, messages: Vec<Value>) -> ThreadRecords {
    let mut records: OrchestrationV2ThreadProjection =
        projection(THREAD, PROJECT, json!({ "messages": messages }));
    records.thread.title = turin_contracts::TrimmedNonEmptyString::new("Seed title").unwrap();
    records.thread.title_regeneration = request_id.map(|id| Some(marker(id)));
    records
}

struct Fixture {
    state: Arc<Mutex<ThreadRecords>>,
    orchestrator: Arc<MockOrchestrator>,
    threads: ThreadManagementService,
    projects: StaticProjects,
    settings: Arc<StaticSettings>,
    text: FakeText,
}

async fn fixture(state: Arc<Mutex<ThreadRecords>>, respond: Respond) -> Fixture {
    let reads = state.clone();
    let writes = state.clone();
    let orchestrator =
        Arc::new(MockOrchestrator::default().with_records(move |_| Ok(reads.lock().clone())));
    // Mirrors the projection: a completion clears the marker and lands the title.
    *orchestrator.dispatch_fn.lock() = Box::new(move |command| {
        if let OrchestrationV2ServerCommand::Client(
            OrchestrationV2Command::ThreadTitleRegenerationComplete {
                request_id, title, ..
            },
        ) = &command
        {
            let mut thread = writes.lock();
            let current = thread
                .thread
                .title_regeneration
                .as_ref()
                .and_then(Option::as_ref)
                .map(|marker| marker.request_id.clone());
            if current.as_ref() == Some(request_id) {
                thread.thread.title_regeneration = None;
            }
            if let Some(title) = title {
                thread.thread.title = title.clone();
            }
        }
        Ok(DispatchResult {
            sequence: 1,
            stored_events: vec![],
        })
    });
    let projects = StaticProjects(HashMap::from([(
        ProjectId::new(PROJECT).unwrap(),
        "/repo".to_owned(),
    )]));
    let settings = StaticSettings::default_settings();
    let threads = ThreadManagementService::new(
        ThreadCommandExecutor::new(),
        orchestrator.clone(),
        Arc::new(NoSessions),
        settings.clone(),
        CommandReceiptStore::new(Db::open_in_memory().await.unwrap()),
        Arc::new(StaticProjects(HashMap::from([(
            ProjectId::new(PROJECT).unwrap(),
            "/repo".to_owned(),
        )]))),
        Arc::new(NoHooks),
    );
    Fixture {
        state,
        orchestrator,
        threads,
        projects,
        settings,
        text: FakeText {
            inputs: Mutex::new(vec![]),
            respond: Mutex::new(respond),
        },
    }
}

impl Fixture {
    fn service(&self, retry_base: Duration) -> ThreadTitleRegenerationService<'_> {
        ThreadTitleRegenerationService::new(
            &self.threads,
            &self.projects,
            &*self.settings,
            &self.text,
        )
        .with_retry_base(retry_base)
    }

    fn title(&self) -> String {
        self.state.lock().thread.title.as_str().to_owned()
    }

    fn marker(&self) -> Option<String> {
        self.state
            .lock()
            .thread
            .title_regeneration
            .as_ref()
            .and_then(Option::as_ref)
            .map(|marker| marker.request_id.as_str().to_owned())
    }

    fn calls(&self) -> usize {
        self.text.inputs.lock().len()
    }

    /// The title of each completion dispatched, `None` when it carried none.
    fn completed_titles(&self) -> Vec<Option<String>> {
        self.orchestrator
            .dispatched
            .lock()
            .iter()
            .map(|command| match command {
                OrchestrationV2ServerCommand::Client(
                    OrchestrationV2Command::ThreadTitleRegenerationComplete { title, .. },
                ) => title.as_ref().map(|title| title.as_str().to_owned()),
                other => panic!("unexpected dispatch: {other:?}"),
            })
            .collect()
    }
}

fn regenerate(request_id: &str) -> TitleRegenerationInput {
    TitleRegenerationInput {
        thread_id: ThreadId::new(THREAD).unwrap(),
        request_id: CommandId::new(request_id).unwrap(),
        kind: TitleRegenerationKind::Regenerate,
    }
}

fn initial(request_id: &str, message_id: &str) -> TitleRegenerationInput {
    TitleRegenerationInput {
        thread_id: ThreadId::new(THREAD).unwrap(),
        request_id: CommandId::new(request_id).unwrap(),
        kind: TitleRegenerationKind::Initial {
            message_id: MessageId::new(message_id).unwrap(),
        },
    }
}

#[tokio::test]
async fn superseded_marker_generates_nothing() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:current"),
        vec![],
    )));
    let fixture = fixture(state, Box::new(|_| titled("Unused"))).await;

    fixture
        .service(Duration::from_millis(1))
        .execute(regenerate("command:title:stale"))
        .await
        .unwrap();

    assert_eq!(fixture.calls(), 0);
    assert!(fixture.completed_titles().is_empty());
    assert_eq!(fixture.marker().as_deref(), Some("command:title:current"));
    assert_eq!(fixture.title(), "Seed title");
}

#[tokio::test]
async fn regenerated_title_lands_from_the_conversation_digest() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:landing"),
        vec![message(
            "message:landing",
            "user",
            "Investigate the flaky login test",
            false,
        )],
    )));
    let fixture = fixture(state, Box::new(|_| titled("Fresh title"))).await;

    fixture
        .service(Duration::from_millis(1))
        .execute(regenerate("command:title:landing"))
        .await
        .unwrap();

    let input = &fixture.text.inputs.lock()[0];
    assert_eq!(input.previous_title.as_deref(), Some("Seed title"));
    assert_eq!(input.cwd, "/repo");
    assert!(
        input
            .message
            .contains("USER:\nInvestigate the flaky login test")
    );
    assert_eq!(
        fixture.completed_titles(),
        vec![Some("Fresh title".to_owned())]
    );
    assert_eq!(fixture.title(), "Fresh title");
    assert_eq!(fixture.marker(), None);
}

#[tokio::test]
async fn fallback_title_completes_without_replacing_the_current_one() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:fallback"),
        vec![message(
            "message:fallback",
            "user",
            "Some conversation",
            false,
        )],
    )));
    let fixture = fixture(state, Box::new(|_| titled("New thread"))).await;

    fixture
        .service(Duration::from_millis(1))
        .execute(regenerate("command:title:fallback"))
        .await
        .unwrap();

    assert_eq!(fixture.completed_titles(), vec![None]);
    assert_eq!(fixture.title(), "Seed title");
    assert_eq!(fixture.marker(), None);
}

#[tokio::test]
async fn regeneration_reproducing_the_current_title_completes_without_it() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:unchanged"),
        vec![message(
            "message:unchanged",
            "user",
            "Some conversation",
            false,
        )],
    )));
    let fixture = fixture(state, Box::new(|_| titled("Seed title"))).await;

    fixture
        .service(Duration::from_millis(1))
        .execute(regenerate("command:title:unchanged"))
        .await
        .unwrap();

    assert_eq!(fixture.completed_titles(), vec![None]);
    assert_eq!(fixture.title(), "Seed title");
}

#[tokio::test]
async fn failed_regeneration_is_not_retried_and_keeps_the_title() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:failure"),
        vec![message(
            "message:failure",
            "user",
            "Some conversation",
            false,
        )],
    )));
    let fixture = fixture(state, Box::new(|_| failed())).await;

    fixture
        .service(Duration::from_millis(1))
        .execute(regenerate("command:title:failure"))
        .await
        .unwrap();

    assert_eq!(fixture.calls(), 1);
    assert_eq!(fixture.completed_titles(), vec![None]);
    assert_eq!(fixture.title(), "Seed title");
    assert_eq!(fixture.marker(), None);
}

#[tokio::test]
async fn initial_title_completes_without_generating_when_its_message_is_missing() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:missing"),
        vec![],
    )));
    let fixture = fixture(state, Box::new(|_| titled("Unused"))).await;

    fixture
        .service(Duration::from_millis(1))
        .execute(initial("command:title:missing", "message:missing"))
        .await
        .unwrap();

    assert_eq!(fixture.calls(), 0);
    assert_eq!(fixture.completed_titles(), vec![None]);
    assert_eq!(fixture.title(), "Seed title");
}

#[tokio::test]
async fn initial_title_recovers_on_the_third_attempt() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:recover"),
        vec![message("message:recover", "user", "Fix the title", false)],
    )));
    let fixture = fixture(
        state,
        Box::new(|call| {
            if call == 3 {
                titled("Recovered title")
            } else {
                failed()
            }
        }),
    )
    .await;

    fixture
        .service(Duration::from_millis(1))
        .execute(initial("command:title:recover", "message:recover"))
        .await
        .unwrap();

    assert_eq!(fixture.calls(), 3);
    assert_eq!(
        fixture.completed_titles(),
        vec![Some("Recovered title".to_owned())]
    );
    assert_eq!(fixture.title(), "Recovered title");
}

#[tokio::test]
async fn initial_title_gives_up_after_three_attempts() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:exhausted"),
        vec![message("message:exhausted", "user", "Fix the title", false)],
    )));
    let fixture = fixture(state, Box::new(|_| failed())).await;

    fixture
        .service(Duration::from_millis(1))
        .execute(initial("command:title:exhausted", "message:exhausted"))
        .await
        .unwrap();

    assert_eq!(fixture.calls(), 3);
    assert_eq!(fixture.completed_titles(), vec![None]);
    assert_eq!(fixture.title(), "Seed title");
    assert_eq!(fixture.marker(), None);
}

#[tokio::test]
async fn manual_rename_during_a_failed_attempt_stops_the_retries() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:stale"),
        vec![message("message:stale", "user", "Fix the title", false)],
    )));
    let rename = state.clone();
    let fixture = fixture(
        state,
        Box::new(move |_| {
            // A manual rename clears the marker while the first attempt runs.
            let mut thread = rename.lock();
            thread.thread.title =
                turin_contracts::TrimmedNonEmptyString::new("Manual title").unwrap();
            thread.thread.title_regeneration = None;
            failed()
        }),
    )
    .await;

    fixture
        .service(Duration::from_millis(1))
        .execute(initial("command:title:stale", "message:stale"))
        .await
        .unwrap();

    assert_eq!(fixture.calls(), 1);
    assert!(fixture.completed_titles().is_empty());
    assert_eq!(fixture.title(), "Manual title");
}

#[tokio::test]
async fn cancelled_initial_title_keeps_the_marker_for_its_retry() {
    let state = Arc::new(Mutex::new(seeded_thread(
        Some("command:title:cancelled"),
        vec![message("message:cancelled", "user", "Fix the title", false)],
    )));
    let fixture = fixture(state, Box::new(|_| failed())).await;

    // The first attempt fails, then the one-hour backoff holds the call open
    // until the timeout drops it, the same as an interrupt.
    let outcome = tokio::time::timeout(
        Duration::from_millis(500),
        fixture
            .service(Duration::from_secs(3600))
            .execute(initial("command:title:cancelled", "message:cancelled")),
    )
    .await;

    assert!(outcome.is_err());
    assert_eq!(fixture.calls(), 1);
    assert!(fixture.completed_titles().is_empty());
    assert_eq!(fixture.marker().as_deref(), Some("command:title:cancelled"));
}
