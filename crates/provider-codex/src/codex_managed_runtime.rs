//! Port of upstream/apps/server/src/provider/CodexManagedRuntime.ts and
//! upstream/apps/server/src/provider/CodexManagedHome.ts.
//!
//! A managed Codex instance signs in with ChatGPT, keeps the tokens in T3's
//! credential store, and never writes native `auth.json`. `resolve` turns the
//! signed-in token, the installed executable, and the home layout into the
//! settings and environment one session launches with.
//!
//! The server config, secret store, and environment identity enter as plain
//! values on [`CodexManagedHost`]. The installation enters as a trait so tests
//! hand in a fake executable.

#![allow(clippy::result_large_err)]

use std::{any::Any, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use serde_json::json;
use turin_contracts::{
    provider_instance::ProviderInstanceId, provider_setup::ProviderSetupError,
    settings::CodexSettings,
};
use turin_provider_core::{credential_store::SecretStore, host_process::ProcessEnv};
use turin_shared::cli_args::tokenize_cli_args;

use crate::{
    codex_chatgpt_auth::{CodexChatGptAuth, CodexChatGptAuthOptions, make, store::failure},
    codex_home_layout::{
        CodexHomeLayout, managed_codex_launch_args, managed_runtime_environment,
        materialize_codex_shadow_home, resolve_managed_codex_home_layout,
    },
    codex_installation::{CodexExecutable, CodexInstallation, CodexInstallationError},
    runtime::{CodexRuntimeConfig, CodexRuntimeResolver, ResolvedCodexLaunch},
};

/// What one session launches with. `revision` changes whenever the access
/// token does, so callers can tell a stale launch from a current one.
#[derive(Debug, Clone)]
pub struct CodexEffectiveRuntime {
    pub config: CodexSettings,
    pub environment: ProcessEnv,
    pub revision: String,
}

/// A held executable. Dropping it releases the installation for removal.
pub type CodexExecutableLease = Box<dyn Any + Send + Sync>;

/// The slice of `CodexInstallation` the managed runtime uses.
#[async_trait]
pub trait CodexExecutableSource: Send + Sync {
    async fn resolve(&self) -> Result<CodexExecutable, CodexInstallationError>;
    async fn acquire(
        &self,
    ) -> Result<(CodexExecutable, CodexExecutableLease), CodexInstallationError>;
}

#[async_trait]
impl CodexExecutableSource for CodexInstallation {
    async fn resolve(&self) -> Result<CodexExecutable, CodexInstallationError> {
        CodexInstallation::resolve(self).await
    }

    async fn acquire(
        &self,
    ) -> Result<(CodexExecutable, CodexExecutableLease), CodexInstallationError> {
        let lease = CodexInstallation::acquire(self).await?;
        Ok(((*lease).clone(), Box::new(lease)))
    }
}

/// `ServerConfig`, `ServerSecretStore`, and `ServerEnvironmentIdentity` as values.
#[derive(Clone)]
pub struct CodexManagedHost {
    /// `config.stateDir`.
    pub state_dir: PathBuf,
    /// The host home directory, for the default `~/.codex`.
    pub user_home: String,
    /// `config.devUrl ?? http://localhost:<port>`. Sign-in returns to `/welcome` here.
    pub server_url: String,
    pub secrets: Arc<dyn SecretStore>,
    /// `None` for an in-memory store: the session file lock is skipped.
    pub secrets_directory: Option<PathBuf>,
    pub environment_id: String,
    pub installation: Arc<dyn CodexExecutableSource>,
    /// Overrides OpenAI's discovery document. Tests point it at a mock server.
    pub discovery_url: Option<String>,
    /// Overrides OpenAI's model catalog URL. Tests point it at a mock server.
    pub catalog_url: Option<String>,
}

pub struct CodexManagedRuntimeOptions {
    pub instance_id: ProviderInstanceId,
    pub enabled: bool,
    pub environment: ProcessEnv,
    pub config: CodexSettings,
    pub host: CodexManagedHost,
}

/// Names `managed_runtime_environment` takes out of the environment. A spawn
/// must remove them from the inherited process environment as well.
pub const MANAGED_REMOVED_ENVIRONMENT: [&str; 3] = [
    "T3CODE_CODEX_LAUNCH_ARGS",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
];

/// The app-server launch for an effective runtime. `base` supplies the cwd and
/// client version, which managed settings do not carry.
pub fn managed_launch_config(
    base: &CodexRuntimeConfig,
    effective: &CodexEffectiveRuntime,
) -> CodexRuntimeConfig {
    CodexRuntimeConfig {
        binary: PathBuf::from(effective.config.binary_path.as_str()),
        cwd: base.cwd.clone(),
        home_path: Some(effective.config.home_path.as_str().to_owned()),
        launch_args: tokenize_cli_args(Some(effective.config.launch_args.as_str())),
        environment: effective.environment.clone().into_iter().collect(),
        remove_environment: MANAGED_REMOVED_ENVIRONMENT
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        client_version: base.client_version.clone(),
    }
}

/// An effective runtime plus the executable lease it launched from. Keep the
/// lease for as long as the process runs.
pub struct ResolvedCodexRuntime {
    pub effective: CodexEffectiveRuntime,
    pub lease: CodexExecutableLease,
}

pub struct CodexManagedRuntime {
    instance_id: ProviderInstanceId,
    enabled: bool,
    environment: ProcessEnv,
    pub auth: CodexChatGptAuth,
    pub installation: Arc<dyn CodexExecutableSource>,
    pub catalog_url: Option<String>,
    /// `homeLayout.effectiveHomePath ?? homeLayout.sharedHomePath`.
    pub home_path: PathBuf,
    pub home_layout: CodexHomeLayout,
}

/// `resolveManagedCodexHomeLayout` (`CodexManagedHome.ts`). Non-default
/// instances without a configured shadow home get their own under the state dir.
pub fn resolve_managed_codex_home(
    state_dir: &std::path::Path,
    instance_id: &ProviderInstanceId,
    config: &CodexSettings,
    user_home: &str,
) -> CodexHomeLayout {
    resolve_managed_codex_home_layout(
        state_dir,
        instance_id.as_str(),
        config.home_path.as_str(),
        config.shadow_home_path.as_str(),
        user_home,
    )
}

fn server_url_to_welcome(server_url: &str) -> Option<String> {
    let base = url::Url::parse(server_url).ok()?;
    base.join("/welcome").ok().map(|url| url.to_string())
}

impl CodexManagedRuntime {
    /// `makeCodexManagedRuntime`.
    pub async fn make(options: CodexManagedRuntimeOptions) -> Result<Self, ProviderSetupError> {
        let host = options.host;
        let auth = make(CodexChatGptAuthOptions {
            instance_id: options.instance_id.clone(),
            secrets: Arc::clone(&host.secrets),
            secrets_directory: host.secrets_directory.clone(),
            environment_id: host.environment_id.clone(),
            discovery_url: host.discovery_url.clone(),
            resource: None,
            default_return_url: server_url_to_welcome(&host.server_url),
            reconnect_profile: None,
        })
        .await?;
        let home_layout = resolve_managed_codex_home(
            &host.state_dir,
            &options.instance_id,
            &options.config,
            &host.user_home,
        );
        let home_path = home_layout
            .effective_home_path
            .clone()
            .unwrap_or_else(|| home_layout.shared_home_path.clone());
        Ok(Self {
            instance_id: options.instance_id,
            enabled: options.enabled,
            environment: options.environment,
            auth,
            installation: host.installation,
            catalog_url: host.catalog_url,
            home_path,
            home_layout,
        })
    }

    fn setup_error(&self, operation: &str, detail: &str) -> ProviderSetupError {
        failure(&self.instance_id, operation, detail)
    }

    /// `resolve`: acquire the executable, read the signed-in token (refreshing
    /// it when due), materialize the shadow home, and build the launch.
    pub async fn resolve(&self) -> Result<ResolvedCodexRuntime, ProviderSetupError> {
        let (executable, lease) = self.installation.acquire().await.map_err(|_| {
            self.setup_error("install", "Set up managed Codex before starting a session.")
        })?;
        let credentials = self.auth.access().await?;
        let layout = self.home_layout.clone();
        let materialized =
            tokio::task::spawn_blocking(move || materialize_codex_shadow_home(&layout)).await;
        match materialized {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(self.setup_error("runtime", &error.to_string())),
            Err(_) => {
                return Err(
                    self.setup_error("runtime", "Could not prepare the managed Codex runtime.")
                );
            }
        }
        tokio::fs::create_dir_all(&self.home_path)
            .await
            .map_err(|_| {
                self.setup_error("runtime", "Could not prepare the managed Codex runtime.")
            })?;
        let home_path = self.home_path.to_string_lossy().into_owned();
        // Ambient CLI overrides cannot redirect a T3-owned token to a different provider.
        let environment =
            managed_runtime_environment(&self.environment, &credentials.access_token, &home_path);
        let config = serde_json::from_value::<CodexSettings>(json!({
            "enabled": self.enabled,
            "setupMode": "managed",
            "binaryPath": executable.executable_path.to_string_lossy(),
            "homePath": home_path,
            "launchArgs": managed_codex_launch_args(),
        }))
        .map_err(|error| self.setup_error("runtime", &error.to_string()))?;
        Ok(ResolvedCodexRuntime {
            effective: CodexEffectiveRuntime {
                config,
                environment,
                revision: credentials.access_token,
            },
            lease,
        })
    }
}

#[async_trait]
impl CodexRuntimeResolver for CodexManagedRuntime {
    async fn resolve(
        &self,
        base: &CodexRuntimeConfig,
    ) -> Result<ResolvedCodexLaunch, ProviderSetupError> {
        let resolved = CodexManagedRuntime::resolve(self).await?;
        Ok(ResolvedCodexLaunch {
            config: managed_launch_config(base, &resolved.effective),
            keepalive: resolved.lease,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use parking_lot::Mutex;
    use turin_contracts::provider_setup::ProviderInstallSource;
    use turin_provider_core::credential_store::{MemorySecretStore, ProviderCredentials};

    use super::*;
    use crate::{
        codex_chatgpt_auth::mini_http, codex_home_layout::CodexHomeMode,
        codex_launch_args::codex_app_server_args,
    };

    fn settings(value: serde_json::Value) -> CodexSettings {
        serde_json::from_value(value).unwrap()
    }

    fn instance(id: &str) -> ProviderInstanceId {
        ProviderInstanceId::new(id).unwrap()
    }

    #[test]
    fn managed_home_defaults_to_the_global_codex_home_and_honors_configured_home_paths() {
        let state = std::path::Path::new("/t3-state");
        let primary =
            resolve_managed_codex_home(state, &instance("codex"), &settings(json!({})), "/home/u");
        assert_eq!(primary.shared_home_path, PathBuf::from("/home/u/.codex"));
        assert_eq!(primary.mode, CodexHomeMode::Direct);
        let additional = resolve_managed_codex_home(
            state,
            &instance("codex-work"),
            &settings(json!({})),
            "/home/u",
        );
        assert_eq!(additional.shared_home_path, primary.shared_home_path);
        assert_eq!(additional.mode, CodexHomeMode::AuthOverlay);
        let configured = resolve_managed_codex_home(
            state,
            &instance("codex-work"),
            &settings(json!({"homePath": "/custom/shared", "shadowHomePath": "/custom/shadow"})),
            "/home/u",
        );
        assert_eq!(configured.shared_home_path, PathBuf::from("/custom/shared"));
        assert_eq!(
            configured.effective_home_path,
            Some(PathBuf::from("/custom/shadow"))
        );
    }

    struct Lease(Arc<AtomicUsize>);

    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct FakeInstallation {
        executable: CodexExecutable,
        leases: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl CodexExecutableSource for FakeInstallation {
        async fn resolve(&self) -> Result<CodexExecutable, CodexInstallationError> {
            Ok(self.executable.clone())
        }

        async fn acquire(
            &self,
        ) -> Result<(CodexExecutable, CodexExecutableLease), CodexInstallationError> {
            self.leases.fetch_add(1, Ordering::SeqCst);
            Ok((
                self.executable.clone(),
                Box::new(Lease(Arc::clone(&self.leases))),
            ))
        }
    }

    /// OpenAI's discovery and revocation endpoints, nothing else.
    struct Oauth {
        origin: String,
        requests: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Oauth {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn oauth() -> Oauth {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let issuer = origin.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let Ok(request) = mini_http::read_request(&mut socket).await else {
                    continue;
                };
                let path = request.target.split('?').next().unwrap_or("/").to_owned();
                seen.lock().push(path.clone());
                let (status, body) = match path.as_str() {
                    "/discovery" => (
                        200,
                        json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/authorize"),
                            "token_endpoint": format!("{issuer}/token"),
                            "jwks_uri": format!("{issuer}/jwks"),
                            "revocation_endpoint": format!("{issuer}/revoke"),
                        })
                        .to_string(),
                    ),
                    "/revoke" => (200, String::new()),
                    _ => (404, "{}".to_owned()),
                };
                let _ = mini_http::write_response(
                    &mut socket,
                    status,
                    "OK",
                    &[("content-type", "application/json")],
                    body.as_bytes(),
                )
                .await;
            }
        });
        Oauth {
            origin,
            requests,
            task,
        }
    }

    async fn shares_home_without_ambient_overrides(source: &str, primary: bool) {
        let instance_id = instance(if primary { "codex" } else { "codex-personal" });
        let temp = tempfile::tempdir().unwrap();
        let shared_home = temp.path().join("codex-shared-home");
        std::fs::create_dir_all(&shared_home).unwrap();
        std::fs::write(shared_home.join("auth.json"), "native-auth-unchanged").unwrap();
        std::fs::write(shared_home.join("config.toml"), "# shared config\n").unwrap();
        std::fs::create_dir_all(shared_home.join("sessions")).unwrap();
        let managed = source == "managed";
        let executable = CodexExecutable {
            executable_path: PathBuf::from(if managed {
                "/isolated/tools/codex/0.156.1/bin/codex"
            } else {
                "/user/bin/codex"
            }),
            managed_version_directory: managed
                .then(|| PathBuf::from("/isolated/tools/codex/0.156.1")),
            source: if managed {
                ProviderInstallSource::Managed
            } else {
                ProviderInstallSource::Local
            },
            version: "0.156.1".to_owned(),
        };
        let leases = Arc::new(AtomicUsize::new(0));
        let installation: Arc<dyn CodexExecutableSource> = Arc::new(FakeInstallation {
            executable: executable.clone(),
            leases: Arc::clone(&leases),
        });
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let store =
            ProviderCredentials::open(Arc::clone(&secrets), "codex-chatgpt", instance_id.as_str());
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 3_600_000;
        store
            .set(
                json!({
                    "clientId": "oaiapp_test",
                    "accessToken": "dummy-owned-access",
                    "refreshToken": "dummy-refresh",
                    "expiresAt": expires_at,
                    "earliestRefreshAt": null,
                    "scopes": ["chatgpt.tokens.use.direct"],
                    "subject": "test-user",
                    "email": null,
                })
                .to_string()
                .as_bytes(),
            )
            .await
            .unwrap();
        let oauth = oauth().await;
        let ambient: ProcessEnv = [
            ("CODEX_HOME", "/user/.codex"),
            ("OPENAI_API_KEY", "dummy-global-key"),
            ("OPENAI_BASE_URL", "https://user-proxy.test"),
            (
                "T3CODE_CODEX_LAUNCH_ARGS",
                "--config model_provider=global-proxy",
            ),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        let runtime = CodexManagedRuntime::make(CodexManagedRuntimeOptions {
            instance_id: instance_id.clone(),
            enabled: true,
            environment: ambient.clone(),
            config: settings(json!({
                "setupMode": "managed",
                "homePath": shared_home.to_string_lossy(),
            })),
            host: CodexManagedHost {
                state_dir: temp.path().join("userdata"),
                user_home: temp.path().join("home").to_string_lossy().into_owned(),
                server_url: "http://localhost:3773".to_owned(),
                secrets,
                secrets_directory: None,
                environment_id: "00000000-0000-4000-8000-000000000001".to_owned(),
                installation,
                discovery_url: Some(format!("{}/discovery", oauth.origin)),
                catalog_url: None,
            },
        })
        .await
        .unwrap();

        {
            let resolved = runtime.resolve().await.unwrap();
            let effective = &resolved.effective;
            assert_eq!(leases.load(Ordering::SeqCst), 1);
            let home_path = effective.config.home_path.as_str();
            assert_eq!(
                effective.config.binary_path.as_str(),
                executable.executable_path.to_string_lossy()
            );
            assert_ne!(home_path, ambient["CODEX_HOME"]);
            assert_eq!(runtime.home_layout.shared_home_path, shared_home);
            if primary {
                assert_eq!(home_path, shared_home.to_string_lossy());
                assert_eq!(runtime.home_layout.mode, CodexHomeMode::Direct);
            } else {
                assert!(home_path.contains(instance_id.as_str()));
                assert!(home_path.contains("userdata/providers/codex"));
                assert_eq!(runtime.home_layout.mode, CodexHomeMode::AuthOverlay);
                assert_eq!(
                    std::fs::read_link(std::path::Path::new(home_path).join("sessions")).unwrap(),
                    shared_home.join("sessions")
                );
                assert_eq!(
                    std::fs::read_link(std::path::Path::new(home_path).join("config.toml"))
                        .unwrap(),
                    shared_home.join("config.toml")
                );
                assert!(!std::path::Path::new(home_path).join("auth.json").exists());
            }
            assert_eq!(effective.environment["ACCESS_TOKEN"], "dummy-owned-access");
            assert!(!effective.environment.contains_key("OPENAI_API_KEY"));
            assert!(!effective.environment.contains_key("OPENAI_BASE_URL"));
            assert!(
                !effective
                    .environment
                    .contains_key("T3CODE_CODEX_LAUNCH_ARGS")
            );
            let launch_args = effective.config.launch_args.as_str();
            let args: HashSet<String> = codex_app_server_args(Some(launch_args))
                .into_iter()
                .collect();
            for expected in [
                r#"model_providers.openai_token_sharing.base_url="https://api.openai.com/v1""#,
                r#"model_providers.openai_token_sharing.model_catalog_url="https://api.openai.com/v1/models""#,
                "features.api_key_model_discovery=true",
                "features.multi_agent=false",
                "features.multi_agent_v2=false",
                "model_providers.openai_token_sharing.supports_websockets=false",
                "model_providers.openai_token_sharing.requires_openai_auth=false",
            ] {
                assert!(args.contains(expected), "missing {expected}");
            }
            assert!(!launch_args.contains("model_catalog_json"));
            assert!(!launch_args.contains("x-openai-chatpass-test"));
            assert!(!launch_args.contains("dummy-owned-access"));
            assert_eq!(ambient["CODEX_HOME"], "/user/.codex");

            // The launch built from it: tokenized overrides, the owned token, and the
            // inherited ambient overrides removed from the child's environment.
            let base = CodexRuntimeConfig {
                binary: PathBuf::from("codex"),
                cwd: PathBuf::from("/workspace"),
                home_path: None,
                launch_args: Vec::new(),
                environment: Default::default(),
                remove_environment: Vec::new(),
                client_version: "1".to_owned(),
            };
            let launch = managed_launch_config(&base, effective);
            assert_eq!(launch.binary, executable.executable_path);
            assert_eq!(launch.cwd, PathBuf::from("/workspace"));
            assert_eq!(launch.home_path.as_deref(), Some(home_path));
            assert_eq!(launch.environment["ACCESS_TOKEN"], "dummy-owned-access");
            assert_eq!(launch.client_version, "1");
            for name in [
                "OPENAI_API_KEY",
                "OPENAI_BASE_URL",
                "T3CODE_CODEX_LAUNCH_ARGS",
            ] {
                assert!(
                    launch
                        .remove_environment
                        .iter()
                        .any(|removed| removed == name)
                );
            }
            assert_eq!(launch.launch_args[0], "-c");
            assert!(
                launch
                    .launch_args
                    .contains(&r#"model_provider="openai_token_sharing""#.to_owned())
            );
        }
        assert_eq!(leases.load(Ordering::SeqCst), 0);
        (runtime.auth.controller.logout)(Box::pin(async { Ok(()) }))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(shared_home.join("auth.json")).unwrap(),
            "native-auth-unchanged"
        );
        assert!(
            oauth
                .requests
                .lock()
                .iter()
                .all(|path| path == "/discovery" || path == "/revoke")
        );
    }

    #[tokio::test]
    async fn managed_primary_account_shares_home_state_without_ambient_cli_overrides() {
        shares_home_without_ambient_overrides("managed", true).await;
    }

    #[tokio::test]
    async fn managed_additional_account_shares_home_state_without_ambient_cli_overrides() {
        shares_home_without_ambient_overrides("managed", false).await;
    }

    #[tokio::test]
    async fn local_primary_account_shares_home_state_without_ambient_cli_overrides() {
        shares_home_without_ambient_overrides("local", true).await;
    }

    #[tokio::test]
    async fn local_additional_account_shares_home_state_without_ambient_cli_overrides() {
        shares_home_without_ambient_overrides("local", false).await;
    }
}
