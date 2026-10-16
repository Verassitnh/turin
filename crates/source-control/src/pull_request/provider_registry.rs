//! Port of upstream/apps/server/src/pullRequest/PullRequestProviderRegistry.ts
//!
//! GitHub only: upstream builds the registry from every built-in driver's `pullRequests`
//! member; here the caller (the app's driver wiring) hands over the providers it built, and
//! only GitHub's exists.

use std::collections::HashMap;
use std::sync::Arc;

use turin_contracts::source_control::SourceControlProviderKind;

use crate::source_control_core::pull_request_provider::PullRequestProviderApi;

/// The hosts this build can read change requests from. A host with no entry here still shows up
/// in the provider list as unimplemented, so its projects are explained rather than missing.
#[derive(Clone)]
pub struct PullRequestProviderRegistry {
    by_kind: HashMap<SourceControlProviderKind, Arc<dyn PullRequestProviderApi>>,
    kinds: Vec<SourceControlProviderKind>,
}

impl PullRequestProviderRegistry {
    /// `fromProviders`: stands a registry up from providers the caller supplies (tests build
    /// theirs this way). When two providers share a kind the later one answers `get`.
    pub fn from_providers(providers: Vec<Arc<dyn PullRequestProviderApi>>) -> Self {
        let kinds = providers.iter().map(|provider| provider.kind()).collect();
        let by_kind = providers
            .into_iter()
            .map(|provider| (provider.kind(), provider))
            .collect();
        Self { by_kind, kinds }
    }

    /// `None` for a host with no implementation, which the service reports as unsupported.
    pub fn get(&self, kind: &SourceControlProviderKind) -> Option<Arc<dyn PullRequestProviderApi>> {
        self.by_kind.get(kind).cloned()
    }

    /// The kinds of the registered providers, in registration order.
    pub fn kinds(&self) -> &[SourceControlProviderKind] {
        &self.kinds
    }
}

