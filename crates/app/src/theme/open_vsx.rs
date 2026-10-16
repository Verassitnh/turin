//! HTTP access for Open VSX (search + asset fetch). Production uses reqwest; tests
//! use [`FakeOpenVsxClient`] against fixtures under `crates/app/tests/fixtures/open_vsx/`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenVsxHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_length: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenVsxHeadResponse {
    pub status: u16,
    pub content_length: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenVsxClientError {
    #[error("The operation was aborted.")]
    Aborted,
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub type OpenVsxClientResult<T> = Result<T, OpenVsxClientError>;

#[async_trait]
pub trait OpenVsxClient: Send + Sync {
    async fn get(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHttpResponse>;

    async fn head(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHeadResponse>;
}

pub struct ReqwestOpenVsxClient {
    http: reqwest::Client,
}

impl ReqwestOpenVsxClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for ReqwestOpenVsxClient {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_content_length(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

async fn check_cancel(cancel: &CancellationToken) -> OpenVsxClientResult<()> {
    if cancel.is_cancelled() {
        return Err(OpenVsxClientError::Aborted);
    }
    Ok(())
}

#[async_trait]
impl OpenVsxClient for ReqwestOpenVsxClient {
    async fn get(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHttpResponse> {
        check_cancel(cancel).await?;
        let response = self.http.get(url).send().await?;
        check_cancel(cancel).await?;
        let status = response.status().as_u16();
        let content_length = parse_content_length(response.headers());
        let body = response.bytes().await?.to_vec();
        check_cancel(cancel).await?;
        Ok(OpenVsxHttpResponse {
            status,
            body,
            content_length,
        })
    }

    async fn head(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHeadResponse> {
        check_cancel(cancel).await?;
        let response = self.http.head(url).send().await?;
        check_cancel(cancel).await?;
        Ok(OpenVsxHeadResponse {
            status: response.status().as_u16(),
            content_length: parse_content_length(response.headers()),
        })
    }
}

#[derive(Clone)]
enum FakeBehavior {
    Response(OpenVsxHttpResponse),
    Head(OpenVsxHeadResponse),
    HangUntilCancelled,
}

/// Serves canned HTTP responses for Open VSX URLs. Never performs network I/O.
pub struct FakeOpenVsxClient {
    behaviors: Arc<Mutex<HashMap<String, FakeBehavior>>>,
    fixture_root: Option<PathBuf>,
}

impl FakeOpenVsxClient {
    pub fn new() -> Self {
        Self {
            behaviors: Arc::new(Mutex::new(HashMap::new())),
            fixture_root: None,
        }
    }

    pub fn with_fixture_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.fixture_root = Some(root.into());
        self
    }

    pub fn register_get(self, url: &str, response: OpenVsxHttpResponse) -> Self {
        self.behaviors
            .lock()
            .expect("fake open vsx lock")
            .insert(url.to_string(), FakeBehavior::Response(response));
        self
    }

    pub fn register_head(self, url: &str, response: OpenVsxHeadResponse) -> Self {
        self.behaviors
            .lock()
            .expect("fake open vsx lock")
            .insert(format!("HEAD:{url}"), FakeBehavior::Head(response));
        self
    }

    pub fn register_hang_on_get(self, url: &str) -> Self {
        self.behaviors
            .lock()
            .expect("fake open vsx lock")
            .insert(url.to_string(), FakeBehavior::HangUntilCancelled);
        self
    }

    fn fixture_path_for_url(&self, url: &str) -> Option<PathBuf> {
        let root = self.fixture_root.as_ref()?;
        let parsed = url::Url::parse(url).ok()?;
        if parsed.host_str()? != "open-vsx.org" {
            return None;
        }
        let mut relative = parsed.path().trim_start_matches('/').to_string();
        if let Some(query) = parsed.query() {
            relative.push('@');
            relative.push_str(query);
        }
        relative = relative.replace('/', "__");
        Some(root.join(relative))
    }

    async fn resolve_get(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHttpResponse> {
        let behavior = self
            .behaviors
            .lock()
            .expect("fake open vsx lock")
            .get(url)
            .cloned();
        if let Some(behavior) = behavior {
            return match behavior {
                FakeBehavior::Response(response) => Ok(response),
                FakeBehavior::Head(_) => Err(OpenVsxClientError::Message(format!(
                    "No GET handler registered for {url}"
                ))),
                FakeBehavior::HangUntilCancelled => {
                    cancel.cancelled().await;
                    Err(OpenVsxClientError::Aborted)
                }
            };
        }
        if let Some(path) = self.fixture_path_for_url(url)
            && path.is_file()
        {
            let body = std::fs::read(&path).map_err(|error| {
                OpenVsxClientError::Message(format!(
                    "Failed to read fixture {}: {error}",
                    path.display()
                ))
            })?;
            return Ok(OpenVsxHttpResponse {
                status: 200,
                content_length: Some(body.len() as u64),
                body,
            });
        }
        Err(OpenVsxClientError::Message(format!(
            "FakeOpenVsxClient has no response for {url}"
        )))
    }

    async fn resolve_head(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHeadResponse> {
        let key = format!("HEAD:{url}");
        let behavior = self
            .behaviors
            .lock()
            .expect("fake open vsx lock")
            .get(&key)
            .cloned();
        if let Some(behavior) = behavior {
            return match behavior {
                FakeBehavior::Head(response) => Ok(response),
                FakeBehavior::Response(response) => Ok(OpenVsxHeadResponse {
                    status: response.status,
                    content_length: response.content_length.or(Some(response.body.len() as u64)),
                }),
                FakeBehavior::HangUntilCancelled => {
                    cancel.cancelled().await;
                    Err(OpenVsxClientError::Aborted)
                }
            };
        }
        if let Some(path) = self.fixture_path_for_url(url)
            && path.is_file()
        {
            let len = std::fs::metadata(&path)
                .map_err(|error| {
                    OpenVsxClientError::Message(format!(
                        "Failed to stat fixture {}: {error}",
                        path.display()
                    ))
                })?
                .len();
            return Ok(OpenVsxHeadResponse {
                status: 200,
                content_length: Some(len),
            });
        }
        Err(OpenVsxClientError::Message(format!(
            "FakeOpenVsxClient has no HEAD response for {url}"
        )))
    }
}

impl Default for FakeOpenVsxClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OpenVsxClient for FakeOpenVsxClient {
    async fn get(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHttpResponse> {
        check_cancel(cancel).await?;
        self.resolve_get(url, cancel).await
    }

    async fn head(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> OpenVsxClientResult<OpenVsxHeadResponse> {
        check_cancel(cancel).await?;
        self.resolve_head(url, cancel).await
    }
}

/// Returns the default fixture directory for integration tests.
pub fn default_open_vsx_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/open_vsx")
}
