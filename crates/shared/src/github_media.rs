//! Port of upstream/packages/shared/src/githubMedia.ts

use crate::js_string::decode_uri_component;
use url::Url;

const RAW_HOST: &str = "raw.githubusercontent.com";
/// Git LFS pointers resolve here, which is where an LFS-tracked screenshot's bytes live.
const LFS_HOST: &str = "media.githubusercontent.com";

fn attachment_path(pathname: &str) -> bool {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^/user-attachments/assets/[\w-]+$").expect("attachment")
    });
    PATTERN.is_match(pathname)
}

fn legacy_attachment_path(pathname: &str) -> bool {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^/[^/]+/[^/]+/assets/\d+/[\w-]+$").expect("legacy attachment")
    });
    PATTERN.is_match(pathname)
}

fn repository_file(pathname: &str) -> Option<(String, String, String)> {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^/([^/]+)/([^/]+)/(?:raw|blob)/(.*[^/])$").expect("repo file")
    });
    let captures = PATTERN.captures(pathname)?;
    Some((
        captures.get(1)?.as_str().to_string(),
        captures.get(2)?.as_str().to_string(),
        captures.get(3)?.as_str().to_string(),
    ))
}

/// Port, userinfo, and fragment say nothing about which bytes GitHub will serve.
fn canonical_url(host: &str, url: &Url) -> String {
    let search = url
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    format!("https://{host}{}{search}", url.path())
}

/// The URL to fetch with a GitHub credential for `source`, or null when the source is not
/// GitHub-hosted media.
pub fn github_media_fetch_url(source: &str) -> Option<String> {
    let url = Url::parse(source).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host == RAW_HOST || host == LFS_HOST {
        return Some(canonical_url(&host, &url));
    }
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    if attachment_path(url.path()) || legacy_attachment_path(url.path()) {
        return Some(format!("https://github.com{}", url.path()));
    }
    repository_file(url.path())
        .map(|(owner, repo, rest)| format!("https://{RAW_HOST}/{owner}/{repo}/{rest}"))
}

/// Last path segment, for the signed URL's display name.
pub fn github_media_file_name(fetch_url: &str) -> String {
    let url = Url::parse(fetch_url).ok();
    let segment = url
        .as_ref()
        .and_then(|url| url.path().rsplit('/').next())
        .unwrap_or("");
    let decoded = decode_uri_component(segment).unwrap_or_else(|_| segment.to_string());
    static CONTROLS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[\p{Cc}\\/]").expect("controls"));
    let name = CONTROLS.replace_all(&decoded, "");
    if name.is_empty() {
        "github-media".to_string()
    } else {
        name.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetches_github_hosted_media_with_a_credential() {
        assert_eq!(
            github_media_fetch_url("https://github.com/user-attachments/assets/abc-123?jwt=1#frag")
                .as_deref(),
            Some("https://github.com/user-attachments/assets/abc-123")
        );
        assert_eq!(
            github_media_fetch_url("https://github.com/acme/app/blob/main/shot.png").as_deref(),
            Some("https://raw.githubusercontent.com/acme/app/main/shot.png")
        );
        assert_eq!(
            github_media_fetch_url(
                "https://raw.githubusercontent.com/acme/app/main/shot.png?token=1"
            )
            .as_deref(),
            Some("https://raw.githubusercontent.com/acme/app/main/shot.png?token=1")
        );
        assert_eq!(github_media_fetch_url("https://example.com/shot.png"), None);
    }

    #[test]
    fn names_the_last_path_segment() {
        assert_eq!(
            github_media_file_name("https://github.com/user-attachments/assets/shot%20name.png"),
            "shot name.png"
        );
        assert_eq!(
            github_media_file_name("https://github.com/user-attachments/assets/%"),
            "%"
        );
    }
}
