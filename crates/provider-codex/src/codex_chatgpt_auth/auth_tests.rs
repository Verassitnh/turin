//! Port of upstream/apps/server/src/provider/CodexChatGptAuth.test.ts.
//!
//! Telemetry cases (`track`, `accountCounts`) are cut. Handoff cases wait on
//! `CodexChatGptHandoff`, which is the next module; import and export are
//! covered here directly.

use std::{
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use indexmap::IndexMap;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use parking_lot::Mutex;
use rand::rngs::OsRng;
use rsa::{RsaPrivateKey, pkcs8::EncodePrivateKey, traits::PublicKeyParts};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_setup::{
    ProviderAuthInteraction, ProviderAuthMethod, ProviderAuthPhase, ProviderAuthState,
};
use turin_provider_core::auth::{ProviderAuthCallbackMode, ProviderAuthCompleteInput};
use turin_provider_core::credential_store::{
    SecretStore, SecretStoreError, provider_credential_key,
};
use url::Url;

use super::{
    CodexChatGptAuth, CodexChatGptAuthOptions, DiscoveryDoc, Earliest, discovery_is_trusted,
    earliest_millis, make,
};
use crate::codex_chatgpt_auth::mini_http;

const DEFAULT_SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

struct Keys {
    signing: EncodingKey,
    untrusted: EncodingKey,
    n: String,
    e: String,
}

static KEYS: LazyLock<Keys> = LazyLock::new(|| {
    let mut rng = OsRng;
    let signing = RsaPrivateKey::new(&mut rng, 2048).expect("rsa");
    let untrusted = RsaPrivateKey::new(&mut rng, 2048).expect("rsa");
    let public = signing.to_public_key();
    Keys {
        n: URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
        e: URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
        signing: EncodingKey::from_rsa_pem(
            signing
                .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap(),
        untrusted: EncodingKey::from_rsa_pem(
            untrusted
                .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap(),
    }
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum IdentityFailure {
    Issuer,
    Audience,
    Signature,
}

struct Mock {
    origin: String,
    authorize: Mutex<Option<Url>>,
    refreshes: AtomicUsize,
    revoked: AtomicBool,
    refresh_error: Mutex<Option<String>>,
    revocation_status: AtomicU16,
    code_error: Mutex<Option<String>>,
    transient: AtomicBool,
    invalid_nonce: AtomicBool,
    identity_failure: Mutex<Option<IdentityFailure>>,
    subject: Mutex<String>,
    email: Mutex<String>,
    grant_scope: Mutex<String>,
    fault: Mutex<Option<String>>,
    exchanges: Mutex<Vec<Vec<(String, String)>>>,
    revocations: Mutex<Vec<Vec<(String, String)>>>,
}

struct OrderedSecrets {
    values: Mutex<IndexMap<String, Vec<u8>>>,
    environment_id: String,
}

impl OrderedSecrets {
    fn new() -> Self {
        Self {
            values: Mutex::new(IndexMap::new()),
            environment_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[async_trait]
impl SecretStore for OrderedSecrets {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(self.values.lock().get(name).cloned())
    }

    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        self.values.lock().insert(name.to_owned(), value.to_vec());
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
        self.values.lock().shift_remove(name);
        Ok(())
    }
}

struct MockGuard {
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockGuard {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

struct Harness {
    auth: CodexChatGptAuth,
    instance: ProviderInstanceId,
    secrets: Arc<OrderedSecrets>,
    mock: Arc<Mock>,
    origin: String,
    _guard: MockGuard,
    requests: Mutex<Vec<Url>>,
    responses: Mutex<Vec<Fetched>>,
    return_url: Mutex<String>,
    mismatched_state: AtomicBool,
    callback_client_id: Mutex<Option<String>>,
}

struct Fetched {
    body: String,
    headers: Vec<(String, String)>,
}

impl Harness {
    async fn start(instance: &str, secrets: Arc<OrderedSecrets>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let origin = format!("http://127.0.0.1:{port}");
        let mock = Arc::new(Mock {
            origin: origin.clone(),
            authorize: Mutex::new(None),
            refreshes: AtomicUsize::new(0),
            revoked: AtomicBool::new(false),
            refresh_error: Mutex::new(None),
            revocation_status: AtomicU16::new(200),
            code_error: Mutex::new(None),
            transient: AtomicBool::new(false),
            invalid_nonce: AtomicBool::new(false),
            identity_failure: Mutex::new(None),
            subject: Mutex::new("user-test".to_owned()),
            email: Mutex::new("hidden@example.test".to_owned()),
            grant_scope: Mutex::new(DEFAULT_SCOPE.to_owned()),
            fault: Mutex::new(None),
            exchanges: Mutex::new(Vec::new()),
            revocations: Mutex::new(Vec::new()),
        });
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(serve_mock(listener, Arc::clone(&mock), shutdown_rx));
        let instance = ProviderInstanceId::new(instance).unwrap();
        let secrets_dyn: Arc<dyn SecretStore> = secrets.clone();
        let auth = make(CodexChatGptAuthOptions {
            instance_id: instance.clone(),
            secrets: secrets_dyn,
            secrets_directory: None,
            environment_id: secrets.environment_id.clone(),
            discovery_url: Some(format!("{origin}/discovery")),
            resource: Some(format!("{origin}/v1")),
            default_return_url: None,
            reconnect_profile: None,
        })
        .await
        .unwrap();
        Self {
            auth,
            instance,
            secrets,
            mock,
            origin,
            _guard: MockGuard {
                shutdown: Some(shutdown),
                task,
            },
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            return_url: Mutex::new("http://localhost:7001/welcome".to_owned()),
            mismatched_state: AtomicBool::new(false),
            callback_client_id: Mutex::new(None),
        }
    }

    fn check(&self) {
        if let Some(fault) = self.mock.fault.lock().take() {
            panic!("oauth mock: {fault}");
        }
    }

    fn prepare(&self, waiting: &ProviderAuthState) -> String {
        let authorize = Url::parse(waiting.authorization_url.as_deref().unwrap()).unwrap();
        self.requests.lock().push(authorize.clone());
        *self.mock.authorize.lock() = Some(authorize.clone());
        let redirect = param(&authorize, "redirect_uri").unwrap();
        let mut callback = Url::parse(&redirect).unwrap();
        let state = if self.mismatched_state.load(Ordering::SeqCst) {
            "unmatched-state".to_owned()
        } else {
            param(&authorize, "state").unwrap()
        };
        let client_id = if let Some(id) = self.callback_client_id.lock().clone() {
            Some(id)
        } else if param(&authorize, "client_id").as_deref() == Some("dynamic_agent_client") {
            Some("oaiapp_test".to_owned())
        } else {
            None
        };
        {
            let mut query = callback.query_pairs_mut();
            query.append_pair("code", "authorization-code");
            query.append_pair("state", &state);
            if let Some(id) = &client_id {
                query.append_pair("client_id", id);
            }
        }
        callback.to_string()
    }

    async fn sign_in(&self, method: Option<&str>) {
        self.begin(method, None).await;
        let waiting = phase(&self.auth, ProviderAuthPhase::Waiting).await;
        assert!(matches!(
            waiting
                .interaction
                .as_ref()
                .and_then(|value| value.as_ref()),
            Some(ProviderAuthInteraction::Browser { .. })
        ));
        let callback = self.prepare(&waiting);
        let response = reqwest::Client::new().get(&callback).send().await.unwrap();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("").to_owned(),
                )
            })
            .collect();
        let body = response.text().await.unwrap();
        self.responses.lock().push(Fetched { body, headers });
        self.check();
    }

    async fn start_remote(&self, method: Option<&str>) -> (ProviderAuthState, String) {
        self.begin(method, Some(ProviderAuthCallbackMode::Client))
            .await;
        let waiting = phase(&self.auth, ProviderAuthPhase::Waiting).await;
        let callback = self.prepare(&waiting);
        (waiting, callback)
    }

    async fn begin(&self, method: Option<&str>, mode: Option<ProviderAuthCallbackMode>) {
        let return_url = self.return_url.lock().clone();
        (self.auth.controller.start)(
            "owner".to_owned(),
            Some(Box::pin(async { Ok(()) })),
            method.map(str::to_owned),
            Some(return_url),
            mode,
        )
        .await
        .unwrap();
    }

    async fn complete(
        &self,
        owner: &str,
        waiting: &ProviderAuthState,
        callback: &str,
    ) -> Result<ProviderAuthState, turin_contracts::provider_setup::ProviderSetupError> {
        (self.auth.controller.complete)(
            owner.to_owned(),
            ProviderAuthCompleteInput {
                flow_id: waiting.flow_id.as_ref().unwrap().as_str().to_owned(),
                callback_url: callback.to_owned(),
            },
        )
        .await
    }

    async fn logout(&self) -> ProviderAuthState {
        (self.auth.controller.logout)(Box::pin(async { Ok(()) }))
            .await
            .unwrap()
    }

    async fn recreate(&self) -> CodexChatGptAuth {
        make(CodexChatGptAuthOptions {
            instance_id: self.instance.clone(),
            secrets: self.secrets.clone(),
            secrets_directory: None,
            environment_id: self.secrets.environment_id.clone(),
            discovery_url: Some(format!("{}/discovery", self.origin)),
            resource: Some(format!("{}/v1", self.origin)),
            default_return_url: None,
            reconnect_profile: None,
        })
        .await
        .unwrap()
    }

    async fn destination(&self) -> (CodexChatGptAuth, Arc<OrderedSecrets>) {
        let secrets = Arc::new(OrderedSecrets {
            values: Mutex::new(IndexMap::new()),
            environment_id: self.secrets.environment_id.clone(),
        });
        let auth = make(CodexChatGptAuthOptions {
            instance_id: self.instance.clone(),
            secrets: secrets.clone(),
            secrets_directory: None,
            environment_id: secrets.environment_id.clone(),
            discovery_url: Some(format!("{}/discovery", self.origin)),
            resource: Some(format!("{}/v1", self.origin)),
            default_return_url: None,
            reconnect_profile: None,
        })
        .await
        .unwrap();
        (auth, secrets)
    }

    async fn seed_expired(&self) {
        let mut record = self.auth.read().await.unwrap().unwrap();
        record.expires_at = 0.0;
        let key = provider_credential_key("codex-chatgpt", self.instance.as_str());
        self.secrets
            .set(&key, &serde_json::to_vec(&record).unwrap())
            .await
            .unwrap();
    }

    fn stored(&self) -> Vec<Value> {
        self.secrets
            .values
            .lock()
            .values()
            .map(|bytes| {
                let record: Value = serde_json::from_slice(bytes).unwrap();
                record
            })
            .flat_map(|record| {
                record
                    .get("sessions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| vec![record])
            })
            .collect()
    }

    fn rewrite_registration(&self, next: impl Fn(Value) -> Option<Value>) {
        let mut values = self.secrets.values.lock();
        let keys: Vec<String> = values.keys().cloned().collect();
        for key in keys {
            let record: Value = serde_json::from_slice(&values[&key]).unwrap();
            if record.get("profiles").is_some()
                || record.get("clientId").is_some() && record.get("accessToken").is_none()
            {
                match next(record) {
                    Some(updated) => {
                        values.insert(key, serde_json::to_vec(&updated).unwrap());
                    }
                    None => {
                        values.shift_remove(&key);
                    }
                }
            }
        }
    }
}

async fn serve_mock(listener: TcpListener, mock: Arc<Mock>, mut shutdown: oneshot::Receiver<()>) {
    let mut joins = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((mut socket, _)) = accepted else { break };
                let mock = Arc::clone(&mock);
                joins.spawn(async move {
                    let Ok(request) = mini_http::read_request(&mut socket).await else {
                        return;
                    };
                    let path = request.target.split('?').next().unwrap_or("/");
                    let (status, body) = route(&mock, path, &request).await;
                    let _ = mini_http::write_response(
                        &mut socket,
                        status,
                        "OK",
                        &[("content-type", "application/json")],
                        body.as_bytes(),
                    )
                    .await;
                });
            }
        }
    }
}

async fn route(mock: &Mock, path: &str, request: &mini_http::MiniRequest) -> (u16, String) {
    match path {
        "/discovery" => (
            200,
            json!({
                "issuer": mock.origin,
                "authorization_endpoint": format!("{}/authorize", mock.origin),
                "token_endpoint": format!("{}/token", mock.origin),
                "jwks_uri": format!("{}/jwks", mock.origin),
                "revocation_endpoint": format!("{}/revoke", mock.origin),
            })
            .to_string(),
        ),
        "/jwks" => (
            200,
            json!({
                "keys": [{
                    "kty": "RSA",
                    "n": KEYS.n,
                    "e": KEYS.e,
                    "kid": "test",
                    "alg": "RS256",
                    "use": "sig",
                }]
            })
            .to_string(),
        ),
        "/revoke" => {
            let text = String::from_utf8_lossy(&request.body);
            mock.revocations.lock().push(form_pairs(&text));
            (mock.revocation_status.load(Ordering::SeqCst), String::new())
        }
        "/token" => token(mock, request).await,
        _ => (404, "{}".to_owned()),
    }
}

async fn token(mock: &Mock, request: &mini_http::MiniRequest) -> (u16, String) {
    let content_type = request.headers.get("content-type").map(String::as_str);
    if content_type != Some("application/x-www-form-urlencoded") {
        fault(mock, format!("content-type {content_type:?}"));
        return (500, "{}".to_owned());
    }
    if request.headers.contains_key("authorization") {
        fault(mock, "authorization header".to_owned());
        return (500, "{}".to_owned());
    }
    let text = String::from_utf8_lossy(&request.body);
    let body = form_pairs(&text);
    if body.iter().any(|(key, _)| key == "client_secret") {
        fault(mock, "client_secret".to_owned());
        return (500, "{}".to_owned());
    }
    if form_get(&body, "resource") != Some(format!("{}/v1", mock.origin)).as_deref() {
        fault(mock, format!("resource {:?}", form_get(&body, "resource")));
        return (500, "{}".to_owned());
    }
    mock.exchanges.lock().push(body.clone());
    if form_get(&body, "grant_type") == Some("refresh_token") {
        let n = mock.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
        if mock.transient.load(Ordering::SeqCst) {
            return (503, json!({"error": "temporarily_unavailable"}).to_string());
        }
        if mock.revoked.load(Ordering::SeqCst) || mock.refresh_error.lock().is_some() {
            let error = mock
                .refresh_error
                .lock()
                .clone()
                .unwrap_or_else(|| "invalid_grant".to_owned());
            return (400, json!({"error": error}).to_string());
        }
        return (
            200,
            json!({
                "access_token": format!("access-{n}"),
                "refresh_token": format!("refresh-{n}"),
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": mock.grant_scope.lock().clone(),
            })
            .to_string(),
        );
    }
    if let Some(error) = mock.code_error.lock().clone() {
        return (400, json!({"error": error}).to_string());
    }
    let Some(authorize) = mock.authorize.lock().clone() else {
        return (400, "{}".to_owned());
    };
    if form_get(&body, "redirect_uri") != param(&authorize, "redirect_uri").as_deref() {
        // Host and path must match; the port is part of the exact URI too.
        let actual = form_get(&body, "redirect_uri").unwrap_or("");
        let expected = param(&authorize, "redirect_uri").unwrap_or_default();
        if !same_callback(actual, &expected) || actual != expected {
            fault(mock, format!("redirect {actual} != {expected}"));
            return (500, "{}".to_owned());
        }
    }
    let verifier = form_get(&body, "code_verifier").unwrap_or("");
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if Some(challenge.as_str()) != param(&authorize, "code_challenge").as_deref() {
        fault(mock, "pkce".to_owned());
        return (500, "{}".to_owned());
    }
    let client_id = form_get(&body, "client_id").unwrap_or("");
    let token = sign(mock, &authorize, client_id);
    (
        200,
        json!({
            "access_token": "initial-access",
            "refresh_token": "initial-refresh",
            "id_token": token,
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": mock.grant_scope.lock().clone(),
        })
        .to_string(),
    )
}

fn sign(mock: &Mock, authorize: &Url, client_id: &str) -> String {
    let nonce = if mock.invalid_nonce.load(Ordering::SeqCst) {
        "incorrect".to_owned()
    } else {
        param(authorize, "nonce").unwrap()
    };
    let failure = *mock.identity_failure.lock();
    let iss = if failure == Some(IdentityFailure::Issuer) {
        "https://untrusted-issuer.test".to_owned()
    } else {
        mock.origin.clone()
    };
    let aud = if failure == Some(IdentityFailure::Audience) {
        "oaiapp_untrusted_audience".to_owned()
    } else {
        client_id.to_owned()
    };
    let key = if failure == Some(IdentityFailure::Signature) {
        &KEYS.untrusted
    } else {
        &KEYS.signing
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({
        "nonce": nonce,
        "email": mock.email.lock().clone(),
        "iss": iss,
        "aud": aud,
        "sub": mock.subject.lock().clone(),
        "iat": now,
        "exp": now + 3600,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test".to_owned());
    encode(&header, &claims, key).unwrap()
}

fn fault(mock: &Mock, message: String) {
    *mock.fault.lock() = Some(message);
}

fn form_pairs(body: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn form_get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn param(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

fn same_callback(actual: &str, expected: &str) -> bool {
    let Ok(left) = Url::parse(actual) else {
        return false;
    };
    let Ok(right) = Url::parse(expected) else {
        return false;
    };
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.path() == right.path()
}

fn assert_same_callback(actual: &str, expected: &str) {
    assert!(same_callback(actual, expected), "{actual} vs {expected}");
}

fn sorted_keys(url: &Url) -> Vec<String> {
    let mut keys: Vec<String> = url.query_pairs().map(|(key, _)| key.into_owned()).collect();
    keys.sort();
    keys
}

async fn phase(auth: &CodexChatGptAuth, want: ProviderAuthPhase) -> ProviderAuthState {
    let mut stream = std::pin::Pin::from((auth.controller.subscribe)("owner".to_owned()));
    let wait = async {
        loop {
            let next = futures::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
            match next {
                Some(state) if state.phase == want => return state,
                Some(_) => {}
                None => panic!("auth subscription ended before {want:?}"),
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(20), wait).await {
        Ok(state) => state,
        Err(_) => panic!("timed out waiting for {want:?}"),
    }
}

fn method<'a>(state: &'a ProviderAuthState, id: &str) -> &'a ProviderAuthMethod {
    state
        .methods
        .as_ref()
        .unwrap()
        .iter()
        .find(|method| method.id.as_str() == id)
        .unwrap_or_else(|| panic!("missing method {id}"))
}

fn header<'a>(fetched: &'a Fetched, name: &str) -> &'a str {
    fetched
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}

fn profiles(records: &[Value]) -> Vec<Value> {
    records
        .iter()
        .find_map(|record| record.get("profiles").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn with_access(records: &[Value]) -> Value {
    records
        .iter()
        .find(|record| record.get("accessToken").and_then(Value::as_str).is_some())
        .cloned()
        .unwrap()
}

async fn succeeded(h: &Harness) {
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    h.check();
}

#[tokio::test]
async fn changing_account_registers_a_new_client_then_reuses_it() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    *h.callback_client_id.lock() = Some("oaiapp_other_account".to_owned());
    *h.mock.subject.lock() = "other-user".to_owned();
    *h.mock.email.lock() = "other@example.test".to_owned();
    h.sign_in(Some("chatgpt-change-account")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(
        form_get(&h.mock.exchanges.lock()[1], "client_id"),
        Some("oaiapp_other_account")
    );
    let saved = h.auth.read().await.unwrap().unwrap();
    assert_eq!(saved.client_id, "oaiapp_other_account");
    assert_eq!(saved.subject, "other-user");
    assert_eq!(saved.email.as_deref(), Some("other@example.test"));
    let redirect = param(&h.requests.lock()[1], "redirect_uri").unwrap();
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[2], "client_id").as_deref(),
        Some("oaiapp_other_account")
    );
    assert_same_callback(
        param(&h.requests.lock()[2], "redirect_uri")
            .as_deref()
            .unwrap(),
        &redirect,
    );
}

#[tokio::test]
async fn failed_account_change_preserves_the_original_connection() {
    for failure in ["identity", "sharing"] {
        let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
        succeeded(&h).await;
        let before = h.stored();
        *h.callback_client_id.lock() = Some("oaiapp_other_account".to_owned());
        if failure == "identity" {
            h.mock.invalid_nonce.store(true, Ordering::SeqCst);
        } else {
            *h.mock.grant_scope.lock() = "openid profile email".to_owned();
        }
        h.sign_in(Some("chatgpt-change-account")).await;
        phase(&h.auth, ProviderAuthPhase::Failed).await;
        assert_eq!(
            param(&h.requests.lock()[1], "client_id").as_deref(),
            Some("dynamic_agent_client")
        );
        if failure == "sharing" {
            let read = serde_json::to_value(h.auth.read().await.unwrap().unwrap()).unwrap();
            assert_eq!(read, with_access(&before));
            assert_eq!(profiles(&h.stored()).len(), 2);
        } else {
            assert_eq!(h.stored(), before);
        }
    }
}

#[tokio::test]
async fn retains_callback_host_and_path_after_recreation_and_token_removal() {
    let secrets = Arc::new(OrderedSecrets::new());
    let first = Harness::start("managed-codex-test", secrets.clone()).await;
    succeeded(&first).await;
    let redirect = param(&first.requests.lock()[0], "redirect_uri").unwrap();
    first.auth.revoke().await.unwrap();
    let restarted = Harness::start("managed-codex-test", secrets).await;
    restarted.sign_in(None).await;
    phase(&restarted.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&restarted.requests.lock()[0], "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_same_callback(
        param(&restarted.requests.lock()[0], "redirect_uri")
            .as_deref()
            .unwrap(),
        &redirect,
    );
    assert_eq!(
        form_get(&restarted.mock.exchanges.lock()[0], "redirect_uri"),
        param(&restarted.requests.lock()[0], "redirect_uri").as_deref()
    );
}

#[tokio::test]
async fn reauthorizes_on_an_available_port_when_the_original_is_occupied() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    let port = Url::parse(
        param(&h.requests.lock()[0], "redirect_uri")
            .as_deref()
            .unwrap(),
    )
    .unwrap()
    .port()
    .unwrap();
    let _occupied = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    let next = Url::parse(
        param(&h.requests.lock()[1], "redirect_uri")
            .as_deref()
            .unwrap(),
    )
    .unwrap()
    .port();
    assert_ne!(next, Some(port));
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_eq!(h.mock.exchanges.lock().len(), 2);
}

#[tokio::test]
async fn registers_a_fresh_client_when_the_registration_is_gone() {
    let secrets = Arc::new(OrderedSecrets::new());
    let h = Harness::start("managed-codex-test", secrets).await;
    succeeded(&h).await;
    h.rewrite_registration(|_| None);
    *h.callback_client_id.lock() = Some("oaiapp_replacement".to_owned());
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(
        form_get(&h.mock.exchanges.lock()[1], "client_id"),
        Some("oaiapp_replacement")
    );
    assert_eq!(
        h.auth.read().await.unwrap().unwrap().client_id,
        "oaiapp_replacement"
    );
}

#[tokio::test]
async fn reauthorizes_a_registration_without_persisting_its_original_port() {
    let secrets = Arc::new(OrderedSecrets::new());
    let h = Harness::start("managed-codex-test", secrets).await;
    succeeded(&h).await;
    h.auth.revoke().await.unwrap();
    h.rewrite_registration(|_| Some(json!({"clientId": "oaiapp_test"})));
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_eq!(
        Url::parse(form_get(&h.mock.exchanges.lock()[1], "redirect_uri").unwrap())
            .unwrap()
            .host_str(),
        Some("127.0.0.1")
    );
}

#[tokio::test]
async fn separate_accounts_register_independently() {
    let secrets = Arc::new(OrderedSecrets::new());
    let personal = Harness::start("codex_personal", secrets.clone()).await;
    let work = Harness::start("codex_work", secrets).await;
    succeeded(&personal).await;
    let personal_credentials = personal.auth.read().await.unwrap().unwrap();
    assert!(work.auth.read().await.unwrap().is_none());
    succeeded(&work).await;
    assert_eq!(
        param(&work.requests.lock()[0], "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_ne!(
        personal
            .auth
            .controller
            .credential_binding
            .as_ref()
            .map(|b| b.key.as_str()),
        work.auth
            .controller
            .credential_binding
            .as_ref()
            .map(|b| b.key.as_str())
    );
    work.logout().await;
    assert!(work.auth.read().await.unwrap().is_none());
    assert_eq!(
        personal.auth.read().await.unwrap().unwrap(),
        personal_credentials
    );
}

#[tokio::test]
async fn first_authorization_and_reauthorization_match_the_guide() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    assert!(
        h.responses.lock()[0]
            .body
            .contains("content=\"1;url=http://localhost:7001/welcome\"")
    );
    let requests = h.requests.lock().clone();
    let first = &requests[0];
    assert_eq!(
        param(first, "ext_agent_host_id").as_deref(),
        Some(format!("urn:uuid:{}", h.secrets.environment_id)).as_deref()
    );
    assert_eq!(first.origin().ascii_serialization(), h.origin);
    assert_eq!(first.path(), "/authorize");
    assert_eq!(
        sorted_keys(first),
        [
            "agent_name_hint",
            "client_id",
            "code_challenge",
            "code_challenge_method",
            "ext_agent_host_id",
            "nonce",
            "redirect_uri",
            "resource",
            "response_type",
            "scope",
            "state",
        ]
    );
    assert_eq!(
        param(first, "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(param(first, "agent_name_hint").as_deref(), Some("T3 Code"));
    assert_eq!(param(first, "response_type").as_deref(), Some("code"));
    assert_eq!(param(first, "scope").as_deref(), Some(DEFAULT_SCOPE));
    assert_eq!(
        param(first, "resource").as_deref(),
        Some(format!("{}/v1", h.origin)).as_deref()
    );
    assert_eq!(
        param(first, "code_challenge_method").as_deref(),
        Some("S256")
    );
    let callback = Url::parse(param(first, "redirect_uri").as_deref().unwrap()).unwrap();
    assert_eq!(callback.scheme(), "http");
    assert_eq!(callback.host_str(), Some("127.0.0.1"));
    assert_eq!(callback.path(), "/auth/callback");
    assert!(callback.port().unwrap() > 0);
    for key in ["state", "nonce", "code_challenge"] {
        let value = param(first, key).unwrap();
        assert!(
            value.len() == 43
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{value}"
        );
    }
    assert_ne!(param(first, "state"), param(first, "nonce"));
    let original = h.auth.read().await.unwrap().unwrap().id_token.unwrap();
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    let second = h.requests.lock()[1].clone();
    assert_eq!(param(&second, "client_id").as_deref(), Some("oaiapp_test"));
    assert!(param(&second, "agent_name_hint").is_none());
    assert_eq!(
        param(&second, "login_hint").as_deref(),
        Some("hidden@example.test")
    );
    assert_eq!(
        param(&second, "id_token_hint").as_deref(),
        Some(original.as_str())
    );
    assert_eq!(
        param(&second, "ext_agent_host_id"),
        param(first, "ext_agent_host_id")
    );
    assert_same_callback(
        param(&second, "redirect_uri").as_deref().unwrap(),
        param(first, "redirect_uri").as_deref().unwrap(),
    );
    let mut expected = sorted_keys(first);
    expected.retain(|key| key != "agent_name_hint");
    expected.extend(["login_hint".to_owned(), "id_token_hint".to_owned()]);
    expected.sort();
    assert_eq!(sorted_keys(&second), expected);
    for key in ["state", "nonce", "code_challenge"] {
        assert_ne!(param(first, key), param(&second, key));
    }
    h.logout().await;
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    let reconnect = h.requests.lock()[2].clone();
    assert_eq!(
        param(&reconnect, "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_same_callback(
        param(&reconnect, "redirect_uri").as_deref().unwrap(),
        param(first, "redirect_uri").as_deref().unwrap(),
    );
    assert!(param(&reconnect, "agent_name_hint").is_none());
    assert!(param(&reconnect, "id_token_hint").is_none());
    assert_eq!(
        param(&reconnect, "login_hint").as_deref(),
        Some("hidden@example.test")
    );
    assert_eq!(
        param(&reconnect, "ext_agent_host_id"),
        param(first, "ext_agent_host_id")
    );
    assert_eq!(
        form_get(&h.mock.exchanges.lock()[2], "redirect_uri"),
        param(&reconnect, "redirect_uri").as_deref()
    );
}

#[tokio::test]
async fn returns_to_the_client_only_after_credentials_are_saved() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *h.return_url.lock() =
        "http://localhost:7001/settings/providers?instanceId=codex_work&code=secret-code"
            .to_owned();
    h.sign_in(None).await;
    assert!(h.auth.read().await.unwrap().is_some());
    let response = h.responses.lock().remove(0);
    assert!(header(&response, "content-type").contains("text/html"));
    assert_eq!(header(&response, "cache-control"), "no-store");
    assert_eq!(header(&response, "referrer-policy"), "no-referrer");
    assert!(header(&response, "content-security-policy").contains("default-src 'none'"));
    assert!(response.body.contains("You&#39;re signed in"));
    assert!(response.body.contains(
        "content=\"1;url=http://localhost:7001/settings/providers?instanceId=codex_work\""
    ));
    assert!(!response.body.contains("secret-code"));
    assert!(
        response
            .body
            .contains("history.replaceState(null,\"\",\"/auth/callback\")")
    );
    assert!(!response.body.contains("authorization-code"));
    assert!(!response.body.contains("initial-access"));
    assert!(!response.body.contains("hidden@example.test"));
}

#[tokio::test]
async fn preserves_the_welcome_agents_step() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *h.return_url.lock() =
        "http://localhost:7001/welcome?code=never-forward#agents:test-environment".to_owned();
    h.sign_in(None).await;
    let body = h.responses.lock()[0].body.clone();
    assert!(
        body.contains("content=\"1;url=http://localhost:7001/welcome#agents:test-environment\"")
    );
    assert!(!body.contains("never-forward"));
}

#[tokio::test]
async fn does_not_redirect_to_an_arbitrary_url_after_verification_fails() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *h.return_url.lock() = "https://attacker.example/welcome".to_owned();
    h.mock.invalid_nonce.store(true, Ordering::SeqCst);
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Failed).await;
    let body = h.responses.lock()[0].body.clone();
    assert!(body.contains("Sign-in couldn&#39;t finish"));
    assert!(!body.contains("http-equiv=\"refresh\""));
    assert!(!body.contains("attacker.example"));
    assert!(h.auth.read().await.unwrap().is_none());
}

#[tokio::test]
async fn verifies_pkce_and_serializes_concurrent_refresh() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    assert_eq!(h.auth.read().await.unwrap().unwrap().subject, "user-test");
    assert_eq!(
        form_get(&h.mock.exchanges.lock()[0], "client_id"),
        Some("oaiapp_test")
    );
    h.seed_expired().await;
    let (left, right) = tokio::join!(h.auth.access(), h.auth.access());
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(h.mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(left.access_token, "access-1");
    assert_eq!(right.refresh_token.as_deref(), Some("refresh-1"));
    assert!(form_get(&h.mock.exchanges.lock()[1], "scope").is_none());
    h.logout().await;
    assert!(h.auth.read().await.unwrap().is_none());
}

#[tokio::test]
async fn rejects_an_invalid_nonce_without_saving() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    h.mock.invalid_nonce.store(true, Ordering::SeqCst);
    h.sign_in(None).await;
    let failed = phase(&h.auth, ProviderAuthPhase::Failed).await;
    assert!(failed.message.unwrap().contains("could not be verified"));
    assert!(h.auth.read().await.unwrap().is_none());
}

#[tokio::test]
async fn declined_sharing_keeps_identity_but_blocks_access() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *h.mock.grant_scope.lock() = "openid profile email".to_owned();
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Failed).await;
    assert!(h.auth.read().await.unwrap().is_some());
    assert!(h.auth.access().await.is_err());
    assert_eq!(h.mock.refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn revoked_refresh_is_cleared_and_not_replayed() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    h.seed_expired().await;
    h.mock.revoked.store(true, Ordering::SeqCst);
    assert!(h.auth.access().await.is_err());
    assert!(h.auth.access().await.is_err());
    assert_eq!(h.mock.refreshes.load(Ordering::SeqCst), 1);
    assert!(h.auth.read().await.unwrap().is_none());
}

#[tokio::test]
async fn temporary_renewal_failure_preserves_credentials() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    h.seed_expired().await;
    h.mock.transient.store(true, Ordering::SeqCst);
    assert!(h.auth.access().await.is_err());
    assert_eq!(
        h.auth
            .read()
            .await
            .unwrap()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("initial-refresh")
    );
    h.mock.transient.store(false, Ordering::SeqCst);
    let renewed = h.auth.access().await.unwrap();
    assert_eq!(renewed.refresh_token.as_deref(), Some("refresh-2"));
    assert_eq!(
        form_get(h.mock.exchanges.lock().last().unwrap(), "refresh_token"),
        Some("initial-refresh")
    );
}

#[tokio::test]
async fn disconnect_keeps_the_profile_and_a_different_account_registers_fresh() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    let redirect = param(&h.requests.lock()[0], "redirect_uri").unwrap();
    h.logout().await;
    assert!(h.auth.read().await.unwrap().is_none());
    assert_eq!(
        h.stored(),
        vec![json!({
            "profiles": [{
                "clientId": "oaiapp_test",
                "connectionLabel": "Connection 1",
                "sharingEnabled": true,
                "redirectUri": redirect,
                "subject": "user-test",
                "email": "hidden@example.test",
            }],
            "lastClientId": "oaiapp_test",
        })]
    );
    let disconnected = phase(&h.auth, ProviderAuthPhase::Idle).await;
    assert!(
        method(&disconnected, "chatgpt")
            .description
            .as_deref()
            .unwrap()
            .contains("hidden@example.test")
    );
    assert_eq!(
        method(&disconnected, "chatgpt")
            .account_email
            .as_ref()
            .map(|v| v.as_str()),
        Some("hidden@example.test")
    );
    assert_eq!(
        method(&disconnected, "chatgpt-profile:oaiapp_test")
            .account_email
            .as_ref()
            .map(|v| v.as_str()),
        Some("hidden@example.test")
    );
    assert!(h.auth.access().await.is_err());
    assert_eq!(h.mock.refreshes.load(Ordering::SeqCst), 0);
    *h.callback_client_id.lock() = Some("oaiapp_different_workspace".to_owned());
    *h.mock.subject.lock() = "different-user".to_owned();
    *h.mock.email.lock() = "different@example.test".to_owned();
    h.sign_in(Some("chatgpt-change-account")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(
        form_get(&h.mock.exchanges.lock()[1], "client_id"),
        Some("oaiapp_different_workspace")
    );
    assert_eq!(
        h.auth.read().await.unwrap().unwrap().subject,
        "different-user"
    );
}

#[tokio::test]
async fn revoked_connection_keeps_the_registration() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    let redirect = param(&h.requests.lock()[0], "redirect_uri").unwrap();
    h.seed_expired().await;
    h.mock.revoked.store(true, Ordering::SeqCst);
    assert!(h.auth.access().await.is_err());
    assert!(h.auth.read().await.unwrap().is_none());
    assert_eq!(
        h.stored(),
        vec![json!({
            "profiles": [{
                "clientId": "oaiapp_test",
                "connectionLabel": "Connection 1",
                "sharingEnabled": true,
                "redirectUri": redirect,
                "subject": "user-test",
                "email": "hidden@example.test",
            }],
            "lastClientId": "oaiapp_test",
        })]
    );
    h.mock.revoked.store(false, Ordering::SeqCst);
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("oaiapp_test")
    );
}

