//! Port of upstream/apps/web/src/components/CustomSnoozeDialog.tsx
//!
//! "Custom snooze": a date and time, or a duration, resolved with
//! `resolveCustomSnooze`. The date is typed (`YYYY-MM-DD`) rather than
//! picked from a calendar popover, the time is a text field rather than a
//! native `type="time"` input, and the amount is a plain field rather than a
//! number stepper; see Deviations.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, div, px, rems,
};

use super::runtime::thread_settled::{
    CustomSnoozeInput, CustomSnoozeUnit, local_snooze_date, local_snooze_time,
    resolve_custom_snooze,
};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::toggle::{Toggle, ToggleSize, ToggleVariant};
use crate::ui::{Button, ButtonSize, ButtonVariant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Date,
    Duration,
}

type OnChoose = Box<dyn Fn(String, &mut Window, &mut App)>;

pub struct CustomSnoozeForm {
    mode: Mode,
    date: Entity<InputState>,
    time: Entity<InputState>,
    amount: Entity<InputState>,
    unit: CustomSnoozeUnit,
    error: Option<&'static str>,
    on_choose: OnChoose,
    _subscriptions: Vec<Subscription>,
}

impl CustomSnoozeForm {
    fn new(on_choose: OnChoose, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial = chrono::Local::now() + chrono::Duration::hours(1);
        let date =
            cx.new(|cx| InputState::new(window, cx).default_value(local_snooze_date(&initial)));
        let time =
            cx.new(|cx| InputState::new(window, cx).default_value(local_snooze_time(&initial)));
        let amount = cx.new(|cx| InputState::new(window, cx).default_value("2"));
        let clear = |this: &mut Self,
                     _: &Entity<InputState>,
                     event: &InputEvent,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            match event {
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(&date, window, clear),
            cx.subscribe_in(&time, window, clear),
            cx.subscribe_in(&amount, window, clear),
        ];
        Self {
            mode: Mode::Date,
            date,
            time,
            amount,
            unit: CustomSnoozeUnit::Hours,
            error: None,
            on_choose,
            _subscriptions: subscriptions,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.mode {
            Mode::Date => CustomSnoozeInput::Date {
                date: self.date.read(cx).value().trim().to_string(),
                time: self.time.read(cx).value().trim().to_string(),
            },
            Mode::Duration => CustomSnoozeInput::Duration {
                amount: self.amount.read(cx).value().to_string(),
                unit: self.unit,
            },
        };
        match resolve_custom_snooze(&input, &chrono::Local::now()) {
            Some(snoozed_until) => {
                window.close_dialog(cx);
                (self.on_choose)(snoozed_until, window, cx);
            }
            None => {
                self.error = Some(match self.mode {
                    Mode::Date => "Choose a valid date and time in the future.",
                    Mode::Duration => "Enter a positive duration.",
                });
                cx.notify();
            }
        }
    }
}

fn field(label: &'static str, input: &Entity<InputState>, cx: &App) -> impl IntoElement {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .flex_col()
        .gap(rems(0.375))
        .child(
            div()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.app_theme().foreground)
                .child(label),
        )
        .child(gpui_kit::component::input::Input::new(input))
}

impl Render for CustomSnoozeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let mode = self.mode;
        let mode_toggle = |label: &'static str, value: Mode, cx: &mut Context<Self>| {
            Toggle::new(label)
                .variant(ToggleVariant::Segmented)
                .size(ToggleSize::Sm)
                .label(label)
                .pressed(mode == value)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mode = value;
                    this.error = None;
                    cx.notify();
                }))
        };
        let unit_toggle = |label: &'static str,
                           value: CustomSnoozeUnit,
                           current: CustomSnoozeUnit,
                           cx: &mut Context<Self>| {
            Toggle::new(label)
                .variant(ToggleVariant::Segmented)
                .size(ToggleSize::Sm)
                .label(label)
                .pressed(current == value)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.unit = value;
                    cx.notify();
                }))
        };
        let unit = self.unit;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(340.))
            .child(
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("Choose when snoozed threads return to your inbox."),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .gap_1()
                    .child(mode_toggle("Date and time", Mode::Date, cx))
                    .child(mode_toggle("Duration", Mode::Duration, cx)),
            )
            .map(|this| match mode {
                Mode::Date => this.child(
                    div()
                        .flex()
                        .gap_3()
                        .child(field("Date", &self.date, cx))
                        .child(field("Time", &self.time, cx)),
                ),
                Mode::Duration => this.child(
                    div()
                        .flex()
                        .gap_3()
                        .child(field("Snooze for", &self.amount, cx))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(rems(0.375))
                                .child(
                                    div()
                                        .text_size(TEXT_SM.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Unit"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_1()
                                        .child(unit_toggle(
                                            "Minutes",
                                            CustomSnoozeUnit::Minutes,
                                            unit,
                                            cx,
                                        ))
                                        .child(unit_toggle(
                                            "Hours",
                                            CustomSnoozeUnit::Hours,
                                            unit,
                                            cx,
                                        ))
                                        .child(unit_toggle(
                                            "Days",
                                            CustomSnoozeUnit::Days,
                                            unit,
                                            cx,
                                        )),
                                ),
                        ),
                ),
            })
            .when_some(self.error, |this, error| {
                this.child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.destructive_foreground)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("custom-snooze-cancel")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Sm)
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("custom-snooze-submit")
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::Sm)
                            .label("Snooze")
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

/// `requestCustomSnooze()`: opens the dialog; `on_choose` gets the ISO wake time.
pub fn request_custom_snooze(
    window: &mut Window,
    cx: &mut App,
    on_choose: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let form = cx.new(|cx| CustomSnoozeForm::new(Box::new(on_choose), window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title("Custom snooze").child(form.clone())
    });
}
