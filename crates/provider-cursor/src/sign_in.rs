//! Sign-in controller for one Cursor instance.
//!
//! Port of the controller in `upstream/packages/provider-cursor/src/server/auth.ts`,
//! over ACP. Sign-in is `authenticate {methodId: "cursor_login"}` on a probe
//! process. `cursor-agent` opens the browser and blocks until the login
//! finishes, so the call runs as one background flow. Cancelling kills that
//! process. Sign-out is not offered: the login lives in the user's Cursor
//! store, and Turin does not delete it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use futures::stream::{self, Stream, StreamExt};
use tokio::sync::watch;
use turin_acp::{AcpClient, SpawnCommand, spawn};
use turin_contracts::base_schemas::UtcDateTime;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_setup::{
    ProviderAuthMethod, ProviderAuthMethodType, ProviderAuthPhase, ProviderAuthState,
    ProviderCredentialOwner, ProviderSetupError, SetupOperationId,
};
use turin_provider_core::auth::{
    ProviderAuthCallbackMode, ProviderAuthCompleteInput, ProviderAuthController,
    ProviderCredentialBinding,
};
use turin_provider_core::rt::BoxFuture;

use crate::auth::{ACP_ARGS, authenticate_params};
use crate::error::{CursorError, CursorOperation};

/// Upstream `AUTH_TIMEOUT_MS`. It is the expiry the UI shows.
const AUTH_TIMEOUT: Duration = Duration::from_secs(300);
const BROWSER_METHOD_NAME: &str = "Sign in with Cursor";

/// The state the controller publishes, and which client owns the flow.
#[derive(Clone)]
struct Current {
    owner: Option<String>,
    state: ProviderAuthState,
}

struct Flow {
    id: String,
    owner: String,
    task: tokio::task::AbortHandle,
}

struct Shared {
    instance_id: ProviderInstanceId,
    program: String,
    enabled: bool,
    /// Refreshes the snapshot after a sign-in succeeds.
    on_changed: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
    current: watch::Sender<Current>,
    flow: Mutex<Option<Flow>>,
}

pub fn controller(
    instance_id: ProviderInstanceId,
    program: String,
    enabled: bool,
    on_changed: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
) -> ProviderAuthController {
    let idle = Current {
        owner: None,
        state: state(&instance_id, ProviderAuthPhase::Idle, None, None),
    };
    let (current, _) = watch::channel(idle);
    let shared = Arc::new(Shared {
        instance_id: instance_id.clone(),
        program,
        enabled,
        on_changed,
        current,
        flow: Mutex::new(None),
    });

    let instance_id_for_key = instance_id.clone();
    let busy = Arc::clone(&shared);
    let start = Arc::clone(&shared);
    let cancel = Arc::clone(&shared);
    let logout = Arc::clone(&shared);
    let subscribe = Arc::clone(&shared);
    ProviderAuthController {
        credential_binding: Some(ProviderCredentialBinding {
            owner: ProviderCredentialOwner::T3,
            key: format!("cursor:{instance_id_for_key}"),
        }),
        reconnect_profile: None,
        import_profile: None,
        adopt_credentials: None,
        is_changing_credentials: Some(Arc::new(move || {
            let changing = busy.flow.lock().expect("flow").is_some();
            Box::pin(async move { changing })
        })),
        invalidate: None,
        refresh_methods: None,
        with_access: None,
        start: Arc::new(
            move |owner: String,
                  stop_sessions: Option<BoxFuture<'static, Result<(), ProviderSetupError>>>,
                  _redirect: Option<String>,
                  _callback: Option<String>,
                  _mode: Option<ProviderAuthCallbackMode>| {
                let start = Arc::clone(&start);
                Box::pin(async move {
                    if let Some(stop_sessions) = stop_sessions {
                        stop_sessions.await?;
                    }
                    start.start(owner)
                })
                    as BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
            },
        ),
        complete: Arc::new(move |_owner: String, _input: ProviderAuthCompleteInput| {
            let instance_id = instance_id.clone();
            Box::pin(async move {
                Err(setup_error(
                    &instance_id,
                    "complete",
                    "Finish signing in on the Cursor website. No redirect URL is needed.",
                ))
            }) as BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
        }),
        cancel: Arc::new(move |owner: String, id: String| {
            let cancel = Arc::clone(&cancel);
            Box::pin(async move { cancel.cancel(&owner, &id) })
                as BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
        }),
        respond: None,
        logout: Arc::new(
            move |stop_sessions: BoxFuture<'static, Result<(), ProviderSetupError>>| {
                let logout = Arc::clone(&logout);
                Box::pin(async move {
                    stop_sessions.await?;
                    Err(setup_error(
                        &logout.instance_id,
                        "logout",
                        "Cursor sign-out is not available in Turin. Run cursor-agent logout.",
                    ))
                })
                    as BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>>
            },
        ),
        subscribe: Arc::new(move |owner: String| {
            let first = subscribe.view(&owner, &subscribe.current.borrow().clone());
            let rest = stream::unfold(
                (subscribe.current.subscribe(), Arc::clone(&subscribe), owner),
                |(mut rx, shared, owner)| async move {
                    rx.changed().await.ok()?;
                    let current = rx.borrow_and_update().clone();
                    let view = shared.view(&owner, &current);
                    Some((view, (rx, shared, owner)))
                },
            );
            Box::new(stream::once(async move { first }).chain(rest))
                as Box<dyn Stream<Item = ProviderAuthState> + Send>
        }),
        is_logout_prompt: None,
    }
}

