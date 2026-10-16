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
    AnyView, App, Context, CursorStyle, FocusHandle, InteractiveElement as _, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render,
    Styled as _, Subscription, Window, actions, div, px, rems,
};

use super::branch_toolbar::mock_source::MockBranchBackend;
use super::chat::chat_git_host::{ChatGitBackends, ChatGitHost, RoutedGitThread};
use super::chat::panel_layout_controls::PanelLayoutControls;
use super::chat_view::ChatView;
use super::command_palette::command_palette_logic::SearchOverlayMode;
use super::command_palette::mock_source::MockPaletteBackend;
use super::command_palette::palette_source::PaletteContext;
use super::command_palette::{
    self, CommandPalette, CommandPaletteEvent, CommandPaletteOpenDetail, CycleAppearance,
    SelectTheme, ToggleCommandPalette, ToggleFilePicker, ToggleProjectSearch,
};
use super::git_actions_control::mock_source::MockGitActionsBackend;
use super::right_panel_tabs::RightPanelTabs;
use super::sidebar::mock_source::mock_sidebar;
use super::sidebar::view_model::SidebarStore;
use super::sidebar::{self as thread_sidebar, Sidebar, SidebarEvent, SidebarRoute};
use super::thread_sidebar_width::{
    THREAD_SIDEBAR_DEFAULT_WIDTH, THREAD_SIDEBAR_MIN_WIDTH, clamp_thread_sidebar_width,
    resolve_initial_thread_sidebar_width, resolve_thread_sidebar_maximum_width,
};
use super::usage::mock_source::{MockUsageBackend, UsageScenario};
use super::usage::usage_page::{UsagePage, UsagePageEvent};
use super::workspace_titlebar::workspace_controls_left;
use crate::hooks::use_local_storage::MemoryLocalStorage;
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
        /// `threadDetails.toggle` (mod+shift+b).
        ToggleThreadDetails,
        /// `usage.open` (mod+u).
        OpenUsage,
    ]
);

const KEY_CONTEXT: &str = "AppShell";

/// Upstream default keybindings (packages/shared/src/keybindings.ts) for the
/// shell's actions.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleSidebar, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleRightPanel, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-b", ToggleThreadDetails, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-u", OpenUsage, Some(KEY_CONTEXT)),
    ]);
    thread_sidebar::bind_keys(cx, KEY_CONTEXT);
    command_palette::bind_keys(cx, KEY_CONTEXT);
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

