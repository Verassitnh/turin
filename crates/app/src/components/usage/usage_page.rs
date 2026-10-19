//! Port of upstream/apps/web/src/components/usage/UsagePage.tsx
//!
//! The Usage route: a titlebar with the environment and provider filters, the
//! metric and period controls and the refresh button, and a scrolling body
//! with the summary column, the daily/hourly chart, totals, the cost/token
//! mixes and the model/time breakdown, or the Limits tab.
//!
//! The page reads everything through [`UsageBackend`]. Differences from
//! upstream: the dropdowns are menus the page positions itself (like the diff
//! panel's), the HTML tables become fixed-width flex rows, and the page
//! shortcuts resolve on the focused page root instead of a capture-phase
//! `window` listener (an exactly focused page is what "not typing in a field"
//! means here).

use crate::theme::color::solid_over;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use chrono::Utc;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Context, CursorStyle, Entity,
    EventEmitter, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Transformation, Window, deferred, div, percentage, px, relative, rems, svg,
};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::usage::{
    USAGE_CONTRACT_VERSION, UsageProviderKind, UsageResolution, UsageSourceStatus,
    UsageSummaryInput,
};
use turin_shared::keybindings::ShortcutEvent;
use turin_shared::usage_format::{
    enumerate_days, enumerate_hour_starts, format_count, format_date_time_short, format_day_short,
    format_hour_short, format_percent, format_tokens, format_usage_contract_mismatch, format_usd,
    make_window,
};
use turin_shared::usage_limits::uses_chatgpt_sharing;
use turin_shared::usage_merge::{
    DailyTotals, HourlyTotals, MergedUsage, ModelTotals, is_compatible_usage_contract_version,
    is_model_cost_unknown,
};

use super::backend::{LimitsEnvironment, UsageBackend};
use super::usage_breakdown::{
    ShareMetric, cost_type_segments, model_share, sort_models_by_tokens, speed_cost_segments,
    token_type_segments,
};
use super::usage_limits::select_limit_environments;
use super::usage_limits_pooled::{LimitsProps, UsageLimitsPooled};
use super::usage_model_dialog::{UsageChartWindow, UsageModelDialog, speed_premium};
use super::usage_page_preferences::{
    UsagePageMetric, UsagePagePreferences, WINDOW_DAYS, read_usage_page_preferences,
    save_usage_page_preferences,
};
use super::usage_price_overrides::UsagePriceOverrides;
use super::usage_provider_chart::{UsageChartMetric, UsageProviderChart, UsageProviderChartProps};
use super::usage_providers::{PROVIDER_ORDER, presentation, provider_label, providers_with_usage};
use super::usage_share_bar::UsageShareBar;
use super::usage_shortcuts::{
    METRIC_OPTIONS, ShortcutMatchContext, WINDOW_OPTIONS, resolve_usage_shortcut,
    shortcut_label_for_command,
};
use super::usage_ui::{
    figure_opacity, inline_button, menu_checkbox_item, menu_item, menu_separator, rich_tooltip,
};
use super::view_model::{
    EnvironmentSelection, EnvironmentUsageStatus, UsageView, UsageViewMemory, compute_usage_view,
};
use crate::components::drag_region::{DragRegion as _, no_drag};
use crate::components::provider_instance_icon::provider_instance_icon;
use crate::components::workspace_breadcrumb::{
    WorkspaceBreadcrumb, WorkspaceBreadcrumbItem, WorkspaceBreadcrumbSeparator,
    workspace_breadcrumb_text,
};
use crate::components::workspace_titlebar::workspace_titlebar_content_left;
use crate::diff::menu::menu_surface;
use crate::hooks::use_local_storage::SharedLocalStorage;
use crate::state::helpers::usage::cursor_keychain_access_environments;
use crate::state::helpers::usage_progress::{
    UsageEnvironmentProgress, updating_providers_label, usage_environment_progress,
    usage_loading_state,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_2XS, TEXT_SM, TEXT_XS, WORKSPACE_TOPBAR_HEIGHT};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::dialog::{DialogOptions, open_dialog};
use crate::ui::skeleton::{Skeleton, SkeletonShape};
use crate::ui::toggle::{Toggle, ToggleSize, ToggleVariant};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

pub enum UsagePageEvent {
    /// Escape: back to where the user came from.
    Back,
}

impl EventEmitter<UsagePageEvent> for UsagePage {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Breakdown {
    Model,
    Time,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenMenu {
    Environment,
    Provider,
    Metric,
    Period,
}

/// `xl:` (1280px) and `2xl:` (1536px) breakpoints of the header.
const XL: f32 = 1280.;
const XXL: f32 = 1536.;
/// `max-w-5xl`, the `wide` page container.
const PAGE_MAX_WIDTH: f32 = 64.;

pub struct UsagePage {
    backend: Rc<dyn UsageBackend>,
    storage: SharedLocalStorage,
    preferences: UsagePagePreferences,
    window_days: u32,
    window_input: UsageSummaryInput,
    /// Epoch ms the Limits views read as "now"; advanced on refresh.
    limits_now: f64,
    refreshing: bool,
    breakdown: Breakdown,
    selected_environment_ids: EnvironmentSelection,
    memory: UsageViewMemory,
    menu: Option<OpenMenu>,
    limits: Entity<UsageLimitsPooled>,
    focus_handle: FocusHandle,
    /// Whether the sidebar is open (the header clears the traffic lights when not).
    pub sidebar_open: bool,
    cursor_enabling: HashSet<EnvironmentId>,
    last_limits_key: Option<String>,
}

fn window_for(days: u32) -> UsageSummaryInput {
    make_window(
        u64::from(days),
        Utc::now(),
        if days == 1 {
            UsageResolution::Hour
        } else {
            UsageResolution::Day
        },
    )
}

fn now_ms() -> f64 {
    Utc::now().timestamp_millis() as f64
}

impl UsagePage {
    pub fn new(
        backend: Rc<dyn UsageBackend>,
        storage: SharedLocalStorage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = read_usage_page_preferences(storage.as_ref());
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let limits = cx.new({
            let backend = backend.clone();
            move |_| {
                UsageLimitsPooled::new(
                    backend.clone(),
                    LimitsProps {
                        environments: Vec::new(),
                        now: now_ms(),
                        timestamp_format: backend.timestamp_format(),
                        cursor_prompt: None,
                    },
                )
            }
        });
        let window_days = preferences.window_days;
        Self {
            window_input: window_for(window_days),
            window_days,
            backend,
            storage,
            preferences,
            limits_now: now_ms(),
            refreshing: false,
            breakdown: Breakdown::Model,
            selected_environment_ids: None,
            memory: UsageViewMemory::default(),
            menu: None,
            limits,
            focus_handle,
            sidebar_open: true,
            cursor_enabling: HashSet::new(),
            last_limits_key: None,
        }
    }

    /// Dev override for screenshots (`TURIN_USAGE_TAB`, `TURIN_USAGE_WINDOW`).
    pub fn apply_dev_overrides(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(tab) = std::env::var("TURIN_USAGE_TAB") {
            self.preferences.metric = match tab.as_str() {
                "cost" => UsagePageMetric::Cost,
                "tokens" => UsagePageMetric::Tokens,
                _ => UsagePageMetric::Limits,
            };
        }
        if let Some(days) = std::env::var("TURIN_USAGE_WINDOW")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|days| WINDOW_DAYS.contains(days))
        {
            self.window_days = days;
            self.window_input = window_for(days);
        }
        if std::env::var("TURIN_USAGE_BREAKDOWN").is_ok_and(|value| value == "time") {
            self.breakdown = Breakdown::Time;
        }
        match std::env::var("TURIN_USAGE_OPEN").as_deref() {
            Ok("env-menu") => self.menu = Some(OpenMenu::Environment),
            Ok("provider-menu") => self.menu = Some(OpenMenu::Provider),
            Ok("metric-menu") => self.menu = Some(OpenMenu::Metric),
            Ok("period-menu") => self.menu = Some(OpenMenu::Period),
            Ok(open @ ("model-dialog" | "price-dialog" | "limits-popup")) => {
                // Deferred, not timed: a window that is not on screen stops
                // redrawing, so the overlay has to be up before the first frame.
                let open = open.to_owned();
                cx.defer_in(window, move |this, window, cx| match open.as_str() {
                    "model-dialog" => this.demo_model_dialog(window, cx),
                    "price-dialog" => {
                        this.open_price_dialog(Some("deepseek-v4-pro".into()), window, cx)
                    }
                    _ => this
                        .limits
                        .update(cx, |limits, cx| limits.demo_open_first(cx)),
                });
            }
            _ => {}
        }
        cx.notify();
    }

