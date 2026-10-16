//! Port of upstream/apps/server/src/provider/CodexChatGptAuth.ts.
//!
//! `records` holds the persisted schemas and `store` the secret-backed reads
//! and writes. This module is the OAuth controller: discovery, token exchange,
//! the loopback (or client-delivered) sign-in, refresh, logout, and profile
//! import and export.
//!
//! Analytics (`track`, `accountCounts`) are cut with the rest of telemetry.

// `ProviderSetupError` is the contract error this flow returns. Boxing it
// would change nothing on the wire but every call site.
#![allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::type_complexity
)]

mod mini_http;
pub mod records;
pub mod store;

#[cfg(test)]
mod auth_tests;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::Future;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use parking_lot::Mutex as SyncMutex;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
    task::JoinSet,
};
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_setup::{
    ChatGptReconnectProfile, ChatGptTransferredProfile, ChatGptTransferredProfileCredentials,
    ProviderAuthInteraction, ProviderAuthMethod, ProviderAuthMethodType, ProviderSetupError,
    SetupOperationId,
};
use turin_provider_core::auth::ProviderAuthCallbackMode;
use turin_provider_core::auth_flow::{self, CompleteFn, FlowContext, ProviderAuthFlowOptions};
use turin_provider_core::credential_store::SecretStore;
use turin_provider_core::rt::BoxFuture;
use turin_shared::codex_auth_handoff::codex_callback_url;
use turin_shared::provider_auth_return_url::provider_auth_return_url;
use url::Url;

use crate::codex_auth_callback_page::codex_auth_callback_page;
use crate::codex_chatgpt_session_lock::{CredentialLockError, with_chatgpt_session_lock};

use self::records::{CodexChatGptCredentials, StoredRegistration};
use self::store::ChatGptStore;

const RESOURCE: &str = "https://api.openai.com/v1";
const REQUIRED_SCOPE: &str = "chatgpt.tokens.use.direct";
const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const TRUSTED_ISSUER: &str = "https://auth.openai.com";
const PROFILE_PREFIX: &str = "chatgpt-profile:";
const AUTH_SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const REFRESH_DEATH: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// What `make` needs. Environment identity, the secret store, and its directory
/// are plain values: there is no server-settings or environment service here.
pub struct CodexChatGptAuthOptions {
    pub instance_id: ProviderInstanceId,
    pub secrets: Arc<dyn SecretStore>,
    /// `None` for an in-memory store: the session file lock is skipped.
    pub secrets_directory: Option<PathBuf>,
    /// Becomes `urn:uuid:{environment_id}` on the authorization request.
    pub environment_id: String,
    pub discovery_url: Option<String>,
    pub resource: Option<String>,
    pub default_return_url: Option<String>,
    pub reconnect_profile: Option<ChatGptReconnectProfile>,
}

/// The controller plus the credential operations adapters call directly.
pub struct CodexChatGptAuth {
    inner: Arc<Inner>,
    pub controller: turin_provider_core::auth::ProviderAuthController,
}

struct Inner {
    instance_id: ProviderInstanceId,
    discovery_url: Option<String>,
    resource: String,
    default_return_url: Option<String>,
    /// Hint from `reconnect_profile`, kept after the registration itself is saved.
    reconnect_id_token_hint: Option<String>,
    store: Arc<ChatGptStore>,
    directory: Option<PathBuf>,
    binding_key: String,
    host_id: String,
    http: reqwest::Client,
    /// Upstream's per-environment semaphore. Shared by every controller built
    /// on the same secret store and credential binding.
    permit: Arc<Mutex<()>>,
    metadata: Mutex<Option<DiscoveryDoc>>,
    jwks: Mutex<Option<Vec<Jwk>>>,
}

#[derive(Debug, Clone, Deserialize)]
struct DiscoveryDoc {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    revocation_endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token_type: String,
    expires_in: i64,
    scope: String,
    earliest_refresh_at: Option<Earliest>,
}

enum Earliest {
    Number(f64),
    Text(String),
}

struct VerifiedIdentity {
    subject: String,
    /// `typeof payload.email === "string" ? payload.email : null`.
    email: Option<String>,
    /// `payload.email ?? null`, so a non-string claim stays non-null.
    email_claim: Value,
}

struct Incoming {
    url: Url,
    reply: Option<oneshot::Sender<CallbackPage>>,
    done: Option<oneshot::Receiver<()>>,
}

struct CallbackPage {
    success: bool,
    return_url: Option<String>,
    nonce: String,
}

struct CallbackGate {
    used: AtomicBool,
    tx: SyncMutex<Option<oneshot::Sender<Incoming>>>,
}

impl CallbackGate {
    fn claim(&self, incoming: Incoming) -> bool {
        if self.used.swap(true, Ordering::SeqCst) {
            return false;
        }
        if let Some(tx) = self.tx.lock().take() {
            let _ = tx.send(incoming);
        }
        true
    }
}

struct CallbackServer {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for CallbackServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl CodexChatGptAuth {
    pub async fn read(&self) -> Result<Option<CodexChatGptCredentials>, ProviderSetupError> {
        self.inner.store.read().await
    }

    /// Refresh the active token set when it is inside the 60 s window.
    /// The refresh finishes even if the caller is dropped, so a rotated
    /// refresh token is not lost.
    pub async fn access(&self) -> Result<CodexChatGptCredentials, ProviderSetupError> {
        let inner = Arc::clone(&self.inner);
        uninterruptible(async move {
            let _permit = inner.permit.lock().await;
            let locked = Arc::clone(&inner);
            session_locked(&inner, async move { locked.access_locked().await }).await
        })
        .await
    }

    pub async fn export_profile(&self) -> Result<ChatGptTransferredProfile, ProviderSetupError> {
        self.inner.export_profile().await
    }

