//! `startRootRun` before the provider event loop: preparation, the ownership
//! checks around the provider call, compaction routing, and start failures.
//! Upstream: `RunExecutionService.test.ts` cases from "rechecks run ownership"
//! to "handles %s before the provider turn starts".
//!
//! Not ported: "passes the thread's MCP app context to the provider under a
//! safe key" and "refreshes MCP credential liveness before calling the
//! provider". The MCP apps host and its credential registry are on the cut
//! list, so `startRootRun` has nothing to read or touch. The upstream
//! `interruption` scenario (settings read ends in `Effect.interrupt`) has no
//! Rust equivalent: dropping the future is the only interruption.

use std::sync::{Arc, atomic::Ordering};

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2RunStatus;

use super::test_support::{
    FailingSettings, Fixture, Recorder, ScriptedSession, default_service, scripted_bool, service,
    turn_start_error,
};
use crate::pipeline::{seams::ServerSettingsSource, test_support::StaticSettings};

fn wire(event: &turin_contracts::orchestration_v2::OrchestrationV2DomainEvent) -> Value {
    serde_json::to_value(event).expect("event serializes")
}

fn of_type(recorder: &Recorder, kind: &str) -> Vec<Value> {
    recorder
        .events()
        .iter()
        .map(wire)
        .filter(|event| event["type"] == kind)
        .collect()
}

#[tokio::test]
async fn rechecks_run_ownership_immediately_before_calling_the_provider() {
    let recorder = Recorder::new();
    let session = ScriptedSession::quiet();
    let fixture = Fixture::new("start-guard");
    let mut input = fixture.input(session.clone(), "Do not start after ownership changes.");
    let (guard, calls) = scripted_bool(vec![Ok(true), Ok(false)]);
    input.should_start_provider_turn = Some(guard);

    default_service(&recorder)
        .start_root_run(input)
        .await
        .expect("a declined start is a normal exit");

    assert_eq!(*calls.lock(), 2);
    assert!(session.starts.lock().is_empty());
    assert!(recorder.events().is_empty());
}

#[tokio::test]
async fn fails_the_run_when_its_ownership_check_cannot_be_read_before_calling_the_provider() {
    let recorder = Recorder::new();
    let session = ScriptedSession::quiet();
    let fixture = Fixture::new("start-guard-read");
    let mut input = fixture.input(session.clone(), "Start while the store is down.");
    // The preparation check passes; the check right before the provider call
    // cannot read the run.
    let (guard, calls) = scripted_bool(vec![Ok(true), Err("database unavailable")]);
    input.should_start_provider_turn = Some(guard);
    // The failure is settled by the guarded write, not by another read.
    let (finalize, finalize_calls) = scripted_bool(vec![Err("database unavailable")]);
    input.should_finalize_run = Some(finalize);

    default_service(&recorder)
        .start_root_run(input)
        .await
        .expect("the failure is recorded, not returned");

    assert_eq!(*calls.lock(), 2);
    assert_eq!(*finalize_calls.lock(), 0);
    assert!(session.starts.lock().is_empty());
    let runs = of_type(&recorder, "run.updated");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["payload"]["status"], "failed");
    // The write is gated on the attempt still running.
    let state = recorder.state.lock();
    assert_eq!(state.guarded.len(), 1);
    assert_eq!(
        state.guarded[0].expected_status,
        OrchestrationV2RunStatus::Running
    );
    assert_eq!(
        state.guarded[0].active_attempt_id.as_str(),
        "attempt:start-guard-read"
    );
}

#[tokio::test]
async fn dispatches_only_attachment_free_compact_commands_through_native_compaction() {
    let attachment = json!({
        "type": "file", "id": "compact-file-12345678-1234-1234-1234-123456789abc",
        "name": "notes.txt", "mimeType": "text/plain", "sizeBytes": 1,
    });
    let cases = [
        (" /compact ", false, "compact"),
        ("/compact", true, "prompt"),
        ("Continue the work", false, "prompt"),
        // JS `trim` strips U+FEFF but keeps U+0085.
        ("\u{feff}/compact\u{feff}", false, "compact"),
        ("\u{85}/compact", false, "prompt"),
    ];
    for (index, (text, with_attachment, expected)) in cases.into_iter().enumerate() {
        let recorder = Recorder::new();
        let session = ScriptedSession::quiet();
        let fixture = Fixture::new(&format!("compact-routing:{index}"));
        let mut input = fixture.input(session.clone(), text);
        if with_attachment {
            input.message.attachments =
                vec![serde_json::from_value(attachment.clone()).expect("attachment")];
        }
        default_service(&recorder)
            .start_root_run(input)
            .await
            .expect("start");
        let prompts = session.starts.lock().len();
        let compacts = session.compacts.lock().len();
        let actual = match (prompts, compacts) {
            (1, 0) => "prompt",
            (0, 1) => "compact",
            other => panic!("case {index} called the provider {other:?}"),
        };
        assert_eq!(actual, expected, "case {index}: {text:?}");
    }
}

#[tokio::test]
async fn fails_a_compact_command_when_the_provider_cannot_compact() {
    let recorder = Recorder::new();
    let session = ScriptedSession::quiet();
    session.supports_compact.store(false, Ordering::SeqCst);
    let fixture = Fixture::new("compact-unsupported");
    default_service(&recorder)
        .start_root_run(fixture.input(session.clone(), "/compact"))
        .await
        .expect("the failure is recorded, not returned");

    assert!(session.starts.lock().is_empty());
    let runs = of_type(&recorder, "run.updated");
    assert_eq!(runs[0]["payload"]["status"], "failed");
    let items = of_type(&recorder, "turn-item.updated");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["payload"]["type"], "error");
    assert_eq!(items[0]["payload"]["failure"]["class"], "provider_error");
}

