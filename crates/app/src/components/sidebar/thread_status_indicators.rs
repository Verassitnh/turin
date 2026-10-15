//! Port of the sidebar parts of upstream/apps/web/src/components/ThreadStatusIndicators.tsx:
//! the pure PR-status rules (`resolveThreadPr`, `resolveDisplayedThreadPr`
//! and its provider variant, `nextThreadChangeRequestSnapshot`,
//! `threadChangeRequestSnapshotsEqual`, `linkedPullRequestSnapshotStatus`,
//! `prStatusIndicator`, `resolveThreadPullRequestBadgePresentation`), and the
//! `ThreadWorktreeIndicator` row glyph. Also `PULL_REQUEST_STATE_PRESENTATION`
//! (pullRequest/pullRequestIcons.tsx, pullRequestPresentation.tsx),
//! `formatWorktreePathForDisplay` (worktreeCleanup.ts) and `formatDiffCount`
//! (client-runtime diffCount.ts).
//!
//! Upstream keys tones by Tailwind classes; here a badge carries its
//! [`PullRequestBadgeState`] and [`pull_request_state_tone`] resolves the color
//! at render time, so the rules stay theme-free and testable.
//!
//! Not ported here: the `threadChangeRequestSnapshotsAtom` store and its
//! setter (the sidebar store holds the snapshot map, T14), the
//! `ThreadPullRequestBadgeControl` / `useLinkedThreadPullRequest` React
//! plumbing, `ChangeRequestStatusIcon` / `PrStatusTooltipContent` (the row
//! renders the glyph and tooltip directly), `synchronizeTerminalPulse` (DOM
//! animation events), and terminal status (`ThreadTerminalStatusButton`,
//! cut with the terminal). The row's badge still draws from the narrowed
//! `ThreadPullRequestLink`; the stack badge and the `+N` list open the PR
//! task's surface (T19).

// A faithful port keeps upstream's whole API (rule 3); the row wires the
// rest as the PR surface (T19) and the snapshot store (T14) land.
#![allow(dead_code)]

use gpui_kit::{
    App, ClickEvent, CursorStyle, ElementId, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, rems,
};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::git::{GitPullRequestState, VcsStatusChangeRequest, VcsStatusResult};
use turin_contracts::pull_request::PullRequestState as ContractPullRequestState;
use turin_contracts::source_control::{SourceControlProviderInfo, SourceControlProviderKind};
use turin_contracts::thread_pull_request::{
    ThreadLinkedPullRequest, ThreadPullRequestLink as ContractPullRequestLink,
};

use crate::components::drag_region::no_drag;
use crate::state::helpers::source_control_clients::SOURCE_CONTROL_CLIENTS;
use crate::state::models::{PullRequestState, ThreadPullRequestLink};
use crate::state::support::thread_pull_requests::{
    PullRequestBadgeState, ThreadPullRequestBadge as ThreadPrBadge,
};
use crate::theme::color::{hex, with_alpha};
use crate::theme::tokens::TEXT_XS;
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::sidebar::ClickHandler;
use crate::ui::{Icon, IconName, Tooltip};

/// `formatDiffCount`: the viewer's digit grouping (en-US: `83,065`).
pub fn format_diff_count(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `formatWorktreePathForDisplay`: the last path segment.
pub fn format_worktree_path_for_display(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return path.to_string();
    }
    let normalized = trimmed.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    match normalized.rsplit('/').next().map(str::trim) {
        Some(last) if !last.is_empty() => last.to_string(),
        _ => trimmed.to_string(),
    }
}

/// `resolvePullRequestState`: merged and closed win over draft; an unsynced
/// link (no state yet) reads as open.
fn presented_state(state: Option<PullRequestState>, is_draft: bool) -> PullRequestBadgeState {
    match (state, is_draft) {
        (Some(PullRequestState::Merged), _) => PullRequestBadgeState::Merged,
        (Some(PullRequestState::Closed), _) => PullRequestBadgeState::Closed,
        (_, true) => PullRequestBadgeState::Draft,
        _ => PullRequestBadgeState::Open,
    }
}

fn change_request_state(state: GitPullRequestState) -> PullRequestState {
    match state {
        GitPullRequestState::Open => PullRequestState::Open,
        GitPullRequestState::Closed => PullRequestState::Closed,
        GitPullRequestState::Merged => PullRequestState::Merged,
    }
}

fn contract_pull_request_state(state: ContractPullRequestState) -> GitPullRequestState {
    match state {
        ContractPullRequestState::Open => GitPullRequestState::Open,
        ContractPullRequestState::Closed => GitPullRequestState::Closed,
        ContractPullRequestState::Merged => GitPullRequestState::Merged,
    }
}

/// `PULL_REQUEST_STATE_PRESENTATION[state]`: label and glyph.
pub fn pull_request_state_label_and_icon(state: PullRequestBadgeState) -> (&'static str, IconName) {
    match state {
        PullRequestBadgeState::Open => ("Open", IconName::GitPullRequestArrow),
        PullRequestBadgeState::Draft => ("Draft", IconName::GitPullRequestDraft),
        PullRequestBadgeState::Closed => ("Closed", IconName::GitPullRequestClosed),
        PullRequestBadgeState::Merged => ("Merged", IconName::GitMerge),
    }
}

