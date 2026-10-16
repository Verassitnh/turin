//! The remaining decoders and request-body builders: previews, ids, heads, workflow runs, check
//! contexts, candidates, permissions, files, viewed state, host-native stacks.

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use turin_contracts::pull_request::{
    PullRequestFileViewedState, PullRequestLabelCandidate, PullRequestLabelCandidateList,
    PullRequestOmittedFileStat, PullRequestReviewCommentDraft, PullRequestReviewPosition,
    PullRequestReviewVerdict, PullRequestReviewerCandidate, PullRequestReviewerCandidateList,
    PullRequestReviewerKind,
};

use super::convert::{
    next_cursor_of, non_empty, rollup_checks_state, to_actor, to_can_triage, to_can_write,
    to_checks, to_pull_request_viewer_fields, to_state, trimmed,
};
use super::list::{decode_json, to_check_contexts};
use super::raw::{
    RawActorAvatars, RawCheckContextsEnvelope, RawCommitFiles, RawLabelCandidates, RawPreview,
    RawPullRequestFile, RawPullRequestFilesViewed, RawPullRequestHeads, RawPullRequestNodeId,
    RawReactionSubjectScope, RawReviewerCandidates, RawStackBase, RawStackId, RawStackListing,
    RawViewerPermissions, RawWorkflowRuns,
};
use super::types::{
    DecodeResult, GitHubCheckContext, GitHubCheckContextsPage, GitHubPullRequestChecksFromContexts,
    GitHubPullRequestFilesPatch, GitHubPullRequestFilesViewedPage, GitHubPullRequestHead,
    GitHubPullRequestHeadsPage, GitHubPullRequestStack, GitHubPullRequestStackLayer,
    GitHubViewedFile, GitHubViewerAccess, GitHubViewerRepositoryAccess, GitHubWorkflowRunApproval,
    GitHubWorkflowRunPage,
};
use crate::github::graphql::{
    AliasedGraphQlDocumentInput, GraphQlDocument, GraphQlOperation, GraphQlPlaceholders,
    GraphQlVariable, GraphQlVariables, aliased_graphql_document,
};
use crate::source_control_core::pull_request_provider::ProviderChangeRequestPreview;
use turin_contracts::pull_request::PullRequestMergeCapabilities;

// ---------------------------------------------------------------------------------------------
// Avatars, preview, node ids
// ---------------------------------------------------------------------------------------------

pub fn decode_actor_avatars_json(raw: &str) -> DecodeResult<IndexMap<String, String>> {
    let decoded: RawActorAvatars = decode_json(raw)?;
    let mut avatars_by_login = IndexMap::new();
    for node in decoded.data.nodes.iter().flatten() {
        let login = trimmed(node.login.as_deref());
        let avatar_url = trimmed(node.avatar_url.as_deref());
        if let (Some(login), Some(avatar_url)) = (login, avatar_url) {
            avatars_by_login.insert(login, avatar_url);
        }
    }
    Ok(avatars_by_login)
}

/// `Omit<PullRequestPreview, "projectId" | "repository">`.
pub fn decode_pull_request_preview_json(raw: &str) -> DecodeResult<ProviderChangeRequestPreview> {
    let decoded: RawPreview = decode_json(raw)?;
    let pr = decoded.data.repository.pull_request;
    Ok(ProviderChangeRequestPreview {
        number: pr.number.non_negative(),
        title: pr.title.clone(),
        url: pr.url.clone(),
        author: to_actor(pr.author.as_ref()),
        state: to_state(Some(&pr.state), None),
        is_draft: pr.is_draft,
        created_at: pr.created_at.clone(),
    })
}

pub fn decode_pull_request_node_id_json(raw: &str) -> DecodeResult<String> {
    let decoded: RawPullRequestNodeId = decode_json(raw)?;
    Ok(decoded.data.repository.pull_request.id)
}

