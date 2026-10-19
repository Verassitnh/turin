//! Port of upstream/apps/web/src/hooks/useThreadActions.ts (+ its tests): settle,
//! unsettle, snooze, pin, unpin, archive, delete, mark unread, the Woke ack,
//! and the Undo notices those actions raise.
//! Also: Port of upstream/apps/web/src/lib/archivedThreadsState.ts.
//!
//! Upstream's hook is React glue over orchestration atoms. Here the logic is
//! `ThreadActions`, generic over two seams:
//!
//! - `ThreadCommands` dispatches the orchestration commands. T22 implements it
//!   over the real command service. Its methods block until the command
//!   settles, so call `ThreadActions` from a background task, not the UI
//!   thread.
//! - `ThreadActionHost` reads the store (thread shells, grants, capability
//!   flags, settings, the route) and runs the UI effects (navigation, toasts,
//!   confirmation dialogs, clearing drafts).
//!
//! Upstream gates each orchestration call inside its command wrapper
//! (`useOrchestrationCommand`), which refuses without the operate grant. This
//! port applies that gate in `ThreadActions::operate`, before `ThreadCommands`
//! is reached, so the gate holds for every implementation of the seam.
//!
//! Upstream's `AsyncResult` values become `ActionResult`. Upstream's
//! `ThreadUndo.begin` / `showThreadUndoNotice` pairs use `ThreadUndo` and
//! `ThreadUndoNotices`, shared by every owner so they expire each other.
//! Upstream's `onArchived` callback is the caller's: `archive_thread` returns
//! whether the archive ran.
// Consumers are wired by T22.
#![allow(dead_code)]

use std::collections::HashSet;
use std::fmt;

use turin_contracts::environment::{ScopedProjectRef, ScopedThreadRef};
use turin_contracts::{EnvironmentId, ProjectId, ThreadId};

use crate::components::sidebar::runtime::thread_settled::{can_snooze, thread_woke_at};
use crate::components::sidebar::runtime::thread_sort::{ThreadSortOrder, pin_order_key_between};
use crate::components::sidebar::sidebar_logic::get_fallback_thread_id_after_delete;
use crate::components::sidebar::thread_status_indicators::format_worktree_path_for_display;
use crate::hooks::thread_undo::{ThreadUndo, ThreadUndoNotices, UndoAction, UndoPlan, UndoRun};
use crate::state::command_result::{CommandFailure, CommandResult, is_command_interrupted};
use crate::state::models::{EnvironmentProject, EnvironmentThreadShell};
use crate::state::projects::is_scratch_project;
use crate::state::right_panel_store::thread_key;
use crate::state::worktree_cleanup::get_orphaned_worktree_path_for_thread;

/// `AuthOrchestrationOperateScope`.
pub const AUTH_ORCHESTRATION_OPERATE_SCOPE: &str = "orchestration:operate";
/// `AuthSourceControlWriteScope`.
pub const AUTH_SOURCE_CONTROL_WRITE_SCOPE: &str = "source-control:write";

/// A dispatch result: a value, a message, or an interruption.
pub type DispatchResult<T = ()> = CommandResult<T, String>;

/// `AsyncResult` for the thread actions.
pub type ActionResult<T = ()> = Result<T, ThreadActionError>;

/// The errors the thread actions raise. Each keeps upstream's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadActionError {
    /// `ThreadArchiveBlockedError`.
    ArchiveBlocked(ScopedThreadRef),
    /// `ThreadSettlementUnsupportedError`.
    SettlementUnsupported(ScopedThreadRef),
    /// `ThreadSnoozeUnsupportedError`.
    SnoozeUnsupported(ScopedThreadRef),
    /// `ThreadSnoozeBlockedError`.
    SnoozeBlocked(ScopedThreadRef),
    /// `ThreadAutoSettleOptOutUnsupportedError`.
    AutoSettleOptOutUnsupported(ScopedThreadRef),
    /// `ThreadPinningUnsupportedError`.
    PinningUnsupported(ScopedThreadRef),
    /// `ThreadPinReorderUnsupportedError`.
    PinReorderUnsupported(ScopedThreadRef),
    /// `ThreadActiveReorderUnsupportedError`.
    ActiveReorderUnsupported(ScopedThreadRef),
    /// `EnvironmentAuthorizationError`: the connection lacks `required_scope`.
    Unauthorized {
        message: &'static str,
        required_scope: &'static str,
    },
    /// The archive ran, then navigating to the draft failed.
    ArchivedNavigationFailed(CommandFailure<String>),
    /// A dispatch that failed or was interrupted.
    Dispatch(CommandFailure<String>),
    /// A discarded composer draft could not be restored; carries the message.
    DraftRestore(String),
}

