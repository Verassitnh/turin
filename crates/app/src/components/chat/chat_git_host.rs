//! The chat view's git surfaces, hosted: the branch toolbar strip under the
//! composer and the thread details card (ThreadDetailsPanel.tsx +
//! ThreadDetailsCard.tsx + ThreadDetailsSection.tsx) with the workspace and
//! version control sections.
//!
//! Upstream's ChatView owns this state (the draft's env mode and branch, the
//! start-from-origin toggle, the launched thread's branch override) and
//! threads it into the toolbar and the panel as props; until the composer
//! (T17) and the thread details card (T16) land, [`ChatGitHost`] keeps that
//! state itself so the controls behave end to end against the mock backends.
//! The card is placed at its inline position (top right, 12px gaps, 280px);
//! its measured density folding (`full` / `compact` / `essential`) and the
//! popover presentation are not ported.

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Styled as _,
    Subscription, Window, div, px, rems,
};
use turin_contracts::{EnvironmentId, ThreadId};

use crate::components::branch_toolbar::backend::{
    BranchThreadContext, ProjectThreadInfo, SharedBranchBackend,
};
use crate::components::branch_toolbar::branch_toolbar_logic::{
    EnvMode, ResolveEffectiveEnvModeInput, resolve_effective_env_mode,
};
use crate::components::branch_toolbar::{
    BranchToolbar, BranchToolbarEvent, BranchToolbarLayout, BranchToolbarProps, PanelSection,
};
use crate::components::chat::open_in_picker::OpenInPickerView;
use crate::components::git_actions_control::backend::{GitThreadContext, SharedGitActionsBackend};
use crate::components::git_actions_control::{
    GitActionsControl, GitActionsControlEvent, GitActionsControlProps, GitDisplayMode,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_3XL};

/// Where "Open in" opens the thread: its worktree, else the project checkout
/// (`openInCwd`: the git cwd).
pub fn open_in_cwd(routed: &RoutedGitThread) -> Option<String> {
    Some(
        routed
            .worktree_path
            .clone()
            .unwrap_or_else(|| routed.project_cwd.clone()),
    )
    .filter(|cwd| !cwd.is_empty())
}

/// The routed thread, as the git surfaces read it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutedGitThread {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub project_cwd: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub has_session: bool,
    pub pull_requests: Vec<crate::state::models::ThreadPullRequestLink>,
    pub project_threads: Vec<ProjectThreadInfo>,
}

#[derive(Clone)]
pub struct ChatGitBackends {
    pub git: SharedGitActionsBackend,
    pub branch: SharedBranchBackend,
    /// The app's tokio runtime, for the editor launcher. `None` in the mock
    /// shell, which has no checkout to open.
    pub runtime: Option<tokio::runtime::Handle>,
}

