//! Port of upstream/apps/web/src/components/pullRequest/PullRequestListFilters.tsx and the
//! `CompactFilterMenu`, `ExpandableSearch`-less pieces of routes/_chat.pull-requests.tsx: the
//! state/involvement/sort/provider pills and the single "Filters" menu.
//!
//! The page keeps its scope as `PullRequestListPreferences` (the same fields the route's URL
//! search carries); menus report [`ScopeChange`]s and the page applies them with
//! [`ScopeChange::apply`], so every menu and every shortcut edits the scope one way.

use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{Anchor, AnyElement, App, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, rems};
use turin_contracts::pull_request::{
    PullRequestChecksFilter, PullRequestDraftFilter, PullRequestInvolvement, PullRequestListState,
    PullRequestReviewFilter,
};

use super::dropdown::dropdown;
use super::pull_request_list_logic::{PullRequestAuthorFacet, PullRequestLabelFacet};
use super::pull_request_list_preferences::{PullRequestListPreferences, PullRequestListSort};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

/// `PullRequestFilterOption`.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterOption<V> {
    pub value: V,
    pub label: SharedString,
    pub icon: IconName,
    /// Why it cannot be chosen, carried onto the item.
    pub unavailable: Option<SharedString>,
}

impl<V> FilterOption<V> {
    pub fn new(value: V, label: impl Into<SharedString>, icon: IconName) -> Self {
        Self {
            value,
            label: label.into(),
            icon,
            unavailable: None,
        }
    }
}

pub fn involvement_tabs() -> Vec<FilterOption<PullRequestInvolvement>> {
    vec![
        FilterOption::new(PullRequestInvolvement::All, "All", IconName::Layers),
        FilterOption::new(PullRequestInvolvement::Reviewing, "Reviewing", IconName::Eye),
        FilterOption::new(PullRequestInvolvement::Authored, "Authored", IconName::PenLine),
    ]
}

pub fn state_tabs() -> Vec<FilterOption<PullRequestListState>> {
    vec![
        FilterOption::new(PullRequestListState::All, "All", IconName::Layers),
        FilterOption::new(PullRequestListState::Open, "Open", IconName::GitPullRequestArrow),
        FilterOption::new(PullRequestListState::Closed, "Closed", IconName::GitPullRequestClosed),
        FilterOption::new(PullRequestListState::Merged, "Merged", IconName::GitMerge),
    ]
}

pub fn sort_options() -> Vec<FilterOption<PullRequestListSort>> {
    vec![
        FilterOption::new(PullRequestListSort::Ready, "Merge readiness", IconName::ListChecks),
        FilterOption::new(PullRequestListSort::Blocked, "Blocked on me", IconName::UserLock),
        FilterOption::new(PullRequestListSort::Updated, "Recently updated", IconName::Clock),
        FilterOption::new(PullRequestListSort::Newest, "Newest shown", IconName::CalendarArrowDown),
        FilterOption::new(PullRequestListSort::Oldest, "Oldest shown", IconName::CalendarArrowUp),
        FilterOption::new(PullRequestListSort::Largest, "Largest shown", IconName::Maximize2),
        FilterOption::new(PullRequestListSort::Smallest, "Smallest shown", IconName::Minimize2),
    ]
}

fn draft_options() -> Vec<FilterOption<Option<PullRequestDraftFilter>>> {
    vec![
        FilterOption::new(None, "All", IconName::Layers),
        FilterOption::new(Some(PullRequestDraftFilter::Only), "Drafts only", IconName::GitPullRequestDraft),
        FilterOption::new(Some(PullRequestDraftFilter::Hide), "Hide drafts", IconName::EyeOff),
    ]
}

fn review_options() -> Vec<FilterOption<Option<PullRequestReviewFilter>>> {
    vec![
        FilterOption::new(None, "All", IconName::Layers),
        FilterOption::new(Some(PullRequestReviewFilter::Approved), "Approved", IconName::CircleCheck),
        FilterOption::new(Some(PullRequestReviewFilter::ChangesRequested), "Changes requested", IconName::CircleX),
        FilterOption::new(Some(PullRequestReviewFilter::ReviewRequired), "Review required", IconName::CircleDashed),
        FilterOption::new(Some(PullRequestReviewFilter::None), "No reviews", IconName::CircleSlash),
    ]
}

