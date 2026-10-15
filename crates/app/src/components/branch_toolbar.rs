//! Port of upstream/apps/web/src/components/BranchToolbar.tsx
//! (+ BranchToolbarBranchSelector, BranchToolbarEnvironmentSelector,
//! BranchToolbarEnvModeSelector, BranchPicker).
//!
//! The strip under the composer: the workspace (run context) selector on the
//! left, the composer's resting controls host in the middle and the ref
//! selector on the right; and, in `panel` layout, the same pieces as rows of
//! the thread details card. `Run on` (machine) choices are cut with remote
//! environments: with one machine the strip shows only the workspace.
//!
//! Not ported: the strip's measured label collapse (`useLabelsOverflow` reads
//! DOM widths; the host passes `compact_labels` instead), the workspace
//! right-click "Copy full path" menu (copying is on the row's right-click)
//! and the panel test's machine-choice case.

pub mod backend;
pub mod branch_selector;
pub mod branch_toolbar_logic;
pub mod composer_control;
pub mod env_selectors;
pub mod mock_source;
pub mod paginated_branches;

use gpui_kit::component::Selectable as _;
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, CursorStyle, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Styled as _,
    Subscription, WeakEntity, Window, div, rems,
};
use turin_contracts::{EnvironmentId, ThreadId};

use self::backend::{BranchThreadContext, SharedBranchBackend};
use self::branch_selector::{
    BranchDisplayMode, BranchSelectorEvent, BranchSelectorProps, BranchToolbarBranchSelector,
};
use self::branch_toolbar_logic::{
    EnvMode, EnvironmentOption, PreviousWorktreeSeed, ShouldShowComposerContextStripInput,
    resolve_current_workspace_label, resolve_env_mode_label, resolve_previous_worktree_seed,
    should_show_composer_context_strip, should_show_environment_indicator,
};
use self::composer_control::{
    ComposerControl, ComposerControlSize, composer_context_label, composer_context_strip,
    composer_control_chevron,
};
use self::env_selectors::{
    environment_indicator, previous_worktree_item, workspace_icon, workspace_labels,
};
use crate::components::chat::thread_details_control::{
    ThreadDetailsControl, ThreadDetailsPart, panel_icon, panel_select_chevron,
};
use crate::components::git_actions_control::backend::SharedGitActionsBackend;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_SM, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::icon::{Icon, IconName};
use crate::ui::middle_truncate::MiddleTruncate;
use crate::ui::tooltip::{Tooltip, dropdown_glass_background, dropdown_glass_border};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BranchToolbarLayout {
    #[default]
    Composer,
    Panel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelSection {
    #[default]
    All,
    Workspace,
    Branch,
}

/// `BranchToolbarProps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchToolbarProps {
    pub layout: BranchToolbarLayout,
    pub panel_section: PanelSection,
    pub force_new_worktree: bool,
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub show_git_controls: bool,
    /// The thread's env mode as ChatView resolves it.
    pub env_mode: EnvMode,
    pub start_from_origin: bool,
    pub env_locked: bool,
    pub thread: BranchThreadContext,
    pub active_thread_branch_override: Option<Option<String>>,
    pub available_environments: Vec<EnvironmentOption>,
    pub context_strip_visible: bool,
    /// The strip's content no longer fits: labels collapse to icons.
    pub compact_labels: bool,
}

/// What the toolbar asks of its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchToolbarEvent {
    EnvModeChanged(EnvMode),
    StartFromOriginChanged(bool),
    ActiveBranchOverrideChanged(Option<String>),
    DraftContextChanged {
        branch: Option<String>,
        worktree_path: Option<String>,
        env_mode: EnvMode,
        manual_environment: bool,
    },
    ComposerFocusRequested,
    CheckoutPullRequestRequested(String),
}

pub struct BranchToolbar {
    backend: SharedBranchBackend,
    props: BranchToolbarProps,
    selector: Entity<BranchToolbarBranchSelector>,
    workspace_menu_open: bool,
    _selector_events: Subscription,
}

