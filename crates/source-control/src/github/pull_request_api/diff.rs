//! The patch and file contents (`getPullRequestDiff`, `getPullRequestDiffFileContents`).

use std::sync::LazyLock;
use std::time::Duration;

use futures::future::try_join;
use regex::Regex;
use serde::Deserialize;
use turin_contracts::pull_request::{PullRequestDiffFileChangeType, PullRequestOmittedFileStat};

use super::errors::{
    GitHubDiffCommitError, GitHubDiffCursorError, GitHubDiffFileContentsUnavailableError,
    GitHubDiffFileContentsUnavailableReason, GitHubDiffRevisionsUnavailableError,
    GitHubPullRequestApiError,
};
use super::{
    GitHubPullRequestApi, GitHubPullRequestTarget, RepositorySelector, parse_repository_selector,
};
use crate::github::api::{GitHubApiError, GitHubRestInput};
use crate::github::pull_request_json::de;
use crate::github::pull_request_json::{decode_commit_files_json, decode_pull_request_files_json};
use crate::shared::js::encode_uri_component;

/// A large pull request can produce a multi-megabyte patch; past this it is truncated.
const DIFF_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const DIFF_TIMEOUT: Duration = Duration::from_secs(60);
/// Pierre expansion is for source files, not blobs large enough to stall a review surface.
const DIFF_FILE_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// What the files API serves at most in one response, which is what one slice is made of.
const DIFF_FILES_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestDiffSlice {
    pub patch: String,
    /// Files in this slice had their hunks withheld, as opposed to there being more slices.
    pub truncated: bool,
    /// Where the next slice starts, or `None` once the patch is whole.
    pub next_cursor: Option<String>,
    /// GitHub's own counts for the files whose hunks it withheld from this slice.
    pub omitted_file_stats: Option<Vec<PullRequestOmittedFileStat>>,
}

