//! Port of the pure parts of upstream/apps/web/src/components/pullRequest/:
//! `LinkPullRequestDialog.tsx` (`resolveLinkPullRequestInput`, the ownProject derivation, the
//! validation copy and submit gating), `PullRequestCandidatePicker.tsx` (body state, open-change
//! gating), `PullRequestLinkPreview.tsx` (author label, card and tooltip gating, modifier
//! bypass), `PullRequestThreadLinks.tsx` (count and menu copy, thread picker search and order),
//! `ThreadPullRequestsPanel.tsx` (source labels, row indent, speed mode, footer, project order),
//! and `PullRequestThreadDialog.tsx` (checkout validation, error precedence, confirm gating).
//! Also `pullRequestReference.ts` (`parsePullRequestReference`) and the pure finders in
//! `lib/openPullRequestLink.ts` that the panel imports.
//!
//! Skipped: JSX, React hooks, RPC/atom commands, toasts, focus and timers, popup wiring, and
//! `PullRequestContextDetails.tsx` (its only logic is `resolvePullRequestState`, ported in
//! `pull_request_presentation.rs`). `pullRequestLinkContextMenu.ts` is ported in
//! `pull_request_editing_logic.rs`. Thread-dialog styling (`statusTone`) is presentation only.

use crate::state::helpers::source_control_clients::{
    ChangeRequestUrlInput, SOURCE_CONTROL_CLIENTS, SourceControlClientDefinition,
    is_change_request_in_project_repository, is_change_request_on_project_host,
};
use crate::state::support::change_request_url::{ChangeRequestLink, parse_change_request_url};
use regex::Regex;
use std::cmp::Ordering;
use std::sync::LazyLock;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::js_trim;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestState, RepositoryIdentityLike, pull_request_host_of,
};
use turin_contracts::thread_pull_request::ThreadPullRequestLinkSource;

static BARE_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#?([0-9]+)$").unwrap());

// ---------------------------------------------------------------------------------------------
// LinkPullRequestDialog.tsx
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPullRequestResolution {
    Link(ResolvedLink),
    Error(String),
}

pub struct LinkPullRequestProject<'a> {
    pub host: &'a str,
    pub repository: &'a str,
    pub web_url: &'a dyn Fn(u64) -> Option<String>,
}

/// URL owns its host and repository; a bare number needs this thread's own project.
pub fn resolve_link_pull_request_input(
    reference: &str,
    project: Option<&LinkPullRequestProject<'_>>,
    has_project: &dyn Fn(&ResolvedLink) -> bool,
) -> Option<LinkPullRequestResolution> {
    let trimmed = js_trim(reference);
    let parsed = if parse_change_request_url(trimmed).is_some() {
        trimmed.to_owned()
    } else {
        parse_pull_request_reference(trimmed)?
    };
    if let Some(url) = parse_change_request_url(&parsed) {
        let link = ResolvedLink {
            host: url.host,
            repository: url.repository,
            number: url.number,
            url: parsed,
        };
        if !has_project(&link) {
            return Some(LinkPullRequestResolution::Error(format!(
                "No project in this environment can read {}/{}.",
                link.host, link.repository
            )));
        }
        return Some(LinkPullRequestResolution::Link(link));
    }
    let number: u64 = parsed.parse().ok()?;
    // `Number.isSafeInteger(n) && n >= 1`: anything past 2^53 - 1 is not a pull request number.
    if number == 0 || number > 9_007_199_254_740_991 {
        return None;
    }
    let Some(project) = project else {
        return Some(LinkPullRequestResolution::Error(
            "Paste a full URL to link a pull request from another repository.".to_owned(),
        ));
    };
    let _ = (project.host, project.repository);
    let Some(web_url) = (project.web_url)(number) else {
        return Some(LinkPullRequestResolution::Error(
            "Paste a full URL; this project's host has no known pull request URL.".to_owned(),
        ));
    };
    let Some(web_reference) = parse_change_request_url(&web_url) else {
        return Some(LinkPullRequestResolution::Error(
            "Paste a full URL; this project's host has no known pull request URL.".to_owned(),
        ));
    };
    Some(LinkPullRequestResolution::Link(ResolvedLink {
        host: web_reference.host,
        repository: web_reference.repository,
        number: web_reference.number,
        url: web_url,
    }))
}

/// The change request a pasted reference names: a host's change request URL, returned as is, or
/// a bare number. A host's checkout command, such as `gh pr checkout 42`, is read for its
/// argument first. `None` for anything else, such as a branch name.
pub fn parse_pull_request_reference(input: &str) -> Option<String> {
    let trimmed = js_trim(input);
    if trimmed.is_empty() {
        return None;
    }
    let normalized = SOURCE_CONTROL_CLIENTS
        .definitions()
        .iter()
        .find_map(|definition| (definition.checkout_command_argument)(trimmed))
        .unwrap_or_else(|| trimmed.to_owned());
    if normalized.is_empty() {
        return None;
    }
    if SOURCE_CONTROL_CLIENTS
        .definitions()
        .iter()
        .any(|definition| (definition.is_change_request_reference)(&normalized))
    {
        return Some(normalized);
    }
    BARE_NUMBER
        .captures(&normalized)
        .map(|captures| captures[1].to_owned())
}

/// The thread's own repository as a link target: `ownProject` in `LinkPullRequestDialog.tsx`.
/// `None` when the identity has no repository path to address.
pub struct OwnLinkProject {
    pub host: String,
    pub repository: String,
    identity: RepositoryIdentity,
    definition: &'static SourceControlClientDefinition,
}

