//! [`ThreadCommands`] over the command bus: archive, delete, pin, settle,
//! snooze and the rest dispatch the same orchestrator commands a web client
//! sends. The methods block until the command settles (they are short
//! database commits); the trait's contract is that callers run them off the
//! UI thread's hot path.

use serde_json::{Value, json};
use turin_contracts::EnvironmentId;
use turin_contracts::environment::ScopedThreadRef;

use super::bus::CommandBus;
use crate::hooks::use_thread_actions::{DispatchResult, ThreadCommands};
use crate::state::command_result::CommandFailure;

pub struct LiveThreadCommands {
    bus: CommandBus,
}

impl LiveThreadCommands {
    pub fn new(bus: CommandBus) -> Self {
        Self { bus }
    }

    fn thread_command(
        &self,
        kind: &'static str,
        thread: &ScopedThreadRef,
        extra: Value,
    ) -> DispatchResult {
        let thread_id = thread.thread_id.clone();
        self.bus
            .run_blocking(move |commands| async move {
                commands.thread_command(kind, &thread_id, extra).await
            })
            .map_err(|error| CommandFailure::Failed(error.message))
    }
}

impl ThreadCommands for LiveThreadCommands {
    fn archive(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.archive", thread, json!({}))
    }

    fn unarchive(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.unarchive", thread, json!({}))
    }

    fn delete(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.delete", thread, json!({}))
    }

    fn settle(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.settle", thread, json!({}))
    }

    fn unsettle(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.unsettle", thread, json!({"reason": "user"}))
    }

    fn pin(&mut self, thread: &ScopedThreadRef, order_key: Option<&str>) -> DispatchResult {
        let extra = match order_key {
            Some(key) => json!({"orderKey": key}),
            None => json!({}),
        };
        self.thread_command("thread.pin", thread, extra)
    }

    fn unpin(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.unpin", thread, json!({}))
    }

    fn set_auto_settle(&mut self, thread: &ScopedThreadRef, enabled: bool) -> DispatchResult {
        self.thread_command(
            "thread.auto-settle.set",
            thread,
            json!({"enabled": enabled}),
        )
    }

    fn reorder_pinned(&mut self, thread: &ScopedThreadRef, order_key: &str) -> DispatchResult {
        self.thread_command("thread.pin.reorder", thread, json!({"orderKey": order_key}))
    }

    fn reorder_active(&mut self, thread: &ScopedThreadRef, order_key: &str) -> DispatchResult {
        self.thread_command(
            "thread.active.reorder",
            thread,
            json!({"orderKey": order_key}),
        )
    }

    fn snooze(&mut self, thread: &ScopedThreadRef, snoozed_until: &str) -> DispatchResult {
        self.thread_command(
            "thread.snooze",
            thread,
            json!({"snoozedUntil": snoozed_until}),
        )
    }

    fn unsnooze(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.unsnooze", thread, json!({"reason": "user"}))
    }

    fn mark_unread(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.thread_command("thread.mark-unread", thread, json!({}))
    }

    fn visit(&mut self, thread: &ScopedThreadRef, visited_at: &str) -> DispatchResult {
        self.thread_command("thread.visit", thread, json!({"visitedAt": visited_at}))
    }

    fn stop_session(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        let thread_id = thread.thread_id.clone();
        self.bus
            .run_blocking(move |commands| async move { commands.stop_thread(&thread_id).await })
            .map_err(|error| CommandFailure::Failed(error.message))
    }

    /// The in-process owner holds every scope.
    fn session_grants_scope(
        &mut self,
        _environment: &EnvironmentId,
        _scope: &str,
    ) -> DispatchResult<bool> {
        Ok(true)
    }

    fn remove_worktree(
        &mut self,
        _environment: &EnvironmentId,
        _cwd: &str,
        _path: &str,
        _force: bool,
    ) -> DispatchResult {
        Err(CommandFailure::Failed(
            "Removing worktrees is not wired yet (git worktree service).".into(),
        ))
    }

    /// Nothing caches VCS status in the app yet.
    fn refresh_vcs_status(&mut self, _environment: &EnvironmentId, _cwd: &str) -> DispatchResult {
        Ok(())
    }
}
