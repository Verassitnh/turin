//! Port of upstream/apps/web/src/components/BranchToolbarBranchSelector.tsx
//! and BranchPicker.tsx: the ref picker in the composer's context strip (and
//! the thread details panel): a trigger showing the active ref and a popover
//! with a search field, the ref list (with synthetic "Create new ref" and
//! "Checkout pull request" rows), a "Start from origin" switch while choosing
//! a new worktree's base, and paged loading.
//!
//! Deviations (see PORTING.md): the list is GPUI's virtual [`ListState`]
//! driven by its scroll handler instead of LegendList + scroll-offset math;
//! the optimistic ref (`useOptimistic`) is an explicit field reverted on
//! failure; the right-click "Copy branch name" menu is a copy on
//! right-click without a native menu.

use gpui_kit::component::Selectable as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::switch::Switch;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, CursorStyle, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, ScrollHandle,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    WeakEntity, Window, div, rems,
};
use turin_contracts::git::{VcsRef, VcsStatusResult};
use turin_contracts::{EnvironmentId, ThreadId};

use super::backend::{BranchThreadContext, RefPage, SharedBranchBackend};
use super::branch_toolbar_logic::{
    BranchSelectionRef, EnvMode, ResolveBranchTriggerLabelInput, ResolveEffectiveEnvModeInput,
    ShouldIncludeBranchPickerItemInput, derive_local_branch_name_from_remote_ref,
    resolve_branch_selection_target, resolve_branch_toolbar_pr_branch,
    resolve_branch_toolbar_value, resolve_branch_trigger_label,
    resolve_draft_env_mode_after_branch_change, resolve_effective_env_mode, sanitize_new_ref_name,
    should_include_branch_picker_item,
};
use super::composer_control::{
    ComposerControl, ComposerControlSize, composer_context_label, composer_control_chevron,
};
use crate::components::chat::thread_details_control::{
    ThreadDetailsControl, ThreadDetailsPart, panel_icon, panel_select_chevron,
};
use crate::components::git_actions_control::backend::SharedGitActionsBackend;
use crate::components::sidebar::thread_status_indicators::pull_request_state_tone;
use crate::pull_request_reference::parse_pull_request_reference;
use crate::state::support::thread_pull_requests::PullRequestBadgeState;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_SM, TEXT_3XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::icon::{Icon, IconName};
use crate::ui::middle_truncate::MiddleTruncate;
use crate::ui::tooltip::{Tooltip, dropdown_glass_background, dropdown_glass_border};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BranchDisplayMode {
    #[default]
    Toolbar,
    Panel,
}

/// `BranchToolbarBranchSelectorProps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchSelectorProps {
    pub display_mode: BranchDisplayMode,
    pub force_new_worktree: bool,
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub thread: BranchThreadContext,
    pub env_locked: bool,
    pub effective_env_mode_override: Option<EnvMode>,
    pub active_thread_branch_override: Option<Option<String>>,
    pub start_from_origin: bool,
    /// The strip collapsed its labels to icons.
    pub compact_labels: bool,
    pub can_checkout_pull_request: bool,
}

/// What the selector asks of its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchSelectorEvent {
    ComposerFocusRequested,
    StartFromOriginChanged(bool),
    /// `onActiveThreadBranchOverrideChange` (a launched thread).
    ActiveBranchOverrideChanged(Option<String>),
    /// `setDraftThreadContext`.
    DraftContextChanged {
        branch: Option<String>,
        worktree_path: Option<String>,
        env_mode: EnvMode,
        manual_environment: bool,
    },
    CheckoutPullRequestRequested(String),
}

/// A row of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PickerItem {
    CheckoutPullRequest(String),
    CreateBranch(String),
    Ref(String),
}

impl PickerItem {
    fn value(&self) -> String {
        match self {
            Self::CheckoutPullRequest(reference) => {
                format!("__checkout_pull_request__:{reference}")
            }
            Self::CreateBranch(query) => format!("__create_new_branch__:{query}"),
            Self::Ref(name) => name.clone(),
        }
    }
}

