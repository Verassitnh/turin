//! Port of the list half of upstream/apps/web/src/routes/_chat.pull-requests.tsx: the
//! `PullRequestsColumn` (page header, search / sort / filters / provider / refresh controls) and the
//! list body (groups, rows, ghosts, empty and unavailable states).
//!
//! The route keeps its scope in the URL search; here it is `PullRequestListPreferences` on the
//! page. Rows come from the [`PullRequestStore`]; the narrowing, grouping and sorting are
//! upstream's pure functions applied in the route's order.
//!
//! Speed mode (hold shift) offers the row actions of `PullRequestSpeedActions`; Close can be
//! dragged down the list to close a run of rows (`list_close_sweep`). What an action did shows on
//! its row at once (`overrides`) until a read of the list agrees.
//!
//! Not ported: the condensed topbar that replaces the controls once they scroll away, line-count
//! (stats) lazy reads, the right-panel surface store (the host of this page opens the detail
//! panel on [`PullRequestsPageEvent::Select`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, App, AppContext as _, Context, DispatchPhase, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, canvas, div, px, rems,
};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestInvolvement, PullRequestListFilters, PullRequestListState,
    PullRequestRef, PullRequestState,
};

use super::action_runner::{
    MergeMethodSettings, PullRequestActionRunner, PullRequestActionRunnerEvent, entry_key,
};
use super::list_close_sweep::CloseSweep;
use super::list_filters::{
    FilterOption, FiltersMenuInput, OnScopeChange, ScopeChange, compact_filter_menu, filters_menu,
    involvement_tabs, sort_options, state_tabs,
};
use super::list_row::{PullRequestRow, RowBounds, RowSpeed};
use super::list_states::{
    PullRequestListEmptyState, PullRequestsUnavailableState, group_header, list_ghost,
};
use super::presentation_view::Spinner;
use super::pull_request_list_logic::{
    EnvironmentPullRequestEntry, PullRequestGroup, PullRequestListOverride,
    apply_pull_request_overrides, collect_pull_request_list_facets,
    filter_pull_requests_by_involvement, group_pull_requests_by_involvement,
    matches_pull_request_filters, matches_pull_request_query, parse_pull_request_query,
    pull_request_entry_key, pull_request_entry_key_of, pull_request_entry_viewer,
    pull_request_host_actions, pull_request_override_after_action, rank_pull_request_matches,
    score_pull_request_match, settle_pull_request_overrides, sort_pull_request_groups,
};
use super::pull_request_list_preferences::{PullRequestListPreferences, PullRequestListSort};
use super::toast::render_toasts;
use super::view_model::PullRequestStore;
use crate::components::drag_region::DragRegion as _;
use crate::components::sidebar::sidebar_logic::sidebar_thread_key_at_y;
use crate::components::sidebar::sidebar_pointer::Point as PointerPoint;
use crate::components::workspace_breadcrumb::{WorkspaceBreadcrumb, WorkspaceBreadcrumbItem};
use crate::components::workspace_titlebar::workspace_titlebar_content_left;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS, WORKSPACE_TOPBAR_HEIGHT};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, scroll_area};

/// Ten is the floor the ranking gives a row whose own fields say nothing about the search.
const MATCHED_ELSEWHERE_SCORE: u8 = 10;

/// Raised when the reader opens a row.
#[derive(Clone, Debug)]
pub enum PullRequestsPageEvent {
    Select {
        environment_id: EnvironmentId,
        reference: PullRequestRef,
    },
}

