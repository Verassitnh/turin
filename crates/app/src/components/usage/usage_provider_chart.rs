//! Port of upstream/apps/web/src/components/usage/UsageProviderChart.tsx
//!
//! The chart's pure math (scale, columns, monotone curve) first; the GPUI
//! drawing is below it.

use std::collections::{HashMap, HashSet};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Bounds, DispatchPhase, ElementId, Hsla, InteractiveElement as _, IntoElement,
    MouseMoveEvent, ParentElement as _, PathBuilder, RenderOnce, Styled as _, Window, anchored,
    canvas, deferred, div, fill, point, px, rems, size,
};
use indexmap::IndexMap;
use turin_contracts::usage::{UsageProviderKind, UsageResolution};
use turin_shared::usage_format::{
    format_day_short, format_hour_short, format_relative_hour_short, format_tokens, format_usd,
};
use turin_shared::usage_merge::{DailyTotals, HourlyTotals, ProviderSpend};

use super::usage_providers::{PROVIDER_ORDER, presentation, provider_slot};
use crate::components::provider_instance_icon::provider_instance_icon;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_XL, TEXT_3XS, TEXT_XS};

pub const VIEW_WIDTH: f32 = 960.;
pub const VIEW_HEIGHT: f32 = 260.;
pub const TICK_COUNT: usize = 4;
pub const PLOT_TOP: f32 = 8.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageChartMetric {
    Tokens,
    Cost,
}

/// One band of a [`DayColumn`]: a provider's value for the period.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub provider: UsageProviderKind,
    pub value: f64,
}

/// One period's per-provider values, shared by the paths and the hover readout.
#[derive(Clone, Debug, PartialEq)]
pub struct DayColumn {
    pub bands: Vec<Band>,
    pub total: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A period's per-provider spend (`DailyTotals.byProvider` / `HourlyTotals.byProvider`).
pub type ByProvider = IndexMap<UsageProviderKind, ProviderSpend>;

fn value_for(
    totals: Option<&ByProvider>,
    provider: UsageProviderKind,
    metric: UsageChartMetric,
) -> f64 {
    let Some(entry) = totals.and_then(|totals| totals.get(&provider)) else {
        return 0.;
    };
    match metric {
        UsageChartMetric::Tokens => entry.total_tokens as f64,
        UsageChartMetric::Cost => entry.cost_usd,
    }
}

pub fn build_period_columns(
    periods: &[String],
    by_period: &HashMap<String, &ByProvider>,
    metric: UsageChartMetric,
) -> Vec<DayColumn> {
    periods
        .iter()
        .map(|period| {
            let entry = by_period.get(period).copied();
            let bands: Vec<Band> = PROVIDER_ORDER
                .iter()
                .map(|provider| Band {
                    provider: *provider,
                    value: value_for(entry, *provider, metric),
                })
                .collect();
            let total = bands.iter().map(|band| band.value).sum();
            DayColumn { bands, total }
        })
        .collect()
}

/// Shape-preserving cubic tangents that cannot overshoot spiky usage data.
fn monotone_tangents(points: &[Point]) -> Vec<f32> {
    let count = points.len();
    if count < 2 {
        return vec![0.];
    }

    let slopes: Vec<f32> = points
        .windows(2)
        .map(|pair| {
            let dx = pair[1].x - pair[0].x;
            let dy = pair[1].y - pair[0].y;
            if dx == 0. { 0. } else { dy / dx }
        })
        .collect();

    let mut tangents = vec![0.; count];
    tangents[0] = slopes[0];
    tangents[count - 1] = slopes[count - 2];
    for index in 1..count - 1 {
        let previous = slopes[index - 1];
        let next = slopes[index];
        tangents[index] = if previous * next <= 0. {
            0.
        } else {
            (previous + next) / 2.
        };
    }

    for index in 0..count - 1 {
        let slope = slopes[index];
        if slope == 0. {
            tangents[index] = 0.;
            tangents[index + 1] = 0.;
            continue;
        }
        let a = tangents[index] / slope;
        let b = tangents[index + 1] / slope;
        let magnitude = a * a + b * b;
        if magnitude > 9. {
            let scale = 3. / magnitude.sqrt();
            tangents[index] = scale * a * slope;
            tangents[index + 1] = scale * b * slope;
        }
    }

    tangents
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveSegment {
    pub from: Point,
    pub c1: Point,
    pub c2: Point,
    pub to: Point,
}

pub fn smooth_curve(points: &[Point]) -> Vec<CurveSegment> {
    if points.len() < 2 {
        return Vec::new();
    }
    let tangents = monotone_tangents(points);
    points
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            let (from, to) = (pair[0], pair[1]);
            let dx = to.x - from.x;
            CurveSegment {
                from,
                c1: Point {
                    x: from.x + dx / 3.,
                    y: from.y + (tangents[index] * dx) / 3.,
                },
                c2: Point {
                    x: to.x - dx / 3.,
                    y: to.y - (tangents[index + 1] * dx) / 3.,
                },
                to,
            }
        })
        .collect()
}

