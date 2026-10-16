//! Port of upstream/apps/web/src/components/usage/UsageShareBar.tsx

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};
use turin_shared::usage_format::format_percent;

use super::usage_providers::ChartColor;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::Tooltip;

#[derive(Clone, Debug, PartialEq)]
pub struct ShareSegment {
    pub label: &'static str,
    pub value: f64,
    pub color: ChartColor,
}

/// One part-to-whole bar with its legend, for cost or tokens split by type or
/// speed. Empty segments are left out, and nothing renders without a total.
#[derive(IntoElement)]
pub struct UsageShareBar {
    label: SharedString,
    segments: Vec<ShareSegment>,
    format: Rc<dyn Fn(f64) -> String>,
    aside: Option<AnyElement>,
}

impl UsageShareBar {
    pub fn new(
        label: impl Into<SharedString>,
        segments: Vec<ShareSegment>,
        format: impl Fn(f64) -> String + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            segments,
            format: Rc::new(format),
            aside: None,
        }
    }

    pub fn aside(mut self, aside: impl IntoElement) -> Self {
        self.aside = Some(aside.into_any_element());
        self
    }
}

impl RenderOnce for UsageShareBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let visible: Vec<ShareSegment> = self
            .segments
            .into_iter()
            .filter(|segment| segment.value > 0.)
            .collect();
        let total: f64 = visible.iter().map(|segment| segment.value).sum();
        if total <= 0. {
            return div().into_any_element();
        }
        let theme = cx.app_theme().clone();
        let format = self.format;
        let last = visible.len() - 1;
        let label = self.label;

        let bar = div()
            .flex()
            .h(px(8.))
            .gap(px(2.))
            .children(visible.iter().enumerate().map(|(index, segment)| {
                let color = segment.color.resolve(&theme);
                let tooltip = format!(
                    "{} · {} · {}",
                    segment.label,
                    format(segment.value),
                    format_percent(segment.value / total, 1)
                );
                // `h-full min-w-1 rounded-xs first:rounded-l-full last:rounded-r-full`
                // with `flex: <value> 1 0`.
                let mut cell = div()
                    .id(SharedString::from(format!("{label}:{}", segment.label)))
                    .h_full()
                    .min_w(px(4.))
                    .flex_basis(px(0.))
                    .flex_grow(segment.value as f32)
                    .flex_shrink(1.)
                    .bg(color)
                    .rounded(px(2.))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
                if index == 0 {
                    cell = cell.rounded_l_full();
                }
                if index == last {
                    cell = cell.rounded_r_full();
                }
                cell
            }));

        let legend = div()
            .flex()
            .flex_wrap()
            .gap_x_4()
            .gap_y_1()
            .text_size(TEXT_XS.size)
            .children(visible.iter().map(|segment| {
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.375))
                    .child(
                        div()
                            .size(px(8.))
                            .rounded(px(2.))
                            .bg(segment.color.resolve(&theme)),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(segment.label),
                    )
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(format(segment.value)),
                    )
            }));

        div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap(rems(0.625))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(label.clone()),
                    )
                    .children(self.aside),
            )
            .child(bar)
            .child(legend)
            .into_any_element()
    }
}