pub struct PullRequestsPage {
    store: Entity<PullRequestStore>,
    scope: PullRequestListPreferences,
    search: Entity<InputState>,
    selected_key: Option<String>,
    sidebar_open: bool,
    /// Width of the list column, for the row's label slots.
    column_width: f32,
    /// `speedMode`: shift held on its own.
    speed_mode: bool,
    runner: Entity<PullRequestActionRunner>,
    /// What the reader just did to a row, shown before any host confirms it.
    overrides: IndexMap<String, PullRequestListOverride>,
    override_token: u64,
    /// Where each row last laid out, for the close sweep.
    row_bounds: RowBounds,
    sweep: Option<CloseSweep>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PullRequestsPageEvent> for PullRequestsPage {}

/// A window-level pointer event forwarded to the live close sweep.
enum SweepPointer {
    Moved {
        position: PointerPoint,
        primary: bool,
    },
    Released {
        position: PointerPoint,
    },
    Cancelled,
}

impl PullRequestsPage {
    pub fn new(
        store: Entity<PullRequestStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search pull requests, or label:bug")
        });
        let runner = cx.new(|cx| {
            PullRequestActionRunner::new(store.clone(), MergeMethodSettings::default(), cx)
        });
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| {
                this.settle_overrides(cx);
                cx.notify();
            }),
            cx.observe(&runner, |_, _, cx| cx.notify()),
            cx.subscribe(&runner, |this, _, event, cx| {
                let PullRequestActionRunnerEvent::Acted { entry, action } = event;
                this.acted(entry, *action, cx);
            }),
            cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value().to_string();
                    ScopeChange::Query(text).apply(&mut this.scope);
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            scope: PullRequestListPreferences::default(),
            search,
            selected_key: None,
            sidebar_open: true,
            column_width: 720.,
            speed_mode: false,
            runner,
            overrides: IndexMap::new(),
            override_token: 0,
            row_bounds: Rc::new(RefCell::new(HashMap::new())),
            sweep: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn scope(&self) -> &PullRequestListPreferences {
        &self.scope
    }

    /// Dev hook: start from a given scope (screenshots).
    pub fn set_scope(&mut self, scope: PullRequestListPreferences, cx: &mut Context<Self>) {
        self.scope = scope;
        cx.notify();
    }

    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.sidebar_open = open;
        cx.notify();
    }

    pub fn set_column_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if (self.column_width - width).abs() > 0.5 {
            self.column_width = width;
            cx.notify();
        }
    }

    pub fn select(&mut self, entry: &EnvironmentPullRequestEntry, cx: &mut Context<Self>) {
        self.selected_key = Some(pull_request_entry_key(
            &entry.entry,
            Some(entry.environment_id.as_str()),
        ));
        cx.emit(PullRequestsPageEvent::Select {
            environment_id: entry.environment_id.clone(),
            reference: PullRequestRef {
                project_id: entry.project_id.clone(),
                host: Some(entry.host.clone()),
                expected_account_id: None,
                allow_stale: None,
                repository: entry.repository.clone(),
                number: entry.number,
            },
        });
        cx.notify();
    }

    /// A stack layer was chosen in a row's stack menu: open it like a row.
    pub fn select_reference(
        &mut self,
        environment_id: EnvironmentId,
        reference: PullRequestRef,
        cx: &mut Context<Self>,
    ) {
        cx.emit(PullRequestsPageEvent::Select {
            environment_id,
            reference,
        });
        cx.notify();
    }

    /// `speedActionRef.current`: writes the outcome of an accepted action onto its row.
    fn acted(
        &mut self,
        entry: &EnvironmentPullRequestEntry,
        action: PullRequestAction,
        cx: &mut Context<Self>,
    ) {
        // Some hosts accept a merge before it completes. Let the next host read declare it merged.
        if action == PullRequestAction::Merge {
            return;
        }
        self.override_token += 1;
        if let Some(applied) = pull_request_override_after_action(
            &entry.entry,
            action,
            crate::timestamp_format::now_ms(),
            self.override_token,
        ) {
            self.overrides.insert(entry_key(entry), applied);
            cx.notify();
        }
    }

    /// The host's word outranks the reader's, once it has actually said it: an override is cleared
    /// by an answer that agrees with it, not by any answer that happens to land.
    fn settle_overrides(&mut self, cx: &App) {
        if self.overrides.is_empty() {
            return;
        }
        let store = self.store.read(cx);
        self.overrides = settle_pull_request_overrides(
            &self.overrides,
            store.entries(),
            pull_request_entry_key_of,
            crate::timestamp_format::now_ms(),
        );
    }

    /// `modifiers.shiftKey && !metaKey && !ctrlKey && !altKey`.
    pub fn set_modifiers(&mut self, modifiers: &gpui_kit::Modifiers, cx: &mut Context<Self>) {
        let speed = modifiers.shift && !modifiers.platform && !modifiers.control && !modifiers.alt;
        if speed != self.speed_mode {
            self.speed_mode = speed;
            cx.notify();
        }
    }

    pub fn speed_mode(&self) -> bool {
        self.speed_mode
    }

    /// `startCloseSweep`: a press on a row's Close in speed mode.
    fn start_close_sweep(
        &mut self,
        entry: &EnvironmentPullRequestEntry,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let origin_key = entry_key(entry);
        if self.runner.read(cx).closing(&origin_key) {
            return;
        }
        let (groups, _) = self.display_groups(cx);
        let Some(group) = groups
            .iter()
            .find(|group| group.entries.iter().any(|row| entry_key(row) == origin_key))
        else {
            return;
        };
        let host_actions_of = self.host_actions_of(cx);
        let runner = self.runner.read(cx);
        let closable: HashSet<String> = group
            .entries
            .iter()
            .filter(|row| {
                let key = entry_key(row);
                row.state == PullRequestState::Open
                    && host_actions_of(&row.entry).contains(&PullRequestAction::Close)
                    && !runner.closing(&key)
                    && !runner.action_pending(&key)
            })
            .map(entry_key)
            .collect();
        let ordered_keys: Vec<String> = groups
            .iter()
            .flat_map(|group| group.entries.iter().map(entry_key))
            .collect();
        self.sweep = Some(CloseSweep::new(
            origin_key,
            PointerPoint::new(f32::from(event.position.x), f32::from(event.position.y)),
            ordered_keys,
            closable,
        ));
        cx.notify();
    }

    fn host_actions_of(
        &self,
        cx: &App,
    ) -> impl Fn(&turin_contracts::pull_request::PullRequestListEntry) -> HashSet<PullRequestAction>
    + use<> {
        let providers = self
            .store
            .read(cx)
            .list()
            .merged
            .as_ref()
            .map(|merged| merged.providers.clone())
            .unwrap_or_default();
        pull_request_host_actions(&providers)
    }

    /// The tops of the rows laid out, in list order (`[data-pull-request-key]` rects).
    fn row_tops(&self, sweep_order: &[String]) -> Vec<(String, f32)> {
        let bounds = self.row_bounds.borrow();
        sweep_order
            .iter()
            .filter_map(|key| {
                bounds
                    .get(key)
                    .map(|bounds| (key.clone(), f32::from(bounds.origin.y)))
            })
            .collect()
    }

    fn sweep_pointer(&mut self, event: SweepPointer, cx: &mut Context<Self>) {
        let Some(mut sweep) = self.sweep.take() else {
            return;
        };
        if sweep.is_finished() {
            self.sweep = Some(sweep);
            return;
        }
        let order = self.sweep_order(cx);
        let rows = self.row_tops(&order);
        let row_at = |y: f32| sidebar_thread_key_at_y(&rows, y, None);
        let progress = match event {
            SweepPointer::Moved { position, primary } => {
                sweep.pointer_moved(position, primary, row_at)
            }
            SweepPointer::Released { position } => sweep.pointer_released(position, row_at),
            SweepPointer::Cancelled => sweep.cancel(),
        };
        // Kept after it ends so the release click can be told from a plain one.
        self.sweep = Some(sweep);
        if let Some(batch) = progress.close {
            let (groups, _) = self.display_groups(cx);
            let by_key: HashMap<String, EnvironmentPullRequestEntry> = groups
                .into_iter()
                .flat_map(|group| group.entries)
                .map(|entry| (entry_key(&entry), entry))
                .collect();
            let entries: Vec<EnvironmentPullRequestEntry> = batch
                .iter()
                .filter_map(|key| by_key.get(key).cloned())
                .collect();
            self.runner
                .update(cx, |runner, cx| runner.close_batch(&entries, cx));
        }
        if progress.changed || progress.done {
            cx.notify();
        }
    }

    fn sweep_order(&self, cx: &App) -> Vec<String> {
        let (groups, _) = self.display_groups(cx);
        groups
            .iter()
            .flat_map(|group| group.entries.iter().map(entry_key))
            .collect()
    }

    /// The click that follows a sweep's release is the sweep's own.
    fn consume_sweep_click(&mut self) -> bool {
        match self.sweep.take() {
            Some(mut sweep) => {
                let consumed = sweep.consume_click();
                if !sweep.is_finished() {
                    self.sweep = Some(sweep);
                }
                consumed
            }
            None => false,
        }
    }

    /// Window-level pointer and Escape listeners while a sweep is live (upstream: document
    /// listeners owned by the sensor).
    fn sweep_listeners(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let live = self
            .sweep
            .as_ref()
            .is_some_and(|sweep| !sweep.is_finished());
        if !live {
            return None;
        }
        let page = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let on_move = page.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        let position = PointerPoint::new(
                            f32::from(event.position.x),
                            f32::from(event.position.y),
                        );
                        let primary = event.pressed_button == Some(MouseButton::Left);
                        on_move
                            .update(cx, |this, cx| {
                                this.sweep_pointer(SweepPointer::Moved { position, primary }, cx)
                            })
                            .ok();
                    });
                    let on_up = page.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                            return;
                        }
                        let position = PointerPoint::new(
                            f32::from(event.position.x),
                            f32::from(event.position.y),
                        );
                        on_up
                            .update(cx, |this, cx| {
                                this.sweep_pointer(SweepPointer::Released { position }, cx)
                            })
                            .ok();
                    });
                    let on_key = page.clone();
                    window.on_key_event(move |event: &KeyDownEvent, phase, _, cx| {
                        if phase != DispatchPhase::Capture || event.keystroke.key != "escape" {
                            return;
                        }
                        on_key
                            .update(cx, |this, cx| {
                                this.sweep_pointer(SweepPointer::Cancelled, cx)
                            })
                            .ok();
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected_key = None;
        cx.notify();
    }

    fn apply(&mut self, change: ScopeChange, cx: &mut Context<Self>) {
        change.apply(&mut self.scope);
        cx.notify();
    }

    fn on_change(&self, cx: &mut Context<Self>) -> OnScopeChange {
        let page = cx.entity().downgrade();
        Rc::new(move |change, _, cx| {
            page.update(cx, |this, cx| this.apply(change, cx)).ok();
        })
    }

    /// The route's pipeline from the loaded rows to the groups on screen.
    fn display_groups(
        &self,
        cx: &App,
    ) -> (Vec<PullRequestGroup<EnvironmentPullRequestEntry>>, String) {
        let store = self.store.read(cx);
        let Some(merged) = store.list().merged.as_ref() else {
            return (Vec::new(), String::new());
        };
        let parsed = parse_pull_request_query(self.scope.q.as_deref().unwrap_or(""));
        let local_filters = self.local_filters(&parsed.filters);
        let viewers = &merged.viewers;
        // Rows narrowed to the state / project / host the page is asking about.
        let narrowed: Vec<EnvironmentPullRequestEntry> = merged
            .entries
            .iter()
            .filter(|entry| {
                (self.scope.state == PullRequestListState::All
                    || entry.state
                        == super::pull_request_list_logic::list_state_to_state(self.scope.state))
                    && self
                        .scope
                        .project_id
                        .as_ref()
                        .is_none_or(|project| entry.project_id.as_str() == project.as_str())
                    && self
                        .scope
                        .host
                        .as_deref()
                        .is_none_or(|host| entry.host.as_str() == host)
            })
            .cloned()
            .collect();
        let by_involvement =
            filter_pull_requests_by_involvement(&narrowed, viewers, self.scope.involvement);
        let mut entries: Vec<EnvironmentPullRequestEntry> = by_involvement
            .into_iter()
            .filter(|entry| {
                matches_pull_request_filters(
                    entry,
                    &local_filters,
                    pull_request_entry_viewer(entry, viewers).as_deref(),
                ) && (parsed.text.is_empty() || matches_pull_request_query(entry, &parsed.text))
            })
            .collect();
        // Newest update first once nothing is typed; a search keeps its relevance order.
        if parsed.text.is_empty() {
            entries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        } else {
            entries = rank_pull_request_matches(&entries, &parsed.text);
        }
        let groups = if self.scope.involvement != PullRequestInvolvement::All {
            vec![PullRequestGroup {
                key: super::pull_request_list_logic::PullRequestGroupKey::Others,
                label: String::new(),
                entries,
            }]
        } else {
            group_pull_requests_by_involvement(&entries, viewers)
        };
        // The reader's pending answers go on here, after grouping: a row closed a moment ago has
        // to leave whichever group it was in. A row whose draft flag just changed has to pass the
        // local filters again.
        let groups: Vec<PullRequestGroup<EnvironmentPullRequestEntry>> = groups
            .into_iter()
            .map(|group| {
                let applied = apply_pull_request_overrides(
                    &group.entries,
                    &self.overrides,
                    pull_request_entry_key_of,
                    self.scope.state,
                );
                let entries = if self.overrides.is_empty() {
                    applied
                } else {
                    applied
                        .into_iter()
                        .filter(|entry| {
                            !self.overrides.contains_key(&entry_key(entry))
                                || matches_pull_request_filters(
                                    entry,
                                    &local_filters,
                                    pull_request_entry_viewer(entry, viewers).as_deref(),
                                )
                        })
                        .collect()
                };
                PullRequestGroup { entries, ..group }
            })
            .collect();
        let sorted = sort_pull_request_groups(
            &groups,
            self.scope.sort.unwrap_or(PullRequestListSort::Ready),
            &parsed.text,
            |entry: &EnvironmentPullRequestEntry| entry.additions + entry.deletions > 0,
            self.scope.involvement,
        );
        (sorted, parsed.text)
    }

    /// Filters the page's menus hold, plus those typed into the query.
    fn local_filters(&self, typed: &PullRequestListFilters) -> PullRequestListFilters {
        let mut filters = typed.clone();
        filters.draft = filters.draft.or(self.scope.draft);
        filters.review = filters.review.or(self.scope.review);
        filters.checks = filters.checks.or(self.scope.checks);
        if filters.author.is_none()
            && let Some(author) = self.scope.author.as_deref()
        {
            filters.author = super::pull_request_list_logic::qualifier_value(author);
        }
        if let Some(labels) = self.scope.labels.as_ref() {
            let groups = labels
                .iter()
                .filter_map(|label| super::pull_request_list_logic::qualifier_value(label))
                .map(|value| vec![value])
                .collect::<Vec<_>>();
            if !groups.is_empty() {
                filters.labels = Some(groups);
            }
        }
        filters
    }

    fn search_input(&self, cx: &App) -> impl IntoElement {
        let theme = cx.app_theme();
        let busy = false;
        // `InputGroup`: `h-9 sm:h-8`, rounded control radius, `border-input`, dark `bg-input/32`.
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .h(rems(2.))
            .rounded(crate::theme::tokens::CONTROL_RADIUS)
            .border_1()
            .border_color(theme.input)
            .bg(if theme.is_dark() {
                with_alpha(theme.input, 0.32).into()
            } else {
                theme.background
            })
            .shadow(crate::theme::tokens::shadow_xs(0.05))
            .pl(rems(0.6875))
            .child(if busy {
                Spinner::new("pr-search-busy")
                    .size(rems(1.))
                    .into_any_element()
            } else {
                Icon::new(IconName::Search)
                    .size(rems(1.))
                    .color(with_alpha(theme.foreground, 0.8).into())
                    .into_any_element()
            })
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.search)
                        .appearance(false)
                        .bordered(false)
                        .text_size(TEXT_SM.size),
                ),
            )
    }
}

