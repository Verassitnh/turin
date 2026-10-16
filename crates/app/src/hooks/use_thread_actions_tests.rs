//! Tests for `use_thread_actions.rs`, ported from upstream
//! `apps/web/src/hooks/useThreadActions.test.ts`,
//! `useThreadActions.undo.test.ts` and `useThreadActions.permissions.test.ts`.
//!
//! Upstream mocks the atoms and the stores. Here `RecordingCommands` and
//! `RecordingHost` share one `World`. The world logs each command as
//! `"<op> <env>/<thread> [detail]"` and each UI effect as a string, so the
//! assertions read as upstream's do.
//!
//! Upstream cases that only exercise the cut features are not ported. Those
//! are the device and terminal cases and the browser-tab reconcile, plus the
//! `files` explorer. `useThreadActions.permissions.test.ts`'s `sessionLookup`
//! cases are ported by `FakeCommands::session_grant_fails`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::{EnvironmentId, ProjectId, ThreadId};

use super::{
    AUTH_ORCHESTRATION_OPERATE_SCOPE, AUTH_SOURCE_CONTROL_WRITE_SCOPE, ActionResult,
    DispatchResult, ErrorToast, ServerCapability, ThreadActionError, ThreadActionHost,
    ThreadActions, ThreadCommands, navigate_after_thread_deletion,
    request_thread_unpin_confirmation,
};
use crate::components::sidebar::runtime::thread_sort::ThreadSortOrder;
use crate::hooks::thread_undo::{ThreadUndo, ThreadUndoClaim, ThreadUndoNotices, UndoAction, UndoPlan};
use crate::state::right_panel_store::thread_key;
use crate::state::command_result::CommandFailure;
use crate::state::models::{
    EnvironmentProject, EnvironmentThreadShell, RunStatus, ThreadRunSummary, ThreadRuntimeSummary,
};

/// Shared by every fake for one test: the grants, the command log, and the UI effects.
#[derive(Default)]
struct World {
    /// Environment id → granted scopes.
    grants: HashMap<String, HashSet<String>>,
    calls: Vec<String>,
    effects: Vec<String>,
    toasts: Vec<ErrorToast>,
    confirm_messages: Vec<String>,
}

type Shared = Rc<RefCell<World>>;

const NOW: &str = "2026-10-15T00:00:00.000Z";
const FUTURE: &str = "2099-01-01T00:00:00Z";

fn env(id: &str) -> EnvironmentId {
    EnvironmentId::new(id).unwrap()
}

fn thread_ref(environment: &str, thread: &str) -> ScopedThreadRef {
    ScopedThreadRef {
        environment_id: env(environment),
        thread_id: ThreadId::new(thread).unwrap(),
    }
}

/// The target every upstream case uses.
fn target() -> ScopedThreadRef {
    thread_ref("undo-env", "thread")
}

fn shell(environment: &str, thread: &str) -> EnvironmentThreadShell {
    let mut shell = EnvironmentThreadShell::new(
        env(environment),
        ThreadId::new(thread).unwrap(),
        ProjectId::new("project").unwrap(),
        "Thread",
        "2026-01-01T00:00:00.000Z",
    );
    shell.pin_order_key = Some("a0".into());
    shell
}

fn with_runtime(mut shell: EnvironmentThreadShell) -> EnvironmentThreadShell {
    shell.runtime = Some(ThreadRuntimeSummary {
        status: RunStatus::Idle,
        active_run_id: None,
        activity_started_at: None,
        provider_instance_id: "codex".into(),
        provider_name: None,
        last_error: None,
        last_error_class: None,
        updated_at: NOW.into(),
    });
    shell
}

fn project(environment: &str, workspace_root: &str) -> EnvironmentProject {
    EnvironmentProject {
        environment_id: env(environment),
        id: ProjectId::new("project").unwrap(),
        title: "Project".into(),
        workspace_root: workspace_root.into(),
        favicon_path: None,
        project_icon: None,
        created_at: None,
        updated_at: None,
        is_scratch: false,
        source: None,
    }
}

fn grant(world: &Shared, environment: &str, scopes: &[&str]) {
    world.borrow_mut().grants.insert(
        environment.to_owned(),
        scopes.iter().map(|scope| (*scope).to_owned()).collect(),
    );
}

fn revoke(world: &Shared, environment: &str, scope: &str) {
    if let Some(scopes) = world.borrow_mut().grants.get_mut(environment) {
        scopes.remove(scope);
    }
}

fn calls(world: &Shared) -> Vec<String> {
    world.borrow().calls.clone()
}

fn effects(world: &Shared) -> Vec<String> {
    world.borrow().effects.clone()
}

/// Dispatch behavior the individual tests vary.
#[derive(Default)]
struct FakeCommands {
    world: Shared,
    /// Operation name → message the dispatch fails with.
    fail: HashMap<&'static str, String>,
    /// Operation names whose dispatch is `CommandFailure::Interrupted`.
    interrupt: HashSet<&'static str>,
    /// `stopSession` revokes this scope (the permission-revoked-mid-delete case).
    revoke_on_stop: Option<&'static str>,
    /// The session lookup fails (`loadSessionState` is a failure).
    session_grant_fails: bool,
    /// `removeWorktree` fails with this message.
    remove_fails: Option<String>,
}

impl FakeCommands {
    fn record(&self, line: String) {
        self.world.borrow_mut().calls.push(line);
    }

    fn dispatch(&self, op: &'static str, line: String) -> DispatchResult {
        self.record(line);
        if self.interrupt.contains(op) {
            return Err(CommandFailure::Interrupted);
        }
        match self.fail.get(op) {
            Some(message) => Err(CommandFailure::Failed(message.clone())),
            None => Ok(()),
        }
    }
}

fn line(op: &str, thread: &ScopedThreadRef) -> String {
    format!("{op} {}/{}", thread.environment_id, thread.thread_id)
}

