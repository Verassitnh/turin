//! Port of upstream/apps/web/src/components/pullRequest/PullRequestEditButton.tsx.
//!
//! The pencil that reveals on hovering its row is shared by every remark on the Timeline, Code
//! and Summary tabs, so it lives with the comment pieces in `comment_body`; this module is the
//! upstream file's name for it.

#[allow(unused_imports)]
pub use super::comment_body::EditButton as PullRequestEditButton;