#[tokio::test]
async fn fresh_sign_in_ignores_a_registration_left_by_revoke() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    h.auth.revoke().await.unwrap();
    assert!(h.auth.read().await.unwrap().is_none());
    assert_eq!(h.stored().len(), 1);
    *h.callback_client_id.lock() = Some("oaiapp_new_workspace".to_owned());
    *h.mock.subject.lock() = "new-workspace-user".to_owned();
    *h.mock.email.lock() = "new@example.test".to_owned();
    h.sign_in(Some("chatgpt-change-account")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    let saved = h.auth.read().await.unwrap().unwrap();
    assert_eq!(saved.client_id, "oaiapp_new_workspace");
    assert_eq!(saved.subject, "new-workspace-user");
}

#[tokio::test]
async fn mismatched_callback_state_exchanges_nothing() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    h.mismatched_state.store(true, Ordering::SeqCst);
    h.sign_in(None).await;
    let failed = phase(&h.auth, ProviderAuthPhase::Failed).await;
    assert!(failed.message.unwrap().contains("could not be verified"));
    assert!(h.mock.exchanges.lock().is_empty());
    assert!(h.stored().is_empty());
    assert!(h.auth.read().await.unwrap().is_none());
}

#[tokio::test]
async fn identity_verification_failures_save_nothing() {
    for failure in [
        IdentityFailure::Issuer,
        IdentityFailure::Audience,
        IdentityFailure::Signature,
    ] {
        let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
        *h.mock.identity_failure.lock() = Some(failure);
        h.sign_in(None).await;
        let failed = phase(&h.auth, ProviderAuthPhase::Failed).await;
        assert!(failed.message.unwrap().contains("could not be verified"));
        assert_eq!(h.mock.exchanges.lock().len(), 1);
        assert!(h.stored().is_empty());
        assert!(h.auth.read().await.unwrap().is_none());
    }
}

