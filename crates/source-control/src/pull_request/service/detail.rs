//! Single-pull-request reads of `PullRequestService.ts`: summary, stack, detail, preview,
//! activity, checks, watch fingerprint, review thread pages, and the state-change announcements a
//! detail read makes.

use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use turin_contracts::pull_request::{
    PullRequestActivity, PullRequestChecks, PullRequestDetail, PullRequestPreview, PullRequestRef,
    PullRequestReportStateInput, PullRequestStack, PullRequestStackLayer, PullRequestState,
    PullRequestSummary, PullRequestThreadCommentsInput, PullRequestThreadCommentsResult,
};
use turin_contracts::thread_pull_request::ThreadPullRequestKey;

use super::cache::{Scoped, ServeMode};
use super::epochs::{
    CredentialRef, REF_EPOCH_CAPACITY, ref_cache_key, ref_of_cache_key, ref_scope,
};
use super::errors::{
    PullRequestError, operation_error, operation_error_with_cause, positive, tne, tne_opt,
    to_pull_request_error,
};
use super::{DETAIL_CACHE_TTL_MS, Inner, MERGED_DETAIL_CACHE_TTL_MS, REPORTED_STATE_DEDUPE_MS};
use crate::shared::js::encode_uri_component;
use crate::source_control_core::pull_request_provider::{
    GetChangeRequestStackInput, GetReviewThreadCommentsInput,
    ProviderChangeRequestWatchFingerprint, PullRequestOperation,
};

/// How long a detail-shaped answer is held for, by the state it found the change request in.
pub(crate) fn detail_time_to_live(state: Option<PullRequestState>) -> i64 {
    if state == Some(PullRequestState::Merged) {
        MERGED_DETAIL_CACHE_TTL_MS
    } else {
        DETAIL_CACHE_TTL_MS
    }
}

/// `PullRequestReading`: `Pick<PullRequestSummary, "state" | "updatedAt" | "observedAt">`
#[derive(Debug, Clone)]
pub(crate) struct PullRequestReading {
    pub state: PullRequestState,
    pub updated_at: String,
    pub observed_at: Option<f64>,
}

impl From<&PullRequestSummary> for PullRequestReading {
    fn from(summary: &PullRequestSummary) -> Self {
        Self {
            state: summary.state,
            updated_at: summary.updated_at.clone(),
            observed_at: summary.observed_at,
        }
    }
}

/// Whether `next` is a newer reading of a pull request than `current`. Merged is final.
pub(crate) fn supersedes_reading(
    current: Option<&PullRequestReading>,
    next: &PullRequestReading,
) -> bool {
    let Some(current) = current else {
        return true;
    };
    if current.state == PullRequestState::Merged && next.state != PullRequestState::Merged {
        return false;
    }
    if next.updated_at != current.updated_at {
        return next.updated_at > current.updated_at;
    }
    next.observed_at.unwrap_or(f64::NEG_INFINITY)
        >= current.observed_at.unwrap_or(f64::NEG_INFINITY)
}

/// The preview a hover card needs, out of a detail that is already held.
pub(crate) fn preview_fields(value: &PullRequestPreview) -> PullRequestPreview {
    PullRequestPreview {
        project_id: value.project_id.clone(),
        repository: value.repository.clone(),
        number: value.number,
        title: value.title.clone(),
        url: value.url.clone(),
        author: value.author.clone(),
        state: value.state,
        is_draft: value.is_draft,
        created_at: value.created_at.clone(),
    }
}

pub(crate) fn preview_of_detail(detail: &PullRequestDetail) -> PullRequestPreview {
    PullRequestPreview {
        project_id: detail.project_id.clone(),
        repository: detail.repository.clone(),
        number: detail.number,
        title: detail.title.clone(),
        url: detail.url.clone(),
        author: detail.author.clone(),
        state: detail.state,
        is_draft: detail.is_draft,
        created_at: detail.created_at.clone(),
    }
}

