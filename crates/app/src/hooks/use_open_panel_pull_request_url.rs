//! Port of upstream/apps/web/src/hooks/useOpenPanelPullRequestUrl.ts: the URL of the pull request
//! the thread's active right-panel surface shows, for "open on the host" and the palette.
//!
//! [`open_panel_pull_request_reference`] is the hook's `reference` memo (the surface, resolved to a
//! host where the environment routes by host) and [`open_panel_pull_request_url`] is the whole
//! hook over closures for the two reads — the live detail and the stored snapshot — that
//! upstream makes with `useEnvironmentQuery` and `localStorage`.

#![allow(dead_code)]

use turin_contracts::ProjectId;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::pull_request::{PullRequestDetail, PullRequestRef};
use turin_contracts::thread_pull_request::ThreadPullRequestLink;
use turin_shared::thread_reference::{
    ThreadReferenceCopyTarget, resolve_thread_reference_copy_target,
};

use crate::state::right_panel_store::{PullRequestTarget, RightPanelSurface};
use crate::state::support::change_request_url::github_pull_request_browser_url;

use crate::pull_request::pull_request_detail_logic::{
    PullRequestDetailSnapshotRef, resolve_displayed_pull_request_detail,
    resolve_pull_request_reference_host,
};

pub fn resolve_open_panel_pull_request_url(
    live: Option<&PullRequestDetail>,
    cached: Option<&PullRequestDetail>,
    reference: &PullRequestRef,
    requested_url: Option<&str>,
    browser_fallback: Option<String>,
) -> Option<String> {
    let snapshot_ref = PullRequestDetailSnapshotRef {
        host: reference.host.as_ref().map(|host| host.as_str()),
        project_id: reference.project_id.as_str(),
        repository: reference.repository.as_str(),
        number: reference.number.get(),
    };
    resolve_displayed_pull_request_detail(live, cached, snapshot_ref)
        .map(|detail| detail.url.as_str().to_owned())
        .or_else(|| requested_url.map(str::to_string))
        .or(browser_fallback)
}

/// The pull request the active surface asks for: only a `pull-request` surface does, and the
/// reference carries its host only on environments that route by host (`supportsMultiplePullRequests`).
pub fn open_panel_pull_request_reference(
    surface: Option<&RightPanelSurface>,
    supports_multiple_pull_requests: bool,
    project_identity: Option<&RepositoryIdentity>,
) -> Option<PullRequestRef> {
    let RightPanelSurface::PullRequest(surface) = surface? else {
        return None;
    };
    let PullRequestTarget {
        project_id,
        host,
        repository,
        number,
        ..
    } = &surface.target;
    let base = serde_json::from_value::<PullRequestRef>(serde_json::json!({
        "projectId": project_id,
        "repository": repository,
        "number": number,
    }))
    .ok()?;
    if !supports_multiple_pull_requests {
        return Some(base);
    }
    let with_host = PullRequestRef {
        host: host
            .as_deref()
            .and_then(|host| serde_json::from_value(serde_json::json!(host)).ok()),
        ..base
    };
    Some(resolve_pull_request_reference_host(
        &with_host,
        project_identity,
    ))
}

/// `useOpenPanelPullRequestUrl`'s result, in the three states upstream gives it: `None` when the
/// active surface is no pull request (`undefined`), `Some(None)` when it is one whose URL is
/// unknown (`null`), and `Some(Some(url))`: the URL the host reported, else the one the surface was
/// opened with, else a GitHub URL built from the project's remote.
pub fn open_panel_pull_request_url(
    surface: Option<&RightPanelSurface>,
    supports_multiple_pull_requests: bool,
    project_identity: Option<&RepositoryIdentity>,
    live: impl Fn(&PullRequestRef) -> Option<PullRequestDetail>,
    cached: impl Fn(&PullRequestRef) -> Option<PullRequestDetail>,
) -> Option<Option<String>> {
    let reference = open_panel_pull_request_reference(
        surface,
        supports_multiple_pull_requests,
        project_identity,
    )?;
    let requested_url = match surface {
        Some(RightPanelSurface::PullRequest(surface)) => surface.target.url.as_deref(),
        _ => None,
    };
    Some(resolve_open_panel_pull_request_url(
        live(&reference).as_ref(),
        cached(&reference).as_ref(),
        &reference,
        requested_url,
        github_pull_request_browser_url(
            project_identity,
            reference.repository.as_str(),
            reference.number.get(),
        ),
    ))
}

/// The copy-reference target of the command palette (`CommandPalette.tsx`): the pull requests
/// page has no routed thread, so the reference is the open panel's pull request or nothing; a
/// thread route copies the open panel's pull request, else the thread's own, else its id.
pub fn palette_reference_copy_target(
    on_pull_requests_page: bool,
    reference_thread_id: Option<&str>,
    open_panel_url: Option<&Option<String>>,
    pull_requests: &[ThreadPullRequestLink],
    linked_pull_request_url: Option<&str>,
) -> Option<ThreadReferenceCopyTarget> {
    let thread_id = reference_thread_id?;
    let open_url = open_panel_url.map(Option::as_deref);
    if on_pull_requests_page && open_url.flatten().is_none() {
        return None;
    }
    resolve_thread_reference_copy_target(
        thread_id,
        open_url,
        pull_requests,
        linked_pull_request_url,
    )
}

