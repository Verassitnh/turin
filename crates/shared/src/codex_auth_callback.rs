//! Port of upstream/packages/shared/src/codexAuthCallback.ts.
//!
//! A loopback listener for one ChatGPT authorization code. Tokens stay on the
//! environment that started sign-in; this computer only receives the redirect.

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, LazyLock},
    time::Duration,
};

use parking_lot::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinSet,
};
use url::Url;

use crate::codex_auth_handoff::{codex_authorization_request, codex_callback_url};

const ALREADY_OPEN: &str = "This sign-in is already open on this computer.";
const CANCELLED: &str = "Sign-in cancelled on this computer.";
const EXPIRED: &str = "Sign-in expired. Try again.";
const PORT_IN_USE: &str = "The ChatGPT callback port is in use on this computer. Close the other sign-in and try again, or paste the redirect URL in T3 Code.";
const BROWSER: &str = "Could not open your sign-in browser.";
const REJECTED: &str = "This response does not belong to the active sign-in.";
const PAGE: &str = "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><title>T3 Code</title><style>body{font-family:system-ui;display:grid;place-items:center;min-height:90vh;margin:0}main{max-width:360px;padding:32px}h1{font-size:24px}p{line-height:1.6;opacity:.7}</style></head><body><main><h1>Return to T3 Code</h1><p>Your sign-in response has been received. T3 Code is finishing the connection. You can close this tab.</p></main></body></html>";

/// `CodexAuthCallbackError`. `message` is the detail string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct CodexAuthCallbackError {
    pub detail: String,
}

fn fail(detail: impl Into<String>) -> CodexAuthCallbackError {
    CodexAuthCallbackError {
        detail: detail.into(),
    }
}

fn fail_anyhow(error: anyhow::Error) -> CodexAuthCallbackError {
    fail(error.to_string())
}

/// Maps a verified callback URL to the browser's next location.
pub type CodexAuthCallbackDestination =
    Arc<dyn Fn(&str) -> Result<String, anyhow::Error> + Send + Sync>;

static LISTENERS: LazyLock<Mutex<HashMap<String, watch::Sender<bool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `cancelCodexAuthCallback`. An unknown sign-in is a no-op. An invalid
/// authorization URL fails the same way `codexAuthorizationRequest` does.
pub fn cancel_codex_auth_callback(authorization_url: &str) -> Result<(), CodexAuthCallbackError> {
    let request = codex_authorization_request(authorization_url).map_err(fail_anyhow)?;
    if let Some(cancel) = LISTENERS.lock().get(&request.state) {
        let _ = cancel.send(true);
    }
    Ok(())
}

