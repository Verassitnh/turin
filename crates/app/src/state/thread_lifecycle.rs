//! Port of upstream/packages/client-runtime/src/state/threadLifecycle.ts and
//! the optimistic `apply` functions `createThreadEnvironmentAtoms` passes to
//! `optimistic.wrap` in threadCommands.ts (settle, unsettle, snooze, unsnooze,
//! setAutoSettle, pin, unpin, reorderPin, reorderActive).
//!
//! Upstream keeps the pending updates in an atom and wraps each lifecycle
//! command so the visible shell snapshot shows the change before the server
//! answers. Here the pending list lives in [`ClientStateCore`]
//! (`ClientStateEvent::Optimistic*`), updates are data (events must be
//! `Clone + PartialEq`, so no closures), and [`with_optimistic`] plays the
//! part of `wrap`.
//!
//! [`ClientStateCore`]: crate::state::client_state::ClientStateCore

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::state::contracts::{
    OrchestrationV2SettleOverride, OrchestrationV2ShellThreadStatus, OrchestrationV2ThreadShell,
    ThreadId, TrimmedNonEmptyString, UtcDateTime,
};
use crate::state::events::{ClientStateEvent, ClientStateSender};
use crate::state::operations::commands::{
    CommandError, CommandMetadata, DispatchResult, EnvironmentCommands,
};
use crate::state::shell_reducer::ShellSnapshot;
use crate::state::time::{epoch_millis, iso, parse_ms};

/// One lifecycle command's optimistic effect on a thread row.
#[derive(Clone, Debug, PartialEq)]
pub enum OptimisticThreadUpdate {
    Settle,
    /// `user`: the command's `reason` is `"user"`.
    Unsettle {
        user: bool,
    },
    /// `snoozed_until` is the command's ISO input, compared as text like upstream.
    Snooze {
        snoozed_until: String,
    },
    Unsnooze,
    SetAutoSettle {
        enabled: bool,
    },
    Pin {
        order_key: Option<TrimmedNonEmptyString>,
    },
    Unpin,
    ReorderPin {
        order_key: TrimmedNonEmptyString,
    },
    ReorderActive {
        order_key: TrimmedNonEmptyString,
    },
}

fn is_busy(
    status: OrchestrationV2ShellThreadStatus,
    statuses: &[OrchestrationV2ShellThreadStatus],
) -> bool {
    statuses.contains(&status)
}