/// Builds a scale whose maximum is a readable 1/2/5 x 10^n step at or above the
/// peak.
///
/// Rounding the maximum *up* is the point: stopping at the last step below the
/// peak leaves the tallest day drawn past the top of the plot, where it is
/// clipped.
pub fn nice_scale(peak: f64, count: usize) -> (f64, Vec<f64>) {
    if peak <= 0. {
        return (0., vec![0.]);
    }

    let raw_step = peak / count as f64;
    let magnitude = 10f64.powf(raw_step.log10().floor());
    let normalized = raw_step / magnitude;
    let step = (if normalized > 5. {
        10.
    } else if normalized > 2. {
        5.
    } else if normalized > 1. {
        2.
    } else {
        1.
    }) * magnitude;

    let max = (peak / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut value = 0.;
    while value <= max + step * 1e-6 {
        ticks.push(value);
        value += step;
    }
    (max, ticks)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChartScale {
    pub max: f64,
    pub ticks: Vec<f64>,
    pub labeled: bool,
}

/// Scales to the largest single provider-period. With nothing to show yet while
/// providers load, unlabeled placeholder gridlines hold their usual spacing so
/// nothing shifts on arrival.
pub fn chart_scale(
    columns: &[DayColumn],
    loading_providers: &HashSet<UsageProviderKind>,
) -> ChartScale {
    // Not the sum: layered series each measure from zero, so a combined peak
    // would leave the plot permanently half empty.
    let peak = columns
        .iter()
        .flat_map(|column| column.bands.iter())
        .fold(0f64, |max, band| max.max(band.value));
    if peak == 0. && !loading_providers.is_empty() {
        return ChartScale {
            max: TICK_COUNT as f64,
            ticks: (0..=TICK_COUNT).map(|index| index as f64).collect(),
            labeled: false,
        };
    }
    let (max, ticks) = nice_scale(peak, TICK_COUNT);
    ChartScale {
        max,
        ticks,
        labeled: true,
    }
}

/// Leave room above the top gridline so the constant-width stroke is not
/// clipped when a series reaches the peak.
pub fn value_to_y(value: f64, max: f64) -> f32 {
    if max == 0. {
        VIEW_HEIGHT
    } else {
        VIEW_HEIGHT - (value / max) as f32 * (VIEW_HEIGHT - PLOT_TOP)
    }
}

/// One provider's line in view-box coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderPath {
    pub provider: UsageProviderKind,
    pub loading: bool,
    pub total: f64,
    pub segments: Vec<CurveSegment>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub columns: Vec<DayColumn>,
    pub scale: ChartScale,
    pub step_x: f32,
    /// Paint order, heaviest first.
    pub paths: Vec<ProviderPath>,
}

pub fn build_chart(
    periods: &[String],
    by_period: &HashMap<String, &ByProvider>,
    metric: UsageChartMetric,
    providers: &[UsageProviderKind],
    loading_providers: &HashSet<UsageProviderKind>,
) -> Chart {
    let columns = build_period_columns(periods, by_period, metric);
    let scale = chart_scale(&columns, loading_providers);
    let step_x = if periods.len() < 2 {
        0.
    } else {
        VIEW_WIDTH / (periods.len() - 1) as f32
    };
    let mut paths: Vec<ProviderPath> = providers
        .iter()
        .map(|provider| {
            let slot = provider_slot(*provider);
            let points: Vec<Point> = columns
                .iter()
                .enumerate()
                .map(|(period_index, column)| Point {
                    x: period_index as f32 * step_x,
                    y: value_to_y(
                        column.bands.get(slot).map_or(0., |band| band.value),
                        scale.max,
                    ),
                })
                .collect();
            ProviderPath {
                provider: *provider,
                loading: loading_providers.contains(provider),
                total: columns
                    .iter()
                    .map(|column| column.bands.get(slot).map_or(0., |band| band.value))
                    .sum(),
                segments: smooth_curve(&points),
            }
        })
        .collect();
    // Paint the heavier series first so the lighter one is not buried.
    paths.sort_by(|a, b| b.total.total_cmp(&a.total));
    Chart {
        columns,
        scale,
        step_x,
        paths,
    }
}

// ── Drawing ─────────────────────────────────────────────────────────────────

/// The data a chart draws; mirrors `UsageProviderChartProps`.
#[derive(Clone, Debug)]
pub struct UsageProviderChartProps {
    pub providers: Vec<UsageProviderKind>,
    /// Providers whose figures are still coming in: their lines are muted.
    pub loading_providers: HashSet<UsageProviderKind>,
    pub days: Vec<String>,
    pub daily: Vec<DailyTotals>,
    pub hours: Vec<String>,
    pub hourly: Vec<HourlyTotals>,
    pub metric: UsageChartMetric,
    pub reference_time: Option<String>,
    pub resolution: UsageResolution,
    pub time_zone: String,
}

/// Plot height: `h-56`.
const PLOT_HEIGHT: f32 = 224.;
/// Axis label column: `w-14`.
const AXIS_WIDTH: f32 = 56.;
/// `text-3xs` line height (10px/14px), for the `-translate-y-1/2` of a tick label.
const TICK_LABEL_HEIGHT: f32 = 14.;
/// Tooltip offset from the pointer (`gap = 12`).
const TOOLTIP_GAP: f32 = 12.;

/// Pointer state over the plot; the tooltip follows `position` (window coordinates).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ChartHover {
    index: Option<usize>,
    position: gpui_kit::Point<gpui_kit::Pixels>,
}

