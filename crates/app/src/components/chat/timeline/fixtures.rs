//! Fixture threads for the mock timeline source.
//!
//! The turn structure follows upstream's orchestration-v2 replay scenarios
//! (upstream/apps/server/src/orchestration-v2/testkit/fixtures):
//! `multi_turn`, `claude_local_bash_task`, `claude_mcp_tool_presentation`,
//! `claude_background_subagent_lifecycle`, `proposed_plan`, `todo_list`,
//! `opencode2_question`, `opencode2_permission`, `opencode2_compaction`,
//! `claude_result_is_error` and `message_steering`. Those transcripts use
//! terse probe text (`ROOT_STARTED`, `A_FIRST`), so the prose here is
//! written to exercise the markdown renderer the way real replies do: code
//! fences in several languages, tables, alerts, nested lists, inline code,
//! long unbroken tokens and URLs.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::json;

use super::turn_item::{
    CheckpointFileSummary, NotificationOutcome, NotificationSource, PlanStep, PlanStepStatus,
    ProjectedTurnItem, ProjectedVisibility, ProviderApprovalDecision, ProviderApprovalOption,
    ProviderFailure, ProviderFailureClass, ProviderRequestKind, TurnItem, TurnItemPayload,
    TurnItemStatus, UserInputAnswerPayload, UserInputQuestion, UserInputQuestionOption,
};
use super::types::{
    Actor, CheckpointStatus, CreationSource, InputIntent, PendingApproval, PendingUserInput,
    ResponseCapability, RunStatus, TimelineLatestRun, TurnDiffSummary,
};

/// One run (turn) recorded by the builder.
#[derive(Clone, Debug)]
pub struct FixtureRun {
    pub run_id: String,
    pub status: RunStatus,
    pub started_at: String,
    pub completed_at: Option<String>,
}

/// Everything a fixture thread provides to the timeline source.
#[derive(Clone, Debug, Default)]
pub struct FixtureThread {
    pub thread_id: String,
    pub title: String,
    pub items: Vec<ProjectedTurnItem>,
    pub runs: Vec<FixtureRun>,
    pub checkpoints: Vec<TurnDiffSummary>,
    pub pending_approvals: Vec<PendingApproval>,
    pub pending_user_inputs: Vec<PendingUserInput>,
    pub workspace_root: Option<String>,
}

impl FixtureThread {
    pub fn latest_run(&self) -> Option<TimelineLatestRun> {
        self.runs.last().map(|run| TimelineLatestRun {
            run_id: run.run_id.clone(),
            status: run.status,
            started_at: Some(run.started_at.clone()),
            completed_at: run.completed_at.clone(),
        })
    }
}

fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub struct ThreadBuilder {
    thread: FixtureThread,
    clock: DateTime<Utc>,
    ordinal: u64,
    run: Option<FixtureRun>,
    run_count: u64,
}

impl ThreadBuilder {
    pub fn new(thread_id: &str, title: &str, start: DateTime<Utc>) -> Self {
        Self {
            thread: FixtureThread {
                thread_id: thread_id.into(),
                title: title.into(),
                workspace_root: Some("/Users/dev/code/turin".into()),
                ..Default::default()
            },
            clock: start,
            ordinal: 0,
            run: None,
            run_count: 0,
        }
    }

    fn tick(&mut self, seconds: i64) -> String {
        self.clock += Duration::seconds(seconds);
        iso(self.clock)
    }

    fn run_id(&self) -> Option<String> {
        self.run.as_ref().map(|run| run.run_id.clone())
    }

