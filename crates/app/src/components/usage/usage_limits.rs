//! Port of upstream/apps/web/src/components/usage/UsageLimits.tsx
//!
//! Subscription-limit building blocks: the window bars, pace glyph, banked
//! reset credits and the confirm for redeeming one, plus the environment and
//! provider filtering the Limits tab applies before pooling accounts
//! ([`select_limit_environments`], upstream's `UsageLimitsSection`).

use crate::theme::color::solid_over;
use std::collections::HashSet;
use std::rc::Rc;

use chrono::{DateTime, Datelike, Local};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, CursorStyle, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, relative, rems,
};
use turin_contracts::EnvironmentId;
use turin_contracts::provider_usage_limits::{
    ProviderConsumeResetCreditInput, ProviderConsumeResetCreditOutcome, ServerProviderResetCredits,
    ServerProviderUsageWindow,
};
use turin_contracts::usage::UsageProviderKind;
use turin_shared::usage_limits::{
    LimitPace, elapsed_share, format_duration, format_resets_in, pace_of, remaining_percent,
};

use super::backend::{LimitsEnvironment, ResetCreditError, ResetCreditResult, UsageBackend};
use super::usage_providers::{ChartColor, presentation};
use super::usage_ui::rich_tooltip;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::timestamp_format::{
    TimestampFormat, format_time_of_day, parse_timestamp_date, parse_timestamp_ms,
};
use crate::ui::dialog::{DialogOptions, dialog_footer, dialog_header, dialog_popup, open_dialog};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};
use gpui_kit::component::WindowExt as _;

/// The series colour the cost chart uses for this driver, so the two views read as one.
pub fn bar_color(driver: &str) -> ChartColor {
    let kind = match driver {
        "codex" => Some(UsageProviderKind::Codex),
        "claudeAgent" => Some(UsageProviderKind::Claude),
        "antigravity" => Some(UsageProviderKind::Antigravity),
        _ => None,
    };
    // `var(--foreground)` when the driver has no series colour.
    kind.map_or(ChartColor::Ink(100), |kind| presentation(kind).color)
}

pub fn pace_label(pace: LimitPace) -> &'static str {
    match pace {
        LimitPace::Ahead => "Ahead of pace: spending faster than the window elapses",
        LimitPace::On => "On pace with the window",
        LimitPace::Under => "Under pace: headroom left for the rest of the window",
    }
}

fn pace_icon_name(pace: LimitPace) -> IconName {
    match pace {
        LimitPace::Ahead => IconName::TrendingUp,
        LimitPace::On => IconName::Gauge,
        LimitPace::Under => IconName::TrendingDown,
    }
}

/// Pace as a glyph with the words on hover.
pub fn pace_icon(id: impl Into<SharedString>, pace: LimitPace, cx: &App) -> AnyElement {
    let label = pace_label(pace);
    div()
        .id(id.into())
        .flex()
        .text_color(cx.app_theme().muted_foreground)
        .child(Icon::new(pace_icon_name(pace)).size(rems(0.875)))
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .into_any_element()
}

/// `formatUpcomingTimestamp`: an instant that has not happened yet (a
/// usage-limit reset): today `12:34 PM`, tomorrow `tomorrow at 12:34 PM`, later
/// `8/13 12:34 PM` (with the year when it is not this one). A reset already in
/// a past day falls back to the numeric date and time.
pub fn format_upcoming_timestamp(iso: &str, format: TimestampFormat, now_ms: i64) -> String {
    let Some(date) = parse_timestamp_date(iso) else {
        return String::new();
    };
    let date = date.with_timezone(&Local);
    let time = format_time_of_day(&date, format);
    let Some(now) = DateTime::from_timestamp_millis(now_ms).map(|now| now.with_timezone(&Local))
    else {
        return time;
    };
    let day_diff = (date.date_naive() - now.date_naive()).num_days();
    let numeric_date = |with_year: bool| {
        if with_year {
            format!("{}/{}/{}", date.month(), date.day(), date.year())
        } else {
            format!("{}/{}", date.month(), date.day())
        }
    };
    match day_diff {
        0 => time,
        1 => format!("tomorrow at {time}"),
        d if d < 0 => format!("{} {time}", numeric_date(date.year() != now.year())),
        _ => format!("{} {time}", numeric_date(date.year() != now.year())),
    }
}

