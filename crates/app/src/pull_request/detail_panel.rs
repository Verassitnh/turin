//! Port of upstream/apps/web/src/components/pullRequest/PullRequestDetailPanel.tsx: the header
//! (repository and number, merge controls, the "more" menu, title, author, branches), the tab bar
//! with its checks line, and the mounted tabs.
//!
//! A tab stays mounted once visited: its entity (and with it its scroll offset) is kept while
//! another tab shows, as upstream keeps it behind `visibility: hidden`; only the active tab is
//! laid out. The reads come from the view-model store; the commands go to its source.
//!
//! Not ported (each needs the thread, worktree or composer services, which are out of scope):
//! the checkout menu and the "act on" environment picker, "Resolve conflicts" (rendered disabled),
//! the "Ask a question" / "Explain this PR" / "Fix findings" hand-offs into a composer or new
//! thread, per-finding "Fix" buttons, the linked-threads count and menu item, stack merge and
//! rebase (the source has no stack action), the floating comment/review composer (mounted by the
//! composer task), cached detail snapshots (the store keeps its own), the `useLiveRefresh`
//! revalidation, the keyboard shortcut for "Copy PR number", and the right-click copy menu on the
//! number. `useEnvironmentSettings` is replaced by [`REVIEW_GUIDES_ENABLED`]; the host's "stacked
//! base" refs query by the native stack's position. Transitions (`transition-[opacity,transform]`,
//! `grid-template-rows`) are instant swaps.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClipboardItem, Context, CursorStyle, Entity,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div, px, rems,
};
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestChecksState, PullRequestDetail, PullRequestDetailView,
    PullRequestMergeMethod, PullRequestRef, PullRequestState, PullRequestUpdateMethod,
};

use super::checks_popover::PullRequestChecksPopover;
use super::code_tab::CodeTab;
use super::comment_body::EditButton;
use super::copyable_code::PullRequestCopyableCode;
use super::detail_context::{DetailContext, DetailNav, DetailTab};
use super::detail_layout::{
    HEADER_LABELS_MIN_WIDTH, HeaderBody, alpha, checks_status_line, detail_ghost, files_label,
    format_count, header_body, tab_bar, timeline_ghost, title_row, visible_tabs,
};
use super::guide_tab::GuideTab;
use super::list_states::PullRequestsUnavailableState;
use super::presentation_view::{
    PullRequestActorLabel, PullRequestDiffStat, PullRequestStateGlyph, Spinner, tone_color,
};
use super::pull_request_detail_logic::{
    PULL_REQUEST_MERGE_METHOD_LABELS, PullRequestPrimaryControl, PullRequestReviewOutcome,
    allowed_pull_request_merge_methods, is_pull_request_conflicting,
    latest_pull_request_review_outcomes, loading_pull_request_checkout_command,
    pull_request_action_menu_has_group, pull_request_action_needs_host_refresh,
    pull_request_checkout_command, readable_failure, resolve_base_freshness,
    resolve_pull_request_merge_method, resolve_pull_request_primary_control,
};
use super::pull_request_editing_logic::{
    ConfirmClick, EditingSubject, InlineActionConfirm, can_edit_pull_request_change_request,
    open_on_host_label,
};
use super::pull_request_presentation::{
    pull_request_checks_state, pull_request_review_outcome_tone_class_name,
    resolve_pull_request_state, summarize_pull_request_checks,
};
use super::summary_tab::{SummaryTab, SummaryTabEvent};
use super::timeline_tab::TimelineTab;
use super::toast::{ToastKind, ToastQueue, render_toasts};
use super::view_model::{Loadable, PullRequestStore, pull_request_key};
use crate::state::helpers::source_control_clients::SOURCE_CONTROL_CLIENTS;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_BASE, TEXT_XS};
use crate::ui::{
    Badge, BadgeSize, BadgeVariant, Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip,
};

/// The Guide tab is a beta each server opts into in Settings → Source Control
/// (`reviewGuidesEnabled`). The settings screen is not ported yet, so the tab is offered.
pub const REVIEW_GUIDES_ENABLED: bool = true;

/// Said as the thing that did not happen, rather than as the operation that returned an error.
pub fn action_failure_label(action: PullRequestAction) -> &'static str {
    match action {
        PullRequestAction::Merge => "Could not merge this pull request",
        PullRequestAction::Ready => "Could not mark this ready for review",
        PullRequestAction::Draft => "Could not convert this to a draft",
        PullRequestAction::Close => "Could not close this pull request",
        PullRequestAction::Reopen => "Could not reopen this pull request",
        PullRequestAction::UpdateBranch => "Could not update this branch",
        PullRequestAction::EnableAutoMerge => "Could not turn on auto-merge",
        PullRequestAction::DisableAutoMerge => "Could not turn off auto-merge",
        PullRequestAction::Revert => "Could not open a revert pull request",
        PullRequestAction::ApproveWorkflows => "Could not approve workflows",
    }
}

/// What to try, for the times the host says only that it refused.
pub fn action_failure_hint(action: PullRequestAction) -> &'static str {
    match action {
        PullRequestAction::Merge => {
            "The host refused the merge. Check that you have write access, that the checks it requires have passed, and that the branch is not conflicting."
        }
        PullRequestAction::Ready | PullRequestAction::Draft => {
            "The host refused it. Check that you have write access to this repository."
        }
        PullRequestAction::Close => {
            "The host refused it. Check that you have write access, or that you opened it."
        }
        PullRequestAction::Reopen => {
            "The host refused it. Check that you have write access, and that the branch still exists."
        }
        // Said for the merge commit, which is what an update is unless a rebase was asked for.
        PullRequestAction::UpdateBranch => {
            "The host refused it. Check that you have write access to the branch — one from a fork also needs its author to allow edits from maintainers — and that it does not conflict with the base."
        }
        // The one refusal that is usually a repository setting rather than anything about this
        // branch: GitHub will not arm an auto-merge unless the repository has the feature on.
        PullRequestAction::EnableAutoMerge => {
            "The host refused it. Check that this repository allows auto-merge, that you have write access, and that there is something left for it to wait on."
        }
        PullRequestAction::DisableAutoMerge => {
            "The host refused it. Check that you have write access, and that the merge has not already happened."
        }
        PullRequestAction::Revert => {
            "The host refused it. Check that you have write access and that this pull request was merged on the host."
        }
        PullRequestAction::ApproveWorkflows => {
            "The host refused it. Check that you have Actions write access and that these workflow runs are still awaiting approval."
        }
    }
}

/// Said instead of the update hint when the reader asked for a rebase: it is the one that fails
/// on its own merits, because the host replays the commits and stops at the first that does not
/// apply.
pub const UPDATE_BRANCH_REBASE_FAILURE_HINT: &str = "The host refused it. A rebase stops at the first commit that does not apply cleanly; updating with a merge commit may still work.";

/// The timeline's order toggle in the tab bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineOrder {
    Newest,
    Oldest,
}

impl TimelineOrder {
    fn as_comment_order(self) -> super::pull_request_detail_logic::PullRequestCommentOrder {
        use super::pull_request_detail_logic::PullRequestCommentOrder as Order;
        match self {
            Self::Newest => Order::Newest,
            Self::Oldest => Order::Oldest,
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Newest => Self::Oldest,
            Self::Oldest => Self::Newest,
        }
    }
}

/// `changeRequestRepositoryUrl` for the hosts that write `…/pull/N`: the repository page.
pub fn repository_url(url: &str) -> Option<String> {
    let (repository, rest) = url.rsplit_once("/pull/")?;
    let number: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if number.is_empty() || !repository.starts_with("http") {
        return None;
    }
    Some(repository.to_owned())
}