    fn push(
        &mut self,
        status: TurnItemStatus,
        title: Option<&str>,
        seconds: i64,
        payload: TurnItemPayload,
    ) -> String {
        let started = iso(self.clock);
        let updated = self.tick(seconds);
        self.ordinal += 1;
        let id = format!("{}-item-{}", self.thread.thread_id, self.ordinal);
        let live = status.is_live();
        self.thread.items.push(ProjectedTurnItem {
            position: self.ordinal,
            visibility: ProjectedVisibility::Local,
            source_thread_id: self.thread.thread_id.clone(),
            source_item_id: id.clone(),
            item: TurnItem {
                id: id.clone(),
                thread_id: self.thread.thread_id.clone(),
                run_id: self.run_id(),
                node_id: self.run_id().map(|run| format!("{run}-node")),
                provider_turn_id: self.run_id().map(|run| format!("{run}-provider-turn")),
                parent_item_id: None,
                ordinal: self.ordinal,
                status,
                title: title.map(str::to_string),
                started_at: Some(started),
                completed_at: (!live).then(|| updated.clone()),
                updated_at: updated,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                payload,
            },
        });
        id
    }

    /// A user prompt that starts a new run.
    pub fn user(&mut self, text: &str) -> &mut Self {
        self.finish_run(RunStatus::Completed);
        self.run_count += 1;
        let run_id = format!("{}-run-{}", self.thread.thread_id, self.run_count);
        self.tick(40);
        self.run = Some(FixtureRun {
            run_id: run_id.clone(),
            status: RunStatus::Running,
            started_at: iso(self.clock),
            completed_at: None,
        });
        let message_id = format!("{run_id}-user");
        self.push(
            TurnItemStatus::Completed,
            None,
            1,
            TurnItemPayload::UserMessage {
                message_id,
                text: text.into(),
                created_by: Actor::User,
                creation_source: CreationSource::Web,
                scheduled_task_id: None,
                sender_thread_id: None,
                input_intent: InputIntent::TurnStart,
                attachments: Vec::new(),
            },
        );
        self
    }

    pub fn assistant(&mut self, text: &str) -> &mut Self {
        let message_id = format!("{}-assistant-{}", self.thread.thread_id, self.ordinal + 1);
        self.push(
            TurnItemStatus::Completed,
            None,
            6,
            TurnItemPayload::AssistantMessage {
                message_id,
                text: text.into(),
                streaming: false,
                attachments: Vec::new(),
            },
        );
        self
    }

    /// A reply still arriving.
    pub fn assistant_streaming(&mut self, text: &str) -> &mut Self {
        let message_id = format!("{}-assistant-{}", self.thread.thread_id, self.ordinal + 1);
        self.push(
            TurnItemStatus::Running,
            None,
            2,
            TurnItemPayload::AssistantMessage {
                message_id,
                text: text.into(),
                streaming: true,
                attachments: Vec::new(),
            },
        );
        self
    }

