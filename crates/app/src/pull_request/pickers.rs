//! Port of upstream/apps/web/src/components/pullRequest/PullRequestCandidatePicker.tsx,
//! PullRequestReviewerPicker.tsx and PullRequestLabelPicker.tsx: an icon button beside the
//! reviewers / labels row that opens a searchable list of the people who may be asked, or the
//! repository's labels, each row checked when it already applies.
//!
//! The candidates arrive in the detail snapshot (`reviewer_candidates`, `label_candidates`); the
//! list shows its ghost until the source has put them there. The picker stays open on a pick, as
//! upstream's combobox does (the row's check turns over), and sends the confirmed set to the
//! source: [`PullRequestSource::request_reviewers`] takes the logins that should be requested
//! afterwards, [`PullRequestSource::set_labels`] the labels the pull request should wear.
//!
//! Not ported: the combobox's arrow-key highlight and type-ahead (rows are pressed), the
//! per-pick success / failure toasts (the source reports failures through the snapshot), and
//! the truncated-list footer's exact wrapping.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, CursorStyle, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rems,
};
use turin_contracts::pull_request::{
    PullRequestLabelCandidate, PullRequestReviewerCandidate, PullRequestReviewerKind,
};

use super::detail_context::DetailContext;
use super::detail_layout::{TriggerBox, alpha, people_ghost, scroll_box};
use super::presentation_view::PullRequestActorAvatar;
use super::pull_request_list_logic::pull_request_label_color;
use crate::theme::tokens::{RADIUS_SM, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconName, PopoverOptions, PopoverPadding,
    PopoverWidth, Tooltip, popover,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Reviewer,
    Label,
}

impl Kind {
    fn icon(self) -> IconName {
        match self {
            Self::Reviewer => IconName::UserPlus,
            Self::Label => IconName::Tag,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Reviewer => "Request a review",
            Self::Label => "Change labels",
        }
    }

    fn disabled_reason(self) -> &'static str {
        match self {
            Self::Reviewer => "Asking someone to review needs write access on this repository",
            Self::Label => "Changing labels needs triage access on this repository",
        }
    }

    fn search_label(self) -> &'static str {
        match self {
            Self::Reviewer => "Search people with access",
            Self::Label => "Search labels",
        }
    }

    fn empty_label(self) -> &'static str {
        match self {
            Self::Reviewer => "Nobody else has access to this repository.",
            Self::Label => "This repository has no labels.",
        }
    }

    fn no_match_label(self) -> &'static str {
        match self {
            Self::Reviewer => "Nobody with access matches that.",
            Self::Label => "No label matches that.",
        }
    }

    fn truncated_label(self) -> &'static str {
        match self {
            Self::Reviewer => {
                "This repository has more people with access than are listed here. Ask for the rest on the host."
            }
            Self::Label => {
                "This repository has more labels than are listed here. Apply the rest on the host."
            }
        }
    }
}

/// Narrows only what arrived: the host is asked once, when the menu opens.
pub fn reviewer_matches(candidate: &PullRequestReviewerCandidate, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let needle = query.to_lowercase();
    candidate.login.as_str().to_lowercase().contains(&needle)
        || candidate
            .name
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(&needle)
}

pub fn label_matches(candidate: &PullRequestLabelCandidate, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let needle = query.to_lowercase();
    candidate.name.as_str().to_lowercase().contains(&needle)
        || candidate
            .description
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains(&needle)
}