impl ThreadCommands for FakeCommands {
    fn archive(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("archive", line("archive", thread))
    }
    fn unarchive(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("unarchive", line("unarchive", thread))
    }
    fn delete(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("delete", line("delete", thread))
    }
    fn settle(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("settle", line("settle", thread))
    }
    fn unsettle(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("unsettle", line("unsettle", thread))
    }
    fn pin(&mut self, thread: &ScopedThreadRef, order_key: Option<&str>) -> DispatchResult {
        let detail = order_key.map(|key| format!(" {key}")).unwrap_or_default();
        self.dispatch("pin", format!("{}{detail}", line("pin", thread)))
    }
    fn unpin(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("unpin", line("unpin", thread))
    }
    fn set_auto_settle(&mut self, thread: &ScopedThreadRef, enabled: bool) -> DispatchResult {
        self.dispatch(
            "setAutoSettle",
            format!("{} {enabled}", line("setAutoSettle", thread)),
        )
    }
    fn reorder_pinned(&mut self, thread: &ScopedThreadRef, order_key: &str) -> DispatchResult {
        self.dispatch(
            "reorderPin",
            format!("{} {order_key}", line("reorderPin", thread)),
        )
    }
    fn reorder_active(&mut self, thread: &ScopedThreadRef, order_key: &str) -> DispatchResult {
        self.dispatch(
            "reorderActive",
            format!("{} {order_key}", line("reorderActive", thread)),
        )
    }
    fn snooze(&mut self, thread: &ScopedThreadRef, snoozed_until: &str) -> DispatchResult {
        self.dispatch(
            "snooze",
            format!("{} {snoozed_until}", line("snooze", thread)),
        )
    }
    fn unsnooze(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("unsnooze", line("unsnooze", thread))
    }
    fn mark_unread(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        self.dispatch("markUnread", line("markUnread", thread))
    }
    fn visit(&mut self, thread: &ScopedThreadRef, visited_at: &str) -> DispatchResult {
        self.dispatch("visit", format!("{} {visited_at}", line("visit", thread)))
    }
    fn stop_session(&mut self, thread: &ScopedThreadRef) -> DispatchResult {
        let result = self.dispatch("stopSession", line("stopSession", thread));
        if let Some(scope) = self.revoke_on_stop {
            revoke(&self.world, thread.environment_id.as_str(), scope);
        }
        result
    }
    fn session_grants_scope(
        &mut self,
        environment: &EnvironmentId,
        scope: &str,
    ) -> DispatchResult<bool> {
        if self.session_grant_fails {
            return Err(CommandFailure::Failed("Session lookup failed".into()));
        }
        Ok(self
            .world
            .borrow()
            .grants
            .get(environment.as_str())
            .is_some_and(|scopes| scopes.contains(scope)))
    }
    fn remove_worktree(
        &mut self,
        environment: &EnvironmentId,
        cwd: &str,
        path: &str,
        force: bool,
    ) -> DispatchResult {
        self.record(format!(
            "removeWorktree {environment} {cwd} {path} force={force}"
        ));
        match &self.remove_fails {
            Some(message) => Err(CommandFailure::Failed(message.clone())),
            None => Ok(()),
        }
    }
    fn refresh_vcs_status(&mut self, environment: &EnvironmentId, cwd: &str) -> DispatchResult {
        self.record(format!("refreshVcsStatus {environment} {cwd}"));
        Ok(())
    }
}

/// The store and UI the actions read and drive.
struct FakeHost {
    world: Shared,
    threads: Vec<EnvironmentThreadShell>,
    projects: Vec<EnvironmentProject>,
    unsupported: HashSet<ServerCapability>,
    route: Option<ScopedThreadRef>,
    local_api: bool,
    /// Answer to every `confirm`.
    confirm_answer: bool,
    /// The dialog fails.
    confirm_fails: bool,
    /// `confirm` revokes this scope on the target environment (the mid-dialog revoke).
    revoke_on_confirm: Option<&'static str>,
    confirm_thread_delete: bool,
    confirm_thread_unpin: bool,
    worktree_cleanup_on_delete: bool,
    navigation_fails: Option<String>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            world: Shared::default(),
            threads: Vec::new(),
            projects: Vec::new(),
            unsupported: HashSet::new(),
            route: None,
            local_api: true,
            confirm_answer: true,
            confirm_fails: false,
            revoke_on_confirm: None,
            confirm_thread_delete: true,
            confirm_thread_unpin: true,
            worktree_cleanup_on_delete: false,
            navigation_fails: None,
        }
    }
}

impl FakeHost {
    fn effect(&self, line: String) {
        self.world.borrow_mut().effects.push(line);
    }

    fn navigate(&self, line: String) -> DispatchResult {
        self.effect(line);
        match &self.navigation_fails {
            Some(message) => Err(CommandFailure::Failed(message.clone())),
            None => Ok(()),
        }
    }
}

