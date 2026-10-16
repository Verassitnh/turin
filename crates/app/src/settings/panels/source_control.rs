//! Port of SourceControlSettings.tsx (`SourceControlSettingsPanel`),
//! GitHubTokenSettings.tsx, GitHubAccountSettings.tsx,
//! SourceControlWritingSettings.tsx, BranchNamingSettings.tsx and the
//! `source-control` rows of ProjectDefaultsSettings.tsx for the single local
//! environment.
//!
//! Not ported (PORTING.md Deviations): host settings forms for the other
//! source-control hosts (GitLab, Bitbucket, Azure DevOps, GitCafe: only GitHub
//! is kept), the scope notice / mixed-value states of multi-environment
//! scopes, and the `Collapsible` height animation.

use std::collections::HashSet;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, rems,
};
use indexmap::IndexMap;
use serde_json::{Value, json};
use turin_contracts::source_control::{
    SourceControlDiscoveryStatus, SourceControlProviderAuth, SourceControlProviderAuthStatus,
    SourceControlProviderDiscoveryItem, VcsDiscoveryItem,
};

use super::super::controls::{
    FieldSize, SelectWidth, redacted_placeholder, switch, text_field, textarea_field,
};
use super::super::github_account_logic::{
    GitHubHostChange, GitHubHostChoice, GitHubHosts, group_github_accounts, next_github_hosts,
};
use super::super::layout::{SettingsRow, SettingsSection, setting_reset_button};
use super::super::model_picker::{provider_is_selectable, resolve_default_selection};
use super::super::panels_logic::{
    background_activity_fetch_interval_settings, duration_to_seconds,
};
use super::super::rows::{BoolRow, Json, SelectRowSpec};
use super::super::search::searchable_setting;
use super::super::view::{SettingsView, Which};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_SM, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{
    Badge, BadgeSize, BadgeVariant, Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip,
};

/// `GIT_FETCH_INTERVAL_STEP_SECONDS`.
const GIT_FETCH_INTERVAL_STEP_SECONDS: u64 = 5;
/// Sentinel select value for "follow gh's active login".
const ACTIVE_ACCOUNT: &str = "active gh account";
const DEFAULT_HOST: &str = "github.com";

#[derive(Default)]
pub struct SourceControlState {
    pub expanded: HashSet<String>,
    pub github_host: Option<String>,
    pub revealed_accounts: bool,
}

/// `code rounded bg-muted px-1 py-px text-2xs`.
fn code(text: impl Into<SharedString>, theme: &AppTheme) -> gpui_kit::Div {
    div()
        .rounded(RADIUS_SM)
        .bg(theme.muted)
        .px_1()
        .py(px(1.))
        .font_family(theme.fonts.mono.clone())
        .text_size(TEXT_2XS.size)
        .child(text.into())
}

#[derive(Clone)]
enum Item {
    Vcs(VcsDiscoveryItem),
    Provider(SourceControlProviderDiscoveryItem),
}

impl Item {
    fn label(&self) -> String {
        match self {
            Item::Vcs(item) => item.label.as_str().to_owned(),
            Item::Provider(item) => item.label.as_str().to_owned(),
        }
    }
    fn kind(&self) -> String {
        match self {
            Item::Vcs(item) => serde_json::to_value(item.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            Item::Provider(item) => item.kind.as_str().to_owned(),
        }
    }
    fn available(&self) -> bool {
        match self {
            Item::Vcs(item) => item.status == SourceControlDiscoveryStatus::Available,
            Item::Provider(item) => item.status == SourceControlDiscoveryStatus::Available,
        }
    }
    fn version(&self) -> Option<String> {
        match self {
            Item::Vcs(item) => item.version.as_ref_option().map(|v| v.as_str().to_owned()),
            Item::Provider(item) => item.version.as_ref_option().map(|v| v.as_str().to_owned()),
        }
    }
    fn executable(&self) -> Option<String> {
        match self {
            Item::Vcs(item) => item.executable.as_ref().map(|v| v.as_str().to_owned()),
            Item::Provider(item) => item.executable.as_ref().map(|v| v.as_str().to_owned()),
        }
    }
    fn install_hint(&self) -> String {
        match self {
            Item::Vcs(item) => item.install_hint.as_str().to_owned(),
            Item::Provider(item) => item.install_hint.as_str().to_owned(),
        }
    }
    fn not_ready(&self) -> bool {
        matches!(self, Item::Vcs(item) if !item.implemented)
    }
    fn auth(&self) -> Option<&SourceControlProviderAuth> {
        match self {
            Item::Provider(item) => Some(&item.auth),
            Item::Vcs(_) => None,
        }
    }
    fn enabled(&self) -> bool {
        match self {
            Item::Provider(item) => {
                item.status == SourceControlDiscoveryStatus::Available
                    && item.auth.status == SourceControlProviderAuthStatus::Authenticated
            }
            Item::Vcs(item) => {
                item.status == SourceControlDiscoveryStatus::Available && item.implemented
            }
        }
    }
}

/// `itemStatusDot`.
fn item_status_dot(item: &Item, theme: &AppTheme) -> gpui_kit::Hsla {
    if item.not_ready() {
        return with_alpha(theme.muted_foreground, 0.35).into();
    }
    if !item.available() {
        return theme.warning;
    }
    if let Some(auth) = item.auth()
        && auth.status != SourceControlProviderAuthStatus::Authenticated
    {
        return theme.warning;
    }
    theme.success
}

fn auth_presentation(auth: &SourceControlProviderAuth) -> (&'static str, bool) {
    match auth.status {
        SourceControlProviderAuthStatus::Authenticated => ("Authenticated", false),
        SourceControlProviderAuthStatus::Unauthenticated => ("Not authenticated", true),
        SourceControlProviderAuthStatus::Unknown => ("Status unknown", false),
    }
}

impl SettingsView {
    pub(super) fn render_source_control(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let json = self.json(cx);
        let mut sections = vec![self.repositories_section(&json, cx)];

        let result = vm.source_control.clone();
        let has_vcs = !result.version_control_systems.is_empty();
        let has_providers = !result.source_control_providers.is_empty();
        let scan = Button::new("rescan-source-control")
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::IconXs)
            .icon(IconName::RefreshCw)
            .tooltip(Tooltip::new("Rescan Git and hosting integrations"))
            .on_click(cx.listener(|this, _, _, cx| {
                let source = this.store.read(cx).source();
                source.rescan_source_control(cx);
            }));
        let (source_control_id, _) = searchable_setting("source-control");