impl OptimisticThreadUpdate {
    /// The row as it would look once the command lands. `None` leaves the row
    /// untouched. `accepted` is whether the server acknowledged the command:
    /// before that, settle and snooze hold back while the thread is busy or
    /// waiting on the user, since the server would reject them.
    pub fn apply(
        &self,
        thread: &OrchestrationV2ThreadShell,
        now: &UtcDateTime,
        accepted: bool,
    ) -> Option<OrchestrationV2ThreadShell> {
        use OrchestrationV2ShellThreadStatus as Status;
        let mut next = thread.clone();
        match self {
            Self::Settle => {
                if !accepted
                    && (thread.pending_runtime_request.is_some()
                        || is_busy(
                            thread.status,
                            &[
                                Status::Preparing,
                                Status::Queued,
                                Status::Starting,
                                Status::Running,
                                Status::Waiting,
                            ],
                        ))
                {
                    return None;
                }
                next.pending_runtime_request = None;
                next.settled_override = Some(OrchestrationV2SettleOverride::Settled);
                next.settled_at = Some(
                    if thread.settled_override == Some(OrchestrationV2SettleOverride::Settled) {
                        thread.settled_at.unwrap_or(*now)
                    } else {
                        *now
                    },
                );
                next.unsettled_at = Some(None);
                next.active_order_key = Some(None);
                next.pinned_at = Some(None);
                next.pin_order_key = Some(None);
                next.snoozed_at = Some(None);
                next.snoozed_until = Some(None);
            }
            Self::Unsettle { user } => {
                next.settled_override = user.then_some(OrchestrationV2SettleOverride::Active);
                next.settled_at = None;
                next.unsettled_at = Some(
                    if thread.settled_override == Some(OrchestrationV2SettleOverride::Active) {
                        thread.unsettled_at.flatten()
                    } else {
                        Some(*now)
                    },
                );
            }
            Self::Snooze { snoozed_until } => {
                let blocked = !accepted
                    && (thread.pending_runtime_request.is_some()
                        || is_busy(
                            thread.status,
                            &[Status::Preparing, Status::Queued, Status::Starting],
                        ));
                let in_future =
                    parse_ms(snoozed_until).is_some_and(|until| until > epoch_millis(now));
                if blocked || !in_future {
                    return None;
                }
                let until: UtcDateTime = snoozed_until.parse().ok()?;
                let current_until = thread.snoozed_until.flatten();
                next.pending_runtime_request = None;
                next.snoozed_at = Some(Some(
                    if current_until.is_some_and(|current| iso(&current) == *snoozed_until) {
                        thread.snoozed_at.flatten().unwrap_or(*now)
                    } else {
                        *now
                    },
                ));
                next.snoozed_until = Some(Some(until));
            }
            Self::Unsnooze => {
                next.snoozed_until = Some(None);
                next.snoozed_at = Some(None);
            }
            Self::SetAutoSettle { enabled } => {
                next.auto_settle_disabled_at = Some(if *enabled {
                    None
                } else {
                    Some(thread.auto_settle_disabled_at.flatten().unwrap_or(*now))
                });
            }
            Self::Pin { order_key } => {
                let pinned_at = thread.pinned_at.flatten();
                next.pin_order_key = if pinned_at.is_none() {
                    Some(order_key.clone())
                } else {
                    thread.pin_order_key.clone()
                };
                next.pinned_at = Some(Some(pinned_at.unwrap_or(*now)));
                if thread.settled_override == Some(OrchestrationV2SettleOverride::Settled) {
                    next.settled_override = Some(OrchestrationV2SettleOverride::Active);
                    next.settled_at = None;
                    next.unsettled_at = Some(Some(*now));
                }
                next.snoozed_until = Some(None);
                next.snoozed_at = Some(None);
            }
            Self::Unpin => {
                next.pinned_at = Some(None);
                next.pin_order_key = Some(None);
            }
            Self::ReorderPin { order_key } => {
                next.pin_order_key = Some(Some(order_key.clone()));
            }
            Self::ReorderActive { order_key } => {
                next.active_order_key = Some(Some(order_key.clone()));
            }
        }
        // Keep the row's identity when nothing visible changed.
        (next != *thread).then_some(next)
    }
}

/// Identifies one in-flight optimistic update.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OptimisticUpdateId(pub u64);

impl OptimisticUpdateId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// `PendingThreadUpdate`.
#[derive(Clone, Debug, PartialEq)]
struct PendingThreadUpdate {
    id: OptimisticUpdateId,
    thread_id: ThreadId,
    update: OptimisticThreadUpdate,
    now: UtcDateTime,
    /// Set once the server accepted the command at this event sequence.
    sequence: Option<u64>,
}

struct OverlayMemo {
    source: Arc<ShellSnapshot>,
    generation: u64,
    output: Arc<ShellSnapshot>,
}

/// The pending list and the overlay it draws over the shell snapshot
/// (`pendingAtom` + `snapshotAtom`).
#[derive(Default)]
pub struct OptimisticThreads {
    pending: Vec<PendingThreadUpdate>,
    /// Bumped whenever `pending` changes, to invalidate the memo.
    generation: u64,
    memo: Option<OverlayMemo>,
}

impl OptimisticThreads {
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    fn touch(&mut self) -> bool {
        self.generation += 1;
        true
    }