impl Shared {
    fn start(self: &Arc<Self>, owner: String) -> Result<ProviderAuthState, ProviderSetupError> {
        if !self.enabled {
            return Err(setup_error(
                &self.instance_id,
                "start",
                "Enable Cursor before signing in.",
            ));
        }
        let mut flow = self.flow.lock().expect("flow");
        if let Some(active) = flow.as_ref() {
            if active.owner == owner {
                return Ok(self.current.borrow().state.clone());
            }
            return Err(setup_error(
                &self.instance_id,
                "start",
                "Cursor setup is already in progress.",
            ));
        }
        let id = format!("cursor-login-{}", Utc::now().timestamp_millis());
        let operation_id = SetupOperationId::new(&id).map_err(|_| {
            setup_error(
                &self.instance_id,
                "start",
                "Could not start Cursor sign-in. Try again.",
            )
        })?;
        let expires_at = UtcDateTime::from_datetime(
            Utc::now() + chrono::Duration::from_std(AUTH_TIMEOUT).expect("auth timeout"),
        );
        let starting = state(
            &self.instance_id,
            ProviderAuthPhase::Starting,
            Some(operation_id.clone()),
            Some(expires_at.to_string()),
        );
        let mut starting = starting;
        starting.message = Some("Starting Cursor sign-in.".to_owned());
        self.publish(Some(owner.clone()), starting.clone());

        let program = self.program.clone();
        let instance_id = self.instance_id.clone();
        let on_changed = Arc::clone(&self.on_changed);
        let task_shared = Arc::clone(self);
        let task_id = id.clone();
        let handle = tokio::spawn(async move {
            let outcome = run_login(&program).await;
            let finished = match outcome {
                Ok(()) => {
                    let mut state = state(&instance_id, ProviderAuthPhase::Succeeded, None, None);
                    state.message = Some("Signed in to Cursor.".to_owned());
                    (state, true)
                }
                Err(error) => {
                    let mut state = state(&instance_id, ProviderAuthPhase::Failed, None, None);
                    state.message = Some(format!("Cursor sign-in failed: {}", error.detail));
                    (state, false)
                }
            };
            task_shared.finish(&task_id, finished.0);
            if finished.1 {
                on_changed().await;
            }
        });
        *flow = Some(Flow {
            id,
            owner,
            task: handle.abort_handle(),
        });
        Ok(starting)
    }

    fn cancel(&self, owner: &str, id: &str) -> Result<ProviderAuthState, ProviderSetupError> {
        let mut flow = self.flow.lock().expect("flow");
        match flow.as_ref() {
            Some(active) if active.owner == owner && active.id == id => {}
            _ => {
                return Err(setup_error(
                    &self.instance_id,
                    "cancel",
                    "This sign-in is no longer active in this client.",
                ));
            }
        }
        if let Some(active) = flow.take() {
            active.task.abort();
        }
        let mut cancelled = state(&self.instance_id, ProviderAuthPhase::Cancelled, None, None);
        cancelled.message = Some("Cursor sign-in was cancelled.".to_owned());
        drop(flow);
        self.publish(None, cancelled.clone());
        Ok(cancelled)
    }

    /// Ends the flow with `state`, unless a cancel already ended it.
    fn finish(&self, id: &str, state: ProviderAuthState) {
        let mut flow = self.flow.lock().expect("flow");
        if flow.as_ref().is_some_and(|active| active.id == id) {
            *flow = None;
            drop(flow);
            self.publish(None, state);
        }
    }

    fn publish(&self, owner: Option<String>, state: ProviderAuthState) {
        self.current.send_replace(Current { owner, state });
    }

    fn view(&self, owner: &str, current: &Current) -> ProviderAuthState {
        let mut state = current.state.clone();
        if current.owner.is_none() || current.owner.as_deref() == Some(owner) {
            return state;
        }
        state.interaction = None;
        state.flow_id = None;
        state.authorization_url = None;
        state.expires_at = None;
        state.message = Some("Sign-in is in progress in another client.".to_owned());
        state
    }
}

