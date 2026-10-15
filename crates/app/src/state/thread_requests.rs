//! Port of upstream/packages/client-runtime/src/state/threadRequests.ts

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::state::contracts::{
    OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
    OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
    OrchestrationV2TurnItem, OrchestrationV2UserInputQuestion,
    OrchestrationV2UserInputQuestionOption, ProviderApprovalOption, ProviderRequestKind,
    RuntimeRequestId, TrimmedNonEmptyString, UserInputAttachmentAnswerPayload,
};
use crate::state::thread_projection::{Rows, VisibleTurnItem, VisibleTurnItemRow};
use crate::state::time::iso;

/// `ThreadPendingApproval["responseCapability"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalResponseCapability {
    Live,
    NotResumable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadPendingApproval {
    pub request_id: RuntimeRequestId,
    pub request_kind: ProviderRequestKind,
    pub created_at: String,
    pub detail: Option<String>,
    /// App requesting access for mcp-elicitation approvals (#8058).
    pub app_name: Option<String>,
    /// Approval choices advertised by the provider (#8058); defaults apply when absent.
    pub options: Option<Vec<ProviderApprovalOption>>,
    pub response_capability: ApprovalResponseCapability,
}

/// `ThreadUserInputQuestion`: the question with `multiSelect` defaulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadUserInputQuestion {
    pub id: TrimmedNonEmptyString,
    pub header: TrimmedNonEmptyString,
    pub question: TrimmedNonEmptyString,
    pub options: Vec<OrchestrationV2UserInputQuestionOption>,
    pub multi_select: bool,
    pub allow_custom_answer: Option<bool>,
    pub initial_answer: Option<String>,
    pub required: Option<bool>,
}

impl From<&OrchestrationV2UserInputQuestion> for ThreadUserInputQuestion {
    fn from(question: &OrchestrationV2UserInputQuestion) -> Self {
        Self {
            id: question.id.clone(),
            header: question.header.clone(),
            question: question.question.clone(),
            options: question.options.clone(),
            multi_select: question.multi_select.unwrap_or(false),
            allow_custom_answer: question.allow_custom_answer,
            initial_answer: question.initial_answer.clone(),
            required: question.required,
        }
    }
}