/// The color a state is drawn in on the current appearance.
pub fn pull_request_state_tone(state: PullRequestBadgeState, theme: &AppTheme) -> Hsla {
    let dark = theme.is_dark();
    let tone = |light: u32, dark_color: u32, alpha: f32| -> Hsla {
        if dark {
            with_alpha(hex(dark_color), alpha).into()
        } else {
            hex(light).into()
        }
    };
    match state {
        PullRequestBadgeState::Merged => tone(0x7f22fe, 0xc4b4ff, 0.9),
        PullRequestBadgeState::Closed => tone(0xe7000b, 0xffa2a2, 0.9),
        PullRequestBadgeState::Draft => tone(0x71717b, 0x9f9fa9, 0.8),
        PullRequestBadgeState::Open => tone(0x009966, 0x5ee9b5, 0.9),
    }
}

/// `PULL_REQUEST_STATE_PRESENTATION` for a link: label, tone and glyph.
pub fn pull_request_state_presentation(
    state: Option<PullRequestState>,
    is_draft: bool,
    theme: &AppTheme,
) -> (&'static str, Hsla, IconName) {
    let state = presented_state(state, is_draft);
    let (label, icon) = pull_request_state_label_and_icon(state);
    (label, pull_request_state_tone(state, theme), icon)
}

/// `PrStatusIndicator`: a change request as the status slot shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct PrStatusIndicator {
    pub label: String,
    pub state: PullRequestBadgeState,
    pub icon: IconName,
    pub tooltip: String,
    pub tooltip_lead: String,
    pub tooltip_title: String,
    pub url: String,
}

/// `LinkedThreadPullRequestStatus`.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkedThreadPullRequestStatus {
    pub pr: VcsStatusChangeRequest,
    pub source_control_provider: SourceControlProviderInfo,
}

/// `ThreadChangeRequestSnapshot`: the PR a thread last showed, kept across
/// branch switches and rows remounting. `linked_pull_request` is `Some` only
/// for a snapshot taken from an explicit link.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadChangeRequestSnapshot {
    pub branch: String,
    pub pr: VcsStatusChangeRequest,
    pub source_control_provider: Option<SourceControlProviderInfo>,
    pub linked_pull_request: Option<ThreadLinkedPullRequest>,
}

/// `prStatusIndicator`: label, tooltip and glyph for a change request. The
/// short label (`PR`, `MR`, …) comes from the provider's client definition.
pub fn pr_status_indicator(
    pr: Option<&VcsStatusChangeRequest>,
    provider: Option<&SourceControlProviderInfo>,
) -> Option<PrStatusIndicator> {
    let pr = pr?;
    let short_label = SOURCE_CONTROL_CLIENTS
        .get(provider.map(|provider| provider.kind.as_str()))
        .change_request
        .short_label;
    let state = presented_state(
        Some(change_request_state(pr.state)),
        pr.is_draft == Some(true),
    );
    let (state_label, icon) = pull_request_state_label_and_icon(state);
    let title = pr.title.as_str();
    let tooltip_lead = format!("{short_label} #{} - {state_label}", pr.number.0);
    Some(PrStatusIndicator {
        label: format!("{short_label} {}", state_label.to_lowercase()),
        state,
        icon,
        tooltip: format!("{tooltip_lead}: {title}"),
        tooltip_lead,
        tooltip_title: title.to_owned(),
        url: pr.url.clone(),
    })
}

/// `linkedPullRequestSnapshotStatus`: the status a link's persisted snapshot
/// stands for; `None` until the sync reactor has written one. A link records
/// no host kind, so the kind comes from its change request URL.
pub fn linked_pull_request_snapshot_status(
    link: &ContractPullRequestLink,
) -> Option<LinkedThreadPullRequestStatus> {
    let snapshot = link.snapshot.as_ref()?;
    let kind: SourceControlProviderKind = SOURCE_CONTROL_CLIENTS
        .find_by_change_request_url(link.url.as_str())
        .unwrap_or_else(|| SOURCE_CONTROL_CLIENTS.get(None))
        .kind
        .clone();
    Some(LinkedThreadPullRequestStatus {
        pr: VcsStatusChangeRequest {
            number: link.number,
            title: snapshot.title.clone(),
            url: link.url.as_str().to_owned(),
            base_ref: snapshot.base_branch.clone(),
            head_ref: snapshot.head_branch.clone(),
            state: contract_pull_request_state(snapshot.state),
            is_draft: Some(snapshot.is_draft),
            updated_at: snapshot.updated_at.clone().map(Some),
        },
        source_control_provider: SourceControlProviderInfo {
            name: TrimmedNonEmptyString::new(kind.as_str()).ok()?,
            base_url: String::new(),
            kind,
        },
    })
}

/// `resolveThreadPr`: the checkout's PR, only while the checkout is on the
/// stored thread branch.
pub fn resolve_thread_pr(
    thread_branch: Option<&str>,
    git_status: Option<&VcsStatusResult>,
) -> Option<VcsStatusChangeRequest> {
    let (branch, status) = (thread_branch?, git_status?);
    if !ref_name_is(status, branch) {
        return None;
    }
    status.pr.clone()
}

fn ref_name_is(status: &VcsStatusResult, branch: &str) -> bool {
    status
        .ref_name
        .as_ref()
        .is_some_and(|name| name.as_str() == branch)
}