/// The body of an open picker: a search box over the rows, with the states a list can be in.
struct PickerBody {
    ctx: DetailContext,
    kind: Kind,
    search: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl PickerBody {
    fn new(ctx: DetailContext, kind: Kind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(kind.search_label()));
        let subscriptions = vec![
            cx.observe(&ctx.store, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        search.update(cx, |search, cx| search.focus(window, cx));
        Self {
            ctx,
            kind,
            search,
            _subscriptions: subscriptions,
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }

    fn toggle_reviewer(&self, candidate: &PullRequestReviewerCandidate, cx: &mut App) {
        let Some(source) = self.ctx.source(cx) else {
            return;
        };
        let current: Vec<String> = self
            .ctx
            .snapshot(cx)
            .and_then(|snapshot| snapshot.reviewer_candidates.as_ref())
            .map(|list| {
                list.candidates
                    .iter()
                    .filter(|entry| entry.is_requested)
                    .map(|entry| entry.login.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let login = candidate.login.as_str().to_owned();
        let next: Vec<String> = if candidate.is_requested {
            current
                .into_iter()
                .filter(|entry| *entry != login)
                .collect()
        } else {
            current.into_iter().chain([login]).collect()
        };
        source.request_reviewers(&self.ctx.reference, next, cx);
    }

    fn toggle_label(&self, candidate: &PullRequestLabelCandidate, cx: &mut App) {
        let Some(source) = self.ctx.source(cx) else {
            return;
        };
        let current: Vec<String> = self
            .ctx
            .snapshot(cx)
            .and_then(|snapshot| snapshot.label_candidates.as_ref())
            .map(|list| {
                list.candidates
                    .iter()
                    .filter(|entry| entry.is_applied)
                    .map(|entry| entry.name.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let name = candidate.name.as_str().to_owned();
        let next: Vec<String> = if candidate.is_applied {
            current.into_iter().filter(|entry| *entry != name).collect()
        } else {
            current.into_iter().chain([name]).collect()
        };
        source.set_labels(&self.ctx.reference, next, cx);
    }
}

/// One row of the list: `min-h-7 rounded-sm px-2 py-1 text-sm hover:bg-accent`.
fn row(id: SharedString, theme: &AppTheme) -> gpui_kit::Stateful<gpui_kit::Div> {
    let hover = theme.accent;
    div()
        .id(id)
        .flex()
        .min_h(rems(1.75))
        .items_center()
        .gap_2()
        .rounded(RADIUS_SM)
        .px_2()
        .py_1()
        .cursor(CursorStyle::PointingHand)
        .text_size(TEXT_SM.size)
        .hover(move |style| style.bg(hover))
}

fn message(text: impl Into<SharedString>, theme: &AppTheme) -> AnyElement {
    div()
        .p_2()
        .text_size(TEXT_XS.size)
        .text_color(theme.muted_foreground)
        .child(text.into())
        .into_any_element()
}

impl Render for PickerBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let query = self.query(cx);
        let weak = cx.entity().downgrade();
        let snapshot = self.ctx.snapshot(cx);
        let kind = self.kind;

        let (rows, truncated, pending): (Vec<AnyElement>, bool, bool) = match kind {
            Kind::Reviewer => match snapshot.and_then(|s| s.reviewer_candidates.as_ref()) {
                None => (Vec::new(), false, true),
                Some(list) => {
                    let rows = list
                        .candidates
                        .iter()
                        .filter(|candidate| reviewer_matches(candidate, &query))
                        .map(|candidate| {
                            let weak = weak.clone();
                            let target = candidate.clone();
                            let actor = turin_contracts::pull_request::PullRequestActor {
                                is_bot: candidate.is_bot,
                                login: candidate.login.clone(),
                                name: candidate.name.clone(),
                                avatar_url: candidate.avatar_url.clone(),
                            };
                            row(
                                SharedString::from(format!(
                                    "pr-reviewer-pick-{}",
                                    candidate.id.as_str()
                                )),
                                &theme,
                            )
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .flex_1()
                                    .items_center()
                                    .gap(rems(0.375))
                                    .child(PullRequestActorAvatar::new(Some(&actor)))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(SharedString::from(
                                                actor.login.as_str().to_owned(),
                                            )),
                                    ),
                            )
                            .when(candidate.kind == PullRequestReviewerKind::Team, |this| {
                                this.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(theme.muted_foreground)
                                        .child("team"),
                                )
                            })
                            .when(candidate.is_requested, |this| {
                                this.child(Icon::new(IconName::Check).size(rems(0.875)))
                            })
                            .on_click(move |_, _, cx| {
                                weak.update(cx, |body, cx| body.toggle_reviewer(&target, cx))
                                    .ok();
                            })
                            .into_any_element()
                        })
                        .collect();
                    (rows, list.truncated, false)
                }
            },
            Kind::Label => match snapshot.and_then(|s| s.label_candidates.as_ref()) {
                None => (Vec::new(), false, true),
                Some(list) => {
                    let rows = list
                        .candidates
                        .iter()
                        .filter(|candidate| label_matches(candidate, &query))
                        .map(|candidate| {
                            let weak = weak.clone();
                            let target = candidate.clone();
                            let dot = pull_request_label_color(candidate.color.as_deref())
                                .and_then(|hex| hex.strip_prefix('#').map(str::to_owned))
                                .and_then(|digits| u32::from_str_radix(&digits, 16).ok())
                                .map_or(theme.muted_foreground, |value| {
                                    gpui_kit::rgb(value).into()
                                });
                            row(
                                SharedString::from(format!(
                                    "pr-label-pick-{}",
                                    candidate.name.as_str()
                                )),
                                &theme,
                            )
                            .child(div().flex_shrink_0().size(rems(0.5)).rounded_full().bg(dot))
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(candidate.name.as_str().to_owned()))
                                    .children(candidate.description.clone().map(|description| {
                                        div()
                                            .flex()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(" · {description}"))
                                    })),
                            )
                            .when(candidate.is_applied, |this| {
                                this.child(Icon::new(IconName::Check).size(rems(0.875)))
                            })
                            .on_click(move |_, _, cx| {
                                weak.update(cx, |body, cx| body.toggle_label(&target, cx))
                                    .ok();
                            })
                            .into_any_element()
                        })
                        .collect();
                    (rows, list.truncated, false)
                }
            },
        };

        let list: AnyElement = if pending {
            people_ghost(4, cx)
        } else if rows.is_empty() {
            message(
                if query.is_empty() {
                    kind.empty_label()
                } else {
                    kind.no_match_label()
                },
                &theme,
            )
        } else {
            div()
                .flex()
                .flex_col()
                .p_1()
                .children(rows)
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .w(rems(18.))
            .max_h(rems(23.))
            .overflow_hidden()
            .child(
                // `ComboboxSearchInput`: `px-3 pt-2.5`, a hairline under the box.
                div().px_3().pt(rems(0.625)).flex_shrink_0().child(
                    div()
                        .relative()
                        .border_b_1()
                        .border_color(alpha(theme.border, 0.7))
                        .pb(rems(0.375))
                        .child(
                            div().absolute().top(rems(0.375)).left_0().child(
                                Icon::new(IconName::Search)
                                    .size(rems(1.))
                                    .color(alpha(theme.muted_foreground, 0.55)),
                            ),
                        )
                        .child(
                            div().pl(rems(1.25)).child(
                                Input::new(&self.search)
                                    .appearance(false)
                                    .bordered(false)
                                    .text_size(TEXT_SM.size),
                            ),
                        ),
                ),
            )
            .child(
                scroll_box(
                    SharedString::from(format!("pr-picker-list-{kind:?}")),
                    rems(18.),
                )
                .child(list)
                .when(truncated, |this| {
                    this.child(
                        div()
                            .px_2()
                            .py(rems(0.375))
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(kind.truncated_label()),
                    )
                }),
            )
    }
}

/// `PullRequestCandidatePicker`: the icon button, and what it opens.
fn candidate_picker(
    ctx: DetailContext,
    kind: Kind,
    allowed: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if !allowed {
        return Button::new(SharedString::from(format!("pr-picker-{kind:?}-disabled")))
            .size(ButtonSize::IconXs)
            .variant(ButtonVariant::Ghost)
            .disabled(true)
            .icon(kind.icon())
            .tooltip(Tooltip::new(kind.disabled_reason()))
            .into_any_element();
    }
    let id = SharedString::from(format!("pr-picker-{kind:?}"));
    let trigger = Button::new(id.clone())
        .size(ButtonSize::IconXs)
        .variant(ButtonVariant::Ghost)
        .icon(kind.icon())
        .tooltip(Tooltip::new(kind.label()));
    let body_ctx = ctx;
    let (popup_bg, popup_font) = {
        let theme = cx.app_theme();
        (theme.popover, theme.fonts.sans.clone())
    };
    let body_key = SharedString::from(format!("{id}-body"));
    popover(
        SharedString::from(format!("{id}-popover")),
        PopoverOptions {
            padding: PopoverPadding::None,
            width: PopoverWidth::Auto,
            ..PopoverOptions::default()
        },
        window,
        cx,
    )
    .anchor(Anchor::TopLeft)
    .bg(popup_bg)
    .font_family(popup_font)
    .default_open(super::dropdown::demo_open(&id))
    .trigger(trigger)
    .content(move |_, window, cx| {
        let ctx = body_ctx.clone();
        window.use_keyed_state(body_key.clone(), cx, move |window, cx| {
            PickerBody::new(ctx, kind, window, cx)
        })
    })
    .into_any_element()
}

/// `PullRequestReviewerPicker`.
#[derive(IntoElement)]
pub struct PullRequestReviewerPicker {
    ctx: DetailContext,
    allowed: bool,
}

impl PullRequestReviewerPicker {
    /// `allowed` is false where the host would refuse this account's request: the control is
    /// disabled with the reason rather than hidden.
    pub fn new(ctx: DetailContext, allowed: bool) -> Self {
        Self { ctx, allowed }
    }
}

impl RenderOnce for PullRequestReviewerPicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        candidate_picker(self.ctx, Kind::Reviewer, self.allowed, window, cx)
    }
}