    pub async fn revoke(&self) -> Result<(), ProviderSetupError> {
        let _permit = self.inner.permit.lock().await;
        let inner = Arc::clone(&self.inner);
        let locked = Arc::clone(&inner);
        session_locked(&inner, async move { locked.store.remove().await }).await
    }
}

/// `makeCodexChatGptAuth`. Method discovery starts immediately.
pub async fn make(
    options: CodexChatGptAuthOptions,
) -> Result<CodexChatGptAuth, ProviderSetupError> {
    let store = Arc::new(ChatGptStore::new(
        Arc::clone(&options.secrets),
        options.instance_id.clone(),
    ));
    let binding_key = store.binding().key.clone();
    let reconnect_id_token_hint = options
        .reconnect_profile
        .as_ref()
        .and_then(|profile| profile.id_token_hint.clone());
    if let Some(profile) = options.reconnect_profile.clone() {
        store
            .save_registration(stored_registration(&profile))
            .await?;
    }
    let http = reqwest::Client::builder().build().expect("reqwest client");
    let inner = Arc::new(Inner {
        permit: shared_permit(&options.secrets, &binding_key),
        instance_id: options.instance_id.clone(),
        discovery_url: options.discovery_url,
        resource: options.resource.unwrap_or_else(|| RESOURCE.to_owned()),
        default_return_url: options.default_return_url,
        reconnect_id_token_hint,
        directory: options.secrets_directory,
        host_id: format!("urn:uuid:{}", options.environment_id),
        store,
        binding_key,
        http,
        metadata: Mutex::new(None),
        jwks: Mutex::new(None),
    });
    let methods_inner = Arc::clone(&inner);
    let auth_inner = Arc::clone(&inner);
    let logout_inner = Arc::clone(&inner);
    let binding = inner.store.binding().clone();
    let instance_id = options.instance_id;
    let mut controller = auth_flow::make(ProviderAuthFlowOptions {
        instance_id,
        credential_binding: binding,
        methods: Arc::new(move || {
            let inner = Arc::clone(&methods_inner);
            Box::pin(async move { inner.methods().await })
        }),
        default_method_id: None,
        refresh_methods_after_auth: true,
        authenticate: Arc::new(move |method, context| {
            let inner = Arc::clone(&auth_inner);
            Box::pin(async move {
                let _permit = inner.permit.lock().await;
                inner.authenticate(method, context).await
            })
        }),
        logout: Arc::new(move || {
            let inner = Arc::clone(&logout_inner);
            Box::pin(async move {
                let _permit = inner.permit.lock().await;
                let locked = Arc::clone(&inner);
                session_locked(&inner, async move { locked.logout_locked().await }).await
            })
        }),
        timeout: ProviderAuthFlowOptions::DEFAULT_TIMEOUT,
    });
    let reconnect_inner = Arc::clone(&inner);
    controller.reconnect_profile = Some(Arc::new(move |method| {
        let inner = Arc::clone(&reconnect_inner);
        Box::pin(async move { inner.reconnect_profile(method).await })
    }));
    let import_inner = Arc::clone(&inner);
    let adopt = controller
        .adopt_credentials
        .clone()
        .expect("auth flow adopts credentials");
    controller.import_profile = Some(Arc::new(move |profile, stop_sessions| {
        let inner = Arc::clone(&import_inner);
        let adopt = Arc::clone(&adopt);
        Box::pin(async move { inner.import_profile(profile, stop_sessions, adopt).await })
    }));
    Ok(CodexChatGptAuth { inner, controller })
}

impl Inner {
    fn fail(&self, operation: &str, detail: &str) -> ProviderSetupError {
        store::failure(&self.instance_id, operation, detail)
    }

    async fn discover(&self) -> Result<DiscoveryDoc, ProviderSetupError> {
        if let Some(cached) = self.metadata.lock().await.clone() {
            return Ok(cached);
        }
        let url = self.discovery_url.as_deref().unwrap_or(DISCOVERY);
        let doc = match self.http.get(url).send().await {
            Ok(response) => {
                let status = response.status();
                let bytes = response.bytes().await.map_err(|_| {
                    self.fail("discover", "Could not reach ChatGPT sign-in. Try again.")
                })?;
                if !status.is_success() {
                    return Err(
                        self.fail("discover", "Could not reach ChatGPT sign-in. Try again.")
                    );
                }
                serde_json::from_slice::<DiscoveryDoc>(&bytes).map_err(|_| {
                    self.fail("discover", "Could not reach ChatGPT sign-in. Try again.")
                })?
            }
            Err(_) => {
                return Err(self.fail("discover", "Could not reach ChatGPT sign-in. Try again."));
            }
        };
        if self.discovery_url.is_none() && !discovery_is_trusted(&doc) {
            return Err(self.fail(
                "discover",
                "ChatGPT sign-in configuration could not be verified.",
            ));
        }
        *self.metadata.lock().await = Some(doc.clone());
        Ok(doc)
    }

    async fn jwks(&self, uri: &str, force: bool) -> Result<Vec<Jwk>, ()> {
        if !force && let Some(cached) = self.jwks.lock().await.clone() {
            return Ok(cached);
        }
        let response = self.http.get(uri).send().await.map_err(|_| ())?;
        if !response.status().is_success() {
            return Err(());
        }
        let bytes = response.bytes().await.map_err(|_| ())?;
        let parsed: Jwks = serde_json::from_slice(&bytes).map_err(|_| ())?;
        *self.jwks.lock().await = Some(parsed.keys.clone());
        Ok(parsed.keys)
    }

    async fn exchange(&self, form: &[(String, String)]) -> Result<Tokens, ProviderSetupError> {
        let endpoints = self.discover().await?;
        let body = encode_form(form);
        let response = self
            .http
            .post(&endpoints.token_endpoint)
            .header("accept", "application/json")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(_) => {
                return Err(self.fail(
                    "exchange",
                    "ChatGPT sign-in is temporarily unavailable. Try again.",
                ));
            }
        };
        let status = response.status();
        let bytes = response.bytes().await.map_err(|_| {
            self.fail(
                "exchange",
                "ChatGPT sign-in is temporarily unavailable. Try again.",
            )
        })?;
        let raw: Value = serde_json::from_slice(&bytes).map_err(|_| {
            self.fail(
                "exchange",
                "ChatGPT sign-in is temporarily unavailable. Try again.",
            )
        })?;
        if !status.is_success() {
            return self.oauth_failure(form, &raw).await;
        }
        decode_tokens(&raw).map_err(|_| {
            self.fail(
                "exchange",
                "ChatGPT returned an invalid token response. Sign in again.",
            )
        })
    }

