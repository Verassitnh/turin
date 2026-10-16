//! Tests for `thread_undo.rs`: upstream `threadUndo.test.ts` (all cases) and the
//! queue cases of `showThreadUndoNotice.test.ts`. The 5 s timer cases become
//! explicit `expire` calls, since the timer is the caller's.

use turin_contracts::environment::ScopedThreadRef;

use super::{ThreadUndo, ThreadUndoNotices, UndoAction, UndoPlan};

fn thread(id: &str) -> ScopedThreadRef {
    ScopedThreadRef {
        environment_id: turin_contracts::EnvironmentId::new("env").unwrap(),
        thread_id: turin_contracts::ThreadId::new(id).unwrap(),
    }
}

fn unpin_plan(id: &str) -> UndoPlan {
    UndoPlan::Pin {
        thread: thread(id),
        order_key: None,
    }
}

#[test]
fn keeps_claims_for_different_action_kinds_independent() {
    let undo = ThreadUndo::default();
    let pin = undo.begin("pin", "env/shared");
    let archive = undo.begin("archive", "env/shared");
    undo.invalidate("pin", "env/shared");
    assert!(!pin.is_current());
    assert!(archive.is_current());
    archive.finish();
}

#[test]
fn does_not_revive_the_first_undo_after_a_later_pin_and_unpin() {
    let undo = ThreadUndo::default();
    let first_unpin = undo.begin("pin", "env/thread");
    undo.invalidate("pin", "env/thread");
    let second_unpin = undo.begin("pin", "env/thread");
    assert!(!first_unpin.is_current());
    assert!(second_unpin.is_current());
    first_unpin.finish();
    assert!(second_unpin.is_current());
    second_unpin.finish();
    assert!(!first_unpin.is_current());
}

#[test]
fn rejects_a_late_unpin_completion_after_a_newer_pin_started() {
    let undo = ThreadUndo::default();
    let pending_unpin = undo.begin("pin", "env/late");
    undo.invalidate("pin", "env/late");
    assert!(!pending_unpin.is_current());
}

#[test]
fn expires_an_undo_without_invalidating_another_environment_or_thread() {
    let undo = ThreadUndo::default();
    let first = undo.begin("pin", "one/thread");
    let other_environment = undo.begin("pin", "two/thread");
    let other_thread = undo.begin("pin", "one/other");
    first.finish();
    assert!(!first.is_current());
    assert!(other_environment.is_current());
    assert!(other_thread.is_current());
    other_environment.finish();
    other_thread.finish();
}

#[test]
fn a_later_claim_of_the_same_kind_expires_the_earlier_one() {
    let undo = ThreadUndo::default();
    let earlier = undo.begin("archive", "env/a");
    let later = undo.begin("archive", "env/a");
    assert!(!earlier.is_current());
    assert!(later.is_current());
}

// --- showThreadUndoNotice queue ---------------------------------------------

#[test]
fn aggregates_consecutive_actions_and_restores_the_group_once() {
    let undo = ThreadUndo::default();
    let notices = ThreadUndoNotices::default();
    notices.show(
        UndoAction::Settled,
        undo.begin("settle", "env/a"),
        unpin_plan("a"),
        "Restore failed",
    );
    notices.show(
        UndoAction::Settled,
        undo.begin("settle", "env/b"),
        unpin_plan("b"),
        "Restore failed",
    );
    let group = notices.current_notice().unwrap();
    assert_eq!(group.action, UndoAction::Settled);
    assert_eq!(group.count, 2);

    assert_eq!(notices.take_undo(&group.ids).len(), 2);
    assert!(notices.take_undo(&group.ids).is_empty());
    assert!(notices.current_notice().is_none());
}

#[test]
fn drops_invalidated_claims_immediately_and_rejects_a_captured_stale_undo() {
    let undo = ThreadUndo::default();
    let notices = ThreadUndoNotices::default();
    notices.show(
        UndoAction::Unpinned,
        undo.begin("pin", "env/thread"),
        unpin_plan("thread"),
        "Restore failed",
    );
    let stale = notices.current_notice().unwrap();
    undo.invalidate("pin", "env/thread");
    assert!(notices.current_notice().is_none());

    notices.show(
        UndoAction::Unpinned,
        undo.begin("pin", "env/thread"),
        unpin_plan("thread"),
        "Restore failed",
    );
    assert!(notices.take_undo(&stale.ids).is_empty());
    let fresh = notices.current_notice().unwrap();
    assert_eq!(notices.take_undo(&fresh.ids).len(), 1);
}

#[test]
fn does_not_show_a_notice_for_a_late_completion_after_a_newer_action() {
    let undo = ThreadUndo::default();
    let notices = ThreadUndoNotices::default();
    let claim = undo.begin("pin", "env/thread");
    undo.invalidate("pin", "env/thread");
    notices.show(
        UndoAction::Unpinned,
        claim,
        unpin_plan("thread"),
        "Restore failed",
    );
    assert!(notices.current_notice().is_none());
}

#[test]
fn expiry_finishes_every_claim_and_empties_the_queue() {
    let undo = ThreadUndo::default();
    let notices = ThreadUndoNotices::default();
    let first = undo.begin("pin", "env/thread");
    notices.show(
        UndoAction::Unpinned,
        first.clone(),
        unpin_plan("thread"),
        "Restore failed",
    );
    let second = undo.begin("pin", "env/second");
    notices.show(
        UndoAction::Unpinned,
        second.clone(),
        unpin_plan("second"),
        "Restore failed",
    );
    let group = notices.current_notice().unwrap();
    assert_eq!(group.count, 2);

    notices.expire();
    assert!(notices.current_notice().is_none());
    assert!(!first.is_current());
    assert!(!second.is_current());
    assert!(notices.take_undo(&group.ids).is_empty());
}

#[test]
fn undoes_the_latest_kind_first_then_reveals_the_preceding_group() {
    let undo = ThreadUndo::default();
    let notices = ThreadUndoNotices::default();
    notices.show(
        UndoAction::Settled,
        undo.begin("settle", "env/a"),
        unpin_plan("a"),
        "Restore failed",
    );
    notices.show(
        UndoAction::Snoozed,
        undo.begin("snooze", "env/b"),
        UndoPlan::Unsnooze {
            thread: thread("b"),
        },
        "Failed to wake thread",
    );
    let newest = notices.current_notice().unwrap();
    assert_eq!(newest.action, UndoAction::Snoozed);
    let runs = notices.take_undo(&newest.ids);
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].plan,
        UndoPlan::Unsnooze {
            thread: thread("b")
        }
    );

    let older = notices.current_notice().unwrap();
    assert_eq!(older.action, UndoAction::Settled);
    assert_eq!(notices.take_undo(&older.ids).len(), 1);
    assert!(notices.current_notice().is_none());
}

#[test]
fn take_undo_returns_a_same_kind_group_newest_first() {
    let undo = ThreadUndo::default();
    let notices = ThreadUndoNotices::default();
    notices.show(
        UndoAction::Archived,
        undo.begin("archive", "env/older"),
        unpin_plan("older"),
        "Restore failed",
    );
    notices.show(
        UndoAction::Archived,
        undo.begin("archive", "env/newer"),
        unpin_plan("newer"),
        "Restore failed",
    );
    let group = notices.current_notice().unwrap();
    assert_eq!(group.count, 2);
    let runs = notices.take_undo(&group.ids);
    assert_eq!(
        runs.into_iter().map(|run| run.plan).collect::<Vec<_>>(),
        vec![unpin_plan("newer"), unpin_plan("older")]
    );
}
