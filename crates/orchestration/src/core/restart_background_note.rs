//! Port of upstream/apps/server/src/orchestration-v2/RestartBackgroundNote.ts.
//!
//! Restart recovery cancels background work (shells, subagents, monitors,
//! provider tasks) and records it on the run. This module describes that work
//! for the provider, decides which run continues with the note, and derives
//! the note a provider thread is still owed.

use std::collections::HashSet;

use turin_contracts::{
    MessageId, ProviderThreadId, RunAttemptId, RunId,
    base_schemas::{TrimmedNonEmptyString, js_trim},
    orchestration_v2::{
        OrchestrationV2PendingBackgroundTask, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2RestartCancelledBackgroundWork as Work,
        OrchestrationV2RestartCancelledBackgroundWorkKind as WorkKind, OrchestrationV2Run,
        OrchestrationV2RunStatus, OrchestrationV2TurnItem,
    },
};

use crate::projection::shared::thread_error::run_ran_after;

const MAX_LABEL_LENGTH: usize = 160;
const MAX_NOTE_ENTRIES: usize = 10;

/// `compactLabel`: collapses whitespace and truncates with an ellipsis.
/// Upstream counts UTF-16 units; this counts scalar values (see PORTING.md).
fn compact_label(value: Option<&str>) -> Option<String> {
    let collapsed = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = js_trim(&collapsed);
    if text.is_empty() {
        return None;
    }
    if text.chars().count() > MAX_LABEL_LENGTH {
        let kept: String = text.chars().take(MAX_LABEL_LENGTH - 1).collect();
        return Some(format!("{kept}…"));
    }
    Some(text.to_owned())
}

/// Every label path ends in a non-empty fallback, so the trim cannot fail.
fn label(text: impl AsRef<str>) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(text).expect("restart work label is non-empty")
}

/// `cancelledTurnItemWork`: describes a background-capable turn item that restart recovery cancels.
pub fn cancelled_turn_item_work(item: &OrchestrationV2TurnItem) -> Option<Work> {
    match item {
        OrchestrationV2TurnItem::Subagent {
            id, title, prompt, ..
        } => Some(Work {
            kind: WorkKind::Subagent,
            label: label(
                compact_label(title.as_deref())
                    .or_else(|| compact_label(Some(prompt.as_str())))
                    .unwrap_or_else(|| "subagent".to_owned()),
            ),
            id: TrimmedNonEmptyString::new(id.as_str()).ok(),
        }),
        OrchestrationV2TurnItem::CommandExecution {
            id, title, input, ..
        } => Some(Work {
            kind: WorkKind::Shell,
            label: label(
                compact_label(Some(input.as_str()))
                    .or_else(|| compact_label(title.as_deref()))
                    .unwrap_or_else(|| "background command".to_owned()),
            ),
            id: TrimmedNonEmptyString::new(id.as_str()).ok(),
        }),
        OrchestrationV2TurnItem::DynamicTool {
            id,
            title,
            tool_name,
            input,
            ..
        } => {
            let monitor = input.get("persistent").and_then(|value| value.as_bool()) == Some(true);
            Some(Work {
                kind: if monitor {
                    WorkKind::Monitor
                } else {
                    WorkKind::Task
                },
                label: label(
                    compact_label(title.as_deref())
                        .or_else(|| {
                            tool_name
                                .as_ref()
                                .and_then(|name| compact_label(Some(name.as_str())))
                        })
                        .unwrap_or_else(|| "background tool".to_owned()),
                ),
                id: TrimmedNonEmptyString::new(id.as_str()).ok(),
            })
        }
        _ => None,
    }
}

/// `restartWorkKind`.
fn restart_work_kind(task: &OrchestrationV2PendingBackgroundTask) -> WorkKind {
    match task {
        OrchestrationV2PendingBackgroundTask::Command { .. } => WorkKind::Shell,
        OrchestrationV2PendingBackgroundTask::Subagent { .. } => WorkKind::Subagent,
        OrchestrationV2PendingBackgroundTask::Monitor { .. } => WorkKind::Monitor,
        OrchestrationV2PendingBackgroundTask::BackgroundTask { .. } => WorkKind::Task,
    }
}

