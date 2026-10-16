//! Writes of `PullRequestService.ts`: actions (merge, ready, draft, close, reopen, update-branch,
//! auto-merge, revert, approve-workflows, including stack actions), comments, rewrites, reviews,
//! replies, resolutions, reactions, reviewers and labels. Each refuses what the host never
//! claimed, then what this viewer may not do, before the provider is asked.

use std::sync::Arc;

use turin_contracts::pull_request::{
    PullRequestAction, PullRequestActionInput, PullRequestCommentInput,
    PullRequestCommentUpdateInput, PullRequestCommentUpdateKind, PullRequestLabelCandidateList,
    PullRequestLabelChangeInput, PullRequestReactionInput, PullRequestRef,
    PullRequestReviewVerdict, PullRequestReviewerCandidateList, PullRequestReviewerRequestInput,
    PullRequestSubmitReviewInput, PullRequestThreadReplyInput, PullRequestThreadResolutionInput,
    PullRequestUpdateInput, PullRequestUpdateMethod,
};

use super::AsPullRequestRef;
use super::Inner;
use super::errors::{
    LABEL_CHANGE_REFUSAL, PullRequestError, REVIEWER_REQUEST_REFUSAL, action_access_refusal,
    operation_error, to_pull_request_error, verdict_label, wire,
};
use crate::shared::js::js_trim;
use crate::source_control_core::pull_request_provider::{
    CommentInput, CommentUpdateKind, PullRequestOperation, ReplyToThreadInput, RunActionInput,
    SetLabelsInput, SetReactionInput, SetReviewerRequestInput, SetThreadResolutionInput,
    SubmitReviewInput, UpdateChangeRequestInput, UpdateCommentInput,
};

fn is_blank(value: &str) -> bool {
    js_trim(value).is_empty()
}

impl Inner {
    /// Runs the action and says which repository spelling the epochs should be bumped against.
    pub(crate) async fn run_action(
        self: &Arc<Self>,
        input: &PullRequestActionInput,
    ) -> Result<String, PullRequestError> {
        let reference = input.pull_request_ref();
        let project = self.require_project(&reference).await?;
        let capabilities = project.api.capabilities();
        if input.stack_number.is_some()
            && (capabilities.stack_actions != Some(true)
                || !matches!(
                    input.action,
                    PullRequestAction::Merge | PullRequestAction::UpdateBranch
                )
                || input.expected_stack_heads.is_none()
                || (input.action == PullRequestAction::UpdateBranch
                    && input.update_method != Some(PullRequestUpdateMethod::Rebase)))
        {
            return Err(operation_error(
                "runAction",
                "This stack action is not supported or has no expected head revision.",
            ));
        }
        // The surface hides what a host cannot do, and this refuses it as well: a request that
        // reached here anyway must not be handed to a provider that never claimed the action.
        if !capabilities.actions.contains(&input.action) {
            return Err(operation_error(
                "runAction",
                format!("This host cannot {} a change request.", wire(&input.action)),
            ));
        }
        // A strategy the host does not offer must be refused rather than passed on: every
        // provider maps an unrecognised method to its own default, so asking Azure DevOps to
        // rebase would quietly merge instead of failing.
        if let Some(merge_method) = input.merge_method
            && !capabilities.merge_methods.contains(&merge_method)
        {
            return Err(operation_error(
                "runAction",
                format!(
                    "This host cannot merge with the {} strategy.",
                    wire(&merge_method)
                ),
            ));
        }
        // The same for the way a stale branch is brought up to date: a host that only merges
        // must not be asked to rebase and left to pick something else.
        if let Some(update_method) = input.update_method
            && !capabilities
                .update_methods
                .as_deref()
                .unwrap_or_default()
                .contains(&update_method)
        {
            return Err(operation_error(
                "runAction",
                format!(
                    "This host cannot update a branch by {}.",
                    wire(&update_method)
                ),
            ));
        }
        // What the host can do and what this account may ask of it are two questions, and both
        // have to say yes. The second is asked last, because it costs a request and the checks
        // above do not.
        let viewer = self
            .viewer_permissions_of(
                &project,
                &reference,
                "runAction",
                input.action == PullRequestAction::UpdateBranch,
            )
            .await?;
        let stack_rebase =
            input.stack_number.is_some() && input.action == PullRequestAction::UpdateBranch;
        let allowed = if stack_rebase {
            viewer.stack_rebase == Some(true)
        } else {
            viewer.actions.contains(&input.action)
        };
        if !allowed {
            return Err(operation_error(
                "runAction",
                action_access_refusal(input.action),
            ));
        }
        if !stack_rebase
            && let Some(update_method) = input.update_method
            && !viewer
                .update_methods
                .as_deref()
                .unwrap_or_default()
                .contains(&update_method)
        {
            return Err(operation_error(
                "runAction",
                action_access_refusal(PullRequestAction::UpdateBranch),
            ));
        }
        let remove_agent_credits_on_merge = if project
            .api
            .supports(PullRequestOperation::MergeMessageRewrite)
            && input.stack_number.is_none()
            && matches!(
                input.action,
                PullRequestAction::Merge | PullRequestAction::EnableAutoMerge
            ) {
            self.settings
                .remove_agent_credits_on_merge(&project.project.id)
                .await
                .map_err(|_| operation_error("runAction", "Could not read merge settings."))?
        } else {
            false
        };
        let result = project
            .api
            .run_action(RunActionInput {
                request: project.provider_ref(input.number.get()),
                action: input.action,
                stack_number: input.stack_number.map(|number| number.get()),
                expected_stack_heads: input.expected_stack_heads.clone(),
                remove_agent_credits_on_merge,
                merge_method: input.merge_method,
                update_method: input.update_method,
            })
            .await;
        // Once the authorized provider action starts, a failure may leave partial remote
        // updates. Validation and permission failures above changed nothing.
        if input.stack_number.is_some() {
            self.refresh_after_turn(&project.project.id).await;
        }
        result.map_err(|error| to_pull_request_error("runAction", error))?;
        Ok(if project.repository_key.is_some() {
            input.repository.as_str().trim().to_owned()
        } else {
            project.repository.clone()
        })
    }

