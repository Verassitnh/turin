//! Port of upstream/apps/web/src/components/AppSidebarLayout.tsx, built on
//! the ui/sidebar.tsx primitives (`SidebarProvider`, `Sidebar
//! collapsible="offcanvas"`, `SidebarRail`, `SidebarTrigger`, `SidebarInset`)
//! and the chat-column + inline right panel row from ChatView.tsx. It owns
//! the sidebar view, forwards its keyboard commands, and routes its
//! selection into the main view.
//!
//! The window root: a flush (non-inset) sidebar with a right border, the
//! main column, the inline right panel, and the titlebar controls that
//! upstream pins with `position: fixed` (sidebar toggle at
//! `--workspace-controls-left`, layout toggles at the right).
//!
//! Widths are not persisted yet (upstream: localStorage keys
//! `chat_thread_sidebar_width`, `t3code:preview-panel-width`); the settings
//! task owns persistence. The width animation (`--panel-animation-duration`)
//! defaults to 0 upstream and is not ported.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, CursorStyle, FocusHandle, InteractiveElement as _, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, Styled as _,
    Subscription, Window, actions, div, px, rems,
};

use super::chat::panel_layout_controls::PanelLayoutControls;
use super::chat_view::ChatView;
use super::right_panel_tabs::RightPanelTabs;
use super::sidebar::mock_source::mock_sidebar;
use super::sidebar::view_model::SidebarStore;
use super::sidebar::{self as thread_sidebar, Sidebar, SidebarEvent, SidebarRoute};
use super::thread_sidebar_width::{
    THREAD_SIDEBAR_DEFAULT_WIDTH, THREAD_SIDEBAR_MIN_WIDTH, clamp_thread_sidebar_width,
    resolve_initial_thread_sidebar_width, resolve_thread_sidebar_maximum_width,
};
use super::workspace_titlebar::workspace_controls_left;
use crate::hooks::use_preview_panel_inline_size::{
    PREVIEW_PANEL_DEFAULT_WIDTH, clamp_preview_panel_width, get_preview_panel_max_width,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{WORKSPACE_CONTROLS_INSET, WORKSPACE_TOPBAR_HEIGHT};
use crate::theme::{self, ActiveAppTheme};
use crate::ui::Tooltip;
use crate::ui::sidebar::{
    Sidebar as SidebarColumn, SidebarInset, SidebarProvider, SidebarRail, SidebarTrigger,
};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Entity, SharedString};

actions!(
    turin,
    [
        /// `sidebar.toggle` (mod+b).
        ToggleSidebar,
        /// `rightPanel.toggle` (mod+alt+b).
        ToggleRightPanel,
    ]
);

const KEY_CONTEXT: &str = "AppShell";

/// Upstream default keybindings (packages/shared/src/keybindings.ts) for the
/// shell's actions.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleSidebar, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleRightPanel, Some(KEY_CONTEXT)),
    ]);
    thread_sidebar::bind_keys(cx, KEY_CONTEXT);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeTarget {
    Sidebar,
    RightPanel,
}

/// Port of the drag bookkeeping in upstream hooks/useResizeDrag.ts: the
/// width follows `start_width ± (x - start_x)`, so pressing off-centre in
/// the hit area never jumps the edge.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ResizeDrag {
    target: ResizeTarget,
    start_x: f32,
    start_width: f32,
}

/// useResizeDrag's 2px "moved" threshold.
const RESIZE_CLICK_SLOP: f32 = 2.;

pub struct AppSidebarLayout {
    focus_handle: FocusHandle,
    sidebar_open: bool,
    sidebar_width: f32,
    /// Closed by default (`EMPTY_THREAD_STATE.isOpen: false`).
    right_panel_open: bool,
    right_panel_maximized: bool,
    right_panel_width: f32,
    resizing: Option<ResizeDrag>,
    /// The last drag moved past [`RESIZE_CLICK_SLOP`]; the rail's
    /// double-click reset ignores it, as useResizeDrag suppresses the click.
    last_drag_moved: bool,
    sidebar: Entity<Sidebar>,
    store: Entity<SidebarStore>,
    _appearance: Subscription,
    _sidebar_events: Subscription,
}

