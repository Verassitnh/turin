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

use super::chat::panel_layout_controls::PanelLayoutControls;
use super::confirm_dialog_host::ConfirmDialogHost;
use super::quit_hold_overlay::QuitHoldOverlay;
use super::reopen_closed_view_shortcut::try_reopen_closed_view;
use super::right_panel_tabs::RightPanelTabs;
use super::sidebar::mock_source::mock_sidebar;
use super::sidebar::view_model::SidebarStore;
use super::sidebar::{self as thread_sidebar, Sidebar, SidebarEvent, SidebarRoute};
use super::thread_notification_coordinator::ThreadNotificationCoordinator;
use super::thread_route_view::thread_route_view;
use super::thread_sidebar_width::{
    THREAD_SIDEBAR_DEFAULT_WIDTH, THREAD_SIDEBAR_MIN_WIDTH, clamp_thread_sidebar_width,
    resolve_initial_thread_sidebar_width, resolve_thread_sidebar_maximum_width,
};
use super::workspace_titlebar::workspace_controls_left;
use crate::desktop::desktop_app_settings::{read_desktop_app_settings, write_desktop_app_settings};
use crate::desktop::desktop_application_menu::{CloseWindow, QuitApplication};
use crate::desktop::desktop_client_settings::{
    client_settings_path_for_home, read_client_settings,
};
use crate::desktop::notification_badge::clear_notification_badge_on_focus;
use crate::desktop::quit_hold::{
    QUIT_HOLD_CLOCK_MS, QuitHoldKey, QuitHoldKeyInput, QuitHoldKeyPhase, QuitShortcutCallbacks,
    QuitShortcutEffect, QuitShortcutHandler, QuitShortcutHint,
};
use crate::desktop::window_bounds_persist::{
    WindowBoundsPersist, debounce_delay, geometry_document,
};
use crate::hooks::use_preview_panel_inline_size::{
    PREVIEW_PANEL_DEFAULT_WIDTH, clamp_preview_panel_width, get_preview_panel_max_width,
};
use crate::shell::app_route::{AppRoute, NavigationStack, route_from_draft};
use crate::shell::chat_route_shortcuts::{OpenCommandPalette, ReopenClosedView};
use crate::shell::environment_coordinator::poll_environment_notifications;
use crate::shell::reopen_closed_view::{ReopenOwnerState, closed_view_entries_from_store};
use crate::shell::thread_routes::{ThreadRouteRenderState, ThreadRouteTarget};
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
use turin_contracts::settings::{ClientSettings, QuitConfirmationMode};

actions!(
    turin,
    [
        /// `sidebar.toggle` (mod+b).
        ToggleSidebar,
        /// `rightPanel.toggle` (mod+alt+b).
        ToggleRightPanel,
        /// ⌘Q key-down (quit-hold handler).
        QuitShortcut,
    ]
);

pub const KEY_CONTEXT: &str = "AppShell";

