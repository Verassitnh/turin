//! Port of upstream/packages/effect-acp/src/agent.ts
//!
//! The agent is the other end of the same JSON-RPC connection. Inbound agent
//! methods (`initialize`, `session/prompt`, …) are requests. `session/cancel`
//! is a notification. Outbound client methods such as `session/request_permission`
//! use the RPC id space; `ext_request` uses the extension id space.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;

use crate::client::{NotifyHandler, RequestHandler};
use crate::errors::{AcpError, AcpRequestError};
use crate::meta::{agent, client as client_methods};
use crate::protocol::{
    Connection, ConnectionConfig, IncomingNotification, NotificationCallback, RequestCallback,
    RequestContext,
};

#[derive(Default)]
struct AgentHandlers {
    core: HashMap<String, RequestHandler>,
    ext_requests: HashMap<String, RequestHandler>,
    ext_notifications: HashMap<String, NotifyHandler>,
    cancel: Vec<NotifyHandler>,
    unknown_ext_request: Option<RequestHandler>,
}

#[derive(Clone)]
pub struct AcpAgent {
    connection: Connection,
    handlers: Arc<Mutex<AgentHandlers>>,
}

impl AcpAgent {
    pub fn connect<R, W>(read: R, write: W) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let handlers = Arc::new(Mutex::new(AgentHandlers::default()));
        let on_request = agent_request_callback(Arc::clone(&handlers));
        let on_notification = agent_notification_callback(Arc::clone(&handlers));
        let connection = Connection::start(
            read,
            write,
            ConnectionConfig {
                on_request,
                on_notification,
                on_termination: Arc::new(|_| {}),
                termination_on_eof: None,
                termination_on_exit: None,
                transform_session_update: Arc::new(|value| value),
            },
        );
        Self {
            connection,
            handlers,
        }
    }

    pub async fn request_permission(&self, payload: Value) -> Result<Value, AcpError> {
        self.connection
            .rpc_request(client_methods::SESSION_REQUEST_PERMISSION, payload)
            .await
    }

    pub async fn session_update(&self, payload: Value) -> Result<(), AcpError> {
        self.connection
            .notify(client_methods::SESSION_UPDATE, payload)
            .await
    }

    pub async fn ext_request(&self, method: &str, payload: Value) -> Result<Value, AcpError> {
        self.connection.ext_request(method, payload).await
    }

    pub fn on_initialize<F, Fut>(&self, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.insert(agent::INITIALIZE, handler);
    }

    pub fn on_prompt<F, Fut>(&self, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.insert(agent::SESSION_PROMPT, handler);
    }

    pub fn on_create_session<F, Fut>(&self, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.insert(agent::SESSION_NEW, handler);
    }

    pub fn on_cancel<F, Fut>(&self, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        self.handlers.lock().cancel.push(box_notify(handler));
    }

    pub fn on_ext_request<F, Fut>(&self, method: impl Into<String>, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.handlers
            .lock()
            .ext_requests
            .insert(method.into(), box_request(handler));
    }

    fn insert<F, Fut>(&self, method: &str, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.handlers
            .lock()
            .core
            .insert(method.to_owned(), box_request(handler));
    }
}

fn agent_request_callback(handlers: Arc<Mutex<AgentHandlers>>) -> RequestCallback {
    Arc::new(move |method, params, context| {
        let handlers = Arc::clone(&handlers);
        Box::pin(async move {
            let (handler, extension) = {
                let handlers = handlers.lock();
                if crate::meta::is_agent_core_method(&method) {
                    (handlers.core.get(&method).cloned(), false)
                } else if let Some(handler) = handlers.ext_requests.get(&method).cloned() {
                    (Some(handler), true)
                } else {
                    (handlers.unknown_ext_request.clone(), true)
                }
            };
            let Some(handler) = handler else {
                return Err(AcpError::from(AcpRequestError::method_not_found(&method)));
            };
            match handler(params, context).await {
                Ok(value) => Ok(value),
                Err(error) => Err(AcpError::from(AcpRequestError::from_handler(
                    error, &method, extension,
                ))),
            }
        })
    })
}

fn agent_notification_callback(handlers: Arc<Mutex<AgentHandlers>>) -> NotificationCallback {
    Arc::new(move |notification| {
        let handlers = Arc::clone(&handlers);
        Box::pin(async move {
            let IncomingNotification::Ext { method, params } = notification else {
                return;
            };
            if method == agent::SESSION_CANCEL {
                let cancel = handlers.lock().cancel.clone();
                for handler in cancel {
                    let _ = handler(params.clone()).await;
                }
                return;
            }
            let handler = handlers.lock().ext_notifications.get(&method).cloned();
            if let Some(handler) = handler {
                let _ = handler(params).await;
            }
        })
    })
}

fn box_request<F, Fut>(handler: F) -> RequestHandler
where
    F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
{
    let handler = Arc::new(handler);
    Arc::new(move |params, context| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler(params, context).await })
    })
}

fn box_notify<F, Fut>(handler: F) -> NotifyHandler
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
{
    let handler = Arc::new(handler);
    Arc::new(move |params| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler(params).await })
    })
}
