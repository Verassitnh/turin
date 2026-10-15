//! Port of upstream/apps/web/src/components/Sidebar.tsx (layout only).
//!
//! The static shell: chrome header, search/actions row, a placeholder thread
//! list using the card (active/pinned/working) and slim (settled) row
//! layouts, a collapsible section header and the utility footer. Row data,
//! search, snooze, drag, hover cards and status logic arrive with the
//! sidebar task; the row geometry here is the upstream geometry they plug
//! into.

pub mod runtime;
pub mod sidebar_chrome;
pub mod sidebar_thread_header;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, CursorStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div, px, rems,
};

use self::sidebar_chrome::{SidebarChromeFooter, SidebarChromeHeader};
use self::sidebar_thread_header::SidebarThreadHeader;
use super::project_monogram::ProjectMonogram;
use crate::theme::color::{hex, with_alpha};
use crate::theme::tokens::{
    RADIUS_MD, SIDEBAR_CONTENT_INSET, SIDEBAR_ROW_CONTENT_INSET, TEXT_SM, TEXT_XS,
};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName, Separator, scroll_area_hidden};

/// Sidebar thread status labels (`ThreadStatusIndicators`), subset used by
/// the placeholder rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRowStatus {
    Working,
    Done,
    Failed,
}

impl ThreadRowStatus {
    fn presentation(self, theme: &AppTheme) -> (IconName, &'static str, Hsla) {
        match self {
            Self::Working => (IconName::CircleDashed, "Working", theme.info),
            Self::Done => (IconName::CircleCheck, "Done", theme.success),
            Self::Failed => (IconName::CircleAlert, "Failed", theme.error),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ThreadRowData {
    pub id: &'static str,
    pub project: &'static str,
    pub monogram: &'static str,
    pub monogram_color: u32,
    pub title: &'static str,
    pub time: &'static str,
    pub status: Option<ThreadRowStatus>,
    pub branch: Option<&'static str>,
    pub diff: Option<(u32, u32)>,
}

/// Placeholder rows so the shell reads like the real app.
pub fn placeholder_threads() -> (Vec<ThreadRowData>, Vec<ThreadRowData>) {
    let active = vec![
        ThreadRowData {
            id: "t-design-system",
            project: "turin",
            monogram: "T",
            monogram_color: 0x155dfc,
            title: "Port design system and app shell",
            time: "now",
            status: Some(ThreadRowStatus::Working),
            branch: Some("task/T03-design-system"),
            diff: Some((1240, 18)),
        },
        ThreadRowData {
            id: "t-contracts",
            project: "turin",
            monogram: "T",
            monogram_color: 0x155dfc,
            title: "Port orchestration contracts",
            time: "12m",
            status: Some(ThreadRowStatus::Done),
            branch: Some("task/T01-contracts"),
            diff: Some((3812, 4)),
        },
        ThreadRowData {
            id: "t-review",
            project: "t3code",
            monogram: "T3",
            monogram_color: 0x009966,
            title: "Review composer keyboard handling",
            time: "2h",
            status: None,
            branch: Some("main"),
            diff: None,
        },
    ];
    let settled = vec![
        ThreadRowData {
            id: "t-settled-1",
            project: "turin",
            monogram: "T",
            monogram_color: 0x155dfc,
            title: "Scaffold workspace",
            time: "1d",
            status: None,
            branch: None,
            diff: None,
        },
        ThreadRowData {
            id: "t-settled-2",
            project: "t3code",
            monogram: "T3",
            monogram_color: 0x009966,
            title: "Investigate sidebar drag jank",
            time: "3d",
            status: None,
            branch: None,
            diff: None,
        },
    ];
    (active, settled)
}

/// Card variant row (`Sidebar.tsx` thread row, 78px content box).
#[derive(IntoElement)]
pub struct SidebarThreadRow {
    data: ThreadRowData,
    active: bool,
}

impl SidebarThreadRow {
    pub fn new(data: ThreadRowData, active: bool) -> Self {
        Self { data, active }
    }
}

impl RenderOnce for SidebarThreadRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let data = self.data;
        let status_slot = match data.status {
            Some(status) => {
                let (icon, label, color) = status.presentation(theme);
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child(Icon::new(icon).color(color))
                    .child(label)
            }
            None => div()
                .text_color(theme.secondary_label)
                .child(SharedString::from(data.time)),
        };

        let line3 = (data.branch.is_some() || data.diff.is_some()).then(|| {
            div()
                .mt(rems(0.125))
                .flex()
                .min_w_0()
                .items_center()
                .gap(rems(0.375))
                .text_size(TEXT_XS.size)
                .line_height(TEXT_XS.line_height)
                .text_color(theme.secondary_label)
                .when_some(data.branch, |this, branch| {
                    this.child(
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap_1()
                            .text_color(with_alpha(theme.muted_foreground, 0.4))
                            .child(
                                Icon::new(IconName::GitBranch)
                                    .size(rems(0.75))
                                    .color(with_alpha(theme.muted_foreground, 0.4).into()),
                            )
                            .child(div().min_w_0().truncate().child(branch)),
                    )
                })
                .when_some(data.diff, |this, (additions, deletions)| {
                    this.child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .gap_1()
                            .font_family(theme.fonts.mono.clone())
                            .child(
                                div()
                                    .text_color(theme.diff_addition_foreground)
                                    .child(format!("+{additions}")),
                            )
                            .child(
                                div()
                                    .text_color(theme.diff_deletion_foreground)
                                    .child(format!("\u{2212}{deletions}")),
                            ),
                    )
                })
        });

        div().py(px(2.)).child(
            div()
                .id(data.id)
                .relative()
                .w_full()
                .overflow_hidden()
                .rounded(RADIUS_MD)
                .cursor(CursorStyle::PointingHand)
                .map(|this| {
                    if self.active {
                        this.bg(theme.sidebar_row_active)
                    } else {
                        this.hover(|style| style.bg(theme.sidebar_row_hover))
                    }
                })
                .child(
                    div()
                        .h(rems(4.875))
                        .px(SIDEBAR_ROW_CONTENT_INSET)
                        .py(SIDEBAR_CONTENT_INSET)
                        .child(
                            div()
                                .flex()
                                .h_5()
                                .min_w_0()
                                .items_center()
                                .gap(rems(0.375))
                                .child(ProjectMonogram::new(
                                    data.monogram,
                                    hex(data.monogram_color).into(),
                                ))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_size(TEXT_XS.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.secondary_label)
                                        .child(data.project),
                                )
                                .child(
                                    div()
                                        .ml_auto()
                                        .flex()
                                        .h_5()
                                        .min_w_8()
                                        .flex_shrink_0()
                                        .items_center()
                                        .justify_end()
                                        .text_size(TEXT_XS.size)
                                        .child(status_slot),
                                ),
                        )
                        .child(
                            div().mt_1().flex().min_w_0().child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_size(TEXT_SM.size)
                                    .line_height(TEXT_SM.line_height)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(with_alpha(theme.foreground, 0.9))
                                    .child(data.title),
                            ),
                        )
                        .children(line3),
                ),
        )
    }
}

