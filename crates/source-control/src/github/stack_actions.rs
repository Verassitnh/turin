//! Port of upstream/packages/source-control-github/src/server/githubStackActions.ts
//!
//! Merge and update-branch for a whole GitHub stack ("stacked pull requests").

use std::collections::HashSet;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestMergeMethod, PullRequestStackHead,
};

use super::api::{GitHubApi, GitHubGraphQlInput, GitHubRestInput, RestMethod};
use super::graphql::{
    AliasedGraphQlDocumentInput, GraphQlOperation, GraphQlVariable, aliased_graphql_document,
};
use super::pull_request_api::errors::GitHubPullRequestApiError;
use super::pull_request_json::de;
use super::pull_request_json::{decode_pull_request_stacks_json, to_can_write};
use super::stack_rebase::{
    CascadeLayer, CascadeRebaseInput, GitHubStackRebaseError, cascade_rebase_stack,
};
use crate::source_control_core::clock::Clock;
use crate::source_control_core::host::SourceControlHost;
use crate::source_control_core::{Cause, cause_from_message};

/// `stackErrorIdentity`: what every stack failure names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackErrorIdentity {
    pub repository: String,
    pub number: u64,
    pub stack_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubStackChangedError {
    pub identity: StackErrorIdentity,
    pub completed: u64,
}

impl std::fmt::Display for GitHubStackChangedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.completed > 0 {
            write!(
                f,
                "The stack changed at PR #{} after {} layers. Earlier updates remain on GitHub. Refresh it before trying again.",
                self.identity.number, self.completed
            )
        } else {
            f.write_str("The stack changed. Refresh it before trying again.")
        }
    }
}
impl std::error::Error for GitHubStackChangedError {}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("This operation is not supported for this stack.")]
pub struct GitHubStackUnsupportedError {
    pub identity: StackErrorIdentity,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("GitHub returned an unreadable stack operation response.")]
pub struct GitHubStackResponseInvalidError {
    pub identity: StackErrorIdentity,
    pub cause: Option<Cause>,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("GitHub refused the stack merge. Check the stack's branch rules and merge requirements.")]
pub struct GitHubStackMergeRejectedError {
    pub identity: StackErrorIdentity,
    pub cause: Cause,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "The merge is still running on GitHub. Check its status there before submitting another request."
)]
pub struct GitHubStackMergePendingError {
    pub identity: StackErrorIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "You cannot update every branch in this stack. Check write access and fork maintainer permissions before retrying."
)]
pub struct GitHubStackPermissionError {
    pub identity: StackErrorIdentity,
}

#[derive(Debug, Clone)]
pub struct GitHubStackRebaseFailedError {
    pub identity: StackErrorIdentity,
    pub completed: u64,
    pub cause: GitHubStackRebaseError,
}

impl std::fmt::Display for GitHubStackRebaseFailedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A conflict or a refused push already says which layer and what to do about it.
        let message = self.cause.to_string();
        if message.is_empty() {
            write!(
                f,
                "Stack rebase stopped at PR #{} after {} layers. Earlier updates remain on GitHub; resolve the failing layer before retrying.",
                self.identity.number, self.completed
            )
        } else {
            f.write_str(&message)
        }
    }
}
impl std::error::Error for GitHubStackRebaseFailedError {}