/// `PullRequestLabelPicker`.
#[derive(IntoElement)]
pub struct PullRequestLabelPicker {
    ctx: DetailContext,
    allowed: bool,
}

impl PullRequestLabelPicker {
    pub fn new(ctx: DetailContext, allowed: bool) -> Self {
        Self { ctx, allowed }
    }
}

impl RenderOnce for PullRequestLabelPicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        candidate_picker(self.ctx, Kind::Label, self.allowed, window, cx)
    }
}

#[allow(dead_code)]
fn _unused(_: Option<Popover>, _: Option<TriggerBox>) {
    let _ = px(0.);
}

/// Dev hook for screenshots (`TURIN_PR_OPEN` naming a picker): fills the candidate lists the
/// source would read from the host.
pub fn demo_seed(
    store: &Entity<super::view_model::PullRequestStore>,
    reference: &turin_contracts::pull_request::PullRequestRef,
    cx: &mut App,
) {
    if !std::env::var("TURIN_PR_OPEN").is_ok_and(|wanted| wanted.contains("picker")) {
        return;
    }
    let reviewers = serde_json::json!({"truncated": false, "candidates": [
        {"login": "mara", "name": "Mara Voss", "avatarUrl": null, "id": "u1", "kind": "user", "isRequested": true},
        {"login": "noah", "name": "Noah Okafor", "avatarUrl": null, "id": "u2", "kind": "user", "isRequested": false},
        {"login": "platform", "name": null, "avatarUrl": null, "id": "t1", "kind": "team", "isRequested": false},
    ]});
    let labels = serde_json::json!({"truncated": true, "candidates": [
        {"name": "ui", "color": "1d76db", "description": "Visual changes", "isApplied": true},
        {"name": "bug", "color": "d73a4a", "description": null, "isApplied": false},
        {"name": "port", "color": "a371f7", "description": "Upstream port", "isApplied": true},
    ]});
    let reference = reference.clone();
    store.update(cx, |store, cx| {
        store.update_detail(&reference, cx, |snapshot| {
            snapshot.reviewer_candidates = serde_json::from_value(reviewers).ok();
            snapshot.label_candidates = serde_json::from_value(labels).ok();
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reviewer(login: &str, name: Option<&str>) -> PullRequestReviewerCandidate {
        serde_json::from_value(serde_json::json!({
            "login": login, "name": name, "avatarUrl": null,
            "id": format!("id-{login}"), "kind": "user", "isRequested": false,
        }))
        .unwrap()
    }

    fn label(name: &str, description: Option<&str>) -> PullRequestLabelCandidate {
        serde_json::from_value(serde_json::json!({
            "name": name, "color": null, "description": description, "isApplied": false,
        }))
        .unwrap()
    }

    #[test]
    fn narrows_reviewers_by_login_or_name() {
        let ada = reviewer("ada", Some("Ada Lovelace"));
        assert!(reviewer_matches(&ada, ""));
        assert!(reviewer_matches(&ada, "AD"));
        assert!(reviewer_matches(&ada, "lovelace"));
        assert!(!reviewer_matches(&ada, "grace"));
    }

    #[test]
    fn narrows_labels_by_name_or_description() {
        let bug = label("bug", Some("Something is broken"));
        assert!(label_matches(&bug, "BUG"));
        assert!(label_matches(&bug, "broken"));
        assert!(!label_matches(&bug, "feature"));
    }
}