impl ThreadActionError {
    fn operate_denied() -> Self {
        Self::Unauthorized {
            message: "This connection cannot change threads.",
            required_scope: AUTH_ORCHESTRATION_OPERATE_SCOPE,
        }
    }
}

impl fmt::Display for ThreadActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ArchiveBlocked(_) => "Cannot archive while the provider is active.",
            Self::SettlementUnsupported(_) => {
                "This environment's server does not support settling yet. Update the server to use Settle."
            }
            Self::SnoozeUnsupported(_) => {
                "This environment's server does not support snoozing yet. Update the server to use Snooze."
            }
            Self::SnoozeBlocked(_) => {
                "This thread is waiting on you. Respond to the pending request before snoozing it."
            }
            Self::AutoSettleOptOutUnsupported(_) => {
                "This environment's server does not support turning auto-settle off per thread yet. Update the server to use it."
            }
            Self::PinningUnsupported(_) => {
                "This environment's server does not support pinning yet. Update the server to use Pin."
            }
            Self::PinReorderUnsupported(_) => {
                "This environment's server does not support reordering pinned threads yet. Update the server to reorder pins."
            }
            Self::ActiveReorderUnsupported(_) => {
                "Update this environment's server to reorder active threads."
            }
            Self::Unauthorized { message, .. } => message,
            Self::ArchivedNavigationFailed(failure) | Self::Dispatch(failure) => {
                return fmt::Display::fmt(failure, f);
            }
            Self::DraftRestore(message) => message.as_str(),
        };
        f.write_str(message)
    }
}

impl From<CommandFailure<String>> for ThreadActionError {
    fn from(failure: CommandFailure<String>) -> Self {
        Self::Dispatch(failure)
    }
}

impl std::error::Error for ThreadActionError {}

/// A server capability `readEnvironmentSupports*` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerCapability {
    Settlement,
    AutoSettleOptOut,
    Pinning,
    PinReorder,
    ActiveReorder,
    Snooze,
    VisitedTracking,
}

/// `toastManager.add(stackedThreadToast({ type: "error", ... }))`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorToast {
    pub title: String,
    pub description: String,
}