fn format_value(metric: UsageChartMetric, value: f64) -> String {
    match metric {
        UsageChartMetric::Tokens => format_tokens(value),
        UsageChartMetric::Cost => format_usd(value),
    }
}

#[derive(IntoElement)]
pub struct UsageProviderChart {
    id: ElementId,
    props: UsageProviderChartProps,
}

impl UsageProviderChart {
    pub fn new(id: impl Into<ElementId>, props: UsageProviderChartProps) -> Self {
        Self {
            id: id.into(),
            props,
        }
    }
}

impl UsageProviderChartProps {
    fn format_period(&self, period: &str) -> String {
        match self.resolution {
            UsageResolution::Hour => format_hour_short(period, Some(&self.time_zone))
                .unwrap_or_else(|_| period.to_owned()),
            UsageResolution::Day => format_day_short(period),
        }
    }

    fn format_tooltip_period(&self, period: &str) -> String {
        match (&self.resolution, &self.reference_time) {
            (UsageResolution::Hour, Some(reference)) => {
                format_relative_hour_short(period, reference, Some(&self.time_zone))
                    .unwrap_or_else(|_| period.to_owned())
            }
            _ => self.format_period(period),
        }
    }
}

impl RenderOnce for UsageProviderChart {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let UsageProviderChart { id, props } = self;
        let theme = cx.app_theme().clone();
        let hover_state =
            window.use_keyed_state((id.clone(), "hover"), cx, |_, _| ChartHover::default());
        let hover = *hover_state.read(cx);

