//! reqwest stand-in for the Effect HTTP client both OpenCode SDKs use.
//!
//! There is no client-wide request timeout: a timeout on the `Client` would
//! cut the long-lived SSE body. JSON calls pass their own deadline. The SSE
//! response is live once the headers are 200; the body is watched separately.

use std::time::Duration;

use bytes::Bytes;
use reqwest::Method;

use crate::auth::{basic_authorization, encode_uri_component};
use crate::error::{EventStreamError, OpenCodeRuntimeError};
use crate::sse::{self, EventStream, SseFrame};
use crate::urlutil::{append_query, join_server_path, parse_http_base};

#[derive(Clone)]
pub struct HttpClient {
    http: reqwest::Client,
    base: reqwest::Url,
    password: String,
    /// v2 always sends Basic, including an empty password. v1 omits the header
    /// when the password is empty (the SDK treats `""` as missing).
    always_authorize: bool,
    directory: Option<String>,
}

impl HttpClient {
    pub fn new(
        base_url: &str,
        password: impl Into<String>,
        always_authorize: bool,
    ) -> Result<Self, OpenCodeRuntimeError> {
        Ok(Self {
            http: shared_http(),
            base: parse_http_base(base_url, "http")?,
            password: password.into(),
            always_authorize,
            directory: None,
        })
    }