    async fn oauth_failure(
        &self,
        form: &[(String, String)],
        raw: &Value,
    ) -> Result<Tokens, ProviderSetupError> {
        let Some(error) = raw.get("error").and_then(Value::as_str) else {
            return Err(self.fail(
                "exchange",
                "ChatGPT did not accept this sign-in. Try again.",
            ));
        };
        let grant = form_value(form, "grant_type");
        if grant == Some("refresh_token") && REFRESH_DEATH.contains(&error) {
            self.store.remove().await?;
            return Err(self.fail(
                "refresh",
                "Your ChatGPT connection expired or was disconnected. Sign in again.",
            ));
        }
        if error == "invalid_client" {
            return Err(self.fail(
                "client",
                "OpenAI rejected this app's client registration. Check the ChatGPT connection configuration.",
            ));
        }
        if grant == Some("authorization_code") && error == "invalid_grant" {
            return Err(self.fail("code-expired", "This sign-in code expired. Start again."));
        }
        Err(self.fail(
            "exchange",
            if error == "access_denied" {
                "ChatGPT sign-in was declined. Sign in again when you are ready."
            } else {
                "ChatGPT could not complete sign-in. Try again."
            },
        ))
    }

    async fn access_locked(&self) -> Result<CodexChatGptCredentials, ProviderSetupError> {
        let Some(mut record) = self.store.read().await? else {
            return Err(self.fail("access", "Sign in with ChatGPT to use managed Codex."));
        };
        if !record.scopes.iter().any(|scope| scope == REQUIRED_SCOPE) {
            return Err(self.fail(
                "sharing",
                "Token sharing is disabled. Sign in with ChatGPT and enable token sharing.",
            ));
        }
        let now = now_ms();
        if record.expires_at - now > 60_000.0 {
            return Ok(record);
        }
        if let Some(earliest) = record.earliest_refresh_at
            && earliest > now
        {
            if record.expires_at > now {
                return Ok(record);
            }
            return Err(self.fail(
                "refresh",
                "ChatGPT cannot renew this connection yet. Try again shortly.",
            ));
        }
        let Some(refresh_token) = record.refresh_token.clone() else {
            self.store.remove().await?;
            return Err(self.fail("refresh", "Your ChatGPT connection expired. Sign in again."));
        };
        let exchanged = tokio::time::timeout(
            Duration::from_secs(20),
            self.exchange(&[
                ("grant_type".to_owned(), "refresh_token".to_owned()),
                ("client_id".to_owned(), record.client_id.clone()),
                ("refresh_token".to_owned(), refresh_token),
                ("resource".to_owned(), self.resource.clone()),
            ]),
        )
        .await;
        let tokens = match exchanged {
            Ok(Ok(tokens)) => tokens,
            Ok(Err(error)) if operation_of(&error) == "client" => return Err(error),
            _ => {
                return Err(self.fail(
                    "refresh",
                    "Could not renew the ChatGPT connection. Retry, or sign in again.",
                ));
            }
        };
        if tokens.refresh_token.is_none() || !tokens.token_type.eq_ignore_ascii_case("bearer") {
            return Err(self.fail(
                "refresh",
                "ChatGPT returned an invalid renewal. Sign in again.",
            ));
        }
        record.access_token = tokens.access_token;
        record.refresh_token = tokens.refresh_token;
        record.expires_at = now_ms() + tokens.expires_in as f64 * 1000.0;
        record.earliest_refresh_at = earliest_millis(tokens.earliest_refresh_at);
        record.scopes = split_scopes(&tokens.scope);
        self.store.save(record.clone(), true).await?;
        if !record.scopes.iter().any(|scope| scope == REQUIRED_SCOPE) {
            return Err(self.fail(
                "sharing",
                "ChatGPT token sharing is no longer enabled. Sign in again.",
            ));
        }
        Ok(record)
    }

    async fn authenticate(
        self: &Arc<Self>,
        method: String,
        context: FlowContext,
    ) -> Result<(), ProviderSetupError> {
        let endpoints = self.discover().await?;
        let previous = self.store.read().await?;
        let registrations = self.store.read_registrations().await?;
        let changing_account = method == "chatgpt-change-account";
        let selected_client_id = if let Some(id) = method.strip_prefix(PROFILE_PREFIX) {
            id.to_owned()
        } else {
            previous
                .as_ref()
                .map(|record| record.client_id.clone())
                .or(registrations.last_client_id.clone())
                .unwrap_or_default()
        };
        let saved_registration = if changing_account {
            None
        } else {
            registrations
                .profiles
                .iter()
                .find(|profile| profile.client_id == selected_client_id)
                .cloned()
        };
        let legacy_callback = saved_registration
            .as_ref()
            .and_then(|profile| profile.redirect_uri.as_deref())
            .is_some_and(|uri| uri.contains("//localhost:"));
        let registered_client_id = if legacy_callback {
            None
        } else {
            saved_registration
                .as_ref()
                .map(|profile| profile.client_id.clone())
        };
        let selected_tokens = self
            .store
            .read_sessions()
            .await?
            .sessions
            .into_iter()
            .find(|session| session.client_id == selected_client_id);
        let state = random_token(32);
        let nonce = random_token(32);
        let verifier = random_token(64);
        let return_url = provider_auth_return_url(context.return_url.as_deref())
            .or_else(|| provider_auth_return_url(self.default_return_url.as_deref()));
        let page_nonce = random_token(24);
        let (tx, rx) = oneshot::channel();
        let gate = Arc::new(CallbackGate {
            used: AtomicBool::new(false),
            tx: SyncMutex::new(Some(tx)),
        });
        let client_callback = context.callback_mode == Some(ProviderAuthCallbackMode::Client);
        let server = if client_callback {
            None
        } else {
            Some(spawn_callback_server(Arc::clone(&gate), &self.instance_id).await?)
        };
        let port = match &server {
            Some(server) => server.port,
            None => ephemeral_client_port(),
        };
        let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
        let id_token_hint = selected_tokens
            .as_ref()
            .and_then(|tokens| tokens.id_token.clone())
            .or_else(|| self.reconnect_id_token_hint.clone());
        let authorize = authorization_url(
            &endpoints.authorization_endpoint,
            registered_client_id.as_deref(),
            saved_registration.as_ref(),
            selected_tokens.as_ref(),
            id_token_hint.as_deref(),
            &self.host_id,
            &redirect_uri,
            &self.resource,
            &state,
            &nonce,
            &code_challenge(&verifier),
        );
        let complete = complete_callback(
            Arc::clone(&gate),
            self.instance_id.clone(),
            redirect_uri.clone(),
            state.clone(),
        );
        let interaction = ProviderAuthInteraction::Browser {
            id: SetupOperationId::new(&context.flow_id).expect("flow id"),
            url: TrimmedNonEmptyString::new(&authorize).expect("authorization url"),
            requires_consent: false,
            accepts_callback: Some(true),
        };
        (context.set_interaction)(interaction, None, Some(complete)).await;
        let mut incoming = rx
            .await
            .map_err(|_| self.fail("callback", "ChatGPT sign-in could not be completed."))?;
        let outcome = self
            .finish_sign_in(
                incoming.url.clone(),
                &state,
                &nonce,
                &verifier,
                &redirect_uri,
                registered_client_id.as_deref(),
                saved_registration.as_ref(),
                previous.as_ref(),
                &registrations.profiles,
                &endpoints,
            )
            .await;
        deliver_page(&mut incoming, outcome.is_ok(), return_url, &page_nonce).await;
        drop(server);
        outcome
    }