impl EventEmitter<BranchToolbarEvent> for BranchToolbar {}

fn selector_props(
    props: &BranchToolbarProps,
    display_mode: BranchDisplayMode,
) -> BranchSelectorProps {
    let effective_env_mode = if props.force_new_worktree {
        EnvMode::Worktree
    } else {
        props.env_mode
    };
    BranchSelectorProps {
        display_mode,
        force_new_worktree: props.force_new_worktree,
        environment_id: props.environment_id.clone(),
        thread_id: props.thread_id.clone(),
        thread: props.thread.clone(),
        env_locked: props.env_locked,
        effective_env_mode_override: Some(effective_env_mode),
        active_thread_branch_override: props.active_thread_branch_override.clone(),
        start_from_origin: props.start_from_origin,
        compact_labels: props.compact_labels,
        can_checkout_pull_request: true,
    }
}

impl BranchToolbar {
    pub fn new(
        backend: SharedBranchBackend,
        git_backend: SharedGitActionsBackend,
        props: BranchToolbarProps,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let display_mode = if props.layout == BranchToolbarLayout::Panel {
            BranchDisplayMode::Panel
        } else {
            BranchDisplayMode::Toolbar
        };
        let selector_props = selector_props(&props, display_mode);
        let selector = {
            let backend = backend.clone();
            cx.new(|cx| {
                BranchToolbarBranchSelector::new(backend, git_backend, selector_props, window, cx)
            })
        };
        let events = cx.subscribe(&selector, |_, _, event: &BranchSelectorEvent, cx| {
            cx.emit(match event.clone() {
                BranchSelectorEvent::ComposerFocusRequested => {
                    BranchToolbarEvent::ComposerFocusRequested
                }
                BranchSelectorEvent::StartFromOriginChanged(value) => {
                    BranchToolbarEvent::StartFromOriginChanged(value)
                }
                BranchSelectorEvent::ActiveBranchOverrideChanged(branch) => {
                    BranchToolbarEvent::ActiveBranchOverrideChanged(branch)
                }
                BranchSelectorEvent::DraftContextChanged {
                    branch,
                    worktree_path,
                    env_mode,
                    manual_environment,
                } => BranchToolbarEvent::DraftContextChanged {
                    branch,
                    worktree_path,
                    env_mode,
                    manual_environment,
                },
                BranchSelectorEvent::CheckoutPullRequestRequested(reference) => {
                    BranchToolbarEvent::CheckoutPullRequestRequested(reference)
                }
            });
        });
        Self {
            backend,
            props,
            selector,
            workspace_menu_open: false,
            _selector_events: events,
        }
    }

    pub fn set_props(&mut self, props: BranchToolbarProps, cx: &mut Context<Self>) {
        if self.props == props {
            return;
        }
        let display_mode = if props.layout == BranchToolbarLayout::Panel {
            BranchDisplayMode::Panel
        } else {
            BranchDisplayMode::Toolbar
        };
        let selector_props = selector_props(&props, display_mode);
        self.props = props;
        self.selector
            .update(cx, |selector, cx| selector.set_props(selector_props, cx));
        cx.notify();
    }

    /// Opens the workspace menu (screenshots).
    pub fn open_workspace_menu(&mut self, cx: &mut Context<Self>) {
        self.workspace_menu_open = true;
        cx.notify();
    }