    pub fn with_directory(mut self, directory: impl Into<String>) -> Self {
        self.directory = Some(directory.into());
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_json(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        extra_query: &[(String, String)],
        body: Option<&serde_json::Value>,
        expected: &[u16],
        timeout: Option<Duration>,
    ) -> Result<Bytes, OpenCodeRuntimeError> {
        // The deadline covers the body as well as the headers. Upstream wraps the
        // whole SDK call in `Effect.timeoutOption`, so a server that sends headers
        // and then stalls must still fail.
        let exchange = async {
            let response = self
                .send(operation, method, path, query, extra_query, body)
                .await?;
            let status = response.status().as_u16();
            let content_type = header_str(response.headers(), reqwest::header::CONTENT_TYPE);
            if content_type.as_deref() == Some("text/html") {
                return Err(OpenCodeRuntimeError::new(
                    operation,
                    "Request is not supported by this version of OpenCode Server (Server responded with text/html)",
                ));
            }
            let bytes = response.bytes().await.map_err(|err| {
                OpenCodeRuntimeError::new(
                    operation,
                    format!("Couldn't read the OpenCode response: {err}"),
                )
            })?;
            Ok((status, bytes))
        };
        let (status, bytes) = within(operation, timeout, exchange).await?;
        if expected.contains(&status) {
            return Ok(bytes);
        }
        let text = String::from_utf8_lossy(&bytes);
        let text = truncate(&text, 2 * 1024);
        Err(OpenCodeRuntimeError::new(
            operation,
            format!("status={status} body={text}"),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_empty(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        extra_query: &[(String, String)],
        body: Option<&serde_json::Value>,
        expected: &[u16],
        timeout: Option<Duration>,
    ) -> Result<(), OpenCodeRuntimeError> {
        let _ = self
            .send_json(
                operation,
                method,
                path,
                query,
                extra_query,
                body,
                expected,
                timeout,
            )
            .await?;
        Ok(())
    }

    /// Subscribes to an SSE path. The future resolves when the server accepts
    /// the stream (HTTP 200). It does not wait for `server.connected`, and it
    /// does not reconnect.
    pub async fn open_event_stream(
        &self,
        operation: &str,
        path: &str,
        query: &[(&str, String)],
        last_event_id: Option<&str>,
        silent_timeout: Duration,
    ) -> Result<EventStream<SseFrame>, OpenCodeRuntimeError> {
        // The SDK's SSE request skips the interceptor that moves the directory header
        // into `?directory=`, so the subscription sends the header and no query.
        let url = self.build_url(path, query, &[], false);
        let mut builder = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(last_event_id) = last_event_id {
            builder = builder.header("Last-Event-ID", last_event_id);
        }
        builder = self.authorize(builder);
        if let Some(directory) = &self.directory {
            builder = builder.header("x-opencode-directory", encode_uri_component(directory));
        }
        let response = builder
            .send()
            .await
            .map_err(|err| unreachable(operation, err))?;
        let status = response.status().as_u16();
        let content_type = header_str(response.headers(), reqwest::header::CONTENT_TYPE);
        if content_type.as_deref() == Some("text/html") {
            return Err(OpenCodeRuntimeError::new(
                operation,
                "Request is not supported by this version of OpenCode Server (Server responded with text/html)",
            ));
        }
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(OpenCodeRuntimeError::new(
                operation,
                format!("status={status} body={}", truncate(&body, 2 * 1024)),
            )
            .with_cause(EventStreamError::Status {
                status,
                body: truncate(&body, 2 * 1024),
            }));
        }
        let stream = Box::pin(response.bytes_stream());
        Ok(sse::decode_sse(stream, silent_timeout))
    }

    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        extra_query: &[(String, String)],
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, OpenCodeRuntimeError> {
        let url = self.url(&method, path, query, extra_query);
        let mut builder = self.http.request(method.clone(), url);
        builder = self.authorize(builder);
        builder = self.directory_header(builder, method.clone());
        if let Some(body) = body {
            builder = builder.json(body);
        }
        builder
            .send()
            .await
            .map_err(|err| unreachable(operation, err))
    }

    fn url(
        &self,
        method: &Method,
        path: &str,
        query: &[(&str, String)],
        extra_query: &[(String, String)],
    ) -> reqwest::Url {
        // The 1.x SDK copies `x-opencode-directory` onto `?directory=` for GET/HEAD
        // and leaves the header in place for every other method.
        let directory_query = matches!(*method, Method::GET | Method::HEAD);
        self.build_url(path, query, extra_query, directory_query)
    }

    fn build_url(
        &self,
        path: &str,
        query: &[(&str, String)],
        extra_query: &[(String, String)],
        directory_query: bool,
    ) -> reqwest::Url {
        let mut url = join_server_path(&self.base, path);
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if directory_query && let Some(directory) = self.directory.as_deref() {
            extra.push(("directory", directory));
        }
        if extra.is_empty() && query.is_empty() && extra_query.is_empty() {
            return url;
        }
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in extra {
                pairs.append_pair(key, value);
            }
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
            for (key, value) in extra_query {
                pairs.append_pair(key, value);
            }
        }
        url
    }

    pub fn location_query(directory: &str) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        append_query(
            &mut pairs,
            "location",
            &serde_json::json!({ "directory": directory }),
        );
        pairs
    }

    fn authorize(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.password.is_empty() && !self.always_authorize {
            return builder;
        }
        builder.header(
            reqwest::header::AUTHORIZATION,
            basic_authorization(&self.password),
        )
    }

    fn directory_header(
        &self,
        builder: reqwest::RequestBuilder,
        method: Method,
    ) -> reqwest::RequestBuilder {
        let Some(directory) = &self.directory else {
            return builder;
        };
        if method == Method::GET || method == Method::HEAD {
            return builder;
        }
        builder.header("x-opencode-directory", encode_uri_component(directory))
    }
}

/// Runs one whole JSON exchange under `timeout`, if the caller set one.
async fn within<T>(
    operation: &str,
    timeout: Option<Duration>,
    exchange: impl std::future::Future<Output = Result<T, OpenCodeRuntimeError>>,
) -> Result<T, OpenCodeRuntimeError> {
    let Some(timeout) = timeout else {
        return exchange.await;
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result,
        Err(_elapsed) => Err(OpenCodeRuntimeError::new(
            operation,
            "Timed out while waiting for the OpenCode server.",
        )),
    }
}

/// `no_proxy`: upstream's Effect/Node fetch ignores `HTTP(S)_PROXY`, `ALL_PROXY`
/// and the system proxy, so a proxy in the environment must not route the
/// loopback server (or an external one) away from the adapter.
pub fn shared_http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10))
        .no_proxy()
        .build()
        .expect("reqwest client")
}