#[derive(Debug, Clone)]
pub struct GetPullRequestDiffInput {
    pub target: GitHubPullRequestTarget,
    /// Absent asks for the first slice; anything else is a cursor a slice handed back.
    pub cursor: Option<String>,
    /// One commit's own changes, rather than everything the pull request carries.
    pub commit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GetPullRequestDiffFileContentsInput {
    pub target: GitHubPullRequestTarget,
    pub commit: Option<String>,
    pub change_type: PullRequestDiffFileChangeType,
    pub old_path: String,
    pub new_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestDiffFileContents {
    pub old_contents: String,
    pub new_contents: String,
}

static DIFF_CURSOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[1-9][0-9]{0,6}$").expect("static regex"));
static COMMIT_SHA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{7,64}$").expect("static regex"));

/// The page a diff cursor names, or `None` for anything this walk cannot have issued. The cursor
/// arrives from the reader as a string and goes straight into a request path, so it is parsed
/// rather than trusted; the length bound keeps a page number out of exponential notation.
fn diff_cursor_page(cursor: &str) -> Option<u64> {
    DIFF_CURSOR
        .is_match(cursor)
        .then(|| cursor.parse().ok())
        .flatten()
}

/// A commit sha arrives from the reader and goes straight into a request path, so it is checked
/// rather than trusted: hexadecimal only, from the shortest abbreviation a host prints up to a
/// whole sha.
fn is_commit_sha(value: &str) -> bool {
    COMMIT_SHA.is_match(value)
}

/// The two revisions a file is expanded between: a pull request's base and head, or a commit's
/// parent and itself. Any shape mismatch makes the whole value unusable (`decodeUnknownOption`).
#[derive(Deserialize)]
struct RevisionRefs {
    #[serde(default, deserialize_with = "de::opt")]
    sha: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    parents: Option<Vec<ShaRef>>,
    #[serde(default, deserialize_with = "de::opt")]
    base: Option<ShaRef>,
    #[serde(default, deserialize_with = "de::opt")]
    head: Option<ShaRef>,
}

#[derive(Deserialize)]
struct ShaRef {
    sha: String,
}

impl GitHubPullRequestApi {
    /// One page of the patch, read from the files API. GitHub refuses a whole diff past 300
    /// changed files, and still serves those files' hunks here.
    ///
    /// A page is a whole number of files, so each one parses on its own; the caller carries on
    /// from `next_cursor` for as long as GitHub keeps handing pages back.
    ///
    /// A named commit is read from the commit endpoint, which lists the same file entries and
    /// pages them the same way, only wrapped in an object.
    async fn diff_files_page(
        &self,
        target: &GitHubPullRequestTarget,
        page: u64,
        commit: Option<&str>,
    ) -> Result<GitHubPullRequestDiffSlice, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let paging = format!("per_page={DIFF_FILES_PAGE_SIZE}&page={page}");
        let path = match commit {
            None => format!(
                "repos/{owner}/{name}/pulls/{}/files?{paging}",
                target.number
            ),
            Some(commit) => format!("repos/{owner}/{name}/commits/{commit}?{paging}"),
        };
        let response = self
            .api()
            .rest(GitHubRestInput {
                host: target.host.clone(),
                operation: "getPullRequestDiff".into(),
                path,
                max_response_bytes: Some(DIFF_MAX_OUTPUT_BYTES),
                timeout: Some(DIFF_TIMEOUT),
                ..Default::default()
            })
            .await?;
        // Checked before decoding: a byte-truncated response is a JSON prefix, which would fail to
        // parse. Nothing of this page can be shown, and an empty patch would render as a change
        // with no files rather than as the failure it is; slices already handed over stay with
        // the reader either way.
        if response.truncated {
            return Err(Self::read_error_message(
                &target.cwd,
                "getPullRequestDiff",
                format!("Page {page} of the changed files was too large to read."),
            ));
        }
        let files = Self::decode_with(
            &target.cwd,
            "getPullRequestDiff",
            if commit.is_none() {
                decode_pull_request_files_json
            } else {
                decode_commit_files_json
            },
            &response.body,
        )?;
        // Counted before decoding, so a page whose files all failed to decode still moves on
        // rather than pointing the reader back at the page it just read.
        let more_pages = files.raw_count >= DIFF_FILES_PAGE_SIZE;
        Ok(GitHubPullRequestDiffSlice {
            patch: files.patch,
            truncated: files.truncated,
            next_cursor: more_pages.then(|| (page + 1).to_string()),
            omitted_file_stats: (!files.omitted_file_stats.is_empty())
                .then_some(files.omitted_file_stats),
        })
    }

    pub async fn get_pull_request_diff(
        &self,
        input: &GetPullRequestDiffInput,
    ) -> Result<GitHubPullRequestDiffSlice, GitHubPullRequestApiError> {
        let target = &input.target;
        let commit = input.commit.as_deref();
        if commit.is_some_and(|commit| !is_commit_sha(commit)) {
            return Err(GitHubDiffCommitError {
                cwd: target.cwd.clone(),
            }
            .into());
        }
        // A cursor only ever comes from the files walk, so a reader carrying one is already past
        // the point where `gh pr diff` had anything to say.
        if let Some(cursor) = &input.cursor {
            return match diff_cursor_page(cursor) {
                None => Err(GitHubDiffCursorError {
                    cwd: target.cwd.clone(),
                }
                .into()),
                Some(page) => self.diff_files_page(target, page, commit).await,
            };
        }
        // `gh pr diff` speaks for the whole pull request and has no way to name one commit of it.
        if commit.is_some() {
            return self.diff_files_page(target, 1, commit).await;
        }
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let direct = async {
            let response = self
                .api()
                .rest(GitHubRestInput {
                    host: target.host.clone(),
                    operation: "getPullRequestDiff".into(),
                    path: format!("repos/{owner}/{name}/pulls/{}", target.number),
                    accept: Some("application/vnd.github.diff".into()),
                    max_response_bytes: Some(DIFF_MAX_OUTPUT_BYTES),
                    timeout: Some(DIFF_TIMEOUT),
                    ..Default::default()
                })
                .await?;
            // A patch cut at a byte boundary ends mid-file, which is neither a whole slice nor
            // something the reader can carry on from. The files API can serve the same change a
            // whole number of files at a time, so an oversized patch takes that road as well.
            if response.truncated {
                self.diff_files_page(target, 1, None).await
            } else {
                // One read served the whole patch, so there is no next slice to ask for.
                Ok(GitHubPullRequestDiffSlice {
                    patch: response.body,
                    truncated: false,
                    next_cursor: None,
                    omitted_file_stats: None,
                })
            }
        }
        .await;
        match direct {
            // GitHub answers 406 rather than a diff past 300 changed files, so the patch is read
            // from the files API instead, a page per call. Only once the direct read has failed: a
            // pull request GitHub will serve a diff for must not pay for a second request. A
            // fallback that fails too reports the original refusal, which is the one that explains
            // the page. Narrowed to a request GitHub answered and refused: a missing credential or
            // a rate limit fails the same way for every request.
            Err(GitHubPullRequestApiError::Api(GitHubApiError::Response(error))) => self
                .diff_files_page(target, 1, None)
                .await
                .map_err(|_| GitHubPullRequestApiError::Api(GitHubApiError::Response(error))),
            other => other,
        }
    }

    async fn read_diff_file(
        &self,
        target: &GitHubPullRequestTarget,
        revision: &str,
        file_path: &str,
    ) -> Result<String, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let encoded_path = file_path
            .split('/')
            .map(encode_uri_component)
            .collect::<Vec<_>>()
            .join("/");
        let response = self
            .api()
            .rest(GitHubRestInput {
                host: target.host.clone(),
                operation: "getPullRequestDiffFileContents".into(),
                accept: Some("application/vnd.github.raw+json".into()),
                path: format!(
                    "repos/{owner}/{name}/contents/{encoded_path}?ref={}",
                    encode_uri_component(revision)
                ),
                max_response_bytes: Some(DIFF_FILE_MAX_OUTPUT_BYTES),
                timeout: Some(DIFF_TIMEOUT),
                ..Default::default()
            })
            .await?;
        if response.truncated || response.body.contains('\0') || response.invalid_utf8 {
            return Err(GitHubDiffFileContentsUnavailableError {
                cwd: target.cwd.clone(),
                path: file_path.to_owned(),
                reason: if response.truncated {
                    GitHubDiffFileContentsUnavailableReason::Oversized
                } else {
                    GitHubDiffFileContentsUnavailableReason::Binary
                },
            }
            .into());
        }
        Ok(response.body)
    }