impl ThreadActionHost for FakeHost {
    fn has_scope(&self, environment: &EnvironmentId, scope: &str) -> bool {
        self.world
            .borrow()
            .grants
            .get(environment.as_str())
            .is_some_and(|scopes| scopes.contains(scope))
    }
    fn thread(&self, thread: &ScopedThreadRef) -> Option<EnvironmentThreadShell> {
        self.threads
            .iter()
            .find(|shell| {
                shell.environment_id == thread.environment_id && shell.id == thread.thread_id
            })
            .cloned()
    }
    fn environment_threads(&self, environment: &EnvironmentId) -> Vec<EnvironmentThreadShell> {
        self.threads
            .iter()
            .filter(|shell| &shell.environment_id == environment)
            .cloned()
            .collect()
    }
    fn all_threads(&self) -> Vec<EnvironmentThreadShell> {
        self.threads.clone()
    }
    fn project(
        &self,
        environment: &EnvironmentId,
        project_id: &ProjectId,
    ) -> Option<EnvironmentProject> {
        self.projects
            .iter()
            .find(|project| &project.environment_id == environment && &project.id == project_id)
            .cloned()
    }
    fn supports(&self, _environment: &EnvironmentId, capability: ServerCapability) -> bool {
        !self.unsupported.contains(&capability)
    }
    fn scratch_workspace_root(&self, _environment: &EnvironmentId) -> Option<String> {
        None
    }
    fn worktree_cleanup_on_delete(
        &self,
        _environment: &EnvironmentId,
        _project_id: &ProjectId,
    ) -> bool {
        self.worktree_cleanup_on_delete
    }
    fn sort_order(&self) -> ThreadSortOrder {
        ThreadSortOrder::CreatedAt
    }
    fn confirm_thread_delete(&self) -> bool {
        self.confirm_thread_delete
    }
    fn confirm_thread_unpin(&self) -> bool {
        self.confirm_thread_unpin
    }
    fn now(&self) -> String {
        NOW.into()
    }
    fn current_route_thread(&self) -> Option<ScopedThreadRef> {
        self.route.clone()
    }
    fn local_api_available(&self) -> bool {
        self.local_api
    }
    fn confirm(&mut self, message: &str, _destructive: bool) -> DispatchResult<bool> {
        self.world
            .borrow_mut()
            .confirm_messages
            .push(message.to_owned());
        self.effect(format!(
            "confirm {}",
            message.lines().next().unwrap_or_default()
        ));
        if let Some(scope) = self.revoke_on_confirm {
            revoke(&self.world, self.threads[0].environment_id.as_str(), scope);
        }
        if self.confirm_fails {
            return Err(CommandFailure::Failed("dialog unavailable".into()));
        }
        Ok(self.confirm_answer)
    }
    fn navigate_to_thread(&mut self, thread: &ScopedThreadRef, replace: bool) -> DispatchResult {
        self.navigate(format!(
            "navigate {}/{} replace={replace}",
            thread.environment_id, thread.thread_id
        ))
    }
    fn navigate_to_root(&mut self, replace: bool) -> DispatchResult {
        self.navigate(format!("navigate / replace={replace}"))
    }
    fn open_draft_thread(
        &mut self,
        project: &turin_contracts::environment::ScopedProjectRef,
    ) -> DispatchResult {
        self.navigate(format!(
            "draft {}/{}",
            project.environment_id, project.project_id
        ))
    }
    fn mark_visited_locally(&mut self, thread_key: &str, visited_at: &str) {
        self.effect(format!("mark-visited {thread_key} {visited_at}"));
    }
    fn mark_unread_locally(&mut self, thread_key: &str, completed_at: Option<&str>) {
        self.effect(format!(
            "mark-unread {thread_key} {}",
            completed_at.unwrap_or("-")
        ));
    }
    fn refresh_archived_threads(&mut self, environment: &EnvironmentId) {
        self.effect(format!("refresh-archive {environment}"));
    }
    fn forget_thread_local_state(&mut self, thread: &ScopedThreadRef, _project_id: &ProjectId) {
        self.effect(format!("clear-thread-state {}", thread.thread_id));
    }
    fn clear_thread_preview_state(&mut self, thread: &ScopedThreadRef) {
        self.effect(format!("clear-preview {}", thread.thread_id));
    }
    fn show_error_toast(&mut self, toast: ErrorToast) {
        self.world.borrow_mut().toasts.push(toast);
    }
}

/// Two owners that share one undo claim table and notice queue, as the
/// sidebar, header and menus do in the app.
struct Harness {
    world: Shared,
    undo: ThreadUndo,
    notices: ThreadUndoNotices,
}

impl Harness {
    fn new() -> Self {
        Self {
            world: Shared::default(),
            undo: ThreadUndo::default(),
            notices: ThreadUndoNotices::default(),
        }
    }

    fn owner(&self, host: FakeHost) -> ThreadActions<FakeCommands, FakeHost> {
        ThreadActions::new(
            FakeCommands {
                world: self.world.clone(),
                ..FakeCommands::default()
            },
            FakeHost {
                world: self.world.clone(),
                ..host
            },
            self.undo.clone(),
            self.notices.clone(),
        )
    }

    /// A harness with one thread in `undo-env` and operate granted there.
    fn with_thread(
        thread: EnvironmentThreadShell,
    ) -> (Self, ThreadActions<FakeCommands, FakeHost>) {
        let harness = Self::new();
        grant(
            &harness.world,
            "undo-env",
            &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
        );
        let actions = harness.owner(FakeHost {
            threads: vec![thread],
            ..FakeHost::default()
        });
        (harness, actions)
    }
}

/// The undo group the notice shows now, as ids.
fn current_ids(notices: &ThreadUndoNotices) -> Vec<u64> {
    notices.current_notice().expect("an Undo notice").ids
}

// --- useThreadActions.test.ts -----------------------------------------------

#[test]
fn navigate_after_thread_deletion_reports_a_rejected_navigation_without_failing() {
    let toast =
        navigate_after_thread_deletion(|| Err(CommandFailure::Failed("route unavailable".into())));
    assert_eq!(
        toast,
        Some(ErrorToast {
            title: "Thread deleted, but navigation failed".into(),
            description: "route unavailable".into(),
        })
    );
}

#[test]
fn navigate_after_thread_deletion_does_not_report_after_successful_navigation() {
    assert_eq!(navigate_after_thread_deletion(|| Ok(())), None);
}

#[test]
fn archive_blocked_error_keeps_the_thread_and_its_fixed_message() {
    let error = ThreadActionError::ArchiveBlocked(target());
    assert_eq!(
        error.to_string(),
        "Cannot archive while the provider is active."
    );
    assert_eq!(error, ThreadActionError::ArchiveBlocked(target()));
}

