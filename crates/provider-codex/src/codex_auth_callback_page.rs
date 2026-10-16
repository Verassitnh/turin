//! Port of upstream/apps/server/src/provider/CodexAuthCallbackPage.ts.
//!
//! The page the browser lands on after the ChatGPT sign-in redirect. It
//! escapes every interpolated value and returns to T3 Code only through the
//! shared return-URL allowlist.

use turin_shared::provider_auth_return_url::provider_auth_return_url;

/// Placeholders are replaced in one pass, so CSS braces need no escaping.
const TEMPLATE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark"><title>@@TITLE@@ · T3 Code</title>
@@REFRESH@@
<style>
:root{font-family:ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;color:#18181b;background:#fafafa;color-scheme:light dark}
*{box-sizing:border-box}body{margin:0;min-height:100svh;display:grid;place-items:center;padding:24px}
main{width:100%;max-width:440px;background:#fff;border:1px solid #e4e4e7;border-radius:20px;padding:32px;box-shadow:0 12px 40px #00000008}
.brand{font-size:18px;letter-spacing:-.5px;margin-bottom:36px;color:#71717a}.brand strong{color:#18181b}
.status{width:40px;height:40px;border-radius:50%;display:grid;place-items:center;background:@@STATUS_BG@@;color:@@STATUS_FG@@;margin-bottom:20px}
h1{font-size:24px;font-weight:600;letter-spacing:-.7px;line-height:1.25;margin:0 0 12px}p{font-size:15px;line-height:1.6;color:#71717a;margin:0}
a{display:inline-flex;align-items:center;justify-content:center;margin-top:28px;padding:10px 16px;border-radius:8px;background:#2563eb;color:white;font-size:14px;font-weight:500;text-decoration:none}a:focus-visible{outline:3px solid #93c5fd;outline-offset:3px}
@media(prefers-color-scheme:dark){:root{color:#fafafa;background:#09090b}main{background:#18181b;border-color:#27272a;box-shadow:0 12px 40px #0003}.brand,p{color:#a1a1aa}.brand strong{color:#fafafa}.status{background:@@DARK_STATUS_BG@@;color:@@DARK_STATUS_FG@@}}
</style></head><body><main><div class="brand"><strong>T3</strong> Code</div>
<div class="status" aria-hidden="true"><svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">@@ICON@@</svg></div>
<h1>@@TITLE@@</h1><p>@@DESCRIPTION@@</p>
@@LINK@@
</main><script nonce="@@NONCE@@">history.replaceState(null,"","/auth/callback");</script></body></html>"##;

/// Fills each `@@NAME@@` placeholder in one pass over the template. Substituted text is
/// never scanned again, so a value that contains a placeholder stays as written.
fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 512);
    let mut rest = template;
    while let Some(start) = rest.find("@@") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let replaced = after.find("@@").and_then(|end| {
            values
                .iter()
                .find(|(name, _)| *name == &after[..end])
                .map(|(_, value)| (end, *value))
        });
        match replaced {
            Some((end, value)) => {
                out.push_str(value);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("@@");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// `codexAuthCallbackPage`. `return_url` is only honored when the shared
/// allowlist accepts it.
pub fn codex_auth_callback_page(success: bool, return_url: Option<&str>, nonce: &str) -> String {
    let destination = provider_auth_return_url(return_url);
    let title = if success {
        "You're signed in"
    } else {
        "Sign-in couldn't finish"
    };
    let description = if success {
        if destination.is_some() {
            "Returning to T3 Code. You're ready to continue."
        } else {
            "Return to T3 Code to continue. You can close this tab."
        }
    } else {
        "Return to T3 Code and try signing in again."
    };
    let refresh = match (&destination, success) {
        (Some(url), true) => format!(
            r#"<meta http-equiv="refresh" content="1;url={}">"#,
            escape_html(url)
        ),
        _ => String::new(),
    };
    let link = match &destination {
        Some(url) => format!(r#"<a href="{}">Return to T3 Code</a>"#, escape_html(url)),
        None => String::new(),
    };
    let icon = if success {
        r#"<path d="m5 12 4 4L19 6"/>"#
    } else {
        r#"<path d="m6 6 12 12M6 18 18 6"/>"#
    };
    let (status_bg, status_fg, dark_bg, dark_fg) = if success {
        ("#ecfdf5", "#059669", "#064e3b", "#34d399")
    } else {
        ("#fef2f2", "#dc2626", "#450a0a", "#f87171")
    };
    let escaped_title = escape_html(title);
    render(
        TEMPLATE,
        &[
            ("TITLE", &escaped_title),
            ("REFRESH", &refresh),
            ("STATUS_BG", status_bg),
            ("STATUS_FG", status_fg),
            ("DARK_STATUS_BG", dark_bg),
            ("DARK_STATUS_FG", dark_fg),
            ("ICON", icon),
            ("DESCRIPTION", description),
            ("LINK", &link),
            ("NONCE", &escape_html(nonce)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESTINATION: &str = "http://localhost:3773/welcome";

    #[test]
    fn success_with_a_return_url_refreshes_and_links_back() {
        let page = codex_auth_callback_page(true, Some(DESTINATION), "n1");
        assert!(
            page.contains("You&#39;re signed in · T3 Code</title>"),
            "{page}"
        );
        assert!(page.contains(
            r#"<meta http-equiv="refresh" content="1;url=http://localhost:3773/welcome">"#
        ));
        assert!(page.contains(r#"<a href="http://localhost:3773/welcome">Return to T3 Code</a>"#));
        assert!(page.contains("Returning to T3 Code. You're ready to continue."));
        assert!(page.contains("m5 12 4 4L19 6"));
    }

    #[test]
    fn success_without_a_return_url_asks_the_user_to_return_manually() {
        let page = codex_auth_callback_page(true, None, "n1");
        assert!(!page.contains("http-equiv=\"refresh\""));
        assert!(!page.contains("<a href="));
        assert!(page.contains("Return to T3 Code to continue. You can close this tab."));
    }

    #[test]
    fn failure_never_refreshes_even_with_a_return_url() {
        let page = codex_auth_callback_page(false, Some(DESTINATION), "n1");
        assert!(!page.contains("http-equiv=\"refresh\""));
        assert!(page.contains("Sign-in couldn&#39;t finish"));
        assert!(page.contains("m6 6 12 12M6 18 18 6"));
        assert!(page.contains("#fef2f2"));
    }

    #[test]
    fn disallowed_return_urls_are_dropped() {
        let page = codex_auth_callback_page(true, Some("https://evil.example/welcome"), "n1");
        assert!(!page.contains("evil.example"));
        assert!(!page.contains("<a href="));
    }

    #[test]
    fn nonce_is_escaped_into_the_script_attribute() {
        let page = codex_auth_callback_page(false, None, r#"a"b<c>"#);
        assert!(page.contains(r#"<script nonce="a&quot;b&lt;c&gt;">"#));
    }

    #[test]
    fn a_value_that_contains_a_placeholder_is_not_expanded() {
        // The return-URL allowlist keeps such text out of real pages; the renderer must not
        // depend on that.
        let out = render(
            "<a href=\"@@LINK@@\">@@TITLE@@</a>",
            &[("LINK", "x=@@TITLE@@&y=@@LINK@@"), ("TITLE", "t@@LINK@@")],
        );
        assert_eq!(out, "<a href=\"x=@@TITLE@@&y=@@LINK@@\">t@@LINK@@</a>");
        // An unknown or unterminated marker is left as it is.
        assert_eq!(render("a @@NOPE@@ b @@ c", &[]), "a @@NOPE@@ b @@ c");
    }
}