    pub async fn get_pull_request_diff_file_contents(
        &self,
        input: &GetPullRequestDiffFileContentsInput,
    ) -> Result<GitHubPullRequestDiffFileContents, GitHubPullRequestApiError> {
        let target = &input.target;
        let commit = input.commit.as_deref();
        if commit.is_some_and(|commit| !is_commit_sha(commit)) {
            return Err(GitHubDiffCommitError {
                cwd: target.cwd.clone(),
            }
            .into());
        }
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let refs_response = self
            .api()
            .rest(GitHubRestInput {
                host: target.host.clone(),
                operation: "getPullRequestDiffFileContents".into(),
                path: match commit {
                    None => format!("repos/{owner}/{name}/pulls/{}", target.number),
                    Some(commit) => format!("repos/{owner}/{name}/commits/{commit}"),
                },
                timeout: Some(DIFF_TIMEOUT),
                ..Default::default()
            })
            .await?;
        let refs: Option<RevisionRefs> = serde_json::from_str(&refs_response.body).ok();
        // A root commit has no parent, which is an absent old revision. Every file in it is new,
        // so that is a usable answer whenever the caller does not need the old side.
        let base_ref: Option<String> = refs.as_ref().and_then(|refs| match commit {
            None => refs.base.as_ref().map(|base| base.sha.clone()),
            Some(_) => Some(
                refs.parents
                    .as_ref()
                    .and_then(|parents| parents.first())
                    .map_or_else(String::new, |parent| parent.sha.clone()),
            ),
        });
        let head_ref: Option<String> = refs.as_ref().and_then(|refs| match commit {
            None => refs.head.as_ref().map(|head| head.sha.clone()),
            Some(_) => refs.sha.clone(),
        });
        let root_commit_new_file = commit.is_some()
            && input.change_type == PullRequestDiffFileChangeType::New
            && base_ref.as_deref() == Some("");
        let usable = match &head_ref {
            None => false,
            Some(head) => {
                (root_commit_new_file || base_ref.as_deref().is_some_and(is_commit_sha))
                    && is_commit_sha(head)
            }
        };
        let Some(head_ref) = head_ref.filter(|_| usable) else {
            return Err(GitHubDiffRevisionsUnavailableError {
                cwd: target.cwd.clone(),
                number: target.number,
                commit: input.commit.clone(),
            }
            .into());
        };
        let old = async {
            if input.change_type == PullRequestDiffFileChangeType::New {
                Ok(String::new())
            } else {
                self.read_diff_file(target, base_ref.as_deref().unwrap_or(""), &input.old_path)
                    .await
            }
        };
        let new = async {
            if input.change_type == PullRequestDiffFileChangeType::Deleted {
                Ok(String::new())
            } else {
                self.read_diff_file(target, &head_ref, &input.new_path)
                    .await
            }
        };
        let (old_contents, new_contents) = try_join(old, new).await?;
        Ok(GitHubPullRequestDiffFileContents {
            old_contents,
            new_contents,
        })
    }
}
