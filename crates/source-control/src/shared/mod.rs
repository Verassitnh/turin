//! Helpers upstream keeps in `packages/shared/src` that this crate needs before `turin-shared`
//! (T05b) lands them. Each module is `//! Port of upstream/packages/shared/src/<file>`; when T05b
//! provides the same function, delete the copy here and re-export (PORTING.md Deviations).

pub mod change_request_url;
pub mod composer_pull_request_matches;
pub mod git;
pub mod github_markdown_links;
pub mod github_media;
pub mod host_process;
pub mod js;
pub mod mime_types;
pub mod source_control;
pub mod thread_pull_requests;