/// True when the subject named is the pull request itself, or hangs off it; false for anything
/// else, including a subject or a pull request this host could not find.
pub fn decode_reaction_subject_scope_json(raw: &str) -> DecodeResult<bool> {
    let decoded: RawReactionSubjectScope = decode_json(raw)?;
    let expected = decoded
        .data
        .repository
        .as_ref()
        .and_then(|repository| repository.pull_request.as_ref())
        .map(|pull_request| pull_request.id.as_str());
    let actual = decoded.data.node.as_ref().map(|node| {
        node.pull_request
            .as_ref()
            .map_or(node.id.as_str(), |pull_request| pull_request.id.as_str())
    });
    Ok(matches!((expected, actual), (Some(expected), Some(actual)) if expected == actual))
}

// ---------------------------------------------------------------------------------------------
// Workflow runs, heads, check contexts
// ---------------------------------------------------------------------------------------------

/// One page of `actions/runs`.
pub fn decode_workflow_runs_json(raw: &str) -> DecodeResult<GitHubWorkflowRunPage> {
    let decoded: RawWorkflowRuns = decode_json(raw)?;
    Ok(GitHubWorkflowRunPage {
        raw_count: decoded.workflow_runs.len(),
        runs: decoded
            .workflow_runs
            .iter()
            .map(|run| GitHubWorkflowRunApproval {
                id: run.id.0,
                name: trimmed(run.name.as_deref())
                    .unwrap_or_else(|| format!("Workflow run {}", run.id.0)),
                url: trimmed(run.html_url.as_deref()),
            })
            .collect(),
    })
}

pub fn decode_pull_request_heads_json(raw: &str) -> DecodeResult<GitHubPullRequestHeadsPage> {
    let decoded: RawPullRequestHeads = decode_json(raw)?;
    let connection = decoded
        .data
        .repository
        .as_ref()
        .map(|repository| &repository.pull_requests);
    Ok(GitHubPullRequestHeadsPage {
        heads: connection
            .map(|connection| connection.nodes.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|pull_request| GitHubPullRequestHead {
                number: pull_request.number.non_negative(),
                head_sha: pull_request.head_ref_oid.clone(),
                is_cross_repository: pull_request.is_cross_repository,
                head_repository_owner: trimmed(
                    pull_request
                        .head_repository_owner
                        .as_ref()
                        .map(|owner| owner.login.as_str()),
                ),
            })
            .collect(),
        next_cursor: next_cursor_of(
            connection.and_then(|connection| connection.page_info.as_ref()),
        ),
    })
}

pub fn decode_pull_request_check_contexts_json(raw: &str) -> DecodeResult<GitHubCheckContextsPage> {
    let decoded: RawCheckContextsEnvelope = decode_json(raw)?;
    let pr = &decoded.data.repository.pull_request;
    let contexts = pr
        .commits
        .nodes
        .first()
        .and_then(|node| node.commit.status_check_rollup.as_ref())
        .map(|rollup| &rollup.contexts);
    Ok(GitHubCheckContextsPage {
        head_sha: pr.head_ref_oid.clone(),
        contexts: contexts
            .map(|contexts| to_check_contexts(&contexts.nodes))
            .unwrap_or_default(),
        next_cursor: next_cursor_of(contexts.map(|contexts| &contexts.page_info)),
    })
}

/// The checks and their one-word verdict, from every context of the head commit.
pub fn pull_request_checks_from_contexts(
    contexts: &[GitHubCheckContext],
) -> GitHubPullRequestChecksFromContexts {
    GitHubPullRequestChecksFromContexts {
        checks: to_checks(Some(contexts)),
        checks_state: rollup_checks_state(Some(contexts)),
    }
}

// ---------------------------------------------------------------------------------------------
// Reviewer and label candidates
// ---------------------------------------------------------------------------------------------

