//! Port of upstream/apps/web/src/components/DiffPanelShell.tsx (skeletons)
//! and `ui/skeleton.tsx`: the loading placeholders shown while a diff or
//! its next files load.
//!
//! Port of upstream/apps/web/src/components/diffs/DiffFileLoadingBoundary.tsx (placeholders).

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Styled as _,
    div, px, relative, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkeletonWidth {
    Short,
    Medium,
    Long,
}

/// `Skeleton`: `bg-muted-foreground/15 rounded-sm` (pill: `rounded-full`).
/// The pulse animation is not ported (GPUI style refinements do not animate).
fn skeleton(width: gpui_kit::Length, height: Pixels, pill: bool, cx: &App) -> gpui_kit::Div {
    let theme = cx.app_theme();
    div()
        .flex_shrink_0()
        .w(width)
        .h(height)
        .bg(with_alpha(theme.muted_foreground, 0.15))
        .when_pill(pill)
}

trait PillExt {
    fn when_pill(self, pill: bool) -> Self;
}

impl PillExt for gpui_kit::Div {
    fn when_pill(self, pill: bool) -> Self {
        if pill {
            self.rounded_full()
        } else {
            self.rounded(px(6.))
        }
    }
}

/// `DiffFileHeaderSkeleton`.
pub fn diff_file_header_skeleton(title: SkeletonWidth, cx: &App) -> AnyElement {
    let (fraction, max_width) = match title {
        SkeletonWidth::Short => (0.4, rems(13.)),
        SkeletonWidth::Medium => (0.5, rems(16.)),
        SkeletonWidth::Long => (0.6, rems(18.)),
    };
    div()
        .flex()
        .h(rems(2.))
        .items_center()
        .gap_2()
        .pl_2()
        .pr(px(12.))
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .size(rems(1.25))
                .items_center()
                .justify_center()
                .child(skeleton(
                    rems(0.625).into(),
                    rems(0.625).to_pixels(px(16.)),
                    false,
                    cx,
                )),
        )
        .child(skeleton(
            rems(1.25).into(),
            rems(1.25).to_pixels(px(16.)),
            false,
            cx,
        ))
        .child(
            div()
                .min_w_0()
                .w(relative(fraction))
                .max_w(max_width)
                .child(skeleton(relative(1.).into(), px(12.), true, cx)),
        )
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .ml_auto()
                .child(skeleton(rems(1.25).into(), px(12.), true, cx))
                .child(skeleton(rems(1.25).into(), px(12.), true, cx)),
        )
        .into_any_element()
}

fn code_line_skeleton(width: SkeletonWidth, cx: &App) -> AnyElement {
    let fraction = match width {
        SkeletonWidth::Short => 0.6,
        SkeletonWidth::Medium => 2. / 3.,
        SkeletonWidth::Long => 0.8,
    };
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(skeleton(rems(1.25).into(), px(10.), true, cx))
        .child(skeleton(relative(fraction).into(), px(10.), true, cx))
        .into_any_element()
}

/// `DiffPanelLoadingState`.
pub fn diff_panel_loading_state(cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    div()
        .id("diff-panel-loading")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .bg(theme.background)
        .child(diff_file_header_skeleton(SkeletonWidth::Medium, cx))
        .child(
            div()
                .flex()
                .h_6()
                .items_center()
                .gap_2()
                .pl_2()
                .pr(px(12.))
                .child(div().h(px(1.)).flex_1().bg(with_alpha(theme.border, 0.4)))
                .child(skeleton(rems(6.).into(), px(10.), true, cx))
                .child(div().h(px(1.)).flex_1().bg(with_alpha(theme.border, 0.4))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .py_2()
                .child(code_line_skeleton(SkeletonWidth::Medium, cx))
                .child(code_line_skeleton(SkeletonWidth::Long, cx))
                .child(code_line_skeleton(SkeletonWidth::Short, cx)),
        )
        .child(diff_file_header_skeleton(SkeletonWidth::Short, cx))
        .child(diff_file_header_skeleton(SkeletonWidth::Long, cx))
        .into_any_element()
}

/// `DiffFileLoadingBoundary`'s placeholders: up to four skeleton headers.
pub fn diff_file_loading_boundary(count: usize, cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    div()
        .flex()
        .flex_col()
        .children((0..count.min(4)).map(|_| {
            div()
                .border_b_1()
                .border_color(with_alpha(theme.border, 0.4))
                .child(diff_file_header_skeleton(SkeletonWidth::Medium, cx))
        }))
        .into_any_element()
}