pub fn own_link_project(identity: &RepositoryIdentity) -> Option<OwnLinkProject> {
    let repository = identity
        .display_name
        .as_ref()
        .map(|name| name.as_str().to_owned())
        .or_else(|| match (&identity.owner, &identity.name) {
            (Some(owner), Some(name)) => Some(format!("{}/{}", owner.as_str(), name.as_str())),
            _ => None,
        })?;
    let definition = SOURCE_CONTROL_CLIENTS.get(identity.provider.as_ref().map(|p| p.as_str()));
    let host = pull_request_host_of(
        Some(&RepositoryIdentityLike {
            canonical_key: Some(identity.canonical_key.as_str()),
        }),
        &definition.kind,
    );
    Some(OwnLinkProject {
        host,
        repository,
        identity: identity.clone(),
        definition,
    })
}

impl OwnLinkProject {
    /// The web URL of `number` in this repository, when the host can build one.
    pub fn web_url(&self, number: u64) -> Option<String> {
        (self.definition.change_request_url)(&ChangeRequestUrlInput {
            host: &self.host,
            repository: &self.repository,
            number,
            remote_url: Some(self.identity.locator.remote_url.as_str()),
            web_url: self.identity.web_url.as_ref().map(|url| url.as_str()),
        })
    }
}

pub const LINK_PULL_REQUEST_EMPTY_MESSAGE: &str =
    "Paste a pull request URL or enter 123 / #123.";
pub const LINK_PULL_REQUEST_INVALID_MESSAGE: &str = "Use a pull request URL, 123, or #123.";

/// The line under the input: an empty or unreadable reference is only named once the user has
/// typed (`dirty`), and a resolution error is shown as-is.
pub fn link_pull_request_validation_message(
    dirty: bool,
    reference: &str,
    resolution: Option<&LinkPullRequestResolution>,
) -> Option<String> {
    if !dirty {
        return None;
    }
    if js_trim(reference).is_empty() {
        return Some(LINK_PULL_REQUEST_EMPTY_MESSAGE.to_owned());
    }
    match resolution {
        None => Some(LINK_PULL_REQUEST_INVALID_MESSAGE.to_owned()),
        Some(LinkPullRequestResolution::Error(message)) => Some(message.clone()),
        Some(LinkPullRequestResolution::Link(_)) => None,
    }
}

/// `host/repository #number` under the input once it resolves to a link.
pub fn link_pull_request_summary(resolution: Option<&LinkPullRequestResolution>) -> Option<String> {
    match resolution {
        Some(LinkPullRequestResolution::Link(link)) => Some(format!(
            "{}/{} #{}",
            link.host, link.repository, link.number
        )),
        _ => None,
    }
}

/// The Link button and Enter only act on a resolved link; an error or nothing does nothing.
pub fn link_pull_request_can_submit(resolution: Option<&LinkPullRequestResolution>) -> bool {
    matches!(resolution, Some(LinkPullRequestResolution::Link(_)))
}

/// `LinkPullRequestDialogHost` renders nothing without a thread or where linking is unsupported.
pub fn link_pull_request_dialog_host_visible(thread_selected: bool, unsupported: bool) -> bool {
    thread_selected && !unsupported
}

// ---------------------------------------------------------------------------------------------
// PullRequestCandidatePicker.tsx
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidatePickerBody {
    Pending,
    /// The label leads the host's own message in one sentence.
    Error(String),
    /// A search that found nothing says so; an empty list says it is empty.
    Empty { no_match: bool },
    Rows,
}

/// Which state the picker's scrolling body shows, in upstream's precedence order.
pub fn candidate_picker_body(
    is_pending: bool,
    error: Option<&str>,
    error_label: &str,
    candidate_count: usize,
    query: &str,
) -> CandidatePickerBody {
    if is_pending {
        CandidatePickerBody::Pending
    } else if let Some(error) = error {
        CandidatePickerBody::Error(format!("{error_label} {error}"))
    } else if candidate_count == 0 {
        CandidatePickerBody::Empty {
            no_match: !query.is_empty(),
        }
    } else {
        CandidatePickerBody::Rows
    }
}

/// Picking a row keeps the menu open (a second label or reviewer is usually wanted next), so an
/// item-press close is cancelled. Returns whether the open change is forwarded to the caller.
pub fn candidate_picker_forwards_open_change(next_open: bool, reason: &str) -> bool {
    next_open || reason != "item-press"
}

// ---------------------------------------------------------------------------------------------
// PullRequestLinkPreview.tsx
// ---------------------------------------------------------------------------------------------

/// The author line of the preview card. `None` is a ghost account (upstream's `author === null`).
pub fn link_preview_author_label(author: Option<&PullRequestActor>) -> String {
    let Some(author) = author else {
        return "ghost".to_owned();
    };
    let login = author.login.as_str();
    match author.name.as_deref() {
        Some(name) if !name.is_empty() && name != login => format!("{name} (@{login})"),
        _ => login.to_owned(),
    }
}

/// The card shows once the detail loads, or instead of a failed load when a fallback exists.
pub fn link_preview_shows_card(has_detail: bool, has_error: bool, has_fallback: bool) -> bool {
    has_detail || (has_error && has_fallback)
}

/// The plain URL tooltip covers a failed load that has no card to show.
pub fn link_preview_shows_url_tooltip(open: bool, has_error: bool, show_card: bool) -> bool {
    open && has_error && !show_card
}

/// A click with a modifier is left to the browser: no preview confirmation, no preventDefault.
pub fn link_preview_bypasses_confirm(meta: bool, ctrl: bool, shift: bool, alt: bool) -> bool {
    meta || ctrl || shift || alt
}