#[tokio::test]
async fn conflicting_callback_client_id_is_rejected() {
    for revoked in [false, true] {
        let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
        succeeded(&h).await;
        if revoked {
            h.auth.revoke().await.unwrap();
        }
        let before = h.stored();
        *h.callback_client_id.lock() = Some("oaiapp_untrusted_callback".to_owned());
        h.sign_in(None).await;
        let failed = phase(&h.auth, ProviderAuthPhase::Failed).await;
        assert!(
            failed
                .message
                .unwrap()
                .contains("registration is incomplete")
        );
        assert_eq!(
            param(&h.requests.lock()[1], "client_id").as_deref(),
            Some("oaiapp_test")
        );
        assert_eq!(h.mock.exchanges.lock().len(), 1);
        assert_eq!(h.stored(), before);
        assert_eq!(h.auth.read().await.unwrap().is_none(), revoked);
    }
}

#[tokio::test]
async fn desktop_sign_in_returns_to_the_welcome_step() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *h.return_url.lock() = "t3code-dev://app/welcome#agents:test-environment".to_owned();
    succeeded(&h).await;
    let body = h.responses.lock()[0].body.clone();
    assert!(body.contains("content=\"1;url=t3code-dev://app/welcome#agents:test-environment\""));
    assert!(body.contains("href=\"t3code-dev://app/welcome#agents:test-environment\""));
}

