//! Port of upstream/packages/source-control-github/src/server/gitHubPullRequestJson.ts
//!
//! Every GraphQL / REST payload decoder and normalizer: raw GitHub JSON into the neutral types
//! from `source_control_core::pull_request_provider` and `turin_contracts::pull_request`.
//!
//! Layout (the upstream file is 3500 lines):
//! - [`de`]: serde helpers that reproduce Effect Schema's struct decoding (`optional`, `NullOr`,
//!   `Int`).
//! - [`raw`]: one struct per upstream `Raw*Schema`.
//! - [`types`]: the normalized shapes handed to the rest of the driver.
//! - [`queries`]: GraphQL document text and builders.
//! - [`convert`]: the `to*` normalizers.
//! - [`list`] / [`threads`] / [`misc`]: the decoders, by concern.
//! - the batch documents are built with `github::graphql::aliased_graphql_document`.
//!
//! Decoding semantics. Upstream decodes with Effect Schema: a struct ignores keys it does not
//! name, `optional(X)` rejects an explicit `null`, a required `NullOr(X)` key must be present,
//! and the first mismatch fails the whole payload (`DecodeFailure`). Row-by-row reads
//! (`decode_pull_request_list_json`, `..._search_json`, the alias-keyed batches, the file
//! entries) decode each row on its own and skip a row that does not decode. Those are the same
//! places here.
//!
//! Where a contract field is a `TrimmedNonEmptyString` and upstream passes a raw id straight
//! through (comment ids, thread ids, commit oids), a blank value cannot be represented, so that
//! one element is skipped. GitHub never sends one.

pub mod convert;
pub mod de;
pub mod list;
pub mod misc;
pub mod queries;
pub mod raw;
pub mod threads;
pub mod types;

#[cfg(test)]
mod tests;

pub use crate::github::graphql::GraphQlDocument;
pub use convert::{github_reaction_content, to_can_write};
pub use list::{
    ChangeRequestSelector, build_pull_request_stack_memberships_graphql_query,
    build_pull_request_stats_graphql_query, build_pull_request_summaries_graphql_query,
    build_pull_request_watch_fingerprints_graphql_query, decode_pull_request_core_json,
    decode_pull_request_detail_json, decode_pull_request_list_json,
    decode_pull_request_search_json, decode_pull_request_stack_memberships_json,
    decode_pull_request_stats_json, decode_pull_request_summaries_json,
    decode_pull_request_watch_fingerprints_json,
};
pub use misc::{
    LabelRequest, ReviewEvent, ReviewSide, ReviewSubmission, ReviewSubmissionComment,
    ReviewerRequest, build_label_request, build_review_submission, build_reviewer_request,
    build_set_files_viewed_graphql_mutation, decode_actor_avatars_json,
    decode_commit_files_json, decode_label_candidates_json,
    decode_pull_request_check_contexts_json, decode_pull_request_files_json,
    decode_pull_request_files_viewed_json, decode_pull_request_heads_json,
    decode_pull_request_node_id_json, decode_pull_request_preview_json,
    decode_pull_request_stacks_json, decode_reaction_subject_scope_json,
    decode_reviewer_candidates_json, decode_viewer_permissions_json, decode_workflow_runs_json,
    pull_request_checks_from_contexts, quote_git_patch_path,
};
pub use queries::*;
pub use threads::{
    decode_pull_request_activity_json, decode_review_dismissals_json,
    decode_review_thread_comments_json, decode_review_threads_json, review_thread_conversation,
};
pub use types::*;