fn checks_options() -> Vec<FilterOption<Option<PullRequestChecksFilter>>> {
    vec![
        FilterOption::new(None, "All", IconName::Layers),
        FilterOption::new(Some(PullRequestChecksFilter::Passing), "Passing", IconName::CircleCheck),
        FilterOption::new(Some(PullRequestChecksFilter::Failing), "Failing", IconName::CircleX),
    ]
}

/// One edit to the page's scope.
#[derive(Clone, Debug)]
pub enum ScopeChange {
    State(PullRequestListState),
    Involvement(PullRequestInvolvement),
    Sort(PullRequestListSort),
    Host(Option<String>),
    Draft(Option<PullRequestDraftFilter>),
    Review(Option<PullRequestReviewFilter>),
    Checks(Option<PullRequestChecksFilter>),
    Author(Option<String>),
    Labels(Vec<String>),
    Project(Option<(String, String)>),
    Query(String),
}

impl ScopeChange {
    /// `updateListScope`.
    pub fn apply(self, scope: &mut PullRequestListPreferences) {
        match self {
            Self::State(state) => scope.state = state,
            Self::Involvement(involvement) => scope.involvement = involvement,
            Self::Sort(sort) => scope.sort = Some(sort),
            Self::Host(host) => scope.host = host,
            Self::Draft(draft) => scope.draft = draft,
            Self::Review(review) => scope.review = review,
            Self::Checks(checks) => scope.checks = checks,
            Self::Author(author) => scope.author = author,
            Self::Labels(labels) => {
                scope.labels = (!labels.is_empty()).then_some(labels);
            }
            Self::Project(project) => match project {
                Some((environment_id, project_id)) => {
                    scope.environment_id =
                        crate::state::contracts::EnvironmentId::new(environment_id).ok();
                    scope.project_id = crate::state::contracts::ProjectId::new(project_id).ok();
                }
                None => scope.project_id = None,
            },
            Self::Query(query) => scope.q = (!query.is_empty()).then_some(query),
        }
    }
}

pub type OnScopeChange = Rc<dyn Fn(ScopeChange, &mut Window, &mut App)>;

fn menu_icon(icon: IconName) -> gpui_kit::component::Icon {
    gpui_kit::component::Icon::new(icon)
}

/// One radio group as menu items: the checked option wears the check.
fn radio_items<V: Clone + PartialEq + 'static>(
    mut menu: PopupMenu,
    value: &V,
    options: &[FilterOption<V>],
    on_pick: Rc<dyn Fn(V, &mut Window, &mut App)>,
) -> PopupMenu {
    for option in options {
        let pick = on_pick.clone();
        let picked = option.value.clone();
        let mut item = PopupMenuItem::new(option.label.clone())
            .icon(menu_icon(option.icon))
            .checked(option.value == *value)
            .disabled(option.unavailable.is_some())
            .on_click(move |_, window, cx| pick(picked.clone(), window, cx));
        if option.unavailable.is_some() {
            item = item.disabled(true);
        }
        menu = menu.item(item);
    }
    menu
}

/// `CompactFilterMenu`: a pill that opens one radio group.
#[allow(clippy::too_many_arguments)]
pub fn compact_filter_menu<V: Clone + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    trigger_icon: Option<IconName>,
    trigger_label: Option<&'static str>,
    outlined: bool,
    icon_only: bool,
    value: V,
    options: Vec<FilterOption<V>>,
    on_change: Rc<dyn Fn(V, &mut Window, &mut App)>,
    cx: &App,
) -> Option<AnyElement> {
    let current = options
        .iter()
        .find(|option| option.value == value)
        .or(options.first())?
        .clone();
    let theme = cx.app_theme();
    let muted = with_alpha(theme.muted_foreground, 0.7);
    let mut button = Button::new(id)
        .variant(if outlined {
            ButtonVariant::Outline
        } else {
            ButtonVariant::GhostMuted
        })
        .size(match (outlined, icon_only) {
            (true, true) => ButtonSize::Icon,
            (true, false) => ButtonSize::Default,
            (false, _) => ButtonSize::Sm,
        })
        .min_w_0();
    button = if icon_only {
        button.child(Icon::new(current.icon).size(rems(1.)))
    } else if let Some(trigger_label) = trigger_label {
        button
            .when_some(trigger_icon, |this, icon| this.child(Icon::new(icon).size(rems(1.))))
            .child(trigger_label)
    } else {
        button
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(current.label.clone()),
            )
            .child(Icon::new(IconName::ChevronDown).size(rems(0.75)).color(muted.into()))
    };
    Some(
        dropdown(id, Anchor::TopLeft, button, move |menu, _, _| {
            radio_items(menu, &value, &options, on_change.clone())
        })
        .into_any_element(),
    )
}

