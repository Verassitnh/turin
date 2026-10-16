//! How a root run settles: superseded attempts, the hard-stop interrupt
//! result, the ownership-gated write, and the pull request refresh.
//! Upstream: `RunExecutionService.test.ts`, `captureRootRunTermination`.

use std::sync::Arc;

use serde_json::Value;
use turin_contracts::orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2TurnItem};
use turin_provider_core::{
    id_allocator::derive,
    provider_adapter::{ProviderAdapterErrorKind, ProviderAdapterV2Error, ProviderAdapterV2Event},
};

use super::{
    AsyncBool,
    test_support::{
        Fixture, Recorder, ScriptedSession, default_service, ev, scripted_bool, turn_start_error,
    },
};

#[derive(Default)]
struct Termination {
    should_finalize: Option<Result<bool, &'static str>>,
    reject_terminal_write: bool,
    unpaired: Option<bool>,
    seed_open_subagent: bool,
    events: Option<Vec<ProviderAdapterV2Event>>,
    stream_error: Option<ProviderAdapterV2Error>,
    start_error: Option<ProviderAdapterV2Error>,
    refresh_fails: bool,
}

struct Captured {
    recorder: Arc<Recorder>,
}

impl Captured {
    fn observed(&self) -> Vec<String> {
        self.recorder.state.lock().observed.clone()
    }

