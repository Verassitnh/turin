//! GraphQL documents and the builders for the ones that vary.
//!
//! The text is upstream's byte for byte, interpolations expanded; the document is the contract
//! with GitHub and with the recorded traffic the upstream tests replay.

use std::fmt::Write as _;

/// A reaction group as every reactable node reports it. `reactors` is bounded rather than paged:
/// a hover says who reacted, and a hundred and forty names is a count, not a sentence.
pub const REACTION_GROUPS_FIELDS: &str = r##"reactionGroups {
  content
  viewerHasReacted
  reactors(first: 10) {
    totalCount
    nodes {
      ... on User { login }
      ... on Bot { login }
      ... on Organization { login }
      ... on Mannequin { login }
    }
  }
}"##;

pub const ACTOR_AVATARS_GRAPHQL_QUERY: &str = r##"query($ids: [ID!]!) {
  nodes(ids: $ids) {
    ... on User { login avatarUrl }
    ... on Bot { login avatarUrl }
  }
}"##;

pub const PULL_REQUEST_PREVIEW_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      number title url state isDraft createdAt
      author { login avatarUrl ... on User { name } }
    }
  }
}"##;

pub const PULL_REQUEST_ACTIVITY_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!, $head: Boolean!, $withComments: Boolean!, $commentsAfter: String, $withReviews: Boolean!, $reviewsAfter: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      author @include(if: $head) { __typename login avatarUrl ... on User { name } }
      commits(last: 100) @include(if: $head) {
        nodes { commit { oid messageHeadline committedDate authors(first: 10) { nodes { name email user { login } } } } }
      }
      comments(first: 100, after: $commentsAfter) @include(if: $withComments) {
        pageInfo { hasNextPage endCursor }
        nodes { id body createdAt lastEditedAt url author { __typename login avatarUrl ... on User { name } } }
      }
      reviews(first: 100, after: $reviewsAfter) @include(if: $withReviews) {
        pageInfo { hasNextPage endCursor }
        nodes { id body state submittedAt lastEditedAt url author { __typename login avatarUrl ... on User { name } } }
      }
    }
  }
}"##;

pub const REVIEW_THREADS_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  viewer { login }
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        totalCount
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          diffSide
          comments(first: 10) {
            totalCount
            pageInfo { hasNextPage endCursor }
            nodes { id author { __typename login avatarUrl } body createdAt lastEditedAt url reactionGroups {
  content
  viewerHasReacted
  reactors(first: 10) {
    totalCount
    nodes {
      ... on User { login }
      ... on Bot { login }
      ... on Organization { login }
      ... on Mannequin { login }
    }
  }
} }
          }
        }
      }
      viewerCanUpdate
      viewerDidAuthor
      author { __typename login avatarUrl }
      reactionGroups {
  content
  viewerHasReacted
  reactors(first: 10) {
    totalCount
    nodes {
      ... on User { login }
      ... on Bot { login }
      ... on Organization { login }
      ... on Mannequin { login }
    }
  }
}
      comments(first: 100) {
        nodes { id lastEditedAt author { __typename login avatarUrl } reactionGroups {
  content
  viewerHasReacted
  reactors(first: 10) {
    totalCount
    nodes {
      ... on User { login }
      ... on Bot { login }
      ... on Organization { login }
      ... on Mannequin { login }
    }
  }
} }
      }
      reviews(first: 100) { nodes { id lastEditedAt author { __typename login avatarUrl } reactionGroups {
  content
  viewerHasReacted
  reactors(first: 10) {
    totalCount
    nodes {
      ... on User { login }
      ... on Bot { login }
      ... on Organization { login }
      ... on Mannequin { login }
    }
  }
} } }
      reviewRequests(first: 50) {
        nodes {
          requestedReviewer {
            ... on User { login name avatarUrl }
            ... on Bot { __typename login avatarUrl }
            ... on Team { combinedSlug name avatarUrl }
          }
        }
      }
      latestReviews(first: 50) {
        nodes { state author { __typename login avatarUrl } }
      }
      reviewDismissals: timelineItems(itemTypes: [REVIEW_DISMISSED_EVENT], first: 100) {
        pageInfo { hasNextPage endCursor }
        nodes { ... on ReviewDismissedEvent { dismissalMessage review { id } } }
      }
      commits(last: 100) {
        nodes {
          commit {
            oid
            messageHeadline
            committedDate
            additions
            deletions
            parents(first: 1) { totalCount }
            authors(first: 3) { nodes { name avatarUrl user { login } } }
          }
        }
      }
    }
  }
}"##;

