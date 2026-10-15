//! Port of upstream/apps/server/src/textGeneration/ThreadTitleLinks.ts
//!
//! Source control links in a message are looked up so the title model can name a PR or
//! issue by its real subject. The lookup goes through [`SourceControlLinkResolver`], the
//! seam for T07's `SourceControlProviderRegistry.resolveLink`, which is not on `stage` yet.

use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;
use url::Url;

/// What a source control provider knows about a linked change or issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSubject {
    pub title: String,
    pub body: Option<String>,
}

/// A lookup in flight. Failure text is never shown to the model; it becomes `unavailable`.
pub type LinkLookup = Pin<Box<dyn Future<Output = Result<LinkSubject, String>> + Send>>;

/// The seam for `SourceControlProviderRegistry.resolveLink`.
pub trait SourceControlLinkResolver: Send + Sync {
    /// `None` when no provider recognizes the URL. Otherwise the lookup, not yet run.
    fn resolve_link(&self, cwd: &str, url: &Url) -> Option<LinkLookup>;
}

/// Upstream `Schema.fromJsonString(Schema.Struct({ title, body }))`.
#[derive(Serialize)]
struct SubjectSummary<'a> {
    title: &'a str,
    body: &'a str,
}

/// At most this many links are looked up. Providers select links before the budget applies.
const MAX_LINKS: usize = 2;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const TITLE_CHARS: usize = 300;
const BODY_CHARS: usize = 1_200;

static LINK_CANDIDATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https://[^\s<>"')\]`]+"#).unwrap());
static TRAILING_PUNCTUATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.,;!?]+$").unwrap());

/// Summarises up to two source control links in `message`, one block per link.
///
/// Returns `None` when the message has no link a provider recognizes. A lookup that
/// fails or takes longer than 3 seconds leaves `<url>: unavailable` in its place.
pub async fn resolve_thread_title_links(
    resolver: &dyn SourceControlLinkResolver,
    message: &str,
    cwd: &str,
) -> Option<String> {
    let mut links: Vec<(String, LinkLookup)> = Vec::new();
    for found in LINK_CANDIDATE.find_iter(message) {
        let candidate = TRAILING_PUNCTUATION.replace(found.as_str(), "");
        let Ok(mut url) = Url::parse(&candidate) else {
            continue;
        };
        url.set_fragment(None);
        url.set_query(None);
        let href = url.as_str().to_owned();
        if links.iter().any(|(seen, _)| *seen == href) {
            continue;
        }
        let Some(lookup) = resolver.resolve_link(cwd, &url) else {
            continue;
        };
        links.push((href, lookup));
        if links.len() == MAX_LINKS {
            break;
        }
    }
    if links.is_empty() {
        return None;
    }

    // Upstream runs these with `concurrency: 2`, which is every link we collected.
    let summaries = futures::future::join_all(links.into_iter().map(|(href, lookup)| async move {
        match tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await {
            Ok(Ok(subject)) => {
                let title = truncate_chars(&subject.title, TITLE_CHARS);
                let body = truncate_chars(subject.body.as_deref().unwrap_or(""), BODY_CHARS);
                let encoded = serde_json::to_string(&SubjectSummary {
                    title: &title,
                    body: &body,
                })
                .unwrap_or_default();
                format!("{href}\n{encoded}")
            }
            Ok(Err(_)) | Err(_) => format!("{href}: unavailable"),
        }
    }))
    .await;
    Some(summaries.join("\n\n"))
}

