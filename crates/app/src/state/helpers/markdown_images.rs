//! Port of upstream/packages/client-runtime/src/markdownImages.ts

use std::sync::LazyLock;

use regex::Regex;

use super::shared_support::{
    normalize_markdown_link_destination, parse_file_url_href, safe_decode_uri_component,
    split_markdown_link_search_and_hash,
};
use crate::state::support::path::{is_windows_absolute_path, strip_slash_prefixed_windows_drive};

// `/i` without `u`: ASCII-only case folding.
static DIRECT_IMAGE_SOURCE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)^(?:https?:|data:|blob:|//)").expect("valid regex"));
static FILE_SCHEME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)^file:").expect("valid regex"));
static URI_SCHEME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("valid regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownImageSource {
    Direct { uri: String },
    WorkspaceFile { path: String },
    Blocked,
}

pub fn markdown_image_source_fragment(source: &str) -> String {
    split_markdown_link_search_and_hash(normalize_markdown_link_destination(source)).hash
}

fn join_workspace_path(workspace_root: &str, relative_path: &str) -> String {
    let separator = if is_windows_absolute_path(workspace_root) {
        '\\'
    } else {
        '/'
    };
    let root = workspace_root.trim_end_matches(['\\', '/']);
    let path: String = relative_path
        .chars()
        .map(|c| if c == '\\' || c == '/' { separator } else { c })
        .collect();
    let path = path.trim_start_matches(separator);
    format!("{root}{separator}{path}")
}

/// Classifies a markdown image or video source by where its bytes must be loaded from.
/// Filesystem paths belong to the environment host and must never reach a
/// browser or native image component without first becoming a signed asset URL.
pub fn classify_markdown_image_source(
    value: Option<&str>,
    workspace_root: Option<&str>,
) -> MarkdownImageSource {
    let Some(value) = value else {
        return MarkdownImageSource::Blocked;
    };

    let source = normalize_markdown_link_destination(value);
    if source.is_empty() || source.starts_with('#') || source.starts_with('?') {
        return MarkdownImageSource::Blocked;
    }
    if DIRECT_IMAGE_SOURCE_PATTERN.is_match(source) {
        return MarkdownImageSource::Direct {
            uri: source.to_owned(),
        };
    }

    if FILE_SCHEME_PATTERN.is_match(source) {
        return match parse_file_url_href(source) {
            None => MarkdownImageSource::Blocked,
            Some(target) => MarkdownImageSource::WorkspaceFile {
                path: strip_slash_prefixed_windows_drive(&safe_decode_uri_component(&target.path))
                    .to_owned(),
            },
        };
    }

    let decoded = safe_decode_uri_component(&split_markdown_link_search_and_hash(source).path);
    let path = strip_slash_prefixed_windows_drive(&decoded);
    if path.is_empty() {
        return MarkdownImageSource::Blocked;
    }
    if path.starts_with('/') || is_windows_absolute_path(path) {
        return MarkdownImageSource::WorkspaceFile {
            path: path.to_owned(),
        };
    }
    if URI_SCHEME_PATTERN.is_match(path) || path.starts_with("~/") || path.starts_with("~\\") {
        return MarkdownImageSource::Blocked;
    }
    let Some(workspace_root) = workspace_root.filter(|root| !root.is_empty()) else {
        return MarkdownImageSource::Blocked;
    };

    MarkdownImageSource::WorkspaceFile {
        path: join_workspace_path(workspace_root, path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_direct_sources_directly_loadable() {
        for uri in [
            "https://example.com/image.png",
            "HTTP://example.com/image.png",
            "data:image/png;base64,AAAA",
            "blob:https://app.t3.codes/image-id",
            "//cdn.example.com/image.png",
        ] {
            assert_eq!(
                classify_markdown_image_source(Some(uri), Some("/workspace/project")),
                MarkdownImageSource::Direct {
                    uri: uri.to_owned()
                },
                "{uri}"
            );
        }
    }

    #[test]
    fn maps_sources_to_a_workspace_file() {
        let cases: [(&str, Option<&str>, &str); 12] = [
            (
                "images/result.png",
                Some("/workspace/project"),
                "/workspace/project/images/result.png",
            ),
            (
                "./images/result.png",
                Some("/workspace/project"),
                "/workspace/project/./images/result.png",
            ),
            (
                "images/result.png",
                Some("C:\\Users\\dara\\project"),
                "C:\\Users\\dara\\project\\images\\result.png",
            ),
            (
                "images\\result.png",
                Some("C:\\Users\\dara\\project"),
                "C:\\Users\\dara\\project\\images\\result.png",
            ),
            (
                "/workspace/project/image.png",
                None,
                "/workspace/project/image.png",
            ),
            (
                "/C:/Users/dara/project/image.png",
                None,
                "C:/Users/dara/project/image.png",
            ),
            (
                "C:/Users/dara/project/image.png",
                None,
                "C:/Users/dara/project/image.png",
            ),
            (
                "\\\\server\\share\\image.png",
                None,
                "\\\\server\\share\\image.png",
            ),
            (
                "file:///workspace/project/image%20one.png",
                None,
                "/workspace/project/image one.png",
            ),
            (
                "file:///C:/Users/dara/project/image.png",
                None,
                "C:/Users/dara/project/image.png",
            ),
            (
                "file://localhost/C:/Users/dara/project/image.png",
                None,
                "C:/Users/dara/project/image.png",
            ),
            (
                "file://server/share/image.png",
                None,
                "\\\\server\\share\\image.png",
            ),
        ];
        for (source, workspace_root, path) in cases {
            assert_eq!(
                classify_markdown_image_source(Some(source), workspace_root),
                MarkdownImageSource::WorkspaceFile {
                    path: path.to_owned()
                },
                "{source}"
            );
        }
    }

    #[test]
    fn blocks_unsupported_or_unresolved_sources() {
        for source in [
            None,
            Some(""),
            Some("#image"),
            Some("?image=1"),
            Some("image.png"),
            Some("~/image.png"),
            Some("javascript:alert(1)"),
            Some("ftp://example.com/image.png"),
            Some("content://media/image/1"),
            Some("custom:image.png"),
            Some("file://%"),
        ] {
            assert_eq!(
                classify_markdown_image_source(source, None),
                MarkdownImageSource::Blocked,
                "{source:?}"
            );
        }
    }

    #[test]
    fn extracts_the_source_fragment() {
        assert_eq!(
            markdown_image_source_fragment("<icons.svg?version=2#logo>"),
            "#logo"
        );
        assert_eq!(markdown_image_source_fragment("icons.svg?version=2"), "");
    }
}
