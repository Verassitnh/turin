//! Port of upstream/apps/web/src/hooks/useOpenPanelPullRequestUrl.ts (URL resolution).

#![allow(dead_code)]

use turin_contracts::pull_request::{PullRequestDetail, PullRequestRef};

use crate::pull_request::pull_request_detail_logic::{
    PullRequestDetailSnapshotRef, resolve_displayed_pull_request_detail,
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