fn percent(value: f64) -> String {
    format!("{}", value as i64)
}

/// One window as a full-width bar from the moment it opened to its reset.
/// The fill is the share of quota left; the hairline is how far through the
/// window the clock is, which is also where even spending would have put the
/// fill. Hover for the exact figures and reset time.
pub fn window_bar(
    id: impl Into<SharedString>,
    color: Hsla,
    window: &ServerProviderUsageWindow,
    now: f64,
    timestamp_format: TimestampFormat,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let remaining = remaining_percent(window);
    let elapsed = elapsed_share(window, now);
    // The fill is quota left, so the even-spending mark is the time left.
    let time_left = elapsed.map(|elapsed| ((1. - elapsed) * 100.).round());
    let resets_in = format_resets_in(window, now);
    let resets_at = window
        .resets_at
        .as_deref()
        .map(|at| format_upcoming_timestamp(at, timestamp_format, now as i64))
        .filter(|text| !text.is_empty());

    let mut rows: Vec<(SharedString, bool)> = vec![(
        format!(
            "{}% left{}",
            percent(remaining),
            time_left.map_or_else(String::new, |left| format!(
                " · {}% of the window left",
                percent(left)
            ))
        )
        .into(),
        true,
    )];
    if time_left.is_some() {
        rows.push(("The line is where even spending would be.".into(), false));
    }
    if let Some(resets_at) = &resets_at {
        rows.push((
            format!(
                "Resets {resets_at}{}",
                resets_in
                    .as_ref()
                    .map_or_else(String::new, |text| format!(" · {text}"))
            )
            .into(),
            false,
        ));
    }

    let inset = rems(0.375);
    div()
        .id(id.into())
        .relative()
        .h(rems(1.5))
        .flex_1()
        .min_w_0()
        .rounded_full()
        .cursor(CursorStyle::Arrow)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(inset)
                .bottom(inset)
                .rounded_full()
                .bg(theme.muted),
        )
        .when(remaining > 0., |this| {
            this.child(
                div()
                    .absolute()
                    .top(inset)
                    .bottom(inset)
                    .left_0()
                    .w(relative((remaining / 100.) as f32))
                    .rounded_full()
                    .bg(color),
            )
        })
        .when_some(time_left, |this, left| {
            this.child(
                div()
                    .absolute()
                    .top(rems(0.125))
                    .bottom(rems(0.125))
                    .w(px(1.))
                    .left(relative((left / 100.) as f32))
                    .ml(px(-0.5))
                    .bg(solid_over(theme.foreground, 0.6, theme.background)),
            )
        })
        .tooltip(rich_tooltip(rows))
        .into_any_element()
}

/// One account's windows as rows: label and percent, bar, pace and countdown.
/// Compact rows fit the composer panel with narrower columns.
pub fn limit_windows(
    id: &str,
    driver: &str,
    windows: &[ServerProviderUsageWindow],
    now: f64,
    compact: bool,
    timestamp_format: TimestampFormat,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let color = bar_color(driver).resolve(theme);
    div()
        .flex()
        .flex_col()
        .gap(if compact { px(2.) } else { px(4.) })
        .children(windows.iter().map(|window| {
            let pace = pace_of(window, now);
            let resets_in = format_resets_in(window, now);
            let label_width = if compact { rems(9.) } else { rems(11.) };
            div()
                .flex()
                .items_center()
                .gap_x(if compact { px(12.) } else { px(16.) })
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .w(label_width)
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .text_size(TEXT_XS.size)
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(window.label.as_str().to_owned()),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .flex_shrink_0()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(format!("{}% left", percent(remaining_percent(window)))),
                        ),
                )
                .child(window_bar(
                    format!("{id}:{}", window.id.as_str()),
                    color,
                    window,
                    now,
                    timestamp_format,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .when(!compact, |this| this.w(rems(7.)))
                        .items_center()
                        .gap_2()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .when_some(pace, |this, pace| {
                            this.child(pace_icon(
                                format!("{id}:{}:pace", window.id.as_str()),
                                pace,
                                cx,
                            ))
                        })
                        .child(
                            div()
                                .ml_auto()
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .child(resets_in.unwrap_or_default()),
                        ),
                )
        }))
        .into_any_element()
}