/// A configurable provider for the PR-service tests of this module tree.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use crate::source_control_core::pull_request_provider::*;
    use turin_contracts::pull_request::{
        PullRequestCapabilities, PullRequestReviewerCandidateList, PullRequestViewerPermissions,
    };
    use turin_contracts::source_control::SourceControlProviderKind;

    type FileRevisionsHandler =
        Arc<dyn Fn(GetFileRevisionsInput) -> ProviderResult<ProviderFileRevisions> + Send + Sync>;
    type FilesViewedHandler =
        Arc<dyn Fn(ProviderChangeRequestRef) -> ProviderResult<ProviderFilesViewed> + Send + Sync>;
    type SetFilesViewedHandler =
        Arc<dyn Fn(SetFilesViewedInput) -> ProviderResult<()> + Send + Sync>;

    pub(crate) struct FakeProvider {
        kind: SourceControlProviderKind,
        capabilities: PullRequestCapabilities,
        pub file_revisions: Option<FileRevisionsHandler>,
        pub files_viewed: Option<FilesViewedHandler>,
        pub set_files_viewed: Option<SetFilesViewedHandler>,
    }

    impl FakeProvider {
        pub(crate) fn new(kind: &str, viewed_files: Option<&str>) -> Self {
            let mut capabilities = serde_json::json!({
                "diff": false,
                "comment": false,
                "actions": [],
                "mergeMethods": [],
                "search": false,
                "review": { "inlineComment": false, "reply": false, "resolve": false, "verdicts": [] },
                "reviewers": { "request": false, "listCandidates": false },
            });
            if let Some(store) = viewed_files {
                capabilities["viewedFiles"] = store.into();
            }
            Self {
                kind: SourceControlProviderKind::new(kind).unwrap(),
                capabilities: serde_json::from_value(capabilities).unwrap(),
                file_revisions: None,
                files_viewed: None,
                set_files_viewed: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl PullRequestProviderApi for FakeProvider {
        fn kind(&self) -> SourceControlProviderKind {
            self.kind.clone()
        }
        fn capabilities(&self) -> &PullRequestCapabilities {
            &self.capabilities
        }
        fn supports(&self, operation: PullRequestOperation) -> bool {
            match operation {
                PullRequestOperation::GetFileRevisions => self.file_revisions.is_some(),
                PullRequestOperation::GetFilesViewed => self.files_viewed.is_some(),
                PullRequestOperation::SetFilesViewed => self.set_files_viewed.is_some(),
                _ => false,
            }
        }
        async fn get_file_revisions(
            &self,
            input: GetFileRevisionsInput,
        ) -> ProviderResult<ProviderFileRevisions> {
            (self.file_revisions.as_ref().expect("supported"))(input)
        }
        async fn get_files_viewed(
            &self,
            input: ProviderChangeRequestRef,
        ) -> ProviderResult<ProviderFilesViewed> {
            (self.files_viewed.as_ref().expect("supported"))(input)
        }
        async fn set_files_viewed(&self, input: SetFilesViewedInput) -> ProviderResult<()> {
            (self.set_files_viewed.as_ref().expect("supported"))(input)
        }
        async fn get_viewer(&self, _: &str, _: Option<&str>) -> ProviderResult<String> {
            unimplemented!()
        }
        async fn list_change_requests(
            &self,
            _: ListChangeRequestsInput,
        ) -> ProviderResult<ProviderChangeRequestPage> {
            unimplemented!()
        }
        async fn get_change_request(
            &self,
            _: ProviderChangeRequestRef,
        ) -> ProviderResult<ProviderChangeRequestDetail> {
            unimplemented!()
        }
        async fn get_change_request_activity(
            &self,
            _: ProviderChangeRequestRef,
        ) -> ProviderResult<ProviderChangeRequestActivity> {
            unimplemented!()
        }
        async fn get_viewer_permissions(
            &self,
            _: GetViewerPermissionsInput,
        ) -> ProviderResult<PullRequestViewerPermissions> {
            unimplemented!()
        }
        async fn get_diff(&self, _: GetDiffInput) -> ProviderResult<ProviderDiffSlice> {
            unimplemented!()
        }
        async fn run_action(&self, _: RunActionInput) -> ProviderResult<()> {
            unimplemented!()
        }
        async fn comment(&self, _: CommentInput) -> ProviderResult<()> {
            unimplemented!()
        }
        async fn submit_review(&self, _: SubmitReviewInput) -> ProviderResult<()> {
            unimplemented!()
        }
        async fn list_reviewer_candidates(
            &self,
            _: ProviderChangeRequestRef,
        ) -> ProviderResult<PullRequestReviewerCandidateList> {
            unimplemented!()
        }
        async fn set_reviewer_request(&self, _: SetReviewerRequestInput) -> ProviderResult<()> {
            unimplemented!()
        }
        async fn reply_to_thread(&self, _: ReplyToThreadInput) -> ProviderResult<()> {
            unimplemented!()
        }
        async fn set_reaction(&self, _: SetReactionInput) -> ProviderResult<()> {
            unimplemented!()
        }
        async fn set_thread_resolution(&self, _: SetThreadResolutionInput) -> ProviderResult<()> {
            unimplemented!()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeProvider;
    use super::*;

    #[test]
    fn looks_providers_up_by_kind_and_reports_none_for_an_unimplemented_host() {
        let registry = PullRequestProviderRegistry::from_providers(vec![Arc::new(
            FakeProvider::new("github", None),
        )]);
        let github = SourceControlProviderKind::new("github").unwrap();
        assert_eq!(registry.get(&github).unwrap().kind(), github);
        assert!(
            registry
                .get(&SourceControlProviderKind::new("unknown").unwrap())
                .is_none()
        );
        assert_eq!(registry.kinds(), std::slice::from_ref(&github));
    }
}