/// The panel's detail read, without the activity fields (the shape the pure logic is typed on).
pub fn core_detail(view: &PullRequestDetailView) -> PullRequestDetail {
    PullRequestDetail {
        provider: view.provider.clone(),
        capabilities: view.capabilities.clone(),
        viewer_permissions: view.viewer_permissions.clone(),
        project_id: view.project_id.clone(),
        project_title: view.project_title.clone(),
        workspace_root: view.workspace_root.clone(),
        repository: view.repository.clone(),
        number: view.number,
        title: view.title.clone(),
        body: view.body.clone(),
        url: view.url.clone(),
        author: view.author.clone(),
        state: view.state,
        is_draft: view.is_draft,
        mergeability: view.mergeability,
        additions: view.additions,
        deletions: view.deletions,
        changed_files: view.changed_files,
        head_branch: view.head_branch.clone(),
        head_repository_name_with_owner: view.head_repository_name_with_owner.clone(),
        head_sha: view.head_sha.clone(),
        base_branch: view.base_branch.clone(),
        created_at: view.created_at.clone(),
        updated_at: view.updated_at.clone(),
        observed_at: view.observed_at,
        merged_at: view.merged_at.clone(),
        closed_at: view.closed_at.clone(),
        reviewers: view.reviewers.clone(),
        labels: view.labels.clone(),
        checks: view.checks.clone(),
        merge_capabilities: view.merge_capabilities,
        viewer: view.viewer.clone(),
        base_comparison: view.base_comparison,
        behind_by: view.behind_by,
        auto_merge_enabled: view.auto_merge_enabled,
        auto_merge_method: view.auto_merge_method,
        workflow_approvals_required: view.workflow_approvals_required,
    }
}

/// The facts `canEditPullRequestChangeRequest` / `canEditPullRequestComment` read. A reader who
/// may not write the host sees no edit controls (the panel zeroes `capabilities.edit` then).
pub fn editing_subject(detail: &PullRequestDetailView, can_write: bool) -> EditingSubject<'_> {
    EditingSubject {
        author: detail.author.as_ref(),
        capabilities: &detail.capabilities,
        viewer: if can_write {
            detail.viewer.as_ref().map(|viewer| viewer.as_str())
        } else {
            None
        },
        viewer_permissions: &detail.viewer_permissions,
    }
}

/// What the header decides from the detail: which controls exist and what they say. Computed in
/// one place so the buttons, the menu and the tests read the same answers.
#[derive(Clone, Debug)]
pub struct HeaderState {
    pub allowed_merge_methods: Vec<PullRequestMergeMethod>,
    pub selected_merge_method: PullRequestMergeMethod,
    pub selected_merge_method_label: &'static str,
    pub conflicting: bool,
    pub auto_merge_armed: bool,
    pub armed_auto_merge_label: String,
    pub pending_auto_merge_label: String,
    pub primary: Option<PullRequestPrimaryControl>,
    pub shows_draft_toggle: bool,
    pub shows_auto_merge: bool,
    pub shows_merge_now: bool,
    pub shows_merge_methods: bool,
    pub shows_approve_workflows: bool,
    pub checks_state: Option<PullRequestChecksState>,
    pub checks_summary: String,
    pub approval_count: usize,
}

pub fn merge_method_label(method: PullRequestMergeMethod) -> &'static str {
    PULL_REQUEST_MERGE_METHOD_LABELS
        .iter()
        .find(|(candidate, _)| *candidate == method)
        .map_or("Merge", |(_, label)| *label)
}

impl HeaderState {
    pub fn resolve(
        detail: &PullRequestDetailView,
        can_write: bool,
        merge_method: Option<PullRequestMergeMethod>,
        project_default: Option<PullRequestMergeMethod>,
        last_selected: PullRequestMergeMethod,
        can_merge_single: bool,
    ) -> Self {
        let core = core_detail(detail);
        let allowed = allowed_pull_request_merge_methods(Some(&core));
        let selected = resolve_pull_request_merge_method(
            &allowed,
            merge_method,
            project_default,
            last_selected,
        );
        let selected_label = merge_method_label(selected);
        let can = |action: PullRequestAction| {
            can_write
                && detail.capabilities.actions.contains(&action)
                && detail.viewer_permissions.actions.contains(&action)
        };
        let open = detail.state == PullRequestState::Open;
        let conflicting = is_pull_request_conflicting(Some(&core));
        // Only an outright yes arms it. A host that reports nothing has not said the merge is
        // already spoken for, and an off switch for something that may not be on says the wrong
        // thing twice.
        let auto_merge_armed = open && detail.auto_merge_enabled == Some(true);
        let armed_auto_merge_label = detail.auto_merge_method.map_or_else(
            || "Auto-merge".to_owned(),
            |method| format!("Auto-merge ({})", merge_method_label(method).to_lowercase()),
        );
        let workflow_approvals = if open {
            detail.workflow_approvals_required.unwrap_or(0)
        } else {
            0
        };
        let detail_checks_state = pull_request_checks_state(&detail.checks);
        // List rollups can omit workflows awaiting approval. Only refreshed detail can clear those.
        let checks_state = if detail_checks_state != Some(PullRequestChecksState::Failing)
            && detail.checks.iter().any(|check| {
                check.status
                    == turin_contracts::pull_request::PullRequestCheckStatus::ActionRequired
            }) {
            Some(PullRequestChecksState::Pending)
        } else {
            detail_checks_state
        };
        use super::pull_request_detail_logic::PullRequestPrimaryControlInput as Input;
        let primary = resolve_pull_request_primary_control(Input {
            state: detail.state,
            is_draft: detail.is_draft,
            mergeability: detail.mergeability,
            checks_state,
            auto_merge_enabled: detail.auto_merge_enabled,
            has_merge_method: !allowed.is_empty(),
            can_merge: can_merge_single && can(PullRequestAction::Merge),
            can_mark_ready: can(PullRequestAction::Ready),
            can_enable_auto_merge: can_merge_single && can(PullRequestAction::EnableAutoMerge),
        });
        let shows_draft_toggle =
            open && can(if detail.is_draft {
                PullRequestAction::Ready
            } else {
                PullRequestAction::Draft
            }) && !(detail.is_draft && primary == Some(PullRequestPrimaryControl::Ready));
        let shows_auto_merge = can_merge_single
            && open
            && ((auto_merge_armed && can(PullRequestAction::DisableAutoMerge))
                || (!auto_merge_armed
                    && primary != Some(PullRequestPrimaryControl::EnableAutoMerge)
                    && !detail.is_draft
                    && !conflicting
                    && can(PullRequestAction::EnableAutoMerge)
                    && !allowed.is_empty()));
        let shows_merge_now = can_merge_single
            && open
            && matches!(
                primary,
                Some(PullRequestPrimaryControl::EnableAutoMerge)
                    | Some(PullRequestPrimaryControl::AutoMergeArmed)
            )
            && can(PullRequestAction::Merge)
            && !detail.is_draft
            && !conflicting
            && !allowed.is_empty();
        let shows_merge_methods = open
            && can(PullRequestAction::Merge)
            && !detail.is_draft
            && !conflicting
            && allowed.len() > 1;
        let checks_summary = summarize_pull_request_checks(&detail.checks);
        // Approvals that still stand, and only those. Not counted from a conversation this page
        // only holds the recent end of: an approval older than the window would be missing, and
        // "1" beside a tick is read as the whole answer.
        let approval_count = if detail.comments_truncated {
            0
        } else {
            latest_pull_request_review_outcomes(&detail.comments, &detail.commits)
                .iter()
                .filter(|entry| entry.outcome == PullRequestReviewOutcome::Approved && !entry.stale)
                .count()
        };
        Self {
            pending_auto_merge_label: format!("Auto-merge ({})", selected_label.to_lowercase()),
            allowed_merge_methods: allowed,
            selected_merge_method: selected,
            selected_merge_method_label: selected_label,
            conflicting,
            auto_merge_armed,
            armed_auto_merge_label,
            primary,
            shows_draft_toggle,
            shows_auto_merge,
            shows_merge_now,
            shows_merge_methods,
            shows_approve_workflows: workflow_approvals > 0
                && can(PullRequestAction::ApproveWorkflows),
            checks_state,
            checks_summary,
            approval_count,
        }
    }
}

/// `window.setTimeout(disarm, 3000)` of `useInlineActionConfirm`.
const CONFIRM_FOR: Duration = Duration::from_secs(3);

/// The scroll distance past which the chrome folds into one line (`top > foldHeight + 32`), and
/// the distance back to the top that reopens it (`top < 4`).
const CONDENSE_MARGIN: f32 = 32.;
const REOPEN_AT: f32 = 4.;
/// Height of the expanded header body before it has been measured.
const DEFAULT_FOLD_HEIGHT: f32 = 132.;
/// Height of the condensed row that stands in for the body (`px-4 pb-2 pt-1` + one `text-xs` line).
const CONDENSED_ROW_HEIGHT: f32 = 36.;

