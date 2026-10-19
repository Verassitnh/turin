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

use super::branch_toolbar::live_source::LiveBranchBackend;
use super::branch_toolbar::mock_source::MockBranchBackend;
use super::chat::chat_git_host::{ChatGitBackends, ChatGitHost, RoutedGitThread};
use super::chat::panel_layout_controls::PanelLayoutControls;
use super::chat_view::{ChatView, LiveChatTarget};
use super::command_palette::command_palette_logic::SearchOverlayMode;
use super::command_palette::live_source::LivePaletteBackend;
use super::command_palette::mock_source::MockPaletteBackend;
use super::command_palette::palette_source::PaletteContext;
use super::command_palette::palette_source::SharedPaletteBackend;
use super::command_palette::{
    self, CommandPalette, CommandPaletteEvent, CommandPaletteOpenDetail, CycleAppearance,
    OpenTarget, SelectTheme, ToggleCommandPalette, ToggleFilePicker, ToggleProjectSearch,
};
use super::confirm_dialog_host::ConfirmDialogHost;
use super::git_actions_control::live_source::LiveGitActionsBackend;
use super::git_actions_control::mock_source::MockGitActionsBackend;
use super::no_active_thread_state::NoActiveThreadState;
use super::preview_panel_shell::{PreviewPanelMode, PreviewPanelShell};
use super::quit_hold_overlay::QuitHoldOverlay;
use super::reopen_closed_view_shortcut::try_reopen_closed_view;
use super::right_panel_tabs::RightPanelTabs;
use super::sidebar::mock_source::mock_sidebar;
use super::sidebar::view_model::SidebarStore;
use super::sidebar::{self as thread_sidebar, Sidebar, SidebarEvent, SidebarRoute};
use super::thread_notification_coordinator::ThreadNotificationCoordinator;
use super::thread_sidebar_width::{
    THREAD_SIDEBAR_DEFAULT_WIDTH, THREAD_SIDEBAR_MIN_WIDTH, clamp_thread_sidebar_width,
    resolve_initial_thread_sidebar_width, resolve_thread_sidebar_maximum_width,
};
use super::usage::live_source::LiveUsageBackend;
use super::usage::mock_source::{MockUsageBackend, UsageScenario};
use super::usage::usage_page::{UsagePage, UsagePageEvent};
use super::workspace_titlebar::workspace_controls_left;
use crate::desktop::desktop_app_settings::{read_desktop_app_settings, write_desktop_app_settings};
use crate::desktop::desktop_application_menu::{CloseWindow, QuitApplication};
use crate::desktop::desktop_client_settings::{client_settings_path, read_client_settings};
use crate::desktop::notification_badge::clear_notification_badge_on_focus;
use crate::desktop::quit_hold::{
    QUIT_HOLD_CLOCK_MS, QuitHoldKey, QuitHoldKeyInput, QuitHoldKeyPhase, QuitShortcutCallbacks,
    QuitShortcutEffect, QuitShortcutHandler, QuitShortcutHint,
};
use crate::desktop::window_bounds_persist::{
    WindowBoundsPersist, debounce_delay, geometry_document,
};
use crate::hooks::use_local_storage::MemoryLocalStorage;
use crate::hooks::use_preview_panel_inline_size::{
    PREVIEW_PANEL_DEFAULT_WIDTH, clamp_preview_panel_width, get_preview_panel_max_width,
};
use crate::pull_request::live_navigation::{PullRequestsRouteEvent, mount_route};
use crate::pull_request::pull_requests_route_search::validate_pull_requests_search;
use crate::shell::chat_route_shortcuts::{OpenCommandPalette, ReopenClosedView};
use crate::shell::environment_coordinator::poll_environment_notifications;
use crate::shell::live_settings::{LiveShellHandle, open_setting_by_id, open_settings_named};
use crate::shell::pr_preferences::LocalPreferenceStorage;
use crate::shell::reopen_closed_view::{ReopenOwnerState, closed_view_entries_from_store};
use crate::state::right_panel_layout::{
    ThreadPanelPresentation, right_panel_presentation_for_width,
};
use crate::state::right_panel_store::RightPanelStore;
use crate::state::store::ClientStoreGlobal;
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
use serde_json::Map;
use std::rc::Rc;
use std::sync::Arc;
use turin_contracts::ThreadId;
use turin_contracts::settings::{ClientSettings, QuitConfirmationMode};

