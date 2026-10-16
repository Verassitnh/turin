//! Synthetic dev data: two projects and four threads with user and assistant
//! messages, a command, and a file diff.
//!
//! Written through the write side (`EventSink::commit_project_command` and
//! `commit_command`), so the read model is folded by the same applier the app
//! runs. Every name, path and message is invented. Nothing here reads user data.
//!
//! Shared by `examples/seed_dev_db.rs` (which includes this file by path, so
//! it may only use external crates) and the read-path test.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_contracts::{CommandId, EventId, ProjectId, ThreadId, UtcDateTime};
use turin_orchestration::pipeline::event_sink::{CommandCommit, EventSink, ProjectCommit};
use turin_persistence::ProjectEventInput;

const T0: &str = "2026-10-01T09:00:00.000Z";
const T1: &str = "2026-10-01T09:05:00.000Z";
const T2: &str = "2026-10-01T09:10:00.000Z";

/// Writes the whole fixture. Idempotent only on an empty database: a second run
/// fails on the duplicate project event id.
pub async fn seed(sink: &EventSink) -> anyhow::Result<()> {
    for project in &PROJECTS {
        commit_project(sink, project).await?;
    }
    for thread in &THREADS {
        commit_thread(sink, thread).await?;
    }
    Ok(())
}

struct SeedProject {
    id: &'static str,
    title: &'static str,
    workspace_root: &'static str,
}

const PROJECTS: [SeedProject; 2] = [
    SeedProject {
        id: "project-atlas",
        title: "Atlas",
        workspace_root: "/dev/synthetic/atlas",
    },
    SeedProject {
        id: "project-lumen",
        title: "Lumen",
        workspace_root: "/dev/synthetic/lumen",
    },
];

struct SeedThread {
    id: &'static str,
    project_id: &'static str,
    title: &'static str,
    /// Generated turns placed before `turns`, so a thread can exceed the
    /// bounded window and exercise history paging.
    filler_turns: usize,
    turns: &'static [SeedTurn],
}

/// One user prompt and its assistant reply, plus optional tool and diff items.
struct SeedTurn {
    prompt: &'static str,
    reply: &'static str,
    command: Option<(&'static str, &'static str)>,
    diff: Option<(&'static str, &'static str)>,
}

const THREADS: [SeedThread; 4] = [
    SeedThread {
        id: "thread-atlas-login",
        project_id: "project-atlas",
        title: "Fix the login redirect",
        filler_turns: 10,
        turns: &[
            SeedTurn {
                prompt: "Why does login bounce back to / after the callback?",
                reply: "The callback drops the `next` parameter, so the app falls back to `/`.",
                command: Some((
                    "rg -n \"next\" src/auth",
                    "src/auth/redirect.ts:14:  const next = params.get(\"next\");",
                )),
                diff: None,
            },
            SeedTurn {
                prompt: "Please keep the `next` parameter through the callback.",
                reply: "Fixed: the callback now restores `next` before it redirects.",
                command: None,
                diff: Some((
                    "src/auth/redirect.ts",
                    "@@ -12,3 +12,5 @@\n const params = new URLSearchParams(location.search);\n-const target = \"/\";\n+const next = params.get(\"next\");\n+const target = next ?? \"/\";\n router.replace(target);",
                )),
            },
        ],
    },
    SeedThread {
        id: "thread-atlas-uploads",
        project_id: "project-atlas",
        title: "Add retry to uploads",
        filler_turns: 0,
        turns: &[SeedTurn {
            prompt: "Uploads fail on flaky networks. Add a retry with backoff.",
            reply: "Added three attempts with exponential backoff, capped at five seconds.",
            command: None,
            diff: None,
        }],
    },
    SeedThread {
        id: "thread-lumen-changelog",
        project_id: "project-lumen",
        title: "Summarise the changelog",
        filler_turns: 0,
        turns: &[SeedTurn {
            prompt: "Summarise the changes since the last tag.",
            reply: "Three fixes and one feature: dark mode, a faster index, and two crash fixes.",
            command: Some((
                "git log --oneline v0.4.0..HEAD",
                "a1b2c3d dark mode\nd4e5f6a index\n",
            )),
            diff: None,
        }],
    },
    SeedThread {
        id: "thread-lumen-sidebar",
        project_id: "project-lumen",
        title: "Tidy the sidebar copy",
        filler_turns: 0,
        turns: &[SeedTurn {
            prompt: "Shorten the empty-state copy in the sidebar.",
            reply: "Shortened it to one line and matched the button label.",
            command: None,
            diff: Some((
                "src/sidebar/empty.tsx",
                "@@ -4,2 +4,2 @@\n-  No projects have been added to this workspace yet.\n+  No projects yet.",
            )),
        }],
    },
];