    /// Turn items written by the terminal, in order.
    fn written(&self) -> Vec<OrchestrationV2TurnItem> {
        self.recorder
            .events()
            .into_iter()
            .filter_map(|event| match event {
                OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } => Some(payload),
                _ => None,
            })
            .collect()
    }

    fn written_types(&self) -> Vec<String> {
        self.written()
            .iter()
            .map(|item| {
                serde_json::to_value(item).expect("item")["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /// Effect types of every terminal write attempted, committed or not.
    fn submitted_effects(&self) -> Vec<String> {
        let state = self.recorder.state.lock();
        state
            .writes
            .iter()
            .map(|write| &write.effects)
            .chain(state.guarded.iter().map(|write| &write.write.effects))
            .flatten()
            .map(|effect| {
                serde_json::to_value(&effect.request).expect("effect")["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }
}

fn root_terminal(fixture: &Fixture, status: &str) -> ProviderAdapterV2Event {
    ev::terminal(
        &fixture.provider_thread_id(),
        &format!("provider-turn:{}", fixture.key),
        status,
    )
}

fn probe(answer: Result<bool, &'static str>) -> AsyncBool {
    scripted_bool(vec![answer]).0
}

async fn capture(key: &str, options: Termination) -> Captured {
    let recorder = Recorder::new();
    {
        let mut state = recorder.state.lock();
        state.reject_guarded = options.reject_terminal_write;
        state.refresh_fails = options.refresh_fails;
    }
    let fixture = Fixture::new(key);
    let session = match (options.stream_error, options.events) {
        (Some(error), _) => ScriptedSession::erroring(error),
        (None, Some(events)) => ScriptedSession::scripted(events, false),
        (None, None) if options.start_error.is_some() => ScriptedSession::quiet(),
        (None, None) => {
            let mut events = Vec::new();
            if options.seed_open_subagent {
                let thread = fixture.thread_id();
                events.push(ev::subagent(ev::subagent_json(
                    &format!("node:{key}:subagent"),
                    &thread,
                    Some(&fixture.run_id()),
                    "provider_native",
                    Some(&format!("{thread}:child")),
                    "running",
                )));
                events.push(ev::node(
                    &format!("node:{key}:subagent"),
                    &thread,
                    Some(&fixture.run_id()),
                    "subagent",
                    "running",
                ));
                events.push(ev::turn_item(
                    &thread,
                    &format!("turn-item:{key}"),
                    Some(&fixture.run_id()),
                    Some(&format!("provider-turn:{key}")),
                    1,
                    "subagent",
                    "running",
                ));
            }
            events.push(root_terminal(&fixture, "interrupted"));
            ScriptedSession::scripted(events, false)
        }
    };
    if let Some(error) = options.start_error {
        *session.start_error.lock() = Some(error);
    }
    let mut input = fixture.input(session.clone(), "interrupt projection");
    input.attempt.provider_turn_id = Some(
        turin_contracts::ProviderTurnId::new(format!("provider-turn:{key}")).expect("turn id"),
    );
    input.root_node.provider_turn_id = input.attempt.provider_turn_id.clone();
    input.should_finalize_run = options.should_finalize.map(probe);
    input.has_unpaired_run_interrupt_request = options.unpaired.map(|answer| probe(Ok(answer)));
    default_service(&recorder)
        .start_root_run(input)
        .await
        .expect("start");
    if session
        .subscribable
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        session.closed.wait().await;
    } else if session.start_error.lock().is_none() && !session.starts.lock().is_empty() {
        // Nothing to wait for: a started turn on a quiet stream.
    }
    Captured { recorder }
}

fn strings(labels: &[&str]) -> Vec<String> {
    labels.iter().map(|label| (*label).to_owned()).collect()
}

#[tokio::test]
async fn omits_interrupt_results_and_subagent_cascade_for_a_superseded_attempt() {
    let captured = capture(
        "steer-supersede",
        Termination {
            should_finalize: Some(Ok(false)),
            seed_open_subagent: true,
            ..Termination::default()
        },
    )
    .await;
    assert!(captured.written().is_empty());
    assert!(captured.observed().is_empty());
}

#[tokio::test]
async fn emits_run_interrupt_result_when_a_superseded_attempt_still_has_a_hard_stop_request() {
    let captured = capture(
        "stop-then-steer-supersede",
        Termination {
            should_finalize: Some(Ok(false)),
            unpaired: Some(true),
            ..Termination::default()
        },
    )
    .await;
    assert_eq!(captured.written_types(), strings(&["run_interrupt_result"]));
    assert_eq!(captured.observed(), strings(&["pull-requests-refreshed"]));
    let expected_request_id = derive::run_signal_turn_item(
        &turin_contracts::RunId::new("run:stop-then-steer-supersede").expect("run id"),
        "interrupt-request",
    );
    let wire: Value = serde_json::to_value(&captured.written()[0]).expect("item");
    assert_eq!(wire["parentItemId"], expected_request_id.as_str());
}

#[tokio::test]
async fn omits_run_interrupt_result_when_a_superseded_attempts_request_is_already_paired() {
    let captured = capture(
        "stop-then-steer-already-paired",
        Termination {
            should_finalize: Some(Ok(false)),
            unpaired: Some(false),
            ..Termination::default()
        },
    )
    .await;
    assert!(captured.written().is_empty());
    assert!(captured.observed().is_empty());
}

#[tokio::test]
async fn does_not_overwrite_stop_when_ownership_changes_after_the_finalization_read() {
    let captured = capture(
        "stop-wins-finalization-gap",
        Termination {
            should_finalize: Some(Ok(true)),
            reject_terminal_write: true,
            ..Termination::default()
        },
    )
    .await;
    assert!(captured.written().is_empty());
    assert!(captured.observed().is_empty());
    // The capture rides the rejected write, so it never commits.
    assert_eq!(
        captured.submitted_effects(),
        strings(&["checkpoint.capture"])
    );
    assert!(captured.recorder.events().is_empty());
}

#[tokio::test]
async fn emits_run_interrupt_result_when_hard_stop_finalizes_the_active_attempt() {
    let captured = capture(
        "hard-stop",
        Termination {
            should_finalize: Some(Ok(true)),
            ..Termination::default()
        },
    )
    .await;
    assert_eq!(captured.written_types(), strings(&["run_interrupt_result"]));
    assert_eq!(
        captured.observed(),
        strings(&["run:interrupted", "pull-requests-refreshed"])
    );
    assert_eq!(
        captured.submitted_effects(),
        strings(&["checkpoint.capture"])
    );
}

async fn refreshes_after(status: &str) {
    let key = format!("pull-request-refresh:{status}");
    let fixture = Fixture::new(&key);
    let captured = capture(
        &key,
        Termination {
            should_finalize: Some(Ok(true)),
            events: Some(vec![root_terminal(&fixture, status)]),
            ..Termination::default()
        },
    )
    .await;
    let settled = if status == "completed" {
        "waiting"
    } else {
        status
    };
    assert_eq!(
        captured.observed(),
        vec![
            format!("run:{settled}"),
            "pull-requests-refreshed".to_owned()
        ]
    );
    // Only a run that ends in a state with a checkpoint to take captures one.
    let expected: &[&str] = if status == "failed" {
        &[]
    } else {
        &["checkpoint.capture"]
    };
    assert_eq!(captured.submitted_effects(), strings(expected));
}

#[tokio::test]
async fn refreshes_pull_requests_after_the_current_root_run_completed() {
    refreshes_after("completed").await;
}

#[tokio::test]
async fn refreshes_pull_requests_after_the_current_root_run_interrupted() {
    refreshes_after("interrupted").await;
}

#[tokio::test]
async fn refreshes_pull_requests_after_the_current_root_run_cancelled() {
    refreshes_after("cancelled").await;
}

#[tokio::test]
async fn refreshes_pull_requests_after_the_current_root_run_failed() {
    refreshes_after("failed").await;
}

#[tokio::test]
async fn records_a_finished_run_as_failed_when_its_ownership_check_cannot_be_read() {
    let fixture = Fixture::new("finalize-guard-read-failure");
    let captured = capture(
        "finalize-guard-read-failure",
        Termination {
            should_finalize: Some(Err("database unavailable")),
            events: Some(vec![root_terminal(&fixture, "completed")]),
            ..Termination::default()
        },
    )
    .await;
    // The fallback settles through the guarded write instead of the same
    // failing read, so the run does not stay running.
    assert!(captured.observed().contains(&"run:failed".to_owned()));
}

#[tokio::test]
async fn does_not_refresh_pull_requests_for_auxiliary_or_stale_provider_terminals() {
    let fixture = Fixture::new("pull-request-refresh:auxiliary");
    let captured = capture(
        "pull-request-refresh:auxiliary",
        Termination {
            should_finalize: Some(Ok(true)),
            events: Some(vec![
                ev::terminal(&fixture.provider_thread_id(), "turn:child", "completed"),
                ev::terminal(
                    &fixture.provider_thread_id(),
                    "turn:previous",
                    "interrupted",
                ),
            ]),
            ..Termination::default()
        },
    )
    .await;
    assert!(captured.observed().is_empty());
}

#[tokio::test]
async fn refreshes_pull_requests_after_a_provider_stream_exits_with_an_error() {
    let captured = capture(
        "pull-request-refresh:stream-error",
        Termination {
            should_finalize: Some(Ok(true)),
            stream_error: Some(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::EventStream {
                    driver: turin_contracts::provider_instance::ProviderDriverKind::new("codex")
                        .expect("driver"),
                    provider_session_id: turin_contracts::ProviderSessionId::new("session:exited")
                        .expect("session id"),
                },
            )),
            ..Termination::default()
        },
    )
    .await;
    assert_eq!(
        captured.observed(),
        strings(&["run:failed", "pull-requests-refreshed"])
    );
    assert_eq!(captured.written_types(), strings(&["error"]));
    let wire: Value = serde_json::to_value(&captured.written()[0]).expect("item");
    assert!(
        wire["failure"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("provider event stream closed unexpectedly"),
        "{wire}"
    );
}

#[tokio::test]
async fn refreshes_pull_requests_only_once_when_startup_failure_closes_its_event_stream() {
    let captured = capture(
        "pull-request-refresh:startup-error",
        Termination {
            should_finalize: Some(Ok(true)),
            start_error: Some(turn_start_error("pull-request-refresh:startup-error")),
            ..Termination::default()
        },
    )
    .await;
    let observed = captured.observed();
    assert_eq!(
        observed
            .iter()
            .filter(|item| *item == "pull-requests-refreshed")
            .count(),
        1
    );
    assert_eq!(observed[0], "run:failed");
    let wire: Value = serde_json::to_value(&captured.written()[0]).expect("item");
    assert!(
        wire["failure"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("provider could not start this turn"),
        "{wire}"
    );
}

#[tokio::test]
async fn keeps_completed_runs_completed_when_pull_request_refresh_fails() {
    let fixture = Fixture::new("pull-request-refresh:refresh-failure");
    let captured = capture(
        "pull-request-refresh:refresh-failure",
        Termination {
            should_finalize: Some(Ok(true)),
            events: Some(vec![root_terminal(&fixture, "completed")]),
            refresh_fails: true,
            ..Termination::default()
        },
    )
    .await;
    assert_eq!(
        captured.observed(),
        strings(&["run:waiting", "pull-requests-refreshed"])
    );
}
