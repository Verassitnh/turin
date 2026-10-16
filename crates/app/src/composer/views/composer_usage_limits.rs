//! Port of upstream/apps/web/src/components/chat/ComposerUsageLimits.tsx
//!
//! The `/usage-limits` result as a composer notice: it stacks under warnings
//! and dismisses like one. The row names the account (or the account count);
//! the body below it lists each account's limit windows, a `Manage usage`
//! link and the banked reset credits.
//!
//! The report is data on the banner item ([`BannerBody::UsageLimits`]); the
//! composer paints the body with [`usage_limits_banner_body`]. A single
//! account's label is the row's [`BannerAccount`] so the composer can redact
//! it when it holds an address (`RedactedSensitiveText`). The reset-credit
//! controls need a [`UsageBackend`]; without one they are left out.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window, div,
    px, rems,
};
use turin_contracts::EnvironmentId;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::provider_usage_limits::{UsageLimitsReport, UsageLimitsReportAccount};
use turin_shared::usage_limits::limits_notice;

use crate::components::usage::backend::UsageBackend;
use crate::components::usage::usage_limits::{ResetCredits, limit_windows};
use crate::composer::view_model::{
    BannerAccount, BannerBody, BannerItem, BannerPriority, BannerVariant,
};
use crate::settings::provider_meta::driver_label;
use crate::theme::AppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::timestamp_format::{TimestampFormat, parse_timestamp_ms};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName};

/// Driver name, then the instance when there could be more than one of that
/// driver.
pub fn account_label(account: &UsageLimitsReportAccount) -> String {
    let Some(instance_id) = &account.instance_id else {
        return account.label.as_str().to_owned();
    };
    let driver = driver_label(account.driver.as_str());
    // The default instance is often named after its driver; saying it twice
    // adds nothing.
    let instance = account
        .display_name
        .as_deref()
        .map(js_trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            if instance_id.as_str() != account.driver.as_str() {
                instance_id.as_str()
            } else {
                ""
            }
        });
    if !instance.is_empty() && instance.to_lowercase() != driver.to_lowercase() {
        format!("{driver} · {instance}")
    } else {
        driver
    }
}

/// `AccountSummary` as data: the label and the plan appended after it.
pub fn account_summary(account: &UsageLimitsReportAccount) -> BannerAccount {
    BannerAccount {
        label: account_label(account),
        plan: account.plan.as_ref().map(|plan| plan.as_str().to_owned()),
    }
}

/// `{label} · {plan}` for a place that cannot redact.
pub fn account_summary_text(account: &BannerAccount) -> String {
    match &account.plan {
        Some(plan) => format!("{} · {plan}", account.label),
        None => account.label.clone(),
    }
}

/// `usageLimitsBannerItem`.
pub fn usage_limits_banner_item(
    id: &str,
    report: Arc<UsageLimitsReport>,
    environment_id: &str,
) -> BannerItem {
    let single = match report.accounts.as_slice() {
        [only] => Some(account_summary(only)),
        _ => None,
    };
    let description = single
        .is_none()
        .then(|| format!("{} accounts", report.accounts.len()));
    BannerItem {
        id: id.to_owned(),
        variant: BannerVariant::Info,
        priority: Some(BannerPriority::Notice),
        icon: IconName::Gauge,
        title: "Usage limits".to_owned(),
        description,
        account: single,
        dismissible: true,
        dismiss_label: Some("Dismiss usage limits".to_owned()),
        actions: Vec::new(),
        copy_action: None,
        body: Some(BannerBody::UsageLimits {
            report,
            environment_id: environment_id.to_owned(),
        }),
    }
}

