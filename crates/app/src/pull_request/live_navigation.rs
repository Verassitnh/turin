//! Live `/_chat/pull-requests` host: starts [`super::live_source`], mounts
//! [`PullRequestsPage`] and [`PullRequestPanelHost`], and wires list selection to
//! the detail panel. Replaces [`super::demo`] in the running app (`TURIN_PULL_REQUESTS_DEMO`
//! stays for screenshots).
//!
//! Shell integration (see `docs/handoff/WIRE-pr-ui.md`):
//! ```ignore
//! let host = pull_request::live_navigation::mount_route(
//!     bus, handle, Some(open_thread), search, prefs, window, cx,
//! );
//! layout.set_main_content(Some(host.read(cx).page_view()), cx);
//! // subscribe to PullRequestsRouteEvent::OpenPanel → set_right_panel_content(panel_view)
//! ```

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Subscription, Window};
use turin_contracts::{EnvironmentId, PositiveInt, ProjectId, ThreadId, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestChecksFilter, PullRequestDraftFilter, PullRequestRef, PullRequestReviewFilter,
};

use super::list_page::{PullRequestsPage, PullRequestsPageEvent};
use super::panel_host::PullRequestPanelHost;
use super::pull_request_list_logic::{find_scoped_project, resolve_project_scope};
use super::pull_request_list_preferences::{
    PullRequestListPreferences, PreferenceStorage, read_pull_request_list_preferences,
};
use super::pull_requests_route_search::PullRequestsSearch;
use super::view_model::{PullRequestListSnapshot, PullRequestProject, PullRequestStore};

use crate::app_runtime::write::bus::CommandBus;
use crate::state::store::ClientStateHandle;

/// Shell callback when the reader opens a linked thread from a pull request surface.
pub type OpenThreadCallback = Arc<dyn Fn(&ThreadId, &mut App)>;

/// Raised for layout chrome the route does not own (right panel open/close).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullRequestsRouteEvent {
    /// A row was chosen or a deep link resolved: mount [`PullRequestsRouteHost::panel_view`].
    OpenPanel,
}

/// Holds the live store and the two surfaces the shell places in main/right panels.
pub struct PullRequestsRouteHost {
    store: Entity<PullRequestStore>,
    page: Entity<PullRequestsPage>,
    panel: Entity<PullRequestPanelHost>,
    search: PullRequestsSearch,
    deep_link_opened: Rc<Cell<bool>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestsRouteEvent> for PullRequestsRouteHost {}

impl PullRequestsRouteHost {
    /// Starts [`super::live_source`] and wires page ↔ panel. Pass `open_thread` when the shell
    /// can navigate to `/_chat/$environment/$thread` (linked threads, thread picker).
    pub fn new(
        bus: CommandBus,
        client: ClientStateHandle,
        open_thread: Option<OpenThreadCallback>,
        search: PullRequestsSearch,
        preferences: &dyn PreferenceStorage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = match open_thread {
            Some(navigate) => {
                super::live_source::start_with_navigation(bus.clone(), client, navigate, cx)
            }
            None => super::live_source::start(bus, client, cx),
        };
        if let Some(source) = store.read(cx).source() {
            source.refresh_list(cx);
        }
        let page = cx.new(|cx| PullRequestsPage::new(store.clone(), window, cx));
        let panel = cx.new(|cx| PullRequestPanelHost::new(store.clone(), cx));
        let scope = list_scope_from_search(&search, preferences);
        page.update(cx, |page, cx| page.set_scope(scope, cx));

        let deep_link_opened = Rc::new(Cell::new(false));
        let entity = cx.entity();
        let panel_for_select = panel.clone();
        let mut subscriptions = vec![cx.subscribe_in(
            &page,
            window,
            move |_, _, event: &PullRequestsPageEvent, window, cx| {
                let PullRequestsPageEvent::Select {
                    environment_id,
                    reference,
                } = event;
                panel_for_select.update(cx, |host, cx| {
                    host.open(
                        environment_id.clone(),
                        reference.clone(),
                        None,
                        window,
                        cx,
                    )
                });
                cx.emit(PullRequestsRouteEvent::OpenPanel);
            },
        )];

        let opened_for_store = deep_link_opened.clone();
        let search_for_store = search.clone();
        let panel_for_store = panel.clone();
        let store_for_store = store.clone();
        subscriptions.push(cx.observe(&store, move |_, _, cx| {
            try_open_deep_link(
                &search_for_store,
                &store_for_store,
                &panel_for_store,
                &opened_for_store,
                entity.clone(),
                cx,
            );
        }));

        let this = Self {
            store,
            page,
            panel,
            search,
            deep_link_opened: deep_link_opened.clone(),
            _subscriptions: subscriptions,
        };
        try_open_deep_link(
            &this.search,
            &this.store,
            &this.panel,
            &deep_link_opened,
            entity.clone(),
            cx,
        );
        this
    }

    pub fn store(&self) -> &Entity<PullRequestStore> {
        &self.store
    }

    pub fn page(&self) -> &Entity<PullRequestsPage> {
        &self.page
    }

    pub fn panel(&self) -> &Entity<PullRequestPanelHost> {
        &self.panel
    }

    pub fn page_view(&self) -> gpui_kit::AnyView {
        self.page.clone().into()
    }

    pub fn panel_view(&self) -> gpui_kit::AnyView {
        self.panel.clone().into()
    }

