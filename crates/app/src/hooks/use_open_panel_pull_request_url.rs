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

/// `useOpenPanelPullRequestUrl`'s result: the URL the host reported, else the one the surface was
/// opened with, else a GitHub URL built from the project's remote.
pub fn open_panel_pull_request_url(
    surface: Option<&RightPanelSurface>,
    supports_multiple_pull_requests: bool,
    project_identity: Option<&RepositoryIdentity>,
    live: impl Fn(&PullRequestRef) -> Option<PullRequestDetail>,
    cached: impl Fn(&PullRequestRef) -> Option<PullRequestDetail>,
) -> Option<String> {
    let reference = open_panel_pull_request_reference(
        surface,
        supports_multiple_pull_requests,
        project_identity,
    )?;
    let requested_url = match surface {
        Some(RightPanelSurface::PullRequest(surface)) => surface.target.url.as_deref(),
        _ => None,
    };
    resolve_open_panel_pull_request_url(
        live(&reference).as_ref(),
        cached(&reference).as_ref(),
        &reference,
        requested_url,
        github_pull_request_browser_url(
            project_identity,
            reference.repository.as_str(),
            reference.number.get(),
        ),
    )
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
        assert_eq!(url.as_deref(), Some("https://github.com/acme/turin/pull/7"));
        assert_eq!(
            open_panel_pull_request_url(Some(&surface(None)), true, None, |_| None, |_| None),
            None
        );
    }
}