// ---------------------------------------------------------------------------------------------
// PullRequestThreadLinks.tsx
// ---------------------------------------------------------------------------------------------

pub fn linked_threads_label(count: usize) -> String {
    if count > 0 {
        format!(
            "Linked from {count} {}",
            if count == 1 { "thread" } else { "threads" }
        )
    } else {
        "Linked threads".to_owned()
    }
}

/// The count button shows the number of linked threads, or `?` while only an error is known.
pub fn linked_threads_count_text(count: usize) -> String {
    if count == 0 {
        "?".to_owned()
    } else {
        count.to_string()
    }
}

/// The count button appears when a thread is linked, or when the relation query failed.
pub fn show_linked_threads_count(count_display: bool, count: usize, has_error: bool) -> bool {
    count_display && (count > 0 || has_error)
}

pub fn thread_link_menu_label(linked_here: bool, has_current_thread: bool) -> &'static str {
    if linked_here {
        "Unlink from this thread"
    } else if has_current_thread {
        "Link to this thread"
    } else {
        "Link to thread"
    }
}

/// Nothing renders for a URL that is not a change request, or one that cannot be linked here.
pub fn thread_links_render(parsed: bool, linked_here: bool, can_link: bool) -> bool {
    parsed && (linked_here || can_link)
}

pub fn change_link_failure_title(remove: bool) -> &'static str {
    if remove {
        "Could not unlink the pull request"
    } else {
        "Could not link the pull request"
    }
}

/// The thread picker's search: trimmed and lowercased.
pub fn thread_picker_search(query: &str) -> String {
    js_trim(query).to_lowercase()
}

pub fn thread_picker_title(title: &str) -> &str {
    if title.is_empty() {
        "Untitled thread"
    } else {
        title
    }
}

/// One thread as the picker reads it.
pub struct PickerThread<'a> {
    pub id: &'a str,
    pub environment_id: &'a str,
    pub title: &'a str,
    pub project_id: &'a str,
    pub archived: bool,
    pub updated_at: &'a str,
}

/// Active threads in `environment_id` whose title or project name contains `query`, most
/// recently updated first. `project_title` resolves a project id to its name.
pub fn thread_picker_candidates<'a, 'b>(
    threads: &'b [PickerThread<'a>],
    environment_id: &str,
    project_title: &dyn Fn(&str) -> Option<&'a str>,
    query: &str,
) -> Vec<&'b PickerThread<'a>> {
    let search = thread_picker_search(query);
    let mut candidates: Vec<&PickerThread<'a>> = threads
        .iter()
        .filter(|thread| {
            thread.environment_id == environment_id
                && !thread.archived
                && format!(
                    "{} {}",
                    thread.title,
                    project_title(thread.project_id).unwrap_or("")
                )
                .to_lowercase()
                .contains(&search)
        })
        .collect();
    candidates.sort_by(|left, right| {
        super::super::state::support::collate::locale_compare(right.updated_at, left.updated_at)
    });
    candidates
}

// ---------------------------------------------------------------------------------------------
// ThreadPullRequestsPanel.tsx
// ---------------------------------------------------------------------------------------------

pub fn thread_link_source_label(source: ThreadPullRequestLinkSource) -> &'static str {
    match source {
        ThreadPullRequestLinkSource::Manual => "Linked by you",
        ThreadPullRequestLinkSource::Created => "Created from this thread",
        ThreadPullRequestLinkSource::Agent => "Linked by the agent",
        ThreadPullRequestLinkSource::Stack => "Found in the stack",
        ThreadPullRequestLinkSource::StackDismissed => "Dismissed",
    }
}

/// Each stacked layer steps in under the one it targets, capped at three steps: beyond that the
/// indent only says "still in the stack", which the connector line already does.
pub fn thread_link_row_indent_rem(depth: usize) -> f32 {
    0.5 + depth.min(3) as f32 * 1.25
}

/// A link without a snapshot has not been read yet, so it counts as open.
pub fn thread_link_is_open(state: Option<PullRequestState>) -> bool {
    matches!(state, None | Some(PullRequestState::Open))
}

pub fn thread_link_watch_label(watching: bool) -> &'static str {
    if watching {
        "Stop watching"
    } else {
        "Watch for changes"
    }
}

pub fn thread_link_unlink_label(source: ThreadPullRequestLinkSource) -> &'static str {
    if source == ThreadPullRequestLinkSource::Stack {
        "Dismiss from thread"
    } else {
        "Unlink from thread"
    }
}

/// Tooltip on a stack badge. A native stack merges as one; otherwise the layers are chained by
/// base branch.
pub fn thread_link_stack_tooltip(native: bool, host_label: &str, size: u64) -> String {
    if native {
        format!(
            "{host_label} stack of {size}: merging a layer lands the ones below it."
        )
    } else {
        format!("{size} pull requests chained by base branch.")
    }
}

/// Speed mode is Shift held with no other modifier.
pub fn thread_links_speed_mode(shift: bool, meta: bool, ctrl: bool, alt: bool) -> bool {
    shift && !meta && !ctrl && !alt
}

/// Moves the thread's own project to the front; the rest keep their order.
pub fn sort_projects_thread_first<T>(projects: &mut [T], is_thread_project: impl Fn(&T) -> bool) {
    projects.sort_by_key(|project| !is_thread_project(project));
}

/// The project a row's actions run against: the link's project on a host that reports pull
/// requests, otherwise the thread's own project.
pub fn thread_link_row_project_id<T: Clone>(
    host_reports_pull_requests: bool,
    found_project: Option<T>,
    thread_project: Option<T>,
) -> Option<T> {
    if host_reports_pull_requests {
        found_project.or(thread_project)
    } else {
        None
    }
}

