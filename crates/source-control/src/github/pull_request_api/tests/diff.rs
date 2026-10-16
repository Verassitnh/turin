use serde_json::json;
use turin_contracts::pull_request::PullRequestDiffFileChangeType;

use super::super::diff::*;
use super::super::errors::{GitHubDiffFileContentsUnavailableReason, GitHubPullRequestApiError};
use super::super::test_support::*;
use crate::github::api::{GitHubApiError, RestMethod};

fn diff_input(
    harness: &Harness,
    cursor: Option<&str>,
    commit: Option<&str>,
) -> GetPullRequestDiffInput {
    GetPullRequestDiffInput {
        target: harness.target(7),
        cursor: cursor.map(str::to_owned),
        commit: commit.map(str::to_owned),
    }
}

fn contents_input(
    harness: &Harness,
    commit: Option<&str>,
    change_type: PullRequestDiffFileChangeType,
    path: &str,
) -> GetPullRequestDiffFileContentsInput {
    GetPullRequestDiffFileContentsInput {
        target: harness.target(7),
        commit: commit.map(str::to_owned),
        change_type,
        old_path: path.into(),
        new_path: path.into(),
    }
}

fn is_diff_refused(error: &GitHubPullRequestApiError) -> bool {
    matches!(
        error,
        GitHubPullRequestApiError::Api(GitHubApiError::Response(response))
            if response.status == 406 && response.operation == "getPullRequestDiff"
    )
}

#[tokio::test]
async fn serves_a_diff_github_hands_over_whole_in_one_request_with_no_next_slice() {
    let harness = Harness::new();
    harness.once_body("diff --git a/a b/a");
    let diff = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, None))
        .await
        .unwrap();
    assert_eq!(diff.patch, "diff --git a/a b/a");
    assert_eq!(diff.next_cursor, None);
    assert!(!diff.truncated);
    // The common case pays for one request and not the files API on top of it.
    assert_eq!(harness.call_count(), 1);
    // GitHub's combined pull-request diff: one section per changed file, not one per commit.
    let call = harness.call_at(0);
    assert!(call.is_rest());
    assert_eq!(call.path(), "repos/acme/web/pulls/7");
    assert_eq!(
        call.rest().accept.as_deref(),
        Some("application/vnd.github.diff")
    );
}

#[tokio::test]
async fn reads_one_files_page_when_github_refuses_the_diff_and_says_it_is_the_last() {
    // GitHub answers 406 rather than a diff past 300 changed files.
    let harness = Harness::new();
    harness.once(Err(diff_refused()));
    harness.once_body(pull_request_files(2, 1));
    let mut input = diff_input(&harness, None, None);
    input.target.host = "github.acme.dev".into();
    let diff = harness.cli.get_pull_request_diff(&input).await.unwrap();
    assert!(!diff.truncated);
    // A short page is the end of the change set, so there is nothing to carry on from.
    assert_eq!(diff.next_cursor, None);
    assert!(
        diff.patch
            .contains("diff --git a/src/file1.ts b/src/file1.ts")
    );
    assert!(
        diff.patch
            .contains("diff --git a/src/file2.ts b/src/file2.ts")
    );
    assert_eq!(harness.call_at(1).host(), "github.acme.dev");
    assert_eq!(
        harness.path_at(1),
        "repos/acme/web/pulls/7/files?per_page=100&page=1"
    );
}

#[tokio::test]
async fn hands_back_a_cursor_for_the_next_page_rather_than_walking_on_by_itself() {
    let harness = Harness::new();
    harness.once(Err(diff_refused()));
    harness.once_body(pull_request_files(100, 0));
    let diff = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, None))
        .await
        .unwrap();
    // A full page means more files, which the reader asks for; it is not a truncated slice.
    assert!(!diff.truncated);
    assert!(diff.next_cursor.is_some());
    assert_eq!(harness.call_count(), 2);
}

#[tokio::test]
async fn carries_on_from_a_cursor_without_asking_for_the_whole_diff_again() {
    let harness = Harness::new();
    harness.once(Err(diff_refused()));
    harness.once_body(pull_request_files(100, 0));
    let first = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, None))
        .await
        .unwrap();
    assert!(first.next_cursor.is_some());
    harness.once_body(pull_request_files(4, 100));
    let second = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, first.next_cursor.as_deref(), None))
        .await
        .unwrap();
    assert_eq!(second.next_cursor, None);
    assert!(
        second
            .patch
            .contains("diff --git a/src/file100.ts b/src/file100.ts")
    );
    // The second slice is one request: the cursor already says where to read.
    assert_eq!(harness.call_count(), 3);
    assert_eq!(
        harness.path_at(2),
        "repos/acme/web/pulls/7/files?per_page=100&page=2"
    );
}