pub fn outcome_text(outcome: ProviderConsumeResetCreditOutcome) -> &'static str {
    match outcome {
        ProviderConsumeResetCreditOutcome::Reset => "Reset applied. Your windows have cleared.",
        ProviderConsumeResetCreditOutcome::NothingToReset => "Nothing to reset right now.",
        ProviderConsumeResetCreditOutcome::NoCredit => "No reset credit left.",
        ProviderConsumeResetCreditOutcome::AlreadyRedeemed => "That credit was already redeemed.",
    }
}

/// What to tell the user after a redeem attempt.
pub fn reset_credit_status(result: &ResetCreditResult) -> String {
    match result {
        Ok(result) => result.warning.as_ref().map_or_else(
            || outcome_text(result.outcome).to_owned(),
            |w| w.as_str().to_owned(),
        ),
        Err(ResetCreditError::Message(message)) => message.clone(),
        Err(ResetCreditError::Unknown) => "Could not use the reset credit.".to_owned(),
    }
}

/// `2 reset credits banked · next expires in 27d 23h`, or the short form for a popover.
pub fn reset_credits_summary(
    credits: &ServerProviderResetCredits,
    now: f64,
    compact: bool,
) -> String {
    let expires_in = credits
        .next_expires_at
        .as_deref()
        .and_then(parse_timestamp_ms)
        .map(|at| format_duration(at as f64 - now));
    if credits.available_count == 0 {
        return "No reset credits banked".to_owned();
    }
    let count = credits.available_count;
    if compact {
        return format!(
            "{count} banked{}",
            expires_in.map_or_else(String::new, |text| format!(" · expires in {text}"))
        );
    }
    format!(
        "{count} {} banked{}",
        if count == 1 {
            "reset credit"
        } else {
            "reset credits"
        },
        expires_in.map_or_else(String::new, |text| format!(" · next expires in {text}"))
    )
}

/// The confirm for a redeem. Redeeming spends a credit the provider granted the
/// user, so it never fires on a bare click.
type ConfirmHandler = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct ResetCreditConfirm {
    on_confirm: ConfirmHandler,
    disabled: bool,
}

impl Render for ResetCreditConfirm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let on_confirm = self.on_confirm.clone();
        dialog_popup(window, false)
            .child(dialog_header(
                "Use a reset credit?",
                Some(
                    "This redeems one credit on your account and clears the current rate-limit windows. It cannot be undone."
                        .into(),
                ),
                false,
                &theme,
            ))
            .child(
                dialog_footer(&theme, false)
                    .child(
                        Button::new("reset-credit-cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("reset-credit-confirm")
                            .label("Use credit")
                            .disabled(self.disabled)
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                on_confirm(window, cx);
                            }),
                    ),
            )
    }
}

pub fn open_reset_credit_dialog(
    window: &mut Window,
    cx: &mut App,
    disabled: bool,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
) {
    let view = cx.new(|_| ResetCreditConfirm {
        on_confirm: Rc::new(on_confirm),
        disabled,
    });
    open_dialog(window, cx, DialogOptions::default(), view);
}

/// Banked reset credits with the redeem button and its confirm, self-contained.
pub struct ResetCredits {
    backend: Rc<dyn UsageBackend>,
    environment_id: EnvironmentId,
    input: ProviderConsumeResetCreditInput,
    credits: ServerProviderResetCredits,
    now: f64,
    busy: bool,
    status: Option<String>,
}

impl ResetCredits {
    pub fn new(
        backend: Rc<dyn UsageBackend>,
        environment_id: EnvironmentId,
        input: ProviderConsumeResetCreditInput,
        credits: ServerProviderResetCredits,
        now: f64,
    ) -> Self {
        Self {
            backend,
            environment_id,
            input,
            credits,
            now,
            busy: false,
            status: None,
        }
    }