        if has_vcs || has_providers {
            if has_vcs {
                let mut section = SettingsSection::new("Version Control")
                    .id(source_control_id)
                    .header_action(scan);
                for item in &result.version_control_systems {
                    let item = Item::Vcs(item.clone());
                    let details = (item.kind() == "git")
                        .then(|| self.git_fetch_interval_settings(window, cx));
                    section = section.child(self.discovery_item_row(&item, details, cx));
                }
                sections.push(section.into_any_element());
                if has_providers {
                    sections.push(self.providers_discovery_section(
                        "Source Control Providers",
                        None,
                        window,
                        cx,
                    ));
                }
            } else {
                sections.push(self.providers_discovery_section(
                    "Source Control Providers",
                    Some((source_control_id, scan.into_any_element())),
                    window,
                    cx,
                ));
            }
        } else {
            sections.push(
                SettingsSection::new("Server environment")
                    .id(source_control_id)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_4()
                            .px_6()
                            .py_10()
                            .child(
                                div()
                                    .flex()
                                    .size_10()
                                    .items_center()
                                    .justify_center()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme.border)
                                    .child(Icon::new(IconName::GitPullRequest).size(rems(1.25)).color(theme.muted_foreground)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(rems(1.125))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Nothing detected yet"),
                                    )
                                    .child(
                                        div()
                                            .max_w(rems(24.))
                                            .text_center()
                                            .text_size(TEXT_SM.size)
                                            .text_color(theme.muted_foreground)
                                            .child("Install Git on the server, add optional hosting integrations or credentials your workspace needs, then rescan."),
                                    ),
                            )
                            .child(
                                Button::new("scan-source-control")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .icon(IconName::RefreshCw)
                                    .label("Scan")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let source = this.store.read(cx).source();
                                        source.rescan_source_control(cx);
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }
        sections.push(self.source_control_text_generation_section(window, cx));
        sections
    }

