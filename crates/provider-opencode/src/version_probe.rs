//! Port of `upstream/packages/provider-opencode/src/server/versionProbe.ts`.
//!
//! A spawned binary is classified from `opencode --version`. An external URL is
//! classified from `GET /api/info` (`{version,pid}` JSON) and then
//! `GET /global/health` (`{healthy:true,version}` JSON). Each generation serves
//! HTML with status 200 on the other's path, so only `application/json` counts.
//! A 401 is a bad password and says nothing about the generation.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::auth::basic_authorization;
use crate::error::OpenCodeRuntimeError;
use crate::semver::{parse_generic_cli_version, parse_semver};
use crate::urlutil::{join_server_path, parse_http_base};

pub const OPENCODE_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
pub const OPENCODE_SERVER_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// `1.14.19`. Servers older than this are refused by the v1 health check.
pub const MINIMUM_OPENCODE_VERSION: &str = "1.14.19";

const PROBE_PATHS: [&str; 2] = ["/api/info", "/global/health"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeGeneration {
    V1,
    V2,
}

impl OpenCodeGeneration {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedOpenCode {
    pub generation: OpenCodeGeneration,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct ProbeSettings {
    pub binary_path: String,
    pub server_url: String,
    pub server_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeCommandResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Runs `opencode --version`. `SystemOpenCodeBinary` is the real implementation;
/// tests substitute their own.
pub trait OpenCodeBinary: Send + Sync {
    fn run_version<'a>(
        &'a self,
        binary_path: &'a str,
        environment: Option<&'a BTreeMap<String, String>>,
    ) -> impl Future<Output = Result<OpenCodeCommandResult, OpenCodeRuntimeError>> + Send + 'a;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeHit {
    Version,
    Unauthorized,
    Miss,
}

fn probed(version: Option<&str>) -> Option<ProbedOpenCode> {
    let version = version?;
    let major = parse_semver(version)?.major;
    let generation = if major >= 2 {
        OpenCodeGeneration::V2
    } else {
        OpenCodeGeneration::V1
    };
    Some(ProbedOpenCode {
        generation,
        version: version.to_string(),
    })
}

/// `opencode --version` prints `1.18.32` on 1.x and `opencode v2.0.18` on 2.x.
pub fn classify_opencode_cli_version(output: &str) -> Option<ProbedOpenCode> {
    probed(parse_generic_cli_version(output).as_deref())
}

#[derive(Deserialize)]
struct ApiInfo {
    version: String,
    /// Required on the wire. A body without an integer pid is not `/api/info`.
    #[allow(dead_code)]
    pid: i64,
}

#[derive(Deserialize)]
struct GlobalHealth {
    #[serde(deserialize_with = "deserialize_true")]
    #[allow(dead_code)]
    healthy: bool,
    version: String,
}

fn deserialize_true<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = bool::deserialize(deserializer)?;
    if value {
        Ok(true)
    } else {
        Err(serde::de::Error::custom("healthy must be true"))
    }
}

fn classify_probe_response(
    path: &str,
    status: u16,
    content_type: Option<&str>,
    body: &str,
) -> ProbeHit {
    if status == 401 {
        return ProbeHit::Unauthorized;
    }
    let media_type = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    if status != 200 || media_type.as_deref() != Some("application/json") {
        return ProbeHit::Miss;
    }
    let version = if path.ends_with("/api/info") {
        serde_json::from_str::<ApiInfo>(body)
            .ok()
            .map(|info| info.version)
    } else {
        serde_json::from_str::<GlobalHealth>(body)
            .ok()
            .map(|health| health.version)
    };
    match probed(version.as_deref()) {
        Some(_) => ProbeHit::Version,
        None => ProbeHit::Miss,
    }
}

fn classify_owned(
    path: &str,
    status: u16,
    content_type: Option<&str>,
    body: &str,
) -> Option<ProbedOpenCode> {
    if !matches!(
        classify_probe_response(path, status, content_type, body),
        ProbeHit::Version
    ) {
        return None;
    }
    if path.ends_with("/api/info") {
        let info = serde_json::from_str::<ApiInfo>(body).ok()?;
        probed(Some(&info.version))
    } else {
        let health = serde_json::from_str::<GlobalHealth>(body).ok()?;
        probed(Some(&health.version))
    }
}

pub async fn probe_open_code_runtime(
    http: &reqwest::Client,
    binary: &impl OpenCodeBinary,
    settings: &ProbeSettings,
    environment: Option<&BTreeMap<String, String>>,
) -> Result<ProbedOpenCode, OpenCodeRuntimeError> {
    if !settings.server_url.trim().is_empty() {
        probe_open_code_server(http, &settings.server_url, &settings.server_password).await
    } else {
        probe_open_code_binary(binary, &settings.binary_path, environment).await
    }
}

async fn probe_open_code_binary(
    binary: &impl OpenCodeBinary,
    binary_path: &str,
    environment: Option<&BTreeMap<String, String>>,
) -> Result<ProbedOpenCode, OpenCodeRuntimeError> {
    let ran = tokio::time::timeout(
        OPENCODE_VERSION_PROBE_TIMEOUT,
        binary.run_version(binary_path, environment),
    )
    .await;
    let result = match ran {
        Ok(result) => result?,
        Err(_elapsed) => {
            return Err(OpenCodeRuntimeError::new(
                "probeOpenCodeBinary",
                format!(
                    "OpenCode CLI version probe timed out after {} seconds.",
                    OPENCODE_VERSION_PROBE_TIMEOUT.as_secs()
                ),
            ));
        }
    };
    if let Some(probed) = classify_opencode_cli_version(&result.stdout) {
        return Ok(probed);
    }
    Err(OpenCodeRuntimeError::new(
        "probeOpenCodeBinary",
        format!(
            "Unable to determine OpenCode version from `opencode --version` output. T3 Code requires OpenCode v{MINIMUM_OPENCODE_VERSION} or newer."
        ),
    ))
}

async fn probe_open_code_server(
    http: &reqwest::Client,
    server_url: &str,
    server_password: &str,
) -> Result<ProbedOpenCode, OpenCodeRuntimeError> {
    let base = parse_http_base(server_url, "probeOpenCodeServer")?;
    let authorization = if server_password.is_empty() {
        None
    } else {
        Some(basic_authorization(server_password))
    };
    for path in PROBE_PATHS {
        let url = join_server_path(&base, path);
        let response = probe_one(http, url, authorization.as_deref(), path).await?;
        match response {
            ServerProbe::Unauthorized => {
                return Err(OpenCodeRuntimeError::new(
                    "probeOpenCodeServer",
                    "401 Unauthorized: the OpenCode server rejected the password.",
                ));
            }
            ServerProbe::Found(probed) => return Ok(probed),
            ServerProbe::Miss => {}
        }
    }
    Err(OpenCodeRuntimeError::new(
        "probeOpenCodeServer",
        format!(
            "The server did not identify itself as OpenCode. T3 Code requires OpenCode v{MINIMUM_OPENCODE_VERSION} or newer."
        ),
    ))
}

enum ServerProbe {
    Found(ProbedOpenCode),
    Unauthorized,
    Miss,
}

async fn probe_one(
    http: &reqwest::Client,
    url: reqwest::Url,
    authorization: Option<&str>,
    path: &str,
) -> Result<ServerProbe, OpenCodeRuntimeError> {
    let request = async {
        let mut builder = http.get(url);
        if let Some(authorization) = authorization {
            builder = builder.header("authorization", authorization);
        }
        let response = builder
            .send()
            .await
            .map_err(|err| crate::http_client::unreachable("probeOpenCodeServer", err))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .text()
            .await
            .map_err(|err| crate::http_client::unreachable("probeOpenCodeServer", err))?;
        Ok::<_, OpenCodeRuntimeError>((status, content_type, body))
    };
    let (status, content_type, body) =
        match tokio::time::timeout(OPENCODE_SERVER_PROBE_TIMEOUT, request).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(OpenCodeRuntimeError::new(
                    "probeOpenCodeServer",
                    "Timed out while checking the OpenCode server version.",
                ));
            }
        };
    match classify_probe_response(path, status, content_type.as_deref(), &body) {
        ProbeHit::Unauthorized => Ok(ServerProbe::Unauthorized),
        ProbeHit::Miss => Ok(ServerProbe::Miss),
        ProbeHit::Version => Ok(ServerProbe::Found(
            classify_owned(path, status, content_type.as_deref(), &body).ok_or_else(|| {
                OpenCodeRuntimeError::new(
                    "probeOpenCodeServer",
                    format!(
                        "The server did not identify itself as OpenCode. T3 Code requires OpenCode v{MINIMUM_OPENCODE_VERSION} or newer."
                    ),
                )
            })?,
        )),
    }
}