/// The inputs `resolveDisplayedThreadPr` and `nextThreadChangeRequestSnapshot`
/// share. Upstream's `undefined` and `null` both arrive here as `None`; the
/// functions below spell out the outcomes that differ.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadPrInput<'a> {
    pub thread_branch: Option<&'a str>,
    pub git_status: Option<&'a VcsStatusResult>,
    pub snapshot: Option<&'a ThreadChangeRequestSnapshot>,
    pub retain_terminal_on_branch_mismatch: bool,
    pub linked_pull_request: Option<&'a ThreadLinkedPullRequest>,
    pub linked_pull_request_status: Option<&'a LinkedThreadPullRequestStatus>,
}

fn is_terminal(state: GitPullRequestState) -> bool {
    matches!(
        state,
        GitPullRequestState::Merged | GitPullRequestState::Closed
    )
}

/// A terminal PR a snapshot keeps across a branch switch. Linked snapshots
/// are never retained this way: their link decides their lifetime.
fn retains_terminal(input: &ThreadPrInput<'_>) -> bool {
    input.retain_terminal_on_branch_mismatch
        && input.snapshot.is_some_and(|snapshot| {
            snapshot.linked_pull_request.is_none() && is_terminal(snapshot.pr.state)
        })
}

/// `linkedPullRequestsEqual` against the stored snapshot. Only asked when the
/// input has a link.
fn linked_matches(input: &ThreadPrInput<'_>) -> bool {
    input
        .snapshot
        .and_then(|snapshot| snapshot.linked_pull_request.as_ref())
        == input.linked_pull_request
}

/// The PR a row displays and the provider that owns it.
struct DisplayedPr<'a> {
    pr: &'a VcsStatusChangeRequest,
    provider: Option<&'a SourceControlProviderInfo>,
}

fn displayed_pr<'a>(input: &ThreadPrInput<'a>) -> Option<DisplayedPr<'a>> {
    if input.linked_pull_request.is_some() {
        return match input.linked_pull_request_status {
            Some(status) => Some(DisplayedPr {
                pr: &status.pr,
                provider: Some(&status.source_control_provider),
            }),
            None if linked_matches(input) => input.snapshot.map(|snapshot| DisplayedPr {
                pr: &snapshot.pr,
                provider: snapshot.source_control_provider.as_ref(),
            }),
            None => None,
        };
    }
    if let (Some(branch), Some(git)) = (input.thread_branch, input.git_status)
        && ref_name_is(git, branch)
        && let Some(pr) = &git.pr
    {
        return Some(DisplayedPr {
            pr,
            provider: git.source_control_provider.as_ref(),
        });
    }
    if let (None, Some(branch), Some(snapshot)) =
        (input.git_status, input.thread_branch, input.snapshot)
        && snapshot.branch == branch
        && snapshot.linked_pull_request.is_none()
    {
        return Some(DisplayedPr {
            pr: &snapshot.pr,
            provider: snapshot.source_control_provider.as_ref(),
        });
    }
    if input.thread_branch.is_some() && retains_terminal(input) {
        let snapshot = input.snapshot?;
        return Some(DisplayedPr {
            pr: &snapshot.pr,
            provider: snapshot.source_control_provider.as_ref(),
        });
    }
    None
}

/// `resolveDisplayedThreadPr`: the live PR when the checkout is on the thread
/// branch; a linked PR while its link matches; otherwise a cached PR for the
/// thread branch, and a merged or closed one across a branch switch.
pub fn resolve_displayed_thread_pr(input: ThreadPrInput<'_>) -> Option<VcsStatusChangeRequest> {
    displayed_pr(&input).map(|displayed| displayed.pr.clone())
}

/// `resolveDisplayedThreadPrProvider`: the provider of the PR above.
pub fn resolve_displayed_thread_pr_provider(
    input: ThreadPrInput<'_>,
) -> Option<SourceControlProviderInfo> {
    displayed_pr(&input).and_then(|displayed| displayed.provider.cloned())
}

/// `nextThreadChangeRequestSnapshot`. The outer `None` is upstream's
/// `undefined`: leave the stored snapshot alone. `Some(None)` clears it and
/// `Some(Some(_))` stores the new one.
pub fn next_thread_change_request_snapshot(
    input: ThreadPrInput<'_>,
) -> Option<Option<ThreadChangeRequestSnapshot>> {
    if let Some(linked) = input.linked_pull_request {
        let Some(status) = input.linked_pull_request_status else {
            return if linked_matches(&input) {
                None
            } else {
                Some(None)
            };
        };
        return Some(Some(ThreadChangeRequestSnapshot {
            branch: input
                .thread_branch
                .map_or_else(|| status.pr.head_ref.as_str().to_owned(), str::to_owned),
            pr: status.pr.clone(),
            source_control_provider: Some(status.source_control_provider.clone()),
            linked_pull_request: Some(linked.clone()),
        }));
    }
    let Some(git) = input.git_status else {
        // Missing VCS data keeps a snapshot unless it is a linked one.
        return if input
            .snapshot
            .is_none_or(|snapshot| snapshot.linked_pull_request.is_none())
        {
            None
        } else {
            Some(None)
        };
    };
    let Some(branch) = input.thread_branch else {
        return Some(None);
    };
    if !ref_name_is(git, branch) {
        return if retains_terminal(&input) {
            None
        } else {
            Some(None)
        };
    }
    match &git.pr {
        None if retains_terminal(&input) => None,
        None => Some(None),
        Some(pr) => Some(Some(ThreadChangeRequestSnapshot {
            branch: branch.to_owned(),
            pr: pr.clone(),
            source_control_provider: git.source_control_provider.clone(),
            linked_pull_request: None,
        })),
    }
}