    fn providers_discovery_section(
        &mut self,
        title: &'static str,
        first: Option<(&'static str, AnyElement)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let vm = self.vm(cx);
        let mut section = SettingsSection::new(title);
        if let Some((id, action)) = first {
            section = section.id(id).header_action(action);
        }
        for item in &vm.source_control.source_control_providers {
            let item = Item::Provider(item.clone());
            let details =
                (item.kind() == "github").then(|| self.github_settings(&item, window, cx));
            section = section.child(self.discovery_item_row(&item, details, cx));
        }
        section.into_any_element()
    }

    // ── repositories (ProjectDefaultsSettings, category source-control) ──

    fn repositories_section(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        let auto_pull = self.bool_row(
            cx,
            json,
            BoolRow {
                search_id: "automatic-pull",
                title: None,
                description: "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.".into(),
                which: Which::Server,
                key: "defaultAutoPull",
                label: "Default automatic pull",
                reset_label: "default automatic pull",
            },
        );
        let credits = self.bool_row(
            cx,
            json,
            BoolRow {
                search_id: "remove-agent-credits-on-merge",
                title: None,
                description: "Remove recognized agent credit lines from GitHub merge and squash messages, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.".into(),
                which: Which::Server,
                key: "removeAgentCreditsOnMerge",
                label: "Remove agent credits when merging",
                reset_label: "agent credit removal",
            },
        );
        let opts = |items: &[(&str, &str)]| -> Vec<(Value, SharedString)> {
            items
                .iter()
                .map(|(value, label)| (json!(value), SharedString::from((*label).to_owned())))
                .collect()
        };
        // `pullRequestMergeMethod: null` is "Last selected".
        let current = json.get(Which::Server, "pullRequestMergeMethod").clone();
        let options = opts(&[
            ("last", "Last selected"),
            ("merge", "Create merge commit"),
            ("squash", "Squash and merge"),
            ("rebase", "Rebase and merge"),
        ]);
        let current_key = current.as_str().unwrap_or("last").to_owned();
        let label = options
            .iter()
            .find(|(value, _)| value.as_str() == Some(current_key.as_str()))
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let control = self.select_control(
            cx,
            "pull-request-merge-method",
            label,
            options
                .iter()
                .map(|(value, label)| {
                    (
                        value.clone(),
                        label.clone(),
                        value.as_str() == Some(current_key.as_str()),
                    )
                })
                .collect(),
            SelectWidth::Auto,
            false,
            |this, value, _, cx| {
                let next = match value.as_str() {
                    Some("last") | None => Value::Null,
                    Some(other) => json!(other),
                };
                this.update_server(json!({ "pullRequestMergeMethod": next }), cx);
            },
        );
        let reset = (!current.is_null()).then(|| {
            let action = self.cb(cx, |this, _, cx| {
                this.update_server(json!({ "pullRequestMergeMethod": Value::Null }), cx)
            });
            setting_reset_button(
                "reset-merge-method",
                "Reset to last selected",
                move |w, c| action(w, c),
            )
            .into_any_element()
        });
        let merge = Self::base_row("pull-request-merge-method")
            .description("Pull requests start with this method. Last selected reuses whatever you chose most recently on this device.")
            .reset_action(reset)
            .control(control)
            .into_any_element();
        SettingsSection::new("Repositories")
            .id("source-control-defaults")
            .child(auto_pull)
            .child(credits)
            .child(merge)
            .into_any_element()
    }

    // ── discovery rows ──────────────────────────────────────────────────

    fn discovery_item_row(
        &mut self,
        item: &Item,
        details: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let kind = format!(
            "{}:{}",
            if matches!(item, Item::Vcs(_)) {
                "vcs"
            } else {
                "provider"
            },
            item.kind()
        );
        let expanded = self.panels.source_control.expanded.contains(&kind);
        let auth = item.auth();
        let (auth_label, auth_warn) = auth.map(auth_presentation).unzip();
        let dot = item_status_dot(item, &theme);
        let icon = match (item, item.kind().as_str()) {
            (Item::Provider(_), "github") => Some(IconName::Github),
            (Item::Vcs(_), "git") => Some(IconName::GitBranch),
            _ => None,
        };
        let mark: AnyElement = match icon {
            Some(icon) => div()
                .relative()
                .flex()
                .size_5()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(icon)
                        .size(rems(1.125))
                        .color(with_alpha(theme.foreground, 0.8).into()),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .left(px(-4.))
                        .size(px(12.))
                        .rounded_full()
                        .border_2()
                        .border_color(theme.background)
                        .bg(dot),
                )
                .into_any_element(),
            None => div()
                .size_2()
                .flex_shrink_0()
                .rounded_full()
                .bg(dot)
                .into_any_element(),
        };

        let summary = self.item_summary(item, &theme);
        let toggle_kind = kind.clone();
        let has_details = details.is_some();
        let chevron = has_details.then(|| {
            Button::new(SharedString::from(format!("toggle-details-{kind}")))
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::IconXs)
                .icon(if expanded {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                })
                .icon_size(rems(0.875))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let expanded = &mut this.panels.source_control.expanded;
                    if !expanded.remove(&toggle_kind) {
                        expanded.insert(toggle_kind.clone());
                    }
                    cx.notify();
                }))
        });
        let label = item.label();
        let enabled = item.enabled();
        let availability = (!item.not_ready()).then(|| {
            switch(
                SharedString::from(format!("availability-{kind}")),
                enabled,
                true,
                cx,
                |_, _, _| {},
            )
        });
        div()
            .flex()
            .flex_col()
            .when(item.not_ready(), |this| this.opacity(0.8))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .child(mark)
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(TEXT_SM.size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(label),
                                    )
                                    .children(item.version().map(|version| {
                                        div()
                                            .font_family(theme.fonts.mono.clone())
                                            .text_size(TEXT_XS.size)
                                            .text_color(theme.muted_foreground)
                                            .child(version)
                                    }))
                                    .when(item.not_ready(), |this| {
                                        this.child(
                                            Badge::label("Coming Soon")
                                                .variant(BadgeVariant::Warning)
                                                .size(BadgeSize::Sm),
                                        )
                                    })
                                    .when(auth_warn == Some(true), |this| {
                                        this.child(
                                            Badge::label(auth_label.unwrap_or_default())
                                                .variant(BadgeVariant::Warning)
                                                .size(BadgeSize::Sm),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .min_w_0()
                                    .items_center()
                                    .gap_x_1()
                                    .text_size(TEXT_XS.size)
                                    .line_height(rems(1.25))
                                    .text_color(with_alpha(theme.muted_foreground, 0.8))
                                    .children(summary),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_2()
                            .children(chevron)
                            .children(availability),
                    ),
            )
            .when(expanded, |this| {
                this.children(details.map(|details| div().px_4().pb_4().pt_1().child(details)))
            })
            .into_any_element()
    }

    /// `itemSummary`.
    fn item_summary(&self, item: &Item, theme: &AppTheme) -> Vec<AnyElement> {
        let text = |value: String| div().child(value).into_any_element();
        if item.not_ready() {
            return vec![text(format!(
                "Support for {} is coming soon.",
                item.label()
            ))];
        }
        if !item.available() {
            return vec![text(format!(
                "Not available on this server: {}",
                item.install_hint()
            ))];
        }
        let Some(auth) = item.auth() else {
            return vec![text("Available".into())];
        };
        let detail = auth.detail.as_ref_option().map(|d| d.as_str().to_owned());
        match auth.status {
            SourceControlProviderAuthStatus::Authenticated => {
                let account = auth.account.as_ref_option().map(|a| a.as_str().to_owned());
                let mut parts = vec![text("Authenticated".into())];
                if let Some(account) = account {
                    parts.push(text("as".into()));
                    parts.push(
                        div()
                            .font_family(theme.fonts.mono.clone())
                            .text_size(TEXT_2XS.size)
                            .child(if self.panels.source_control.revealed_accounts {
                                account
                            } else {
                                redacted_placeholder(&account)
                            })
                            .into_any_element(),
                    );
                }
                if let Some(detail) = detail {
                    parts.push(text(format!("· {detail}")));
                }
                parts
            }
            SourceControlProviderAuthStatus::Unauthenticated => {
                let executable = item.executable();
                if executable.is_none() {
                    return vec![text(format!("Available. {}", item.install_hint()))];
                }
                if auth
                    .accounts
                    .as_ref()
                    .is_some_and(|accounts| accounts.iter().any(|a| a.authenticated))
                {
                    return vec![text(detail.unwrap_or_else(|| {
                        format!("Every {} host is turned off.", item.label())
                    }))];
                }
                vec![
                    text(format!(
                        "{} is not authenticated on this server. Sign in or configure credentials using the",
                        item.label()
                    )),
                    code(executable.unwrap_or_default(), theme).into_any_element(),
                    text("tool on the server host to enable change request features.".into()),
                ]
            }
            SourceControlProviderAuthStatus::Unknown => {
                vec![text(format!(
                    "Could not verify {}. {}",
                    item.label(),
                    detail.unwrap_or_else(|| item.install_hint())
                ))]
            }
        }
    }

    // ── git fetch interval ──────────────────────────────────────────────

    fn git_fetch_interval_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let resolved =
            turin_shared::server_settings::resolve_server_background_activity_settings(&vm.server);
        let base = turin_shared::server_settings::get_background_activity_base_profile(
            &vm.server.background_activity,
        );
        let preset = turin_shared::server_settings::get_background_activity_preset_settings(base);
        let seconds = duration_to_seconds(resolved.automatic_git_fetch_interval);
        let default_seconds = duration_to_seconds(preset.automatic_git_fetch_interval);
        let can_reset = seconds != default_seconds;
        let current = vm.server.background_activity.clone();
        let reset = {
            let current = current.clone();
            let action = self.cb(cx, move |this, _, cx| {
                let next = background_activity_fetch_interval_settings(&current, None);
                this.update_server(
                    json!({ "backgroundActivity": serde_json::to_value(next).unwrap_or(Value::Null) }),
                    cx,
                );
            });
            div()
                .flex()
                .size_5()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .when(!can_reset, |this| this.opacity(0.))
                .when(can_reset, |this| {
                    this.child(setting_reset_button(
                        "reset-git-fetch-interval",
                        "Reset to default",
                        move |w, c| action(w, c),
                    ))
                })
        };
        let field = self.number_field(
            "git-fetch-interval",
            seconds,
            0,
            GIT_FETCH_INTERVAL_STEP_SECONDS,
            window,
            cx,
            move |this, value, cx| {
                let next =
                    background_activity_fetch_interval_settings(&current, Some(value * 1_000));
                this.update_server(
                    json!({ "backgroundActivity": serde_json::to_value(next).unwrap_or(Value::Null) }),
                    cx,
                );
            },
        );
        let (id, title) = searchable_setting("git-fetch-interval");
        div()
            .id(SharedString::from(id.to_owned()))
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(reset),
                    )
                    .child(
                        div()
                            .max_w(rems(42.))
                            .text_size(TEXT_XS.size)
                            .line_height(rems(1.625).to_pixels(px(16.)) / 1.625 * 1.625)
                            .text_color(theme.muted_foreground)
                            .child("Refresh remote branches in the background. Set to 0 to avoid automatic Git prompts."),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .child(field)
                    .child(div().text_size(TEXT_XS.size).text_color(theme.muted_foreground).child("seconds")),
            )
            .into_any_element()
    }

    // ── GitHub: token + accounts ────────────────────────────────────────

    fn github_hosts_state(&self, cx: &gpui_kit::App) -> (IndexMap<String, String>, GitHubHosts) {
        let vm = self.vm(cx);
        let github = vm.server.source_control_hosts.get("github");
        let tokens = github
            .and_then(|settings| settings.get("tokens"))
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(host, value)| {
                        value.as_str().map(|v| (host.clone(), v.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let hosts = github
            .and_then(|settings| settings.get("hosts"))
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(host, choice)| {
                        (
                            host.clone(),
                            GitHubHostChoice {
                                enabled: choice
                                    .get("enabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true),
                                account: choice
                                    .get("account")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        (tokens, hosts)
    }

    fn github_settings(
        &mut self,
        item: &Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut column = div().flex().flex_col().gap_6();
        column = column.child(self.github_token_settings(window, cx));
        if item.available()
            && let Some(auth) = item.auth()
        {
            column = column.child(self.github_account_settings(auth, cx));
        }
        let (id, _) = searchable_setting("github-accounts");
        column
            .id(SharedString::from(id.to_owned()))
            .into_any_element()
    }

    fn github_token_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let (tokens, _) = self.github_hosts_state(cx);
        let host_value = self
            .panels
            .source_control
            .github_host
            .clone()
            .unwrap_or_else(|| DEFAULT_HOST.to_owned());
        let normalized = host_value.trim().to_lowercase();
        let is_saved = tokens.get(&normalized).is_some_and(|t| !t.is_empty());
        let saved_hosts: Vec<String> = tokens
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, _)| k.clone())
            .collect();

        let host_input = self.input_live(
            "github-token-host",
            &host_value,
            DEFAULT_HOST,
            window,
            cx,
            |this, text, _, cx| {
                this.panels.source_control.github_host = Some(text);
                cx.notify();
            },
        );
        let token_input = self.secret_input("github-token", is_saved, window, cx);
        let draft = token_input.read(cx).value().trim().to_owned();
        let save_tokens = tokens.clone();
        let remove_tokens = tokens.clone();
        let save_host = normalized.clone();
        let remove_host = normalized.clone();
        let save_token_input = token_input.clone();
        let save = Button::new("github-token-save")
            .size(ButtonSize::Xs)
            .label("Save")
            .disabled(normalized.is_empty() || draft.is_empty())
            .on_click(cx.listener(move |this, _, window, cx| {
                let value = save_token_input.read(cx).value().trim().to_owned();
                if save_host.is_empty() || value.is_empty() {
                    return;
                }
                let mut next: IndexMap<String, String> = save_tokens.clone();
                next.insert(save_host.clone(), value);
                this.update_server(
                    json!({ "sourceControlHosts": { "github": { "tokens": next } } }),
                    cx,
                );
                save_token_input.update(cx, |state, cx| state.set_value("", window, cx));
                let source = this.store.read(cx).source();
                source.rescan_source_control(cx);
            }));
        let remove = is_saved.then(|| {
            Button::new("github-token-remove")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Xs)
                .label("Remove")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut next: IndexMap<String, String> = remove_tokens.clone();
                    next.insert(remove_host.clone(), String::new());
                    this.update_server(
                        json!({ "sourceControlHosts": { "github": { "tokens": next } } }),
                        cx,
                    );
                    let source = this.store.read(cx).source();
                    source.rescan_source_control(cx);
                }))
        });
        let label = |text: &'static str| {
            div()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(text)
        };
        let token_url = format!(
            "https://{}/settings/personal-access-tokens/new",
            if normalized.is_empty() {
                DEFAULT_HOST
            } else {
                &normalized
            }
        );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .max_w(rems(42.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_1()
                    .text_size(TEXT_XS.size)
                    .line_height(rems(1.25))
                    .text_color(theme.muted_foreground)
                    .child("A token saved here is used before")
                    .child(code("GH_TOKEN", &theme))
                    .child("and the")
                    .child(code("gh", &theme))
                    .child("login, so GitHub works without the GitHub CLI. Give it read and write access to pull requests and contents.")
                    .child(
                        div()
                            .id("github-create-token")
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor(CursorStyle::PointingHand)
                            .text_color(theme.foreground)
                            .hover(|style| style.underline())
                            .on_click(move |_, _, cx| cx.open_url(&token_url))
                            .child("Create a token")
                            .child(Icon::new(IconName::ExternalLink).size(rems(0.75)).color(theme.foreground)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .w(rems(12.))
                            .flex_shrink_0()
                            .child(label("Host"))
                            .child(text_field(&host_input, None, FieldSize::Sm, false, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .min_w_0()
                            .flex_1()
                            .child(label("Token"))
                            .child(text_field(&token_input, None, FieldSize::Sm, false, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(if saved_hosts.is_empty() {
                                "No token saved; the server uses GH_TOKEN or the gh login.".to_owned()
                            } else {
                                format!("Saved for {}.", saved_hosts.join(", "))
                            }),
                    )
                    .child(div().flex().flex_shrink_0().gap_2().children(remove).child(save)),
            )
            .into_any_element()
    }

    fn github_account_settings(
        &mut self,
        auth: &SourceControlProviderAuth,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let (_, hosts) = self.github_hosts_state(cx);
        let groups = group_github_accounts(auth.accounts.as_deref().unwrap_or_default());
        let revealed = self.panels.source_control.revealed_accounts;
        if groups.is_empty() {
            return div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x_1()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child("Sign in with")
                .child(code("gh auth login", &theme))
                .child("on the server host, then rescan to choose accounts here.")
                .into_any_element();
        }
        let login_theme = theme.clone();
        let login = move |account: &str| -> AnyElement {
            div()
                .min_w_0()
                .truncate()
                .font_family(theme_mono(&login_theme))
                .text_size(TEXT_2XS.size)
                .child(if revealed {
                    account.to_owned()
                } else {
                    redacted_placeholder(account)
                })
                .into_any_element()
        };
        let reveal = Button::new("reveal-github-accounts")
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::IconXs)
            .icon(if revealed {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .pressed(revealed)
            .tooltip(Tooltip::new(if revealed {
                "Hide GitHub accounts"
            } else {
                "Reveal GitHub accounts"
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                this.panels.source_control.revealed_accounts =
                    !this.panels.source_control.revealed_accounts;
                cx.notify();
            }));
        let mut column = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .max_w(rems(42.))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x_1()
                        .text_size(TEXT_XS.size)
                        .line_height(rems(1.25))
                        .text_color(theme.muted_foreground)
                        .child("Choose which")
                        .child(code("gh", &theme))
                        .child("login each GitHub host uses, or turn a host off."),
                )
                .child(reveal),
        );
        for group in groups {
            let choice = hosts.get(&group.host);
            let enabled = choice.map(|c| c.enabled).unwrap_or(true);
            let stale_pin = choice
                .and_then(|c| c.account.as_ref())
                .is_some_and(|account| !group.selectable.contains(account));
            let pinned = match choice.and_then(|c| c.account.clone()) {
                Some(account) if !stale_pin => account,
                _ => ACTIVE_ACCOUNT.to_owned(),
            };
            let host = group.host.clone();
            let hosts_for_toggle = hosts.clone();
            let entity = cx.entity();
            let host_for_toggle = host.clone();
            let toggle = switch(
                SharedString::from(format!("github-host-{host}")),
                enabled,
                false,
                cx,
                move |next, _, cx| {
                    let hosts = hosts_for_toggle.clone();
                    let host = host_for_toggle.clone();
                    entity.update(cx, |this, cx| {
                        this.save_github_hosts(
                            &hosts,
                            &host,
                            GitHubHostChange {
                                enabled: Some(next),
                                account: None,
                            },
                            cx,
                        );
                    });
                },
            );
            let mut block = div().flex().flex_col().gap_2().child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(group.host.clone()),
                            )
                            .when(group.selectable.len() == 1, |this| {
                                this.children(group.selectable.first().map(|account| {
                                    div()
                                        .flex()
                                        .min_w_0()
                                        .items_center()
                                        .gap_1()
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child("Signed in as")
                                        .child(login(account))
                                }))
                            }),
                    )
                    .child(toggle),
            );
            if group.selectable.len() > 1 {
                let active_label = |account: &Option<String>| -> AnyElement {
                    let mut row = div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .child("Active gh account");
                    if let Some(account) = account {
                        row = row.child("(").child(login(account)).child(")");
                    }
                    row.into_any_element()
                };
                let mut options: Vec<(Value, AnyElement, bool)> = vec![(
                    json!(ACTIVE_ACCOUNT),
                    active_label(&group.active_account),
                    pinned == ACTIVE_ACCOUNT,
                )];
                for account in &group.selectable {
                    options.push((json!(account), login(account), *account == pinned));
                }
                let label = if pinned == ACTIVE_ACCOUNT {
                    active_label(&group.active_account)
                } else {
                    login(&pinned)
                };
                let pick_hosts = hosts.clone();
                let pick_host = host.clone();
                let control = self.select_control_rich(
                    cx,
                    &format!("github-account-{host}"),
                    label,
                    options,
                    SelectWidth::Fixed(16.),
                    move |this, value, _, cx| {
                        let Some(value) = value.as_str() else { return };
                        let account = if value == ACTIVE_ACCOUNT {
                            None
                        } else {
                            Some(value.to_owned())
                        };
                        this.save_github_hosts(
                            &pick_hosts,
                            &pick_host,
                            GitHubHostChange {
                                enabled: None,
                                account: Some(account),
                            },
                            cx,
                        );
                    },
                );
                block = block.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .when(!enabled, |this| this.opacity(0.64))
                        .child(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child("Account"),
                        )
                        .child(control),
                );
            }
            if stale_pin {
                let reset_hosts = hosts.clone();
                let reset_host = host.clone();
                block = block.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.warning)
                                .child("The chosen login is no longer signed in, so the active gh login is used."),
                        )
                        .child(
                            Button::new(SharedString::from(format!("github-use-active-{host}")))
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Xs)
                                .label("Use active login")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.save_github_hosts(
                                        &reset_hosts,
                                        &reset_host,
                                        GitHubHostChange { enabled: None, account: Some(None) },
                                        cx,
                                    );
                                })),
                        ),
                );
            }
            for entry in &group.broken {
                block = block.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .text_size(TEXT_XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.7))
                        .child(login(entry.account.as_str()))
                        .child(format!(
                            "can't be used: {}",
                            entry
                                .error
                                .as_ref()
                                .map(|e| e.as_str())
                                .unwrap_or("gh reports this login as invalid.")
                        )),
                );
            }
            if let Some(variable) = &group.environment_variable {
                block = block.child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.warning)
                        .child(format!("{variable} is set on the server, so it overrides the account chosen here until it is unset.")),
                );
            }
            column = column.child(block);
        }
        column.into_any_element()
    }

    fn save_github_hosts(
        &mut self,
        hosts: &GitHubHosts,
        host: &str,
        change: GitHubHostChange,
        cx: &mut Context<Self>,
    ) {
        let next = next_github_hosts(hosts, host, change);
        let value: serde_json::Map<String, Value> = next
            .iter()
            .map(|(host, choice)| {
                let mut entry = serde_json::Map::new();
                entry.insert("enabled".into(), json!(choice.enabled));
                if let Some(account) = &choice.account {
                    entry.insert("account".into(), json!(account));
                }
                (host.clone(), Value::Object(entry))
            })
            .collect();
        self.update_server(
            json!({ "sourceControlHosts": { "github": { "hosts": value } } }),
            cx,
        );
        let source = self.store.read(cx).source();
        source.rescan_source_control(cx);
    }

    // ── text generation ─────────────────────────────────────────────────

    fn source_control_text_generation_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let vm = self.vm(cx);
        let json = self.json(cx);
        let text_providers: Vec<_> = vm
            .providers
            .iter()
            .filter(|p| p.is_text_generation_capable())
            .cloned()
            .collect();
        let can_enable_dedicated = text_providers.iter().any(provider_is_selectable);
        let default_selection = {
            let configured = json
                .get(Which::Server, "textGenerationModelSelection")
                .clone();
            if configured.is_object() {
                configured
            } else {
                resolve_default_selection(&text_providers)
                    .map(|(instance, model)| json!({ "instanceId": instance, "model": model }))
                    .unwrap_or(Value::Null)
            }
        };

        let mut section =
            SettingsSection::new("Text generation").id("source-control-text-generation");
        section = section.child(self.branch_naming_row(&json, window, cx));
        for row in self.branch_naming_detail_rows(&json, window, cx) {
            section = section.child(row);
        }

        // Writing style.
        let style = json.get(Which::Server, "sourceControlWritingStyle").clone();
        let default_style = json
            .default_of(Which::Server, "sourceControlWritingStyle")
            .clone();
        let mode = style
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("repo_conventions")
            .to_owned();
        let custom_instructions = style
            .get("customInstructions")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let modes: [(&str, &str, &str); 3] = [
            (
                "repo_conventions",
                "Repository conventions",
                "In each project, matches recent change descriptions and change request titles.",
            ),
            (
                "conventional_commits",
                "Conventional Commits",
                "Use Conventional Commit prefixes and keep change request text concise.",
            ),
            (
                "custom",
                "Custom instructions",
                "Use your instructions for change descriptions and change requests in every project.",
            ),
        ];
        let (_, mode_label, mode_description) = modes
            .iter()
            .find(|(m, _, _)| *m == mode)
            .copied()
            .unwrap_or(modes[0]);
        let dirty = style.get("mode") != default_style.get("mode")
            || style.get("customInstructions") != default_style.get("customInstructions");
        let reset = dirty.then(|| {
            let default_mode = default_style.get("mode").cloned().unwrap_or(Value::Null);
            let default_instructions = default_style.get("customInstructions").cloned().unwrap_or(json!(""));
            let action = self.cb(cx, move |this, _, cx| {
                this.update_server(
                    json!({ "sourceControlWritingStyle": { "mode": default_mode, "customInstructions": default_instructions } }),
                    cx,
                );
            });
            setting_reset_button("reset-writing-style", "Reset to default", move |w, c| action(w, c)).into_any_element()
        });
        let control = self.select_control(
            cx,
            "source-control-writing-style",
            mode_label.into(),
            modes
                .iter()
                .map(|(m, label, _)| (json!(m), SharedString::from(*label), *m == mode))
                .collect(),
            SelectWidth::Fixed(14.),
            false,
            |this, value, _, cx| {
                this.update_server(
                    json!({ "sourceControlWritingStyle": { "mode": value } }),
                    cx,
                );
            },
        );
        let mut style_row = Self::base_row("source-control-writing-style")
            .description(mode_description)
            .reset_action(reset)
            .control(control);
        if mode == "custom" {
            let area = self.textarea(
                "source-control-custom-instructions",
                &custom_instructions,
                "Keep titles concise. Use short bullet points in descriptions.",
                4,
                window,
                cx,
                |this, text, _, cx| {
                    let text = text.trim().to_owned();
                    this.update_server(
                        json!({ "sourceControlWritingStyle": { "customInstructions": text } }),
                        cx,
                    );
                },
            );
            style_row = style_row.child(
                div()
                    .mt_3()
                    .max_w(rems(42.))
                    .pb(px(14.))
                    .child(textarea_field(&area, 4, cx)),
            );
        }
        section = section.child(style_row);

        // Follow change request templates.
        let templates = style
            .get("followChangeRequestTemplates")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let default_templates = default_style
            .get("followChangeRequestTemplates")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let templates_reset = (templates != default_templates).then(|| {
            let action = self.cb(cx, move |this, _, cx| {
                this.update_server(json!({ "sourceControlWritingStyle": { "followChangeRequestTemplates": default_templates } }), cx);
            });
            setting_reset_button("reset-templates", "Reset to default", move |w, c| action(w, c)).into_any_element()
        });
        let entity = cx.entity();
        section = section.child(
            Self::base_row("follow-change-request-templates")
                .description("Use the repository's template for change request descriptions when available.")
                .reset_action(templates_reset)
                .control(switch("switch-follow-templates", templates, false, cx, move |next, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.update_server(json!({ "sourceControlWritingStyle": { "followChangeRequestTemplates": next } }), cx);
                    });
                })),
        );

        // Writer model.
        section = section.child(self.dedicated_model_row(
            "source-control-writer-model",
            "Model for source control text and branch or bookmark names. Off uses the environment's text generation model.",
            "sourceControlWriterModelSelection",
            "source-control-writer-model-picker",
            &text_providers,
            can_enable_dedicated,
            default_selection.clone(),
            &json,
            cx,
        ));

        // Review guides.
        let enabled = json.bool(Which::Server, "reviewGuidesEnabled");
        let entity = cx.entity();
        section = section.child(
            Self::base_row("review-guides")
                .description("Adds a Guide tab to pull requests: an agent writes a tour of the change, stop by stop, with the calls a maintainer should make.")
                .control(switch("switch-review-guides", enabled, false, cx, move |next, _, cx| {
                    entity.update(cx, |this, cx| this.update_server(json!({ "reviewGuidesEnabled": next }), cx));
                })),
        );
        if enabled {
            let writer = json
                .get(Which::Server, "sourceControlWriterModelSelection")
                .clone();
            let guide_default = if writer.is_object() {
                writer
            } else {
                default_selection.clone()
            };
            section = section.child(self.dedicated_model_row(
                "review-guide-model",
                "Model that reads a pull request to write its review guide. It reads the change itself over many steps, so a stronger model helps. Off uses the source control writer model.",
                "reviewGuideModelSelection",
                "review-guide-model-picker",
                &text_providers,
                can_enable_dedicated,
                guide_default,
                &json,
                cx,
            ));
            let instructions = json
                .str(Which::Server, "reviewGuideInstructions")
                .to_owned();
            let area = self.textarea(
                "review-guide-instructions",
                &instructions,
                "Show service interfaces before implementations. Skip test files unless they change behavior. Keep summaries to two sentences.",
                4,
                window,
                cx,
                |this, text, _, cx| {
                    let text = text.trim().to_owned();
                    this.update_server(json!({ "reviewGuideInstructions": text }), cx);
                },
            );
            let reset = (!instructions.is_empty()).then(|| {
                let action = self.cb(cx, |this, _, cx| {
                    this.update_server(json!({ "reviewGuideInstructions": "" }), cx)
                });
                setting_reset_button(
                    "reset-review-guide-instructions",
                    "Reset to default",
                    move |w, c| action(w, c),
                )
                .into_any_element()
            });
            section = section.child(
                Self::base_row("review-guide-instructions")
                    .description("Your preferences for review guides: what to show first, dwell on, or skip. The guide's format and coverage stay the same.")
                    .reset_action(reset)
                    .child(div().mt_3().max_w(rems(42.)).pb(px(14.)).child(textarea_field(&area, 4, cx))),
            );
        }
        section.into_any_element()
    }

    /// A row with a model picker and an on/off switch (`usesDedicatedModel`).
    #[allow(clippy::too_many_arguments)]
    fn dedicated_model_row(
        &mut self,
        search_id: &'static str,
        description: &'static str,
        key: &'static str,
        menu_id: &'static str,
        providers: &[turin_provider_core::server_provider::ServerProvider],
        can_enable: bool,
        enable_with: Value,
        json: &Json,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let selection = json.get(Which::Server, key).clone();
        let uses = !selection.is_null();
        let mut control = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap_2();
        if uses && !can_enable {
            control = control.child(
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("No text generation providers available."),
            );
        }
        if uses && can_enable {
            control = control.child(self.model_selection_control(
                cx,
                menu_id,
                providers,
                &selection,
                move |this, value, cx| this.update_server(json!({ key: value }), cx),
            ));
        }
        let entity = cx.entity();
        control = control.child(switch(
            SharedString::from(format!("switch-{search_id}")),
            uses,
            !uses && !can_enable,
            cx,
            move |next, _, cx| {
                let value = if next {
                    enable_with.clone()
                } else {
                    Value::Null
                };
                entity.update(cx, |this, cx| this.update_server(json!({ key: value }), cx));
            },
        ));
        Self::base_row(search_id)
            .description(description)
            .control(control)
            .into_any_element()
    }

    fn branch_naming_row(
        &mut self,
        json: &Json,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mode = json.str(Which::Server, "branchNamingMode").to_owned();
        let row = SelectRowSpec {
            search_id: "worktree-branch-naming",
            description: "Choose how new worktree branches are named from your first message."
                .into(),
            which: Which::Server,
            key: "branchNamingMode",
            reset_label: "branch naming",
            options: vec![
                (json!("static"), "Static prefix".into()),
                (json!("semantic"), "Semantic prefix".into()),
                (json!("custom"), "Custom instructions".into()),
            ],
            width: SelectWidth::Auto,
            check: false,
        };
        let element = self.select_row(cx, json, row);
        // `status` is a SettingsRow field; the semantic hint is rendered under the row.
        if mode == "semantic" {
            let theme = cx.app_theme().clone();
            return div()
                .flex()
                .flex_col()
                .child(element)
                .child(
                    div()
                        .px_4()
                        .pb_3()
                        .text_size(TEXT_XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.8))
                        .child("The model chooses a prefix that describes the work, such as feat/add-search, fix/login-timeout, or refactor/auth."),
                )
                .into_any_element();
        }
        element
    }

    fn branch_naming_detail_rows(
        &mut self,
        json: &Json,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mode = json.str(Which::Server, "branchNamingMode").to_owned();
        let mut rows = Vec::new();
        if mode == "static" {
            let prefix = json.str(Which::Server, "branchNamePrefix").to_owned();
            let input = self.input(
                "branch-prefix",
                &prefix,
                "No prefix",
                window,
                cx,
                |this, text, _, cx| {
                    this.update_server(json!({ "branchNamePrefix": text.trim() }), cx);
                },
            );
            rows.push(
                SettingsRow::new("Branch prefix")
                    .description(
                        "For example, t3 or t3/ produces t3/add-search. Leave empty for no prefix.",
                    )
                    .reset_action(self.reset_button(
                        cx,
                        json,
                        Which::Server,
                        "branchNamePrefix",
                        "branch prefix",
                    ))
                    .control(div().w(rems(14.)).child(text_field(
                        &input,
                        None,
                        FieldSize::Sm,
                        false,
                        cx,
                    )))
                    .into_any_element(),
            );
        }
        if mode == "custom" {
            let instructions = json.str(Which::Server, "branchNameInstructions").to_owned();
            let area = self.textarea(
                "branch-naming-instructions",
                &instructions,
                "Use julius/ followed by the issue ID and a short description.",
                4,
                window,
                cx,
                |this, text, _, cx| {
                    this.update_server(json!({ "branchNameInstructions": text.trim() }), cx);
                },
            );
            rows.push(
                SettingsRow::new("Branch naming instructions")
                    .description("Appended to the naming prompt. The model returns the complete branch name; no prefix or suffix is added.")
                    .reset_action(self.reset_button(cx, json, Which::Server, "branchNameInstructions", "branch naming instructions"))
                    .child(div().mt_3().max_w(rems(42.)).pb(px(14.)).child(textarea_field(&area, 4, cx)))
                    .into_any_element(),
            );
        }
        rows
    }
}

fn theme_mono(theme: &AppTheme) -> SharedString {
    theme.fonts.mono.clone().into()
}