use super::sidebar::live_source::LiveSidebar;
use super::sidebar::view_model::SharedSidebarBackend;
use crate::app_runtime::write::bus::CommandBusGlobal;
use crate::app_runtime::write::commands::CreateThread;
use crate::state::local_environment_id;
use crate::state::models::scoped_thread_key;

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
        /// ⌘Q key-down (quit-hold handler).
        QuitShortcut,
        /// `thread.undo` (mod+z).
        UndoLatestThread,
    ]
);

pub const KEY_CONTEXT: &str = "AppShell";

/// The model a new thread starts on until the default-model setting is read.
const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-4-6";

/// Upstream default keybindings (packages/shared/src/keybindings.ts) for the
/// shell's actions.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleSidebar, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleRightPanel, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-b", ToggleThreadDetails, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-u", OpenUsage, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-z", UndoLatestThread, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-q", QuitShortcut, Some(KEY_CONTEXT)),
    ]);
    thread_sidebar::bind_keys(cx, KEY_CONTEXT);
    command_palette::bind_keys(cx, KEY_CONTEXT);
    crate::shell::bind_chat_route_keys(cx);
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

/// `/_chat/pull-requests`: live list + detail panel host.
struct PullRequestsRoute {
    host: Entity<crate::pull_request::live_navigation::PullRequestsRouteHost>,
    _subscription: Subscription,
}

/// What the shell needs to run against Turin's own data.
type LiveShell = LiveShellHandle;