pub const REVIEW_THREAD_COMMENTS_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!, $threadId: ID!, $cursor: String) {
  viewer { login }
  repository(owner: $owner, name: $name) { pullRequest(number: $number) { id } }
  node(id: $threadId) {
    ... on PullRequestReviewThread {
      pullRequest { id }
      comments(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { id author { __typename login avatarUrl } body createdAt lastEditedAt url reactionGroups {
  content
  viewerHasReacted
  reactors(first: 10) {
    totalCount
    nodes {
      ... on User { login }
      ... on Bot { login }
      ... on Organization { login }
      ... on Mannequin { login }
    }
  }
} }
      }
    }
  }
}"##;

pub const REVIEW_THREAD_REPLY_GRAPHQL_MUTATION: &str = r##"mutation($threadId: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: { pullRequestReviewThreadId: $threadId, body: $body }) {
    comment { id }
  }
}"##;

pub const PULL_REQUEST_NODE_ID_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) { pullRequest(number: $number) { id } }
}"##;

pub const REACTION_SUBJECT_PULL_REQUEST_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!, $subjectId: ID!) {
  repository(owner: $owner, name: $name) { pullRequest(number: $number) { id } }
  node(id: $subjectId) {
    id
    ... on IssueComment { pullRequest { id } }
    ... on PullRequestReviewComment { pullRequest { id } }
    ... on PullRequestReview { pullRequest { id } }
  }
}"##;

pub const ADD_REACTION_GRAPHQL_MUTATION: &str = r##"mutation($subjectId: ID!, $content: ReactionContent!) {
  addReaction(input: { subjectId: $subjectId, content: $content }) { reaction { content } }
}"##;

pub const REMOVE_REACTION_GRAPHQL_MUTATION: &str = r##"mutation($subjectId: ID!, $content: ReactionContent!) {
  removeReaction(input: { subjectId: $subjectId, content: $content }) { reaction { content } }
}"##;

pub const RESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION: &str = r##"mutation($threadId: ID!) {
  resolveReviewThread(input: { threadId: $threadId }) { thread { isResolved } }
}"##;

pub const UNRESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION: &str = r##"mutation($threadId: ID!) {
  unresolveReviewThread(input: { threadId: $threadId }) { thread { isResolved } }
}"##;

pub const UPDATE_PULL_REQUEST_GRAPHQL_MUTATION: &str = r##"mutation($pullRequestId: ID!, $title: String, $body: String) {
  updatePullRequest(input: { pullRequestId: $pullRequestId, title: $title, body: $body }) {
    pullRequest { id }
  }
}"##;

pub const REVERT_PULL_REQUEST_GRAPHQL_MUTATION: &str = r##"mutation($pullRequestId: ID!) {
  revertPullRequest(input: { pullRequestId: $pullRequestId }) {
    revertPullRequest { id }
  }
}"##;

pub const UPDATE_ISSUE_COMMENT_GRAPHQL_MUTATION: &str = r##"mutation($commentId: ID!, $body: String!) {
  updateIssueComment(input: { id: $commentId, body: $body }) { issueComment { id } }
}"##;

pub const UPDATE_REVIEW_COMMENT_GRAPHQL_MUTATION: &str = r##"mutation($commentId: ID!, $body: String!) {
  updatePullRequestReviewComment(input: { pullRequestReviewCommentId: $commentId, body: $body }) {
    pullRequestReviewComment { id }
  }
}"##;

pub const REVIEW_DISMISSALS_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      timelineItems(itemTypes: [REVIEW_DISMISSED_EVENT], first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { ... on ReviewDismissedEvent { dismissalMessage review { id } } }
      }
    }
  }
}"##;

pub const PULL_REQUEST_HEADS_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $head: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: 100, after: $after, states: [OPEN], headRefName: $head) {
      pageInfo { hasNextPage endCursor }
      nodes { number headRefOid isCrossRepository headRepositoryOwner { login } }
    }
  }
}"##;

pub const REVIEWER_CANDIDATES_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    assignableUsers(first: 100) {
      pageInfo { hasNextPage }
      nodes { login name avatarUrl }
    }
    pullRequest(number: $number) {
      author { login }
      reviewRequests(first: 100) {
        nodes {
          requestedReviewer {
            ... on User { login name avatarUrl }
            ... on Team { slug combinedSlug name avatarUrl }
            ... on Bot { login avatarUrl }
          }
        }
      }
    }
  }
}"##;

pub const LABEL_CANDIDATES_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    labels(first: 100, orderBy: { field: NAME, direction: ASC }) {
      pageInfo { hasNextPage }
      nodes { name color description }
    }
    pullRequest(number: $number) {
      labels(first: 100) { nodes { name } }
    }
  }
}"##;

pub const VIEWER_PERMISSIONS_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerPermission
    pullRequest(number: $number) { viewerCanUpdate viewerDidAuthor }
  }
}"##;

