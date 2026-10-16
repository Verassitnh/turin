//! Port of upstream/apps/web/src/components/usage/UsageModelDialog.tsx
//!
//! One model's usage in the current window, opened from the Breakdown list.
//! The trend and mixes come from the same merge as the page, narrowed to this
//! model's buckets.

use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement as _, Render, Styled as _,
    Window, div, px, relative, rems,
};
use turin_contracts::usage::UsageResolution;
use turin_shared::usage_format::{format_percent, format_tokens, format_usd};
use turin_shared::usage_merge::{ModelTotals, is_model_cost_unknown};

use super::usage_breakdown::{
    cache_hit_rate, cost_per_million_tokens, cost_type_segments, speed_cost_segments,
    token_type_segments,
};
use super::usage_provider_chart::{UsageChartMetric, UsageProviderChart, UsageProviderChartProps};
use super::usage_providers::presentation;
use super::usage_share_bar::UsageShareBar;
use super::view_model::{EnvironmentUsageStatus, merge_answered_usage};
use crate::components::provider_instance_icon::provider_instance_icon;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_XL, TEXT_XS};
use crate::ui::Button;
use crate::ui::dialog::{dialog_footer, dialog_panel, dialog_popup};

/// The period axis the page's chart uses, handed to the dialog's chart.
#[derive(Clone, Debug)]
pub struct UsageChartWindow {
    pub days: Vec<String>,
    pub hours: Vec<String>,
    pub resolution: UsageResolution,
    pub time_zone: String,
    pub reference_time: Option<String>,
}

type SetPrice = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct UsageModelDialog {
    pub model: ModelTotals,
    pub environments: Vec<EnvironmentUsageStatus>,
    pub metric: UsageChartMetric,
    pub chart_window: UsageChartWindow,
    pub on_set_price: SetPrice,
}

/// What the faster speeds cost above standard rates, beside the speed bar.
pub fn speed_premium(premium_usd: f64, cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(TEXT_XS.size)
        .text_color(theme.muted_foreground)
        .child("Premium")
        .child(
            div()
                .text_color(theme.foreground)
                .child(format_usd(premium_usd)),
        )
        .into_any_element()
}

impl Render for UsageModelDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let model = self.model.clone();
        let usage = merge_answered_usage(
            &self.environments,
            Some(&|bucket| {
                bucket.provider == model.provider && bucket.model.as_str() == model.model
            }),
        );
        let presentation = presentation(model.provider);
        let cost_unknown = is_model_cost_unknown(&model);
        let hit_rate = cache_hit_rate(&model);
        let per_million = cost_per_million_tokens(&model);
        let stats: Vec<(&str, String)> = [
            Some((
                "Cost",
                if cost_unknown {
                    "Unpriced".to_owned()
                } else {
                    format_usd(model.cost_usd)
                },
            )),
            Some(("Tokens", format_tokens(model.total_tokens as f64))),
            per_million.map(|value| ("Per 1M tokens", format_usd(value))),
            hit_rate.map(|rate| ("Cache hit", format_percent(rate, 1))),
        ]
        .into_iter()
        .flatten()
        .collect();

        let chart = UsageProviderChart::new(
            "model-dialog-chart",
            UsageProviderChartProps {
                providers: vec![model.provider],
                loading_providers: HashSet::new(),
                days: self.chart_window.days.clone(),
                daily: usage.daily.clone(),
                hours: self.chart_window.hours.clone(),
                hourly: usage.hourly.clone(),
                // Unpriced cost is unknown, not zero, so its trend shows tokens.
                metric: if cost_unknown {
                    UsageChartMetric::Tokens
                } else {
                    self.metric
                },
                reference_time: self.chart_window.reference_time.clone(),
                resolution: self.chart_window.resolution,
                time_zone: self.chart_window.time_zone.clone(),
            },
        );

        let mut shares: Vec<AnyElement> = Vec::new();
        if !cost_unknown {
            shares.push(
                UsageShareBar::new(
                    "Cost by type",
                    cost_type_segments(&usage.category_cost),
                    format_usd,
                )
                .into_any_element(),
            );
        }
        shares.push(
            UsageShareBar::new(
                "Tokens by type",
                token_type_segments((&model.tokens).into()),
                format_tokens,
            )
            .into_any_element(),
        );
        if usage.speed_cost.fast + usage.speed_cost.ultrafast > 0. {
            shares.push(
                UsageShareBar::new(
                    "Cost by speed",
                    speed_cost_segments(&usage.speed_cost),
                    format_usd,
                )
                .aside(speed_premium(usage.speed_cost.premium, cx))
                .into_any_element(),
            );
        }

        let on_set_price = self.on_set_price.clone();
        dialog_popup(window, true)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap_2()
                    .p_6()
                    .pb_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(provider_instance_icon(
                                presentation.driver_kind,
                                presentation.label,
                                rems(1.25),
                                None,
                                cx,
                            ))
                            .child(
                                div()
                                    .text_size(TEXT_XL.size)
                                    .line_height(relative(1.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(model.model.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::tokens::TEXT_SM.size)
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{}{}",
                                presentation.label,
                                if cost_unknown {
                                    String::new()
                                } else {
                                    format!(" · {} of cost", format_percent(model.cost_share, 1))
                                }
                            )),
                    ),
            )
            .child(
                dialog_panel("usage-model-panel", true, model.unpriced_tokens > 0.).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_8()
                        .child(
                            div()
                                .grid()
                                .grid_cols(stats.len().min(4) as u16)
                                .gap_x_6()
                                .gap_y_4()
                                .children(stats.iter().map(|(label, value)| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .gap(px(2.))
                                        .child(
                                            div()
                                                .text_size(TEXT_XS.size)
                                                .text_color(theme.muted_foreground)
                                                .child(*label),
                                        )
                                        .child(
                                            div()
                                                .text_size(rems(1.5))
                                                .line_height(rems(2.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.foreground)
                                                .child(value.clone()),
                                        )
                                })),
                        )
                        .child(chart)
                        .child(
                            div()
                                .grid()
                                .grid_cols(2)
                                .gap_x(px(40.))
                                .gap_y_8()
                                .children(shares),
                        ),
                ),
            )
            .when(model.unpriced_tokens > 0., |this| {
                this.child(
                    dialog_footer(&theme, true)
                        .justify_between()
                        .child(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} tokens have no known price",
                                    format_tokens(model.unpriced_tokens)
                                )),
                        )
                        .child(
                            Button::new("usage-model-set-price")
                                .label("Set price")
                                .on_click(move |_, window, cx| on_set_price(window, cx)),
                        ),
                )
            })
    }
}