        let periods: &[String] = match props.resolution {
            UsageResolution::Hour => &props.hours,
            UsageResolution::Day => &props.days,
        };
        let by_period: HashMap<String, &ByProvider> = match props.resolution {
            UsageResolution::Hour => props
                .hourly
                .iter()
                .map(|entry| (entry.hour_start.clone(), &entry.by_provider))
                .collect(),
            UsageResolution::Day => props
                .daily
                .iter()
                .map(|entry| (entry.day.clone(), &entry.by_provider))
                .collect(),
        };
        let chart = build_chart(
            periods,
            &by_period,
            props.metric,
            &props.providers,
            &props.loading_providers,
        );
        let metric = props.metric;
        // The delay keeps a quick answer from flashing, as with the page's figures.
        let series_alpha = |loading: bool| if loading { 0.4 } else { 1.0 };

        // Axis labels sit outside the plot so they stay aligned to gridlines.
        let ticks = chart.scale.ticks.clone();
        let axis = div()
            .relative()
            .flex_shrink_0()
            .w(px(AXIS_WIDTH))
            .h(px(PLOT_HEIGHT))
            .when(chart.scale.labeled, |this| {
                this.children(ticks.iter().map(|tick| {
                    let y = value_to_y(*tick, chart.scale.max) / VIEW_HEIGHT * PLOT_HEIGHT;
                    div()
                        .absolute()
                        .right_0()
                        .top(px(y - TICK_LABEL_HEIGHT / 2.))
                        .text_size(TEXT_3XS.size)
                        .line_height(px(TICK_LABEL_HEIGHT))
                        .text_color(theme.muted_foreground)
                        .child(if *tick == 0. {
                            "0".to_owned()
                        } else {
                            format_value(metric, *tick)
                        })
                }))
            });

        // Fills first, then every stroke, so no series covers another's line.
        let series: Vec<(Hsla, bool, Vec<CurveSegment>)> = chart
            .paths
            .iter()
            .map(|path| {
                (
                    presentation(path.provider).color.resolve(&theme),
                    path.loading,
                    path.segments.clone(),
                )
            })
            .collect();
        let grid_color = theme.border;
        let hover_line = theme.muted_foreground;
        let step_x = chart.step_x;
        let tick_ys: Vec<f32> = chart
            .scale
            .ticks
            .iter()
            .map(|tick| value_to_y(*tick, chart.scale.max))
            .collect();
        let period_count = periods.len();
        let hover_index = hover.index;

        let on_hover = hover_state.clone();
        let plot_canvas = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                let origin = bounds.origin;
                let width = f32::from(bounds.size.width);
                let height = f32::from(bounds.size.height);
                let map = |x: f32, y: f32| {
                    point(
                        origin.x + px(x / VIEW_WIDTH * width),
                        origin.y + px(y / VIEW_HEIGHT * height),
                    )
                };
                for y in &tick_ys {
                    let top = origin.y + px(y / VIEW_HEIGHT * height) - px(0.5);
                    window.paint_quad(fill(
                        Bounds::new(point(origin.x, top), size(bounds.size.width, px(1.))),
                        grid_color,
                    ));
                }
                for (color, loading, segments) in &series {
                    let Some(first) = segments.first() else {
                        continue;
                    };
                    let mut area = PathBuilder::fill();
                    area.move_to(map(first.from.x, first.from.y));
                    for segment in segments {
                        area.cubic_bezier_to(
                            map(segment.to.x, segment.to.y),
                            map(segment.c1.x, segment.c1.y),
                            map(segment.c2.x, segment.c2.y),
                        );
                    }
                    area.line_to(map(VIEW_WIDTH, VIEW_HEIGHT));
                    area.line_to(map(0., VIEW_HEIGHT));
                    area.close();
                    if let Ok(path) = area.build() {
                        window.paint_path(
                            path,
                            with_alpha_hsla(*color, 0.12 * series_alpha(*loading)),
                        );
                    }
                }
                for (color, loading, segments) in &series {
                    let Some(first) = segments.first() else {
                        continue;
                    };
                    let mut line = PathBuilder::stroke(px(2.));
                    line.move_to(map(first.from.x, first.from.y));
                    for segment in segments {
                        line.cubic_bezier_to(
                            map(segment.to.x, segment.to.y),
                            map(segment.c1.x, segment.c1.y),
                            map(segment.c2.x, segment.c2.y),
                        );
                    }
                    if let Ok(path) = line.build() {
                        window.paint_path(path, with_alpha_hsla(*color, series_alpha(*loading)));
                    }
                }
                if let Some(index) = hover_index {
                    let x = origin.x + px(index as f32 * step_x / VIEW_WIDTH * width) - px(0.5);
                    let top = origin.y + px(PLOT_TOP / VIEW_HEIGHT * height);
                    window.paint_quad(fill(
                        Bounds::new(
                            point(x, top),
                            size(px(1.), origin.y + bounds.size.height - top),
                        ),
                        hover_line,
                    ));
                }