pub fn thread_link_row_key(host: &str, repository: &str, number: u64) -> String {
    format!("{host}/{repository}#{number}")
}

/// The panel footer's tally: open and total links, and when the newest snapshot synced.
pub fn thread_links_footer(open: usize, linked: usize, synced_label: Option<&str>) -> String {
    let base = format!("{open} open · {linked} linked");
    match synced_label {
        Some(label) => format!("{base} · synced {label}"),
        None => base,
    }
}

/// The latest `syncedAt` across links, comparing the ISO strings as text.
pub fn thread_links_last_synced<'a>(
    synced_at: impl IntoIterator<Item = Option<&'a str>>,
) -> Option<&'a str> {
    let mut latest: Option<&'a str> = None;
    for at in synced_at.into_iter().flatten() {
        if latest.is_none_or(|current| at > current) {
            latest = Some(at);
        }
    }
    latest
}

// ---------------------------------------------------------------------------------------------
// PullRequestThreadDialog.tsx
// ---------------------------------------------------------------------------------------------

/// What the checkout dialog's confirm does with a click or Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutConfirmStep {
    Ignore,
    MarkReferenceDirty,
    Prepare,
}

#[derive(Debug, Clone, Copy)]
pub struct CheckoutConfirmInput {
    /// The source-control action is allowed on this connection.
    pub is_allowed: bool,
    pub worktree: bool,
    /// The task-operate scope, which a worktree needs and a local checkout does not.
    pub operate_scope_granted: bool,
    pub reference_parses: bool,
    pub has_resolved_pull_request: bool,
    pub has_cwd: bool,
}

pub fn checkout_confirm_step(input: CheckoutConfirmInput) -> CheckoutConfirmStep {
    if !input.is_allowed {
        return CheckoutConfirmStep::Ignore;
    }
    if input.worktree && !input.operate_scope_granted {
        return CheckoutConfirmStep::Ignore;
    }
    if !input.reference_parses {
        return CheckoutConfirmStep::MarkReferenceDirty;
    }
    if !input.has_resolved_pull_request || !input.has_cwd {
        return CheckoutConfirmStep::Ignore;
    }
    CheckoutConfirmStep::Prepare
}

pub fn checkout_validation_message(
    dirty: bool,
    reference: &str,
    reference_parses: bool,
    singular: &str,
) -> Option<String> {
    if !dirty {
        return None;
    }
    if js_trim(reference).is_empty() {
        return Some(format!(
            "Paste a {singular} URL, checkout command, or enter 123 / #123."
        ));
    }
    if !reference_parses {
        return Some(format!(
            "Use a {singular} URL, checkout command, 123, or #123."
        ));
    }
    None
}

/// An error from the prepare action: an `Error` carries its message, anything else gets the
/// generic line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutFailure<'a> {
    Error(&'a str),
    Other,
}

/// The line a failed prepare leaves behind. An interrupted prepare sets no message at all; the
/// caller clears the action's error instead.
pub fn checkout_prepare_failure_message(failure: CheckoutFailure<'_>, singular: &str) -> String {
    match failure {
        CheckoutFailure::Error(message) => message.to_owned(),
        CheckoutFailure::Other => format!("Failed to prepare {singular} thread."),
    }
}

pub struct CheckoutErrorInput<'a> {
    pub validation: Option<String>,
    pub prepare_error: Option<String>,
    pub has_resolved_pull_request: bool,
    /// The resolution query's error, shown only while nothing resolved.
    pub resolution_error: Option<&'a str>,
    /// The prepare action's own error state.
    pub action_failure: Option<CheckoutFailure<'a>>,
    pub singular: &'a str,
}

/// The one line under the dialog, in upstream's precedence: validation, then the last prepare
/// failure, then the resolution error while nothing resolved, then the action error.
pub fn checkout_error_message(input: CheckoutErrorInput<'_>) -> Option<String> {
    if let Some(message) = input.validation {
        return Some(message);
    }
    if let Some(message) = input.prepare_error {
        return Some(message);
    }
    if !input.has_resolved_pull_request
        && let Some(message) = input.resolution_error
    {
        return Some(message.to_owned());
    }
    input
        .action_failure
        .map(|failure| checkout_prepare_failure_message(failure, input.singular))
}

pub fn checkout_button_disabled(
    worktree: bool,
    is_allowed: bool,
    can_operate: bool,
    has_cwd: bool,
    has_resolved_pull_request: bool,
    is_resolving: bool,
    is_pending: bool,
) -> bool {
    !is_allowed
        || (worktree && !can_operate)
        || !has_cwd
        || !has_resolved_pull_request
        || is_resolving
        || is_pending
}

/// Whether the dialog is still looking the reference up. `reference_settled` is true once the
/// debounced reference equals the typed one.
pub fn checkout_is_resolving(
    open: bool,
    reference_parses: bool,
    has_resolved_pull_request: bool,
    debouncer_pending: bool,
    reference_settled: bool,
    resolution_pending: bool,
    resolution_fetching: bool,
) -> bool {
    open
        && reference_parses
        && !has_resolved_pull_request
        && (debouncer_pending
            || !reference_settled
            || resolution_pending
            || resolution_fetching)
}

