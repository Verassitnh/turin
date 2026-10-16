//! Port of upstream/apps/server/src/orchestration-v2/workflowAgentAnswers.ts
//!
//! Reads what a workflow member actually answered.
//!
//! A member agent never becomes a provider task and emits nothing on the event
//! stream, so the transcript the harness writes is the only full record of its
//! turn — the progress snapshot carries a capped excerpt. The path is derived
//! here from the run's transcript directory and the member's provider id rather
//! than accepted from the caller, and the read itself is contained by
//! `read_contained_workflow_file`.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::orchestration_v2::{
    OrchestrationV2WorkflowFileErrorReason as Reason, OrchestrationWorkflowFileError,
};

use super::unknown_field::{field, text};
use super::workflow_file_read::{
    ContainedWorkflowFileRequest, read_contained_workflow_file, workflow_file_error,
};

const TRANSCRIPT_BYTE_CAP: usize = 512 * 1024;

/// A member id names a file, so anything path-shaped is rejected outright.
/// `^[A-Za-z0-9_-]{1,128}$`.
fn is_valid_agent_id(agent_id: &str) -> bool {
    (1..=128).contains(&agent_id.len())
        && agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Collects the assistant text of each turn in the harness's JSONL.
///
/// Two shapes have to be handled: `attachment` lines and `isMeta` user lines are
/// harness bookkeeping (hook output, skill listings, injected context) and are
/// never conversation, and one assistant API message is split across several
/// lines sharing a `message.id`, so those are joined into a single turn rather
/// than read as separate ones.
pub fn parse_workflow_agent_answers(contents: &str) -> Vec<String> {
    let mut turns: Vec<String> = Vec::new();
    let mut turn_index_by_id: HashMap<String, usize> = HashMap::new();

    for line in contents.split('\n') {
        if js_trim(line).is_empty() {
            continue;
        }
        // A capped read can cut the final line mid-object; drop it and keep the rest.
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let entry = Some(&entry);
        if text(field(entry, "type")) != Some("assistant") {
            continue;
        }
        if field(entry, "isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }

        let message = field(entry, "message");
        let Some(content) = field(message, "content").and_then(Value::as_array) else {
            continue;
        };
        let body = content
            .iter()
            .filter(|block| text(field(Some(*block), "type")) == Some("text"))
            .filter_map(|block| field(Some(block), "text").and_then(Value::as_str))
            .filter(|value| !js_trim(value).is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if body.is_empty() {
            continue;
        }

        let message_id = text(field(message, "id"));
        if let Some(&existing) = message_id.and_then(|id| turn_index_by_id.get(id)) {
            turns[existing] = format!("{}\n\n{}", turns[existing], body);
            continue;
        }
        if let Some(id) = message_id {
            turn_index_by_id.insert(id.to_owned(), turns.len());
        }
        turns.push(body);
    }

    turns
}

/// `readWorkflowAgentAnswers`: the answers in `<transcriptDir>/agent-<agentId>.jsonl`.
/// `home_directory` is `HostProcess.HomeDirectory`.
pub async fn read_workflow_agent_answers(
    transcript_dir: &str,
    agent_id: &str,
    config_dir: Option<&str>,
    home_directory: &str,
) -> Result<Vec<String>, OrchestrationWorkflowFileError> {
    if !is_valid_agent_id(agent_id) {
        return Err(workflow_file_error(Reason::InvalidPath, agent_id, None));
    }
    let path = Path::new(transcript_dir)
        .join(format!("agent-{agent_id}.jsonl"))
        .to_string_lossy()
        .into_owned();
    let file = read_contained_workflow_file(
        ContainedWorkflowFileRequest {
            path: &path,
            extension: ".jsonl",
            byte_cap: TRANSCRIPT_BYTE_CAP,
            tail: true,
            config_dir,
        },
        home_directory,
    )
    .await?;
    // A tail read can start mid-line: drop that partial first line. Without a
    // newline the whole capped contents are one (partial) line and parse drops it.
    let contents = if file.truncated {
        match file.contents.find('\n') {
            Some(index) => file.contents[index + 1..].to_owned(),
            None => file.contents,
        }
    } else {
        file.contents
    };
    Ok(parse_workflow_agent_answers(&contents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(value: Value) -> String {
        format!("{value}\n")
    }

    fn assistant(id: &str, content: Value) -> String {
        line(
            json!({ "type": "assistant", "message": { "role": "assistant", "id": id, "content": content } }),
        )
    }

    #[test]
    fn returns_one_entry_per_assistant_turn() {
        assert_eq!(
            parse_workflow_agent_answers(
                &(assistant("a", json!([{ "type": "text", "text": "first" }]))
                    + &assistant("b", json!([{ "type": "text", "text": "second" }]))),
            ),
            vec!["first", "second"]
        );
    }

    #[test]
    fn joins_the_lines_of_one_split_assistant_message_into_a_single_turn() {
        assert_eq!(
            parse_workflow_agent_answers(
                &(assistant(
                    "a",
                    json!([{ "type": "thinking", "thinking": "weighing it up" }])
                ) + &assistant("a", json!([{ "type": "text", "text": "the answer" }]))
                    + &assistant("a", json!([{ "type": "text", "text": "and more" }]))),
            ),
            vec!["the answer\n\nand more"]
        );
    }

    #[test]
    fn ignores_harness_bookkeeping_and_the_members_own_prompt() {
        assert_eq!(
            parse_workflow_agent_answers(
                &(line(json!({
                    "type": "attachment",
                    "attachment": { "type": "skill_listing", "content": "- adhd" }
                })) + &line(json!({
                    "type": "user",
                    "message": { "role": "user", "content": "the prompt" }
                })) + &line(json!({
                    "type": "assistant",
                    "isMeta": true,
                    "message": {
                        "role": "assistant",
                        "id": "m",
                        "content": [{ "type": "text", "text": "injected" }]
                    }
                })) + &assistant("a", json!([{ "type": "text", "text": "real answer" }]))),
            ),
            vec!["real answer"]
        );
    }

    #[test]
    fn skips_a_turn_that_produced_no_text() {
        assert!(
            parse_workflow_agent_answers(&assistant(
                "a",
                json!([{ "type": "tool_use", "id": "t", "name": "Bash", "input": {} }])
            ))
            .is_empty()
        );
    }

    #[test]
    fn preserves_answer_indentation_and_ignores_blank_text() {
        assert_eq!(
            parse_workflow_agent_answers(
                &(assistant(
                    "a",
                    json!([{ "type": "text", "text": "    indented code" }])
                ) + &assistant("b", json!([{ "type": "text", "text": "   " }]))),
            ),
            vec!["    indented code"]
        );
    }

    #[test]
    fn survives_a_trailing_line_the_byte_cap_cut_in_half() {
        assert_eq!(
            parse_workflow_agent_answers(
                &(assistant("a", json!([{ "type": "text", "text": "complete" }]))
                    + r#"{"type":"assistant","mess"#),
            ),
            vec!["complete"]
        );
    }

    /// Transcripts must sit under <configDir>/projects: the containment read
    /// rejects any path outside it. A temporary configDir keeps the real
    /// ~/.claude untouched.
    struct Fixture {
        _temp: tempfile::TempDir,
        config_dir: String,
        transcript_dir: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        let transcript_dir = config_dir.join("projects").join("test");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        Fixture {
            config_dir: config_dir.to_string_lossy().into_owned(),
            transcript_dir,
            _temp: temp,
        }
    }

    // The under-cap case is the control: without it a read stuck on [] would pass.
    #[tokio::test]
    async fn reads_final_answers_from_both_small_and_capped_transcripts() {
        let fixture = fixture();
        let transcript_dir = fixture.transcript_dir.to_string_lossy().into_owned();
        let turn = assistant("a", json!([{ "type": "text", "text": "the answer" }]));
        let filler = line(json!({
            "type": "user",
            "message": { "role": "user", "content": "x".repeat(4096) }
        }));
        let write = |agent_id: &str, contents: &str| {
            std::fs::write(
                fixture
                    .transcript_dir
                    .join(format!("agent-{agent_id}.jsonl")),
                contents,
            )
            .unwrap();
        };
        let read = |agent_id: &'static str| {
            let transcript_dir = transcript_dir.clone();
            let config_dir = fixture.config_dir.clone();
            async move {
                read_workflow_agent_answers(
                    &transcript_dir,
                    agent_id,
                    Some(&config_dir),
                    &config_dir,
                )
                .await
                .unwrap()
            }
        };
        write("small", &turn);
        let edge_answer = "x".repeat(
            512 * 1024 + 1 - assistant("edge", json!([{ "type": "text", "text": "" }])).len(),
        );
        write(
            "edge",
            &assistant(
                "edge",
                json!([{ "type": "text", "text": edge_answer.as_str() }]),
            ),
        );
        write(
            "boundary",
            &(filler.repeat(160) + &turn + &" ".repeat(512 * 1024 - turn.len())),
        );
        write(
            "huge",
            &(turn.clone()
                + &filler.repeat(160)
                + &assistant("final", json!([{ "type": "text", "text": "final answer" }]))),
        );
        assert_eq!(read("small").await, vec!["the answer"]);
        assert_eq!(read("edge").await, vec![edge_answer.clone()]);
        assert_eq!(read("huge").await, vec!["final answer"]);
        assert_eq!(read("boundary").await, vec!["the answer"]);
    }

    #[tokio::test]
    async fn rejects_path_shaped_agent_ids() {
        let fixture = fixture();
        let transcript_dir = fixture.transcript_dir.to_string_lossy().into_owned();
        for agent_id in ["", "../escape", "a/b", "dot.name"] {
            let error = read_workflow_agent_answers(
                &transcript_dir,
                agent_id,
                Some(&fixture.config_dir),
                &fixture.config_dir,
            )
            .await
            .unwrap_err();
            let OrchestrationWorkflowFileError::OrchestrationWorkflowFileError { reason, .. } =
                error;
            assert_eq!(reason, Reason::InvalidPath, "{agent_id:?}");
        }
    }
}