#[test]
fn unpin_confirmation_skips_the_dialog_when_disabled() {
    let mut calls = 0;
    let result = request_thread_unpin_confirmation(
        false,
        "Pinned thread",
        Some(|_: &str| {
            calls += 1;
            Ok(false)
        }),
    );
    assert_eq!(result, Ok(true));
    assert_eq!(calls, 0);
}

#[test]
fn unpin_confirmation_degrades_gracefully_when_dialogs_are_unavailable() {
    let result = request_thread_unpin_confirmation(
        true,
        "Pinned thread",
        None::<fn(&str) -> DispatchResult<bool>>,
    );
    assert_eq!(result, Ok(true));
}

#[test]
fn unpin_confirmation_uses_the_thread_title_and_returns_the_decision() {
    let mut message = String::new();
    let result = request_thread_unpin_confirmation(
        true,
        "Release prep",
        Some(|next: &str| {
            message = next.to_owned();
            Ok(false)
        }),
    );
    assert_eq!(
        message,
        "Unpin thread \"Release prep\"?\nThis will move the thread out of your pinned section."
    );
    assert_eq!(result, Ok(false));
}

#[test]
fn unpin_confirmation_keeps_dialog_failures_observable() {
    let result = request_thread_unpin_confirmation(
        true,
        "Pinned thread",
        Some(|_: &str| Err(CommandFailure::Failed("dialog unavailable".into()))),
    );
    assert!(result.is_err());
}

// --- useThreadActions.undo.test.ts ------------------------------------------

#[test]
fn unpin_undo_ignores_an_old_notice_across_owners_and_restores_the_latest_unpin() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let host = || FakeHost {
        threads: vec![shell("undo-env", "thread")],
        ..FakeHost::default()
    };
    let mut sidebar = harness.owner(host());
    let mut header = harness.owner(host());

    sidebar.unpin_thread(&target()).unwrap();
    let stale = current_ids(&harness.notices);
    header.pin_thread(&target(), Some("a1")).unwrap();
    header.unpin_thread(&target()).unwrap();
    let latest = current_ids(&harness.notices);

    sidebar.undo_notice(&stale);
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|c| c.starts_with("pin "))
            .count(),
        1
    );

    sidebar.undo_notice(&latest);
    let pins: Vec<String> = calls(&harness.world)
        .into_iter()
        .filter(|c| c.starts_with("pin "))
        .collect();
    assert_eq!(pins.len(), 2);
    assert_eq!(pins.last().unwrap(), "pin undo-env/thread a0");

    sidebar.undo_notice(&latest);
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|c| c.starts_with("pin "))
            .count(),
        2
    );
}

#[test]
fn archive_undo_unarchives_and_returns_to_the_thread_when_archiving_left_it() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "thread")],
        route: Some(target()),
        ..FakeHost::default()
    });

    assert_eq!(actions.archive_thread(&target()), Ok(true));
    let notice = harness.notices.current_notice().unwrap();
    assert_eq!((notice.action, notice.count), (UndoAction::Archived, 1));
    assert!(harness.world.borrow().toasts.is_empty());

    actions.undo_notice(&notice.ids);
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|c| c.starts_with("unarchive"))
            .count(),
        1
    );
    assert!(effects(&harness.world).contains(&"navigate undo-env/thread replace=false".to_owned()));
}

#[test]
fn archive_undo_stays_put_when_the_archived_thread_was_not_open() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "thread")],
        ..FakeHost::default()
    });
    actions.archive_thread(&target()).unwrap();
    actions.undo_notice(&current_ids(&harness.notices));
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|c| c.starts_with("unarchive"))
            .count(),
        1
    );
    assert!(
        !effects(&harness.world)
            .iter()
            .any(|e| e.starts_with("navigate"))
    );
}

#[test]
fn archive_shows_no_undo_when_the_archive_failed() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut commands = FakeCommands {
        world: harness.world.clone(),
        ..FakeCommands::default()
    };
    commands.fail.insert("archive", "nope".into());
    let mut actions = ThreadActions::new(
        commands,
        FakeHost {
            world: harness.world.clone(),
            threads: vec![shell("undo-env", "thread")],
            ..FakeHost::default()
        },
        harness.undo.clone(),
        harness.notices.clone(),
    );
    assert!(actions.archive_thread(&target()).is_err());
    assert!(harness.notices.current_notice().is_none());
}

#[test]
fn settle_undo_un_settles_from_the_notice_and_expires_after_a_manual_unsettle() {
    let (harness, mut actions) = Harness::with_thread(shell("undo-env", "thread"));
    actions.settle_thread(&target()).unwrap();
    let notice = harness.notices.current_notice().unwrap();
    assert_eq!((notice.action, notice.count), (UndoAction::Settled, 1));
    assert!(harness.world.borrow().toasts.is_empty());

    actions.unsettle_thread(&target()).unwrap();
    actions.undo_notice(&notice.ids);
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|c| c.starts_with("unsettle "))
            .count(),
        1
    );
}

#[test]
fn settle_undo_re_pins_and_re_snoozes_a_thread_that_settling_had_cleared() {
    let mut thread = shell("undo-env", "thread");
    thread.pinned_at = Some("2026-01-01T00:00:00.000Z".into());
    thread.snoozed_until = Some("2030-01-01T09:00:00.000Z".into());
    let (harness, mut actions) = Harness::with_thread(thread);
    actions.settle_thread(&target()).unwrap();
    actions.undo_notice(&current_ids(&harness.notices));

    let log = calls(&harness.world);
    assert_eq!(log.iter().filter(|c| c.starts_with("unsettle ")).count(), 1);
    assert_eq!(
        log.iter()
            .filter(|c| c.starts_with("pin "))
            .collect::<Vec<_>>(),
        ["pin undo-env/thread a0"]
    );
    assert_eq!(
        log.iter()
            .filter(|c| c.starts_with("snooze "))
            .collect::<Vec<_>>(),
        ["snooze undo-env/thread 2030-01-01T09:00:00.000Z"]
    );
}

