//! Port of `upstream/packages/provider-opencode/src/server/v2/OpenCode2Server.ts`.
//!
//! A spawned 2.x server gets a generated `OPENCODE_PASSWORD`. The inherited
//! `OPENCODE_SERVER_PASSWORD` is removed so OpenCode 2 cannot prefer it.
//! Readiness is `GET /api/info`, not `/global/health`.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::sync::Mutex;

use crate::error::OpenCodeRuntimeError;
use crate::runtime::VerifyFn;
use crate::server_owner::{OpenCodeServerOwner, OwnerConfig};
use crate::v2_client::V2Client;

pub fn generate_password() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("os random");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn server_environment(
    environment: &BTreeMap<String, String>,
    password: &str,
) -> BTreeMap<String, String> {
    let mut environment = environment.clone();
    environment.remove("OPENCODE_SERVER_PASSWORD");
    environment.insert("OPENCODE_PASSWORD".to_string(), password.to_string());
    environment
}

pub struct OpenCode2Config {
    pub binary_path: String,
    pub server_url: String,
    pub server_password: String,
    pub directory: String,
    pub environment: BTreeMap<String, String>,
    pub idle: Option<Duration>,
    pub shutdown_grace: Option<Duration>,
}

#[derive(Clone)]
pub struct OpenCode2Connection {
    pub url: String,
    pub version: String,
    pub external: bool,
    pub client: V2Client,
}

enum Mode {
    External {
        url: String,
        password: String,
        cached: Box<Mutex<Option<OpenCode2Connection>>>,
    },
    Spawned {
        owner: OpenCodeServerOwner,
        cached: Arc<Mutex<Option<OpenCode2Connection>>>,
    },
}

pub struct OpenCode2Server {
    mode: Mode,
}

impl OpenCode2Server {
    pub fn start(config: OpenCode2Config) -> Result<Self, OpenCodeRuntimeError> {
        let server_url = config.server_url.trim().to_string();
        if !server_url.is_empty() {
            return Ok(Self {
                mode: Mode::External {
                    url: server_url,
                    password: config.server_password,
                    cached: Box::new(Mutex::new(None)),
                },
            });
        }
        let password = generate_password();
        let cached = Arc::new(Mutex::new(None));
        let verify = spawned_verify(password.clone(), Arc::clone(&cached));
        let owner = OpenCodeServerOwner::new(OwnerConfig {
            binary_path: config.binary_path,
            directory: config.directory,
            server_password: None,
            environment: Some(server_environment(&config.environment, &password)),
            idle: config.idle,
            shutdown_grace: config.shutdown_grace,
            verify: Some(verify),
        });
        Ok(Self {
            mode: Mode::Spawned { owner, cached },
        })
    }

    pub async fn close(&self) {
        if let Mode::Spawned { owner, .. } = &self.mode {
            owner.close().await;
        }
    }

    pub async fn with_connection<T, E, F, Fut>(&self, use_connection: F) -> Result<T, E>
    where
        E: From<OpenCodeRuntimeError>,
        F: FnOnce(OpenCode2Connection) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let connection = match &self.mode {
            Mode::External {
                url,
                password,
                cached,
            } => {
                let mut slot = cached.lock().await;
                if slot.is_none() {
                    let client = V2Client::connect(url, password)?;
                    let version = verify_server(&client).await?;
                    *slot = Some(OpenCode2Connection {
                        url: url.clone(),
                        version,
                        external: true,
                        client,
                    });
                }
                slot.as_ref()
                    .expect("cached after a successful verify")
                    .clone()
            }
            Mode::Spawned { owner, cached } => {
                let cached = Arc::clone(cached);
                owner
                    .with_server(move |server| {
                        let cached = Arc::clone(&cached);
                        async move {
                            let connection = cached.lock().await.clone().ok_or_else(|| {
                                OpenCodeRuntimeError::new(
                                    "OpenCode2Server.withConnection",
                                    "OpenCode 2 server was lent before verification.",
                                )
                            })?;
                            if connection.url != server.url {
                                return Err(OpenCodeRuntimeError::new(
                                    "OpenCode2Server.withConnection",
                                    "OpenCode 2 server was lent before verification.",
                                ));
                            }
                            Ok(connection)
                        }
                    })
                    .await?
            }
        };
        use_connection(connection).await
    }
}

fn spawned_verify(password: String, cached: Arc<Mutex<Option<OpenCode2Connection>>>) -> VerifyFn {
    Arc::new(move |input| {
        let password = password.clone();
        let cached = Arc::clone(&cached);
        Box::pin(async move {
            let client = V2Client::connect(&input.url, &password)?;
            let version = verify_server(&client).await?;
            *cached.lock().await = Some(OpenCode2Connection {
                url: input.url,
                version: version.clone(),
                external: false,
                client,
            });
            Ok(version)
        })
    })
}

