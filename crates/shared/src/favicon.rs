//! Port of upstream/packages/shared/src/favicon.ts

use crate::host_classification::is_public_favicon_host;
use crate::js_string::encode_uri_component;
use url::Url;

/// Mirrors Codex's generic Browser Use fallback: ask the page origin for its
/// conventional favicon and let the image element fall back to a browser glyph.
fn favicon_url_for_page(raw_url: Option<&str>) -> Option<String> {
    let raw_url = raw_url.filter(|value| !value.is_empty() && value.len() <= 4096)?;
    let page_url = Url::parse(raw_url).ok()?;
    if page_url.scheme() != "http" && page_url.scheme() != "https" {
        return None;
    }
    let mut icon = page_url;
    icon.set_path("/favicon.ico");
    icon.set_query(None);
    icon.set_fragment(None);
    Some(icon.to_string())
}

const THEMED_FAVICON_BY_HOSTNAME: &[(&str, &str, &str)] = &[(
    "github.com",
    "https://github.githubassets.com/favicons/favicon.svg",
    "https://github.githubassets.com/favicons/favicon-dark.svg",
)];

fn themed_favicon_url_for_page(raw_url: Option<&str>, appearance: &str) -> Option<String> {
    let raw_url = raw_url.filter(|value| !value.is_empty() && value.len() <= 4096)?;
    let page_url = Url::parse(raw_url).ok()?;
    if page_url.scheme() != "http" && page_url.scheme() != "https" {
        return None;
    }
    let host = page_url.host_str()?.to_ascii_lowercase();
    THEMED_FAVICON_BY_HOSTNAME
        .iter()
        .find(|(name, _, _)| *name == host)
        .map(|(_, light, dark)| {
            if appearance == "dark" {
                (*dark).to_string()
            } else {
                (*light).to_string()
            }
        })
}