    async fn finish_sign_in(
        &self,
        returned: Url,
        state: &str,
        nonce: &str,
        verifier: &str,
        redirect_uri: &str,
        registered_client_id: Option<&str>,
        saved_registration: Option<&StoredRegistration>,
        previous: Option<&CodexChatGptCredentials>,
        known_profiles: &[StoredRegistration],
        endpoints: &DiscoveryDoc,
    ) -> Result<(), ProviderSetupError> {
        if query_param(&returned, "state").as_deref() != Some(state) {
            return Err(self.fail(
                "callback",
                "ChatGPT sign-in could not be verified. Start again.",
            ));
        }
        if let Some(error) = query_param(&returned, "error") {
            return Err(self.fail(
                "callback",
                if error == "access_denied" {
                    "ChatGPT sign-in was declined. Sign in again when you are ready."
                } else {
                    "ChatGPT sign-in could not be completed. Start again."
                },
            ));
        }
        let code = query_param(&returned, "code");
        let returned_client = query_param(&returned, "client_id");
        let client_id = registered_client_id
            .map(str::to_owned)
            .or(returned_client.clone());
        let client_ok = client_id
            .as_deref()
            .is_some_and(|id| id.starts_with("oaiapp_"));
        let client_conflict = registered_client_id.is_some()
            && returned_client.is_some()
            && returned_client.as_deref() != registered_client_id;
        if code.is_none() || !client_ok || client_conflict {
            return Err(self.fail(
                "callback",
                "ChatGPT registration is incomplete. Start sign-in again.",
            ));
        }
        let client_id = client_id.expect("checked");
        let code = code.expect("checked");
        let tokens = match self
            .exchange(&[
                ("grant_type".to_owned(), "authorization_code".to_owned()),
                ("client_id".to_owned(), client_id.clone()),
                ("code".to_owned(), code),
                ("code_verifier".to_owned(), verifier.to_owned()),
                ("redirect_uri".to_owned(), redirect_uri.to_owned()),
                ("resource".to_owned(), self.resource.clone()),
            ])
            .await
        {
            Ok(tokens) => tokens,
            Err(error) if operation_of(&error) == "code-expired" => {
                if registered_client_id.is_none() {
                    let store = Arc::clone(&self.store);
                    let client_id = client_id.clone();
                    let redirect_uri = redirect_uri.to_owned();
                    let instance = self.instance_id.clone();
                    let directory = self.directory.clone();
                    let key = self.binding_key.clone();
                    session_lock(directory, key, &instance, async move {
                        store
                            .save_registration(StoredRegistration {
                                client_id,
                                subject: None,
                                connection_label: None,
                                sharing_enabled: None,
                                email: None,
                                redirect_uri: Some(redirect_uri),
                            })
                            .await
                    })
                    .await?;
                }
                return Err(self.fail(
                    "exchange",
                    "This sign-in code expired. Reconnect the saved ChatGPT profile to start a fresh sign-in.",
                ));
            }
            Err(error) => return Err(error),
        };
        let Some(id_token) = tokens.id_token.clone() else {
            return Err(self.fail(
                "verify",
                "ChatGPT did not return a verified identity. Sign in again.",
            ));
        };
        let identity = self
            .verify_id_token(&id_token, endpoints, &client_id, Some(nonce))
            .await
            .map_err(|_| {
                self.fail(
                    "verify",
                    "ChatGPT sign-in could not be verified. Start again.",
                )
            })?;
        let expected_subject = saved_registration
            .and_then(|profile| profile.subject.clone())
            .or_else(|| {
                previous.and_then(|record| {
                    (registered_client_id == Some(record.client_id.as_str()))
                        .then(|| record.subject.clone())
                })
            });
        if registered_client_id.is_some()
            && expected_subject.is_some()
            && expected_subject.as_deref() != Some(identity.subject.as_str())
        {
            return Err(self.fail(
                "verify",
                "This sign-in returned a different ChatGPT account. Use the different-account sign-in option instead. Your saved connection is unchanged.",
            ));
        }
        if let Some(known) = known_profiles
            .iter()
            .find(|profile| profile.client_id == client_id)
            && ((known
                .redirect_uri
                .as_deref()
                .is_some_and(|original| redirect_identity_differs(original, redirect_uri)))
                || known
                    .subject
                    .as_deref()
                    .is_some_and(|subject| subject != identity.subject))
        {
            return Err(self.fail(
                "verify",
                "ChatGPT returned a conflicting account registration. Start again.",
            ));
        }
        if !tokens.token_type.eq_ignore_ascii_case("bearer") {
            return Err(self.fail(
                "verify",
                "ChatGPT returned an unsupported connection. Sign in again.",
            ));
        }
        let scopes = split_scopes(&tokens.scope);
        let sharing = scopes.iter().any(|scope| scope == REQUIRED_SCOPE);
        let record = CodexChatGptCredentials {
            client_id: client_id.clone(),
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: now_ms() + tokens.expires_in as f64 * 1000.0,
            earliest_refresh_at: earliest_millis(tokens.earliest_refresh_at),
            scopes: scopes.clone(),
            subject: identity.subject.clone(),
            email: identity.email.clone(),
            id_token: Some(id_token),
            issuer: Some(endpoints.issuer.clone()),
        };
        let registration = StoredRegistration {
            client_id: client_id.clone(),
            redirect_uri: Some(redirect_uri.to_owned()),
            sharing_enabled: Some(sharing),
            subject: Some(identity.subject),
            email: Some(identity.email),
            connection_label: None,
        };
        let store = Arc::clone(&self.store);
        let directory = self.directory.clone();
        let key = self.binding_key.clone();
        let instance = self.instance_id.clone();
        session_lock(directory, key, &instance, async move {
            store.save_registration(registration).await?;
            let active_missing = store.read().await?.is_none();
            store.save(record, sharing || active_missing).await
        })
        .await?;
        if !sharing {
            return Err(self.fail(
                "sharing",
                if previous.is_some_and(|record| record.client_id != client_id) {
                    "Token sharing was not enabled for the new account. Your existing ChatGPT connection is unchanged."
                } else {
                    "Signed in with ChatGPT, but token sharing is disabled. Sign in again and enable token sharing, or use another provider."
                },
            ));
        }
        Ok(())
    }

