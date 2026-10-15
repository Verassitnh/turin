//! Port of upstream/packages/client-runtime/src/t3ToolSummary.ts

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};
use turin_contracts::base_schemas::js_trim;

use super::t3_mcp_tool_presentation::T3McpToolSummaryAction;

/// `T3ToolSummaryCall["outcome"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum T3ToolSummaryOutcome {
    Completed,
    Failed,
    Unfinished,
}

impl T3ToolSummaryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unfinished => "unfinished",
        }
    }
}

/// `T3ToolSummaryCall`. `None` is JS `undefined`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct T3ToolSummaryCall<'a> {
    pub input: Option<&'a Value>,
    pub output: Option<&'a Value>,
    pub outcome: T3ToolSummaryOutcome,
}

/// Return value of [`summarize_t3_tool_calls`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct T3ToolSummary {
    pub label: String,
    pub failed_count: usize,
}

type Record = Map<String, Value>;

fn as_record(value: Option<&Value>) -> Option<&Record> {
    value.and_then(Value::as_object)
}

/// JS `a ?? b` on property reads: `null` counts as missing for `a`, `b` is
/// returned as-is (it may still be `null`).
fn nullish(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

fn id(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !js_trim(value).is_empty())
}

struct ToolResult<'a> {
    data: Option<Cow<'a, Record>>,
    failed: bool,
}

impl ToolResult<'_> {
    const NOT_FAILED: Self = Self {
        data: None,
        failed: false,
    };

    fn into_owned(self) -> ToolResult<'static> {
        ToolResult {
            data: self.data.map(|data| Cow::Owned(data.into_owned())),
            failed: self.failed,
        }
    }
}

/// Reads the structured/JSON MCP result envelopes retained by the provider adapters.
fn read_result(value: Option<&Value>, depth: usize) -> ToolResult<'_> {
    if depth > 4 {
        return ToolResult::NOT_FAILED;
    }
    let Some(value) = value else {
        return ToolResult::NOT_FAILED;
    };
    if let Value::String(text) = value {
        return match serde_json::from_str::<Value>(text) {
            Ok(parsed) => read_result(Some(&parsed), depth + 1).into_owned(),
            Err(_) => ToolResult::NOT_FAILED,
        };
    }
    if let Value::Array(blocks) = value {
        let mut data = None;
        let mut failed = false;
        for block in blocks {
            let record = block.as_object();
            let text = nullish(record.and_then(|record| record.get("text"))).or_else(|| {
                as_record(record.and_then(|record| record.get("content")))
                    .and_then(|content| content.get("text"))
            });
            let inner = nullish(as_record(text).and_then(|text| text.get("text"))).or(text);
            let result = read_result(inner, depth + 1);
            if data.is_none() {
                data = result.data;
            }
            failed = failed || result.failed;
        }
        return ToolResult { data, failed };
    }
    let Some(record) = value.as_object() else {
        return ToolResult::NOT_FAILED;
    };
    let failed = record.get("isError") == Some(&Value::Bool(true))
        || record.get("is_error") == Some(&Value::Bool(true))
        || record
            .get("_tag")
            .and_then(Value::as_str)
            .is_some_and(|tag| tag.ends_with("Error") || tag.ends_with("Failure"))
        || nullish(record.get("error")).is_some();
    let content = nullish(record.get("structuredContent")).or_else(|| record.get("content"));
    if content.is_some() {
        let result = read_result(content, depth + 1);
        return ToolResult {
            data: result.data,
            failed: failed || result.failed,
        };
    }
    ToolResult {
        data: Some(Cow::Borrowed(record)),
        failed,
    }
}

fn read_input(value: Option<&Value>) -> Option<Cow<'_, Record>> {
    let input = read_result(value, 0).data?;
    // Cursor retains its MCP args envelope; other adapters retain the arguments directly.
    if input.get("toolName").is_some_and(Value::is_string) {
        return match input {
            Cow::Borrowed(input) => input
                .get("args")
                .and_then(Value::as_object)
                .map(Cow::Borrowed),
            Cow::Owned(mut input) => match input.remove("args") {
                Some(Value::Object(args)) => Some(Cow::Owned(args)),
                _ => None,
            },
        };
    }
    Some(input)
}

/// MCP errors can be returned as data even when the provider completed the tool call.
pub fn t3_tool_result_indicates_failure(output: Option<&Value>) -> bool {
    read_result(output, 0).failed
}