pub struct BranchToolbarBranchSelector {
    backend: SharedBranchBackend,
    git_backend: SharedGitActionsBackend,
    pub(crate) props: BranchSelectorProps,
    open: bool,
    query: Entity<InputState>,
    scroll: ScrollHandle,
    previous_scroll_top: Option<f32>,
    highlighted: Option<String>,
    /// `useOptimistic`: the ref the user just picked, until the switch settles.
    optimistic: Option<Option<String>>,
    action_pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BranchSelectorEvent> for BranchToolbarBranchSelector {}

/// Everything derived from props, status and the ref page for one render.
struct Derived {
    branch_cwd: Option<String>,
    active_project_cwd: Option<String>,
    active_worktree_path: Option<String>,
    effective_env_mode: EnvMode,
    active_thread_branch: Option<String>,
    status: Option<VcsStatusResult>,
    page: RefPage,
    current_git_branch: Option<String>,
    resolved_active_branch: Option<String>,
    trimmed_query: String,
    is_selecting_worktree_base: bool,
    items: Vec<PickerItem>,
    filtered: Vec<PickerItem>,
    new_ref_name: String,
    pr_reference: Option<String>,
}

impl BranchToolbarBranchSelector {
    pub fn new(
        backend: SharedBranchBackend,
        git_backend: SharedGitActionsBackend,
        props: BranchSelectorProps,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
        let subscriptions =
            vec![
                cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::Change => {
                            this.highlighted = None;
                            this.previous_scroll_top = None;
                            cx.notify();
                        }
                        InputEvent::PressEnter { .. } => this.select_highlighted(window, cx),
                        _ => {}
                    }
                }),
            ];
        Self {
            backend,
            git_backend,
            props,
            open: false,
            query,
            scroll: ScrollHandle::new(),
            previous_scroll_top: None,
            highlighted: None,
            optimistic: None,
            action_pending: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_props(&mut self, props: BranchSelectorProps, cx: &mut Context<Self>) {
        if self.props == props {
            return;
        }
        self.props = props;
        cx.notify();
    }

    /// `BranchToolbarBranchSelectorHandle.open`.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let derived = self.derive(cx);
        if derived.page.is_pending && derived.page.refs.is_empty() || self.action_pending {
            return;
        }
        self.set_open(true, window, cx);
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open = open;
        self.highlighted = None;
        if open {
            self.query.update(cx, |query, cx| query.focus(window, cx));
        } else {
            self.query
                .update(cx, |query, cx| query.set_value("", window, cx));
        }
        self.previous_scroll_top = None;
        cx.notify();
    }

    /// `maybeFetchNextBranchPage`: a downward scroll near the end loads the next page.
    fn maybe_load_next_page(&mut self, cx: &mut Context<Self>) {
        let derived = self.derive(cx);
        let offset = self.scroll.offset();
        let scroll_top = -f32::from(offset.y);
        let client_height = f32::from(self.scroll.bounds().size.height);
        let scroll_height = client_height + f32::from(self.scroll.max_offset().y.abs());
        let previous = self.previous_scroll_top.replace(scroll_top);
        if !self.open
            || !derived.page.has_next_page
            || derived.page.is_fetching_next_page
            || !super::paginated_branches::should_load_next_branch_page_after_scroll(
                previous,
                scroll_top,
                scroll_height,
                client_height,
            )
        {
            return;
        }
        if let Some(cwd) = derived.branch_cwd {
            self.backend
                .load_next_refs(&cwd, &derived.trimmed_query, cx);
        }
    }

    fn derive(&self, cx: &gpui_kit::App) -> Derived {
        let props = &self.props;
        let thread = &props.thread;
        let active_worktree_path = if props.force_new_worktree {
            None
        } else {
            thread.worktree_path.clone()
        };
        let active_project_cwd = thread
            .project_workspace_root
            .clone()
            .or_else(|| thread.project_cwd.clone());
        let branch_cwd = active_worktree_path
            .clone()
            .or_else(|| active_project_cwd.clone());
        let effective_env_mode = props.effective_env_mode_override.unwrap_or_else(|| {
            resolve_effective_env_mode(&ResolveEffectiveEnvModeInput {
                active_worktree_path: active_worktree_path.as_deref(),
                has_server_thread: thread.is_server_thread,
                draft_thread_env_mode: thread.draft_env_mode,
                preparing_worktree: false,
            })
        });
        let active_thread_branch = match &props.active_thread_branch_override {
            Some(branch) => branch.clone(),
            None => thread.branch.clone(),
        };
        let status = branch_cwd
            .as_deref()
            .and_then(|cwd| self.git_backend.status(cwd, cx).status);
        let query_text = self.query.read(cx).value().to_string();
        let trimmed_query = query_text.trim().to_owned();
        // The server filters refs by substring, so it has to be given the
        // sanitized name as well: querying the raw "new branch" drops an
        // existing new-branch from the response entirely.
        let ref_query = sanitize_new_ref_name(&trimmed_query);
        let page = branch_cwd
            .as_deref()
            .map(|cwd| self.backend.list_refs(cwd, &ref_query, cx))
            .unwrap_or_default();
        let current_git_branch = status
            .as_ref()
            .and_then(|status| status.ref_name.as_ref())
            .map(|name| name.as_str().to_owned())
            .or_else(|| {
                page.refs
                    .iter()
                    .find(|r| r.current)
                    .map(|r| r.name.as_str().to_owned())
            });
        let canonical = resolve_branch_toolbar_value(
            effective_env_mode,
            active_worktree_path.as_deref(),
            active_thread_branch.as_deref(),
            current_git_branch.as_deref(),
        );
        let resolved_active_branch = match &self.optimistic {
            Some(optimistic) => optimistic.clone(),
            None => canonical,
        };
        let is_selecting_worktree_base = effective_env_mode == EnvMode::Worktree
            && !props.env_locked
            && active_worktree_path.is_none();
        let pr_reference = parse_pull_request_reference(&trimmed_query);
        let checkout_item = (props.can_checkout_pull_request)
            .then_some(())
            .and(pr_reference.clone());
        let can_create_branch = !is_selecting_worktree_base && !trimmed_query.is_empty();
        let new_ref_name = sanitize_new_ref_name(&trimmed_query);
        let has_exact_branch_match = page.refs.iter().any(|r| r.name.as_str() == new_ref_name);
        let mut items: Vec<PickerItem> = page
            .refs
            .iter()
            .map(|r| PickerItem::Ref(r.name.as_str().to_owned()))
            .collect();
        let create_value =
            can_create_branch.then(|| PickerItem::CreateBranch(trimmed_query.clone()));
        if let Some(create) = &create_value
            && !has_exact_branch_match
        {
            items.push(create.clone());
        }
        let checkout_value = checkout_item.map(PickerItem::CheckoutPullRequest);
        if let Some(checkout) = &checkout_value {
            items.insert(0, checkout.clone());
        }
        let normalized = trimmed_query.to_lowercase();
        let create_value_text = create_value.as_ref().map(PickerItem::value);
        let checkout_value_text = checkout_value.as_ref().map(PickerItem::value);
        let filtered = if normalized.is_empty() {
            items.clone()
        } else {
            items
                .iter()
                .filter(|item| {
                    should_include_branch_picker_item(&ShouldIncludeBranchPickerItemInput {
                        item_value: &item.value(),
                        normalized_query: &normalized,
                        create_branch_item_value: create_value_text.as_deref(),
                        checkout_pull_request_item_value: checkout_value_text.as_deref(),
                    })
                })
                .cloned()
                .collect()
        };
        Derived {
            branch_cwd,
            active_project_cwd,
            active_worktree_path,
            effective_env_mode,
            active_thread_branch,
            status,
            page,
            current_git_branch,
            resolved_active_branch,
            trimmed_query,
            is_selecting_worktree_base,
            items,
            filtered,
            new_ref_name,
            pr_reference,
        }
    }

    // ── Thread branch mutation ──────────────────────────────────────────

    /// `setThreadBranch`.
    fn set_thread_branch(
        &mut self,
        branch: Option<String>,
        worktree_path: Option<String>,
        automatic: bool,
        derived: &Derived,
        cx: &mut Context<Self>,
    ) {
        let thread = self.props.thread.clone();
        if !thread.has_active_project {
            return;
        }
        if thread.has_session && worktree_path != derived.active_worktree_path {
            self.backend
                .stop_thread_session(&self.props.environment_id, &self.props.thread_id, cx);
        }
        if thread.is_server_thread {
            let task = self.backend.update_thread_metadata(
                &self.props.environment_id,
                &self.props.thread_id,
                branch.clone(),
                worktree_path,
                automatic,
                cx,
            );
            cx.spawn(async move |_, _| {
                let _ = task.await;
            })
            .detach();
            cx.emit(BranchSelectorEvent::ActiveBranchOverrideChanged(branch));
            return;
        }
        let env_mode = resolve_draft_env_mode_after_branch_change(
            worktree_path.as_deref(),
            derived.active_worktree_path.as_deref(),
            derived.effective_env_mode,
        );
        cx.emit(BranchSelectorEvent::DraftContextChanged {
            branch,
            worktree_path,
            env_mode,
            manual_environment: !automatic,
        });
    }

    /// The effect that defaults a new worktree's base to the repo default
    /// branch (origin/HEAD), falling back to the checked-out branch.
    pub fn apply_worktree_base_default(&mut self, cx: &mut Context<Self>) {
        let derived = self.derive(cx);
        let thread_started = self.props.thread.has_session;
        if thread_started
            || derived.effective_env_mode != EnvMode::Worktree
            || derived.active_worktree_path.is_some()
            || derived.active_thread_branch.is_some()
        {
            return;
        }
        let candidate = if derived.page.is_pending && derived.page.refs.is_empty() {
            None
        } else {
            derived
                .page
                .refs
                .iter()
                .find(|r| r.is_default)
                .map(|r| r.name.as_str().to_owned())
                .or_else(|| derived.current_git_branch.clone())
        };
        if let Some(candidate) = candidate {
            self.set_thread_branch(Some(candidate), None, true, &derived, cx);
        }
    }

    // ── Actions ─────────────────────────────────────────────────────────

    fn select_item(&mut self, item: &PickerItem, window: &mut Window, cx: &mut Context<Self>) {
        let derived = self.derive(cx);
        match item {
            PickerItem::CheckoutPullRequest(reference) => {
                self.set_open(false, window, cx);
                cx.emit(BranchSelectorEvent::ComposerFocusRequested);
                cx.emit(BranchSelectorEvent::CheckoutPullRequestRequested(
                    reference.clone(),
                ));
            }
            PickerItem::CreateBranch(query) => self.create_ref(query.clone(), &derived, window, cx),
            PickerItem::Ref(name) => {
                if let Some(vcs_ref) = derived
                    .page
                    .refs
                    .iter()
                    .find(|r| r.name.as_str() == name)
                    .cloned()
                {
                    self.select_branch(vcs_ref, &derived, window, cx);
                }
            }
        }
    }

    fn select_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let derived = self.derive(cx);
        let value = self
            .highlighted
            .clone()
            .or_else(|| derived.filtered.first().map(PickerItem::value));
        if let Some(item) = derived
            .filtered
            .iter()
            .find(|item| Some(item.value()) == value)
            .cloned()
        {
            self.select_item(&item, window, cx);
        }
    }

    fn run_branch_action(
        &mut self,
        task: gpui_kit::Task<Result<BranchActionResult, (String, String)>>,
        previous: Option<String>,
        derived_cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.action_pending = true;
        cx.notify();
        let _ = derived_cwd;
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.action_pending = false;
                match outcome {
                    Ok(done) => {
                        this.optimistic = Some(Some(done.branch.clone()));
                        let derived = this.derive(cx);
                        this.set_thread_branch(
                            Some(done.branch),
                            done.worktree_path,
                            false,
                            &derived,
                            cx,
                        );
                        this.optimistic = None;
                    }
                    Err((title, message)) => {
                        this.optimistic = Some(previous);
                        this.optimistic = None;
                        window.push_notification(Notification::error(message).title(title), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `selectBranch`.
    fn select_branch(
        &mut self,
        vcs_ref: VcsRef,
        derived: &Derived,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(branch_cwd), Some(active_project_cwd)) = (
            derived.branch_cwd.clone(),
            derived.active_project_cwd.clone(),
        ) else {
            return;
        };
        if self.action_pending {
            return;
        }
        let ref_name = vcs_ref.name.as_str().to_owned();

        if derived.is_selecting_worktree_base {
            self.set_thread_branch(Some(ref_name), None, false, derived, cx);
            self.set_open(false, window, cx);
            cx.emit(BranchSelectorEvent::ComposerFocusRequested);
            return;
        }

        let target = resolve_branch_selection_target(
            &active_project_cwd,
            derived.active_worktree_path.as_deref(),
            &BranchSelectionRef::from(&vcs_ref),
        );
        if target.reuse_existing_worktree {
            self.set_thread_branch(
                Some(ref_name),
                target.next_worktree_path,
                false,
                derived,
                cx,
            );
            self.set_open(false, window, cx);
            cx.emit(BranchSelectorEvent::ComposerFocusRequested);
            return;
        }

        let selected_branch_name = if vcs_ref.is_remote == Some(true) {
            derive_local_branch_name_from_remote_ref(&ref_name).to_owned()
        } else {
            ref_name.clone()
        };
        self.set_open(false, window, cx);
        cx.emit(BranchSelectorEvent::ComposerFocusRequested);

        let previous = derived.resolved_active_branch.clone();
        self.optimistic = Some(Some(selected_branch_name.clone()));
        let is_remote = vcs_ref.is_remote == Some(true);
        let switch = self.backend.switch_ref(&target.checkout_cwd, &ref_name, cx);
        let next_worktree_path = target.next_worktree_path;
        let task = cx.spawn(async move |_, _| match switch.await {
            Ok(result) => Ok(BranchActionResult {
                branch: if is_remote {
                    result.ref_name.unwrap_or(selected_branch_name)
                } else {
                    selected_branch_name
                },
                worktree_path: next_worktree_path,
            }),
            Err(message) => Err(("Failed to switch ref.".to_owned(), message)),
        });
        self.run_branch_action(task, previous, Some(branch_cwd), window, cx);
    }

    /// `createRef`.
    fn create_ref(
        &mut self,
        raw_name: String,
        derived: &Derived,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = sanitize_new_ref_name(&raw_name);
        let Some(branch_cwd) = derived.branch_cwd.clone() else {
            return;
        };
        if name.is_empty() || self.action_pending {
            return;
        }
        self.set_open(false, window, cx);
        cx.emit(BranchSelectorEvent::ComposerFocusRequested);
        let previous = derived.resolved_active_branch.clone();
        self.optimistic = Some(Some(name.clone()));
        let create = self.backend.create_ref(&branch_cwd, &name, cx);
        let worktree_path = derived.active_worktree_path.clone();
        let task = cx.spawn(async move |_, _| match create.await {
            Ok(created) => Ok(BranchActionResult {
                branch: created,
                worktree_path,
            }),
            Err(message) => Err(("Failed to create and switch ref.".to_owned(), message)),
        });
        self.run_branch_action(task, previous, Some(branch_cwd), window, cx);
    }

    fn copy_branch_name(&self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.backend.copy_text(name.clone(), cx);
        window.push_notification(Notification::success(name).title("Branch name copied"), cx);
    }

    // ── Rendering ───────────────────────────────────────────────────────

    fn render_pr_badge(
        &self,
        derived: &Derived,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Branch status is the fallback when this thread has no linked pull requests.
        let thread = &self.props.thread;
        let linked = crate::components::sidebar::thread_status_indicators::current_link(
            &thread.pull_requests,
        );
        let pr_branch = resolve_branch_toolbar_pr_branch(
            derived.active_thread_branch.as_deref(),
            derived.resolved_active_branch.as_deref(),
        );
        let branch_pr = pr_branch.and_then(|branch| {
            let status = derived.status.as_ref()?;
            (status.ref_name.as_ref().map(|name| name.as_str()) == Some(branch))
                .then(|| status.pr.clone())
                .flatten()
        });
        let (number, url, state) = match (linked, branch_pr) {
            (Some(link), _) => (
                link.number,
                link.url.clone(),
                match link.state {
                    Some(crate::state::models::PullRequestState::Merged) => {
                        PullRequestBadgeState::Merged
                    }
                    Some(crate::state::models::PullRequestState::Closed) => {
                        PullRequestBadgeState::Closed
                    }
                    _ if link.is_draft => PullRequestBadgeState::Draft,
                    _ => PullRequestBadgeState::Open,
                },
            ),
            (None, Some(pr)) => (
                pr.number.get(),
                pr.url.clone(),
                match pr.state {
                    turin_contracts::git::GitPullRequestState::Merged => {
                        PullRequestBadgeState::Merged
                    }
                    turin_contracts::git::GitPullRequestState::Closed => {
                        PullRequestBadgeState::Closed
                    }
                    _ if pr.is_draft == Some(true) => PullRequestBadgeState::Draft,
                    _ => PullRequestBadgeState::Open,
                },
            ),
            _ => return None,
        };
        let tone = pull_request_state_tone(state, theme);
        let git_backend = self.git_backend.clone();
        let _ = cx;
        Some(
            ComposerControl::new("branch-pr-badge")
                .size(ComposerControlSize::Xs)
                .tooltip(Tooltip::new(format!("Open pull request #{number}")))
                .on_click(move |_, _, cx| git_backend.open_link(url.clone(), cx))
                .child(
                    Icon::new(IconName::GitPullRequestArrow)
                        .size(rems(0.75))
                        .color(tone),
                )
                .child(div().text_color(tone).child(format!("#{number}")))
                .into_any_element(),
        )
    }

    fn picker_row(
        &self,
        item: &PickerItem,
        index: usize,
        derived: &Derived,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = item.value();
        let highlighted = self.highlighted.as_deref() == Some(value.as_str());
        let accent = theme.accent;
        let accent_foreground = theme.accent_foreground;
        let weak = cx.entity().downgrade();
        let item_for_click = item.clone();
        let hover_value = value.clone();
        let weak_hover = weak.clone();
        let mut row = div()
            .id(("branch-item", index))
            .flex()
            .min_h(rems(1.75))
            .w_full()
            .items_center()
            .rounded(RADIUS_SM)
            .px_2()
            .py_1()
            .text_size(TEXT_SM.size)
            .cursor(CursorStyle::PointingHand)
            .when(highlighted, move |this| {
                this.bg(accent).text_color(accent_foreground)
            })
            .on_mouse_move(move |_, _, cx| {
                let value = hover_value.clone();
                weak_hover
                    .update(cx, |this, cx| {
                        if this.highlighted.as_deref() != Some(value.as_str()) {
                            this.highlighted = Some(value);
                            cx.notify();
                        }
                    })
                    .ok();
            });
        let content: AnyElement = match item {
            PickerItem::CheckoutPullRequest(reference) => div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_2()
                .py_1()
                .child(
                    Icon::new(IconName::Github)
                        .size(rems(0.875))
                        .color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child("Checkout pull request"),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(reference.clone()),
                        ),
                )
                .into_any_element(),
            PickerItem::CreateBranch(_) => div()
                .truncate()
                .child(format!("Create new ref \"{}\"", derived.new_ref_name))
                .into_any_element(),
            PickerItem::Ref(name) => {
                let vcs_ref = derived.page.refs.iter().find(|r| r.name.as_str() == name);
                let has_secondary_worktree = vcs_ref.is_some_and(|r| {
                    r.worktree_path.as_ref().is_some_and(|path| {
                        derived
                            .active_project_cwd
                            .as_deref()
                            .is_some_and(|cwd| path.as_str() != cwd)
                    })
                });
                let badge = vcs_ref.and_then(|r| {
                    if r.current {
                        Some("current")
                    } else if has_secondary_worktree {
                        Some("worktree")
                    } else if r.is_remote == Some(true) {
                        Some("remote")
                    } else if r.is_default {
                        Some("default")
                    } else {
                        None
                    }
                });
                let copy_name = name.clone();
                let weak_copy = weak.clone();
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                        let name = copy_name.clone();
                        weak_copy
                            .update(cx, |this, cx| this.copy_branch_name(name, window, cx))
                            .ok();
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(MiddleTruncate::new(name.clone())),
                    )
                    .children(badge.map(|badge| {
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_3XS.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.45))
                            .child(badge)
                    }))
                    .into_any_element()
            }
        };
        row = row
            .child(content)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                let item = item_for_click.clone();
                weak.update(cx, |this, cx| this.select_item(&item, window, cx))
                    .ok();
            });
        row.into_any_element()
    }

    fn popup_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let derived = self.derive(cx);
        let weak = cx.entity().downgrade();
        let status_text = if derived.page.is_pending && derived.page.refs.is_empty() {
            Some("Loading refs...".to_owned())
        } else if derived.page.is_fetching_next_page {
            Some("Loading more refs...".to_owned())
        } else if derived.page.has_next_page {
            Some(format!(
                "Showing {} of {} refs",
                derived.page.refs.len(),
                derived.page.total_count
            ))
        } else {
            None
        };
        let rows: Vec<AnyElement> = derived
            .filtered
            .iter()
            .enumerate()
            .map(|(index, item)| self.picker_row(item, index, &derived, &theme, cx))
            .collect();
        let origin_control = derived.is_selecting_worktree_base.then(|| {
            let weak_origin = weak.clone();
            let checked = self.props.start_from_origin;
            div()
                .id("branch-start-from-origin")
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .border_t_1()
                .border_color(with_alpha(theme.border, 0.6))
                .px_3()
                .py_2()
                .text_size(TEXT_XS.size)
                .tooltip(|window, cx| {
                    Tooltip::new(
                        "Creates the worktree from the latest matching branch on origin instead of your local branch.",
                    )
                    .build(window, cx)
                })
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(rems(0.375))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::RefreshCw).size(rems(0.75)))
                        .child(div().truncate().child("Start from origin")),
                )
                .child(Switch::new("start-from-origin").checked(checked).on_click(
                    move |checked, _, cx| {
                        let checked = *checked;
                        weak_origin
                            .update(cx, |_, cx| cx.emit(BranchSelectorEvent::StartFromOriginChanged(checked)))
                            .ok();
                    },
                ))
        });
        let panel = self.props.display_mode == BranchDisplayMode::Panel;
        div()
            .flex()
            .flex_col()
            .when(!panel, |this| this.w(rems(20.)))
            .rounded_lg()
            .border_1()
            .border_color(dropdown_glass_border(&theme))
            .bg(dropdown_glass_background(&theme))
            .shadow(tokens::shadow_popover(theme.is_dark()))
            .text_color(theme.popover_foreground)
            .overflow_hidden()
            .child(
                // `ComboboxSearchInput`: `px-3 pt-2.5`, `border-b border-border/70 pb-1.5`.
                div().px_3().pt(rems(0.625)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(with_alpha(theme.border, 0.7))
                        .pb(rems(0.375))
                        .child(
                            Icon::new(IconName::Search)
                                .size(rems(1.))
                                .color(with_alpha(theme.muted_foreground, 0.55).into()),
                        )
                        .child(
                            div().flex_1().text_size(TEXT_SM.size).child(
                                Input::new(&self.query)
                                    .appearance(false)
                                    .bordered(false)
                                    .p_0(),
                            ),
                        ),
                ),
            )
            .child(if rows.is_empty() {
                div()
                    .p_2()
                    .flex()
                    .justify_center()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("No refs found.")
                    .into_any_element()
            } else {
                // `max-h-56` list, `ps-1 pt-2 pb-1`.
                div()
                    .id("branch-list")
                    .max_h(rems(14.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| {
                        this.maybe_load_next_page(cx);
                    }))
                    .pl_1()
                    .pr_1()
                    .pt_2()
                    .pb_1()
                    .children(rows)
                    .into_any_element()
            })
            .children(origin_control)
            .children(status_text.map(|text| {
                div()
                    .px_3()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child(text)
            }))
            .into_any_element()
    }
}