    async fn verify_id_token(
        &self,
        token: &str,
        endpoints: &DiscoveryDoc,
        audience: &str,
        nonce: Option<&str>,
    ) -> Result<VerifiedIdentity, ()> {
        let header = decode_header(token).map_err(|_| ())?;
        if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
            return Err(());
        }
        let mut keys = self.jwks(&endpoints.jwks_uri, false).await?;
        if !keys.iter().any(|key| kid_matches(key, &header)) {
            keys = self.jwks(&endpoints.jwks_uri, true).await?;
        }
        let mut validation = Validation::new(header.alg);
        validation.leeway = 5;
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.set_issuer(&[&endpoints.issuer]);
        validation.set_audience(&[audience]);
        let mut verified = None;
        for key in keys.iter().filter(|key| kid_matches(key, &header)) {
            let Some(decoding) = decoding_key(key) else {
                continue;
            };
            if let Ok(data) = decode::<Value>(token, &decoding, &validation) {
                verified = Some(data.claims);
                break;
            }
        }
        let Some(payload) = verified else {
            return Err(());
        };
        let payload = payload.as_object().ok_or(())?;
        if let Some(expected) = nonce {
            match payload.get("nonce") {
                Some(Value::String(value)) if value == expected => {}
                _ => return Err(()),
            }
        }
        let subject = match payload.get("sub") {
            Some(Value::String(value)) if !value.is_empty() => value.clone(),
            _ => return Err(()),
        };
        if payload.get("exp").and_then(Value::as_i64).unwrap_or(0) == 0
            && payload.get("exp").and_then(Value::as_f64).unwrap_or(0.0) == 0.0
        {
            return Err(());
        }
        let email_claim = payload.get("email").cloned().unwrap_or(Value::Null);
        let email = match &email_claim {
            Value::String(value) => Some(value.clone()),
            _ => None,
        };
        Ok(VerifiedIdentity {
            subject,
            email,
            email_claim,
        })
    }

    async fn logout_locked(&self) -> Result<Option<String>, ProviderSetupError> {
        let credentials = self.store.read().await?;
        let mut confirmed = credentials
            .as_ref()
            .and_then(|record| record.refresh_token.as_ref())
            .is_none();
        if let Some(record) = credentials
            .as_ref()
            .filter(|record| record.refresh_token.is_some())
        {
            for attempt in 0..3 {
                let result =
                    match tokio::time::timeout(Duration::from_secs(10), self.revoke_once(record))
                        .await
                    {
                        Ok(result) => result,
                        Err(_) => RevokeAttempt {
                            confirmed: false,
                            retry: true,
                        },
                    };
                confirmed = result.confirmed;
                if confirmed || !result.retry || attempt == 2 {
                    break;
                }
                let delay = if attempt == 0 {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(1)
                };
                tokio::time::sleep(delay).await;
            }
        }
        self.store.remove().await?;
        if confirmed {
            Ok(None)
        } else {
            Ok(Some(
                "Signed out locally. Remote revocation could not be confirmed. Disconnect the app in ChatGPT Settings."
                    .to_owned(),
            ))
        }
    }

    async fn revoke_once(&self, record: &CodexChatGptCredentials) -> RevokeAttempt {
        let endpoints = match self.discover().await {
            Ok(endpoints) => endpoints,
            Err(_) => {
                return RevokeAttempt {
                    confirmed: false,
                    retry: true,
                };
            }
        };
        let Some(url) = endpoints.revocation_endpoint else {
            return RevokeAttempt {
                confirmed: false,
                retry: false,
            };
        };
        let Some(token) = record.refresh_token.clone() else {
            return RevokeAttempt {
                confirmed: false,
                retry: false,
            };
        };
        let body = encode_form(&[
            ("token".to_owned(), token),
            ("token_type_hint".to_owned(), "refresh_token".to_owned()),
            ("client_id".to_owned(), record.client_id.clone()),
        ]);
        match self
            .http
            .post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status().as_u16();
                RevokeAttempt {
                    confirmed: status == 200,
                    retry: status >= 500,
                }
            }
            Err(_) => RevokeAttempt {
                confirmed: false,
                retry: true,
            },
        }
    }

    async fn methods(&self) -> Result<Vec<ProviderAuthMethod>, ProviderSetupError> {
        let saved = self.store.read_registrations().await?;
        let active = self.store.read().await?;
        let current_id = active
            .as_ref()
            .map(|record| record.client_id.clone())
            .or(saved.last_client_id.clone());
        let current = current_id.as_ref().and_then(|id| {
            saved
                .profiles
                .iter()
                .find(|profile| &profile.client_id == id)
                .cloned()
        });
        let mut profiles = Vec::new();
        if let Some(current) = current.clone() {
            profiles.push(current);
            profiles.extend(
                saved
                    .profiles
                    .iter()
                    .filter(|profile| Some(&profile.client_id) != current_id.as_ref())
                    .cloned(),
            );
        } else {
            profiles = saved.profiles;
        }
        let email = current.as_ref().and_then(profile_email);
        let mut methods = vec![
            auth_method(
                "chatgpt",
                "Sign in with ChatGPT",
                email.map(|email| format!("Reconnect {email}.")),
                email,
            ),
            auth_method(
                "chatgpt-change-account",
                "Use a different ChatGPT account",
                Some("Register a connection for another ChatGPT account.".to_owned()),
                None,
            ),
        ];
        for profile in profiles.into_iter().take(30) {
            let email = profile_email(&profile);
            let label = profile.connection_label.as_deref().unwrap_or("Connection");
            let name = format!("{} · {label}", email.unwrap_or("ChatGPT account"));
            methods.push(auth_method(
                &format!("{PROFILE_PREFIX}{}", profile.client_id),
                &name,
                Some("Reuse this account's original sign-in registration.".to_owned()),
                email,
            ));
        }
        Ok(methods)
    }

    async fn reconnect_profile(
        &self,
        method_id: String,
    ) -> Result<Option<ChatGptReconnectProfile>, ProviderSetupError> {
        if method_id == "chatgpt-change-account" {
            return Ok(None);
        }
        let registrations = self.store.read_registrations().await?;
        let active = self.store.read().await?;
        let client_id = if let Some(id) = method_id.strip_prefix(PROFILE_PREFIX) {
            id.to_owned()
        } else {
            active
                .as_ref()
                .map(|record| record.client_id.clone())
                .or(registrations.last_client_id.clone())
                .unwrap_or_default()
        };
        let Some(registration) = registrations
            .profiles
            .into_iter()
            .find(|profile| profile.client_id == client_id)
        else {
            if method_id.starts_with(PROFILE_PREFIX) {
                return Err(self.fail("export", "This saved connection is no longer available."));
            }
            return Ok(None);
        };
        let hint = self
            .store
            .read_sessions()
            .await?
            .sessions
            .into_iter()
            .find(|session| session.client_id == client_id)
            .and_then(|session| session.id_token);
        Ok(Some(reconnect_profile(registration, hint)))
    }

    async fn export_profile(&self) -> Result<ChatGptTransferredProfile, ProviderSetupError> {
        let credentials = self.store.read().await?;
        let registration = if let Some(credentials) = credentials.as_ref() {
            self.store
                .read_registrations()
                .await?
                .profiles
                .into_iter()
                .find(|profile| profile.client_id == credentials.client_id)
        } else {
            None
        };
        let Some(credentials) = credentials else {
            return Err(self.fail(
                "export",
                "Complete ChatGPT sign-in before transferring this connection.",
            ));
        };
        let (Some(id_token), Some(issuer), Some(registration)) = (
            credentials.id_token.clone(),
            credentials.issuer.clone(),
            registration,
        ) else {
            return Err(self.fail(
                "export",
                "Complete ChatGPT sign-in before transferring this connection.",
            ));
        };
        Ok(ChatGptTransferredProfile {
            registration: reconnect_profile(registration, None),
            credentials: ChatGptTransferredProfileCredentials {
                client_id: credentials.client_id,
                access_token: credentials.access_token,
                refresh_token: credentials.refresh_token,
                id_token,
                issuer,
                expires_at: credentials.expires_at,
                earliest_refresh_at: credentials.earliest_refresh_at,
                scopes: credentials.scopes,
                subject: credentials.subject,
                email: credentials.email,
            },
        })
    }

    async fn import_profile(
        self: &Arc<Self>,
        profile: ChatGptTransferredProfile,
        stop_sessions: BoxFuture<'static, Result<(), ProviderSetupError>>,
        adopt: Arc<
            dyn Fn(
                    BoxFuture<'static, Result<(), ProviderSetupError>>,
                    BoxFuture<'static, Result<(), ProviderSetupError>>,
                ) -> BoxFuture<
                    'static,
                    Result<turin_contracts::provider_setup::ProviderAuthState, ProviderSetupError>,
                > + Send
                + Sync,
        >,
    ) -> Result<turin_contracts::provider_setup::ProviderAuthState, ProviderSetupError> {
        let _permit = self.permit.lock().await;
        self.validate_import(&profile).await?;
        let store = Arc::clone(&self.store);
        let directory = self.directory.clone();
        let key = self.binding_key.clone();
        let instance = self.instance_id.clone();
        let saved_as = instance.clone();
        let update = Box::pin(async move {
            session_lock(directory, key, &instance, async move {
                save_imported(&store, &saved_as, profile).await
            })
            .await
        });
        adopt(update, stop_sessions).await
    }

    async fn validate_import(
        &self,
        profile: &ChatGptTransferredProfile,
    ) -> Result<(), ProviderSetupError> {
        let endpoints = self.discover().await?;
        let credentials = &profile.credentials;
        if credentials.client_id != profile.registration.client_id
            || credentials.issuer != endpoints.issuer
            || !credentials
                .scopes
                .iter()
                .any(|scope| scope == REQUIRED_SCOPE)
            || credentials.expires_at <= now_ms()
        {
            return Err(self.fail(
                "import",
                "The transferred ChatGPT connection is invalid or expired.",
            ));
        }
        let identity = self
            .verify_id_token(
                &credentials.id_token,
                &endpoints,
                &credentials.client_id,
                None,
            )
            .await
            .map_err(|_| {
                self.fail(
                    "import",
                    "The transferred ChatGPT identity could not be verified.",
                )
            })?;
        let registration_email_conflicts = profile.registration.email.is_some()
            && profile.registration.email.as_ref() != Some(&credentials.email);
        if identity.subject != credentials.subject
            || profile.registration.subject.as_deref() != Some(credentials.subject.as_str())
            || !email_claim_matches(&identity.email_claim, &credentials.email)
            || registration_email_conflicts
        {
            return Err(self.fail(
                "import",
                "The transferred ChatGPT identity does not match its profile.",
            ));
        }
        Ok(())
    }
}