/// `UsageLimitsBannerBody`: `ComposerBanner.Body` (`ps-7`) holding one block
/// per account and the report's notices.
pub fn usage_limits_banner_body(
    item_id: &str,
    report: &UsageLimitsReport,
    environment_id: &str,
    backend: Option<&Rc<dyn UsageBackend>>,
    timestamp_format: TimestampFormat,
    theme: &AppTheme,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let now = parse_timestamp_ms(report.created_at.as_str()).unwrap_or(0) as f64;
    let multiple = report.accounts.len() > 1;
    let environment = EnvironmentId::new(environment_id).ok();
    let mut blocks: Vec<AnyElement> = Vec::new();
    for account in &report.accounts {
        let reset_credit_input = account.reset_credit_input.clone().or_else(|| {
            account.instance_id.clone().map(|instance_id| {
                turin_contracts::provider_usage_limits::ProviderConsumeResetCreditInput::Instance {
                    instance_id,
                }
            })
        });
        let notice = limits_notice(&account.limits);
        let mut block = div().flex().min_w_0().flex_col().gap_1();
        if multiple {
            block = block.child(
                div()
                    .truncate()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(account_summary_text(&account_summary(account))),
            );
        }
        block = match notice {
            Some(notice) => block.child(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(notice),
            ),
            None => block.child(limit_windows(
                &format!("{item_id}:{}", account.id.as_str()),
                account.driver.as_str(),
                &account.limits.windows,
                now,
                true,
                timestamp_format,
                cx,
            )),
        };
        if let Some(external) = &account.limits.external_usage {
            let url = external.url.as_str().to_owned();
            block = block.child(
                div().flex().child(
                    Button::new(SharedString::from(format!(
                        "{item_id}:manage:{}",
                        account.id.as_str()
                    )))
                    .variant(ButtonVariant::Link)
                    .size(ButtonSize::Sm)
                    .label("Manage usage")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
            );
        }
        if let (Some(backend), Some(environment), Some(input), Some(credits)) = (
            backend,
            environment.clone(),
            reset_credit_input,
            account.limits.reset_credits.clone(),
        ) {
            let key = format!(
                "{item_id}:credits:{}:{:?}",
                account.id.as_str(),
                credits.available_count
            );
            let backend = backend.clone();
            let credits_view = window.use_keyed_state(key, cx, move |_, _| {
                ResetCredits::new(backend, environment, input, credits, now)
            });
            block = block.child(credits_view);
        }
        blocks.push(block.into_any_element());
    }
    div()
        .flex()
        .min_w_0()
        .flex_col()
        .gap_2()
        .pl(rems(1.75))
        .pt_1()
        .pb(px(6.))
        .pr_2()
        .children(blocks)
        .children(report.notices.iter().map(|notice| {
            div()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(notice.clone())
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(accounts: serde_json::Value) -> UsageLimitsReport {
        serde_json::from_value(json!({
            "createdAt": "2026-01-01T00:00:00.000Z",
            "accounts": accounts,
            "notices": [],
        }))
        .expect("report")
    }

    fn account(extra: serde_json::Value) -> serde_json::Value {
        let mut base = json!({
            "id": "acct-1",
            "driver": "codex",
            "label": "Codex",
            "limits": {
                "checkedAt": "2026-01-01T00:00:00.000Z",
                "windows": [],
            },
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    }

    #[test]
    fn a_source_account_keeps_its_own_label() {
        let report = report(json!([account(json!({"label": "me@example.com"}))]));
        assert_eq!(account_label(&report.accounts[0]), "me@example.com");
    }

    #[test]
    fn an_instance_account_reads_driver_then_instance() {
        let named = report(json!([account(json!({
            "instanceId": "codex_work",
            "displayName": "  Work  ",
        }))]));
        assert_eq!(account_label(&named.accounts[0]), "Codex · Work");

        let by_id = report(json!([account(json!({"instanceId": "codex_work"}))]));
        assert_eq!(account_label(&by_id.accounts[0]), "Codex · codex_work");
    }

    #[test]
    fn the_default_instance_is_not_named_twice() {
        let default = report(json!([account(json!({"instanceId": "codex"}))]));
        assert_eq!(account_label(&default.accounts[0]), "Codex");

        let same_name = report(json!([account(json!({
            "instanceId": "codex_2",
            "displayName": "CODEX",
        }))]));
        assert_eq!(account_label(&same_name.accounts[0]), "Codex");
    }

    #[test]
    fn one_account_names_itself_and_its_plan() {
        let report = Arc::new(report(json!([account(json!({
            "instanceId": "codex",
            "plan": "Pro",
        }))])));
        let item = usage_limits_banner_item("usage-limits:1", report, "local");
        assert_eq!(item.title, "Usage limits");
        assert_eq!(item.description, None);
        let account = item.account.expect("single account");
        assert_eq!(account_summary_text(&account), "Codex · Pro");
        assert_eq!(item.dismiss_label.as_deref(), Some("Dismiss usage limits"));
        assert_eq!(item.priority, Some(BannerPriority::Notice));
        assert_eq!(item.variant, BannerVariant::Info);
    }

    #[test]
    fn several_accounts_are_counted() {
        let report = Arc::new(report(json!([
            account(json!({"id": "a"})),
            account(json!({"id": "b"})),
        ])));
        let item = usage_limits_banner_item("usage-limits:1", report, "local");
        assert_eq!(item.description.as_deref(), Some("2 accounts"));
        assert_eq!(item.account, None);
    }
}
