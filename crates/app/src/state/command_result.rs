//! The result of dispatching a command, in place of upstream's
//! `AtomCommandResult` (`state/runtime.ts`): a value, a typed failure, or an
//! interruption (the command's task was cancelled before it finished).

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandFailure<E> {
    #[error("{0}")]
    Failed(E),
    #[error("The command was interrupted.")]
    Interrupted,
}

pub type CommandResult<T, E> = Result<T, CommandFailure<E>>;

/// `isAtomCommandInterrupted`.
pub fn is_command_interrupted<T, E>(result: &CommandResult<T, E>) -> bool {
    matches!(result, Err(CommandFailure::Interrupted))
}