/// The people this pull request may be sent to, with whoever is already on it marked. The author
/// is dropped rather than shown as an unusable row: GitHub refuses a review request from the
/// person who opened the pull request, so offering them is offering a failure.
///
/// Whoever has been asked leads the list even where GitHub does not count them assignable (an
/// outside collaborator, an app) because a request that cannot be seen cannot be taken back.
pub fn decode_reviewer_candidates_json(
    raw: &str,
) -> DecodeResult<PullRequestReviewerCandidateList> {
    let decoded: RawReviewerCandidates = decode_json(raw)?;
    let repository = &decoded.data.repository;
    let pull_request = repository.pull_request.as_ref();
    let author = trimmed(
        pull_request
            .and_then(|pull_request| pull_request.author.as_ref())
            .and_then(|author| author.login.as_deref()),
    );
    let mut candidates: IndexMap<String, PullRequestReviewerCandidate> = IndexMap::new();
    for node in pull_request
        .and_then(|pull_request| pull_request.review_requests.as_ref())
        .map(|requests| requests.nodes.as_slice())
        .unwrap_or_default()
    {
        let reviewer = node.requested_reviewer.as_ref();
        let slug = trimmed(reviewer.and_then(|reviewer| reviewer.slug.as_deref()));
        let Some(id) = slug
            .clone()
            .or_else(|| trimmed(reviewer.and_then(|reviewer| reviewer.actor.login.as_deref())))
        else {
            continue;
        };
        let is_team = slug.is_some();
        // The same `org/slug` the reviewer row shows, so taking the request back finds that row.
        let login = if is_team {
            trimmed(reviewer.and_then(|reviewer| reviewer.combined_slug.as_deref()))
                .unwrap_or_else(|| id.clone())
        } else {
            id.clone()
        };
        let (Some(candidate_id), Some(candidate_login)) = (non_empty(&id), non_empty(&login))
        else {
            continue;
        };
        candidates.insert(
            format!("{} {id}", if is_team { "team" } else { "user" }),
            PullRequestReviewerCandidate {
                is_bot: None,
                login: candidate_login,
                name: trimmed(reviewer.and_then(|reviewer| reviewer.actor.name.as_deref())),
                avatar_url: trimmed(
                    reviewer.and_then(|reviewer| reviewer.actor.avatar_url.as_deref()),
                ),
                id: candidate_id,
                kind: if is_team {
                    PullRequestReviewerKind::Team
                } else {
                    PullRequestReviewerKind::User
                },
                is_requested: true,
            },
        );
    }
    for node in repository.assignable_users.nodes.iter() {
        let Some(login) = trimmed(node.as_ref().and_then(|node| node.login.as_deref())) else {
            continue;
        };
        if Some(&login) == author.as_ref() || candidates.contains_key(&format!("user {login}")) {
            continue;
        }
        let Some(non_empty_login) = non_empty(&login) else {
            continue;
        };
        candidates.insert(
            format!("user {login}"),
            PullRequestReviewerCandidate {
                is_bot: None,
                id: non_empty_login.clone(),
                kind: PullRequestReviewerKind::User,
                login: non_empty_login,
                name: trimmed(node.as_ref().and_then(|node| node.name.as_deref())),
                avatar_url: trimmed(node.as_ref().and_then(|node| node.avatar_url.as_deref())),
                is_requested: false,
            },
        );
    }
    Ok(PullRequestReviewerCandidateList {
        candidates: candidates.into_values().collect(),
        truncated: repository
            .assignable_users
            .page_info
            .as_ref()
            .and_then(|page_info| page_info.has_next_page)
            == Some(true),
    })
}

/// The body of `POST`/`DELETE /repos/{owner}/{repo}/pulls/{number}/requested_reviewers`, which
/// takes people and teams in two lists of its own. The same body serves both methods, because
/// GitHub takes a request back from exactly whoever it was made of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewerRequest {
    pub reviewers: Vec<String>,
    pub team_reviewers: Vec<String>,
}

impl ReviewerRequest {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("a request body of strings serializes")
    }
}

/// `reviewers` are `(id, kind)` pairs.
pub fn build_reviewer_request(reviewers: &[(String, PullRequestReviewerKind)]) -> ReviewerRequest {
    ReviewerRequest {
        reviewers: reviewers
            .iter()
            .filter(|(_, kind)| *kind == PullRequestReviewerKind::User)
            .map(|(id, _)| id.clone())
            .collect(),
        team_reviewers: reviewers
            .iter()
            .filter(|(_, kind)| *kind == PullRequestReviewerKind::Team)
            .map(|(id, _)| id.clone())
            .collect(),
    }
}