struct RevokeAttempt {
    confirmed: bool,
    retry: bool,
}

async fn save_imported(
    store: &ChatGptStore,
    instance_id: &ProviderInstanceId,
    profile: ChatGptTransferredProfile,
) -> Result<(), ProviderSetupError> {
    let registration = stored_registration(&profile.registration);
    let existing = store
        .read_registrations()
        .await?
        .profiles
        .into_iter()
        .find(|saved| saved.client_id == registration.client_id);
    if existing
        .as_ref()
        .and_then(|saved| saved.subject.as_deref())
        .is_some_and(|subject| subject != profile.credentials.subject)
    {
        return Err(store::failure(
            instance_id,
            "import",
            "The transferred identity conflicts with the saved ChatGPT connection.",
        ));
    }
    store.save_registration(registration).await?;
    store
        .save(credentials_from_transferred(profile.credentials), true)
        .await
}

fn stored_registration(profile: &ChatGptReconnectProfile) -> StoredRegistration {
    StoredRegistration {
        client_id: profile.client_id.clone(),
        subject: profile.subject.clone(),
        connection_label: profile.connection_label.clone(),
        sharing_enabled: profile.sharing_enabled,
        email: profile.email.clone(),
        redirect_uri: profile.redirect_uri.clone(),
    }
}

fn reconnect_profile(
    registration: StoredRegistration,
    id_token_hint: Option<String>,
) -> ChatGptReconnectProfile {
    ChatGptReconnectProfile {
        client_id: registration.client_id,
        subject: registration.subject,
        email: registration.email,
        redirect_uri: registration.redirect_uri,
        connection_label: registration.connection_label,
        sharing_enabled: registration.sharing_enabled,
        id_token_hint,
    }
}

