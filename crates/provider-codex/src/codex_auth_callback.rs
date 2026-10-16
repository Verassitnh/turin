//! Port of upstream/apps/server/src/provider/CodexAuthCallback.ts.
//!
//! A connected local environment receives the callback. Only the remote
//! environment owns tokens.

// `ProviderSetupError` is the contract error this flow returns.
#![allow(clippy::result_large_err)]

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Stream;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_setup::{
    CodexAuthCallbackInput, CodexAuthCallbackState, ProviderSetupError,
};
use turin_shared::codex_auth_callback::receive_codex_auth_callback;
use turin_shared::codex_auth_handoff::codex_authorization_request;
use turin_shared::provider_auth_return_url::{is_loopback_host, provider_auth_return_url};
use url::Url;

enum Note {
    Ready,
    Done(Result<String, ProviderSetupError>),
}

/// `subscribeCodexAuthCallback`. Dropping the subscription cancels the
/// listener. [`CodexAuthCallbackSubscription::interrupt`] waits until the port
/// is released, matching an interrupted Effect fiber.
pub struct CodexAuthCallbackSubscription {
    events: mpsc::UnboundedReceiver<Note>,
    cancel: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
}

impl CodexAuthCallbackSubscription {
    pub async fn interrupt(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(true);
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for CodexAuthCallbackSubscription {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(true);
        }
    }
}