pub struct PullRequestDetailPanel {
    pub ctx: DetailContext,
    pub tab: DetailTab,
    mounted: Vec<DetailTab>,
    summary: Entity<SummaryTab>,
    timeline: Option<Entity<TimelineTab>>,
    code: Option<Entity<CodeTab>>,
    guide: Option<Entity<GuideTab>>,
    /// The floating comment / review composer (`PullRequestComposer`).
    composer: Entity<super::comment_form::PullRequestComposer>,
    /// The timeline's order, set by the toggle in the tab bar.
    pub timeline_order: TimelineOrder,
    /// The commit the Code tab is scoped to (`selectedCodeCommitOid`), set by the summary's commit
    /// rows through [`DetailNav::open_commit`].
    pub code_commit: Option<String>,
    /// The file the guide asked the Code tab to reveal (`codeRevealRequest`).
    pub code_reveal: Option<String>,
    condensed: bool,
    fold_height: f32,
    width: f32,
    confirm: Option<InlineActionConfirm>,
    confirm_epoch: u64,
    merge_method: Option<PullRequestMergeMethod>,
    last_merge_method: PullRequestMergeMethod,
    title_edit: Option<Entity<InputState>>,
    title_saving: bool,
    toasts: ToastQueue,
    seen_error: Option<String>,
    seen_guide_error: Option<String>,
    /// The action last sent, so a failure can be said in its own words.
    last_action: Option<(PullRequestAction, Option<PullRequestUpdateMethod>)>,
    _subscriptions: Vec<Subscription>,
}

impl PullRequestDetailPanel {
    pub fn new(
        store: Entity<PullRequestStore>,
        environment_id: EnvironmentId,
        reference: PullRequestRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Some(source) = store.read(cx).source() {
            source.open_detail(&reference, cx);
        }
        let panel = cx.entity().downgrade();
        let nav = DetailNav {
            open_commit: {
                let panel = panel.clone();
                Rc::new(move |oid, cx| {
                    let oid = oid.to_owned();
                    panel
                        .update(cx, |panel, cx| panel.open_commit(oid, cx))
                        .ok();
                })
            },
            open_file_in_code: {
                let panel = panel.clone();
                Rc::new(move |path, cx| {
                    let path = path.to_owned();
                    panel
                        .update(cx, |panel, cx| panel.open_file_in_code(path, cx))
                        .ok();
                })
            },
            open_tab: {
                let panel = panel.clone();
                Rc::new(move |tab, cx| {
                    panel.update(cx, |panel, cx| panel.set_tab(tab, cx)).ok();
                })
            },
            select_pull_request: Rc::new(|_, _| {}),
        };
        let ctx = DetailContext {
            store: store.clone(),
            environment_id,
            reference,
            nav,
        };
        let summary = cx.new(|cx| SummaryTab::new(ctx.clone(), window, cx));
        let composer =
            cx.new(|cx| super::comment_form::PullRequestComposer::new(ctx.clone(), window, cx));
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.on_store(cx)),
            cx.subscribe(&summary, |this, _, event: &SummaryTabEvent, cx| {
                let SummaryTabEvent::Scrolled(top) = event;
                this.on_summary_scroll(*top, cx);
            }),
        ];
        Self {
            ctx,
            tab: DetailTab::Summary,
            mounted: vec![DetailTab::Summary],
            summary,
            timeline: None,
            code: None,
            guide: None,
            composer,
            timeline_order: TimelineOrder::Newest,
            code_commit: None,
            code_reveal: None,
            condensed: false,
            fold_height: DEFAULT_FOLD_HEIGHT,
            width: 640.,
            confirm: None,
            confirm_epoch: 0,
            merge_method: None,
            last_merge_method: PullRequestMergeMethod::Merge,
            title_edit: None,
            title_saving: false,
            toasts: ToastQueue::default(),
            seen_error: None,
            seen_guide_error: None,
            last_action: None,
            _subscriptions: subscriptions,
        }
    }

    /// The panel-level navigation a stack layer or linked pull request uses: the host of the panel
    /// supplies it once it knows how to open another pull request.
    pub fn set_select_pull_request(
        &mut self,
        select: super::stack_popover::SelectLayer,
        cx: &mut Context<Self>,
    ) {
        self.ctx.nav.select_pull_request = select;
        cx.notify();
    }

    pub fn set_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            // Each mounted tab remembers its own chrome state; only the summary reports scroll
            // here, so the others show the expanded header.
            if tab != DetailTab::Summary {
                self.condensed = false;
            }
        }
        if !self.mounted.contains(&tab) {
            self.mounted.push(tab);
        }
        cx.notify();
    }

    /// `openCommit`: scope the Code tab to the commit and show it.
    pub fn open_commit(&mut self, oid: String, cx: &mut Context<Self>) {
        self.code_commit = Some(oid.clone());
        if let Some(code) = &self.code {
            code.update(cx, |code, cx| code.set_commit(Some(oid), cx));
        }
        self.set_tab(DetailTab::Code, cx);
    }

    /// `openFileInCode`: the guide names a file; show it in the whole diff, not one commit's.
    pub fn open_file_in_code(&mut self, path: String, cx: &mut Context<Self>) {
        self.code_commit = None;
        self.code_reveal = Some(path);
        if let Some(code) = &self.code {
            let path = self.code_reveal.take().unwrap_or_default();
            code.update(cx, |code, cx| {
                code.set_commit(None, cx);
                code.reveal_file(&path, cx);
            });
        }
        self.set_tab(DetailTab::Code, cx);
    }

    fn on_store(&mut self, cx: &mut Context<Self>) {
        // A failed action says why, once (the toast `finishAction` raises).
        let error = self
            .ctx
            .snapshot(cx)
            .and_then(|snapshot| snapshot.action_error.clone());
        if error != self.seen_error {
            self.seen_error = error.clone();
            if let Some(error) = error {
                // Said as the thing that did not happen, with the host's own sentence under it, or
                // what to check when it said only that it refused.
                let (title, hint) = match self.last_action {
                    Some((action, update_method)) => (
                        action_failure_label(action),
                        if update_method == Some(PullRequestUpdateMethod::Rebase) {
                            UPDATE_BRANCH_REBASE_FAILURE_HINT
                        } else {
                            action_failure_hint(action)
                        },
                    ),
                    None => (
                        "Could not complete the action",
                        "The host refused it. Check that you have write access to this repository.",
                    ),
                };
                self.push_toast(
                    ToastKind::Error,
                    title,
                    Some(readable_failure(Some(&error), hint)),
                    cx,
                );
            }
        }
        // A guide that could not be started says why, once, and nothing is left to wait for
        // (`useReviewGuideWriter`'s failure branch).
        let guide_error = self
            .ctx
            .snapshot(cx)
            .and_then(|snapshot| snapshot.guide_start_error.clone());
        if guide_error != self.seen_guide_error {
            self.seen_guide_error = guide_error.clone();
            if let Some(error) = guide_error {
                let key = super::guide_run_store::guide_run_key(
                    &self.ctx.environment_id,
                    &self.ctx.reference,
                );
                super::guide_run_store::GuideRunStore::global(cx)
                    .update(cx, |runs, cx| runs.settle(&key, cx));
                self.push_toast(
                    ToastKind::Error,
                    "Could not start a review guide",
                    Some(readable_failure(
                        Some(&error),
                        "Check the review guide model in Settings → Source Control.",
                    )),
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn on_summary_scroll(&mut self, top: f32, cx: &mut Context<Self>) {
        if self.tab != DetailTab::Summary {
            return;
        }
        let condensed = if self.condensed {
            // The hard top reopens the chrome.
            !(top < REOPEN_AT && self.fold_height > 0.)
        } else {
            self.fold_height > 0. && top > self.fold_height + CONDENSE_MARGIN
        };
        if condensed != self.condensed {
            self.condensed = condensed;
            if condensed {
                // Refund after the fold so the content under the reader does not jump with the
                // header's height: the chrome that leaves is the body minus the condensed row.
                let delta = (self.fold_height - CONDENSED_ROW_HEIGHT).max(0.);
                let handle = self.summary.read(cx).scroll_handle().clone();
                let offset = handle.offset();
                let refunded = (offset.y + px(delta)).min(px(0.));
                handle.set_offset(gpui_kit::point(offset.x, refunded));
            }
            cx.notify();
        }
    }

    fn push_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        description: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.toasts
            .push(cx, |panel| &mut panel.toasts, kind, title, description);
    }

    fn scope_key(&self) -> String {
        format!(
            "{}:{}",
            self.ctx.environment_id.as_str(),
            pull_request_key(&self.ctx.reference)
        )
    }

    fn is_armed(&self, action: &str) -> bool {
        self.confirm
            .as_ref()
            .is_some_and(|armed| armed.is_armed(&self.scope_key(), action))
    }

    /// `inlineConfirm.props(action, onConfirm)`: the first press arms, a second press on the same
    /// control confirms, and three seconds without one disarms.
    fn confirm_click(&mut self, action: &str, element_key: &str, cx: &mut Context<Self>) -> bool {
        let scope = self.scope_key();
        let click = InlineActionConfirm::click(&mut self.confirm, &scope, action, element_key);
        self.confirm_epoch += 1;
        let epoch = self.confirm_epoch;
        if click == ConfirmClick::Armed {
            let panel = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(CONFIRM_FOR).await;
                panel
                    .update(cx, |panel, cx| {
                        if panel.confirm_epoch == epoch {
                            InlineActionConfirm::disarm(&mut panel.confirm);
                            cx.notify();
                        }
                    })
                    .ok();
            })
            .detach();
        }
        cx.notify();
        click == ConfirmClick::Confirmed
    }

    fn can_write(&self, cx: &App) -> bool {
        self.ctx
            .snapshot(cx)
            .is_some_and(|snapshot| snapshot.can_write)
    }

    fn pending_action(&self, cx: &App) -> Option<PullRequestAction> {
        self.ctx
            .snapshot(cx)
            .and_then(|snapshot| snapshot.pending_action)
    }

    /// `perform`: every control is disabled while one action runs.
    fn perform(
        &mut self,
        action: PullRequestAction,
        method: Option<PullRequestMergeMethod>,
        update_method: Option<PullRequestUpdateMethod>,
        cx: &mut Context<Self>,
    ) {
        if !self.can_write(cx) || self.pending_action(cx).is_some() {
            return;
        }
        let Some(source) = self.ctx.source(cx) else {
            return;
        };
        InlineActionConfirm::disarm(&mut self.confirm);
        self.last_action = Some((action, update_method));
        source.run_action(&self.ctx.reference, action, method, update_method, cx);
        if pull_request_action_needs_host_refresh(action) {
            source.refresh_detail(&self.ctx.reference, cx);
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(source) = self.ctx.source(cx) {
            source.refresh_detail(&self.ctx.reference, cx);
        }
    }

    fn copy(&mut self, value: String, label: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(value));
        self.push_toast(ToastKind::Success, format!("{label} copied"), None, cx);
    }

    fn open_external(&self, url: &str, cx: &mut App) {
        if let Some(source) = self.ctx.source(cx) {
            source.open_external(url, cx);
        } else {
            cx.open_url(url);
        }
    }

    fn begin_title_edit(&mut self, title: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title.to_owned()));
        let panel = cx.entity().downgrade();
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let text = state.read(cx).value().to_string();
                    this.save_title(text, cx);
                }
                let _ = &panel;
            },
        );
        self._subscriptions.push(subscription);
        input.update(cx, |input, cx| input.focus(window, cx));
        self.title_edit = Some(input);
        cx.notify();
    }

    /// `saveTitle`.
    fn save_title(&mut self, next: String, cx: &mut Context<Self>) {
        let title = next.trim().to_owned();
        let Some(Loadable::Ready(detail)) = self.ctx.snapshot(cx).map(|s| &s.detail) else {
            return;
        };
        if !self.can_write(cx) || self.title_saving {
            return;
        }
        if title.is_empty() || title == detail.title.as_str() {
            self.title_edit = None;
            cx.notify();
            return;
        }
        if let Some(source) = self.ctx.source(cx) {
            source.update_title(&self.ctx.reference, title, cx);
        }
        self.title_edit = None;
        cx.notify();
    }

    fn ensure_mounted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ctx = self.ctx.clone();
        match self.tab {
            DetailTab::Summary => {}
            DetailTab::Timeline if self.timeline.is_none() => {
                let timeline = cx.new(|cx| TimelineTab::new(ctx, window, cx));
                let order = self.timeline_order.as_comment_order();
                timeline.update(cx, |timeline, cx| timeline.set_order(order, cx));
                self.timeline = Some(timeline);
            }
            DetailTab::Code if self.code.is_none() => {
                let code = cx.new(|cx| CodeTab::new(ctx, window, cx));
                let commit = self.code_commit.clone();
                let reveal = self.code_reveal.take();
                code.update(cx, |code, cx| {
                    if commit.is_some() {
                        code.set_commit(commit, cx);
                    }
                    if let Some(path) = reveal {
                        code.reveal_file(&path, cx);
                    }
                });
                self.code = Some(code);
            }
            DetailTab::Guide if self.guide.is_none() => {
                let guide = cx.new(|cx| GuideTab::new(ctx, window, cx));
                // The guide's last stop asks for the review: open the composer on it.
                let composer = self.composer.downgrade();
                guide.update(cx, |guide, cx| {
                    guide.set_on_finish(
                        Some(std::rc::Rc::new(move |cx: &mut App| {
                            composer
                                .update(cx, |composer, cx| composer.open_review(cx))
                                .ok();
                        })),
                        cx,
                    )
                });
                self.guide = Some(guide);
            }
            _ => {}
        }
    }
}