    pub fn reasoning(&mut self, text: &str) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            None,
            3,
            TurnItemPayload::Reasoning {
                text: text.into(),
                streaming: false,
            },
        );
        self
    }

    pub fn command(&mut self, input: &str, output: &str, exit_code: i64) -> &mut Self {
        let status = if exit_code == 0 {
            TurnItemStatus::Completed
        } else {
            TurnItemStatus::Failed
        };
        self.push(
            status,
            None,
            4,
            TurnItemPayload::CommandExecution {
                input: input.into(),
                output: Some(output.into()),
                output_omitted: false,
                output_indicates_failure: Some(exit_code != 0),
                exit_code: Some(exit_code),
            },
        );
        self
    }

    pub fn command_running(&mut self, input: &str) -> &mut Self {
        self.push(
            TurnItemStatus::Running,
            None,
            2,
            TurnItemPayload::CommandExecution {
                input: input.into(),
                output: None,
                output_omitted: false,
                output_indicates_failure: None,
                exit_code: None,
            },
        );
        self
    }

    pub fn file_change(&mut self, path: &str, additions: u64, deletions: u64) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            None,
            3,
            TurnItemPayload::FileChange {
                file_name: path.into(),
                additions: Some(additions),
                deletions: Some(deletions),
                diff: None,
                changes: None,
            },
        );
        self
    }

    pub fn file_search(&mut self, pattern: &str, results: &[&str]) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            Some(&format!("Searched for {pattern}")),
            2,
            TurnItemPayload::FileSearch {
                pattern: Some(pattern.into()),
                results: results.iter().map(|s| s.to_string()).collect(),
            },
        );
        self
    }

    pub fn read_file(&mut self, path: &str) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            Some("Read"),
            1,
            TurnItemPayload::DynamicTool {
                tool_name: Some("Read".into()),
                viewed_image_path: None,
                input: json!({ "file_path": path }),
                output: Some(json!("(file contents)")),
                output_omitted: true,
            },
        );
        self
    }

    pub fn web_search(&mut self, query: &str) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            Some("Web search"),
            3,
            TurnItemPayload::WebSearch {
                patterns: vec![query.into()],
                results: vec![(Some("GPUI docs".into()), Some("https://www.gpui.rs".into()))],
            },
        );
        self
    }

    pub fn mcp_tool(
        &mut self,
        tool: &str,
        input: serde_json::Value,
        output: serde_json::Value,
    ) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            None,
            2,
            TurnItemPayload::DynamicTool {
                tool_name: Some(tool.into()),
                viewed_image_path: None,
                input,
                output: Some(output),
                output_omitted: false,
            },
        );
        self
    }

    pub fn subagent(&mut self, prompt: &str, result: Option<&str>, live: bool) -> &mut Self {
        let id = format!("{}-subagent-{}", self.thread.thread_id, self.ordinal + 1);
        self.push(
            if live {
                TurnItemStatus::Running
            } else {
                TurnItemStatus::Completed
            },
            Some("Explore the timeline logic"),
            20,
            TurnItemPayload::Subagent {
                subagent_id: id,
                origin: "provider_native".into(),
                child_thread_id: None,
                prompt: prompt.into(),
                progress: live.then(|| "Reading MessagesTimeline.logic.ts".into()),
                result: result.map(str::to_string),
            },
        );
        self
    }

    pub fn notification(
        &mut self,
        summary: &str,
        source: NotificationSource,
        outcome: NotificationOutcome,
    ) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            None,
            1,
            TurnItemPayload::Notification {
                source,
                outcome,
                summary: summary.into(),
                detail: None,
            },
        );
        self
    }

    pub fn proposed_plan(&mut self, markdown: &str) -> &mut Self {
        let plan_id = format!("{}-plan-{}", self.thread.thread_id, self.ordinal + 1);
        self.push(
            TurnItemStatus::Completed,
            None,
            8,
            TurnItemPayload::ProposedPlan {
                plan_id,
                markdown: markdown.into(),
                streaming: false,
            },
        );
        self
    }

    pub fn todo_list(&mut self, steps: &[(&str, PlanStepStatus)]) -> &mut Self {
        let plan_id = format!("{}-todo", self.thread.thread_id);
        self.push(
            TurnItemStatus::Completed,
            None,
            1,
            TurnItemPayload::TodoList {
                plan_id,
                steps: steps
                    .iter()
                    .enumerate()
                    .map(|(ix, (text, status))| PlanStep {
                        id: format!("step-{ix}"),
                        text: text.to_string(),
                        status: *status,
                        duration_anchor_at: None,
                        duration_ms: None,
                    })
                    .collect(),
                explanation: None,
            },
        );
        self
    }

    pub fn answered_question(&mut self, question: &str, answer: &str) -> &mut Self {
        let request_id = format!("{}-question-{}", self.thread.thread_id, self.ordinal + 1);
        self.push(
            TurnItemStatus::Completed,
            Some("Asked a question"),
            12,
            TurnItemPayload::UserInputRequest {
                request_id: request_id.clone(),
                questions: vec![question_fixture("scope", question)],
                question_answer: Some(UserInputAnswerPayload {
                    request_id,
                    question_text_by_id: vec![("scope".into(), question.into())],
                    answers: vec![("scope".into(), json!(answer))],
                    attachments_by_question_id: Vec::new(),
                }),
            },
        );
        self
    }

    pub fn approval(
        &mut self,
        kind: ProviderRequestKind,
        prompt: &str,
        status: TurnItemStatus,
    ) -> &mut Self {
        let request_id = format!("{}-approval-{}", self.thread.thread_id, self.ordinal + 1);
        self.push(
            status,
            Some("Approval requested"),
            5,
            TurnItemPayload::ApprovalRequest {
                request_id,
                request_kind: kind,
                prompt: Some(prompt.into()),
                app_name: None,
                options: Vec::new(),
            },
        );
        self
    }

    pub fn compaction(&mut self, before: u64, after: u64) -> &mut Self {
        self.push(
            TurnItemStatus::Completed,
            None,
            9,
            TurnItemPayload::Compaction {
                summary: Some("Summarised the first half of the thread.".into()),
                before_token_count: Some(before),
                after_token_count: Some(after),
            },
        );
        self
    }

    pub fn provider_error(&mut self, message: &str, class: ProviderFailureClass) -> &mut Self {
        self.push(
            TurnItemStatus::Failed,
            Some("Provider error"),
            1,
            TurnItemPayload::Error {
                failure: ProviderFailure {
                    class,
                    message: message.into(),
                    code: None,
                    retryable: Some(false),
                    reset_at: None,
                },
                retry: None,
            },
        );
        self
    }

    /// Records a checkpoint for the current run, as turn-diff summaries do.
    pub fn checkpoint(&mut self, files: &[(&str, u64, u64)]) -> &mut Self {
        let Some(run_id) = self.run_id() else {
            return self;
        };
        let assistant_message_id = self.thread.items.iter().rev().find_map(|item| {
            match (&item.item.payload, &item.item.run_id) {
                (TurnItemPayload::AssistantMessage { message_id, .. }, Some(run))
                    if *run == run_id =>
                {
                    Some(message_id.clone())
                }
                _ => None,
            }
        });
        let count = self.thread.checkpoints.len() as u64 + 1;
        self.thread.checkpoints.push(TurnDiffSummary {
            checkpoint_id: Some(format!("{run_id}-checkpoint")),
            scope_id: Some("root".into()),
            run_id: run_id.clone(),
            checkpoint_turn_count: count,
            checkpoint_ref: format!("refs/t3/checkpoints/{count}"),
            status: CheckpointStatus::Ready,
            files: files
                .iter()
                .map(|(path, additions, deletions)| CheckpointFileSummary {
                    path: path.to_string(),
                    kind: "modified".into(),
                    additions: *additions,
                    deletions: *deletions,
                })
                .collect(),
            assistant_message_id,
            completed_at: iso(self.clock),
        });
        self
    }

    pub fn finish_run(&mut self, status: RunStatus) -> &mut Self {
        if let Some(mut run) = self.run.take() {
            run.status = status;
            run.completed_at = Some(iso(self.clock));
            self.thread.runs.push(run);
        }
        self
    }

    pub fn pending_approval(&mut self, kind: ProviderRequestKind, detail: &str) -> &mut Self {
        self.thread.pending_approvals.push(PendingApproval {
            request_id: format!("{}-pending-approval", self.thread.thread_id),
            request_kind: kind,
            created_at: iso(self.clock),
            detail: Some(detail.into()),
            app_name: None,
            options: Some(vec![
                ProviderApprovalOption {
                    decision: ProviderApprovalDecision::Accept,
                    label: "Approve once".into(),
                    warning: None,
                },
                ProviderApprovalOption {
                    decision: ProviderApprovalDecision::AcceptForSession,
                    label: "Always allow this session".into(),
                    warning: None,
                },
                ProviderApprovalOption {
                    decision: ProviderApprovalDecision::Decline,
                    label: "Decline".into(),
                    warning: None,
                },
            ]),
            response_capability: ResponseCapability::Live,
        });
        self
    }

    pub fn pending_question(
        &mut self,
        header: &str,
        question: &str,
        options: &[(&str, &str)],
    ) -> &mut Self {
        self.thread.pending_user_inputs.push(PendingUserInput {
            request_id: format!("{}-pending-question", self.thread.thread_id),
            created_at: iso(self.clock),
            questions: vec![UserInputQuestion {
                id: "q1".into(),
                header: header.into(),
                question: question.into(),
                options: options
                    .iter()
                    .map(|(label, description)| UserInputQuestionOption {
                        label: label.to_string(),
                        description: description.to_string(),
                        value: None,
                    })
                    .collect(),
                multi_select: false,
                allow_custom_answer: Some(true),
                initial_answer: None,
                required: Some(true),
            }],
            response_capability: ResponseCapability::Live,
            dismissible: true,
        });
        self
    }

    /// Leaves the last run open (the thread is working).
    pub fn build_working(mut self) -> FixtureThread {
        if let Some(run) = self.run.take() {
            self.thread.runs.push(run);
        }
        self.thread
    }

    pub fn build(mut self) -> FixtureThread {
        self.finish_run(RunStatus::Completed);
        self.thread
    }
}