    pub fn begin(
        &mut self,
        id: OptimisticUpdateId,
        thread_id: ThreadId,
        update: OptimisticThreadUpdate,
        now: UtcDateTime,
    ) -> bool {
        self.pending.push(PendingThreadUpdate {
            id,
            thread_id,
            update,
            now,
            sequence: None,
        });
        self.touch()
    }

    /// The command succeeded at `sequence`. The preview stays until the shell
    /// catches up with it (`snapshot_sequence` is the shell's current one).
    pub fn confirm(
        &mut self,
        id: OptimisticUpdateId,
        sequence: u64,
        snapshot_sequence: Option<u64>,
    ) -> bool {
        let Some(entry) = self.pending.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.sequence = Some(sequence);
        self.touch();
        self.reconcile(snapshot_sequence);
        true
    }

    /// The command failed (or was abandoned): drop its preview.
    pub fn discard(&mut self, id: OptimisticUpdateId) -> bool {
        let before = self.pending.len();
        self.pending.retain(|entry| entry.id != id);
        self.pending.len() != before && self.touch()
    }

    /// Drops accepted updates the shell has caught up with (or every accepted
    /// update once the shell is gone).
    pub fn reconcile(&mut self, snapshot_sequence: Option<u64>) -> bool {
        let before = self.pending.len();
        self.pending
            .retain(|entry| match (entry.sequence, snapshot_sequence) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(sequence), Some(snapshot)) => snapshot < sequence,
            });
        self.pending.len() != before && self.touch()
    }

    /// The visible snapshot: `snapshot` with every pending update applied in
    /// order. Rows no update touched keep their `Arc`; with nothing to apply
    /// the source snapshot itself is returned.
    pub fn overlay(&mut self, snapshot: &Arc<ShellSnapshot>) -> Arc<ShellSnapshot> {
        if self.pending.is_empty() {
            self.memo = None;
            return Arc::clone(snapshot);
        }
        if let Some(memo) = &self.memo
            && memo.generation == self.generation
            && Arc::ptr_eq(&memo.source, snapshot)
        {
            return Arc::clone(&memo.output);
        }
        let output = self.compute_overlay(snapshot);
        self.memo = Some(OverlayMemo {
            source: Arc::clone(snapshot),
            generation: self.generation,
            output: Arc::clone(&output),
        });
        output
    }

    fn compute_overlay(&self, snapshot: &Arc<ShellSnapshot>) -> Arc<ShellSnapshot> {
        let live: Vec<&PendingThreadUpdate> = self
            .pending
            .iter()
            .filter(|entry| {
                entry
                    .sequence
                    .is_none_or(|sequence| sequence > snapshot.snapshot_sequence)
            })
            .collect();
        if live.is_empty() {
            return Arc::clone(snapshot);
        }
        let mut changed = false;
        let threads: Vec<_> = snapshot
            .threads
            .iter()
            .map(|thread| {
                let mut current = Arc::clone(thread);
                for entry in live.iter().filter(|entry| entry.thread_id == thread.id) {
                    if let Some(next) =
                        entry
                            .update
                            .apply(&current, &entry.now, entry.sequence.is_some())
                    {
                        current = Arc::new(next);
                        changed = true;
                    }
                }
                current
            })
            .collect();
        if !changed {
            return Arc::clone(snapshot);
        }
        Arc::new(ShellSnapshot {
            threads: Arc::new(threads),
            ..(**snapshot).clone()
        })
    }
}

/// Sends `OptimisticDiscard` unless the command was confirmed, so a dropped
/// (cancelled) command never leaves its preview behind (upstream's `finally`).
struct DiscardGuard<'a> {
    sender: &'a ClientStateSender,
    id: OptimisticUpdateId,
    armed: bool,
}

impl Drop for DiscardGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.sender
                .send(ClientStateEvent::OptimisticDiscard { id: self.id });
        }
    }
}