#[test]
fn settle_expires_an_older_unpin_undo() {
    let (harness, mut actions) = Harness::with_thread(shell("undo-env", "thread"));
    actions.unpin_thread(&target()).unwrap();
    let stale = current_ids(&harness.notices);
    actions.settle_thread(&target()).unwrap();
    actions.undo_notice(&stale);
    assert!(!calls(&harness.world).iter().any(|c| c.starts_with("pin ")));
}

/// Offer one archive Undo whose claim the test still holds, matching
/// `showThreadUndoNotice.test.ts`'s `setup` + `showThreadUndoNotice`.
fn offer_archive_restore(harness: &Harness) -> ThreadUndoClaim {
    let claim = harness.undo.begin("archive", &thread_key(&target()));
    harness.notices.show(
        UndoAction::Archived,
        claim.clone(),
        UndoPlan::Unarchive {
            thread: target(),
            navigate: false,
        },
        "Restore failed",
    );
    claim
}

fn actions_with_unarchive(
    harness: &Harness,
    fail: Option<&str>,
    interrupt: bool,
) -> ThreadActions<FakeCommands, FakeHost> {
    let mut commands = FakeCommands {
        world: harness.world.clone(),
        ..FakeCommands::default()
    };
    if let Some(message) = fail {
        commands.fail.insert("unarchive", message.to_owned());
    }
    if interrupt {
        commands.interrupt.insert("unarchive");
    }
    ThreadActions::new(
        commands,
        FakeHost {
            world: harness.world.clone(),
            threads: vec![shell("undo-env", "thread")],
            ..FakeHost::default()
        },
        harness.undo.clone(),
        harness.notices.clone(),
    )
}

fn granted_restore_harness() -> Harness {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    harness
}

/// `showThreadUndoNotice.test.ts`: "reports a failed restore and releases its claim".
#[test]
fn a_failed_restore_toasts_and_releases_its_claim() {
    let harness = granted_restore_harness();
    let claim = offer_archive_restore(&harness);
    let mut actions = actions_with_unarchive(&harness, Some("offline"), false);
    actions.undo_notice(&current_ids(&harness.notices));
    assert_eq!(
        harness.world.borrow().toasts,
        vec![ErrorToast {
            title: "Restore failed".into(),
            description: "offline".into(),
        }]
    );
    assert!(!claim.is_current());
}

/// `showThreadUndoNotice.test.ts`: "reports a rejected restore promise".
/// Commands return `Result`, so a thrown restore is a failed dispatch.
#[test]
fn a_rejected_restore_toasts_the_error_message() {
    let harness = granted_restore_harness();
    offer_archive_restore(&harness);
    let mut actions = actions_with_unarchive(&harness, Some("disconnected"), false);
    actions.undo_notice(&current_ids(&harness.notices));
    let toasts = harness.world.borrow().toasts.clone();
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].description, "disconnected");
}

/// Two offers of one kind dispatch newest first (`showThreadUndoNotice.ts`
/// builds `group` from the end, then `current.map`s in that order).
#[test]
fn an_undo_group_restores_the_newest_offer_first() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "older"), shell("undo-env", "newer")],
        ..FakeHost::default()
    });
    actions
        .archive_thread(&thread_ref("undo-env", "older"))
        .unwrap();
    actions
        .archive_thread(&thread_ref("undo-env", "newer"))
        .unwrap();
    let notice = harness.notices.current_notice().unwrap();
    assert_eq!((notice.action, notice.count), (UndoAction::Archived, 2));

    actions.undo_notice(&notice.ids);
    let unarchives: Vec<String> = calls(&harness.world)
        .into_iter()
        .filter(|call| call.starts_with("unarchive "))
        .collect();
    assert_eq!(
        unarchives,
        vec![
            "unarchive undo-env/newer".to_owned(),
            "unarchive undo-env/older".to_owned(),
        ]
    );
}

/// `showThreadUndoNotice.test.ts`: "does not report interrupted restores as errors".
#[test]
fn an_interrupted_restore_does_not_toast() {
    let harness = granted_restore_harness();
    offer_archive_restore(&harness);
    let mut actions = actions_with_unarchive(&harness, None, true);
    actions.undo_notice(&current_ids(&harness.notices));
    assert!(harness.world.borrow().toasts.is_empty());
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|call| call.starts_with("unarchive "))
            .count(),
        1
    );
}

#[test]
fn snooze_notice_wakes_the_thread_when_undone() {
    let (harness, mut actions) = Harness::with_thread(shell("undo-env", "thread"));
    actions.snooze_thread(&target(), FUTURE).unwrap();
    let notice = harness.notices.current_notice().unwrap();
    assert_eq!((notice.action, notice.count), (UndoAction::Snoozed, 1));
    assert!(harness.world.borrow().toasts.is_empty());

    actions.undo_notice(&notice.ids);
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|c| c.as_str() == "unsnooze undo-env/thread")
            .count(),
        1
    );
}

#[test]
fn undo_latest_restores_the_newest_kind_first_then_reveals_the_preceding_group() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "a"), shell("undo-env", "b")],
        ..FakeHost::default()
    });
    actions.settle_thread(&thread_ref("undo-env", "a")).unwrap();
    actions
        .snooze_thread(&thread_ref("undo-env", "b"), FUTURE)
        .unwrap();

    assert!(actions.undo_latest());
    assert_eq!(
        harness.notices.current_notice().unwrap().action,
        UndoAction::Settled
    );
    assert_eq!(
        calls(&harness.world)
            .iter()
            .filter(|c| c.starts_with("unsnooze "))
            .count(),
        1
    );
    assert!(
        !calls(&harness.world)
            .iter()
            .any(|c| c.starts_with("unsettle "))
    );

    assert!(actions.undo_latest());
    assert!(!actions.undo_latest());
}