    fn demo_model_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let environments = self.backend.usage_environments(&self.window_input);
        let merged = super::view_model::merge_answered_usage(&environments, None);
        let Some(model) = merged.models.iter().find(|m| m.model == "gpt-5.5").cloned() else {
            return;
        };
        let w = &self.window_input;
        let chart_window = UsageChartWindow {
            days: enumerate_days(w.since_day.as_str(), w.until_day.as_str()),
            hours: Vec::new(),
            resolution: UsageResolution::Day,
            time_zone: w.time_zone.as_str().to_owned(),
            reference_time: None,
        };
        let metric = if self.preferences.metric == UsagePageMetric::Tokens {
            UsageChartMetric::Tokens
        } else {
            UsageChartMetric::Cost
        };
        self.open_model_dialog(model, chart_window, metric, environments, window, cx);
    }

    fn hidden_providers(&self) -> HashSet<UsageProviderKind> {
        self.preferences
            .hidden_providers
            .iter()
            .flatten()
            .copied()
            .collect()
    }

    fn update_preferences(&mut self, edit: impl FnOnce(&mut UsagePagePreferences)) {
        edit(&mut self.preferences);
        save_usage_page_preferences(self.storage.as_ref(), &self.preferences);
    }

    fn select_window(&mut self, days: u32, cx: &mut Context<Self>) {
        if !WINDOW_DAYS.contains(&days) {
            return;
        }
        self.update_preferences(|preferences| preferences.window_days = days);
        self.window_days = days;
        self.window_input = window_for(days);
        cx.notify();
    }

    fn select_metric(&mut self, metric: UsagePageMetric, cx: &mut Context<Self>) {
        if metric == UsagePageMetric::Limits {
            self.limits_now = now_ms();
        }
        self.update_preferences(|preferences| preferences.metric = metric);
        cx.notify();
    }