/// One instance's runtime, remembered after the first successful probe.
/// A failed probe is never stored. `last_success` never probes and never waits
/// on a probe in flight: upstream `Cache.getSuccess` is non-blocking, and this
/// is read on hot paths.
pub struct OpenCodeRuntimeProbe<P> {
    /// Single-flights `get` and `refresh`. Held across the probe, never by `last_success`.
    flight: Mutex<()>,
    success: parking_lot::Mutex<Option<ProbedOpenCode>>,
    probe: P,
}

impl<P, Fut> OpenCodeRuntimeProbe<P>
where
    P: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<ProbedOpenCode, OpenCodeRuntimeError>> + Send,
{
    pub fn new(probe: P) -> Self {
        Self {
            flight: Mutex::new(()),
            success: parking_lot::Mutex::new(None),
            probe,
        }
    }

    pub async fn get(&self) -> Result<ProbedOpenCode, OpenCodeRuntimeError> {
        let _flight = self.flight.lock().await;
        if let Some(success) = self.success.lock().clone() {
            return Ok(success);
        }
        self.probe_and_store().await
    }

    pub async fn refresh(&self) -> Result<ProbedOpenCode, OpenCodeRuntimeError> {
        let _flight = self.flight.lock().await;
        self.probe_and_store().await
    }

    async fn probe_and_store(&self) -> Result<ProbedOpenCode, OpenCodeRuntimeError> {
        let probed = (self.probe)().await?;
        *self.success.lock() = Some(probed.clone());
        Ok(probed)
    }

    pub fn last_success(&self) -> Option<ProbedOpenCode> {
        self.success.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::basic_authorization;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn classifies_recorded_cli_output() {
        assert_eq!(
            classify_opencode_cli_version("1.18.32\n"),
            Some(ProbedOpenCode {
                generation: OpenCodeGeneration::V1,
                version: "1.18.32".to_string(),
            })
        );
        assert_eq!(
            classify_opencode_cli_version("opencode v2.0.18\n"),
            Some(ProbedOpenCode {
                generation: OpenCodeGeneration::V2,
                version: "2.0.18".to_string(),
            })
        );
        assert_eq!(classify_opencode_cli_version("opencode dev build\n"), None);
    }

    struct PanicBinary;

    impl OpenCodeBinary for PanicBinary {
        async fn run_version(
            &self,
            _binary_path: &str,
            _environment: Option<&BTreeMap<String, String>>,
        ) -> Result<OpenCodeCommandResult, OpenCodeRuntimeError> {
            panic!("a configured server must not run the local binary");
        }
    }

    fn settings(server_url: &str, password: &str) -> ProbeSettings {
        ProbeSettings {
            binary_path: "opencode".to_string(),
            server_url: server_url.to_string(),
            server_password: password.to_string(),
        }
    }

    async fn serve(
        handler: impl Fn(Recorded) -> FakeResponse + Send + Sync + 'static,
    ) -> FakeServer {
        FakeServer::spawn(handler).await
    }

    #[tokio::test]
    async fn finds_v2_at_api_info_without_falling_through() {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&paths);
        let server = serve(move |request| replay(&recorded, &request, V2, "pw")).await;
        let http = reqwest::Client::new();
        let probed = probe_open_code_runtime(
            &http,
            &PanicBinary,
            &settings(&format!("{}/", server.url), "pw"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            probed,
            ProbedOpenCode {
                generation: OpenCodeGeneration::V2,
                version: "2.0.18".to_string(),
            }
        );
        assert_eq!(paths.lock().await.as_slice(), ["/api/info"]);
    }

    #[tokio::test]
    async fn skips_html_at_api_info_and_reads_global_health() {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&paths);
        let server = serve(move |request| replay(&recorded, &request, V1, "pw")).await;
        let http = reqwest::Client::new();
        let probed =
            probe_open_code_runtime(&http, &PanicBinary, &settings(&server.url, "pw"), None)
                .await
                .unwrap();
        assert_eq!(
            probed,
            ProbedOpenCode {
                generation: OpenCodeGeneration::V1,
                version: "1.18.32".to_string(),
            }
        );
        assert_eq!(
            paths.lock().await.as_slice(),
            ["/api/info", "/global/health"]
        );
    }

    #[tokio::test]
    async fn keeps_a_server_urls_path_prefix_and_query() {
        let urls = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&urls);
        let server = serve(move |request| {
            recorded.try_lock().unwrap().push(request.target.clone());
            let path = request
                .target
                .split('?')
                .next()
                .unwrap()
                .trim_start_matches("/opencode");
            replay_path(path, &request, V1, "pw")
        })
        .await;
        let http = reqwest::Client::new();
        let probed = probe_open_code_runtime(
            &http,
            &PanicBinary,
            &settings(&format!("{}/opencode/?route=oc", server.url), "pw"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(probed.generation, OpenCodeGeneration::V1);
        assert_eq!(
            urls.lock().await.as_slice(),
            [
                "/opencode/api/info?route=oc",
                "/opencode/global/health?route=oc"
            ]
        );
    }

    #[tokio::test]
    async fn reports_a_rejected_password_instead_of_guessing() {
        for generation in [V1, V2] {
            let paths = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&paths);
            let server = serve(move |request| replay(&recorded, &request, generation, "pw")).await;
            let http = reqwest::Client::new();
            let err =
                probe_open_code_runtime(&http, &PanicBinary, &settings(&server.url, "wrong"), None)
                    .await
                    .unwrap_err();
            assert!(err.detail.contains("401 Unauthorized"), "{}", err.detail);
            assert_eq!(paths.lock().await.as_slice(), ["/api/info"]);
        }
    }

    #[tokio::test]
    async fn sends_a_non_ascii_password_as_utf8() {
        for (generation, password, expected) in [
            (V1, "pässwörd", OpenCodeGeneration::V1),
            (V2, "pass€word", OpenCodeGeneration::V2),
        ] {
            let server = serve({
                let password = password.to_string();
                move |request| replay_auth(&request, generation, &password)
            })
            .await;
            let http = reqwest::Client::new();
            let probed = probe_open_code_runtime(
                &http,
                &PanicBinary,
                &settings(&server.url, password),
                None,
            )
            .await
            .unwrap();
            assert_eq!(probed.generation, expected);
        }
    }

    #[tokio::test]
    async fn does_not_take_other_json_at_api_info_for_opencode_2() {
        let server = serve(|request| {
            let path = request.target.split('?').next().unwrap();
            if path == "/api/info" {
                return FakeResponse::json(200, r#"{"version":"3.4.1"}"#);
            }
            FakeResponse::html()
        })
        .await;
        let http = reqwest::Client::new();
        let err = probe_open_code_runtime(&http, &PanicBinary, &settings(&server.url, "pw"), None)
            .await
            .unwrap_err();
        assert!(
            err.detail.contains("did not identify itself as OpenCode"),
            "{}",
            err.detail
        );
    }

    #[tokio::test]
    async fn never_puts_the_url_or_credentials_in_an_error_detail() {
        let secret = "userinfo-secret";
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let cases = [
            format!("127.0.0.1:4096?token={secret}"),
            format!("localhost:4096/{secret}"),
            format!("http://user:{secret}@127.0.0.1:1/?token={secret}"),
        ];
        for server_url in cases {
            let err =
                probe_open_code_runtime(&http, &PanicBinary, &settings(&server_url, secret), None)
                    .await
                    .unwrap_err();
            assert!(
                !err.detail.contains(secret),
                "{} -> {}",
                server_url,
                err.detail
            );
            assert!(
                !err.detail.contains("127.0.0.1"),
                "{} -> {}",
                server_url,
                err.detail
            );
        }
    }

    #[tokio::test]
    async fn remembers_success_and_refreshes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let outputs = Arc::new(Mutex::new(vec![
            Err(OpenCodeRuntimeError::new("spawn", "ENOENT")),
            Ok(ProbedOpenCode {
                generation: OpenCodeGeneration::V2,
                version: "2.0.18".to_string(),
            }),
            Ok(ProbedOpenCode {
                generation: OpenCodeGeneration::V1,
                version: "1.18.32".to_string(),
            }),
        ]));
        let calls_probe = Arc::clone(&calls);
        let outputs_probe = Arc::clone(&outputs);
        let probe = OpenCodeRuntimeProbe::new(move || {
            let calls_probe = Arc::clone(&calls_probe);
            let outputs_probe = Arc::clone(&outputs_probe);
            async move {
                calls_probe.fetch_add(1, Ordering::SeqCst);
                outputs_probe.lock().await.remove(0)
            }
        });
        assert!(probe.get().await.is_err());
        assert!(probe.last_success().is_none());
        assert_eq!(
            probe.get().await.unwrap().generation,
            OpenCodeGeneration::V2
        );
        assert_eq!(
            probe.get().await.unwrap().generation,
            OpenCodeGeneration::V2
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            probe.refresh().await.unwrap().generation,
            OpenCodeGeneration::V1
        );
        assert_eq!(
            probe.get().await.unwrap().generation,
            OpenCodeGeneration::V1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn last_success_does_not_wait_for_a_probe_in_flight() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_probe = Arc::clone(&calls);
        let probe = Arc::new(OpenCodeRuntimeProbe::new(move || {
            let calls_probe = Arc::clone(&calls_probe);
            async move {
                calls_probe.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(800)).await;
                Ok(ProbedOpenCode {
                    generation: OpenCodeGeneration::V1,
                    version: "1.19.0".to_string(),
                })
            }
        }));
        let refreshing = {
            let probe = Arc::clone(&probe);
            tokio::spawn(async move { probe.refresh().await })
        };
        // Let the refresh take the single-flight lock and start probing.
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let started = std::time::Instant::now();
        assert!(probe.last_success().is_none());
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "last_success waited {:?} behind the probe",
            started.elapsed()
        );
        refreshing.await.unwrap().unwrap();
        assert_eq!(probe.last_success().unwrap().version, "1.19.0");
    }

    #[derive(Clone, Copy)]
    struct Generation {
        info_json: bool,
        version: &'static str,
    }

    const V1: Generation = Generation {
        info_json: false,
        version: "1.18.32",
    };
    const V2: Generation = Generation {
        info_json: true,
        version: "2.0.18",
    };

    const SPA_HTML: &str = "<!doctype html>\n<html lang=\"en\">\n";

    fn replay(
        paths: &Arc<Mutex<Vec<String>>>,
        request: &Recorded,
        generation: Generation,
        password: &str,
    ) -> FakeResponse {
        let path = request.target.split('?').next().unwrap().to_string();
        paths.try_lock().unwrap().push(path.clone());
        replay_path(&path, request, generation, password)
    }

    fn replay_auth(request: &Recorded, generation: Generation, password: &str) -> FakeResponse {
        let path = request.target.split('?').next().unwrap();
        replay_path(path, request, generation, password)
    }

    fn replay_path(
        path: &str,
        request: &Recorded,
        generation: Generation,
        password: &str,
    ) -> FakeResponse {
        let expected = basic_authorization(password);
        if request.authorization.as_deref() != Some(expected.as_str()) {
            return unauthorized(generation);
        }
        if path.ends_with("/api/info") {
            if generation.info_json {
                return FakeResponse::json(
                    200,
                    &format!(
                        r#"{{"version":"{}","pid":2393556,"urls":["http://127.0.0.1:48771"],"paths":{{"tmp":"/tmp/opencode"}}}}"#,
                        generation.version
                    ),
                );
            }
            return FakeResponse::html();
        }
        if path.ends_with("/global/health") {
            if generation.info_json {
                return FakeResponse::html();
            }
            return FakeResponse::json(
                200,
                &format!(r#"{{"healthy":true,"version":"{}"}}"#, generation.version),
            );
        }
        unauthorized(generation)
    }

    fn unauthorized(generation: Generation) -> FakeResponse {
        if generation.info_json {
            FakeResponse::json(
                401,
                r#"{"_tag":"UnauthorizedError","message":"Authentication required"}"#,
            )
        } else {
            FakeResponse {
                status: 401,
                content_type: None,
                body: Vec::new(),
                hold: false,
            }
        }
    }

    struct Recorded {
        target: String,
        authorization: Option<String>,
    }

    struct FakeResponse {
        status: u16,
        content_type: Option<&'static str>,
        body: Vec<u8>,
        hold: bool,
    }

    impl FakeResponse {
        fn json(status: u16, body: &str) -> Self {
            Self {
                status,
                content_type: Some("application/json"),
                body: body.as_bytes().to_vec(),
                hold: false,
            }
        }

        fn html() -> Self {
            Self {
                status: 200,
                content_type: Some("text/html"),
                body: SPA_HTML.as_bytes().to_vec(),
                hold: false,
            }
        }
    }

    struct FakeServer {
        url: String,
        shutdown: tokio::sync::watch::Sender<bool>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            let _ = self.shutdown.send(true);
            self.task.abort();
        }
    }

    impl FakeServer {
        async fn spawn(handler: impl Fn(Recorded) -> FakeResponse + Send + Sync + 'static) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (shutdown, mut watch) = tokio::sync::watch::channel(false);
            let handler = Arc::new(handler);
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = watch.changed() => break,
                        accepted = listener.accept() => {
                            let Ok((socket, _)) = accepted else { break };
                            let handler = Arc::clone(&handler);
                            tokio::spawn(async move {
                                let _ = handle_conn(socket, handler).await;
                            });
                        }
                    }
                }
            });
            Self {
                url: format!("http://127.0.0.1:{port}"),
                shutdown,
                task,
            }
        }
    }

    async fn handle_conn(
        mut socket: tokio::net::TcpStream,
        handler: Arc<dyn Fn(Recorded) -> FakeResponse + Send + Sync>,
    ) -> std::io::Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
            if buf.len() > 64 * 1024 {
                return Ok(());
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let _method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("/").to_string();
        let mut authorization = None;
        let mut content_length = 0usize;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_string());
            }
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        while buf.len() - header_end < content_length {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let response = handler(Recorded {
            target,
            authorization,
        });
        let mut head = format!("HTTP/1.1 {} OK\r\n", response.status);
        if response.status == 401 {
            head = "HTTP/1.1 401 Unauthorized\r\n".to_string();
        }
        if let Some(content_type) = response.content_type {
            head.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            response.body.len()
        ));
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(&response.body).await?;
        if response.hold {
            let _ = socket.read(&mut tmp).await;
        }
        Ok(())
    }
}