pub const PULL_REQUEST_FILES_VIEWED_GRAPHQL_QUERY: &str = r##"query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      files(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes { path viewerViewedState }
      }
    }
  }
}"##;

const CHECK_CONTEXT_NODES_TEMPLATE: &str = r##"nodes {
            __typename
            ... on StatusContext { context state targetUrl createdAt description@@REQUIRED@@ }
            ... on CheckRun {
              name status conclusion startedAt completedAt detailsUrl@@REQUIRED@@
              checkSuite { workflowRun { workflow { name } } }
            }
          }"##;
const CORE_QUERY_TEMPLATE: &str = r##"query($owner: String!, $name: String!, $number: Int!, $headRef: String!) {
  repository(owner: $owner, name: $name) {
    mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerPermission
    pullRequest(number: $number) {
      number title url body state isDraft mergeable reviewDecision
      additions deletions changedFiles createdAt updatedAt mergedAt closedAt
      headRefName baseRefName headRefOid isCrossRepository
      headRepositoryOwner { login }
      author { login avatarUrl ... on User { id name } }
      autoMergeRequest { mergeMethod }
      viewerCanUpdate viewerDidAuthor viewerCanUpdateBranch
      baseRef { compare(headRef: $headRef) { behindBy } }
      reviewRequests(first: 100) {
        nodes { requestedReviewer { ... on User { login name } ... on Bot { login } ... on Team { slug combinedSlug name avatarUrl } } }
      }
      labels(first: 100) { nodes { name color } }
      commits(last: 1) {
        nodes { commit { statusCheckRollup { contexts(first: 100) {
          @@NODES@@
          pageInfo { hasNextPage }
        } } } }
      }
    }
  }
}"##;
const SEARCH_QUERY_TEMPLATE: &str = r##"query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: @@ROWS@@, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        @@SELECTION@@
      }
    }
  }
}"##;
const LIST_QUERY_TEMPLATE: &str = r##"query($owner: String!, $name: String!, $states: [PullRequestState!], $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: @@ROWS@@, after: $after, states: $states, orderBy: { field: CREATED_AT, direction: DESC }) {
      pageInfo { hasNextPage endCursor }
      nodes { @@SELECTION@@ }
    }
  }
}"##;
const ROW_SELECTION_TEMPLATE: &str = r##"@@STACKS@@
        number
        title
        url
        author { __typename login avatarUrl ... on User { name } }
        headRefName
        baseRefName
        state
        isDraft
        mergeable
        reviewDecision
        latestReviews(first: 20) { nodes { state author { login } } }
        @@STATS@@
        createdAt
        updatedAt
        mergedAt
        repository { nameWithOwner }
        reviewRequests(first: 20) { nodes { requestedReviewer { ... on User { login } ... on Team { slug } } } }
        labels(first: 20) { nodes { name color } }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }"##;
const CHECK_CONTEXTS_QUERY_TEMPLATE: &str = r##"query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      headRefOid
      commits(last: 1) {
        nodes { commit { statusCheckRollup { contexts(first: 100, after: $after) {
          @@NODES@@
          pageInfo { hasNextPage endCursor }
        } } } }
      }
    }
  }
}"##;

// ---------------------------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------------------------

/// GitHub's own ceiling on a connection page, which is what both thread reads ask for.
pub const GRAPHQL_PAGE_SIZE: u32 = 100;

/// The ceiling on `search`, which refuses anything larger with EXCESSIVE_PAGINATION (measured:
/// `first: 101` is an error, `first: 100` is not).
pub const PULL_REQUEST_SEARCH_MAX_ROWS: u32 = GRAPHQL_PAGE_SIZE;

pub const STACK_MEMBERSHIP_SELECTION: &str =
    "stack { number size baseRefName } stackEntry { position }";

/// The fields a linked thread keeps current, for many pull requests in one aliased read. Same
/// shape as the search row where the two overlap: the checks arrive as GitHub's one-word rollup
/// rather than the whole check list `gh pr view` hands back, which is what keeps a batch cheap.
pub const PULL_REQUEST_SUMMARY_SELECTION: &str = concat!(
    "number title url state isDraft mergeable reviewDecision additions deletions changedFiles ",
    "updatedAt mergedAt closedAt headRefName baseRefName ",
    "author { __typename login avatarUrl ... on User { name } } ",
    "latestReviews(first: 20) { nodes { state author { login } } } ",
    "commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }"
);

const WATCH_FINGERPRINT_EDITS: &str = "totalCount nodes { lastEditedAt }";
const WATCH_FINGERPRINT_CHECK_COUNTS: &str =
    "checkRunCountsByState { state count } statusContextCountsByState { state count }";