/// `cancelledRosterTaskWork`: describes a provider-reported background task (the provider-thread roster).
pub fn cancelled_roster_task_work(task: &OrchestrationV2PendingBackgroundTask) -> Work {
    let (task_id, description) = match task {
        OrchestrationV2PendingBackgroundTask::Subagent {
            task_id,
            description,
            ..
        }
        | OrchestrationV2PendingBackgroundTask::Command {
            task_id,
            description,
        }
        | OrchestrationV2PendingBackgroundTask::Monitor {
            task_id,
            description,
        }
        | OrchestrationV2PendingBackgroundTask::BackgroundTask {
            task_id,
            description,
        } => (task_id, description),
    };
    let description = compact_label(description.as_ref().map(|value| value.as_str()));
    let text = match &description {
        None => task_id.as_str().to_owned(),
        Some(description) => format!("{description} (id {})", task_id.as_str()),
    };
    Work {
        kind: restart_work_kind(task),
        label: label(compact_label(Some(&text)).unwrap_or_else(|| "background task".to_owned())),
        id: Some(task_id.clone()),
    }
}

/// `restartCancelledBackgroundWorkNote`: provider-facing text for work the
/// model still expects to hear back from. Bounded so it cannot crowd out the
/// turn's context.
pub fn restart_cancelled_background_work_note(work: &[Work]) -> String {
    let mut lines = vec![
        "Note: the T3 server restarted, and this background work was cancelled before it finished. It will not report back:"
            .to_owned(),
    ];
    for entry in work.iter().take(MAX_NOTE_ENTRIES) {
        lines.push(format!(
            "- {}: {}",
            kind_name(entry.kind),
            entry.label.as_str()
        ));
    }
    let omitted = work.len().saturating_sub(MAX_NOTE_ENTRIES);
    if omitted > 0 {
        lines.push(format!("- and {omitted} more"));
    }
    lines.join("\n")
}

/// The wire spelling of a work kind (`WorkKind`'s `lowercase` serde name).
fn kind_name(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Subagent => "subagent",
        WorkKind::Shell => "shell",
        WorkKind::Monitor => "monitor",
        WorkKind::Task => "task",
    }
}

/// `Pick<OrchestrationV2ProviderTurn, "runAttemptId" | "providerThreadId" | "status">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTurnState {
    pub run_attempt_id: Option<RunAttemptId>,
    pub provider_thread_id: ProviderThreadId,
    pub status: OrchestrationV2ProviderTurnStatus,
}

impl From<&OrchestrationV2ProviderTurn> for ProviderTurnState {
    fn from(turn: &OrchestrationV2ProviderTurn) -> Self {
        Self {
            run_attempt_id: turn.run_attempt_id.clone(),
            provider_thread_id: turn.provider_thread_id.clone(),
            status: turn.status,
        }
    }
}

/// `Pick<OrchestrationV2RunAttempt, "id" | "runId">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAttemptRef {
    pub id: RunAttemptId,
    pub run_id: RunId,
}

/// `isRestartNoteSource`: a run whose turn had settled before the restart
/// cancelled its background work. Continuing it prompts the provider with the note.
pub fn is_restart_note_source(
    source: &OrchestrationV2Run,
    provider_turns: &[ProviderTurnState],
) -> bool {
    // A failed or interrupted turn is never continued. Recovery cancels a
    // waiting run it cannot checkpoint, so a cancelled one still qualifies.
    matches!(
        source.status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Waiting
            | OrchestrationV2RunStatus::Cancelled
    ) && restart_work_len(source) > 0
        && !provider_turns.iter().any(|turn| {
            turn.run_attempt_id == source.active_attempt_id
                && turn.status == OrchestrationV2ProviderTurnStatus::Cancelled
        })
}

fn restart_work_len(run: &OrchestrationV2Run) -> usize {
    run.restart_cancelled_background_work
        .as_ref()
        .map_or(0, Vec::len)
}

fn restart_work(run: &OrchestrationV2Run) -> &[Work] {
    run.restart_cancelled_background_work
        .as_deref()
        .unwrap_or_default()
}

/// `isAwaitedWork`: work an agent ends its turn to wait for. A shell does not
/// count: most are dev servers that nobody waits on.
pub fn is_awaited_work(work: &Work) -> bool {
    work.kind != WorkKind::Shell
}