/// The glyph beside a menu row.
fn menu_icon(icon: IconName) -> gpui_kit::component::Icon {
    gpui_kit::component::Icon::new(icon)
}

fn header_icon_button(id: &'static str, icon: IconName, label: &'static str) -> Button {
    Button::new(id)
        .size(ButtonSize::IconXs)
        .variant(ButtonVariant::GhostMuted)
        .icon(icon)
        .tooltip(Tooltip::new(label))
}

impl Render for PullRequestDetailPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_mounted(window, cx);
        let theme = cx.app_theme().clone();
        let weak = cx.entity().downgrade();
        let snapshot = self.ctx.snapshot(cx);
        let detail_load = match std::env::var("TURIN_PR_OPEN").as_deref() {
            // Dev hooks for screenshots: the loading ghost and the failed read.
            Ok("detail-ghost") => Loadable::Pending,
            Ok("detail-error") => Loadable::Failed(
                "gh: could not reach api.github.com. Check your connection.".to_owned(),
            ),
            _ => snapshot
                .map(|s| s.detail.clone())
                .unwrap_or(Loadable::Pending),
        };
        let root = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .w_full()
            .bg(theme.background)
            .on_children_prepainted({
                let weak = weak.clone();
                move |bounds, _, cx| {
                    let Some(first) = bounds.first() else {
                        return;
                    };
                    let width: f32 = first.size.width.into();
                    weak.update(cx, |panel, cx| {
                        if (panel.width - width).abs() > 0.5 {
                            panel.width = width;
                            cx.notify();
                        }
                    })
                    .ok();
                }
            });
        match detail_load {
            Loadable::Pending => {
                let seed = self.list_seed(cx);
                root.child(detail_ghost(seed, self.tab, self.width, cx))
                    .into_any_element()
            }
            Loadable::Failed(error) => {
                let weak = weak.clone();
                // `isPullRequestNotFound`: the host says nothing is there at that number.
                let not_found = error.to_lowercase().contains("not found");
                let mut unavailable = if not_found {
                    PullRequestsUnavailableState::new(
                        "It may be an issue rather than a pull request, or this account can't see it.",
                    )
                    .title(format!("Pull request #{} not found", self.ctx.reference.number.get()))
                } else {
                    PullRequestsUnavailableState::new(error)
                        .title("Could not load this pull request")
                };
                unavailable.on_retry = Some(Rc::new(move |_, _, cx| {
                    weak.update(cx, |panel, cx| panel.refresh(cx)).ok();
                }));
                root.child(div().flex().flex_1().min_h_0().child(unavailable))
                    .into_any_element()
            }
            Loadable::Ready(detail) => self.render_ready(root, detail, window, cx),
        }
    }
}

impl PullRequestDetailPanel {
    /// What the list already knows of the pull request, shown while the detail arrives.
    fn list_seed(
        &self,
        cx: &App,
    ) -> Option<super::pull_request_list_logic::EnvironmentPullRequestEntry> {
        let store = self.ctx.store.read(cx);
        store
            .entries()
            .iter()
            .find(|entry| {
                entry.number == self.ctx.reference.number
                    && entry
                        .repository
                        .as_str()
                        .eq_ignore_ascii_case(self.ctx.reference.repository.as_str())
            })
            .cloned()
    }