/// What a pull request watch needs to notice, priced by GitHub at one point for twenty-five pull
/// requests, where the detail and activity reads it gates cost sixteen for one. Counts and the
/// newest edit cover new comments, reviews (a reply in a review thread is a review), threads, and
/// a bot rewriting its summary; check counts by state move whenever a check starts or finishes.
/// Comments come most recently updated first, since an edit moves `updatedAt`, so an edit to an
/// old comment on a long pull request is still in the page. Reviews have no such order: an edit
/// to a review older than the last hundred waits for the watch's periodic full read, as do edits
/// to comments inside review threads, which cost a point per pull request to ask for.
pub fn pull_request_watch_fingerprint_selection() -> String {
    format!(
        "state mergeable headRefOid comments(first: 100, orderBy: {{ field: UPDATED_AT, direction: DESC }}) {{ {edits} }} reviews(last: 100) {{ {edits} }} reviewThreads {{ totalCount }} commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ contexts {{ {counts} }} }} }} }} }}",
        edits = WATCH_FINGERPRINT_EDITS,
        counts = WATCH_FINGERPRINT_CHECK_COUNTS,
    )
}

/// Pull refs let the comparison share the detail read without first resolving a fork branch.
/// `isRequired` is asked for on github.com only: an older Enterprise server may not know it, and
/// an unknown field fails the whole read.
fn check_context_nodes_selection(host: &str) -> String {
    let required = if host.to_lowercase() == "github.com" {
        " isRequired(pullRequestNumber: $number)"
    } else {
        ""
    };
    CHECK_CONTEXT_NODES_TEMPLATE.replace("@@REQUIRED@@", required)
}

pub fn pull_request_core_graphql_query(host: &str) -> String {
    CORE_QUERY_TEMPLATE.replace("@@NODES@@", &check_context_nodes_selection(host))
}

/// Every check context of the head commit, a page at a time, for a rollup past one page.
pub fn pull_request_check_contexts_graphql_query(host: &str) -> String {
    CHECK_CONTEXTS_QUERY_TEMPLATE.replace("@@NODES@@", &check_context_nodes_selection(host))
}

/// `Math.min(Math.max(Math.trunc(rows), 1), PULL_REQUEST_SEARCH_MAX_ROWS)`.
fn page_rows(rows: u32) -> u32 {
    rows.clamp(1, PULL_REQUEST_SEARCH_MAX_ROWS)
}

/// One listing row, the same whether it came from a search or from a repository's own list.
fn pull_request_row_selection(include_stacks: bool, include_stats: bool) -> String {
    ROW_SELECTION_TEMPLATE
        .replace(
            "@@STACKS@@",
            if include_stacks {
                STACK_MEMBERSHIP_SELECTION
            } else {
                ""
            },
        )
        .replace(
            "@@STATS@@",
            if include_stats {
                "additions deletions"
            } else {
                ""
            },
        )
}

/// Every repository of a host in one read, which is what makes a listing one request rather than
/// one process per repository.
///
/// `additions` and `deletions` are deliberately absent: measured over twelve repositories at a
/// hundred rows, this query answers in ~4.0s with them left out and ~7.1s with them in, for two
/// numbers at the end of a row. They are read afterwards, by
/// `build_pull_request_stats_graphql_query`.
///
/// The row count is written into the document rather than sent as a variable because every
/// variable here travels as a string, and it is this module's own number, clamped by the caller,
/// never a reader's.
///
/// `first` on the two inner connections is a bound rather than a page: a pull request with more
/// than twenty labels shows twenty, and one that has asked more than twenty people for a review
/// is already past what a row can say.
pub fn pull_request_search_graphql_query(
    rows: u32,
    include_stacks: bool,
    include_stats: bool,
) -> String {
    SEARCH_QUERY_TEMPLATE
        .replace("@@ROWS@@", &page_rows(rows).to_string())
        .replace(
            "@@SELECTION@@",
            &pull_request_row_selection(include_stacks, include_stats),
        )
}

/// A repository's pull requests without search, newest created first: the order `gh pr list`
/// lists in when it does not search, for a repository GitHub's search index does not cover.
pub fn pull_request_list_graphql_query(rows: u32, include_stacks: bool) -> String {
    LIST_QUERY_TEMPLATE
        .replace("@@ROWS@@", &page_rows(rows).to_string())
        .replace(
            "@@SELECTION@@",
            &pull_request_row_selection(include_stacks, true),
        )
}

/// One pull request's summary, aliased the way the batch is so the same decoder reads it.
pub fn pull_request_summary_graphql_query(include_stacks: bool) -> String {
    let mut selection = String::from(PULL_REQUEST_SUMMARY_SELECTION);
    if include_stacks {
        let _ = write!(selection, " {STACK_MEMBERSHIP_SELECTION}");
    }
    format!(
        "query($owner: String!, $name: String!, $number: Int!) {{\n  s0: repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{ {selection} }} }}\n}}"
    )
}
