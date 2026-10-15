//! Behaviour tests for [`limit_recovery_command`] and the sweep. Upstream has
//! no test file for the worker; these cover each guard in the TS function.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Command, OrchestrationV2LimitRecovery, OrchestrationV2ProviderFailureClass,
    OrchestrationV2ServerCommand, OrchestrationV2SettleOverride, OrchestrationV2ShellThreadStatus,
};
use turin_contracts::settings::{ServerSettings, default_server_settings};
use turin_contracts::{CommandId, RunId, ThreadId, UtcDateTime};

use super::*;
use crate::pipeline::seams::ServerSettingsSource;
use crate::projection::store::shell::ProjectionLimitRecoveryCandidate;

const NOW: &str = "2026-10-15T12:00:00.000Z";
const RESET_FUTURE: &str = "2026-10-15T13:00:00.000Z";
const RESET_PAST: &str = "2026-10-15T11:00:00.000Z";

fn utc(value: &str) -> UtcDateTime {
    value.parse().expect("fixture timestamp parses")
}

fn reset_future_ms() -> i64 {
    utc(RESET_FUTURE).0.timestamp_millis()
}

fn now_ms() -> i64 {
    utc(NOW).0.timestamp_millis()
}

fn failed_thread(reset_at: &str) -> ProjectionLimitRecoveryCandidate {
    ProjectionLimitRecoveryCandidate {
        id: ThreadId::new("thread-1").expect("thread id"),
        status: OrchestrationV2ShellThreadStatus::Failed,
        last_error_class: Some(OrchestrationV2ProviderFailureClass::UsageLimit),
        latest_run_id: Some(RunId::new("run-1").expect("run id")),
        usage_limit_reset_at: Some(reset_at.to_owned()),
        archived_at: None,
        settled_override: None,
        pending_runtime_request: None,
        latest_run_completed_at: Some(utc("2026-10-15T11:30:00.000Z")),
        updated_at: utc("2026-10-15T11:31:00.000Z"),
        limit_recovery: None,
        snoozed_until: None,
    }
}

fn recovery(auto_resume: bool, request_id: Option<&str>) -> OrchestrationV2LimitRecovery {
    OrchestrationV2LimitRecovery {
        request_id: request_id.map(|id| CommandId::new(id).expect("command id")),
        run_id: RunId::new("run-1").expect("run id"),
        reset_at: RESET_FUTURE.to_owned(),
        auto_resume,
        snooze: Some(false),
    }
}
fn command_json(command: &OrchestrationV2Command) -> serde_json::Value {
    serde_json::to_value(command).expect("command serializes")
}

#[test]
fn arms_recovery_when_auto_resume_is_on_and_no_choice_is_recorded() {
    let thread = failed_thread(RESET_FUTURE);
    let command = limit_recovery_command(&thread, true, now_ms(), false).expect("arm command");
    let value = command_json(&command);
    assert_eq!(value["type"], "thread.metadata.update");
    assert_eq!(value["commandId"], "limit-arm:thread-1:run-1:1792069200000");
    assert_eq!(value["limitRecovery"]["runId"], "run-1");
    assert_eq!(value["limitRecovery"]["resetAt"], RESET_FUTURE);
    assert_eq!(value["limitRecovery"]["autoResume"], true);
    assert_eq!(value["limitRecovery"]["snooze"], false);
}

#[test]
fn does_not_arm_without_auto_resume_or_snooze() {
    let thread = failed_thread(RESET_FUTURE);
    assert!(limit_recovery_command(&thread, false, now_ms(), false).is_none());
}

#[test]
fn snooze_arms_a_future_reset_with_snooze_set() {
    let thread = failed_thread(RESET_FUTURE);
    let command = limit_recovery_command(&thread, false, now_ms(), true).expect("arm command");
    let value = command_json(&command);
    assert_eq!(value["limitRecovery"]["autoResume"], false);
    assert_eq!(value["limitRecovery"]["snooze"], true);
}

#[test]
fn snooze_does_not_arm_a_reset_that_has_already_passed() {
    let thread = failed_thread(RESET_PAST);
    assert!(limit_recovery_command(&thread, false, now_ms(), true).is_none());
}

#[test]
fn expired_reset_reported_with_a_fresh_failure_is_ignored() {
    // The reset is before the run completed, so the window was already over.
    let thread = failed_thread("2026-10-15T11:10:00.000Z");
    assert!(limit_recovery_command(&thread, true, now_ms(), false).is_none());
}

#[test]
fn guards_reject_non_usage_limit_failures_and_settled_or_archived_threads() {
    let mut other_class = failed_thread(RESET_FUTURE);
    other_class.last_error_class = Some(OrchestrationV2ProviderFailureClass::ProviderError);
    let mut running = failed_thread(RESET_FUTURE);
    running.status = OrchestrationV2ShellThreadStatus::Running;
    let mut settled = failed_thread(RESET_FUTURE);
    settled.settled_override = Some(OrchestrationV2SettleOverride::Settled);
    let mut archived = failed_thread(RESET_FUTURE);
    archived.archived_at = Some(utc(NOW));
    for thread in [other_class, running, settled, archived] {
        assert!(limit_recovery_command(&thread, true, now_ms(), false).is_none());
    }
}