#[tokio::test]
async fn refuses_a_cursor_it_never_handed_out_rather_than_reading_it_into_a_request() {
    let harness = Harness::new();
    let error = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, Some("1&per_page=1"), None))
        .await
        .unwrap_err();
    assert!(matches!(error, GitHubPullRequestApiError::DiffCursor(_)));
    assert_eq!(harness.call_count(), 0);
}

#[tokio::test]
async fn reads_a_named_commit_from_the_commit_endpoint_rather_than_the_whole_diff() {
    let harness = Harness::new();
    harness.once_body(format!("{{\"files\":{}}}", pull_request_files(2, 1)));
    let diff = harness
        .cli
        .get_pull_request_diff(&diff_input(
            &harness,
            None,
            Some("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"),
        ))
        .await
        .unwrap();
    // One request: the commit's own changes never take the whole-diff road.
    assert_eq!(harness.call_count(), 1);
    assert_eq!(diff.next_cursor, None);
    // The commit endpoint wraps its files in an object, which the decoder unwraps.
    assert!(
        diff.patch
            .contains("diff --git a/src/file1.ts b/src/file1.ts")
    );
    assert_eq!(
        harness.path_at(0),
        "repos/acme/web/commits/a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0?per_page=100&page=1"
    );
}

#[tokio::test]
async fn pages_inside_a_commit_the_way_it_pages_the_pull_requests_own_files() {
    let harness = Harness::new();
    harness.once_body(format!("{{\"files\":{}}}", pull_request_files(100, 0)));
    let first = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, Some("a1b2c3d")))
        .await
        .unwrap();
    assert!(first.next_cursor.is_some());
    harness.once_body(format!("{{\"files\":{}}}", pull_request_files(4, 100)));
    let second = harness
        .cli
        .get_pull_request_diff(&diff_input(
            &harness,
            first.next_cursor.as_deref(),
            Some("a1b2c3d"),
        ))
        .await
        .unwrap();
    assert_eq!(second.next_cursor, None);
    assert!(second.patch.contains("src/file100.ts"));
    assert_eq!(
        harness.path_at(1),
        "repos/acme/web/commits/a1b2c3d?per_page=100&page=2"
    );
}

#[tokio::test]
async fn refuses_a_commit_that_is_not_a_sha_rather_than_reading_it_into_a_request() {
    let harness = Harness::new();
    let error = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, Some("../../pulls/8/files")))
        .await
        .unwrap_err();
    assert!(matches!(error, GitHubPullRequestApiError::DiffCommit(_)));
    assert_eq!(harness.call_count(), 0);
}

#[tokio::test]
async fn expands_a_new_file_from_a_root_commit_without_requiring_a_parent() {
    let harness = Harness::new();
    harness.once_body(json!({ "sha": "a1b2c3d", "parents": [] }).to_string());
    harness.once_body("root contents\n");
    let contents = harness
        .cli
        .get_pull_request_diff_file_contents(&contents_input(
            &harness,
            Some("a1b2c3d"),
            PullRequestDiffFileChangeType::New,
            "src/root.ts",
        ))
        .await
        .unwrap();
    assert_eq!(contents.old_contents, "");
    assert_eq!(contents.new_contents, "root contents\n");
    assert_eq!(harness.call_count(), 2);
    assert_eq!(harness.path_at(0), "repos/acme/web/commits/a1b2c3d");
    assert_eq!(
        harness.path_at(1),
        "repos/acme/web/contents/src/root.ts?ref=a1b2c3d"
    );
}

#[tokio::test]
async fn reports_unusable_diff_revisions_as_a_structured_error() {
    let harness = Harness::new();
    harness.once_body("not-a-sha\tstill-not-a-sha\n");
    let error = harness
        .cli
        .get_pull_request_diff_file_contents(&contents_input(
            &harness,
            Some("a1b2c3d"),
            PullRequestDiffFileChangeType::Change,
            "src/a.ts",
        ))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::DiffRevisionsUnavailable(unavailable) => {
            assert_eq!(unavailable.number, 7);
            assert_eq!(unavailable.commit.as_deref(), Some("a1b2c3d"));
        }
        other => panic!("expected unusable revisions, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_an_oversized_diff_file_with_its_path_and_reason() {
    let harness = Harness::new();
    harness.once_body(pull_request_refs());
    harness.once(Ok(output_truncated("partial")));
    let error = harness
        .cli
        .get_pull_request_diff_file_contents(&contents_input(
            &harness,
            None,
            PullRequestDiffFileChangeType::Deleted,
            "src/large.ts",
        ))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::DiffFileContentsUnavailable(unavailable) => {
            assert_eq!(unavailable.path, "src/large.ts");
            assert_eq!(
                unavailable.reason,
                GitHubDiffFileContentsUnavailableReason::Oversized
            );
            assert_eq!(
                unavailable.to_string(),
                "The diff file 'src/large.ts' exceeds the 1 MB expansion limit."
            );
        }
        other => panic!("expected unavailable contents, got {other:?}"),
    }
    // A deleted file is read at the base revision only.
    assert_eq!(
        harness.path_at(1),
        "repos/acme/web/contents/src/large.ts?ref=a1b2c3d"
    );
}