    fn render_ready(
        &mut self,
        root: gpui_kit::Div,
        detail: PullRequestDetailView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = window;
        let theme = cx.app_theme().clone();
        let weak = cx.entity().downgrade();
        let snapshot = self.ctx.snapshot(cx);
        let can_write = snapshot.is_some_and(|s| s.can_write);
        let pending_action = snapshot.and_then(|s| s.pending_action);
        let native_stack = self.ctx.store.read(cx).stack_view(&self.ctx.reference).data;
        let activity_pending = snapshot.is_some_and(|s| s.activity_pending);
        let refreshing = false;

        let state = HeaderState::resolve(
            &detail,
            can_write,
            self.merge_method,
            None,
            self.last_merge_method,
            true,
        );
        let presentation = resolve_pull_request_state(detail.state, detail.is_draft);
        let state_color = tone_color(presentation.tone_class_name, &theme);
        let host_label = SOURCE_CONTROL_CLIENTS
            .get(Some(detail.provider.as_str()))
            .label;
        let open_label = open_on_host_label(Some(host_label));
        let repository_url = repository_url(detail.url.as_str());
        // `PullRequestMarkdownContext.Provider`: the bodies beneath link into this repository.
        super::comment_body::provide_pull_request_markdown_context(cx, repository_url.clone());
        let narrow = self.width < HEADER_LABELS_MIN_WIDTH;
        let reference = self.ctx.reference.clone();

        // ── Top row: repository and number, then the controls ──────────────────────────────────
        let number_button = {
            let url = detail.url.as_str().to_owned();
            let weak = weak.clone();
            div()
                .id("pr-number-link")
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(2.))
                .cursor(CursorStyle::PointingHand)
                .font_weight(FontWeight::MEDIUM)
                .text_color(state_color)
                .hover(|style| style.underline())
                .child(format!("#{}", detail.number.get()))
                .child(Icon::new(IconName::ExternalLink).size(rems(0.625)))
                .tooltip({
                    let label = open_label.clone();
                    move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                })
                .on_click(move |_, _, cx| {
                    let url = url.clone();
                    weak.update(cx, |panel, cx| panel.open_external(&url, cx))
                        .ok();
                })
        };
        let repository_label = {
            let repository = SharedString::from(detail.repository.as_str().to_owned());
            match repository_url.clone() {
                Some(url) => {
                    let weak = weak.clone();
                    let tooltip = format!("Open {} repository", detail.repository.as_str());
                    div()
                        .id("pr-repository-link")
                        .min_w_0()
                        .truncate()
                        .cursor(CursorStyle::PointingHand)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .hover({
                            let foreground = theme.foreground;
                            move |style| style.text_color(foreground).underline()
                        })
                        .child(repository)
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(move |_, _, cx| {
                            let url = url.clone();
                            weak.update(cx, |panel, cx| panel.open_external(&url, cx))
                                .ok();
                        })
                        .into_any_element()
                }
                None => div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child(repository)
                    .into_any_element(),
            }
        };
        let identity = if self.condensed {
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .child(number_button)
                .child(
                    div()
                        .id("pr-condensed-title")
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(SharedString::from(detail.title.as_str().to_owned()))
                        .tooltip({
                            let title = detail.title.as_str().to_owned();
                            move |window, cx| Tooltip::new(title.clone()).build(window, cx)
                        }),
                )
        } else {
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .child(repository_label)
                .child(number_button)
        };

