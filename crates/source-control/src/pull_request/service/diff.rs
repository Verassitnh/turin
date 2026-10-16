//! Diffs, file contents and viewed files of `PullRequestService.ts`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use futures::FutureExt;
use serde_json::{Value, json};
use turin_contracts::pull_request::{
    PullRequestDiffFileContentsInput, PullRequestDiffFileContentsResult, PullRequestDiffInput,
    PullRequestDiffResult, PullRequestFilesViewedResult, PullRequestSetFilesViewedInput,
};

use super::epochs::{CredentialRef, ref_of_cache_key};
use super::errors::{PullRequestError, operation_error, tne_opt, to_pull_request_error};
use super::{AsPullRequestRef, DIFF_SWEEP_INTERVAL, Inner};
use crate::source_control_core::pull_request_provider::{
    GetDiffFileContentsInput, GetDiffInput, PullRequestOperation,
};

impl Inner {
    async fn diff_uncached(
        self: &Arc<Self>,
        input: &CredentialRef,
        cursor: Option<String>,
        commit: Option<String>,
    ) -> Result<PullRequestDiffResult, PullRequestError> {
        let project = self.require_project(input).await?;
        if !project.api.capabilities().diff {
            return Err(operation_error(
                "diff",
                "This host cannot provide a diff for a change request.",
            ));
        }
        let slice = project
            .api
            .get_diff(GetDiffInput {
                request: project.provider_ref(input.number.get()),
                cursor,
                commit,
            })
            .await
            .map_err(|error| to_pull_request_error("diff", error))?;
        Ok(PullRequestDiffResult {
            patch: slice.patch,
            truncated: slice.truncated,
            next_cursor: tne_opt(slice.next_cursor.as_deref()),
            omitted_file_stats: slice.omitted_file_stats,
        })
    }

    pub(crate) async fn diff_file_contents(
        self: &Arc<Self>,
        input: &PullRequestDiffFileContentsInput,
    ) -> Result<PullRequestDiffFileContentsResult, PullRequestError> {
        let project = self.require_project(&input.pull_request_ref()).await?;
        if !(project.api.capabilities().diff
            && project
                .api
                .supports(PullRequestOperation::GetDiffFileContents))
        {
            return Err(operation_error(
                "diffFileContents",
                "This host cannot expand unchanged pull request lines.",
            ));
        }
        let contents = project
            .api
            .get_diff_file_contents(GetDiffFileContentsInput {
                request: project.provider_ref(input.number.get()),
                commit: input
                    .commit
                    .as_ref()
                    .map(|commit| commit.as_str().to_owned()),
                change_type: input.change_type,
                old_path: input.old_path.as_str().to_owned(),
                new_path: input.new_path.as_str().to_owned(),
            })
            .await
            .map_err(|error| to_pull_request_error("diffFileContents", error))?;
        Ok(PullRequestDiffFileContentsResult {
            old_contents: contents.old_contents,
            new_contents: contents.new_contents,
        })
    }

    /// One pending sweep at a time, armed in the same step that leaves a value held and re-armed
    /// while anything still is, so an idle server keeps no timer.
    ///
    /// Both diff holders drop an expired value only when its own key is read again, and a diff's
    /// key moves with every push, so a closed page's patches would stay held until the server
    /// restarted.
    pub(crate) fn arm_diff_sweep(self: &Arc<Self>) {
        if self.diff_sweep_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        // Run from the service's own task set, in one step with the flag: a caller's
        // cancellation cannot strand the flag, and its request is not kept alive by the wait.
        let weak: Weak<Inner> = Arc::downgrade(self);
        self.background.spawn(async move {
            tokio::time::sleep(DIFF_SWEEP_INTERVAL).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            // Cleared before the sweep, so a read that settles during it arms the next one itself.
            inner.diff_sweep_pending.store(false, Ordering::SeqCst);
            let stale = inner.stale_diff.drop_expired();
            // The cache drops its expired entries as its keys are walked.
            let held = stale + inner.diff_cache.live_count();
            if held > 0 {
                inner.arm_diff_sweep();
            }
        });
    }