#[tokio::test]
async fn remote_sign_in_reuses_the_callback() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    let (waiting, callback) = h.start_remote(None).await;
    let interaction = waiting.interaction.clone().unwrap().unwrap();
    assert!(matches!(
        interaction,
        ProviderAuthInteraction::Browser {
            accepts_callback: Some(true),
            ..
        }
    ));
    h.complete("owner", &waiting, &callback).await.unwrap();
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        h.auth.read().await.unwrap().unwrap().email.as_deref(),
        Some("hidden@example.test")
    );
    let (second, second_callback) = h.start_remote(None).await;
    assert_same_callback(&second_callback, &callback);
    let url = Url::parse(second.authorization_url.as_deref().unwrap()).unwrap();
    assert_eq!(param(&url, "client_id").as_deref(), Some("oaiapp_test"));
    h.complete("owner", &second, &second_callback)
        .await
        .unwrap();
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(h.mock.exchanges.lock().len(), 2);
}

#[tokio::test]
async fn foreign_callbacks_do_not_consume_the_sign_in() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    let (waiting, callback) = h.start_remote(None).await;
    assert!(h.auth.read().await.unwrap().is_none());
    assert!(
        h.complete("other-client", &waiting, &callback)
            .await
            .is_err()
    );
    let mut wrong_port = Url::parse(&callback).unwrap();
    let port = if wrong_port.port() == Some(65535) {
        65534
    } else {
        65535
    };
    wrong_port.set_port(Some(port)).unwrap();
    let invalid = [
        wrong_port.to_string(),
        callback.replace("/auth/callback", "/other"),
        format!("{callback}&state=foreign"),
        format!("{callback}&code=duplicate"),
        format!("{callback}&error=access_denied"),
        callback.replace("127.0.0.1", "attacker.example"),
    ];
    for callback_url in invalid {
        assert!(
            h.complete("owner", &waiting, &callback_url).await.is_err(),
            "{callback_url}"
        );
    }
    assert!(h.mock.exchanges.lock().is_empty());
    assert!(h.auth.read().await.unwrap().is_none());
    h.complete("owner", &waiting, &callback).await.unwrap();
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
}