// --- useThreadActions.permissions.test.ts -----------------------------------

type Op = fn(&mut ThreadActions<FakeCommands, FakeHost>, &ScopedThreadRef) -> ActionResult;

/// Every orchestration action the permissions suite covers.
fn operations() -> [(&'static str, Op); 9] {
    [
        ("archive", |a, t| a.archive_thread(t).map(|_| ())),
        ("unarchive", |a, t| a.unarchive_thread(t, false)),
        ("settle", |a, t| a.settle_thread(t)),
        ("unsettle", |a, t| a.unsettle_thread(t)),
        ("snooze", |a, t| a.snooze_thread(t, FUTURE)),
        ("unsnooze", |a, t| a.unsnooze_thread(t)),
        ("pin", |a, t| a.pin_thread(t, None)),
        ("unpin", |a, t| a.unpin_thread(t)),
        ("reorderPin", |a, t| a.reorder_pinned_thread(t, "b")),
    ]
}

/// The permissions suite's world: `primary` grants operate, `secondary` (the
/// target's environment) grants nothing yet.
fn permissions_world() -> (Harness, ScopedThreadRef) {
    let harness = Harness::new();
    grant(
        &harness.world,
        "primary",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    grant(&harness.world, "secondary", &[]);
    (harness, thread_ref("secondary", "thread"))
}

fn permissions_owner(harness: &Harness) -> ThreadActions<FakeCommands, FakeHost> {
    harness.owner(FakeHost {
        threads: vec![with_runtime(shell("secondary", "thread"))],
        ..FakeHost::default()
    })
}

#[test]
fn every_action_requires_the_target_environments_grant() {
    for (name, run) in operations() {
        let (harness, target) = permissions_world();
        let mut actions = permissions_owner(&harness);
        let result = run(&mut actions, &target);
        assert!(result.is_err(), "{name}");
        assert!(calls(&harness.world).is_empty(), "{name}");
        assert!(effects(&harness.world).is_empty(), "{name}");
    }
}

#[test]
fn every_action_works_after_the_target_gains_only_the_task_permission() {
    for (name, run) in operations() {
        let (harness, target) = permissions_world();
        let mut actions = permissions_owner(&harness);
        revoke(&harness.world, "primary", AUTH_ORCHESTRATION_OPERATE_SCOPE);
        grant(
            &harness.world,
            "secondary",
            &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
        );
        assert!(run(&mut actions, &target).is_ok(), "{name}");
        assert!(
            calls(&harness.world)
                .iter()
                .any(|c| c.contains("secondary/thread")),
            "{name}"
        );
    }
}

#[test]
fn confirmed_delete_and_unpin_block_a_forbidden_confirmation() {
    let (harness, target) = permissions_world();
    let mut actions = permissions_owner(&harness);
    assert!(actions.confirm_and_delete_thread(&target, &[]).is_err());
    assert!(actions.confirm_and_unpin_thread(&target).is_err());
    assert!(harness.world.borrow().confirm_messages.is_empty());
    assert!(calls(&harness.world).is_empty());
}

#[test]
fn confirmed_delete_rechecks_the_grant_after_the_dialog() {
    let (harness, target) = permissions_world();
    grant(
        &harness.world,
        "secondary",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = ThreadActions::new(
        FakeCommands {
            world: harness.world.clone(),
            ..FakeCommands::default()
        },
        FakeHost {
            world: harness.world.clone(),
            threads: vec![shell("secondary", "thread")],
            revoke_on_confirm: Some(AUTH_ORCHESTRATION_OPERATE_SCOPE),
            ..FakeHost::default()
        },
        harness.undo.clone(),
        harness.notices.clone(),
    );
    assert!(actions.confirm_and_delete_thread(&target, &[]).is_err());
    assert_eq!(harness.world.borrow().confirm_messages.len(), 1);
    assert!(calls(&harness.world).is_empty());
    assert!(
        effects(&harness.world)
            .iter()
            .all(|e| !e.starts_with("clear-"))
    );
}

#[test]
fn confirmed_unpin_rechecks_the_grant_after_the_dialog() {
    let (harness, target) = permissions_world();
    grant(
        &harness.world,
        "secondary",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = ThreadActions::new(
        FakeCommands {
            world: harness.world.clone(),
            ..FakeCommands::default()
        },
        FakeHost {
            world: harness.world.clone(),
            threads: vec![shell("secondary", "thread")],
            revoke_on_confirm: Some(AUTH_ORCHESTRATION_OPERATE_SCOPE),
            ..FakeHost::default()
        },
        harness.undo.clone(),
        harness.notices.clone(),
    );
    // The unpin dialog runs; the grant is gone when the command would dispatch.
    assert!(actions.confirm_and_unpin_thread(&target).is_err());
    assert!(calls(&harness.world).is_empty());
}

#[test]
fn delete_identifies_the_missing_task_scope() {
    let (harness, target) = permissions_world();
    let mut actions = permissions_owner(&harness);
    assert_eq!(
        actions.delete_thread(&target, &[]),
        Err(ThreadActionError::Unauthorized {
            message: "This connection cannot change threads.",
            required_scope: AUTH_ORCHESTRATION_OPERATE_SCOPE,
        })
    );
    assert!(calls(&harness.world).is_empty());
    assert!(effects(&harness.world).is_empty());
}

#[test]
fn deleting_an_archived_thread_needs_its_own_environments_grant() {
    let (harness, target) = permissions_world();
    let mut actions = harness.owner(FakeHost::default());
    assert!(actions.delete_thread(&target, &[]).is_err());
    assert!(calls(&harness.world).is_empty());

    grant(
        &harness.world,
        "secondary",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    assert!(actions.delete_thread(&target, &[]).is_ok());
    assert_eq!(calls(&harness.world), ["delete secondary/thread"]);
}

#[test]
fn delete_stops_before_local_cleanup_when_permission_is_revoked_during_session_stop() {
    let (harness, target) = permissions_world();
    grant(
        &harness.world,
        "secondary",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = ThreadActions::new(
        FakeCommands {
            world: harness.world.clone(),
            revoke_on_stop: Some(AUTH_ORCHESTRATION_OPERATE_SCOPE),
            ..FakeCommands::default()
        },
        FakeHost {
            world: harness.world.clone(),
            threads: vec![with_runtime(shell("secondary", "thread"))],
            ..FakeHost::default()
        },
        harness.undo.clone(),
        harness.notices.clone(),
    );
    assert!(actions.delete_thread(&target, &[]).is_err());
    assert_eq!(calls(&harness.world), ["stopSession secondary/thread"]);
    assert!(effects(&harness.world).is_empty());
}

#[test]
fn delete_keeps_the_worktree_without_source_control_permission_or_when_the_lookup_fails() {
    for session_grant_fails in [false, true] {
        let harness = Harness::new();
        grant(
            &harness.world,
            "secondary",
            &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
        );
        let mut thread = with_runtime(shell("secondary", "thread"));
        thread.worktree_path = Some("/worktrees/thread".into());
        let mut actions = ThreadActions::new(
            FakeCommands {
                world: harness.world.clone(),
                session_grant_fails,
                ..FakeCommands::default()
            },
            FakeHost {
                world: harness.world.clone(),
                threads: vec![thread],
                projects: vec![project("secondary", "/project")],
                ..FakeHost::default()
            },
            harness.undo.clone(),
            harness.notices.clone(),
        );
        assert!(actions.delete_thread(&target_secondary(), &[]).is_ok());
        assert!(harness.world.borrow().confirm_messages.is_empty());
        assert_eq!(
            calls(&harness.world),
            ["stopSession secondary/thread", "delete secondary/thread"]
        );
        assert!(effects(&harness.world).contains(&"clear-thread-state thread".to_owned()));
    }
}

#[test]
fn delete_does_not_request_worktree_removal_after_its_grant_is_revoked_during_delete() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "secondary",
        &[
            AUTH_ORCHESTRATION_OPERATE_SCOPE,
            AUTH_SOURCE_CONTROL_WRITE_SCOPE,
        ],
    );
    let mut thread = with_runtime(shell("secondary", "thread"));
    thread.worktree_path = Some("/worktrees/thread".into());
    let mut actions = ThreadActions::new(
        FakeCommands {
            world: harness.world.clone(),
            // The grant is revoked right after the session stop, before cleanup.
            revoke_on_stop: Some(AUTH_SOURCE_CONTROL_WRITE_SCOPE),
            ..FakeCommands::default()
        },
        FakeHost {
            world: harness.world.clone(),
            threads: vec![thread],
            projects: vec![project("secondary", "/project")],
            ..FakeHost::default()
        },
        harness.undo.clone(),
        harness.notices.clone(),
    );
    // The thread is already gone, so cleanup reports itself in a toast instead of failing the deletion.
    assert!(actions.delete_thread(&target_secondary(), &[]).is_ok());
    assert_eq!(
        calls(&harness.world),
        ["stopSession secondary/thread", "delete secondary/thread"]
    );
    assert!(
        harness
            .world
            .borrow()
            .toasts
            .iter()
            .any(|toast| toast.title == "Failed to delete worktree")
    );
    assert!(effects(&harness.world).contains(&"clear-thread-state thread".to_owned()));
}

fn target_secondary() -> ScopedThreadRef {
    thread_ref("secondary", "thread")
}

#[test]
fn worktree_removal_failure_toasts_without_failing_the_deletion() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "secondary",
        &[
            AUTH_ORCHESTRATION_OPERATE_SCOPE,
            AUTH_SOURCE_CONTROL_WRITE_SCOPE,
        ],
    );
    let mut thread = shell("secondary", "thread");
    thread.worktree_path = Some("/worktrees/thread".into());
    let mut actions = ThreadActions::new(
        FakeCommands {
            world: harness.world.clone(),
            remove_fails: Some("busy".into()),
            ..FakeCommands::default()
        },
        FakeHost {
            world: harness.world.clone(),
            threads: vec![thread],
            projects: vec![project("secondary", "/project")],
            ..FakeHost::default()
        },
        harness.undo.clone(),
        harness.notices.clone(),
    );
    assert!(actions.delete_thread(&target_secondary(), &[]).is_ok());
    let world = harness.world.borrow();
    assert_eq!(world.confirm_messages.len(), 1);
    assert_eq!(
        world.confirm_messages[0],
        "This thread is the only one linked to this worktree:\nthread\n\nDelete the worktree too?"
    );
    assert_eq!(
        world.toasts,
        [ErrorToast {
            title: "Failed to delete worktree".into(),
            description: "Could not remove thread. busy".into(),
        }]
    );
}

// --- Ported from the same suites, with the cut surfaces removed --------------

#[test]
fn unsupported_servers_reject_before_any_command() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut host = FakeHost {
        threads: vec![shell("undo-env", "thread")],
        ..FakeHost::default()
    };
    host.unsupported.insert(ServerCapability::Pinning);
    host.unsupported.insert(ServerCapability::Settlement);
    let mut actions = harness.owner(host);
    assert_eq!(
        actions.pin_thread(&target(), None),
        Err(ThreadActionError::PinningUnsupported(target()))
    );
    assert_eq!(
        actions.settle_thread(&target()),
        Err(ThreadActionError::SettlementUnsupported(target()))
    );
    assert!(calls(&harness.world).is_empty());
}

#[test]
fn pin_places_the_thread_at_the_top_of_the_run_unless_the_caller_gives_an_anchor() {
    let mut pinned = shell("undo-env", "other");
    pinned.pinned_at = Some("2026-01-01T00:00:00.000Z".into());
    pinned.pin_order_key = Some("bm".into());
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![pinned, shell("undo-env", "thread")],
        ..FakeHost::default()
    });
    actions.pin_thread(&target(), None).unwrap();
    actions.pin_thread(&target(), Some("c5")).unwrap();
    let pins: Vec<String> = calls(&harness.world)
        .into_iter()
        .filter(|c| c.starts_with("pin "))
        .collect();
    // A key below the top pinned key ("bm"); the caller's key wins outright.
    assert!(pins[0].starts_with("pin undo-env/thread "));
    assert!(!pins[0].ends_with(" c5"));
    assert_eq!(pins[1], "pin undo-env/thread c5");
}