/// `runOutcome`: a run's status as its waiter should read it. A turn that
/// ended to wait on work the restart cancelled is "interrupted".
pub fn run_outcome(run: &OrchestrationV2Run) -> OrchestrationV2RunStatus {
    if matches!(
        run.status,
        OrchestrationV2RunStatus::Completed | OrchestrationV2RunStatus::Cancelled
    ) && restart_work(run).iter().any(is_awaited_work)
    {
        OrchestrationV2RunStatus::Interrupted
    } else {
        run.status
    }
}

/// `isRestartContinuationSource`: whether a restart continuation may prompt from `source`.
pub fn is_restart_continuation_source(
    source: &OrchestrationV2Run,
    provider_turns: &[ProviderTurnState],
) -> bool {
    if is_restart_note_source(source, provider_turns) {
        restart_work(source).iter().any(is_awaited_work)
    } else {
        source.status == OrchestrationV2RunStatus::Cancelled
    }
}

/// `restartContinuationNote` result: the work a continuation carries, and
/// whether the turn it continues had settled (the note is then the whole prompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartContinuationNote {
    pub work: Vec<Work>,
    pub settled: bool,
}

/// `restartContinuationNote`. A continuation cut before its provider turn
/// completed may not have delivered its own note, so the note is carried
/// forward until a completed turn proves delivery.
pub fn restart_continuation_note(
    source: &OrchestrationV2Run,
    runs: &[OrchestrationV2Run],
    provider_turns: &[ProviderTurnState],
    attempts: &[RunAttemptRef],
) -> RestartContinuationNote {
    let completed_attempts: HashSet<&RunAttemptId> = provider_turns
        .iter()
        .filter(|turn| turn.status == OrchestrationV2ProviderTurnStatus::Completed)
        .filter_map(|turn| turn.run_attempt_id.as_ref())
        .collect();
    let completed_runs: HashSet<&RunId> = attempts
        .iter()
        .filter(|attempt| completed_attempts.contains(&attempt.id))
        .map(|attempt| &attempt.run_id)
        .collect();
    let mut current = source;
    let mut work = restart_work(current).to_vec();
    let mut visited: HashSet<RunId> = HashSet::from([source.id.clone()]);
    while let Some(continuation_of) = current.restart_continuation_of_run_id.as_ref() {
        let attempt_completed = current
            .active_attempt_id
            .as_ref()
            .is_some_and(|id| completed_attempts.contains(id));
        if completed_runs.contains(&current.id) || attempt_completed {
            break;
        }
        let Some(previous) = runs
            .iter()
            .find(|candidate| &candidate.id == continuation_of)
        else {
            break;
        };
        if visited.contains(&previous.id) {
            break;
        }
        visited.insert(previous.id.clone());
        current = previous;
        work = merge_restart_cancelled_background_work(restart_work(current), &work);
    }
    RestartContinuationNote {
        work,
        settled: is_restart_note_source(current, provider_turns),
    }
}

/// `isRestartNoteContinuation`: a restart continuation prompted with the note
/// rather than a native resume.
pub fn is_restart_note_continuation(
    run: &OrchestrationV2Run,
    runs: &[OrchestrationV2Run],
    provider_turns: &[ProviderTurnState],
    attempts: &[RunAttemptRef],
) -> bool {
    let Some(source_id) = run.restart_continuation_of_run_id.as_ref() else {
        return false;
    };
    let Some(source) = runs.iter().find(|candidate| &candidate.id == source_id) else {
        return false;
    };
    !restart_continuation_note(source, runs, provider_turns, attempts)
        .work
        .is_empty()
}

/// The inputs of `pendingRestartCancelledBackgroundWork`.
pub struct PendingRestartWork<'a> {
    pub runs: &'a [OrchestrationV2Run],
    pub provider_turns: &'a [ProviderTurnState],
    pub compaction_message_ids: &'a HashSet<MessageId>,
    pub run: &'a OrchestrationV2Run,
    /// Include earlier attempts: a steer replaces the attempt but not the run.
    pub attempts: &'a [RunAttemptRef],
}