#[tokio::test]
async fn simultaneous_remote_accounts_stay_independent() {
    let first = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    let second = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *second.mock.subject.lock() = "other-user".to_owned();
    *second.mock.email.lock() = "other@example.test".to_owned();
    *second.callback_client_id.lock() = Some("oaiapp_other".to_owned());
    let (a, a_callback) = first.start_remote(None).await;
    let (b, b_callback) = second.start_remote(None).await;
    let a_url = Url::parse(a.authorization_url.as_deref().unwrap()).unwrap();
    let b_url = Url::parse(b.authorization_url.as_deref().unwrap()).unwrap();
    assert_ne!(
        param(&a_url, "ext_agent_host_id"),
        param(&b_url, "ext_agent_host_id")
    );
    assert!(first.complete("owner", &a, &b_callback).await.is_err());
    first.complete("owner", &a, &a_callback).await.unwrap();
    second.complete("owner", &b, &b_callback).await.unwrap();
    phase(&first.auth, ProviderAuthPhase::Succeeded).await;
    phase(&second.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        first.auth.read().await.unwrap().unwrap().subject,
        "user-test"
    );
    assert_eq!(
        second.auth.read().await.unwrap().unwrap().subject,
        "other-user"
    );
}

#[tokio::test]
async fn reconnect_after_disconnect_reuses_client_and_host() {
    let secrets = Arc::new(OrderedSecrets::new());
    let first = Harness::start("managed-codex-test", secrets.clone()).await;
    succeeded(&first).await;
    let redirect = param(&first.requests.lock()[0], "redirect_uri").unwrap();
    first.logout().await;
    let restarted = Harness::start("managed-codex-test", secrets).await;
    restarted.sign_in(None).await;
    phase(&restarted.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&restarted.requests.lock()[0], "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_same_callback(
        param(&restarted.requests.lock()[0], "redirect_uri")
            .as_deref()
            .unwrap(),
        &redirect,
    );
    assert_eq!(
        form_get(&restarted.mock.exchanges.lock()[0], "redirect_uri"),
        param(&restarted.requests.lock()[0], "redirect_uri").as_deref()
    );
    assert_eq!(
        restarted.auth.read().await.unwrap().unwrap().subject,
        "user-test"
    );
}

