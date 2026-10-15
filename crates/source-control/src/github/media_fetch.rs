//! Port of upstream/apps/server/src/assets/GitHubMediaFetch.ts
//!
//! Serves media a pull request body points at on GitHub through the `gh` credential, which is the
//! only thing that distinguishes a readable private attachment from a 404. Upstream returns an
//! `HttpServerResponse` from an Effect route; here [`github_media_response`] returns the status,
//! headers and body for the app's HTTP layer to write out.
//!
//! Upstream's asset also carries `cwd`, which this fetch never reads, so it is not carried here.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use regex::Regex;

use super::credentials::GitHubCredentials;
use crate::shared::github_media::github_media_file_name;
use crate::shared::mime_types::get_type;
use crate::source_control_core::Cause;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::redacted::Redacted;

/// Exactly the hosts the credential is for. Everything a redirect leads to — the presigned
/// object stores GitHub hands assets off to — authorizes with its own signature, and some of
/// them reject a request that also carries a bearer token.
const CREDENTIALED_HOSTS: [&str; 4] = [
    "github.com",
    "www.github.com",
    "raw.githubusercontent.com",
    "media.githubusercontent.com",
];

/// GitHub answers an asset request with a 302 to a signed object URL that needs no credential.
const MAX_REDIRECTS: usize = 3;
/// How long a hop may take to connect, and then to send its headers. The streamed body is not
/// timed: a large asset takes as long as it takes. Upstream's media fetch has no timeout at all.
const MEDIA_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MEDIA_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// A hop that did not connect, or did not send headers, in time. The redirect chain treats it as
/// a broken hop, which is a 502, rather than as a failure of the route.
#[derive(Debug)]
struct HopTimeout;

impl fmt::Display for HopTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("The media host did not answer in time.")
    }
}

impl std::error::Error for HopTimeout {}
/// Passed through so a seek in a long video costs one upstream range request, not a full download.
const FORWARDED_REQUEST_HEADERS: [&str; 2] = ["range", "if-range"];
const FORWARDED_RESPONSE_HEADERS: [&str; 6] = [
    "content-type",
    "content-length",
    "content-range",
    "accept-ranges",
    "etag",
    "last-modified",
];
/// A pull request embeds pictures and recordings. Anything else is not served from our origin.
static MEDIA_CONTENT_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?i)(?:image|video|audio)/[A-Za-z0-9_!#$&^.+-]+$").unwrap());
const SVG_CONTENT_TYPE: &str = "image/svg+xml";
// An SVG is a document: same policy the asset route gives a workspace SVG, so one embedded in a
// body cannot run script against this origin.
const SVG_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// The media a pull request body points at: the GitHub URL and when the signed URL it was
/// issued with stops working, in epoch milliseconds.
#[derive(Debug, Clone)]
pub struct GitHubMediaAsset {
    pub url: String,
    pub expires_at: i64,
}

/// One request to GitHub or to a signed object store. Redirects are never followed by the
/// transport: [`github_media_response`] follows them so the credential can be withheld.
#[derive(Debug, Clone)]
pub struct GitHubMediaRequest {
    pub url: String,
    /// Lower-cased names.
    pub headers: Vec<(String, String)>,
}

pub struct GitHubMediaHttpResponse {
    pub status: u16,
    /// Lower-cased names; a repeated header is joined with ", " as `fetch` does.
    pub headers: HashMap<String, String>,
    /// Streamed, never buffered: a video is served as it arrives.
    pub stream: BoxStream<'static, Result<Bytes, Cause>>,
}

/// What [`github_media_response`] answers with. Upstream's `HttpServerResponse`, with the body
/// either empty or streamed.
pub struct GitHubMediaResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: GitHubMediaBody,
}

pub enum GitHubMediaBody {
    Empty,
    Stream(BoxStream<'static, Result<Bytes, Cause>>),
}

/// What GitHub media fetches need of an HTTP client. An implementation must not follow redirects:
/// [`github_media_response`] does, and withholds the credential from every hop off GitHub.
#[async_trait]
pub trait GitHubMediaHttp: Send + Sync {
    async fn get(&self, request: GitHubMediaRequest) -> Result<GitHubMediaHttpResponse, Cause>;
}

/// Exactly the hosts the credential is for, compared the way `new URL(url).hostname` is.
fn is_credentialed_host(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| CREDENTIALED_HOSTS.contains(&host.as_str()))
}