/// `optimistic.wrap`: shows `update` on `thread_id` while `command` runs,
/// keeps it after success until the shell reaches the returned sequence, and
/// rolls it back on failure.
pub async fn with_optimistic<F>(
    sender: &ClientStateSender,
    thread_id: ThreadId,
    update: OptimisticThreadUpdate,
    command: F,
) -> Result<DispatchResult, CommandError>
where
    F: Future<Output = Result<DispatchResult, CommandError>>,
{
    let id = OptimisticUpdateId::next();
    sender.send(ClientStateEvent::OptimisticBegin {
        id,
        thread_id,
        update,
        now: UtcDateTime::now(),
    });
    let mut guard = DiscardGuard {
        sender,
        id,
        armed: true,
    };
    let result = command.await;
    if let Ok(dispatched) = &result {
        guard.armed = false;
        sender.send(ClientStateEvent::OptimisticConfirm {
            id,
            sequence: dispatched.sequence,
        });
    }
    result
}

/// The lifecycle commands with their optimistic previews
/// (`createThreadEnvironmentAtoms`' wrapped `settle` … `reorderActive`).
#[derive(Clone)]
pub struct ThreadLifecycleCommands {
    commands: EnvironmentCommands,
    sender: ClientStateSender,
}

impl ThreadLifecycleCommands {
    pub fn new(commands: EnvironmentCommands, sender: ClientStateSender) -> Self {
        Self { commands, sender }
    }

    pub async fn settle(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        with_optimistic(
            &self.sender,
            thread_id.clone(),
            OptimisticThreadUpdate::Settle,
            self.commands.settle_thread(meta, thread_id),
        )
        .await
    }

    pub async fn unsettle(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        with_optimistic(
            &self.sender,
            thread_id.clone(),
            OptimisticThreadUpdate::Unsettle { user: true },
            self.commands.unsettle_thread(meta, thread_id),
        )
        .await
    }

    pub async fn snooze(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        snoozed_until: String,
    ) -> Result<DispatchResult, CommandError> {
        with_optimistic(
            &self.sender,
            thread_id.clone(),
            OptimisticThreadUpdate::Snooze {
                snoozed_until: snoozed_until.clone(),
            },
            self.commands.snooze_thread(meta, thread_id, snoozed_until),
        )
        .await
    }

    pub async fn unsnooze(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        with_optimistic(
            &self.sender,
            thread_id.clone(),
            OptimisticThreadUpdate::Unsnooze,
            self.commands.unsnooze_thread(meta, thread_id),
        )
        .await
    }

    pub async fn set_auto_settle(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        enabled: bool,
    ) -> Result<DispatchResult, CommandError> {
        with_optimistic(
            &self.sender,
            thread_id.clone(),
            OptimisticThreadUpdate::SetAutoSettle { enabled },
            self.commands
                .set_thread_auto_settle(meta, thread_id, enabled),
        )
        .await
    }

    pub async fn pin(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        order_key: Option<String>,
    ) -> Result<DispatchResult, CommandError> {
        let preview_key = order_key
            .as_deref()
            .and_then(|key| TrimmedNonEmptyString::new(key).ok());
        with_optimistic(
            &self.sender,
            thread_id.clone(),
            OptimisticThreadUpdate::Pin {
                order_key: preview_key,
            },
            self.commands.pin_thread(meta, thread_id, order_key),
        )
        .await
    }

    pub async fn unpin(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        with_optimistic(
            &self.sender,
            thread_id.clone(),
            OptimisticThreadUpdate::Unpin,
            self.commands.unpin_thread(meta, thread_id),
        )
        .await
    }

    pub async fn reorder_pin(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        order_key: &str,
    ) -> Result<DispatchResult, CommandError> {
        let command = self
            .commands
            .reorder_pinned_thread(meta, thread_id.clone(), order_key);
        match TrimmedNonEmptyString::new(order_key) {
            Ok(order_key) => {
                with_optimistic(
                    &self.sender,
                    thread_id,
                    OptimisticThreadUpdate::ReorderPin { order_key },
                    command,
                )
                .await
            }
            // The command rejects the key itself; there is nothing to preview.
            Err(_) => command.await,
        }
    }