async fn commit_project(sink: &EventSink, project: &SeedProject) -> anyhow::Result<()> {
    let project_id = ProjectId::new(project.id)?;
    let command_id = CommandId::new(format!("cmd-{}", project.id))?;
    let event_id = EventId::new(format!("evt-{}", project.id))?;
    sink.commit_project_command(ProjectCommit {
        command_id: command_id.clone(),
        project_id,
        command_type: "project.create".to_owned(),
        accepted_at: T0.parse::<UtcDateTime>()?,
        event: ProjectEventInput {
            event_id,
            aggregate_id: project.id.to_owned(),
            event_type: "project.created".to_owned(),
            occurred_at: T0.to_owned(),
            command_id: Some(command_id.clone()),
            causation_event_id: None,
            correlation_id: Some(command_id),
            payload: json!({
                "projectId": project.id,
                "title": project.title,
                "workspaceRoot": project.workspace_root,
                "createdAt": T0,
                "updatedAt": T0,
            }),
            metadata: json!({}),
        },
    })
    .await?;
    Ok(())
}

/// Builds one thread's events in order: the thread, one run per turn, then the
/// turn's items. A single command commits them all.
async fn commit_thread(sink: &EventSink, thread: &SeedThread) -> anyhow::Result<()> {
    let mut events = Vec::new();
    let mut seq = 0_u32;
    let next_event = |seq: &mut u32| -> anyhow::Result<EventId> {
        *seq += 1;
        Ok(EventId::new(format!("evt-{}-{seq}", thread.id))?)
    };

    let thread_json = thread_fields(thread);
    events.push(decode(json!({
        "type": "thread.created",
        "id": next_event(&mut seq)?,
        "threadId": thread.id,
        "occurredAt": T0,
        "payload": thread_json,
    }))?);

    let turn_count = thread.filler_turns + thread.turns.len();
    for index in 0..turn_count {
        let ordinal = index as u64 + 1;
        let run_id = format!("run-{}-{ordinal}", thread.id);
        let run_at = if index == 0 { T0 } else { T1 };
        events.push(decode(json!({
            "type": "run.created",
            "id": next_event(&mut seq)?,
            "threadId": thread.id,
            "runId": run_id,
            "occurredAt": run_at,
            "payload": run_fields(thread.id, &run_id, ordinal),
        }))?);

        let message_id = format!("message-{}-{ordinal}", thread.id);
        let (prompt, reply, command, diff) = match index.checked_sub(thread.filler_turns) {
            Some(static_index) => {
                let turn = &thread.turns[static_index];
                (
                    turn.prompt.to_owned(),
                    turn.reply.to_owned(),
                    turn.command,
                    turn.diff,
                )
            }
            None => (
                format!("Filler question {ordinal}?"),
                format!("Filler answer {ordinal}."),
                None,
                None,
            ),
        };
        let mut items: Vec<Value> = vec![user_message(
            thread.id,
            &run_id,
            ordinal,
            &message_id,
            &prompt,
        )];
        items.push(assistant_message(
            thread.id,
            &run_id,
            ordinal,
            &message_id,
            &reply,
        ));
        if let Some((input, output)) = command {
            items.push(command_execution(
                thread.id, &run_id, ordinal, input, output,
            ));
        }
        if let Some((file, diff)) = diff {
            items.push(file_change(thread.id, &run_id, ordinal, file, diff));
        }
        let occurred = if index == 0 { T2 } else { T1 };
        for item in items {
            events.push(decode(json!({
                "type": "turn-item.updated",
                "id": next_event(&mut seq)?,
                "threadId": thread.id,
                "runId": run_id,
                "occurredAt": occurred,
                "payload": item,
            }))?);
        }
    }

    let command_id = CommandId::new(format!("cmd-{}", thread.id))?;
    sink.commit_command(CommandCommit {
        command_id,
        thread_id: ThreadId::new(thread.id)?,
        command_type: "thread.create".to_owned(),
        accepted_at: T0.parse::<UtcDateTime>()?,
        events,
        effects: Vec::new(),
        cancel_unsettled_effects: None,
    })
    .await?;
    Ok(())
}

fn decode(value: Value) -> anyhow::Result<OrchestrationV2DomainEvent> {
    Ok(serde_json::from_value(value)?)
}

fn thread_fields(thread: &SeedThread) -> Value {
    json!({
        "id": thread.id,
        "projectId": thread.project_id,
        "title": thread.title,
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": { "rootThreadId": thread.id, "parentThreadId": null, "relationshipToParent": null },
        "forkedFrom": null,
        "createdBy": "user",
        "creationSource": "web",
        "createdAt": T0,
        "updatedAt": T2,
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "lastVisitedAt": null,
        "deletedAt": null,
        "latestRunId": null,
        "activeRunId": null,
        "status": "idle",
        "pendingRuntimeRequest": null,
        "latestVisibleMessage": null,
        "latestUserMessageAt": null,
        "hasActionableProposedPlan": false,
        "itemCount": 0,
        "visibleItemCount": 0,
    })
}