/// Enter in the reference field. `None` leaves the key alone (IME composition, key code 229);
/// `Some(submit)` prevents the default and submits a worktree checkout when nothing is in flight.
pub fn checkout_enter_key(
    is_composing: bool,
    key_code: u32,
    is_resolving: bool,
    is_pending: bool,
) -> Option<bool> {
    if is_composing || key_code == 229 {
        return None;
    }
    Some(!is_resolving && !is_pending)
}

// ---------------------------------------------------------------------------------------------
// lib/openPullRequestLink.ts (pure finders used by the thread panel)
// ---------------------------------------------------------------------------------------------

/// Whether a click's modifier sends a change request link to the browser instead of the page.
pub fn should_open_pull_request_externally(meta: bool, ctrl: bool) -> bool {
    meta || ctrl
}

/// The project whose checkout names the link's repository on the link's host.
pub fn find_project_for_change_request<'a, T>(
    projects: &'a [T],
    identity_of: impl Fn(&T) -> Option<&RepositoryIdentity>,
    link: &ChangeRequestLink,
) -> Option<&'a T> {
    projects.iter().find(|project| {
        identity_of(project).is_some_and(|identity| {
            identity.provider.as_ref().is_some_and(|provider| {
                let definition = SOURCE_CONTROL_CLIENTS.get(Some(provider.as_str()));
                is_change_request_in_project_repository(&definition.kind, identity, link)
            })
        })
    })
}

/// Any project checked out from the link's host. A link's own project comes first.
pub fn find_project_on_change_request_host<'a, T>(
    projects: &'a [T],
    identity_of: impl Fn(&T) -> Option<&RepositoryIdentity>,
    link: &ChangeRequestLink,
) -> Option<&'a T> {
    find_project_for_change_request(projects, &identity_of, link).or_else(|| {
        projects.iter().find(|project| {
            identity_of(project).is_some_and(|identity| {
                identity.provider.as_ref().is_some_and(|provider| {
                    let definition = SOURCE_CONTROL_CLIENTS.get(Some(provider.as_str()));
                    is_change_request_on_project_host(&definition.kind, identity, link)
                })
            })
        })
    })
}