    fn redeem(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let can_manage = self.backend.can_manage_providers(&self.environment_id);
        if !can_manage {
            return;
        }
        self.busy = true;
        self.status = None;
        cx.notify();
        let task =
            self.backend
                .consume_reset_credit(self.environment_id.clone(), self.input.clone(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, _, cx| {
                this.busy = false;
                this.status = Some(reset_credit_status(&result));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ResetCredits {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        if self.credits.available_count == 0 && self.status.is_none() {
            return div().into_any_element();
        }
        let can_manage = self.backend.can_manage_providers(&self.environment_id);
        let entity: Entity<Self> = cx.entity();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_1()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(reset_credits_summary(&self.credits, self.now, false))
            .when(self.credits.available_count > 0, |this| {
                this.child(
                    Button::new("reset-credits-use")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Xs)
                        .label(if self.busy { "Using…" } else { "Use reset" })
                        .disabled(self.busy || !can_manage)
                        .on_click(move |_, window, cx| {
                            let entity = entity.clone();
                            open_reset_credit_dialog(window, cx, !can_manage, move |window, cx| {
                                entity.update(cx, |this, cx| this.redeem(window, cx));
                            });
                        }),
                )
            })
            .children(
                self.status
                    .clone()
                    .map(|status| div().text_color(theme.foreground).child(status)),
            )
            .into_any_element()
    }
}

/// Environments the Limits tab pools: the selected ones, with hidden
/// providers' drivers dropped from both native accounts and hub accounts, so
/// their bars, notices, and external links all drop out together. A hub left
/// with no visible accounts is dropped, not reported as empty.
pub fn select_limit_environments(
    environments: &[LimitsEnvironment],
    selected_environment_ids: Option<&HashSet<EnvironmentId>>,
    hidden_providers: &HashSet<UsageProviderKind>,
) -> Vec<LimitsEnvironment> {
    let hidden_drivers: HashSet<&'static str> = hidden_providers
        .iter()
        .map(|provider| presentation(*provider).driver_kind)
        .collect();
    let is_visible = |driver: &str| !hidden_drivers.contains(driver);
    environments
        .iter()
        .filter(|environment| {
            selected_environment_ids.is_none_or(|ids| ids.contains(&environment.environment_id))
        })
        .map(|environment| {
            if hidden_drivers.is_empty() || !environment.has_server_config {
                return environment.clone();
            }
            LimitsEnvironment {
                providers: environment
                    .providers
                    .iter()
                    .filter(|provider| is_visible(provider.driver.as_str()))
                    .cloned()
                    .collect(),
                usage_limit_sources: environment
                    .usage_limit_sources
                    .iter()
                    .filter_map(|source| {
                        let accounts: Vec<_> = source
                            .accounts
                            .iter()
                            .filter(|account| is_visible(account.driver.as_str()))
                            .cloned()
                            .collect();
                        if accounts.is_empty() && !source.accounts.is_empty() {
                            return None;
                        }
                        let mut source = source.clone();
                        source.accounts = accounts;
                        Some(source)
                    })
                    .collect(),
                ..environment.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::usage::backend::LimitsProvider;
    use serde_json::json;
    use turin_contracts::provider_usage_limits::ServerProviderUsageLimits;

    fn credits(count: u64, expires: Option<&str>) -> ServerProviderResetCredits {
        serde_json::from_value(match expires {
            Some(at) => json!({ "availableCount": count, "nextExpiresAt": at }),
            None => json!({ "availableCount": count }),
        })
        .unwrap()
    }

    #[test]
    fn credit_summary_reads_like_upstream() {
        let now = parse_timestamp_ms("2026-09-03T12:00:00.000Z").unwrap() as f64;
        let expiring = credits(2, Some("2026-10-01T11:00:00.000Z"));
        assert_eq!(
            reset_credits_summary(&expiring, now, false),
            "2 reset credits banked · next expires in 27d 23h"
        );
        assert_eq!(
            reset_credits_summary(&expiring, now, true),
            "2 banked · expires in 27d 23h"
        );
        assert_eq!(
            reset_credits_summary(&credits(1, None), now, false),
            "1 reset credit banked"
        );
        assert_eq!(
            reset_credits_summary(&credits(0, None), now, false),
            "No reset credits banked"
        );
    }

    #[test]
    fn redeem_status_prefers_the_warning_then_the_outcome() {
        let ok = |outcome, warning: Option<&str>| -> ResetCreditResult {
            Ok(
                turin_contracts::provider_usage_limits::ProviderConsumeResetCreditResult {
                    outcome,
                    warning: warning
                        .map(|w| turin_contracts::TrimmedNonEmptyString::new(w).unwrap()),
                },
            )
        };
        assert_eq!(
            reset_credit_status(&ok(ProviderConsumeResetCreditOutcome::Reset, None)),
            "Reset applied. Your windows have cleared."
        );
        assert_eq!(
            reset_credit_status(&ok(
                ProviderConsumeResetCreditOutcome::Reset,
                Some("Cooldown not cleared.")
            )),
            "Cooldown not cleared."
        );
        assert_eq!(
            reset_credit_status(&Err(ResetCreditError::Message("offline".into()))),
            "offline"
        );
        assert_eq!(
            reset_credit_status(&Err(ResetCreditError::Unknown)),
            "Could not use the reset credit."
        );
    }

    #[test]
    fn bar_color_follows_the_chart_series() {
        assert_eq!(
            bar_color("codex"),
            presentation(UsageProviderKind::Codex).color
        );
        assert_eq!(
            bar_color("claudeAgent"),
            presentation(UsageProviderKind::Claude).color
        );
        assert_eq!(bar_color("cursor"), ChartColor::Ink(100));
    }

    fn provider(instance: &str, driver: &str) -> LimitsProvider {
        LimitsProvider {
            instance_id: turin_contracts::provider_instance::ProviderInstanceId::new(instance)
                .unwrap(),
            driver: turin_contracts::provider_instance::ProviderDriverKind::new(driver).unwrap(),
            enabled: true,
            installed: true,
            available: true,
            display_name: None,
            accent_color: None,
            authenticated: true,
            auth_email: None,
            auth_label: None,
            auth_workspace_id: None,
            subscription_sharing: false,
            usage_limits: None::<ServerProviderUsageLimits>,
            slash_commands: Vec::new(),
        }
    }

    fn environment(id: &str, providers: Vec<LimitsProvider>) -> LimitsEnvironment {
        LimitsEnvironment {
            environment_id: EnvironmentId::new(id).unwrap(),
            label: id.to_owned(),
            connected: true,
            has_server_config: true,
            providers,
            usage_limit_sources: Vec::new(),
        }
    }

    #[test]
    fn selection_and_hidden_providers_narrow_the_pooled_environments() {
        let environments = vec![
            environment(
                "a",
                vec![
                    provider("codex", "codex"),
                    provider("claude", "claudeAgent"),
                ],
            ),
            environment("b", vec![provider("codex", "codex")]),
        ];
        let all = select_limit_environments(&environments, None, &HashSet::new());
        assert_eq!(all.len(), 2);

        let only_a = HashSet::from([EnvironmentId::new("a").unwrap()]);
        let hidden = HashSet::from([UsageProviderKind::Codex]);
        let narrowed = select_limit_environments(&environments, Some(&only_a), &hidden);
        assert_eq!(narrowed.len(), 1);
        let drivers: Vec<_> = narrowed[0]
            .providers
            .iter()
            .map(|provider| provider.driver.as_str().to_owned())
            .collect();
        assert_eq!(drivers, ["claudeAgent"]);
    }

    #[test]
    fn upcoming_timestamps_name_today_and_tomorrow() {
        let now = Local::now();
        let now_ms = now.timestamp_millis();
        let today = (now + chrono::Duration::minutes(1)).to_rfc3339();
        let tomorrow = (now + chrono::Duration::days(1)).to_rfc3339();
        assert!(
            !format_upcoming_timestamp(&today, TimestampFormat::TwentyFourHour, now_ms)
                .contains("tomorrow")
        );
        assert!(
            format_upcoming_timestamp(&tomorrow, TimestampFormat::TwentyFourHour, now_ms)
                .starts_with("tomorrow at ")
        );
        assert_eq!(
            format_upcoming_timestamp("nope", TimestampFormat::Locale, now_ms),
            ""
        );
    }
}