        // ── Title and author lines ─────────────────────────────────────────────────────────────
        let can_edit_title =
            can_write && can_edit_pull_request_change_request(&editing_subject(&detail, can_write));
        let title_element: AnyElement = match self.title_edit.clone() {
            None => {
                let weak_edit = weak.clone();
                let title = detail.title.as_str().to_owned();
                title_row()
                    .id("pr-title-row")
                    .group("pr-title-row")
                    .gap_1()
                    .child(
                        div()
                            .id("pr-title")
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(TEXT_BASE.size)
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(rems(1.375))
                            .child(SharedString::from(title.clone()))
                            .tooltip({
                                let title = title.clone();
                                move |window, cx| Tooltip::new(title.clone()).build(window, cx)
                            }),
                    )
                    .when(can_edit_title, |this| {
                        this.child(
                            EditButton::new("pr-edit-title", "pr-title-row", "Edit title")
                                .on_click(move |window, cx| {
                                    let title = title.clone();
                                    weak_edit
                                        .update(cx, |panel, cx| {
                                            panel.begin_title_edit(&title, window, cx)
                                        })
                                        .ok();
                                }),
                        )
                    })
                    .into_any_element()
            }
            Some(input) => {
                let weak_cancel = weak.clone();
                let weak_save = weak.clone();
                let save_input = input.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Input::new(&input))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("pr-title-cancel")
                                    .size(ButtonSize::Xs)
                                    .variant(ButtonVariant::Ghost)
                                    .disabled(self.title_saving)
                                    .label("Cancel")
                                    .on_click(move |_, _, cx| {
                                        weak_cancel
                                            .update(cx, |panel, cx| {
                                                panel.title_edit = None;
                                                cx.notify();
                                            })
                                            .ok();
                                    }),
                            )
                            .child(
                                Button::new("pr-title-save")
                                    .size(ButtonSize::Xs)
                                    .variant(ButtonVariant::Outline)
                                    .disabled(!can_write || self.title_saving)
                                    .label(if self.title_saving {
                                        "Saving..."
                                    } else {
                                        "Save"
                                    })
                                    .on_click(move |_, _, cx| {
                                        let text = save_input.read(cx).value().to_string();
                                        weak_save
                                            .update(cx, |panel, cx| panel.save_title(text, cx))
                                            .ok();
                                    }),
                            ),
                    )
                    .into_any_element()
            }
        };

        // ── Base and head branches ─────────────────────────────────────────────────────────────
        let stacked = native_stack.as_ref().is_some_and(|stack| {
            stack
                .layers
                .first()
                .is_some_and(|layer| layer.number != detail.number)
        });
        let freshness = resolve_base_freshness(&core_detail(&detail));
        let base_element = self.render_base_branch(
            &detail,
            stacked,
            freshness.as_ref(),
            pending_action.is_some(),
            cx,
        );
        let head_element = PullRequestCopyableCode::new(
            SharedString::from(format!("pr-head-{}", detail.head_branch.as_str())),
            detail.head_branch.as_str().to_owned(),
            "Copy pull request branch",
        );
        let checkout_command = pull_request_checkout_command(
            &detail.provider,
            detail.number.get(),
            detail.head_branch.as_str(),
            detail
                .head_repository_name_with_owner
                .as_ref()
                .and_then(|name| name.as_ref())
                .map(|name| name.as_str()),
            repository_url.as_deref(),
        );
        let checkout_element = checkout_command.map(|command| {
            PullRequestCopyableCode::new(
                SharedString::from(format!("pr-checkout-{command}")),
                command,
                "Copy checkout command",
            )
            .ml_auto()
            .into_any_element()
        });
        let _ = loading_pull_request_checkout_command;

        let body = header_body(
            HeaderBody {
                title: title_element,
                author: PullRequestActorLabel::new(detail.author.clone()).into_any_element(),
                updated: div()
                    .child(format!(
                        "updated {}",
                        super::relative_time::format_relative_time_label(
                            detail.updated_at.as_str()
                        )
                    ))
                    .into_any_element(),
                checkout: checkout_element,
                base: base_element,
                head: head_element.into_any_element(),
                files: div()
                    .child(files_label(detail.changed_files))
                    .into_any_element(),
                diff_stat: PullRequestDiffStat::new(detail.additions, detail.deletions)
                    .mono()
                    .into_any_element(),
            },
            &theme,
        );

        // ── Condensed row ──────────────────────────────────────────────────────────────────────
        let condensed_row = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_2()
            .px_4()
            .pb_2()
            .pt_1()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.375))
                    .overflow_hidden()
                    .child(PullRequestActorLabel::new(detail.author.clone()).avatar_only())
                    .child(div().flex_shrink_0().child(
                        super::relative_time::format_relative_time_label(
                            detail.updated_at.as_str(),
                        ),
                    )),
            )
            .child(
                div()
                    .h(rems(0.75))
                    .w(px(1.))
                    .flex_shrink_0()
                    .bg(alpha(theme.border, 0.7)),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.375))
                    .font_family(theme.fonts.mono.clone())
                    .text_size(crate::theme::tokens::TEXT_2XS.size)
                    .text_color(alpha(theme.muted_foreground, 0.65))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(((self.width - 32. - 190.) * 0.4).max(48.)))
                            .truncate()
                            .child(SharedString::from(detail.base_branch.as_str().to_owned())),
                    )
                    .child(
                        Icon::new(IconName::ArrowLeft)
                            .size(rems(0.75))
                            .color(alpha(theme.muted_foreground, 0.6)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(SharedString::from(detail.head_branch.as_str().to_owned())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .ml_auto()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .text_size(crate::theme::tokens::TEXT_2XS.size)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(Icon::new(IconName::FileDiff).size(rems(0.75)))
                            .child(format_count(detail.changed_files)),
                    )
                    .child(PullRequestDiffStat::new(detail.additions, detail.deletions).mono()),
            );

        // ── Tab bar and its trailing slot ──────────────────────────────────────────────────────
        let tabs = visible_tabs(detail.capabilities.diff, REVIEW_GUIDES_ENABLED);
        let trailing =
            self.render_tab_trailing(&detail, &state, activity_pending, pending_action, cx);
        let tab_bar_element = tab_bar(
            &tabs,
            self.tab,
            Some(Rc::new({
                let weak = weak.clone();
                move |tab, _, _, cx| {
                    weak.update(cx, |panel, cx| panel.set_tab(tab, cx)).ok();
                }
            })),
            trailing,
            &theme,
        );

        // ── Right cell actions ─────────────────────────────────────────────────────────────────
        let actions = self.render_actions(
            &detail,
            &state,
            native_stack.as_ref(),
            pending_action,
            refreshing,
            narrow,
            &open_label,
            cx,
        );

        let top_row = div()
            .flex()
            .items_start()
            .gap_x_2()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h(rems(1.75))
                    .pl_4()
                    .items_center()
                    .overflow_hidden()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(identity),
            )
            .child(actions);

        let header = div()
            .flex()
            .flex_col()
            .min_w_0()
            .flex_shrink_0()
            .border_b_1()
            .border_color(alpha(theme.border, 0.6))
            .child(top_row)
            .child(if self.condensed {
                condensed_row.into_any_element()
            } else {
                let weak = weak.clone();
                div()
                    .on_children_prepainted(move |bounds, _, cx| {
                        let Some(first) = bounds.first() else {
                            return;
                        };
                        let height: f32 = first.size.height.into();
                        weak.update(cx, |panel, _| panel.fold_height = height).ok();
                    })
                    .child(body)
                    .into_any_element()
            })
            .child(tab_bar_element);

        // ── Content ────────────────────────────────────────────────────────────────────────────
        let content: AnyElement = match self.tab {
            DetailTab::Summary => self.summary.clone().into_any_element(),
            DetailTab::Timeline => {
                if activity_pending {
                    timeline_ghost(6, cx).into_any_element()
                } else if let Some(timeline) = self.timeline.clone() {
                    timeline.into_any_element()
                } else {
                    div().into_any_element()
                }
            }
            DetailTab::Code => self
                .code
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            DetailTab::Guide => self
                .guide
                .clone()
                .map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
        };
        let _ = &reference;
        root.child(header)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(content)
                    // `absolute right-4 bottom-3`: the composer floats over every tab.
                    .child(
                        div()
                            .absolute()
                            .right(rems(1.))
                            .bottom(rems(0.75))
                            .child(self.composer.clone()),
                    ),
            )
            .children(render_toasts(&self.toasts, cx))
            .into_any_element()
    }

    /// The base branch of the branch line, with the stack glyph for a stacked pull request and
    /// the out-of-date warning when the host says the branch fell behind.
    fn render_base_branch(
        &self,
        detail: &PullRequestDetailView,
        stacked: bool,
        freshness: Option<&super::pull_request_detail_logic::BaseFreshness>,
        action_pending: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let base = SharedString::from(detail.base_branch.as_str().to_owned());
        let name = div()
            .flex()
            .min_w_0()
            .child(div().min_w_0().truncate().child(base.clone()));
        let weak = cx.entity().downgrade();
        let max_w = px(((self.width - 32. - 150.) * 0.4).max(64.));
        match freshness {
            Some(freshness) => {
                let behind = match freshness.behind_by {
                    None => String::new(),
                    Some(count) => format!(
                        " by {} {}",
                        format_count(count),
                        if count == 1 { "commit" } else { "commits" }
                    ),
                };
                let summary = format!(
                    "This branch is out-of-date with {}{behind}.",
                    detail.base_branch.as_str()
                );
                super::checks_popover::freshness_warning(
                    super::checks_popover::FreshnessWarning {
                        summary,
                        methods: freshness.methods.clone(),
                        pending: action_pending,
                        stacked,
                        name: name.into_any_element(),
                        max_width: max_w,
                        on_update: Rc::new(move |method, _, cx| {
                            weak.update(cx, |panel, cx| {
                                panel.perform(
                                    PullRequestAction::UpdateBranch,
                                    None,
                                    Some(method),
                                    cx,
                                )
                            })
                            .ok();
                        }),
                    },
                    &theme,
                )
            }
            None => {
                let tooltip = if stacked {
                    format!("Stacked on {}", detail.base_branch.as_str())
                } else {
                    detail.base_branch.as_str().to_owned()
                };
                div()
                    .id("pr-base-branch")
                    .flex()
                    .min_w_0()
                    .max_w(max_w)
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .when(stacked, |this| {
                        this.child(Icon::new(IconName::Layers).size(rems(0.75)))
                    })
                    .child(name)
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .into_any_element()
            }
        }
    }

    /// What rides beside the tabs: the checks line on Summary, the counts and order on Timeline.
    fn render_tab_trailing(
        &self,
        detail: &PullRequestDetailView,
        state: &HeaderState,
        activity_pending: bool,
        pending_action: Option<PullRequestAction>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        let weak = cx.entity().downgrade();
        match self.tab {
            DetailTab::Summary => {
                if state.shows_approve_workflows {
                    let armed = self.is_armed("approve-workflows");
                    let label = if pending_action == Some(PullRequestAction::ApproveWorkflows) {
                        "Approving..."
                    } else if armed {
                        "Confirm approve?"
                    } else {
                        "Approve workflows to run"
                    };
                    return Some(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .ml_auto()
                            .child(
                                Button::new("pr-approve-workflows")
                                    .size(ButtonSize::Xs)
                                    .variant(ButtonVariant::WarningOutline)
                                    .disabled(pending_action.is_some())
                                    .icon(IconName::Play)
                                    .label(label)
                                    .tooltip(Tooltip::new(label))
                                    .on_click(move |_, _, cx| {
                                        weak.update(cx, |panel, cx| {
                                            if panel.confirm_click(
                                                "approve-workflows",
                                                "button:approve-workflows",
                                                cx,
                                            ) {
                                                panel.perform(
                                                    PullRequestAction::ApproveWorkflows,
                                                    None,
                                                    None,
                                                    cx,
                                                );
                                            }
                                        })
                                        .ok();
                                    }),
                            )
                            .into_any_element(),
                    );
                }
                let icon: AnyElement = match state.checks_state {
                    Some(checks_state) => PullRequestChecksPopover::new(
                        "pr-header-checks",
                        detail.checks.clone(),
                        checks_state,
                    )
                    .into_any_element(),
                    None => Icon::new(IconName::CircleDot)
                        .size(rems(0.875))
                        .into_any_element(),
                };
                Some(
                    checks_status_line(icon, state.checks_summary.clone(), theme.muted_foreground)
                        .into_any_element(),
                )
            }
            DetailTab::Timeline => {
                let pending = activity_pending;
                let count_text = |value: usize| {
                    if pending {
                        "…".to_owned()
                    } else {
                        format_count(value as u64)
                    }
                };
                let faded = if pending { 0.35 } else { 1. };
                let muted = theme.muted_foreground;
                let counts = super::presentation_view::pull_request_meta_line(
                    [
                        Some(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(Icon::new(IconName::MessageSquare).size(rems(0.75)))
                                .child(count_text(detail.comment_count as usize))
                                .into_any_element(),
                        ),
                        Some(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(Icon::new(IconName::GitCommitHorizontal).size(rems(0.75)))
                                .child(count_text(detail.commits.len()))
                                .into_any_element(),
                        ),
                        (state.approval_count > 0).then(|| {
                            let tone = tone_color(
                                pull_request_review_outcome_tone_class_name(
                                    PullRequestReviewOutcome::Approved,
                                ),
                                &theme,
                            );
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_color(tone)
                                .child(
                                    Icon::new(IconName::CircleCheck)
                                        .size(rems(0.75))
                                        .color(tone),
                                )
                                .child(format_count(state.approval_count as u64))
                                .into_any_element()
                        }),
                    ],
                    &theme,
                )
                .whitespace_nowrap()
                .text_size(crate::theme::tokens::TEXT_2XS.size)
                .opacity(faded);
                let order = self.timeline_order;
                Some(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .ml_auto()
                        .items_center()
                        .gap_2()
                        .text_size(TEXT_XS.size)
                        .text_color(muted)
                        .child(counts)
                        .child(
                            Button::new("pr-timeline-order")
                                .size(ButtonSize::Xs)
                                .variant(ButtonVariant::GhostMuted)
                                .icon(IconName::ArrowDownUp)
                                .icon_size(rems(0.75))
                                .label(match order {
                                    TimelineOrder::Newest => "Newest first",
                                    TimelineOrder::Oldest => "Oldest first",
                                })
                                .on_click(move |_, _, cx| {
                                    weak.update(cx, |panel, cx| {
                                        panel.timeline_order = panel.timeline_order.toggled();
                                        if let Some(timeline) = &panel.timeline {
                                            let order = panel.timeline_order.as_comment_order();
                                            timeline.update(cx, |timeline, cx| {
                                                timeline.set_order(order, cx)
                                            });
                                        }
                                        cx.notify();
                                    })
                                    .ok();
                                }),
                        )
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }

    /// The right cell: stack menu, merge controls, the "more" menu.
    #[allow(clippy::too_many_arguments)]
    fn render_actions(
        &self,
        detail: &PullRequestDetailView,
        state: &HeaderState,
        native_stack: Option<&turin_contracts::pull_request::PullRequestStack>,
        pending_action: Option<PullRequestAction>,
        refreshing: bool,
        narrow: bool,
        open_label: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let weak = cx.entity().downgrade();
        let action_pending = pending_action.is_some();
        let presentation = resolve_pull_request_state(detail.state, detail.is_draft);
        let state_color = tone_color(presentation.tone_class_name, &theme);
        let mut row = div()
            .flex()
            .flex_shrink_0()
            .h(rems(1.75))
            .mr_4()
            .items_center()
            .justify_end()
            .gap_1();

        if let Some(stack) = native_stack {
            row = row.child(super::stack_popover::stack_menu(&self.ctx, stack, None, cx));
        }

        let armed_badge = |label: String| {
            Badge::new()
                .size(BadgeSize::Control)
                .variant(BadgeVariant::Info)
                .child(Icon::new(IconName::GitMerge).size(rems(0.875)))
                .when(!narrow, |this| this.child(label.clone()))
                .into_any_element()
        };
        if state.auto_merge_armed
            && state.primary != Some(PullRequestPrimaryControl::AutoMergeArmed)
        {
            row = row.child(armed_badge(state.armed_auto_merge_label.clone()));
        }
        match state.primary {
            Some(PullRequestPrimaryControl::Resolve) => {
                row = row.child(
                    Button::new("pr-resolve-conflicts")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::DestructiveOutline)
                        .disabled(true)
                        .icon(IconName::TriangleAlert)
                        .when(!narrow, |this| this.label("Resolve conflicts"))
                        .tooltip(Tooltip::new("Resolve conflicts")),
                );
            }
            Some(PullRequestPrimaryControl::Ready) => {
                let weak = weak.clone();
                row = row.child(
                    Button::new("pr-ready")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Default)
                        .disabled(action_pending)
                        .icon(IconName::GitPullRequestArrow)
                        .when(!narrow, |this| this.label("Ready for review"))
                        .tooltip(Tooltip::new("Ready for review"))
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |panel, cx| {
                                panel.perform(PullRequestAction::Ready, None, None, cx)
                            })
                            .ok();
                        }),
                );
            }
            Some(
                control @ (PullRequestPrimaryControl::EnableAutoMerge
                | PullRequestPrimaryControl::Merge),
            ) => {
                let (action, key, idle_label, pending_label, confirm_label) =
                    if control == PullRequestPrimaryControl::Merge {
                        (
                            PullRequestAction::Merge,
                            "merge",
                            state.selected_merge_method_label.to_owned(),
                            "Merging...",
                            "Confirm merge?",
                        )
                    } else {
                        (
                            PullRequestAction::EnableAutoMerge,
                            "enable-auto-merge",
                            state.pending_auto_merge_label.clone(),
                            "Enabling...",
                            "Confirm auto-merge?",
                        )
                    };
                let armed = self.is_armed(key);
                let label = if pending_action == Some(action) {
                    pending_label.to_owned()
                } else if armed {
                    confirm_label.to_owned()
                } else {
                    idle_label
                };
                let method = state.selected_merge_method;
                let weak = weak.clone();
                row = row.child(
                    Button::new(SharedString::from(format!("pr-primary-{key}")))
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Default)
                        .disabled(action_pending)
                        .icon(if armed {
                            IconName::Check
                        } else {
                            IconName::GitMerge
                        })
                        .when(!narrow, |this| this.label(label.clone()))
                        .tooltip(Tooltip::new(label.clone()))
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |panel, cx| {
                                if panel.confirm_click(key, &format!("button:{key}"), cx) {
                                    panel.perform(action, Some(method), None, cx);
                                }
                            })
                            .ok();
                        }),
                );
            }
            Some(PullRequestPrimaryControl::AutoMergeArmed) => {
                row = row.child(armed_badge(state.armed_auto_merge_label.clone()));
            }
            Some(PullRequestPrimaryControl::Merged) | Some(PullRequestPrimaryControl::Closed) => {
                row = row.child(
                    Badge::new()
                        .size(BadgeSize::Control)
                        .variant(BadgeVariant::Outline)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_color(state_color)
                                .child(
                                    PullRequestStateGlyph::new(detail.state, detail.is_draft)
                                        .size(rems(0.875)),
                                )
                                .child(presentation.label),
                        ),
                );
            }
            None => {}
        }

        row.child(self.render_more_menu(detail, state, pending_action, refreshing, open_label, cx))
            .into_any_element()
    }

    /// The "more pull request actions" menu.
    fn render_more_menu(
        &self,
        detail: &PullRequestDetailView,
        state: &HeaderState,
        pending_action: Option<PullRequestAction>,
        refreshing: bool,
        open_label: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        let action_pending = pending_action.is_some();
        let can_write = self.can_write(cx);
        let can = |action: PullRequestAction| {
            can_write
                && detail.capabilities.actions.contains(&action)
                && detail.viewer_permissions.actions.contains(&action)
        };
        let open = detail.state == PullRequestState::Open;
        let model = MoreMenu {
            weak: weak.clone(),
            url: detail.url.as_str().to_owned(),
            number: detail.number.get(),
            open_label: open_label.to_owned(),
            state: state.clone(),
            is_draft: detail.is_draft,
            pr_state: detail.state,
            can_close: open && can(PullRequestAction::Close),
            can_reopen: detail.state == PullRequestState::Closed && can(PullRequestAction::Reopen),
            can_revert: detail.state == PullRequestState::Merged && can(PullRequestAction::Revert),
            action_pending,
            refreshing,
            armed_merge: self.is_armed("merge"),
            armed_auto_merge: self.is_armed("enable-auto-merge"),
            armed_close: self.is_armed("close"),
            armed_revert: self.is_armed("revert"),
            guides: REVIEW_GUIDES_ENABLED && can_write,
        };
        let button = Button::new("pr-more-menu")
            .size(ButtonSize::IconXs)
            .variant(ButtonVariant::GhostMuted)
            .tooltip(Tooltip::new(if refreshing {
                "Refreshing pull request"
            } else {
                "More pull request actions"
            }));
        let button = if refreshing {
            button.child(Spinner::new("pr-more-refreshing").size(rems(1.)))
        } else {
            button.icon(IconName::Ellipsis)
        };
        super::dropdown::dropdown(
            "pr-more-menu",
            Anchor::TopRight,
            button,
            move |menu, _, _| model.build(menu),
        )
        .into_any_element()
    }
}

