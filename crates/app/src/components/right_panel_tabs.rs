//! Port of upstream/apps/web/src/components/RightPanelTabs.tsx (inline mode,
//! layout only) and the panel shell from preview/PreviewPanelShell.tsx.
//!
//! `bg-background border-l` column with the 52px tab bar (`pl-2 pr-28`, the
//! right 112px reserved for the fixed layout controls) and the surface tabs.
//! The surfaces kept by Turin's cut list are the diff and pull requests; both
//! always have a tab, so the add-surface button is dropped (PORTING.md).
//! The diff surface reads the routed thread's checkpoints and workspace git
//! ([`LiveDiffSource`]); the fixture source backs the mock UI. The resize
//! handle is drawn by `AppSidebarLayout`, which owns the width.

use std::sync::Arc;

use gpui_kit::{
    App, CursorStyle, Entity, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, RenderOnce, StatefulInteractiveElement as _, Styled as _, Window, div,
    rems,
};
use turin_contracts::ThreadId;

use super::drag_region::{DragRegion as _, no_drag};
use crate::app_runtime::write::bus::CommandBus;
use crate::components::chat::timeline::fixtures::startup_thread;
use crate::diff::live_source::LiveDiffSource;
use crate::diff::mock_source::MockDiffSource;
use crate::diff::panel::DiffPanel;
use crate::diff::view_model::DiffSource;
use crate::state::store::ClientStore;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_XS, WORKSPACE_TOPBAR_HEIGHT};
use crate::ui::{Icon, IconName};

/// A surface tab: `h-6 max-w-36 rounded-md pl-1.5 pr-2 text-xs gap-0.5`,
/// icons per `SurfaceIcon` (diff → FileDiff, pull-requests → Link2),
/// active `bg-accent text-foreground`.
fn surface_tab(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.app_theme().clone();
    let tab = div()
        .id(id)
        .group(id)
        .on_mouse_down(MouseButton::Left, no_drag)
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
        // PanelTabCloseButton slot: a `size-4` box around the `size-3` surface icon.
        // The icon turns foreground with the tab (`group-hover/tab`); upstream
        // swaps it for an X there, which is cut with the close action.
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .size(rems(1.))
                .items_center()
                .justify_center()
                .child(
                    Icon::new(icon)
                        .size(rems(0.75))
                        .color(if active {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .group_hover_color(id, theme.foreground),
                ),
        )
        .child(div().min_w_0().truncate().child(label));
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

/// The routed thread the diff surface reads.
#[derive(Clone)]
pub struct LiveDiffTarget {
    pub client: Entity<ClientStore>,
    pub bus: CommandBus,
    pub thread: Option<ThreadId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    Diff,
    PullRequests,
}

#[derive(IntoElement)]
pub struct RightPanelTabs {
    pub live: Option<LiveDiffTarget>,
}

impl RightPanelTabs {
    pub fn new(live: Option<LiveDiffTarget>) -> Self {
        Self { live }
    }
}

impl RenderOnce for RightPanelTabs {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let surface = window.use_keyed_state("right-panel-surface", cx, |_, _| Surface::Diff);
        let active = *surface.read(cx);
        let diff_panel = match self.live {
            Some(LiveDiffTarget {
                client,
                bus,
                thread: Some(thread),
            }) => {
                let source = window.use_keyed_state(
                    format!("live-diff-source:{}", thread.as_str()),
                    cx,
                    |_, cx| Arc::new(LiveDiffSource::new(thread.clone(), bus.clone(), cx)),
                );
                let source: Arc<LiveDiffSource> = source.read(cx).clone();
                let state = client.read(cx).state().clone();
                if let Some(projection) = state
                    .thread(&thread)
                    .and_then(|entry| entry.state.data.as_ref())
                {
                    let workspace_root = state
                        .shell_view
                        .as_ref()
                        .and_then(|view| view.project(&projection.thread.project_id))
                        .map(|project| project.workspace_root.clone());
                    source.sync(projection, workspace_root);
                }
                Some(window.use_keyed_state(
                    format!("diff-panel:{}", thread.as_str()),
                    cx,
                    |window, cx| DiffPanel::new(source as Arc<dyn DiffSource>, window, cx),
                ))
            }
            // A live window with no thread routed has nothing to diff.
            Some(_) => None,
            None => Some(window.use_keyed_state("diff-panel", cx, |window, cx| {
                let source = Arc::new(MockDiffSource::new(startup_thread().checkpoints));
                DiffPanel::new(source, window, cx)
            })),
        };
        let select = |target: Surface| {
            let surface = surface.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                surface.update(cx, |surface, cx| {
                    if *surface != target {
                        *surface = target;
                        cx.notify();
                    }
                });
            }
        };
        let body = match (active, diff_panel) {
            (Surface::Diff, Some(panel)) => div().flex().flex_col().flex_1().min_h_0().child(panel),
            (Surface::Diff, None) => empty_state("Open a thread to see its changes.", cx),
            (Surface::PullRequests, _) => empty_state(
                "A thread's pull requests are listed on the Pull requests page.",
                cx,
            ),
        };
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
                    .child(
                        surface_tab(
                            "right-panel-tab-diff",
                            IconName::FileDiff,
                            "Diff",
                            active == Surface::Diff,
                            cx,
                        )
                        .on_click(select(Surface::Diff)),
                    )
                    .child(
                        surface_tab(
                            "right-panel-tab-prs",
                            IconName::Link2,
                            "Pull requests",
                            active == Surface::PullRequests,
                            cx,
                        )
                        .on_click(select(Surface::PullRequests)),
                    )
                    .drag_region(window, cx),
            )
            // The surface below the tab bar, filling the rest of the column.
            .child(body)
    }
}

fn empty_state(message: &'static str, cx: &App) -> gpui_kit::Div {
    let theme = cx.app_theme().clone();
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .px_6()
        .text_size(TEXT_XS.size)
        .text_color(theme.muted_foreground)
        .child(message)
}
