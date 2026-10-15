//! Cursor extension methods spoken by `cursor-agent acp`.
//!
//! These are not in the pinned ACP schema. Shapes are from the shipped CLI
//! bundle (QC `wf_9dc37bfd-861`, items 1 and 4). Every method is a JSON-RPC
//! request: register it with [`crate::AcpClient::on_ext_request`], not with a
//! notification handler. None of the params carry `sessionId`; the owning
//! process is the route.
//!
//! `ask_question` and `create_plan` results nest the outcome. A flat
//! `{"outcome":"answered"}` is read by the CLI as a cancel.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ASK_QUESTION: &str = "cursor/ask_question";
pub const CREATE_PLAN: &str = "cursor/create_plan";
pub const UPDATE_TODOS: &str = "cursor/update_todos";
pub const TASK: &str = "cursor/task";
pub const GENERATE_IMAGE: &str = "cursor/generate_image";
pub const LIST_AVAILABLE_MODELS: &str = "cursor/list_available_models";

/// Side-channel requests the CLI sends and then ignores the result of.
pub const SIDE_REQUESTS: &[&str] = &[UPDATE_TODOS, TASK, GENERATE_IMAGE];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorTodo {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskQuestionItem {
    pub id: String,
    pub prompt: String,
    pub options: Vec<AskOption>,
    pub allow_multiple: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskQuestionParams {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub questions: Vec<AskQuestionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskAnswer {
    pub question_id: String,
    pub selected_option_ids: Vec<String>,
}

/// Inner object stored at `result.outcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum AskQuestionOutcome {
    #[serde(rename = "answered")]
    Answered { answers: Vec<AskAnswer> },
    #[serde(rename = "skipped")]
    Skipped {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskQuestionResult {
    pub outcome: AskQuestionOutcome,
}

impl AskQuestionResult {
    pub fn answered(answers: Vec<AskAnswer>) -> Self {
        Self {
            outcome: AskQuestionOutcome::Answered { answers },
        }
    }

    pub fn skipped(reason: Option<String>) -> Self {
        Self {
            outcome: AskQuestionOutcome::Skipped { reason },
        }
    }

    pub fn cancelled() -> Self {
        Self {
            outcome: AskQuestionOutcome::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPhase {
    pub name: String,
    pub todos: Vec<CursorTodo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlanParams {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    pub plan: String,
    pub todos: Vec<CursorTodo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_project: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<PlanPhase>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum CreatePlanOutcome {
    #[serde(rename = "accepted")]
    Accepted {
        #[serde(default, rename = "planUri", skip_serializing_if = "Option::is_none")]
        plan_uri: Option<String>,
    },
    #[serde(rename = "rejected")]
    Rejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePlanResult {
    pub outcome: CreatePlanOutcome,
}

impl CreatePlanResult {
    pub fn accepted(plan_uri: Option<String>) -> Self {
        Self {
            outcome: CreatePlanOutcome::Accepted { plan_uri },
        }
    }

    pub fn rejected(reason: Option<String>) -> Self {
        Self {
            outcome: CreatePlanOutcome::Rejected { reason },
        }
    }

    pub fn cancelled() -> Self {
        Self {
            outcome: CreatePlanOutcome::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTodosParams {
    pub tool_call_id: String,
    pub todos: Vec<CursorTodo>,
    pub merge: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubagentType {
    Named(SubagentKind),
    Custom { custom: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentKind {
    #[serde(rename = "computer_use")]
    ComputerUse,
    #[serde(rename = "explore")]
    Explore,
    #[serde(rename = "video_review")]
    VideoReview,
    #[serde(rename = "browser_use")]
    BrowserUse,
    #[serde(rename = "shell")]
    Shell,
    #[serde(rename = "vm_setup_helper")]
    VmSetupHelper,
    #[serde(rename = "unspecified")]
    Unspecified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskParams {
    pub tool_call_id: String,
    pub description: String,
    pub prompt: String,
    pub subagent_type: SubagentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateImageParams {
    pub tool_call_id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_image_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelectOption {
    pub value: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfigOption {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(rename = "currentValue")]
    pub current_value: String,
    pub options: Vec<ModelSelectOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModel {
    pub value: String,
    pub name: String,
    pub config_options: Vec<ModelConfigOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAvailableModelsResult {
    pub models: Vec<AvailableModel>,
}

pub fn empty_result() -> Value {
    serde_json::json!({})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn todo(id: &str) -> CursorTodo {
        CursorTodo {
            id: id.to_owned(),
            content: "Inspect the repo".to_owned(),
            status: TodoStatus::Pending,
        }
    }

    #[test]
    fn ask_question_result_is_nested_and_a_flat_outcome_does_not_decode() {
        let wire = serde_json::to_value(AskQuestionResult::answered(vec![AskAnswer {
            question_id: "q1".to_owned(),
            selected_option_ids: vec!["a".to_owned()],
        }]))
        .unwrap();
        assert_eq!(
            wire,
            json!({
                "outcome": {
                    "outcome": "answered",
                    "answers": [{"questionId": "q1", "selectedOptionIds": ["a"]}]
                }
            })
        );
        assert!(
            serde_json::from_value::<AskQuestionResult>(json!({
                "outcome": "answered",
                "answers": []
            }))
            .is_err()
        );
        let skipped =
            serde_json::to_value(AskQuestionResult::skipped(Some("later".to_owned()))).unwrap();
        assert_eq!(skipped["outcome"]["outcome"], "skipped");
        assert_eq!(skipped["outcome"]["reason"], "later");
        assert_eq!(
            serde_json::to_value(AskQuestionResult::cancelled()).unwrap(),
            json!({"outcome": {"outcome": "cancelled"}})
        );
    }

    #[test]
    fn create_plan_result_nests_accepted_and_rejected() {
        let accepted = serde_json::to_value(CreatePlanResult::accepted(Some(
            "file:///plan.md".to_owned(),
        )))
        .unwrap();
        assert_eq!(
            accepted,
            json!({"outcome": {"outcome": "accepted", "planUri": "file:///plan.md"}})
        );
        let bare = serde_json::to_value(CreatePlanResult::accepted(None)).unwrap();
        assert_eq!(bare, json!({"outcome": {"outcome": "accepted"}}));
        assert!(
            serde_json::from_value::<CreatePlanResult>(json!({"outcome": "accepted"})).is_err()
        );
        let rejected = serde_json::to_value(CreatePlanResult::rejected(None)).unwrap();
        assert_eq!(rejected, json!({"outcome": {"outcome": "rejected"}}));
    }

    #[test]
    fn cursor_requests_decode_without_a_session_id() {
        let question: AskQuestionParams = serde_json::from_value(json!({
            "toolCallId": "tool-1",
            "title": "Pick one",
            "questions": [{
                "id": "q1",
                "prompt": "Which?",
                "options": [{"id": "a", "label": "A"}],
                "allowMultiple": false
            }]
        }))
        .unwrap();
        assert_eq!(question.tool_call_id, "tool-1");
        assert_eq!(question.questions[0].options[0].label, "A");

        let plan: CreatePlanParams = serde_json::from_value(json!({
            "toolCallId": "tool-2",
            "plan": "# Plan",
            "todos": [{"id": "t1", "content": "Inspect the repo", "status": "pending"}],
            "phases": [{"name": "Now", "todos": [{"id": "t1", "content": "Inspect the repo", "status": "in_progress"}]}]
        }))
        .unwrap();
        assert_eq!(plan.todos, vec![todo("t1")]);
        assert_eq!(
            plan.phases.unwrap()[0].todos[0].status,
            TodoStatus::InProgress
        );

        let todos: UpdateTodosParams = serde_json::from_value(json!({
            "toolCallId": "tool-3",
            "todos": [{"id": "t1", "content": "Inspect the repo", "status": "completed"}],
            "merge": true
        }))
        .unwrap();
        assert!(todos.merge);

        let task: TaskParams = serde_json::from_value(json!({
            "toolCallId": "tool-4",
            "description": "Look",
            "prompt": "Find the parser",
            "subagentType": "explore"
        }))
        .unwrap();
        assert_eq!(
            task.subagent_type,
            SubagentType::Named(SubagentKind::Explore)
        );
        let custom: TaskParams = serde_json::from_value(json!({
            "toolCallId": "tool-4",
            "description": "Look",
            "prompt": "Find the parser",
            "subagentType": {"custom": "reviewer"}
        }))
        .unwrap();
        assert_eq!(
            custom.subagent_type,
            SubagentType::Custom {
                custom: "reviewer".to_owned()
            }
        );

        let image: GenerateImageParams = serde_json::from_value(json!({
            "toolCallId": "tool-5",
            "description": "A diagram"
        }))
        .unwrap();
        assert!(image.file_path.is_none());
        assert!(image.reference_image_paths.is_none());
    }

    #[test]
    fn list_available_models_keeps_config_option_ids() {
        let parsed: ListAvailableModelsResult = serde_json::from_value(json!({
            "models": [{
                "value": "gpt-5.5",
                "name": "GPT-5.5",
                "configOptions": [{
                    "id": "reasoning",
                    "name": "Reasoning",
                    "description": "Effort",
                    "category": "thought_level",
                    "type": "select",
                    "currentValue": "high",
                    "options": [{"value": "high", "name": "High"}]
                }]
            }]
        }))
        .unwrap();
        assert_eq!(parsed.models[0].config_options[0].id, "reasoning");
        assert_eq!(parsed.models[0].config_options[0].type_name, "select");
        assert_eq!(parsed.models[0].config_options[0].current_value, "high");
    }
}