/// Everything the "more" menu reads, captured when it opens.
struct MoreMenu {
    weak: WeakEntity<PullRequestDetailPanel>,
    url: String,
    number: u64,
    open_label: String,
    state: HeaderState,
    is_draft: bool,
    pr_state: PullRequestState,
    can_close: bool,
    can_reopen: bool,
    can_revert: bool,
    action_pending: bool,
    refreshing: bool,
    armed_merge: bool,
    armed_auto_merge: bool,
    armed_close: bool,
    armed_revert: bool,
    guides: bool,
}

impl MoreMenu {
    fn item(
        &self,
        label: impl Into<SharedString>,
        icon: IconName,
        disabled: bool,
        run: impl Fn(&mut PullRequestDetailPanel, &mut Context<PullRequestDetailPanel>) + 'static,
    ) -> PopupMenuItem {
        let weak = self.weak.clone();
        PopupMenuItem::new(label)
            .icon(menu_icon(icon))
            .disabled(disabled)
            .on_click(move |_, _, cx| {
                weak.update(cx, |panel, cx| run(panel, cx)).ok();
            })
    }

    fn confirm_item(
        &self,
        key: &'static str,
        armed: bool,
        idle: &str,
        confirm: &str,
        icon: IconName,
        run: (PullRequestAction, Option<PullRequestMergeMethod>),
    ) -> PopupMenuItem {
        let (action, method) = run;
        self.item(
            if armed {
                confirm.to_owned()
            } else {
                idle.to_owned()
            },
            icon,
            self.action_pending,
            move |panel, cx| {
                if panel.confirm_click(key, &format!("menu:{key}"), cx) {
                    panel.perform(action, method, None, cx);
                }
            },
        )
    }