/// `pendingRestartCancelledBackgroundWork`: work cancelled by a restart that
/// the run's provider thread has not been told about yet. The note belongs to
/// the provider thread that lost the work. A later completed turn on the same
/// thread proves delivery, so the pending set is derived rather than cleared.
pub fn pending_restart_cancelled_background_work(input: PendingRestartWork<'_>) -> Vec<Work> {
    let run = input.run;
    let is_compaction = |candidate: &OrchestrationV2Run| {
        input
            .compaction_message_ids
            .contains(&candidate.user_message_id)
    };
    // The current run prepends the note unless it is a compaction or a
    // continuation (whose own prompt is the note, or which resumes natively).
    let Some(provider_thread_id) = run.provider_thread_id.as_ref() else {
        return Vec::new();
    };
    if run.restart_continuation_of_run_id.is_some() || is_compaction(run) {
        return Vec::new();
    }
    let delivered_attempt_ids: HashSet<&RunAttemptId> = input
        .provider_turns
        .iter()
        .filter(|turn| {
            &turn.provider_thread_id == provider_thread_id
                && turn.status == OrchestrationV2ProviderTurnStatus::Completed
        })
        .filter_map(|turn| turn.run_attempt_id.as_ref())
        .collect();
    let same_thread: Vec<&OrchestrationV2Run> = input
        .runs
        .iter()
        .filter(|candidate| candidate.provider_thread_id.as_ref() == Some(provider_thread_id))
        .collect();
    let prompted: Vec<&OrchestrationV2Run> = same_thread
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.id != run.id
                && candidate.status != OrchestrationV2RunStatus::RolledBack
                && candidate.active_attempt_id.as_ref().is_some_and(|active| {
                    delivered_attempt_ids.contains(active)
                        || input.attempts.iter().any(|attempt| {
                            attempt.run_id == candidate.id
                                && delivered_attempt_ids.contains(&attempt.id)
                        })
                })
                && !is_compaction(candidate)
                && (candidate.restart_continuation_of_run_id.is_none()
                    || is_restart_note_continuation(
                        candidate,
                        input.runs,
                        input.provider_turns,
                        input.attempts,
                    ))
        })
        .collect();
    // A steer restarts this run on a new attempt: an earlier attempt that already
    // reached the provider delivered the note, so the replacement must not repeat it.
    let already_delivered = input.attempts.iter().any(|attempt| {
        attempt.run_id == run.id
            && run.active_attempt_id.as_ref() != Some(&attempt.id)
            && delivered_attempt_ids.contains(&attempt.id)
    });
    if already_delivered {
        return Vec::new();
    }
    same_thread
        .iter()
        .copied()
        .filter(|source| {
            run_ran_after(run, *source)
                && restart_work_len(source) > 0
                && !prompted.iter().any(|later| run_ran_after(*later, *source))
        })
        .fold(Vec::new(), |work, source| {
            merge_restart_cancelled_background_work(&work, restart_work(source))
        })
}