/// The live bindings, unless `TURIN_MOCK_UI=1` asks for the fixture UI
/// (screenshots, demos).
fn live_bindings(cx: &App) -> Option<LiveShell> {
    if std::env::var("TURIN_MOCK_UI").is_ok_and(|value| value == "1") {
        return None;
    }
    Some(LiveShellHandle::new(
        cx.try_global::<ClientStoreGlobal>()?.0.clone(),
        cx.try_global::<CommandBusGlobal>()?.0.clone(),
    ))
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
    /// The running app's bindings; `None` runs the fixture-driven demo UI.
    live: Option<LiveShell>,
    thread_details_open: bool,
    /// The routed page when it is not a thread (upstream: the router).
    usage: Option<UsageRoute>,
    pull_requests: Option<PullRequestsRoute>,
    pr_preferences: crate::hooks::use_local_storage::SharedLocalStorage,
    notifications: ThreadNotificationCoordinator,
    quit_handler: QuitShortcutHandler,
    quit_clock_start: std::time::Instant,
    quit_clock_generation: u64,
    quit_wake_generation: u64,
    quit_overlay: Entity<QuitHoldOverlay>,
    confirm_dialog: Entity<ConfirmDialogHost>,
    client_settings: ClientSettings,
    client_settings_path: std::path::PathBuf,
    _client_settings_watch: gpui_kit::Task<()>,
    bounds_persist: WindowBoundsPersist,
    right_panel_store: RightPanelStore,
    _appearance: Subscription,
    _sidebar_events: Subscription,
    _palette_events: Subscription,
    _bounds: Subscription,
    _store_watch: Subscription,
    _activation: Subscription,
    toast_viewport: Entity<crate::ui::toast::ToastViewport>,
    anchored_toast_viewport: Entity<crate::ui::toast::AnchoredToastViewport>,
    draft_host: Entity<crate::shell::composer_draft_host::ComposerDraftHost>,
    _keybindings_warning: Option<Subscription>,
    /// Launch toast and sidebar pill for provider updates (live shell only).
    provider_updates: Option<Entity<crate::shell::provider_updates::ProviderUpdateHost>>,
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
        let live = live_bindings(cx);
        let (store, backend, palette_backend): (
            Entity<SidebarStore>,
            SharedSidebarBackend,
            SharedPaletteBackend,
        ) = match &live {
            Some(live) => {
                let (store, backend) =
                    LiveSidebar::start(live.client.clone(), live.bus.clone(), cx);
                let backend: SharedSidebarBackend = backend;
                let palette: SharedPaletteBackend = std::rc::Rc::new(LivePaletteBackend::new(
                    store.clone(),
                    backend.clone(),
                    live.bus.clone(),
                ));
                (store, backend, palette)
            }
            None => {
                let (store, backend) = mock_sidebar(cx);
                let palette: SharedPaletteBackend =
                    std::rc::Rc::new(MockPaletteBackend::new(store.clone(), backend.clone()));
                (store, backend, palette)
            }
        };
        let sidebar = cx.new(|cx| Sidebar::new(store.clone(), backend, window, cx));
        let sidebar_events = cx.subscribe_in(&sidebar, window, Self::on_sidebar_event);
        let palette = cx.new(|cx| CommandPalette::new(palette_backend, window, cx));
        let git_host = cx.new(|_| {
            let backends = match &live {
                Some(live) => {
                    let git = LiveGitActionsBackend::new(live.bus.clone());
                    ChatGitBackends {
                        branch: Rc::new(LiveBranchBackend::new(live.bus.clone(), git.clone())),
                        git: Rc::new(git),
                    }
                }
                None => ChatGitBackends {
                    git: Rc::new(MockGitActionsBackend::new()),
                    branch: Rc::new(MockBranchBackend::new()),
                },
            };
            ChatGitHost::new(backends)
        });
        let palette_events = cx.subscribe_in(&palette, window, Self::on_palette_event);
        sidebar.update(cx, |sidebar, cx| sidebar.apply_dev_overrides(window, cx));
        if let Ok(spec) = std::env::var("TURIN_GIT_UI") {
            git_host.update(cx, |host, cx| host.run_demo_steps(spec, window, cx));
        }
        if let Ok(spec) = std::env::var("TURIN_PALETTE") {
            palette.update(cx, |palette, cx| palette.run_demo_steps(spec, window, cx));
        }
        let quit_overlay = cx.new(|_| QuitHoldOverlay::new());
        let confirm_dialog = cx.new(|cx| ConfirmDialogHost::new(window, cx));
        let client_settings_path = client_settings_path();
        let bounds_persist =
            WindowBoundsPersist::new(crate::desktop::desktop_app_settings::desktop_settings_path());
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.schedule_bounds_write(window, cx);
        });
        {
            let entity = cx.entity().downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                entity
                    .update(cx, |this, cx| {
                        this.flush_bounds(window, cx);
                    })
                    .ok();
                true
            });
        }
        let store_watch = cx.observe_global::<ClientStoreGlobal>(|this, cx| {
            this.sync_notifications(cx);
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                clear_notification_badge_on_focus();
                this.notifications.clear();
                this.reload_client_settings(cx);
            }
            cx.notify();
        });
        let client_settings_watch = Self::watch_client_settings(client_settings_path.clone(), cx);
        let toast_viewport = crate::ui::toast::toasts(cx);
        let anchored_toast_viewport = crate::ui::toast::anchored_toasts(cx);
        let draft_host = cx.new(|_| crate::shell::composer_draft_host::ComposerDraftHost::new());
        crate::shell::composer_draft_host::ComposerDraftHost::register(draft_host.clone(), cx);
        let _keybindings_warning = live
            .as_ref()
            .map(|live| crate::shell::keybindings_warning::start(live.bus.clone(), cx));
        let provider_updates = live.as_ref().map(|live| {
            crate::shell::provider_updates::start(live.clone(), sidebar.downgrade(), cx)
        });
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
            live,
            thread_details_open: std::env::var("TURIN_THREAD_DETAILS").is_ok_and(|v| v == "1"),
            usage: None,
            pull_requests: None,
            pr_preferences: MemoryLocalStorage::shared(),
            notifications: ThreadNotificationCoordinator::new(),
            quit_handler: QuitShortcutHandler::new(cfg!(target_os = "macos")),
            quit_clock_start: std::time::Instant::now(),
            quit_clock_generation: 0,
            quit_wake_generation: 0,
            quit_overlay,
            confirm_dialog,
            client_settings: ClientSettings::default(),
            client_settings_path,
            _client_settings_watch: client_settings_watch,
            bounds_persist,
            right_panel_store: RightPanelStore::default(),
            _appearance: appearance,
            _sidebar_events: sidebar_events,
            _palette_events: palette_events,
            _bounds: bounds,
            _store_watch: store_watch,
            _activation: activation,
            toast_viewport,
            anchored_toast_viewport,
            draft_host,
            _keybindings_warning,
            provider_updates,
        }
    }

    fn undo_latest_thread_action(
        &mut self,
        _: &UndoLatestThread,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_host.update(cx, |host, cx| {
            if let Some(message) = host.undo_latest() {
                crate::ui::toast::toast_manager(cx).update(cx, |store, cx| {
                    store.add(
                        crate::ui::toast::ToastOptions::new("Failed to restore draft")
                            .type_(crate::ui::toast_logic::ToastType::Error)
                            .description(message),
                        cx,
                    );
                });
            }
            cx.notify();
        });
        cx.notify();
    }

    fn settings_window_options(&self, cx: &App) -> gpui_kit::WindowOptions {
        crate::shell::live_settings::settings_window_options(cx)
    }

    fn open_live_settings(&mut self, page: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(live) = self.live.clone() else {
            window.push_notification(Notification::info("Settings is not available yet."), cx);
            return;
        };
        let options = self.settings_window_options(cx);
        open_settings_named(&live, page, options, window, cx);
    }

    /// `navigate({ to: "/usage" })`.
    fn clear_auxiliary_routes(&mut self, cx: &mut Context<Self>) {
        if self.usage.is_some() || self.pull_requests.is_some() {
            self.usage = None;
            self.pull_requests = None;
            self.main_content = None;
            self.right_panel_content = None;
            cx.notify();
        }
    }

    /// `/_chat/pull-requests` (live navigation host).
    fn open_pull_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pull_requests.is_some() {
            return;
        }
        let Some(live) = self.live.clone() else {
            window.push_notification(
                Notification::info("Pull requests are not available yet."),
                cx,
            );
            return;
        };
        self.usage = None;
        let search = validate_pull_requests_search(&Map::new());
        let prefs = LocalPreferenceStorage(self.pr_preferences.clone());
        let sidebar = self.sidebar.clone();
        let open_thread = Arc::new(move |thread_id: &ThreadId, cx: &mut App| {
            let key = scoped_thread_key(&local_environment_id(), thread_id);
            sidebar.update(cx, |sidebar, cx| sidebar.navigate_to_thread(&key, cx));
        });
        let client = live.client.read(cx).handle().clone();
        let host = mount_route(
            live.bus.clone(),
            client,
            Some(open_thread),
            search,
            &prefs,
            window,
            cx,
        );
        self.set_main_content(Some(host.read(cx).page_view()), cx);
        let layout = cx.entity().downgrade();
        let host_for_sub = host.clone();
        let subscription = cx.subscribe_in(&host, window, move |_, _, event, _, cx| {
            if matches!(event, PullRequestsRouteEvent::OpenPanel) {
                layout
                    .update(cx, |this, cx| {
                        if let Some(route) = &this.pull_requests {
                            this.set_right_panel_content(
                                Some(route.host.read(cx).panel_view()),
                                cx,
                            );
                        }
                    })
                    .ok();
            }
        });
        self.pull_requests = Some(PullRequestsRoute {
            host: host_for_sub,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.usage.is_some() {
            return;
        }
        self.pull_requests = None;
        self.main_content = None;
        let backend: std::rc::Rc<dyn super::usage::backend::UsageBackend> =
            if let Some(live) = &self.live {
                LiveUsageBackend::shared(live.bus.clone())
            } else {
                MockUsageBackend::shared(UsageScenario::from_env())
            };
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

    fn notify_unavailable(label: &str, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            Notification::info(format!("{label} is not available yet.")),
            cx,
        );
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
                    None if self.live.is_some() => {
                        self.create_thread_in_project(project_key.clone(), window, cx)
                    }
                    None => unavailable("New thread", window, cx),
                }
            }
            CommandPaletteEvent::NewThreadInProject { project_key } if self.live.is_some() => {
                self.create_thread_in_project(project_key.clone(), window, cx)
            }
            CommandPaletteEvent::NewThreadFromContext if self.live.is_some() => {
                self.new_thread_from_context(window, cx)
            }
            CommandPaletteEvent::NewThreadInProject { .. }
            | CommandPaletteEvent::NewThreadFromContext => unavailable("New thread", window, cx),
            CommandPaletteEvent::NewThreadWithoutProject => {
                unavailable("New thread without a project", window, cx)
            }
            CommandPaletteEvent::FindInCurrentThread => unavailable("Find in thread", window, cx),
            CommandPaletteEvent::LinkPullRequest => self.open_pull_requests(window, cx),
            CommandPaletteEvent::ShowLinkedPullRequests => {
                self.open_pull_requests(window, cx);
            }
            CommandPaletteEvent::OpenPullRequests => self.open_pull_requests(window, cx),
            CommandPaletteEvent::OpenUsage => self.open_usage(window, cx),
            CommandPaletteEvent::OpenSettings => self.open_live_settings("general", window, cx),
            CommandPaletteEvent::OpenSetting { id } => {
                let Some(live) = self.live.clone() else {
                    unavailable("Settings", window, cx);
                    return;
                };
                let options = self.settings_window_options(cx);
                open_setting_by_id(&live, id, options, window, cx);
            }
            CommandPaletteEvent::OpenProjectSettings { .. } => {
                self.open_live_settings("projects", window, cx);
            }
            CommandPaletteEvent::ToggleThemeEditor => {
                self.open_live_settings("appearance", window, cx);
            }
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
                    self.clear_auxiliary_routes(cx);
                }
                self.sync_palette_context(cx);
                cx.notify();
            }
            SidebarEvent::OpenUsage => self.open_usage(window, cx),
            SidebarEvent::OpenProviderSettings => {
                if let Some(live) = self.live.clone() {
                    crate::shell::live_settings::open_provider_settings(&live, None, cx);
                }
            }
            SidebarEvent::DismissProviderUpdatePill(key) => {
                if let Some(host) = self.provider_updates.clone() {
                    host.update(cx, |host, cx| host.dismiss_pill(key, cx));
                }
            }
            SidebarEvent::OpenSettings => self.open_live_settings("general", window, cx),
            SidebarEvent::OpenProjectSettings(_) => {
                self.open_live_settings("projects", window, cx);
            }
            SidebarEvent::OpenPullRequests(_) | SidebarEvent::OpenPullRequestsPage => {
                self.open_pull_requests(window, cx);
            }
            SidebarEvent::AddProject if self.live.is_some() => self.open_command_palette(
                CommandPaletteOpenDetail {
                    open: Some(OpenTarget::AddProject),
                    ..Default::default()
                },
                window,
                cx,
            ),
            SidebarEvent::NewThread {
                project_key: Some(project_key),
                ..
            } if self.live.is_some() => {
                self.create_thread_in_project(project_key.clone(), window, cx)
            }
            SidebarEvent::NewThread { .. } if self.live.is_some() => {
                self.new_thread_from_context(window, cx)
            }
            SidebarEvent::NewThread { .. } if self.live.is_none() => {
                Self::notify_unavailable("New thread", window, cx);
            }
            SidebarEvent::AddProject if self.live.is_none() => {
                Self::notify_unavailable("Add project", window, cx);
            }
            _ => {}
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

    /// The routed thread's id, read from the route key (`<environment>:<thread>`)
    /// so a thread that was just created opens before the shell feed lists it.
    fn routed_thread_id(&self, cx: &App) -> Option<ThreadId> {
        let Some(SidebarRoute::Thread(key)) = self.sidebar.read(cx).route() else {
            return None;
        };
        let (_, thread_id) = key.split_once(':')?;
        ThreadId::new(thread_id).ok()
    }

    /// `handleNewThread(projectRef)`: creates the thread at once and opens it.
    /// Upstream keeps an unsent draft and creates the thread with its first
    /// message; Turin has no draft store yet (docs/handoff/T22.md).
    fn create_thread_in_project(
        &mut self,
        project_key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(live) = self.live.clone() else {
            return;
        };
        let Some(project_id) = project_key
            .split_once(':')
            .and_then(|(_, id)| turin_contracts::ProjectId::new(id).ok())
        else {
            return;
        };
        let task = live.bus.task(cx, move |commands| async move {
            commands
                .create_thread(CreateThread {
                    project_id,
                    title: "New thread".into(),
                    model_selection: serde_json::json!({
                        "instanceId": "claudeAgent",
                        "model": DEFAULT_CLAUDE_MODEL,
                    }),
                    runtime_mode: "full-access".into(),
                    interaction_mode: "default".into(),
                })
                .await
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| match outcome {
                Ok(thread_id) => {
                    let key = scoped_thread_key(&local_environment_id(), &thread_id);
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.navigate_to_thread(&key, cx));
                    this.sync_palette_context(cx);
                    cx.notify();
                }
                Err(error) => window.push_notification(
                    Notification::error(error.message).title("Failed to create thread"),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// "New thread" with no project in mind: the project of the routed thread,
    /// else the palette's project picker.
    fn new_thread_from_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let routed = self.routed_thread_id(cx);
        let snapshot = self.store.read(cx).snapshot().clone();
        let project_key = routed
            .and_then(|id| snapshot.threads.iter().find(|thread| thread.id == id))
            .map(|thread| thread.project_key());
        match project_key {
            Some(key) => self.create_thread_in_project(key, window, cx),
            None => self.open_command_palette(
                CommandPaletteOpenDetail {
                    open: Some(OpenTarget::NewThreadIn),
                    ..Default::default()
                },
                window,
                cx,
            ),
        }
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

    fn sync_notifications(&mut self, cx: &App) {
        let environment_ids = vec![crate::state::local_environment_id()];
        let snapshot = cx.global::<ClientStoreGlobal>().0.read(cx).state().clone();
        poll_environment_notifications(
            &mut self.notifications,
            &environment_ids,
            Some(snapshot.as_ref()),
            &self.client_settings,
        );
    }

    fn quit_confirmation_mode(&self) -> QuitConfirmationMode {
        self.client_settings.confirm_quit
    }

    fn quit_now_ms(&self) -> u64 {
        self.quit_clock_start.elapsed().as_millis() as u64
    }

    fn honor_quit(&self, effect: QuitShortcutEffect, cx: &mut Context<Self>) {
        if effect == QuitShortcutEffect::Quit {
            cx.quit();
        }
    }

    fn run_quit_shortcut(
        &mut self,
        phase: QuitHoldKeyPhase,
        key: QuitHoldKey,
        is_auto_repeat: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now_ms = self.quit_now_ms();
        self.quit_handler.set_now_ms(now_ms);
        let mode = self.quit_confirmation_mode();
        let (effect, wake) = {
            let mut notify = |hint: QuitShortcutHint| {
                self.quit_overlay.update(cx, |overlay, cx| {
                    overlay.apply_hint(hint, now_ms, cx);
                });
            };
            let mut conceal = || {
                crate::desktop::desktop_window::conceal_pending_quit_window(window);
            };
            let mut callbacks = QuitShortcutCallbacks {
                notify: &mut notify,
                conceal_window: &mut conceal,
            };
            let effect = if is_auto_repeat {
                self.quit_handler.held_key_tick(mode, &mut callbacks)
            } else {
                self.quit_handler.handle(
                    QuitHoldKeyInput {
                        phase,
                        key,
                        meta: cfg!(target_os = "macos"),
                        control: !cfg!(target_os = "macos"),
                        alt: false,
                        shift: false,
                        is_auto_repeat,
                    },
                    mode,
                    &mut callbacks,
                )
            };
            let wake = self.quit_handler.poll_watchdog(&mut callbacks);
            (effect, wake)
        };
        self.honor_quit(effect, cx);
        self.honor_quit(wake, cx);
        self.schedule_quit_wake(cx);
        if phase == QuitHoldKeyPhase::KeyDown
            && !is_auto_repeat
            && self.quit_handler.clock_should_run()
        {
            self.start_hold_clock(cx);
        }
        cx.notify();
    }

    fn start_hold_clock(&mut self, cx: &mut Context<Self>) {
        self.quit_clock_generation = self.quit_clock_generation.wrapping_add(1);
        let generation = self.quit_clock_generation;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(QUIT_HOLD_CLOCK_MS))
                    .await;
                let keep = this
                    .update_in(cx, |this, window, cx| {
                        if this.quit_clock_generation != generation {
                            return false;
                        }
                        this.run_quit_shortcut(
                            QuitHoldKeyPhase::KeyDown,
                            QuitHoldKey::Character('q'),
                            true,
                            window,
                            cx,
                        );
                        this.quit_handler.clock_should_run()
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    fn schedule_quit_wake(&mut self, cx: &mut Context<Self>) {
        let Some(deadline) = self.quit_handler.next_wake_ms() else {
            return;
        };
        let delay = deadline.saturating_sub(self.quit_now_ms());
        self.quit_wake_generation = self.quit_wake_generation.wrapping_add(1);
        let generation = self.quit_wake_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(delay.max(1)))
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.quit_wake_generation != generation {
                    return;
                }
                let now_ms = this.quit_now_ms();
                this.quit_handler.set_now_ms(now_ms);
                let wake = {
                    let mut notify = |hint: QuitShortcutHint| {
                        this.quit_overlay.update(cx, |overlay, cx| {
                            overlay.apply_hint(hint, now_ms, cx);
                        });
                    };
                    let mut conceal = || {
                        crate::desktop::desktop_window::conceal_pending_quit_window(window);
                    };
                    let mut callbacks = QuitShortcutCallbacks {
                        notify: &mut notify,
                        conceal_window: &mut conceal,
                    };
                    this.quit_handler.poll_watchdog(&mut callbacks)
                };
                this.honor_quit(wake, cx);
                this.schedule_quit_wake(cx);
            })
            .ok();
        })
        .detach();
    }

    fn on_quit_shortcut(&mut self, _: &QuitShortcut, window: &mut Window, cx: &mut Context<Self>) {
        self.run_quit_shortcut(
            QuitHoldKeyPhase::KeyDown,
            QuitHoldKey::Character('q'),
            false,
            window,
            cx,
        );
    }

    fn on_quit_application(
        &mut self,
        _: &QuitApplication,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.quit();
    }

    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_bounds(window, cx);
        window.remove_window();
    }

    fn on_open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.palette.read(cx).is_open() {
            self.sync_palette_context(cx);
            self.palette.update(cx, |palette, cx| {
                palette.toggle_mode(SearchOverlayMode::Command, window, cx)
            });
        }
    }

    fn on_reopen_closed_view(
        &mut self,
        _: &ReopenClosedView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = closed_view_entries_from_store(&self.right_panel_store);
        let owners: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.id.clone(),
                    self.right_panel_store.thread_state(&entry.thread_ref),
                )
            })
            .collect();
        let reopened = try_reopen_closed_view(
            &entries,
            |entry| ReopenOwnerState {
                environment_known: true,
                catalog_ready: true,
                owner_exists: true,
                shell_live: true,
                panel: owners
                    .iter()
                    .find(|(id, _)| id == &entry.id)
                    .map(|(_, panel)| panel.clone())
                    .unwrap_or_default(),
            },
            &mut self.right_panel_store,
            true,
        );
        if reopened {
            self.right_panel_open = true;
        }
        cx.notify();
    }

    fn schedule_bounds_write(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(generation) = self.bounds_persist.note_bounds_changed(window) else {
            return;
        };
        let path = self.bounds_persist.path().to_path_buf();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(debounce_delay()).await;
            let pending = this
                .update(cx, |this, _| {
                    this.bounds_persist.pending_if_current(generation)
                })
                .ok()
                .flatten();
            let Some(pending) = pending else {
                return;
            };
            let document = geometry_document(&pending);
            cx.background_executor()
                .spawn(async move {
                    let mut doc = read_desktop_app_settings(&path).unwrap_or_default();
                    doc.main_window_bounds = document.main_window_bounds;
                    doc.main_window_maximized = document.main_window_maximized;
                    let _ = write_desktop_app_settings(&path, &doc);
                })
                .detach();
        })
        .detach();
    }

    fn flush_bounds(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(pending) = self.bounds_persist.flush(window) else {
            return;
        };
        let path = self.bounds_persist.path().to_path_buf();
        let document = geometry_document(&pending);
        cx.background_executor()
            .spawn(async move {
                let mut doc = read_desktop_app_settings(&path).unwrap_or_default();
                doc.main_window_bounds = document.main_window_bounds;
                doc.main_window_maximized = document.main_window_maximized;
                let _ = write_desktop_app_settings(&path, &doc);
            })
            .detach();
    }

    fn apply_client_settings(&mut self, settings: ClientSettings, cx: &mut Context<Self>) {
        self.client_settings = settings;
        self.sync_notifications(cx);
        cx.notify();
    }

    fn reload_client_settings(&mut self, cx: &mut Context<Self>) {
        let path = self.client_settings_path.clone();
        cx.spawn(async move |this, cx| {
            let settings = cx
                .background_executor()
                .spawn(async move { read_client_settings(&path).ok().flatten() })
                .await;
            if let Some(settings) = settings {
                this.update(cx, |this, cx| this.apply_client_settings(settings, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn watch_client_settings(
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Task<()> {
        cx.spawn(async move |this, cx| {
            let initial_path = path.clone();
            let initial = cx
                .background_executor()
                .spawn(async move { read_client_settings(&initial_path).ok().flatten() })
                .await;
            if let Some(settings) = initial {
                this.update(cx, |this, cx| this.apply_client_settings(settings, cx))
                    .ok();
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let watched = path.clone();
            let watcher =
                cx.background_executor()
                    .spawn(async move {
                        turin_runtime::support::file_watch::watch_file(&watched, tx).ok()
                    })
                    .await;
            let Some(watcher) = watcher else {
                return;
            };
            while rx.recv().await.is_some() {
                // This future runs on GPUI, so its debounce timer must use
                // GPUI's executor rather than Tokio's ambient reactor.
                loop {
                    let event = rx.recv();
                    let quiet = cx
                        .background_executor()
                        .timer(turin_runtime::support::file_watch::WATCH_DEBOUNCE);
                    futures::pin_mut!(event, quiet);
                    match futures::future::select(event, quiet).await {
                        futures::future::Either::Left((Some(()), _)) => continue,
                        _ => break,
                    }
                }
                let path = path.clone();
                let settings = cx
                    .background_executor()
                    .spawn(async move { read_client_settings(&path).ok().flatten() })
                    .await;
                if let Some(settings) = settings
                    && this
                        .update(cx, |this, cx| this.apply_client_settings(settings, cx))
                        .is_err()
                {
                    break;
                }
            }
            drop(watcher);
        })
    }
}

impl Render for AppSidebarLayout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now_ms = self.quit_clock_start.elapsed().as_millis() as u64;
        self.quit_handler.set_now_ms(now_ms);
        self.quit_overlay
            .update(cx, |overlay, cx| overlay.tick(now_ms, cx));
        // "Open diff" on a turn's changed files card shows the right panel.
        if crate::diff::take_open_panel_request(cx) {
            self.right_panel_open = true;
        }
        if let Some(thread_id) =
            crate::components::chat::thread_navigation::take_thread_open_request(cx)
            && let Ok(thread_id) = ThreadId::new(&thread_id)
        {
            self.clear_auxiliary_routes(cx);
            let key = scoped_thread_key(&local_environment_id(), &thread_id);
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.navigate_to_thread(&key, cx));
            self.sync_palette_context(cx);
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
        let panel_presentation = right_panel_presentation_for_width(viewport_width);
        let right_panel_sheet = panel_presentation == ThreadPanelPresentation::Popover;
        let maximized = right_panel_open && self.right_panel_maximized;
        let right_panel_content = self.right_panel_content.clone();
        let resizing = self.resizing.map(|drag| drag.target);
        let routed_thread = self.routed_thread(cx);
        let routed_thread_id = self.routed_thread_id(cx);
        // `NoActiveThreadState`: the index route, with no thread or draft open.
        let no_active_thread = self.sidebar.read(cx).route().is_none();
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
            .on_action(cx.listener(Self::undo_latest_thread_action))
            .on_action(cx.listener(Self::toggle_command_palette))
            .on_action(cx.listener(Self::toggle_file_picker))
            .on_action(cx.listener(Self::toggle_project_search))
            .on_action(cx.listener(Self::select_theme))
            .on_action(cx.listener(Self::cycle_appearance))
            .on_action(cx.listener(Self::on_quit_shortcut))
            .on_action(cx.listener(Self::on_quit_application))
            .on_action(cx.listener(Self::on_close_window))
            .on_action(cx.listener(Self::on_open_command_palette))
            .on_action(cx.listener(Self::on_reopen_closed_view))
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui_kit::ModifiersChangedEvent, window, cx| {
                    let modifiers = event.modifiers;
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.modifiers_changed(&modifiers, cx));
                    if !modifiers.platform {
                        this.quit_clock_generation = this.quit_clock_generation.wrapping_add(1);
                        this.run_quit_shortcut(
                            QuitHoldKeyPhase::KeyUp,
                            QuitHoldKey::Modifier,
                            false,
                            window,
                            cx,
                        );
                    }
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
                                    None if no_active_thread => {
                                        this.child(NoActiveThreadState { sidebar_open })
                                    }
                                    None => this.child(ChatView {
                                        live: self.live.as_ref().map(|live| LiveChatTarget {
                                            client: live.client.clone(),
                                            bus: live.bus.clone(),
                                            thread: routed_thread_id.clone(),
                                        }),
                                        sidebar_open,
                                        right_panel_open,
                                        thread: routed_thread.clone(),
                                        git_host: self.git_host.clone(),
                                        thread_details_open: self.thread_details_open,
                                        subagent_bar:
                                            crate::components::chat_view::subagent_bar_demo(),
                                    }),
                                }
                            })
                            .when(right_panel_open && !right_panel_sheet, |this| {
                                let body = match right_panel_content.clone() {
                                    Some(content) => content.into_any_element(),
                                    None => RightPanelTabs.into_any_element(),
                                };
                                this.child(
                                    PreviewPanelShell::new(
                                        PreviewPanelMode::Inline,
                                        right_panel_width,
                                    )
                                    .maximized(maximized)
                                    .open(true)
                                    .child(body),
                                )
                            }),
                    ),
            )
            .when(sidebar_open, |this| this.child(sidebar_rail))
            .when(
                right_panel_open && !maximized && !right_panel_sheet,
                |this| this.child(right_panel_handle),
            )
            .when(right_panel_open && right_panel_sheet, |this| {
                let body = match right_panel_content {
                    Some(content) => content.into_any_element(),
                    None => RightPanelTabs.into_any_element(),
                };
                this.child(
                    div().absolute().inset_0().flex().justify_end().child(
                        PreviewPanelShell::new(PreviewPanelMode::Sheet, right_panel_width)
                            .maximized(false)
                            .open(true)
                            .child(body),
                    ),
                )
            })
            .child(sidebar_toggle)
            .when(!on_usage, |this| this.child(layout_controls))
            .children(self.overlay.clone())
            .child(self.confirm_dialog.clone())
            .child(self.quit_overlay.clone())
            .child(self.toast_viewport.clone())
            .child(self.anchored_toast_viewport.clone())
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