    pub(crate) async fn diff(
        self: &Arc<Self>,
        input: &CredentialRef,
        diff_input: &PullRequestDiffInput,
    ) -> Result<PullRequestDiffResult, PullRequestError> {
        let ref_key = self.ref_key(input);
        let cursor = diff_input
            .cursor
            .as_ref()
            .map(|cursor| cursor.as_str().to_owned());
        let commit = diff_input
            .commit
            .as_ref()
            .map(|commit| commit.as_str().to_owned());
        let key = json!([
            ref_key,
            cursor,
            commit,
            if commit.is_none() {
                self.last_good_summary
                    .peek(&ref_key)
                    .map(|summary| summary.updated_at)
            } else {
                None
            },
        ])
        .to_string();
        let read = {
            let inner = self.clone();
            let key = key.clone();
            async move {
                let value = inner
                    .diff_cache
                    .get(&key, {
                        let inner = inner.clone();
                        let key = key.clone();
                        move || {
                            async move {
                                let slots: Vec<Value> =
                                    serde_json::from_str(&key).unwrap_or_default();
                                let reference = ref_of_cache_key(
                                    slots.first().and_then(Value::as_str).unwrap_or_default(),
                                );
                                let text = |index: usize| {
                                    slots.get(index).and_then(Value::as_str).map(str::to_owned)
                                };
                                inner.diff_uncached(&reference, text(1), text(2)).await
                            }
                            .boxed()
                        }
                    })
                    .await;
                if let Ok(value) = &value
                    && !super::can_cache_diff(value)
                {
                    inner.diff_cache.invalidate_if(&key, |held| held == value);
                }
                // However the read ends, it may have left a value behind in the cache.
                inner.arm_diff_sweep();
                value
            }
            .boxed()
        };
        let arm = {
            let inner = self.clone();
            Arc::new(move || inner.arm_diff_sweep()) as Arc<dyn Fn() + Send + Sync>
        };
        self.stale_diff.serve(&key, read, arm).await
    }

    /// Canonicalised before it is keyed: both epochs are bumped against the remote's own
    /// spelling, so a reference keyed as the client spelled it would never see a refresh or a
    /// press.
    pub(crate) async fn files_viewed(
        self: &Arc<Self>,
        input: &CredentialRef,
    ) -> Result<PullRequestFilesViewedResult, PullRequestError> {
        let canonical = self.canonical_ref(&input.reference).await?;
        let reference = CredentialRef {
            reference: canonical,
            credential_namespace: input.credential_namespace.clone(),
        };
        let key = {
            let state = self.state.lock();
            json!([
                super::epochs::ref_cache_key(&state.epochs, &reference),
                state.epochs.files_viewed_epoch(&reference.reference)
            ])
            .to_string()
        };
        let inner = self.clone();
        let key_owned = key.clone();
        self.files_viewed_cache
            .get(&key, move || {
                async move {
                    let slots: Vec<Value> = serde_json::from_str(&key_owned).unwrap_or_default();
                    let reference =
                        ref_of_cache_key(slots.first().and_then(Value::as_str).unwrap_or_default());
                    inner.viewed_files.files_viewed(&reference.reference).await
                }
                .boxed()
            })
            .await
    }

    pub(crate) async fn set_files_viewed(
        self: &Arc<Self>,
        input: &PullRequestSetFilesViewedInput,
    ) -> Result<(), PullRequestError> {
        let reference = self.canonical_ref(&input.pull_request_ref()).await?;
        self.viewed_files.set_files_viewed(input).await?;
        // Deliberately not `invalidated_by_mutation`: ticking a file off says nothing about the
        // change request, and dropping a 300-file diff on every checkbox is the whole cost of
        // the feature. Only this reader's own bookkeeping is forgotten.
        self.state.lock().epochs.bump_files_viewed_epoch(&reference);
        Ok(())
    }
}