    pub async fn reorder_active(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        order_key: &str,
    ) -> Result<DispatchResult, CommandError> {
        let command = self
            .commands
            .reorder_active_thread(meta, thread_id.clone(), order_key);
        match TrimmedNonEmptyString::new(order_key) {
            Ok(order_key) => {
                with_optimistic(
                    &self.sender,
                    thread_id,
                    OptimisticThreadUpdate::ReorderActive { order_key },
                    command,
                )
                .await
            }
            Err(_) => command.await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::{OrchestrationV2PendingRuntimeRequestSummary, PositiveInt};
    use crate::state::test_fixtures::v2_thread_shell;
    use serde_json::json;

    const NOW: &str = "2026-09-12T10:00:00.000Z";
    const FUTURE: &str = "2099-01-01T00:00:00.000Z";

    fn at(value: &str) -> UtcDateTime {
        value.parse().unwrap()
    }

    fn key(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn request(kind: &str) -> OrchestrationV2PendingRuntimeRequestSummary {
        serde_json::from_value(json!({ "id": kind, "kind": kind, "createdAt": NOW })).unwrap()
    }

    fn thread() -> OrchestrationV2ThreadShell {
        OrchestrationV2ThreadShell {
            title: "Remote thread".into(),
            pending_runtime_request: None,
            ..v2_thread_shell()
        }
    }

    fn snapshot(sequence: u64, threads: Vec<OrchestrationV2ThreadShell>) -> Arc<ShellSnapshot> {
        Arc::new(ShellSnapshot {
            schema_version: PositiveInt::new(2).unwrap(),
            snapshot_sequence: sequence,
            threads: Arc::new(threads.into_iter().map(Arc::new).collect()),
            archived_threads: Arc::new(vec![]),
            projects: Arc::new(vec![]),
        })
    }

    struct Harness {
        optimistic: OptimisticThreads,
        source: Arc<ShellSnapshot>,
    }

    impl Harness {
        fn new(source: Arc<ShellSnapshot>) -> Self {
            Self {
                optimistic: OptimisticThreads::default(),
                source,
            }
        }

        fn run(&mut self, update: OptimisticThreadUpdate) -> OptimisticUpdateId {
            let id = OptimisticUpdateId::next();
            self.optimistic
                .begin(id, thread().id, update, UtcDateTime::now());
            id
        }

        fn succeed(&mut self, id: OptimisticUpdateId, sequence: u64) {
            self.optimistic
                .confirm(id, sequence, Some(self.source.snapshot_sequence));
        }

        fn fail(&mut self, id: OptimisticUpdateId) {
            self.optimistic.discard(id);
        }

        fn set_source(&mut self, source: Arc<ShellSnapshot>) {
            self.optimistic.reconcile(Some(source.snapshot_sequence));
            self.source = source;
        }

        fn visible(&mut self) -> Arc<ShellSnapshot> {
            self.optimistic.overlay(&self.source)
        }

        fn visible_thread(&mut self) -> Arc<OrchestrationV2ThreadShell> {
            Arc::clone(&self.visible().threads[0])
        }
    }

    type Expectation = fn(&OrchestrationV2ThreadShell) -> bool;

    #[test]
    fn shows_each_action_before_a_delayed_reply_and_rolls_back_a_rejection() {
        let cases: Vec<(&str, OptimisticThreadUpdate, Expectation)> = vec![
            ("settle", OptimisticThreadUpdate::Settle, |t| {
                t.settled_override == Some(OrchestrationV2SettleOverride::Settled)
                    && t.pinned_at == Some(None)
                    && t.snoozed_until == Some(None)
            }),
            (
                "unsettle",
                OptimisticThreadUpdate::Unsettle { user: true },
                |t| {
                    t.settled_override == Some(OrchestrationV2SettleOverride::Active)
                        && t.settled_at.is_none()
                },
            ),
            (
                "snooze",
                OptimisticThreadUpdate::Snooze {
                    snoozed_until: FUTURE.into(),
                },
                |t| t.snoozed_until == Some(Some(at(FUTURE))),
            ),
            ("unsnooze", OptimisticThreadUpdate::Unsnooze, |t| {
                t.snoozed_until == Some(None) && t.snoozed_at == Some(None)
            }),
            (
                "pin",
                OptimisticThreadUpdate::Pin {
                    order_key: Some(key("a")),
                },
                |t| t.pinned_at.flatten().is_some() && t.pin_order_key == Some(Some(key("a"))),
            ),
            ("unpin", OptimisticThreadUpdate::Unpin, |t| {
                t.pinned_at == Some(None) && t.pin_order_key == Some(None)
            }),
            (
                "setAutoSettle",
                OptimisticThreadUpdate::SetAutoSettle { enabled: false },
                |t| t.auto_settle_disabled_at.flatten().is_some(),
            ),
            (
                "reorderPin",
                OptimisticThreadUpdate::ReorderPin {
                    order_key: key("b"),
                },
                |t| t.pin_order_key == Some(Some(key("b"))),
            ),
            (
                "reorderActive",
                OptimisticThreadUpdate::ReorderActive {
                    order_key: key("b"),
                },
                |t| t.active_order_key == Some(Some(key("b"))),
            ),
        ];
        for (action, update, expected) in cases {
            let mut initial = thread();
            if matches!(action, "unsettle" | "pin") {
                initial.settled_override = Some(OrchestrationV2SettleOverride::Settled);
                initial.settled_at = Some(at(NOW));
            }
            if matches!(action, "unsnooze" | "settle" | "pin") {
                initial.snoozed_until = Some(Some(at(FUTURE)));
                initial.snoozed_at = Some(Some(at(NOW)));
            }
            if matches!(action, "unpin" | "settle") {
                initial.pinned_at = Some(Some(at(NOW)));
                initial.pin_order_key = Some(Some(key("a")));
            }
            let source = snapshot(1, vec![initial]);
            let mut h = Harness::new(Arc::clone(&source));
            let id = h.run(update);
            assert!(expected(&h.visible_thread()), "{action}");
            h.fail(id);
            assert!(Arc::ptr_eq(&h.visible(), &source), "{action}");
        }
    }

    #[test]
    fn keeps_the_preview_after_acknowledgement_until_the_matching_shell_update_arrives() {
        let mut h = Harness::new(snapshot(1, vec![thread()]));
        let id = h.run(OptimisticThreadUpdate::Settle);
        h.succeed(id, 3);
        h.set_source(snapshot(
            2,
            vec![OrchestrationV2ThreadShell {
                title: "Renamed remotely".into(),
                ..thread()
            }],
        ));
        let visible = h.visible_thread();
        assert_eq!(visible.title, "Renamed remotely");
        assert_eq!(
            visible.settled_override,
            Some(OrchestrationV2SettleOverride::Settled)
        );
        let confirmed = snapshot(
            3,
            vec![OrchestrationV2ThreadShell {
                title: "Renamed remotely".into(),
                settled_override: Some(OrchestrationV2SettleOverride::Settled),
                settled_at: Some(at("2026-09-12T12:00:00.000Z")),
                ..thread()
            }],
        );
        h.set_source(Arc::clone(&confirmed));
        assert!(Arc::ptr_eq(&h.visible(), &confirmed));
        h.set_source(snapshot(4, vec![thread()]));
        assert_eq!(h.visible_thread().settled_override, None);
    }

    #[test]
    fn shows_a_queued_reverse_action_immediately_and_preserves_it_if_the_earlier_action_fails() {
        let mut h = Harness::new(snapshot(1, vec![thread()]));
        let settle = h.run(OptimisticThreadUpdate::Settle);
        let unsettle = h.run(OptimisticThreadUpdate::Unsettle { user: true });
        assert_eq!(
            h.visible_thread().settled_override,
            Some(OrchestrationV2SettleOverride::Active)
        );
        h.fail(settle);
        assert_eq!(
            h.visible_thread().settled_override,
            Some(OrchestrationV2SettleOverride::Active)
        );
        let confirmed = snapshot(
            2,
            vec![OrchestrationV2ThreadShell {
                settled_override: Some(OrchestrationV2SettleOverride::Active),
                ..thread()
            }],
        );
        h.set_source(Arc::clone(&confirmed));
        h.succeed(unsettle, 2);
        assert!(Arc::ptr_eq(&h.visible(), &confirmed));
        assert!(h.optimistic.is_empty());
    }

    #[test]
    fn does_not_restore_a_remotely_removed_thread() {
        let mut h = Harness::new(snapshot(1, vec![thread()]));
        let id = h.run(OptimisticThreadUpdate::Settle);
        let removed = snapshot(2, vec![]);
        h.set_source(Arc::clone(&removed));
        assert!(h.visible().threads.is_empty());
        h.fail(id);
        assert!(Arc::ptr_eq(&h.visible(), &removed));
    }

    #[test]
    fn keeps_pending_approvals_visible_while_a_lifecycle_request_is_pending() {
        let blocked = snapshot(
            1,
            vec![OrchestrationV2ThreadShell {
                pending_runtime_request: Some(request("command")),
                ..thread()
            }],
        );
        let mut h = Harness::new(Arc::clone(&blocked));
        h.run(OptimisticThreadUpdate::Settle);
        assert!(Arc::ptr_eq(&h.visible_thread(), &blocked.threads[0]));
    }

    fn undoable() -> [(OptimisticThreadUpdate, OptimisticThreadUpdate); 2] {
        [
            (
                OptimisticThreadUpdate::Settle,
                OptimisticThreadUpdate::Unsettle { user: true },
            ),
            (
                OptimisticThreadUpdate::Snooze {
                    snoozed_until: FUTURE.into(),
                },
                OptimisticThreadUpdate::Unsnooze,
            ),
        ]
    }

    fn is_awake(update: &OptimisticThreadUpdate, thread: &OrchestrationV2ThreadShell) -> bool {
        match update {
            OptimisticThreadUpdate::Settle => {
                thread.settled_override == Some(OrchestrationV2SettleOverride::Active)
            }
            _ => thread.snoozed_until == Some(None),
        }
    }

    fn parked(update: &OptimisticThreadUpdate) -> OrchestrationV2ThreadShell {
        match update {
            OptimisticThreadUpdate::Settle => OrchestrationV2ThreadShell {
                settled_override: Some(OrchestrationV2SettleOverride::Settled),
                ..thread()
            },
            _ => OrchestrationV2ThreadShell {
                snoozed_until: Some(Some(at(FUTURE))),
                ..thread()
            },
        }
    }

    #[test]
    fn restores_a_confirmed_action_when_a_queued_undo_fails() {
        for (action, undo_action) in undoable() {
            let mut h = Harness::new(snapshot(1, vec![thread()]));
            let first = h.run(action.clone());
            let undo = h.run(undo_action);
            assert!(is_awake(&action, &h.visible_thread()));
            h.succeed(first, 2);
            assert!(is_awake(&action, &h.visible_thread()));
            let confirmed = snapshot(2, vec![parked(&action)]);
            h.set_source(Arc::clone(&confirmed));
            assert!(is_awake(&action, &h.visible_thread()));
            h.fail(undo);
            assert!(Arc::ptr_eq(&h.visible(), &confirmed));
        }
    }

    #[test]
    fn preserves_a_newer_approval_when_the_reply_arrives_after_the_shell() {
        for (action, _) in undoable() {
            let mut h = Harness::new(snapshot(1, vec![thread()]));
            let id = h.run(action);
            let newer = snapshot(
                3,
                vec![OrchestrationV2ThreadShell {
                    pending_runtime_request: Some(request("command")),
                    ..thread()
                }],
            );
            h.set_source(Arc::clone(&newer));
            assert!(Arc::ptr_eq(&h.visible_thread(), &newer.threads[0]));
            h.succeed(id, 2);
            assert!(Arc::ptr_eq(&h.visible(), &newer));
        }
    }

    #[test]
    fn shows_an_accepted_action_while_the_shell_still_has_an_old_input_request() {
        for (action, _) in undoable() {
            let stale = snapshot(
                1,
                vec![OrchestrationV2ThreadShell {
                    pending_runtime_request: Some(request("user_input")),
                    ..thread()
                }],
            );
            let mut h = Harness::new(Arc::clone(&stale));
            let id = h.run(action.clone());
            assert!(Arc::ptr_eq(&h.visible_thread(), &stale.threads[0]));
            h.succeed(id, 2);
            let visible = h.visible_thread();
            match action {
                OptimisticThreadUpdate::Settle => assert_eq!(
                    visible.settled_override,
                    Some(OrchestrationV2SettleOverride::Settled)
                ),
                _ => assert_eq!(visible.snoozed_until, Some(Some(at(FUTURE)))),
            }
            assert!(visible.pending_runtime_request.is_none());
            assert!(h.source.threads[0].pending_runtime_request.is_some());
        }
    }

    #[test]
    fn a_snooze_in_the_past_previews_nothing() {
        let source = snapshot(1, vec![thread()]);
        let mut h = Harness::new(Arc::clone(&source));
        h.run(OptimisticThreadUpdate::Snooze {
            snoozed_until: "2000-01-01T00:00:00.000Z".into(),
        });
        assert!(Arc::ptr_eq(&h.visible(), &source));
    }

    #[test]
    fn the_overlay_is_memoized_until_the_source_or_pending_list_changes() {
        let mut h = Harness::new(snapshot(1, vec![thread()]));
        h.run(OptimisticThreadUpdate::Unpin);
        let first = h.visible();
        assert!(Arc::ptr_eq(&first, &h.visible()));
        h.run(OptimisticThreadUpdate::Unsnooze);
        assert!(!Arc::ptr_eq(&first, &h.visible()));
    }

    #[test]
    fn re_snoozing_to_the_same_time_keeps_the_original_snoozed_at() {
        let original = OrchestrationV2ThreadShell {
            snoozed_until: Some(Some(at(FUTURE))),
            snoozed_at: Some(Some(at(NOW))),
            ..thread()
        };
        let next = OptimisticThreadUpdate::Snooze {
            snoozed_until: FUTURE.into(),
        }
        .apply(&original, &UtcDateTime::now(), false);
        // Same until and same snoozedAt: nothing visible changes.
        assert!(next.is_none());
    }

    #[tokio::test]
    async fn with_optimistic_confirms_on_success_and_discards_on_failure() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = ClientStateSender { tx };
        let ok = with_optimistic(&sender, thread().id, OptimisticThreadUpdate::Unpin, async {
            Ok(DispatchResult { sequence: 7 })
        })
        .await;
        assert_eq!(ok, Ok(DispatchResult { sequence: 7 }));
        let Some(ClientStateEvent::OptimisticBegin { id, .. }) = rx.recv().await else {
            panic!("begin first");
        };
        assert_eq!(
            rx.recv().await,
            Some(ClientStateEvent::OptimisticConfirm { id, sequence: 7 })
        );
        let err = with_optimistic(&sender, thread().id, OptimisticThreadUpdate::Unpin, async {
            Err(CommandError::Rejected("no".into()))
        })
        .await;
        assert!(err.is_err());
        let Some(ClientStateEvent::OptimisticBegin { id, .. }) = rx.recv().await else {
            panic!("begin first");
        };
        assert_eq!(
            rx.recv().await,
            Some(ClientStateEvent::OptimisticDiscard { id })
        );
    }
}