/// `OrchestrationV2RuntimeRequest["responseCapability"]["type"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserInputResponseCapability {
    Live,
    Message,
    NotResumable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserInputResponseMode {
    Message,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadPendingUserInput {
    pub request_id: RuntimeRequestId,
    pub created_at: String,
    pub questions: Vec<ThreadUserInputQuestion>,
    pub response_capability: UserInputResponseCapability,
    pub response_mode: Option<UserInputResponseMode>,
    pub dismissible: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingThreadRequests {
    pub approvals: Vec<ThreadPendingApproval>,
    pub user_inputs: Vec<ThreadPendingUserInput>,
}

/// Seed each question once. Existing edits, cleared answers, and option selections win.
/// `make_draft` builds a draft holding only the seeded custom answer.
pub fn seed_user_input_draft_answers<Draft: Clone>(
    questions: &[OrchestrationV2UserInputQuestion],
    drafts: &IndexMap<String, Draft>,
    make_draft: impl Fn(&str) -> Draft,
) -> IndexMap<String, Draft> {
    let mut seeded = drafts.clone();
    for question in questions {
        let Some(initial_answer) = &question.initial_answer else {
            continue;
        };
        if question.allow_custom_answer == Some(false) || seeded.contains_key(question.id.as_str())
        {
            continue;
        }
        seeded.insert(question.id.as_str().to_owned(), make_draft(initial_answer));
    }
    seeded
}

/// Joins pending request entities to the request items that carry display data.
pub fn derive_pending_thread_requests(
    runtime_requests: &[Arc<OrchestrationV2RuntimeRequest>],
    turn_items: &[Arc<OrchestrationV2TurnItem>],
) -> PendingThreadRequests {
    let mut approvals = Vec::new();
    let mut user_inputs = Vec::new();

    for request in runtime_requests {
        if request.status != OrchestrationV2RuntimeRequestStatus::Pending {
            continue;
        }
        if request.kind == OrchestrationV2RuntimeRequestKind::UserInput {
            let item = turn_items
                .iter()
                .rev()
                .find_map(|candidate| match &**candidate {
                    OrchestrationV2TurnItem::UserInputRequest {
                        request_id,
                        questions,
                        response_mode,
                        ..
                    } if *request_id == request.id => Some((questions, response_mode.is_some())),
                    _ => None,
                });
            let Some((questions, message_mode)) = item else {
                continue;
            };
            let message_capability = matches!(
                request.response_capability,
                OrchestrationV2ResponseCapability::Message
            );
            let dismissible = message_mode || message_capability;
            user_inputs.push(ThreadPendingUserInput {
                request_id: request.id.clone(),
                created_at: iso(&request.created_at),
                questions: questions
                    .iter()
                    .map(ThreadUserInputQuestion::from)
                    .collect(),
                response_capability: match request.response_capability {
                    OrchestrationV2ResponseCapability::Live { .. } => {
                        UserInputResponseCapability::Live
                    }
                    OrchestrationV2ResponseCapability::Message => {
                        UserInputResponseCapability::Message
                    }
                    OrchestrationV2ResponseCapability::NotResumable { .. } => {
                        UserInputResponseCapability::NotResumable
                    }
                },
                response_mode: dismissible.then_some(UserInputResponseMode::Message),
                dismissible,
            });
            continue;
        }

        let request_kind = match request.kind {
            OrchestrationV2RuntimeRequestKind::AuthRefresh
            | OrchestrationV2RuntimeRequestKind::DynamicToolCall
            | OrchestrationV2RuntimeRequestKind::UserInput => continue,
            OrchestrationV2RuntimeRequestKind::Command => ProviderRequestKind::Command,
            OrchestrationV2RuntimeRequestKind::FileRead => ProviderRequestKind::FileRead,
            OrchestrationV2RuntimeRequestKind::FileChange => ProviderRequestKind::FileChange,
            OrchestrationV2RuntimeRequestKind::McpElicitation => {
                ProviderRequestKind::McpElicitation
            }
            OrchestrationV2RuntimeRequestKind::Permission => ProviderRequestKind::Permission,
        };
        let item = turn_items
            .iter()
            .rev()
            .find_map(|candidate| match &**candidate {
                OrchestrationV2TurnItem::ApprovalRequest {
                    request_id,
                    prompt,
                    app_name,
                    options,
                    ..
                } if *request_id == request.id => Some((prompt, app_name, options)),
                _ => None,
            });
        approvals.push(ThreadPendingApproval {
            request_id: request.id.clone(),
            request_kind,
            created_at: iso(&request.created_at),
            detail: item
                .and_then(|(prompt, _, _)| prompt.clone())
                .filter(|prompt| !prompt.is_empty()),
            app_name: item
                .and_then(|(_, app_name, _)| app_name.clone())
                .filter(|name| !name.is_empty()),
            options: item.and_then(|(_, _, options)| options.clone()),
            response_capability: match request.response_capability {
                OrchestrationV2ResponseCapability::Live { .. } => ApprovalResponseCapability::Live,
                _ => ApprovalResponseCapability::NotResumable,
            },
        });
    }

    PendingThreadRequests {
        approvals,
        user_inputs,
    }
}

/// Older V2 answers were saved on requests without updating their timeline items.
///
/// Stateful like upstream's closure: the last result is reused while its inputs
/// keep their identity, and a restored row is reused while its row and request
/// keep theirs.
#[derive(Default)]
pub struct QuestionHistoryProjector {
    previous_rows: Option<Arc<Vec<VisibleTurnItemRow>>>,
    previous_requests: Option<Rows<OrchestrationV2RuntimeRequest>>,
    previous_result: Option<Arc<Vec<VisibleTurnItemRow>>>,
    /// Keyed by the source row's address; the entry holds the row, so the
    /// address stays unique while the entry lives.
    cache: HashMap<
        usize,
        (
            VisibleTurnItemRow,
            Arc<OrchestrationV2RuntimeRequest>,
            VisibleTurnItemRow,
        ),
    >,
}

impl QuestionHistoryProjector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn project(
        &mut self,
        rows: &Arc<Vec<VisibleTurnItemRow>>,
        requests: &Rows<OrchestrationV2RuntimeRequest>,
    ) -> Arc<Vec<VisibleTurnItemRow>> {
        if let (Some(previous_rows), Some(previous_requests), Some(previous_result)) = (
            &self.previous_rows,
            &self.previous_requests,
            &self.previous_result,
        ) && Arc::ptr_eq(previous_rows, rows)
            && Arc::ptr_eq(previous_requests, requests)
        {
            return Arc::clone(previous_result);
        }
        let by_id: HashMap<&RuntimeRequestId, &Arc<OrchestrationV2RuntimeRequest>> = requests
            .iter()
            .map(|request| (&request.id, request))
            .collect();
        let mut result: Option<Vec<VisibleTurnItemRow>> = None;
        let mut next_cache = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            let mut next = Arc::clone(row);
            if let OrchestrationV2TurnItem::UserInputRequest {
                request_id,
                questions,
                question_answer: None,
                ..
            } = &*row.item
                && let Some(request) = by_id.get(request_id).copied()
                && request.status == OrchestrationV2RuntimeRequestStatus::Resolved
                && let Some(answers) = &request.answers
            {
                let key = Arc::as_ptr(row) as usize;
                let cached = self
                    .cache
                    .get(&key)
                    .filter(|(_, cached_request, _)| Arc::ptr_eq(cached_request, request))
                    .map(|(_, _, cached_row)| Arc::clone(cached_row));
                next = match cached {
                    Some(cached_row) => cached_row,
                    None => {
                        let mut item = (*row.item).clone();
                        if let OrchestrationV2TurnItem::UserInputRequest {
                            question_answer, ..
                        } = &mut item
                        {
                            *question_answer = Some(UserInputAttachmentAnswerPayload {
                                request_id: TrimmedNonEmptyString::new(request.id.as_str())
                                    .expect("request ids are non-blank"),
                                question_text_by_id: Some(
                                    questions
                                        .iter()
                                        .map(|question| {
                                            (
                                                question.id.as_str().to_owned(),
                                                question.question.as_str().to_owned(),
                                            )
                                        })
                                        .collect(),
                                ),
                                answers: answers.clone(),
                                attachments_by_question_id: IndexMap::new(),
                            });
                        }
                        Arc::new(VisibleTurnItem {
                            item: Arc::new(item),
                            ..(**row).clone()
                        })
                    }
                };
                next_cache.insert(
                    key,
                    (Arc::clone(row), Arc::clone(request), Arc::clone(&next)),
                );
            }
            if !Arc::ptr_eq(&next, row) {
                result.get_or_insert_with(|| rows[..index].to_vec());
            }
            if let Some(result) = result.as_mut() {
                result.push(next);
            }
        }
        self.cache = next_cache;
        self.previous_rows = Some(Arc::clone(rows));
        self.previous_requests = Some(Arc::clone(requests));
        let projected = result.map_or_else(|| Arc::clone(rows), Arc::new);
        self.previous_result = Some(Arc::clone(&projected));
        projected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::{OrchestrationV2ThreadProjection, ProviderSessionId};
    use crate::state::support::turn_item::TurnItemExt;
    use crate::state::test_fixtures::{V2_NOW, from_json, v2_projection};
    use crate::state::thread_projection::ThreadProjection;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn question(value: serde_json::Value) -> OrchestrationV2UserInputQuestion {
        let mut base = json!({ "header": "H", "question": "Q?", "options": [] });
        for (key, field) in value.as_object().unwrap() {
            base[key] = field.clone();
        }
        from_json(base)
    }

    #[test]
    fn seeds_editable_answers_and_preserves_edits_and_selections_on_later_snapshots() {
        let questions = vec![
            question(json!({ "id": "editor", "initialAnswer": "  Proposed commit\n\n" })),
            question(json!({ "id": "empty", "initialAnswer": "" })),
            question(json!({ "id": "ordinary" })),
            question(
                json!({ "id": "choices", "initialAnswer": "Ignore this", "allowCustomAnswer": false }),
            ),
        ];
        #[derive(Clone, Debug, PartialEq)]
        struct Draft {
            selected: Vec<String>,
            custom_answer: String,
        }
        let make = |text: &str| Draft {
            selected: vec![],
            custom_answer: text.to_owned(),
        };
        let seeded = seed_user_input_draft_answers(&questions, &IndexMap::new(), make);
        assert_eq!(
            seeded,
            IndexMap::from([
                ("editor".to_owned(), make("  Proposed commit\n\n")),
                ("empty".to_owned(), make("")),
            ])
        );
        let mut edits = seeded.clone();
        edits.insert("editor".into(), make(""));
        edits.insert(
            "empty".into(),
            Draft {
                selected: vec!["empty".into()],
                custom_answer: String::new(),
            },
        );
        assert_eq!(
            seed_user_input_draft_answers(&questions, &edits, make),
            edits
        );
        let edited = IndexMap::from([("editor".to_owned(), make("Edited commit"))]);
        assert_eq!(
            seed_user_input_draft_answers(&questions, &edited, make)["editor"],
            make("Edited commit")
        );
    }

    fn pending_projection() -> OrchestrationV2ThreadProjection {
        let mut projection = v2_projection();
        projection.runtime_requests = vec![from_json(json!({
            "id": "async-question",
            "nodeId": "async-question-node",
            "providerTurnId": null,
            "nativeRequestRef": null,
            "kind": "user_input",
            "status": "pending",
            "responseCapability": { "type": "message" },
            "createdAt": V2_NOW,
            "resolvedAt": null,
        }))];
        projection.turn_items = vec![from_json(json!({
            "id": "async-question-item",
            "threadId": "thread-v2",
            "runId": null,
            "nodeId": "async-question-node",
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": 0,
            "status": "completed",
            "title": null,
            "startedAt": V2_NOW,
            "completedAt": V2_NOW,
            "updatedAt": V2_NOW,
            "type": "user_input_request",
            "requestId": "async-question",
            "responseMode": "message",
            "questions": [{
                "id": "next",
                "header": "Next",
                "question": "What should happen next?",
                "required": true,
                "allowCustomAnswer": false,
                "options": [{ "label": "Continue", "value": "  continue  ", "description": "Resume work" }],
            }],
        }))];
        projection
    }

    fn pending(projection: &ThreadProjection) -> PendingThreadRequests {
        derive_pending_thread_requests(&projection.runtime_requests, &projection.turn_items)
    }

    #[test]
    fn keeps_message_responses_available_after_the_originating_runtime_exits() {
        let projection: ThreadProjection = pending_projection().into();
        assert!(projection.provider_sessions.is_empty());
        let requests = pending(&projection);
        assert_eq!(requests.user_inputs.len(), 1);
        let input = &requests.user_inputs[0];
        assert_eq!(input.request_id.as_str(), "async-question");
        assert_eq!(input.created_at, "2026-06-20T00:00:00.000Z");
        assert_eq!(
            input.response_capability,
            UserInputResponseCapability::Message
        );
        assert_eq!(input.response_mode, Some(UserInputResponseMode::Message));
        assert!(input.dismissible);
        assert_eq!(input.questions.len(), 1);
        let next = &input.questions[0];
        assert_eq!(next.id.as_str(), "next");
        assert_eq!(next.question.as_str(), "What should happen next?");
        assert!(!next.multi_select);
        assert_eq!(next.allow_custom_answer, Some(false));
        assert_eq!(next.options[0].value.as_deref(), Some("  continue  "));
    }

    #[test]
    fn only_marks_asynchronous_questions_as_dismissible() {
        let mut contract = pending_projection();
        contract.runtime_requests[0].response_capability =
            OrchestrationV2ResponseCapability::Live {
                provider_session_id: ProviderSessionId::new("live-session").unwrap(),
            };
        if let OrchestrationV2TurnItem::UserInputRequest { response_mode, .. } =
            &mut contract.turn_items[0]
        {
            *response_mode = None;
        }
        let live: ThreadProjection = contract.into();
        assert!(!pending(&live).user_inputs[0].dismissible);
    }

    #[test]
    fn removes_answered_requests_from_the_composer_while_retaining_their_answers() {
        let mut contract = pending_projection();
        contract.runtime_requests[0].status = OrchestrationV2RuntimeRequestStatus::Resolved;
        contract.runtime_requests[0].resolved_at = Some(V2_NOW.parse().unwrap());
        contract.runtime_requests[0].answers =
            Some(IndexMap::from([("next".to_owned(), json!("  continue  "))]));
        let answered: ThreadProjection = contract.into();
        assert_eq!(pending(&answered).user_inputs, Vec::new());
        assert_eq!(
            answered.runtime_requests[0].answers,
            Some(IndexMap::from([("next".to_owned(), json!("  continue  "))]))
        );
    }

    #[test]
    fn restores_old_text_answers_without_mutating_history_or_replacing_unchanged_rows() {
        let base: ThreadProjection = pending_projection().into();
        let item = Arc::clone(&base.turn_items[0]);
        let row: VisibleTurnItemRow = Arc::new(VisibleTurnItem {
            position: 0,
            visibility: crate::state::contracts::OrchestrationV2ProjectedTurnItemVisibility::Local,
            source_thread_id: item.thread_id().clone(),
            source_item_id: item.id().clone(),
            item: Arc::clone(&item),
        });
        let rows = Arc::new(vec![Arc::clone(&row)]);
        let mut project = QuestionHistoryProjector::new();
        let requests = Arc::clone(&base.runtime_requests);
        assert!(Arc::ptr_eq(&project.project(&rows, &requests), &rows));

        let mut resolved = (*requests[0]).clone();
        resolved.status = OrchestrationV2RuntimeRequestStatus::Resolved;
        resolved.answers = Some(IndexMap::from([("next".to_owned(), json!("Continue"))]));
        let answered_requests: Rows<OrchestrationV2RuntimeRequest> =
            Arc::new(vec![Arc::new(resolved)]);
        let result = project.project(&rows, &answered_requests);
        let OrchestrationV2TurnItem::UserInputRequest {
            question_answer: Some(answer),
            ..
        } = &*result[0].item
        else {
            panic!("the answer is restored");
        };
        assert_eq!(answer.answers["next"], json!("Continue"));
        assert!(answer.attachments_by_question_id.is_empty());
        assert_eq!(
            answer.question_text_by_id,
            Some(IndexMap::from([(
                "next".to_owned(),
                "What should happen next?".to_owned()
            )]))
        );
        assert!(matches!(
            &*row.item,
            OrchestrationV2TurnItem::UserInputRequest {
                question_answer: None,
                ..
            }
        ));
        assert!(Arc::ptr_eq(
            &project.project(&rows, &answered_requests),
            &result
        ));
        // A new rows array with the same row objects keeps the restored row.
        let copied = Arc::new(vec![Arc::clone(&row)]);
        assert!(Arc::ptr_eq(
            &project.project(&copied, &answered_requests)[0],
            &result[0]
        ));
    }
}