    fn connected_limits_environments(&self) -> Vec<EnvironmentId> {
        let mut ids: Vec<EnvironmentId> = self
            .backend
            .limits_environments()
            .into_iter()
            .filter(|environment| {
                environment.connected
                    && environment.has_server_config
                    && self
                        .selected_environment_ids
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&environment.environment_id))
            })
            .map(|environment| environment.environment_id)
            .collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    fn refresh_limits(
        &mut self,
        automatic: bool,
        after_pending: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Task<()> {
        let tasks: Vec<_> = self
            .connected_limits_environments()
            .into_iter()
            .map(|id| {
                self.backend
                    .refresh_limits(id, automatic, after_pending, cx)
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            for task in tasks {
                task.await;
            }
            this.update_in(cx, |this, _, cx| {
                this.limits_now = now_ms();
                cx.notify();
            })
            .ok();
        })
    }

    fn refresh_usage_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let environments: Vec<EnvironmentId> = self
            .backend
            .usage_environments(&self.window_input)
            .into_iter()
            .filter(|environment| {
                environment.can_read_diagnostics
                    && self
                        .selected_environment_ids
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&environment.environment_id))
            })
            .map(|environment| environment.environment_id)
            .collect();
        let task = self
            .backend
            .refresh_usage(environments, self.window_input.clone(), cx);
        cx.spawn_in(window, async move |this, cx| {
            task.await;
            this.update_in(cx, |this, _, cx| {
                this.refreshing = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        if self.preferences.metric == UsagePageMetric::Limits {
            self.refreshing = true;
            cx.notify();
            let task = self.refresh_limits(false, false, window, cx);
            cx.spawn_in(window, async move |this, cx| {
                task.await;
                this.update_in(cx, |this, _, cx| {
                    this.refreshing = false;
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        let next = window_for(self.window_days);
        if next.since_day != self.window_input.since_day
            || next.until_day != self.window_input.until_day
            || next.since_time != self.window_input.since_time
            || next.until_time != self.window_input.until_time
        {
            self.window_input = next;
        }
        self.refreshing = true;
        cx.notify();
        self.refresh_usage_window(window, cx);
    }

    fn after_cursor_enabled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_usage_window(window, cx);
        self.refresh_limits(false, true, window, cx).detach();
    }

    fn enable_cursor(
        &mut self,
        environment_id: EnvironmentId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cursor_enabling.insert(environment_id.clone()) {
            return;
        }
        cx.notify();
        let task = self
            .backend
            .enable_cursor_keychain(environment_id.clone(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let enabled = task.await;
            this.update_in(cx, |this, window, cx| {
                this.cursor_enabling.remove(&environment_id);
                if enabled {
                    this.after_cursor_enabled(window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.is_held {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" {
            cx.emit(UsagePageEvent::Back);
            cx.stop_propagation();
            return;
        }
        // The page root itself holds focus: no field or popup is taking the keys.
        if !self.focus_handle.is_focused(window) {
            return;
        }
        let code = keystroke
            .key
            .chars()
            .next()
            .filter(|c| c.is_ascii_digit() && keystroke.key.len() == 1)
            .map(|digit| format!("Digit{digit}"));
        let shortcut = ShortcutEvent {
            key: keystroke.key.clone(),
            code,
            alt_graph: false,
            meta_key: keystroke.modifiers.platform,
            ctrl_key: keystroke.modifiers.control,
            shift_key: keystroke.modifiers.shift,
            alt_key: keystroke.modifiers.alt,
        };
        let keybindings = self.backend.keybindings();
        let platform = if cfg!(target_os = "macos") {
            "MacIntel"
        } else {
            "Linux"
        };
        let Some(command) = resolve_usage_shortcut(&shortcut, None, &keybindings, platform) else {
            return;
        };
        let metric = METRIC_OPTIONS
            .iter()
            .find(|option| option.command == command);
        let period = WINDOW_OPTIONS
            .iter()
            .find(|option| option.command == command);
        if metric.is_none() && period.is_none() {
            return;
        }
        cx.stop_propagation();
        if let Some(option) = metric {
            self.select_metric(option.value, cx);
        }
        if let Some(option) = period
            && self.preferences.metric != UsagePageMetric::Limits
        {
            self.select_window(option.days, cx);
        }
    }

    fn shortcut_title(&self, label: &str, command: &str) -> String {
        let keybindings = self.backend.keybindings();
        let platform = if cfg!(target_os = "macos") {
            "MacIntel"
        } else {
            "Linux"
        };
        let context = ShortcutMatchContext::with(&[("usagePageOpen", true)]);
        match shortcut_label_for_command(&keybindings, command, platform, &context) {
            Some(shortcut) => format!("{label} ({shortcut})"),
            None => label.to_owned(),
        }
    }

    fn open_price_dialog(
        &mut self,
        model: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backend = self.backend.clone();
        let usage = self.backend.usage_environments(&self.window_input);
        let selection = self.selected_environment_ids.clone();
        let view = cx.new(|_| UsagePriceOverrides::new(backend, usage, selection, model));
        open_dialog(
            window,
            cx,
            DialogOptions {
                max_width: rems(56.),
                ..Default::default()
            },
            view,
        );
    }

    fn open_model_dialog(
        &mut self,
        model: ModelTotals,
        chart_window: UsageChartWindow,
        metric: UsageChartMetric,
        environments: Vec<EnvironmentUsageStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = cx.entity().downgrade();
        let model_name = model.model.clone();
        let view = cx.new(|_| UsageModelDialog {
            model,
            environments,
            metric,
            chart_window,
            on_set_price: Rc::new(move |window, cx| {
                use gpui_kit::component::WindowExt as _;
                window.close_dialog(cx);
                let model_name = model_name.clone();
                page.update(cx, |this, cx| {
                    this.open_price_dialog(Some(model_name), window, cx)
                })
                .ok();
            }),
        });
        open_dialog(
            window,
            cx,
            DialogOptions {
                max_width: rems(48.),
                ..Default::default()
            },
            view,
        );
    }
}

/// Values the render derives once and the sections share.
struct Derived {
    merged: MergedUsage,
    shown_window: UsageSummaryInput,
    shown_hourly: bool,
    loading_partial: bool,
    loading_every_provider: bool,
    loading_providers: HashSet<UsageProviderKind>,
    providers_with_data: Vec<UsageProviderKind>,
    active_providers: Vec<UsageProviderKind>,
    chart_loading: HashSet<UsageProviderKind>,
    days: Vec<String>,
    hours: Vec<String>,
    breakdown_models: Vec<ModelTotals>,
    breakdown_periods: Vec<PeriodRow>,
    breakdown_peak: f64,
    cursor_access: Vec<EnvironmentUsageStatus>,
    source_messages: Vec<String>,
    can_read_diagnostics: bool,
    window_label: String,
}

struct PeriodRow {
    label: String,
    cost_usd: f64,
    total_tokens: u64,
    by_provider: IndexMap<UsageProviderKind, turin_shared::usage_merge::ProviderSpend>,
}

impl Derived {
    fn provider_loading(&self, provider: UsageProviderKind) -> bool {
        self.loading_every_provider || self.loading_providers.contains(&provider)
    }
}

impl UsagePage {
    fn derive(
        &self,
        view: &UsageView,
        metric: UsagePageMetric,
        hidden: &HashSet<UsageProviderKind>,
    ) -> Derived {
        let merged = view
            .shown
            .as_ref()
            .map_or_else(|| view.merged.clone(), |shown| shown.merged.clone());
        let shown_window = view
            .shown
            .as_ref()
            .map_or_else(|| self.window_input.clone(), |shown| shown.window.clone());
        let shown_hourly = shown_window.resolution == Some(UsageResolution::Hour);
        let showing_limits = metric == UsagePageMetric::Limits;
        let refreshing_usage = self.refreshing && !showing_limits;
        // Usage kept from another window is all old, so every figure stays muted.
        let showing_kept = view
            .shown
            .as_ref()
            .is_some_and(|shown| shown.window != self.window_input);
        let progress: Vec<_> = view
            .selected_environments
            .iter()
            .map(EnvironmentUsageStatus::progress_environment)
            .collect();
        let (loading_partial, loading_every_provider, loading_providers) = if showing_kept {
            (true, true, HashSet::new())
        } else {
            let state = usage_loading_state(&progress, refreshing_usage);
            // A hidden provider still refreshing must not add its row or mute the totals.
            let providers: HashSet<_> = state
                .providers
                .iter()
                .copied()
                .filter(|provider| !hidden.contains(provider))
                .collect();
            (
                state.every_provider || !providers.is_empty(),
                state.every_provider,
                providers,
            )
        };
        let providers_with_data = providers_with_usage(&merged.providers);
        // A provider still refreshing keeps its row and line before its usage lands.
        let active_providers: Vec<UsageProviderKind> = PROVIDER_ORDER
            .iter()
            .copied()
            .filter(|provider| {
                providers_with_data.contains(provider) || loading_providers.contains(provider)
            })
            .collect();
        let chart_loading: HashSet<UsageProviderKind> = active_providers
            .iter()
            .copied()
            .filter(|provider| loading_every_provider || loading_providers.contains(provider))
            .collect();
        let days = enumerate_days(
            shown_window.since_day.as_str(),
            shown_window.until_day.as_str(),
        );
        let hours = match (&shown_window.since_time, &shown_window.until_time) {
            (Some(since), Some(until)) => enumerate_hour_starts(since.as_str(), until.as_str()),
            _ => Vec::new(),
        };
        let breakdown_models =
            if self.breakdown == Breakdown::Model && metric == UsagePageMetric::Tokens {
                sort_models_by_tokens(&merged.models)
            } else {
                merged.models.clone()
            };
        let time_zone = shown_window.time_zone.as_str().to_owned();
        // Newest first: the window can run 90 periods, so the interesting end
        // belongs at the top of the table.
        let breakdown_periods: Vec<PeriodRow> = if shown_hourly {
            merged
                .hourly
                .iter()
                .rev()
                .map(|period: &HourlyTotals| PeriodRow {
                    label: format_hour_short(&period.hour_start, Some(&time_zone))
                        .unwrap_or_else(|_| period.hour_start.clone()),
                    cost_usd: period.cost_usd,
                    total_tokens: period.total_tokens,
                    by_provider: period.by_provider.clone(),
                })
                .collect()
        } else {
            merged
                .daily
                .iter()
                .rev()
                .map(|period: &DailyTotals| PeriodRow {
                    label: format_day_short(&period.day),
                    cost_usd: period.cost_usd,
                    total_tokens: period.total_tokens,
                    by_provider: period.by_provider.clone(),
                })
                .collect()
        };
        let breakdown_peak = breakdown_models.iter().fold(0f64, |peak, model| {
            peak.max(if metric == UsagePageMetric::Tokens {
                model.total_tokens as f64
            } else {
                model.cost_usd
            })
        });
        let cursor_access: Vec<EnvironmentUsageStatus> =
            if hidden.contains(&UsageProviderKind::Cursor) {
                Vec::new()
            } else {
                cursor_keychain_access_environments(&view.selected_environments)
                    .into_iter()
                    .cloned()
                    .collect()
            };
        let mut source_messages: Vec<String> = Vec::new();
        for environment in &view.selected_environments {
            let Some(summary) = &environment.summary else {
                continue;
            };
            for source in &summary.sources {
                let Some(message) = &source.message else {
                    continue;
                };
                if source.action.is_none()
                    && !hidden.contains(&source.fingerprint.provider)
                    && (matches!(
                        source.status,
                        UsageSourceStatus::Partial | UsageSourceStatus::Failed
                    ) || source.fingerprint.provider == UsageProviderKind::Cursor)
                    && !source_messages
                        .iter()
                        .any(|existing| existing == message.as_str())
                {
                    source_messages.push(message.as_str().to_owned());
                }
            }
        }
        let can_read_diagnostics = view
            .selected_environments
            .iter()
            .any(|environment| environment.can_read_diagnostics);
        // Names the period on screen, which is the previous one until the new one answers.
        let window_label = match (
            shown_hourly,
            &shown_window.since_time,
            &shown_window.until_time,
        ) {
            (true, Some(since), Some(until)) => format!(
                "{} to {}",
                format_date_time_short(since.as_str(), Some(&time_zone)).unwrap_or_default(),
                format_date_time_short(until.as_str(), Some(&time_zone)).unwrap_or_default()
            ),
            _ => format!(
                "{} to {}",
                format_day_short(shown_window.since_day.as_str()),
                format_day_short(shown_window.until_day.as_str())
            ),
        };
        Derived {
            merged,
            shown_window,
            shown_hourly,
            loading_partial,
            loading_every_provider,
            loading_providers,
            providers_with_data,
            active_providers,
            chart_loading,
            days,
            hours,
            breakdown_models,
            breakdown_periods,
            breakdown_peak,
            cursor_access,
            source_messages,
            can_read_diagnostics,
            window_label,
        }
    }
}

// ── Pieces ──────────────────────────────────────────────────────────────────

fn h2(text: impl Into<SharedString>, theme: &AppTheme) -> gpui_kit::Div {
    div()
        .text_size(TEXT_SM.size)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.foreground)
        .child(text.into())
}

fn provider_mark(provider: UsageProviderKind, size: f32, cx: &App) -> AnyElement {
    let presentation = presentation(provider);
    provider_instance_icon(
        presentation.driver_kind,
        presentation.label,
        rems(size / 16.),
        None,
        cx,
    )
}

fn metric_figure(label: &str, value: String, loading: bool, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(px(2.))
        .child(
            div()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_size(rems(1.))
                .line_height(rems(1.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .opacity(figure_opacity(loading))
                .child(value),
        )
        .into_any_element()
}

fn refresh_icon(refreshing: bool, color: gpui_kit::Hsla) -> AnyElement {
    let icon = svg()
        .path(IconName::RefreshCw.path())
        .flex_shrink_0()
        .size(rems(0.875))
        .text_color(color);
    if refreshing {
        icon.with_animation(
            "usage-refresh-spin",
            Animation::new(Duration::from_millis(1000)).repeat(),
            |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
    } else {
        icon.into_any_element()
    }
}

/// `ToggleGroup variant="segmented"`: `flex w-fit gap-0.5 rounded-lg bg-input/40 p-0.5`.
fn segmented_group(theme: &AppTheme) -> gpui_kit::Div {
    div()
        .flex()
        .flex_shrink_0()
        .gap(px(2.))
        .rounded(RADIUS_LG)
        .bg(solid_over(theme.input, 0.4, theme.background))
        .p(px(2.))
}

impl Render for UsagePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let viewport_width = f32::from(window.viewport_size().width);
        let metric = self.preferences.metric;
        let showing_limits = metric == UsagePageMetric::Limits;
        let hidden = self.hidden_providers();

        // Reading a window starts its scan, as reading the usage atom does upstream.
        if let Some(load) = self.backend.load_usage(&self.window_input, cx) {
            cx.spawn(async move |this, cx| {
                load.await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        let environments = self.backend.usage_environments(&self.window_input);
        let view = compute_usage_view(
            environments,
            &self.selected_environment_ids,
            &hidden,
            &self.window_input,
            &mut self.memory,
        );
        let derived = self.derive(&view, metric, &hidden);

        // Limits refresh on entering the tab and whenever the connected set changes.
        let limits_key = showing_limits.then(|| {
            self.connected_limits_environments()
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
                .join(",")
        });
        if limits_key != self.last_limits_key {
            self.last_limits_key = limits_key.clone();
            if limits_key.as_deref().is_some_and(|key| !key.is_empty()) {
                self.refresh_limits(true, false, window, cx).detach();
            }
        }

        let header = self.render_header(&view, &derived, viewport_width, window, cx);
        let body = self.render_body(&view, &derived, window, cx);

        div()
            .id("usage-page")
            .key_context("UsagePage")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(header)
            .child(body)
    }
}

const CURSOR_KEYCHAIN_COPY: &str = "Requires access to your Cursor login in macOS Keychain.";

enum SummaryRow<'a> {
    Usage(UsageProviderKind),
    Enable(&'a EnvironmentUsageStatus),
}

impl UsagePage {
    fn toggle_menu(&mut self, menu: OpenMenu, cx: &mut Context<Self>) {
        self.menu = if self.menu == Some(menu) {
            None
        } else {
            Some(menu)
        };
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Menu popup anchored under its trigger, dismissed by an outside press.
    fn menu_popup(
        &self,
        id: &'static str,
        content: gpui_kit::Div,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        deferred(
            menu_surface(cx)
                .id(id)
                .absolute()
                .top(px(30.))
                .left_0()
                .min_w(rems(12.))
                .child(content)
                .on_mouse_down_out(move |_, _, cx| {
                    entity.update(cx, |this, cx| this.close_menu(cx));
                }),
        )
        .with_priority(40)
        .into_any_element()
    }

    fn status_label(&self, environment: &EnvironmentUsageStatus, refreshing_usage: bool) -> String {
        let progress =
            usage_environment_progress(&environment.progress_environment(), refreshing_usage);
        if environment.error.is_some() {
            "Unavailable".to_owned()
        } else if environment.summary.as_ref().is_some_and(|summary| {
            !is_compatible_usage_contract_version(summary.contract_version, USAGE_CONTRACT_VERSION)
        }) {
            "Update required".to_owned()
        } else if !environment.is_connected {
            "Connecting…".to_owned()
        } else {
            match progress {
                UsageEnvironmentProgress::Loading => "Scanning…".to_owned(),
                UsageEnvironmentProgress::Stale => "Refreshing…".to_owned(),
                UsageEnvironmentProgress::Partway { providers } => {
                    updating_providers_label(&providers, |provider| {
                        provider_label(provider).to_owned()
                    })
                }
                _ => "Ready".to_owned(),
            }
        }
    }

    /// Explains failed or incompatible environments and deduplicated transcripts.
    /// Shown inside the environment filter so arriving results do not move the page.
    fn coverage_notice(
        &self,
        selected: &[EnvironmentUsageStatus],
        merged: &MergedUsage,
        theme: &AppTheme,
    ) -> Option<AnyElement> {
        let failed: Vec<&EnvironmentUsageStatus> = selected
            .iter()
            .filter(|environment| environment.error.is_some())
            .collect();
        let incompatible: Vec<(&EnvironmentUsageStatus, _)> = selected
            .iter()
            .filter_map(|environment| {
                merged
                    .contract_mismatches
                    .iter()
                    .find(|mismatch| mismatch.environment_id == environment.environment_id)
                    .map(|mismatch| (environment, mismatch.direction))
            })
            .collect();
        if failed.is_empty() && incompatible.is_empty() && merged.duplicate_sources.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .border_t_1()
                .border_color(theme.border)
                .px_2()
                .py_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .children(failed.iter().map(|environment| {
                    div().child(format!(
                        "{}: {}",
                        environment.label,
                        environment.error.clone().unwrap_or_default()
                    ))
                }))
                .children(incompatible.iter().map(|(environment, direction)| {
                    div().child(format_usage_contract_mismatch(
                        &environment.label,
                        *direction,
                    ))
                }))
                .children((!merged.duplicate_sources.is_empty()).then(|| {
                    div().child(format!(
                        "Counted once across environments sharing a transcript directory: {}",
                        merged.duplicate_sources.join(", ")
                    ))
                }))
                .into_any_element(),
        )
    }

    fn environment_filter(
        &mut self,
        view: &UsageView,
        d: &Derived,
        showing_limits: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let all_selected = self.selected_environment_ids.is_none();
        let selected = &view.selected_environments;
        let label = if all_selected {
            "All environments".to_owned()
        } else if selected.len() == 1 {
            selected[0].label.clone()
        } else {
            format!("{} environments", selected.len())
        };
        let has_issue = selected
            .iter()
            .any(|environment| environment.error.is_some())
            || !d.merged.contract_mismatches.is_empty();
        let show_status = !showing_limits;
        let open = self.menu == Some(OpenMenu::Environment);
        let refreshing_usage = self.refreshing && !showing_limits;
        let group: SharedString = "usage-environment-filter".into();

        let trigger = inline_button("usage-environment", false, &theme)
            .group(group.clone())
            .min_w_0()
            .max_w_full()
            .child(workspace_breadcrumb_text(label))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .size(rems(0.875))
                    .text_color(theme.muted_foreground)
                    .child(if show_status && has_issue {
                        Icon::new(IconName::CircleAlert)
                            .size(rems(0.875))
                            .color(theme.warning_foreground)
                            .into_any_element()
                    } else {
                        div()
                            .opacity(if open { 1. } else { 0. })
                            .group_hover(group.clone(), |style| style.opacity(1.))
                            .child(Icon::new(IconName::ChevronDown).size(rems(0.875)))
                            .into_any_element()
                    }),
            )
            .on_mouse_down(gpui_kit::MouseButton::Left, no_drag)
            .on_click({
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |this, cx| this.toggle_menu(OpenMenu::Environment, cx))
                }
            });

        let menu = open.then(|| {
            let mut content = div().flex().flex_col();
            let all_entity = entity.clone();
            content = content
                .child(
                    menu_checkbox_item("usage-env-all", all_selected, "All environments", &theme)
                        .on_click(move |_, _, cx| {
                            all_entity.update(cx, |this, cx| {
                                this.selected_environment_ids =
                                    if this.selected_environment_ids.is_none() {
                                        Some(HashSet::new())
                                    } else {
                                        None
                                    };
                                cx.notify();
                            });
                        }),
                )
                .child(menu_separator(&theme));
            for environment in &view.environments {
                let checked = self
                    .selected_environment_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&environment.environment_id));
                let status = self.status_label(environment, refreshing_usage);
                let toggle_entity = entity.clone();
                let id = environment.environment_id.clone();
                let total = view.environments.len();
                let current: HashSet<EnvironmentId> = selected
                    .iter()
                    .map(|environment| environment.environment_id.clone())
                    .collect();
                let errored = environment.error.is_some();
                content = content.child(
                    menu_checkbox_item(
                        SharedString::from(format!("usage-env:{}", id.as_str())),
                        checked,
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .child(environment.label.clone()),
                            )
                            .when(show_status, |this| {
                                this.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(TEXT_XS.size)
                                        .text_color(if errored {
                                            theme.destructive
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(status),
                                )
                            }),
                        &theme,
                    )
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        let mut next = current.clone();
                        toggle_entity.update(cx, |this, cx| {
                            if !checked {
                                next.insert(id);
                            } else {
                                next.remove(&id);
                            }
                            this.selected_environment_ids = if next.len() == total {
                                None
                            } else {
                                Some(next)
                            };
                            cx.notify();
                        });
                    }),
                );
            }
            if view.environments.is_empty() {
                content = content.child(
                    div()
                        .px_2()
                        .py_2()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child("No environments connected."),
                );
            }
            if show_status && view.is_partial {
                content = content.child(
                    div()
                        .px_2()
                        .py_2()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child("Totals are partial while selected environments scan."),
                );
            }
            if show_status && let Some(notice) = self.coverage_notice(selected, &d.merged, &theme) {
                content = content.child(notice);
            }
            let prices_entity = entity.clone();
            content = content.child(menu_separator(&theme)).child(
                menu_item(
                    "usage-model-prices",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::SlidersHorizontal).size(rems(1.)))
                        .child("Model prices"),
                    &theme,
                )
                .on_click(move |_, window, cx| {
                    prices_entity.update(cx, |this, cx| {
                        this.close_menu(cx);
                        this.open_price_dialog(None, window, cx);
                    });
                }),
            );
            self.menu_popup("usage-environment-menu", content, cx)
        });

        div()
            .relative()
            .flex()
            .min_w_0()
            .child(trigger)
            .children(menu)
            .into_any_element()
    }

    /// Provider visibility shared by every tab. Stored as the hidden set.
    fn provider_filter(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let hidden = self.hidden_providers();
        let visible: Vec<UsageProviderKind> = PROVIDER_ORDER
            .iter()
            .copied()
            .filter(|provider| !hidden.contains(provider))
            .collect();
        let label = if visible.len() == PROVIDER_ORDER.len() {
            "All providers".to_owned()
        } else if visible.is_empty() {
            "No providers".to_owned()
        } else if visible.len() == 1 {
            provider_label(visible[0]).to_owned()
        } else {
            format!("{} providers", visible.len())
        };
        let open = self.menu == Some(OpenMenu::Provider);
        let group: SharedString = "usage-provider-filter".into();
        let trigger = inline_button("usage-provider", false, &theme)
            .group(group.clone())
            .min_w_0()
            .max_w_full()
            .child(div().min_w_0().truncate().child(label))
            .child(
                div()
                    .opacity(if open { 1. } else { 0. })
                    .group_hover(group.clone(), |style| style.opacity(1.))
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::ChevronDown).size(rems(0.875))),
            )
            .on_mouse_down(gpui_kit::MouseButton::Left, no_drag)
            .on_click({
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |this, cx| this.toggle_menu(OpenMenu::Provider, cx))
                }
            });
        let menu = open.then(|| {
            let all_entity = entity.clone();
            let mut content = div().flex().flex_col().child(
                menu_checkbox_item(
                    "usage-provider-all",
                    hidden.is_empty(),
                    "All providers",
                    &theme,
                )
                .on_click(move |_, _, cx| {
                    all_entity.update(cx, |this, cx| {
                        let everything_hidden = this.hidden_providers().is_empty();
                        this.update_preferences(|preferences| {
                            preferences.hidden_providers = Some(if everything_hidden {
                                PROVIDER_ORDER.to_vec()
                            } else {
                                Vec::new()
                            });
                        });
                        cx.notify();
                    });
                }),
            );
            content = content.child(menu_separator(&theme));
            for provider in PROVIDER_ORDER {
                let checked = !hidden.contains(&provider);
                let toggle_entity = entity.clone();
                content = content.child(
                    menu_checkbox_item(
                        SharedString::from(format!(
                            "usage-provider:{}",
                            presentation(provider).label
                        )),
                        checked,
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(provider_mark(provider, 14., cx))
                            .child(div().truncate().child(presentation(provider).label)),
                        &theme,
                    )
                    .on_click(move |_, _, cx| {
                        toggle_entity.update(cx, |this, cx| {
                            let current = this.hidden_providers();
                            let next: Vec<UsageProviderKind> = PROVIDER_ORDER
                                .iter()
                                .copied()
                                .filter(|entry| {
                                    if *entry == provider {
                                        checked
                                    } else {
                                        current.contains(entry)
                                    }
                                })
                                .collect();
                            this.update_preferences(|preferences| {
                                preferences.hidden_providers = Some(next)
                            });
                            cx.notify();
                        });
                    }),
                );
            }
            self.menu_popup("usage-provider-menu", content, cx)
        });
        div()
            .relative()
            .flex()
            .min_w_0()
            .child(trigger)
            .children(menu)
            .into_any_element()
    }

    fn render_header(
        &mut self,
        view: &UsageView,
        d: &Derived,
        viewport_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let metric = self.preferences.metric;
        let showing_limits = metric == UsagePageMetric::Limits;
        let header_left = if self.sidebar_open {
            px(20.)
        } else {
            workspace_titlebar_content_left(window)
        };
        let environment_filter = self.environment_filter(view, d, showing_limits, cx);
        let provider_filter = self.provider_filter(cx);
        let can_refresh = !self.refreshing && (showing_limits || d.can_read_diagnostics);
        let refresh_color: gpui_kit::Hsla = with_alpha(theme.foreground, 0.8).into();

        let refresh_button = {
            let entity = entity.clone();
            Button::new("usage-refresh")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::IconSm)
                .disabled(!can_refresh)
                .child(refresh_icon(self.refreshing, refresh_color))
                .on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.refresh_window(window, cx));
                })
        };

        let breadcrumb = WorkspaceBreadcrumb::new()
            .child(WorkspaceBreadcrumbItem::new(false).child(div().child("Usage")))
            .child(WorkspaceBreadcrumbSeparator)
            .child(
                WorkspaceBreadcrumbItem::new(false)
                    .min_w(rems(2.5))
                    .flex_shrink(1.)
                    .child(environment_filter),
            )
            .child(WorkspaceBreadcrumbSeparator)
            .child(
                WorkspaceBreadcrumbItem::new(true)
                    .min_w(rems(2.5))
                    .child(provider_filter),
            );

        let controls = if viewport_width >= XL {
            let metric_group =
                segmented_group(&theme).children(METRIC_OPTIONS.iter().map(|option| {
                    let entity = entity.clone();
                    let value = option.value;
                    Toggle::new(SharedString::from(format!(
                        "usage-metric:{}",
                        option.command
                    )))
                    .variant(ToggleVariant::Segmented)
                    .size(ToggleSize::Segmented)
                    .label(option.label)
                    .pressed(metric == option.value)
                    .tooltip(Tooltip::new(
                        self.shortcut_title(option.label, option.command),
                    ))
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| this.select_metric(value, cx));
                    })
                }));
            // The period does not apply to Limits, so it stays in place but
            // disabled; unmounting it shifted the metric toggle ~300px.
            let period_group =
                segmented_group(&theme).children(WINDOW_OPTIONS.iter().map(|option| {
                    let entity = entity.clone();
                    let days = option.days;
                    Toggle::new(SharedString::from(format!("usage-period:{days}")))
                        .variant(ToggleVariant::Segmented)
                        .size(ToggleSize::Segmented)
                        .label(option.label)
                        .pressed(self.window_days == option.days)
                        .disabled(showing_limits)
                        .tooltip(Tooltip::new(
                            self.shortcut_title(option.label, option.command),
                        ))
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| this.select_window(days, cx));
                        })
                }));
            div()
                .ml_auto()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_end()
                .gap_2()
                .child(metric_group)
                .child(period_group)
                .child(refresh_button)
        } else {
            let metric_label = METRIC_OPTIONS
                .iter()
                .find(|option| option.value == metric)
                .map_or("", |option| option.label);
            let period_label = WINDOW_OPTIONS
                .iter()
                .find(|option| option.days == self.window_days)
                .map_or("", |option| option.label);
            let metric_select = self.compact_select(
                "usage-metric-select",
                OpenMenu::Metric,
                metric_label,
                false,
                METRIC_OPTIONS
                    .iter()
                    .map(|option| (option.label, option.command, metric == option.value))
                    .collect(),
                |this, index, cx| this.select_metric(METRIC_OPTIONS[index].value, cx),
                cx,
            );
            let period_select = self.compact_select(
                "usage-period-select",
                OpenMenu::Period,
                period_label,
                showing_limits,
                WINDOW_OPTIONS
                    .iter()
                    .map(|option| {
                        (
                            option.label,
                            option.command,
                            self.window_days == option.days,
                        )
                    })
                    .collect(),
                |this, index, cx| this.select_window(WINDOW_OPTIONS[index].days, cx),
                cx,
            );
            div()
                .ml_auto()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_end()
                .gap_1()
                .child(metric_select)
                .child(period_select)
                .child(refresh_button)
        };

        div()
            .id("usage-header")
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
                    .min_w_0()
                    .items_center()
                    .gap_x_3()
                    .py_2()
                    .child(div().flex().flex_1().min_w_0().child(breadcrumb))
                    .when(!showing_limits && viewport_width >= XXL, |this| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(d.window_label.clone()),
                        )
                    })
                    .child(controls),
            )
            .drag_region(window, cx)
            .into_any_element()
    }

    /// `Select size="compact" variant="ghost"`: a ghost button with a chevron and a list popup.
    #[allow(clippy::too_many_arguments)]
    fn compact_select(
        &mut self,
        id: &'static str,
        menu: OpenMenu,
        label: &'static str,
        disabled: bool,
        items: Vec<(&'static str, &'static str, bool)>,
        on_select: fn(&mut Self, usize, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let open = self.menu == Some(menu) && !disabled;
        let trigger = {
            let entity = entity.clone();
            Button::new(id)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .disabled(disabled)
                .pressed(open)
                .child(div().min_w_0().truncate().child(label))
                .child(Icon::new(IconName::ChevronDown).size(rems(0.875)))
                .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.toggle_menu(menu, cx)))
        };
        let popup = open.then(|| {
            let mut content = div().flex().flex_col();
            for (index, (item_label, command, selected)) in items.iter().enumerate() {
                let entity = entity.clone();
                content = content.child(
                    menu_checkbox_item(
                        SharedString::from(format!("{id}:{index}")),
                        *selected,
                        *item_label,
                        &theme,
                    )
                    .tooltip({
                        let title = self.shortcut_title(item_label, command);
                        move |window, cx| Tooltip::new(title.clone()).build(window, cx)
                    })
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.menu = None;
                            on_select(this, index, cx);
                        });
                    }),
                );
            }
            let entity = cx.entity();
            deferred(
                menu_surface(cx)
                    .absolute()
                    .top(px(30.))
                    .right_0()
                    .min_w(rems(8.))
                    .child(content)
                    .on_mouse_down_out(move |_, _, cx| {
                        entity.update(cx, |this, cx| this.close_menu(cx))
                    }),
            )
            .with_priority(40)
            .into_any_element()
        });
        div()
            .relative()
            .child(trigger)
            .children(popup)
            .into_any_element()
    }
}

