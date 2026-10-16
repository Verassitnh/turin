//! Port of upstream/apps/server/src/provider/CodexChatGptHandoff.ts.
//!
//! The primary runs OAuth for this stream. The destination owns the refresh
//! session. Telemetry (`telemetryFlow`, analytics events) is cut.

// `ProviderSetupError` is the contract error this flow returns.
#![allow(clippy::result_large_err)]

use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use futures::Stream;
use parking_lot::Mutex;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};
use turin_contracts::provider_setup::{
    ChatGptHandoffInput, ChatGptHandoffState, ProviderAuthPhase, ProviderAuthState,
    ProviderSetupError,
};
use turin_provider_core::auth::{ProviderAuthCallbackMode, ProviderAuthCancel};
use turin_provider_core::credential_store::{SecretStore, SecretStoreError};
use url::Url;

use crate::codex_chatgpt_auth::{CodexChatGptAuthOptions, make};

/// Optional discovery and resource override. `None` uses the OpenAI defaults.
pub struct ChatGptHandoffEndpoints {
    pub discovery_url: String,
    pub resource: String,
}

enum Note {
    Item(Box<ChatGptHandoffState>),
    Failed(ProviderSetupError),
}

/// `subscribeChatGptHandoff`. Dropping the subscription aborts the flow task.
/// [`ChatGptHandoffSubscription::interrupt`] cancels the sign-in and waits
/// until the callback port is closed, matching an interrupted Effect fiber.
pub struct ChatGptHandoffSubscription {
    events: mpsc::UnboundedReceiver<Note>,
    /// Filled once `make` returns. Empty until then.
    cancel: Arc<Mutex<Option<Arc<ProviderAuthCancel>>>>,
    owner: String,
    flow_id: Arc<Mutex<Option<String>>>,
    port: Arc<Mutex<Option<u16>>>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ChatGptHandoffSubscription {
    pub async fn interrupt(mut self) {
        let _ = self.stop.send(true);
        let port = *self.port.lock();
        let cancel = self.cancel.lock().clone();
        let flow_id = self.flow_id.lock().clone();
        if let (Some(cancel), Some(id)) = (cancel, flow_id) {
            let _ = cancel(self.owner.clone(), id).await;
        }
        if let Some(port) = port {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_err()
                {
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ChatGptHandoffSubscription {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Stream for ChatGptHandoffSubscription {
    type Item = Result<ChatGptHandoffState, ProviderSetupError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().events.poll_recv(cx) {
            Poll::Ready(Some(Note::Item(state))) => Poll::Ready(Some(Ok(*state))),
            Poll::Ready(Some(Note::Failed(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// `subscribeChatGptHandoff`.
pub fn subscribe_chatgpt_handoff(
    input: ChatGptHandoffInput,
    owner: impl Into<String>,
    endpoints: Option<ChatGptHandoffEndpoints>,
) -> ChatGptHandoffSubscription {
    let owner = owner.into();
    let (events_tx, events) = mpsc::unbounded_channel();
    let (stop, _) = watch::channel(false);
    let flow_id = Arc::new(Mutex::new(None));
    let port = Arc::new(Mutex::new(None));
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets {
        values: Mutex::new(HashMap::new()),
    });
    let options = CodexChatGptAuthOptions {
        instance_id: input.instance_id.clone(),
        secrets,
        secrets_directory: None,
        environment_id: input.environment_id.as_str().to_owned(),
        discovery_url: endpoints
            .as_ref()
            .map(|endpoints| endpoints.discovery_url.clone()),
        resource: endpoints
            .as_ref()
            .map(|endpoints| endpoints.resource.clone()),
        default_return_url: Some(input.return_url.clone()),
        reconnect_profile: input.profile.clone(),
    };
    let return_url = input.return_url;
    let flow_for_task = Arc::clone(&flow_id);
    let port_for_task = Arc::clone(&port);
    let stop_for_task = stop.clone();
    let owner_for_task = owner.clone();
    let cancel = Arc::new(Mutex::new(None));
    let cancel_for_task = Arc::clone(&cancel);
    let task = tokio::spawn(async move {
        let auth = match make(options).await {
            Ok(auth) => auth,
            Err(error) => {
                let _ = events_tx.send(Note::Failed(error));
                return;
            }
        };
        *cancel_for_task.lock() = Some(Arc::clone(&auth.controller.cancel));
        let started = (auth.controller.start)(
            owner_for_task.clone(),
            Some(Box::pin(async { Ok(()) })),
            Some("chatgpt".to_owned()),
            Some(return_url),
            Some(ProviderAuthCallbackMode::Server),
        )
        .await;
        let state = match started {
            Ok(state) => state,
            Err(error) => {
                let _ = events_tx.send(Note::Failed(error));
                return;
            }
        };
        remember(&flow_for_task, &port_for_task, &state);
        if *stop_for_task.borrow() {
            cancel_flow(&cancel_for_task, &owner_for_task, &flow_for_task).await;
            return;
        }
        let mut stream = Pin::from((auth.controller.subscribe)(owner_for_task.clone()));
        loop {
            let next = futures::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
            let Some(state) = next else { break };
            remember(&flow_for_task, &port_for_task, &state);
            let terminal = matches!(
                state.phase,
                ProviderAuthPhase::Succeeded
                    | ProviderAuthPhase::Failed
                    | ProviderAuthPhase::Cancelled
            );
            let note = if state.phase == ProviderAuthPhase::Succeeded {
                match auth.export_profile().await {
                    Ok(profile) => Note::Item(Box::new(ChatGptHandoffState::Finished { profile })),
                    Err(error) => Note::Failed(error),
                }
            } else {
                Note::Item(Box::new(ChatGptHandoffState::Auth { state }))
            };
            let failed = matches!(note, Note::Failed(_));
            let _ = events_tx.send(note);
            if terminal || failed || *stop_for_task.borrow() {
                if *stop_for_task.borrow() && !terminal {
                    cancel_flow(&cancel_for_task, &owner_for_task, &flow_for_task).await;
                }
                break;
            }
        }
    });
    ChatGptHandoffSubscription {
        events,
        cancel,
        owner,
        flow_id,
        port,
        stop,
        task: Some(task),
    }
}

fn remember(flow_id: &Mutex<Option<String>>, port: &Mutex<Option<u16>>, state: &ProviderAuthState) {
    if let Some(id) = state.flow_id.as_ref() {
        *flow_id.lock() = Some(id.as_str().to_owned());
    }
    let missing = port.lock().is_none();
    if missing && let Some(callback_port) = callback_port(state) {
        *port.lock() = Some(callback_port);
    }
}

fn callback_port(state: &ProviderAuthState) -> Option<u16> {
    let authorize = Url::parse(state.authorization_url.as_deref()?).ok()?;
    let redirect = authorize
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")?
        .1;
    Url::parse(&redirect).ok()?.port()
}

async fn cancel_flow(
    cancel: &Mutex<Option<Arc<ProviderAuthCancel>>>,
    owner: &str,
    flow_id: &Mutex<Option<String>>,
) {
    let (cancel, id) = {
        let cancel = cancel.lock().clone();
        let id = flow_id.lock().clone();
        (cancel, id)
    };
    let (Some(cancel), Some(id)) = (cancel, id) else {
        return;
    };
    let _ = cancel(owner.to_owned(), id).await;
}

/// In-memory stand-in for the handoff's private secret map. It never touches
/// the primary store. Creating a persistent secret is refused.
struct MemorySecrets {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

#[async_trait]
impl SecretStore for MemorySecrets {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(self.values.lock().get(name).cloned())
    }

    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        self.values.lock().insert(name.to_owned(), value.to_vec());
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
        self.values.lock().remove(name);
        Ok(())
    }

    async fn exclusive_create(&self, name: &str, _value: &[u8]) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Secure {
            resource: format!("secret {name}"),
            detail: "Handoff does not create persistent secrets.".to_owned(),
        })
    }

    async fn get_or_create_random(
        &self,
        name: &str,
        _bytes: usize,
    ) -> Result<Vec<u8>, SecretStoreError> {
        Err(SecretStoreError::Secure {
            resource: format!("secret {name}"),
            detail: "Handoff uses the destination environment identity.".to_owned(),
        })
    }
}