/// Detail does not carry review/check summaries. Keep the last summary observation.
fn summary_from_detail(
    detail: &PullRequestDetail,
    previous: Option<&PullRequestSummary>,
) -> PullRequestSummary {
    PullRequestSummary {
        provider: detail.provider.clone(),
        project_id: detail.project_id.clone(),
        repository: detail.repository.clone(),
        number: detail.number,
        title: detail.title.clone(),
        url: detail.url.clone(),
        state: detail.state,
        is_draft: Some(detail.is_draft),
        head_branch: detail.head_branch.clone(),
        base_branch: detail.base_branch.clone(),
        closed_at: Some(detail.closed_at.clone()),
        merged_at: Some(detail.merged_at.clone()),
        updated_at: detail.updated_at.clone(),
        observed_at: detail.observed_at,
        author: Some(detail.author.clone()),
        additions: Some(detail.additions),
        deletions: Some(detail.deletions),
        changed_files: Some(detail.changed_files),
        review_decision: previous.and_then(|p| p.review_decision),
        checks_state: previous.and_then(|p| p.checks_state),
        mergeability: Some(detail.mergeability),
        stack: previous.and_then(|p| p.stack.clone()),
    }
}

impl Inner {
    // -----------------------------------------------------------------------------------------
    // Uncached reads
    // -----------------------------------------------------------------------------------------

