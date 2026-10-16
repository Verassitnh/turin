//! Port of upstream TanStack route paths under `apps/web/src/routes/*` as
//! in-app navigation state (no URL bar).

use turin_contracts::{EnvironmentId, ProjectId, ThreadId};

use super::thread_routes::{
    ThreadRouteParamsPartial, ThreadRouteTarget, resolve_thread_route_target,
};

/// Mirrors the `_chat` layout's active child route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppRoute {
    /// `/_chat/` index landing (draft bootstrap).
    ChatIndex,
    /// `/_chat/new` deep link handler.
    ChatNew { search: String },
    /// Thread or draft route (`ThreadRouteView`).
    ChatThread { target: ThreadRouteTarget },
    /// `/projects/$projectKey` redirect target.
    ProjectSettings { project_key: String },
}

impl Default for AppRoute {
    fn default() -> Self {
        Self::ChatIndex
    }
}

/// In-app back stack (`useNavigateBack` without `window.history`).
#[derive(Clone, Debug, Default)]
pub struct NavigationStack {
    routes: Vec<AppRoute>,
}

impl NavigationStack {
    pub fn current(&self) -> AppRoute {
        self.routes.last().cloned().unwrap_or_default()
    }

    pub fn push(&mut self, route: AppRoute) {
        self.routes.push(route);
    }

    pub fn replace(&mut self, route: AppRoute) {
        if self.routes.is_empty() {
            self.routes.push(route);
        } else {
            let last = self.routes.len() - 1;
            self.routes[last] = route;
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.routes.len() > 1
    }

    /// Returns to the previous route, or home when opened without app history.
    pub fn navigate_back(&mut self) -> AppRoute {
        if self.routes.len() > 1 {
            self.routes.pop();
        } else {
            self.routes.clear();
        }
        self.current()
    }

    pub fn navigate_home(&mut self) {
        self.routes.clear();
    }
}

/// Maps sidebar selection into a chat thread route.
pub fn route_from_thread(environment_id: &EnvironmentId, thread_id: &ThreadId) -> AppRoute {
    AppRoute::ChatThread {
        target: ThreadRouteTarget::Server {
            thread_ref: turin_contracts::environment::ScopedThreadRef {
                environment_id: environment_id.clone(),
                thread_id: thread_id.clone(),
            },
        },
    }
}

pub fn route_from_draft(draft_id: impl Into<String>) -> AppRoute {
    AppRoute::ChatThread {
        target: ThreadRouteTarget::Draft {
            draft_id: draft_id.into(),
        },
    }
}

/// Parses a legacy pathname-shaped string for imports and tests.
pub fn app_route_from_path(path: &str) -> Option<AppRoute> {
    let path = path.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        return Some(AppRoute::ChatIndex);
    }
    if path == "/new" || path.ends_with("/new") {
        return Some(AppRoute::ChatNew {
            search: String::new(),
        });
    }
    if let Some(rest) = path.strip_prefix("/projects/") {
        return Some(AppRoute::ProjectSettings {
            project_key: rest.to_owned(),
        });
    }
    let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match segments.as_slice() {
        [environment_id, thread_id] => resolve_thread_route_target(&ThreadRouteParamsPartial {
            environment_id: Some(environment_id.to_string()),
            thread_id: Some(thread_id.to_string()),
            draft_id: None,
        })
        .map(|target| AppRoute::ChatThread { target }),
        ["draft", draft_id] => Some(route_from_draft(*draft_id)),
        _ => None,
    }
}

/// Redirect from `/projects/$projectKey` to project settings.
pub fn project_settings_route(project_key: &str) -> AppRoute {
    AppRoute::ProjectSettings {
        project_key: project_key.to_owned(),
    }
}

#[allow(dead_code)]
pub fn scoped_project_key(environment_id: &EnvironmentId, project_id: &ProjectId) -> String {
    format!("{}:{}", environment_id.as_str(), project_id.as_str())
}
