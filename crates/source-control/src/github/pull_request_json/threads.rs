//! The conversation reads: review threads, dismissals, one thread's comments, and an activity page.

use indexmap::{IndexMap, IndexSet};
use turin_contracts::base_schemas::PositiveInt;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestComment, PullRequestCommentKind, PullRequestCommit,
    PullRequestDiffSide, PullRequestReviewThread, PullRequestThreadComment,
};

use super::convert::{
    next_cursor_of, non_empty, to_activity_commit_authors, to_actor, to_comments, to_commit,
    to_graphql_commit_actor, to_pull_request_viewer_fields, to_reactions, to_requested_team,
    trimmed,
};
use super::list::decode_json;
use super::raw::{
    RawActivity, RawActor, RawComment, RawDismissal, RawReviewDismissalsEnvelope,
    RawReviewThreadComments, RawReviewThreadsEnvelope,
};
use super::types::{
    DecodeResult, GitHubDiffCounts, GitHubPullRequestActivityPage, GitHubReviewDismissalsPage,
    GitHubReviewThreadCommentsPage, GitHubReviewThreadEntry, GitHubReviewThreadPage,
};

/// An actor plus, for a requested team, the combined slug, name and avatar it is shown by.
type ReviewerSource<'a> = (
    Option<&'a RawActor>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

/// The threads as one flat conversation, which is what the timeline reads. Every comment of
/// every thread, resolved or not: a resolved conversation is still what was said, and a reply is
/// as much of it as the remark it answers.
pub fn review_thread_conversation(threads: &[PullRequestReviewThread]) -> Vec<PullRequestComment> {
    threads
        .iter()
        .flat_map(|thread| {
            thread.comments.iter().map(|comment| PullRequestComment {
                id: comment.id.clone(),
                kind: PullRequestCommentKind::ReviewComment,
                author: comment.author.clone(),
                body: comment.body.clone(),
                created_at: comment.created_at.clone(),
                edited_at: Some(comment.edited_at.clone().flatten()),
                url: comment.url.clone(),
                path: Some(thread.path.as_str().to_owned()),
                review_state: None,
                reactions: Some(comment.reactions.clone().unwrap_or_default()),
                can_edit: None,
            })
        })
        .collect()
}

fn to_dismissal_entries(nodes: &[RawDismissal]) -> IndexMap<String, String> {
    let mut entries = IndexMap::new();
    for node in nodes {
        let review_id = trimmed(node.review.as_ref().and_then(|review| review.id.as_deref()));
        let message = trimmed(node.dismissal_message.as_deref());
        if let (Some(review_id), Some(message)) = (review_id, message) {
            entries.insert(review_id, message);
        }
    }
    entries
}

/// One further page of dismissal events, in the shape the thread read's own page carries.
pub fn decode_review_dismissals_json(raw: &str) -> DecodeResult<GitHubReviewDismissalsPage> {
    let decoded: RawReviewDismissalsEnvelope = decode_json(raw)?;
    let items = &decoded.data.repository.pull_request.timeline_items;
    Ok(GitHubReviewDismissalsPage {
        dismissals_by_review_id: to_dismissal_entries(&items.nodes),
        next_cursor: next_cursor_of(items.page_info.as_ref()),
    })
}

fn to_thread_comment(
    comment: &RawComment,
    viewer: Option<&str>,
) -> Option<PullRequestThreadComment> {
    Some(PullRequestThreadComment {
        id: non_empty(&comment.id)?,
        author: to_actor(comment.author.as_ref()),
        body: comment.body.clone().unwrap_or_default(),
        created_at: comment.created_at.clone(),
        edited_at: Some(comment.last_edited_at.clone()),
        url: trimmed(comment.url.as_deref()),
        reactions: Some(to_reactions(&comment.reaction_groups, viewer)),
        can_edit: None,
    })
}

