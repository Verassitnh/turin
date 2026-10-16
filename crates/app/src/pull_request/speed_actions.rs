//! Port of upstream/apps/web/src/components/pullRequest/PullRequestSpeedActions.tsx: the row
//! buttons shift reveals (Close, Merge, Ready for review, Reopen), with their labels and tooltip
//! copy.
//!
//! "No detail or stack reads until a merge is clicked, even on a long list": the buttons only
//! call `on_perform`, and the page's [`PullRequestActionRunner`](super::action_runner) reads the
//! detail when a merge is sent.

#![allow(dead_code)]

use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div, rems,
};
use turin_contracts::pull_request::PullRequestAction;

use super::presentation_view::Spinner;
use super::pull_request_list_logic::{
    EnvironmentPullRequestEntry, PullRequestQuickAction, pull_request_quick_actions,
};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

pub fn pull_request_speed_action_label(action: PullRequestQuickAction) -> &'static str {
    match action {
        PullRequestQuickAction::Close => "Close",
        PullRequestQuickAction::Merge => "Merge",
        PullRequestQuickAction::Ready => "Ready for review",
        PullRequestQuickAction::Reopen => "Reopen",
    }
}

pub fn pull_request_speed_action_tooltip(
    action: PullRequestQuickAction,
    has_stack: bool,
    supports_close_sweep: bool,
) -> String {
    match action {
        PullRequestQuickAction::Merge if has_stack => {
            "Open this pull request to merge its stack".into()
        }
        PullRequestQuickAction::Close if supports_close_sweep => {
            "Close immediately, or drag across rows to close several".into()
        }
        other => format!("{} immediately", pull_request_speed_action_label(other)),
    }
}

/// `ACTIONS[action].Icon`.
fn speed_action_icon(action: PullRequestQuickAction) -> IconName {
    match action {
        PullRequestQuickAction::Close => IconName::GitPullRequestClosed,
        PullRequestQuickAction::Merge => IconName::GitMerge,
        PullRequestQuickAction::Ready | PullRequestQuickAction::Reopen => {
            IconName::GitPullRequestArrow
        }
    }
}

fn quick_action_to_action(action: PullRequestQuickAction) -> PullRequestAction {
    match action {
        PullRequestQuickAction::Close => PullRequestAction::Close,
        PullRequestQuickAction::Merge => PullRequestAction::Merge,
        PullRequestQuickAction::Ready => PullRequestAction::Ready,
        PullRequestQuickAction::Reopen => PullRequestAction::Reopen,
    }
}

pub type PerformAction = Rc<dyn Fn(PullRequestAction, &mut Window, &mut App)>;
pub type SweepStart = Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// `PullRequestSpeedActions`.
#[derive(IntoElement)]
pub struct PullRequestSpeedActions {
    entry: EnvironmentPullRequestEntry,
    host_actions: HashSet<PullRequestAction>,
    visible: bool,
    can_write: bool,
    /// `actionPending || closing || sweeping`.
    busy: bool,
    on_perform: PerformAction,
    on_close_sweep_start: Option<SweepStart>,
}

impl PullRequestSpeedActions {
    pub fn new(
        entry: EnvironmentPullRequestEntry,
        host_actions: HashSet<PullRequestAction>,
        on_perform: PerformAction,
    ) -> Self {
        Self {
            entry,
            host_actions,
            visible: false,
            can_write: true,
            busy: false,
            on_perform,
            on_close_sweep_start: None,
        }
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn can_write(mut self, can_write: bool) -> Self {
        self.can_write = can_write;
        self
    }

    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    pub fn on_close_sweep_start(mut self, start: SweepStart) -> Self {
        self.on_close_sweep_start = Some(start);
        self
    }
}

impl RenderOnce for PullRequestSpeedActions {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let actions = pull_request_quick_actions(&self.entry, &self.host_actions);
        let number = self.entry.number.get();
        let has_stack = self.entry.stack.is_some();
        let supports_sweep = self.on_close_sweep_start.is_some();
        let busy = self.busy;
        // `display: visible || busy ? "flex" : "none"`; no actions renders nothing at all.
        let show = !actions.is_empty() && (self.visible || busy);
        div()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .pr_3()
            .map(|this| if show { this.flex() } else { this.hidden() })
            .children(actions.into_iter().map(|quick| {
                let label = pull_request_speed_action_label(quick);
                let action = quick_action_to_action(quick);
                let tooltip = pull_request_speed_action_tooltip(quick, has_stack, supports_sweep);
                let on_perform = self.on_perform.clone();
                let button = Button::new(SharedString::from(format!("pr-speed-{number}-{label}")))
                    .variant(if quick == PullRequestQuickAction::Close {
                        ButtonVariant::DestructiveOutline
                    } else {
                        ButtonVariant::Outline
                    })
                    .size(ButtonSize::Xs)
                    .disabled(
                        !self.can_write
                            || busy
                            || (quick == PullRequestQuickAction::Merge && has_stack),
                    )
                    .tooltip(Tooltip::new(tooltip))
                    .child(if busy {
                        Spinner::new(SharedString::from(format!(
                            "pr-speed-spin-{number}-{label}"
                        )))
                        .size(rems(0.75))
                        .into_any_element()
                    } else {
                        Icon::new(speed_action_icon(quick))
                            .size(rems(0.75))
                            .into_any_element()
                    })
                    .child(label)
                    .on_click(move |_, window, cx| on_perform(action, window, cx));
                // `onPointerDown`: a press on Close may become a sweep across rows. The button
                // stops the press itself, so the wrapper listens before it.
                let sweep = self
                    .on_close_sweep_start
                    .clone()
                    .filter(|_| quick == PullRequestQuickAction::Close);
                div()
                    .when_some(sweep, |this, start| {
                        this.capture_any_mouse_down(move |event, window, cx| {
                            if event.button == MouseButton::Left {
                                start(event, window, cx);
                            }
                        })
                    })
                    .child(button)
            }))
    }
}