/// Accepts image URLs supplied by a trusted provider event.
fn explicit_favicon_url(raw_url: Option<&str>) -> Option<String> {
    let raw_url = raw_url.filter(|value| !value.is_empty() && value.len() <= 4096)?;
    let url = Url::parse(raw_url).ok()?;
    if url.scheme() == "http" || url.scheme() == "https" || url.scheme() == "data" {
        Some(url.to_string())
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ToolActivityIcon<'a> {
    pub page_url: &'a str,
    pub favicon_url: Option<&'a str>,
    pub favicon_url_dark: Option<&'a str>,
}

/// Chooses a website icon for the app's resolved theme.
pub fn tool_activity_favicon_url(icon: ToolActivityIcon<'_>, appearance: &str) -> Option<String> {
    if appearance == "dark" {
        return explicit_favicon_url(icon.favicon_url_dark)
            .or_else(|| themed_favicon_url_for_page(Some(icon.page_url), "dark"))
            .or_else(|| explicit_favicon_url(icon.favicon_url))
            .or_else(|| favicon_url_for_page(Some(icon.page_url)));
    }
    explicit_favicon_url(icon.favicon_url)
        .or_else(|| themed_favicon_url_for_page(Some(icon.page_url), "light"))
        .or_else(|| favicon_url_for_page(Some(icon.page_url)))
}

/// Return a public favicon URL that discloses only a public hostname, never the
/// port, path, or a private or internal-looking host.
pub fn favicon_url_for_origin(raw_url: Option<&str>, size: u32) -> Option<String> {
    let raw_url = raw_url.filter(|value| !value.is_empty())?;
    let url = Url::parse(raw_url).ok()?;
    let hostname = url.host_str().filter(|host| !host.is_empty())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    if !is_public_favicon_host(hostname) {
        return None;
    }
    Some(format!(
        "https://www.google.com/s2/favicons?domain={}&sz={size}",
        encode_uri_component(hostname)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_disclose_private_origins() {
        for origin in [
            "http://192.168.1.10:8080",
            "http://localhost:3000",
            "http://home.arpa",
            "https://printer.local.",
            "https://api.internal",
            "https://box.tailnet.ts.net",
            "http://127.1",
            "http://0x7f000001",
            "http://[::]",
            "http://[::1]",
            "http://[::ffff:192.168.1.10]",
            "http://[fd00::1]",
            "http://[fe80::1]",
            "http://100.64.0.1",
            "http://198.51.100.1",
            "http://[2001:db8::1]",
            "http://service.test",
            "http://private.onion",
            "http://127.1..",
            "https://grafana.corp",
            "https://build.lan",
            "https://wiki.intranet",
            "https://grafana.internal.acme-corp.com",
            "https://jira.corp.acme-corp.com:8443",
        ] {
            assert_eq!(favicon_url_for_origin(Some(origin), 32), None, "{origin}");
        }
    }

    #[test]
    fn sends_only_the_public_hostname_and_size() {
        assert_eq!(
            favicon_url_for_origin(
                Some("https://github.com:8443/pingdotgg/t3code?private=query"),
                64
            ),
            Some("https://www.google.com/s2/favicons?domain=github.com&sz=64".to_string())
        );
    }

    #[test]
    fn rejects_invalid_origins() {
        assert_eq!(favicon_url_for_origin(None, 32), None);
        assert_eq!(favicon_url_for_origin(Some(""), 32), None);
        assert_eq!(favicon_url_for_origin(Some("invalid URL"), 32), None);
        assert_eq!(
            favicon_url_for_origin(Some("file:///tmp/private"), 32),
            None
        );
        assert_eq!(
            favicon_url_for_origin(Some("data:text/plain,private"), 32),
            None
        );
    }

    #[test]
    fn tool_activity_uses_the_page_origin() {
        assert_eq!(
            tool_activity_favicon_url(
                ToolActivityIcon {
                    page_url: "https://example.com/docs/page?q=1",
                    favicon_url: None,
                    favicon_url_dark: None,
                },
                "light",
            )
            .as_deref(),
            Some("https://example.com/favicon.ico")
        );
        assert_eq!(
            tool_activity_favicon_url(
                ToolActivityIcon {
                    page_url: "http://localhost:5173/app",
                    favicon_url: None,
                    favicon_url_dark: None,
                },
                "light",
            )
            .as_deref(),
            Some("http://localhost:5173/favicon.ico")
        );
    }

    #[test]
    fn selects_site_owned_light_and_dark_variants() {
        let github = ToolActivityIcon {
            page_url: "https://github.com/openai/codex",
            favicon_url: None,
            favicon_url_dark: None,
        };
        assert_eq!(
            tool_activity_favicon_url(github.clone(), "light").as_deref(),
            Some("https://github.githubassets.com/favicons/favicon.svg")
        );
        assert_eq!(
            tool_activity_favicon_url(github, "dark").as_deref(),
            Some("https://github.githubassets.com/favicons/favicon-dark.svg")
        );
        let full_color = ToolActivityIcon {
            page_url: "https://example.com/docs",
            favicon_url: Some("https://cdn.example.com/full-color.png"),
            favicon_url_dark: None,
        };
        assert_eq!(
            tool_activity_favicon_url(full_color.clone(), "light").as_deref(),
            Some("https://cdn.example.com/full-color.png")
        );
        assert_eq!(
            tool_activity_favicon_url(full_color, "dark").as_deref(),
            Some("https://cdn.example.com/full-color.png")
        );
    }

    #[test]
    fn prefers_a_provider_supplied_dark_favicon() {
        assert_eq!(
            tool_activity_favicon_url(
                ToolActivityIcon {
                    page_url: "https://example.com/docs",
                    favicon_url: Some("https://example.com/light.svg"),
                    favicon_url_dark: Some("https://example.com/dark.svg"),
                },
                "dark",
            )
            .as_deref(),
            Some("https://example.com/dark.svg")
        );
    }

    #[test]
    fn rejects_extension_urls() {
        assert_eq!(
            tool_activity_favicon_url(
                ToolActivityIcon {
                    page_url: "https://example.com/docs",
                    favicon_url: Some("chrome-extension://example/_favicon/"),
                    favicon_url_dark: None,
                },
                "light",
            )
            .as_deref(),
            Some("https://example.com/favicon.ico")
        );
    }
}