pub struct ChatGitHost {
    backends: ChatGitBackends,
    composer_toolbar: Option<Entity<BranchToolbar>>,
    workspace_toolbar: Option<Entity<BranchToolbar>>,
    branch_toolbar: Option<Entity<BranchToolbar>>,
    git: Option<Entity<GitActionsControl>>,
    /// The "Open in" control of the workspace section (`OpenInPicker`, panel mode).
    open_in: Option<Entity<OpenInPickerView>>,
    routed: Option<RoutedGitThread>,
    // The draft/launch state ChatView owns upstream.
    env_mode: Option<EnvMode>,
    start_from_origin: bool,
    branch_override: Option<Option<String>>,
    /// The checkout whose git status was last requested.
    refreshed_cwd: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ChatGitHost {
    pub fn new(backends: ChatGitBackends) -> Self {
        Self {
            backends,
            composer_toolbar: None,
            workspace_toolbar: None,
            branch_toolbar: None,
            git: None,
            open_in: None,
            routed: None,
            env_mode: None,
            start_from_origin: false,
            branch_override: None,
            refreshed_cwd: None,
            _subscriptions: Vec::new(),
        }
    }

    fn thread_context(routed: &RoutedGitThread) -> BranchThreadContext {
        BranchThreadContext {
            is_server_thread: true,
            has_session: routed.has_session,
            has_active_project: true,
            project_cwd: Some(routed.project_cwd.clone()),
            project_workspace_root: Some(routed.project_cwd.clone()),
            worktree_path: routed.worktree_path.clone(),
            branch: routed.branch.clone(),
            draft_env_mode: None,
            project_threads: routed.project_threads.clone(),
            pull_requests: routed.pull_requests.clone(),
        }
    }

    fn toolbar_props(
        &self,
        routed: &RoutedGitThread,
        layout: BranchToolbarLayout,
        section: PanelSection,
    ) -> BranchToolbarProps {
        let thread = Self::thread_context(routed);
        let env_mode = self.env_mode.unwrap_or_else(|| {
            resolve_effective_env_mode(&ResolveEffectiveEnvModeInput {
                active_worktree_path: thread.worktree_path.as_deref(),
                has_server_thread: true,
                draft_thread_env_mode: None,
                preparing_worktree: false,
            })
        });
        BranchToolbarProps {
            layout,
            panel_section: section,
            force_new_worktree: false,
            environment_id: routed.environment_id.clone(),
            thread_id: routed.thread_id.clone(),
            show_git_controls: true,
            env_mode,
            start_from_origin: self.start_from_origin,
            // A started thread's workspace is pinned.
            env_locked: routed.has_session,
            thread,
            active_thread_branch_override: self.branch_override.clone(),
            available_environments: Vec::new(),
            context_strip_visible: true,
            compact_labels: false,
        }
    }

    fn git_props(&self, routed: &RoutedGitThread) -> GitActionsControlProps {
        GitActionsControlProps {
            display_mode: GitDisplayMode::Panel,
            compact: false,
            git_cwd: Some(
                routed
                    .worktree_path
                    .clone()
                    .unwrap_or_else(|| routed.project_cwd.clone()),
            ),
            target: Some((routed.environment_id.clone(), routed.thread_id.clone())),
            thread: GitThreadContext {
                is_server_thread: true,
                project_id: None,
                branch: self
                    .branch_override
                    .clone()
                    .unwrap_or_else(|| routed.branch.clone()),
            },
            is_draft: false,
            draft_branch: None,
            is_selecting_worktree_base: false,
            can_open_changes: false,
        }
    }

    /// Follows the route: creates the entities once, then keeps their props current.
    pub fn sync(
        &mut self,
        routed: Option<RoutedGitThread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(routed) = routed else {
            self.routed = None;
            return;
        };
        if self.routed.as_ref() != Some(&routed) {
            // A different thread resets the per-thread UI state.
            if self
                .routed
                .as_ref()
                .is_none_or(|previous| previous.thread_id != routed.thread_id)
            {
                self.env_mode = None;
                self.start_from_origin = false;
                self.branch_override = None;
            }
            self.routed = Some(routed.clone());
        }
        // Git status is a pull model here: ask for it when the checkout
        // changes (mount, new thread, worktree switch) and on window focus.
        let cwd = self.git_props(&routed).git_cwd;
        if self.refreshed_cwd != cwd {
            if let Some(cwd) = &cwd {
                self.backends.git.refresh_status(cwd, cx);
            }
            self.refreshed_cwd = cwd;
        }
        if self.composer_toolbar.is_none() {
            self._subscriptions
                .push(cx.observe_window_activation(window, |this, window, cx| {
                    if window.is_window_active()
                        && let Some(cwd) = this.refreshed_cwd.clone()
                    {
                        this.backends.git.refresh_status(&cwd, cx);
                    }
                }));
            let make =
                |layout, section, window: &mut Window, cx: &mut Context<Self>, this: &Self| {
                    let props = this.toolbar_props(&routed, layout, section);
                    let backends = this.backends.clone();
                    cx.new(|cx| {
                        BranchToolbar::new(backends.branch, backends.git, props, window, cx)
                    })
                };
            let composer = make(
                BranchToolbarLayout::Composer,
                PanelSection::All,
                window,
                cx,
                self,
            );
            let workspace = make(
                BranchToolbarLayout::Panel,
                PanelSection::Workspace,
                window,
                cx,
                self,
            );
            let branch = make(
                BranchToolbarLayout::Panel,
                PanelSection::Branch,
                window,
                cx,
                self,
            );
            for toolbar in [&composer, &workspace, &branch] {
                self._subscriptions.push(cx.subscribe(
                    toolbar,
                    |this, _, event: &BranchToolbarEvent, cx| {
                        match event {
                            BranchToolbarEvent::EnvModeChanged(mode) => this.env_mode = Some(*mode),
                            BranchToolbarEvent::StartFromOriginChanged(value) => {
                                this.start_from_origin = *value
                            }
                            BranchToolbarEvent::ActiveBranchOverrideChanged(branch) => {
                                this.branch_override = Some(branch.clone())
                            }
                            BranchToolbarEvent::DraftContextChanged { .. }
                            | BranchToolbarEvent::ComposerFocusRequested
                            | BranchToolbarEvent::CheckoutPullRequestRequested(_) => {}
                        }
                        cx.notify();
                    },
                ));
            }
            let git_props = self.git_props(&routed);
            let backends = self.backends.clone();
            let git = cx.new(|cx| GitActionsControl::new(backends.git, git_props, cx));
            self._subscriptions.push(cx.subscribe(
                &git,
                |this, _, event: &GitActionsControlEvent, cx| {
                    if let GitActionsControlEvent::ThreadBranchUpdated { branch } = event {
                        this.branch_override = Some(branch.clone());
                    }
                    cx.notify();
                },
            ));
            if let Some(runtime) = self.backends.runtime.clone() {
                let cwd = open_in_cwd(&routed);
                self.open_in =
                    Some(cx.new(|cx| OpenInPickerView::new(cwd, true, runtime, window, cx)));
            }
            self.composer_toolbar = Some(composer);
            self.workspace_toolbar = Some(workspace);
            self.branch_toolbar = Some(branch);
            self.git = Some(git);
            return;
        }
        let composer_props =
            self.toolbar_props(&routed, BranchToolbarLayout::Composer, PanelSection::All);
        let workspace_props =
            self.toolbar_props(&routed, BranchToolbarLayout::Panel, PanelSection::Workspace);
        let branch_props =
            self.toolbar_props(&routed, BranchToolbarLayout::Panel, PanelSection::Branch);
        let git_props = self.git_props(&routed);
        if let Some(toolbar) = &self.composer_toolbar {
            toolbar.update(cx, |toolbar, cx| toolbar.set_props(composer_props, cx));
        }
        if let Some(toolbar) = &self.workspace_toolbar {
            toolbar.update(cx, |toolbar, cx| toolbar.set_props(workspace_props, cx));
        }
        if let Some(toolbar) = &self.branch_toolbar {
            toolbar.update(cx, |toolbar, cx| toolbar.set_props(branch_props, cx));
        }
        if let Some(git) = &self.git {
            git.update(cx, |git, cx| git.set_props(git_props, cx));
        }
        if let Some(open_in) = &self.open_in {
            let cwd = open_in_cwd(&routed);
            open_in.update(cx, |open_in, cx| open_in.set_cwd(cwd, cx));
        }
    }

    /// The strip under the composer (`ComposerSurface.ContextStrip`).
    pub fn composer_strip(&self) -> Option<AnyElement> {
        self.composer_toolbar
            .as_ref()
            .filter(|_| self.routed.is_some())
            .map(|toolbar| toolbar.clone().into_any_element())
    }

    /// The thread details card, anchored top right of the chat canvas.
    pub fn details_card(&self, cx: &gpui_kit::App) -> Option<AnyElement> {
        self.details_card_with_lineage(None, cx)
    }

    pub fn details_card_with_lineage(
        &self,
        lineage: Option<
            gpui_kit::Entity<
                crate::components::chat::thread_relationships_panel::ThreadRelationshipsPanel,
            >,
        >,
        cx: &gpui_kit::App,
    ) -> Option<AnyElement> {
        let (workspace, branch, git) = (
            self.workspace_toolbar.as_ref()?,
            self.branch_toolbar.as_ref()?,
            self.git.as_ref()?,
        );
        self.routed.as_ref()?;
        let theme = cx.app_theme().clone();
        // `ThreadDetailsSection`: `px-2 pt-2 pb-2.5`, `border-t border-border/65` when separated.
        let section = |separated: bool| {
            let section = div().flex().flex_col().px_2().pt_2().pb(rems(0.625));
            if separated {
                section
                    .border_t_1()
                    .border_color(with_alpha(theme.border, 0.65))
            } else {
                section
            }
        };
        Some(
            div()
                .absolute()
                .top(px(12.))
                .right(px(12.))
                .w(px(280.))
                .max_h_full()
                .overflow_hidden()
                .rounded(RADIUS_3XL)
                .border_1()
                .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
                // `dropdown-glass` blurs what is behind it; GPUI cannot, so the
                // card is opaque to keep the timeline out of its text.
                .bg(theme.popover)
                .shadow(tokens::shadow_popover(theme.is_dark()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .children(lineage)
                        .child(
                            section(false)
                                .child(workspace.clone())
                                .children(self.open_in.clone()),
                        )
                        .child(section(true).child(branch.clone()).child(git.clone())),
                )
                .into_any_element(),
        )
    }
}

impl ChatGitHost {
    /// Dev hook for screenshots (`TURIN_GIT_UI=<step>;<step>…`, 350ms apart):
    /// `menu`, `commit`, `publish`, `default-ref` (push on the default ref),
    /// `branch-picker`, `workspace-menu`, `run` (the quick action).
    pub fn run_demo_steps(&mut self, spec: String, window: &mut Window, cx: &mut Context<Self>) {
        let steps: Vec<String> = spec.split(';').map(|step| step.trim().to_owned()).collect();
        cx.spawn_in(window, async move |this, cx| {
            for step in steps {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(350))
                    .await;
                let outcome =
                    this.update_in(cx, |this, window, cx| this.demo_step(&step, window, cx));
                if outcome.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn demo_step(&mut self, step: &str, window: &mut Window, cx: &mut Context<Self>) {
        use crate::components::git_actions_control::lifecycle::RunGitActionInput;
        use turin_contracts::git::GitStackedAction;
        match step {
            "menu" => {
                if let Some(git) = &self.git {
                    git.update(cx, |git, cx| {
                        git.menu_open = true;
                        cx.notify();
                    });
                }
            }
            "commit" => {
                if let Some(git) = &self.git {
                    git.update(cx, |git, cx| git.open_commit_dialog(window, cx));
                }
            }
            "publish" => {
                if let Some(git) = &self.git {
                    git.update(cx, |git, cx| git.open_publish_dialog(window, cx));
                }
            }
            "default-ref" => {
                if let Some(git) = &self.git {
                    git.update(cx, |git, cx| {
                        if let Some(target) = git.current_target() {
                            git.run_git_action(
                                target,
                                RunGitActionInput::new(GitStackedAction::Push),
                                window,
                                cx,
                            );
                        }
                    });
                }
            }
            "run" => {
                if let Some(git) = &self.git {
                    git.update(cx, |git, cx| git.run_quick_action(window, cx));
                }
            }
            "branch-picker" => {
                if let Some(toolbar) = &self.composer_toolbar {
                    toolbar.update(cx, |toolbar, cx| toolbar.open_branch_picker(window, cx));
                }
            }
            "workspace-menu" => {
                if let Some(toolbar) = &self.composer_toolbar {
                    toolbar.update(cx, |toolbar, cx| toolbar.open_workspace_menu(cx));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routed(worktree: Option<&str>, project: &str) -> RoutedGitThread {
        RoutedGitThread {
            environment_id: EnvironmentId::new("local").unwrap(),
            thread_id: ThreadId::new("t1").unwrap(),
            project_cwd: project.to_owned(),
            branch: None,
            worktree_path: worktree.map(str::to_owned),
            has_session: false,
            pull_requests: Vec::new(),
            project_threads: Vec::new(),
        }
    }

    #[test]
    fn open_in_targets_the_worktree_before_the_project_checkout() {
        assert_eq!(
            open_in_cwd(&routed(Some("/wt/feature"), "/repo")).as_deref(),
            Some("/wt/feature")
        );
        assert_eq!(
            open_in_cwd(&routed(None, "/repo")).as_deref(),
            Some("/repo")
        );
        assert_eq!(open_in_cwd(&routed(None, "")), None);
    }
}