/// Slim variant row (settled/snoozed sections): 36px, favicon dimmed at rest.
#[derive(IntoElement)]
pub struct SidebarSlimThreadRow {
    data: ThreadRowData,
}

impl SidebarSlimThreadRow {
    pub fn new(data: ThreadRowData) -> Self {
        Self { data }
    }
}

impl RenderOnce for SidebarSlimThreadRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let data = self.data;
        div()
            .id(data.id)
            .flex()
            .h_9()
            .items_center()
            .gap(rems(0.625))
            .px(rems(0.625))
            .rounded(RADIUS_MD)
            .cursor(CursorStyle::PointingHand)
            .hover(|style| style.bg(theme.sidebar_row_hover))
            .child(
                div()
                    .opacity(0.4)
                    .child(ProjectMonogram::new(data.monogram, theme.muted_foreground)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(TEXT_SM.size)
                    .text_color(with_alpha(theme.secondary_label, 0.7))
                    .child(data.title),
            )
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .h_6()
                    .min_w_8()
                    .items_center()
                    .justify_end()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.secondary_label)
                    .child(data.time),
            )
    }
}

/// `CollapsibleSectionHeader` (ui/collapsible-section-header.tsx), muted tone.
#[derive(IntoElement)]
pub struct SidebarSectionHeader {
    label: SharedString,
}

impl SidebarSectionHeader {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl RenderOnce for SidebarSectionHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let muted = with_alpha(theme.sidebar_muted_foreground, 0.6).into();
        div().mx(px(2.)).h_8().child(
            div()
                .id(SharedString::from(format!("section-{}", self.label)))
                .flex()
                .h_8()
                .w_full()
                .items_center()
                .gap_2()
                .px_2()
                .rounded(RADIUS_MD)
                .cursor(CursorStyle::PointingHand)
                .text_size(TEXT_XS.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted)
                .child(self.label)
                .child(div().flex_1().min_w_2().child(
                    Separator::horizontal().color(with_alpha(theme.sidebar_border, 0.6).into()),
                ))
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(rems(0.75))
                        .color(muted),
                ),
        )
    }
}

/// The sidebar column's contents (inside the `bg-sidebar` container).
#[derive(IntoElement)]
pub struct Sidebar;

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (active, settled) = placeholder_threads();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(SidebarChromeHeader)
            .child(
                div()
                    .p(SIDEBAR_CONTENT_INSET)
                    .child(SidebarThreadHeader { has_projects: true }),
            )
            .child(
                div().flex_1().min_h_0().child(
                    scroll_area_hidden("sidebar-content").child(
                        div()
                            .flex()
                            .flex_col()
                            .min_h_full()
                            .px(SIDEBAR_CONTENT_INSET)
                            .pb(SIDEBAR_CONTENT_INSET)
                            .children(
                                active
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, row)| SidebarThreadRow::new(row, index == 0)),
                            )
                            .child(
                                div()
                                    .mt_auto()
                                    .flex()
                                    .flex_col()
                                    .child(SidebarSectionHeader::new("Settled"))
                                    .children(settled.into_iter().map(SidebarSlimThreadRow::new)),
                            ),
                    ),
                ),
            )
            .child(SidebarChromeFooter)
    }
}