/// `receiveCodexAuthCallback`.
///
/// `cancel` is the AbortSignal. Sending `true` (or dropping every sender)
/// rejects with the cancelled message. `destination` maps a verified callback
/// to the browser's next URL; `None` shows the plain return page.
pub async fn receive_codex_auth_callback<F, Fut>(
    authorization_url: &str,
    open_browser: F,
    destination: Option<CodexAuthCallbackDestination>,
    cancel: watch::Sender<bool>,
) -> Result<String, CodexAuthCallbackError>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = bool> + Send,
{
    let request = codex_authorization_request(authorization_url).map_err(fail_anyhow)?;
    {
        let mut listeners = LISTENERS.lock();
        if listeners.contains_key(&request.state) {
            return Err(fail(ALREADY_OPEN));
        }
        listeners.insert(request.state.clone(), cancel.clone());
    }
    let mut aborted = cancel.subscribe();
    let state = request.state.clone();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let outcome = async {
        if aborted_now(&aborted) {
            return Err(fail(CANCELLED));
        }
        let port = Url::parse(&request.redirect_uri)
            .ok()
            .and_then(|url| url.port())
            .ok_or_else(|| fail(PORT_IN_USE))?;
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|_| fail(PORT_IN_USE))?;
        let (callback_tx, callback_rx) = oneshot::channel();
        let slot = Arc::new(Mutex::new(Some(callback_tx)));
        let redirect_uri = request.redirect_uri.clone();
        let callback_state = request.state.clone();
        let mut shutdown_rx = shutdown_rx;
        let mut server_abort = cancel.subscribe();
        let server = tokio::spawn(async move {
            let mut joins = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    _ = server_abort.changed() => {
                        if aborted_now(&server_abort) || server_abort.has_changed().is_err() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let slot = Arc::clone(&slot);
                        let redirect_uri = redirect_uri.clone();
                        let callback_state = callback_state.clone();
                        let destination = destination.clone();
                        joins.spawn(async move {
                            let _ = handle_connection(
                                socket,
                                &redirect_uri,
                                &callback_state,
                                destination.as_ref(),
                                &slot,
                            )
                            .await;
                        });
                    }
                }
            }
            joins.abort_all();
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        let result = async {
            if aborted_now(&aborted) {
                return Err(fail(CANCELLED));
            }
            let opened = open_browser(request.authorization_url.clone()).await;
            if aborted_now(&aborted) {
                return Err(fail(CANCELLED));
            }
            if !opened {
                return Err(fail(BROWSER));
            }
            tokio::select! {
                biased;
                _ = wait_abort(&mut aborted) => Err(fail(CANCELLED)),
                _ = tokio::time::sleep_until(deadline) => Err(fail(EXPIRED)),
                callback = callback_rx => callback.map_err(|_| fail(CANCELLED)),
            }
        }
        .await;
        let _ = shutdown_tx.send(());
        let _ = server.await;
        result
    }
    .await;
    LISTENERS.lock().remove(&state);
    outcome
}

fn aborted_now(flag: &watch::Receiver<bool>) -> bool {
    *flag.borrow() || flag.has_changed().is_err()
}

async fn wait_abort(flag: &mut watch::Receiver<bool>) {
    if aborted_now(flag) {
        return;
    }
    while flag.changed().await.is_ok() {
        if *flag.borrow() {
            return;
        }
    }
}

async fn handle_connection(
    mut socket: TcpStream,
    redirect_uri: &str,
    state: &str,
    destination: Option<&CodexAuthCallbackDestination>,
    slot: &Mutex<Option<oneshot::Sender<String>>>,
) -> std::io::Result<()> {
    let request = match read_request(&mut socket).await {
        Ok(request) => request,
        Err(_) => return Ok(()),
    };
    if request.method != "GET" {
        reject(&mut socket).await?;
        return Ok(());
    }
    let absolute = match resolve_callback(redirect_uri, &request.target) {
        Ok(absolute) => absolute,
        Err(()) => {
            reject(&mut socket).await?;
            return Ok(());
        }
    };
    let callback = match codex_callback_url(&absolute, redirect_uri, state) {
        Ok(callback) => callback,
        Err(_) => {
            reject(&mut socket).await?;
            return Ok(());
        }
    };
    let callback_url = callback.to_string();
    let located = match destination {
        Some(destination) => match destination(&callback_url) {
            Ok(location) => Some(location),
            Err(_) => {
                reject(&mut socket).await?;
                return Ok(());
            }
        },
        None => None,
    };
    if let Some(location) = located.as_deref() {
        write_response(
            &mut socket,
            303,
            "See Other",
            &[
                ("cache-control", "no-store"),
                ("referrer-policy", "no-referrer"),
                ("x-content-type-options", "nosniff"),
                ("location", location),
            ],
            b"",
        )
        .await?;
    } else {
        write_response(
            &mut socket,
            200,
            "OK",
            &[
                ("cache-control", "no-store"),
                ("referrer-policy", "no-referrer"),
                ("x-content-type-options", "nosniff"),
                (
                    "content-security-policy",
                    "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
                ),
                ("content-type", "text/html; charset=utf-8"),
            ],
            PAGE.as_bytes(),
        )
        .await?;
    }
    if let Some(sender) = slot.lock().take() {
        let _ = sender.send(callback_url);
    }
    Ok(())
}