fn run_fields(thread_id: &str, run_id: &str, ordinal: u64) -> Value {
    json!({
        "id": run_id,
        "threadId": thread_id,
        "ordinal": ordinal,
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
        "providerThreadId": null,
        "userMessageId": format!("message-{thread_id}-{ordinal}"),
        "rootNodeId": null,
        "activeAttemptId": null,
        "status": "completed",
        "requestedAt": T0,
        "startedAt": T0,
        "completedAt": T2,
        "checkpointId": null,
        "contextHandoffId": null,
    })
}

/// The fields every turn item shares. `ordinal` is a placeholder: the applier
/// allocates the real position on write.
fn item_common(kind: &str, id: String, thread_id: &str, run_id: &str, ordinal: u64) -> Value {
    json!({
        "type": kind,
        "id": id,
        "threadId": thread_id,
        "runId": run_id,
        "nodeId": null,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": ordinal,
        "status": "completed",
        "title": null,
        "startedAt": T0,
        "completedAt": T2,
        "updatedAt": T2,
    })
}

fn user_message(
    thread_id: &str,
    run_id: &str,
    ordinal: u64,
    message_id: &str,
    text: &str,
) -> Value {
    let mut item = item_common(
        "user_message",
        format!("item-{message_id}-user"),
        thread_id,
        run_id,
        ordinal,
    );
    merge(
        &mut item,
        json!({
            "createdBy": "user",
            "creationSource": "web",
            "messageId": message_id,
            "inputIntent": "turn_start",
            "text": text,
            "attachments": [],
        }),
    );
    item
}

fn assistant_message(
    thread_id: &str,
    run_id: &str,
    ordinal: u64,
    message_id: &str,
    text: &str,
) -> Value {
    let mut item = item_common(
        "assistant_message",
        format!("item-{message_id}-assistant"),
        thread_id,
        run_id,
        ordinal,
    );
    merge(
        &mut item,
        json!({
            "messageId": format!("{message_id}-reply"),
            "text": text,
            "attachments": [],
            "streaming": false,
        }),
    );
    item
}

fn command_execution(
    thread_id: &str,
    run_id: &str,
    ordinal: u64,
    input: &str,
    output: &str,
) -> Value {
    let mut item = item_common(
        "command_execution",
        format!(
            "item-{run_id}-command-{input_slug}",
            input_slug = slug(input)
        ),
        thread_id,
        run_id,
        ordinal,
    );
    merge(
        &mut item,
        json!({ "input": input, "output": output, "exitCode": 0 }),
    );
    item
}

fn file_change(thread_id: &str, run_id: &str, ordinal: u64, file: &str, diff: &str) -> Value {
    let mut item = item_common(
        "file_change",
        format!("item-{run_id}-file-{file_slug}", file_slug = slug(file)),
        thread_id,
        run_id,
        ordinal,
    );
    let additions = diff.lines().filter(|line| line.starts_with('+')).count();
    let deletions = diff.lines().filter(|line| line.starts_with('-')).count();
    merge(
        &mut item,
        json!({
            "fileName": file,
            "additions": additions,
            "deletions": deletions,
            "diffStr": diff,
        }),
    );
    item
}

fn slug(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect()
}

fn merge(target: &mut Value, extra: Value) {
    if let (Some(target), Value::Object(extra)) = (target.as_object_mut(), extra) {
        target.extend(extra);
    }
}

/// Appends one turn to a seeded thread as a new run, through the same write
/// path. Tests use it to drive the live thread feed.
pub async fn append_turn(
    sink: &EventSink,
    thread_id: &str,
    ordinal: u64,
    prompt: &str,
    reply: &str,
) -> anyhow::Result<()> {
    let run_id = format!("run-{thread_id}-{ordinal}");
    let message_id = format!("message-{thread_id}-{ordinal}");
    let mut events = vec![decode(json!({
        "type": "run.created",
        "id": format!("evt-{thread_id}-run-{ordinal}"),
        "threadId": thread_id,
        "runId": run_id,
        "occurredAt": T2,
        "payload": run_fields(thread_id, &run_id, ordinal),
    }))?];
    let items = [
        user_message(thread_id, &run_id, ordinal, &message_id, prompt),
        assistant_message(thread_id, &run_id, ordinal, &message_id, reply),
    ];
    for (position, item) in items.into_iter().enumerate() {
        events.push(decode(json!({
            "type": "turn-item.updated",
            "id": format!("evt-{thread_id}-turn-{ordinal}-{position}"),
            "threadId": thread_id,
            "runId": run_id,
            "occurredAt": T2,
            "payload": item,
        }))?);
    }
    sink.commit_command(CommandCommit {
        command_id: CommandId::new(format!("cmd-{thread_id}-turn-{ordinal}"))?,
        thread_id: ThreadId::new(thread_id)?,
        command_type: "thread.turn.start".to_owned(),
        accepted_at: T2.parse::<UtcDateTime>()?,
        events,
        effects: Vec::new(),
        cancel_unsettled_effects: None,
    })
    .await?;
    Ok(())
}