/// One page of review threads. Following the cursors it hands back is the caller's job.
pub fn decode_review_threads_json(raw: &str) -> DecodeResult<GitHubReviewThreadPage> {
    let decoded: RawReviewThreadsEnvelope = decode_json(raw)?;
    let viewer = trimmed(
        decoded
            .data
            .viewer
            .as_ref()
            .and_then(|viewer| viewer.login.as_deref()),
    );
    let viewer = viewer.as_deref();
    let pull_request = &decoded.data.repository.pull_request;
    let threads = &pull_request.review_threads;
    let entries: Vec<GitHubReviewThreadEntry> = threads
        .nodes
        .iter()
        .filter_map(|thread| {
            let path = trimmed(thread.path.as_deref());
            let id = trimmed(thread.id.as_deref());
            let (Some(path), Some(id)) = (path, id) else {
                return None;
            };
            if thread.comments.nodes.is_empty() {
                return None;
            }
            Some(GitHubReviewThreadEntry {
                thread: PullRequestReviewThread {
                    id: non_empty(&id)?,
                    path: non_empty(&path)?,
                    // Null once the thread's line has left the diff, which is exactly when GitHub
                    // reports it outdated. Such a thread is listed rather than pinned to a line
                    // it no longer has.
                    line: thread
                        .line
                        .and_then(|line| u64::try_from(line.0).ok())
                        .and_then(PositiveInt::new),
                    side: if thread
                        .diff_side
                        .as_deref()
                        .is_some_and(|side| side.to_uppercase() == "LEFT")
                    {
                        PullRequestDiffSide::Left
                    } else {
                        PullRequestDiffSide::Right
                    },
                    is_resolved: thread.is_resolved == Some(true),
                    is_outdated: thread.is_outdated == Some(true),
                    comments: thread
                        .comments
                        .nodes
                        .iter()
                        .filter_map(|comment| to_thread_comment(comment, viewer))
                        .collect(),
                    comment_count: None,
                    next_comments_cursor: None,
                    can_resolve: None,
                },
                comment_count: thread
                    .comments
                    .total_count
                    .map_or(thread.comments.nodes.len() as u64, |total| {
                        total.non_negative()
                    }),
                next_comment_cursor: next_cursor_of(thread.comments.page_info.as_ref()),
            })
        })
        .collect();

    let comment_nodes = pull_request
        .comments
        .as_ref()
        .map(|comments| comments.nodes.as_slice())
        .unwrap_or_default();
    let review_nodes = pull_request
        .reviews
        .as_ref()
        .map(|reviews| reviews.nodes.as_slice())
        .unwrap_or_default();
    let requested_nodes = pull_request
        .review_requests
        .as_ref()
        .map(|requests| requests.nodes.as_slice())
        .unwrap_or_default();
    let latest_nodes = pull_request
        .latest_reviews
        .as_ref()
        .map(|reviews| reviews.nodes.as_slice())
        .unwrap_or_default();

    let mut avatars_by_login = IndexMap::new();
    let mut bot_logins = IndexSet::new();
    let named: Vec<Option<&RawActor>> = std::iter::once(pull_request.author.as_ref())
        .chain(comment_nodes.iter().map(|node| node.author.as_ref()))
        .chain(review_nodes.iter().map(|node| node.author.as_ref()))
        .chain(requested_nodes.iter().map(|node| {
            node.requested_reviewer
                .as_ref()
                .map(|reviewer| &reviewer.actor)
        }))
        .chain(latest_nodes.iter().map(|node| node.author.as_ref()))
        .chain(threads.nodes.iter().flat_map(|thread| {
            thread
                .comments
                .nodes
                .iter()
                .map(|comment| comment.author.as_ref())
        }))
        .collect();
    for raw_actor in named {
        let login = trimmed(raw_actor.and_then(|actor| actor.login.as_deref()));
        let avatar_url = trimmed(raw_actor.and_then(|actor| actor.avatar_url.as_deref()));
        if let (Some(login), Some(avatar_url)) = (&login, avatar_url) {
            avatars_by_login.insert(login.clone(), avatar_url);
        }
        if let Some(login) = login
            && to_actor(raw_actor).is_some_and(|actor| actor.is_bot == Some(true))
        {
            bot_logins.insert(login);
        }
    }

    // A requested reviewer may be a team, which goes by its combined slug; a latest review's
    // author never is.
    let reviewer_sources: Vec<ReviewerSource<'_>> = requested_nodes
        .iter()
        .map(|node| {
            let reviewer = node.requested_reviewer.as_ref();
            (
                reviewer.map(|reviewer| &reviewer.actor),
                reviewer.and_then(|reviewer| reviewer.combined_slug.as_deref()),
                reviewer.and_then(|reviewer| reviewer.actor.name.as_deref()),
                reviewer.and_then(|reviewer| reviewer.actor.avatar_url.as_deref()),
            )
        })
        .chain(
            latest_nodes
                .iter()
                .map(|node| (node.author.as_ref(), None, None, None)),
        )
        .collect();
    let mut reviewers: IndexMap<String, PullRequestActor> = IndexMap::new();
    for (actor, combined_slug, name, avatar_url) in reviewer_sources {
        let actor = to_actor(actor).or_else(|| to_requested_team(combined_slug, name, avatar_url));
        // Keyed by login, so someone who was asked and then answered appears once.
        if let Some(actor) = actor
            && !reviewers.contains_key(actor.login.as_str())
        {
            reviewers.insert(actor.login.as_str().to_owned(), actor);
        }
    }

    let mut commit_stats = IndexMap::new();
    let mut commits: Vec<PullRequestCommit> = Vec::new();
    for node in pull_request
        .commits
        .as_ref()
        .map(|commits| commits.nodes.as_slice())
        .unwrap_or_default()
    {
        let commit = &node.commit;
        let Some(oid) = trimmed(Some(&commit.oid)) else {
            continue;
        };
        // GitHub measures a merge commit against its first parent, so merging the base into the
        // head reports every upstream change as if it belonged to the pull request. There is no
        // useful per-commit stat to show for that integration commit without another comparison
        // request.
        let parents = commit
            .parents
            .as_ref()
            .and_then(|parents| parents.total_count)
            .map_or(1, |total| total.0);
        if let (true, Some(additions), Some(deletions)) =
            (parents <= 1, commit.additions, commit.deletions)
        {
            commit_stats.insert(
                oid.clone(),
                GitHubDiffCounts {
                    additions: additions.non_negative(),
                    deletions: deletions.non_negative(),
                },
            );
        }
        let Some(committed_date) = trimmed(commit.committed_date.as_deref()) else {
            continue;
        };
        let authors = commit
            .authors
            .as_ref()
            .map(|authors| authors.nodes.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|author| {
                to_graphql_commit_actor(
                    author.user.as_ref().and_then(|user| user.login.as_deref()),
                    author.name.as_deref(),
                    author.avatar_url.as_deref(),
                )
            })
            .collect();
        if let Some(commit) = to_commit(
            &oid,
            commit.message_headline.as_deref(),
            &committed_date,
            authors,
        ) {
            commits.push(commit);
        }
    }

    let mut reactions_by_id = IndexMap::new();
    let mut edited_at_by_id = IndexMap::new();
    for node in comment_nodes.iter().chain(review_nodes) {
        let Some(id) = trimmed(node.id.as_deref()) else {
            continue;
        };
        if let Some(edited_at) = trimmed(node.last_edited_at.as_deref()) {
            edited_at_by_id.insert(id.clone(), edited_at);
        }
        let reactions = to_reactions(&node.reaction_groups, viewer);
        if !reactions.is_empty() {
            reactions_by_id.insert(id, reactions);
        }
    }

    Ok(GitHubReviewThreadPage {
        threads: entries,
        next_cursor: next_cursor_of(threads.page_info.as_ref()),
        reactions: to_reactions(&pull_request.reaction_groups, viewer),
        reactions_by_id,
        edited_at_by_id,
        reviewers: reviewers.into_values().collect(),
        avatars_by_login,
        bot_logins,
        commit_stats,
        commits,
        viewer: to_pull_request_viewer_fields(Some(&pull_request.viewer)),
        dismissals_by_review_id: to_dismissal_entries(
            pull_request
                .review_dismissals
                .as_ref()
                .map(|dismissals| dismissals.nodes.as_slice())
                .unwrap_or_default(),
        ),
        next_dismissal_cursor: next_cursor_of(
            pull_request
                .review_dismissals
                .as_ref()
                .and_then(|dismissals| dismissals.page_info.as_ref()),
        ),
    })
}