/// `mergeRestartCancelledBackgroundWork`. Rows recorded before ids existed fall back to kind + label.
pub fn merge_restart_cancelled_background_work(current: &[Work], added: &[Work]) -> Vec<Work> {
    let identity = |entry: &Work| match &entry.id {
        Some(id) => id.as_str().to_owned(),
        None => format!("{}\u{0}{}", kind_name(entry.kind), entry.label.as_str()),
    };
    let mut seen: HashSet<String> = current.iter().map(identity).collect();
    let mut merged = current.to_vec();
    for entry in added {
        if seen.insert(identity(entry)) {
            merged.push(entry.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::projection::test_support;

    const CLAUDE: &str = "provider-thread:claude";
    const CODEX: &str = "provider-thread:codex";

    fn lost_work() -> Vec<Work> {
        vec![Work {
            kind: WorkKind::Subagent,
            label: label("Background subagent test"),
            id: None,
        }]
    }

    fn lost_json() -> Value {
        json!([{"kind": "subagent", "label": "Background subagent test"}])
    }

    /// A run as the projection reads it. `extra` overrides or adds fields.
    fn run(ordinal: u64, provider_thread: &str, extra: Value) -> OrchestrationV2Run {
        let mut value = test_support::run(
            "thread:restart-note",
            &format!("run:{ordinal}"),
            ordinal,
            "completed",
        );
        value["providerThreadId"] = json!(provider_thread);
        value["userMessageId"] = json!(format!("message:{ordinal}"));
        value["activeAttemptId"] = json!(format!("attempt:{ordinal}"));
        for (key, field) in extra.as_object().expect("run extra is an object") {
            value[key] = field.clone();
        }
        serde_json::from_value(value).expect("run fixture")
    }

    fn turn_for(source: &OrchestrationV2Run) -> ProviderTurnState {
        ProviderTurnState {
            run_attempt_id: source.active_attempt_id.clone(),
            provider_thread_id: source.provider_thread_id.clone().expect("thread"),
            status: OrchestrationV2ProviderTurnStatus::Completed,
        }
    }

    fn attempt_for(run: &OrchestrationV2Run) -> RunAttemptRef {
        RunAttemptRef {
            id: run.active_attempt_id.clone().expect("attempt"),
            run_id: run.id.clone(),
        }
    }

    /// `pending(target, runs)` in the upstream tests: every other run's turn is delivered.
    fn pending_for(target: &OrchestrationV2Run, runs: &[OrchestrationV2Run]) -> Vec<Work> {
        let turns: Vec<ProviderTurnState> = runs
            .iter()
            .filter(|source| source.id != target.id)
            .map(turn_for)
            .collect();
        pending_restart_cancelled_background_work(PendingRestartWork {
            runs,
            provider_turns: &turns,
            compaction_message_ids: &HashSet::new(),
            run: target,
            attempts: &[attempt_for(target)],
        })
    }

    fn pending_with(
        target: &OrchestrationV2Run,
        runs: &[OrchestrationV2Run],
        turns: &[ProviderTurnState],
        attempts: &[RunAttemptRef],
    ) -> Vec<Work> {
        pending_restart_cancelled_background_work(PendingRestartWork {
            runs,
            provider_turns: turns,
            compaction_message_ids: &HashSet::new(),
            run: target,
            attempts,
        })
    }

    #[test]
    fn keeps_the_note_for_the_provider_thread_that_lost_the_work_across_a_provider_switch() {
        let root = run(
            1,
            CLAUDE,
            json!({"restartCancelledBackgroundWork": lost_json()}),
        );
        let on_codex = run(2, CODEX, json!({}));
        let back_on_claude = run(3, CLAUDE, json!({}));
        // Codex never lost the work, so it neither receives nor consumes the note.
        assert!(pending_for(&on_codex, &[root.clone(), on_codex.clone()]).is_empty());
        assert_eq!(
            pending_for(
                &back_on_claude,
                &[root.clone(), on_codex.clone(), back_on_claude.clone()]
            ),
            lost_work()
        );
        // Once Claude was told, later Claude turns are not.
        let later = run(4, CLAUDE, json!({}));
        assert!(pending_for(&later, &[root, on_codex, back_on_claude, later.clone()]).is_empty());
    }

    #[test]
    fn delivers_a_resumed_queued_runs_note_even_after_a_higher_ordinal_run() {
        // Run 3 ran ahead of held run 2; run 2 then resumed, lost background work in
        // a second restart, and its continuation was superseded by a new message.
        let ran_first = run(
            3,
            CLAUDE,
            json!({"completedAt": "2026-10-03T10:00:00.000Z"}),
        );
        let resumed = run(
            2,
            CLAUDE,
            json!({
                "completedAt": "2026-10-03T10:05:00.000Z",
                "restartCancelledBackgroundWork": lost_json(),
            }),
        );
        let next = run(4, CLAUDE, json!({}));
        let runs = [resumed.clone(), ran_first.clone(), next.clone()];
        let turns = [turn_for(&resumed), turn_for(&ran_first)];
        assert_eq!(
            pending_with(&next, &runs, &turns, &[attempt_for(&next)]),
            lost_work()
        );
    }

    #[test]
    fn bounds_the_note_so_it_cannot_crowd_out_the_turns_context() {
        let work: Vec<Work> = (0..25)
            .map(|index| Work {
                kind: WorkKind::Shell,
                label: label(format!("sleep {index}")),
                id: None,
            })
            .collect();
        let note = restart_cancelled_background_work_note(&work);
        assert_eq!(note.split('\n').count(), 12);
        assert!(note.ends_with("- and 15 more"));

        let item: OrchestrationV2TurnItem = serde_json::from_value(test_support::turn_item(
            "thread:restart-note",
            "turn-item:command",
            Some("run:1"),
            1,
            "command_execution",
            "running",
            json!({"input": "x".repeat(10_000), "title": null, "output": null, "exitCode": null}),
        ))
        .expect("command item fixture");
        let command = cancelled_turn_item_work(&item).expect("command is background work");
        assert!(command.label.as_str().chars().count() <= 160);
    }

    #[test]
    fn bounds_roster_task_labels_including_a_long_task_id() {
        let long = "t".repeat(400);
        let work = cancelled_roster_task_work(&OrchestrationV2PendingBackgroundTask::Command {
            task_id: label(&long),
            description: Some(label("d".repeat(400))),
        });
        assert_eq!(work.kind, WorkKind::Shell);
        assert_eq!(work.label.as_str().chars().count(), 160);
        let id_only =
            cancelled_roster_task_work(&OrchestrationV2PendingBackgroundTask::BackgroundTask {
                task_id: label(&long),
                description: None,
            });
        assert_eq!(id_only.label.as_str().chars().count(), 160);
    }

    #[test]
    fn keeps_separate_cancelled_tasks_that_share_a_kind_and_label() {
        let first = Work {
            kind: WorkKind::Shell,
            label: label("sleep 20"),
            id: Some(label("item-1")),
        };
        let second = Work {
            kind: WorkKind::Shell,
            label: label("sleep 20"),
            id: Some(label("item-2")),
        };
        let merged = merge_restart_cancelled_background_work(
            std::slice::from_ref(&first),
            &[second.clone(), first.clone()],
        );
        assert_eq!(merged, vec![first, second]);
        // Rows recorded before ids existed still collapse by kind + label.
        let legacy = Work {
            kind: WorkKind::Shell,
            label: label("sleep 20"),
            id: None,
        };
        assert_eq!(
            merge_restart_cancelled_background_work(
                std::slice::from_ref(&legacy),
                std::slice::from_ref(&legacy)
            )
            .len(),
            1
        );
    }

    #[test]
    fn does_not_repeat_the_note_when_a_steer_restarts_the_run_on_a_new_attempt() {
        let root = run(
            1,
            CLAUDE,
            json!({"restartCancelledBackgroundWork": lost_json()}),
        );
        let steered = run(2, CLAUDE, json!({"activeAttemptId": "attempt:2b"}));
        let first_attempt = RunAttemptId::new("attempt:2a").expect("attempt id");
        let second_attempt = RunAttemptId::new("attempt:2b").expect("attempt id");
        let attempts = [
            RunAttemptRef {
                id: first_attempt.clone(),
                run_id: steered.id.clone(),
            },
            RunAttemptRef {
                id: second_attempt,
                run_id: steered.id.clone(),
            },
        ];
        let delivered_first = ProviderTurnState {
            run_attempt_id: Some(first_attempt.clone()),
            provider_thread_id: ProviderThreadId::new(CLAUDE).expect("thread id"),
            status: OrchestrationV2ProviderTurnStatus::Completed,
        };
        let runs = [root.clone(), steered.clone()];
        let root_turn = turn_for(&root);

        // The first attempt reached the provider with the note; its replacement must not repeat it.
        assert!(
            pending_with(
                &steered,
                &runs,
                &[root_turn.clone(), delivered_first],
                &attempts
            )
            .is_empty()
        );
        // A first attempt that never reached the provider did not deliver it.
        assert_eq!(
            pending_with(&steered, &runs, std::slice::from_ref(&root_turn), &attempts),
            lost_work()
        );
    }

    #[test]
    fn counts_a_prompted_mid_turn_or_chained_continuation_as_delivering_the_note() {
        let cut = run(
            1,
            CLAUDE,
            json!({"status": "cancelled", "restartCancelledBackgroundWork": lost_json()}),
        );
        let cut_turn = ProviderTurnState {
            status: OrchestrationV2ProviderTurnStatus::Cancelled,
            ..turn_for(&cut)
        };
        let continuation = run(2, CLAUDE, json!({"restartContinuationOfRunId": "run:1"}));
        // A turn cut mid-way that lost work is prompted with the note, not resumed natively.
        assert!(is_restart_note_continuation(
            &continuation,
            &[cut.clone(), continuation.clone()],
            std::slice::from_ref(&cut_turn),
            &[]
        ));
        let cut_without_work = run(
            1,
            CLAUDE,
            json!({"status": "cancelled", "restartCancelledBackgroundWork": []}),
        );
        assert!(!is_restart_note_continuation(
            &continuation,
            &[cut_without_work, continuation.clone()],
            std::slice::from_ref(&cut_turn),
            &[]
        ));

        let later = run(3, CLAUDE, json!({}));
        let runs = [cut.clone(), continuation.clone(), later.clone()];
        let attempts = [attempt_for(&later)];
        let delivered_continuation = turn_for(&continuation);
        assert!(
            pending_with(
                &later,
                &runs,
                &[cut_turn.clone(), delivered_continuation],
                &attempts
            )
            .is_empty()
        );
        // A continuation that never reached the provider delivered nothing.
        assert_eq!(
            pending_with(&later, &runs, std::slice::from_ref(&cut_turn), &attempts),
            lost_work()
        );

        // Its own continuation carries the original note forward.
        let unstarted = run(
            2,
            CLAUDE,
            json!({"restartContinuationOfRunId": "run:1", "status": "cancelled"}),
        );
        let chained = run(3, CLAUDE, json!({"restartContinuationOfRunId": "run:2"}));
        assert_eq!(
            restart_continuation_note(
                &unstarted,
                &[cut.clone(), unstarted.clone()],
                std::slice::from_ref(&cut_turn),
                &[]
            ),
            RestartContinuationNote {
                work: lost_work(),
                settled: false,
            }
        );
        assert!(is_restart_note_continuation(
            &chained,
            &[cut.clone(), unstarted.clone(), chained.clone()],
            std::slice::from_ref(&cut_turn),
            &[]
        ));
        // Claude announces running before accepting the prompt. A second restart
        // can cancel that turn without delivering anything to the provider.
        for status in [
            OrchestrationV2ProviderTurnStatus::Running,
            OrchestrationV2ProviderTurnStatus::Cancelled,
        ] {
            let turns = [
                cut_turn.clone(),
                ProviderTurnState {
                    status,
                    ..turn_for(&unstarted)
                },
            ];
            assert_eq!(
                restart_continuation_note(
                    &unstarted,
                    &[cut.clone(), unstarted.clone()],
                    &turns,
                    &[]
                ),
                RestartContinuationNote {
                    work: lost_work(),
                    settled: false,
                }
            );
            assert_eq!(
                pending_with(
                    &later,
                    &[cut.clone(), unstarted.clone(), later.clone()],
                    &turns,
                    &attempts
                ),
                lost_work()
            );
        }

        // A steer replaced the attempt after its completed turn delivered the note.
        let steered = OrchestrationV2Run {
            active_attempt_id: Some(RunAttemptId::new("attempt:replacement").expect("attempt id")),
            ..unstarted.clone()
        };
        let steer_attempts = [RunAttemptRef {
            id: unstarted.active_attempt_id.clone().expect("attempt"),
            run_id: steered.id.clone(),
        }];
        let turns = [
            cut_turn.clone(),
            turn_for(&unstarted),
            ProviderTurnState {
                status: OrchestrationV2ProviderTurnStatus::Cancelled,
                ..turn_for(&steered)
            },
        ];
        assert!(
            restart_continuation_note(
                &steered,
                &[cut.clone(), steered.clone()],
                &turns,
                &steer_attempts
            )
            .work
            .is_empty()
        );
        assert!(!is_restart_note_continuation(
            &chained,
            &[cut.clone(), steered.clone(), chained.clone()],
            &turns,
            &steer_attempts
        ));
        assert!(
            pending_with(
                &later,
                &[cut.clone(), steered.clone(), later.clone()],
                &turns,
                &steer_attempts
            )
            .is_empty()
        );
    }
}