fn count_entities(ids: &[Option<&str>]) -> usize {
    let distinct: HashSet<&str> = ids.iter().flatten().copied().collect();
    distinct.len() + ids.iter().filter(|value| value.is_none()).count()
}

fn quantity(count: usize, noun: &str) -> String {
    quantity_plural(count, noun, &format!("{noun}s"))
}

fn quantity_plural(count: usize, noun: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { noun } else { plural })
}

struct SummaryCall<'a> {
    input: Option<Cow<'a, Record>>,
    output: Option<Cow<'a, Record>>,
    outcome: T3ToolSummaryOutcome,
}

impl SummaryCall<'_> {
    fn output_get(&self, key: &str) -> Option<&Value> {
        self.output.as_deref().and_then(|output| output.get(key))
    }

    fn input_get(&self, key: &str) -> Option<&Value> {
        self.input.as_deref().and_then(|input| input.get(key))
    }
}

/// Counts successful effects separately from failed or unfinished tool calls.
pub fn summarize_t3_tool_calls(
    action: T3McpToolSummaryAction,
    calls: &[T3ToolSummaryCall<'_>],
) -> T3ToolSummary {
    use T3McpToolSummaryAction as A;

    let results: Vec<SummaryCall<'_>> = calls
        .iter()
        .map(|call| {
            let result = read_result(call.output, 0);
            SummaryCall {
                input: read_input(call.input),
                output: result.data,
                outcome: if result.failed {
                    T3ToolSummaryOutcome::Failed
                } else {
                    call.outcome
                },
            }
        })
        .collect();
    let completed: Vec<&SummaryCall<'_>> = results
        .iter()
        .filter(|call| call.outcome == T3ToolSummaryOutcome::Completed)
        .collect();
    let mut failed_count = results
        .iter()
        .filter(|call| call.outcome == T3ToolSummaryOutcome::Failed)
        .count();
    let selected: Vec<&SummaryCall<'_>> = if completed.is_empty() {
        results.iter().collect()
    } else {
        completed.clone()
    };
    let times = quantity(selected.len(), "time");
    let any_completed = !completed.is_empty();
    let phrase = |past: &str, infinitive: &str, object: &str| {
        if any_completed {
            format!("{past} {object}")
        } else {
            format!("Tried to {infinitive} {object}")
        }
    };
    let entity_ids = |key: &str| -> Vec<Option<&str>> {
        selected
            .iter()
            .map(|call| id(call.output_get(key)).or_else(|| id(call.input_get(key))))
            .collect()
    };
    let project_ids: Vec<Option<&str>> = selected
        .iter()
        .map(|call| {
            id(call.output_get("id"))
                .or_else(|| id(call.output_get("projectId")))
                .or_else(|| id(call.input_get("projectId")))
        })
        .collect();
    let thread_ids: Vec<Option<&str>> = selected
        .iter()
        .map(|call| {
            id(call.output_get("threadId"))
                .or_else(|| {
                    id(as_record(call.output_get("thread"))
                        .and_then(|thread| thread.get("threadId")))
                })
                .or_else(|| id(call.input_get("threadId")))
        })
        .collect();
    let thread_targets_known = thread_ids.iter().all(Option::is_some);
    let distinct_threads = || thread_ids.iter().collect::<HashSet<_>>().len();
    let counted = |key: &str, noun: &str| quantity(count_entities(&entity_ids(key)), noun);

    let label = match action {
        A::ThreadSend => {
            let message_ids: Vec<Option<&str>> = selected
                .iter()
                .map(|call| id(call.output_get("messageId")))
                .collect();
            let messages = count_entities(&message_ids);
            let threads = distinct_threads();
            let object = if thread_targets_known {
                if messages == threads && messages > 1 {
                    format!("messages to {}", quantity(threads, "thread"))
                } else {
                    format!(
                        "{} to {}",
                        quantity(messages, "message"),
                        quantity(threads, "thread")
                    )
                }
            } else {
                quantity(messages, "message")
            };
            phrase("Sent", "send", &object)
        }
        A::ThreadCreate => {
            let mut created_ids: HashSet<&str> = HashSet::new();
            let results_known = !completed.is_empty()
                && completed.iter().all(|call| {
                    let output = call.output.as_deref();
                    let threads: Vec<Option<&Value>> =
                        match output.and_then(|output| output.get("threads")) {
                            Some(Value::Array(threads)) => threads.iter().map(Some).collect(),
                            _ => vec![None],
                        };
                    threads.into_iter().all(|thread| {
                        // `[call.output]` when there is no batch: the output record itself.
                        let record = match thread {
                            Some(thread) => thread.as_object(),
                            None => output,
                        };
                        if record.and_then(|record| record.get("status"))
                            == Some(&Value::from("rolled_back"))
                        {
                            return true;
                        }
                        let Some(thread_id) = id(record.and_then(|record| record.get("threadId")))
                        else {
                            return false;
                        };
                        created_ids.insert(thread_id);
                        true
                    })
                });
            if results_known {
                format!("Created {}", quantity(created_ids.len(), "thread"))
            } else {
                format!("Requested thread creation {times}")
            }
        }
        A::Delegate => phrase("Delegated", "delegate", &counted("taskId", "task")),
        A::ThreadRead | A::ThreadWait => {
            let targets = if thread_targets_known {
                quantity(distinct_threads(), "thread")
            } else {
                format!("threads {times}")
            };
            if action == A::ThreadRead {
                phrase("Read", "read", &targets)
            } else {
                phrase("Waited on", "wait on", &targets)
            }
        }
        A::ThreadList => phrase("Listed", "list", &format!("threads {times}")),
        A::ThreadInterrupt => phrase(
            "Requested interrupts for",
            "interrupt",
            &quantity(count_entities(&thread_ids), "thread"),
        ),
        A::WorkflowStop => phrase(
            "Requested stops for",
            "stop",
            &counted("subagentId", "workflow"),
        ),
        A::TaskStatus => phrase("Checked", "check", &format!("task status {times}")),
        A::TaskCancel => phrase(
            "Requested cancellation of",
            "cancel",
            &counted("taskId", "task"),
        ),
        A::ScheduleCreate => phrase("Scheduled", "schedule", &counted("scheduledTaskId", "task")),
        A::ScheduleList => phrase("Listed", "list", &format!("scheduled tasks {times}")),
        A::ScheduleUpdate => phrase(
            "Updated",
            "update",
            &counted("scheduledTaskId", "scheduled task"),
        ),
        // A successful delete can report deleted:false; it still represents a deletion request.
        A::ScheduleDelete => phrase(
            "Requested deletion of",
            "delete",
            &counted("scheduledTaskId", "scheduled task"),
        ),
        A::ScheduleRun => phrase(
            "Requested",
            "request",
            &quantity(selected.len(), "scheduled task run"),
        ),
        A::ThreadConfiguration => {
            phrase("Checked", "check", &format!("thread configuration {times}"))
        }
        A::ThreadConfigure => phrase("Set", "set", &format!("thread model {times}")),
        A::ThreadFork => phrase(
            "Requested",
            "request",
            &quantity(selected.len(), "thread fork"),
        ),
        A::ThreadMerge => phrase(
            "Requested",
            "request",
            &quantity(selected.len(), "context merge"),
        ),
        A::ThreadSearch => phrase("Searched", "search", &format!("threads {times}")),
        A::ThreadTransfers => phrase("Checked", "check", &format!("thread transfers {times}")),
        A::ThreadOrganize => phrase("Organized", "organize", &format!("threads {times}")),
        A::ThreadUpdate => phrase(
            "Updated",
            "update",
            &quantity(count_entities(&thread_ids), "thread"),
        ),
        A::QueueList => phrase("Listed", "list", &format!("queued messages {times}")),
        A::QueueRead => phrase("Read", "read", &counted("queuedRunId", "queued message")),
        A::QueueEdit => phrase("Edited", "edit", &counted("queuedRunId", "queued message")),
        A::QueueCancel => phrase(
            "Requested cancellation of",
            "cancel",
            &counted("queuedRunId", "queued run"),
        ),
        A::QueueReorder => phrase(
            "Reordered",
            "reorder",
            &counted("queuedRunId", "queued run"),
        ),
        A::QueueSteer => phrase(
            "Requested steering with",
            "steer with",
            &counted("queuedRunId", "queued message"),
        ),
        A::QuestionList => phrase("Listed", "list", &format!("pending questions {times}")),
        A::QuestionRead => phrase(
            "Read",
            "read",
            &counted("requestId", "pending question request"),
        ),
        A::QuestionRespond => phrase(
            "Answered",
            "answer",
            &counted("requestId", "pending question request"),
        ),
        A::SecretRequest => phrase("Asked for", "ask for", &quantity(selected.len(), "secret")),
        A::WorktreeHandoff => phrase(
            "Handed off to",
            "hand off to",
            &counted("worktreePath", "worktree"),
        ),
        A::WorktreeList => phrase("Listed", "list", &format!("workspace branches {times}")),
        A::WorktreeStatus => phrase("Checked", "check", &format!("worktree status {times}")),
        A::ProjectList => phrase("Listed", "list", &format!("projects {times}")),
        A::ProjectRead => phrase(
            "Read",
            "read",
            &quantity(count_entities(&project_ids), "project"),
        ),
        A::ProjectCreate => phrase(
            "Registered",
            "register",
            &quantity(count_entities(&project_ids), "project"),
        ),
        A::ProjectUpdate => phrase(
            "Updated",
            "update",
            &quantity(count_entities(&project_ids), "project"),
        ),
        A::ProjectDelete => phrase(
            "Deleted",
            "delete",
            &quantity(count_entities(&project_ids), "project"),
        ),
        A::ProjectClone => phrase(
            "Cloned",
            "clone",
            &quantity_plural(
                count_entities(&entity_ids("cwd")),
                "repository",
                "repositories",
            ),
        ),
        A::EnvironmentRead => phrase(
            "Checked",
            "check",
            &format!("environment preferences {times}"),
        ),
        A::EnvironmentUpdate => phrase(
            "Updated",
            "update",
            &format!("environment preferences {times}"),
        ),
        A::AttachmentPrepare => phrase(
            "Prepared",
            "prepare",
            &counted("attachmentId", "attachment upload"),
        ),
        A::AttachmentDiscard => phrase(
            "Discarded",
            "discard",
            &counted("attachmentId", "pending attachment"),
        ),
        A::AttachmentSend => {
            // A message can contain several attachments. Count retries by message identity.
            let mut messages: HashMap<Cow<'_, str>, &SummaryCall<'_>> = HashMap::new();
            for (index, call) in selected.iter().enumerate() {
                let key = id(call.output_get("messageId"))
                    .map_or_else(|| Cow::Owned(format!("call-{index}")), Cow::Borrowed);
                messages.insert(key, call);
            }
            let attachments = |call: &SummaryCall<'_>| match call.input_get("attachments") {
                Some(Value::Array(attachments)) => Some(attachments.len()),
                _ => None,
            };
            let attachment_count: usize = messages
                .values()
                .map(|call| attachments(call).unwrap_or(0))
                .sum();
            let counts_known = messages
                .values()
                .all(|call| attachments(call).is_some_and(|count| count > 0));
            let targets = if thread_targets_known {
                format!(" to {}", quantity(distinct_threads(), "thread"))
            } else {
                String::new()
            };
            let object = if counts_known {
                format!("{}{targets}", quantity(attachment_count, "attachment"))
            } else {
                format!("attachments{targets} {times}")
            };
            phrase("Sent", "send", &object)
        }
        A::LinkPr => phrase("Linked", "link", &quantity(selected.len(), "pull request")),
        A::UnlinkPr => phrase(
            "Unlinked",
            "unlink",
            &quantity(selected.len(), "pull request"),
        ),
        A::WatchPr => phrase(
            "Watching",
            "watch",
            &quantity(selected.len(), "pull request"),
        ),
        A::UnwatchPr => phrase(
            "Stopped watching",
            "stop watching",
            &quantity(selected.len(), "pull request"),
        ),
        A::ListPrs => phrase(
            "Checked",
            "check",
            &if selected.len() == 1 {
                "linked pull requests".to_owned()
            } else {
                format!("linked pull requests {times}")
            },
        ),
        A::WritePrGuides => {
            // One call writes many guides, and reports a refusal per guide rather than failing.
            let outcomes: Vec<Option<&Value>> = selected
                .iter()
                .flat_map(|call| match call.output_get("guides") {
                    Some(Value::Array(guides)) => guides
                        .iter()
                        .map(|guide| guide.as_object().and_then(|guide| guide.get("outcome")))
                        .collect(),
                    _ => Vec::new(),
                })
                .collect();
            let started = outcomes
                .iter()
                .filter(|outcome| outcome.and_then(Value::as_str) != Some("failed"))
                .count();
            let failed = outcomes.len() - started;
            failed_count += failed;
            if !outcomes.is_empty() && started == 0 {
                format!("Could not start {}", quantity(failed, "review guide"))
            } else {
                let object = if outcomes.is_empty() {
                    format!("review guides {times}")
                } else if failed > 0 {
                    format!("{}, {failed} failed", quantity(started, "review guide"))
                } else {
                    quantity(started, "review guide")
                };
                phrase("Started", "start", &object)
            }
        }
        A::ReadPrGuide => phrase("Read", "read", &quantity(selected.len(), "review guide")),
        A::Browser => phrase("Used", "use", &format!("browser {times}")),
        A::Device => phrase("Used", "use", &format!("device controls {times}")),
        A::HtmlPreview => phrase(
            "Previewed",
            "preview",
            &quantity(selected.len(), "HTML page"),
        ),
        A::HtmlRender => phrase("Rendered", "render", &quantity(selected.len(), "HTML page")),
        A::Capabilities => phrase(
            "Checked",
            "check",
            &format!("orchestration capabilities {times}"),
        ),
    };
    T3ToolSummary {
        label,
        failed_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn completed<'a>(input: Option<&'a Value>, output: Option<&'a Value>) -> T3ToolSummaryCall<'a> {
        T3ToolSummaryCall {
            input,
            output,
            outcome: T3ToolSummaryOutcome::Completed,
        }
    }

    fn summary(label: &str, failed_count: usize) -> T3ToolSummary {
        T3ToolSummary {
            label: label.to_owned(),
            failed_count,
        }
    }

    fn label(action: T3McpToolSummaryAction, calls: &[T3ToolSummaryCall<'_>]) -> String {
        summarize_t3_tool_calls(action, calls).label
    }

    #[test]
    fn counts_registered_projects_repository_destinations_and_accepted_thread_launches() {
        let empty = json!({});
        let (p1, p2) = (json!({ "id": "project-1" }), json!({ "id": "project-2" }));
        assert_eq!(
            label(
                T3McpToolSummaryAction::ProjectCreate,
                &[
                    completed(Some(&empty), Some(&p1)),
                    completed(Some(&empty), Some(&p1)),
                    completed(Some(&empty), Some(&p2)),
                ]
            ),
            "Registered 2 projects"
        );
        let (c1, c2) = (
            json!({ "cwd": "/tmp/first" }),
            json!({ "cwd": "/tmp/second" }),
        );
        assert_eq!(
            label(
                T3McpToolSummaryAction::ProjectClone,
                &[
                    completed(Some(&empty), Some(&c1)),
                    completed(Some(&empty), Some(&c2)),
                ]
            ),
            "Cloned 2 repositories"
        );
        let launched = json!({ "threadId": "launched-thread", "status": "preparing" });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadCreate,
                &[completed(Some(&empty), Some(&launched))]
            ),
            "Created 1 thread"
        );
    }

    #[test]
    fn reports_review_guides_by_outcome_and_a_call_where_none_started_as_a_failure() {
        let guides = |outcomes: &[&str]| {
            json!({
                "guides": outcomes
                    .iter()
                    .enumerate()
                    .map(|(index, outcome)| json!({ "number": index + 1, "outcome": outcome }))
                    .collect::<Vec<_>>(),
            })
        };
        let empty = json!({});
        let mixed = guides(&["started", "running", "failed"]);
        assert_eq!(
            summarize_t3_tool_calls(
                T3McpToolSummaryAction::WritePrGuides,
                &[completed(Some(&empty), Some(&mixed))]
            ),
            summary("Started 2 review guides, 1 failed", 1)
        );
        let failed = guides(&["failed", "failed"]);
        let all_failed = summarize_t3_tool_calls(
            T3McpToolSummaryAction::WritePrGuides,
            &[completed(Some(&empty), Some(&failed))],
        );
        assert_eq!(all_failed.label, "Could not start 2 review guides");
        assert_eq!(all_failed.failed_count, 2);
    }

    #[test]
    fn deduplicates_the_queued_run_target() {
        let q1 = json!({ "queuedRunId": "queued-1" });
        let q2 = json!({ "queuedRunId": "queued-2" });
        for (action, expected) in [
            (T3McpToolSummaryAction::QueueRead, "Read 1 queued message"),
            (T3McpToolSummaryAction::QueueEdit, "Edited 1 queued message"),
            (
                T3McpToolSummaryAction::QueueCancel,
                "Requested cancellation of 1 queued run",
            ),
            (
                T3McpToolSummaryAction::QueueReorder,
                "Reordered 1 queued run",
            ),
            (
                T3McpToolSummaryAction::QueueSteer,
                "Requested steering with 1 queued message",
            ),
        ] {
            assert_eq!(
                summarize_t3_tool_calls(
                    action,
                    &[
                        completed(Some(&q1), None),
                        completed(Some(&q1), None),
                        T3ToolSummaryCall {
                            input: Some(&q2),
                            output: None,
                            outcome: T3ToolSummaryOutcome::Unfinished,
                        },
                    ]
                ),
                summary(expected, 0),
                "{}",
                action.as_str()
            );
        }
    }

    #[test]
    fn counts_answered_requests_rather_than_pretending_every_request_contains_one_question() {
        let first =
            json!({ "requestId": "request-1", "answers": { "one": ["Yes"], "two": ["No"] } });
        let repeat = json!({ "requestId": "request-1" });
        let second = json!({ "requestId": "request-2" });
        assert_eq!(
            label(
                T3McpToolSummaryAction::QuestionRespond,
                &[
                    completed(Some(&first), None),
                    completed(Some(&repeat), None),
                    completed(Some(&second), None),
                ]
            ),
            "Answered 2 pending question requests"
        );
    }

    #[test]
    fn counts_attachments_in_distinct_messages_and_falls_back_when_attachment_counts_are_missing() {
        let first_input =
            json!({ "threadId": "thread-1", "attachments": [{ "id": "one" }, { "id": "two" }] });
        let first_output = json!({ "messageId": "message-1", "threadId": "thread-1" });
        let second_input = json!({ "threadId": "thread-2", "attachments": [{ "id": "one" }] });
        let second_output = json!({ "messageId": "message-2", "threadId": "thread-2" });
        let first = completed(Some(&first_input), Some(&first_output));
        let second = completed(Some(&second_input), Some(&second_output));
        assert_eq!(
            summarize_t3_tool_calls(
                T3McpToolSummaryAction::AttachmentSend,
                &[first, first, second]
            ),
            summary("Sent 3 attachments to 2 threads", 0)
        );
        let bare = json!({ "threadId": "thread-1" });
        assert_eq!(
            label(
                T3McpToolSummaryAction::AttachmentSend,
                &[first, completed(Some(&bare), None)]
            ),
            "Sent attachments to 1 thread 2 times"
        );
    }

    #[test]
    fn keeps_repeated_manual_runs_separate_and_describes_asynchronous_controls_as_requests() {
        let task = json!({ "taskId": "schedule-1" });
        let (running, skipped) = (
            json!({ "lastRunStatus": "running" }),
            json!({ "lastRunStatus": "skipped" }),
        );
        assert_eq!(
            label(
                T3McpToolSummaryAction::ScheduleRun,
                &[
                    completed(Some(&task), Some(&running)),
                    completed(Some(&task), Some(&skipped)),
                ]
            ),
            "Requested 2 scheduled task runs"
        );
        let empty = json!({});
        let fork = json!({ "targetThreadId": "fork", "sequence": 3 });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadFork,
                &[completed(Some(&empty), Some(&fork))]
            ),
            "Requested 1 thread fork"
        );
        let parent = json!({ "targetThreadId": "parent" });
        let sequence = json!({ "sequence": 4 });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadMerge,
                &[completed(Some(&parent), Some(&sequence))]
            ),
            "Requested 1 context merge"
        );
    }

    #[test]
    fn treats_a_returned_error_tag_as_a_failed_call_even_if_the_provider_says_completed() {
        let empty = json!({});
        for tag in [
            "WorktreeMcpFailure",
            "DeviceOperationError",
            "PreviewAutomationExecutionError",
            "PullRequestOperationError",
        ] {
            let output = json!([{
                "type": "content",
                "content": {
                    "type": "text",
                    "text": json!({ "_tag": tag, "message": "Unavailable" }).to_string(),
                },
            }]);
            assert_eq!(
                summarize_t3_tool_calls(
                    T3McpToolSummaryAction::Browser,
                    &[completed(Some(&empty), Some(&output))]
                ),
                summary("Tried to use browser 1 time", 1),
                "{tag}"
            );
        }
    }

    #[test]
    fn counts_messages_and_distinct_destinations_across_delivery_modes_deduplicating_retries() {
        let fixtures: Vec<(Value, Value)> = (0..5)
            .map(|i| {
                let mode = ["auto", "queue", "steer", "restart"][i % 4];
                (
                    json!({
                        "threadId": format!("thread-{}", i % 2),
                        "mode": mode,
                    }),
                    json!({
                        "messageId": format!("message-{i}"),
                        "threadId": format!("thread-{}", i % 2),
                    }),
                )
            })
            .collect();
        let mut calls: Vec<T3ToolSummaryCall<'_>> = fixtures
            .iter()
            .map(|(input, output)| completed(Some(input), Some(output)))
            .collect();
        calls.push(calls[0]);
        assert_eq!(
            summarize_t3_tool_calls(T3McpToolSummaryAction::ThreadSend, &calls),
            summary("Sent 5 messages to 2 threads", 0)
        );
    }

    #[test]
    fn reads_provider_result_envelopes_without_treating_json_in_the_message_as_result_data() {
        let result = json!({ "messageId": "message-1", "threadId": "actual-thread" });
        let text = result.to_string();
        let outputs = [
            result.clone(),
            json!({ "structuredContent": result }),
            json!([{ "type": "text", "text": text }]),
            json!({ "content": [{ "text": { "text": text } }], "isError": false }),
            Value::String(text.clone()),
        ];
        let input = json!({
            "toolName": "t3_thread_send",
            "args": { "threadId": "input-thread", "message": "{\"threadId\":\"fake\"}" },
        });
        let calls: Vec<T3ToolSummaryCall<'_>> = outputs
            .iter()
            .map(|output| completed(Some(&input), Some(output)))
            .collect();
        assert_eq!(
            label(T3McpToolSummaryAction::ThreadSend, &calls),
            "Sent 1 message to 1 thread"
        );
        let wrapped =
            json!({ "toolName": "t3_thread_send", "args": { "threadId": "input-thread" } });
        let direct = json!({ "threadId": "input-thread" });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadSend,
                &[
                    completed(Some(&wrapped), None),
                    completed(Some(&direct), None)
                ]
            ),
            "Sent 2 messages to 1 thread"
        );
    }

    #[test]
    fn falls_back_to_message_counts_when_a_destination_is_missing_or_a_result_is_malformed() {
        let known = json!({ "threadId": "known" });
        let message = json!({ "message": "{\"threadId\":\"not-a-destination\"}" });
        let truncated = json!("{truncated");
        let sent = json!("Message sent");
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadSend,
                &[
                    completed(Some(&known), None),
                    completed(Some(&message), Some(&truncated)),
                    completed(None, Some(&sent)),
                ]
            ),
            "Sent 3 messages"
        );
    }

    #[test]
    fn counts_batch_created_threads_excludes_rollbacks_and_deduplicates_returned_thread_ids() {
        let mut threads: Vec<Value> = (0..4)
            .map(|i| json!({ "threadId": format!("thread-{i}"), "status": "running" }))
            .collect();
        threads.push(json!({ "threadId": "rolled-back", "status": "rolled_back" }));
        let empty = json!({});
        let batch = json!({ "threads": threads });
        let single = json!({ "threadId": "thread-0", "status": "running" });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadCreate,
                &[
                    completed(Some(&empty), Some(&batch)),
                    completed(Some(&empty), Some(&single)),
                ]
            ),
            "Created 4 threads"
        );
        let requested = json!({ "threads": [{ "title": "Requested, not confirmed" }] });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadCreate,
                &[completed(Some(&requested), None)]
            ),
            "Requested thread creation 1 time"
        );
    }

    #[test]
    fn excludes_failed_and_unfinished_sends_even_if_the_provider_reports_completed() {
        let success_input = json!({ "threadId": "success" });
        let success_output = json!({ "messageId": "ok", "threadId": "success" });
        let failed_input = json!({ "threadId": "failed" });
        let failed_output =
            json!({ "isError": true, "structuredContent": { "threadId": "failed" } });
        let tagged_output = json!([{
            "type": "text",
            "text": json!({ "_tag": "OrchestratorMcpFailure" }).to_string(),
        }]);
        let cancelled_input = json!({ "threadId": "cancelled" });
        let calls = [
            completed(Some(&success_input), Some(&success_output)),
            completed(Some(&failed_input), Some(&failed_output)),
            completed(Some(&failed_input), Some(&tagged_output)),
            T3ToolSummaryCall {
                input: Some(&cancelled_input),
                output: None,
                outcome: T3ToolSummaryOutcome::Unfinished,
            },
        ];
        assert_eq!(
            summarize_t3_tool_calls(T3McpToolSummaryAction::ThreadSend, &calls),
            summary("Sent 1 message to 1 thread", 2)
        );
        assert_eq!(
            summarize_t3_tool_calls(T3McpToolSummaryAction::ThreadSend, &[calls[1]]),
            summary("Tried to send 1 message to 1 thread", 1)
        );
    }

    #[test]
    fn does_not_confuse_a_childs_failure_or_wait_timeout_with_failure_of_the_orchestration_call() {
        let empty = json!({});
        let failed_child =
            json!({ "taskId": "task-1", "status": "failed", "summary": "command not found" });
        assert_eq!(
            summarize_t3_tool_calls(
                T3McpToolSummaryAction::Delegate,
                &[
                    completed(Some(&empty), Some(&failed_child)),
                    completed(Some(&empty), Some(&failed_child)),
                ]
            ),
            summary("Delegated 1 task", 0)
        );
        let task = json!({ "taskId": "task-1" });
        let calls: Vec<T3ToolSummaryCall<'_>> = (0..4)
            .map(|_| completed(Some(&task), Some(&failed_child)))
            .collect();
        assert_eq!(
            label(T3McpToolSummaryAction::TaskStatus, &calls),
            "Checked task status 4 times"
        );
        let thread = json!({ "threadId": "thread-1" });
        let timed_out = json!({ "threadId": "thread-1", "timedOut": true });
        assert_eq!(
            summarize_t3_tool_calls(
                T3McpToolSummaryAction::ThreadWait,
                &[completed(Some(&thread), Some(&timed_out))]
            ),
            summary("Waited on 1 thread", 0)
        );
    }

    #[test]
    fn describes_control_requests_without_claiming_that_a_thread_stopped_or_a_task_was_deleted() {
        let thread = json!({ "threadId": "thread-1" });
        let interrupt = json!({ "threadId": "thread-1", "status": "interrupt_requested" });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ThreadInterrupt,
                &[completed(Some(&thread), Some(&interrupt))]
            ),
            "Requested interrupts for 1 thread"
        );
        let task = json!({ "taskId": "task-1" });
        let cancel = json!({ "taskId": "task-1", "status": "cancel_requested" });
        assert_eq!(
            label(
                T3McpToolSummaryAction::TaskCancel,
                &[completed(Some(&task), Some(&cancel))]
            ),
            "Requested cancellation of 1 task"
        );
        let schedule = json!({ "scheduledTaskId": "schedule-1" });
        let deleted = json!({ "scheduledTaskId": "schedule-1", "deleted": false });
        assert_eq!(
            label(
                T3McpToolSummaryAction::ScheduleDelete,
                &[completed(Some(&schedule), Some(&deleted))]
            ),
            "Requested deletion of 1 scheduled task"
        );
    }

    #[test]
    fn counts_workflow_stop_requests_by_coordinator_and_excludes_failed_requests() {
        let empty = json!({});
        let is_error = json!({ "isError": true });
        let w1 = json!({ "threadId": "parent", "subagentId": "workflow-1" });
        let w2 = json!({ "threadId": "parent", "subagentId": "workflow-2" });
        let w3 = json!({ "threadId": "parent", "subagentId": "workflow-3" });
        let first = completed(Some(&w1), Some(&empty));
        let failed = completed(Some(&w3), Some(&is_error));
        assert_eq!(
            summarize_t3_tool_calls(
                T3McpToolSummaryAction::WorkflowStop,
                &[first, first, completed(Some(&w2), Some(&empty)), failed]
            ),
            summary("Requested stops for 2 workflows", 1)
        );
        assert_eq!(
            summarize_t3_tool_calls(T3McpToolSummaryAction::WorkflowStop, &[failed]),
            summary("Tried to stop 1 workflow", 1)
        );
    }

    #[test]
    fn result_failure_reads_nested_envelopes() {
        assert!(t3_tool_result_indicates_failure(Some(
            &json!({ "is_error": true })
        )));
        assert!(!t3_tool_result_indicates_failure(Some(
            &json!({ "error": null })
        )));
        assert!(!t3_tool_result_indicates_failure(None));
    }
}