/// The repository's labels, with the ones already on this pull request marked. A label the pull
/// request wears that the repository no longer defines (deleted since, or past the page) leads
/// the list anyway, because a label that cannot be seen cannot be taken off.
pub fn decode_label_candidates_json(raw: &str) -> DecodeResult<PullRequestLabelCandidateList> {
    let decoded: RawLabelCandidates = decode_json(raw)?;
    let repository = &decoded.data.repository;
    let applied: IndexSet<String> = repository
        .pull_request
        .as_ref()
        .and_then(|pull_request| pull_request.labels.as_ref())
        .map(|labels| labels.nodes.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|label| trimmed(label.as_ref().map(|label| label.name.as_str())))
        .collect();
    let mut candidates: IndexMap<String, PullRequestLabelCandidate> = IndexMap::new();
    for node in repository
        .labels
        .as_ref()
        .map(|labels| labels.nodes.as_slice())
        .unwrap_or_default()
        .iter()
        .flatten()
    {
        let Some(name) = trimmed(Some(&node.label.name)) else {
            continue;
        };
        let Some(candidate_name) = non_empty(&name) else {
            continue;
        };
        candidates.insert(
            name.clone(),
            PullRequestLabelCandidate {
                name: candidate_name,
                color: trimmed(node.label.color.as_deref()),
                description: trimmed(node.description.as_deref()),
                is_applied: applied.contains(&name),
            },
        );
    }
    let missing: Vec<PullRequestLabelCandidate> = applied
        .iter()
        .filter(|name| !candidates.contains_key(*name))
        .filter_map(|name| {
            Some(PullRequestLabelCandidate {
                name: non_empty(name)?,
                color: None,
                description: None,
                is_applied: true,
            })
        })
        .collect();
    let all: Vec<PullRequestLabelCandidate> = missing
        .into_iter()
        .chain(candidates.into_values())
        .collect();
    Ok(PullRequestLabelCandidateList {
        candidates: all,
        truncated: repository
            .labels
            .as_ref()
            .and_then(|labels| labels.page_info.as_ref())
            .and_then(|page_info| page_info.has_next_page)
            == Some(true),
    })
}

/// The body of `POST /repos/{owner}/{repo}/issues/{number}/labels`, which adds to what is there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LabelRequest {
    pub labels: Vec<String>,
}

impl LabelRequest {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("a request body of strings serializes")
    }
}

pub fn build_label_request(labels: &[String]) -> LabelRequest {
    LabelRequest {
        labels: labels.to_vec(),
    }
}

// ---------------------------------------------------------------------------------------------
// Viewer permissions
// ---------------------------------------------------------------------------------------------

pub fn decode_viewer_permissions_json(raw: &str) -> DecodeResult<GitHubViewerRepositoryAccess> {
    let decoded: RawViewerPermissions = decode_json(raw)?;
    let repository = &decoded.data.repository;
    let viewer_permission = repository
        .access
        .viewer_permission
        .as_ref()
        .map(|permission| permission.as_deref());
    let viewer = to_pull_request_viewer_fields(repository.pull_request.as_ref());
    Ok(GitHubViewerRepositoryAccess {
        merge_capabilities: PullRequestMergeCapabilities {
            merge: repository.access.merge_commit_allowed,
            squash: repository.access.squash_merge_allowed,
            rebase: repository.access.rebase_merge_allowed,
        },
        access: GitHubViewerAccess {
            can_write: to_can_write(viewer_permission),
            can_triage: to_can_triage(viewer_permission),
            can_update: viewer.can_update,
            did_author: viewer.did_author,
            can_update_branch: None,
        },
    })
}

// ---------------------------------------------------------------------------------------------
// Review submission
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ReviewEvent {
    #[serde(rename = "COMMENT")]
    Comment,
    #[serde(rename = "APPROVE")]
    Approve,
    #[serde(rename = "REQUEST_CHANGES")]
    RequestChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ReviewSide {
    #[serde(rename = "LEFT")]
    Left,
    #[serde(rename = "RIGHT")]
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewSubmissionComment {
    pub path: String,
    pub line: u64,
    pub side: ReviewSide,
    pub body: String,
}

/// The body of `POST /repos/{owner}/{repo}/pulls/{number}/reviews`, which sends a review whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewSubmission {
    pub event: ReviewEvent,
    pub body: String,
    pub comments: Vec<ReviewSubmissionComment>,
}

impl ReviewSubmission {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("a review body serializes")
    }
}

