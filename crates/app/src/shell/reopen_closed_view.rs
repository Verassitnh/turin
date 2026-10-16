#![allow(dead_code)]

//! Port of upstream/apps/web/src/reopenClosedView.ts (+ reopenClosedView.test.ts).

use turin_contracts::{EnvironmentId, ProjectId};

use crate::state::right_panel_store::{
    RightPanelStore, RightPanelSurface, ThreadRightPanelState, thread_key,
};

/// `ClosedViewEntry` (panel-tab only; browser entries are cut).
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedViewEntry {
    pub id: String,
    pub thread_ref: turin_contracts::environment::ScopedThreadRef,
    pub surface: RightPanelSurface,
}

/// `ReopenOwnerState`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReopenOwnerState {
    pub environment_known: bool,
    pub catalog_ready: bool,
    pub owner_exists: bool,
    pub shell_live: bool,
    pub panel: ThreadRightPanelState,
}

/// `planNextReopen`.
pub fn plan_next_reopen(
    entries: &[ClosedViewEntry],
    owner_state: impl Fn(&ClosedViewEntry) -> ReopenOwnerState,
) -> PlanNextReopen {
    let mut dropped = Vec::new();
    for entry in entries {
        let owner = owner_state(entry);
        if !owner.environment_known {
            if !owner.catalog_ready {
                break;
            }
            dropped.push(entry.clone());
            continue;
        }
        if !owner.owner_exists {
            if owner.shell_live {
                continue;
            }
            break;
        }
        let already_open = owner.panel.is_open
            && owner
                .panel
                .surfaces
                .iter()
                .any(|surface| surface.id() == entry.surface.id());
        if already_open {
            dropped.push(entry.clone());
            continue;
        }
        return PlanNextReopen {
            dropped,
            restore: Some(entry.clone()),
        };
    }
    PlanNextReopen {
        dropped,
        restore: None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanNextReopen {
    pub dropped: Vec<ClosedViewEntry>,
    pub restore: Option<ClosedViewEntry>,
}

/// Pull-request list search shape for restore (`pullRequestsSearchForRestore`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PullRequestsSearchLike {
    pub involvement: Option<String>,
    pub state: Option<String>,
    pub q: Option<String>,
    pub host: Option<String>,
    pub repository: Option<String>,
    pub number: Option<u64>,
    pub selected_project_id: Option<ProjectId>,
    pub selected_host: Option<String>,
    pub selected_environment_id: Option<EnvironmentId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestListPreferences {
    pub involvement: String,
    pub state: String,
}

/// `pullRequestsSearchForRestore`.
pub fn pull_requests_search_for_restore(
    previous: PullRequestsSearchLike,
    selected: Option<&RightPanelSurface>,
) -> PullRequestsSearchForRestore {
    let PullRequestsSearchLike {
        involvement,
        state,
        q,
        host,
        repository: _repository,
        number: _number,
        selected_project_id: _project_id,
        selected_host: _host,
        selected_environment_id: _environment_id,
    } = previous;
    let mut result = PullRequestsSearchForRestore {
        involvement: involvement.unwrap_or_else(|| "all".into()),
        state: state.unwrap_or_else(|| "open".into()),
        q,
        host,
        repository: None,
        number: None,
        selected_project_id: None,
        selected_host: None,
        selected_environment_id: None,
    };
    if let Some(RightPanelSurface::PullRequest(surface)) = selected {
        result.repository = Some(surface.target.repository.clone());
        result.number = Some(surface.target.number);
        if let Ok(project_id) = ProjectId::new(&surface.target.project_id) {
            result.selected_project_id = Some(project_id);
        }
        if let Some(host) = &surface.target.host {
            result.selected_host = Some(host.clone());
        }
        if let Some(environment_id) = &surface.target.environment_id {
            result.selected_environment_id = EnvironmentId::new(environment_id).ok();
        }
    }
    result
}

#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestsSearchForRestore {
    pub involvement: String,
    pub state: String,
    pub q: Option<String>,
    pub host: Option<String>,
    pub repository: Option<String>,
    pub number: Option<u64>,
    pub selected_project_id: Option<ProjectId>,
    pub selected_host: Option<String>,
    pub selected_environment_id: Option<EnvironmentId>,
}

/// Reopens a closed panel tab into `panels` (`reopenClosedView`, panel-tab branch).
pub fn reopen_closed_panel_tab(
    panels: &mut RightPanelStore,
    entry: &ClosedViewEntry,
    workspace_available: bool,
) -> bool {
    let thread = &entry.thread_ref;
    match &entry.surface {
        RightPanelSurface::File(file) if file.attachment.is_none() && !workspace_available => {
            return false;
        }
        _ => {}
    }
    match &entry.surface {
        RightPanelSurface::File(file) => {
            if let Some(attachment) = &file.attachment {
                panels.open_attachment(thread, attachment.clone());
            } else {
                panels.open_file(thread, &file.relative_path, file.reveal_line);
            }
        }
        RightPanelSurface::PullRequest(surface) => {
            panels.open_pull_request(thread, &surface.target);
        }
        RightPanelSurface::Diff => {
            panels.open(thread, crate::state::right_panel_store::SingletonKind::Diff);
        }
        RightPanelSurface::PullRequests => {
            panels.open(
                thread,
                crate::state::right_panel_store::SingletonKind::PullRequests,
            );
        }
    }
    true
}

/// Builds closed-view entries from the right-panel store log.
pub fn closed_view_entries_from_store(store: &RightPanelStore) -> Vec<ClosedViewEntry> {
    store
        .closed_panel_tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| ClosedViewEntry {
            id: format!("closed-{index}-{}", thread_key(&tab.thread_ref)),
            thread_ref: tab.thread_ref.clone(),
            surface: tab.surface.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::right_panel_store::{
        FileSurface, PullRequestTarget, RightPanelSurface, ThreadRightPanelState,
        pull_request_surface,
    };
    use turin_contracts::environment::ScopedThreadRef;
    use turin_contracts::{EnvironmentId, ThreadId};

    fn thread_ref() -> ScopedThreadRef {
        ScopedThreadRef {
            environment_id: EnvironmentId::new("local").unwrap(),
            thread_id: ThreadId::new("thread-1").unwrap(),
        }
    }

    fn entry(id: &str, surface: RightPanelSurface) -> ClosedViewEntry {
        ClosedViewEntry {
            id: id.into(),
            thread_ref: thread_ref(),
            surface,
        }
    }

    fn empty_panel() -> ThreadRightPanelState {
        ThreadRightPanelState {
            is_open: false,
            active_surface_id: None,
            surfaces: vec![],
            maximized: false,
        }
    }

    fn known(panel: ThreadRightPanelState) -> ReopenOwnerState {
        ReopenOwnerState {
            environment_known: true,
            catalog_ready: true,
            owner_exists: true,
            shell_live: true,
            panel,
        }
    }

    #[test]
    fn plan_next_reopen_scenarios() {
        let entries = vec![
            entry("a", RightPanelSurface::Diff),
            entry("b", RightPanelSurface::PullRequests),
        ];
        let plan = plan_next_reopen(&entries, |_| known(empty_panel()));
        assert!(plan.dropped.is_empty());
        assert_eq!(plan.restore.as_ref().map(|e| e.id.as_str()), Some("a"));

        let plan = plan_next_reopen(&entries, |entry| {
            if entry.id == "a" {
                ReopenOwnerState {
                    environment_known: false,
                    catalog_ready: true,
                    ..known(empty_panel())
                }
            } else {
                known(empty_panel())
            }
        });
        assert_eq!(plan.dropped.len(), 1);
        assert_eq!(plan.restore.as_ref().map(|e| e.id.as_str()), Some("b"));
    }

    #[test]
    fn pull_requests_search_restore_cases() {
        let surface = RightPanelSurface::PullRequest(pull_request_surface(&PullRequestTarget {
            environment_id: Some("remote".into()),
            project_id: "project-1".into(),
            host: Some("github.example.com".into()),
            repository: "owner/repo".into(),
            number: 42,
            url: None,
        }));
        let search = pull_requests_search_for_restore(
            PullRequestsSearchLike {
                involvement: Some("reviewing".into()),
                state: Some("open".into()),
                q: Some("mine".into()),
                host: Some("filter.example.com".into()),
                selected_host: Some("old".into()),
                number: Some(1),
                ..Default::default()
            },
            Some(&surface),
        );
        assert_eq!(search.involvement, "reviewing");
        assert_eq!(search.repository.as_deref(), Some("owner/repo"));
        assert_eq!(search.number, Some(42));
        let cleared = pull_requests_search_for_restore(
            PullRequestsSearchLike {
                repository: Some("old/repo".into()),
                number: Some(7),
                ..Default::default()
            },
            None,
        );
        assert_eq!(cleared.involvement, "all");
    }

    #[test]
    fn reopen_file_tab() {
        let mut store = RightPanelStore::default();
        let entry = entry(
            "file",
            RightPanelSurface::File(FileSurface {
                id: "file:src/app.ts".into(),
                relative_path: "src/app.ts".into(),
                reveal_line: Some(18.),
                reveal_request_id: 1,
                attachment: None,
            }),
        );
        assert!(reopen_closed_panel_tab(&mut store, &entry, true));
        let state = store.thread_state(&thread_ref());
        assert!(state.is_open);
        assert_eq!(state.surfaces.len(), 1);
    }
}