#[test]
fn archive_of_an_unknown_thread_is_a_no_op() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost::default());
    assert_eq!(actions.archive_thread(&target()), Ok(false));
    assert!(calls(&harness.world).is_empty());
}

#[test]
fn mark_unread_uses_the_server_marker_only_when_visits_are_tracked() {
    let mut thread = shell("undo-env", "thread");
    thread.latest_run = Some(ThreadRunSummary {
        run_id: turin_contracts::RunId::new("run-1").unwrap(),
        status: RunStatus::Completed,
        requested_at: None,
        started_at: None,
        completed_at: Some("2026-10-14T10:00:00.000Z".into()),
    });
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut local = FakeHost {
        threads: vec![thread.clone()],
        ..FakeHost::default()
    };
    local.unsupported.insert(ServerCapability::VisitedTracking);
    let mut actions = harness.owner(local);
    actions.mark_thread_unread(&target());
    assert_eq!(
        effects(&harness.world),
        ["mark-unread undo-env:thread 2026-10-14T10:00:00.000Z"]
    );

    let mut tracked = harness.owner(FakeHost {
        threads: vec![thread],
        ..FakeHost::default()
    });
    tracked.acknowledge_thread_woke(&target(), NOW);
    assert!(
        calls(&harness.world)
            .contains(&"visit undo-env/thread 2026-10-15T00:00:00.000Z".to_owned())
    );
}

