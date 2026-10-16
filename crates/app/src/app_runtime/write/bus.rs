//! The UI's handle to the write path.
//!
//! `main` creates the bus before the window exists; the composition root
//! installs [`Commands`] into it once the database is open and the write
//! runtime is built. Until then (or when the database failed to open) every
//! call reports why instead of dropping the user's input.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use tokio::runtime::Handle;

use super::commands::{CommandError, Commands};

#[derive(Default)]
struct Shared {
    commands: OnceLock<Commands>,
    failure: OnceLock<String>,
}

#[derive(Clone)]
pub struct CommandBus {
    handle: Handle,
    shared: Arc<Shared>,
}

impl CommandBus {
    pub fn new(handle: Handle) -> Self {
        Self {
            handle,
            shared: Arc::default(),
        }
    }

    pub(crate) fn install(&self, commands: Commands) {
        let _ = self.shared.commands.set(commands);
    }

    /// Startup failed: every command reports `message`.
    pub(crate) fn fail(&self, message: String) {
        let _ = self.shared.failure.set(message);
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub fn commands(&self) -> Result<Commands, CommandError> {
        if let Some(commands) = self.shared.commands.get() {
            return Ok(commands.clone());
        }
        Err(match self.shared.failure.get() {
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