/// The rest of one thread's comments, in the shape the first page already delivered them.
pub fn decode_review_thread_comments_json(
    raw: &str,
) -> DecodeResult<GitHubReviewThreadCommentsPage> {
    let decoded: RawReviewThreadComments = decode_json(raw)?;
    let viewer = trimmed(
        decoded
            .data
            .viewer
            .as_ref()
            .and_then(|viewer| viewer.login.as_deref()),
    );
    let viewer = viewer.as_deref();
    let comments = decoded
        .data
        .node
        .as_ref()
        .and_then(|node| node.comments.as_ref());
    let expected = decoded
        .data
        .repository
        .as_ref()
        .and_then(|repository| repository.pull_request.as_ref())
        .map(|pull_request| pull_request.id.as_str());
    let actual = decoded
        .data
        .node
        .as_ref()
        .and_then(|node| node.pull_request.as_ref())
        .map(|pull_request| pull_request.id.as_str());
    Ok(GitHubReviewThreadCommentsPage {
        belongs_to_pull_request: expected.is_some() && expected == actual,
        comments: comments
            .map(|comments| comments.nodes.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|comment| to_thread_comment(comment, viewer))
            .collect(),
        next_cursor: next_cursor_of(comments.and_then(|comments| comments.page_info.as_ref())),
    })
}

/// One page of `PULL_REQUEST_ACTIVITY_GRAPHQL_QUERY`.
pub fn decode_pull_request_activity_json(raw: &str) -> DecodeResult<GitHubPullRequestActivityPage> {
    let decoded: RawActivity = decode_json(raw)?;
    let pr = &decoded.data.repository.pull_request;
    let comments = pr
        .comments
        .as_ref()
        .map(|comments| comments.nodes.as_slice())
        .unwrap_or_default();
    let reviews = pr
        .reviews
        .as_ref()
        .map(|reviews| reviews.nodes.as_slice())
        .unwrap_or_default();
    Ok(GitHubPullRequestActivityPage {
        author: pr.author.as_ref().map(|author| to_actor(author.as_ref())),
        commits: pr.commits.as_ref().map(|commits| {
            commits
                .as_ref()
                .map(|commits| commits.nodes.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|node| {
                    let commit = &node.commit;
                    to_commit(
                        &commit.oid,
                        commit.message_headline.as_deref(),
                        &commit.committed_date,
                        to_activity_commit_authors(
                            commit
                                .authors
                                .as_ref()
                                .map(|authors| authors.nodes.as_slice())
                                .unwrap_or_default(),
                        ),
                    )
                })
                .collect()
        }),
        remarks: to_comments(comments, reviews),
        next_comments_cursor: next_cursor_of(
            pr.comments
                .as_ref()
                .and_then(|comments| comments.page_info.as_ref()),
        ),
        next_reviews_cursor: next_cursor_of(
            pr.reviews
                .as_ref()
                .and_then(|reviews| reviews.page_info.as_ref()),
        ),
    })
}