    fn build(&self, mut menu: PopupMenu) -> PopupMenu {
        let url = self.url.clone();
        let number = self.number;
        let state = &self.state;
        // Refresh, then the guide, as the hand-offs (ask, explain, fix) need a thread.
        menu = menu.item(self.item(
            "Refresh",
            IconName::RefreshCw,
            self.refreshing,
            |panel, cx| panel.refresh(cx),
        ));
        if self.guides {
            menu = menu.item(
                self.item("Write guide", IconName::Sparkles, false, |panel, cx| {
                    if let Some(source) = panel.ctx.source(cx) {
                        source.generate_guide(&panel.ctx.reference, cx);
                    }
                    panel.set_tab(DetailTab::Guide, cx);
                }),
            );
        }
        menu = menu.separator();
        if self.pr_state == PullRequestState::Open {
            if state.shows_draft_toggle {
                let is_draft = self.is_draft;
                menu = menu.item(self.item(
                    if is_draft {
                        "Ready for review"
                    } else {
                        "Convert to draft"
                    },
                    if is_draft {
                        IconName::GitPullRequestArrow
                    } else {
                        IconName::GitPullRequestDraft
                    },
                    self.action_pending,
                    move |panel, cx| {
                        panel.perform(
                            if is_draft {
                                PullRequestAction::Ready
                            } else {
                                PullRequestAction::Draft
                            },
                            None,
                            None,
                            cx,
                        )
                    },
                ));
            }
            if state.shows_merge_now {
                menu = menu.item(self.confirm_item(
                    "merge",
                    self.armed_merge,
                    "Merge now",
                    "Confirm merge?",
                    IconName::GitMerge,
                    (PullRequestAction::Merge, Some(state.selected_merge_method)),
                ));
            }
            if state.auto_merge_armed && state.shows_auto_merge {
                menu = menu.item(self.item(
                    "Disable auto-merge",
                    IconName::GitMerge,
                    self.action_pending,
                    |panel, cx| panel.perform(PullRequestAction::DisableAutoMerge, None, None, cx),
                ));
            } else if state.shows_auto_merge {
                menu = menu.item(self.confirm_item(
                    "enable-auto-merge",
                    self.armed_auto_merge,
                    "Enable auto-merge",
                    "Confirm auto-merge?",
                    IconName::GitMerge,
                    (
                        PullRequestAction::EnableAutoMerge,
                        Some(state.selected_merge_method),
                    ),
                ));
            }
            if state.shows_merge_methods {
                if state.shows_draft_toggle || state.shows_merge_now || state.shows_auto_merge {
                    menu = menu.separator();
                }
                for method in state.allowed_merge_methods.clone() {
                    let weak = self.weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(merge_method_label(method))
                            .checked(method == state.selected_merge_method)
                            .disabled(self.action_pending)
                            .on_click(move |_, _, cx| {
                                weak.update(cx, |panel, cx| {
                                    panel.merge_method = Some(method);
                                    panel.last_merge_method = method;
                                    cx.notify();
                                })
                                .ok();
                            }),
                    );
                }
            }
            if pull_request_action_menu_has_group(
                state.shows_draft_toggle,
                state.shows_auto_merge || state.shows_merge_now,
                state.shows_merge_methods,
            ) {
                menu = menu.separator();
            }
        }
        let open_url = url.clone();
        menu = menu.item(self.item(
            self.open_label.clone(),
            IconName::ArrowUpRight,
            false,
            move |panel, cx| panel.open_external(&open_url, cx),
        ));
        let copy_url = url.clone();
        menu = menu.item(
            self.item("Copy link", IconName::Link, false, move |panel, cx| {
                panel.copy(copy_url.clone(), "PR link", cx)
            }),
        );
        menu = menu.item(
            self.item("Copy PR number", IconName::Copy, false, move |panel, cx| {
                panel.copy(format!("#{number}"), "PR number", cx)
            }),
        );
        if self.can_close {
            menu = menu.separator().item(self.confirm_item(
                "close",
                self.armed_close,
                "Close pull request",
                "Confirm close?",
                IconName::GitPullRequestClosed,
                (PullRequestAction::Close, None),
            ));
        } else if self.can_reopen {
            menu = menu.separator().item(self.item(
                "Reopen pull request",
                IconName::GitPullRequestArrow,
                self.action_pending,
                |panel, cx| panel.perform(PullRequestAction::Reopen, None, None, cx),
            ));
        } else if self.can_revert {
            menu = menu.separator().item(self.confirm_item(
                "revert",
                self.armed_revert,
                "Revert changes",
                "Confirm revert?",
                IconName::RotateCcw,
                (PullRequestAction::Revert, None),
            ));
        }
        menu
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_request::fixtures;

    fn detail() -> PullRequestDetailView {
        fixtures::detail_view()
    }

    fn state_of(detail: &PullRequestDetailView, can_write: bool) -> HeaderState {
        HeaderState::resolve(
            detail,
            can_write,
            None,
            None,
            PullRequestMergeMethod::Merge,
            true,
        )
    }

    #[test]
    fn finds_the_repository_page_of_a_pull_request_url() {
        assert_eq!(
            repository_url("https://github.com/acme/turin/pull/482"),
            Some("https://github.com/acme/turin".to_owned())
        );
        assert_eq!(
            repository_url("https://github.com/acme/turin/issues/4"),
            None
        );
        assert_eq!(repository_url("https://github.com/acme/turin/pull/"), None);
    }

    #[test]
    fn says_what_did_not_happen_for_every_action() {
        assert_eq!(
            action_failure_label(PullRequestAction::Merge),
            "Could not merge this pull request"
        );
        assert!(action_failure_hint(PullRequestAction::EnableAutoMerge).contains("auto-merge"));
        assert!(UPDATE_BRANCH_REBASE_FAILURE_HINT.contains("rebase"));
    }

    #[test]
    fn a_reader_who_cannot_write_gets_no_merge_controls() {
        let detail = detail();
        let state = state_of(&detail, false);
        assert!(!state.shows_draft_toggle);
        assert!(!state.shows_merge_now);
        assert!(!state.shows_merge_methods);
        assert_ne!(state.primary, Some(PullRequestPrimaryControl::Merge));
    }

    #[test]
    fn a_merged_pull_request_shows_its_state_in_the_merge_slot() {
        let mut detail = detail();
        detail.state = PullRequestState::Merged;
        assert_eq!(
            state_of(&detail, true).primary,
            Some(PullRequestPrimaryControl::Merged)
        );
        detail.state = PullRequestState::Closed;
        assert_eq!(
            state_of(&detail, true).primary,
            Some(PullRequestPrimaryControl::Closed)
        );
    }

    #[test]
    fn labels_the_selected_merge_method() {
        let detail = detail();
        let state = state_of(&detail, true);
        assert_eq!(
            state.selected_merge_method_label,
            merge_method_label(state.selected_merge_method)
        );
        assert!(state.pending_auto_merge_label.starts_with("Auto-merge ("));
    }

    #[test]
    fn core_detail_keeps_the_fields_the_logic_reads() {
        let detail = detail();
        let core = core_detail(&detail);
        assert_eq!(core.number, detail.number);
        assert_eq!(core.state, detail.state);
        assert_eq!(core.checks.len(), detail.checks.len());
    }

    #[test]
    fn toggles_the_timeline_order() {
        assert_eq!(TimelineOrder::Newest.toggled(), TimelineOrder::Oldest);
        assert_eq!(TimelineOrder::Oldest.toggled(), TimelineOrder::Newest);
    }
}
