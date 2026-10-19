//! The Undo for thread lifecycle actions: archive, settle, snooze and unpin
//! (`ThreadUndo.begin` + `showThreadUndoNotice` in upstream `useThreadActions.ts`).
//!
//! The claims and the notice queue are the app-wide handles of the composer draft host, so a
//! discarded draft and a parked thread share one notice and Cmd+Z undoes the newest group.
//! A later action of the same kind on a thread expires the earlier Undo (`invalidate`). Each Undo
//! runs through the same backend commands the original action used.

use std::time::Duration;

use gpui_kit::{App, Context, Window};
use turin_contracts::environment::ScopedThreadRef;

use super::Sidebar;
use super::view_model::ThreadRef;
use crate::hooks::thread_undo::{
    ThreadUndo, ThreadUndoClaim, ThreadUndoNotices, UndoAction, UndoPlan,
};
use crate::shell::composer_draft_host::ComposerDraftHostGlobal;

/// `showThreadUndoNotice`'s 5 second expiry.
const UNDO_EXPIRY: Duration = Duration::from_secs(5);

/// The `ThreadUndo` kinds (`ThreadUndo.begin("archive" | "pin" | "settle" | "snooze", ...)`).
pub(crate) mod kind {
    pub const ARCHIVE: &str = "archive";
    pub const PIN: &str = "pin";
    pub const SETTLE: &str = "settle";
    pub const SNOOZE: &str = "snooze";
}

pub(crate) fn scoped(thread: &ThreadRef) -> ScopedThreadRef {
    ScopedThreadRef {
        environment_id: thread.environment_id.clone(),
        thread_id: thread.thread_id.clone(),
    }
}

fn thread_ref(thread: &ScopedThreadRef) -> ThreadRef {
    ThreadRef {
        environment_id: thread.environment_id.clone(),
        thread_id: thread.thread_id.clone(),
    }
}

fn handles(cx: &App) -> Option<(ThreadUndo, ThreadUndoNotices)> {
    let host = cx.try_global::<ComposerDraftHostGlobal>()?.0.read(cx);
    Some((host.undo.clone(), host.notices.clone()))
}

impl Sidebar {
    /// `ThreadUndo.begin`: claims one kind of action on a thread. `None` when the shell has no
    /// undo queue (then the action just runs, without an Undo).
    pub(crate) fn begin_undo(
        &self,
        kind: &str,
        thread: &ThreadRef,
        cx: &App,
    ) -> Option<ThreadUndoClaim> {
        handles(cx).map(|(undo, _)| undo.begin(kind, &thread.key()))
    }

    /// `ThreadUndo.invalidate`: a newer action supersedes the earlier Undo.
    pub(crate) fn invalidate_undo(&self, kind: &str, thread: &ThreadRef, cx: &App) {
        if let Some((undo, _)) = handles(cx) {
            undo.invalidate(kind, &thread.key());
        }
    }

    /// `showThreadUndoNotice`: offers Undo for 5 seconds, grouped with consecutive offers of the
    /// same kind.
    pub(crate) fn show_undo(
        &mut self,
        action: UndoAction,
        claim: Option<ThreadUndoClaim>,
        plan: UndoPlan,
        failure_title: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(claim) = claim else {
            return;
        };
        let Some((_, notices)) = handles(cx) else {
            return;
        };
        notices.show(action, claim, plan, failure_title);
        let expiring = notices.clone();
        // A newer offer restarts the clock (upstream clears and re-arms one timer).
        self.undo_expiry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_EXPIRY).await;
            this.update(cx, |_, cx| {
                expiring.expire();
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// `undoLatestThreadAction`: undoes the newest notice's group (a discarded draft or parked
    /// threads). Nothing happens when nothing is left to undo.
    pub(crate) fn undo_latest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(global) = cx.try_global::<ComposerDraftHostGlobal>() else {
            return;
        };
        let host = global.0.clone();
        let outcome = host.update(cx, |host, cx| {
            let outcome = host.undo_latest();
            cx.notify();
            outcome
        });
        if let Some(message) = outcome.draft_failure {
            self.toast_error("Failed to restore draft", &message, window, cx);
        }
        for run in outcome.thread_runs {
            self.run_undo_plan(run.plan, run.failure_title, window, cx);
        }
        cx.notify();
    }

    /// Runs one Undo through the backend commands the action used; a failure raises the
    /// offer's toast.
    fn run_undo_plan(
        &mut self,
        plan: UndoPlan,
        failure_title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match plan {
            UndoPlan::Unarchive { thread, navigate } => {
                let target = thread_ref(&thread);
                self.invalidate_undo(kind::ARCHIVE, &target, cx);
                let key = target.key();
                let task = self.backend.unarchive(target, cx);
                self.run_command(task, failure_title, window, cx, move |this, ok, _, cx| {
                    // Undo also brings the reader back when archiving moved them away.
                    if ok && navigate {
                        this.navigate_to_thread(&key, cx);
                    }
                });
            }
            UndoPlan::Pin { thread, order_key } => {
                let target = thread_ref(&thread);
                self.invalidate_undo(kind::PIN, &target, cx);
                let task = self.backend.pin(target, order_key, cx);
                self.run_command(task, failure_title, window, cx, |_, _, _, _| {});
            }
            UndoPlan::Unsnooze { thread } => {
                let target = thread_ref(&thread);
                self.invalidate_undo(kind::SNOOZE, &target, cx);
                let task = self.backend.unsnooze(target, cx);
                self.run_command(task, failure_title, window, cx, |_, _, _, _| {});
            }
            UndoPlan::Settle {
                thread,
                was_pinned,
                pin_order_key,
                snoozed_until,
            } => {
                // Settling dropped the pin and the snooze server-side, so Undo puts them back,
                // each step only after the one before it worked.
                let target = thread_ref(&thread);
                self.invalidate_undo(kind::SETTLE, &target, cx);
                let first = self.backend.unsettle(target.clone(), cx);
                cx.spawn_in(window, async move |this, cx| {
                    use super::sidebar_logic::CommandOutcome;
                    let mut outcome = first.await;
                    if outcome == CommandOutcome::Success && was_pinned {
                        let Ok(next) = this.update(cx, |this, cx| {
                            this.invalidate_undo(kind::PIN, &target, cx);
                            this.backend.pin(target.clone(), pin_order_key, cx)
                        }) else {
                            return;
                        };
                        outcome = next.await;
                    }
                    if outcome == CommandOutcome::Success
                        && let Some(until) = snoozed_until
                    {
                        let Ok(next) = this.update(cx, |this, cx| {
                            this.backend.snooze(target.clone(), until, cx)
                        }) else {
                            return;
                        };
                        outcome = next.await;
                    }
                    if let CommandOutcome::Failure(error) = outcome {
                        this.update_in(cx, |this, window, cx| {
                            this.toast_error(failure_title, &error, window, cx)
                        })
                        .ok();
                    }
                })
                .detach();
            }
            // Handled by the draft host.
            UndoPlan::RestoreDraft { .. } => {}
        }
    }
}