#[tokio::test]
async fn a_different_identity_is_rejected_on_reauth() {
    for disconnected in [false, true] {
        let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
        succeeded(&h).await;
        if disconnected {
            h.logout().await;
        }
        let before = h.stored();
        *h.mock.subject.lock() = "another-user".to_owned();
        *h.mock.email.lock() = "another@example.test".to_owned();
        h.sign_in(None).await;
        let failed = phase(&h.auth, ProviderAuthPhase::Failed).await;
        assert!(
            failed
                .message
                .unwrap()
                .contains("different ChatGPT account")
        );
        assert_eq!(h.stored(), before);
        assert_eq!(h.auth.read().await.unwrap().is_none(), disconnected);
    }
}

#[tokio::test]
async fn both_profiles_stay_available_when_returning_to_the_original() {
    let secrets = Arc::new(OrderedSecrets::new());
    let first = Harness::start("managed-codex-test", secrets.clone()).await;
    succeeded(&first).await;
    let redirect = param(&first.requests.lock()[0], "redirect_uri").unwrap();
    *first.mock.subject.lock() = "other-user".to_owned();
    *first.mock.email.lock() = "other@example.test".to_owned();
    *first.callback_client_id.lock() = Some("oaiapp_other_account".to_owned());
    first.sign_in(Some("chatgpt-change-account")).await;
    let changed = phase(&first.auth, ProviderAuthPhase::Succeeded).await;
    let ids: Vec<_> = changed
        .methods
        .unwrap()
        .into_iter()
        .map(|method| method.id.as_str().to_owned())
        .collect();
    assert!(ids.iter().any(|id| id == "chatgpt-profile:oaiapp_test"));
    assert!(
        ids.iter()
            .any(|id| id == "chatgpt-profile:oaiapp_other_account")
    );
    first.logout().await;
    let restarted = Harness::start("managed-codex-test", secrets).await;
    restarted.sign_in(Some("chatgpt-profile:oaiapp_test")).await;
    phase(&restarted.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&restarted.requests.lock()[0], "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_same_callback(
        param(&restarted.requests.lock()[0], "redirect_uri")
            .as_deref()
            .unwrap(),
        &redirect,
    );
    assert_eq!(
        restarted.auth.read().await.unwrap().unwrap().subject,
        "user-test"
    );
}

#[tokio::test]
async fn fresh_sign_in_keeps_separate_profiles() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    *h.mock.subject.lock() = "other-user".to_owned();
    *h.mock.email.lock() = "other@example.test".to_owned();
    *h.callback_client_id.lock() = Some("oaiapp_other_account".to_owned());
    h.sign_in(Some("chatgpt-change-account")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    *h.mock.subject.lock() = "user-test".to_owned();
    *h.mock.email.lock() = "hidden@example.test".to_owned();
    *h.callback_client_id.lock() = Some("oaiapp_replacement".to_owned());
    h.sign_in(Some("chatgpt-change-account")).await;
    let changed = phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    let profiles: Vec<_> = changed
        .methods
        .unwrap()
        .into_iter()
        .filter(|method| method.id.as_str().starts_with("chatgpt-profile:"))
        .map(|method| method.id.as_str().to_owned())
        .collect();
    assert_eq!(
        profiles,
        [
            "chatgpt-profile:oaiapp_replacement",
            "chatgpt-profile:oaiapp_other_account",
            "chatgpt-profile:oaiapp_test",
        ]
    );
    assert_eq!(
        h.auth.read().await.unwrap().unwrap().client_id,
        "oaiapp_replacement"
    );
    let redirect = param(&h.requests.lock()[2], "redirect_uri").unwrap();
    h.logout().await;
    h.sign_in(Some("chatgpt-profile:oaiapp_replacement")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[3], "client_id").as_deref(),
        Some("oaiapp_replacement")
    );
    assert_same_callback(
        param(&h.requests.lock()[3], "redirect_uri")
            .as_deref()
            .unwrap(),
        &redirect,
    );
}

#[tokio::test]
async fn legacy_profiles_with_the_same_email_stay_distinct() {
    let secrets = Arc::new(OrderedSecrets::new());
    let h = Harness::start("managed-codex-test", secrets.clone()).await;
    succeeded(&h).await;
    let redirect = param(&h.requests.lock()[0], "redirect_uri").unwrap();
    h.rewrite_registration(|mut record| {
        let profiles = record.get_mut("profiles").unwrap().as_array_mut().unwrap();
        let mut duplicate = profiles[0].clone();
        duplicate["clientId"] = json!("oaiapp_old_duplicate");
        profiles.insert(0, duplicate);
        Some(record)
    });
    let restarted = Harness::start("managed-codex-test", secrets).await;
    let state = phase_with_methods(&restarted.auth).await;
    let ids: Vec<_> = state
        .methods
        .unwrap()
        .into_iter()
        .filter(|method| method.id.as_str().starts_with("chatgpt-profile:"))
        .map(|method| method.id.as_str().to_owned())
        .collect();
    assert_eq!(
        ids,
        [
            "chatgpt-profile:oaiapp_test",
            "chatgpt-profile:oaiapp_old_duplicate"
        ]
    );
    restarted.sign_in(None).await;
    phase(&restarted.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&restarted.requests.lock()[0], "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_same_callback(
        param(&restarted.requests.lock()[0], "redirect_uri")
            .as_deref()
            .unwrap(),
        &redirect,
    );
    assert_eq!(profiles(&restarted.stored()).len(), 2);
}