async fn reject(socket: &mut TcpStream) -> std::io::Result<()> {
    write_response(socket, 400, "Bad Request", &[], REJECTED.as_bytes()).await
}

fn resolve_callback(redirect_uri: &str, target: &str) -> Result<String, ()> {
    let base = Url::parse(redirect_uri).map_err(|_| ())?;
    Ok(base.join(target).map_err(|_| ())?.to_string())
}

struct MiniRequest {
    method: String,
    target: String,
}

async fn read_request(socket: &mut TcpStream) -> std::io::Result<MiniRequest> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "closed before headers",
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = find_subsequence(&buf, b"\r\n\r\n") {
            break end;
        }
        if buf.len() > 65_536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("/").to_owned();
    Ok(MiniRequest { method, target })
}

async fn write_response(
    socket: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    let _ = socket.shutdown().await;
    Ok(())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex_auth_handoff::{
        CodexAuthHandoff, codex_auth_delivery_url, read_codex_auth_delivery,
    };
    use parking_lot::Mutex;
    use tokio::net::TcpListener;
    use tokio::sync::{oneshot, watch};
    use turin_contracts::base_schemas::EnvironmentId;
    use turin_contracts::provider_instance::ProviderInstanceId;
    use url::Url;

    fn replace_state(url: &str, state: &str) -> Url {
        let parsed = Url::parse(url).unwrap();
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let mut invalid = parsed;
        invalid.set_query(None);
        {
            let mut query = invalid.query_pairs_mut();
            for (key, value) in &pairs {
                if key == "state" {
                    query.append_pair(key, state);
                } else {
                    query.append_pair(key, value);
                }
            }
        }
        invalid
    }

    fn idle_cancel() -> watch::Sender<bool> {
        watch::channel(false).0
    }

    async fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn request(port: u16, state: &str) -> String {
        let mut url = Url::parse("https://auth.openai.com/api/accounts/authorize").unwrap();
        url.query_pairs_mut()
            .append_pair("client_id", "dynamic_agent_client")
            .append_pair("response_type", "code")
            .append_pair(
                "redirect_uri",
                &format!("http://127.0.0.1:{port}/auth/callback"),
            )
            .append_pair("state", state)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &"b".repeat(43));
        url.to_string()
    }

    fn callback(authorization_url: &str) -> String {
        let request = Url::parse(authorization_url).unwrap();
        let mut url = Url::parse(
            request
                .query_pairs()
                .find(|(key, _)| key == "redirect_uri")
                .unwrap()
                .1
                .as_ref(),
        )
        .unwrap();
        let state = request
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        url.query_pairs_mut()
            .clear()
            .append_pair("state", &state)
            .append_pair("code", "test-code")
            .append_pair("client_id", "oaiapp_test");
        url.to_string()
    }

    fn client(manual: bool) -> reqwest::Client {
        let redirects = if manual {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::default()
        };
        reqwest::Client::builder()
            .redirect(redirects)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn binds_before_opening_sign_in_and_ignores_a_foreign_response() {
        let authorization_url = request(free_port().await, &"a".repeat(43));
        let expected = callback(&authorization_url);
        let received = receive_codex_auth_callback(
            &authorization_url,
            |_| async {
                let invalid = replace_state(&expected, "foreign");
                let http = client(false);
                assert_eq!(http.get(invalid).send().await.unwrap().status(), 400);
                let response = http.get(&expected).send().await.unwrap();
                assert_eq!(
                    response
                        .headers()
                        .get("cache-control")
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    "no-store"
                );
                let body = response.text().await.unwrap();
                assert!(!body.contains("test-code"));
                true
            },
            None,
            idle_cancel(),
        )
        .await
        .unwrap();
        assert_eq!(received, expected);
    }

    #[tokio::test]
    async fn returns_hosted_web_to_the_exact_instance_without_the_code_in_the_query() {
        let authorization_url = request(free_port().await, &"d".repeat(43));
        let expected = callback(&authorization_url);
        let input = CodexAuthHandoff {
            authorization_url: authorization_url.clone(),
            return_url:
                "https://app.t3.codes/settings/providers?environmentId=remote-one&instanceId=work"
                    .to_owned(),
            environment_id: EnvironmentId::new("remote-one").unwrap(),
            instance_id: ProviderInstanceId::new("work").unwrap(),
            flow_id: "flow-one".to_owned(),
        };
        let handoff = input.clone();
        receive_codex_auth_callback(
            &authorization_url,
            {
                let expected = expected.clone();
                let return_url = input.return_url.clone();
                move |_| async move {
                    let response = client(true).get(&expected).send().await.unwrap();
                    assert_eq!(response.status(), 303);
                    let delivery = response
                        .headers()
                        .get("location")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned();
                    assert!(
                        !Url::parse(&delivery)
                            .unwrap()
                            .query_pairs()
                            .any(|(key, _)| key == "code")
                    );
                    let parsed = read_codex_auth_delivery(&delivery).unwrap();
                    assert_eq!(parsed.callback_url, expected);
                    assert_eq!(parsed.return_url, return_url);
                    true
                }
            },
            Some(Arc::new(move |url| codex_auth_delivery_url(&handoff, url))),
            idle_cancel(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancels_and_releases_its_listener_so_the_same_port_can_run_again() {
        let authorization_url = request(free_port().await, &"e".repeat(43));
        let error = receive_codex_auth_callback(
            &authorization_url,
            {
                let authorization_url = authorization_url.clone();
                move |_| async move {
                    cancel_codex_auth_callback(&authorization_url).unwrap();
                    true
                }
            },
            None,
            idle_cancel(),
        )
        .await
        .unwrap_err();
        assert!(error.detail.contains("cancelled"), "{}", error.detail);
        let expected = callback(&authorization_url);
        let received = receive_codex_auth_callback(
            &authorization_url,
            {
                let expected = expected.clone();
                move |_| async move {
                    client(false).get(&expected).send().await.unwrap();
                    true
                }
            },
            None,
            idle_cancel(),
        )
        .await
        .unwrap();
        assert_eq!(received, expected);
    }

    #[tokio::test]
    async fn allows_two_accounts_to_complete_independently() {
        let a = request(free_port().await, &"f".repeat(43));
        let b = request(free_port().await, &"g".repeat(43));
        let (opened_a_tx, opened_a_rx) = oneshot::channel();
        let (opened_b_tx, opened_b_rx) = oneshot::channel();
        let opened_a_tx = Mutex::new(Some(opened_a_tx));
        let opened_b_tx = Mutex::new(Some(opened_b_tx));
        let receive_a = receive_codex_auth_callback(
            &a,
            {
                let expected = callback(&a);
                move |_| async move {
                    opened_a_tx.lock().take().unwrap().send(()).unwrap();
                    opened_b_rx.await.unwrap();
                    client(false).get(&expected).send().await.unwrap();
                    true
                }
            },
            None,
            idle_cancel(),
        );
        let receive_b = receive_codex_auth_callback(
            &b,
            {
                let expected = callback(&b);
                move |_| async move {
                    opened_b_tx.lock().take().unwrap().send(()).unwrap();
                    opened_a_rx.await.unwrap();
                    client(false).get(&expected).send().await.unwrap();
                    true
                }
            },
            None,
            idle_cancel(),
        );
        let (left, right) = tokio::join!(receive_a, receive_b);
        assert_eq!(left.unwrap(), callback(&a));
        assert_eq!(right.unwrap(), callback(&b));
    }
}