impl AppSidebarLayout {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            theme::follow_system_appearance(window, cx);
        });
        theme::follow_system_appearance(window, cx);
        let viewport_width = f32::from(window.viewport_size().width);
        // T14 replaces the mock store/backend with the client view model.
        let (store, backend) = mock_sidebar(cx);
        let sidebar = cx.new(|cx| Sidebar::new(store.clone(), backend, window, cx));
        let sidebar_events = cx.subscribe_in(&sidebar, window, Self::on_sidebar_event);
        sidebar.update(cx, |sidebar, cx| sidebar.apply_dev_overrides(window, cx));
        Self {
            focus_handle,
            sidebar_open: true,
            sidebar_width: resolve_initial_thread_sidebar_width(None, viewport_width),
            right_panel_open: false,
            right_panel_maximized: false,
            right_panel_width: PREVIEW_PANEL_DEFAULT_WIDTH,
            resizing: None,
            last_drag_moved: false,
            sidebar,
            store,
            _appearance: appearance,
            _sidebar_events: sidebar_events,
        }
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<Sidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::Navigate(_) => cx.notify(),
            SidebarEvent::OpenPullRequests(_) => {
                self.right_panel_open = true;
                cx.notify();
            }
            // Screens owned by later tasks (composer, settings, PRs, usage).
            other => {
                let label = match other {
                    SidebarEvent::NewThread { .. } => "New thread",
                    SidebarEvent::AddProject => "Add project",
                    SidebarEvent::OpenProjectSettings(_) => "Project settings",
                    SidebarEvent::OpenSettings => "Settings",
                    SidebarEvent::OpenPullRequestsPage => "Pull requests",
                    SidebarEvent::OpenUsage => "Usage",
                    _ => "",
                };
                window.push_notification(
                    Notification::info(format!("{label} is not available yet.")),
                    cx,
                );
            }
        }
    }

    /// `(project, title)` of the routed thread for the chat header.
    fn routed_thread(&self, cx: &App) -> Option<(SharedString, SharedString)> {
        let Some(SidebarRoute::Thread(key)) = self.sidebar.read(cx).route() else {
            return None;
        };
        let snapshot = self.store.read(cx).snapshot();
        let thread = snapshot
            .threads
            .iter()
            .find(|thread| &thread.key() == key)?;
        let project = snapshot
            .project_group_for(&thread.project_key())
            .map(|group| group.display_name.clone())
            .unwrap_or_default();
        Some((project.into(), thread.title.clone().into()))
    }

    /// Opens the right panel; used by the screenshot harness (`TURIN_SHELL_RIGHT_PANEL`).
    pub fn set_right_panel_open(&mut self, open: bool) {
        self.right_panel_open = open;
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    fn toggle_right_panel(&mut self, _: &ToggleRightPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.right_panel_open = !self.right_panel_open;
        if !self.right_panel_open {
            self.right_panel_maximized = false;
        }
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.resizing else {
            return;
        };
        if !event.dragging() {
            self.resizing = None;
            cx.notify();
            return;
        }
        let delta = f32::from(event.position.x) - drag.start_x;
        if delta.abs() > RESIZE_CLICK_SLOP {
            self.last_drag_moved = true;
        }
        let viewport_width = f32::from(window.viewport_size().width);
        match drag.target {
            ResizeTarget::Sidebar => {
                let max =
                    resolve_thread_sidebar_maximum_width(viewport_width, THREAD_SIDEBAR_MIN_WIDTH);
                self.sidebar_width = clamp_thread_sidebar_width(
                    drag.start_width + delta,
                    THREAD_SIDEBAR_MIN_WIDTH,
                    max,
                );
            }
            ResizeTarget::RightPanel => {
                // The panel's edge is on its left: dragging left widens it.
                let max = get_preview_panel_max_width(
                    viewport_width,
                    Some(self.main_width(viewport_width)),
                );
                self.right_panel_width = clamp_preview_panel_width(drag.start_width - delta, max);
            }
        }
        cx.notify();
    }

    fn effective_sidebar_width(&self, viewport_width: f32) -> f32 {
        let max = resolve_thread_sidebar_maximum_width(viewport_width, THREAD_SIDEBAR_MIN_WIDTH);
        clamp_thread_sidebar_width(self.sidebar_width, THREAD_SIDEBAR_MIN_WIDTH, max)
    }

    /// Width of the row holding the chat column and the right panel.
    fn main_width(&self, viewport_width: f32) -> f32 {
        if self.sidebar_open {
            viewport_width - self.effective_sidebar_width(viewport_width)
        } else {
            viewport_width
        }
    }
}