/// Transport failure with the tokens upstream's status classifier matches
/// (`fetch failed`, `ECONNREFUSED`). reqwest's own Display includes the URL,
/// which must not reach a detail, so the text is written here.
pub fn unreachable(operation: &str, err: reqwest::Error) -> OpenCodeRuntimeError {
    let detail = if connection_refused(&err) {
        "Couldn't reach the OpenCode server (fetch failed: ECONNREFUSED)."
    } else {
        "Couldn't reach the OpenCode server (fetch failed)."
    };
    OpenCodeRuntimeError::new(operation, detail).with_cause(err)
}

fn connection_refused(err: &reqwest::Error) -> bool {
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io.kind() == std::io::ErrorKind::ConnectionRefused;
        }
        source = cause.source();
    }
    false
}

pub fn decode_json<T: serde::de::DeserializeOwned>(
    operation: &str,
    bytes: &[u8],
) -> Result<T, OpenCodeRuntimeError> {
    serde_json::from_slice(bytes).map_err(|err| {
        OpenCodeRuntimeError::new(
            operation,
            format!("OpenCode returned JSON this build cannot read: {err}"),
        )
    })
}

fn header_str(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use reqwest::Method;

    use super::*;
    use crate::test_support::{FakeResponse, FakeServer};

    #[tokio::test]
    async fn head_sends_the_directory_query_and_not_the_header() {
        let server = FakeServer::spawn(|request| {
            assert_eq!(request.method, "HEAD");
            assert!(
                request.target.contains("directory=%2Fwork+dir"),
                "{}",
                request.target
            );
            assert!(request.directory_header.is_none());
            FakeResponse::empty(200)
        })
        .await;
        let client = HttpClient::new(&server.url, "", false)
            .unwrap()
            .with_directory("/work dir");
        client
            .send_empty(
                "head",
                Method::HEAD,
                "/global/health",
                &[],
                &[],
                None,
                &[200],
                None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_deadline_covers_a_body_that_stalls_after_the_headers() {
        // Headers arrive with a 1 MiB Content-Length, a few bytes follow, then the socket stays open.
        let server = FakeServer::spawn(|_| FakeResponse {
            status: 200,
            content_type: Some("application/json".to_string()),
            body: Vec::new(),
            stream: Some(vec![b"{\"healthy\"".to_vec()]),
            hold: true,
            follow: None,
            truncated: true,
        })
        .await;
        let client = HttpClient::new(&server.url, "", false).unwrap();
        let started = std::time::Instant::now();
        let err = client
            .send_json(
                "global.health",
                Method::GET,
                "/global/health",
                &[],
                &[],
                None,
                &[200],
                Some(Duration::from_millis(200)),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.detail,
            "Timed out while waiting for the OpenCode server."
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Child half of [`ignores_proxy_environment_for_loopback`]. The parent sets
    /// the proxy variables and the server URL; without them this test is a no-op.
    #[tokio::test]
    async fn proxy_child() {
        let Ok(url) = std::env::var("TURIN_PROXY_TEST_URL") else {
            return;
        };
        let status = shared_http()
            .get(format!("{url}/global/health"))
            .send()
            .await
            .expect("request reached the loopback server without a proxy")
            .status();
        assert_eq!(status.as_u16(), 200);
    }

    /// A proxy in the environment must not capture a loopback request. reqwest
    /// reads proxy variables when the client is built, so the check runs in a child
    /// process that carries them; the parent's environment is left alone.
    #[tokio::test]
    async fn ignores_proxy_environment_for_loopback() {
        let server = FakeServer::spawn(|_| FakeResponse::json(200, r#"{"healthy":true}"#)).await;
        let exe = std::env::current_exe().unwrap();
        let output = tokio::process::Command::new(exe)
            .args(["--exact", "http_client::tests::proxy_child", "--nocapture"])
            .env("TURIN_PROXY_TEST_URL", &server.url)
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