    /// Re-read route search (in-app navigation without remounting).
    pub fn apply_search(
        &mut self,
        search: PullRequestsSearch,
        preferences: &dyn PreferenceStorage,
        cx: &mut Context<Self>,
    ) {
        self.search = search.clone();
        self.deep_link_opened.set(false);
        let scope = list_scope_from_search(&search, preferences);
        self.page.update(cx, |page, cx| page.set_scope(scope, cx));
        try_open_deep_link(
            &search,
            &self.store,
            &self.panel,
            &self.deep_link_opened,
            cx.entity(),
            cx,
        );
    }
}

/// Single entry the shell calls when routing to pull requests.
pub fn mount_route(
    bus: CommandBus,
    client: ClientStateHandle,
    open_thread: Option<OpenThreadCallback>,
    search: PullRequestsSearch,
    preferences: &dyn PreferenceStorage,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PullRequestsRouteHost> {
    cx.new(|cx| {
        PullRequestsRouteHost::new(
            bus,
            client,
            open_thread,
            search,
            preferences,
            window,
            cx,
        )
    })
}

fn try_open_deep_link(
    search: &PullRequestsSearch,
    store: &Entity<PullRequestStore>,
    panel: &Entity<PullRequestPanelHost>,
    opened: &Rc<Cell<bool>>,
    entity: Entity<PullRequestsRouteHost>,
    cx: &mut App,
) {
    if opened.get() {
        return;
    }
    let Some((environment_id, reference)) =
        deep_link_target(search, store.read(cx).list())
    else {
        return;
    };
    let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        return;
    };
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |host, cx| {
                host.open(environment_id, reference, None, window, cx);
            });
        })
        .ok();
    opened.set(true);
    let _ = entity.update(cx, |_, cx| cx.emit(PullRequestsRouteEvent::OpenPanel));
}

fn list_scope_from_search(
    search: &PullRequestsSearch,
    preferences: &dyn PreferenceStorage,
) -> PullRequestListPreferences {
    let remembered = read_pull_request_list_preferences(preferences);
    PullRequestListPreferences {
        involvement: search.involvement,
        state: search.state,
        environment_id: search
            .environment_id
            .as_deref()
            .and_then(|id| EnvironmentId::new(id).ok()),
        project_id: search
            .project_id
            .as_deref()
            .and_then(|id| ProjectId::new(id).ok()),
        host: search.host.clone(),
        q: search.q.clone(),
        draft: search.draft.as_deref().map(draft_filter),
        review: search.review.as_deref().map(review_filter),
        checks: search.checks.as_deref().map(checks_filter),
        author: search.author.clone(),
        labels: search.labels.clone(),
        sort: search.sort.or(remembered.sort),
    }
}

fn draft_filter(raw: &str) -> PullRequestDraftFilter {
    match raw {
        "only" => PullRequestDraftFilter::Only,
        _ => PullRequestDraftFilter::Hide,
    }
}

fn review_filter(raw: &str) -> PullRequestReviewFilter {
    match raw {
        "approved" => PullRequestReviewFilter::Approved,
        "changes-requested" => PullRequestReviewFilter::ChangesRequested,
        "review-required" => PullRequestReviewFilter::ReviewRequired,
        _ => PullRequestReviewFilter::None,
    }
}

fn checks_filter(raw: &str) -> PullRequestChecksFilter {
    match raw {
        "failing" => PullRequestChecksFilter::Failing,
        _ => PullRequestChecksFilter::Passing,
    }
}

/// Upstream `linkedSelection`: open the pull request named in the route search once projects are known.
fn deep_link_target(
    search: &PullRequestsSearch,
    list: &PullRequestListSnapshot,
) -> Option<(EnvironmentId, PullRequestRef)> {
    let repository = search.repository.as_ref()?;
    let number = PositiveInt::new(search.number?)?;
    if !list.projects_known {
        return None;
    }
    let project_ids: Vec<&str> = list.projects.iter().map(|p| p.id.as_str()).collect();
    let linked_id = resolve_project_scope(
        search.selected_project_id.as_deref(),
        &project_ids,
        true,
    );
    let scoped_id = resolve_project_scope(search.project_id.as_deref(), &project_ids, true);
    let repository_owner = project_id_for_repository(&list.projects, repository, search)?;
    let selected_id = linked_id.or(repository_owner).or(scoped_id);
    let selected_host = search
        .selected_host
        .as_deref()
        .or(search.host.as_deref());
    let environment_hint = search
        .selected_environment_id
        .as_deref()
        .or(search.environment_id.as_deref());
    let project = find_scoped_project(
        &list.projects,
        environment_hint,
        selected_id,
        |project| project.id.as_str(),
        |project| project.environment_id.as_str(),
    )?;
    let project_id = ProjectId::new(project.id.as_str()).ok()?;
    let repository = TrimmedNonEmptyString::new(repository).ok()?;
    let mut reference = PullRequestRef {
        project_id,
        host: None,
        expected_account_id: None,
        allow_stale: None,
        repository,
        number,
    };
    if let Some(host) = selected_host {
        reference.host = TrimmedNonEmptyString::new(host).ok();
    }
    Some((project.environment_id.clone(), reference))
}

fn project_id_for_repository<'a>(
    projects: &'a [PullRequestProject],
    repository: &str,
    _search: &PullRequestsSearch,
) -> Option<&'a str> {
    let repository = repository.to_lowercase();
    projects
        .iter()
        .find(|project| {
            project
                .repository
                .as_ref()
                .is_some_and(|repo| repo.to_lowercase() == repository)
        })
        .map(|project| project.id.as_str())
}