fn question_fixture(id: &str, question: &str) -> UserInputQuestion {
    UserInputQuestion {
        id: id.into(),
        header: "Scope".into(),
        question: question.into(),
        options: vec![
            UserInputQuestionOption {
                label: "Port everything".into(),
                description: "Logic, rendering and tests in one pass".into(),
                value: None,
            },
            UserInputQuestionOption {
                label: "Rendering first".into(),
                description: "Get pixels on screen, then fill in logic".into(),
                value: None,
            },
        ],
        multi_select: false,
        allow_custom_answer: Some(true),
        initial_answer: None,
        required: Some(true),
    }
}

const REPLY_OVERVIEW: &str = r#"I read through the upstream timeline and here's how it fits together.

## What renders a row

`MessagesTimeline.tsx` owns the virtualized list. Each row comes from `deriveMessagesTimelineRows` in [`MessagesTimeline.logic.ts`](https://github.com/pingdotgg/t3code/blob/main/apps/web/src/components/chat/MessagesTimeline.logic.ts), which folds settled turns, groups tool calls and keeps row identity stable while text streams.

1. **Entries** come from the projection (`deriveTimelineEntriesFromVisibleTurnItems`).
2. **Rows** fold and group those entries:
   - finished turns collapse into a *"Worked for 2m 4s"* header
   - consecutive tool calls become one work group
   - the live tail keeps a single activity row
3. **Rendering** picks a component per `row.kind`.

> The stable-rows pass matters: without it every streamed token would remount the whole list.

| Row kind | Component | Notes |
| --- | --- | --- |
| `message` | `UserTimelineRow` / `AssistantTimelineRow` | markdown, meta, copy |
| `work` | `WorkGroupSection` | expandable tool calls |
| `turn-fold` | `TurnFoldTimelineRow` | "Worked for …" |
| `proposed-plan` | `ProposedPlanCard` | plan follow-up |

The geometry lives in `index.css` under `.chat-markdown`, e.g. paragraphs use `margin: 0.65rem 0`."#;

const REPLY_RUST: &str = r#"Here's the shape I'd use for the Rust side. Rows are `Arc`-shared so the list can tell an unchanged row by pointer:

```rust
pub fn compute_stable_rows(
    rows: Vec<MessagesTimelineRow>,
    previous: &StableRowsState,
) -> StableRowsState {
    let mut by_id = HashMap::with_capacity(rows.len());
    let result = rows
        .into_iter()
        .map(|row| {
            let row = match previous.by_id.get(row.id()) {
                Some(prev) if is_row_unchanged(prev, &row) => prev.clone(),
                _ => Arc::new(row),
            };
            by_id.insert(row.id().to_string(), row.clone());
            row
        })
        .collect();
    StableRowsState { by_id, result }
}
```

And the TypeScript it replaces, for comparison:

```ts title="MessagesTimeline.logic.ts"
export function computeStableMessagesTimelineRows(
  rows: MessagesTimelineRow[],
  previous: StableMessagesTimelineRowsState,
): StableMessagesTimelineRowsState {
  const next = new Map<string, MessagesTimelineRow>();
  const result = rows.map((row) => {
    const prevRow = previous.byId.get(row.id);
    const nextRow = prevRow && isRowUnchanged(prevRow, row) ? prevRow : row;
    next.set(row.id, nextRow);
    return nextRow;
  });
  return { byId: next, result };
}
```

> [!NOTE]
> GPUI's `ListState` measures rows lazily, so only rows near the viewport are laid out. A thread with thousands of rows only pays for what's visible.

> [!WARNING]
> Don't re-highlight code on every frame. Cache by `(hash, language, theme)` — streaming re-highlights only the block that changed."#;

const REPLY_WRAPPING: &str = r#"Wrapping edge cases I checked against the browser:

- A long path with no spaces: `crates/app/src/components/chat/markdown/inline_flow_with_a_deliberately_long_module_name.rs`
- A long URL: https://github.com/pingdotgg/t3code/blob/ec1313c8f8333292506f05a355bb72d308a455d8/apps/web/src/components/ChatMarkdown.tsx#L3432-L3500
- A hash: `ec1313c8f8333292506f05a355bb72d308a455d8ec1313c8f8333292506f05a355bb72d308a455d8`
- Mixed scripts: 日本語のテキストも正しく折り返す必要があります。中文也需要正确换行，不能溢出容器。
- Nested lists:
  - second level uses a circle
    - third level uses a square
- [x] Task lists render a checkbox
- [ ] Unchecked tasks too

A code block with a very long line (wraps by default, toggle to scroll):

```bash
cargo test -p turin -- --nocapture components::chat::timeline::messages_timeline_logic::tests::derive_rows_folds_settled_turns_into_a_worked_for_header
```

```json
{ "rows": 4210, "visible": 23, "layoutMs": 1.8, "highlightCacheHits": 41, "highlightCacheMisses": 2 }
```

---

### Smaller heading

Plain paragraph after a rule, with ~~struck~~ text, *emphasis*, **strong**, and a footnote.[^1]

[^1]: Footnotes render in a muted section at the end."#;

const REPLY_SHORT: &[&str] = &[
    "Done — `cargo check -p turin` passes and the new module is wired into `chat.rs`.",
    "Fixed. The off-by-one came from counting the trailing newline in the fence; `strip_suffix('\\n')` before highlighting matches Shiki.",
    "I ran the tests again:\n\n```text\ntest result: ok. 214 passed; 0 failed; 0 ignored; 0 measured\n```\n\nEverything is green.",
    "Good catch. Upstream collapses whitespace inside inline code too (`white-space: normal`), so `a  b` renders as `a b`. I kept that.",
    "> [!TIP]\n> Hold **Shift** while scrolling to move code blocks horizontally when wrapping is off.\n\nThe toggle sits in the block header next to copy.",
];

const PLAN: &str = r#"# Port the thread view

## Summary

Bring the messages timeline over with upstream's grouping and folding, render markdown with matching wrapping, and feed it from a typed mock source until the real reducers land.

## Steps

1. Port `MessagesTimeline.logic.ts` and its tests.
2. Build the inline flow element for markdown text.
3. Render rows: messages, work groups, folds, plans.
4. Wire approvals and pending questions above the composer.

## Risks

- Selection across rows depends on gpui-base's window selection.
- Highlight colors map tree-sitter captures onto TextMate scopes, so a few tokens differ."#;

/// A long, settled thread: many turns of realistic work.
pub fn long_thread(turns: usize) -> FixtureThread {
    let start = Utc::now() - Duration::hours(30);
    let mut b = ThreadBuilder::new("thread-long", "Port the thread view", start);
    add_settled_turns(&mut b, turns);
    b.build()
}

fn add_settled_turns(b: &mut ThreadBuilder, turns: usize) {
    for turn in 0..turns {
        match turn % 6 {
            0 => {
                b.user("Can you walk me through how the upstream messages timeline decides what each row is? I want to port it faithfully.");
                b.reasoning("The user wants the row derivation explained. I should read MessagesTimeline.logic.ts and the session logic that builds entries.\n\nKey parts: turn folds, work groups, stable rows.");
                b.file_search(
                    "deriveMessagesTimelineRows",
                    &["apps/web/src/components/chat/MessagesTimeline.logic.ts"],
                );
                b.read_file("/Users/dev/code/turin/upstream/apps/web/src/components/chat/MessagesTimeline.logic.ts");
                b.read_file("/Users/dev/code/turin/upstream/apps/web/src/session-logic.ts");
                b.command("rg -n \"kind: \\\"turn-fold\\\"\" upstream/apps/web/src", "apps/web/src/components/chat/MessagesTimeline.logic.ts:576:      kind: \"turn-fold\";", 0);
                b.assistant(REPLY_OVERVIEW);
            }
            1 => {
                b.user("Sketch the Rust version of the stable rows pass.");
                b.read_file(
                    "/Users/dev/code/turin/crates/app/src/components/chat/timeline/types.rs",
                );
                b.file_change(
                    "crates/app/src/components/chat/timeline/messages_timeline_logic.rs",
                    212,
                    0,
                );
                b.command(
                    "cargo check -p turin",
                    "    Checking turin v0.1.0\n    Finished `dev` profile in 4.21s",
                    0,
                );
                b.assistant(REPLY_RUST);
                b.checkpoint(&[
                    (
                        "crates/app/src/components/chat/timeline/messages_timeline_logic.rs",
                        212,
                        0,
                    ),
                    ("crates/app/src/components/chat/timeline/mod.rs", 2, 0),
                ]);
            }
            2 => {
                b.user("Check the wrapping behaviour against long paths, URLs and CJK text.\n\nAlso nested lists and task lists please.");
                b.command(
                    "cargo test -p turin inline_flow",
                    "running 6 tests\ntest result: ok. 6 passed",
                    0,
                );
                b.command("cargo clippy -p turin -- -D warnings", "error: unused variable: `width`\n  --> crates/app/src/components/chat/markdown/inline_flow.rs:212:9", 1);
                b.file_change(
                    "crates/app/src/components/chat/markdown/inline_flow.rs",
                    3,
                    3,
                );
                b.command(
                    "cargo clippy -p turin -- -D warnings",
                    "    Finished `dev` profile in 9.8s",
                    0,
                );
                b.assistant(REPLY_WRAPPING);
                b.checkpoint(&[(
                    "crates/app/src/components/chat/markdown/inline_flow.rs",
                    3,
                    3,
                )]);
            }
            3 => {
                b.user("Look at how the background subagents show up.");
                b.subagent("Read MessagesTimeline.tsx and summarise how subagent groups render.", Some("Subagent groups render one card per delegated agent with elapsed time and status."), false);
                b.notification(
                    "Subagent finished",
                    NotificationSource::Subagent {
                        child_thread_id: None,
                    },
                    NotificationOutcome::Completed,
                );
                b.mcp_tool(
                    "mcp__claude_ai_Firecrawl__firecrawl_scrape",
                    json!({"url": "https://example.com", "formats": ["markdown"]}),
                    json!({"markdown": "Example Domain"}),
                );
                b.assistant(REPLY_SHORT[turn % REPLY_SHORT.len()]);
            }
            4 => {
                b.user("Plan the remaining work before you write more code.");
                b.answered_question("How much should this pass cover?", "Rendering first");
                b.proposed_plan(PLAN);
            }
            _ => {
                b.user("Run the full gate.");
                b.command("scripts/check.sh -p turin", "cargo fmt --all --check\ncargo build -p turin --all-targets\n    Finished `dev` profile in 31.2s\ncargo test -p turin\ntest result: ok. 214 passed\ncargo clippy -p turin --all-targets -- -D warnings\n    Finished `dev` profile in 12.0s", 0);
                b.assistant(REPLY_SHORT[(turn / 6) % REPLY_SHORT.len()]);
            }
        }
    }
}

/// A thread with a turn in progress, a pending approval and a question.
pub fn working_thread() -> FixtureThread {
    let start = Utc::now() - Duration::minutes(12);
    let mut b = ThreadBuilder::new("thread-live", "Wire approvals", start);
    add_working_turns(&mut b);
    b.build_working()
}

fn add_working_turns(b: &mut ThreadBuilder) {
    b.clock = b.clock.max(Utc::now() - Duration::minutes(12));
    b.user("Port the approval panel and the pending question panel.");
    b.reasoning("The approval panel sits above the composer. It needs the request kind label and the provider's options.");
    b.read_file("/Users/dev/code/turin/upstream/apps/web/src/components/chat/ComposerPendingApprovalPanel.tsx");
    b.assistant("Both panels render above the composer. Upstream labels approvals by request kind:\n\n- `command` → *Command approval requested*\n- `file-read` → *File-read approval requested*\n- `file-change` → *File-change approval requested*");
    b.checkpoint(&[(
        "crates/app/src/components/chat/composer_pending_approval_panel.rs",
        96,
        0,
    )]);
    b.user("Now run the dev build and screenshot it.");
    b.todo_list(&[
        ("Build the app", PlanStepStatus::Completed),
        ("Capture light screenshot", PlanStepStatus::Running),
        ("Capture dark screenshot", PlanStepStatus::Pending),
    ]);
    b.command(
        "cargo build -p turin",
        "    Finished `dev` profile in 41.0s",
        0,
    );
    b.approval(
        ProviderRequestKind::Command,
        "screencapture -x -o -l$WINDOW docs/screenshots/T16/light.png",
        TurnItemStatus::Waiting,
    );
    b.command_running("screencapture -x -o -l$WINDOW docs/screenshots/T16/light.png");
    b.assistant_streaming("Capturing the light theme now. The window id comes from `CGWindowListCopyWindowInfo`, so the screenshot covers exactly the Turin window and nothing else on screen.\n\nNext I'll");
    b.pending_approval(
        ProviderRequestKind::Command,
        "screencapture -x -o -l$WINDOW docs/screenshots/T16/dark.png",
    );
}

/// The fixture thread to show at startup (`TURIN_FIXTURE=long|working|stress`).
pub fn startup_thread() -> FixtureThread {
    match std::env::var("TURIN_FIXTURE").as_deref() {
        Ok("working") => working_thread(),
        Ok("stress") => long_thread(1200),
        Ok("short") => long_thread(6),
        _ => {
            let start = Utc::now() - Duration::hours(30);
            let mut b = ThreadBuilder::new("thread-main", "Port the thread view", start);
            add_settled_turns(&mut b, 24);
            add_working_turns(&mut b);
            b.build_working()
        }
    }
}

/// Pending question fixture (`opencode2_question`).
pub fn with_pending_question(mut thread: FixtureThread) -> FixtureThread {
    let mut b = ThreadBuilder::new(&thread.thread_id, &thread.title, Utc::now());
    b.pending_question(
        "Screenshots",
        "Which themes should the screenshots cover?",
        &[
            (
                "Light and dark",
                "Capture both appearances of the same thread",
            ),
            ("Dark only", "Faster; light can come later"),
        ],
    );
    thread.pending_user_inputs = b.thread.pending_user_inputs;
    thread
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_thread_has_runs_and_items() {
        let thread = long_thread(12);
        assert_eq!(thread.runs.len(), 12);
        assert!(thread.items.len() > 50);
        assert!(
            thread
                .runs
                .iter()
                .all(|run| run.status == RunStatus::Completed)
        );
    }

    #[test]
    fn working_thread_leaves_the_last_run_running() {
        let thread = working_thread();
        assert_eq!(
            thread.runs.last().map(|run| run.status),
            Some(RunStatus::Running)
        );
        assert_eq!(thread.pending_approvals.len(), 1);
    }
}