/// `threadChangeRequestSnapshotsEqual`. `updatedAt` compares with `null` and
/// `undefined` alike, so both sides flatten to `Option<String>`.
pub fn thread_change_request_snapshots_equal(
    left: &ThreadChangeRequestSnapshot,
    right: &ThreadChangeRequestSnapshot,
) -> bool {
    let updated_at = |snapshot: &ThreadChangeRequestSnapshot| {
        snapshot
            .pr
            .updated_at
            .as_ref()
            .and_then(|updated| updated.as_ref())
            .cloned()
    };
    left.branch == right.branch
        && left.pr.number == right.pr.number
        && left.pr.title == right.pr.title
        && left.pr.url == right.pr.url
        && left.pr.base_ref == right.pr.base_ref
        && left.pr.head_ref == right.pr.head_ref
        && left.pr.state == right.pr.state
        && left.pr.is_draft == right.pr.is_draft
        && updated_at(left) == updated_at(right)
        && left.source_control_provider == right.source_control_provider
        && left.linked_pull_request == right.linked_pull_request
}

/// What the sidebar's PR badge draws: icon, label and text, toned by `state`.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadPullRequestBadgePresentation {
    pub icon: IconName,
    /// `None` is the pending tone, before a snapshot exists.
    pub state: Option<PullRequestBadgeState>,
    pub label: String,
    pub text: String,
}

impl ThreadPullRequestBadgePresentation {
    pub fn tone(&self, theme: &AppTheme) -> Hsla {
        match self.state {
            Some(state) => pull_request_state_tone(state, theme),
            None => theme.sidebar_scope().muted_foreground,
        }
    }
}

/// `resolveThreadPullRequestBadgePresentation`. A stack or several unrelated
/// links show the aggregate state; a single link shows its own status.
pub fn resolve_thread_pull_request_badge_presentation(
    badge: Option<&ThreadPrBadge>,
    number: Option<u64>,
    url: Option<&str>,
    status: Option<&PrStatusIndicator>,
) -> Option<ThreadPullRequestBadgePresentation> {
    if let Some(ThreadPrBadge::Stack { state, layers }) = badge {
        let (aggregate_label, _) = pull_request_state_label_and_icon(*state);
        return Some(ThreadPullRequestBadgePresentation {
            // PullRequestGlyph.stack is LayersIcon, which the icon set lacks.
            icon: IconName::GitBranch,
            state: Some(*state),
            label: format!(
                "Stack of {layers} pull requests, {}",
                aggregate_label.to_lowercase()
            ),
            text: layers.to_string(),
        });
    }
    let number = number?;
    url?;
    let tooltip = status.map_or_else(
        || format!("PR #{number}, status pending"),
        |status| status.tooltip.clone(),
    );
    if let Some(ThreadPrBadge::PullRequest { state, others }) = badge
        && *others > 0
    {
        let (aggregate_label, icon) = pull_request_state_label_and_icon(*state);
        return Some(ThreadPullRequestBadgePresentation {
            icon,
            state: Some(*state),
            label: format!(
                "{tooltip}, and {others} more linked; overall {}",
                aggregate_label.to_lowercase()
            ),
            text: format!("+{}", others + 1),
        });
    }
    Some(ThreadPullRequestBadgePresentation {
        icon: status.map_or(IconName::GitPullRequestArrow, |status| status.icon),
        state: status.map(|status| status.state),
        label: tooltip,
        text: number.to_string(),
    })
}

/// The linked-PR badge in a row's meta line: state glyph + number, in the
/// state's tone. A single PR opens itself; several open the list.
#[derive(IntoElement)]
pub struct ThreadPullRequestBadge {
    id: ElementId,
    link: ThreadPullRequestLink,
    others: usize,
    on_open_list: Option<ClickHandler>,
}

impl ThreadPullRequestBadge {
    pub fn new(id: impl Into<ElementId>, link: ThreadPullRequestLink, others: usize) -> Self {
        Self {
            id: id.into(),
            link,
            others,
            on_open_list: None,
        }
    }

    pub fn on_open_list(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_list = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for ThreadPullRequestBadge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let (label, tone, icon) =
            pull_request_state_presentation(self.link.state, self.link.is_draft, theme);
        let number = self.link.number;
        let tooltip_lead = format!("PR #{number} - {label}");
        let tooltip: SharedString = match &self.link.title {
            Some(title) => format!("{tooltip_lead}: {title}"),
            None => tooltip_lead,
        }
        .into();
        let (text, tooltip) = if self.others > 0 {
            (
                format!("+{}", self.others + 1),
                SharedString::from(format!(
                    "{tooltip}, and {} more linked; overall {}",
                    self.others,
                    label.to_lowercase()
                )),
            )
        } else {
            (number.to_string(), tooltip)
        };
        let url = self.link.url.clone();
        let opens_list = self.others > 0;
        let on_open_list = self.on_open_list;
        div()
            .id(self.id)
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .cursor(CursorStyle::PointingHand)
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .text_color(tone)
            .hover(|style| style.opacity(0.8))
            .child(Icon::new(icon).size(rems(0.75)).color(tone))
            .child(text)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                if opens_list {
                    if let Some(handler) = &on_open_list {
                        handler(event, window, cx);
                    }
                } else {
                    cx.open_url(&url);
                }
            })
    }
}

/// `ThreadWorktreeIndicator`: a `FolderGit2` glyph for worktree threads.
#[derive(IntoElement)]
pub struct ThreadWorktreeIndicator {
    id: ElementId,
    worktree_path: Option<String>,
    branch: Option<String>,
}

