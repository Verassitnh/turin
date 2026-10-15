//! Port of upstream/apps/web/src/components/RightPanelTabs.tsx (inline mode,
//! layout only) and the panel shell from preview/PreviewPanelShell.tsx.
//!
//! `bg-background border-l` column with the 52px tab bar (`pl-2 pr-28`, the
//! right 112px reserved for the fixed layout controls), surface tabs and
//! the add-surface button. The only surface kept by Turin's cut list in this
//! slot is the diff (and, later, pull requests); the panel body is a
//! placeholder until the diff task. The resize handle is drawn by
//! `AppSidebarLayout`, which owns the width.

use gpui_kit::{
    App, CursorStyle, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div, rems,
};

use super::drag_region::DragRegion as _;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS, WORKSPACE_TOPBAR_HEIGHT};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// A surface tab: `h-6 max-w-36 rounded-md pl-1.5 pr-2 text-xs gap-0.5`,
/// active `bg-accent text-foreground`.
fn surface_tab(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.app_theme().clone();
    let tab = div()
        .id(id)
        .flex()
        .h_6()
        .max_w(rems(9.))
        .flex_shrink_0()
        .items_center()
        .gap(rems(0.125))
        .pl(rems(0.375))
        .pr_2()
        .rounded(RADIUS_MD)
        .text_size(TEXT_XS.size)
        .cursor(CursorStyle::PointingHand)
        .child(Icon::new(icon).size(rems(0.875)).color(if active {
            theme.foreground
        } else {
            theme.muted_foreground
        }))
        .child(div().pl_1().truncate().child(label));
    if active {
        tab.bg(theme.accent).text_color(theme.foreground)
    } else {
        tab.text_color(theme.muted_foreground).hover(|style| {
            style
                .bg(with_alpha(theme.accent, 0.6))
                .text_color(theme.foreground)
        })
    }
}

#[derive(IntoElement)]
pub struct RightPanelTabs;

impl RenderOnce for RightPanelTabs {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(theme.background)
            .child(
                div()
                    .id("right-panel-tab-bar")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .h(WORKSPACE_TOPBAR_HEIGHT)
                    .pl_2()
                    .pr(rems(7.))
                    .child(surface_tab(
                        "right-panel-tab-diff",
                        IconName::FileDiff,
                        "Diff",
                        true,
                        cx,
                    ))
                    .child(surface_tab(
                        "right-panel-tab-prs",
                        IconName::GitPullRequest,
                        "Pull requests",
                        false,
                        cx,
                    ))
                    .child(
                        Button::new("right-panel-add-surface")
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Plus)
                            .tooltip(Tooltip::new("New tab").shortcut("⌘T")),
                    )
                    .drag_region(window, cx),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("No changes yet")
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.6))
                            .child("Turn diffs will appear here."),
                    ),
            )
    }
}