/// Stable comparison used where upstream calls `localeCompare` on ASCII identifiers.
pub fn compare_locale(left: &str, right: &str) -> Ordering {
    crate::state::support::collate::locale_compare(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn resolve(
        reference: &str,
        project: Option<&LinkPullRequestProject<'_>>,
        has_project: &dyn Fn(&ResolvedLink) -> bool,
    ) -> Option<LinkPullRequestResolution> {
        resolve_link_pull_request_input(reference, project, has_project)
    }
    fn project<'a>(url: &'a dyn Fn(u64) -> Option<String>) -> LinkPullRequestProject<'a> {
        LinkPullRequestProject {
            host: "github.com",
            repository: "acme/web",
            web_url: url,
        }
    }

    // ---- LinkPullRequestDialog.logic.test.ts (1:1, in upstream order) ----

    #[test]
    fn links_supported_host_url_without_thread_project() {
        // `it.each` over three host URLs.
        for (url, host) in [
            (
                "https://bitbucket.org/acme/web/pull-requests/42",
                "bitbucket.org",
            ),
            (
                "https://github.acme.test/acme/web/pull/42",
                "github.acme.test",
            ),
            (
                "https://git.acme.test/acme/web/-/merge_requests/42",
                "git.acme.test",
            ),
        ] {
            assert_eq!(
                resolve(&format!(" {url} "), None, &|candidate| candidate.host == host),
                Some(LinkPullRequestResolution::Link(ResolvedLink {
                    host: host.to_owned(),
                    repository: "acme/web".to_owned(),
                    number: 42,
                    url: url.to_owned()
                }))
            );
        }
    }

    #[test]
    fn validates_full_azure_repository_when_resolving_a_browser_url() {
        let allowed = |r: &ResolvedLink| {
            r.host == "dev.azure.com" && r.repository == "org-a/project/_git/web"
        };
        assert!(matches!(
            resolve(
                "https://dev.azure.com/org-a/project/_git/web/pullrequest/42",
                None,
                &allowed
            ),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                repository,
                number: 42,
                ..
            })) if repository == "org-a/project/_git/web"
        ));
        let denied = resolve(
            "https://dev.azure.com/org-b/project/_git/web/pullrequest/42",
            None,
            &allowed,
        );
        assert!(matches!(
            denied,
            Some(LinkPullRequestResolution::Error(message))
                if message.contains("org-b/project/_git/web")
        ));
    }

    #[test]
    fn resolves_bare_azure_numbers_into_canonical_browser_urls() {
        // Upstream builds this URL with the Azure DevOps client, which Turin cuts; the closure
        // stands in for it (see PORTING.md Deviations).
        let web = |n| {
            Some(format!(
                "https://dev.azure.com/org/project/_git/web/pullrequest/{n}"
            ))
        };
        let project = LinkPullRequestProject {
            host: "ssh.dev.azure.com",
            repository: "v3/org/project/web",
            web_url: &web,
        };
        assert_eq!(
            resolve("#42", Some(&project), &|_| true),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                host: "dev.azure.com".to_owned(),
                repository: "org/project/_git/web".to_owned(),
                number: 42,
                url: "https://dev.azure.com/org/project/_git/web/pullrequest/42".to_owned()
            }))
        );
    }

    #[test]
    fn returns_none_for_input_that_is_not_a_reference() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert_eq!(resolve("hello", Some(&project(&web)), &|_| true), None);
    }

    #[test]
    fn resolves_a_bare_number_against_the_thread_own_repository() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert_eq!(
            resolve("#42", Some(&project(&web)), &|_| true),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                host: "github.com".to_owned(),
                repository: "acme/web".to_owned(),
                number: 42,
                url: "https://github.com/acme/web/pull/42".to_owned()
            }))
        );
    }

    #[test]
    fn links_a_url_from_another_repository_on_a_host_with_a_project() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert_eq!(
            resolve(
                "https://github.com/acme/api/pull/7",
                Some(&project(&web)),
                &|r| r.host == "github.com"
            ),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                host: "github.com".to_owned(),
                repository: "acme/api".to_owned(),
                number: 7,
                url: "https://github.com/acme/api/pull/7".to_owned()
            }))
        );
    }

    #[test]
    fn refuses_a_url_on_a_host_nothing_is_checked_out_from() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert!(matches!(
            resolve(
                "https://gitlab.com/acme/api/-/merge_requests/7",
                Some(&project(&web)),
                &|_| false
            ),
            Some(LinkPullRequestResolution::Error(message)) if message.contains("gitlab.com")
        ));
    }

    #[test]
    fn asks_for_a_url_when_a_bare_number_has_no_project_to_resolve_against() {
        assert!(matches!(
            resolve("12", None, &|_| true),
            Some(LinkPullRequestResolution::Error(message)) if message.contains("full URL")
        ));
    }

    #[test]
    fn accepts_a_checkout_command_as_a_reference() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert!(matches!(
            resolve(
                "gh pr checkout https://github.com/acme/web/pull/3",
                Some(&project(&web)),
                &|_| true
            ),
            Some(LinkPullRequestResolution::Link(ResolvedLink { number: 3, repository, .. }))
                if repository == "acme/web"
        ));
    }

    // `describe("changeRequestWebUrl")`: GitLab and Azure DevOps definitions are cut, so only the
    // GitHub builder and the unknown client's refusal can be checked here.
    #[test]
    fn change_request_web_url_knows_github_and_nothing_else() {
        let build = |definition: &SourceControlClientDefinition, host, repository, number| {
            (definition.change_request_url)(&ChangeRequestUrlInput {
                host,
                repository,
                number,
                remote_url: None,
                web_url: None,
            })
        };
        assert_eq!(
            build(
                SOURCE_CONTROL_CLIENTS.get(Some("github")),
                "github.com",
                "g/repo",
                5
            ),
            Some("https://github.com/g/repo/pull/5".to_owned())
        );
        assert_eq!(
            build(&UNKNOWN_CLIENT_FOR_TESTS, "x", "a/b", 1),
            None
        );
    }

    /// The registry's fallback definition, which builds no change request URL.
    static UNKNOWN_CLIENT_FOR_TESTS: LazyLockUnknown = LazyLockUnknown;
    struct LazyLockUnknown;
    impl std::ops::Deref for LazyLockUnknown {
        type Target = SourceControlClientDefinition;
        fn deref(&self) -> &SourceControlClientDefinition {
            &crate::state::helpers::source_control_clients::UNKNOWN_SOURCE_CONTROL_CLIENT
        }
    }

    #[test]
    fn known_web_url_and_unknown_returns_none() {
        assert!(
            parse_change_request_url("https://gitlab.com/g/sub/repo/-/merge_requests/5").is_some()
        );
        assert!(parse_change_request_url("https://x/a/b/pull/1").is_none());
    }

    // ---- Helper coverage (no upstream test file; behaviour from the component source) ----

    #[test]
    fn validation_names_the_empty_and_invalid_reference_only_after_typing() {
        assert_eq!(link_pull_request_validation_message(false, "", None), None);
        assert_eq!(
            link_pull_request_validation_message(true, "   ", None).as_deref(),
            Some(LINK_PULL_REQUEST_EMPTY_MESSAGE)
        );
        assert_eq!(
            link_pull_request_validation_message(true, "hello", None).as_deref(),
            Some(LINK_PULL_REQUEST_INVALID_MESSAGE)
        );
    }

    #[test]
    fn only_a_resolved_link_submits_and_summarises() {
        let link = LinkPullRequestResolution::Link(ResolvedLink {
            host: "github.com".into(),
            repository: "acme/web".into(),
            number: 42,
            url: "https://github.com/acme/web/pull/42".into(),
        });
        let error = LinkPullRequestResolution::Error("nope".into());
        assert!(link_pull_request_can_submit(Some(&link)));
        assert!(!link_pull_request_can_submit(Some(&error)));
        assert!(!link_pull_request_can_submit(None));
        assert_eq!(
            link_pull_request_summary(Some(&link)).as_deref(),
            Some("github.com/acme/web #42")
        );
        assert_eq!(link_pull_request_summary(Some(&error)), None);
    }

    #[test]
    fn candidate_picker_body_precedence() {
        assert_eq!(
            candidate_picker_body(true, Some("x"), "Failed:", 0, ""),
            CandidatePickerBody::Pending
        );
        assert_eq!(
            candidate_picker_body(false, Some("boom"), "Could not load:", 3, ""),
            CandidatePickerBody::Error("Could not load: boom".into())
        );
        assert_eq!(
            candidate_picker_body(false, None, "", 0, "ann"),
            CandidatePickerBody::Empty { no_match: true }
        );
        assert_eq!(
            candidate_picker_body(false, None, "", 0, ""),
            CandidatePickerBody::Empty { no_match: false }
        );
        assert_eq!(candidate_picker_body(false, None, "", 1, ""), CandidatePickerBody::Rows);
    }

    #[test]
    fn item_press_close_is_cancelled_and_other_changes_pass() {
        assert!(!candidate_picker_forwards_open_change(false, "item-press"));
        assert!(candidate_picker_forwards_open_change(false, "escape-key"));
        assert!(candidate_picker_forwards_open_change(true, "item-press"));
    }

    #[test]
    fn author_label_covers_ghost_name_and_login_only() {
        let actor = |name: Option<&str>| PullRequestActor {
            is_bot: None,
            login: "octocat".into(),
            name: name.map(str::to_owned),
            avatar_url: None,
        };
        assert_eq!(link_preview_author_label(None), "ghost");
        assert_eq!(
            link_preview_author_label(Some(&actor(Some("Mona")))),
            "Mona (@octocat)"
        );
        assert_eq!(link_preview_author_label(Some(&actor(Some("octocat")))), "octocat");
        assert_eq!(link_preview_author_label(Some(&actor(Some("")))), "octocat");
        assert_eq!(link_preview_author_label(Some(&actor(None))), "octocat");
    }

    #[test]
    fn preview_card_and_tooltip_gating() {
        assert!(link_preview_shows_card(true, false, false));
        assert!(link_preview_shows_card(false, true, true));
        assert!(!link_preview_shows_card(false, true, false));
        assert!(link_preview_shows_url_tooltip(true, true, false));
        assert!(!link_preview_shows_url_tooltip(true, true, true));
        assert!(link_preview_bypasses_confirm(false, false, true, false));
        assert!(!link_preview_bypasses_confirm(false, false, false, false));
    }

    #[test]
    fn thread_link_counts_and_menu_copy() {
        assert_eq!(linked_threads_label(0), "Linked threads");
        assert_eq!(linked_threads_label(1), "Linked from 1 thread");
        assert_eq!(linked_threads_label(2), "Linked from 2 threads");
        assert_eq!(linked_threads_count_text(0), "?");
        assert!(show_linked_threads_count(true, 1, false));
        assert!(show_linked_threads_count(true, 0, true));
        assert!(!show_linked_threads_count(true, 0, false));
        assert!(!show_linked_threads_count(false, 3, false));
        assert_eq!(thread_link_menu_label(true, false), "Unlink from this thread");
        assert_eq!(thread_link_menu_label(false, true), "Link to this thread");
        assert_eq!(thread_link_menu_label(false, false), "Link to thread");
        assert!(thread_links_render(true, true, false));
        assert!(!thread_links_render(false, true, true));
        assert_eq!(change_link_failure_title(true), "Could not unlink the pull request");
    }

    #[test]
    fn thread_picker_filters_archived_and_other_environments_then_sorts_by_recency() {
        let threads = [
            PickerThread {
                id: "old",
                environment_id: "env",
                title: "Fix login",
                project_id: "p1",
                archived: false,
                updated_at: "2026-10-01T00:00:00.000Z",
            },
            PickerThread {
                id: "new",
                environment_id: "env",
                title: "Login polish",
                project_id: "p2",
                archived: false,
                updated_at: "2026-10-10T00:00:00.000Z",
            },
            PickerThread {
                id: "archived",
                environment_id: "env",
                title: "Login old",
                project_id: "p1",
                archived: true,
                updated_at: "2026-10-12T00:00:00.000Z",
            },
            PickerThread {
                id: "elsewhere",
                environment_id: "other",
                title: "Login",
                project_id: "p1",
                archived: false,
                updated_at: "2026-10-13T00:00:00.000Z",
            },
        ];
        let names = |id: &str| match id {
            "p1" => Some("Web"),
            "p2" => Some("Docs"),
            _ => None,
        };
        let ids: Vec<&str> = thread_picker_candidates(&threads, "env", &names, "  LOGIN ")
            .into_iter()
            .map(|thread| thread.id)
            .collect();
        assert_eq!(ids, vec!["new", "old"]);
        // Matches on the project name too.
        let by_project: Vec<&str> = thread_picker_candidates(&threads, "env", &names, "docs")
            .into_iter()
            .map(|thread| thread.id)
            .collect();
        assert_eq!(by_project, vec!["new"]);
        assert_eq!(thread_picker_title(""), "Untitled thread");
        assert_eq!(thread_picker_title("Fix"), "Fix");
    }

    #[test]
    fn thread_panel_labels_indent_and_footer() {
        assert_eq!(thread_link_source_label(ThreadPullRequestLinkSource::Agent), "Linked by the agent");
        assert_eq!(thread_link_source_label(ThreadPullRequestLinkSource::StackDismissed), "Dismissed");
        assert_eq!(thread_link_unlink_label(ThreadPullRequestLinkSource::Stack), "Dismiss from thread");
        assert_eq!(thread_link_unlink_label(ThreadPullRequestLinkSource::Manual), "Unlink from thread");
        assert_eq!(thread_link_watch_label(true), "Stop watching");
        assert_eq!(thread_link_watch_label(false), "Watch for changes");
        assert_eq!(thread_link_row_indent_rem(0), 0.5);
        assert_eq!(thread_link_row_indent_rem(2), 3.0);
        assert_eq!(thread_link_row_indent_rem(16), 0.5 + 3.0 * 1.25);
        assert!(thread_link_is_open(None));
        assert!(thread_link_is_open(Some(PullRequestState::Open)));
        assert!(!thread_link_is_open(Some(PullRequestState::Merged)));
        assert_eq!(
            thread_link_stack_tooltip(true, "GitHub", 3),
            "GitHub stack of 3: merging a layer lands the ones below it."
        );
        assert_eq!(
            thread_link_stack_tooltip(false, "", 2),
            "2 pull requests chained by base branch."
        );
        assert!(thread_links_speed_mode(true, false, false, false));
        assert!(!thread_links_speed_mode(true, true, false, false));
        assert_eq!(thread_links_footer(2, 5, None), "2 open · 5 linked");
        assert_eq!(
            thread_links_footer(0, 1, Some("2m ago")),
            "0 open · 1 linked · synced 2m ago"
        );
        assert_eq!(
            thread_links_last_synced([
                Some("2026-10-01T00:00:00.000Z"),
                None,
                Some("2026-10-03T00:00:00.000Z"),
                Some("2026-10-02T00:00:00.000Z"),
            ]),
            Some("2026-10-03T00:00:00.000Z")
        );
        assert_eq!(thread_links_last_synced([None::<&str>]), None);
    }

    #[test]
    fn thread_project_sorts_first_and_row_project_falls_back_to_thread() {
        let mut projects = vec!["a", "thread", "b"];
        sort_projects_thread_first(&mut projects, |project| *project == "thread");
        assert_eq!(projects, vec!["thread", "a", "b"]);
        assert_eq!(thread_link_row_project_id(true, Some("found"), Some("own")), Some("found"));
        assert_eq!(thread_link_row_project_id(true, None, Some("own")), Some("own"));
        assert_eq!(thread_link_row_project_id(false, Some("found"), Some("own")), None);
        assert_eq!(thread_link_row_key("github.com", "acme/web", 7), "github.com/acme/web#7");
    }

    #[test]
    fn checkout_confirm_follows_permission_then_reference_then_resolution() {
        let base = CheckoutConfirmInput {
            is_allowed: true,
            worktree: true,
            operate_scope_granted: true,
            reference_parses: true,
            has_resolved_pull_request: true,
            has_cwd: true,
        };
        assert_eq!(checkout_confirm_step(base), CheckoutConfirmStep::Prepare);
        assert_eq!(
            checkout_confirm_step(CheckoutConfirmInput { operate_scope_granted: false, ..base }),
            CheckoutConfirmStep::Ignore
        );
        assert_eq!(
            checkout_confirm_step(CheckoutConfirmInput {
                worktree: false,
                operate_scope_granted: false,
                ..base
            }),
            CheckoutConfirmStep::Prepare
        );
        assert_eq!(
            checkout_confirm_step(CheckoutConfirmInput { reference_parses: false, ..base }),
            CheckoutConfirmStep::MarkReferenceDirty
        );
        assert_eq!(
            checkout_confirm_step(CheckoutConfirmInput { has_resolved_pull_request: false, ..base }),
            CheckoutConfirmStep::Ignore
        );
        assert_eq!(
            checkout_confirm_step(CheckoutConfirmInput { is_allowed: false, ..base }),
            CheckoutConfirmStep::Ignore
        );
    }

    #[test]
    fn checkout_buttons_and_resolving_state() {
        assert!(!checkout_button_disabled(false, true, false, true, true, false, false));
        assert!(checkout_button_disabled(true, true, false, true, true, false, false));
        assert!(checkout_button_disabled(false, true, true, true, true, true, false));
        assert!(checkout_is_resolving(true, true, false, true, true, false, false));
        assert!(checkout_is_resolving(true, true, false, false, false, false, false));
        assert!(!checkout_is_resolving(true, true, true, true, true, true, true));
        assert!(!checkout_is_resolving(false, true, false, true, true, true, true));
    }

    #[test]
    fn checkout_error_precedence_and_copy() {
        let base = || CheckoutErrorInput {
            validation: None,
            prepare_error: None,
            has_resolved_pull_request: false,
            resolution_error: None,
            action_failure: None,
            singular: "pull request",
        };
        assert_eq!(
            checkout_error_message(CheckoutErrorInput {
                validation: Some("bad".into()),
                prepare_error: Some("prep".into()),
                ..base()
            }),
            Some("bad".into())
        );
        assert_eq!(
            checkout_error_message(CheckoutErrorInput {
                resolution_error: Some("lookup failed"),
                action_failure: Some(CheckoutFailure::Other),
                ..base()
            }),
            Some("lookup failed".into())
        );
        assert_eq!(
            checkout_error_message(CheckoutErrorInput {
                has_resolved_pull_request: true,
                resolution_error: Some("lookup failed"),
                action_failure: Some(CheckoutFailure::Other),
                ..base()
            }),
            Some("Failed to prepare pull request thread.".into())
        );
        assert_eq!(
            checkout_error_message(CheckoutErrorInput {
                action_failure: Some(CheckoutFailure::Error("denied")),
                ..base()
            }),
            Some("denied".into())
        );
        assert_eq!(checkout_error_message(base()), None);
        assert_eq!(
            checkout_validation_message(true, "  ", false, "pull request").as_deref(),
            Some("Paste a pull request URL, checkout command, or enter 123 / #123.")
        );
        assert_eq!(
            checkout_validation_message(true, "x", false, "merge request").as_deref(),
            Some("Use a merge request URL, checkout command, 123, or #123.")
        );
        assert_eq!(checkout_validation_message(false, "", false, "pull request"), None);
    }

    #[test]
    fn enter_key_is_left_alone_while_composing() {
        assert_eq!(checkout_enter_key(true, 13, false, false), None);
        assert_eq!(checkout_enter_key(false, 229, false, false), None);
        assert_eq!(checkout_enter_key(false, 13, false, false), Some(true));
        assert_eq!(checkout_enter_key(false, 13, true, false), Some(false));
    }

    #[test]
    fn external_open_only_on_meta_or_ctrl() {
        assert!(should_open_pull_request_externally(true, false));
        assert!(should_open_pull_request_externally(false, true));
        assert!(!should_open_pull_request_externally(false, false));
    }
}