                // Pointer tracking: the nearest period, or none once the pointer leaves.
                let state = on_hover.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Capture || period_count == 0 {
                        return;
                    }
                    let next = if bounds.contains(&event.position) && width > 0. {
                        let local_x =
                            f32::from(event.position.x - bounds.origin.x).clamp(0., width);
                        let fraction = local_x / width;
                        let index = (fraction * (period_count - 1) as f32).round() as usize;
                        ChartHover {
                            index: Some(index.min(period_count - 1)),
                            position: event.position,
                        }
                    } else {
                        ChartHover::default()
                    };
                    state.update(cx, |hover, cx| {
                        if *hover != next {
                            *hover = next;
                            cx.notify();
                        }
                    });
                });
                let _ = cx;
            },
        )
        .size_full();

        let tooltip = hover.index.and_then(|index| {
            let period = periods.get(index)?;
            let column = chart.columns.get(index)?;
            let partial = props
                .providers
                .iter()
                .any(|provider| props.loading_providers.contains(provider));
            let rows = props.providers.iter().map(|provider| {
                let presentation = presentation(*provider);
                let value = column
                    .bands
                    .iter()
                    .find(|band| band.provider == *provider)
                    .map_or(0., |band| band.value);
                let loading = props.loading_providers.contains(provider);
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.375))
                            .text_color(theme.muted_foreground)
                            .child(provider_instance_icon(
                                presentation.driver_kind,
                                presentation.label,
                                rems(0.75),
                                None,
                                cx,
                            ))
                            .child(presentation.label),
                    )
                    .child(
                        div()
                            .text_color(if loading {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            })
                            .child(format_value(metric, value)),
                    )
            });
            Some(
                deferred(
                    anchored()
                        .position(hover.position)
                        .offset(point(px(TOOLTIP_GAP), px(TOOLTIP_GAP)))
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            // `surface-glass pointer-events-none rounded-xl border
                            // border-border/50 px-2.5 py-2 text-xs shadow-lg`. No backdrop
                            // blur here, so the surface is opaque like the menus'.
                            div()
                                .occlude()
                                .min_w(rems(9.))
                                .px(rems(0.625))
                                .py_2()
                                .rounded(RADIUS_XL)
                                .border_1()
                                .border_color(with_alpha(theme.border, 0.5))
                                .bg(theme.background)
                                .shadow(tokens::shadow_lg(0.1))
                                .text_size(TEXT_XS.size)
                                .child(
                                    div()
                                        .mb_1()
                                        .text_color(theme.muted_foreground)
                                        .child(props.format_tooltip_period(period)),
                                )
                                .children(rows)
                                .child(
                                    div()
                                        .mt_1()
                                        .pt_1()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(
                                            div().text_color(theme.muted_foreground).child("Total"),
                                        )
                                        .child(
                                            div()
                                                .text_color(if partial {
                                                    theme.muted_foreground
                                                } else {
                                                    theme.foreground
                                                })
                                                .child(format_value(metric, column.total)),
                                        ),
                                ),
                        ),
                )
                .with_priority(20),
            )
        });

        let edge_label = |period: Option<&String>| -> String {
            period.map_or_else(String::new, |period| props.format_period(period))
        };
        let middle = periods.get(periods.len() / 2);
        let labels = div()
            .flex()
            .justify_between()
            .pl(rems(4.))
            .text_size(TEXT_3XS.size)
            .line_height(px(TICK_LABEL_HEIGHT))
            .text_color(theme.muted_foreground)
            .child(edge_label(periods.first()).to_uppercase())
            .child(edge_label(middle).to_uppercase())
            .child(edge_label(periods.last()).to_uppercase());

        div()
            .id(id)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div().flex().gap_2().child(axis).child(
                    div()
                        .relative()
                        .flex_1()
                        .h(px(PLOT_HEIGHT))
                        .child(plot_canvas)
                        .children(tooltip),
                ),
            )
            .child(labels)
    }
}

