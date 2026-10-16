//! Port of upstream/packages/provider-core/src/server/auth.ts.
//!
//! The controller is a record of closures, matching the upstream interface
//! (optional methods are `None`). Adapters fill in the ones they support.

#![allow(clippy::result_large_err, clippy::type_complexity)]

use std::{any::Any, sync::Arc};

use futures::Stream;
use turin_contracts::provider_setup::{
    ChatGptReconnectProfile, ChatGptTransferredProfile, ProviderAuthRespondInput,
    ProviderAuthState, ProviderCredentialOwner, ProviderSetupError,
};

use crate::rt::BoxFuture;

/// Failure from a [`ProviderAuthWithAccess`] gate.
///
/// Upstream's `withAccess` is generic over the admitted effect. This keeps
/// the controller object-safe: a sign-in rejection stays
/// [`AdmittedFailure::Setup`] so the adapter registry can remap it onto an
/// open-session error, and every other error is downcast by the caller.
pub enum AdmittedFailure {
    Setup(ProviderSetupError),
    Other(Box<dyn Any + Send>),
}

impl std::fmt::Debug for AdmittedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(error) => f.debug_tuple("Setup").field(error).finish(),
            Self::Other(_) => f.write_str("Other(..)"),
        }
    }
}

/// Lifetime of one admitted task's resources. Finalizers run in reverse order
/// when the session closes, like upstream `Scope` finalizers. The admitted
/// task receives a clone and may register cleanup on it.
#[derive(Clone, Default)]
pub struct SessionScope {
    state: Arc<parking_lot::Mutex<SessionState>>,
}

#[derive(Default)]
struct SessionState {
    closed: bool,
    finalizers: Vec<Box<dyn FnOnce() + Send>>,
}

impl SessionScope {
    /// Runs now when the session is already closed.
    pub fn add_finalizer(&self, finalizer: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock();
        if state.closed {
            drop(state);
            finalizer();
        } else {
            state.finalizers.push(Box::new(finalizer));
        }
    }

    /// Idempotent. Finalizers run synchronously; a task aborted by the owner
    /// drops its own state when the runtime next polls it.
    pub fn close(&self) {
        let finalizers = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            std::mem::take(&mut state.finalizers)
        };
        for finalizer in finalizers.into_iter().rev() {
            finalizer();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    pub(crate) fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

/// One unit of work a credential gate may run or reject.
pub struct AdmittedTask {
    pub(crate) make: Box<
        dyn FnOnce(SessionScope) -> BoxFuture<'static, Result<Box<dyn Any + Send>, AdmittedFailure>>
            + Send,
    >,
}

impl AdmittedTask {
    pub fn new<T, E>(fut: impl std::future::Future<Output = Result<T, E>> + Send + 'static) -> Self
    where
        T: Send + 'static,
        E: Send + 'static,
    {
        Self::scoped(move |_scope| fut)
    }

    /// A task that registers its resources on the session it is admitted to.
    pub fn scoped<T, E, F>(make: impl FnOnce(SessionScope) -> F + Send + 'static) -> Self
    where
        F: std::future::Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: Send + 'static,
    {
        Self {
            make: Box::new(move |scope| {
                let fut = make(scope);
                Box::pin(async move {
                    match fut.await {
                        Ok(value) => Ok(Box::new(value) as Box<dyn Any + Send>),
                        Err(error) => Err(AdmittedFailure::Other(Box::new(error))),
                    }
                })
            }),
        }
    }

    /// Re-box a gate's own future without wrapping its error twice.
    pub fn from_output(
        fut: impl std::future::Future<Output = Result<Box<dyn Any + Send>, AdmittedFailure>>
        + Send
        + 'static,
    ) -> Self {
        Self {
            make: Box::new(move |_scope| Box::pin(fut)),
        }
    }

    /// Runs outside any sign-in session. Resources registered on the
    /// detached scope are never finalized.
    #[allow(clippy::result_large_err)]
    pub async fn run(self) -> Result<Box<dyn Any + Send>, AdmittedFailure> {
        (self.make)(SessionScope::default()).await
    }

    #[allow(clippy::result_large_err)]
    pub async fn wait<T: Send + 'static>(self) -> Result<T, AdmittedFailure> {
        match (self.make)(SessionScope::default()).await {
            Ok(value) => value
                .downcast::<T>()
                .map(|boxed| *boxed)
                .map_err(AdmittedFailure::Other),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCredentialBinding {
    pub owner: ProviderCredentialOwner,
    pub key: String,
}

/// Sign-in controller owned by one provider instance.
#[allow(clippy::type_complexity)]
pub struct ProviderAuthController {
    /// Equal keys mean these instances share credentials on this environment.
    pub credential_binding: Option<ProviderCredentialBinding>,
    pub reconnect_profile: Option<
        Arc<
            dyn Fn(
                    String,
                ) -> BoxFuture<
                    'static,
                    Result<Option<ChatGptReconnectProfile>, ProviderSetupError>,
                > + Send
                + Sync,
        >,
    >,
    pub import_profile: Option<
        Arc<
            dyn Fn(
                    ChatGptTransferredProfile,
                    BoxFuture<'static, Result<(), ProviderSetupError>>,
                )
                    -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
                + Send
                + Sync,
        >,
    >,
    pub adopt_credentials: Option<
        Arc<
            dyn Fn(
                    BoxFuture<'static, Result<(), ProviderSetupError>>,
                    BoxFuture<'static, Result<(), ProviderSetupError>>,
                )
                    -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
                + Send
                + Sync,
        >,
    >,
    pub is_changing_credentials: Option<Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>>,
    pub invalidate: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    pub refresh_methods: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    pub with_access: Option<Arc<ProviderAuthWithAccess>>,
    pub start: Arc<ProviderAuthStart>,
    pub complete: Arc<ProviderAuthComplete>,
    pub cancel: Arc<ProviderAuthCancel>,
    pub respond: Option<Arc<ProviderAuthRespond>>,
    /// The controller closes process admission before it stops routed sessions.
    pub logout: Arc<ProviderAuthLogout>,
    pub subscribe: Arc<ProviderAuthSubscribe>,
    pub is_logout_prompt: Option<Arc<dyn Fn(&str, bool) -> bool + Send + Sync>>,
}

pub type ProviderAuthStart = dyn Fn(
        String,
        Option<BoxFuture<'static, Result<(), ProviderSetupError>>>,
        Option<String>,
        Option<String>,
        Option<ProviderAuthCallbackMode>,
    ) -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
    + Send
    + Sync;

pub type ProviderAuthComplete = dyn Fn(
        String,
        ProviderAuthCompleteInput,
    ) -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
    + Send
    + Sync;

pub type ProviderAuthCancel = dyn Fn(String, String) -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
    + Send
    + Sync;

pub type ProviderAuthRespond = dyn Fn(
        String,
        ProviderAuthRespondInput,
    ) -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
    + Send
    + Sync;

pub type ProviderAuthLogout = dyn Fn(
        BoxFuture<'static, Result<(), ProviderSetupError>>,
    ) -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
    + Send
    + Sync;

pub type ProviderAuthSubscribe =
    dyn Fn(String) -> Box<dyn Stream<Item = ProviderAuthState> + Send> + Send + Sync;

pub type ProviderAuthWithAccess = dyn Fn(AdmittedTask) -> BoxFuture<'static, Result<Box<dyn Any + Send>, AdmittedFailure>>
    + Send
    + Sync;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAuthCallbackMode {
    Server,
    Client,
}

#[derive(Debug, Clone)]
pub struct ProviderAuthCompleteInput {
    pub flow_id: String,
    pub callback_url: String,
}