impl ThreadWorktreeIndicator {
    pub fn new(
        id: impl Into<ElementId>,
        worktree_path: Option<String>,
        branch: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            worktree_path,
            branch,
        }
    }
}

impl RenderOnce for ThreadWorktreeIndicator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let path = self
            .worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(format_worktree_path_for_display);
        let color: Hsla = with_alpha(cx.app_theme().sidebar_scope().muted_foreground, 0.4).into();
        let tooltip: Option<SharedString> = path.map(|path| match &self.branch {
            Some(branch) => format!("Worktree: {path} ({branch})").into(),
            None => format!("Worktree: {path}").into(),
        });
        div()
            .id(self.id)
            .flex_shrink_0()
            .children(tooltip.map(|tooltip| {
                div()
                    .id("worktree")
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::FolderGit2)
                            .size(rems(0.75))
                            .color(color),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::git::VcsStatusWorkingTree;
    use turin_contracts::{PositiveInt, ProjectId};

    const FEATURE: &str = "feature/current";
    const PR_URL: &str = "https://github.com/pingdotgg/t3code/pull/42";

    fn text(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn change_request(
        state: GitPullRequestState,
        is_draft: Option<bool>,
        title: &str,
    ) -> VcsStatusChangeRequest {
        VcsStatusChangeRequest {
            number: PositiveInt::new(42).unwrap(),
            title: text(title),
            url: PR_URL.into(),
            base_ref: text("main"),
            head_ref: text(FEATURE),
            state,
            is_draft,
            updated_at: None,
        }
    }

    fn open_pr() -> VcsStatusChangeRequest {
        change_request(GitPullRequestState::Open, None, "PR branch")
    }

    fn merged_pr() -> VcsStatusChangeRequest {
        change_request(GitPullRequestState::Merged, None, "Feature PR")
    }

    fn github() -> SourceControlProviderInfo {
        SourceControlProviderInfo {
            kind: SourceControlProviderKind::new("github").unwrap(),
            name: text("GitHub"),
            base_url: "https://github.com".into(),
        }
    }

    fn status(
        ref_name: &str,
        pr: Option<VcsStatusChangeRequest>,
        provider: Option<SourceControlProviderInfo>,
    ) -> VcsStatusResult {
        VcsStatusResult {
            is_repo: true,
            source_control_provider: provider,
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: Some(text(ref_name)),
            has_working_tree_changes: false,
            working_tree: VcsStatusWorkingTree {
                files: vec![],
                insertions: 0,
                deletions: 0,
            },
            branch_changes: None,
            has_upstream: true,
            ahead_count: 0,
            behind_count: 0,
            ahead_of_default_count: None,
            pr,
        }
    }

    fn snapshot_for(
        branch: &str,
        pr: VcsStatusChangeRequest,
        provider: Option<SourceControlProviderInfo>,
    ) -> ThreadChangeRequestSnapshot {
        ThreadChangeRequestSnapshot {
            branch: branch.into(),
            pr,
            source_control_provider: provider,
            linked_pull_request: None,
        }
    }

    fn linked() -> ThreadLinkedPullRequest {
        ThreadLinkedPullRequest {
            project_id: ProjectId::new("project-1").unwrap(),
            repository: text("pingdotgg/t3code"),
            number: PositiveInt::new(42).unwrap(),
            url: text(PR_URL),
        }
    }

    fn with_link(mut snapshot: ThreadChangeRequestSnapshot) -> ThreadChangeRequestSnapshot {
        snapshot.linked_pull_request = Some(linked());
        snapshot
    }

    fn link_status(pr: VcsStatusChangeRequest) -> LinkedThreadPullRequestStatus {
        LinkedThreadPullRequestStatus {
            pr,
            source_control_provider: github(),
        }
    }

    #[test]
    fn groups_diff_counts_like_intl() {
        assert_eq!(format_diff_count(0), "0");
        assert_eq!(format_diff_count(999), "999");
        assert_eq!(format_diff_count(1240), "1,240");
        assert_eq!(format_diff_count(83065), "83,065");
        assert_eq!(format_diff_count(1234567), "1,234,567");
    }

    #[test]
    fn shows_the_last_worktree_segment() {
        assert_eq!(
            format_worktree_path_for_display("/a/b/T15-sidebar/"),
            "T15-sidebar"
        );
        assert_eq!(format_worktree_path_for_display("C:\\wt\\x"), "x");
        assert_eq!(format_worktree_path_for_display("  "), "  ");
    }

    // describe("ChangeRequestStatusIcon")
    #[test]
    fn uses_the_glyph_for_each_pull_request_state() {
        for (state, is_draft, expected) in [
            (
                GitPullRequestState::Open,
                false,
                IconName::GitPullRequestArrow,
            ),
            (
                GitPullRequestState::Open,
                true,
                IconName::GitPullRequestDraft,
            ),
            (
                GitPullRequestState::Closed,
                false,
                IconName::GitPullRequestClosed,
            ),
            (GitPullRequestState::Merged, false, IconName::GitMerge),
        ] {
            let presented = presented_state(Some(change_request_state(state)), is_draft);
            assert_eq!(pull_request_state_label_and_icon(presented).1, expected);
        }
    }

    // describe("resolveThreadPr")
    #[test]
    fn keeps_local_checkout_pr_indicators_scoped_to_the_stored_thread_branch() {
        let git = status("feature/other", Some(open_pr()), None);
        assert_eq!(resolve_thread_pr(Some("feature/current"), Some(&git)), None);
    }

    #[test]
    fn hides_pr_indicators_when_a_worktree_has_switched_away_from_the_thread_branch() {
        let git = status(FEATURE, Some(open_pr()), None);
        assert_eq!(resolve_thread_pr(Some("stack/base"), Some(&git)), None);
    }

    #[test]
    fn hides_pr_indicators_when_thread_branch_metadata_is_missing() {
        let git = status(FEATURE, Some(open_pr()), None);
        assert_eq!(resolve_thread_pr(None, Some(&git)), None);
    }

    #[test]
    fn shows_the_pr_when_the_live_checkout_matches_the_stored_thread_branch() {
        let git = status(FEATURE, Some(open_pr()), None);
        assert_eq!(
            resolve_thread_pr(Some(FEATURE), Some(&git)),
            Some(open_pr())
        );
    }

    // describe("resolveDisplayedThreadPr + nextThreadChangeRequestSnapshot")
    #[test]
    fn returns_the_live_merged_pr_when_the_checkout_matches_the_feature_branch() {
        let git = status(FEATURE, Some(merged_pr()), Some(github()));
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&git),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), Some(merged_pr()));
        assert_eq!(resolve_displayed_thread_pr_provider(input), Some(github()));
    }

    #[test]
    fn shows_a_linked_pull_request_when_the_checkout_has_a_different_branch() {
        let git = status("feature/other", None, None);
        let link = linked();
        let status_for_link = link_status(merged_pr());
        let input = ThreadPrInput {
            thread_branch: Some("feature/other"),
            git_status: Some(&git),
            linked_pull_request: Some(&link),
            linked_pull_request_status: Some(&status_for_link),
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), Some(merged_pr()));
        assert_eq!(resolve_displayed_thread_pr_provider(input), Some(github()));
        assert_eq!(
            next_thread_change_request_snapshot(input),
            Some(Some(ThreadChangeRequestSnapshot {
                branch: "feature/other".into(),
                pr: merged_pr(),
                source_control_provider: Some(github()),
                linked_pull_request: Some(link),
            }))
        );
    }

    #[test]
    fn keeps_a_matching_linked_pull_request_snapshot_while_its_status_reloads() {
        let snapshot = with_link(snapshot_for(FEATURE, merged_pr(), Some(github())));
        let link = linked();
        let input = ThreadPrInput {
            snapshot: Some(&snapshot),
            linked_pull_request: Some(&link),
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), Some(merged_pr()));
        // Undefined: the stored snapshot stays as it is.
        assert_eq!(next_thread_change_request_snapshot(input), None);
    }

    #[test]
    fn clears_an_old_snapshot_when_a_different_pull_request_is_linked() {
        let mut other = linked();
        other.number = PositiveInt::new(41).unwrap();
        let mut snapshot = snapshot_for(FEATURE, merged_pr(), Some(github()));
        snapshot.linked_pull_request = Some(other);
        let link = linked();
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: true,
            linked_pull_request: Some(&link),
            ..Default::default()
        };
        assert_eq!(next_thread_change_request_snapshot(input), Some(None));
    }

    #[test]
    fn removes_a_linked_pull_request_snapshot_after_the_link_is_cleared() {
        let snapshot = with_link(snapshot_for(FEATURE, merged_pr(), Some(github())));
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&git),
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), None);
        let no_git = ThreadPrInput {
            git_status: None,
            ..input
        };
        assert_eq!(next_thread_change_request_snapshot(no_git), Some(None));
    }

    #[test]
    fn after_caching_a_merged_pr_resolves_main_status_back_to_the_cached_feature_pr() {
        let matching = status(FEATURE, Some(merged_pr()), Some(github()));
        let cached = next_thread_change_request_snapshot(ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&matching),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        })
        .flatten()
        .unwrap();
        assert_eq!(cached, snapshot_for(FEATURE, merged_pr(), Some(github())));

        let unrelated = change_request(GitPullRequestState::Open, None, "Unrelated main PR");
        let unrelated = VcsStatusChangeRequest {
            number: PositiveInt::new(99).unwrap(),
            url: "https://github.com/pingdotgg/t3code/pull/99".into(),
            base_ref: text("main"),
            head_ref: text("main"),
            ..unrelated
        };
        let main = status("main", Some(unrelated), Some(github()));
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&main),
            snapshot: Some(&cached),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), Some(merged_pr()));
        assert_eq!(resolve_displayed_thread_pr_provider(input), Some(github()));
    }

    #[test]
    fn never_attaches_a_pr_reported_by_main_to_the_feature_thread() {
        let main_pr = VcsStatusChangeRequest {
            number: PositiveInt::new(99).unwrap(),
            url: "https://github.com/pingdotgg/t3code/pull/99".into(),
            base_ref: text("develop"),
            head_ref: text("main"),
            ..change_request(GitPullRequestState::Merged, None, "Unrelated main PR")
        };
        let main = status("main", Some(main_pr), None);
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&main),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), None);
        assert_eq!(next_thread_change_request_snapshot(input), Some(None));
    }

    #[test]
    fn does_not_show_a_cached_open_pr_across_a_branch_mismatch() {
        let open = snapshot_for(
            FEATURE,
            change_request(GitPullRequestState::Open, None, "Still open"),
            None,
        );
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&git),
            snapshot: Some(&open),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), None);
    }

    #[test]
    fn retains_a_cached_closed_pr_across_a_branch_mismatch() {
        let closed = change_request(GitPullRequestState::Closed, None, "Closed feature");
        let snapshot = snapshot_for(FEATURE, closed.clone(), None);
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&git),
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), Some(closed));
    }

    #[test]
    fn does_not_retain_or_display_a_terminal_pr_when_a_worktree_switches_branches() {
        let snapshot = snapshot_for(FEATURE, merged_pr(), Some(github()));
        let mismatched = status("feature/other", None, None);
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&mismatched),
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: false,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), None);
        assert_eq!(resolve_displayed_thread_pr_provider(input), None);
        assert_eq!(next_thread_change_request_snapshot(input), Some(None));
    }

    #[test]
    fn retains_a_local_terminal_snapshot_when_thread_metadata_follows_the_new_branch() {
        let snapshot = snapshot_for("feature/other", merged_pr(), Some(github()));
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&git),
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), Some(merged_pr()));
    }

    #[test]
    fn retains_a_terminal_snapshot_when_a_local_thread_and_status_move_to_a_branch_with_no_pr() {
        let snapshot = snapshot_for(FEATURE, merged_pr(), Some(github()));
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: Some("main"),
            git_status: Some(&git),
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(next_thread_change_request_snapshot(input), None);
        assert_eq!(resolve_displayed_thread_pr(input), Some(merged_pr()));
    }

    #[test]
    fn clears_an_open_snapshot_when_a_local_thread_moves_to_a_branch_with_no_pr() {
        let open = snapshot_for(
            FEATURE,
            change_request(GitPullRequestState::Open, None, "PR branch"),
            None,
        );
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: Some("main"),
            git_status: Some(&git),
            snapshot: Some(&open),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(next_thread_change_request_snapshot(input), Some(None));
    }

    #[test]
    fn clears_an_open_snapshot_when_a_local_checkout_moves_to_a_different_branch() {
        let open = snapshot_for(
            FEATURE,
            change_request(GitPullRequestState::Open, None, "PR branch"),
            None,
        );
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&git),
            snapshot: Some(&open),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(next_thread_change_request_snapshot(input), Some(None));
    }

    #[test]
    fn clears_a_retained_snapshot_when_the_thread_branch_is_cleared() {
        let snapshot = snapshot_for(FEATURE, merged_pr(), Some(github()));
        let git = status("main", None, None);
        let input = ThreadPrInput {
            thread_branch: None,
            git_status: Some(&git),
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(next_thread_change_request_snapshot(input), Some(None));
        assert_eq!(resolve_displayed_thread_pr(input), None);
        assert_eq!(resolve_displayed_thread_pr_provider(input), None);
    }

    #[test]
    fn does_not_erase_a_terminal_snapshot_when_vcs_data_is_missing() {
        let snapshot = snapshot_for(FEATURE, merged_pr(), Some(github()));
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: None,
            snapshot: Some(&snapshot),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        };
        assert_eq!(next_thread_change_request_snapshot(input), None);
        assert_eq!(resolve_displayed_thread_pr(input), Some(merged_pr()));
    }

    #[test]
    fn shows_the_last_matching_pr_snapshot_while_its_live_status_lease_is_released() {
        let open = change_request(GitPullRequestState::Open, None, "PR branch");
        let snapshot = snapshot_for(FEATURE, open.clone(), Some(github()));
        let input = ThreadPrInput {
            thread_branch: Some(FEATURE),
            snapshot: Some(&snapshot),
            ..Default::default()
        };
        assert_eq!(resolve_displayed_thread_pr(input), Some(open));
        assert_eq!(resolve_displayed_thread_pr_provider(input), Some(github()));
        let other_branch = ThreadPrInput {
            thread_branch: Some("feature/other"),
            ..input
        };
        assert_eq!(resolve_displayed_thread_pr(other_branch), None);
        let linked_snapshot = with_link(snapshot.clone());
        let linked_input = ThreadPrInput {
            snapshot: Some(&linked_snapshot),
            ..input
        };
        assert_eq!(resolve_displayed_thread_pr(linked_input), None);
    }

    #[test]
    fn retains_a_merged_pr_after_a_main_checkout() {
        let matching = status(FEATURE, Some(merged_pr()), Some(github()));
        let cached = next_thread_change_request_snapshot(ThreadPrInput {
            thread_branch: Some(FEATURE),
            git_status: Some(&matching),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        })
        .flatten()
        .unwrap();
        let main = status("main", None, None);
        let displayed = resolve_displayed_thread_pr(ThreadPrInput {
            thread_branch: Some("main"),
            git_status: Some(&main),
            snapshot: Some(&cached),
            retain_terminal_on_branch_mismatch: true,
            ..Default::default()
        });
        assert_eq!(
            displayed.map(|pr| pr.state),
            Some(GitPullRequestState::Merged)
        );
    }

    #[test]
    fn refreshes_a_cached_snapshot_when_a_pull_request_becomes_ready() {
        let draft = change_request(GitPullRequestState::Open, Some(true), "PR branch");
        let ready = change_request(GitPullRequestState::Open, Some(false), "PR branch");
        assert!(!thread_change_request_snapshots_equal(
            &snapshot_for(FEATURE, draft, None),
            &snapshot_for(FEATURE, ready, None),
        ));
    }

    // describe("prStatusIndicator")
    #[test]
    fn formats_pr_tooltips_with_number_status_and_title() {
        let indicator = pr_status_indicator(Some(&open_pr()), None).unwrap();
        assert_eq!(indicator.tooltip, "PR #42 - Open: PR branch");
        assert_eq!(indicator.tooltip_lead, "PR #42 - Open");
        assert_eq!(indicator.tooltip_title, "PR branch");
    }

    #[test]
    fn marks_closed_pull_requests_closed() {
        let closed = change_request(GitPullRequestState::Closed, None, "PR branch");
        let indicator = pr_status_indicator(Some(&closed), None).unwrap();
        assert_eq!(indicator.state, PullRequestBadgeState::Closed);
        assert_eq!(indicator.label, "PR closed");
    }

    #[test]
    fn uses_draft_wording_for_draft_pull_requests() {
        let draft = change_request(GitPullRequestState::Open, Some(true), "PR branch");
        let indicator = pr_status_indicator(Some(&draft), None).unwrap();
        assert_eq!(indicator.label, "PR draft");
        assert_eq!(indicator.state, PullRequestBadgeState::Draft);
        assert_eq!(indicator.tooltip_lead, "PR #42 - Draft");
    }

    // describe("resolveThreadPullRequestBadgePresentation")
    fn presentation(
        icon: IconName,
        state: Option<PullRequestBadgeState>,
        label: &str,
        text: &str,
    ) -> ThreadPullRequestBadgePresentation {
        ThreadPullRequestBadgePresentation {
            icon,
            state,
            label: label.into(),
            text: text.into(),
        }
    }

    #[test]
    fn returns_the_pending_pull_request_badge_when_no_snapshot_is_available() {
        assert_eq!(
            resolve_thread_pull_request_badge_presentation(None, Some(42), Some(PR_URL), None),
            Some(presentation(
                IconName::GitPullRequestArrow,
                None,
                "PR #42, status pending",
                "42",
            ))
        );
    }

    #[test]
    fn keeps_the_state_for_one_linked_pull_request() {
        let cases = [
            (
                change_request(GitPullRequestState::Open, Some(false), "PR branch"),
                IconName::GitPullRequestArrow,
                PullRequestBadgeState::Open,
                "PR #42 - Open: PR branch",
            ),
            (
                change_request(GitPullRequestState::Open, Some(true), "PR branch"),
                IconName::GitPullRequestDraft,
                PullRequestBadgeState::Draft,
                "PR #42 - Draft: PR branch",
            ),
            (
                change_request(GitPullRequestState::Closed, None, "PR branch"),
                IconName::GitPullRequestClosed,
                PullRequestBadgeState::Closed,
                "PR #42 - Closed: PR branch",
            ),
            (
                change_request(GitPullRequestState::Merged, None, "PR branch"),
                IconName::GitMerge,
                PullRequestBadgeState::Merged,
                "PR #42 - Merged: PR branch",
            ),
        ];
        for (pr, expected_icon, expected_state, expected_label) in cases {
            let indicator = pr_status_indicator(Some(&pr), None).unwrap();
            assert_eq!(
                resolve_thread_pull_request_badge_presentation(
                    Some(&ThreadPrBadge::PullRequest {
                        state: PullRequestBadgeState::Open,
                        others: 0,
                    }),
                    Some(42),
                    Some(PR_URL),
                    Some(&indicator),
                ),
                Some(presentation(
                    expected_icon,
                    Some(expected_state),
                    expected_label,
                    "42",
                )),
                "{expected_label}"
            );
        }
    }

    #[test]
    fn uses_a_layers_badge_with_the_stack_state_without_a_link_identity() {
        for (state, label) in [
            (PullRequestBadgeState::Open, "open"),
            (PullRequestBadgeState::Draft, "draft"),
            (PullRequestBadgeState::Merged, "merged"),
        ] {
            assert_eq!(
                resolve_thread_pull_request_badge_presentation(
                    Some(&ThreadPrBadge::Stack { state, layers: 3 }),
                    None,
                    None,
                    None,
                ),
                Some(presentation(
                    IconName::GitBranch,
                    Some(state),
                    &format!("Stack of 3 pull requests, {label}"),
                    "3",
                ))
            );
        }
    }

    #[test]
    fn draws_the_count_of_unrelated_linked_pull_requests_with_their_aggregate_state() {
        let closed = pr_status_indicator(
            Some(&change_request(
                GitPullRequestState::Closed,
                None,
                "PR branch",
            )),
            None,
        )
        .unwrap();
        for (state, icon, label) in [
            (
                PullRequestBadgeState::Open,
                IconName::GitPullRequestArrow,
                "open",
            ),
            (
                PullRequestBadgeState::Draft,
                IconName::GitPullRequestDraft,
                "draft",
            ),
            (PullRequestBadgeState::Merged, IconName::GitMerge, "merged"),
        ] {
            assert_eq!(
                resolve_thread_pull_request_badge_presentation(
                    Some(&ThreadPrBadge::PullRequest { state, others: 2 }),
                    Some(42),
                    Some(PR_URL),
                    Some(&closed),
                ),
                Some(presentation(
                    icon,
                    Some(state),
                    &format!("PR #42 - Closed: PR branch, and 2 more linked; overall {label}"),
                    "+3",
                ))
            );
        }
    }

    #[test]
    fn omits_the_control_when_neither_a_stack_nor_a_linked_identity_can_be_shown() {
        assert_eq!(
            resolve_thread_pull_request_badge_presentation(None, None, None, None),
            None
        );
    }
}
