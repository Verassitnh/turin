//! Port of upstream/packages/shared/src/providerAuthReturnUrl.ts
//!
//! `isLoopbackHost` is copied from `preview.ts`. That file is a cut preview
//! feature; this check is the only piece kept code needs.

use url::Url;

/// Exact host strings from upstream `preview.ts` `LOOPBACK_HOSTS`, plus `[::1]`.
fn is_loopback_host(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]"
    )
}

fn js_host(url: &Url) -> String {
    let Some(host) = url.host_str() else {
        return String::new();
    };
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

fn search(url: &Url) -> String {
    url.query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default()
}

fn hash(url: &Url) -> String {
    url.fragment()
        .map(|fragment| format!("#{fragment}"))
        .unwrap_or_default()
}

/// Only return to a local client or the hosted T3 client, never an arbitrary OAuth-supplied URL.
pub fn provider_auth_return_url(value: Option<&str>) -> Option<String> {
    let value = value.filter(|value| !value.is_empty())?;
    let mut url = Url::parse(value).ok()?;
    let scheme = url.scheme();
    let desktop = (scheme == "t3code" || scheme == "t3code-dev") && js_host(&url) == "app";
    let hostname = url.host_str().unwrap_or("");
    let origin = url.origin().ascii_serialization();
    let web = (scheme == "http" || scheme == "https")
        && (is_loopback_host(hostname) || origin == "https://app.t3.codes");
    let path = url.path().to_string();
    if !url.username().is_empty()
        || url.password().is_some()
        || (!desktop && !web)
        || (path != "/welcome" && path != "/settings" && !path.starts_with("/settings/"))
    {
        return None;
    }
    let welcome = path == "/welcome";
    let kept: Vec<(String, String)> = if welcome {
        Vec::new()
    } else {
        url.query_pairs()
            .filter(|(key, _)| {
                matches!(
                    key.as_ref(),
                    "machine" | "project" | "checkout" | "environmentId" | "instanceId"
                )
            })
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    };
    if kept.is_empty() {
        url.set_query(None);
    } else {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in &kept {
            serializer.append_pair(key, value);
        }
        url.set_query(Some(&serializer.finish()));
    }
    let current_hash = hash(&url);
    static AGENTS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^#agents:[\w-]+$").expect("agents hash"));
    if path != "/welcome" || !AGENTS.is_match(&current_hash) {
        url.set_fragment(None);
    }
    Some(format!(
        "{}://{}{}{}{}",
        url.scheme(),
        js_host(&url),
        url.path(),
        search(&url),
        hash(&url)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_to_welcome_and_the_selected_settings_instance() {
        for scheme in ["t3code", "t3code-dev"] {
            assert_eq!(
                provider_auth_return_url(Some(&format!(
                    "{scheme}://app/welcome?code=secret#agents:machine-id"
                )))
                .as_deref(),
                Some(format!("{scheme}://app/welcome#agents:machine-id")).as_deref()
            );
            assert_eq!(
                provider_auth_return_url(Some(&format!(
                    "{scheme}://app/settings/providers?instanceId=work&code=secret"
                )))
                .as_deref(),
                Some(format!("{scheme}://app/settings/providers?instanceId=work")).as_deref()
            );
        }
    }

    #[test]
    fn rejects_untrusted_destinations() {
        for url in [
            "t3code://attacker/welcome",
            "t3code://app:123/welcome",
            "t3code://app/auth/callback",
            "t3code://user@ app/welcome",
            "t3code://app/welcome/../evil",
            "https://attacker.example/welcome",
            "file:///welcome",
            "javascript:alert(1)",
        ] {
            assert_eq!(provider_auth_return_url(Some(url)), None, "{url}");
        }
    }
}