fn with_alpha_hsla(color: Hsla, alpha: f32) -> Hsla {
    Hsla {
        a: color.a * alpha,
        ..color
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_shared::usage_merge::ProviderSpend;

    fn column(codex: f64) -> DayColumn {
        DayColumn {
            total: codex,
            bands: vec![Band {
                provider: UsageProviderKind::Codex,
                value: codex,
            }],
        }
    }

    fn loading_codex() -> HashSet<UsageProviderKind> {
        HashSet::from([UsageProviderKind::Codex])
    }

    #[test]
    fn holds_unlabeled_placeholder_gridlines_while_loading_providers_have_nothing_to_show() {
        let scale = chart_scale(&[column(0.)], &loading_codex());
        assert!(!scale.labeled);
        assert!(scale.ticks.len() > 1);
    }

    #[test]
    fn scales_to_what_is_on_screen_loading_or_not() {
        let scale = chart_scale(&[column(40.)], &loading_codex());
        assert_eq!(scale.max, 40.);
        assert!(scale.labeled);
    }

    #[test]
    fn nice_scale_never_puts_the_peak_above_the_top_of_the_scale() {
        // Regression: an earlier version stopped at the last step below the peak,
        // so the tallest day was drawn past the plot and clipped.
        for peak in [
            1122.71,
            999.,
            1.,
            0.04,
            1_400_000_000.,
            37.5,
            5000.,
            100.001,
        ] {
            let (max, _) = nice_scale(peak, 4);
            assert!(max >= peak, "peak {peak}");
        }
    }

    #[test]
    fn nice_scale_starts_at_zero_and_ends_at_the_maximum() {
        let (max, ticks) = nice_scale(1122.71, 4);
        assert_eq!(ticks[0], 0.);
        assert!((ticks[ticks.len() - 1] - max).abs() < 1e-6);
    }

    #[test]
    fn nice_scale_uses_evenly_spaced_1_2_5_steps() {
        let (_, ticks) = nice_scale(1122.71, 4);
        let steps: Vec<f64> = ticks.windows(2).map(|pair| pair[1] - pair[0]).collect();
        for step in &steps {
            assert!((step - steps[0]).abs() < 1e-6);
        }
        let first = steps[0];
        let normalized = first / 10f64.powf(first.log10().floor());
        assert!([1., 2., 5., 10.].contains(&normalized.round()));
    }

    #[test]
    fn nice_scale_keeps_the_tick_count_near_the_requested_resolution() {
        let (_, ticks) = nice_scale(1122.71, 4);
        assert!((3..=7).contains(&ticks.len()));
    }

    #[test]
    fn nice_scale_degrades_to_a_single_zero_tick_with_no_data() {
        assert_eq!(nice_scale(0., 4), (0., vec![0.]));
    }

    fn spend(cost_usd: f64, total_tokens: u64) -> ProviderSpend {
        ProviderSpend {
            cost_usd,
            total_tokens,
        }
    }

    fn by_provider(entries: &[(UsageProviderKind, f64, u64)]) -> ByProvider {
        entries
            .iter()
            .map(|(provider, cost, tokens)| (*provider, spend(*cost, *tokens)))
            .collect()
    }

    fn days() -> Vec<String> {
        ["2026-08-01", "2026-08-02", "2026-08-03"]
            .map(str::to_owned)
            .to_vec()
    }

    fn fixture() -> (ByProvider, ByProvider) {
        (
            by_provider(&[
                (UsageProviderKind::Codex, 10., 100),
                (UsageProviderKind::Claude, 20., 200),
                (UsageProviderKind::Pi, 5., 50),
            ]),
            by_provider(&[(UsageProviderKind::Claude, 5., 50)]),
        )
    }

    fn lookup<'a>(first: &'a ByProvider, third: &'a ByProvider) -> HashMap<String, &'a ByProvider> {
        // 2026-08-02 is deliberately absent: a day with no activity.
        HashMap::from([
            ("2026-08-01".to_owned(), first),
            ("2026-08-03".to_owned(), third),
        ])
    }

    #[test]
    fn plots_each_day_on_its_own_and_reads_the_requested_metric() {
        let (first, third) = fixture();
        let by_day = lookup(&first, &third);
        let totals = |metric| {
            build_period_columns(&days(), &by_day, metric)
                .iter()
                .map(|column| column.total)
                .collect::<Vec<_>>()
        };
        assert_eq!(totals(UsageChartMetric::Cost), [35., 0., 5.]);
        assert_eq!(totals(UsageChartMetric::Tokens), [350., 0., 50.]);
    }

    #[test]
    fn keeps_band_values_absolute_rather_than_cumulative() {
        // Regression: the bands were once stack offsets, which drew Claude Code
        // permanently above Codex regardless of which provider spent more.
        let (first, third) = fixture();
        let columns =
            build_period_columns(&days(), &lookup(&first, &third), UsageChartMetric::Cost);
        let band = |provider, value| Band { provider, value };
        assert_eq!(
            columns[0].bands,
            vec![
                band(UsageProviderKind::Codex, 10.),
                band(UsageProviderKind::Claude, 20.),
                band(UsageProviderKind::Grok, 0.),
                band(UsageProviderKind::Cursor, 0.),
                band(UsageProviderKind::Opencode, 0.),
                band(UsageProviderKind::Antigravity, 0.),
                band(UsageProviderKind::Pi, 5.),
            ]
        );
    }

    #[test]
    fn reports_the_total_as_the_sum_of_its_bands() {
        let (first, third) = fixture();
        for column in build_period_columns(&days(), &lookup(&first, &third), UsageChartMetric::Cost)
        {
            let sum: f64 = column.bands.iter().map(|band| band.value).sum();
            assert!((column.total - sum).abs() < 1e-9);
        }
    }

    #[test]
    fn hourly_columns_zero_fill_inactive_hours() {
        let entry = by_provider(&[(UsageProviderKind::Codex, 4., 40)]);
        let by_hour = HashMap::from([("2026-08-11T09:37:00.000Z".to_owned(), &entry)]);
        let periods = [
            "2026-08-11T08:37:00.000Z",
            "2026-08-11T09:37:00.000Z",
            "2026-08-11T10:37:00.000Z",
        ]
        .map(str::to_owned);
        let totals: Vec<f64> = build_period_columns(&periods, &by_hour, UsageChartMetric::Cost)
            .iter()
            .map(|column| column.total)
            .collect();
        assert_eq!(totals, [0., 4., 0.]);
    }

    #[test]
    fn monotone_curve_never_overshoots_spiky_data() {
        let points: Vec<Point> = [0., 100., 0., 0., 50.]
            .iter()
            .enumerate()
            .map(|(index, y)| Point {
                x: index as f32 * 10.,
                y: *y,
            })
            .collect();
        for segment in smooth_curve(&points) {
            let (low, high) = (
                segment.from.y.min(segment.to.y),
                segment.from.y.max(segment.to.y),
            );
            assert!(segment.c1.y >= low - 1e-3 && segment.c1.y <= high + 1e-3);
            assert!(segment.c2.y >= low - 1e-3 && segment.c2.y <= high + 1e-3);
        }
        assert!(smooth_curve(&points[..1]).is_empty());
    }

    #[test]
    fn chart_paints_the_heavier_series_first() {
        let (first, third) = fixture();
        let chart = build_chart(
            &days(),
            &lookup(&first, &third),
            UsageChartMetric::Cost,
            &[UsageProviderKind::Codex, UsageProviderKind::Claude],
            &HashSet::new(),
        );
        assert_eq!(chart.paths[0].provider, UsageProviderKind::Claude);
        assert_eq!(chart.step_x, VIEW_WIDTH / 2.);
        assert_eq!(chart.scale.max, 20.);
    }
}