/// The copy-reference target of the chat view (`ChatView.tsx`): only a server thread has one.
pub fn chat_reference_copy_target(
    thread_id: Option<&str>,
    is_server_thread: bool,
    open_panel_url: Option<&Option<String>>,
    pull_requests: &[ThreadPullRequestLink],
    linked_pull_request_url: Option<&str>,
) -> Option<ThreadReferenceCopyTarget> {
    let thread_id = thread_id.filter(|_| is_server_thread)?;
    resolve_thread_reference_copy_target(
        thread_id,
        open_panel_url.map(Option::as_deref),
        pull_requests,
        linked_pull_request_url,
    )
}

/// The pull requests route's copy shortcut (`copyPullRequestFromShortcut`): the link of the open
/// panel's pull request, when it has one.
pub fn pull_request_link_for_shortcut(open_panel_url: Option<&Option<String>>) -> Option<&str> {
    open_panel_url?.as_deref()
}

/// The project id a surface names, for the caller's project lookup (`useProject`).
pub fn open_panel_project_id(surface: Option<&RightPanelSurface>) -> Option<ProjectId> {
    match surface? {
        RightPanelSurface::PullRequest(surface) => {
            ProjectId::new(surface.target.project_id.as_str()).ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::right_panel_store::pull_request_surface;
    use turin_contracts::PositiveInt;

    fn surface(url: Option<&str>) -> RightPanelSurface {
        RightPanelSurface::PullRequest(pull_request_surface(&PullRequestTarget {
            environment_id: None,
            project_id: "project-1".to_owned(),
            host: Some("github.com".to_owned()),
            repository: "acme/turin".to_owned(),
            number: 7,
            url: url.map(str::to_owned),
        }))
    }

    #[test]
    fn only_a_pull_request_surface_names_a_pull_request() {
        assert!(open_panel_pull_request_reference(None, true, None).is_none());
        assert!(
            open_panel_pull_request_reference(Some(&RightPanelSurface::Diff), true, None).is_none()
        );
        let reference =
            open_panel_pull_request_reference(Some(&surface(None)), false, None).unwrap();
        assert_eq!(reference.number, PositiveInt::new(7).unwrap());
        // Without host routing the reference carries no host.
        assert!(reference.host.is_none());
    }

    #[test]
    fn host_routing_keeps_the_surfaces_host() {
        let reference =
            open_panel_pull_request_reference(Some(&surface(None)), true, None).unwrap();
        assert_eq!(
            reference.host.as_ref().map(|host| host.as_str()),
            Some("github.com")
        );
    }

    #[test]
    fn the_url_the_surface_was_opened_with_stands_in_for_a_detail_not_yet_read() {
        let url = open_panel_pull_request_url(
            Some(&surface(Some("https://github.com/acme/turin/pull/7"))),
            true,
            None,
            |_| None,
            |_| None,
        );
        assert_eq!(
            url,
            Some(Some("https://github.com/acme/turin/pull/7".to_owned()))
        );
        // Open, but nothing says where it lives: known to be a pull request, URL unknown.
        assert_eq!(
            open_panel_pull_request_url(Some(&surface(None)), true, None, |_| None, |_| None),
            Some(None)
        );
        // No pull request surface at all.
        assert_eq!(
            open_panel_pull_request_url(None, true, None, |_| None, |_| None),
            None
        );
    }

    const URL: &str = "https://github.com/acme/turin/pull/7";

    #[test]
    fn the_palette_copies_the_open_panels_link_and_never_a_thread_id_on_the_pull_requests_page() {
        let open = Some(URL.to_owned());
        let target =
            palette_reference_copy_target(true, Some("pr-panel"), Some(&open), &[], None).unwrap();
        assert_eq!(target.value, URL);
        // On the page with no URL to copy, there is no entry at all.
        assert_eq!(
            palette_reference_copy_target(true, Some("pr-panel"), Some(&None), &[], None),
            None
        );
        assert_eq!(
            palette_reference_copy_target(true, Some("pr-panel"), None, &[], None),
            None
        );
        assert_eq!(
            palette_reference_copy_target(false, None, None, &[], None),
            None
        );
    }

    #[test]
    fn a_thread_route_falls_back_to_the_linked_link_then_the_thread_id() {
        let linked =
            palette_reference_copy_target(false, Some("t1"), None, &[], Some(URL)).unwrap();
        assert_eq!(linked.value, URL);
        let id = palette_reference_copy_target(false, Some("t1"), None, &[], None).unwrap();
        assert_eq!(id.value, "t1");
    }

    #[test]
    fn only_a_server_thread_has_a_chat_reference() {
        assert_eq!(
            chat_reference_copy_target(Some("t1"), false, None, &[], None),
            None
        );
        assert_eq!(
            chat_reference_copy_target(None, true, None, &[], None),
            None
        );
        let target = chat_reference_copy_target(Some("t1"), true, None, &[], None).unwrap();
        assert_eq!(target.value, "t1");
    }

    #[test]
    fn the_route_shortcut_copies_only_a_known_link() {
        let open = Some(URL.to_owned());
        assert_eq!(pull_request_link_for_shortcut(Some(&open)), Some(URL));
        assert_eq!(pull_request_link_for_shortcut(Some(&None)), None);
        assert_eq!(pull_request_link_for_shortcut(None), None);
    }
}