#[test]
fn snooze_is_blocked_while_an_approval_waits_on_the_user_and_rejects_before_dispatch() {
    let mut blocked = shell("undo-env", "thread");
    blocked.has_pending_approvals = true;
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![blocked],
        ..FakeHost::default()
    });
    assert_eq!(
        actions.snooze_thread(&target(), FUTURE),
        Err(ThreadActionError::SnoozeBlocked(target()))
    );
    assert!(calls(&harness.world).is_empty());
}

#[test]
fn unpin_confirmation_is_skipped_when_the_setting_is_off() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "thread")],
        confirm_thread_unpin: false,
        ..FakeHost::default()
    });
    assert!(actions.confirm_and_unpin_thread(&target()).is_ok());
    assert!(harness.world.borrow().confirm_messages.is_empty());
    assert_eq!(calls(&harness.world), ["unpin undo-env/thread"]);
}

#[test]
fn confirm_and_delete_asks_first_and_stops_when_the_user_declines() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "thread")],
        confirm_answer: false,
        ..FakeHost::default()
    });
    assert!(actions.confirm_and_delete_thread(&target(), &[]).is_ok());
    assert_eq!(
        harness.world.borrow().confirm_messages,
        [
            "Delete thread \"Thread\"?\nThis permanently clears conversation history for this thread."
        ]
    );
    assert!(calls(&harness.world).is_empty());
}

#[test]
fn delete_falls_back_to_the_top_remaining_thread_on_the_route() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "thread"), shell("undo-env", "other")],
        projects: vec![project("undo-env", "/project")],
        route: Some(target()),
        ..FakeHost::default()
    });
    assert!(actions.delete_thread(&target(), &[]).is_ok());
    assert!(effects(&harness.world).contains(&"navigate undo-env/other replace=true".to_owned()));
}

#[test]
fn delete_navigation_failure_is_reported_without_failing_the_delete() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "thread")],
        projects: vec![project("undo-env", "/project")],
        route: Some(target()),
        navigation_fails: Some("route unavailable".into()),
        ..FakeHost::default()
    });
    assert!(actions.delete_thread(&target(), &[]).is_ok());
    assert_eq!(
        harness.world.borrow().toasts,
        [ErrorToast {
            title: "Thread deleted, but navigation failed".into(),
            description: "route unavailable".into(),
        }]
    );
}

#[test]
fn a_delete_of_a_thread_that_is_also_being_removed_skips_it_as_a_fallback() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![
            shell("undo-env", "thread"),
            shell("undo-env", "sibling"),
            shell("undo-env", "survivor"),
        ],
        projects: vec![project("undo-env", "/project")],
        route: Some(target()),
        ..FakeHost::default()
    });
    let sibling = thread_ref("undo-env", "sibling");
    assert!(actions.delete_thread(&target(), &[sibling]).is_ok());
    assert!(
        effects(&harness.world).contains(&"navigate undo-env/survivor replace=true".to_owned())
    );
}

#[test]
fn an_empty_deleted_set_does_not_change_the_fallback() {
    let harness = Harness::new();
    grant(
        &harness.world,
        "undo-env",
        &[AUTH_ORCHESTRATION_OPERATE_SCOPE],
    );
    let mut actions = harness.owner(FakeHost {
        threads: vec![shell("undo-env", "thread"), shell("undo-env", "other")],
        projects: vec![project("undo-env", "/project")],
        route: Some(target()),
        ..FakeHost::default()
    });
    assert!(actions.delete_thread(&target(), &[]).is_ok());
    assert!(effects(&harness.world).contains(&"navigate undo-env/other replace=true".to_owned()));
}
