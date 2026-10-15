//! Port of upstream/apps/web/src/components/chat/WorkLog.tsx: the compact
//! row geometry every work-log entry shares.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, relative, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{LEADING_RELAXED, RADIUS_MD, TEXT_SM};

/// `WorkLogBlock` layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkLogLayout {
    Standalone,
    GroupHeader,
    GroupContent,
}

/// `WorkLogBlock`: groups may span virtualized timeline items, so each part
/// owns its trailing space.
pub fn work_log_block(layout: WorkLogLayout, continues: bool, child: impl IntoElement) -> Div {
    let bottom = if continues || layout == WorkLogLayout::GroupHeader {
        rems(0.)
    } else if layout == WorkLogLayout::GroupContent {
        rems(0.25)
    } else {
        rems(0.5)
    };
    div().w_full().min_w_0().pb(bottom).child(child)
}

/// Group name for `group/timeline-row` hover reveals.
pub fn timeline_row_group(id: &ElementId) -> SharedString {
    format!("timeline-row-{id:?}").into()
}

/// `WorkLogLine`: icon column, label, trailing slot.
#[derive(IntoElement)]
pub struct WorkLogLine {
    pub icon: Option<AnyElement>,
    pub label: AnyElement,
    pub trailing: Vec<AnyElement>,
    pub wrap_label: bool,
}

impl RenderOnce for WorkLogLine {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        div()
            .flex()
            .min_h(rems(1.5))
            .min_w_0()
            .gap(rems(0.375))
            .text_size(TEXT_SM.size)
            .line_height(relative(LEADING_RELAXED))
            .when(self.wrap_label, |this| this.items_start())
            .when(!self.wrap_label, |this| this.items_center())
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap(rems(0.375))
                    .when_some(self.icon, |this, icon| {
                        this.child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .size(rems(1.5))
                                .items_center()
                                .justify_center()
                                .child(icon),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_color(theme.secondary_label)
                            .when(!self.wrap_label, |this| this.truncate())
                            .child(self.label),
                    ),
            )
            .children(self.trailing)
    }
}

/// `useRowClassName`: `group/timeline-row relative w-full rounded-md px-0.5`,
/// `py-0` inside an expanded group, `py-0.5` standalone.
pub fn work_log_row_frame(id: impl Into<ElementId>, grouped: bool) -> Stateful<Div> {
    let id = id.into();
    let group = timeline_row_group(&id);
    div()
        .id(id)
        .group(group)
        .relative()
        .w_full()
        .min_w_0()
        .rounded(RADIUS_MD)
        .px(rems(0.125))
        .py(if grouped { rems(0.) } else { rems(0.125) })
}

/// `WorkLogButton` / `WorkLogRow` with a click handler: the whole row toggles.
pub fn work_log_button(
    id: impl Into<ElementId>,
    grouped: bool,
    line: WorkLogLine,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    work_log_row_frame(id, grouped)
        .cursor_pointer()
        .child(line)
        .on_click(on_click)
}

/// `WorkLogDetails` kinds. `Media` hosts viewed images (not ported yet).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkLogDetailsKind {
    Text,
    Panel,
    Media,
}

/// `WorkLogDetails`: the disclosure body under a row.
pub fn work_log_details(id: impl Into<ElementId>, kind: WorkLogDetailsKind) -> Stateful<Div> {
    let base = div().id(id).cursor_default();
    match kind {
        // `ms-7 flex max-h-96 flex-col gap-3 overflow-auto px-0.5 py-1 select-text`
        WorkLogDetailsKind::Text => base
            .ml(rems(1.75))
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .max_h(rems(24.))
            .overflow_y_scroll()
            .px(rems(0.125))
            .py(rems(0.25)),
        // `mt-0.5 mb-1.5 pb-2`
        WorkLogDetailsKind::Panel => base.mt(rems(0.125)).mb(rems(0.375)).pb(rems(0.5)),
        WorkLogDetailsKind::Media => base.mt(rems(0.25)),
    }
}