struct BranchActionResult {
    branch: String,
    worktree_path: Option<String>,
}

impl Render for BranchToolbarBranchSelector {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let derived = self.derive(cx);
        let weak = cx.entity().downgrade();
        let panel = self.props.display_mode == BranchDisplayMode::Panel;
        let remote = derived.resolved_active_branch.as_deref().and_then(|name| {
            derived
                .page
                .refs
                .iter()
                .find(|r| r.name.as_str() == name)
                .map(|r| r.is_remote == Some(true))
                .or_else(|| {
                    derived
                        .branch_cwd
                        .as_deref()
                        .and_then(|cwd| self.backend.find_ref(cwd, name, cx))
                        .map(|r| r.is_remote == Some(true))
                })
        });
        let trigger_label = resolve_branch_trigger_label(&ResolveBranchTriggerLabelInput {
            active_worktree_path: derived.active_worktree_path.as_deref(),
            effective_env_mode: derived.effective_env_mode,
            resolved_active_branch: derived.resolved_active_branch.as_deref(),
            resolved_active_branch_is_remote: remote,
            start_from_origin: self.props.start_from_origin,
        });
        let disabled =
            (derived.page.is_pending && derived.page.refs.is_empty()) || self.action_pending;
        let weak_open = weak.clone();

        let trigger: AnyElement = if panel {
            ThreadDetailsControl::new("branch-trigger")
                .part(ThreadDetailsPart::Select)
                .disabled(disabled)
                .selected(self.open)
                .child(panel_icon(IconName::GitBranch, &theme))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(MiddleTruncate::new(SharedString::from(trigger_label))),
                )
                .child(panel_select_chevron(&theme))
                .into_any_element()
        } else {
            ComposerControl::new("branch-trigger")
                .size(ComposerControlSize::Xs)
                .shrinkable()
                .disabled(disabled)
                .selected(self.open)
                .child(
                    div().flex_shrink_0().opacity(0.7).child(
                        Icon::new(IconName::GitBranch)
                            .size(rems(0.75))
                            .color(theme.muted_foreground),
                    ),
                )
                .child(composer_context_label(
                    MiddleTruncate::new(SharedString::from(trigger_label)),
                    self.props.compact_labels,
                ))
                .child(composer_control_chevron(&theme))
                .into_any_element()
        };

        let popover = Popover::new("branch-picker")
            .appearance(false)
            .anchor(if panel {
                gpui_kit::Anchor::TopLeft
            } else {
                gpui_kit::Anchor::BottomRight
            })
            .trigger(BranchTrigger {
                inner: trigger,
                selected: self.open,
            })
            .open(self.open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                weak_open
                    .update(cx, |this, cx| this.set_open(open, window, cx))
                    .ok();
            })
            .content({
                let weak_content = weak.clone();
                move |_, _, cx| match weak_content.upgrade() {
                    Some(selector) => {
                        selector.update(cx, |selector, cx| selector.popup_content(cx))
                    }
                    None => div().into_any_element(),
                }
            });

        div()
            .flex()
            .min_w_0()
            .when(panel, |this| this.w_full().flex_col().items_stretch())
            .when(!panel, |this| this.items_center().gap_1())
            .children(
                (!panel)
                    .then(|| self.render_pr_badge(&derived, &theme, cx))
                    .flatten(),
            )
            .child(div().flex().min_w_0().child(popover))
    }
}

/// Wraps a pre-rendered trigger so it can open the popover (`Selectable`).
#[derive(IntoElement)]
struct BranchTrigger {
    inner: AnyElement,
    selected: bool,
}

impl gpui_kit::component::Selectable for BranchTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl gpui_kit::RenderOnce for BranchTrigger {
    fn render(self, _: &mut Window, _: &mut gpui_kit::App) -> impl IntoElement {
        self.inner
    }
}

/// Builds the selector as an entity.
pub fn new_branch_selector(
    backend: SharedBranchBackend,
    git_backend: SharedGitActionsBackend,
    props: BranchSelectorProps,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> Entity<BranchToolbarBranchSelector> {
    cx.new(|cx| BranchToolbarBranchSelector::new(backend, git_backend, props, window, cx))
}