/// Login on a fresh process: `authenticate` blocks until the browser flow ends.
async fn run_login(program: &str) -> Result<(), CursorError> {
    let client: AcpClient = spawn(SpawnCommand {
        program,
        args: ACP_ARGS,
        cwd: None,
    })
    .await
    .map_err(|error| CursorError::from_acp(CursorOperation::Spawn, &error))?;
    client
        .initialize(AcpClient::cursor_initialize(
            "turin",
            env!("CARGO_PKG_VERSION"),
        ))
        .await
        .map_err(|error| CursorError::from_acp(CursorOperation::Initialize, &error))?;
    client
        .authenticate(authenticate_params())
        .await
        .map(|_| ())
        .map_err(|error| CursorError::from_acp(CursorOperation::Authenticate, &error))
}

fn state(
    instance_id: &ProviderInstanceId,
    phase: ProviderAuthPhase,
    flow_id: Option<SetupOperationId>,
    expires_at: Option<String>,
) -> ProviderAuthState {
    ProviderAuthState {
        instance_id: instance_id.clone(),
        phase,
        flow_id,
        authorization_url: None,
        expires_at,
        message: None,
        methods: Some(vec![ProviderAuthMethod {
            id: SetupOperationId::new("browser").expect("browser method id"),
            account_email: None,
            name: turin_contracts::base_schemas::TrimmedNonEmptyString::new(BROWSER_METHOD_NAME)
                .expect("method name"),
            description: None,
            type_: ProviderAuthMethodType::Agent,
        }]),
        interaction: None,
        credential_owner: Some(ProviderCredentialOwner::T3),
    }
}

fn setup_error(
    instance_id: &ProviderInstanceId,
    operation: &str,
    detail: &str,
) -> ProviderSetupError {
    ProviderSetupError::SetupError {
        instance_id: instance_id.clone(),
        operation: operation.to_owned(),
        detail: detail.to_owned(),
        cause: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    const MISSING_AGENT: &str = "/nonexistent/turin-cursor-agent-for-tests";

    fn controller_for(enabled: bool) -> ProviderAuthController {
        controller(
            ProviderInstanceId::new("cursor").unwrap(),
            MISSING_AGENT.to_owned(),
            enabled,
            Arc::new(|| Box::pin(async {}) as BoxFuture<'static, ()>),
        )
    }

    fn start(
        auth: &ProviderAuthController,
        owner: &str,
    ) -> BoxFuture<'static, Result<ProviderAuthState, ProviderSetupError>> {
        (auth.start)(owner.to_owned(), None, None, None, None)
    }

    #[tokio::test]
    async fn sign_in_refuses_while_cursor_is_disabled() {
        let auth = controller_for(false);
        let error = start(&auth, "t3").await.unwrap_err();
        assert_eq!(error.to_string(), "Enable Cursor before signing in.",);
    }

    #[tokio::test]
    async fn one_flow_at_a_time_and_cancel_ends_it() {
        let auth = controller_for(true);
        let starting = start(&auth, "client-a").await.unwrap();
        assert_eq!(starting.phase, ProviderAuthPhase::Starting);
        let flow_id = starting.flow_id.clone().expect("flow id");
        let refused = start(&auth, "client-b").await.unwrap_err();
        assert_eq!(refused.to_string(), "Cursor setup is already in progress.");

        let wrong = (auth.cancel)("client-b".to_owned(), flow_id.as_str().to_owned())
            .await
            .unwrap_err();
        assert_eq!(
            wrong.to_string(),
            "This sign-in is no longer active in this client."
        );
        let cancelled = (auth.cancel)("client-a".to_owned(), flow_id.as_str().to_owned())
            .await
            .unwrap();
        assert_eq!(cancelled.phase, ProviderAuthPhase::Cancelled);
        assert_eq!(
            cancelled.message.as_deref(),
            Some("Cursor sign-in was cancelled.")
        );
    }

    #[tokio::test]
    async fn a_failed_login_publishes_the_failure_to_subscribers() {
        let auth = controller_for(true);
        let mut states = Box::into_pin((auth.subscribe)("client-a".to_owned()));
        assert_eq!(
            states.next().await.map(|state| state.phase),
            Some(ProviderAuthPhase::Idle)
        );
        start(&auth, "client-a").await.unwrap();
        let mut failed = None;
        while let Some(state) = states.next().await {
            if state.phase == ProviderAuthPhase::Failed {
                failed = Some(state);
                break;
            }
        }
        let failed = failed.expect("login failure is published");
        assert!(
            failed
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with("Cursor sign-in failed: ")),
        );
        assert_eq!(failed.flow_id, None);
    }

    #[tokio::test]
    async fn complete_and_logout_are_not_offered() {
        let auth = controller_for(true);
        let complete = (auth.complete)(
            "client-a".to_owned(),
            ProviderAuthCompleteInput {
                flow_id: "x".to_owned(),
                callback_url: "x".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            complete.to_string(),
            "Finish signing in on the Cursor website. No redirect URL is needed."
        );
        let logout = (auth.logout)(Box::pin(async { Ok(()) })).await.unwrap_err();
        assert!(logout.to_string().contains("cursor-agent logout"));
    }
}