    /// `BranchToolbarHandle.openBranchPicker`.
    pub fn open_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selector
            .update(cx, |selector, cx| selector.open(window, cx));
    }

    // ── Derived state ───────────────────────────────────────────────────

    fn active_worktree_path(&self) -> Option<String> {
        if self.props.force_new_worktree {
            None
        } else {
            self.props.thread.worktree_path.clone()
        }
    }

    fn effective_env_mode(&self) -> EnvMode {
        if self.props.force_new_worktree {
            EnvMode::Worktree
        } else {
            self.props.env_mode
        }
    }

    fn env_mode_locked(&self) -> bool {
        self.props.env_locked
            || (self.props.thread.is_server_thread && self.active_worktree_path().is_some())
    }

    /// "Previous worktree" hops a draft into the most recently active worktree
    /// of this project: only drafts can hop; started server threads have their
    /// workspace pinned.
    fn previous_worktree_seed(&self) -> Option<PreviousWorktreeSeed> {
        let can_use = !self.props.thread.is_server_thread
            && !self.env_mode_locked()
            && !self.props.force_new_worktree;
        if !can_use {
            return None;
        }
        let threads: Vec<_> = self
            .props
            .thread
            .project_threads
            .iter()
            .map(|thread| thread.as_worktree_thread())
            .collect();
        resolve_previous_worktree_seed(&threads, self.active_worktree_path().as_deref())
    }

    /// Same shape the branch selector writes when picking a ref that already
    /// lives in a worktree: point the draft at the existing tree.
    pub fn use_previous_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(seed) = self.previous_worktree_seed() else {
            return;
        };
        if !self.props.show_git_controls {
            return;
        }
        cx.emit(BranchToolbarEvent::DraftContextChanged {
            branch: seed.branch,
            worktree_path: Some(seed.worktree_path),
            env_mode: EnvMode::Worktree,
            manual_environment: true,
        });
        cx.emit(BranchToolbarEvent::ComposerFocusRequested);
    }

    fn show_environment_indicator(&self) -> bool {
        let active = self
            .props
            .available_environments
            .iter()
            .find(|env| env.environment_id == self.props.environment_id);
        should_show_environment_indicator(
            active.map(|env| env.is_primary),
            self.props.available_environments.len() > 1,
        )
    }

    /// `shouldShowComposerContextStrip`.
    pub fn is_visible(
        &self,
        is_draft_hero_state: bool,
        persist_in_active_threads: bool,
        is_git_repo: bool,
    ) -> bool {
        should_show_composer_context_strip(&ShouldShowComposerContextStripInput {
            is_draft_hero_state,
            persist_in_active_threads,
            has_active_project: self.props.thread.has_active_project,
            is_git_repo,
            show_environment_indicator: self.show_environment_indicator(),
            hosts_resting_composer_controls: false,
        })
    }

    // ── Rendering ───────────────────────────────────────────────────────

    fn workspace_menu_row(
        &self,
        id: &'static str,
        icon: IconName,
        content: AnyElement,
        selected: bool,
        disabled: bool,
        weak: WeakEntity<Self>,
        value: WorkspaceChoice,
        theme: &AppTheme,
    ) -> AnyElement {
        let accent = theme.accent;
        let accent_foreground = theme.accent_foreground;
        div()
            .id(id)
            .flex()
            .min_h(rems(1.75))
            .w_full()
            .items_center()
            .gap_2()
            .rounded(RADIUS_SM)
            .px_2()
            .py_1()
            .text_size(TEXT_SM.size)
            .child(
                Icon::new(icon)
                    .size(rems(0.75))
                    .color(theme.muted_foreground),
            )
            .child(div().min_w_0().flex_1().child(content))
            .child(div().w(rems(1.)).when(selected, |this| {
                this.child(
                    Icon::new(IconName::Check)
                        .size(rems(0.875))
                        .color(theme.foreground),
                )
            }))
            .map(|this| {
                if disabled {
                    this.opacity(0.64).cursor(CursorStyle::OperationNotAllowed)
                } else {
                    this.cursor(CursorStyle::PointingHand)
                        .hover(move |style| style.bg(accent).text_color(accent_foreground))
                }
            })
            .when(!disabled, |this| {
                this.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    weak.update(cx, |this, cx| {
                        this.workspace_menu_open = false;
                        match value {
                            WorkspaceChoice::Mode(mode) => {
                                cx.emit(BranchToolbarEvent::EnvModeChanged(mode))
                            }
                            WorkspaceChoice::PreviousWorktree => this.use_previous_worktree(cx),
                        }
                        cx.notify();
                    })
                    .ok();
                })
            })
            .into_any_element()
    }

    /// The Workspace group of the run context menu.
    fn workspace_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let weak = cx.entity().downgrade();
        let effective = self.effective_env_mode();
        let worktree_path = self.active_worktree_path();
        let locked = self.env_mode_locked();
        let seed = self.previous_worktree_seed();
        let wide = seed.is_some();
        let mut rows = vec![
            self.workspace_menu_row(
                "workspace-local",
                if worktree_path.is_some() {
                    IconName::FolderGit
                } else {
                    IconName::Folder
                },
                MiddleTruncate::new(resolve_current_workspace_label(worktree_path.as_deref()))
                    .into_any_element(),
                effective == EnvMode::Local,
                locked || self.props.force_new_worktree,
                weak.clone(),
                WorkspaceChoice::Mode(EnvMode::Local),
                &theme,
            ),
            self.workspace_menu_row(
                "workspace-worktree",
                IconName::FolderGit2,
                div()
                    .truncate()
                    .child(resolve_env_mode_label(EnvMode::Worktree))
                    .into_any_element(),
                effective == EnvMode::Worktree,
                locked,
                weak.clone(),
                WorkspaceChoice::Mode(EnvMode::Worktree),
                &theme,
            ),
        ];
        if let Some(seed) = &seed {
            rows.push(self.workspace_menu_row(
                "workspace-previous",
                IconName::FolderGit2,
                previous_worktree_item(seed.branch.as_deref(), &theme),
                false,
                locked,
                weak.clone(),
                WorkspaceChoice::PreviousWorktree,
                &theme,
            ));
        }
        div()
            .flex()
            .flex_col()
            .min_w(rems(if wide { 21. } else { 12. }))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(dropdown_glass_border(&theme))
            .bg(dropdown_glass_background(&theme))
            .shadow(tokens::shadow_popover(theme.is_dark()))
            .text_color(theme.popover_foreground)
            .child(
                div()
                    .px_2()
                    .py(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child("Workspace"),
            )
            .children(rows)
            .into_any_element()
    }

    /// `RunContextSelector`: the workspace trigger (a static row when locked).
    fn render_run_context(&mut self, panel: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let effective = self.effective_env_mode();
        let worktree_path = self.active_worktree_path();
        let labels = workspace_labels(
            self.props.force_new_worktree,
            self.env_mode_locked(),
            effective,
            worktree_path.as_deref(),
        );
        let icon = workspace_icon(effective, worktree_path.as_deref());
        let locked = self.props.env_locked || self.env_mode_locked();
        let tooltip_text = if panel && self.props.force_new_worktree {
            "Each model starts in its own worktree.".to_owned()
        } else if panel {
            worktree_path
                .clone()
                .or_else(|| self.props.thread.project_workspace_root.clone())
                .unwrap_or_else(|| labels.trigger.clone())
        } else {
            labels.trigger.clone()
        };
        let weak = cx.entity().downgrade();

        if locked {
            return if panel {
                ThreadDetailsControl::new("run-context-locked")
                    .part(ThreadDetailsPart::Row)
                    .tooltip(Tooltip::new(tooltip_text))
                    .child(panel_icon(icon, &theme))
                    .child(div().min_w_0().flex_1().truncate().child(labels.trigger))
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .h(rems(1.75))
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .px(rems(0.4375))
                    .text_size(TEXT_XS.size)
                    .text_color(with_alpha(theme.muted_foreground, 0.7))
                    .child(
                        Icon::new(icon)
                            .size(rems(0.75))
                            .color(with_alpha(theme.muted_foreground, 0.7).into()),
                    )
                    .child(composer_context_label(
                        labels.trigger,
                        self.props.compact_labels,
                    ))
                    .into_any_element()
            };
        }

        let trigger: AnyElement = if panel {
            ThreadDetailsControl::new("run-context")
                .part(ThreadDetailsPart::Select)
                .selected(self.workspace_menu_open)
                .child(panel_icon(icon, &theme))
                .child(div().min_w_0().flex_1().truncate().child(labels.trigger))
                .child(panel_select_chevron(&theme))
                .into_any_element()
        } else {
            ComposerControl::new("run-context")
                .size(ComposerControlSize::Xs)
                .shrinkable()
                .selected(self.workspace_menu_open)
                .tooltip(Tooltip::new(tooltip_text))
                .child(
                    div().flex_shrink_0().child(
                        Icon::new(icon)
                            .size(rems(0.75))
                            .color(theme.muted_foreground),
                    ),
                )
                .child(composer_context_label(
                    labels.trigger,
                    self.props.compact_labels,
                ))
                .child(composer_control_chevron(&theme))
                .into_any_element()
        };
        let weak_open = weak.clone();
        let popover = Popover::new("run-context-menu")
            .appearance(false)
            .anchor(if panel {
                gpui_kit::Anchor::TopLeft
            } else {
                gpui_kit::Anchor::BottomLeft
            })
            .trigger(MenuTrigger {
                inner: trigger,
                selected: self.workspace_menu_open,
            })
            .open(self.workspace_menu_open)
            .on_open_change(move |open, _, cx| {
                let open = *open;
                weak_open
                    .update(cx, |this, cx| {
                        this.workspace_menu_open = open;
                        cx.notify();
                    })
                    .ok();
            })
            .content(move |_, _, cx| match weak.upgrade() {
                Some(toolbar) => toolbar.update(cx, |toolbar, cx| toolbar.workspace_menu(cx)),
                None => div().into_any_element(),
            });
        div()
            .flex()
            .min_w_0()
            .when(panel, |this| this.w_full())
            .child(popover)
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
enum WorkspaceChoice {
    Mode(EnvMode),
    PreviousWorktree,
}

/// Wraps a pre-rendered trigger so it can open a popover (`Selectable`).
#[derive(IntoElement)]
struct MenuTrigger {
    inner: AnyElement,
    selected: bool,
}

impl gpui_kit::component::Selectable for MenuTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl gpui_kit::RenderOnce for MenuTrigger {
    fn render(self, _: &mut Window, _: &mut gpui_kit::App) -> impl IntoElement {
        self.inner
    }
}

impl Render for BranchToolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let thread = &self.props.thread;
        if !thread.has_active_project {
            return div().into_any_element();
        }
        let show_git = self.props.show_git_controls;
        let show_environment = self.show_environment_indicator();

        if self.props.layout == BranchToolbarLayout::Panel {
            let section = self.props.panel_section;
            let mut column = div().flex().w_full().flex_col();
            if section != PanelSection::Branch {
                column = column.child(self.render_run_context(true, cx));
            }
            if section != PanelSection::Workspace {
                column = column.child(self.selector.clone());
            }
            return column.into_any_element();
        }

        // Composer layout: workspace left, branch right. The wide layout's
        // separate environment/env-mode selectors only show at `@3xl`
        // (48rem), wider than the 46rem composer lane.
        let mut strip = composer_context_strip(&theme)
            .id("composer-context-strip")
            .when(!self.props.context_strip_visible, |this| {
                this.absolute().left_0().right_0().top_full().invisible()
            });
        if show_git {
            strip = strip.child(self.render_run_context(false, cx));
        }
        if show_environment {
            let active = self
                .props
                .available_environments
                .iter()
                .find(|env| env.environment_id == self.props.environment_id);
            strip = strip.child(environment_indicator(
                active,
                self.props.compact_labels,
                &theme,
            ));
        }
        strip = strip.child(div().flex_1().min_w_0());
        if show_git {
            strip = strip.child(self.selector.clone());
        }
        strip.into_any_element()
    }
}

/// Builds the toolbar as an entity.
pub fn new_branch_toolbar(
    backend: SharedBranchBackend,
    git_backend: SharedGitActionsBackend,
    props: BranchToolbarProps,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> Entity<BranchToolbar> {
    cx.new(|cx| BranchToolbar::new(backend, git_backend, props, window, cx))
}