pub async fn verify_server(client: &V2Client) -> Result<String, OpenCodeRuntimeError> {
    match client.server_info().await {
        Ok(info) => Ok(info.version),
        Err(err) => Err(describe_info_failure(err)),
    }
}

fn describe_info_failure(err: OpenCodeRuntimeError) -> OpenCodeRuntimeError {
    if err.detail == "Timed out while waiting for the OpenCode server." {
        return OpenCodeRuntimeError::new(
            "server.info",
            "Timed out waiting for the OpenCode server.",
        )
        .with_cause(err);
    }
    if err.detail == "Couldn't reach the OpenCode server." {
        return OpenCodeRuntimeError::new("server.info", "Could not reach the OpenCode server.")
            .with_cause(err);
    }
    if let Some(status) = status_of(&err.detail) {
        if status == 401 {
            return OpenCodeRuntimeError::new(
                "server.info",
                "The OpenCode server rejected the server password.",
            )
            .with_cause(err);
        }
        if !(200..300).contains(&status) {
            return OpenCodeRuntimeError::new(
                "server.info",
                format!("The OpenCode server returned HTTP {status}."),
            )
            .with_cause(err);
        }
    }
    OpenCodeRuntimeError::new("server.info", "The server is not an OpenCode 2 server.")
        .with_cause(err)
}

fn status_of(detail: &str) -> Option<u16> {
    let rest = detail.strip_prefix("status=")?;
    let number = rest.split_whitespace().next()?;
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::basic_authorization;
    use crate::test_support::{FakeResponse, FakeServer};

    #[test]
    fn spawned_environment_drops_the_v1_password() {
        let mut env = BTreeMap::new();
        env.insert("OPENCODE_SERVER_PASSWORD".into(), "old".into());
        env.insert("HOME".into(), "/tmp".into());
        let next = server_environment(&env, "generated");
        assert!(!next.contains_key("OPENCODE_SERVER_PASSWORD"));
        assert_eq!(
            next.get("OPENCODE_PASSWORD").map(String::as_str),
            Some("generated")
        );
        assert_eq!(next.get("HOME").map(String::as_str), Some("/tmp"));
        let password = generate_password();
        assert!(!password.is_empty());
        assert!(!password.contains('+') && !password.contains('/') && !password.contains('='));
    }

    #[tokio::test]
    async fn external_server_caches_a_successful_info_probe() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits_handler = Arc::clone(&hits);
        let server = FakeServer::spawn(move |request| {
            hits_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(request.authorization.as_deref(), Some(basic_authorization("pw").as_str()));
            FakeResponse::json(
                200,
                r#"{"version":"2.0.18","pid":3,"urls":["http://127.0.0.1:1"],"paths":{"tmp":"/tmp"}}"#,
            )
        })
        .await;
        let opencode = OpenCode2Server::start(OpenCode2Config {
            binary_path: "opencode".into(),
            server_url: server.url.clone(),
            server_password: "pw".into(),
            directory: "/work".into(),
            environment: BTreeMap::new(),
            idle: None,
            shutdown_grace: None,
        })
        .unwrap();
        let version = opencode
            .with_connection(|connection| async move {
                Ok::<_, OpenCodeRuntimeError>(connection.version)
            })
            .await
            .unwrap();
        let again = opencode
            .with_connection(|connection| async move {
                Ok::<_, OpenCodeRuntimeError>(connection.version)
            })
            .await
            .unwrap();
        assert_eq!(version, "2.0.18");
        assert_eq!(again, version);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_rejected_password_is_not_cached() {
        let server =
            FakeServer::spawn(|_| FakeResponse::json(401, r#"{"name":"UnauthorizedError"}"#)).await;
        let opencode = OpenCode2Server::start(OpenCode2Config {
            binary_path: "opencode".into(),
            server_url: server.url.clone(),
            server_password: "nope".into(),
            directory: "/work".into(),
            environment: BTreeMap::new(),
            idle: None,
            shutdown_grace: None,
        })
        .unwrap();
        let err = opencode
            .with_connection(|_connection| async { Ok::<(), OpenCodeRuntimeError>(()) })
            .await
            .unwrap_err();
        assert_eq!(
            err.detail,
            "The OpenCode server rejected the server password."
        );
    }

    #[tokio::test]
    async fn html_is_not_an_opencode_2_server() {
        let server = FakeServer::spawn(|_| FakeResponse {
            status: 200,
            content_type: Some("text/html".to_string()),
            body: b"<!doctype html>".to_vec(),
            stream: None,
            hold: false,
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        let err = verify_server(&client).await.unwrap_err();
        assert_eq!(err.detail, "The server is not an OpenCode 2 server.");
    }
}
