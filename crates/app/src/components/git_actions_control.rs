//! Port of upstream/apps/web/src/components/GitActionsControl.tsx
//! (+ `GitActionsControl.lifecycle`).
//!
//! The git action split button: a quick action (Commit / Push / Create PR /
//! Pull / Publish, resolved by `turin-review`'s `resolve_quick_action`) with a
//! chevron menu, the in-flight progress row, the inline success row, a
//! "Changes" row with the branch's diff totals (panel), and the dialogs behind
//! them: commit, the default-ref confirmation and the publish wizard. Pure
//! decisions live in [`lifecycle`] and in `turin_review::action_state`; this
//! module renders them and runs the effects through a [`backend::GitActionsBackend`].
//!
//! Not ported: the `presentation="menu"` touch variant (mobile), the
//! write-scope gates (no sessions in one process) and the provider choice in
//! Publish (GitHub is the one host Turin ships).

// A faithful port keeps upstream's whole API (PORTING rule 3); the hosts wire
// the rest as the composer, thread details and settings tasks land.
#![allow(dead_code)]

pub mod backend;
pub mod commit_dialog;
pub mod default_branch_dialog;
pub mod lifecycle;
pub mod live_source;
pub mod mock_source;
pub mod publish_dialog;
mod view;

use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity, Window};
use turin_contracts::git::{GitStackedAction, VcsStatusResult};
use turin_contracts::{EnvironmentId, ThreadId};
use turin_review::action_state::git_actions_control_logic::{
    GIT_ACTION_SUCCESS_VISIBLE_MS, GitQuickActionKind, build_menu_items,
    change_request_terminology, resolve_default_branch_action_dialog_copy, resolve_quick_action,
};

use self::backend::{GitActionTarget, GitThreadContext, SharedGitActionsBackend, VcsActionState};
use self::lifecycle::{
    ActionPresentation, BranchSync, GitActionPlan, PendingDefaultBranchAction, ResultToastCta,
    RunGitActionInput, continue_pending_action, live_draft_branch_sync, plan_git_action,
    present_action_result, resolve_branch_sync, sync_after_git_action,
};
use crate::timestamp_format::now_ms;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GitDisplayMode {
    #[default]
    Toolbar,
    /// The thread details panel's rows.
    Panel,
}

/// `GitActionsControlProps`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitActionsControlProps {
    pub display_mode: GitDisplayMode,
    pub compact: bool,
    pub git_cwd: Option<String>,
    pub target: Option<(EnvironmentId, ThreadId)>,
    pub thread: GitThreadContext,
    pub is_draft: bool,
    pub draft_branch: Option<String>,
    /// A draft choosing a new worktree's base ref.
    pub is_selecting_worktree_base: bool,
    pub can_open_changes: bool,
}

/// What the control asks of its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitActionsControlEvent {
    /// `onOpenChanges`: the diff panel's Changes view.
    OpenChanges,
    /// `setDraftThreadContext({ branch, … })`.
    DraftBranchChanged {
        branch: Option<String>,
        manual: bool,
    },
    /// A server thread's stored branch was updated.
    ThreadBranchUpdated { branch: Option<String> },
}

/// `InlineGitActionSuccess`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InlineSuccess {
    pub title: String,
    pub description: Option<String>,
    /// `successScopeKey`: the result only shows for the checkout it came from.
    pub scope_key: String,
    pub shown_at_ms: i64,
}

pub struct GitActionsControl {
    pub(crate) backend: SharedGitActionsBackend,
    pub(crate) props: GitActionsControlProps,
    pub(crate) inline_success: Option<InlineSuccess>,
    pub(crate) pending_default: Option<PendingDefaultBranchAction>,
    pub(crate) menu_open: bool,
    pub(crate) pull_running: bool,
    pub(crate) init_pending: bool,
    _ticker: Task<()>,
}

impl EventEmitter<GitActionsControlEvent> for GitActionsControl {}