impl UsagePage {
    fn cursor_enable_button(
        &self,
        environment: &EnvironmentUsageStatus,
        button_text: String,
        tooltip: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let id = environment.environment_id.clone();
        let pending = self.cursor_enabling.contains(&id);
        let element_id = SharedString::from(format!("cursor-enable:{}", id.as_str()));
        let on_click = move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
            let id = id.clone();
            entity.update(cx, |this, cx| this.enable_cursor(id, window, cx));
        };
        if tooltip {
            let button = inline_button(element_id, false, &theme)
                .opacity(if pending { 0.64 } else { 1. })
                .child(button_text)
                .when(!pending, |this| this.on_click(on_click))
                .tooltip(move |window, cx| Tooltip::new(CURSOR_KEYCHAIN_COPY).build(window, cx));
            button.into_any_element()
        } else {
            Button::new(element_id)
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .disabled(pending)
                .label(button_text)
                .on_click(on_click)
                .into_any_element()
        }
    }

    fn cursor_enable_row(
        &self,
        environment: &EnvironmentUsageStatus,
        show_environment: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let color = presentation(UsageProviderKind::Cursor)
            .color
            .resolve(&theme);
        div()
            .flex()
            .min_w_0()
            .items_baseline()
            .justify_between()
            .gap_4()
            .text_size(TEXT_SM.size)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .text_color(theme.foreground)
                    .child(div().flex_shrink_0().size(px(8.)).rounded_full().bg(color))
                    .child(provider_mark(UsageProviderKind::Cursor, 16., cx))
                    .child(div().truncate().child(if show_environment {
                        format!("Cursor · {}", environment.label)
                    } else {
                        "Cursor".to_owned()
                    })),
            )
            .child(self.cursor_enable_button(environment, "Enable".to_owned(), true, cx))
            .into_any_element()
    }

    fn cursor_enable_limits(
        &self,
        environments: &[EnvironmentUsageStatus],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let many = environments.len() > 1;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(h2("Cursor", &theme).flex().items_center().gap_2().child(
                provider_instance_icon(
                    "cursor",
                    "Cursor",
                    rems(1.),
                    Some(with_alpha(theme.foreground, 0.8).into()),
                    cx,
                ),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_3()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .p_4()
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(CURSOR_KEYCHAIN_COPY),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(environments.iter().map(|environment| {
                                self.cursor_enable_button(
                                    environment,
                                    if many {
                                        format!("Enable on {}", environment.label)
                                    } else {
                                        "Enable".to_owned()
                                    },
                                    false,
                                    cx,
                                )
                            })),
                    ),
            )
            .into_any_element()
    }

    fn summary_section(
        &self,
        view: &UsageView,
        d: &Derived,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let metric = self.preferences.metric;
        let hidden = self.hidden_providers();
        let merged = &d.merged;
        let loading_opacity = figure_opacity(d.loading_partial);
        let chatgpt_visible = !hidden.contains(&UsageProviderKind::Codex)
            && self
                .backend
                .limits_environments()
                .iter()
                .filter(|environment| {
                    self.selected_environment_ids
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&environment.environment_id))
                })
                .any(|environment| {
                    environment
                        .providers
                        .iter()
                        .any(|provider| uses_chatgpt_sharing(Some(provider)))
                });

        let mut rows: Vec<SummaryRow<'_>> = d
            .active_providers
            .iter()
            .map(|provider| SummaryRow::Usage(*provider))
            .collect();
        let cursor_insert_at = [UsageProviderKind::Codex, UsageProviderKind::Claude]
            .iter()
            .filter_map(|provider| {
                d.active_providers
                    .iter()
                    .position(|active| active == provider)
            })
            .max()
            .map_or(0, |index| index + 1);
        for (offset, environment) in d.cursor_access.iter().enumerate() {
            rows.insert(cursor_insert_at + offset, SummaryRow::Enable(environment));
        }

        let big_figure = if metric == UsagePageMetric::Cost {
            format_usd(merged.cost_usd)
        } else {
            format_tokens(merged.total_tokens as f64)
        };
        let unpriced_share = merged.cost_quality.unpriced_share;
        let headline = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(rems(2.25))
                    .line_height(rems(2.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .opacity(loading_opacity)
                    .child(big_figure),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex()
                            .child(div().opacity(loading_opacity).child(format!(
                                "{} sessions",
                                format_count(merged.sessions as f64)
                            )))
                            .when(metric == UsagePageMetric::Cost, |this| {
                                this.child(" · API estimate")
                            }),
                    )
                    .when(
                        metric == UsagePageMetric::Cost && unpriced_share > 0.,
                        |this| {
                            this.child(
                                div()
                                    .id("usage-unpriced-info")
                                    .flex()
                                    .items_center()
                                    .cursor(CursorStyle::PointingHand)
                                    .child(Icon::new(IconName::Info).size(rems(0.75)))
                                    .tooltip(rich_tooltip(vec![(
                                        format!(
                                            "API estimate excludes {} unpriced records.",
                                            format_percent(unpriced_share, 1)
                                        )
                                        .into(),
                                        true,
                                    )])),
                            )
                        },
                    ),
            );

        let provider_rows = rows.iter().map(|row| match row {
            SummaryRow::Enable(environment) => {
                self.cursor_enable_row(environment, view.selected_environments.len() > 1, cx)
            }
            SummaryRow::Usage(provider) => {
                let provider = *provider;
                let totals = merged
                    .providers
                    .iter()
                    .find(|entry| entry.provider == provider);
                let share = totals.map_or(0., |entry| {
                    if metric == UsagePageMetric::Cost {
                        entry.cost_share
                    } else {
                        entry.token_share
                    }
                });
                let sessions = totals.map_or(0, |entry| entry.sessions);
                let session_label = format!(
                    "{} {}",
                    format_count(sessions as f64),
                    if sessions == 1 { "session" } else { "sessions" }
                );
                let loading = d.provider_loading(provider);
                let awaiting = loading && !d.providers_with_data.contains(&provider);
                let opacity = figure_opacity(loading);
                let color = presentation(provider).color.resolve(&theme);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .text_size(TEXT_SM.size)
                                    .text_color(theme.foreground)
                                    .child(
                                        div().flex_shrink_0().size(px(8.)).rounded_full().bg(color),
                                    )
                                    .child(provider_mark(provider, 16., cx))
                                    .child(
                                        div()
                                            .flex()
                                            .min_w_0()
                                            .items_baseline()
                                            .gap(rems(0.375))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .child(presentation(provider).label),
                                            )
                                            .child(
                                                div()
                                                    .flex_shrink_0()
                                                    .whitespace_nowrap()
                                                    .text_size(TEXT_2XS.size)
                                                    .text_color(theme.muted_foreground)
                                                    .opacity(if awaiting { 0. } else { opacity })
                                                    .child(session_label),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .opacity(opacity)
                                    .child(if awaiting {
                                        "—".to_owned()
                                    } else if metric == UsagePageMetric::Cost {
                                        format_usd(totals.map_or(0., |entry| entry.cost_usd))
                                    } else {
                                        format_tokens(
                                            totals.map_or(0, |entry| entry.total_tokens) as f64
                                        )
                                    }),
                            ),
                    )
                    // Kept while awaiting data so the row does not grow when it lands.
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .opacity(if awaiting { 0. } else { opacity })
                            .child(if metric == UsagePageMetric::Cost {
                                format!(
                                    "{} of cost · {} tokens",
                                    format_percent(share, 1),
                                    format_tokens(
                                        totals.map_or(0, |entry| entry.total_tokens) as f64
                                    )
                                )
                            } else {
                                format!(
                                    "{} of tokens · {}",
                                    format_percent(share, 1),
                                    format_usd(totals.map_or(0., |entry| entry.cost_usd))
                                )
                            }),
                    )
                    .into_any_element()
            }
        });

        let chatgpt_row = chatgpt_visible.then(|| {
            let backend = self.backend.clone();
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child("ChatGPT shared usage")
                .child(
                    Button::new("usage-chatgpt-manage")
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::Xs)
                        .label("Manage usage")
                        .icon(IconName::ExternalLink)
                        .on_click(move |_, _, cx| {
                            backend.open_external(turin_shared::usage_limits::CHATGPT_USAGE_URL, cx)
                        }),
                )
        });

        let left = div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_5()
            .child(headline)
            .children(chatgpt_row)
            .children(provider_rows);

        let title = format!(
            "{} {}",
            if d.shown_hourly { "Hourly" } else { "Daily" },
            if metric == UsagePageMetric::Tokens {
                "processed tokens"
            } else {
                "cost"
            }
        );
        let chart = UsageProviderChart::new(
            "usage-provider-chart",
            UsageProviderChartProps {
                providers: d.active_providers.clone(),
                loading_providers: d.chart_loading.clone(),
                days: d.days.clone(),
                daily: merged.daily.clone(),
                hours: d.hours.clone(),
                hourly: merged.hourly.clone(),
                metric: if metric == UsagePageMetric::Tokens {
                    UsageChartMetric::Tokens
                } else {
                    UsageChartMetric::Cost
                },
                reference_time: d
                    .shown_window
                    .until_time
                    .as_ref()
                    .map(|t| t.as_str().to_owned()),
                resolution: if d.shown_hourly {
                    UsageResolution::Hour
                } else {
                    UsageResolution::Day
                },
                time_zone: d.shown_window.time_zone.as_str().to_owned(),
            },
        );
        let right = div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_3()
            .child(h2(title, &theme))
            .child(chart);

        // `lg:grid-cols-[minmax(0,18rem)_minmax(0,1fr)]`
        if viewport_width >= 1024. {
            div()
                .flex()
                .gap_6()
                .child(left.w(rems(18.)).flex_shrink_0())
                .child(right.flex_1())
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_6()
                .child(left)
                .child(right)
                .into_any_element()
        }
    }

    fn totals_section(&self, d: &Derived, theme: &AppTheme) -> AnyElement {
        let merged = &d.merged;
        let loading = d.loading_partial;
        let figures = [
            (
                "Processed tokens",
                format_tokens(merged.total_tokens as f64),
            ),
            (
                "Cached input",
                format_tokens(merged.cached_input_tokens as f64),
            ),
            (
                "Uncached input",
                format_tokens(merged.uncached_input_tokens as f64),
            ),
            ("Output", format_tokens(merged.output_tokens as f64)),
            (
                "Cache savings",
                format_usd(merged.cost_quality.cache_savings_usd),
            ),
        ];
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(h2("Totals", theme))
            .child(
                div()
                    .grid()
                    .grid_cols(5)
                    .gap_x_6()
                    .gap_y_4()
                    .py_1()
                    .children(
                        figures
                            .into_iter()
                            .map(|(label, value)| metric_figure(label, value, loading, theme)),
                    ),
            )
            .into_any_element()
    }

    fn mixes_section(&self, d: &Derived, cx: &mut Context<Self>) -> Option<AnyElement> {
        let merged = &d.merged;
        if merged.total_tokens == 0 {
            return None;
        }
        let metric = self.preferences.metric;
        let mut bars: Vec<AnyElement> = Vec::new();
        if metric == UsagePageMetric::Tokens {
            bars.push(
                UsageShareBar::new(
                    "Tokens by type",
                    token_type_segments(merged.into()),
                    format_tokens,
                )
                .into_any_element(),
            );
        } else {
            bars.push(
                UsageShareBar::new(
                    "Cost by type",
                    cost_type_segments(&merged.category_cost),
                    format_usd,
                )
                .into_any_element(),
            );
            if merged.speed_cost.fast + merged.speed_cost.ultrafast > 0. {
                bars.push(
                    UsageShareBar::new(
                        "Cost by speed",
                        speed_cost_segments(&merged.speed_cost),
                        format_usd,
                    )
                    .aside(speed_premium(merged.speed_cost.premium, cx))
                    .into_any_element(),
                );
            }
        }
        Some(
            div()
                .grid()
                .grid_cols(2)
                .gap_x_12()
                .gap_y_8()
                .opacity(figure_opacity(d.loading_partial))
                .children(bars)
                .into_any_element(),
        )
    }

    fn breakdown_section(
        &mut self,
        view: &UsageView,
        d: &Derived,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entity = cx.entity();
        let metric = self.preferences.metric;
        let tabs = segmented_group(&theme).children(
            [
                (Breakdown::Model, "Model"),
                (Breakdown::Time, if d.shown_hourly { "Hour" } else { "Day" }),
            ]
            .into_iter()
            .map(|(value, label)| {
                let entity = entity.clone();
                Toggle::new(SharedString::from(format!("usage-breakdown:{label}")))
                    .variant(ToggleVariant::Segmented)
                    .size(ToggleSize::Segmented)
                    .label(label)
                    .pressed(self.breakdown == value)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.breakdown = value;
                            cx.notify();
                        });
                    })
            }),
        );

        let header_cell = |label: &str| {
            div()
                .py_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(label.to_owned())
        };
        let table = if self.breakdown == Breakdown::Model {
            let chart_window = UsageChartWindow {
                days: d.days.clone(),
                hours: d.hours.clone(),
                resolution: if d.shown_hourly {
                    UsageResolution::Hour
                } else {
                    UsageResolution::Day
                },
                time_zone: d.shown_window.time_zone.as_str().to_owned(),
                reference_time: d
                    .shown_window
                    .until_time
                    .as_ref()
                    .map(|t| t.as_str().to_owned()),
            };
            let chart_metric = if metric == UsagePageMetric::Tokens {
                UsageChartMetric::Tokens
            } else {
                UsageChartMetric::Cost
            };
            let selected_environments = view.selected_environments.clone();
            let mut rows: Vec<AnyElement> = Vec::new();
            if d.breakdown_models.is_empty() {
                rows.push(
                    div()
                        .py_6()
                        .flex()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child("No activity in this window.")
                        .into_any_element(),
                );
            }
            for (index, model) in d.breakdown_models.iter().enumerate() {
                let value = if metric == UsagePageMetric::Tokens {
                    model.total_tokens as f64
                } else {
                    model.cost_usd
                };
                let share = model_share(
                    model,
                    if metric == UsagePageMetric::Tokens {
                        ShareMetric::Tokens
                    } else {
                        ShareMetric::Cost
                    },
                );
                let opacity = figure_opacity(d.provider_loading(model.provider));
                let color = presentation(model.provider).color.resolve(&theme);
                let bar_fraction = if value > 0. && d.breakdown_peak > 0. {
                    Some((value / d.breakdown_peak) as f32)
                } else {
                    None
                };
                let open_entity = entity.clone();
                let model_clone = model.clone();
                let chart_window = chart_window.clone();
                let environments = selected_environments.clone();
                rows.push(
                    div()
                        .id(SharedString::from(format!(
                            "usage-model-row:{}:{}",
                            model.provider as u8, model.model
                        )))
                        .flex()
                        .items_start()
                        .border_b_1()
                        .border_color(with_alpha(theme.border, 0.5))
                        .text_color(theme.muted_foreground)
                        .whitespace_nowrap()
                        .cursor(CursorStyle::PointingHand)
                        .hover({
                            let hover = with_alpha(theme.muted, 0.5);
                            move |style| style.bg(hover)
                        })
                        .child(
                            div()
                                .w(rems(2.))
                                .flex_shrink_0()
                                .py(rems(0.625))
                                .pr_3()
                                .text_size(TEXT_XS.size)
                                .child((index + 1).to_string()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .py(rems(0.625))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_color(theme.foreground)
                                        .child(provider_mark(model.provider, 14., cx))
                                        .child(model.model.clone()),
                                )
                                .child(
                                    div()
                                        .mt(rems(0.375))
                                        .h(px(2.))
                                        .max_w(rems(12.))
                                        .opacity(opacity)
                                        .children(bar_fraction.map(|fraction| {
                                            // A short minimum keeps tiny shares a dash, not a dot.
                                            div()
                                                .h_full()
                                                .min_w(rems(0.5))
                                                .w(relative(fraction))
                                                .rounded_full()
                                                .bg(color)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .w(rems(6.5))
                                .flex_shrink_0()
                                .py(rems(0.625))
                                .pl_6()
                                .text_right()
                                .text_color(theme.foreground)
                                .opacity(opacity)
                                .child(if is_model_cost_unknown(model) {
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child("Unpriced")
                                        .into_any_element()
                                } else {
                                    div().child(format_usd(model.cost_usd)).into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .w(rems(5.5))
                                .flex_shrink_0()
                                .py(rems(0.625))
                                .pl_6()
                                .text_right()
                                .opacity(opacity)
                                .child(
                                    share
                                        .map_or_else(String::new, |share| format_percent(share, 1)),
                                ),
                        )
                        .child(
                            div()
                                .w(rems(6.))
                                .flex_shrink_0()
                                .py(rems(0.625))
                                .pl_6()
                                .text_right()
                                .opacity(opacity)
                                .child(format_tokens(model.total_tokens as f64)),
                        )
                        .on_click(move |_, window, cx| {
                            let model = model_clone.clone();
                            let chart_window = chart_window.clone();
                            let environments = environments.clone();
                            open_entity.update(cx, |this, cx| {
                                this.open_model_dialog(
                                    model,
                                    chart_window,
                                    chart_metric,
                                    environments,
                                    window,
                                    cx,
                                )
                            });
                        })
                        .into_any_element(),
                );
            }
            div()
                .flex()
                .flex_col()
                .text_size(TEXT_SM.size)
                .child(
                    div()
                        .flex()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(header_cell("#").w(rems(2.)).flex_shrink_0().pr_3())
                        .child(header_cell("Model").flex_1())
                        .child(
                            header_cell("Cost")
                                .w(rems(6.5))
                                .flex_shrink_0()
                                .pl_6()
                                .text_right(),
                        )
                        .child(
                            header_cell("Share")
                                .w(rems(5.5))
                                .flex_shrink_0()
                                .pl_6()
                                .text_right(),
                        )
                        .child(
                            header_cell("Tokens")
                                .w(rems(6.))
                                .flex_shrink_0()
                                .pl_6()
                                .text_right(),
                        ),
                )
                .children(rows)
                .into_any_element()
        } else {
            // `table-fixed`: the first column is two fifths, the rest share 60%.
            let value_width = 0.6 / (d.active_providers.len() as f32 + 2.);
            let mut rows: Vec<AnyElement> = Vec::new();
            if d.breakdown_periods.is_empty() {
                rows.push(
                    div()
                        .py_6()
                        .flex()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child("No activity in this window.")
                        .into_any_element(),
                );
            }
            for (index, period) in d.breakdown_periods.iter().enumerate() {
                rows.push(
                    div()
                        .id(SharedString::from(format!("usage-period-row:{index}")))
                        .flex()
                        .border_b_1()
                        .border_color(with_alpha(theme.border, 0.5))
                        .hover({
                            let hover = with_alpha(theme.muted, 0.5);
                            move |style| style.bg(hover)
                        })
                        .child(
                            div()
                                .w(relative(0.4))
                                .py_2()
                                .text_color(theme.foreground)
                                .child(period.label.clone()),
                        )
                        .children(d.active_providers.iter().map(|provider| {
                            div()
                                .w(relative(value_width))
                                .py_2()
                                .text_right()
                                .text_color(theme.muted_foreground)
                                .opacity(figure_opacity(d.provider_loading(*provider)))
                                .child(format_usd(
                                    period
                                        .by_provider
                                        .get(provider)
                                        .map_or(0., |spend| spend.cost_usd),
                                ))
                        }))
                        .child(
                            div()
                                .w(relative(value_width))
                                .py_2()
                                .text_right()
                                .text_color(theme.foreground)
                                .opacity(figure_opacity(d.loading_partial))
                                .child(format_usd(period.cost_usd)),
                        )
                        .child(
                            div()
                                .w(relative(value_width))
                                .py_2()
                                .text_right()
                                .text_color(theme.muted_foreground)
                                .opacity(figure_opacity(d.loading_partial))
                                .child(format_tokens(period.total_tokens as f64)),
                        )
                        .into_any_element(),
                );
            }
            div()
                .flex()
                .flex_col()
                .text_size(TEXT_SM.size)
                .child(
                    div()
                        .flex()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            header_cell(if d.shown_hourly { "Hour" } else { "Day" })
                                .w(relative(0.4)),
                        )
                        .children(d.active_providers.iter().map(|provider| {
                            header_cell(presentation(*provider).label)
                                .w(relative(value_width))
                                .text_right()
                        }))
                        .child(header_cell("Total").w(relative(value_width)).text_right())
                        .child(header_cell("Tokens").w(relative(value_width)).text_right()),
                )
                .children(rows)
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(h2("Breakdown", &theme))
                    .child(tabs),
            )
            .child(table)
            .into_any_element()
    }

    /// Stand-in with the loaded page's shape, using the shared skeleton bars so
    /// it breathes with the same pulse as every other loading state.
    fn skeleton(&self, viewport_width: f32, theme: &AppTheme) -> AnyElement {
        let bar = |id: &'static str, w: Option<f32>, h: f32| {
            let skeleton = Skeleton::new(id).h(px(h));
            match w {
                Some(w) => skeleton.w(px(w)),
                None => skeleton.w_full(),
            }
        };
        let left = div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(bar("usage-skeleton-total", Some(144.), 40.))
                    .child(bar("usage-skeleton-sessions", Some(128.), 16.)),
            )
            .children(PROVIDER_ORDER.iter().map(|provider| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .min_h(rems(1.25))
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Skeleton::new(SharedString::from(format!(
                                            "usage-skeleton-dot:{}",
                                            *provider as u8
                                        )))
                                        .shape(SkeletonShape::Pill)
                                        .size(px(8.)),
                                    )
                                    .child(
                                        Skeleton::new(SharedString::from(format!(
                                            "usage-skeleton-mark:{}",
                                            *provider as u8
                                        )))
                                        .shape(SkeletonShape::Pill)
                                        .size(px(16.)),
                                    )
                                    .child(bar("usage-skeleton-name", Some(80.), 14.)),
                            )
                            .child(bar("usage-skeleton-value", Some(56.), 14.)),
                    )
                    .child(bar("usage-skeleton-share", Some(144.), 16.))
            }));
        let right = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(bar("usage-skeleton-title", Some(96.), 20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(bar("usage-skeleton-chart", None, 224.).ml(px(64.)))
                    .child(bar("usage-skeleton-axis", None, 16.).ml(px(64.))),
            );
        let summary = if viewport_width >= 1024. {
            div()
                .flex()
                .gap_6()
                .child(left.w(rems(18.)).flex_shrink_0())
                .child(right.flex_1())
        } else {
            div().flex().flex_col().gap_6().child(left).child(right)
        };
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(summary)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(h2("Totals", theme))
                    .child(
                        div()
                            .grid()
                            .grid_cols(5)
                            .gap_x_6()
                            .gap_y_4()
                            .py_1()
                            .children(
                                [
                                    "Processed tokens",
                                    "Cached input",
                                    "Uncached input",
                                    "Output",
                                    "Cache savings",
                                ]
                                .into_iter()
                                .map(|label| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .child(
                                            div()
                                                .text_size(TEXT_XS.size)
                                                .text_color(theme.muted_foreground)
                                                .child(label),
                                        )
                                        .child(bar("usage-skeleton-metric", Some(64.), 24.))
                                }),
                            ),
                    ),
            )
            .child(
                div().grid().grid_cols(2).gap_x_12().gap_y_8().child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.625))
                        .child(bar("usage-skeleton-mix-title", Some(112.), 20.))
                        .child(bar("usage-skeleton-mix-bar", None, 8.))
                        .child(bar("usage-skeleton-mix-legend", Some(288.), 16.)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(h2("Breakdown", theme))
                            .child(
                                Skeleton::new("usage-skeleton-tabs")
                                    .shape(SkeletonShape::Card)
                                    .w(px(112.))
                                    .h(px(28.)),
                            ),
                    )
                    .child(bar("usage-skeleton-table", None, 176.)),
            )
            .into_any_element()
    }

    fn render_body(
        &mut self,
        view: &UsageView,
        d: &Derived,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let viewport_width = f32::from(window.viewport_size().width);
        let metric = self.preferences.metric;
        let showing_limits = metric == UsagePageMetric::Limits;
        let hidden = self.hidden_providers();

        let content: AnyElement = if view.selected_environments.is_empty() {
            div()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child(format!(
                    "{} to see {}.",
                    if view.environments.is_empty() {
                        "Connect an environment"
                    } else {
                        "Select an environment"
                    },
                    if showing_limits { "limits" } else { "usage" }
                ))
                .into_any_element()
        } else if showing_limits {
            let environments: Vec<LimitsEnvironment> = select_limit_environments(
                &self.backend.limits_environments(),
                self.selected_environment_ids.as_ref(),
                &hidden,
            );
            let cursor_prompt = (!d.cursor_access.is_empty()).then(|| {
                let page = cx.entity().downgrade();
                let access = d.cursor_access.clone();
                Rc::new(move |_: &mut Window, cx: &mut App| -> AnyElement {
                    page.update(cx, |this, cx| this.cursor_enable_limits(&access, cx))
                        .unwrap_or_else(|_| div().into_any_element())
                }) as Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>
            });
            let now = self.limits_now;
            let timestamp_format = self.backend.timestamp_format();
            self.limits.update(cx, |limits, _| {
                limits.set_props(LimitsProps {
                    environments,
                    now,
                    timestamp_format,
                    cursor_prompt,
                })
            });
            self.limits.clone().into_any_element()
        } else if view.shown.is_none() {
            self.skeleton(viewport_width, &theme)
        } else if !d.can_read_diagnostics {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .py(rems(3.))
                .items_center()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .children(view.selected_environments.iter().map(|environment| {
                    div().child(format!(
                        "{}{}",
                        if view.selected_environments.len() > 1 {
                            format!("{}: ", environment.label)
                        } else {
                            String::new()
                        },
                        environment.error.clone().unwrap_or_default()
                    ))
                }))
                .into_any_element()
        } else {
            let mut sections: Vec<AnyElement> = d
                .source_messages
                .iter()
                .map(|message| {
                    div()
                        .mb_4()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.muted_foreground)
                        .child(message.clone())
                        .into_any_element()
                })
                .collect();
            sections.push(self.summary_section(view, d, viewport_width, cx));
            sections.push(self.totals_section(d, &theme));
            sections.extend(self.mixes_section(d, cx));
            sections.push(self.breakdown_section(view, d, cx));
            div()
                .flex()
                .flex_col()
                .gap_6()
                .children(sections)
                .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .child(
                crate::ui::scroll_area("usage-scroll").child(
                    // `WorkspacePageContainer width="wide"`: `mx-auto flex w-full flex-col
                    // gap-6 px-5 pt-6 pb-12 sm:px-6`, `max-w-5xl`.
                    div()
                        .mx_auto()
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_w(rems(PAGE_MAX_WIDTH))
                        .gap_6()
                        .px(px(24.))
                        .pt_6()
                        .pb(rems(3.))
                        .child(content),
                ),
            )
            .into_any_element()
    }
}