impl Render for PullRequestsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let on_change = self.on_change(cx);
        let list = self.store.read(cx).list().clone();
        let (groups, query_text) = self.display_groups(cx);
        let shown: usize = groups.iter().map(|group| group.entries.len()).sum();
        let projects_known = list.projects_known;
        let has_projects = !projects_known || !list.projects.is_empty();
        let refreshing = list.refreshing;
        let header_left = if self.sidebar_open {
            px(20.)
        } else {
            workspace_titlebar_content_left(window)
        };

        // Hosts the workspace reads, then the provider pill that switches between them.
        let host_options: Vec<FilterOption<String>> =
            std::iter::once(FilterOption::new(String::new(), "All", IconName::Plug2))
                .chain(
                    list.merged
                        .as_ref()
                        .map(|merged| merged.providers.as_slice())
                        .unwrap_or(&[])
                        .iter()
                        .map(|provider| {
                            let mut option = FilterOption::new(
                                provider.host.as_str().to_owned(),
                                "GitHub",
                                IconName::Github,
                            );
                            if !provider.configured {
                                option.unavailable =
                                    Some(
                                        provider.detail.as_ref().map_or(
                                            "This host could not be read.".into(),
                                            |detail| SharedString::from(detail.as_str().to_owned()),
                                        ),
                                    );
                            }
                            option
                        }),
                )
                .collect();

        let facets = collect_pull_request_list_facets(
            list.merged
                .as_ref()
                .map_or(&[][..], |merged| merged.entries.as_slice()),
            self.scope.state,
        );
        let project_options: Vec<FilterOption<String>> = std::iter::once(FilterOption::new(
            "all".to_owned(),
            "All projects",
            IconName::Layers,
        ))
        .chain(list.projects.iter().map(|project| {
            FilterOption::new(
                format!("{}\u{1f}{}", project.environment_id.as_str(), project.id),
                project.title.clone(),
                IconName::Folder,
            )
        }))
        .collect();
        let project_value = match (&self.scope.environment_id, &self.scope.project_id) {
            (Some(environment), Some(project)) => {
                format!("{}\u{1f}{}", environment.as_str(), project.as_str())
            }
            _ => "all".to_owned(),
        };

        let change = on_change.clone();
        let sort_menu = compact_filter_menu(
            "pr-sort-menu",
            "Sort pull requests",
            Some(IconName::ArrowDownUp),
            Some("Sort"),
            true,
            false,
            self.scope.sort.unwrap_or(PullRequestListSort::Ready),
            sort_options(),
            Rc::new(move |value, window, cx| change(ScopeChange::Sort(value), window, cx)),
            cx,
        );
        let filters = filters_menu(
            FiltersMenuInput {
                scope: self.scope.clone(),
                authors: facets.authors.clone(),
                labels: facets.labels.clone(),
                host_options: host_options.clone(),
                projects: project_options,
                project_value,
            },
            on_change.clone(),
            cx,
        );
        let change = on_change.clone();
        let provider_menu = compact_filter_menu(
            "pr-provider-menu",
            "Filter by provider",
            Some(IconName::Plug2),
            Some("All"),
            true,
            self.scope.host.is_some(),
            self.scope.host.clone().unwrap_or_default(),
            host_options,
            Rc::new(move |value: String, window, cx| {
                change(
                    ScopeChange::Host((!value.is_empty()).then_some(value)),
                    window,
                    cx,
                )
            }),
            cx,
        );
        let store = self.store.clone();
        let refresh = Button::new("pr-refresh")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Icon)
            .disabled(refreshing)
            .child(if refreshing {
                Spinner::new("pr-refresh-spin")
                    .size(rems(1.))
                    .into_any_element()
            } else {
                Icon::new(IconName::RefreshCw)
                    .size(rems(1.))
                    .into_any_element()
            })
            .on_click(move |_, _, cx| {
                if let Some(source) = store.read(cx).source() {
                    source.refresh_list(cx);
                }
            });

        let page = cx.entity().downgrade();
        let meta_width = px((self.column_width - 48. - 24.).max(0.));
        let selected_key = self.selected_key.clone();
        let show_provider = false;
        let body = if !list.supported {
            PullRequestsUnavailableState::new("Update your servers to browse pull requests.")
                .title("Pull requests unavailable")
                .into_any_element()
        } else if list.merged.is_none() && list.error.is_none() {
            list_ghost(7, None, cx)
        } else if let (Some(error), 0) = (list.error.as_ref(), shown) {
            let mut state = PullRequestsUnavailableState::new(error.clone());
            let store = self.store.clone();
            state.refreshing = refreshing;
            state.on_retry = Some(Rc::new(move |_, _, cx| {
                if let Some(source) = store.read(cx).source() {
                    source.refresh_list(cx);
                }
            }));
            state.into_any_element()
        } else if shown == 0 {
            let scope_filtered = self.scope.state != PullRequestListState::Open
                || self.scope.involvement != PullRequestInvolvement::All
                || self.scope.project_id.is_some()
                || self.scope.host.is_some()
                || self.scope.draft.is_some()
                || self.scope.review.is_some()
                || self.scope.checks.is_some()
                || self.scope.author.is_some()
                || self.scope.labels.is_some();
            let search = self.search.clone();
            let store_refresh = self.store.clone();
            PullRequestListEmptyState {
                query: query_text.clone(),
                filtered: scope_filtered,
                searching: false,
                has_projects,
                can_load_more: list.merged.as_ref().is_some_and(|merged| merged.truncated),
                loading_more: list.loading_more,
                refreshing,
                on_clear_query: Rc::new(move |_, window, cx| {
                    search.update(cx, |state, cx| state.set_value("", window, cx));
                }),
                on_load_more: Rc::new({
                    let store = self.store.clone();
                    move |_, _, cx| {
                        if let Some(source) = store.read(cx).source() {
                            source.load_more(cx);
                        }
                    }
                }),
                on_refresh: Rc::new(move |_, _, cx| {
                    if let Some(source) = store_refresh.read(cx).source() {
                        source.refresh_list(cx);
                    }
                }),
            }
            .into_any_element()
        } else {
            let on_select: super::list_row::SelectRow = {
                let page = page.clone();
                Rc::new(move |entry, _, cx| {
                    let entry = entry.clone();
                    page.update(cx, |this, cx| this.select(&entry, cx)).ok();
                })
            };
            let host_actions_of = self.host_actions_of(cx);
            let can_write = list.can_write;
            let speed_mode = self.speed_mode;
            let runner = self.runner.read(cx);
            // The row-level state speed mode reads, taken before the rows are built.
            let busy_rows: HashSet<String> = groups
                .iter()
                .flat_map(|group| group.entries.iter())
                .map(entry_key)
                .filter(|key| {
                    runner.action_pending(key)
                        || runner.closing(key)
                        || self.sweep.as_ref().is_some_and(|sweep| sweep.contains(key))
                })
                .collect();
            let row_bounds = self.row_bounds.clone();
            div()
                .flex()
                .flex_col()
                .gap_3()
                .children(groups.iter().map(|group| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .when(!group.label.is_empty(), |this| {
                            this.child(group_header(
                                group.key,
                                &group.label,
                                group.entries.len(),
                                cx,
                            ))
                        })
                        .children(group.entries.iter().map(|entry| {
                            let key = pull_request_entry_key(
                                &entry.entry,
                                Some(entry.environment_id.as_str()),
                            );
                            let stack_select: super::stack_popover::SelectLayer = {
                                let page = page.clone();
                                let environment_id = entry.environment_id.clone();
                                Rc::new(move |reference, cx| {
                                    let reference = reference.clone();
                                    let environment_id = environment_id.clone();
                                    page.update(cx, |this, cx| {
                                        this.select_reference(environment_id, reference, cx)
                                    })
                                    .ok();
                                })
                            };
                            let on_perform: super::speed_actions::PerformAction = {
                                let page = page.clone();
                                let entry = entry.clone();
                                Rc::new(move |action, _, cx| {
                                    let entry = entry.clone();
                                    page.update(cx, |this, cx| {
                                        // A drag from Close ends in a click on it: that click is
                                        // the sweep's, not a second close of the pressed row.
                                        if action == PullRequestAction::Close
                                            && this.consume_sweep_click()
                                        {
                                            return;
                                        }
                                        this.runner.update(cx, |runner, cx| {
                                            runner.perform(&entry, action, cx)
                                        });
                                    })
                                    .ok();
                                })
                            };
                            let on_close_sweep_start: super::speed_actions::SweepStart = {
                                let page = page.clone();
                                let entry = entry.clone();
                                Rc::new(move |event, _, cx| {
                                    let entry = entry.clone();
                                    page.update(cx, |this, cx| {
                                        this.start_close_sweep(&entry, event, cx)
                                    })
                                    .ok();
                                })
                            };
                            PullRequestRow::new(entry.clone(), on_select.clone())
                                .speed(RowSpeed {
                                    host_actions: host_actions_of(&entry.entry),
                                    visible: speed_mode,
                                    can_write,
                                    busy: busy_rows.contains(&key),
                                    on_perform,
                                    on_close_sweep_start: Some(on_close_sweep_start),
                                })
                                .record_bounds(row_bounds.clone())
                                .stack_popover(self.store.clone(), stack_select)
                                .selected(selected_key.as_deref() == Some(key.as_str()))
                                .show_provider(show_provider)
                                .matched_elsewhere(
                                    !query_text.is_empty()
                                        && score_pull_request_match(entry, &query_text)
                                            <= MATCHED_ELSEWHERE_SCORE,
                                )
                                .meta_width(meta_width)
                        }))
                }))
                .into_any_element()
        };

        let load_more_footer = list
            .merged
            .as_ref()
            .filter(|merged| merged.truncated && shown > 0)
            .map(|_| {
                let store = self.store.clone();
                div()
                    .flex()
                    .justify_center()
                    .py_3()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(
                        Button::new("pr-load-more")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Load more pull requests")
                            .on_click(move |_, _, cx| {
                                if let Some(source) = store.read(cx).source() {
                                    source.load_more(cx);
                                }
                            }),
                    )
            });

        let _ = (state_tabs, involvement_tabs, Anchor::TopLeft, RADIUS_MD);
        // `@container/pr-list`: the column measures itself so `@lg` (32rem) can decide whether the
        // search takes a row of its own.
        let measure = {
            let page = cx.entity().downgrade();
            gpui_kit::canvas(
                move |bounds, _, cx| {
                    let width = f32::from(bounds.size.width);
                    page.update(cx, |page, cx| page.set_column_width(width, cx))
                        .ok();
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };
        let narrow = self.column_width < 512.;

        let toasts = render_toasts(self.runner.read(cx).toasts(), cx);
        let sweep_listeners = self.sweep_listeners(cx);
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(theme.background)
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui_kit::ModifiersChangedEvent, _, cx| {
                    this.set_modifiers(&event.modifiers, cx)
                },
            ))
            .child(measure)
            .children(sweep_listeners)
            .child(
                div()
                    .id("pr-list-header")
                    .relative()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .h(WORKSPACE_TOPBAR_HEIGHT)
                    .pl(header_left)
                    .pr(px(20.))
                    .bg(theme.background)
                    .child(
                        WorkspaceBreadcrumb::new().child(
                            WorkspaceBreadcrumbItem::new(true)
                                .child(div().truncate().child("Pull Requests")),
                        ),
                    )
                    .drag_region(window, cx),
            )
            .child(
                div().flex_1().min_h_0().child(
                    scroll_area("pr-list-scroll").child(
                        // `WorkspacePageContainer width="expanded"`: max-w-6xl, `px-5 sm:px-6 pt-6 pb-12`.
                        div()
                            .mx_auto()
                            .flex()
                            .flex_col()
                            .w_full()
                            .max_w(rems(72.))
                            .gap_4()
                            .px(rems(1.5))
                            .pt(rems(1.5))
                            .pb(rems(3.))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .min_w_0()
                                            .when(narrow, |this| this.w_full())
                                            .when(!narrow, |this| this.flex_1())
                                            .child(self.search_input(cx)),
                                    )
                                    .children(sort_menu)
                                    .child(filters)
                                    .children(provider_menu)
                                    .child(refresh),
                            )
                            .child(body)
                            .children(load_more_footer),
                    ),
                ),
            )
            .children(toasts)
    }
}