fn credentials_from_transferred(
    credentials: ChatGptTransferredProfileCredentials,
) -> CodexChatGptCredentials {
    CodexChatGptCredentials {
        client_id: credentials.client_id,
        access_token: credentials.access_token,
        refresh_token: credentials.refresh_token,
        expires_at: credentials.expires_at,
        earliest_refresh_at: credentials.earliest_refresh_at,
        scopes: credentials.scopes,
        subject: credentials.subject,
        email: credentials.email,
        id_token: Some(credentials.id_token),
        issuer: Some(credentials.issuer),
    }
}

fn profile_email(profile: &StoredRegistration) -> Option<&str> {
    profile
        .email
        .as_ref()
        .and_then(|email| email.as_deref())
        .filter(|email| !email.is_empty())
}

fn auth_method(
    id: &str,
    name: &str,
    description: Option<String>,
    email: Option<&str>,
) -> ProviderAuthMethod {
    ProviderAuthMethod {
        id: SetupOperationId::new(id).expect("method id"),
        name: TrimmedNonEmptyString::new(name).expect("method name"),
        description,
        account_email: email.and_then(|email| TrimmedNonEmptyString::new(email).ok()),
        type_: ProviderAuthMethodType::Agent,
    }
}

fn authorization_url(
    endpoint: &str,
    registered_client_id: Option<&str>,
    saved: Option<&StoredRegistration>,
    selected_tokens: Option<&CodexChatGptCredentials>,
    id_token_hint: Option<&str>,
    host_id: &str,
    redirect_uri: &str,
    resource: &str,
    state: &str,
    nonce: &str,
    challenge: &str,
) -> String {
    let mut url = Url::parse(endpoint).expect("authorization endpoint");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair(
            "client_id",
            registered_client_id.unwrap_or("dynamic_agent_client"),
        );
        if registered_client_id.is_some() {
            if let Some(email) = saved.and_then(profile_email) {
                query.append_pair("login_hint", email);
            }
            if let Some(hint) = id_token_hint {
                query.append_pair("id_token_hint", hint);
            }
            let needs_consent = saved.is_some_and(|profile| profile.sharing_enabled == Some(false))
                || selected_tokens.is_some_and(|tokens| {
                    !tokens.scopes.iter().any(|scope| scope == REQUIRED_SCOPE)
                });
            if needs_consent {
                query.append_pair("prompt", "consent");
            }
        } else {
            query.append_pair("agent_name_hint", "T3 Code");
        }
        query.append_pair("ext_agent_host_id", host_id);
        query.append_pair("response_type", "code");
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("scope", AUTH_SCOPE);
        query.append_pair("resource", resource);
        query.append_pair("state", state);
        query.append_pair("nonce", nonce);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("code_challenge", challenge);
    }
    url.to_string()
}

fn complete_callback(
    gate: Arc<CallbackGate>,
    instance_id: ProviderInstanceId,
    redirect_uri: String,
    state: String,
) -> CompleteFn {
    Arc::new(move |callback_url| {
        let gate = Arc::clone(&gate);
        let instance_id = instance_id.clone();
        let redirect_uri = redirect_uri.clone();
        let state = state.clone();
        Box::pin(async move {
            let returned =
                codex_callback_url(&callback_url, &redirect_uri, &state).map_err(|_| {
                    store::failure(
                        &instance_id,
                        "complete",
                        "This redirect URL does not belong to the active ChatGPT sign-in.",
                    )
                })?;
            if !gate.claim(Incoming {
                url: returned,
                reply: None,
                done: None,
            }) {
                return Err(store::failure(
                    &instance_id,
                    "complete",
                    "This redirect URL does not belong to the active ChatGPT sign-in.",
                ));
            }
            Ok(())
        })
    })
}

struct CallbackListen {
    port: u16,
    /// Held so the listener task is aborted when sign-in finishes.
    #[allow(dead_code)]
    server: CallbackServer,
}

async fn spawn_callback_server(
    gate: Arc<CallbackGate>,
    instance_id: &ProviderInstanceId,
) -> Result<CallbackListen, ProviderSetupError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|_| {
        store::failure(
            instance_id,
            "callback",
            "Could not start the local sign-in callback. Try again.",
        )
    })?;
    let port = listener
        .local_addr()
        .map_err(|_| {
            store::failure(
                instance_id,
                "callback",
                "Could not start the local sign-in callback.",
            )
        })?
        .port();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_callback_server(listener, gate, shutdown_rx));
    Ok(CallbackListen {
        port,
        server: CallbackServer {
            shutdown: Some(shutdown_tx),
            task: Some(task),
        },
    })
}

async fn run_callback_server(
    listener: TcpListener,
    gate: Arc<CallbackGate>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut joins = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else { break };
                let gate = Arc::clone(&gate);
                joins.spawn(async move {
                    let _ = handle_callback(socket, gate).await;
                });
            }
        }
    }
    joins.abort_all();
}

async fn handle_callback(
    mut socket: tokio::net::TcpStream,
    gate: Arc<CallbackGate>,
) -> std::io::Result<()> {
    let request = mini_http::read_request(&mut socket).await?;
    let target = request.target.clone();
    let path = target.split('?').next().unwrap_or("/");
    if request.method != "GET" || path != "/auth/callback" {
        mini_http::write_response(&mut socket, 404, "Not Found", &[], b"").await?;
        return Ok(());
    }
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .unwrap_or_else(|_| Url::parse("http://127.0.0.1/auth/callback").expect("static url"));
    let (page_tx, page_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel();
    if !gate.claim(Incoming {
        url,
        reply: Some(page_tx),
        done: Some(done_rx),
    }) {
        mini_http::write_response(
            &mut socket,
            410,
            "Gone",
            &[],
            b"Sign-in is no longer active.",
        )
        .await?;
        return Ok(());
    }
    let Ok(page) = page_rx.await else {
        let _ = done_tx.send(());
        return Ok(());
    };
    let body = codex_auth_callback_page(page.success, page.return_url.as_deref(), &page.nonce);
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-{}'; base-uri 'none'; frame-ancestors 'none'",
        page.nonce
    );
    let _ = mini_http::write_response(
        &mut socket,
        200,
        "OK",
        &[
            ("content-type", "text/html; charset=utf-8"),
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("content-security-policy", &csp),
            ("x-content-type-options", "nosniff"),
        ],
        body.as_bytes(),
    )
    .await;
    let _ = done_tx.send(());
    Ok(())
}