impl GitActionsControl {
    pub fn new(
        backend: SharedGitActionsBackend,
        props: GitActionsControlProps,
        cx: &mut Context<Self>,
    ) -> Self {
        // Elapsed counters and the progress rows tick by the second, and the
        // window-focus refresh debounce of upstream is the host's concern.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        if this.needs_tick(cx) {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            backend,
            props,
            inline_success: None,
            pending_default: None,
            menu_open: false,
            pull_running: false,
            init_pending: false,
            _ticker: ticker,
        }
    }

    /// The host's props changed (route, status of the draft, …). The target
    /// of an action is read from the props *when the action is requested*.
    pub fn set_props(&mut self, props: GitActionsControlProps, cx: &mut Context<Self>) {
        if self.props == props {
            return;
        }
        self.props = props;
        cx.notify();
    }

    pub(crate) fn needs_tick(&self, cx: &gpui_kit::App) -> bool {
        self.inline_success.is_some()
            || self
                .props
                .git_cwd
                .as_deref()
                .is_some_and(|cwd| self.backend.action_state(cwd, cx).is_running)
    }

    pub(crate) fn is_panel(&self) -> bool {
        self.props.display_mode == GitDisplayMode::Panel
    }

    /// `successScopeKey`.
    pub(crate) fn scope_key(&self) -> String {
        format!(
            "{}\u{0}{}",
            self.props
                .target
                .as_ref()
                .map(|(environment, _)| environment.as_str())
                .unwrap_or(""),
            self.props.git_cwd.as_deref().unwrap_or("")
        )
    }

    pub(crate) fn current_target(&self) -> Option<GitActionTarget> {
        let (environment_id, thread_id) = self.props.target.clone()?;
        Some(GitActionTarget {
            environment_id,
            thread_id,
            cwd: self.props.git_cwd.clone()?,
        })
    }

    pub(crate) fn visible_inline_success(&self) -> Option<&InlineSuccess> {
        self.inline_success
            .as_ref()
            .filter(|success| success.scope_key == self.scope_key())
            .filter(|success| now_ms() - success.shown_at_ms < GIT_ACTION_SUCCESS_VISIBLE_MS as i64)
    }

    pub(crate) fn status_for(&self, cx: &gpui_kit::App) -> backend::GitStatusView {
        match self.props.git_cwd.as_deref() {
            Some(cwd) => self.backend.status(cwd, cx),
            None => backend::GitStatusView::default(),
        }
    }

    pub(crate) fn action_state(&self, cx: &gpui_kit::App) -> VcsActionState {
        match self.props.git_cwd.as_deref() {
            Some(cwd) => self.backend.action_state(cwd, cx),
            None => VcsActionState::default(),
        }
    }

    /// `isGitActionRunning` over the tracked operations.
    pub(crate) fn is_action_running(&self, cx: &gpui_kit::App) -> bool {
        use turin_review::action_state::source_control_actions::{
            SourceControlActionKind, is_action_running,
        };
        let state = self.action_state(cx);
        is_action_running(
            state.is_running,
            state.operation.as_deref(),
            &[
                SourceControlActionKind::RunStackedAction,
                SourceControlActionKind::Pull,
                SourceControlActionKind::PublishRepository,
            ],
        )
    }

    // ── Branch sync ─────────────────────────────────────────────────────

    pub(crate) fn apply_branch_sync(
        &mut self,
        sync: BranchSync,
        target: &GitActionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match sync {
            BranchSync::None => {}
            BranchSync::UpdateServerThread {
                branch,
                expected_branch,
            } => {
                let task =
                    self.backend
                        .update_thread_branch(target, branch.clone(), expected_branch, cx);
                cx.spawn_in(window, async move |this, cx| {
                    let outcome = task.await;
                    this.update_in(cx, |this, window, cx| match outcome {
                        Ok(()) => {
                            this.props.thread.branch = branch.clone();
                            cx.emit(GitActionsControlEvent::ThreadBranchUpdated { branch });
                        }
                        Err(message) => {
                            this.toast_error("Unable to update thread branch", &message, window, cx)
                        }
                    })
                    .ok();
                })
                .detach();
            }
            BranchSync::SetDraftBranch { branch, manual } => {
                self.props.draft_branch = branch.clone();
                cx.emit(GitActionsControlEvent::DraftBranchChanged { branch, manual });
            }
        }
    }

    /// The draft reconciliation effect: follow the live checkout.
    pub fn reconcile_draft_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.current_target() else {
            return;
        };
        let status = self.status_for(cx).status;
        let sync = live_draft_branch_sync(
            &self.props.thread,
            self.props.draft_branch.as_deref(),
            status.as_ref(),
            self.is_action_running(cx),
            self.props.is_selecting_worktree_base,
        );
        self.apply_branch_sync(sync, &target, window, cx);
    }

    // ── Running actions ─────────────────────────────────────────────────

    pub(crate) fn toast_error(
        &self,
        title: &str,
        description: &str,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        window.push_notification(
            Notification::error(description.to_owned()).title(title.to_owned()),
            cx,
        );
    }

    /// `runGitActionWithToast`, bound to the checkout and thread it is
    /// requested for (`target`), so retained toast actions keep running where
    /// they started.
    pub fn run_git_action(
        &mut self,
        target: GitActionTarget,
        input: RunGitActionInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let status = self.status_for(cx).status;
        let is_default_ref = status.as_ref().is_some_and(|status| status.is_default_ref);
        let plan = plan_git_action(
            &input,
            status.as_ref(),
            is_default_ref,
            &self.props.thread,
            &target.thread_id,
            uuid_like(),
        );
        let request = match plan {
            GitActionPlan::Skip => return,
            GitActionPlan::ConfirmDefaultBranch(pending) => {
                self.pending_default = Some(pending);
                let copy = self.pending_default_copy(status.as_ref());
                default_branch_dialog::open(cx.entity().downgrade(), copy, window, cx);
                cx.notify();
                return;
            }
            GitActionPlan::Run(request) => request,
        };
        self.inline_success = None;
        let task = self.backend.run_stacked_action(&target, request, cx);
        let weak = cx.entity().downgrade();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_git_action(outcome, target, weak, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_git_action(
        &mut self,
        outcome: Result<turin_contracts::git::GitRunStackedActionResult, String>,
        target: GitActionTarget,
        weak: WeakEntity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match outcome {
            Ok(result) => result,
            Err(message) => {
                self.toast_error("Action failed", &message, window, cx);
                cx.notify();
                return;
            }
        };
        let sync = sync_after_git_action(
            &result,
            &self.props.thread,
            self.props.is_draft,
            self.props.draft_branch.as_deref(),
        );
        self.apply_branch_sync(sync, &target, window, cx);
        match present_action_result(&result, &target, self.is_panel()) {
            ActionPresentation::Inline { title, description } => {
                self.inline_success = Some(InlineSuccess {
                    title,
                    description,
                    scope_key: self.scope_key(),
                    shown_at_ms: now_ms(),
                });
            }
            ActionPresentation::Toast {
                title,
                description,
                cta,
                ..
            } => {
                let mut note = Notification::success(description.unwrap_or_default()).title(title);
                match cta {
                    ResultToastCta::None => {}
                    ResultToastCta::RunAction {
                        label,
                        target,
                        action,
                    } => {
                        note = note.action(move |_, _, _| {
                            let weak = weak.clone();
                            let target = target.clone();
                            gpui_kit::component::button::Button::new("git-toast-action")
                                .label(label.clone())
                                .on_click(move |_, window, cx| {
                                    weak.update(cx, |this, cx| {
                                        this.run_git_action(
                                            target.clone(),
                                            RunGitActionInput::new(action),
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                                })
                        });
                    }
                    ResultToastCta::OpenPr { label, url } => {
                        let backend = self.backend.clone();
                        note = note.action(move |_, _, _| {
                            let backend = backend.clone();
                            let url = url.clone();
                            gpui_kit::component::button::Button::new("git-toast-open-pr")
                                .label(label.clone())
                                .on_click(move |_, _, cx| backend.open_link(url.clone(), cx))
                        });
                    }
                }
                window.push_notification(note, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn continue_pending_default_branch_action(
        &mut self,
        feature_branch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_default.take() else {
            return;
        };
        let Some(target) = self.current_target() else {
            return;
        };
        self.run_git_action(
            target,
            continue_pending_action(&pending, feature_branch),
            window,
            cx,
        );
    }

    pub(crate) fn open_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let status = self.status_for(cx).status;
        commit_dialog::open(cx.entity().downgrade(), status, window, cx);
    }

    pub(crate) fn open_publish_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.current_target() else {
            return;
        };
        publish_dialog::open(
            cx.entity().downgrade(),
            self.backend.clone(),
            target,
            window,
            cx,
        );
    }

    /// `runQuickAction`.
    pub(crate) fn run_quick_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let status = self.status_for(cx).status;
        let busy = self.is_action_running(cx);
        let is_default_ref = status.as_ref().is_some_and(|status| status.is_default_ref);
        let has_primary_remote = status
            .as_ref()
            .is_some_and(|status| status.has_primary_remote);
        let quick = resolve_quick_action(status.as_ref(), busy, is_default_ref, has_primary_remote);
        let Some(target) = self.current_target() else {
            return;
        };
        match quick.kind {
            GitQuickActionKind::OpenPublish => self.open_publish_dialog(window, cx),
            GitQuickActionKind::RunPull => self.run_pull(target, window, cx),
            GitQuickActionKind::ShowHint => {
                window.push_notification(
                    Notification::info(quick.hint.unwrap_or_default()).title(quick.label),
                    cx,
                );
            }
            GitQuickActionKind::RunAction => {
                if let Some(action) = quick.action {
                    self.run_git_action(target, RunGitActionInput::new(action), window, cx);
                }
            }
        }
    }

    fn run_pull(&mut self, target: GitActionTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.inline_success = None;
        self.pull_running = true;
        let task = self.backend.pull(&target, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.pull_running = false;
                match outcome {
                    Err(message) => this.toast_error("Pull failed", &message, window, cx),
                    Ok(result) => {
                        use turin_contracts::git::VcsPullStatus;
                        let pulled = result.status == VcsPullStatus::Pulled;
                        let title = if pulled {
                            "Pulled"
                        } else {
                            "Already up to date"
                        };
                        let description = if pulled {
                            format!(
                                "Updated {} from {}",
                                result.ref_name,
                                result
                                    .upstream_ref
                                    .as_ref()
                                    .map(|upstream| upstream.as_str())
                                    .unwrap_or("upstream")
                            )
                        } else {
                            format!("{} is already synchronized.", result.ref_name)
                        };
                        if this.is_panel() {
                            this.inline_success = Some(InlineSuccess {
                                title: title.to_owned(),
                                description: Some(description),
                                scope_key: this.scope_key(),
                                shown_at_ms: now_ms(),
                            });
                        } else {
                            window.push_notification(
                                Notification::success(description).title(title),
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn initialize_git(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.current_target() else {
            return;
        };
        self.init_pending = true;
        let task = self.backend.init(&target, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.init_pending = false;
                if let Err(message) = outcome {
                    this.toast_error("Git initialization failed", &message, window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_changed_file(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cwd) = self.props.git_cwd.clone() else {
            self.toast_error("Editor opening is unavailable.", "", window, cx);
            return;
        };
        let task = self.backend.open_in_editor(path, &cwd, cx);
        cx.spawn_in(window, async move |this, cx| {
            if let Err(message) = task.await {
                this.update_in(cx, |this, window, cx| {
                    this.toast_error("Unable to open file", &message, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// The dialog's confirmed commit (`runDialogAction` and its
    /// "Commit on new branch" variant).
    pub(crate) fn run_commit_dialog(
        &mut self,
        commit_message: String,
        file_paths: Option<Vec<String>>,
        feature_branch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.current_target() else {
            return;
        };
        let message = commit_message.trim().to_owned();
        self.run_git_action(
            target,
            RunGitActionInput {
                action: GitStackedAction::Commit,
                commit_message: (!message.is_empty()).then_some(message),
                skip_default_branch_prompt: feature_branch,
                feature_branch,
                file_paths,
            },
            window,
            cx,
        );
    }

    /// Dialog copy for the pending default-ref confirmation.
    pub(crate) fn pending_default_copy(
        &self,
        status: Option<&VcsStatusResult>,
    ) -> Option<turin_review::action_state::git_actions_control_logic::DefaultBranchActionDialogCopy>
    {
        let pending = self.pending_default.as_ref()?;
        let terminology = change_request_terminology(
            status
                .and_then(|status| status.source_control_provider.as_ref())
                .map(|provider| provider.kind.as_str()),
        );
        Some(resolve_default_branch_action_dialog_copy(
            pending.action,
            &pending.branch_name,
            pending.includes_commit,
            Some(terminology),
        ))
    }

    pub(crate) fn menu_items(
        &self,
        cx: &gpui_kit::App,
    ) -> Vec<turin_review::action_state::git_actions_control_logic::GitActionMenuItem> {
        let status = self.status_for(cx).status;
        let has_primary_remote = status
            .as_ref()
            .is_some_and(|status| status.has_primary_remote);
        build_menu_items(
            status.as_ref(),
            self.is_action_running(cx),
            has_primary_remote,
        )
    }
}

/// `randomUUID()`: an id for one run (progress events key on it).
fn uuid_like() -> String {
    format!("{:x}-{:x}", now_ms(), std::process::id())
}

/// Builds the control as an entity.
pub fn new_git_actions_control(
    backend: SharedGitActionsBackend,
    props: GitActionsControlProps,
    cx: &mut gpui_kit::App,
) -> Entity<GitActionsControl> {
    cx.new(|cx| GitActionsControl::new(backend, props, cx))
}

#[allow(dead_code)]
fn _resolve_branch_sync_ref() {
    let _ = resolve_branch_sync;
}
