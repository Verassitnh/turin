//! The settings window content: sidebar nav + routed page (the `/settings`
//! route of upstream/apps/web/src/routes/settings.tsx with
//! AppSidebarLayout's settings branch).
//!
//! One view owns the UI state of every page (open menus, text inputs,
//! search) and renders the active page from the [`SettingsViewModel`].
//!
//! Port of upstream/apps/web/src/routes/settings.tsx.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, WeakEntity, Window, div, px, rems,
};
use serde_json::Value;

use super::controls::MenuState;
use super::layout::{SettingsMetrics, SettingsSearchTarget};
use super::search::SettingsPath;
use super::view_model::{SettingsStore, SettingsViewModel};
use crate::components::drag_region::DragRegion as _;
use crate::components::sidebar::sidebar_chrome::{SidebarChromeFooter, SidebarChromeHeader};
use crate::components::thread_sidebar_width::THREAD_SIDEBAR_DEFAULT_WIDTH;
use crate::components::workspace_breadcrumb::{
    WorkspaceBreadcrumb, WorkspaceBreadcrumbItem, WorkspaceBreadcrumbSeparator,
    workspace_breadcrumb_text,
};
use crate::components::workspace_titlebar::{
    workspace_controls_left, workspace_titlebar_content_left,
};
use crate::settings_logic::settings_scope::SettingsScopeSearch;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, WORKSPACE_TOPBAR_HEIGHT};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

/// `/settings/<section>` plus the General page's detail page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRoute {
    Section(SettingsPath),
    OpenSourceLicenses,
}

impl SettingsRoute {
    pub fn pathname(self) -> &'static str {
        match self {
            Self::Section(path) => path.path(),
            Self::OpenSourceLicenses => "/settings/open-source-licenses",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "open-source-licenses" | "licenses" => Some(Self::OpenSourceLicenses),
            other => SettingsPath::from_path(&format!("/settings/{other}")).map(Self::Section),
        }
    }

    /// `SETTINGS_BREADCRUMB_LABELS`.
    pub fn breadcrumb_label(self) -> &'static str {
        match self {
            Self::Section(path) => path.label(),
            Self::OpenSourceLicenses => "Open source licenses",
        }
    }
}

/// Which settings file a row's key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Client,
    Server,
}

type InputCommit = Rc<dyn Fn(&mut SettingsView, String, &mut Window, &mut Context<SettingsView>)>;
type InputSubmit = Rc<dyn Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>)>;

/// Placeholder of a saved secret field (`SecretInput` with `configured`).
pub(super) const STORED_SECRET_PLACEHOLDER: &str = "Stored secret, enter a new value to replace";

/// How an input commits and renders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputKind {
    /// Commits on Enter and blur (`DraftInput`).
    Draft,
    /// Commits on every change as well (`DraftInput` with `live`).
    Live,
    /// A masked draft that commits like [`InputKind::Draft`] (`SecretInput`).
    Secret,
    /// A masked input that also commits on every change.
    SecretLive,
}

fn input_kind_is_masked(kind: InputKind) -> bool {
    matches!(kind, InputKind::Secret | InputKind::SecretLive)
}

/// `toastManager.add({ type, title, description })`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsToast {
    pub kind: SettingsToastKind,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsToastKind {
    Success,
    Warning,
    Error,
}