impl Stream for CodexAuthCallbackSubscription {
    type Item = Result<CodexAuthCallbackState, ProviderSetupError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().events.poll_recv(cx) {
            Poll::Ready(Some(Note::Ready)) => Poll::Ready(Some(Ok(CodexAuthCallbackState::Ready))),
            Poll::Ready(Some(Note::Done(result))) => {
                Poll::Ready(Some(result.map(|callback_url| {
                    CodexAuthCallbackState::Finished { callback_url }
                })))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn failure(instance_id: &ProviderInstanceId, detail: impl Into<String>) -> ProviderSetupError {
    ProviderSetupError::SetupError {
        instance_id: instance_id.clone(),
        operation: "callback".to_owned(),
        detail: detail.into(),
        cause: None,
    }
}

/// `subscribeCodexAuthCallback`.
pub fn subscribe_codex_auth_callback(
    input: CodexAuthCallbackInput,
) -> CodexAuthCallbackSubscription {
    let (events_tx, events) = mpsc::unbounded_channel();
    let destination = match local_destination(&input) {
        Ok(destination) => destination,
        Err(error) => {
            let _ = events_tx.send(Note::Done(Err(error)));
            return CodexAuthCallbackSubscription {
                events,
                cancel: None,
                task: None,
            };
        }
    };
    let (cancel, _) = watch::channel(false);
    let cancel_for_task = cancel.clone();
    let instance_id = input.instance_id.clone();
    let authorization_url = input.authorization_url.clone();
    let task = tokio::spawn(async move {
        let notes = events_tx.clone();
        let result = receive_codex_auth_callback(
            &authorization_url,
            move |_| async move {
                let _ = notes.send(Note::Ready);
                true
            },
            Some(Arc::new(move |_| Ok(destination.clone()))),
            cancel_for_task,
        )
        .await
        .map_err(|error| failure(&instance_id, error.detail));
        let _ = events_tx.send(Note::Done(result));
    });
    CodexAuthCallbackSubscription {
        events,
        cancel: Some(cancel),
        task: Some(task),
    }
}

fn local_destination(input: &CodexAuthCallbackInput) -> Result<String, ProviderSetupError> {
    let checked = (|| {
        codex_authorization_request(&input.authorization_url)?;
        let destination = provider_auth_return_url(Some(&input.return_url)).ok_or_else(|| {
            anyhow::anyhow!("The local sign-in receiver needs a local T3 Code return address.")
        })?;
        let hostname = Url::parse(&destination)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        if !is_loopback_host(&hostname) {
            anyhow::bail!("The local sign-in receiver needs a local T3 Code return address.");
        }
        Ok(destination)
    })();
    checked.map_err(|error: anyhow::Error| failure(&input.instance_id, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::base_schemas::{EnvironmentId, TrimmedNonEmptyString};
    use turin_contracts::provider_instance::ProviderInstanceId;

    async fn free_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn authorization(port: u16, state: &str) -> String {
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
        let redirect = request
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .unwrap()
            .1
            .into_owned();
        let state = request
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let mut url = Url::parse(&redirect).unwrap();
        url.query_pairs_mut()
            .append_pair("state", &state)
            .append_pair("code", "test-code")
            .append_pair("client_id", "oaiapp_test");
        url.to_string()
    }

    fn input(port: u16, state: &str, return_url: &str) -> CodexAuthCallbackInput {
        CodexAuthCallbackInput {
            authorization_url: authorization(port, state),
            return_url: return_url.to_owned(),
            environment_id: EnvironmentId::new("remote-nuc").unwrap(),
            instance_id: ProviderInstanceId::new("work").unwrap(),
            flow_id: TrimmedNonEmptyString::new("remote-flow").unwrap(),
        }
    }

    async fn next_state(
        subscription: &mut CodexAuthCallbackSubscription,
    ) -> Result<CodexAuthCallbackState, ProviderSetupError> {
        std::future::poll_fn(|cx| Pin::new(&mut *subscription).poll_next(cx))
            .await
            .expect("subscription ended")
    }

    fn http(manual: bool) -> reqwest::Client {
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
    async fn a_local_primary_receives_sign_in_for_a_remote_secondary() {
        let request = input(
            free_port().await,
            &"a".repeat(43),
            "http://localhost:7001/welcome#agents:remote-nuc",
        );
        let mut subscription = subscribe_codex_auth_callback(request.clone());
        let mut states = Vec::new();
        let ready = next_state(&mut subscription).await.unwrap();
        states.push(phase(&ready));
        assert!(matches!(ready, CodexAuthCallbackState::Ready));
        let expected = callback(&request.authorization_url);
        let foreign = {
            let mut url = Url::parse(&expected).unwrap();
            let pairs: Vec<(String, String)> = url
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
            url.set_query(None);
            {
                let mut query = url.query_pairs_mut();
                for (key, value) in &pairs {
                    if key == "state" {
                        query.append_pair(key, "foreign");
                    } else {
                        query.append_pair(key, value);
                    }
                }
            }
            url
        };
        assert_eq!(http(false).get(foreign).send().await.unwrap().status(), 400);
        let response = http(true).get(&expected).send().await.unwrap();
        assert_eq!(response.status(), 303);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .unwrap()
                .to_str()
                .unwrap(),
            "no-store"
        );
        assert_eq!(
            response
                .headers()
                .get("location")
                .unwrap()
                .to_str()
                .unwrap(),
            request.return_url
        );
        let finished = next_state(&mut subscription).await.unwrap();
        let CodexAuthCallbackState::Finished { callback_url } = &finished else {
            panic!("expected finished");
        };
        states.push(phase(&finished));
        assert_eq!(callback_url, &expected);
        assert_eq!(states, ["ready", "finished"]);
    }

    #[tokio::test]
    async fn disconnecting_the_receiving_client_releases_the_exact_callback_port() {
        let request = input(
            free_port().await,
            &"h".repeat(43),
            "http://localhost:7001/welcome#agents:remote-nuc",
        );
        let mut subscription = subscribe_codex_auth_callback(request.clone());
        assert!(matches!(
            next_state(&mut subscription).await.unwrap(),
            CodexAuthCallbackState::Ready
        ));
        subscription.interrupt().await;
        let mut again = subscribe_codex_auth_callback(request.clone());
        let expected = callback(&request.authorization_url);
        while let CodexAuthCallbackState::Ready = next_state(&mut again).await.unwrap() {
            http(true).get(&expected).send().await.unwrap();
        }
    }

    #[tokio::test]
    async fn the_local_server_refuses_nonlocal_return_destinations() {
        let request = input(
            free_port().await,
            &"i".repeat(43),
            "https://app.t3.codes/welcome",
        );
        let mut subscription = subscribe_codex_auth_callback(request);
        let error = next_state(&mut subscription).await.unwrap_err();
        let ProviderSetupError::SetupError { operation, .. } = error;
        assert_eq!(operation, "callback");
    }

    fn phase(state: &CodexAuthCallbackState) -> &'static str {
        match state {
            CodexAuthCallbackState::Ready => "ready",
            CodexAuthCallbackState::Finished { .. } => "finished",
        }
    }
}