/// Follows GitHub's redirect to the signed object itself, and never carries the credential off
/// GitHub: the object URL authorizes with its own signature, and the store it lives in has no
/// business seeing a token. `None` when the chain is not a usable answer.
async fn fetch_following_redirects(
    http: &dyn GitHubMediaHttp,
    url: &str,
    headers: &[(String, String)],
    token: Option<&Redacted<String>>,
) -> Result<Option<GitHubMediaHttpResponse>, Cause> {
    let mut target = url.to_owned();
    for hop in 0.. {
        // The credential rides only on a request to GitHub itself. A redirect leads to a signed
        // object URL that authorizes on its own, and the store it lives in has no business seeing
        // a token — deciding that from the target, not from the hop count, is what makes it so.
        let authorization = token
            .filter(|_| is_credentialed_host(&target))
            .map(|token| format!("Bearer {}", token.value()));
        let mut request_headers = headers.to_vec();
        // The bytes are streamed straight through, so never let an encoding layer in.
        request_headers.push(("accept-encoding".to_owned(), "identity".to_owned()));
        if let Some(authorization) = authorization {
            request_headers.push(("authorization".to_owned(), authorization));
        }
        let response = match http
            .get(GitHubMediaRequest {
                url: target.clone(),
                headers: request_headers,
            })
            .await
        {
            Ok(response) => response,
            Err(error) if error.is::<HopTimeout>() => return Ok(None),
            Err(error) => return Err(error),
        };
        if !(300..400).contains(&response.status) {
            return Ok(Some(response));
        }
        let location = response.headers.get("location");
        // A chain this long is not GitHub answering with bytes, and its body is not the media.
        let (Some(location), true) = (location, hop < MAX_REDIRECTS) else {
            return Ok(None);
        };
        // Upstream's `new URL(location, target)` throws here; a broken hop is a 502 instead.
        let Some(next) = reqwest::Url::parse(&target)
            .ok()
            .and_then(|base| base.join(location).ok())
        else {
            return Ok(None);
        };
        if next.scheme() != "https" {
            return Ok(None);
        }
        target = next.to_string();
    }
    unreachable!("the redirect loop returns from inside")
}

/// Serves media a pull request body points at on GitHub through the `gh` credential, which is the
/// only thing that distinguishes a readable private attachment from a 404. `request_headers` are
/// the client's, lower-cased; only the range headers are forwarded. Errors are the transport's:
/// upstream's `HttpClient` failure is the route's failure.
pub async fn github_media_response(
    credentials: &dyn GitHubCredentials,
    http: &dyn GitHubMediaHttp,
    clock: &dyn Clock,
    asset: &GitHubMediaAsset,
    request_headers: &[(String, String)],
) -> Result<GitHubMediaResponse, Cause> {
    // Both media hosts are served by github.com's account, which is the host `gh` stores it under.
    // A private asset fails the way it does in a browser that is not signed in.
    let token = credentials
        .get("github.com")
        .await
        .ok()
        .map(|credential| credential.token);
    let forwarded: Vec<(String, String)> = FORWARDED_REQUEST_HEADERS
        .iter()
        .filter_map(|name| {
            request_headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(key, value)| (key.clone(), value.clone()))
        })
        .collect();
    let response = fetch_following_redirects(http, &asset.url, &forwarded, token.as_ref()).await?;
    // An upload GitHub hosts never changes under its URL, so the only thing a cached copy must
    // not outlive is the signed URL that granted it — which is the same bound the URL itself has.
    let remaining_seconds = (asset.expires_at - clock.now_millis()).div_euclid(1000);
    let mut headers = HashMap::from([
        (
            "cache-control".to_owned(),
            if remaining_seconds > 0 {
                format!("private, max-age={remaining_seconds}")
            } else {
                "private, no-store".to_owned()
            },
        ),
        ("x-content-type-options".to_owned(), "nosniff".to_owned()),
    ]);
    let empty = |status: u16, headers: HashMap<String, String>| GitHubMediaResponse {
        status,
        headers,
        body: GitHubMediaBody::Empty,
    };
    let Some(response) = response else {
        return Ok(empty(502, headers));
    };
    // An upstream refusal is the client's answer, not this server's fault; only a broken hop is.
    // It carries none of the upstream entity headers: a `content-length` with no body behind it
    // holds the connection open until the browser gives up on it.
    if response.status >= 400 {
        let status = if response.status >= 500 {
            502
        } else {
            response.status
        };
        return Ok(empty(status, headers));
    }
    // Only pictures and recordings leave this origin, and never on GitHub's word alone: the raw
    // host labels every committed binary `application/octet-stream`, so the name decides those.
    let upstream_type = response
        .headers
        .get("content-type")
        .map(|value| value.split(';').next().unwrap_or("").trim().to_lowercase())
        .unwrap_or_default();
    let content_type = if MEDIA_CONTENT_TYPE.is_match(&upstream_type) {
        upstream_type
    } else {
        get_type(&github_media_file_name(&asset.url))
            .unwrap_or("")
            .to_lowercase()
    };
    if !MEDIA_CONTENT_TYPE.is_match(&content_type) {
        return Ok(empty(415, headers));
    }
    for name in FORWARDED_RESPONSE_HEADERS {
        if let Some(value) = response.headers.get(name) {
            headers.insert(name.to_owned(), value.clone());
        }
    }
    headers.insert("content-type".to_owned(), content_type.clone());
    if content_type == SVG_CONTENT_TYPE {
        headers.insert(
            "content-security-policy".to_owned(),
            SVG_CONTENT_SECURITY_POLICY.to_owned(),
        );
    }
    Ok(GitHubMediaResponse {
        status: response.status,
        headers,
        body: GitHubMediaBody::Stream(response.stream),
    })
}