pub struct SettingsView {
    pub(super) store: Entity<SettingsStore>,
    pub(super) route: SettingsRoute,
    /// The selected scope (`SettingsScopeSearch`: project, machine, checkout).
    pub(super) scope_search: SettingsScopeSearch,
    /// The resolved scope for one view model and selection ([`Self::scope_context`]).
    pub(super) scope_cache:
        std::cell::RefCell<Option<(usize, SettingsScopeSearch, Rc<super::scope::ScopeContext>)>>,
    pub(super) menus: MenuState,
    pub(super) inputs: HashMap<SharedString, Entity<InputState>>,
    input_commits: HashMap<SharedString, InputCommit>,
    /// Handlers run when Enter is pressed in an input (a form's `onSubmit`).
    input_submits: HashMap<SharedString, InputSubmit>,
    /// The placeholder each input was last given, so it is only pushed on change.
    input_placeholders: HashMap<SharedString, SharedString>,
    pub(super) textareas: HashMap<SharedString, Entity<TextareaState>>,
    pub(super) nav_search: Entity<InputState>,
    pub(super) nav_query: String,
    /// `activeResultIndex`.
    pub(super) nav_active_result: usize,
    pub(super) sidebar_open: bool,
    pub(super) panels: super::panels::PanelState,
    pub(super) dialog: Option<super::dialog::Dialog>,
    pub(super) slider_bounds:
        HashMap<SharedString, Rc<std::cell::Cell<gpui_kit::Bounds<gpui_kit::Pixels>>>>,
    pub(super) slider_drag: Option<SharedString>,
    /// Focus target of the keybinding recorder.
    pub(super) capture_focus: gpui_kit::FocusHandle,
    /// Toasts queued by actions that have no window at hand (`toastManager.add`);
    /// the next render pushes them.
    toasts: std::cell::RefCell<Vec<SettingsToast>>,
    /// `ThemeEditorHost`: the floating theme editor the Appearance page opens.
    theme_editor_host: Entity<super::theme_ui::theme_editor_host::ThemeEditorHost>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(store: Entity<SettingsStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let nav_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let theme_editor_host =
            cx.new(|cx| super::theme_ui::theme_editor_host::ThemeEditorHost::new(window, cx));
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &nav_search,
                window,
                |this, state, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.nav_query = state.read(cx).value().to_string();
                        this.nav_active_result = 0;
                        cx.notify();
                    }
                },
            ),
        ];
        Self {
            store,
            route: SettingsRoute::Section(SettingsPath::General),
            scope_search: SettingsScopeSearch::default(),
            scope_cache: std::cell::RefCell::new(None),
            menus: MenuState::default(),
            inputs: HashMap::new(),
            input_commits: HashMap::new(),
            input_submits: HashMap::new(),
            input_placeholders: HashMap::new(),
            textareas: HashMap::new(),
            nav_search,
            nav_query: String::new(),
            nav_active_result: 0,
            sidebar_open: true,
            panels: Default::default(),
            dialog: None,
            slider_bounds: HashMap::new(),
            slider_drag: None,
            capture_focus: cx.focus_handle(),
            toasts: std::cell::RefCell::new(Vec::new()),
            theme_editor_host,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_route(&mut self, route: SettingsRoute, cx: &mut Context<Self>) {
        self.route = route;
        self.panels
            .page_scroll
            .set_offset(gpui_kit::point(px(0.), px(0.)));
        self.menus.close();
        cx.notify();
    }

    /// Queues a toast; the next render shows it.
    pub(super) fn toast(
        &self,
        kind: SettingsToastKind,
        title: impl Into<String>,
        description: impl Into<String>,
        cx: &mut App,
    ) {
        self.toasts.borrow_mut().push(SettingsToast {
            kind,
            title: title.into(),
            description: description.into(),
        });
        // The view observes the store, so poking it re-renders the view.
        self.store.update(cx, |_, cx| cx.notify());
    }

    pub fn vm(&self, cx: &App) -> Rc<SettingsViewModel> {
        self.store.read(cx).vm().clone()
    }

    // ── dev hooks (settings/demo.rs) ────────────────────────────────────

    pub(super) fn open_menu_for_demo(&mut self, id: String, cx: &mut Context<Self>) {
        match id.as_str() {
            // The first keybinding row's `when` builder.
            "when-first" => {
                if let Some(row) = self.first_keybinding_row_id(cx) {
                    self.menus.open(format!("when-row:{row}"));
                }
            }
            "kb-new" => self.panels.keybindings.adding = true,
            other if other.starts_with("scroll:") => {
                if let Ok(offset) = other["scroll:".len()..].parse::<f32>() {
                    self.panels.demo_scroll = Some((offset, 30));
                }
            }
            "add-provider" => self.open_add_provider(cx),
            other if other.starts_with("scope:") => {
                let wanted = &other["scope:".len()..];
                let groups = super::scope::settings_project_groups(&self.vm(cx));
                self.scope_search.project = groups
                    .iter()
                    .find(|group| wanted == "first" || group.project_key == wanted)
                    .map(|group| group.project_key.clone())
                    .or_else(|| Some(wanted.to_owned()));
            }
            "bg-dialog" => self.dialog = Some(super::dialog::Dialog::BackgroundActivity),
            other if other.starts_with("editor-scroll:") => {
                if let Ok(offset) = other["editor-scroll:".len()..].parse::<f32>() {
                    self.panels.providers.demo_editor_scroll = Some((offset, 30));
                }
            }
            "add-hub" => self.dialog = Some(super::dialog::Dialog::AddHub),
            other if other.starts_with("expand:") => {
                self.panels
                    .source_control
                    .expanded
                    .insert(other["expand:".len()..].to_owned());
            }
            other if other.starts_with("provider:") => {
                let target = other["provider:".len()..].to_owned();
                self.panels.providers.selected = Some(target.clone());
                self.panels.providers.target = Some(target);
            }
            _ => self.menus.open(id),
        }
        cx.notify();
    }

    pub(super) fn set_nav_query_for_demo(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nav_query = query.clone();
        self.nav_search
            .update(cx, |state, cx| state.set_value(query, window, cx));
        cx.notify();
    }

    // ── patching ────────────────────────────────────────────────────────

    pub(super) fn update_client(&self, patch: Value, cx: &mut App) {
        let source = self.store.read(cx).source();
        source.update_client(patch, cx);
    }

    pub(super) fn update_server(&self, patch: Value, cx: &mut App) {
        let source = self.store.read(cx).source();
        source.update_server(patch, cx);
    }

    /// `useUpdateScopedSettings`: a server patch at a project scope writes
    /// project overrides; anywhere else it writes the environment.
    pub(super) fn update(&self, which: Which, patch: Value, cx: &mut App) {
        match which {
            Which::Client => self.update_client(patch, cx),
            Which::Server => {
                if self.scope_context(cx).is_project_scope() {
                    self.update_scoped(patch, cx);
                } else {
                    self.update_server(patch, cx);
                }
            }
        }
    }

    // ── closures over this view ─────────────────────────────────────────

    /// A `Fn(&mut Window, &mut App)` that runs `f` on this view.
    pub(super) fn cb(
        &self,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl Fn(&mut Window, &mut App) + 'static {
        let entity = cx.entity();
        move |window, cx| {
            entity.update(cx, |this, cx| f(this, window, cx));
        }
    }

    // ── text inputs ─────────────────────────────────────────────────────

    /// A text input whose text is `value` unless the user is editing it; `commit`
    /// runs on Enter or blur (`DraftInput`).
    pub(super) fn input(
        &mut self,
        id: &str,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        self.input_with(id, value, placeholder, InputKind::Draft, window, cx, commit)
    }

    /// Like [`Self::input`], but masked (`type="password"`) with autocomplete off
    /// at the native layer (`ProviderSettingsFormControl::Password`).
    pub(super) fn password_input(
        &mut self,
        id: &str,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        self.input_with(
            id,
            value,
            placeholder,
            InputKind::Secret,
            window,
            cx,
            commit,
        )
    }

    /// Runs `submit` when Enter is pressed in the input `id` (`<form onSubmit>`).
    pub(super) fn on_input_submit(
        &mut self,
        id: &str,
        submit: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.input_submits
            .insert(SharedString::from(id.to_owned()), Rc::new(submit));
    }

    /// A masked input whose `commit` runs on every change.
    pub(super) fn password_input_live(
        &mut self,
        id: &str,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        self.input_with(
            id,
            value,
            placeholder,
            InputKind::SecretLive,
            window,
            cx,
            commit,
        )
    }

    /// Like [`Self::input`] but `commit` also runs on every change.
    pub(super) fn input_live(
        &mut self,
        id: &str,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        self.input_with(id, value, placeholder, InputKind::Live, window, cx, commit)
    }

    /// A multi-line input (`Textarea`, `rows={rows}`); `commit` runs on blur.
    pub(super) fn textarea(
        &mut self,
        id: &str,
        value: &str,
        placeholder: &str,
        rows: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<TextareaState> {
        let key = SharedString::from(id.to_owned());
        self.input_commits.insert(key.clone(), Rc::new(commit));
        if let Some(state) = self.textareas.get(&key) {
            let state = state.clone();
            let focused = state.read(cx).focus_handle(cx).is_focused(window);
            if !focused && state.read(cx).value() != value {
                state.update(cx, |state, cx| {
                    state.set_value(value.to_owned(), window, cx)
                });
            }
            return state;
        }
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(rows)
                .placeholder(placeholder.to_owned())
                .default_value(value.to_owned())
        });
        let subscription_key = key.clone();
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur) {
                    let text = state.read(cx).value().to_string();
                    if let Some(commit) = this.input_commits.get(&subscription_key).cloned() {
                        commit(this, text, window, cx);
                    }
                }
            },
        );
        self._subscriptions.push(subscription);
        self.textareas.insert(key, state.clone());
        state
    }

    /// A masked single-line input with no commit hook (`SecretInput`); callers
    /// read it from [`Self::inputs`] when their button is pressed.
    /// A write-only masked draft that commits only through its form (`SecretInput`
    /// without `onCommit`): the saved value is never read back, so the field starts
    /// empty and `configured` picks the stored-secret hint over "Not set".
    pub(super) fn secret_input(
        &mut self,
        id: &str,
        configured: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let key = SharedString::from(id.to_owned());
        let placeholder = if configured {
            STORED_SECRET_PLACEHOLDER
        } else {
            "Not set"
        };
        if let Some(state) = self.inputs.get(&key).cloned() {
            self.follow_placeholder(&key, &state, placeholder, window, cx);
            return state;
        }
        self.input_placeholders
            .insert(key.clone(), SharedString::from(placeholder.to_owned()));
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(placeholder.to_owned())
        });
        self.inputs.insert(key, state.clone());
        state
    }

    /// Pushes `placeholder` to `state` only when it differs from the last one given,
    /// since a placeholder the caller changes (a saved secret) follows the caller.
    fn follow_placeholder(
        &mut self,
        key: &SharedString,
        state: &Entity<InputState>,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_placeholders.get(key).map(SharedString::as_str) != Some(placeholder) {
            state.update(cx, |state, cx| {
                state.set_placeholder(placeholder.to_owned(), window, cx)
            });
            self.input_placeholders
                .insert(key.clone(), SharedString::from(placeholder.to_owned()));
        }
    }

    fn input_with(
        &mut self,
        id: &str,
        value: &str,
        placeholder: &str,
        kind: InputKind,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        let key = SharedString::from(id.to_owned());
        self.input_commits.insert(key.clone(), Rc::new(commit));
        if let Some(state) = self.inputs.get(&key) {
            let state = state.clone();
            // The placeholder follows the caller, as an upstream prop does (a saved
            // secret swaps "Not set" for the stored-secret hint after save).
            self.follow_placeholder(&key, &state, placeholder, window, cx);
            let masked = input_kind_is_masked(kind);
            if state.read(cx).presentation().is_masked() != masked {
                state.update(cx, |state, cx| state.set_masked(masked, window, cx));
            }
            let focused = state.read(cx).focus_handle(cx).is_focused(window);
            if !focused && state.read(cx).value() != value {
                state.update(cx, |state, cx| {
                    state.set_value(value.to_owned(), window, cx)
                });
            }
            return state;
        }
        self.input_placeholders
            .insert(key.clone(), SharedString::from(placeholder.to_owned()));
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder.to_owned())
                .masked(input_kind_is_masked(kind))
                .default_value(value.to_owned())
        });
        let subscription_key = key.clone();
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur)
                    || (matches!(kind, InputKind::Live | InputKind::SecretLive)
                        && matches!(event, InputEvent::Change))
                {
                    let text = state.read(cx).value().to_string();
                    if let Some(commit) = this.input_commits.get(&subscription_key).cloned() {
                        commit(this, text, window, cx);
                    }
                }
                if matches!(event, InputEvent::PressEnter { .. })
                    && let Some(submit) = this.input_submits.get(&subscription_key).cloned()
                {
                    submit(this, window, cx);
                }
            },
        );
        self._subscriptions.push(subscription);
        self.inputs.insert(key, state.clone());
        state
    }

    /// A masked input with a commit hook (`SecretInput` on `DraftInput`). While
    /// `configured`, the field shows the stored-secret hint and a blank commit is
    /// ignored, so blurring an untouched saved field never clears it. Callers pass
    /// `value` as "" for a saved secret: the app never holds the plaintext back.
    pub(super) fn secret_commit_input(
        &mut self,
        id: &str,
        value: &str,
        placeholder: &str,
        configured: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        let placeholder = if configured {
            STORED_SECRET_PLACEHOLDER
        } else {
            placeholder
        };
        self.input_with(
            id,
            value,
            placeholder,
            InputKind::Secret,
            window,
            cx,
            move |this, text, window, cx| {
                if configured && text.is_empty() {
                    return;
                }
                commit(this, text, window, cx);
            },
        )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::notification::Notification;
        for failure in self.store.read(cx).take_failures() {
            self.toasts.borrow_mut().push(SettingsToast {
                kind: SettingsToastKind::Error,
                title: failure.title,
                description: failure.description,
            });
        }
        for toast in std::mem::take(&mut *self.toasts.borrow_mut()) {
            let notification = match toast.kind {
                SettingsToastKind::Success => Notification::success(toast.description),
                SettingsToastKind::Warning => Notification::warning(toast.description),
                SettingsToastKind::Error => Notification::error(toast.description),
            };
            window.push_notification(notification.title(toast.title), cx);
        }
        self.provider_auth_callback_coordinator(cx);
        let theme = cx.app_theme().clone();
        let viewport_width = f32::from(window.viewport_size().width);
        let sidebar_width = THREAD_SIDEBAR_DEFAULT_WIDTH;
        let main_width = if self.sidebar_open {
            viewport_width - sidebar_width
        } else {
            viewport_width
        };
        // `max-w-4xl`, `px-6` each side, row `px-4` each side.
        let content_width = main_width.min(896.) - 48.;
        cx.set_global(SettingsMetrics {
            row_width: content_width - 32.,
        });

        let route = self.route;
        let header_left = if self.sidebar_open {
            px(20.)
        } else {
            workspace_titlebar_content_left(window)
        };
        let breadcrumb = WorkspaceBreadcrumb::new()
            .child(WorkspaceBreadcrumbItem::new(false).child(workspace_breadcrumb_text("Settings")))
            .child(WorkspaceBreadcrumbSeparator)
            .child(
                WorkspaceBreadcrumbItem::new(true)
                    .child(workspace_breadcrumb_text(route.breadcrumb_label())),
            );
        let restore = (route == SettingsRoute::Section(SettingsPath::General)).then(|| {
            let changed = self.changed_setting_labels(cx);
            Button::new("restore-device-defaults")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Xs)
                .icon(IconName::RotateCcw)
                .label("Restore device defaults")
                .disabled(changed.is_empty())
                .on_click(cx.listener(|this, _, window, cx| this.restore_defaults(window, cx)))
        });

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
                    .icon(if self.sidebar_open {
                        IconName::PanelLeftClose
                    } else {
                        IconName::PanelLeft
                    })
                    .tooltip(Tooltip::new("Toggle main sidebar").shortcut("⌘B"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = !this.sidebar_open;
                        cx.notify();
                    })),
            );

        let page = self.render_page(window, cx);

        div()
            .id("settings-root")
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .overflow_hidden()
            .bg(theme.app_chrome_background)
            .text_color(theme.foreground)
            .font_family(theme.fonts.sans.clone())
            .text_size(rems(1.))
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if this.handle_search_key(event, window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.menus.any_open() {
                        this.menus.close();
                        cx.notify();
                    }
                }),
            )
            .when(self.sidebar_open, |this| {
                this.child(
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .flex_shrink_0()
                        .h_full()
                        .w(px(sidebar_width))
                        .bg(theme.sidebar)
                        .border_r_1()
                        .border_color(theme.sidebar_border)
                        .text_color(theme.sidebar_foreground)
                        .child(SidebarChromeHeader)
                        .child(self.render_nav(window, cx))
                        // The settings window has no app sidebar to route the footer's
                        // events to, so its buttons stay inert here.
                        .child(SidebarChromeFooter::new(WeakEntity::new_invalid())),
                )
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(theme.background)
                    .child(
                        div()
                            .id("settings-header")
                            .relative()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_3()
                            .h(WORKSPACE_TOPBAR_HEIGHT)
                            .pl(header_left)
                            .pr(px(20.))
                            .bg(theme.background)
                            .child(
                                div()
                                    .flex()
                                    .w_full()
                                    .items_center()
                                    .gap_3()
                                    .text_size(TEXT_SM.size)
                                    .child(breadcrumb)
                                    .child(
                                        div()
                                            .ml_auto()
                                            .flex()
                                            .flex_shrink_0()
                                            .items_center()
                                            .children(restore),
                                    ),
                            )
                            .drag_region(window, cx),
                    )
                    .child(div().flex().flex_col().flex_1().min_h_0().child(page)),
            )
            .child(sidebar_toggle)
            .children(self.render_dialog(window, cx))
            // Over everything, like upstream's host above the router (z-110).
            .child(self.theme_editor_host.clone())
    }
}

/// Convenience for panels: a titled, id-anchored section.
pub(super) fn section_title(text: &'static str) -> AnyElement {
    div()
        .font_weight(FontWeight::MEDIUM)
        .child(text)
        .into_any_element()
}

#[allow(dead_code)]
fn _search_target_marker(_: SettingsSearchTarget) {}

#[cfg(test)]
mod tests {
    use super::{InputKind, input_kind_is_masked};

    #[test]
    fn input_mask_follows_kind_on_reuse() {
        assert!(!input_kind_is_masked(InputKind::Draft));
        assert!(!input_kind_is_masked(InputKind::Live));
        assert!(input_kind_is_masked(InputKind::Secret));
        assert!(input_kind_is_masked(InputKind::SecretLive));
    }
}