/// A "Filters" submenu row: icon, name and the current choice on the right.
fn submenu_label(name: &str, current: &str) -> SharedString {
    format!("{name}  ·  {current}").into()
}

fn radio_submenu<V: Clone + PartialEq + 'static>(
    menu: PopupMenu,
    name: &'static str,
    value: V,
    options: Vec<FilterOption<V>>,
    on_pick: Rc<dyn Fn(V, &mut Window, &mut App)>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let current = options
        .iter()
        .find(|option| option.value == value)
        .or(options.first())
        .map(|option| option.label.to_string())
        .unwrap_or_default();
    menu.submenu(submenu_label(name, &current), window, cx, move |submenu, _, _| {
        radio_items(submenu.label(name), &value, &options, on_pick.clone())
    })
}

/// An option callback that reports `wrap(value)` as a scope change.
fn pick_with<V: 'static>(
    change: &OnScopeChange,
    wrap: fn(V) -> ScopeChange,
) -> Rc<dyn Fn(V, &mut Window, &mut App)> {
    let change = change.clone();
    Rc::new(move |value, window, cx| change(wrap(value), window, cx))
}

/// Everything the "Filters" menu reads.
#[derive(Clone)]
pub struct FiltersMenuInput {
    pub scope: PullRequestListPreferences,
    pub authors: Vec<PullRequestAuthorFacet>,
    pub labels: Vec<PullRequestLabelFacet>,
    pub host_options: Vec<FilterOption<String>>,
    pub projects: Vec<FilterOption<String>>,
    pub project_value: String,
}

/// The number the trigger wears: every filter off its default (`filterCount`).
pub fn filter_count(scope: &PullRequestListPreferences) -> usize {
    [
        scope.state != PullRequestListState::Open,
        scope.involvement != PullRequestInvolvement::All,
        scope.host.is_some(),
        scope.project_id.is_some(),
        scope.draft.is_some(),
        scope.review.is_some(),
        scope.checks.is_some(),
        scope.author.is_some(),
    ]
    .into_iter()
    .filter(|on| *on)
    .count()
        + scope.labels.as_ref().map_or(0, Vec::len)
}