impl Render for AppSidebarLayout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let viewport_width = f32::from(window.viewport_size().width);
        let sidebar_width = self.effective_sidebar_width(viewport_width);
        let right_panel_width = clamp_preview_panel_width(
            self.right_panel_width,
            get_preview_panel_max_width(viewport_width, Some(self.main_width(viewport_width))),
        );
        let sidebar_open = self.sidebar_open;
        let right_panel_open = self.right_panel_open;
        let maximized = right_panel_open && self.right_panel_maximized;
        let resizing = self.resizing.map(|drag| drag.target);
        let routed_thread = self.routed_thread(cx);

        let sidebar_rail = SidebarRail::new(px(sidebar_width))
            .active(resizing == Some(ResizeTarget::Sidebar))
            .on_mouse_down({
                let entity = cx.entity().downgrade();
                move |event, _, cx| {
                    cx.stop_propagation();
                    entity
                        .update(cx, |this, cx| {
                            this.last_drag_moved = false;
                            this.resizing = Some(ResizeDrag {
                                target: ResizeTarget::Sidebar,
                                start_x: f32::from(event.position.x),
                                start_width: sidebar_width,
                            });
                            cx.notify();
                        })
                        .ok();
                }
            })
            .on_click({
                let entity = cx.entity().downgrade();
                move |event, _, cx| {
                    // Double-click resets to the default width.
                    if event.click_count() == 2 {
                        entity
                            .update(cx, |this, cx| {
                                if !this.last_drag_moved {
                                    this.sidebar_width = THREAD_SIDEBAR_DEFAULT_WIDTH;
                                    cx.notify();
                                }
                            })
                            .ok();
                    }
                }
            });

        // RightPanelResizeHandle: 8px hit area over the panel's left border;
        // 1px line, `bg-border` on hover, `bg-primary/60` while dragging.
        let dragging_panel = resizing == Some(ResizeTarget::RightPanel);
        let right_panel_handle = div()
            .id("right-panel-resize")
            .group("right-panel-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(viewport_width - right_panel_width - 4.))
            .w(px(8.))
            .cursor(CursorStyle::ResizeLeftRight)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(4.))
                    .w(px(1.))
                    .when(dragging_panel, |this| {
                        this.bg(with_alpha(theme.primary, 0.6))
                    })
                    .when(!dragging_panel, |this| {
                        this.group_hover("right-panel-resize", |style| style.bg(theme.border))
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.resizing = Some(ResizeDrag {
                        target: ResizeTarget::RightPanel,
                        start_x: f32::from(event.position.x),
                        start_width: right_panel_width,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            );

        let sidebar_toggle = div()
            .absolute()
            .top_0()
            .left(workspace_controls_left(window) + px(1.))
            .h(WORKSPACE_TOPBAR_HEIGHT)
            .flex()
            .items_center()
            .child(
                SidebarTrigger::new(sidebar_open)
                    .tooltip(Tooltip::new("Toggle main sidebar").shortcut("⌘B"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleSidebar), cx);
                    }),
            );

        let layout_controls = div()
            .absolute()
            .top_0()
            .right(WORKSPACE_CONTROLS_INSET)
            .mr(px(1.))
            .h(WORKSPACE_TOPBAR_HEIGHT)
            .flex()
            .items_center()
            .child(PanelLayoutControls {
                right_panel_open,
                right_panel_maximized: maximized,
                on_toggle_right_panel: Box::new(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleRightPanel), cx);
                }),
                on_toggle_maximize: Box::new({
                    let entity = cx.entity().downgrade();
                    move |_, _, cx| {
                        entity
                            .update(cx, |this, cx| {
                                this.right_panel_maximized = !this.right_panel_maximized;
                                cx.notify();
                            })
                            .ok();
                    }
                }),
            });

        div()
            .id("app-shell")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_right_panel))
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui_kit::ModifiersChangedEvent, _, cx| {
                    let modifiers = event.modifiers;
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.modifiers_changed(&modifiers, cx));
                },
            ))
            .map(|this| forward_sidebar_actions(this, cx))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.resizing.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .overflow_hidden()
            .bg(theme.app_chrome_background)
            .text_color(theme.foreground)
            .font_family(theme.fonts.sans.clone())
            .text_size(rems(1.))
            .when(resizing.is_some(), |this| {
                this.cursor(CursorStyle::ResizeLeftRight)
            })
            .child(
                SidebarProvider::new()
                    .with(|this| this.size_full())
                    .child(
                        SidebarColumn::new(sidebar_open)
                            .width(px(sidebar_width))
                            .child(self.sidebar.clone()),
                    )
                    .child(
                        SidebarInset::new()
                            .with(|this| this.flex_row().h_full())
                            .when(!maximized, |this| {
                                this.child(ChatView {
                                    sidebar_open,
                                    right_panel_open,
                                    thread: routed_thread.clone(),
                                })
                            })
                            .when(right_panel_open, |this| {
                                this.child(
                                    div()
                                        .relative()
                                        .h_full()
                                        .min_h_0()
                                        .min_w_0()
                                        .border_l_1()
                                        .border_color(theme.border)
                                        .map(|this| {
                                            if maximized {
                                                this.flex_1()
                                            } else {
                                                this.flex_shrink_0().w(px(right_panel_width))
                                            }
                                        })
                                        .child(RightPanelTabs),
                                )
                            }),
                    ),
            )
            .when(sidebar_open, |this| this.child(sidebar_rail))
            .when(right_panel_open && !maximized, |this| {
                this.child(right_panel_handle)
            })
            .child(sidebar_toggle)
            .child(layout_controls)
    }
}

/// The sidebar's keyboard commands bind in the shell's context; forward
/// them to the sidebar view.
fn forward_sidebar_actions(
    root: gpui_kit::Stateful<gpui_kit::Div>,
    cx: &mut Context<AppSidebarLayout>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    macro_rules! forward {
        ($root:expr, $($action:ident),*) => {
            $root$(.on_action(cx.listener(|this, action: &thread_sidebar::$action, window, cx| {
                this.sidebar.update(cx, |sidebar, cx| sidebar.handle_action(action, window, cx));
            })))*
        };
    }
    forward!(
        root,
        ThreadPrevious,
        ThreadNext,
        SettleThread,
        PinThread,
        NewThread,
        NewThreadInProject,
        ThreadJump1,
        ThreadJump2,
        ThreadJump3,
        ThreadJump4,
        ThreadJump5,
        ThreadJump6,
        ThreadJump7,
        ThreadJump8,
        ThreadJump9
    )
}