#[tokio::test]
async fn starts_the_provider_when_checkpoint_baseline_capture_fails() {
    let recorder = Recorder::new();
    recorder.state.lock().baseline_fails = true;
    let session = ScriptedSession::quiet();
    let fixture = Fixture::new("baseline-failure");

    default_service(&recorder)
        .start_root_run(fixture.input(session.clone(), "Start the provider without a baseline."))
        .await
        .expect("start");

    assert_eq!(session.starts.lock().len(), 1);
    // Baseline ordinal is the run's ordinal less one.
    assert_eq!(recorder.state.lock().baseline_calls, vec![0]);
    // No failed run, attempt or node, and no error item.
    assert!(recorder.events().is_empty());
}

#[derive(Clone, Copy)]
enum Scenario {
    Failure,
    StaleAttempt,
    StartGuard,
}

async fn before_the_provider_turn(scenario: Scenario) {
    let recorder = Recorder::new();
    let session = ScriptedSession::quiet();
    let fixture = Fixture::new("settings-failure");
    let mut input = fixture.input(session.clone(), "Start after settings fail.");
    let settings: Arc<dyn ServerSettingsSource> = match scenario {
        Scenario::StartGuard => {
            // A declined start is a normal exit, not a preparation failure.
            let (decline, _) = scripted_bool(vec![Ok(false)]);
            input.should_start_provider_turn = Some(decline);
            StaticSettings::default_settings()
        }
        Scenario::Failure | Scenario::StaleAttempt => Arc::new(FailingSettings),
    };
    if matches!(scenario, Scenario::StaleAttempt) {
        recorder.state.lock().reject_guarded = true;
    }

    service(&recorder, settings)
        .start_root_run(input)
        .await
        .expect("start");

    assert!(session.starts.lock().is_empty());
    let (guarded, refreshes) = {
        let state = recorder.state.lock();
        (state.guarded.len(), state.refreshes)
    };
    match scenario {
        Scenario::StartGuard => {
            assert_eq!(guarded, 0);
            assert_eq!(refreshes, 0);
            assert!(recorder.events().is_empty());
        }
        Scenario::StaleAttempt => {
            assert_eq!(guarded, 1);
            assert_eq!(refreshes, 0);
        }
        Scenario::Failure => {
            assert_eq!(guarded, 1);
            assert_eq!(refreshes, 1);
            {
                let state = recorder.state.lock();
                let write = &state.guarded[0];
                assert_eq!(write.thread_id.as_str(), "thread:settings-failure");
                assert_eq!(write.run_id.as_str(), "run:settings-failure");
                assert_eq!(write.active_attempt_id.as_str(), "attempt:settings-failure");
            }
            let statuses: Vec<Value> = recorder
                .events()
                .iter()
                .map(wire)
                .filter(|event| {
                    ["run.updated", "run-attempt.updated", "node.updated"]
                        .contains(&event["type"].as_str().unwrap_or_default())
                })
                .map(|event| event["payload"]["status"].clone())
                .collect();
            assert_eq!(statuses, vec!["failed", "failed", "failed"]);
            let items = of_type(&recorder, "turn-item.updated");
            assert_eq!(items[0]["payload"]["type"], "error");
            // The persisted item carries a bounded curated message; the exact
            // underlying text stays in the logged cause.
            assert_eq!(
                items[0]["payload"]["failure"]["message"],
                "Run preparation failed."
            );
        }
    }
}

#[tokio::test]
async fn handles_failure_before_the_provider_turn_starts() {
    before_the_provider_turn(Scenario::Failure).await;
}

#[tokio::test]
async fn handles_stale_attempt_before_the_provider_turn_starts() {
    before_the_provider_turn(Scenario::StaleAttempt).await;
}

#[tokio::test]
async fn handles_start_guard_before_the_provider_turn_starts() {
    before_the_provider_turn(Scenario::StartGuard).await;
}

#[tokio::test]
async fn records_a_failed_run_when_the_provider_rejects_the_turn() {
    let recorder = Recorder::new();
    let session = ScriptedSession::quiet();
    *session.start_error.lock() = Some(turn_start_error("provider-rejects"));
    let fixture = Fixture::new("provider-rejects");

    default_service(&recorder)
        .start_root_run(fixture.input(session.clone(), "Hello"))
        .await
        .expect("the failure is recorded, not returned");

    assert_eq!(session.starts.lock().len(), 1);
    {
        let state = recorder.state.lock();
        assert_eq!(state.guarded.len(), 1);
        assert_eq!(state.refreshes, 1);
        // A failed run captures no checkpoint.
        assert!(state.guarded[0].write.effects.is_empty());
    }
    let items = of_type(&recorder, "turn-item.updated");
    assert_eq!(items[0]["payload"]["failure"]["class"], "provider_error");
    // No native turn yet, so the id derives from the attempt. The ordinal
    // continues after the turn's own range.
    assert_eq!(
        items[0]["payload"]["providerTurnId"],
        "provider-turn:provider:codex:native-turn:failed%3Aattempt%3Aprovider-rejects"
    );
    assert_eq!(items[0]["payload"]["ordinal"], 101);
}