#[derive(Debug, Clone, thiserror::Error)]
pub enum GitHubStackActionError {
    #[error(transparent)]
    Changed(#[from] GitHubStackChangedError),
    #[error(transparent)]
    Unsupported(#[from] GitHubStackUnsupportedError),
    #[error(transparent)]
    ResponseInvalid(#[from] GitHubStackResponseInvalidError),
    #[error(transparent)]
    MergeRejected(#[from] GitHubStackMergeRejectedError),
    #[error(transparent)]
    MergePending(#[from] GitHubStackMergePendingError),
    #[error(transparent)]
    Permission(#[from] GitHubStackPermissionError),
    #[error(transparent)]
    RebaseFailed(#[from] GitHubStackRebaseFailedError),
}

/// `status` and `details` of the stacks API's `merge-async`.
#[derive(Debug, Deserialize)]
struct MergeResponse {
    status: MergeStatus,
    details: MergeDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MergeStatus {
    Pending,
    Merged,
    Enqueued,
    Failed,
}

#[derive(Debug, Deserialize)]
struct MergeDetails {
    #[serde(default, deserialize_with = "de::opt")]
    uuid: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    #[allow(dead_code)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BranchAccessResponse {
    data: BranchAccessData,
}

#[derive(Debug, Deserialize)]
struct BranchAccessData {
    #[serde(deserialize_with = "de::nullable")]
    repository: Option<indexmap::IndexMap<String, Option<BranchAccessPullRequest>>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchAccessPullRequest {
    #[serde(deserialize_with = "de::nullable")]
    head_repository: Option<BranchAccessHeadRepository>,
    maintainer_can_modify: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchAccessHeadRepository {
    #[serde(deserialize_with = "de::nullable")]
    viewer_permission: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitHubStackActionInput {
    pub cwd: String,
    pub repository: String,
    pub host: String,
    pub number: u64,
    pub stack_number: u64,
    pub expected_stack_heads: Option<Vec<PullRequestStackHead>>,
    pub action: PullRequestAction,
    pub merge_method: Option<PullRequestMergeMethod>,
}

/// Remote-only updates: a stack rebase works in a scratch clone, never the environment's checkout.
pub async fn run_github_stack_action(
    api: &dyn GitHubApi,
    host: &dyn SourceControlHost,
    clock: &dyn Clock,
    input: &GitHubStackActionInput,
) -> Result<(), GitHubPullRequestApiError> {
    let identity = StackErrorIdentity {
        repository: input.repository.clone(),
        number: input.number,
        stack_number: input.stack_number,
    };
    let changed = || {
        GitHubPullRequestApiError::from(GitHubStackActionError::from(GitHubStackChangedError {
            identity: identity.clone(),
            completed: 0,
        }))
    };
    let unsupported = || {
        GitHubPullRequestApiError::from(GitHubStackActionError::from(GitHubStackUnsupportedError {
            identity: identity.clone(),
        }))
    };
    let invalid = |cause: Option<Cause>| {
        GitHubPullRequestApiError::from(GitHubStackActionError::from(
            GitHubStackResponseInvalidError {
                identity: identity.clone(),
                cause,
            },
        ))
    };
    if input.action != PullRequestAction::Merge && input.action != PullRequestAction::UpdateBranch {
        return Err(unsupported());
    }
    let endpoint = format!("repos/{}", input.repository);
    let read = api
        .rest(GitHubRestInput {
            host: input.host.clone(),
            operation: "runGitHubStackAction".into(),
            path: format!("{endpoint}/stacks?pull_request={}", input.number),
            ..Default::default()
        })
        .await?;
    let stack = decode_pull_request_stacks_json(&read.body)
        .map_err(|failure| invalid(Some(std::sync::Arc::new(failure))))?;
    let target_index = stack.as_ref().and_then(|stack| {
        stack
            .layers
            .iter()
            .position(|layer| layer.number == input.number)
    });
    let target = match (&stack, target_index) {
        (Some(stack), Some(index)) => Some(&stack.layers[index]),
        _ => None,
    };
    let (Some(stack), Some(target_index), Some(target)) = (&stack, target_index, target) else {
        return Err(changed());
    };
    if stack.number != input.stack_number
        || (input.action == PullRequestAction::UpdateBranch
            && target_index != stack.layers.len() - 1)
    {
        return Err(changed());
    }
    let affected_layers = if input.action == PullRequestAction::Merge {
        &stack.layers[..=target_index]
    } else {
        &stack.layers[..]
    };
    let open: Vec<_> = affected_layers
        .iter()
        .filter(|layer| layer.state != turin_contracts::pull_request::PullRequestState::Merged)
        .collect();
    if input.action == PullRequestAction::Merge
        && target.state != turin_contracts::pull_request::PullRequestState::Open
    {
        return Err(unsupported());
    }
    let heads_match = match &input.expected_stack_heads {
        None => false,
        Some(expected) => {
            expected.len() == open.len()
                && expected
                    .iter()
                    .map(|layer| layer.number.get())
                    .collect::<HashSet<_>>()
                    .len()
                    == open.len()
                && open.iter().all(|layer| {
                    layer.head_sha.as_deref().is_some_and(|sha| !sha.is_empty())
                        && expected.iter().any(|expected| {
                            expected.number.get() == layer.number
                                && Some(expected.head_sha.as_str()) == layer.head_sha.as_deref()
                        })
                })
        }
    };
    if !heads_match {
        return Err(changed());
    }
    if open.is_empty()
        || open
            .iter()
            .any(|layer| layer.state != turin_contracts::pull_request::PullRequestState::Open)
    {
        return Err(unsupported());
    }
    if input.action == PullRequestAction::UpdateBranch {
        let mut parts = input.repository.split('/');
        let owner = parts.next().unwrap_or_default().to_owned();
        let name = parts.next().unwrap_or_default().to_owned();
        let document = aliased_graphql_document(AliasedGraphQlDocumentInput {
            operation: GraphQlOperation::Query,
            name: None,
            alias: "pr",
            key: Some(&|layer: &&super::pull_request_json::GitHubPullRequestStackLayer, _| {
                layer.number as i64
            }),
            items: &open,
            shared: vec![
                ("owner".into(), GraphQlVariable::new("String!", owner)),
                ("name".into(), GraphQlVariable::new("String!", name)),
            ],
            variables: &|layer| {
                vec![("number".into(), GraphQlVariable::new("Int!", layer.number))]
            },
            field: &|placeholders, _| {
                format!(
                    "pullRequest(number: {}) {{ headRepository {{ viewerPermission }} maintainerCanModify }}",
                    placeholders["number"]
                )
            },
            within: Some(&|fields| format!("repository(owner: $owner, name: $name) {{\n{fields}\n}}")),
        })
        // `open` is non-empty here, so there is always a document.
        .expect("open layers are non-empty");
        let permissions = api
            .graphql(GitHubGraphQlInput {
                host: input.host.clone(),
                operation: "runGitHubStackAction".into(),
                query: document.query,
                variables: Some(document.variables),
                allow_reserve: Some(true),
                ..Default::default()
            })
            .await?;
        let access: BranchAccessResponse = serde_json::from_str(permissions.trim())
            .map_err(|error| invalid(Some(std::sync::Arc::new(error))))?;
        // viewerCanUpdateBranch is false for an already-current layer, even if rebasing its parent
        // will make it stale. Check branch write access separately before touching any layer.
        let denied = open.iter().any(|layer| {
            let pr = access
                .data
                .repository
                .as_ref()
                .and_then(|repository| repository.get(&format!("pr{}", layer.number)))
                .and_then(Option::as_ref);
            match pr.and_then(|pr| pr.head_repository.as_ref().map(|head| (pr, head))) {
                None => true,
                Some((pr, head)) => {
                    !pr.maintainer_can_modify
                        && !to_can_write(Some(head.viewer_permission.as_deref()))
                }
            }
        });
        if denied {
            return Err(
                GitHubStackActionError::from(GitHubStackPermissionError { identity }).into(),
            );
        }
        // GitHub's own "Rebase stack" has no API, and its per-PR "update branch" replays the old
        // copy of every lower layer into the one above it. The cascade moves each layer's own commits.
        cascade_rebase_stack(
            api,
            host,
            &CascadeRebaseInput {
                host: input.host.clone(),
                repository: input.repository.clone(),
                base: stack.base.clone(),
                layers: open
                    .iter()
                    .map(|layer| CascadeLayer {
                        number: layer.number,
                        head_branch: layer.head_branch.clone(),
                        head_sha: layer.head_sha.clone().unwrap_or_default(),
                    })
                    .collect(),
                remote: None,
            },
        )
        .await
        .map_err(|cause| {
            let (number, completed) = match &cause {
                GitHubStackRebaseError::Conflict(error) => (error.number, error.completed),
                GitHubStackRebaseError::Git(error) => (error.number, error.completed),
            };
            GitHubStackActionError::from(GitHubStackRebaseFailedError {
                identity: StackErrorIdentity {
                    number,
                    ..identity.clone()
                },
                completed,
                cause,
            })
        })?;
        return Ok(());
    }
    if open.iter().any(|layer| layer.is_draft == Some(true)) {
        return Err(unsupported());
    }
    let decode = |raw: &str| -> Result<(MergeResponse, Value), GitHubPullRequestApiError> {
        let value: Value = serde_json::from_str(raw.trim())
            .map_err(|error| invalid(Some(std::sync::Arc::new(error))))?;
        let response = MergeResponse::deserialize(&value)
            .map_err(|error| invalid(Some(std::sync::Arc::new(error))))?;
        Ok((response, value))
    };
    let request = api
        .rest(GitHubRestInput {
            host: input.host.clone(),
            operation: "runGitHubStackAction".into(),
            method: RestMethod::Put,
            path: format!("{endpoint}/pulls/{}/merge-async", input.number),
            body: Some(json!({
                "merge_method": input.merge_method.map_or("merge", |method| match method {
                    PullRequestMergeMethod::Merge => "merge",
                    PullRequestMergeMethod::Squash => "squash",
                    PullRequestMergeMethod::Rebase => "rebase",
                }),
                "merge_action": "default",
                "sha": target.head_sha,
            })),
            ..Default::default()
        })
        .await?;
    let (mut result, mut raw) = decode(&request.body)?;
    let deadline = clock.now_millis() + 5 * 60_000;
    let mut attempt: u32 = 0;
    while result.status == MergeStatus::Pending && clock.now_millis() < deadline {
        let Some(uuid) = result.details.uuid.clone().filter(|uuid| !uuid.is_empty()) else {
            return Err(invalid(None));
        };
        let delay = (1_000u64.saturating_mul(2u64.saturating_pow(attempt))).min(10_000);
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let poll = api
            .rest(GitHubRestInput {
                host: input.host.clone(),
                operation: "runGitHubStackAction".into(),
                path: format!(
                    "{endpoint}/pulls/{}/merge-async/{}",
                    input.number,
                    crate::shared::js::encode_uri_component(&uuid)
                ),
                ..Default::default()
            })
            .await?;
        (result, raw) = decode(&poll.body)?;
        attempt += 1;
    }
    if result.status == MergeStatus::Pending {
        return Err(GitHubStackActionError::from(GitHubStackMergePendingError { identity }).into());
    }
    if result.status == MergeStatus::Failed {
        return Err(GitHubStackActionError::from(GitHubStackMergeRejectedError {
            identity,
            cause: cause_from_message(raw.to_string()),
        })
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use serde_json::{Value, json};

    use super::*;
    use crate::github::api::{GitHubApiError, GitHubRequestCredential, GitHubRestResponse};
    use crate::github::pull_request_api::test_support::{ScriptedHost, TokioClock};
    use crate::source_control_core::redacted::Redacted;

    /// One request as the fake saw it, flattened to words: the path or document, then each
    /// variable or body field as `name=value`, so an assertion reads as "this request carried
    /// that value".
    type Call = Vec<String>;

    struct Scripted {
        responses: Vec<Value>,
        calls: Mutex<Vec<Call>>,
    }

    fn words(head: &str, fields: Option<&Value>) -> Call {
        let mut call = vec![head.to_owned()];
        if let Some(Value::Object(fields)) = fields {
            for (key, value) in fields {
                call.push(match value {
                    Value::String(text) => format!("{key}={text}"),
                    other => format!("{key}={other}"),
                });
            }
        }
        call
    }

    /// GitHub refuses a document that declares a variable it never uses.
    fn unused_variables(query: &str) -> Vec<String> {
        let pattern = regex::Regex::new(r"\$(\w+)").unwrap();
        let mut declared = Vec::new();
        let mut used = std::collections::HashSet::new();
        for found in pattern.captures_iter(query) {
            let whole = found.get(0).unwrap();
            let name = found[1].to_owned();
            if query[whole.end()..].trim_start().starts_with(':') {
                declared.push(name);
            } else {
                used.insert(name);
            }
        }
        declared
            .into_iter()
            .filter(|name| !used.contains(name))
            .collect()
    }

    impl Scripted {
        fn next(&self, call: Call) -> String {
            let mut calls = self.calls.lock();
            calls.push(call);
            self.responses
                .get(calls.len() - 1)
                .unwrap_or_else(|| panic!("Unexpected GitHub request"))
                .to_string()
        }
        fn calls(&self) -> Vec<Call> {
            self.calls.lock().clone()
        }
    }

    #[async_trait]
    impl GitHubApi for Scripted {
        async fn graphql(&self, input: GitHubGraphQlInput) -> Result<String, GitHubApiError> {
            let unused = unused_variables(&input.query);
            assert!(
                unused.is_empty(),
                "Variables declared but not used: {}",
                unused.join(", ")
            );
            let variables = input.variables.map(Value::Object);
            Ok(self.next(words(&input.query, variables.as_ref())))
        }
        async fn rest(&self, input: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> {
            let body = self.next(words(&input.path, input.body.as_ref()));
            Ok(GitHubRestResponse {
                status: 200,
                headers: Default::default(),
                body,
                truncated: false,
                invalid_utf8: false,
            })
        }
        async fn credential(&self, _: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
            Ok(GitHubRequestCredential {
                token: Redacted::new("token".into()),
                fingerprint: "fp".into(),
            })
        }
    }

    fn is_mutation(call: &Call) -> bool {
        call[0].starts_with("mutation")
    }

    fn stack() -> Value {
        json!([{
            "number": 50,
            "url": "https://api.github.com/repos/acme/web/stacks/50",
            "base": { "ref": "main" },
            "pull_requests": [
                { "number": 1, "title": "Base", "head": { "ref": "base", "sha": "aaa" },
                  "state": "closed", "merged_at": "2026-01-01T00:00:00Z" },
                { "number": 2, "title": "Middle", "head": { "ref": "middle", "sha": "bbb" },
                  "state": "open", "draft": false },
                { "number": 3, "title": "Top", "head": { "ref": "top", "sha": "ccc" },
                  "state": "open", "draft": false },
            ],
        }])
    }

    fn heads(heads: &[(u64, &str)]) -> Vec<PullRequestStackHead> {
        heads
            .iter()
            .map(|(number, head_sha)| {
                serde_json::from_value(json!({ "number": number, "headSha": head_sha })).unwrap()
            })
            .collect()
    }

    fn input() -> GitHubStackActionInput {
        GitHubStackActionInput {
            cwd: "/repo".into(),
            repository: "acme/web".into(),
            host: "github.com".into(),
            number: 3,
            stack_number: 50,
            expected_stack_heads: Some(heads(&[(2, "bbb"), (3, "ccc")])),
            action: PullRequestAction::Merge,
            merge_method: None,
        }
    }

    fn access() -> Value {
        json!({ "data": { "repository": {
            "pr2": { "headRepository": { "viewerPermission": "WRITE" }, "maintainerCanModify": false },
            "pr3": { "headRepository": { "viewerPermission": "WRITE" }, "maintainerCanModify": false },
        } } })
    }

    fn access_with_pr3(permission: Value, maintainer_can_modify: bool) -> Value {
        let mut access = access();
        access["data"]["repository"]["pr3"] = json!({
            "headRepository": { "viewerPermission": permission },
            "maintainerCanModify": maintainer_can_modify,
        });
        access
    }

    fn branch(number: u64, head_ref_oid: &str, behind_by: u64, processed: &[&str]) -> Value {
        json!({ "data": {
            "processed": processed.iter().map(|oid| json!({ "headRefOid": oid })).collect::<Vec<_>>(),
            "repository": { "pullRequest": {
                "id": format!("PR_{number}"),
                "headRefOid": head_ref_oid,
                "baseRef": { "compare": { "behindBy": behind_by } },
            } },
        } })
    }

    fn rebase_responses() -> Vec<Value> {
        let rebased = json!({ "data": { "updatePullRequestBranch": { "pullRequest": { "headRefOid": "rebased-sha" } } } });
        vec![
            branch(2, "bbb", 1, &[]),
            rebased.clone(),
            branch(3, "ccc", 1, &["rebased-sha"]),
            rebased,
        ]
    }

    struct Run {
        result: Result<(), GitHubPullRequestApiError>,
        calls: Vec<Call>,
        git: Vec<Vec<String>>,
    }

    async fn run(responses: Vec<Value>, input: GitHubStackActionInput) -> Run {
        let api = Scripted {
            responses,
            calls: Mutex::new(Vec::new()),
        };
        let host = Arc::new(ScriptedHost::default());
        let clock = TokioClock::new();
        let result = run_github_stack_action(&api, host.as_ref(), clock.as_ref(), &input).await;
        let git = host.processes.lock().clone();
        Run {
            result,
            calls: api.calls(),
            git,
        }
    }

    fn stack_error(result: &Result<(), GitHubPullRequestApiError>) -> &GitHubStackActionError {
        match result {
            Err(GitHubPullRequestApiError::Stack(error)) => error,
            other => panic!("expected a stack error, got {other:?}"),
        }
    }

    fn pushes(git: &[Vec<String>]) -> Vec<&Vec<String>> {
        git.iter().filter(|args| args[0] == "push").collect()
    }

    #[tokio::test]
    async fn submits_one_atomic_merge_with_the_reviewed_head_and_respects_the_merge_queue() {
        let run = run(
            vec![stack(), json!({ "status": "enqueued", "details": {} })],
            GitHubStackActionInput {
                merge_method: Some(PullRequestMergeMethod::Squash),
                ..input()
            },
        )
        .await;
        run.result.unwrap();
        assert_eq!(run.calls.len(), 2);
        assert!(run.calls[1].contains(&"repos/acme/web/pulls/3/merge-async".to_owned()));
        assert!(run.calls[1].contains(&"sha=ccc".to_owned()));
        assert!(run.calls[1].contains(&"merge_action=default".to_owned()));
        assert!(run.calls[1].contains(&"merge_method=squash".to_owned()));
    }

    #[tokio::test]
    async fn merges_through_the_selected_layer_without_including_later_draft_layers() {
        let mut five = stack();
        five[0]["pull_requests"] = Value::Array(
            (0..5)
                .map(|index| {
                    json!({
                        "number": index + 1,
                        "head": { "ref": format!("layer-{}", index + 1), "sha": format!("sha-{}", index + 1) },
                        "state": "open",
                        "draft": index >= 3,
                    })
                })
                .collect(),
        );
        let run = run(
            vec![five, json!({ "status": "merged", "details": {} })],
            GitHubStackActionInput {
                number: 3,
                expected_stack_heads: Some(heads(&[(1, "sha-1"), (2, "sha-2"), (3, "sha-3")])),
                ..input()
            },
        )
        .await;
        run.result.unwrap();
        assert_eq!(run.calls.len(), 2);
        assert!(run.calls[1].contains(&"repos/acme/web/pulls/3/merge-async".to_owned()));
        assert!(run.calls[1].contains(&"sha=sha-3".to_owned()));
        assert!(run.calls[1].contains(&"merge_action=default".to_owned()));
    }

    #[tokio::test]
    async fn rejects_stale_reviewed_heads_below_a_selected_middle_layer() {
        let run = run(
            vec![stack()],
            GitHubStackActionInput {
                number: 2,
                expected_stack_heads: Some(heads(&[(2, "old-head")])),
                ..input()
            },
        )
        .await;
        assert!(matches!(
            stack_error(&run.result),
            GitHubStackActionError::Changed(_)
        ));
        assert_eq!(run.calls.len(), 1);
    }

    #[tokio::test]
    async fn does_not_merge_from_an_already_merged_layer() {
        let run = run(
            vec![stack()],
            GitHubStackActionInput {
                number: 1,
                expected_stack_heads: Some(vec![]),
                ..input()
            },
        )
        .await;
        assert!(matches!(
            stack_error(&run.result),
            GitHubStackActionError::Unsupported(_)
        ));
        assert_eq!(run.calls.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn polls_an_accepted_merge_and_reports_a_later_rule_rejection() {
        let run = run(
            vec![
                stack(),
                json!({ "status": "pending", "details": { "uuid": "operation" } }),
                json!({ "status": "failed", "details": { "message": "Required checks have not passed" } }),
            ],
            input(),
        )
        .await;
        assert!(matches!(
            stack_error(&run.result),
            GitHubStackActionError::MergeRejected(_)
        ));
        assert!(run.calls[2].contains(&"repos/acme/web/pulls/3/merge-async/operation".to_owned()));
    }

    #[tokio::test]
    async fn retains_stack_identity_and_a_rejection_response_without_a_message() {
        let rejection = json!({ "status": "failed", "details": {} });
        let run = run(vec![stack(), rejection.clone()], input()).await;
        match stack_error(&run.result) {
            GitHubStackActionError::MergeRejected(error) => {
                assert_eq!(error.identity.repository, "acme/web");
                assert_eq!(error.identity.number, 3);
                assert_eq!(error.identity.stack_number, 50);
                assert_eq!(
                    serde_json::from_str::<Value>(&error.cause.to_string()).unwrap(),
                    rejection
                );
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refuses_a_changed_stack_before_performing_any_mutation() {
        let run = run(
            vec![stack()],
            GitHubStackActionInput {
                expected_stack_heads: Some(heads(&[(2, "old"), (3, "ccc")])),
                ..input()
            },
        )
        .await;
        assert!(matches!(
            stack_error(&run.result),
            GitHubStackActionError::Changed(_)
        ));
        assert_eq!(run.calls.len(), 1);
    }

    #[tokio::test]
    async fn refuses_the_entire_rebase_before_mutation_when_a_later_fork_denies_write_access() {
        let run = run(
            vec![stack(), access_with_pr3(json!("READ"), false)],
            GitHubStackActionInput {
                action: PullRequestAction::UpdateBranch,
                ..input()
            },
        )
        .await;
        assert!(matches!(
            stack_error(&run.result),
            GitHubStackActionError::Permission(_)
        ));
        assert_eq!(run.calls.len(), 2);
    }

    #[tokio::test]
    async fn allows_a_fork_that_explicitly_permits_maintainer_updates() {
        let mut responses = vec![stack(), access_with_pr3(json!("READ"), true)];
        responses.extend(rebase_responses());
        let run = run(
            responses,
            GitHubStackActionInput {
                action: PullRequestAction::UpdateBranch,
                ..input()
            },
        )
        .await;
        run.result.unwrap();
        assert_eq!(pushes(&run.git).len(), 2);
    }

    #[tokio::test]
    async fn rebases_for_a_github_app_which_github_gives_no_permission_to_read() {
        let mut responses = vec![
            stack(),
            json!({ "data": { "repository": {
                "pr2": { "headRepository": { "viewerPermission": null }, "maintainerCanModify": false },
                "pr3": { "headRepository": { "viewerPermission": null }, "maintainerCanModify": false },
            } } }),
        ];
        responses.extend(rebase_responses());
        let run = run(
            responses,
            GitHubStackActionInput {
                action: PullRequestAction::UpdateBranch,
                ..input()
            },
        )
        .await;
        run.result.unwrap();
        assert_eq!(pushes(&run.git).len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn bounds_polling_and_reports_a_still_running_merge_without_claiming_success() {
        let mut responses = vec![stack()];
        responses.extend(
            (0..40).map(|_| json!({ "status": "pending", "details": { "uuid": "operation" } })),
        );
        let run = run(responses, input()).await;
        assert!(matches!(
            stack_error(&run.result),
            GitHubStackActionError::MergePending(_)
        ));
        assert!(run.calls.len() < 40);
    }

    #[tokio::test]
    async fn rebases_the_open_layers_bottom_to_top_in_a_scratch_clone_each_onto_the_one_below() {
        let run = run(
            vec![stack(), access()],
            GitHubStackActionInput {
                action: PullRequestAction::UpdateBranch,
                ..input()
            },
        )
        .await;
        run.result.unwrap();
        // GitHub is only read; every change is a git push with a lease on the reviewed head.
        assert!(!run.calls.iter().any(is_mutation));
        let rebases: Vec<_> = run
            .git
            .iter()
            .filter(|args| args[0] == "rebase")
            .cloned()
            .collect();
        let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            rebases,
            vec![
                strings(&["rebase", "--quiet", "--onto", "origin/main", "new-sha"]),
                strings(&["rebase", "--quiet", "--onto", "new-sha", "bbb"]),
            ]
        );
        let pushed: Vec<_> = pushes(&run.git).into_iter().cloned().collect();
        assert_eq!(
            pushed,
            vec![
                strings(&[
                    "push",
                    "--quiet",
                    "--force-with-lease=refs/heads/middle:bbb",
                    "origin",
                    "new-sha:refs/heads/middle",
                ]),
                strings(&[
                    "push",
                    "--quiet",
                    "--force-with-lease=refs/heads/top:ccc",
                    "origin",
                    "new-sha:refs/heads/top",
                ]),
            ]
        );
    }

    #[tokio::test]
    async fn checks_every_layers_write_access_before_any_git_runs() {
        let run = run(
            vec![stack(), access_with_pr3(json!("READ"), false)],
            GitHubStackActionInput {
                action: PullRequestAction::UpdateBranch,
                ..input()
            },
        )
        .await;
        assert!(run.result.is_err());
        assert!(run.git.is_empty());
    }

    #[test]
    fn stack_error_messages_match_upstream() {
        let identity = StackErrorIdentity {
            repository: "acme/web".into(),
            number: 3,
            stack_number: 50,
        };
        assert_eq!(
            GitHubStackChangedError {
                identity: identity.clone(),
                completed: 0
            }
            .to_string(),
            "The stack changed. Refresh it before trying again."
        );
        assert_eq!(
            GitHubStackChangedError {
                identity: identity.clone(),
                completed: 2
            }
            .to_string(),
            "The stack changed at PR #3 after 2 layers. Earlier updates remain on GitHub. Refresh it before trying again."
        );
        assert_eq!(
            GitHubStackUnsupportedError {
                identity: identity.clone()
            }
            .to_string(),
            "This operation is not supported for this stack."
        );
        assert_eq!(
            GitHubStackMergePendingError { identity }.to_string(),
            "The merge is still running on GitHub. Check its status there before submitting another request."
        );
    }
}
