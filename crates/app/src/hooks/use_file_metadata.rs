//! Port of upstream/apps/web/src/hooks/useFileMetadata.ts: which path a file
//! chip asks the metadata cache about.
//!
//! The hook is React glue over `useEnvironmentQuery`; what is left is the
//! request it builds. The path is stripped of its `:line:column` suffix,
//! resolved against the working directory (a `~/` path passes through for the
//! filesystem to expand), and must end up absolute or home-relative and at
//! most 512 characters. `AuthFilesystemReadScope` is auth and not ported (one
//! process, no sessions), and the React context
//! (`FileMetadataThreadProvider`) becomes the `thread_ref` argument.
//!
//! Port of upstream/apps/web/src/hooks/FileMetadataThreadProvider.tsx (context becomes the `thread_ref` argument).
// The file chips that read this arrive with the chat views.
#![allow(dead_code)]

use turin_shared::file_links::{resolve_path_link_target, split_file_path_position};
use turin_shared::js_string::utf16_len;
use turin_shared::path::{is_absolute_path, normalize_project_path_for_comparison};

/// `targetPath.length <= 512`.
pub const FILE_METADATA_MAX_PATH_LENGTH: usize = 512;

/// The thread a chat view's file chips belong to (`ScopedThreadRef`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadataThreadRef<'a> {
    pub environment_id: &'a str,
    pub thread_id: &'a str,
}

/// `projectEnvironment.fileMetadata({ environmentId, input })`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadataRequest {
    pub path: String,
    /// Set when the chip sits in a thread of the same environment, so the
    /// cache retains the path for that thread.
    pub thread_id: Option<String>,
}

/// The normalized path to look up, or `None` when nothing should be asked.
pub fn resolve_file_metadata_target(path: &str, cwd: Option<&str>) -> Option<String> {
    let host_path = split_file_path_position(path, "").path;
    let resolved = match cwd.filter(|cwd| !cwd.is_empty()) {
        Some(cwd) if !host_path.starts_with("~/") => resolve_path_link_target(&host_path, cwd),
        _ => host_path,
    };
    let target_path = normalize_project_path_for_comparison(&resolved);
    let absolute = is_absolute_path(&target_path) || target_path.starts_with("~/");
    (absolute && utf16_len(&target_path) <= FILE_METADATA_MAX_PATH_LENGTH).then_some(target_path)
}

/// `useFileMetadata`'s query input. `None` is upstream's `null` query.
pub fn file_metadata_request(
    environment_id: Option<&str>,
    path: &str,
    cwd: Option<&str>,
    thread_ref: Option<&FileMetadataThreadRef<'_>>,
) -> Option<FileMetadataRequest> {
    let environment_id = environment_id.filter(|id| !id.is_empty())?;
    let target_path = resolve_file_metadata_target(path, cwd)?;
    Some(FileMetadataRequest {
        path: target_path,
        thread_id: thread_ref
            .filter(|thread_ref| thread_ref.environment_id == environment_id)
            .map(|thread_ref| thread_ref.thread_id.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_relative_path_against_the_working_directory() {
        assert_eq!(
            resolve_file_metadata_target("src/main.ts", Some("/repo")).as_deref(),
            Some("/repo/src/main.ts")
        );
    }

    #[test]
    fn drops_the_line_and_column_suffix() {
        assert_eq!(
            resolve_file_metadata_target("src/main.ts:12:3", Some("/repo")).as_deref(),
            Some("/repo/src/main.ts")
        );
        assert_eq!(
            resolve_file_metadata_target("/abs/file.rs:4", None).as_deref(),
            Some("/abs/file.rs")
        );
    }

    #[test]
    fn a_home_path_passes_through_unresolved() {
        assert_eq!(
            resolve_file_metadata_target("~/notes.md", Some("/repo")).as_deref(),
            Some("~/notes.md")
        );
        assert_eq!(
            resolve_file_metadata_target("~/notes.md", None).as_deref(),
            Some("~/notes.md")
        );
    }

    #[test]
    fn a_relative_path_without_a_working_directory_is_not_looked_up() {
        assert_eq!(resolve_file_metadata_target("src/main.ts", None), None);
        assert_eq!(resolve_file_metadata_target("src/main.ts", Some("")), None);
    }

    #[test]
    fn trailing_separators_are_normalized_for_comparison() {
        assert_eq!(
            resolve_file_metadata_target("/repo/dir/", None).as_deref(),
            Some("/repo/dir")
        );
    }

    #[test]
    fn a_path_over_512_characters_is_not_looked_up() {
        let at_limit = format!("/{}", "a".repeat(FILE_METADATA_MAX_PATH_LENGTH - 1));
        let over_limit = format!("/{}", "a".repeat(FILE_METADATA_MAX_PATH_LENGTH));
        assert_eq!(
            resolve_file_metadata_target(&at_limit, None).as_deref(),
            Some(at_limit.as_str())
        );
        assert_eq!(resolve_file_metadata_target(&over_limit, None), None);
    }

    #[test]
    fn the_request_needs_an_environment() {
        assert_eq!(file_metadata_request(None, "/a/b", None, None), None);
        assert_eq!(file_metadata_request(Some(""), "/a/b", None, None), None);
    }

    #[test]
    fn the_request_is_scoped_to_a_thread_of_the_same_environment_only() {
        let same = FileMetadataThreadRef {
            environment_id: "local",
            thread_id: "thread-1",
        };
        let other = FileMetadataThreadRef {
            environment_id: "elsewhere",
            thread_id: "thread-2",
        };
        assert_eq!(
            file_metadata_request(Some("local"), "/a/b", None, Some(&same)),
            Some(FileMetadataRequest {
                path: "/a/b".to_owned(),
                thread_id: Some("thread-1".to_owned()),
            })
        );
        assert_eq!(
            file_metadata_request(Some("local"), "/a/b", None, Some(&other)),
            Some(FileMetadataRequest {
                path: "/a/b".to_owned(),
                thread_id: None,
            })
        );
    }
}