    pub(crate) async fn summary_uncached(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestSummary, PullRequestError> {
        let project = self.require_project(input).await?;
        let provider_input = project.provider_ref(input.number.get());
        // Captured before the provider read so a slow response keeps its original freshness
        // through caches.
        let observed_at = self.clock.now_millis();
        let observed = Some(observed_at as f64);
        if project
            .api
            .supports(PullRequestOperation::GetChangeRequestSummary)
        {
            let value = project
                .api
                .get_change_request_summary(provider_input)
                .await
                .map_err(|error| to_pull_request_error("summary", error))?;
            return Ok(PullRequestSummary {
                provider: project.api.kind(),
                project_id: project.project.id.clone(),
                repository: tne(&project.repository),
                number: positive(value.number),
                title: tne(&value.title),
                url: tne(&value.url),
                state: value.state,
                head_branch: tne(&value.head_branch),
                base_branch: tne(&value.base_branch),
                closed_at: Some(value.closed_at.clone().flatten()),
                merged_at: Some(value.merged_at.clone().flatten()),
                updated_at: value.updated_at.clone(),
                observed_at: observed,
                is_draft: value.is_draft,
                author: value.author.clone(),
                additions: value.additions,
                deletions: value.deletions,
                changed_files: value.changed_files,
                review_decision: value.review_decision,
                checks_state: value.checks_state,
                mergeability: value.mergeability,
                stack: value.stack.clone(),
            });
        }
        let value = project
            .api
            .get_change_request(provider_input)
            .await
            .map_err(|error| to_pull_request_error("summary", error))?;
        Ok(PullRequestSummary {
            provider: project.api.kind(),
            project_id: project.project.id.clone(),
            repository: tne(&project.repository),
            number: positive(value.number),
            title: tne(&value.title),
            url: tne(&value.url),
            state: value.state,
            head_branch: tne(&value.head_branch),
            base_branch: tne(&value.base_branch),
            closed_at: Some(value.closed_at.clone()),
            merged_at: Some(value.merged_at.clone()),
            updated_at: value.updated_at.clone(),
            observed_at: observed,
            is_draft: Some(value.is_draft),
            author: Some(value.author.clone()),
            additions: Some(value.additions),
            deletions: Some(value.deletions),
            changed_files: Some(value.changed_files),
            review_decision: value.review_decision,
            checks_state: value.checks_state,
            mergeability: Some(value.mergeability),
            stack: value.stack.clone().map(Some),
        })
    }

    async fn stack_uncached(
        self: &Arc<Self>,
        input: &PullRequestRef,
        include_details: Option<bool>,
    ) -> Result<Option<PullRequestStack>, PullRequestError> {
        let project = self.require_project(input).await?;
        if !project
            .api
            .supports(PullRequestOperation::GetChangeRequestStack)
        {
            return Ok(None);
        }
        let stack = project
            .api
            .get_change_request_stack(GetChangeRequestStackInput {
                request: project.provider_ref(input.number.get()),
                include_details: include_details != Some(false),
            })
            .await
            .map_err(|error| to_pull_request_error("stack", error))?;
        Ok(stack.map(|stack| PullRequestStack {
            id: tne(&stack.id),
            number: positive(stack.number),
            url: tne(&stack.url),
            base: tne(&stack.base),
            layers: stack
                .layers
                .iter()
                .map(|layer| PullRequestStackLayer {
                    number: positive(layer.number),
                    title: layer.title.clone(),
                    is_draft: layer.is_draft,
                    head_sha: tne_opt(layer.head_sha.as_deref()),
                    head_branch: tne(&layer.head_branch),
                    state: layer.state,
                })
                .collect(),
        }))
    }

    async fn detail_uncached(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestDetail, PullRequestError> {
        let project = self.require_project(input).await?;
        let read = async {
            let observed_at = self.clock.now_millis();
            let value = project
                .api
                .get_change_request(project.provider_ref(input.number.get()))
                .await
                .map_err(|error| to_pull_request_error("detail", error))?;
            Ok::<_, PullRequestError>((value, observed_at))
        };
        let (read, viewer) = futures::join!(read, self.viewer_of(&project));
        let (value, observed_at) = read?;
        Ok(PullRequestDetail {
            provider: project.api.kind(),
            capabilities: project.api.capabilities().clone(),
            project_id: project.project.id.clone(),
            project_title: project.project.title.clone(),
            workspace_root: project.project.workspace_root.clone(),
            repository: tne(&project.repository),
            number: positive(value.number),
            title: tne(&value.title),
            body: value.body.clone(),
            url: tne(&value.url),
            author: value.author.clone(),
            state: value.state,
            is_draft: value.is_draft,
            mergeability: value.mergeability,
            additions: value.additions,
            deletions: value.deletions,
            changed_files: value.changed_files,
            head_branch: tne(&value.head_branch),
            head_repository_name_with_owner: value
                .head_repository_name_with_owner
                .clone()
                .map(|owner| owner.map(tne)),
            head_sha: value
                .head_sha
                .clone()
                .flatten()
                .filter(|sha| !sha.is_empty())
                .map(tne),
            base_branch: tne(&value.base_branch),
            created_at: value.created_at.clone(),
            updated_at: value.updated_at.clone(),
            observed_at: Some(observed_at as f64),
            merged_at: value.merged_at.clone(),
            closed_at: value.closed_at.clone(),
            reviewers: value.reviewers.clone(),
            labels: value.labels.clone(),
            checks: value.checks.clone(),
            merge_capabilities: value.merge_capabilities,
            viewer_permissions: value.viewer_permissions.clone(),
            viewer: viewer.filter(|viewer| !viewer.trim().is_empty()).map(tne),
            base_comparison: value.base_comparison,
            behind_by: value.behind_by,
            auto_merge_enabled: value.auto_merge_enabled,
            auto_merge_method: value.auto_merge_method,
            workflow_approvals_required: value.workflow_approvals_required,
        })
    }

    async fn preview_uncached(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestPreview, PullRequestError> {
        let project = self.require_project(input).await?;
        let request = project.provider_ref(input.number.get());
        let map_error = |error| to_pull_request_error("preview", error);
        if project
            .api
            .supports(PullRequestOperation::GetChangeRequestPreview)
        {
            let value = project
                .api
                .get_change_request_preview(request)
                .await
                .map_err(map_error)?;
            return Ok(preview_fields(&PullRequestPreview {
                project_id: project.project.id.clone(),
                repository: tne(&project.repository),
                number: positive(value.number),
                title: tne(&value.title),
                url: tne(&value.url),
                author: value.author.clone(),
                state: value.state,
                is_draft: value.is_draft,
                created_at: value.created_at.clone(),
            }));
        }
        let value = project
            .api
            .get_change_request(request)
            .await
            .map_err(map_error)?;
        Ok(preview_fields(&PullRequestPreview {
            project_id: project.project.id.clone(),
            repository: tne(&project.repository),
            number: positive(value.number),
            title: tne(&value.title),
            url: tne(&value.url),
            author: value.author.clone(),
            state: value.state,
            is_draft: value.is_draft,
            created_at: value.created_at.clone(),
        }))
    }

    async fn activity_uncached(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestActivity, PullRequestError> {
        let project = self.require_project(input).await?;
        let activity = project
            .api
            .get_change_request_activity(project.provider_ref(input.number.get()))
            .await
            .map_err(|error| to_pull_request_error("activity", error))?;
        Ok(PullRequestActivity {
            author: activity.author,
            reviewers: activity.reviewers,
            comments: activity.comments,
            comment_count: activity.comment_count,
            comments_truncated: activity.comments_truncated,
            review_threads_truncated: activity.review_threads_truncated,
            review_threads: activity.review_threads,
            commits: activity.commits,
            reactions: activity.reactions,
        })
    }

    pub(crate) async fn thread_comments(
        self: &Arc<Self>,
        input: &PullRequestThreadCommentsInput,
    ) -> Result<PullRequestThreadCommentsResult, PullRequestError> {
        let reference = super::AsPullRequestRef::pull_request_ref(input);
        let project = self.require_project(&reference).await?;
        if !project
            .api
            .supports(PullRequestOperation::GetReviewThreadComments)
        {
            return Err(operation_error(
                "threadComments",
                "This host does not page review thread comments.",
            ));
        }
        project
            .api
            .get_review_thread_comments(GetReviewThreadCommentsInput {
                request: project.provider_ref(input.number.get()),
                thread_id: input.thread_id.as_str().to_owned(),
                cursor: input.cursor.as_str().to_owned(),
            })
            .await
            .map_err(|error| to_pull_request_error("threadComments", error))
    }

    // -----------------------------------------------------------------------------------------
    // Persisted reads
    // -----------------------------------------------------------------------------------------

    /// A read answered from the persisted cache when it holds one, and from `read` when it does
    /// not; `read` runs at most once however many layers ask for its answer.
    async fn persisted_read<A>(
        self: &Arc<Self>,
        input: &CredentialRef,
        operation: &str,
        read: BoxFuture<'static, Result<A, PullRequestError>>,
    ) -> Result<A, PullRequestError>
    where
        A: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    {
        let project = self.require_project(input).await?;
        let key = [
            operation.to_owned(),
            project.api.kind().as_str().to_owned(),
            project.host.to_lowercase(),
            project.repository.to_lowercase(),
            project.project.id.as_str().to_owned(),
            project.project.workspace_root.as_str().to_owned(),
            input.number.get().to_string(),
            input
                .expected_account_id
                .as_ref()
                .map_or_else(String::new, |id| id.as_str().to_owned()),
            input.credential_namespace.clone().unwrap_or_default(),
        ]
        .iter()
        .map(|part| encode_uri_component(part))
        .collect::<Vec<_>>()
        .join(":");
        let lookup = Scoped::capture().run(read).boxed().shared();
        let encoded_read = {
            let lookup = lookup.clone();
            async move {
                let value = lookup.await?;
                serde_json::to_string(&value).map_err(|cause| {
                    operation_error_with_cause(
                        "cache",
                        "Could not encode PR cache data.",
                        serde_json::Value::String(cause.to_string()),
                    )
                })
            }
            .boxed()
        };
        let payload = self
            .read_cache
            .get(
                &key,
                encoded_read,
                &[
                    format!("project:{}", input.project_id),
                    ref_scope(&input.reference),
                ],
            )
            .await?;
        match serde_json::from_str::<A>(&payload) {
            Ok(decoded) => Ok(decoded),
            Err(_) => lookup.await,
        }
    }

    /// `shouldReplaceHeldSummary`
    fn should_replace_held_summary(&self, key: &str, next: &PullRequestSummary) -> bool {
        let held = self.last_good_summary.peek(key);
        supersedes_reading(
            held.as_ref().map(PullRequestReading::from).as_ref(),
            &next.into(),
        )
    }

    pub(crate) async fn summary(
        self: &Arc<Self>,
        input: &CredentialRef,
        recover_transient_failure: Option<bool>,
    ) -> Result<PullRequestSummary, PullRequestError> {
        let key = self.ref_key(input);
        let held = self.last_good_summary.peek(&key);
        if let Some(held) = held
            && input.allow_stale != Some(false)
            && (recover_transient_failure != Some(false) || held.state == PullRequestState::Merged)
        {
            return Ok(held);
        }
        let inner = self.clone();
        let reference = input.reference.clone();
        let value = self
            .persisted_read(
                input,
                "summary",
                async move { inner.summary_uncached(&reference).await }.boxed(),
            )
            .await?;
        if self.should_replace_held_summary(&key, &value) {
            self.last_good_summary.record(&key, value.clone());
        }
        Ok(value)
    }

    /// The host-native stack the pull request belongs to, or `None` when it is not in one or the
    /// host keeps no such object. Cached like a summary; a stack changes about as often.
    pub(crate) async fn stack(
        self: &Arc<Self>,
        input: &CredentialRef,
        include_details: Option<bool>,
    ) -> Result<Option<PullRequestStack>, PullRequestError> {
        let inner = self.clone();
        let reference = input.reference.clone();
        self.persisted_read(
            input,
            &format!("stack:{}", include_details != Some(false)),
            async move { inner.stack_uncached(&reference, include_details).await }.boxed(),
        )
        .await
    }

    // -----------------------------------------------------------------------------------------
    // Cached reads
    // -----------------------------------------------------------------------------------------

    pub(crate) fn ref_key(&self, input: &CredentialRef) -> String {
        ref_cache_key(&self.state.lock().epochs, input)
    }

    pub(crate) async fn detail(
        self: &Arc<Self>,
        input: &CredentialRef,
    ) -> Result<PullRequestDetail, PullRequestError> {
        let key = self.ref_key(input);
        // Record the summary from a host or cache read, not the stale value `serve_held` returns
        // immediately. Skip the write when that read is older than a later strict summary —
        // display reuse would otherwise keep the regression and never ask the host again.
        let read: BoxFuture<'static, Result<PullRequestDetail, PullRequestError>> = {
            let inner = self.clone();
            let key = key.clone();
            let input = input.clone();
            async move {
                let value = inner
                    .detail_cache
                    .get(&key, {
                        let inner = inner.clone();
                        let key = key.clone();
                        move || {
                            async move {
                                let value = inner
                                    .detail_uncached(&ref_of_cache_key(&key).reference)
                                    .await?;
                                let stats_key = inner.stats_cache_key(&key);
                                inner.record_stats(
                                    stats_key,
                                    turin_contracts::pull_request::PullRequestDiffStat {
                                        project_id: value.project_id.clone(),
                                        repository: value.repository.clone(),
                                        number: value.number,
                                        additions: value.additions,
                                        deletions: value.deletions,
                                    },
                                    inner.clock.now_millis(),
                                );
                                Ok(value)
                            }
                            .boxed()
                        }
                    })
                    .await?;
                if value.state == PullRequestState::Merged {
                    inner.state.lock().epochs.note_merged(&input.reference);
                }
                let summary =
                    summary_from_detail(&value, inner.last_good_summary.peek(&key).as_ref());
                inner.note_detail_reading(&input.reference, &summary);
                if inner.should_replace_held_summary(&key, &summary) {
                    inner.last_good_summary.record(&key, summary);
                }
                Ok(value)
            }
            .boxed()
        };
        if input.allow_stale == Some(false) {
            let value = read.await?;
            self.last_good_detail.record(&key, value.clone());
            Ok(value)
        } else {
            self.last_good_detail
                .serve_held(&key, read, ServeMode::Revalidate)
                .await
        }
    }

    pub(crate) async fn preview(
        self: &Arc<Self>,
        input: &CredentialRef,
    ) -> Result<PullRequestPreview, PullRequestError> {
        let key = self.ref_key(input);
        if let Some(detail) = self.detail_cache.get_success(&key) {
            return Ok(preview_of_detail(&detail));
        }
        let inner = self.clone();
        let key_owned = key.clone();
        self.preview_cache
            .get(&key, move || {
                async move {
                    inner
                        .preview_uncached(&ref_of_cache_key(&key_owned).reference)
                        .await
                }
                .boxed()
            })
            .await
    }

    pub(crate) async fn activity(
        self: &Arc<Self>,
        input: &CredentialRef,
    ) -> Result<PullRequestActivity, PullRequestError> {
        let key = self.ref_key(input);
        let inner = self.clone();
        let key_owned = key.clone();
        self.activity_cache
            .get(&key, move || {
                async move {
                    inner
                        .activity_uncached(&ref_of_cache_key(&key_owned).reference)
                        .await
                }
                .boxed()
            })
            .await
    }

    pub(crate) async fn checks(
        self: &Arc<Self>,
        input: &CredentialRef,
    ) -> Result<Option<PullRequestChecks>, PullRequestError> {
        let key = self.ref_key(input);
        let inner = self.clone();
        let key_owned = key.clone();
        self.checks_cache
            .get(&key, move || {
                async move {
                    let reference = ref_of_cache_key(&key_owned).reference;
                    let project = inner.require_project(&reference).await?;
                    if !project
                        .api
                        .supports(PullRequestOperation::GetChangeRequestChecks)
                    {
                        return Ok(None);
                    }
                    project
                        .api
                        .get_change_request_checks(project.provider_ref(reference.number.get()))
                        .await
                        .map(Some)
                        .map_err(|error| to_pull_request_error("checks", error))
                }
                .boxed()
            })
            .await
    }

    /// What a pull request watch compares between passes, so it reads detail and activity only
    /// when something moved. `None` when the host has no fingerprint or gave none for this one.
    pub(crate) async fn watch_fingerprint(
        self: &Arc<Self>,
        input: &CredentialRef,
    ) -> Result<Option<ProviderChangeRequestWatchFingerprint>, PullRequestError> {
        let key = self.ref_key(input);
        let inner = self.clone();
        let key_owned = key.clone();
        self.watch_cache
            .get(&key, move || {
                async move {
                    let reference = ref_of_cache_key(&key_owned).reference;
                    let project = inner.require_project(&reference).await?;
                    if !project
                        .api
                        .supports(PullRequestOperation::GetChangeRequestWatchFingerprint)
                    {
                        return Ok(None);
                    }
                    project
                        .api
                        .get_change_request_watch_fingerprint(
                            project.provider_ref(reference.number.get()),
                        )
                        .await
                        .map_err(|error| to_pull_request_error("watchFingerprint", error))
                }
                .boxed()
            })
            .await
    }

    // -----------------------------------------------------------------------------------------
    // State announcements
    // -----------------------------------------------------------------------------------------

    /// Announces a pull request a detail read sees first or in a new state, so a client showing a
    /// fresh merge brings the thread's link and its settlement along, rather than leaving them to
    /// the next sync sweep.
    fn note_detail_reading(&self, reference: &PullRequestRef, next: &PullRequestSummary) {
        let next: PullRequestReading = next.into();
        let scope = ref_scope(reference);
        let announce = {
            let mut state = self.state.lock();
            let current = state.detail_states.get(&scope).cloned();
            if !supersedes_reading(current.as_ref(), &next) {
                return;
            }
            state.detail_states.shift_remove(&scope);
            if state.detail_states.len() >= REF_EPOCH_CAPACITY {
                state.detail_states.shift_remove_index(0);
            }
            let next_state = next.state;
            state.detail_states.insert(scope, next);
            current.map(|current| current.state) != Some(next_state)
        };
        if announce && let Some(host) = &reference.host {
            let _ = self.state_changes.send(ThreadPullRequestKey {
                host: host.clone(),
                repository: reference.repository.clone(),
                number: reference.number,
            });
        }
    }

    /// The state a read routed to another environment saw. A new one reaches
    /// `subscribe_state_changes`, so the host is asked again; the report itself is never trusted.
    pub(crate) async fn report_state(
        self: &Arc<Self>,
        input: &PullRequestReportStateInput,
    ) -> Result<(), PullRequestError> {
        let reference = self.canonical_ref(&input.reference).await?;
        let now = self.clock.now_millis();
        let scope = ref_scope(&reference);
        {
            let mut state = self.state.lock();
            if let Some((reported, at)) = state.reported_states.get(&scope)
                && *reported == input.state
                && now - *at < REPORTED_STATE_DEDUPE_MS
            {
                return Ok(());
            }
            state.reported_states.shift_remove(&scope);
            if state.reported_states.len() >= REF_EPOCH_CAPACITY {
                state.reported_states.shift_remove_index(0);
            }
            state.reported_states.insert(scope, (input.state, now));
        }
        if let Some(host) = &reference.host {
            let _ = self.state_changes.send(ThreadPullRequestKey {
                host: host.clone(),
                repository: reference.repository.clone(),
                number: reference.number,
            });
        }
        Ok(())
    }
}