/// `value.slice(0, max)` in UTF-16 units. The seam hands back Rust strings, so a cut that
/// would split a surrogate pair drops the pair.
fn truncate_chars(value: &str, max_units: usize) -> String {
    turin_provider_core::js::take_utf16(value, max_units).to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    const SUCCESS_TITLE: &str = "Fix QR pairing expiry";
    const SUCCESS_BODY: &str = "Keep remote connections working.";

    fn encode(title: &str, body: &str) -> String {
        serde_json::to_string(&json!({ "title": title, "body": body })).unwrap()
    }

    fn success() -> LinkSubject {
        LinkSubject {
            title: SUCCESS_TITLE.to_owned(),
            body: Some(SUCCESS_BODY.to_owned()),
        }
    }

    /// Resolves `forge.test` links only, through `lookup`, as the provider registry does.
    struct ForgeLinks<F: Fn(&Url) -> LinkLookup + Send + Sync> {
        lookup: F,
        cwds: Mutex<Vec<String>>,
    }

    impl<F: Fn(&Url) -> LinkLookup + Send + Sync> SourceControlLinkResolver for ForgeLinks<F> {
        fn resolve_link(&self, cwd: &str, url: &Url) -> Option<LinkLookup> {
            if url.host_str() != Some("forge.test") {
                return None;
            }
            self.cwds.lock().unwrap().push(cwd.to_owned());
            Some((self.lookup)(url))
        }
    }

    #[tokio::test]
    async fn uses_provider_selected_links_deduplicates_anchors_and_bounds_lookups_and_summaries() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let resolver = ForgeLinks {
            lookup: move |url: &Url| {
                let recorded = recorded.clone();
                let href = url.as_str().to_owned();
                Box::pin(async move {
                    recorded.lock().unwrap().push(href);
                    Ok(LinkSubject {
                        title: "t".repeat(400),
                        body: Some("b".repeat(2_000)),
                    })
                })
            },
            cwds: Mutex::new(Vec::new()),
        };
        let result = resolve_thread_title_links(
            &resolver,
            "https://docs.test/guide [https://forge.test/change/1] https://forge.test/change/1#discussion https://forge.test/change/1?view=full `https://forge.test/change/2` https://forge.test/change/2. https://forge.test/change/3",
            "/tmp/project",
        )
        .await;

        let calls = calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "https://forge.test/change/1".to_owned(),
                "https://forge.test/change/2".to_owned()
            ]
        );
        assert!(
            resolver
                .cwds
                .lock()
                .unwrap()
                .iter()
                .all(|cwd| cwd == "/tmp/project")
        );
        let expected = calls
            .iter()
            .map(|url| format!("{url}\n{}", encode(&"t".repeat(300), &"b".repeat(1_200))))
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(result, Some(expected));
    }

    #[tokio::test(start_paused = true)]
    async fn returns_unavailable_when_a_lookup_times_out_while_retaining_successful_subjects() {
        let resolver = ForgeLinks {
            lookup: |url: &Url| {
                if url.path().ends_with('1') {
                    Box::pin(std::future::pending())
                } else {
                    let subject = success();
                    Box::pin(async move { Ok(subject) })
                }
            },
            cwds: Mutex::new(Vec::new()),
        };
        let result = resolve_thread_title_links(
            &resolver,
            "https://forge.test/change/1 https://forge.test/change/2",
            "/tmp/project",
        )
        .await;
        assert_eq!(
            result,
            Some(format!(
                "https://forge.test/change/1: unavailable\n\nhttps://forge.test/change/2\n{}",
                encode(SUCCESS_TITLE, SUCCESS_BODY)
            ))
        );
    }

    #[tokio::test]
    async fn keeps_lookup_failure_out_of_generation_and_skips_unlinked_messages() {
        let resolver = ForgeLinks {
            lookup: |_: &Url| Box::pin(async { Err("Unavailable".to_owned()) }),
            cwds: Mutex::new(Vec::new()),
        };
        assert_eq!(
            resolve_thread_title_links(&resolver, "Fix pairing", "/tmp").await,
            None
        );
        let linked = resolve_thread_title_links(&resolver, "https://forge.test/change/1", "/tmp")
            .await
            .expect("summary");
        assert!(linked.contains("unavailable"), "{linked}");
    }
}