#[tokio::test]
async fn reports_undecodable_diff_file_contents_as_binary() {
    let harness = Harness::new();
    harness.once_body(pull_request_refs());
    harness.once(Ok(output_with(
        "binary\u{fffd}contents",
        false,
        true,
        Default::default(),
    )));
    let error = harness
        .cli
        .get_pull_request_diff_file_contents(&contents_input(
            &harness,
            None,
            PullRequestDiffFileChangeType::Deleted,
            "assets/logo.png",
        ))
        .await
        .unwrap_err();
    match error {
        GitHubPullRequestApiError::DiffFileContentsUnavailable(unavailable) => {
            assert_eq!(unavailable.path, "assets/logo.png");
            assert_eq!(
                unavailable.reason,
                GitHubDiffFileContentsUnavailableReason::Binary
            );
        }
        other => panic!("expected unavailable contents, got {other:?}"),
    }
}

#[tokio::test]
async fn returns_valid_text_containing_a_literal_replacement_character() {
    let harness = Harness::new();
    harness.once_body(pull_request_refs());
    harness.once_body("before\u{fffd}after");
    let contents = harness
        .cli
        .get_pull_request_diff_file_contents(&contents_input(
            &harness,
            None,
            PullRequestDiffFileChangeType::Deleted,
            "docs/encoding.md",
        ))
        .await
        .unwrap();
    assert_eq!(contents.old_contents, "before\u{fffd}after");
}

#[tokio::test]
async fn ends_the_diff_on_a_page_with_no_files_rather_than_asking_for_it_again() {
    let harness = Harness::new();
    harness.always(ok("[]"));
    let diff = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, Some("4"), None))
        .await
        .unwrap();
    assert_eq!(diff.patch, "");
    assert_eq!(diff.next_cursor, None);
}

#[tokio::test]
async fn reports_the_refused_diff_when_the_files_api_cannot_answer_either() {
    let harness = Harness::new();
    harness.once(Err(diff_refused()));
    harness.once_body("not json");
    let error = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, None))
        .await
        .unwrap_err();
    assert!(is_diff_refused(&error));
}

#[tokio::test]
async fn fails_a_files_page_too_large_to_read_rather_than_calling_the_diff_whole() {
    let harness = Harness::new();
    harness.once(Err(diff_refused()));
    harness.once(Ok(output_truncated(pull_request_files(1, 1))));
    let error = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, None))
        .await
        .unwrap_err();
    // What matters is that it fails at all: an empty patch with no cursor would render as a
    // change with no files and report the rest of it as already read. The refusal that sent the
    // read down this road is the one reported, by design.
    assert!(is_diff_refused(&error));
}

#[tokio::test]
async fn pages_an_oversized_patch_by_file_rather_than_handing_back_a_severed_one() {
    // The whole diff came back, but cut at a byte, which lands mid-file.
    let harness = Harness::new();
    harness.once(Ok(output_truncated("diff --git a/a b/a\n@@ -1 +1 @@")));
    harness.once_body(pull_request_files(1, 1));
    let slice = harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, None))
        .await
        .unwrap();
    // The severed patch is thrown away; what comes back is assembled from whole files.
    assert_eq!(
        harness.path_at(1),
        "repos/acme/web/pulls/7/files?per_page=100&page=1"
    );
    assert!(slice.patch.contains("src/file1.ts"));
    assert!(!slice.patch.contains("diff --git a/a b/a"));
    assert_eq!(harness.call_count(), 2);
}

#[tokio::test]
async fn sends_the_diff_limits_with_every_rest_read() {
    // Not an upstream test: pins the 8 MiB / 60 s limits the patch reads carry.
    let harness = Harness::new();
    harness.once_body("diff --git a/a b/a");
    harness
        .cli
        .get_pull_request_diff(&diff_input(&harness, None, None))
        .await
        .unwrap();
    let call = harness.call_at(0);
    let rest = call.rest();
    assert_eq!(rest.method, RestMethod::Get);
    assert_eq!(rest.max_response_bytes, Some(8 * 1024 * 1024));
    assert_eq!(rest.timeout, Some(std::time::Duration::from_secs(60)));
}