    pub(crate) async fn comment(
        self: &Arc<Self>,
        input: &PullRequestCommentInput,
    ) -> Result<(), PullRequestError> {
        // The contract keeps the body verbatim because it is markdown, so the "did the user
        // actually write something" check lives here.
        if is_blank(&input.body) {
            return Err(operation_error("comment", "A comment cannot be empty."));
        }
        let reference = input.pull_request_ref();
        let project = self.require_project(&reference).await?;
        if !project.api.capabilities().comment {
            return Err(operation_error(
                "comment",
                "This host cannot post a comment on a change request.",
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, &reference, "comment", false)
            .await?;
        if !viewer.comment {
            return Err(operation_error(
                "comment",
                "You need write access on this repository to comment on a change request.",
            ));
        }
        project
            .api
            .comment(CommentInput {
                request: project.provider_ref(input.number.get()),
                body: input.body.clone(),
            })
            .await
            .map_err(|error| to_pull_request_error("comment", error))
    }

    /// Rewriting the change request's own words, and rewriting a remark, are both left to the
    /// host to allow or refuse. Neither is a question a permission read answers: every host lets
    /// the person who wrote something rewrite it whatever access they have otherwise, and none of
    /// them reports that as a permission — so a check here could only guess, and a wrong guess
    /// takes the control away from the one person certain to be allowed.
    pub(crate) async fn update(
        self: &Arc<Self>,
        input: &PullRequestUpdateInput,
    ) -> Result<(), PullRequestError> {
        let project = self.require_project(&input.pull_request_ref()).await?;
        if project
            .api
            .capabilities()
            .edit
            .as_ref()
            .is_none_or(|edit| !edit.change_request)
            || !project
                .api
                .supports(PullRequestOperation::UpdateChangeRequest)
        {
            return Err(operation_error(
                "update",
                "This host cannot rewrite a change request.",
            ));
        }
        if input.title.is_none() && input.body.is_none() {
            return Err(operation_error("update", "Nothing was changed."));
        }
        project
            .api
            .update_change_request(UpdateChangeRequestInput {
                request: project.provider_ref(input.number.get()),
                title: input.title.as_ref().map(|title| title.as_str().to_owned()),
                body: input.body.clone(),
            })
            .await
            .map_err(|error| to_pull_request_error("update", error))
    }

    pub(crate) async fn update_comment(
        self: &Arc<Self>,
        input: &PullRequestCommentUpdateInput,
    ) -> Result<(), PullRequestError> {
        if is_blank(&input.body) {
            return Err(operation_error(
                "updateComment",
                "A comment cannot be empty.",
            ));
        }
        let project = self.require_project(&input.pull_request_ref()).await?;
        if project
            .api
            .capabilities()
            .edit
            .as_ref()
            .is_none_or(|edit| !edit.comment)
            || !project.api.supports(PullRequestOperation::UpdateComment)
        {
            return Err(operation_error(
                "updateComment",
                "This host cannot rewrite a comment.",
            ));
        }
        project
            .api
            .update_comment(UpdateCommentInput {
                request: project.provider_ref(input.number.get()),
                comment_id: input.comment_id.as_str().to_owned(),
                kind: match input.kind {
                    PullRequestCommentUpdateKind::IssueComment => CommentUpdateKind::IssueComment,
                    PullRequestCommentUpdateKind::ReviewComment => CommentUpdateKind::ReviewComment,
                },
                body: input.body.clone(),
            })
            .await
            .map_err(|error| to_pull_request_error("updateComment", error))
    }

    pub(crate) async fn submit_review(
        self: &Arc<Self>,
        input: &PullRequestSubmitReviewInput,
    ) -> Result<(), PullRequestError> {
        let reference = input.pull_request_ref();
        let project = self.require_project(&reference).await?;
        let review = &project.api.capabilities().review;
        let refuse = |detail: String| operation_error("submitReview", detail);
        // The surface hides what a host cannot do, and this refuses it as well: a request that
        // reached here anyway must not be handed to a provider that never claimed it.
        if !review.verdicts.contains(&input.verdict) {
            return Err(refuse(format!(
                "This host cannot {} a change request.",
                verdict_label(input.verdict)
            )));
        }
        if !input.comments.is_empty() && !review.inline_comment {
            return Err(refuse(
                "This host cannot comment on a line of a change request.".to_owned(),
            ));
        }
        // A verdict with nothing attached to it is a request every host rejects, and doing so
        // here says which of the two is missing rather than reporting the host's refusal.
        if input.verdict != PullRequestReviewVerdict::Approve
            && is_blank(&input.body)
            && input.comments.is_empty()
        {
            return Err(refuse(
                "A review needs a summary or at least one comment.".to_owned(),
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, &reference, "submitReview", false)
            .await?;
        if !viewer.verdicts.contains(&input.verdict) {
            return Err(refuse(format!(
                "You need write access on this repository to {} a change request.",
                verdict_label(input.verdict)
            )));
        }
        if !input.comments.is_empty() && !viewer.comment {
            return Err(refuse(
                "You need write access on this repository to comment on a line of a change request."
                    .to_owned(),
            ));
        }
        project
            .api
            .submit_review(SubmitReviewInput {
                request: project.provider_ref(input.number.get()),
                verdict: input.verdict,
                body: input.body.clone(),
                comments: input.comments.clone(),
            })
            .await
            .map_err(|error| to_pull_request_error("submitReview", error))
    }

    pub(crate) async fn reply_to_thread(
        self: &Arc<Self>,
        input: &PullRequestThreadReplyInput,
    ) -> Result<(), PullRequestError> {
        if is_blank(&input.body) {
            return Err(operation_error("replyToThread", "A reply cannot be empty."));
        }
        let reference = input.pull_request_ref();
        let project = self.require_project(&reference).await?;
        if !project.api.capabilities().review.reply {
            return Err(operation_error(
                "replyToThread",
                "This host cannot reply to a review conversation.",
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, &reference, "replyToThread", false)
            .await?;
        if !viewer.comment {
            return Err(operation_error(
                "replyToThread",
                "You need write access on this repository to reply to a review conversation.",
            ));
        }
        project
            .api
            .reply_to_thread(ReplyToThreadInput {
                request: project.provider_ref(input.number.get()),
                thread_id: input.thread_id.as_str().to_owned(),
                body: input.body.clone(),
            })
            .await
            .map_err(|error| to_pull_request_error("replyToThread", error))
    }

    pub(crate) async fn set_thread_resolution(
        self: &Arc<Self>,
        input: &PullRequestThreadResolutionInput,
    ) -> Result<(), PullRequestError> {
        let reference = input.pull_request_ref();
        let project = self.require_project(&reference).await?;
        if !project.api.capabilities().review.resolve {
            return Err(operation_error(
                "setThreadResolution",
                "This host cannot resolve a review conversation.",
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, &reference, "setThreadResolution", false)
            .await?;
        if !viewer.resolve {
            return Err(operation_error(
                "setThreadResolution",
                "You need write access on this repository, or to have opened this change request, to resolve a review conversation.",
            ));
        }
        project
            .api
            .set_thread_resolution(SetThreadResolutionInput {
                request: project.provider_ref(input.number.get()),
                thread_id: input.thread_id.as_str().to_owned(),
                resolved: input.resolved,
            })
            .await
            .map_err(|error| to_pull_request_error("setThreadResolution", error))
    }

    /// Reacting is gated on the host alone. Every host with reactions takes one from whoever can
    /// read the change request, so there is no access left to check that reading it has not
    /// already settled.
    pub(crate) async fn set_reaction(
        self: &Arc<Self>,
        input: &PullRequestReactionInput,
    ) -> Result<(), PullRequestError> {
        let project = self.require_project(&input.pull_request_ref()).await?;
        if project.api.capabilities().reactions != Some(true) {
            return Err(operation_error(
                "setReaction",
                "This host has no reactions.",
            ));
        }
        project
            .api
            .set_reaction(SetReactionInput {
                request: project.provider_ref(input.number.get()),
                subject_id: input
                    .subject_id
                    .as_ref()
                    .map(|subject| subject.as_str().to_owned()),
                content: input.content,
                reacted: input.reacted,
            })
            .await
            .map_err(|error| to_pull_request_error("setReaction", error))
    }

    /// Who may be asked is only ever wanted by somebody about to ask, because the menu it fills
    /// is the one the request is made from. So the same permission guards both: a page that could
    /// open the menu without it would offer a list whose every press was going to be turned down.
    pub(crate) async fn reviewer_candidates(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestReviewerCandidateList, PullRequestError> {
        let project = self.require_project(input).await?;
        if !project.api.capabilities().reviewers.list_candidates {
            return Err(operation_error(
                "reviewerCandidates",
                "This host cannot say who may review a change request.",
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, input, "reviewerCandidates", false)
            .await?;
        if !viewer.request_reviewers {
            return Err(operation_error(
                "reviewerCandidates",
                REVIEWER_REQUEST_REFUSAL,
            ));
        }
        project
            .api
            .list_reviewer_candidates(project.provider_ref(input.number.get()))
            .await
            .map_err(|error| to_pull_request_error("reviewerCandidates", error))
    }

    pub(crate) async fn request_reviewers(
        self: &Arc<Self>,
        input: &PullRequestReviewerRequestInput,
    ) -> Result<(), PullRequestError> {
        let reference = input.pull_request_ref();
        let project = self.require_project(&reference).await?;
        if !project.api.capabilities().reviewers.request {
            return Err(operation_error(
                "requestReviewers",
                "This host cannot ask somebody for a review.",
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, &reference, "requestReviewers", false)
            .await?;
        if !viewer.request_reviewers {
            return Err(operation_error(
                "requestReviewers",
                REVIEWER_REQUEST_REFUSAL,
            ));
        }
        project
            .api
            .set_reviewer_request(SetReviewerRequestInput {
                request: project.provider_ref(input.number.get()),
                reviewers: input
                    .reviewers
                    .iter()
                    .map(|reviewer| (reviewer.id.as_str().to_owned(), reviewer.kind))
                    .collect(),
                requested: input.requested,
            })
            .await
            .map_err(|error| to_pull_request_error("requestReviewers", error))
    }

    /// The labels, like the reviewer candidates, are wanted only by somebody about to change
    /// them, so the same permission guards the list and the change.
    pub(crate) async fn label_candidates(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestLabelCandidateList, PullRequestError> {
        let project = self.require_project(input).await?;
        if project.api.capabilities().labels != Some(true)
            || !project
                .api
                .supports(PullRequestOperation::ListLabelCandidates)
        {
            return Err(operation_error(
                "labelCandidates",
                "This host cannot change the labels on a change request.",
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, input, "labelCandidates", false)
            .await?;
        if viewer.labels == Some(false) {
            return Err(operation_error("labelCandidates", LABEL_CHANGE_REFUSAL));
        }
        project
            .api
            .list_label_candidates(project.provider_ref(input.number.get()))
            .await
            .map_err(|error| to_pull_request_error("labelCandidates", error))
    }

    pub(crate) async fn set_labels(
        self: &Arc<Self>,
        input: &PullRequestLabelChangeInput,
    ) -> Result<(), PullRequestError> {
        let reference = input.pull_request_ref();
        let project = self.require_project(&reference).await?;
        if project.api.capabilities().labels != Some(true)
            || !project.api.supports(PullRequestOperation::SetLabels)
        {
            return Err(operation_error(
                "setLabels",
                "This host cannot change the labels on a change request.",
            ));
        }
        let viewer = self
            .viewer_permissions_of(&project, &reference, "setLabels", false)
            .await?;
        if viewer.labels == Some(false) {
            return Err(operation_error("setLabels", LABEL_CHANGE_REFUSAL));
        }
        project
            .api
            .set_labels(SetLabelsInput {
                request: project.provider_ref(input.number.get()),
                labels: input
                    .labels
                    .iter()
                    .map(|label| label.as_str().to_owned())
                    .collect(),
                applied: input.applied,
            })
            .await
            .map_err(|error| to_pull_request_error("setLabels", error))
    }
}