fn discovery_is_trusted(doc: &DiscoveryDoc) -> bool {
    if doc.issuer != TRUSTED_ISSUER {
        return false;
    }
    let mut urls = vec![
        doc.authorization_endpoint.as_str(),
        doc.token_endpoint.as_str(),
        doc.jwks_uri.as_str(),
    ];
    if let Some(revocation) = doc.revocation_endpoint.as_deref() {
        urls.push(revocation);
    }
    urls.iter()
        .all(|url| url_origin(url).as_deref() == Some(doc.issuer.as_str()))
}

fn url_origin(value: &str) -> Option<String> {
    Some(Url::parse(value).ok()?.origin().ascii_serialization())
}

fn decode_tokens(raw: &Value) -> Result<Tokens, ()> {
    let obj = raw.as_object().ok_or(())?;
    let access_token = required_non_empty(obj, "access_token")?;
    let refresh_token = optional_non_empty(obj, "refresh_token")?;
    let id_token = optional_string(obj, "id_token")?;
    let token_type = required_string(obj, "token_type")?;
    let expires_in = match obj.get("expires_in") {
        Some(Value::Number(number)) => {
            let value = number.as_i64().ok_or(())?;
            if value <= 0 {
                return Err(());
            }
            value
        }
        _ => return Err(()),
    };
    let scope = required_string(obj, "scope")?;
    let earliest_refresh_at = match obj.get("earliest_refresh_at") {
        None => None,
        Some(Value::Number(number)) => {
            let value = number
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or(())?;
            Some(Earliest::Number(value))
        }
        Some(Value::String(value)) => Some(Earliest::Text(value.clone())),
        Some(_) => return Err(()),
    };
    Ok(Tokens {
        access_token,
        refresh_token,
        id_token,
        token_type,
        expires_in,
        scope,
        earliest_refresh_at,
    })
}

fn required_non_empty(obj: &Map<String, Value>, key: &str) -> Result<String, ()> {
    match obj.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(()),
    }
}

fn required_string(obj: &Map<String, Value>, key: &str) -> Result<String, ()> {
    match obj.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(()),
    }
}

fn optional_non_empty(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => Err(()),
    }
}

fn optional_string(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(()),
    }
}

fn earliest_millis(value: Option<Earliest>) -> Option<f64> {
    let time = match value? {
        Earliest::Number(seconds) => seconds * 1000.0,
        Earliest::Text(text) => chrono::DateTime::parse_from_rfc3339(&text)
            .ok()
            .map(|time| time.timestamp_millis() as f64)?,
    };
    time.is_finite().then_some(time)
}

fn split_scopes(scope: &str) -> Vec<String> {
    scope.split_whitespace().map(str::to_owned).collect()
}

fn encode_form(pairs: &[(String, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn form_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn query_param(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

fn redirect_identity_differs(original: &str, current: &str) -> bool {
    let Ok(original) = Url::parse(original) else {
        return true;
    };
    let Ok(current) = Url::parse(current) else {
        return true;
    };
    original.scheme() != current.scheme()
        || original.host_str() != current.host_str()
        || original.path() != current.path()
}

fn kid_matches(key: &Jwk, header: &jsonwebtoken::Header) -> bool {
    match &header.kid {
        Some(kid) => key.kid.as_deref() == Some(kid.as_str()),
        None => true,
    }
}

fn decoding_key(key: &Jwk) -> Option<DecodingKey> {
    match key.kty.as_str() {
        "RSA" => {
            let n = key.n.as_deref()?;
            let e = key.e.as_deref()?;
            DecodingKey::from_rsa_components(n, e).ok()
        }
        "EC" if key.crv.as_deref() == Some("P-256") => {
            DecodingKey::from_ec_components(key.x.as_deref()?, key.y.as_deref()?).ok()
        }
        _ => None,
    }
}

fn email_claim_matches(claim: &Value, email: &Option<String>) -> bool {
    let right = match email {
        Some(email) => Value::String(email.clone()),
        None => Value::Null,
    };
    let left = match claim {
        Value::Null => Value::Null,
        other => other.clone(),
    };
    left == right
}

fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("os entropy");
    URL_SAFE_NO_PAD.encode(buf)
}

fn ephemeral_client_port() -> u16 {
    let mut buf = [0u8; 2];
    getrandom::fill(&mut buf).expect("os entropy");
    49_152 + (u16::from_le_bytes(buf) % 16_384)
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

fn operation_of(error: &ProviderSetupError) -> &str {
    let ProviderSetupError::SetupError { operation, .. } = error;
    operation
}

fn shared_permit(secrets: &Arc<dyn SecretStore>, key: &str) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<SyncMutex<HashMap<(usize, String), Arc<Mutex<()>>>>> =
        LazyLock::new(|| SyncMutex::new(HashMap::new()));
    let id = Arc::as_ptr(secrets) as *const () as usize;
    LOCKS
        .lock()
        .entry((id, key.to_owned()))
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

async fn session_locked<T>(
    inner: &Inner,
    task: impl Future<Output = Result<T, ProviderSetupError>>,
) -> Result<T, ProviderSetupError> {
    session_lock(
        inner.directory.clone(),
        inner.binding_key.clone(),
        &inner.instance_id,
        task,
    )
    .await
}

async fn session_lock<T>(
    directory: Option<PathBuf>,
    key: String,
    instance_id: &ProviderInstanceId,
    task: impl Future<Output = Result<T, ProviderSetupError>>,
) -> Result<T, ProviderSetupError> {
    match with_chatgpt_session_lock(directory.as_deref(), &key, task).await {
        Ok(result) => result,
        Err(CredentialLockError) => Err(store::failure(
            instance_id,
            "credential-lock",
            "Could not lock the ChatGPT connection for an update. Try again.",
        )),
    }
}

async fn uninterruptible<T: Send + 'static>(fut: impl Future<Output = T> + Send + 'static) -> T {
    match tokio::spawn(fut).await {
        Ok(value) => value,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => panic!("uninterruptible section was cancelled by the runtime"),
    }
}

/// Writes the browser page and waits until those bytes have been sent.
async fn deliver_page(
    incoming: &mut Incoming,
    success: bool,
    return_url: Option<String>,
    nonce: &str,
) {
    let Some(reply) = incoming.reply.take() else {
        return;
    };
    let _ = reply.send(CallbackPage {
        success,
        return_url,
        nonce: nonce.to_owned(),
    });
    if let Some(done) = incoming.done.take() {
        let _ = done.await;
    }
}
