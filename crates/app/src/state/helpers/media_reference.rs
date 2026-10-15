//! Port of upstream/packages/client-runtime/src/mediaReference.ts

use std::sync::LazyLock;

use regex::Regex;
use url::Url;

use super::shared_support::safe_decode_uri_component;
use crate::state::support::path::is_windows_absolute_path;

static WINDOWS_ROOT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // `/^(?:[a-z]:\/|\/\/[^/]+\/[^/]+(?:\/|$))/i`; the only letters are the drive's.
    Regex::new(r"^(?:[A-Za-z]:/|//[^/]+/[^/]+(?:/|$))").expect("valid regex")
});
static WEB_URL_PREFIX_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)^(?:https?://|//)").expect("valid regex"));

/// A file reference: the authored path, plus its workspace-relative form when inside one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFileReference {
    pub path: String,
    pub relative_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaUrlReference {
    pub url: String,
}

/// The authored media location, never the temporary URL used to load its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaReference {
    File(MediaFileReference),
    Url(MediaUrlReference),
}

struct AbsolutePathParts {
    root: String,
    segments: Vec<String>,
    windows: bool,
}

fn absolute_path_parts(path: &str) -> Option<AbsolutePathParts> {
    let windows = is_windows_absolute_path(path) || path.starts_with("//");
    let normalized = if windows {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let prefix: &str = if windows {
        WINDOWS_ROOT_PATTERN.find(&normalized)?.as_str()
    } else if normalized.starts_with('/') {
        "/"
    } else {
        return None;
    };

    let mut segments: Vec<String> = Vec::new();
    for segment in normalized[prefix.len()..].split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            segments.pop();
        } else {
            segments.push(segment.to_owned());
        }
    }
    let root = prefix.strip_suffix('/').unwrap_or(prefix);
    Some(AbsolutePathParts {
        root: if windows {
            root.to_lowercase()
        } else {
            root.to_owned()
        },
        segments,
        windows,
    })
}

/// Compares paths lexically for the copy menu; it does not resolve filesystem symlinks.
pub fn media_file_reference(path: &str, workspace_root: Option<&str>) -> MediaFileReference {
    let target = absolute_path_parts(path);
    let workspace = workspace_root
        .filter(|root| !root.is_empty())
        .and_then(absolute_path_parts);
    if let (Some(target), Some(workspace)) = (&target, &workspace) {
        let inside = target.windows == workspace.windows
            && target.root == workspace.root
            && target.segments.len() > workspace.segments.len()
            && workspace
                .segments
                .iter()
                .enumerate()
                .all(|(index, segment)| {
                    if workspace.windows {
                        segment.to_lowercase() == target.segments[index].to_lowercase()
                    } else {
                        *segment == target.segments[index]
                    }
                });
        if inside {
            return MediaFileReference {
                path: path.to_owned(),
                relative_path: Some(target.segments[workspace.segments.len()..].join("/")),
            };
        }
    }
    MediaFileReference {
        path: path.to_owned(),
        relative_path: None,
    }
}

/// `new URL(url.startsWith("//") ? `https:${url}` : url)`
fn parse_media_url(url: &str) -> Option<Url> {
    if url.starts_with("//") {
        Url::parse(&format!("https:{url}")).ok()
    } else {
        Url::parse(url).ok()
    }
}

/// Pass the authored source, not a generated URL used by the media player.
pub fn media_url_reference(url: &str) -> Option<MediaUrlReference> {
    if !WEB_URL_PREFIX_PATTERN.is_match(url) {
        return None;
    }
    let parsed = parse_media_url(url)?;
    (parsed.scheme() == "http" || parsed.scheme() == "https").then(|| MediaUrlReference {
        url: url.to_owned(),
    })
}

/// Local paths are already decoded; URL filename escapes are decoded exactly once.
pub fn media_reference_file_name(reference: &MediaReference) -> Option<String> {
    match reference {
        MediaReference::File(file) => {
            let windows = is_windows_absolute_path(&file.path) || file.path.starts_with("//");
            let last = if windows {
                file.path.rsplit(['\\', '/']).next()
            } else {
                file.path.rsplit('/').next()
            };
            last.filter(|name| !name.is_empty()).map(str::to_owned)
        }
        MediaReference::Url(reference) => {
            let parsed = parse_media_url(&reference.url)?;
            let basename = parsed
                .path()
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())?;
            Some(safe_decode_uri_component(basename))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_file_reference_preserves_the_path_and_only_labels_paths_inside_the_root_as_relative() {
        let cases: [(&str, &str, Option<&str>); 12] = [
            (
                "/work/project/./media/../clip.mp4",
                "/work/project/",
                Some("clip.mp4"),
            ),
            ("/work/project/../outside.mp4", "/work/project", None),
            ("/work/project-other/clip.mp4", "/work/project", None),
            ("/work/Project/clip.mp4", "/work/project", None),
            ("/work/project/a\\b.mp4", "/work/project", Some("a\\b.mp4")),
            ("/work/clip.mp4", "/work/other/..", Some("clip.mp4")),
            ("/clip.mp4", "/", Some("clip.mp4")),
            (
                "C:\\WORK\\project\\media\\..\\Clip.mp4",
                "c:/work/project/",
                Some("Clip.mp4"),
            ),
            ("D:\\work\\clip.mp4", "C:\\work", None),
            (
                "\\\\Server\\Share\\Project\\Clip.mp4",
                "//server/share/project",
                Some("Clip.mp4"),
            ),
            ("\\\\server\\other\\clip.mp4", "\\\\server\\share", None),
            ("../clip.mp4", "/work/project", None),
        ];
        for (path, root, relative_path) in cases {
            assert_eq!(
                media_file_reference(path, Some(root)),
                MediaFileReference {
                    path: path.to_owned(),
                    relative_path: relative_path.map(str::to_owned),
                },
                "{path} in {root}"
            );
        }
    }

    #[test]
    fn media_reference_file_name_preserves_filename_semantics() {
        let file = |path: &str| {
            MediaReference::File(MediaFileReference {
                path: path.to_owned(),
                relative_path: None,
            })
        };
        let url = |url: &str| {
            MediaReference::Url(MediaUrlReference {
                url: url.to_owned(),
            })
        };
        let cases = [
            (file("/tmp/take\\one%20.mp4"), "take\\one%20.mp4"),
            (file("C:\\clips/take\\one.mp4"), "one.mp4"),
            (file("\\\\server\\share\\one.mp4"), "one.mp4"),
            (
                url("//cdn.example/clip%20one%2Emp4?sig=a+b#t=2"),
                "clip one.mp4",
            ),
            (url("https://cdn.example/clip%2520.mp4"), "clip%20.mp4"),
            (
                url("https://cdn.example/clip%20%oops.mp4"),
                "clip%20%oops.mp4",
            ),
        ];
        for (reference, name) in cases {
            assert_eq!(
                media_reference_file_name(&reference).as_deref(),
                Some(name),
                "{reference:?}"
            );
        }
    }
}