/// Upstream default keybindings (packages/shared/src/keybindings.ts) for the
/// shell's actions.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleSidebar, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleRightPanel, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-q", QuitShortcut, Some(KEY_CONTEXT)),
    ]);
    thread_sidebar::bind_keys(cx, KEY_CONTEXT);
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
    navigation: NavigationStack,
    notifications: ThreadNotificationCoordinator,
    quit_handler: QuitShortcutHandler,
    quit_clock_start: std::time::Instant,
    quit_clock_generation: u64,
    quit_wake_generation: u64,
    quit_overlay: Entity<QuitHoldOverlay>,
    confirm_dialog: Entity<ConfirmDialogHost>,
    client_settings: ClientSettings,
    client_settings_path: std::path::PathBuf,
    bounds_persist: WindowBoundsPersist,
    right_panel_store: RightPanelStore,
    sidebar: Entity<Sidebar>,
    store: Entity<SidebarStore>,
    _appearance: Subscription,
    _sidebar_events: Subscription,
    _bounds: Subscription,
    _store_watch: Subscription,
    _activation: Subscription,
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
        let quit_overlay = cx.new(|_| QuitHoldOverlay::new());
        let confirm_dialog = cx.new(|cx| ConfirmDialogHost::new(window, cx));
        let home = turin_runtime::config::home_directory().unwrap_or_default();
        let client_settings_path = client_settings_path_for_home(&home);
        let client_settings = ClientSettings::default();
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
        let mut layout = Self {
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
            navigation: NavigationStack::default(),
            notifications: ThreadNotificationCoordinator::new(),
            quit_handler: QuitShortcutHandler::new(cfg!(target_os = "macos")),
            quit_clock_start: std::time::Instant::now(),
            quit_clock_generation: 0,
            quit_wake_generation: 0,
            quit_overlay,
            confirm_dialog,
            client_settings,
            client_settings_path: client_settings_path.clone(),
            bounds_persist,
            right_panel_store: RightPanelStore::default(),
            sidebar,
            store,
            _appearance: appearance,
            _sidebar_events: sidebar_events,
            _bounds: bounds,
            _store_watch: store_watch,
            _activation: activation,
        };
        layout.sync_notifications(cx);
        Self::watch_client_settings(client_settings_path, cx);
        layout
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

    fn watch_client_settings(path: std::path::PathBuf, cx: &mut Context<Self>) {
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
            while turin_runtime::support::file_watch::next_debounced(
                &mut rx,
                turin_runtime::support::file_watch::WATCH_DEBOUNCE,
            )
            .await
            .is_some()
            {
                let path = path.clone();
                let settings = cx
                    .background_executor()
                    .spawn(async move { read_client_settings(&path).ok().flatten() })
                    .await;
                if let Some(settings) = settings {
                    this.update(cx, |this, cx| this.apply_client_settings(settings, cx))
                        .ok();
                }
            }
            drop(watcher);
        })
        .detach();
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
            SidebarEvent::Navigate(maybe_route) => {
                if let Some(sidebar_route) = maybe_route {
                    self.navigation.replace(match sidebar_route {
                        SidebarRoute::Thread(key) => {
                            if let Some((environment_id, thread_id)) =
                                crate::state::models::parse_scoped_thread_key(key)
                            {
                                AppRoute::ChatThread {
                                    target: ThreadRouteTarget::Server {
                                        thread_ref: turin_contracts::environment::ScopedThreadRef {
                                            environment_id,
                                            thread_id,
                                        },
                                    },
                                }
                            } else {
                                AppRoute::ChatIndex
                            }
                        }
                        SidebarRoute::Draft(draft_id) => route_from_draft(draft_id),
                    });
                } else {
                    self.navigation.replace(AppRoute::ChatIndex);
                }
                cx.notify();
            }
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
        // Menu Quit is the OS quit role: it always quits. Hold, double-press,
        // and direct apply only to the ⌘Q accelerator.
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
        let _ = window;
        crate::shell::chat_route_shortcuts::open_command_palette_if_closed();
        cx.notify();
    }

    fn on_reopen_closed_view(
        &mut self,
        _: &ReopenClosedView,
        window: &mut Window,
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
        let _ = window;
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
        let now_ms = self.quit_clock_start.elapsed().as_millis() as u64;
        self.quit_handler.set_now_ms(now_ms);
        self.quit_overlay
            .update(cx, |overlay, cx| overlay.tick(now_ms, cx));
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
        let right_panel_open = self.right_panel_open;
        let maximized = right_panel_open && self.right_panel_maximized;
        let resizing = self.resizing.map(|drag| drag.target);
        let routed_thread = self.routed_thread(cx);
        let route_target = match self.navigation.current() {
            AppRoute::ChatThread { target } => Some(target),
            _ => None,
        };
        let render_state = if route_target.is_some() {
            ThreadRouteRenderState::Ready
        } else {
            ThreadRouteRenderState::Loading
        };

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
                            .when(!maximized, |this| match self.main_content.clone() {
                                Some(content) => this.child(content),
                                None => this.child(
                                    route_target
                                        .as_ref()
                                        .map(|target| {
                                            thread_route_view(
                                                target,
                                                render_state,
                                                sidebar_open,
                                                right_panel_open,
                                                routed_thread.clone(),
                                            )
                                        })
                                        .unwrap_or_else(|| {
                                            thread_route_view(
                                                &ThreadRouteTarget::Draft {
                                                    draft_id: String::new(),
                                                },
                                                ThreadRouteRenderState::Loading,
                                                sidebar_open,
                                                right_panel_open,
                                                routed_thread.clone(),
                                            )
                                        }),
                                ),
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
            .child(layout_controls)
            .children(self.overlay.clone())
            .child(self.quit_overlay.clone())
            .child(self.confirm_dialog.clone())
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