#[test]
fn resumes_once_the_reset_has_passed_and_no_snooze_is_pending() {
    let mut thread = failed_thread(RESET_FUTURE);
    thread.limit_recovery = Some(recovery(true, Some("req-1")));
    // Before the reset nothing is sent.
    assert!(limit_recovery_command(&thread, true, now_ms(), false).is_none());

    let after_reset = reset_future_ms() + 1;
    let command =
        limit_recovery_command(&thread, true, after_reset, false).expect("resume command");
    let value = command_json(&command);
    assert_eq!(value["type"], "message.dispatch");
    assert_eq!(
        value["commandId"],
        "limit-resume:thread-1:run-1:1792069200000:req-1"
    );
    assert_eq!(value["messageId"], value["commandId"]);
    assert_eq!(value["text"], "Continue where you left off.");
    assert_eq!(value["usageLimitContinuationOfRunId"], "run-1");
    assert_eq!(value["usageLimitRecoveryRequestId"], "req-1");
    assert_eq!(
        value["dispatchMode"],
        json!({ "type": "start_immediately" })
    );
}

#[test]
fn legacy_recovery_without_request_id_uses_the_legacy_suffix() {
    let mut thread = failed_thread(RESET_FUTURE);
    thread.limit_recovery = Some(recovery(true, None));
    let command = limit_recovery_command(&thread, true, reset_future_ms() + 1, false)
        .expect("resume command");
    let value = command_json(&command);
    assert_eq!(
        value["commandId"],
        "limit-resume:thread-1:run-1:1792069200000:legacy"
    );
    assert!(value.get("usageLimitRecoveryRequestId").is_none());
}

#[test]
fn a_pending_snooze_blocks_resume_until_it_passes() {
    let mut thread = failed_thread(RESET_FUTURE);
    thread.limit_recovery = Some(recovery(true, Some("req-1")));
    thread.snoozed_until = Some(utc("2026-10-15T14:00:00.000Z"));
    assert!(limit_recovery_command(&thread, true, reset_future_ms() + 1, false).is_none());
}

#[test]
fn a_recorded_choice_without_auto_resume_never_dispatches() {
    let mut thread = failed_thread(RESET_FUTURE);
    thread.limit_recovery = Some(recovery(false, Some("req-1")));
    assert!(limit_recovery_command(&thread, true, reset_future_ms() + 1, false).is_none());
}

#[derive(Default)]
struct FakeCandidates {
    threads: Vec<ProjectionLimitRecoveryCandidate>,
    seen: Mutex<Vec<(bool, bool)>>,
}

#[async_trait]
impl LimitRecoveryCandidateSource for FakeCandidates {
    async fn get_limit_recovery_candidates(
        &self,
        _now: UtcDateTime,
        auto_resume: bool,
        snooze: bool,
    ) -> Result<Vec<ProjectionLimitRecoveryCandidate>, BoxError> {
        self.seen.lock().push((auto_resume, snooze));
        Ok(self.threads.clone())
    }
}

#[derive(Default)]
struct FakeDispatch {
    commands: Mutex<Vec<OrchestrationV2ServerCommand>>,
    fail_thread: Option<String>,
}

#[async_trait]
impl LimitRecoveryDispatch for FakeDispatch {
    async fn dispatch(&self, command: OrchestrationV2ServerCommand) -> Result<(), BoxError> {
        let thread_id = match &command {
            OrchestrationV2ServerCommand::Client(
                OrchestrationV2Command::ThreadMetadataUpdate { thread_id, .. },
            ) => thread_id.as_str().to_owned(),
            _ => String::new(),
        };
        self.commands.lock().push(command);
        if self.fail_thread.as_deref() == Some(thread_id.as_str()) {
            return Err("dispatch rejected".into());
        }
        Ok(())
    }
}

struct FixedSettings(ServerSettings);

#[async_trait]
impl ServerSettingsSource for FixedSettings {
    async fn get_settings(&self) -> Result<ServerSettings, BoxError> {
        Ok(self.0.clone())
    }
}

fn settings(auto_resume: bool, snooze: bool) -> ServerSettings {
    let mut settings = default_server_settings();
    settings.auto_resume_limited_threads = auto_resume;
    settings.snooze_limited_threads = snooze;
    settings
}

#[tokio::test]
async fn sweep_reads_preferences_and_dispatches_only_eligible_threads() {
    let mut other = failed_thread(RESET_FUTURE);
    other.id = ThreadId::new("thread-2").expect("thread id");
    other.last_error_class = Some(OrchestrationV2ProviderFailureClass::ProviderError);
    let candidates = Arc::new(FakeCandidates {
        threads: vec![failed_thread(RESET_FUTURE), other],
        ..Default::default()
    });
    let dispatch = Arc::new(FakeDispatch::default());
    let worker = UsageLimitRecoveryWorker::new(
        candidates.clone(),
        dispatch.clone(),
        Arc::new(FixedSettings(settings(true, false))),
    );

    worker.sweep().await.expect("sweep succeeds");

    assert_eq!(candidates.seen.lock().as_slice(), &[(true, false)]);
    let commands = dispatch.commands.lock();
    assert_eq!(commands.len(), 1);
}

#[tokio::test]
async fn a_failed_dispatch_does_not_stop_the_sweep() {
    let mut second = failed_thread(RESET_FUTURE);
    second.id = ThreadId::new("thread-2").expect("thread id");
    let candidates = Arc::new(FakeCandidates {
        threads: vec![failed_thread(RESET_FUTURE), second],
        ..Default::default()
    });
    let dispatch = Arc::new(FakeDispatch {
        fail_thread: Some("thread-1".to_owned()),
        ..Default::default()
    });
    let worker = UsageLimitRecoveryWorker::new(
        candidates,
        dispatch.clone(),
        Arc::new(FixedSettings(settings(true, false))),
    );

    worker.sweep().await.expect("sweep succeeds");

    assert_eq!(dispatch.commands.lock().len(), 2);
}
