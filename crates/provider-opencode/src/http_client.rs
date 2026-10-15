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
        let response = self
            .send(operation, method, path, query, extra_query, body, timeout)
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
        let url = self.url(&Method::GET, path, query, &[]);
        let mut builder = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(last_event_id) = last_event_id {
            builder = builder.header("Last-Event-ID", last_event_id);
        }
        builder = self.authorize(builder);
        builder = self.directory_header(builder, Method::GET);
        let response = builder.send().await.map_err(|err| {
            OpenCodeRuntimeError::new(operation, "Couldn't reach the OpenCode server.")
                .with_cause(err)
        })?;
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
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, OpenCodeRuntimeError> {
        let url = self.url(&method, path, query, extra_query);
        let mut builder = self.http.request(method.clone(), url);
        builder = self.authorize(builder);
        builder = self.directory_header(builder, method.clone());
        if let Some(body) = body {
            builder = builder.json(body);
        }
        let request = async {
            builder.send().await.map_err(|err| {
                OpenCodeRuntimeError::new(operation, "Couldn't reach the OpenCode server.")
                    .with_cause(err)
            })
        };
        match timeout {
            Some(timeout) => match tokio::time::timeout(timeout, request).await {
                Ok(result) => result,
                Err(_elapsed) => Err(OpenCodeRuntimeError::new(
                    operation,
                    "Timed out while waiting for the OpenCode server.",
                )),
            },
            None => request.await,
        }
    }

    fn url(
        &self,
        method: &Method,
        path: &str,
        query: &[(&str, String)],
        extra_query: &[(String, String)],
    ) -> reqwest::Url {
        let mut url = join_server_path(&self.base, path);
        let mut extra: Vec<(&str, &str)> = Vec::new();
        let directory = self.directory.clone();
        if matches!(*method, Method::GET | Method::HEAD)
            && let Some(directory) = directory.as_deref()
        {
            extra.push(("directory", directory));
        }
        if extra.is_empty() && query.is_empty() && extra_query.is_empty() {
            return url;
        }
        {
            let mut pairs = url.query_pairs_mut();
            // The 1.x SDK copies `x-opencode-directory` onto `?directory=` for GET/HEAD
            // and leaves the header in place for every other method.
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

pub fn shared_http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("reqwest client")
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
}