/// [`GitHubMediaHttp`] over `reqwest`, with redirects turned off so [`github_media_response`]
/// decides each hop.
#[derive(Debug, Clone)]
pub struct ReqwestGitHubMediaHttp {
    client: reqwest::Client,
    header_timeout: Duration,
}

impl ReqwestGitHubMediaHttp {
    pub fn new() -> Self {
        Self::with_header_timeout(MEDIA_HEADER_TIMEOUT)
    }

    /// Like [`Self::new`], with a different bound on each hop's connect and headers.
    pub fn with_header_timeout(header_timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(MEDIA_CONNECT_TIMEOUT)
            .build()
            .expect("the GitHub media client builds");
        Self {
            client,
            header_timeout,
        }
    }
}

impl Default for ReqwestGitHubMediaHttp {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GitHubMediaHttp for ReqwestGitHubMediaHttp {
    async fn get(&self, request: GitHubMediaRequest) -> Result<GitHubMediaHttpResponse, Cause> {
        let mut builder = self.client.get(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        // Only the wait for headers is bounded here; the body is streamed below without a deadline.
        let response = match tokio::time::timeout(self.header_timeout, builder.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) if error.is_timeout() => return Err(Arc::new(HopTimeout) as Cause),
            Ok(Err(error)) => return Err(Arc::new(error) as Cause),
            Err(_elapsed) => return Err(Arc::new(HopTimeout) as Cause),
        };
        let status = response.status().as_u16();
        let mut headers: HashMap<String, String> = HashMap::new();
        for (name, value) in response.headers() {
            // Repeated headers combine with ", " as `fetch` does.
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            headers
                .entry(name.as_str().to_ascii_lowercase())
                .and_modify(|existing| {
                    existing.push_str(", ");
                    existing.push_str(&value);
                })
                .or_insert(value);
        }
        let stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| Arc::new(error) as Cause))
            .boxed();
        Ok(GitHubMediaHttpResponse {
            status,
            headers,
            stream,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::stream;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::github::credentials::{
        GitHubCredential, GitHubCredentialSource, GitHubCredentialUnavailableError,
        GitHubNotSignedInError,
    };
    use crate::source_control_core::clock::ManualClock;

    const NOW: i64 = 1_000_000;

    /// Fails the first lookup as a signed-out machine would, then hands out one token.
    struct SignedInLater {
        lookups: AtomicUsize,
    }

    #[async_trait]
    impl GitHubCredentials for SignedInLater {
        async fn get(
            &self,
            host: &str,
        ) -> Result<GitHubCredential, GitHubCredentialUnavailableError> {
            if self.lookups.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(GitHubNotSignedInError {
                    host: host.to_owned(),
                    account: None,
                }
                .into());
            }
            Ok(GitHubCredential {
                host: host.to_owned(),
                token: Redacted::new("signed-in".to_owned()),
                source: GitHubCredentialSource::Gh,
                fingerprint: "fingerprint".to_owned(),
            })
        }
        async fn invalidate(&self, _host: &str) {}
    }

    struct SignedIn;

    #[async_trait]
    impl GitHubCredentials for SignedIn {
        async fn get(
            &self,
            host: &str,
        ) -> Result<GitHubCredential, GitHubCredentialUnavailableError> {
            Ok(GitHubCredential {
                host: host.to_owned(),
                token: Redacted::new("signed-in".to_owned()),
                source: GitHubCredentialSource::Gh,
                fingerprint: "fingerprint".to_owned(),
            })
        }
        async fn invalidate(&self, _host: &str) {}
    }

    type Respond = Box<dyn Fn(&GitHubMediaRequest) -> GitHubMediaHttpResponse + Send + Sync>;

    struct FakeMediaHttp {
        respond: Respond,
        requests: Mutex<Vec<GitHubMediaRequest>>,
    }

    impl FakeMediaHttp {
        fn new(
            respond: impl Fn(&GitHubMediaRequest) -> GitHubMediaHttpResponse + Send + Sync + 'static,
        ) -> Self {
            Self {
                respond: Box::new(respond),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn authorizations(&self) -> Vec<Option<String>> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|request| {
                    request
                        .headers
                        .iter()
                        .find(|(name, _)| name == "authorization")
                        .map(|(_, value)| value.clone())
                })
                .collect()
        }
    }

    #[async_trait]
    impl GitHubMediaHttp for FakeMediaHttp {
        async fn get(&self, request: GitHubMediaRequest) -> Result<GitHubMediaHttpResponse, Cause> {
            let response = (self.respond)(&request);
            self.requests.lock().unwrap().push(request);
            Ok(response)
        }
    }

    fn response(status: u16, headers: &[(&str, &str)]) -> GitHubMediaHttpResponse {
        GitHubMediaHttpResponse {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            stream: stream::empty().boxed(),
        }
    }

    fn asset(url: &str) -> GitHubMediaAsset {
        GitHubMediaAsset {
            url: url.to_owned(),
            expires_at: i64::MAX / 2,
        }
    }

    async fn serve(
        credentials: &dyn GitHubCredentials,
        http: &dyn GitHubMediaHttp,
        asset: &GitHubMediaAsset,
        request_headers: &[(String, String)],
    ) -> GitHubMediaResponse {
        github_media_response(
            credentials,
            http,
            &*ManualClock::new(NOW),
            asset,
            request_headers,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn loads_private_media_immediately_after_login_with_the_github_credential() {
        let credentials = SignedInLater {
            lookups: AtomicUsize::new(0),
        };
        let http = FakeMediaHttp::new(|request| {
            let signed_in = request
                .headers
                .iter()
                .any(|(name, _)| name == "authorization");
            response(
                if signed_in { 200 } else { 404 },
                &[("content-type", "image/png")],
            )
        });
        let media = asset("https://raw.githubusercontent.com/owner/repo/main/shot.png");
        assert_eq!(serve(&credentials, &http, &media, &[]).await.status, 404);
        assert_eq!(serve(&credentials, &http, &media, &[]).await.status, 200);
        assert_eq!(serve(&credentials, &http, &media, &[]).await.status, 200);
        // Caching the token is GitHubCredentials' job; this asks it every time.
        assert_eq!(credentials.lookups.load(Ordering::SeqCst), 3);
        assert_eq!(
            http.authorizations(),
            [
                None,
                Some("Bearer signed-in".to_owned()),
                Some("Bearer signed-in".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn withholds_the_credential_from_a_redirect_off_github() {
        let http = FakeMediaHttp::new(|request| match request.url.as_str() {
            "https://github.com/user-attachments/assets/abc" => response(
                302,
                &[("location", "https://objects.example.test/signed/abc?sig=1")],
            ),
            _ => response(200, &[("content-type", "image/png")]),
        });
        let media = asset("https://github.com/user-attachments/assets/abc");
        let answer = serve(&SignedIn, &http, &media, &[]).await;
        assert_eq!(answer.status, 200);
        assert_eq!(
            http.authorizations(),
            [Some("Bearer signed-in".to_owned()), None]
        );
    }

    #[tokio::test]
    async fn follows_a_redirect_between_github_hosts_with_the_credential() {
        let http = FakeMediaHttp::new(|request| match request.url.as_str() {
            "https://github.com/owner/repo/raw/main/logo.svg" => response(
                302,
                &[(
                    "location",
                    "https://media.githubusercontent.com/owner/repo/main/logo.svg",
                )],
            ),
            _ => response(200, &[("content-type", "image/svg+xml; charset=utf-8")]),
        });
        let media = asset("https://github.com/owner/repo/raw/main/logo.svg");
        let answer = serve(&SignedIn, &http, &media, &[]).await;
        assert_eq!(answer.headers["content-type"], "image/svg+xml");
        assert_eq!(
            answer.headers["content-security-policy"],
            SVG_CONTENT_SECURITY_POLICY
        );
        assert_eq!(
            http.authorizations(),
            [
                Some("Bearer signed-in".to_owned()),
                Some("Bearer signed-in".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn refuses_a_redirect_without_a_location_as_a_broken_hop() {
        let http = FakeMediaHttp::new(|_| response(302, &[]));
        let media = asset("https://github.com/owner/repo/raw/main/shot.png");
        let answer = serve(&SignedIn, &http, &media, &[]).await;
        assert_eq!(answer.status, 502);
        assert!(matches!(answer.body, GitHubMediaBody::Empty));
    }

    #[tokio::test]
    async fn maps_upstream_failures_without_forwarding_entity_headers() {
        let http = FakeMediaHttp::new(|_| response(500, &[("content-length", "9")]));
        let media = asset("https://github.com/owner/repo/raw/main/shot.png");
        let answer = serve(&SignedIn, &http, &media, &[]).await;
        assert_eq!(answer.status, 502);
        assert!(!answer.headers.contains_key("content-length"));
        assert_eq!(answer.headers["x-content-type-options"], "nosniff");
    }

    #[tokio::test]
    async fn names_the_media_type_from_the_file_when_github_says_octet_stream() {
        let http =
            FakeMediaHttp::new(|_| response(200, &[("content-type", "application/octet-stream")]));
        let media = asset("https://raw.githubusercontent.com/owner/repo/main/clip.MP4");
        let answer = serve(&SignedIn, &http, &media, &[]).await;
        assert_eq!(answer.status, 200);
        assert_eq!(answer.headers["content-type"], "video/mp4");
    }

    #[tokio::test]
    async fn refuses_anything_that_is_not_a_picture_or_recording() {
        let http = FakeMediaHttp::new(|_| response(200, &[("content-type", "text/html")]));
        let media = asset("https://raw.githubusercontent.com/owner/repo/main/index.html");
        let answer = serve(&SignedIn, &http, &media, &[]).await;
        assert_eq!(answer.status, 415);
    }

    #[tokio::test]
    async fn marks_an_expired_signed_url_no_store_and_forwards_only_range_headers() {
        let http = FakeMediaHttp::new(|_| response(206, &[("content-type", "video/mp4")]));
        let expired = GitHubMediaAsset {
            url: "https://github.com/user-attachments/assets/abc".to_owned(),
            expires_at: NOW - 1,
        };
        let requests = [
            ("range".to_owned(), "bytes=0-99".to_owned()),
            ("cookie".to_owned(), "session=secret".to_owned()),
        ];
        let answer = serve(&SignedIn, &http, &expired, &requests).await;
        assert_eq!(answer.status, 206);
        assert_eq!(answer.headers["cache-control"], "private, no-store");
        let sent = http.requests.lock().unwrap();
        assert!(
            sent[0]
                .headers
                .contains(&("range".to_owned(), "bytes=0-99".to_owned()))
        );
        assert!(!sent[0].headers.iter().any(|(name, _)| name == "cookie"));
    }

    /// A host that accepts the connection and never answers: the hop is broken, so the answer is a
    /// 502 and the request ends, rather than holding the connection open.
    #[tokio::test]
    async fn a_hop_that_never_sends_headers_is_a_broken_hop_not_a_hang() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let silent = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            // Hold the socket open without a word, as a stalled host does.
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(socket);
        });
        let http = ReqwestGitHubMediaHttp::with_header_timeout(Duration::from_millis(50));
        let asset = GitHubMediaAsset {
            url: format!("http://{address}/attachment.png"),
            expires_at: NOW + 60_000,
        };

        let answer = tokio::time::timeout(
            Duration::from_secs(10),
            github_media_response(&SignedIn, &http, &*ManualClock::new(NOW), &asset, &[]),
        )
        .await
        .expect("the request ends")
        .unwrap();

        assert_eq!(answer.status, 502);
        silent.abort();
    }
}
