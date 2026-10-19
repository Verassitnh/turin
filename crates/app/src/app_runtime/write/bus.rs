//! The UI's handle to the write path.
//!
//! `main` creates the bus before the window exists; the composition root
//! installs [`Commands`] into it once the database is open and the write
//! runtime is built. Until then (or when the database failed to open) every
//! call reports why instead of dropping the user's input.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use tokio::runtime::Handle;
use turin_provider_core::server_provider::ServerProvider;

use super::commands::{CommandError, Commands};

/// What the user can do about a database that would not open
/// (the startup failure dialog's buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseRecovery {
    /// Try opening the same database again.
    Retry,
    /// Move the unopenable database aside (never deleting it) and open a new one.
    StartFresh,
}

struct Shared {
    commands: OnceLock<Commands>,
    /// Why startup failed, `None` while starting or once it succeeded. A
    /// database that would not open can be retried, so this can clear.
    failed: tokio::sync::watch::Sender<Option<String>>,
    /// Whether the failure is an unopenable database the user can recover
    /// from ([`DatabaseRecovery`]).
    recoverable: std::sync::atomic::AtomicBool,
    recovery: tokio::sync::mpsc::UnboundedSender<DatabaseRecovery>,
    recovery_requests:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<DatabaseRecovery>>>,
    /// The provider status list (`ProviderRegistry::get_providers`), kept
    /// current by the composition root.
    providers: tokio::sync::watch::Sender<Arc<Vec<ServerProvider>>>,
}

#[derive(Clone)]
pub struct CommandBus {
    handle: Handle,
    shared: Arc<Shared>,
}

impl CommandBus {
    pub fn new(handle: Handle) -> Self {
        let (recovery, recovery_requests) = tokio::sync::mpsc::unbounded_channel();
        Self {
            handle,
            shared: Arc::new(Shared {
                commands: OnceLock::new(),
                failed: tokio::sync::watch::channel(None).0,
                recoverable: std::sync::atomic::AtomicBool::new(false),
                recovery,
                recovery_requests: std::sync::Mutex::new(Some(recovery_requests)),
                providers: tokio::sync::watch::channel(Arc::default()).0,
            }),
        }
    }

    pub(crate) fn install(&self, commands: Commands) {
        let _ = self.shared.commands.set(commands);
    }

    /// Startup failed: every command reports `message`.
    pub(crate) fn fail(&self, message: String) {
        self.shared
            .recoverable
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.shared.failed.send_replace(Some(message));
    }

    /// [`Self::fail`] for a database that would not open: the user can retry or
    /// start fresh ([`Self::request_recovery`]).
    pub(crate) fn fail_recoverable(&self, message: String) {
        self.shared
            .recoverable
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.shared.failed.send_replace(Some(message));
    }

    /// The database opened after all: commands work again and the UI hears
    /// that the failure is over.
    pub(crate) fn clear_failure(&self) {
        self.shared
            .recoverable
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.shared.failed.send_replace(None);
    }

    /// Whether the current startup failure offers [`DatabaseRecovery`].
    pub fn failure_is_recoverable(&self) -> bool {
        self.shared
            .recoverable
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The user's choice on the startup failure dialog.
    pub fn request_recovery(&self, action: DatabaseRecovery) {
        let _ = self.shared.recovery.send(action);
    }

    /// The composition root's end of [`Self::request_recovery`]; taken once.
    pub(crate) fn take_recovery_requests(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<DatabaseRecovery>> {
        self.shared
            .recovery_requests
            .lock()
            .expect("recovery requests")
            .take()
    }

    /// Holds the startup failure once there is one (`None` while starting or
    /// when startup succeeded).
    pub fn startup_failure(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.shared.failed.subscribe()
    }

    pub(crate) fn publish_providers(&self, providers: Vec<ServerProvider>) {
        self.shared.providers.send_replace(Arc::new(providers));
    }

    /// Provider status snapshots; `changed()` wakes on every registry update.
    pub fn providers(&self) -> tokio::sync::watch::Receiver<Arc<Vec<ServerProvider>>> {
        self.shared.providers.subscribe()
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub fn commands(&self) -> Result<Commands, CommandError> {
        if let Some(commands) = self.shared.commands.get() {
            return Ok(commands.clone());
        }
        Err(match self.shared.failed.borrow().as_deref() {
            Some(message) => CommandError::new(format!("Turin could not start: {message}")),
            None => CommandError::new("Turin is still starting. Try again in a moment."),
        })
    }

    /// Runs `command` on the tokio runtime and hands its result to `done`
    /// (on the runtime; hop to the UI thread from there).
    pub fn spawn<T, F, Fut>(
        &self,
        command: F,
        done: impl FnOnce(Result<T, CommandError>) + Send + 'static,
    ) where
        T: Send + 'static,
        F: FnOnce(Commands) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, CommandError>> + Send + 'static,
    {
        let commands = self.commands();
        self.handle.spawn(async move {
            let result = match commands {
                Ok(commands) => command(commands).await,
                Err(error) => Err(error),
            };
            done(result);
        });
    }

    /// [`Self::spawn`] as a GPUI task the caller awaits on the UI thread.
    pub fn task<T, F, Fut>(
        &self,
        cx: &mut gpui_kit::App,
        command: F,
    ) -> gpui_kit::Task<Result<T, CommandError>>
    where
        T: Send + 'static,
        F: FnOnce(Commands) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, CommandError>> + Send + 'static,
    {
        let (done, outcome) = futures::channel::oneshot::channel();
        self.spawn(command, move |result| {
            let _ = done.send(result);
        });
        cx.spawn(async move |_| {
            outcome
                .await
                .unwrap_or_else(|_| Err(CommandError::new("The command was interrupted.")))
        })
    }

    /// Runs `command` to completion from a non-async caller: a background
    /// task or the UI thread (the commands are short database commits). Never
    /// call it from a tokio worker.
    pub fn run_blocking<T, F, Fut>(&self, command: F) -> Result<T, CommandError>
    where
        F: FnOnce(Commands) -> Fut,
        Fut: Future<Output = Result<T, CommandError>>,
    {
        let commands = self.commands()?;
        self.handle.block_on(command(commands))
    }
}

/// The app-wide bus (`cx.global::<CommandBusGlobal>()`).
pub struct CommandBusGlobal(pub CommandBus);

impl gpui_kit::Global for CommandBusGlobal {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_startup_failure_reaches_subscribers() {
        let bus = CommandBus::new(Handle::current());
        let mut failure = bus.startup_failure();
        assert_eq!(*failure.borrow(), None);
        bus.fail("disk is full".into());
        failure.changed().await.unwrap();
        assert_eq!(failure.borrow().as_deref(), Some("disk is full"));
        assert!(bus.commands().is_err());
    }
}
