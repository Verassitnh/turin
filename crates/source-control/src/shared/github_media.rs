//! Port of upstream/packages/shared/src/githubMedia.ts
//!
//! Media a pull request body points at on GitHub's own hosts. In a private repository GitHub
//! answers an unauthenticated request for one with 404 — a screenshot dropped into a description
//! becomes `github.com/user-attachments/assets/<id>`, and a file committed alongside the code
//! becomes a `raw.githubusercontent.com` path — so the renderer, which carries no GitHub session,
//! draws a broken image where the reviewer expects the evidence. The app can fetch these with
//! the `gh` credential and hand the bytes back.
//!
//! Only hosts where a credential is what decides the answer belong here.
//! `objects.githubusercontent.com` and the `private-user-images` links GitHub's own HTML carries
//! are already signed and load on their own, and a token does nothing for them once that signature
//! expires.

use std::sync::LazyLock;

use regex::Regex;

const RAW_HOST: &str = "raw.githubusercontent.com";
/// Git LFS pointers resolve here, which is where an LFS-tracked screenshot's bytes live.
const LFS_HOST: &str = "media.githubusercontent.com";
static ATTACHMENT_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/user-attachments/assets/[A-Za-z0-9_-]+$").unwrap());
/// What GitHub wrote into a body before `user-attachments`; older descriptions still carry it.
static LEGACY_ATTACHMENT_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/[^/]+/[^/]+/assets/\d+/[A-Za-z0-9_-]+$").unwrap());
/// `blob` and `raw` both address file bytes; `raw` is the one the token is honoured on.
static REPOSITORY_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+)/([^/]+)/(?:raw|blob)/(.*[^/])$").unwrap());

/// Port, userinfo, and fragment say nothing about which bytes GitHub will serve.
fn canonical_url(host: &str, url: &reqwest::Url) -> String {
    let search = match url.query() {
        Some(query) if !query.is_empty() => format!("?{query}"),
        _ => String::new(),
    };
    format!("https://{host}{}{search}", url.path())
}

/// The URL to fetch with a GitHub credential for `source`, or `None` when the source is not
/// GitHub-hosted media — those keep loading directly, exactly as they do today.
pub fn github_media_fetch_url(source: &str) -> Option<String> {
    let url = reqwest::Url::parse(source).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    if host == RAW_HOST || host == LFS_HOST {
        return Some(canonical_url(&host, &url));
    }
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    let path = url.path();
    if ATTACHMENT_PATH.is_match(path) || LEGACY_ATTACHMENT_PATH.is_match(path) {
        return Some(format!("https://github.com{path}"));
    }
    // `?raw=true` is how the web UI spells "the bytes, not the page"; the raw host needs no query.
    REPOSITORY_FILE
        .captures(path)
        .map(|file| format!("https://{RAW_HOST}/{}/{}/{}", &file[1], &file[2], &file[3]))
}

/// Last path segment, for the signed URL's display name. A percent sequence GitHub accepts but
/// `decodeURIComponent` rejects is left encoded rather than failing the whole asset.
pub fn github_media_file_name(fetch_url: &str) -> String {
    let segment = reqwest::Url::parse(fetch_url)
        .ok()
        .and_then(|url| url.path().split('/').next_back().map(str::to_owned))
        .unwrap_or_default();
    let decoded = super::js::decode_uri_component(&segment).unwrap_or(segment);
    // /[\p{Cc}\\/]/gu
    let name: String = decoded
        .chars()
        .filter(|c| !c.is_control() && *c != '\\' && *c != '/')
        .collect();
    if name.is_empty() {
        "github-media".to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_github_hosted_media_and_leaves_the_rest() {
        let cases = [
            (
                "https://github.com/user-attachments/assets/1a2b-3c?x=1#frag",
                Some("https://github.com/user-attachments/assets/1a2b-3c"),
            ),
            (
                "https://www.github.com/acme/web/assets/123/abc-def",
                Some("https://github.com/acme/web/assets/123/abc-def"),
            ),
            (
                "https://github.com/acme/web/blob/main/docs/shot.png?raw=true",
                Some("https://raw.githubusercontent.com/acme/web/main/docs/shot.png"),
            ),
            (
                "https://github.com/acme/web/raw/refs/heads/main/shot.png",
                Some("https://raw.githubusercontent.com/acme/web/refs/heads/main/shot.png"),
            ),
            (
                "https://RAW.githubusercontent.com:443/acme/web/main/a.png?token=1#x",
                Some("https://raw.githubusercontent.com/acme/web/main/a.png?token=1"),
            ),
            (
                "https://media.githubusercontent.com/media/acme/web/main/a.png",
                Some("https://media.githubusercontent.com/media/acme/web/main/a.png"),
            ),
            (
                "https://objects.githubusercontent.com/github-production/x",
                None,
            ),
            (
                "https://private-user-images.githubusercontent.com/1/a.png?jwt=x",
                None,
            ),
            ("http://github.com/user-attachments/assets/abc", None),
            ("https://github.com/acme/web", None),
            ("https://github.com/acme/web/blob/main/", None),
            ("not a url", None),
        ];
        for (source, expected) in cases {
            assert_eq!(
                github_media_fetch_url(source).as_deref(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn names_the_asset_after_its_last_segment() {
        assert_eq!(
            github_media_file_name("https://raw.githubusercontent.com/a/b/main/shot%20one.png"),
            "shot one.png"
        );
        // A sequence decodeURIComponent rejects stays encoded.
        assert_eq!(
            github_media_file_name("https://raw.githubusercontent.com/a/b/main/bad%E0%A4%A.png"),
            "bad%E0%A4%A.png"
        );
        assert_eq!(
            github_media_file_name("https://github.com/user-attachments/assets/"),
            "github-media"
        );
        assert_eq!(
            github_media_file_name("https://raw.githubusercontent.com/a/b/main/x%2Fy%5Cz.png"),
            "xyz.png"
        );
    }
}