/// The orchestration commands the thread actions dispatch. T22 implements it.
///
/// Each method is one command, and carries its input as upstream sends it.
/// `unsettle` and `unsnooze` send `reason: "user"`, which pins the thread
/// active so auto-settle stays off until real activity clears it.
pub trait ThreadCommands {
    fn archive(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    fn unarchive(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    fn delete(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    fn settle(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    fn unsettle(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    fn pin(&mut self, thread: &ScopedThreadRef, order_key: Option<&str>) -> DispatchResult;
    fn unpin(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    fn set_auto_settle(&mut self, thread: &ScopedThreadRef, enabled: bool) -> DispatchResult;
    fn reorder_pinned(&mut self, thread: &ScopedThreadRef, order_key: &str) -> DispatchResult;
    fn reorder_active(&mut self, thread: &ScopedThreadRef, order_key: &str) -> DispatchResult;
    fn snooze(&mut self, thread: &ScopedThreadRef, snoozed_until: &str) -> DispatchResult;
    fn unsnooze(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    /// `thread.mark-unread`. Used only when the server tracks visits.
    fn mark_unread(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    /// `thread.visit`. Used only when the server tracks visits.
    fn visit(&mut self, thread: &ScopedThreadRef, visited_at: &str) -> DispatchResult;
    fn stop_session(&mut self, thread: &ScopedThreadRef) -> DispatchResult;
    /// The session query `loadSessionState` reads, and `sessionGrantsScope` checks.
    fn session_grants_scope(
        &mut self,
        environment: &EnvironmentId,
        scope: &str,
    ) -> DispatchResult<bool>;
    fn remove_worktree(
        &mut self,
        environment: &EnvironmentId,
        cwd: &str,
        path: &str,
        force: bool,
    ) -> DispatchResult;
    fn refresh_vcs_status(&mut self, environment: &EnvironmentId, cwd: &str) -> DispatchResult;
}

/// The reads and UI effects the thread actions need. Upstream reads these from
/// atoms, zustand stores, the router and the local API.
pub trait ThreadActionHost {
    fn has_scope(&self, environment: &EnvironmentId, scope: &str) -> bool;
    fn thread(&self, thread: &ScopedThreadRef) -> Option<EnvironmentThreadShell>;
    /// `readEnvironmentThreadRefs` resolved to shells, dropping missing ones.
    fn environment_threads(&self, environment: &EnvironmentId) -> Vec<EnvironmentThreadShell>;
    /// `readThreadShells`: every thread in every environment.
    fn all_threads(&self) -> Vec<EnvironmentThreadShell>;
    fn project(
        &self,
        environment: &EnvironmentId,
        project_id: &ProjectId,
    ) -> Option<EnvironmentProject>;
    fn supports(&self, environment: &EnvironmentId, capability: ServerCapability) -> bool;
    /// `ServerConfig.scratchWorkspaceRoot`.
    fn scratch_workspace_root(&self, environment: &EnvironmentId) -> Option<String>;
    /// `resolveWorktreeCleanup(settings, projectId).worktreeOnDelete`, or
    /// `false` when the environment's settings are not loaded.
    fn worktree_cleanup_on_delete(
        &self,
        environment: &EnvironmentId,
        project_id: &ProjectId,
    ) -> bool;
    fn sort_order(&self) -> ThreadSortOrder;
    fn confirm_thread_delete(&self) -> bool;
    fn confirm_thread_unpin(&self) -> bool;
    /// `new Date().toISOString()`.
    fn now(&self) -> String;
    /// The thread the route shows, if any.
    fn current_route_thread(&self) -> Option<ScopedThreadRef>;
    /// Whether the local API (dialogs, worktree removal) is available.
    fn local_api_available(&self) -> bool;
    fn confirm(&mut self, message: &str, destructive: bool) -> DispatchResult<bool>;
    fn navigate_to_thread(&mut self, thread: &ScopedThreadRef, replace: bool) -> DispatchResult;
    fn navigate_to_root(&mut self, replace: bool) -> DispatchResult;
    fn open_draft_thread(&mut self, project: &ScopedProjectRef) -> DispatchResult;
    /// `uiStateStore.markThreadVisited`, the browser-local watermark.
    fn mark_visited_locally(&mut self, thread_key: &str, visited_at: &str);
    /// `uiStateStore.markThreadUnread`, the browser-local marker.
    fn mark_unread_locally(&mut self, thread_key: &str, completed_at: Option<&str>);
    fn refresh_archived_threads(&mut self, environment: &EnvironmentId);
    /// Releases the thread's uploads and clears its draft, the project draft,
    /// its terminal UI state, and its preview state.
    fn forget_thread_local_state(&mut self, thread: &ScopedThreadRef, project_id: &ProjectId);
    /// `clearThreadPreviewState` alone, for a thread not in the store.
    fn clear_thread_preview_state(&mut self, thread: &ScopedThreadRef);
    fn show_error_toast(&mut self, toast: ErrorToast);
    /// Undo of `discardComposerDraft`: restores the draft discarded under
    /// `discard_id`. The composer's draft store owns it.
    fn restore_discarded_draft(&mut self, _discard_id: &str) -> Result<(), String> {
        Err("The draft cannot be restored.".to_owned())
    }
}

/// `navigateAfterThreadDeletion`: runs the navigation, and returns the error
/// toast when it fails. The deletion has already completed, so the failure
/// is reported and not returned.
pub fn navigate_after_thread_deletion(
    navigate: impl FnOnce() -> DispatchResult,
) -> Option<ErrorToast> {
    match navigate() {
        Ok(()) => None,
        Err(failure) => Some(ErrorToast {
            title: "Thread deleted, but navigation failed".into(),
            description: failure_message(&failure),
        }),
    }
}

/// `requestThreadUnpinConfirmation`. Skips the dialog when confirmation is off
/// or dialogs are unavailable. Returns the user's decision.
pub fn request_thread_unpin_confirmation(
    enabled: bool,
    title: &str,
    confirm: Option<impl FnOnce(&str) -> DispatchResult<bool>>,
) -> DispatchResult<bool> {
    let Some(confirm) = confirm.filter(|_| enabled) else {
        return Ok(true);
    };
    confirm(&format!(
        "Unpin thread \"{title}\"?\nThis will move the thread out of your pinned section."
    ))
}

/// `result._tag === "Failure" && !isAtomCommandInterrupted(result)`.
fn undo_error_is_interrupted(error: &ThreadActionError) -> bool {
    let failure = match error {
        ThreadActionError::Dispatch(failure)
        | ThreadActionError::ArchivedNavigationFailed(failure) => failure,
        _ => return false,
    };
    let result: CommandResult<(), String> = Err(failure.clone());
    is_command_interrupted(&result)
}

/// `error instanceof Error ? error.message : "An error occurred."`.
fn failure_message(failure: &CommandFailure<String>) -> String {
    match failure {
        CommandFailure::Failed(message) => message.clone(),
        CommandFailure::Interrupted => "An error occurred.".into(),
    }
}

/// `topOfPinnedRunOrderKey`: a key that sorts before every arranged pinned
/// thread. `None` when key math cannot produce one.
fn top_of_pinned_run_order_key(threads: &[EnvironmentThreadShell]) -> Option<String> {
    let mut first_key: Option<&str> = None;
    for shell in threads {
        let (Some(_), Some(key)) = (&shell.pinned_at, &shell.pin_order_key) else {
            continue;
        };
        if first_key.is_none_or(|first| key.as_str() < first) {
            first_key = Some(key.as_str());
        }
    }
    pin_order_key_between(None, first_key)
}

fn same_thread(a: &ScopedThreadRef, b: &ScopedThreadRef) -> bool {
    a.environment_id == b.environment_id && a.thread_id == b.thread_id
}

/// The thread actions over the two seams.
pub struct ThreadActions<C, H> {
    commands: C,
    host: H,
    undo: ThreadUndo,
    notices: ThreadUndoNotices,
}

impl<C: ThreadCommands, H: ThreadActionHost> ThreadActions<C, H> {
    /// `undo` and `notices` must be the same handles for every owner in the
    /// app, so one action's claim expires another's Undo.
    pub fn new(commands: C, host: H, undo: ThreadUndo, notices: ThreadUndoNotices) -> Self {
        Self {
            commands,
            host,
            undo,
            notices,
        }
    }

    pub fn commands(&self) -> &C {
        &self.commands
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    /// `threadOperationFailure`: `None` when the environment grants operate.
    fn require_operate(&self, environment: &EnvironmentId) -> ActionResult {
        if self
            .host
            .has_scope(environment, AUTH_ORCHESTRATION_OPERATE_SCOPE)
        {
            Ok(())
        } else {
            Err(ThreadActionError::operate_denied())
        }
    }

    /// An orchestration command. The operate grant is checked against the
    /// target's environment before the command is sent.
    fn operate<T>(
        &mut self,
        thread: &ScopedThreadRef,
        dispatch: impl FnOnce(&mut C) -> DispatchResult<T>,
    ) -> ActionResult<T> {
        self.require_operate(&thread.environment_id)?;
        dispatch(&mut self.commands).map_err(ThreadActionError::from)
    }

    fn unsupported(
        &self,
        thread: &ScopedThreadRef,
        capability: ServerCapability,
    ) -> Option<ThreadActionError> {
        if self.host.supports(&thread.environment_id, capability) {
            return None;
        }
        Some(match capability {
            ServerCapability::Settlement => {
                ThreadActionError::SettlementUnsupported(thread.clone())
            }
            ServerCapability::AutoSettleOptOut => {
                ThreadActionError::AutoSettleOptOutUnsupported(thread.clone())
            }
            ServerCapability::Pinning => ThreadActionError::PinningUnsupported(thread.clone()),
            ServerCapability::PinReorder => {
                ThreadActionError::PinReorderUnsupported(thread.clone())
            }
            ServerCapability::ActiveReorder => {
                ThreadActionError::ActiveReorderUnsupported(thread.clone())
            }
            ServerCapability::Snooze => ThreadActionError::SnoozeUnsupported(thread.clone()),
            ServerCapability::VisitedTracking => unreachable!("visited tracking has a fallback"),
        })
    }

    /// Runs the Undo a notice carries: each offer goes through the same
    /// method the action used, and a failure raises the offer's toast.
    /// An interrupted command does not (`isAtomCommandInterrupted`).
    pub fn undo_notice(&mut self, ids: &[u64]) {
        for UndoRun {
            plan,
            failure_title,
        } in self.notices.take_undo(ids)
        {
            if let Err(error) = self.run_undo_plan(plan) {
                if undo_error_is_interrupted(&error) {
                    continue;
                }
                self.host.show_error_toast(ErrorToast {
                    title: failure_title.into(),
                    description: error.to_string(),
                });
            }
        }
    }

    /// `undoLatestThreadAction`: restores the group the notice shows. `false`
    /// when nothing is left.
    pub fn undo_latest(&mut self) -> bool {
        let Some(notice) = self.notices.current_notice() else {
            return false;
        };
        self.undo_notice(&notice.ids);
        true
    }

    fn run_undo_plan(&mut self, plan: UndoPlan) -> ActionResult {
        match plan {
            UndoPlan::Unarchive { thread, navigate } => self.unarchive_thread(&thread, navigate),
            UndoPlan::Pin { thread, order_key } => self.pin_thread(&thread, order_key.as_deref()),
            UndoPlan::Settle {
                thread,
                was_pinned,
                pin_order_key,
                snoozed_until,
            } => {
                self.unsettle_thread(&thread)?;
                if was_pinned {
                    self.pin_thread(&thread, pin_order_key.as_deref())?;
                }
                match snoozed_until {
                    Some(snoozed_until) => {
                        self.operate(&thread, |commands| commands.snooze(&thread, &snoozed_until))
                    }
                    None => Ok(()),
                }
            }
            UndoPlan::Unsnooze { thread } => self.unsnooze_thread(&thread),
            UndoPlan::RestoreDraft { discard_id } => self
                .host
                .restore_discarded_draft(&discard_id)
                .map_err(ThreadActionError::DraftRestore),
        }
    }

    /// `useMarkThreadUnread`. Visit-tracking servers own the marker. Older
    /// servers keep it in the browser.
    pub fn mark_thread_unread(&mut self, thread: &ScopedThreadRef) {
        if self
            .host
            .supports(&thread.environment_id, ServerCapability::VisitedTracking)
        {
            // Fire and forget, as upstream does. This command is not gated.
            let _ = self.commands.mark_unread(thread);
            return;
        }
        let completed_at = self
            .host
            .thread(thread)
            .and_then(|shell| shell.latest_run)
            .and_then(|run| run.completed_at);
        self.host
            .mark_unread_locally(&thread_key(thread), completed_at.as_deref());
    }

    /// `useAcknowledgeThreadWoke`. Clears a Woke marker by recording a visit at
    /// the wake time.
    pub fn acknowledge_thread_woke(&mut self, thread: &ScopedThreadRef, woke_at: &str) {
        if self
            .host
            .supports(&thread.environment_id, ServerCapability::VisitedTracking)
        {
            let _ = self.commands.visit(thread, woke_at);
            return;
        }
        self.host.mark_visited_locally(&thread_key(thread), woke_at);
    }

    /// `archiveThread`. Returns `true` when the archive ran, and `false` for a
    /// thread not in the store.
    pub fn archive_thread(&mut self, thread: &ScopedThreadRef) -> ActionResult<bool> {
        let Some(shell) = self.host.thread(thread) else {
            return Ok(false);
        };
        if !crate::state::models::thread_runtime_can_archive(shell.runtime.as_ref()) {
            return Err(ThreadActionError::ArchiveBlocked(thread.clone()));
        }

        let should_navigate_to_draft = self
            .host
            .current_route_thread()
            .is_some_and(|current| same_thread(&current, thread));
        let key = thread_key(thread);
        let action = self.undo.begin("archive", &key);
        if let Err(error) = self.operate(thread, |commands| commands.archive(thread)) {
            action.finish();
            return Err(error);
        }
        let now = self.host.now();
        if let Some(woke_at) = thread_woke_at(&shell, &now) {
            self.host.mark_visited_locally(&key, &woke_at);
        }
        self.host.refresh_archived_threads(&thread.environment_id);
        self.notices.show(
            UndoAction::Archived,
            action,
            // Undo also brings the reader back when archiving moved them to a draft.
            UndoPlan::Unarchive {
                thread: thread.clone(),
                navigate: should_navigate_to_draft,
            },
            "Failed to undo archive",
        );

        if should_navigate_to_draft {
            let project = ScopedProjectRef {
                environment_id: shell.environment_id.clone(),
                project_id: shell.project_id.clone(),
            };
            self.host
                .open_draft_thread(&project)
                .map_err(ThreadActionError::ArchivedNavigationFailed)?;
        }
        Ok(true)
    }

    /// `unarchiveThread`.
    pub fn unarchive_thread(&mut self, thread: &ScopedThreadRef, navigate: bool) -> ActionResult {
        self.undo.invalidate("archive", &thread_key(thread));
        self.operate(thread, |commands| commands.unarchive(thread))?;
        self.host.refresh_archived_threads(&thread.environment_id);
        if navigate {
            self.host
                .navigate_to_thread(thread, false)
                .map_err(ThreadActionError::from)?;
        }
        Ok(())
    }

    /// `deleteThread`. `deleted_threads` are threads the caller removes in the
    /// same action, so the fallback and the worktree check do not count them.
    pub fn delete_thread(
        &mut self,
        target: &ScopedThreadRef,
        deleted_threads: &[ScopedThreadRef],
    ) -> ActionResult {
        self.require_operate(&target.environment_id)?;
        let Some(shell) = self.host.thread(target) else {
            // Not in the main store, for example an archived thread. Dispatch the delete directly.
            self.operate(target, |commands| commands.delete(target))?;
            self.host.refresh_archived_threads(&target.environment_id);
            self.host.clear_thread_preview_state(target);
            return Ok(());
        };

        let environment = &target.environment_id;
        let threads = self.host.environment_threads(environment);
        let project = self.host.project(environment, &shell.project_id);
        let deleted_ids: Option<HashSet<ThreadId>> = {
            let ids: HashSet<ThreadId> = deleted_threads
                .iter()
                .filter(|thread| &thread.environment_id == environment)
                .map(|thread| thread.thread_id.clone())
                .collect();
            (!ids.is_empty()).then_some(ids)
        };
        let surviving: Vec<EnvironmentThreadShell> = match &deleted_ids {
            Some(ids) => threads
                .iter()
                .filter(|entry| entry.id == target.thread_id || !ids.contains(&entry.id))
                .cloned()
                .collect(),
            None => threads.clone(),
        };
        let orphaned_worktree_path =
            get_orphaned_worktree_path_for_thread(&surviving, &target.thread_id);
        let display_worktree_path = orphaned_worktree_path
            .as_deref()
            .map(format_worktree_path_for_display);
        let scratch_root = self.host.scratch_workspace_root(environment);
        let local_api = self.host.local_api_available();

        let mut can_delete_worktree = false;
        let worktree_offerable = orphaned_worktree_path.is_some()
            && project.as_ref().is_some_and(|project| {
                !is_scratch_project(&project.workspace_root, scratch_root.as_deref())
            })
            && local_api;
        if worktree_offerable {
            // A failed session lookup means the grant is not known, so the
            // worktree is not offered.
            can_delete_worktree = self
                .commands
                .session_grants_scope(environment, AUTH_SOURCE_CONTROL_WRITE_SCOPE)
                .unwrap_or(false);
            // The grant may have changed while the lookup was in flight.
            self.require_operate(environment)?;
        }

        let mut should_delete_worktree = false;
        let worktree_cleanup_on_delete = self
            .host
            .worktree_cleanup_on_delete(environment, &shell.project_id);
        if can_delete_worktree && local_api && !worktree_cleanup_on_delete {
            let message = [
                "This thread is the only one linked to this worktree:".to_owned(),
                display_worktree_path.clone().unwrap_or_default(),
                String::new(),
                "Delete the worktree too?".to_owned(),
            ]
            .join("\n");
            should_delete_worktree = self
                .host
                .confirm(&message, true)
                .map_err(ThreadActionError::from)?;
        }

        if shell.runtime.is_some() {
            // A failed stop does not block the delete, as upstream.
            let _ = self.operate(target, |commands| commands.stop_session(target));
        }

        let deleted_thread_ids = deleted_ids.unwrap_or_default();
        let should_navigate_to_fallback = self
            .host
            .current_route_thread()
            .is_some_and(|current| same_thread(&current, target));
        let fallback_thread_id = get_fallback_thread_id_after_delete(
            &threads,
            &target.thread_id,
            self.host.sort_order(),
            Some(&deleted_thread_ids),
        );
        self.operate(target, |commands| commands.delete(target))?;
        self.host.refresh_archived_threads(environment);
        self.host
            .forget_thread_local_state(target, &shell.project_id);

        if should_navigate_to_fallback {
            let fallback = fallback_thread_id.and_then(|id| {
                self.host.thread(&ScopedThreadRef {
                    environment_id: environment.clone(),
                    thread_id: id,
                })
            });
            let navigation = navigate_after_thread_deletion(|| match fallback {
                Some(fallback) => self.host.navigate_to_thread(
                    &ScopedThreadRef {
                        environment_id: fallback.environment_id,
                        thread_id: fallback.id,
                    },
                    true,
                ),
                None => self.host.navigate_to_root(true),
            });
            if let Some(toast) = navigation {
                self.host.show_error_toast(toast);
            }
        }

        let (Some(orphaned_worktree_path), Some(project)) = (orphaned_worktree_path, project)
        else {
            return Ok(());
        };
        if !should_delete_worktree {
            return Ok(());
        }

        // The worktree removal is its own gate, so a revoked grant surfaces as
        // a cleanup failure, not a failed delete.
        let remove_result = if self
            .host
            .has_scope(environment, AUTH_SOURCE_CONTROL_WRITE_SCOPE)
        {
            self.commands
                .remove_worktree(
                    environment,
                    &project.workspace_root,
                    &orphaned_worktree_path,
                    true,
                )
                .map_err(ThreadActionError::from)
        } else {
            Err(ThreadActionError::Unauthorized {
                message: "This connection can no longer remove worktrees.",
                required_scope: AUTH_SOURCE_CONTROL_WRITE_SCOPE,
            })
        };
        let refresh_result = match &remove_result {
            Ok(()) => Some(
                self.commands
                    .refresh_vcs_status(environment, &project.workspace_root)
                    .map_err(ThreadActionError::from),
            ),
            Err(_) => None,
        };
        let cleanup_failure = match (&remove_result, refresh_result) {
            (Err(error), _) => Some((true, error.clone())),
            (Ok(()), Some(Err(error))) => Some((false, error)),
            _ => None,
        };
        if let Some((removal_failed, error)) = cleanup_failure {
            let message = match &error {
                ThreadActionError::Dispatch(failure) => failure_message(failure),
                other => other.to_string(),
            };
            let shown = display_worktree_path.unwrap_or_else(|| orphaned_worktree_path.clone());
            tracing::error!(
                thread_id = %target.thread_id,
                project_cwd = %project.workspace_root,
                worktree_path = %orphaned_worktree_path,
                error = %message,
                "Worktree cleanup failed after thread deletion"
            );
            self.host.show_error_toast(ErrorToast {
                title: if removal_failed {
                    "Failed to delete worktree".into()
                } else {
                    "Worktree deleted, but Git status refresh failed".into()
                },
                description: if removal_failed {
                    format!("Could not remove {shown}. {message}")
                } else {
                    message
                },
            });
        }
        Ok(())
    }

    /// `confirmAndDeleteThread`. Confirms first when that setting is on, and
    /// re-checks the grant when the dialog returns.
    pub fn confirm_and_delete_thread(
        &mut self,
        target: &ScopedThreadRef,
        deleted_threads: &[ScopedThreadRef],
    ) -> ActionResult {
        self.require_operate(&target.environment_id)?;
        let title = self
            .host
            .thread(target)
            .map(|shell| shell.title)
            .unwrap_or_else(|| "this thread".into());
        if self.host.confirm_thread_delete() && self.host.local_api_available() {
            let confirmed = self
                .host
                .confirm(
                    &format!(
                        "Delete thread \"{title}\"?\nThis permanently clears conversation history for this thread."
                    ),
                    true,
                )
                .map_err(ThreadActionError::from)?;
            if !confirmed {
                return Ok(());
            }
        }
        self.delete_thread(target, deleted_threads)
    }

    /// `settleThread`. Settling also drops the pin and the snooze server-side,
    /// so the Undo puts those back.
    pub fn settle_thread(&mut self, target: &ScopedThreadRef) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::Settlement) {
            return Err(error);
        }
        let resolved = self.host.thread(target);
        let now = self.host.now();
        let woke_at = resolved
            .as_ref()
            .and_then(|shell| thread_woke_at(shell, &now));
        let was_pinned = resolved
            .as_ref()
            .is_some_and(|shell| shell.pinned_at.is_some());
        let pin_order_key = resolved
            .as_ref()
            .filter(|shell| shell.pinned_at.is_some())
            .and_then(|shell| shell.pin_order_key.clone());
        let snoozed_until = resolved
            .as_ref()
            .and_then(|shell| shell.snoozed_until.clone());

        // An older unpin or snooze Undo would re-pin or re-snooze, and the
        // server treats either as a promotion that un-settles. Settling supersedes them.
        let key = thread_key(target);
        self.undo.invalidate("pin", &key);
        self.undo.invalidate("snooze", &key);
        let action = self.undo.begin("settle", &key);
        if let Err(error) = self.operate(target, |commands| commands.settle(target)) {
            action.finish();
            return Err(error);
        }
        if let Some(woke_at) = woke_at {
            self.host.mark_visited_locally(&key, &woke_at);
        }
        self.notices.show(
            UndoAction::Settled,
            action,
            UndoPlan::Settle {
                thread: target.clone(),
                was_pinned,
                pin_order_key,
                snoozed_until,
            },
            "Failed to undo settle",
        );
        Ok(())
    }

    /// `unsettleThread`. Reason `user` pins the thread active, so auto-settle
    /// stays suppressed until real activity clears the pin.
    pub fn unsettle_thread(&mut self, target: &ScopedThreadRef) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::Settlement) {
            return Err(error);
        }
        self.undo.invalidate("settle", &thread_key(target));
        self.operate(target, |commands| commands.unsettle(target))
    }

    /// `setThreadAutoSettle`: turns automatic settlement on or off for one thread.
    pub fn set_thread_auto_settle(
        &mut self,
        target: &ScopedThreadRef,
        enabled: bool,
    ) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::AutoSettleOptOut) {
            return Err(error);
        }
        self.operate(target, |commands| commands.set_auto_settle(target, enabled))
    }

    /// `pinThread`. Every pin path places the thread at the top of the
    /// arranged run unless the caller passes an anchor (the sidebar knows the
    /// displayed order). `order_key` is sent only to servers that decode it.
    pub fn pin_thread(
        &mut self,
        target: &ScopedThreadRef,
        order_key: Option<&str>,
    ) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::Pinning) {
            return Err(error);
        }
        let order_key = if self
            .host
            .supports(&target.environment_id, ServerCapability::PinReorder)
        {
            match order_key {
                Some(key) => Some(key.to_owned()),
                None => top_of_pinned_run_order_key(&self.host.all_threads()),
            }
        } else {
            None
        };
        self.undo.invalidate("pin", &thread_key(target));
        self.operate(target, |commands| {
            commands.pin(target, order_key.as_deref())
        })
    }

    /// `unpinThread`. Offers an Undo that re-pins at the thread's old key.
    pub fn unpin_thread(&mut self, target: &ScopedThreadRef) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::Pinning) {
            return Err(error);
        }
        let order_key = self
            .host
            .thread(target)
            .and_then(|shell| shell.pin_order_key);
        let key = thread_key(target);
        let action = self.undo.begin("pin", &key);
        let result = self.operate(target, |commands| commands.unpin(target));
        match result {
            Ok(()) if action.is_current() => {
                self.notices.show(
                    UndoAction::Unpinned,
                    action,
                    UndoPlan::Pin {
                        thread: target.clone(),
                        order_key,
                    },
                    "Failed to undo unpin",
                );
                Ok(())
            }
            other => {
                action.finish();
                other
            }
        }
    }

    /// `confirmAndUnpinThread`.
    pub fn confirm_and_unpin_thread(&mut self, target: &ScopedThreadRef) -> ActionResult {
        self.require_operate(&target.environment_id)?;
        let title = self
            .host
            .thread(target)
            .map(|shell| shell.title)
            .unwrap_or_else(|| "this thread".into());
        let enabled = self.host.confirm_thread_unpin();
        let confirm_available = self.host.local_api_available();
        let host = &mut self.host;
        let confirmed = request_thread_unpin_confirmation(
            enabled,
            &title,
            confirm_available.then_some(|message: &str| host.confirm(message, false)),
        )
        .map_err(ThreadActionError::from)?;
        if !confirmed {
            return Ok(());
        }
        self.unpin_thread(target)
    }

    /// `reorderPinnedThread`. Callers enable drag only on capable environments.
    /// This guard covers capability changes mid-drag.
    pub fn reorder_pinned_thread(
        &mut self,
        target: &ScopedThreadRef,
        order_key: &str,
    ) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::PinReorder) {
            return Err(error);
        }
        self.undo.invalidate("pin", &thread_key(target));
        self.operate(target, |commands| {
            commands.reorder_pinned(target, order_key)
        })
    }

    /// `reorderActiveThread`.
    pub fn reorder_active_thread(
        &mut self,
        target: &ScopedThreadRef,
        order_key: &str,
    ) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::ActiveReorder) {
            return Err(error);
        }
        self.operate(target, |commands| {
            commands.reorder_active(target, order_key)
        })
    }

    /// `unsnoozeThread`.
    pub fn unsnooze_thread(&mut self, target: &ScopedThreadRef) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::Snooze) {
            return Err(error);
        }
        self.undo.invalidate("snooze", &thread_key(target));
        self.operate(target, |commands| commands.unsnooze(target))
    }

    /// `snoozeThread`. Blocked-on-you work and queued turns cannot be snoozed
    /// away. The client check mirrors the server's, so the UI rejects before a
    /// round trip.
    pub fn snooze_thread(&mut self, target: &ScopedThreadRef, snoozed_until: &str) -> ActionResult {
        if let Some(error) = self.unsupported(target, ServerCapability::Snooze) {
            return Err(error);
        }
        let resolved = self.host.thread(target);
        if let Some(shell) = &resolved {
            let now = self.host.now();
            if !can_snooze(shell, &now) {
                return Err(ThreadActionError::SnoozeBlocked(target.clone()));
            }
        }
        let key = thread_key(target);
        let action = self.undo.begin("snooze", &key);
        if let Err(error) = self.operate(target, |commands| commands.snooze(target, snoozed_until))
        {
            action.finish();
            return Err(error);
        }
        // Snooze hides the row, so keep its confirmation in the sidebar.
        self.notices.show(
            UndoAction::Snoozed,
            action,
            UndoPlan::Unsnooze {
                thread: target.clone(),
            },
            "Failed to wake thread",
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "use_thread_actions_tests.rs"]
mod tests;
