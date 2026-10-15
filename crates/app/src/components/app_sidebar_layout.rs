//! Port of upstream/apps/web/src/components/AppSidebarLayout.tsx with the
//! desktop parts of components/ui/sidebar.tsx (`SidebarProvider`,
//! `Sidebar collapsible="offcanvas"`, `SidebarRail`, `SidebarTrigger`) and
//! the chat-column + inline right panel row from ChatView.tsx.
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
    AnyView, App, Context, CursorStyle, FocusHandle, InteractiveElement as _, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, actions, div, px, rems,
};

use super::chat::panel_layout_controls::PanelLayoutControls;
use super::chat_view::ChatView;
use super::right_panel_tabs::RightPanelTabs;
use super::sidebar::Sidebar;
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
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

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
    /// Replaces the chat column (the pull request page and the demo hooks).
    main_content: Option<AnyView>,
    /// Replaces the right panel's body.
    right_panel_content: Option<AnyView>,
    /// A modal layer over the whole window (dialogs).
    overlay: Option<AnyView>,
    _appearance: Subscription,
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
        Self {
            focus_handle,
            sidebar_open: true,
            sidebar_width: resolve_initial_thread_sidebar_width(None, viewport_width),
            right_panel_open: false,
            right_panel_maximized: false,
            right_panel_width: PREVIEW_PANEL_DEFAULT_WIDTH,
            resizing: None,
            last_drag_moved: false,
            main_content: None,
            right_panel_content: None,
            overlay: None,
            _appearance: appearance,
        }
    }

    /// Shows `content` where the chat column normally sits.
    pub fn set_main_content(&mut self, content: Option<AnyView>, cx: &mut Context<Self>) {
        self.main_content = content;
        cx.notify();
    }

    /// Shows `content` over the whole window (a dialog), or removes it.
    pub fn set_overlay(&mut self, content: Option<AnyView>, cx: &mut Context<Self>) {
        self.overlay = content;
        cx.notify();
    }

    /// Shows `content` as the right panel's body and opens the panel when it is set.
    pub fn set_right_panel_content(&mut self, content: Option<AnyView>, cx: &mut Context<Self>) {
        self.right_panel_open = content.is_some();
        self.right_panel_content = content;
        cx.notify();
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

        // SidebarRail: a 16px hit area centred on the sidebar's right edge,
        // showing a 2px `sidebar-border` line on hover. Laid over the main
        // column so its right half stays hittable.
        let sidebar_rail = div()
            .id("sidebar-rail")
            .group("sidebar-rail")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(sidebar_width - 8.))
            .w(px(16.))
            .cursor(CursorStyle::ResizeLeftRight)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(7.))
                    .w(px(2.))
                    .when(resizing == Some(ResizeTarget::Sidebar), |this| {
                        this.bg(theme.sidebar_border)
                    })
                    .group_hover("sidebar-rail", |style| style.bg(theme.sidebar_border)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.last_drag_moved = false;
                    this.resizing = Some(ResizeDrag {
                        target: ResizeTarget::Sidebar,
                        start_x: f32::from(event.position.x),
                        start_width: sidebar_width,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_click(cx.listener(|this, event: &gpui_kit::ClickEvent, _, cx| {
                // Double-click resets to the default width.
                if event.click_count() == 2 && !this.last_drag_moved {
                    this.sidebar_width = THREAD_SIDEBAR_DEFAULT_WIDTH;
                    cx.notify();
                }
            }))
            .tooltip(|window, cx| Tooltip::new("Drag to resize sidebar").build(window, cx));

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
                Button::new("sidebar-trigger")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconSm)
                    .icon(if sidebar_open {
                        IconName::PanelLeftClose
                    } else {
                        IconName::PanelLeft
                    })
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
            .when(sidebar_open, |this| {
                this.child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .h_full()
                        .w(px(sidebar_width))
                        .bg(theme.sidebar)
                        .border_r_1()
                        .border_color(theme.sidebar_border)
                        .text_color(theme.sidebar_foreground)
                        .child(Sidebar),
                )
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(theme.background)
                    .when(!maximized, |this| match self.main_content.clone() {
                        Some(content) => this.child(content),
                        None => this.child(ChatView {
                            sidebar_open,
                            right_panel_open,
                        }),
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
                                .child(match self.right_panel_content.clone() {
                                    Some(content) => content.into_any_element(),
                                    None => RightPanelTabs.into_any_element(),
                                }),
                        )
                    }),
            )
            .when(sidebar_open, |this| this.child(sidebar_rail))
            .when(right_panel_open && !maximized, |this| {
                this.child(right_panel_handle)
            })
            .child(sidebar_toggle)
            .child(layout_controls)
            .children(self.overlay.clone())
    }
}