/// `/usage`: the page and its Escape-to-go-back subscription.
struct UsageRoute {
    page: Entity<UsagePage>,
    _subscription: Subscription,
}

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
    sidebar: Entity<Sidebar>,
    store: Entity<SidebarStore>,
    palette: Entity<CommandPalette>,
    git_host: Entity<ChatGitHost>,
    thread_details_open: bool,
    /// The routed page when it is not a thread (upstream: the router).
    usage: Option<UsageRoute>,
    _appearance: Subscription,
    _sidebar_events: Subscription,
    _palette_events: Subscription,
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
        let palette_backend =
            std::rc::Rc::new(MockPaletteBackend::new(store.clone(), backend.clone()));
        let sidebar = cx.new(|cx| Sidebar::new(store.clone(), backend, window, cx));
        let sidebar_events = cx.subscribe_in(&sidebar, window, Self::on_sidebar_event);
        let palette = cx.new(|cx| CommandPalette::new(palette_backend, window, cx));
        let git_host = cx.new(|_| {
            ChatGitHost::new(ChatGitBackends {
                git: std::rc::Rc::new(MockGitActionsBackend::new()),
                branch: std::rc::Rc::new(MockBranchBackend::new()),
            })
        });
        let palette_events = cx.subscribe_in(&palette, window, Self::on_palette_event);
        sidebar.update(cx, |sidebar, cx| sidebar.apply_dev_overrides(window, cx));
        if let Ok(spec) = std::env::var("TURIN_GIT_UI") {
            git_host.update(cx, |host, cx| host.run_demo_steps(spec, window, cx));
        }
        if let Ok(spec) = std::env::var("TURIN_PALETTE") {
            palette.update(cx, |palette, cx| palette.run_demo_steps(spec, window, cx));
        }
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
            sidebar,
            store,
            palette,
            git_host,
            thread_details_open: std::env::var("TURIN_THREAD_DETAILS").is_ok_and(|v| v == "1"),
            usage: None,
            _appearance: appearance,
            _sidebar_events: sidebar_events,
            _palette_events: palette_events,
        }
    }

    /// `navigate({ to: "/usage" })`.
    pub fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.usage.is_some() {
            return;
        }
        // T22 swaps the mock backend for the live usage service.
        let backend = MockUsageBackend::shared(UsageScenario::from_env());
        let page = cx.new(|cx| {
            let mut page = UsagePage::new(backend, MemoryLocalStorage::shared(), window, cx);
            page.apply_dev_overrides(window, cx);
            page
        });
        let subscription = cx.subscribe(&page, |this, _, event: &UsagePageEvent, cx| match event {
            UsagePageEvent::Back => {
                this.usage = None;
                cx.notify();
            }
        });
        self.usage = Some(UsageRoute {
            page,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn open_usage_action(&mut self, _: &OpenUsage, window: &mut Window, cx: &mut Context<Self>) {
        self.open_usage(window, cx);
    }

    /// Hands the palette the routed thread so its contextual items target it.
    fn sync_palette_context(&mut self, cx: &mut Context<Self>) {
        let active_thread_key = match self.sidebar.read(cx).route() {
            Some(SidebarRoute::Thread(key)) => Some(key.clone()),
            _ => None,
        };
        self.palette.update(cx, |palette, cx| {
            palette.set_context(
                PaletteContext {
                    active_thread_key,
                    active_draft_project_key: None,
                },
                cx,
            )
        });
    }

    /// The shell side of the palette's requests, handled like `SidebarEvent`s:
    /// navigation that has a screen goes there; the rest wait for their tasks.
    fn on_palette_event(
        &mut self,
        _: &Entity<CommandPalette>,
        event: &CommandPaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unavailable = |label: &str, window: &mut Window, cx: &mut Context<Self>| {
            window.push_notification(
                Notification::info(format!("{label} is not available yet.")),
                cx,
            );
        };
        match event {
            CommandPaletteEvent::Closed => window.focus(&self.focus_handle, cx),
            CommandPaletteEvent::OpenThread(thread) => {
                let key = thread.key();
                self.sidebar
                    .update(cx, |sidebar, cx| sidebar.navigate_to_thread(&key, cx));
                self.sync_palette_context(cx);
                cx.notify();
            }
            CommandPaletteEvent::OpenProject { project_key } => {
                // The project's most recent thread, else a new one.
                let snapshot = self.store.read(cx).snapshot().clone();
                let latest = snapshot
                    .threads
                    .iter()
                    .filter(|thread| {
                        thread.archived_at.is_none() && thread.project_key() == *project_key
                    })
                    .max_by(|left, right| left.updated_at.cmp(&right.updated_at))
                    .map(|thread| thread.key());
                match latest {
                    Some(key) => {
                        self.sidebar
                            .update(cx, |sidebar, cx| sidebar.navigate_to_thread(&key, cx));
                        self.sync_palette_context(cx);
                        cx.notify();
                    }
                    None => unavailable("New thread", window, cx),
                }
            }
            CommandPaletteEvent::NewThreadInProject { .. }
            | CommandPaletteEvent::NewThreadFromContext => unavailable("New thread", window, cx),
            CommandPaletteEvent::NewThreadWithoutProject => {
                unavailable("New thread without a project", window, cx)
            }
            CommandPaletteEvent::FindInCurrentThread => unavailable("Find in thread", window, cx),
            CommandPaletteEvent::LinkPullRequest => unavailable("Link pull request", window, cx),
            CommandPaletteEvent::ShowLinkedPullRequests => {
                self.right_panel_open = true;
                cx.notify();
            }
            CommandPaletteEvent::OpenPullRequests => unavailable("Pull requests", window, cx),
            CommandPaletteEvent::OpenUsage => self.open_usage(window, cx),
            CommandPaletteEvent::OpenSettings | CommandPaletteEvent::OpenSetting { .. } => {
                unavailable("Settings", window, cx)
            }
            CommandPaletteEvent::OpenProjectSettings { .. } => {
                unavailable("Project settings", window, cx)
            }
            CommandPaletteEvent::ToggleThemeEditor => unavailable("Theme editor", window, cx),
        }
    }

    /// `openCommandPalette(detail)` for the rest of the app.
    #[allow(dead_code)]
    pub fn open_command_palette(
        &mut self,
        detail: CommandPaletteOpenDetail,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_palette_context(cx);
        self.palette
            .update(cx, |palette, cx| palette.open_with(detail, window, cx));
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

    fn on_sidebar_event(
        &mut self,
        _: &Entity<Sidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::Navigate(_) => {
                // Screenshot runs (`TURIN_PAGE`) keep their route: under load the
                // sidebar's startup selection can land well after the window opens.
                if std::env::var_os("TURIN_PAGE").is_none() {
                    self.usage = None;
                }
                self.sync_palette_context(cx);
                cx.notify();
            }
            SidebarEvent::OpenUsage => self.open_usage(window, cx),
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

    /// The routed thread as the git surfaces read it.
    fn routed_git_thread(&self, cx: &App) -> Option<RoutedGitThread> {
        let Some(SidebarRoute::Thread(key)) = self.sidebar.read(cx).route() else {
            return None;
        };
        let snapshot = self.store.read(cx).snapshot();
        let thread = snapshot
            .threads
            .iter()
            .find(|thread| &thread.key() == key)?;
        let project = snapshot.project(&thread.project_key())?;
        Some(RoutedGitThread {
            environment_id: thread.environment_id.clone(),
            thread_id: thread.id.clone(),
            project_cwd: project.workspace_root.clone(),
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            has_session: thread.runtime.is_some(),
            pull_requests: thread.pull_requests.clone(),
            project_threads: snapshot
                .threads
                .iter()
                .filter(|other| other.project_key() == thread.project_key())
                .map(|other| super::branch_toolbar::backend::ProjectThreadInfo {
                    branch: other.branch.clone(),
                    worktree_path: other.worktree_path.clone(),
                    updated_at: other.updated_at.clone(),
                    archived_at: other.archived_at.clone(),
                })
                .collect(),
        })
    }

    fn toggle_thread_details(
        &mut self,
        _: &ToggleThreadDetails,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.thread_details_open = !self.thread_details_open;
        cx.notify();
    }

    /// Opens the right panel; used by the screenshot harness (`TURIN_SHELL_RIGHT_PANEL`).
    pub fn set_right_panel_open(&mut self, open: bool) {
        self.right_panel_open = open;
    }

    fn toggle_command_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_palette_context(cx);
        self.palette.update(cx, |palette, cx| {
            palette.toggle_mode(SearchOverlayMode::Command, window, cx)
        });
    }

    fn toggle_file_picker(
        &mut self,
        _: &ToggleFilePicker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette.update(cx, |palette, cx| {
            palette.toggle_mode(SearchOverlayMode::Files, window, cx)
        });
    }

    fn toggle_project_search(
        &mut self,
        _: &ToggleProjectSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette.update(cx, |palette, cx| {
            palette.toggle_mode(SearchOverlayMode::Content, window, cx)
        });
    }

    fn select_theme(&mut self, _: &SelectTheme, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_palette_context(cx);
        self.palette
            .update(cx, |palette, cx| palette.open_change_theme(window, cx));
    }

    fn cycle_appearance(
        &mut self,
        _: &CycleAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette
            .update(cx, |palette, cx| palette.cycle_appearance(window, cx));
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
        // "Open diff" on a turn's changed files card shows the right panel.
        if crate::diff::take_open_panel_request(cx) {
            self.right_panel_open = true;
        }
        let theme = cx.app_theme().clone();
        let viewport_width = f32::from(window.viewport_size().width);
        let sidebar_width = self.effective_sidebar_width(viewport_width);
        let right_panel_width = clamp_preview_panel_width(
            self.right_panel_width,
            get_preview_panel_max_width(viewport_width, Some(self.main_width(viewport_width))),
        );
        let sidebar_open = self.sidebar_open;
        // The usage route has no right panel or layout controls.
        let usage_page = self.usage.as_ref().map(|route| route.page.clone());
        if let Some(page) = &usage_page {
            page.update(cx, |page, _| page.sidebar_open = sidebar_open);
        }
        let on_usage = usage_page.is_some();
        let right_panel_open = self.right_panel_open && !on_usage;
        let maximized = right_panel_open && self.right_panel_maximized;
        let resizing = self.resizing.map(|drag| drag.target);
        let routed_thread = self.routed_thread(cx);
        let routed_git = self.routed_git_thread(cx);
        self.git_host
            .update(cx, |host, cx| host.sync(routed_git, window, cx));

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
                thread_details_open: self.thread_details_open,
                on_toggle_thread_details: Box::new(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleThreadDetails), cx);
                }),
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
            .on_action(cx.listener(Self::toggle_thread_details))
            .on_action(cx.listener(Self::open_usage_action))
            .on_action(cx.listener(Self::toggle_command_palette))
            .on_action(cx.listener(Self::toggle_file_picker))
            .on_action(cx.listener(Self::toggle_project_search))
            .on_action(cx.listener(Self::select_theme))
            .on_action(cx.listener(Self::cycle_appearance))
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
                            .when_some(usage_page.clone(), |this, page| this.child(page))
                            .when(!maximized && !on_usage, |this| {
                                match self.main_content.clone() {
                                    Some(content) => this.child(content),
                                    None => this.child(ChatView {
                                        sidebar_open,
                                        right_panel_open,
                                        thread: routed_thread.clone(),
                                        git_host: self.git_host.clone(),
                                        thread_details_open: self.thread_details_open,
                                    }),
                                }
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
                    ),
            )
            .when(sidebar_open, |this| this.child(sidebar_rail))
            .when(right_panel_open && !maximized, |this| {
                this.child(right_panel_handle)
            })
            .child(sidebar_toggle)
            .when(!on_usage, |this| this.child(layout_controls))
            .children(self.overlay.clone())
            // The ⌘K overlay renders last so it covers the whole window.
            .child(self.palette.clone())
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