fn github_review_position(position: &PullRequestReviewPosition) -> (u64, ReviewSide) {
    match position {
        PullRequestReviewPosition::Added { new_line } => (new_line.get(), ReviewSide::Right),
        PullRequestReviewPosition::Deleted { old_line } => (old_line.get(), ReviewSide::Left),
        PullRequestReviewPosition::Context {
            old_line,
            new_line,
            side,
        } => match side {
            turin_contracts::pull_request::PullRequestDiffSide::Left => {
                (old_line.get(), ReviewSide::Left)
            }
            turin_contracts::pull_request::PullRequestDiffSide::Right => {
                (new_line.get(), ReviewSide::Right)
            }
        },
    }
}

/// The whole review as one request body, which is how GitHub keeps it invisible until sent.
pub fn build_review_submission(
    verdict: PullRequestReviewVerdict,
    body: &str,
    comments: &[PullRequestReviewCommentDraft],
) -> ReviewSubmission {
    ReviewSubmission {
        event: match verdict {
            PullRequestReviewVerdict::Comment => ReviewEvent::Comment,
            PullRequestReviewVerdict::Approve => ReviewEvent::Approve,
            PullRequestReviewVerdict::RequestChanges => ReviewEvent::RequestChanges,
        },
        body: body.to_owned(),
        comments: comments
            .iter()
            .map(|comment| {
                let (line, side) = github_review_position(&comment.position);
                ReviewSubmissionComment {
                    path: comment.path.as_str().to_owned(),
                    line,
                    side,
                    body: comment.body.clone(),
                }
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------

/// How a file's name travels in a unified patch, which is not as itself. A reader finds the name
/// in a header by where it stops: `diff --git a/<old> b/<new>` splits on a space, `--- a/<name>`
/// stops at a tab. So a name holding a tab or a newline reads back as the part of itself before
/// that byte, or fabricates a header line of its own, and git's answer is to write such a name
/// quoted with C-style escapes.
///
/// Port of `quoteGitPatchPath` from upstream `packages/shared/src/gitPatchPath.ts`. Not every
/// byte git would escape: `core.quotePath` also escapes anything outside ASCII, which is a
/// setting for what a terminal can show rather than anything the format needs.
pub fn quote_git_patch_path(path: &str) -> String {
    let mut body = String::with_capacity(path.len());
    let mut quoting = false;
    for character in path.chars() {
        let escape = match character {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\u{7}' => Some("\\a"),
            '\u{8}' => Some("\\b"),
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\u{b}' => Some("\\v"),
            '\u{c}' => Some("\\f"),
            '\r' => Some("\\r"),
            _ => None,
        };
        if let Some(escape) = escape {
            body.push_str(escape);
            quoting = true;
            continue;
        }
        let code = character as u32;
        // Every character git has no name for is written as the octal of its byte, and a control
        // character is one byte in UTF-8, so the character's own code point is that byte.
        if code < 0x20 || code == 0x7f {
            body.push_str(&format!("\\{code:03o}"));
            quoting = true;
            continue;
        }
        body.push(character);
    }
    if quoting {
        format!("\"{body}\"")
    } else {
        path.to_owned()
    }
}

/// The files API returns hunks per file with no `diff --git` header, so the unified patch every
/// diff viewer expects is assembled here. This decodes one page; walking pages is the caller's
/// job, which is why the raw file count comes back with the patch.
pub fn decode_pull_request_files_json(raw: &str) -> DecodeResult<GitHubPullRequestFilesPatch> {
    let entries: Vec<Value> = decode_json(raw)?;
    Ok(to_files_patch(&entries))
}

/// The files of one commit, which the commit endpoint lists and pages the same way, only wrapped
/// in an object. An empty commit carries no `files` at all, which is a commit with nothing in it.
pub fn decode_commit_files_json(raw: &str) -> DecodeResult<GitHubPullRequestFilesPatch> {
    let commit: RawCommitFiles = decode_json(raw)?;
    Ok(to_files_patch(commit.files.as_deref().unwrap_or_default()))
}

fn to_files_patch(entries: &[Value]) -> GitHubPullRequestFilesPatch {
    let mut sections: Vec<String> = Vec::new();
    let mut omitted_file_stats: Vec<PullRequestOmittedFileStat> = Vec::new();
    let mut truncated = false;
    for entry in entries {
        let Ok(file) = RawPullRequestFile::deserialize(entry) else {
            continue;
        };
        let hunks = file.patch.as_deref().unwrap_or_default();
        let status = file
            .status
            .as_deref()
            .map(|status| crate::shared::js::js_trim(status).to_lowercase());
        if hunks.is_empty() {
            // A file with no hunks is still a file that changed: a pure rename has none to give,
            // and a binary one has none that can be shown. Both are listed, and only the second
            // is a hole in the patch; leaving them out entirely would drop them from the change
            // altogether.
            let additions = file.additions.map_or(0, |value| value.0);
            let deletions = file.deletions.map_or(0, |value| value.0);
            if additions + deletions > 0 {
                truncated = true;
                if let Some(path) = non_empty(&file.filename) {
                    omitted_file_stats.push(PullRequestOmittedFileStat {
                        path,
                        additions: additions as f64,
                        deletions: deletions as f64,
                    });
                }
            }
        }
        let is = |expected: &str| status.as_deref() == Some(expected);
        // A rename counts its hunks against the old path, which is the only place it is named.
        let old_path: &str = if is("renamed") {
            file.previous_filename
                .as_deref()
                .filter(|previous| !previous.is_empty())
                .unwrap_or(&file.filename)
        } else {
            &file.filename
        };
        let mut header: Vec<String> = vec![format!(
            "diff --git {} {}",
            quote_git_patch_path(&format!("a/{old_path}")),
            quote_git_patch_path(&format!("b/{}", file.filename))
        )];
        // The files API reports no file mode, so the ordinary one stands in: the viewer reads
        // these lines as "added" and "removed" rather than for the mode they carry.
        if is("added") {
            header.push("new file mode 100644".to_owned());
        }
        if is("removed") {
            header.push("deleted file mode 100644".to_owned());
        }
        if is("renamed") {
            header.push(format!("rename from {}", quote_git_patch_path(old_path)));
            header.push(format!(
                "rename to {}",
                quote_git_patch_path(&file.filename)
            ));
        }
        header.push(format!(
            "--- {}",
            if is("added") {
                "/dev/null".to_owned()
            } else {
                quote_git_patch_path(&format!("a/{old_path}"))
            }
        ));
        header.push(format!(
            "+++ {}",
            if is("removed") {
                "/dev/null".to_owned()
            } else {
                quote_git_patch_path(&format!("b/{}", file.filename))
            }
        ));
        let header = header.join("\n");
        if hunks.is_empty() {
            sections.push(format!("{header}\n"));
        } else if hunks.ends_with('\n') {
            sections.push(format!("{header}\n{hunks}"));
        } else {
            sections.push(format!("{header}\n{hunks}\n"));
        }
    }
    GitHubPullRequestFilesPatch {
        patch: sections.concat(),
        truncated,
        raw_count: entries.len(),
        omitted_file_stats,
    }
}

// ---------------------------------------------------------------------------------------------
// Viewed state
// ---------------------------------------------------------------------------------------------

/// Anything this host does not name is treated as unread, which is the state that asks for least.
fn to_file_viewed_state(raw: &str) -> PullRequestFileViewedState {
    match crate::shared::js::js_trim(raw).to_uppercase().as_str() {
        "VIEWED" => PullRequestFileViewedState::Viewed,
        "DISMISSED" => PullRequestFileViewedState::Dismissed,
        _ => PullRequestFileViewedState::Unviewed,
    }
}

pub fn decode_pull_request_files_viewed_json(
    raw: &str,
) -> DecodeResult<GitHubPullRequestFilesViewedPage> {
    let decoded: RawPullRequestFilesViewed = decode_json(raw)?;
    let Some(files) = decoded
        .data
        .repository
        .as_ref()
        .and_then(|repository| repository.pull_request.as_ref())
        .map(|pull_request| &pull_request.files)
    else {
        return Ok(GitHubPullRequestFilesViewedPage {
            files: Vec::new(),
            next_cursor: None,
        });
    };
    Ok(GitHubPullRequestFilesViewedPage {
        files: files
            .nodes
            .as_deref()
            .unwrap_or_default()
            .iter()
            .flatten()
            .filter(|node| !node.path.is_empty())
            .map(|node| GitHubViewedFile {
                path: node.path.clone(),
                state: to_file_viewed_state(&node.viewer_viewed_state),
            })
            .collect(),
        next_cursor: if files.page_info.has_next_page {
            files.page_info.end_cursor.clone()
        } else {
            None
        },
    })
}

/// One document that clears and restores as many files as the reader ticked, rather than one
/// request each. GitHub has no bulk form of `markFileAsViewed`/`unmarkFileAsViewed`, which each
/// take a single path, so the batching is done with aliases; top-level mutation fields run in
/// write order, so the last word about a path is the one that sticks.
pub fn build_set_files_viewed_graphql_mutation(
    pull_request_id: &str,
    files: &[(String, bool)],
) -> Option<GraphQlDocument> {
    let variables = |file: &(String, bool)| -> GraphQlVariables {
        vec![(
            "path".to_owned(),
            GraphQlVariable::new("String!", file.0.as_str()),
        )]
    };
    let field = |placeholders: &GraphQlPlaceholders, file: &(String, bool)| -> String {
        format!(
            "{}(input: {{ pullRequestId: $pullRequestId, path: {} }}) {{ clientMutationId }}",
            if file.1 {
                "markFileAsViewed"
            } else {
                "unmarkFileAsViewed"
            },
            placeholders["path"]
        )
    };
    aliased_graphql_document(AliasedGraphQlDocumentInput {
        operation: GraphQlOperation::Mutation,
        name: None,
        alias: "f",
        key: None,
        items: files,
        variables: &variables,
        field: &field,
        shared: vec![(
            "pullRequestId".to_owned(),
            GraphQlVariable::new("ID!", pull_request_id),
        )],
        within: None,
    })
}

// ---------------------------------------------------------------------------------------------
// Host-native stacks
// ---------------------------------------------------------------------------------------------

/// The first stack of a `?pull_request=` listing, or `None` for an empty one: a pull request is
/// in at most one stack, so the array is GitHub's way of saying "none" rather than a page.
pub fn decode_pull_request_stacks_json(raw: &str) -> DecodeResult<Option<GitHubPullRequestStack>> {
    let decoded: Vec<RawStackListing> = decode_json(raw)?;
    let Some(stack) = decoded.first() else {
        return Ok(None);
    };
    let id = match &stack.id {
        None => trimmed(stack.node_id.as_deref()).unwrap_or_else(|| stack.number.0.to_string()),
        Some(RawStackId::Number(id)) => id.0.to_string(),
        Some(RawStackId::Text(id)) => id.clone(),
    };
    Ok(Some(GitHubPullRequestStack {
        id,
        number: stack.number.non_negative(),
        // The page a person opens where the preview reports one; the API URL is what it always has.
        url: trimmed(stack.html_url.as_deref()).unwrap_or_else(|| stack.url.clone()),
        base: match &stack.base {
            RawStackBase::Name(name) => name.clone(),
            RawStackBase::Ref(base) => base.ref_name.clone(),
        },
        layers: stack
            .pull_requests
            .iter()
            .map(|pull_request| GitHubPullRequestStackLayer {
                title: pull_request.title.clone(),
                is_draft: pull_request.draft,
                head_sha: pull_request.head.sha.clone(),
                number: pull_request.number.non_negative(),
                head_branch: pull_request.head.ref_name.clone(),
                state: to_state(
                    pull_request.state.as_deref(),
                    pull_request.merged_at.as_deref(),
                ),
            })
            .collect(),
    }))
}
