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