/// `PullRequestFiltersMenu`.
pub fn filters_menu(
    input: FiltersMenuInput,
    on_change: OnScopeChange,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let count = filter_count(&input.scope);
    let badge_bg = theme.muted;
    let badge_fg = theme.muted_foreground;
    let button = Button::new("pr-filters-menu")
        .variant(ButtonVariant::Outline)
        .child(Icon::new(IconName::ListFilter).size(rems(1.)))
        .child("Filters")
        .when(count > 0, |this| {
            this.child(
                div()
                    .rounded_full()
                    .bg(badge_bg)
                    .px(rems(0.375))
                    .text_size(crate::theme::tokens::TEXT_XS.size)
                    .text_color(badge_fg)
                    .child(count.to_string()),
            )
        });
    dropdown("pr-filters-menu", Anchor::TopRight, button, move |menu, window, cx| {
        let scope = input.scope.clone();
        let change = on_change.clone();
        let mut menu = menu;
        menu = radio_submenu(menu, "State", scope.state, state_tabs(), pick_with(&change, ScopeChange::State), window, cx);
        menu = radio_submenu(menu, "Involvement", scope.involvement, involvement_tabs(), pick_with(&change, ScopeChange::Involvement), window, cx);
        menu = menu.separator();
        // Author: a flat list of the authors seen in the loaded rows, "Anyone" first.
        let authors = input.authors.clone();
        let author_value = scope.author.clone();
        let author_change = change.clone();
        menu = menu.submenu(
            submenu_label("Author", author_value.as_deref().unwrap_or("Anyone")),
            window,
            cx,
            move |submenu, _, _| {
                let mut submenu = submenu.label("Author");
                let anyone = author_change.clone();
                submenu = submenu.item(
                    PopupMenuItem::new("Anyone")
                        .icon(menu_icon(IconName::Layers))
                        .checked(author_value.is_none())
                        .on_click(move |_, window, cx| anyone(ScopeChange::Author(None), window, cx)),
                );
                for facet in authors.iter().take(10) {
                    let login = facet.actor.login.as_str().to_owned();
                    let checked = author_value
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(&login));
                    let change = author_change.clone();
                    let target = login.clone();
                    submenu = submenu.item(
                        PopupMenuItem::new(format!("{login}  ·  {} merges loaded", facet.merged_count))
                            .icon(menu_icon(IconName::UserRound))
                            .checked(checked)
                            .on_click(move |_, window, cx| {
                                change(ScopeChange::Author(Some(target.clone())), window, cx)
                            }),
                    );
                }
                if authors.is_empty() {
                    submenu = submenu.item(PopupMenuItem::new("No authors found").disabled(true));
                }
                submenu
            },
        );
        // Labels: checkbox list with the count of rows carrying each.
        let labels = input.labels.clone();
        let selected: Vec<String> = scope.labels.clone().unwrap_or_default();
        let label_change = change.clone();
        let summary = if selected.is_empty() {
            "Any".to_owned()
        } else {
            format!("{} selected", selected.len())
        };
        menu = menu.submenu(submenu_label("Labels", &summary), window, cx, move |submenu, _, _| {
            if labels.is_empty() {
                return submenu.item(PopupMenuItem::new("No labels in this view").disabled(true));
            }
            let mut submenu = submenu;
            for facet in &labels {
                let name = facet.label.name.as_str().to_owned();
                let checked = selected.iter().any(|value| value.eq_ignore_ascii_case(&name));
                let next: Vec<String> = if checked {
                    selected
                        .iter()
                        .filter(|value| !value.eq_ignore_ascii_case(&name))
                        .cloned()
                        .collect()
                } else {
                    selected.iter().cloned().chain([name.clone()]).collect()
                };
                let change = label_change.clone();
                submenu = submenu.item(
                    PopupMenuItem::new(format!("{name}  ·  {}", facet.count))
                        .icon(menu_icon(IconName::Tag))
                        .checked(checked)
                        .on_click(move |_, window, cx| {
                            change(ScopeChange::Labels(next.clone()), window, cx)
                        }),
                );
            }
            submenu
        });
        menu = radio_submenu(menu, "Draft", scope.draft, draft_options(), pick_with(&change, ScopeChange::Draft), window, cx);
        menu = radio_submenu(menu, "Review", scope.review, review_options(), pick_with(&change, ScopeChange::Review), window, cx);
        menu = radio_submenu(menu, "Checks", scope.checks, checks_options(), pick_with(&change, ScopeChange::Checks), window, cx);
        if input.host_options.len() > 2 {
            menu = menu.separator();
            let host_value = scope.host.clone().unwrap_or_default();
            let change = change.clone();
            menu = radio_submenu(
                menu,
                "Host",
                host_value,
                input.host_options.clone(),
                Rc::new(move |value: String, window, cx| {
                    change(ScopeChange::Host((!value.is_empty()).then_some(value)), window, cx)
                }),
                window,
                cx,
            );
        }
        menu = menu.separator();
        let project_change = change.clone();
        radio_submenu(
            menu,
            "Project",
            input.project_value.clone(),
            input.projects.clone(),
            Rc::new(move |value: String, window, cx| {
                let project = value
                    .split_once('\u{1f}')
                    .map(|(environment, project)| (environment.to_owned(), project.to_owned()));
                project_change(ScopeChange::Project(project), window, cx)
            }),
            window,
            cx,
        )
    })
    .into_any_element()
}