#[tokio::test]
async fn matching_emails_do_not_merge_identities() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    *h.mock.subject.lock() = "other-user".to_owned();
    *h.mock.email.lock() = "hidden@example.test".to_owned();
    *h.callback_client_id.lock() = Some("oaiapp_other_account".to_owned());
    h.sign_in(Some("chatgpt-change-account")).await;
    let changed = phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    let count = changed
        .methods
        .unwrap()
        .into_iter()
        .filter(|method| method.id.as_str().starts_with("chatgpt-profile:"))
        .count();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn legacy_registration_keeps_its_identity_when_credentials_are_cleared() {
    let secrets = Arc::new(OrderedSecrets::new());
    let first = Harness::start("managed-codex-test", secrets.clone()).await;
    succeeded(&first).await;
    let redirect = param(&first.requests.lock()[0], "redirect_uri").unwrap();
    let host = param(&first.requests.lock()[0], "ext_agent_host_id").unwrap();
    first.rewrite_registration(|_| {
        Some(json!({"clientId": "oaiapp_test", "redirectUri": redirect}))
    });
    first.auth.revoke().await.unwrap();
    assert_eq!(
        first.stored(),
        vec![json!({
            "profiles": [{
                "clientId": "oaiapp_test",
                "connectionLabel": "Connection 1",
                "redirectUri": redirect,
                "subject": "user-test",
                "email": "hidden@example.test",
            }],
            "lastClientId": "oaiapp_test",
        })]
    );
    let restarted = Harness::start("managed-codex-test", secrets).await;
    *restarted.mock.subject.lock() = "another-user".to_owned();
    *restarted.mock.email.lock() = "another@example.test".to_owned();
    restarted.sign_in(None).await;
    let failed = phase(&restarted.auth, ProviderAuthPhase::Failed).await;
    assert!(
        failed
            .message
            .unwrap()
            .contains("different ChatGPT account")
    );
    assert!(restarted.auth.read().await.unwrap().is_none());
    assert_eq!(
        param(&restarted.requests.lock()[0], "ext_agent_host_id").as_deref(),
        Some(host.as_str())
    );
}

#[tokio::test]
async fn declined_sharing_on_an_older_profile_does_not_replace_the_active_account() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    *h.mock.subject.lock() = "other-user".to_owned();
    *h.mock.email.lock() = "other@example.test".to_owned();
    *h.callback_client_id.lock() = Some("oaiapp_other_account".to_owned());
    h.sign_in(Some("chatgpt-change-account")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    let before = h.stored();
    *h.mock.subject.lock() = "user-test".to_owned();
    *h.mock.email.lock() = "hidden@example.test".to_owned();
    *h.callback_client_id.lock() = Some("oaiapp_test".to_owned());
    *h.mock.grant_scope.lock() = "openid profile email".to_owned();
    h.sign_in(Some("chatgpt-profile:oaiapp_test")).await;
    let failed = phase(&h.auth, ProviderAuthPhase::Failed).await;
    assert!(
        failed
            .message
            .unwrap()
            .contains("existing ChatGPT connection is unchanged")
    );
    let read = serde_json::to_value(h.auth.read().await.unwrap().unwrap()).unwrap();
    let expected = before
        .into_iter()
        .find(|record| {
            record.get("clientId").and_then(Value::as_str) == Some("oaiapp_other_account")
        })
        .unwrap();
    assert_eq!(read, expected);
}

#[tokio::test]
async fn disconnect_preserves_a_remote_callback() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    let (first, first_callback) = h.start_remote(None).await;
    h.complete("owner", &first, &first_callback).await.unwrap();
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    h.logout().await;
    let (second, second_callback) = h.start_remote(None).await;
    let second_url = Url::parse(second.authorization_url.as_deref().unwrap()).unwrap();
    let first_url = Url::parse(first.authorization_url.as_deref().unwrap()).unwrap();
    assert_eq!(
        param(&second_url, "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_same_callback(
        param(&second_url, "redirect_uri").as_deref().unwrap(),
        param(&first_url, "redirect_uri").as_deref().unwrap(),
    );
    h.complete("owner", &second, &second_callback)
        .await
        .unwrap();
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(h.auth.read().await.unwrap().unwrap().subject, "user-test");
}

#[tokio::test]
async fn refresh_recovery_follows_the_oauth_error() {
    for code in [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
        "invalid_client",
        "invalid_token",
    ] {
        let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
        succeeded(&h).await;
        h.seed_expired().await;
        *h.mock.refresh_error.lock() = Some(code.to_owned());
        assert!(h.auth.access().await.is_err(), "{code}");
        let cleared = !matches!(code, "invalid_client" | "invalid_token");
        assert_eq!(h.auth.read().await.unwrap().is_none(), cleared, "{code}");
        assert!(
            profiles(&h.stored())
                .iter()
                .any(|profile| profile.get("clientId").and_then(Value::as_str)
                    == Some("oaiapp_test")),
            "{code}"
        );
    }
}

#[tokio::test]
async fn logout_revokes_the_latest_refresh_token() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    let host = param(&h.requests.lock()[0], "ext_agent_host_id").unwrap();
    assert!(host.starts_with("urn:uuid:") && host.len() == "urn:uuid:".len() + 36);
    h.seed_expired().await;
    h.auth.access().await.unwrap();
    let state = h.logout().await;
    assert_eq!(state.message.as_deref(), Some("Signed out."));
    assert_eq!(
        h.mock.revocations.lock()[0],
        [
            ("token".to_owned(), "refresh-1".to_owned()),
            ("token_type_hint".to_owned(), "refresh_token".to_owned()),
            ("client_id".to_owned(), "oaiapp_test".to_owned()),
        ]
    );
    assert!(h.auth.read().await.unwrap().is_none());
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert!(param(&h.requests.lock()[1], "id_token_hint").is_none());
    assert_eq!(
        param(&h.requests.lock()[1], "ext_agent_host_id").as_deref(),
        Some(host.as_str())
    );
}

#[tokio::test]
async fn logout_retries_revocation_and_reports_a_local_sign_out() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    h.mock.revocation_status.store(503, Ordering::SeqCst);
    let state = h.logout().await;
    assert_eq!(h.mock.revocations.lock().len(), 3);
    assert_eq!(state.phase, ProviderAuthPhase::Idle);
    assert!(
        state
            .message
            .unwrap()
            .contains("Remote revocation could not be confirmed")
    );
    assert!(h.auth.read().await.unwrap().is_none());
    assert!(
        h.stored()
            .iter()
            .all(|record| record.get("idToken").is_none())
    );
}

#[tokio::test]
async fn reauth_hints_come_from_the_selected_profile() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    let personal = h.auth.read().await.unwrap().unwrap();
    *h.callback_client_id.lock() = Some("oaiapp_work".to_owned());
    *h.mock.subject.lock() = "work-user".to_owned();
    *h.mock.email.lock() = "work@example.test".to_owned();
    h.sign_in(Some("chatgpt-change-account")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert!(param(&h.requests.lock()[1], "id_token_hint").is_none());
    assert!(param(&h.requests.lock()[1], "login_hint").is_none());
    h.logout().await;
    *h.callback_client_id.lock() = Some("oaiapp_test".to_owned());
    *h.mock.subject.lock() = "user-test".to_owned();
    *h.mock.email.lock() = "hidden@example.test".to_owned();
    h.sign_in(Some("chatgpt-profile:oaiapp_test")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[2], "id_token_hint").as_deref(),
        personal.id_token.as_deref()
    );
    assert_eq!(
        param(&h.requests.lock()[2], "login_hint").as_deref(),
        personal.email.as_deref()
    );
}

#[tokio::test]
async fn an_expired_code_keeps_the_issued_client_id() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *h.mock.code_error.lock() = Some("invalid_grant".to_owned());
    h.sign_in(None).await;
    let failed = phase(&h.auth, ProviderAuthPhase::Failed).await;
    assert!(h.auth.read().await.unwrap().is_none());
    assert!(
        failed
            .methods
            .unwrap()
            .iter()
            .any(|method| method.id.as_str() == "chatgpt-profile:oaiapp_test")
    );
    *h.mock.code_error.lock() = None;
    h.sign_in(Some("chatgpt-profile:oaiapp_test")).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    let requests = h.requests.lock().clone();
    assert_eq!(
        param(&requests[1], "client_id").as_deref(),
        Some("oaiapp_test")
    );
    assert_ne!(param(&requests[0], "state"), param(&requests[1], "state"));
}

#[tokio::test]
async fn identity_only_sign_in_requests_consent_on_retry() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    *h.mock.grant_scope.lock() = "openid profile email".to_owned();
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Failed).await;
    h.sign_in(Some("chatgpt-profile:oaiapp_test")).await;
    phase(&h.auth, ProviderAuthPhase::Failed).await;
    assert_eq!(
        param(&h.requests.lock()[1], "prompt").as_deref(),
        Some("consent")
    );
    assert!(param(&h.requests.lock()[1], "force_reconsent").is_none());
    assert!(
        param(&h.requests.lock()[1], "scope")
            .unwrap()
            .contains("chatgpt.tokens.use.direct")
    );
    assert!(h.auth.access().await.is_err());
}

#[tokio::test]
async fn a_replacement_controller_shares_refresh_serialization() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    let replacement = h.recreate().await;
    h.seed_expired().await;
    let (left, right) = tokio::join!(h.auth.access(), replacement.access());
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(h.mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(left.refresh_token.as_deref(), Some("refresh-1"));
    assert_eq!(left, right);
}

#[tokio::test]
async fn localhost_registrations_get_a_new_client() {
    let secrets = Arc::new(OrderedSecrets::new());
    let h = Harness::start("managed-codex-test", secrets).await;
    succeeded(&h).await;
    h.rewrite_registration(|mut record| {
        let profiles = record.get_mut("profiles").unwrap().as_array_mut().unwrap();
        for profile in profiles {
            let uri = profile["redirectUri"]
                .as_str()
                .unwrap()
                .replace("127.0.0.1", "localhost");
            profile["redirectUri"] = json!(uri);
        }
        Some(record)
    });
    *h.callback_client_id.lock() = Some("oaiapp_migrated".to_owned());
    *h.mock.subject.lock() = "new-client-subject".to_owned();
    *h.mock.email.lock() = "hidden@example.test".to_owned();
    h.sign_in(None).await;
    let changed = phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[1], "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(
        Url::parse(
            param(&h.requests.lock()[1], "redirect_uri")
                .as_deref()
                .unwrap()
        )
        .unwrap()
        .host_str(),
        Some("127.0.0.1")
    );
    assert_eq!(
        h.auth.read().await.unwrap().unwrap().subject,
        "new-client-subject"
    );
    let ids: Vec<_> = changed
        .methods
        .unwrap()
        .into_iter()
        .map(|method| method.id.as_str().to_owned())
        .collect();
    assert!(ids.iter().any(|id| id == "chatgpt-profile:oaiapp_test"));
    assert!(ids.iter().any(|id| id == "chatgpt-profile:oaiapp_migrated"));
    h.sign_in(None).await;
    phase(&h.auth, ProviderAuthPhase::Succeeded).await;
    assert_eq!(
        param(&h.requests.lock()[2], "client_id").as_deref(),
        Some("oaiapp_migrated")
    );
    assert_eq!(
        param(&h.requests.lock()[2], "login_hint").as_deref(),
        Some("hidden@example.test")
    );
    assert!(param(&h.requests.lock()[2], "id_token_hint").is_some());
}

#[tokio::test]
async fn the_active_connection_stays_first_when_another_profile_declines_sharing() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    *h.callback_client_id.lock() = Some("oaiapp_identity_only".to_owned());
    *h.mock.subject.lock() = "other-user".to_owned();
    *h.mock.email.lock() = "other@example.test".to_owned();
    *h.mock.grant_scope.lock() = "openid profile email".to_owned();
    h.sign_in(Some("chatgpt-change-account")).await;
    let state = phase(&h.auth, ProviderAuthPhase::Failed).await;
    assert_eq!(
        h.auth.read().await.unwrap().unwrap().client_id,
        "oaiapp_test"
    );
    let profiles: Vec<_> = state
        .methods
        .unwrap()
        .into_iter()
        .filter(|method| method.id.as_str().starts_with("chatgpt-profile:"))
        .collect();
    assert_eq!(profiles[0].id.as_str(), "chatgpt-profile:oaiapp_test");
    assert!(profiles[0].name.as_str().contains("Connection 1"));
    assert!(profiles[1].name.as_str().contains("Connection 2"));
}

#[tokio::test]
async fn transferred_profiles_reject_mismatched_identities() {
    let h = Harness::start("managed-codex-test", Arc::new(OrderedSecrets::new())).await;
    succeeded(&h).await;
    let profile = h.auth.export_profile().await.unwrap();
    let (destination, _) = h.destination().await;
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let imported = (destination.controller.import_profile.as_ref().unwrap())(
        profile.clone(),
        Box::pin(async move {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert!(stopped.load(Ordering::SeqCst));
    assert_eq!(imported.phase, ProviderAuthPhase::Succeeded);
    let original = destination.read().await.unwrap().unwrap();
    assert_eq!(original.client_id, profile.registration.client_id);
    assert_eq!(original.refresh_token.as_deref(), Some("initial-refresh"));
    let reconnect =
        (destination.controller.reconnect_profile.as_ref().unwrap())("chatgpt".to_owned())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        reconnect.id_token_hint.as_deref(),
        Some(profile.credentials.id_token.as_str())
    );
    let cases = [
        mutated(&profile, |credentials| {
            credentials.subject = "wrong-user".to_owned()
        }),
        mutated(&profile, |credentials| {
            credentials.client_id = "oaiapp_wrong".to_owned()
        }),
        mutated(&profile, |credentials| {
            credentials.id_token = "not-a-jwt".to_owned()
        }),
        mutated(&profile, |credentials| {
            credentials.scopes = vec!["openid".to_owned()]
        }),
        mutated(&profile, |credentials| credentials.expires_at = 0.0),
    ];
    for credentials in cases {
        let result = (destination.controller.import_profile.as_ref().unwrap())(
            credentials,
            Box::pin(async { Ok(()) }),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(destination.read().await.unwrap().unwrap(), original);
    }
}

#[test]
fn discovery_trusts_only_the_openai_issuer_and_its_own_origin() {
    let trusted = DiscoveryDoc {
        issuer: "https://auth.openai.com".to_owned(),
        authorization_endpoint: "https://auth.openai.com/authorize".to_owned(),
        token_endpoint: "https://auth.openai.com/token".to_owned(),
        jwks_uri: "https://auth.openai.com/jwks".to_owned(),
        revocation_endpoint: Some("https://auth.openai.com/revoke".to_owned()),
    };
    assert!(discovery_is_trusted(&trusted));
    let mut foreign = trusted.clone();
    foreign.issuer = "https://evil.example".to_owned();
    assert!(!discovery_is_trusted(&foreign));
    let mut off_origin = trusted.clone();
    off_origin.token_endpoint = "https://tokens.example/token".to_owned();
    assert!(!discovery_is_trusted(&off_origin));
}

#[test]
fn earliest_refresh_accepts_unix_seconds_and_rfc3339() {
    assert_eq!(
        earliest_millis(Some(Earliest::Number(10.0))),
        Some(10_000.0)
    );
    assert_eq!(earliest_millis(None), None);
    assert_eq!(
        earliest_millis(Some(Earliest::Text("not-a-date".to_owned()))),
        None
    );
    assert_eq!(
        earliest_millis(Some(Earliest::Text("2020-01-02T03:04:05Z".to_owned()))),
        Some(1_577_934_245_000.0)
    );
}

async fn phase_with_methods(auth: &CodexChatGptAuth) -> ProviderAuthState {
    let mut stream = std::pin::Pin::from((auth.controller.subscribe)("owner".to_owned()));
    let wait = async {
        loop {
            let next = futures::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
            match next {
                Some(state) if state.methods.is_some() => return state,
                Some(_) => {}
                None => panic!("subscription ended"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait)
        .await
        .expect("methods")
}

fn mutated(
    profile: &turin_contracts::provider_setup::ChatGptTransferredProfile,
    edit: impl FnOnce(&mut turin_contracts::provider_setup::ChatGptTransferredProfileCredentials),
) -> turin_contracts::provider_setup::ChatGptTransferredProfile {
    let mut profile = profile.clone();
    edit(&mut profile.credentials);
    profile
}
